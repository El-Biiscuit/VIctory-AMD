//! Which title memory the title wrote since a host copy took it (decision
//! 0007, "Copies of title memory"). The runtime reserves title GPU memory
//! with write watching (`victory-hle` xmem), so the kernel keeps a dirty bit
//! per 4 KiB page; asking for a range's written pages also clears them. Host
//! copies of the same memory can overlap (nested buffers, textures in
//! buffers), so what one asks is kept here for the others: a journal of
//! each page's last write, numbered by the asking.
//!
//! Physical-page views (xmem's sections) cannot be write-watched. There a
//! copy leaves each 64 KiB block it took read-only, and our vectored handler
//! takes the first write to it: it notes the block as written and gives the
//! write access back. The runtime tells us when it maps or unmaps views
//! ([`title_memory_remapped`]), which resets their protection. Blocks found
//! wholly uncommitted are skipped while they remain so; one query can
//! confirm a whole region, including ordinary reservations between views.
//!
//! Writes into physical pages through another view are seen too: the
//! runtime tells us the views of the pages it wrote itself ([`writing`],
//! xmem's `write_physical` and DirectStorage's page destinations), and a
//! write by the title through any view of a guarded page faults like one
//! at the guarded address (the guard makes every view of the page
//! read-only, [`open`] gives them all back; not a page of more than
//! [`MANY_VIEWS`] views). A
//! system call writing into a guarded block fails (none is known to).

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::sync::Mutex;

/// The unit tracked and copied: 64 KiB, the console's page (the kernel's
/// 4 KiB pages fold into it). Finer units made the checks of the world's
/// big buffers cost more than the copies they saved.
pub(crate) const PAGE: usize = 64 << 10;
/// The kernel's page, the unit of its answers.
const KERNEL_PAGE: usize = 4096;

struct Journal {
    /// Numbers the askings; 0 is "never".
    asked: u64,
    /// Page number → the asking that last found it written.
    written: BTreeMap<usize, u64>,
    /// Room for the kernel's answer.
    found: Vec<usize>,
    /// Page ranges asked ahead for a submission's refreshes ([`ask_ahead`])
    /// and the asking that did: `writes` answers for a range inside one
    /// from the journal, without another kernel call.
    ahead: Vec<(usize, usize)>,
    ahead_asked: u64,
    /// The ahead asking stands for a whole submission ([`ask_once`]): no
    /// other ahead asking is made, and `ahead` stays, until
    /// [`ask_once_end`].
    kept: bool,
}

const EMPTY_JOURNAL: Journal = Journal {
    asked: 0,
    written: BTreeMap::new(),
    found: Vec::new(),
    ahead: Vec::new(),
    ahead_asked: 0,
    kept: false,
};
static JOURNAL: Mutex<Journal> = Mutex::new(EMPTY_JOURNAL);

/// Ranges of one submission closer than this are asked about together:
/// walking the pages between them costs less than another kernel call (a
/// call that resets written pages costs 30-70 microseconds whatever the
/// range).
const AHEAD_GAP: usize = (8 << 20) / PAGE;
/// The most one asking covers (its answer needs 8 bytes per 4 KiB page).
const AHEAD_MAX: usize = (1 << 30) / PAGE;

/// Asks the kernel about many ranges `(start, end)` at once, before the
/// [`writes`] of each: ranges close together are one asking, and `writes`
/// then answers from the journal until [`ask_ahead_done`]. The world's
/// submissions refresh thousands of host copies (6,664 askings a frame,
/// 456 ms, in run 14). Ranges that are not write-watched, or lie alone, are
/// left to `writes`.
pub(crate) fn ask_ahead(ranges: impl Iterator<Item = (usize, usize)>) {
    let mut j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    // The submission's one asking stands ([`ask_once`]): what it does not
    // hold is left to `writes`. Avoid collecting and sorting the ranges
    // every time the submission flushes a batch.
    if j.kept {
        return;
    }
    let pieces = watched_pieces(ranges);
    if pieces.len() >= 2 {
        ask_groups(&mut j, &pieces);
    }
}

fn watched_pieces(ranges: impl Iterator<Item = (usize, usize)>) -> Vec<(usize, usize)> {
    let mut pieces: Vec<_> = ranges
        .filter(|&(start, end)| start < end)
        .map(|(start, end)| (start / PAGE, end.div_ceil(PAGE)))
        .collect();
    pieces.sort_unstable();
    pieces
}

fn ask_groups(j: &mut Journal, pieces: &[(usize, usize)]) {
    j.asked += 1;
    j.ahead_asked = j.asked;
    j.ahead.clear();
    let mut i = 0;
    while i < pieces.len() {
        let first = pieces[i].0;
        let mut last = pieces[i].1;
        let mut k = i + 1;
        while k < pieces.len()
            && pieces[k].0 <= last + AHEAD_GAP
            && last.max(pieces[k].1) - first <= AHEAD_MAX
        {
            last = last.max(pieces[k].1);
            k += 1;
        }
        ask_run(j, &pieces[i..k]);
        i = k;
    }
}

/// Since the last frame line: the kernel askings that took a millisecond
/// or more, their microseconds, and the longest asking's. An asking walks
/// page tables, some ten microseconds for most ranges; in run 75 the 2,644
/// askings of a frame took 219 ms in the lines where the pass over the
/// heap's views ran long, 25-50 ms in the others. These numbers say
/// whether a few askings wait for something or all of them are slow.
#[cfg(feature = "diagnostics")]
static SLOW: [std::sync::atomic::AtomicU64; 3] = [const { std::sync::atomic::AtomicU64::new(0) }; 3];

#[cfg(feature = "diagnostics")]
fn slow(us: u64) {
    use std::sync::atomic::Ordering::Relaxed;
    if us >= 1000 {
        SLOW[0].fetch_add(1, Relaxed);
        SLOW[1].fetch_add(us, Relaxed);
    }
    SLOW[2].fetch_max(us, Relaxed);
}

/// [`SLOW`], taken.
#[cfg(feature = "diagnostics")]
pub(crate) fn slow_asks() -> [u64; 3] {
    [0, 1, 2].map(|i| SLOW[i].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// The submission's refreshes are done: `writes` asks the kernel again.
pub(crate) fn ask_ahead_done() {
    let mut j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    if !j.kept {
        j.ahead.clear();
    }
}

/// Whether a submission asks the kernel once, before its first refresh,
/// about every range its lists refresh (`VICTORY_ASK_ONCE=1`; off by
/// default).
///
/// A submission's refreshes are done in groups, between the title's own
/// buffer copies (each copy flushes the refreshes recorded before it), and
/// every group asked the kernel again: 2,644 askings a frame, 70 a
/// submission. With the switch the journal answers all of them from one
/// asking made when the submission starts.
///
/// What changes: memory the title's other threads write while the
/// submission is being prepared is seen by the next submission's asking,
/// not by this one's later groups. What this submission's lists read was
/// written before they were submitted, so before the asking. Our own
/// writes into title memory while preparing are told to the journal
/// ([`wrote`]) or end the asking ([`ask_once_end`]).
pub(crate) fn once() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    // On by default since run 81 (with card buffers: 375-450 ms a frame
    // against 573-647, the picture unchanged over four minutes);
    // `VICTORY_ASK_ONCE=0` asks per range, as before.
    *ON.get_or_init(|| std::env::var_os("VICTORY_ASK_ONCE").is_none_or(|v| v != "0"))
}

/// With [`once`]: asks about `ranges` (everything a submission's lists
/// refresh) as [`ask_ahead`] does, and lets that asking stand until
/// [`ask_once_end`].
pub(crate) fn ask_once(ranges: &[(usize, usize)]) {
    if !once() {
        return;
    }
    let mut pieces = watched_pieces(ranges.iter().copied());
    // Distinct host copies can ask for the same guest pages. In particular,
    // don't retry a failed GetWriteWatch for every copy with that span.
    pieces.dedup();
    let mut j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    let j = &mut *j;
    ask_groups(j, &pieces);
    // `ask_groups` leaves a range that lies alone to `writes`, which asks
    // once per flush: with one asking for the submission that was every
    // such range asked again at each of its 70 flushes (run 76: 2,300
    // askings a frame with the switch on, as without it). They are asked
    // here, once; what is not write-watched stays with `writes`.
    for (first, last) in pieces {
        if !j.ahead.iter().any(|&(a, b)| a <= first && last <= b) && ask_kernel(j, first, last) {
            j.ahead.push((first, last));
        }
    }
    j.kept = !j.ahead.is_empty();
}

/// The submission's one asking stands no longer: its refreshes are done,
/// or title memory was written in a way the journal was not told.
pub(crate) fn ask_once_end() {
    let mut j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    if j.kept {
        j.kept = false;
        j.ahead.clear();
    }
}

/// We wrote title memory `[at, at + len)` while a submission's one asking
/// stands (the title's buffer copies, done at submission): its pages count
/// as written after that asking, so the refreshes that follow copy them.
pub(crate) fn wrote(at: usize, len: usize) {
    let mut j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    if !j.kept || len == 0 {
        return;
    }
    j.asked += 1;
    let now = j.asked;
    for page in at / PAGE..at.saturating_add(len).div_ceil(PAGE) {
        j.written.insert(page, now);
    }
}

/// One kernel asking for `pieces` (page ranges in order); by halves if it
/// fails, as a run across two reservations or into memory that is not
/// write-watched does. A piece alone is left to [`writes`].
fn ask_run(j: &mut Journal, pieces: &[(usize, usize)]) {
    if pieces.len() < 2 {
        return;
    }
    let first = pieces[0].0;
    let last = pieces.iter().map(|p| p.1).max().unwrap_or(first);
    if ask_kernel(j, first, last) {
        j.ahead.push((first, last));
        return;
    }
    let (a, b) = pieces.split_at(pieces.len() / 2);
    ask_run(j, a);
    ask_run(j, b);
}

/// Asks the kernel for the written pages of pages `[first, last)` and notes
/// them in the journal under the ahead asking; false (and nothing changed)
/// if the range is not write-watched memory of one reservation.
fn ask_kernel(j: &mut Journal, first: usize, last: usize) -> bool {
    use windows_sys::Win32::System::Memory::GetWriteWatch;
    const WRITE_WATCH_FLAG_RESET: u32 = 1;
    let mut count = (last - first) * PAGE / KERNEL_PAGE;
    j.found.resize(count.max(j.found.len()), 0);
    let mut granularity = 0;
    #[cfg(feature = "diagnostics")]
    let started = std::time::Instant::now();
    // SAFETY: the answer buffer holds `count` entries.
    let failed = unsafe {
        GetWriteWatch(
            WRITE_WATCH_FLAG_RESET,
            (first * PAGE) as *const _,
            (last - first) * PAGE,
            j.found.as_mut_ptr().cast(),
            &mut count,
            &mut granularity,
        )
    } != 0;
    if failed {
        return false;
    }
    #[cfg(feature = "diagnostics")]
    {
        use crate::copystat::{C, add};
        add(C::Watches, 1);
        add(C::WatchUs, started.elapsed().as_micros() as u64);
        slow(started.elapsed().as_micros() as u64);
    }
    note_kernel_writes(j, count, j.ahead_asked);
    true
}

fn note_kernel_writes(j: &mut Journal, count: usize, now: u64) {
    let mut previous = None;
    for &address in &j.found[..count] {
        let page = address / PAGE;
        if previous != Some(page) {
            j.written.insert(page, now);
            previous = Some(page);
        }
    }
}

/// What [`writes`] would say of `[start, end)` from the journal alone, if
/// the range lies in one asked ahead ([`ask_ahead`], [`ask_once`]): the
/// pages written since tracking began and the asking that last found each
/// written, with the number of the asking that stands (as [`writes`] gives
/// it). `None` if the kernel would have to be asked.
pub(crate) fn asked(start: usize, end: usize) -> Option<(u64, Vec<(usize, u64)>)> {
    let (first, last) = (start / PAGE, end.div_ceil(PAGE));
    let j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    j.ahead
        .iter()
        .any(|&(a, b)| a <= first && last <= b)
        .then(|| (j.ahead_asked, j.written.range(first..last).map(|(&p, &w)| (p, w)).collect()))
}

/// The pages of `[start, end)` (whole pages) as last written, for pages
/// written since tracking began: `(page number, asking)` in page order, and
/// this asking's number, which is higher than any in the list but those the
/// kernel just reported. `None` if the memory is not write-watched (nor,
/// with `guarded`, view memory, which is then guarded).
pub(crate) fn writes(start: usize, end: usize, guarded: bool) -> Option<(u64, Vec<(usize, u64)>)> {
    use windows_sys::Win32::System::Memory::GetWriteWatch;
    const WRITE_WATCH_FLAG_RESET: u32 = 1;
    let (first, last) = (start / PAGE, end.div_ceil(PAGE));
    let mut j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    let j = &mut *j;
    // Asked ahead with the submission's other ranges: the journal has it.
    if j.ahead.iter().any(|&(a, b)| a <= first && last <= b) {
        return Some((
            j.ahead_asked,
            j.written
                .range(first..last)
                .map(|(&p, &w)| (p, w))
                .collect(),
        ));
    }
    let mut count = (last - first) * PAGE / KERNEL_PAGE;
    j.found.resize(count.max(j.found.len()), 0);
    let mut granularity = 0;
    #[cfg(feature = "diagnostics")]
    let started = std::time::Instant::now();
    // SAFETY: the answer buffer holds `count` entries.
    let failed = unsafe {
        GetWriteWatch(
            WRITE_WATCH_FLAG_RESET,
            (first * PAGE) as *const _,
            (last - first) * PAGE,
            j.found.as_mut_ptr().cast(),
            &mut count,
            &mut granularity,
        )
    } != 0;
    let guarded = if failed {
        count = 0;
        if !guarded {
            return None;
        }
        #[cfg(feature = "diagnostics")]
        let guarding = std::time::Instant::now();
        let written = guard(first * PAGE, last * PAGE);
        #[cfg(feature = "diagnostics")]
        crate::copystat::add(crate::copystat::C::GuardUs, guarding.elapsed().as_micros() as u64);
        written?
    } else {
        Vec::new()
    };
    #[cfg(feature = "diagnostics")]
    {
        use crate::copystat::{C, add};
        add(C::Watches, 1);
        add(C::WatchUs, started.elapsed().as_micros() as u64);
        slow(started.elapsed().as_micros() as u64);
    }
    j.asked += 1;
    let now = j.asked;
    note_kernel_writes(j, count, now);
    for page in guarded {
        j.written.insert(page, now);
    }
    Some((
        now,
        j.written
            .range(first..last)
            .map(|(&p, &w)| (p, w))
            .collect(),
    ))
}

/// Statistics of the guards' protection changes (`VirtualProtect`) and of
/// the waits around them: diagnostics builds and tests only, and nothing
/// they count changes what the guards do. `VICTORY_PROTECT_STATS=0` leaves
/// out the clock readings and counting (diagnostics builds).
///
/// Every number is summed over all threads. What is spent on a thread that
/// is inside a submission ([`set_submitting`], from `copystat`) is also
/// summed apart: that part holds the submission lock, the frame waits for
/// it. The rest runs on the title's threads (faults, its kernel reads) or
/// ours beside the submissions (the relay, texture refreshes).
#[cfg(any(test, feature = "diagnostics"))]
pub(crate) mod stats {
    use super::{Change, Wait};
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    /// The kinds of [`Change`].
    pub(crate) const CHANGES: usize = 7;
    /// Per change: calls, blocks, failed calls, microseconds, microseconds
    /// on a submitting thread, and runs of contiguous blocks (what the
    /// calls could have been, for the kinds that make one call per block).
    pub(crate) const CALLS: usize = 0;
    pub(crate) const BLOCKS: usize = 1;
    pub(crate) const FAILED: usize = 2;
    pub(crate) const US: usize = 3;
    pub(crate) const US_SUBMITTING: usize = 4;
    pub(crate) const RUNS: usize = 5;
    pub(crate) const FIELDS: usize = 6;
    /// Beside them: write faults the handler took and their microseconds;
    /// per [`Wait`], the lock waits and their microseconds; the checks that
    /// guarded, their calls, and the calls joining runs across blocks
    /// already guarded would have made.
    pub(crate) const FAULTS: usize = 0;
    pub(crate) const FAULT_US: usize = 1;
    pub(crate) const WAITS: usize = 2; // then US of it, per Wait: 2..8
    pub(crate) const CHECKS: usize = 8;
    pub(crate) const CHECK_CALLS: usize = 9;
    pub(crate) const CHECK_JOINED: usize = 10;
    pub(crate) const OTHERS: usize = 11;

    static CHANGE: [[AtomicU64; FIELDS]; CHANGES] = [const { [const { AtomicU64::new(0) }; FIELDS] }; CHANGES];
    static OTHER: [AtomicU64; OTHERS] = [const { AtomicU64::new(0) }; OTHERS];

    thread_local! {
        /// This thread is inside a submission.
        static SUBMITTING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    #[cfg(test)]
    thread_local! {
        /// The same counts for this thread alone, for tests (other tests
        /// run on other threads meanwhile).
        static MINE: std::cell::RefCell<([[u64; FIELDS]; CHANGES], [u64; OTHERS])> =
            const { std::cell::RefCell::new(([[0; FIELDS]; CHANGES], [0; OTHERS])) };
    }

    pub(super) fn on() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("VICTORY_PROTECT_STATS").is_none_or(|v| v != "0"))
    }

    fn add(change: Change, field: usize, n: u64) {
        CHANGE[change as usize][field].fetch_add(n, Relaxed);
        #[cfg(test)]
        MINE.with(|m| m.borrow_mut().0[change as usize][field] += n);
    }

    fn add_other(field: usize, n: u64) {
        OTHER[field].fetch_add(n, Relaxed);
        #[cfg(test)]
        MINE.with(|m| m.borrow_mut().1[field] += n);
    }

    /// The thread enters or leaves a submission (`copystat`).
    pub(crate) fn set_submitting(on: bool) {
        SUBMITTING.with(|s| s.set(on));
    }

    pub(super) fn timed(change: Change, blocks: usize, call: impl FnOnce() -> bool) -> bool {
        if !on() {
            return call();
        }
        let started = std::time::Instant::now();
        let done = call();
        let us = started.elapsed().as_micros() as u64;
        add(change, CALLS, 1);
        add(change, BLOCKS, blocks as u64);
        add(change, US, us);
        if SUBMITTING.with(|s| s.get()) {
            add(change, US_SUBMITTING, us);
        }
        if !done {
            add(change, FAILED, 1);
        }
        done
    }

    /// Runs of contiguous blocks in `blocks` (in any order, repeats
    /// allowed).
    pub(crate) fn runs_of(mut blocks: Vec<usize>) -> usize {
        blocks.sort_unstable();
        blocks.dedup();
        blocks.windows(2).filter(|w| w[1] != w[0] + 1).count() + usize::from(!blocks.is_empty())
    }

    /// `blocks` were changed one call each for `change`: how many runs
    /// they make.
    pub(super) fn runs(change: Change, blocks: Vec<usize>) {
        if on() && !blocks.is_empty() {
            add(change, RUNS, runs_of(blocks) as u64);
        }
    }

    /// How many calls the runs `guarded` (in order) need when two runs
    /// with only guarded blocks between them (`all_guarded(from, to)`) are
    /// joined into one.
    pub(crate) fn joined_calls(guarded: &[(usize, usize)], all_guarded: impl Fn(usize, usize) -> bool) -> usize {
        let mut calls = 0;
        let mut end = None;
        for &(from, to) in guarded {
            match end {
                Some(e) if e <= from && all_guarded(e, from) => {}
                _ => calls += 1,
            }
            end = Some(to);
        }
        calls
    }

    /// A check guarded the runs `guarded`.
    pub(super) fn check_guarded(guarded: &[(usize, usize)], all_guarded: impl Fn(usize, usize) -> bool) {
        if !on() || guarded.is_empty() {
            return;
        }
        add_other(CHECKS, 1);
        add_other(CHECK_CALLS, guarded.len() as u64);
        add_other(CHECK_JOINED, joined_calls(guarded, all_guarded) as u64);
    }

    /// Takes the guard lock with `lock`, counting the wait.
    pub(super) fn waited<T>(who: Wait, lock: impl FnOnce() -> T) -> T {
        if !on() {
            return lock();
        }
        let started = std::time::Instant::now();
        let held = lock();
        let at = WAITS + 2 * who as usize;
        add_other(at, 1);
        add_other(at + 1, started.elapsed().as_micros() as u64);
        held
    }

    /// A write fault in the handler, from [`Fault::begin`] to its drop.
    pub(super) struct Fault(Option<std::time::Instant>);

    impl Fault {
        pub(super) fn begin() -> Fault {
            Fault(on().then(std::time::Instant::now))
        }
    }

    impl Drop for Fault {
        fn drop(&mut self) {
            if let Some(started) = self.0 {
                add_other(FAULTS, 1);
                add_other(FAULT_US, started.elapsed().as_micros() as u64);
            }
        }
    }

    /// The counts since the last call, reset.
    pub(crate) fn take() -> ([[u64; FIELDS]; CHANGES], [u64; OTHERS]) {
        (
            CHANGE.each_ref().map(|c| c.each_ref().map(|v| v.swap(0, Relaxed))),
            OTHER.each_ref().map(|v| v.swap(0, Relaxed)),
        )
    }

    /// This thread's counts since the last call, reset (tests).
    #[cfg(test)]
    pub(crate) fn mine() -> ([[u64; FIELDS]; CHANGES], [u64; OTHERS]) {
        MINE.with(|m| std::mem::replace(&mut *m.borrow_mut(), ([[0; FIELDS]; CHANGES], [0; OTHERS])))
    }

    #[cfg(test)]
    mod tests {
        use super::{joined_calls, runs_of};

        #[test]
        fn runs_count_contiguous_blocks() {
            assert_eq!(runs_of(vec![]), 0);
            assert_eq!(runs_of(vec![7]), 1);
            assert_eq!(runs_of(vec![3, 4, 5]), 1);
            assert_eq!(runs_of(vec![5, 3, 4, 9, 10, 4]), 2);
            assert_eq!(runs_of(vec![1, 3, 5]), 3);
            assert_eq!(runs_of(vec![usize::MAX - 1, usize::MAX]), 1);
        }

        #[test]
        fn runs_join_only_across_guarded_blocks() {
            let all = |_: usize, _: usize| true;
            let none = |a: usize, b: usize| a == b;
            assert_eq!(joined_calls(&[], all), 0);
            assert_eq!(joined_calls(&[(0, 2), (5, 6), (9, 12)], all), 1);
            // Touching runs join with nothing between them.
            assert_eq!(joined_calls(&[(0, 2), (2, 4), (7, 8)], none), 2);
            assert_eq!(joined_calls(&[(0, 2), (5, 6), (9, 12)], none), 3);
            // Blocks 2..5 guarded, 6..9 not.
            let some = |a: usize, b: usize| b <= 5 || a >= 9;
            assert_eq!(joined_calls(&[(0, 2), (5, 6), (9, 12)], some), 2);
        }
    }
}

/// Guarded blocks of view memory.
const BLOCK: usize = PAGE;

/// Clean 64 KiB blocks, stored as disjoint, non-adjacent half-open runs.
/// A cached buffer can cover thousands of consecutive protected blocks.
#[derive(Default)]
struct CleanBlocks(BTreeMap<usize, usize>);

impl CleanBlocks {
    fn insert(&mut self, mut start: usize, mut end: usize) {
        if start >= end {
            return;
        }
        if let Some((&before, &stop)) = self.0.range(..=start).next_back()
            && stop >= start
        {
            if stop >= end {
                return;
            }
            start = before;
        }
        while let Some((&next, &stop)) = self.0.range(start..=end).next() {
            self.0.remove(&next);
            end = end.max(stop);
        }
        self.0.insert(start, end);
    }

    fn remove(&mut self, block: usize) {
        let Some((&start, &end)) = self.0.range(..=block).next_back() else {
            return;
        };
        if block >= end {
            return;
        }
        if start == block {
            self.0.remove(&start);
        } else {
            self.0.insert(start, block);
        }
        if block + 1 < end {
            self.0.insert(block + 1, end);
        }
    }

    /// Snapshot just the gaps, so guarding them may update this index.
    /// A fully clean range needs one tree lookup and no allocation.
    fn gaps(&self, first: usize, last: usize) -> Vec<Range<usize>> {
        let mut cursor = first;
        if let Some((_, &end)) = self.0.range(..=first).next_back() {
            cursor = cursor.max(end);
        }
        let mut gaps = Vec::new();
        if cursor < last {
            for (&start, &end) in self.0.range(cursor..last) {
                if cursor < start {
                    gaps.push(cursor..start);
                }
                cursor = end;
            }
            if cursor < last {
                gaps.push(cursor..last);
            }
        }
        gaps
    }
}

struct Guards {
    /// Blocks we made read-only.
    protected: BTreeSet<usize>,
    /// Of those, the ones not written since a copy took them.
    clean: CleanBlocks,
    /// Blocks already copied as zeros. While they remain wholly uncommitted,
    /// the copy can skip hundreds of MB of failing kernel reads per world frame.
    absent: BTreeSet<usize>,
    /// Blocks whose physical page was guarded together with its other views
    /// (those views' blocks are in it too): a write to one opens them all.
    /// A write range can be huge (`fread`), so only these are looked up.
    aliased: BTreeSet<usize>,
}

static GUARDS: Mutex<Guards> = Mutex::new(Guards {
    protected: BTreeSet::new(),
    clean: CleanBlocks(BTreeMap::new()),
    absent: BTreeSet::new(),
    aliased: BTreeSet::new(),
});

fn guards() -> std::sync::MutexGuard<'static, Guards> {
    GUARDS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Who waits for the guard lock, for the statistics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wait {
    /// A check of a copy's range ([`guard`]).
    Check,
    /// The handler of a write to a guarded block (the title's thread).
    Fault,
    /// A write the runtime or we tell of ([`writing`]).
    Write,
}

/// [`guards`], with the wait for the lock counted.
fn guard_lock(who: Wait) -> std::sync::MutexGuard<'static, Guards> {
    #[cfg(any(test, feature = "diagnostics"))]
    {
        stats::waited(who, guards)
    }
    #[cfg(not(any(test, feature = "diagnostics")))]
    {
        let _ = who;
        guards()
    }
}

/// What a protection change is for, for the statistics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    /// A check guarding blocks a copy has taken, one call per run.
    Guard = 0,
    /// The same, block by block after the run's call failed.
    GuardRetry,
    /// Another view of pages just guarded ([`protect_alias`]).
    Alias,
    /// Opening a block for a write that faulted (the title's thread).
    OpenFault,
    /// Opening for a write told ([`writing`]: the runtime's kernel reads,
    /// our writes into title memory).
    OpenWrite,
    /// Opening the other views of a block opened ([`open`]).
    OpenAlias,
    /// Opening blocks of which a new view was mapped ([`views_mapped`]).
    OpenMapped,
}

/// One `VirtualProtect` over `blocks` blocks: `call` makes it and says
/// whether it succeeded. With statistics on, it is counted and timed;
/// nothing else changes.
#[inline]
fn protect_call(change: Change, blocks: usize, call: impl FnOnce() -> bool) -> bool {
    #[cfg(any(test, feature = "diagnostics"))]
    {
        stats::timed(change, blocks, call)
    }
    #[cfg(not(any(test, feature = "diagnostics")))]
    {
        let _ = (change, blocks);
        call()
    }
}

/// For view memory: the pages of `[start, end)`'s blocks written since
/// they were last guarded (all of a block's pages), guarding them again
/// before the caller copies them. `None` if the memory is not a view.
fn guard(start: usize, end: usize) -> Option<Vec<usize>> {
    // A range whose first block the guards hold already (read-only by our
    // hand, or noted absent) is a view: no question to Windows. The
    // question walks the page tables to the end of the pages that share the
    // first one's state, and a guarded view is one long run of read-only
    // pages: 0.6 ms for the title's 174 MB heap, 16 times a frame, and most
    // of the 68 ms a frame of "guards" (run 58).
    // `VICTORY_GUARD_ASK=1` asks every time, as before.
    static ASK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let known = !*ASK.get_or_init(|| std::env::var_os("VICTORY_GUARD_ASK").is_some_and(|v| v == "1")) && {
        let g = guards();
        let first = start / BLOCK;
        g.protected.contains(&first) || g.absent.contains(&first)
    };
    if !known && !is_view(start, end) {
        return None;
    }
    install();
    let mut g = guard_lock(Wait::Check);
    let mut written = Vec::new();
    let (first, last) = (start / BLOCK, end.div_ceil(BLOCK));
    let mut uncommitted_until = first;
    // The runs this check guards, for the statistics: how many calls a
    // join across blocks already guarded would have made.
    #[cfg(any(test, feature = "diagnostics"))]
    let mut guarded: Vec<(usize, usize)> = Vec::new();
    #[cfg_attr(not(any(test, feature = "diagnostics")), allow(unused_mut))]
    let mut protect = |g: &mut Guards, from: usize, to: usize| {
        #[cfg(any(test, feature = "diagnostics"))]
        guarded.push((from, to));
        protect(g, from, to);
    };
    for gap in g.clean.gaps(first, last) {
        let mut run = None;
        for block in gap.clone() {
            if g.absent.contains(&block) {
                // Ordinary commits need no runtime mapping notification. Reuse
                // this query for the following blocks in the same region.
                if block >= uncommitted_until {
                    uncommitted_until = uncommitted_end(block);
                }
                if block < uncommitted_until {
                    if let Some(from) = run.take() {
                        protect(&mut g, from, block);
                    }
                    continue;
                }
                g.absent.remove(&block);
            }
            written.extend(block * BLOCK / PAGE..(block + 1) * BLOCK / PAGE);
            if g.protected.contains(&block) {
                // Written through the runtime, still read-only.
                g.clean.insert(block, block + 1);
            } else {
                run.get_or_insert(block);
                continue;
            }
            if let Some(from) = run.take() {
                protect(&mut g, from, block);
            }
        }
        if let Some(from) = run.take() {
            protect(&mut g, from, gap.end);
        }
    }
    // Between two runs: guarded if every block there is clean or, not
    // clean, still read-only (a few blocks at most; no walk of the set).
    #[cfg(any(test, feature = "diagnostics"))]
    stats::check_guarded(&guarded, |a, b| {
        g.clean.gaps(a, b).into_iter().flatten().all(|block| g.protected.contains(&block))
    });
    Some(written)
}

/// Whether `[start, end)` is view memory. The title's views lie in
/// placeholders, mapped and unmapped block by block as it streams, so a
/// range can begin in a block that is not mapped right now: the first
/// pages of the range that are there decide. A range with nothing there
/// counts as a view (its blocks are noted absent, and looked at again when
/// the runtime maps them).
///
/// Looking at the first address only took such a range for "not a view":
/// its copy was marked not watched for good and copied whole at every
/// refresh, around its unmapped pages (run 54: 300 such ranges, 570 MB
/// copied a frame for them, 255 copies a frame reading around holes).
/// `VICTORY_GUARD_HOLES=0` looks at the first address only, as before.
fn is_view(start: usize, end: usize) -> bool {
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_MAPPED, MEMORY_BASIC_INFORMATION, VirtualQuery,
    };
    static HOLES: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let holes = *HOLES.get_or_init(|| std::env::var_os("VICTORY_GUARD_HOLES").is_none_or(|v| v != "0"));
    let mut at = start;
    loop {
        // SAFETY: plain data filled in by the query.
        let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: a valid out pointer of the right size.
        let n = unsafe { VirtualQuery(at as *const _, &mut info, size_of_val(&info)) };
        if n == 0 {
            return false;
        }
        if info.Type == MEM_MAPPED {
            return true;
        }
        // Committed memory that is no view (the title's heaps, ours), or
        // the answer as it was before.
        if info.State == MEM_COMMIT || !holes {
            return false;
        }
        // A hole (a placeholder, or free): on to what follows it.
        let next = (info.BaseAddress as usize).saturating_add(info.RegionSize);
        if next <= at || next >= end {
            return true;
        }
        at = next;
    }
}

/// Makes blocks `[from, to)` read-only and notes them guarded and clean;
/// block by block if they span views, leaving out any that fail, and
/// noting those not committed (unmapped) absent. The same physical pages
/// at every other view of them are made read-only too: a write through
/// one of those is this memory written, and only a fault can say so.
/// `VICTORY_VIEW_GUARD=0` leaves the other views as they are, as before.
fn protect(g: &mut Guards, from: usize, to: usize) {
    protect_here(g, from, to);
    if !view_guard() {
        return;
    }
    // The other views' blocks this guarded, for the statistics (one call
    // each now; runs of them could be one call).
    #[cfg(any(test, feature = "diagnostics"))]
    let mut aliases: Vec<usize> = Vec::new();
    for (base, bytes) in crate::inplace::views_between(from.saturating_mul(BLOCK), to.saturating_mul(BLOCK)) {
        let first = (base / BLOCK).max(from);
        let last = (base.saturating_add(bytes).div_ceil(BLOCK)).min(to);
        for block in first..last {
            let shown = crate::inplace::pages_of(block * BLOCK);
            // One view (nothing else to guard), or the page the title shows
            // wherever it has nothing (thousands of views): each view of
            // that one is guarded by itself.
            if shown.len() < 2 || shown.len() > MANY_VIEWS {
                continue;
            }
            #[cfg(feature = "diagnostics")]
            ALIASES[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            for alias in shown {
                let alias = alias / BLOCK;
                g.aliased.insert(alias);
                if alias != block && protect_alias(g, alias) {
                    #[cfg(any(test, feature = "diagnostics"))]
                    aliases.push(alias);
                }
            }
        }
    }
    #[cfg(any(test, feature = "diagnostics"))]
    stats::runs(Change::Alias, aliases);
}

/// Views of one page past which its views are not guarded together.
pub(crate) const MANY_VIEWS: usize = 64;

/// Whether all the views of a guarded page are guarded together
/// (`VICTORY_VIEW_GUARD`).
fn view_guard() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_VIEW_GUARD").is_none_or(|v| v != "0"))
}

/// Diagnostics: blocks guarded together with their other views, blocks
/// opened because another view of theirs was written, and blocks opened
/// because a view of theirs was mapped; since the start.
#[cfg(feature = "diagnostics")]
pub(crate) static ALIASES: [std::sync::atomic::AtomicU64; 3] = [const { std::sync::atomic::AtomicU64::new(0) }; 3];

/// Makes `block` read-only: another view of pages a copy has just taken
/// through its own view, so that a write through this one faults too. It is
/// not noted clean. The copies of title memory at this view have not taken
/// the pages: noted clean, the block would not be reported written at their
/// next check and they would keep the bytes from before the write (`guard`
/// reports a block that is read-only and not clean as written, then notes it
/// clean). A block that is clean stays so: had the pages been written since
/// its copies took them, `open` would have opened it.
/// Returns whether it made the block read-only now.
fn protect_alias(g: &mut Guards, block: usize) -> bool {
    use windows_sys::Win32::System::Memory::{PAGE_READONLY, VirtualProtect};
    if g.protected.contains(&block) {
        return false;
    }
    let mut old = 0;
    // SAFETY: title view memory; only its protection changes.
    let done = protect_call(Change::Alias, 1, || unsafe {
        VirtualProtect((block * BLOCK) as *const _, BLOCK, PAGE_READONLY, &mut old) != 0
    });
    if done {
        g.protected.insert(block);
    }
    done
}

/// [`protect`] for one range, without the other views of its pages.
fn protect_here(g: &mut Guards, from: usize, to: usize) {
    use windows_sys::Win32::System::Memory::{PAGE_READONLY, VirtualProtect};
    let set = |change: Change, from: usize, to: usize| {
        let mut old = 0;
        // SAFETY: title view memory; only its protection changes.
        protect_call(change, to - from, || unsafe {
            VirtualProtect(
                (from * BLOCK) as *const _,
                (to - from) * BLOCK,
                PAGE_READONLY,
                &mut old,
            ) != 0
        })
    };
    let note = |g: &mut Guards, from: usize, to: usize| {
        g.protected.extend(from..to);
        g.clean.insert(from, to);
    };
    if set(Change::Guard, from, to) {
        note(g, from, to);
        return;
    }
    for block in from..to {
        if set(Change::GuardRetry, block, block + 1) {
            note(g, block, block + 1);
        } else if block < uncommitted_end(block) {
            g.absent.insert(block);
        }
    }
}

/// The end (exclusive) of the whole uncommitted blocks starting at `block`.
fn uncommitted_end(block: usize) -> usize {
    use windows_sys::Win32::System::Memory::{
        MEM_FREE, MEM_RESERVE, MEMORY_BASIC_INFORMATION, VirtualQuery,
    };
    // SAFETY: plain data filled in by the query.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a valid out pointer of the right size.
    let n = unsafe { VirtualQuery((block * BLOCK) as *const _, &mut info, size_of_val(&info)) };
    if n != 0 && matches!(info.State, MEM_RESERVE | MEM_FREE) {
        (info.BaseAddress as usize).saturating_add(info.RegionSize) / BLOCK
    } else {
        block
    }
}

/// Gives write access back to the guarded blocks of `[start, end)` and
/// notes them written; true if any was guarded. The same physical pages at
/// every other view of them are opened too: the write that faulted (or is
/// ours, `crate::host::copy_mapped`) goes through one view, and every
/// guard over the pages must copy them again.
fn open(g: &mut Guards, start: usize, end: usize, why: Change) -> bool {
    let opened = open_here(g, start, end, why);
    let any = !opened.is_empty();
    #[cfg(any(test, feature = "diagnostics"))]
    stats::runs(why, opened);
    if start >= end || g.aliased.is_empty() {
        return any;
    }
    // The other views' blocks this opens, for the statistics (one call
    // each now; runs of them could be one call).
    #[cfg(any(test, feature = "diagnostics"))]
    let mut others: Vec<usize> = Vec::new();
    let aliased: Vec<usize> = g.aliased.range(start / BLOCK..end.div_ceil(BLOCK)).copied().collect();
    for block in aliased {
        g.aliased.remove(&block);
        for alias in crate::inplace::pages_of(block * BLOCK) {
            if alias / BLOCK != block {
                let opened = open_here(g, alias, alias.saturating_add(BLOCK), Change::OpenAlias);
                if !opened.is_empty() {
                    #[cfg(feature = "diagnostics")]
                    ALIASES[1].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    #[cfg(any(test, feature = "diagnostics"))]
                    others.extend(opened);
                }
            }
            g.aliased.remove(&(alias / BLOCK));
        }
    }
    #[cfg(any(test, feature = "diagnostics"))]
    stats::runs(Change::OpenAlias, others);
    any
}

/// [`open`] for one range, without the other views of its pages; `why`
/// names the cause for the statistics. Returns the blocks it opened.
fn open_here(g: &mut Guards, start: usize, end: usize, why: Change) -> Vec<usize> {
    use windows_sys::Win32::System::Memory::{PAGE_READWRITE, VirtualProtect};
    let blocks: Vec<usize> = g
        .protected
        .range(start / BLOCK..end.div_ceil(BLOCK))
        .copied()
        .collect();
    for &block in &blocks {
        g.protected.remove(&block);
        g.clean.remove(block);
        let mut old = 0;
        // SAFETY: a block we made read-only.
        protect_call(why, 1, || unsafe {
            VirtualProtect((block * BLOCK) as *const _, BLOCK, PAGE_READWRITE, &mut old) != 0
        });
    }
    blocks
}

/// Before our own write into title memory `[at, at + len)` through the
/// kernel (which does not fault into our handler): opens its guarded blocks.
/// The runtime calls it (as `kernel_will_write`) before the title's kernel
/// reads into its own memory.
pub fn writing(at: usize, len: usize) {
    // The length is the title's (`fread`'s size times count, saturated).
    open(&mut guard_lock(Wait::Write), at, at.saturating_add(len), Change::OpenWrite);
    // A placed texture there changes with no descriptor written.
    crate::descriptor::host::remapped(at, len);
}

/// A producer finished filling these title bytes. Its pre-write notification
/// can already have been consumed while a DirectStorage worker was decoding.
/// No guard needs opening again: the write has finished.
pub fn title_bytes_written(bytes: &[u8]) {
    if !bytes.is_empty() {
        crate::descriptor::host::remapped(bytes.as_ptr() as usize, bytes.len());
    }
}

/// The runtime mapped a view of the physical bytes `[offset, offset + len)`
/// of `section` (`crate::inplace::title_memory_view`): the view is
/// writable, so the same bytes guarded at another view must be checked
/// again — and are, the pages noted written, which also has the next check
/// guard the new view (`protect`).
pub(crate) fn views_mapped(section: usize, offset: u64, len: usize) {
    if !view_guard() {
        return;
    }
    let ranges = crate::inplace::views_of_section(section, offset, len);
    if ranges.is_empty() {
        return;
    }
    let mut g = guards();
    #[cfg(any(test, feature = "diagnostics"))]
    let mut opened_all: Vec<usize> = Vec::new();
    for &(at, n) in &ranges {
        let opened = open_here(&mut g, at, at.saturating_add(n), Change::OpenMapped);
        if !opened.is_empty() {
            #[cfg(feature = "diagnostics")]
            ALIASES[2].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            #[cfg(any(test, feature = "diagnostics"))]
            opened_all.extend(opened);
        }
    }
    #[cfg(any(test, feature = "diagnostics"))]
    stats::runs(Change::OpenMapped, opened_all);
}

/// A direct read pins the virtual addresses it will read. A mapping change
/// announces the whole extent it can temporarily remove before waiting for
/// its readers, so new reads cannot starve it. Disjoint reads can proceed.
///
/// The runtime does not change a mapping in one step. To unmap one page of
/// a view it unmaps the whole view and maps what stays again
/// (`xmem.rs` `unmap`), and a view it maps is a reservation until the next
/// call commits it (`map`, then `commit`). For that moment pages the title
/// never gave up are "reserved, never committed", and a thread reading
/// them in place takes an access violation: run 71 in `Layout::detile`,
/// run 74 in the hash before it, both under `Staging::refresh_now`, which
/// had asked whether the texture was all mapped and been told yes. Asking
/// cannot help; not changing the mapping while such a read is under way
/// does.
struct Mapping {
    changing: Option<Range<usize>>,
    readers: Vec<Range<usize>>,
}

static MAPPING: Mutex<Mapping> = Mutex::new(Mapping {
    changing: None,
    readers: Vec::new(),
});
static WRITER: Mutex<()> = Mutex::new(());
static MAPPING_CHANGED: std::sync::Condvar = std::sync::Condvar::new();
const ALL_MEMORY: Range<usize> = 0..usize::MAX;

fn overlaps(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start.max(b.start) < a.end.min(b.end)
}

impl Mapping {
    fn changing(&self, range: &Range<usize>) -> bool {
        self.changing.as_ref().is_some_and(|w| overlaps(w, range))
    }

    fn pin(&mut self, range: Range<usize>) -> Reading {
        self.readers.push(range.clone());
        Reading(range)
    }
}

/// The only active change of physical mappings.
pub struct Remapping {
    _writer: std::sync::MutexGuard<'static, ()>,
    range: Range<usize>,
    previous: Vec<crate::inplace::PhysicalSpan>,
}

/// Protects a mapping change whose extent is unknown.
pub fn title_memory_remapping() -> Remapping {
    title_memory_remapping_range(ALL_MEMORY)
}

/// Protects every address that can be unmapped during the operation,
/// including whole old views that Windows temporarily removes.
pub fn title_memory_remapping_range(range: Range<usize>) -> Remapping {
    let writer = WRITER.lock().unwrap_or_else(|e| e.into_inner());
    let mut state = MAPPING.lock().unwrap_or_else(|e| e.into_inner());
    state.changing = Some(range.clone());
    drop(
        MAPPING_CHANGED
            .wait_while(state, |s| s.readers.iter().any(|r| overlaps(r, &range)))
            .unwrap_or_else(|e| e.into_inner()),
    );
    let previous = crate::inplace::physical_spans(&range);
    Remapping {
        _writer: writer,
        range,
        previous,
    }
}

impl Drop for Remapping {
    fn drop(&mut self) {
        let current = crate::inplace::physical_spans(&self.range);
        crate::inplace::physical_changes(&self.previous, &current, crate::writeback::remapped);
        MAPPING.lock().unwrap_or_else(|e| e.into_inner()).changing = None;
        MAPPING_CHANGED.notify_all();
    }
}

/// Keeps a direct read's virtual address range mapped.
pub struct Reading(Range<usize>);

impl Drop for Reading {
    fn drop(&mut self) {
        let mut state = MAPPING.lock().unwrap_or_else(|e| e.into_inner());
        let i = state
            .readers
            .iter()
            .position(|r| *r == self.0)
            .expect("active mapping reader");
        state.readers.swap_remove(i);
        if state.changing(&self.0) {
            MAPPING_CHANGED.notify_all();
        }
    }
}

/// For reading title memory in place: until what it returns is dropped,
/// the runtime changes no mapping, so memory found mapped stays mapped.
/// Not to be taken twice by one thread, nor held across anything that
/// maps title memory.
pub fn title_memory_reading() -> Reading {
    title_memory_reading_range(ALL_MEMORY)
}

/// Pins only the range the caller will read. Do not nest mapping guards.
pub fn title_memory_reading_range(range: Range<usize>) -> Reading {
    let state = MAPPING.lock().unwrap_or_else(|e| e.into_inner());
    MAPPING_CHANGED
        .wait_while(state, |s| s.changing(&range))
        .unwrap_or_else(|e| e.into_inner())
        .pin(range)
}

/// Pins a range immediately if available, for callers with a fallback.
pub(crate) fn title_memory_reading_now(range: Range<usize>) -> Option<Reading> {
    let mut state = match MAPPING.try_lock() {
        Ok(state) => state,
        Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return None,
    };
    (!state.changing(&range)).then(|| state.pin(range))
}

/// The runtime mapped or unmapped title memory `[at, at + len)`: what it
/// shows is new and read-write, so it is no longer guarded (nor known
/// unmapped).
pub fn title_memory_remapped(at: usize, len: usize) {
    // A placed texture there arrives, goes or changes with no descriptor
    // written.
    crate::descriptor::host::remapped(at, len);
    // The views the runtime told us of there are no longer what it shows.
    crate::inplace::views_gone(at, len);
    // And nothing stays in place over it.
    crate::inplace::title_memory_freeing(at, len);
    let mut g = guards();
    let range = at / BLOCK..at.saturating_add(len).div_ceil(BLOCK);
    let absent: Vec<usize> = g.absent.range(range.clone()).copied().collect();
    for block in absent {
        g.absent.remove(&block);
    }
    let blocks: Vec<usize> = g.protected.range(range).copied().collect();
    for block in blocks {
        g.protected.remove(&block);
        g.clean.remove(block);
    }
    let aliased: Vec<usize> = g.aliased.range(at / BLOCK..at.saturating_add(len).div_ceil(BLOCK)).copied().collect();
    for block in aliased {
        g.aliased.remove(&block);
    }
}

/// Registers the handler that takes writes to guarded blocks, once, ahead
/// of the crash reporter's.
fn install() {
    use windows_sys::Win32::Foundation::{EXCEPTION_ACCESS_VIOLATION, NTSTATUS};
    use windows_sys::Win32::System::Diagnostics::Debug::{
        AddVectoredExceptionHandler, EXCEPTION_CONTINUE_EXECUTION, EXCEPTION_CONTINUE_SEARCH,
        EXCEPTION_POINTERS,
    };
    unsafe extern "system" fn on_write(info: *mut EXCEPTION_POINTERS) -> i32 {
        // SAFETY: the system passes the exception being dispatched.
        let r = unsafe { &*(*info).ExceptionRecord };
        const WRITE: usize = 1;
        if r.ExceptionCode != EXCEPTION_ACCESS_VIOLATION as NTSTATUS
            || r.NumberParameters < 2
            || r.ExceptionInformation[0] != WRITE
        {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        let at = r.ExceptionInformation[1];
        #[cfg(any(test, feature = "diagnostics"))]
        let _fault = stats::Fault::begin();
        // Held across both checks: another thread may have opened the block
        // since this one faulted, and a copy may have guarded it again since
        // (then it is ours again, not read-only for some other reason).
        let mut g = guard_lock(Wait::Fault);
        if open(&mut g, at, at + 1, Change::OpenFault) {
            #[cfg(feature = "diagnostics")]
            crate::copystat::add(crate::copystat::C::GuardFaults, 1);
            return EXCEPTION_CONTINUE_EXECUTION;
        }
        if writable(at) {
            EXCEPTION_CONTINUE_EXECUTION
        } else {
            EXCEPTION_CONTINUE_SEARCH
        }
    }
    static ONCE: std::sync::Once = std::sync::Once::new();
    // SAFETY: the handler only touches our guard state and protections.
    ONCE.call_once(|| unsafe {
        // The statistics' switch is read here, not first in the handler.
        #[cfg(any(test, feature = "diagnostics"))]
        stats::on();
        AddVectoredExceptionHandler(1, Some(on_write));
    });
}

/// Whether `at` is committed read-write memory now.
fn writable(at: usize) -> bool {
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_READWRITE, VirtualQuery,
    };
    // SAFETY: plain data filled in by the query.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a valid out pointer of the right size.
    let n = unsafe { VirtualQuery(at as *const _, &mut info, size_of_val(&info)) };
    n != 0 && info.State == MEM_COMMIT && info.Protect == PAGE_READWRITE
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc, VirtualFree,
    };
    const MEM_WRITE_WATCH: u32 = 0x20_0000;

    #[test]
    fn kernel_pages_fold_into_guest_pages_without_losing_writes() {
        let mut journal = EMPTY_JOURNAL;
        journal.found.extend([0, KERNEL_PAGE, PAGE, PAGE + KERNEL_PAGE, 3 * PAGE]);
        note_kernel_writes(&mut journal, 4, 12);
        assert_eq!(journal.written, BTreeMap::from([(0, 12), (1, 12)]));
        journal.found[..3].copy_from_slice(&[PAGE, 3 * PAGE, PAGE + KERNEL_PAGE]);
        note_kernel_writes(&mut journal, 3, 15);
        assert_eq!(journal.written, BTreeMap::from([(0, 12), (1, 15), (3, 15)]));
    }

    #[test]
    fn clean_runs_match_individual_pages_after_overlaps_and_invalidations() {
        let mut clean = CleanBlocks::default();
        let mut pages = BTreeSet::new();
        let check = |clean: &CleanBlocks, pages: &BTreeSet<usize>| {
            for first in 0..=24 {
                for last in first..=24 {
                    assert_eq!(
                        clean.gaps(first, last).into_iter().flatten().collect::<Vec<_>>(),
                        (first..last).filter(|block| !pages.contains(block)).collect::<Vec<_>>()
                    );
                }
            }
        };
        for (first, last) in [(4, 9), (12, 20), (9, 12), (6, 15), (2, 5), (17, 22), (9, 9)] {
            clean.insert(first, last);
            pages.extend(first..last);
            check(&clean, &pages);
        }
        for block in [13, 2, 21, 14, 12, 0, 10, 20, 13] {
            clean.remove(block);
            pages.remove(&block);
            check(&clean, &pages);
        }
        clean.insert(0, 24);
        assert!(clean.gaps(0, 24).is_empty());
        assert_eq!(clean.0.len(), 1);
    }

    #[test]
    fn written_pages_are_reported_to_every_asker() {
        // SAFETY: a fresh write-watched allocation, freed at the end.
        let p = unsafe {
            VirtualAlloc(
                std::ptr::null(),
                4 * PAGE,
                MEM_RESERVE | MEM_COMMIT | MEM_WRITE_WATCH,
                PAGE_READWRITE,
            )
        } as usize;
        assert_ne!(p, 0);
        let (start, end) = (p, p + 4 * PAGE);
        let (_, before) = writes(start, end, false).expect("watched");
        assert!(before.is_empty());
        // SAFETY: committed pages of ours.
        unsafe { ((p + PAGE + 8) as *mut u8).write(1) };
        let (now, after) = writes(start, end, false).expect("watched");
        assert_eq!(after, [(p / PAGE + 1, now)]);
        // Another asker after the kernel's bit was cleared still sees it.
        let (later, again) = writes(p + PAGE, p + 2 * PAGE, false).expect("watched");
        assert!(later > now);
        assert_eq!(again, [(p / PAGE + 1, now)]);
        let mut plain = vec![0u8; PAGE];
        assert!(
            writes(
                plain.as_mut_ptr() as usize,
                plain.as_ptr() as usize + PAGE,
                true
            )
            .is_none()
        );
        // SAFETY: our allocation.
        unsafe { VirtualFree(p as *mut _, 0, MEM_RELEASE) };
    }

    /// A kernel read into a guarded view block fails; opened first, it
    /// lands and the block counts as written.
    #[test]
    fn kernel_reads_into_guarded_blocks_after_opening() {
        use std::io::{Read, Seek, Write};
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, FILE_MAP_WRITE, MapViewOfFile, UnmapViewOfFile,
        };
        // SAFETY: a pagefile section of two blocks, mapped read-write.
        let view = unsafe {
            let section = CreateFileMappingW(
                -1isize as _,
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                (2 * BLOCK) as u32,
                std::ptr::null(),
            );
            assert!(!section.is_null());
            MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, 2 * BLOCK).Value as usize
        };
        assert_ne!(view, 0);
        assert!(guard(view, view + 2 * BLOCK).is_some()); // now read-only
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"from the disk").unwrap();
        // `ReadFile` inside: the kernel writes the view.
        let read = |file: &mut std::fs::File| {
            file.rewind().unwrap();
            // SAFETY: 13 bytes of our view, nothing else refers to them.
            let into = unsafe { std::slice::from_raw_parts_mut((view + 100) as *mut u8, 13) };
            file.read(into).is_ok_and(|n| n == 13)
        };
        assert!(
            !read(&mut file),
            "the kernel does not fault into our handler"
        );
        writing(view + 100, 13);
        assert!(read(&mut file));
        // SAFETY: the bytes just read.
        let landed = unsafe { std::slice::from_raw_parts((view + 100) as *const u8, 13) };
        assert_eq!(landed, b"from the disk");
        assert_eq!(
            guard(view, view + 2 * BLOCK),
            Some((view / PAGE..view / PAGE + 1).collect())
        );
        // SAFETY: our view.
        unsafe {
            UnmapViewOfFile(
                windows_sys::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS {
                    Value: view as *mut _,
                },
            )
        };
        title_memory_remapped(view, 2 * BLOCK);
    }

    #[test]
    fn writes_in_partly_committed_view_blocks_are_reported() {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
            SEC_RESERVE, UnmapViewOfFile,
        };

        const SIZE: usize = 3 * BLOCK;
        struct View(MEMORY_MAPPED_VIEW_ADDRESS);
        impl Drop for View {
            fn drop(&mut self) {
                title_memory_remapped(self.0.Value as usize, SIZE);
                // SAFETY: our view is still mapped.
                unsafe { UnmapViewOfFile(self.0) };
            }
        }
        // SAFETY: a new reserved pagefile section. The view holds its own
        // reference after we close the handle, including on assertion failure.
        let view = unsafe {
            let section = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                std::ptr::null(),
                PAGE_READWRITE | SEC_RESERVE,
                0,
                SIZE as u32,
                std::ptr::null(),
            );
            assert!(!section.is_null());
            let view = MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, SIZE);
            CloseHandle(section);
            assert!(!view.Value.is_null());
            View(view)
        };
        let start = view.0.Value as usize;
        let commit = |at: usize, len| {
            // SAFETY: these pages belong to our reserved view.
            let committed = unsafe { VirtualAlloc(at as _, len, MEM_COMMIT, PAGE_READWRITE) };
            assert_eq!(committed as usize, at);
        };
        let early = start + BLOCK + 3 * KERNEL_PAGE;
        let late = start + 2 * BLOCK + 3 * KERNEL_PAGE;
        commit(start, BLOCK);
        commit(early, KERNEL_PAGE);
        let refresh = || guard(start, start + SIZE).unwrap();
        assert_eq!(refresh(), [start / PAGE, early / PAGE, late / PAGE]);
        // The last block is still entirely reserved.
        assert!(!refresh().contains(&(late / PAGE)));

        commit(late, KERNEL_PAGE); // Ordinary commit, without a runtime mapping notification.
        for at in [early, late] {
            assert!(writable(at));
            // SAFETY: each address is inside its committed read-write page.
            unsafe { (at as *mut u8).write_volatile(1) };
        }
        assert_eq!(refresh(), [early / PAGE, late / PAGE]);
    }

    /// A range of view memory whose first block is not mapped is a view
    /// all the same: its blocks are reported once, then the ones written
    /// or mapped since.
    #[test]
    fn a_range_that_begins_in_an_unmapped_block_is_guarded() {
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, MEM_PRESERVE_PLACEHOLDER, MEM_REPLACE_PLACEHOLDER,
            MEM_RESERVE_PLACEHOLDER, MapViewOfFile3, PAGE_NOACCESS, UnmapViewOfFile2,
            VirtualAlloc2,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        // SAFETY: a fresh placeholder of three blocks and a pagefile
        // section of three; the middle block is mapped first.
        let (p, section) = unsafe {
            let p = VirtualAlloc2(
                GetCurrentProcess(),
                std::ptr::null(),
                3 * BLOCK,
                MEM_RESERVE | MEM_RESERVE_PLACEHOLDER,
                PAGE_NOACCESS,
                std::ptr::null_mut(),
                0,
            ) as usize;
            assert_ne!(p, 0);
            let section = CreateFileMappingW(
                -1isize as _,
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                (3 * BLOCK) as u32,
                std::ptr::null(),
            );
            assert!(!section.is_null());
            (p, section)
        };
        let map = |block: usize| {
            // SAFETY: splits the placeholder at the block and maps the
            // section's block there.
            let view = unsafe {
                VirtualFree(
                    (p + block * BLOCK) as *mut _,
                    BLOCK,
                    MEM_RELEASE | MEM_PRESERVE_PLACEHOLDER,
                );
                MapViewOfFile3(
                    section,
                    GetCurrentProcess(),
                    (p + block * BLOCK) as *const _,
                    (block * BLOCK) as u64,
                    BLOCK,
                    MEM_REPLACE_PLACEHOLDER,
                    PAGE_READWRITE,
                    std::ptr::null_mut(),
                    0,
                )
            };
            assert_eq!(view.Value as usize, p + block * BLOCK);
            view
        };
        let pages = |block: usize| (p + block * BLOCK) / PAGE..(p + (block + 1) * BLOCK) / PAGE;
        let all: Vec<usize> = (0..3).flat_map(pages).collect();
        // Nothing mapped at all: a view with every block absent.
        assert!(is_view(p, p + 3 * BLOCK));
        let middle = map(1);
        title_memory_remapped(p + BLOCK, BLOCK);
        // The range begins in the placeholder, its middle block is a view.
        assert!(is_view(p, p + 3 * BLOCK));
        assert_eq!(guard(p, p + 3 * BLOCK), Some(all));
        assert_eq!(guard(p, p + 3 * BLOCK), Some(Vec::new()), "nothing written since");
        // SAFETY: inside the mapped middle block.
        unsafe { ((p + BLOCK + 8) as *mut u8).write_volatile(1) };
        assert_eq!(guard(p, p + 3 * BLOCK), Some(pages(1).collect()));
        let first = map(0);
        title_memory_remapped(p, BLOCK);
        assert_eq!(guard(p, p + 3 * BLOCK), Some(pages(0).collect()));
        assert_eq!(guard(p, p + 3 * BLOCK), Some(Vec::new()));
        // Memory of ours that is no view stays none.
        let mut local = [0u8; 16];
        let at = local.as_mut_ptr() as usize;
        assert!(!is_view(at, at + local.len()));
        // SAFETY: our views and placeholder.
        unsafe {
            UnmapViewOfFile2(GetCurrentProcess(), first, MEM_PRESERVE_PLACEHOLDER);
            UnmapViewOfFile2(GetCurrentProcess(), middle, MEM_PRESERVE_PLACEHOLDER);
            for block in 0..3 {
                VirtualFree((p + block * BLOCK) as *mut _, 0, MEM_RELEASE);
            }
            windows_sys::Win32::Foundation::CloseHandle(section);
        }
        title_memory_remapped(p, 3 * BLOCK);
    }

    /// Blocks of a placeholder between views are reported once, then not
    /// again (they cannot be guarded) until the runtime maps them.
    #[test]
    fn unmapped_blocks_are_reported_once_until_mapped() {
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, MEM_PRESERVE_PLACEHOLDER, MEM_REPLACE_PLACEHOLDER,
            MEM_RESERVE_PLACEHOLDER, MapViewOfFile3, PAGE_NOACCESS, UnmapViewOfFile2,
            VirtualAlloc2,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        // SAFETY: a fresh placeholder of three blocks and a pagefile
        // section of two; block 0 shows the section's first block, as the
        // runtime maps physical pages.
        let (p, section) = unsafe {
            let p = VirtualAlloc2(
                GetCurrentProcess(),
                std::ptr::null(),
                3 * BLOCK,
                MEM_RESERVE | MEM_RESERVE_PLACEHOLDER,
                PAGE_NOACCESS,
                std::ptr::null_mut(),
                0,
            ) as usize;
            assert_ne!(p, 0);
            let section = CreateFileMappingW(
                -1isize as _,
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                (2 * BLOCK) as u32,
                std::ptr::null(),
            );
            assert!(!section.is_null());
            (p, section)
        };
        let map = |block: usize| {
            // SAFETY: splits the placeholder at the block and maps the
            // section's block there.
            let view = unsafe {
                VirtualFree(
                    (p + block * BLOCK) as *mut _,
                    BLOCK,
                    MEM_RELEASE | MEM_PRESERVE_PLACEHOLDER,
                );
                MapViewOfFile3(
                    section,
                    GetCurrentProcess(),
                    (p + block * BLOCK) as *const _,
                    (block * BLOCK) as u64,
                    BLOCK,
                    MEM_REPLACE_PLACEHOLDER,
                    PAGE_READWRITE,
                    std::ptr::null_mut(),
                    0,
                )
            };
            assert_eq!(view.Value as usize, p + block * BLOCK);
            view
        };
        let first = map(0);
        let pages = |block: usize| (p + block * BLOCK) / PAGE..(p + (block + 1) * BLOCK) / PAGE;
        let all: Vec<usize> = (0..3).flat_map(pages).collect();
        assert_eq!(guard(p, p + 3 * BLOCK), Some(all));
        assert_eq!(
            guard(p, p + 3 * BLOCK),
            Some(Vec::new()),
            "nothing written since"
        );
        let second = map(1);
        title_memory_remapped(p + BLOCK, BLOCK);
        assert_eq!(guard(p, p + 3 * BLOCK), Some(pages(1).collect()));
        assert_eq!(guard(p, p + 3 * BLOCK), Some(Vec::new()));
        // SAFETY: our views and placeholder.
        unsafe {
            UnmapViewOfFile2(GetCurrentProcess(), first, MEM_PRESERVE_PLACEHOLDER);
            UnmapViewOfFile2(GetCurrentProcess(), second, MEM_PRESERVE_PLACEHOLDER);
            for block in 0..3 {
                VirtualFree((p + block * BLOCK) as *mut _, 0, MEM_RELEASE);
            }
            windows_sys::Win32::Foundation::CloseHandle(section);
        }
        title_memory_remapped(p, 3 * BLOCK);
    }

    /// A pagefile section mapped twice: two views of the same physical
    /// pages, the second left for the test; the section handle, the two
    /// addresses.
    fn two_views(bytes: usize) -> (usize, usize, usize) {
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, FILE_MAP_WRITE, MapViewOfFile,
        };
        // SAFETY: a fresh pagefile section, mapped twice; the views keep
        // the section alive and the handle is closed by the caller.
        unsafe {
            let section = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                bytes as u32,
                std::ptr::null(),
            );
            assert!(!section.is_null());
            let one = MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, bytes).Value as usize;
            let two = MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, bytes).Value as usize;
            assert_ne!(one, 0);
            assert_ne!(two, 0);
            crate::inplace::title_memory_view(one, bytes, section as usize, 0);
            crate::inplace::title_memory_view(two, bytes, section as usize, 0);
            (section as usize, one, two)
        }
    }

    #[test]
    #[ignore = "run on Windows to measure mapped-view guard checks"]
    fn measures_mapped_guard_scans() {
        use std::time::Instant;
        for blocks in [8, 1024] {
            let size = blocks * BLOCK;
            let (section, main, alias) = two_views(size);
            assert_eq!(guard(main, main + size).unwrap().len(), blocks);
            for dirty in [false, true] {
                let rounds = if dirty { 500 } else { 12_000 };
                let mut samples = Vec::new();
                for _ in 0..5 {
                    let begin = Instant::now();
                    for cycle in 0..rounds {
                        let block = (cycle * 17) % blocks;
                        if dirty {
                            // SAFETY: the second mapped view covers the same section as main.
                            unsafe { ((alias + block * BLOCK) as *mut u8).write_volatile(1) };
                        }
                        let changed = guard(main, main + size).unwrap();
                        if dirty {
                            assert_eq!(changed, [(main + block * BLOCK) / PAGE]);
                        } else {
                            assert!(changed.is_empty());
                        }
                    }
                    samples.push(begin.elapsed().as_secs_f64() * 1e6 / rounds as f64);
                }
                samples.sort_by(f64::total_cmp);
                println!(
                    "{blocks} blocks, alias write={dirty}: median {:.3} us/check; samples {samples:?}",
                    samples[2]
                );
            }
            drop_views(section, &[main, alias], size);
        }
    }

    /// The section of [`two_views`] is closed and its views unmapped; the
    /// guard is told they are gone first.
    fn drop_views(section: usize, views: &[usize], bytes: usize) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Memory::{MEMORY_MAPPED_VIEW_ADDRESS, UnmapViewOfFile};
        for &view in views {
            title_memory_remapped(view, bytes);
            // SAFETY: a view of the section mapped by `two_views`.
            unsafe {
                UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                    Value: view as *mut _,
                })
            };
        }
        // SAFETY: the section's last reference.
        unsafe { CloseHandle(section as *mut std::ffi::c_void) };
    }

    /// The title wrote the pages of a guarded texture through another view
    /// of them: the fault at that view is the guard's to see, and the
    /// guarded view's next check reports the pages written.
    #[test]
    fn a_write_through_another_view_is_reported_at_the_guarded_one() {
        const SIZE: usize = 2 * BLOCK;
        let (section, guarded, other) = two_views(SIZE);
        let pages = |block: usize| (guarded + block * BLOCK) / PAGE;
        // The first check takes it in (and every view of its pages).
        assert_eq!(guard(guarded, guarded + SIZE), Some(vec![pages(0), pages(1)]));
        assert_eq!(guard(guarded, guarded + SIZE), Some(Vec::new()), "nothing written yet");
        // Writing through the other view faults into the handler, which
        // opens that view and the guarded one: the next check sees it.
        // SAFETY: the second view, read-write once the handler has opened it.
        unsafe { ((other + BLOCK + 8) as *mut u8).write(1) };
        assert_eq!(guard(guarded, guarded + SIZE), Some(vec![pages(1)]));
        assert_eq!(guard(guarded, guarded + SIZE), Some(Vec::new()));
        drop_views(section, &[guarded, other], SIZE);
    }

    /// A view of guarded pages is mapped anew: it is writable, so the
    /// guarded view checks its pages again (and guards the new view).
    #[test]
    fn a_view_mapped_beside_a_guarded_one_makes_it_check_again() {
        const SIZE: usize = 2 * BLOCK;
        let (section, guarded, other) = two_views(SIZE);
        let pages = |block: usize| (guarded + block * BLOCK) / PAGE;
        assert_eq!(guard(guarded, guarded + SIZE), Some(vec![pages(0), pages(1)]));
        assert_eq!(guard(guarded, guarded + SIZE), Some(Vec::new()), "nothing written yet");
        title_memory_remapped(other, SIZE);
        crate::inplace::title_memory_view(other, SIZE, section, 0);
        assert_eq!(guard(guarded, guarded + SIZE), Some(vec![pages(0), pages(1)]));
        assert_eq!(guard(guarded, guarded + SIZE), Some(Vec::new()));
        drop_views(section, &[guarded, other], SIZE);
    }

    /// A write of the runtime's own, told at one view of the pages
    /// (`writing`): the other views' next checks report the pages written.
    #[test]
    fn a_write_told_at_one_view_is_reported_at_the_others() {
        const SIZE: usize = 2 * BLOCK;
        let (section, guarded, other) = two_views(SIZE);
        let pages = |block: usize| (guarded + block * BLOCK) / PAGE;
        assert_eq!(guard(guarded, guarded + SIZE), Some(vec![pages(0), pages(1)]));
        assert_eq!(guard(guarded, guarded + SIZE), Some(Vec::new()), "nothing written yet");
        writing(other + BLOCK, BLOCK);
        assert_eq!(guard(guarded, guarded + SIZE), Some(vec![pages(1)]));
        assert_eq!(guard(guarded, guarded + SIZE), Some(Vec::new()));
        // Half a page and the next: both blocks, not just the first's.
        writing(other + BLOCK / 2, BLOCK);
        assert_eq!(guard(guarded, guarded + SIZE), Some(vec![pages(0), pages(1)]));
        drop_views(section, &[guarded, other], SIZE);
    }

    /// Both views of the pages are checked, each for a copy of its own
    /// (the title shows one physical page at several addresses, and each
    /// address has its host copy): a write through either is reported at
    /// both, whichever is checked first. Guarding the pages for the first
    /// view's check must not note the second view clean, or its copy would
    /// keep the bytes from before the write.
    #[test]
    fn a_write_is_reported_at_every_checked_view() {
        const SIZE: usize = 2 * BLOCK;
        let (section, one, two) = two_views(SIZE);
        let pages = |view: usize, block: usize| (view + block * BLOCK) / PAGE;
        assert_eq!(guard(one, one + SIZE), Some(vec![pages(one, 0), pages(one, 1)]));
        assert_eq!(guard(two, two + SIZE), Some(vec![pages(two, 0), pages(two, 1)]), "the second view's first check");
        assert_eq!(guard(one, one + SIZE), Some(Vec::new()), "nothing written yet");
        assert_eq!(guard(two, two + SIZE), Some(Vec::new()), "nothing written yet");
        // SAFETY: the second view, read-write once the handler has opened it.
        unsafe { ((two + BLOCK + 8) as *mut u8).write(1) };
        assert_eq!(guard(one, one + SIZE), Some(vec![pages(one, 1)]));
        assert_eq!(guard(two, two + SIZE), Some(vec![pages(two, 1)]), "written through this view, checked second");
        // SAFETY: the first view, as above.
        unsafe { ((one + 8) as *mut u8).write(1) };
        assert_eq!(guard(two, two + SIZE), Some(vec![pages(two, 0)]));
        assert_eq!(guard(one, one + SIZE), Some(vec![pages(one, 0)]), "written through this view, checked second");
        assert_eq!(guard(one, one + SIZE), Some(Vec::new()));
        assert_eq!(guard(two, two + SIZE), Some(Vec::new()));
        drop_views(section, &[one, two], SIZE);
    }

    /// A page shown in more than `MANY_VIEWS` views is guarded at each by
    /// itself: the others stay writable, and nothing looks through them all.
    #[test]
    fn a_page_of_many_views_is_guarded_one_view_at_a_time() {
        let (section, one, two) = two_views(BLOCK);
        let filler = one + (1 << 45);
        for n in 0..MANY_VIEWS {
            crate::inplace::title_memory_view(filler + n * BLOCK, BLOCK, section, 0);
        }
        assert_eq!(guard(one, one + BLOCK), Some(vec![one / PAGE]));
        assert!(writable(two), "another view of it is left alone");
        // SAFETY: the guarded view, opened by the handler.
        unsafe { ((one + 8) as *mut u8).write(1) };
        assert_eq!(guard(one, one + BLOCK), Some(vec![one / PAGE]));
        title_memory_remapped(filler, MANY_VIEWS * BLOCK);
        drop_views(section, &[one, two], BLOCK);
    }

    /// This thread's protection statistics since the last call: per change
    /// (calls, blocks, failed, runs) and the others.
    fn counted() -> (Vec<[u64; 4]>, [u64; stats::OTHERS]) {
        let (change, other) = stats::mine();
        let per = change
            .iter()
            .map(|c| [c[stats::CALLS], c[stats::BLOCKS], c[stats::FAILED], c[stats::RUNS]])
            .collect();
        (per, other)
    }

    /// The counts of one change, [calls, blocks, failed, runs].
    fn of(counts: &(Vec<[u64; 4]>, [u64; stats::OTHERS]), change: Change) -> [u64; 4] {
        counts.0[change as usize]
    }

    /// Every protection change of a check, a write through another view,
    /// the check after it and a write told is counted under its cause, with
    /// the runs its blocks make; nothing the guards do changes.
    #[test]
    fn protection_changes_are_counted_by_cause() {
        const SIZE: usize = 2 * BLOCK;
        let (section, guarded, other) = two_views(SIZE);
        let pages = |block: usize| (guarded + block * BLOCK) / PAGE;
        let _ = counted();

        // The first check guards both blocks in one call, and the other
        // view's two blocks one call each: one run.
        assert_eq!(guard(guarded, guarded + SIZE), Some(vec![pages(0), pages(1)]));
        let c = counted();
        assert_eq!(of(&c, Change::Guard), [1, 2, 0, 0]);
        assert_eq!(of(&c, Change::Alias), [2, 2, 0, 1]);
        for change in [Change::GuardRetry, Change::OpenFault, Change::OpenWrite, Change::OpenAlias, Change::OpenMapped] {
            assert_eq!(of(&c, change), [0; 4], "{change:?}");
        }
        assert_eq!([c.1[stats::CHECKS], c.1[stats::CHECK_CALLS], c.1[stats::CHECK_JOINED]], [1, 1, 1]);
        assert_eq!(c.1[stats::WAITS], 1, "one wait for the lock by the check");

        // Nothing written: no call.
        assert_eq!(guard(guarded, guarded + SIZE), Some(Vec::new()));
        let c = counted();
        assert!(c.0.iter().all(|v| *v == [0; 4]));
        assert_eq!(c.1[stats::CHECKS], 0);

        // A write through the other view faults: that block is opened for
        // the fault, the guarded view's block as another view of it.
        // SAFETY: the second view, read-write once the handler has opened it.
        unsafe { ((other + BLOCK + 8) as *mut u8).write_volatile(1) };
        let c = counted();
        assert_eq!(of(&c, Change::OpenFault), [1, 1, 0, 1]);
        assert_eq!(of(&c, Change::OpenAlias), [1, 1, 0, 1]);
        assert_eq!(c.1[stats::FAULTS], 1);
        assert_eq!(c.1[stats::WAITS + 2], 1, "one wait for the lock by the handler");

        // The next check reports the block and guards it again, at both
        // views.
        assert_eq!(guard(guarded, guarded + SIZE), Some(vec![pages(1)]));
        let c = counted();
        assert_eq!(of(&c, Change::Guard), [1, 1, 0, 0]);
        assert_eq!(of(&c, Change::Alias), [1, 1, 0, 1]);

        // A write told over the other view's two blocks opens both (one
        // run) and their two blocks in the guarded view (one run).
        writing(other, SIZE);
        let c = counted();
        assert_eq!(of(&c, Change::OpenWrite), [2, 2, 0, 1]);
        assert_eq!(of(&c, Change::OpenAlias), [2, 2, 0, 1]);
        assert_eq!(c.1[stats::WAITS + 4], 1, "one wait for the lock by the write told");
        assert_eq!(guard(guarded, guarded + SIZE), Some(vec![pages(0), pages(1)]));
        drop_views(section, &[guarded, other], SIZE);
        let _ = counted();
    }

    /// Two runs guarded by one check with only guarded blocks between them
    /// count as one call joined; time on a submitting thread is summed
    /// apart, and only there.
    #[test]
    fn joinable_runs_and_submission_time_are_counted() {
        const SIZE: usize = 5 * BLOCK;
        let (section, guarded, other) = two_views(SIZE);
        assert_eq!(guard(guarded, guarded + SIZE).map(|w| w.len()), Some(5));
        // SAFETY: the guarded view, opened by the handler at each write.
        unsafe {
            ((guarded + 8) as *mut u8).write_volatile(1);
            ((guarded + 3 * BLOCK + 8) as *mut u8).write_volatile(1);
        }
        let _ = counted();
        stats::set_submitting(true);
        assert_eq!(guard(guarded, guarded + SIZE).map(|w| w.len()), Some(2));
        stats::set_submitting(false);
        let (change, other_counts) = stats::mine();
        assert_eq!(other_counts[stats::CHECK_CALLS], 2);
        assert_eq!(other_counts[stats::CHECK_JOINED], 1, "blocks 1 and 2 between the runs are guarded");
        for c in [Change::Guard, Change::Alias] {
            assert_eq!(change[c as usize][stats::CALLS], 2, "{c:?}");
            assert_eq!(change[c as usize][stats::US_SUBMITTING], change[c as usize][stats::US], "{c:?}");
        }
        // SAFETY: as above.
        unsafe { ((guarded + 8) as *mut u8).write_volatile(2) };
        let _ = counted();
        assert_eq!(guard(guarded, guarded + SIZE).map(|w| w.len()), Some(1));
        let (change, _) = stats::mine();
        assert_eq!(change[Change::Guard as usize][stats::US_SUBMITTING], 0, "not in a submission");
        drop_views(section, &[guarded, other], SIZE);
        let _ = counted();
    }

    /// A run that cannot be guarded in one call (a block in it is not
    /// committed) is guarded block by block: the failed calls are counted,
    /// and the guards do as they did (the block is noted absent).
    #[test]
    fn failed_protections_are_counted() {
        // SAFETY: a reservation of three blocks, the outer two committed.
        let p = unsafe { VirtualAlloc(std::ptr::null(), 3 * BLOCK, MEM_RESERVE, PAGE_READWRITE) } as usize;
        assert_ne!(p, 0);
        for block in [0, 2] {
            // SAFETY: inside our reservation.
            let at = unsafe { VirtualAlloc((p + block * BLOCK) as *const _, BLOCK, MEM_COMMIT, PAGE_READWRITE) };
            assert!(!at.is_null());
        }
        let first = p / BLOCK;
        let _ = counted();
        {
            let mut g = guards();
            protect_here(&mut g, first, first + 3);
            assert!(g.protected.contains(&first) && g.protected.contains(&(first + 2)));
            assert!(!g.protected.contains(&(first + 1)));
            assert!(g.absent.contains(&(first + 1)), "the uncommitted block is noted absent");
        }
        let c = counted();
        assert_eq!(of(&c, Change::Guard), [1, 3, 1, 0], "the run's call failed");
        assert_eq!(of(&c, Change::GuardRetry), [3, 3, 1, 0], "block by block, the middle one failed");
        // Reads still work, and a write to a guarded block is taken by the
        // handler and lands.
        // SAFETY: committed blocks of ours, read-only for now.
        unsafe {
            assert_eq!(((p + 8) as *const u8).read_volatile(), 0);
            ((p + 2 * BLOCK + 8) as *mut u8).write_volatile(7);
            assert_eq!(((p + 2 * BLOCK + 8) as *const u8).read_volatile(), 7);
        }
        let c = counted();
        assert_eq!(of(&c, Change::OpenFault), [1, 1, 0, 1]);
        title_memory_remapped(p, 3 * BLOCK);
        // SAFETY: our reservation.
        unsafe { VirtualFree(p as *mut _, 0, MEM_RELEASE) };
        let _ = counted();
    }

    /// A write or mapping that runs past the end of the address space (a
    /// title's `fread` with a huge count) covers up to the end; the sum used
    /// to overflow. Up there no test has guarded blocks.
    #[test]
    fn ranges_past_the_address_space_end_do_not_overflow() {
        let top = usize::MAX - 16;
        writing(top, PAGE);
        title_memory_remapped(top, usize::MAX);
    }
}

#[cfg(test)]
mod mapping_tests {
    use super::{
        title_memory_reading, title_memory_reading_range, title_memory_remapping,
        title_memory_remapping_range,
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    /// A change of mapping waits for a read in place that is under way, and
    /// a read in place waits for a change of mapping.
    #[test]
    fn reads_in_place_and_remappings_wait_for_each_other() {
        // A reader holds; the change does not begin until it lets go.
        let changed = Arc::new(AtomicBool::new(false));
        let reading = title_memory_reading();
        let (began, wait) = mpsc::channel();
        let change = {
            let changed = changed.clone();
            std::thread::spawn(move || {
                began.send(()).unwrap();
                let _changing = title_memory_remapping_range(0x10000..0x20000);
                changed.store(true, Ordering::SeqCst);
            })
        };
        wait.recv().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(!changed.load(Ordering::SeqCst), "the mapping changed under a read in place");
        drop(reading);
        change.join().unwrap();
        assert!(changed.load(Ordering::SeqCst));
        // A change holds; the read does not begin until it is done.
        let read = Arc::new(AtomicBool::new(false));
        let changing = title_memory_remapping();
        let (began, wait) = mpsc::channel();
        let reader = {
            let read = read.clone();
            std::thread::spawn(move || {
                began.send(()).unwrap();
                let _reading = title_memory_reading_range(0x30000..0x40000);
                read.store(true, Ordering::SeqCst);
            })
        };
        wait.recv().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(!read.load(Ordering::SeqCst), "a read in place began during a change of mapping");
        drop(changing);
        reader.join().unwrap();
        assert!(read.load(Ordering::SeqCst));
    }
}
