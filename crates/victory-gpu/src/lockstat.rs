//! Diagnostics: how long the memory-tracking locks (`GUARDS` and `JOURNAL`
//! in `watch.rs`, `VIEWS` in `inplace.rs`) are waited for and held, by the
//! function that takes them and the kind of thread that does.
//!
//! Diagnostics builds and tests only. Nothing here changes how a lock is
//! taken or held: [`take`] calls `Mutex::lock` as the callers did, reads the
//! clock around it, and on release counts the time held. The counting is
//! atomic adds and maxima on fixed arrays: no allocation, sorting or logging
//! while a lock is held or in the write-fault handler. `copystat` formats
//! the totals at its own line, outside every lock. `VICTORY_LOCK_STATS=0`
//! leaves the clock readings and counting out (then [`take`] only locks).
//!
//! The kinds of thread, [`Role`]:
//! * inside a submission (the submission lock held: the frame waits for
//!   whatever happens there);
//! * a title thread that submits, outside its submissions: the title's
//!   render threads, whose stalls delay the next submission;
//! * the relay (the thread named `victory-relay`), which publishes the copy
//!   back that settles wait for;
//! * a title recording thread: one that has made a list call (marked by
//!   `copystat` at its first) and has not submitted;
//! * a runtime worker: a thread the runtime started under a name
//!   (`dstorage {i}`, `victory-wide-{i}`, `victory-pipelines-{i}`,
//!   `texture copy queue`, `card accounting`, ...), except `title-main`;
//! * another title thread: a thread without a runtime name (the title
//!   starts its threads itself, nameless) or `title-main`, that has neither
//!   made a list call nor submitted. The runtime's few nameless threads (the
//!   input poller, the title's suspend routines, the `VICTORY_STACKS`
//!   sampler) would count here too; none is expected to take these locks;
//! * unclassified: a thread whose kind was not known when it took the lock,
//!   which happens only in the write-fault handler (the thread name is not
//!   looked up there) on a thread that had not taken a lock elsewhere nor
//!   made a list call yet.
//!
//! The four last are the "other threads" of the first counts (PR 01B-0):
//! their sums are logged as before.
//!
//! The kind is the thread's at each taking, not its last: a recording
//! thread's takings before its first list call count under "another title
//! thread" (or "unclassified" in the handler). Those GUARDS waits are
//! added up per thread, and when the thread makes its first list call or
//! submission, the number of such threads and their earlier waits are
//! counted ([`TOOK_TITLE_ROLE`], [`WAITED_BEFORE_TITLE_ROLE_NS`]): a
//! window where they are not zero has takings counted under an earlier
//! kind.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

/// Who takes a lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Site {
    /// `GUARDS` by a check of a copy's range (`watch::guard`).
    GuardCheck = 0,
    /// `GUARDS` by the look before it (is the first block known?).
    GuardKnown,
    /// `GUARDS` by the write-fault handler.
    GuardFault,
    /// `GUARDS` by a write told (`watch::writing`).
    GuardWrite,
    /// `GUARDS` when a view is mapped (`watch::views_mapped`).
    GuardMapped,
    /// `GUARDS` when title memory is remapped (`watch::title_memory_remapped`).
    GuardRemapped,
    /// `JOURNAL` by a write-watch asking (`watch::writes`), which holds it
    /// across the kernel call and, for view memory, the guard check.
    JournalWrites,
    /// `JOURNAL` by a look at what was asked ahead (`watch::asked`).
    JournalAsked,
    /// `JOURNAL` by asking ahead (`ask_ahead`, `ask_once`: kernel calls).
    JournalAsk,
    /// `JOURNAL` otherwise (ends of askings ahead, `wrote`).
    JournalOther,
    /// `VIEWS` on a thread that holds `GUARDS` (alias lookups in checks
    /// and openings): its time is part of that `GUARDS` hold.
    ViewsUnderGuards,
    /// `VIEWS` otherwise.
    ViewsOther,
}

/// The sites, in order.
pub(crate) const SITES: usize = 12;

/// The locks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lock {
    Guards = 0,
    Journal,
    Views,
}

pub(crate) const LOCKS: usize = 3;

impl Site {
    pub(crate) fn lock(self) -> Lock {
        match self as usize {
            0..=5 => Lock::Guards,
            6..=9 => Lock::Journal,
            _ => Lock::Views,
        }
    }
}

/// The kind of thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    Submitting = 0,
    TitleSubmitter,
    Relay,
    /// A title thread that has made a list call and not submitted.
    Recording,
    /// A thread the runtime started under a name.
    Runtime,
    /// A thread without a runtime name (or `title-main`), neither
    /// recording nor submitting.
    TitleOther,
    /// Not known when taken (the fault handler, before any lookup).
    Unclassified,
}

pub(crate) const ROLES: usize = 7;

/// The kinds that made up "other threads" before they were told apart.
pub(crate) const OTHER_ROLES: [Role; 4] = [Role::Recording, Role::Runtime, Role::TitleOther, Role::Unclassified];

/// Per site and role: times taken, nanoseconds waiting, nanoseconds held,
/// longest wait and hold, waits over 100 µs, holds over 100 µs and over
/// 1 ms, and the sum of the other threads already waiting when this one
/// asked (divided by the times taken: the mean queue it joined).
pub(crate) const TAKEN: usize = 0;
pub(crate) const WAIT_NS: usize = 1;
pub(crate) const HOLD_NS: usize = 2;
pub(crate) const MAX_WAIT_NS: usize = 3;
pub(crate) const MAX_HOLD_NS: usize = 4;
pub(crate) const WAITS_OVER_100US: usize = 5;
pub(crate) const HOLDS_OVER_100US: usize = 6;
pub(crate) const HOLDS_OVER_1MS: usize = 7;
pub(crate) const QUEUED: usize = 8;
pub(crate) const FIELDS: usize = 9;

/// A snapshot of every count.
pub(crate) type Counts = [[[u64; FIELDS]; ROLES]; SITES];

static COUNTS: [[[AtomicU64; FIELDS]; ROLES]; SITES] =
    [const { [const { [const { AtomicU64::new(0) }; FIELDS] }; ROLES] }; SITES];

/// Per lock: threads waiting for it now, and the most seen waiting.
static WAITING: [AtomicU32; LOCKS] = [const { AtomicU32::new(0) }; LOCKS];
static MOST_WAITING: [AtomicU64; LOCKS] = [const { AtomicU64::new(0) }; LOCKS];

thread_local! {
    /// This thread is inside a submission.
    static SUBMITTING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// This thread has submitted (a title render thread).
    static SUBMITTER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// This thread has made a list call (a title recording thread).
    static RECORDING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// 0 not known yet, 1 the relay, 2 a runtime worker, 3 a title thread.
    static NAMED: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    /// Nanoseconds this thread waited for `GUARDS` as another title thread
    /// or unclassified, before its first list call or submission.
    static BEFORE_TITLE_ROLE_NS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// `GUARDS` holds on this thread (for [`Site::ViewsUnderGuards`]).
    static GUARDS_HELD: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
thread_local! {
    /// The same counts for this thread alone, for tests (other tests run on
    /// other threads meanwhile).
    static MINE: std::cell::RefCell<Counts> = const { std::cell::RefCell::new([[[0; FIELDS]; ROLES]; SITES]) };
}

/// Whether the counting is on (`VICTORY_LOCK_STATS`, read once).
pub(crate) fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_LOCK_STATS").is_none_or(|v| v != "0"))
}

/// The thread enters or leaves a submission (`copystat`).
pub(crate) fn submission(inside: bool) {
    SUBMITTING.with(|s| s.set(inside));
    if inside && !SUBMITTER.with(|s| s.replace(true)) && !RECORDING.with(|r| r.get()) {
        took_title_role();
    }
}

/// The thread made a list call (`copystat`, at each: only the first
/// changes anything).
pub(crate) fn recording() {
    if !RECORDING.with(|r| r.replace(true)) && !SUBMITTER.with(|s| s.get()) {
        took_title_role();
    }
}

/// The thread's first list call or submission: counts it and its earlier
/// waits under another kind.
fn took_title_role() {
    let before = BEFORE_TITLE_ROLE_NS.with(|b| b.replace(0));
    extra(TOOK_TITLE_ROLE, 1);
    extra(WAITED_BEFORE_TITLE_ROLE_NS, before);
}

/// The kind of a thread by its name: the relay, a title thread (no name,
/// or `title-main`), or a runtime worker.
fn named(name: Option<&str>) -> u8 {
    match name {
        Some("victory-relay") => 1,
        None | Some("title-main") => 3,
        Some(_) => 2,
    }
}

/// The kind of this thread. In the fault handler (`in_handler`) the thread
/// name is not looked up (`thread::current` may allocate on a thread the
/// runtime did not start); a thread not known by then is unclassified,
/// and the relay never takes a fault (its writes are told).
pub(crate) fn role(in_handler: bool) -> Role {
    if SUBMITTING.with(|s| s.get()) {
        return Role::Submitting;
    }
    if SUBMITTER.with(|s| s.get()) {
        return Role::TitleSubmitter;
    }
    if RECORDING.with(|r| r.get()) {
        return Role::Recording;
    }
    let kind = match NAMED.with(|n| n.get()) {
        0 if in_handler => return Role::Unclassified,
        0 => {
            let kind = named(std::thread::current().name());
            NAMED.with(|n| n.set(kind));
            kind
        }
        kind => kind,
    };
    match kind {
        1 => Role::Relay,
        2 => Role::Runtime,
        _ => Role::TitleOther,
    }
}

fn add(site: Site, role: Role, field: usize, n: u64) {
    COUNTS[site as usize][role as usize][field].fetch_add(n, Relaxed);
    #[cfg(test)]
    MINE.with(|m| m.borrow_mut()[site as usize][role as usize][field] += n);
}

fn max(site: Site, role: Role, field: usize, n: u64) {
    COUNTS[site as usize][role as usize][field].fetch_max(n, Relaxed);
    #[cfg(test)]
    MINE.with(|m| {
        let v = &mut m.borrow_mut()[site as usize][role as usize][field];
        *v = (*v).max(n);
    });
}

/// A held lock, counted at release. Derefs to what the lock guards.
///
/// Inside the lock the counting costs two clock readings (when taken and
/// when released); every count is added after the lock is released.
pub(crate) struct Held<'a, T> {
    guard: std::mem::ManuallyDrop<MutexGuard<'a, T>>,
    site: Site,
    role: Role,
    /// When it was taken, if counted.
    since: Option<Instant>,
    /// Nanoseconds waited and the queue joined, if counted; added at
    /// release, outside the lock.
    wait_ns: u64,
    queued: u32,
    /// Microseconds waited, if the wait was timed.
    waited_us: Option<u64>,
}

impl<T> Held<'_, T> {
    /// Microseconds this thread waited for the lock, if timed.
    pub(crate) fn waited_us(&self) -> Option<u64> {
        self.waited_us
    }
}

impl<T> std::ops::Deref for Held<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.guard
    }
}

impl<T> std::ops::DerefMut for Held<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.guard
    }
}

impl<T> Drop for Held<'_, T> {
    fn drop(&mut self) {
        let held = self.since.map(|since| since.elapsed().as_nanos() as u64);
        // SAFETY: dropped once, here; never used after.
        unsafe { std::mem::ManuallyDrop::drop(&mut self.guard) };
        // The lock is free again: count.
        let Some(ns) = held else { return };
        let (site, role) = (self.site, self.role);
        add(site, role, TAKEN, 1);
        add(site, role, WAIT_NS, self.wait_ns);
        max(site, role, MAX_WAIT_NS, self.wait_ns);
        if self.queued > 0 {
            add(site, role, QUEUED, u64::from(self.queued));
        }
        if self.wait_ns > 100_000 {
            add(site, role, WAITS_OVER_100US, 1);
        }
        add(site, role, HOLD_NS, ns);
        max(site, role, MAX_HOLD_NS, ns);
        if ns > 100_000 {
            add(site, role, HOLDS_OVER_100US, 1);
        }
        if ns > 1_000_000 {
            add(site, role, HOLDS_OVER_1MS, 1);
        }
        if site.lock() == Lock::Guards {
            GUARDS_HELD.with(|h| h.set(h.get().saturating_sub(1)));
            if matches!(role, Role::TitleOther | Role::Unclassified) {
                BEFORE_TITLE_ROLE_NS.with(|b| b.set(b.get() + self.wait_ns));
            }
        }
    }
}

/// Takes `lock` for `site`, as `lock.lock()` (a poisoned lock is taken
/// all the same, as every caller did). With counting on, the wait, the
/// queue joined and the hold are counted; with it off, the wait is still
/// timed when `time_wait` (for `watch::stats`' own wait counts).
pub(crate) fn take<T>(site: Site, lock: &Mutex<T>, time_wait: bool) -> Held<'_, T> {
    fn plain<T>(l: &Mutex<T>) -> MutexGuard<'_, T> {
        l.lock().unwrap_or_else(|e| e.into_inner())
    }
    let wrap = |guard, role, since, wait_ns, queued, waited_us| Held {
        guard: std::mem::ManuallyDrop::new(guard),
        site,
        role,
        since,
        wait_ns,
        queued,
        waited_us,
    };
    if !on() {
        if !time_wait {
            return wrap(plain(lock), Role::Unclassified, None, 0, 0, None);
        }
        let asked = Instant::now();
        let guard = plain(lock);
        let waited_us = Some(asked.elapsed().as_micros() as u64);
        return wrap(guard, Role::Unclassified, None, 0, 0, waited_us);
    }
    let role = self::role(site == Site::GuardFault);
    let which = site.lock() as usize;
    let queued = WAITING[which].fetch_add(1, Relaxed);
    if queued > 0 {
        MOST_WAITING[which].fetch_max(u64::from(queued), Relaxed);
    }
    let asked = Instant::now();
    let guard = plain(lock);
    let taken = Instant::now();
    WAITING[which].fetch_sub(1, Relaxed);
    if site.lock() == Lock::Guards {
        GUARDS_HELD.with(|h| h.set(h.get() + 1));
    }
    let ns = taken.duration_since(asked).as_nanos() as u64;
    wrap(guard, role, Some(taken), ns, queued, Some(ns / 1000))
}

/// The site for `VIEWS` on this thread now.
pub(crate) fn views_site() -> Site {
    if GUARDS_HELD.with(|h| h.get()) > 0 { Site::ViewsUnderGuards } else { Site::ViewsOther }
}

/// Every count since the last call, reset, and per lock the most threads
/// seen waiting at once.
#[cfg(feature = "diagnostics")]
pub(crate) fn take_counts() -> (Counts, [u64; LOCKS]) {
    (
        COUNTS.each_ref().map(|r| r.each_ref().map(|f| f.each_ref().map(|v| v.swap(0, Relaxed)))),
        MOST_WAITING.each_ref().map(|v| v.swap(0, Relaxed)),
    )
}

/// This thread's counts since the last call, reset (tests).
#[cfg(test)]
pub(crate) fn mine() -> Counts {
    MINE.with(|m| std::mem::replace(&mut *m.borrow_mut(), [[[0; FIELDS]; ROLES]; SITES]))
}

/// Sums of one field over the roles of a site.
#[cfg(feature = "diagnostics")]
pub(crate) fn site_total(c: &Counts, site: Site, field: usize) -> u64 {
    if matches!(field, MAX_WAIT_NS | MAX_HOLD_NS) {
        c[site as usize].iter().map(|r| r[field]).max().unwrap_or(0)
    } else {
        c[site as usize].iter().map(|r| r[field]).sum()
    }
}

/// Sums of one field over the sites of a lock, for some roles.
#[cfg(any(test, feature = "diagnostics"))]
pub(crate) fn lock_role_total(c: &Counts, lock: Lock, roles: &[Role], field: usize) -> u64 {
    (0..SITES)
        .filter(|&s| ALL_SITES[s].lock() == lock)
        .flat_map(|s| roles.iter().map(move |&r| c[s][r as usize][field]))
        .sum()
}

/// Sums of one field of a site over some roles.
#[cfg(any(test, feature = "diagnostics"))]
pub(crate) fn site_role_total(c: &Counts, site: Site, roles: &[Role], field: usize) -> u64 {
    roles.iter().map(|&r| c[site as usize][r as usize][field]).sum()
}

/// Beside the locks: nanoseconds a guard check spent asking Windows
/// whether its range is view memory (`VirtualQuery`, before taking
/// `GUARDS`), nanoseconds it spent asking about uncommitted blocks while
/// holding `GUARDS`, write faults that found their block open already
/// (another thread opened it while this one waited), threads that made
/// their first list call or submission, and the nanoseconds those threads
/// had waited for `GUARDS` before it (counted then under another title
/// thread or unclassified).
pub(crate) const IS_VIEW_NS: usize = 0;
pub(crate) const UNCOMMITTED_NS: usize = 1;
pub(crate) const FAULTS_FOUND_OPEN: usize = 2;
pub(crate) const TOOK_TITLE_ROLE: usize = 3;
pub(crate) const WAITED_BEFORE_TITLE_ROLE_NS: usize = 4;
pub(crate) const EXTRAS: usize = 5;

static EXTRA: [AtomicU64; EXTRAS] = [const { AtomicU64::new(0) }; EXTRAS];

#[cfg(test)]
thread_local! {
    static MINE_EXTRA: std::cell::Cell<[u64; EXTRAS]> = const { std::cell::Cell::new([0; EXTRAS]) };
}

/// Adds `n` to one of the counts beside the locks, if counting is on.
pub(crate) fn extra(which: usize, n: u64) {
    if on() {
        EXTRA[which].fetch_add(n, Relaxed);
        #[cfg(test)]
        MINE_EXTRA.with(|m| {
            let mut v = m.get();
            v[which] += n;
            m.set(v);
        });
    }
}

/// This thread's counts beside the locks since the last call, reset (tests).
#[cfg(test)]
pub(crate) fn mine_extra() -> [u64; EXTRAS] {
    MINE_EXTRA.with(|m| m.replace([0; EXTRAS]))
}

/// The counts beside the locks since the last call, reset.
pub(crate) fn take_extra() -> [u64; EXTRAS] {
    EXTRA.each_ref().map(|v| v.swap(0, Relaxed))
}

/// The sites by index.
pub(crate) const ALL_SITES: [Site; SITES] = [
    Site::GuardCheck,
    Site::GuardKnown,
    Site::GuardFault,
    Site::GuardWrite,
    Site::GuardMapped,
    Site::GuardRemapped,
    Site::JournalWrites,
    Site::JournalAsked,
    Site::JournalAsk,
    Site::JournalOther,
    Site::ViewsUnderGuards,
    Site::ViewsOther,
];

#[cfg(test)]
mod tests {
    use super::*;

    static M: Mutex<u32> = Mutex::new(0);

    #[test]
    fn sites_belong_to_their_locks() {
        for (i, s) in ALL_SITES.iter().enumerate() {
            assert_eq!(*s as usize, i);
        }
        assert_eq!(Site::GuardRemapped.lock(), Lock::Guards);
        assert_eq!(Site::JournalWrites.lock(), Lock::Journal);
        assert_eq!(Site::JournalOther.lock(), Lock::Journal);
        assert_eq!(Site::ViewsUnderGuards.lock(), Lock::Views);
    }

    /// A hold is counted at release, under the thread's role; a hold over
    /// 1 ms lands in both buckets; the wait of an uncontended lock is short.
    #[test]
    fn holds_and_waits_are_counted_by_site_and_role() {
        let _ = mine();
        {
            let mut g = take(Site::GuardWrite, &M, false);
            *g += 1;
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let c = mine();
        let r = c[Site::GuardWrite as usize][role(false) as usize];
        assert_eq!(r[TAKEN], 1);
        assert!(r[HOLD_NS] >= 2_000_000, "{r:?}");
        assert_eq!(r[MAX_HOLD_NS], r[HOLD_NS]);
        assert_eq!((r[HOLDS_OVER_100US], r[HOLDS_OVER_1MS]), (1, 1));
        assert_eq!(r[WAITS_OVER_100US], 0);
        // Nothing elsewhere.
        let others: u64 = c.iter().flatten().flatten().sum::<u64>() - r.iter().sum::<u64>();
        assert_eq!(others, 0);
    }

    /// Inside a submission the role is "submitting"; after it, a thread that
    /// has submitted is a title submitter.
    #[test]
    fn roles_follow_submissions() {
        // On a thread of its own: the submitter mark stays with a thread.
        let roles = std::thread::spawn(|| {
            let before = role(false);
            submission(true);
            let inside = role(true);
            submission(false);
            (before, inside, role(false))
        })
        .join()
        .unwrap();
        // A nameless thread (as the title's are) is another title thread.
        assert_eq!(roles, (Role::TitleOther, Role::Submitting, Role::TitleSubmitter));
    }

    /// A thread that has made a list call is a recording thread until it
    /// submits; a submitter that makes list calls stays a submitter.
    #[test]
    fn list_calls_make_recording_threads() {
        let roles = std::thread::spawn(|| {
            let before = role(false);
            recording();
            let recording_now = (role(false), role(true));
            submission(true);
            let inside = role(false);
            submission(false);
            recording();
            (before, recording_now, inside, role(false))
        })
        .join()
        .unwrap();
        assert_eq!(roles, (Role::TitleOther, (Role::Recording, Role::Recording), Role::Submitting, Role::TitleSubmitter));
    }

    /// Kinds by thread name: the runtime's named threads are workers;
    /// `title-main` and nameless threads are title threads; a thread first
    /// seen in the fault handler is unclassified until known elsewhere.
    #[test]
    fn thread_kinds_follow_names() {
        let kind = |name: Option<&str>| {
            let b = std::thread::Builder::new();
            let b = match name {
                Some(n) => b.name(n.into()),
                None => b,
            };
            b.spawn(|| (role(true), role(false), role(true))).unwrap().join().unwrap()
        };
        let u = Role::Unclassified;
        assert_eq!(kind(Some("dstorage 0")), (u, Role::Runtime, Role::Runtime));
        assert_eq!(kind(Some("victory-wide-3")), (u, Role::Runtime, Role::Runtime));
        assert_eq!(kind(Some("title-main")), (u, Role::TitleOther, Role::TitleOther));
        assert_eq!(kind(None), (u, Role::TitleOther, Role::TitleOther));
        assert_eq!(kind(Some("victory-relay")), (u, Role::Relay, Role::Relay));
        for r in OTHER_ROLES {
            assert!(!matches!(r, Role::Submitting | Role::TitleSubmitter | Role::Relay));
        }
    }

    /// A thread's GUARDS waits before its first list call count under the
    /// kind it had then; at that call the thread and those waits are
    /// counted, once.
    #[test]
    fn waits_before_the_first_list_call_are_counted_apart() {
        static L: Mutex<()> = Mutex::new(());
        let held = take(Site::JournalOther, &L, false);
        let (tx, rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let _ = (mine(), mine_extra());
            tx.send(()).unwrap();
            // A fault first: unclassified, and it waits for the holder.
            drop(take(Site::GuardFault, &L, false));
            let before = mine();
            recording();
            recording();
            let flushed = mine_extra();
            drop(take(Site::GuardFault, &L, false));
            (before, flushed, mine(), mine_extra())
        });
        rx.recv().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        drop(held);
        let (before, flushed, after, later) = thread.join().unwrap();
        let fault = Site::GuardFault as usize;
        let waited = before[fault][Role::Unclassified as usize][WAIT_NS];
        assert!(waited >= 10_000_000, "{waited}");
        assert_eq!(flushed[TOOK_TITLE_ROLE], 1);
        assert_eq!(flushed[WAITED_BEFORE_TITLE_ROLE_NS], waited);
        assert_eq!(after[fault][Role::Recording as usize][TAKEN], 1);
        assert_eq!(later, [0; EXTRAS]);
    }

    /// The kinds told apart add up to the "other threads" of before.
    #[test]
    fn other_kinds_add_up_to_other_threads() {
        let mut c: Counts = [[[0; FIELDS]; ROLES]; SITES];
        for (i, s) in c.iter_mut().enumerate() {
            for (j, r) in s.iter_mut().enumerate() {
                r[WAIT_NS] = (i * 10 + j) as u64;
            }
        }
        let parts: u64 = OTHER_ROLES.iter().map(|&r| lock_role_total(&c, Lock::Guards, &[r], WAIT_NS)).sum();
        assert_eq!(parts, lock_role_total(&c, Lock::Guards, &OTHER_ROLES, WAIT_NS));
        let all = [Role::Submitting, Role::TitleSubmitter, Role::Relay];
        let total: u64 = (0..6).flat_map(|s| c[s].iter().map(|r| r[WAIT_NS])).sum();
        assert_eq!(lock_role_total(&c, Lock::Guards, &all, WAIT_NS) + parts, total);
        assert_eq!(
            site_role_total(&c, Site::GuardFault, &OTHER_ROLES, WAIT_NS),
            (3..7).map(|j| c[2][j][WAIT_NS]).sum::<u64>()
        );
    }

    #[test]
    fn the_relay_is_known_by_its_thread_name() {
        let r = std::thread::Builder::new()
            .name("victory-relay".into())
            .spawn(|| (role(true), role(false), role(true)))
            .unwrap()
            .join()
            .unwrap();
        // Not looked up in the handler before it was known.
        assert_eq!(r, (Role::Unclassified, Role::Relay, Role::Relay));
    }

    /// A thread waiting while another holds the lock: its wait, the queue
    /// it joined, and the holder's hold are counted.
    #[test]
    fn contention_is_counted() {
        static L: Mutex<()> = Mutex::new(());
        let _ = mine();
        let held = take(Site::JournalOther, &L, false);
        let waiter = std::thread::spawn(|| {
            let _ = mine();
            drop(take(Site::JournalOther, &L, false));
            mine()
        });
        // Let the waiter block.
        std::thread::sleep(std::time::Duration::from_millis(20));
        drop(held);
        let theirs = waiter.join().unwrap();
        let w = theirs[Site::JournalOther as usize][Role::TitleOther as usize];
        assert_eq!(w[TAKEN], 1);
        assert!(w[WAIT_NS] >= 10_000_000, "{w:?}");
        assert_eq!(w[WAITS_OVER_100US], 1);
        let mine = mine()[Site::JournalOther as usize][role(false) as usize];
        assert!(mine[HOLD_NS] >= 10_000_000);
    }

    /// `VIEWS` taken while this thread holds `GUARDS` is told apart.
    #[test]
    fn views_under_guards_are_told_apart() {
        static G: Mutex<()> = Mutex::new(());
        assert_eq!(views_site(), Site::ViewsOther);
        let g = take(Site::GuardCheck, &G, false);
        assert_eq!(views_site(), Site::ViewsUnderGuards);
        drop(g);
        assert_eq!(views_site(), Site::ViewsOther);
    }

    /// The cost of counting an uncontended take and release, against a
    /// plain lock. Printed with `--nocapture`; the bound only catches a
    /// gross regression (a slow virtual machine must not fail it).
    #[test]
    fn counting_costs_little() {
        static B: Mutex<u64> = Mutex::new(0);
        const N: u32 = 200_000;
        let time = |f: &dyn Fn()| {
            let t = Instant::now();
            for _ in 0..N {
                f();
            }
            t.elapsed().as_nanos() as f64 / f64::from(N)
        };
        let plain = || *B.lock().unwrap() += 1;
        let counted = || *take(Site::JournalOther, &B, false) += 1;
        // Warm up, then the best of three of each.
        let _ = (time(&plain), time(&counted));
        let p = (0..3).map(|_| time(&plain)).fold(f64::MAX, f64::min);
        let c = (0..3).map(|_| time(&counted)).fold(f64::MAX, f64::min);
        println!("lock take and release: plain {p:.0} ns, counted {c:.0} ns, cost {:.0} ns", c - p);
        assert!(c - p < 2_000.0, "counting a take costs {:.0} ns", c - p);
        let _ = mine();
    }
}
