//! Diagnostics (PR PREP-0): where a submission's preparation goes, on the
//! submitting thread.
//!
//! `frame.cpu_ms` ("preparing" on the frame line) is, per submission, the
//! time from `started` to `prepared` in `Host::submit_part`, less the
//! settles inside it. This module times the steps of that same window on
//! the same thread, and the rest of each submission (its time less its
//! steps) is worked out on that submission before anything is summed, so
//! the steps and the rest add up to exactly what `frame.cpu_ms` counted.
//!
//! The steps ([`Phase`]) follow `submit_part` in order. Some have parts of
//! their own ([`Part`]), timed inside them: their time is part of the
//! step's, never added to the total again.
//!
//! Diagnostics builds and tests only; nothing here changes what is done.
//! Each step costs two clock readings; the counts are thread-local until
//! the submission ends, then added to atomics. Nothing is allocated,
//! sorted or logged while the submission is prepared (`copystat` formats
//! the totals at its own line). `VICTORY_PREP_STATS=0` turns it off.
//!
//! Its callers (`host.rs`, `shadow.rs`, `copystat.rs`) are diagnostics
//! code: in a test build without that feature only the tests here use it.
#![cfg_attr(not(feature = "diagnostics"), allow(dead_code))]

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Instant;

/// The steps of a submission's preparation, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// One write-watch asking for every refreshed range (`VICTORY_ASK_ONCE`),
    /// settles inside it left out.
    AskOnce = 0,
    /// Each list's deferred steps (`HostList::run_deferred`).
    Deferred,
    /// Sparse textures readied for their uploads.
    Sparse,
    /// Each list's prelude planned (and recorded, when not in parallel).
    Prelude,
    /// The preludes recorded in parallel (`record_preludes`).
    RecordPreludes,
    /// The refreshes still pending, done (`Batch::flush`).
    Flush,
    /// The end of the one asking (`watch::ask_once_end`).
    AskOnceEnd,
    /// The snapshots of the ranges shaders may write
    /// (`shadow::take_snapshots`).
    Snapshots,
    /// Copies in the card's memory: what goes up before the lists, and the
    /// compare after them (`Host::card_before`), settles left out.
    CardBefore,
}

pub(crate) const PHASES: usize = 9;

/// Parts timed inside a step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Part {
    /// Deferred steps by kind: a title copy into title memory, fills, the
    /// title's or our own work (`Run`, `Host`), a range shaders write (its
    /// copy's refresh first, then its snapshot noted or taken), textures
    /// (refreshes, sampled copies, uploads ahead). Refresh steps are only
    /// noted for the flush: they are what is left of the step.
    StepCopy = 0,
    StepFill,
    StepWork,
    StepGpuWrite,
    StepTexture,
    /// Refreshes of host copies done (`Batch::update`), inside the steps
    /// above (before a step that writes title memory or gives shaders a
    /// range) or in the last flush.
    Updates,
    /// The write-watch asking ahead of a flush and its end.
    AskAhead,
    /// Snapshots: taking the noted list, cutting it into jobs, queueing
    /// them for the kept threads, this thread copying, and this thread
    /// waiting for the kept threads to finish.
    SnapTake,
    SnapJobs,
    SnapQueue,
    SnapOwn,
    SnapWait,
}

pub(crate) const PARTS: usize = 12;

/// Counts beside the times: snapshot bytes and jobs.
pub(crate) const SNAP_BYTES: usize = 0;
pub(crate) const SNAP_JOBS: usize = 1;
pub(crate) const COUNTS: usize = 2;

/// One submission's times, on its thread.
#[derive(Clone, Copy)]
struct Open {
    phases: [u64; PHASES],
    parts: [[u64; PARTS]; PHASES],
    counts: [u64; COUNTS],
}

const CLOSED: Open = Open { phases: [0; PHASES], parts: [[0; PARTS]; PHASES], counts: [0; COUNTS] };

thread_local! {
    /// A submission is being prepared on this thread (and counted).
    static OPEN: Cell<bool> = const { Cell::new(false) };
    /// The step running now (for parts timed deeper down).
    static NOW: Cell<Option<Phase>> = const { Cell::new(None) };
    static MINE: RefCell<Open> = const { RefCell::new(CLOSED) };
}

/// Totals since the last [`take`]: per step and part, nanoseconds; the
/// rest; the time counted (as `frame.cpu_ms`); submissions; submissions
/// whose steps added up to more than their time, and by how much; the
/// longest submission; counts; and outside the counted time, the sweep and
/// the waiting slots made before it.
static PHASE_NS: [AtomicU64; PHASES] = [const { AtomicU64::new(0) }; PHASES];
static PART_NS: [[AtomicU64; PARTS]; PHASES] = [const { [const { AtomicU64::new(0) }; PARTS] }; PHASES];
static COUNT: [AtomicU64; COUNTS] = [const { AtomicU64::new(0) }; COUNTS];
pub(crate) const REST_NS: usize = 0;
pub(crate) const TOTAL_NS: usize = 1;
pub(crate) const SUBMISSIONS: usize = 2;
pub(crate) const OVER: usize = 3;
pub(crate) const OVER_NS: usize = 4;
pub(crate) const LONGEST_NS: usize = 5;
pub(crate) const SWEEP_NS: usize = 6;
pub(crate) const WAITING_SLOTS_NS: usize = 7;
pub(crate) const SUMS: usize = 8;
static SUM: [AtomicU64; SUMS] = [const { AtomicU64::new(0) }; SUMS];

/// Whether the timing is on (`VICTORY_PREP_STATS`, read once).
pub(crate) fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_PREP_STATS").is_none_or(|v| v != "0"))
}

/// A submission's timed preparation begins on this thread.
pub(crate) fn begin() {
    if !on() {
        return;
    }
    MINE.with(|m| *m.borrow_mut() = CLOSED);
    NOW.with(|n| n.set(None));
    OPEN.with(|o| o.set(true));
}

/// A step begins: its parts go to it. Returns the clock to pass to
/// [`phase_ends`].
pub(crate) fn phase_begins(phase: Phase) -> Instant {
    NOW.with(|n| n.set(Some(phase)));
    Instant::now()
}

/// The step begun at `since` ends; `excluded` (settles inside it, not
/// counted as preparing) is left out.
pub(crate) fn phase_ends(phase: Phase, since: Instant, excluded: std::time::Duration) {
    let ns = since.elapsed().saturating_sub(excluded).as_nanos() as u64;
    NOW.with(|n| n.set(None));
    if OPEN.with(|o| o.get()) {
        MINE.with(|m| m.borrow_mut().phases[phase as usize] += ns);
    }
}

/// A part of the step running now took `ns` (ignored outside a counted
/// submission, or outside every step).
pub(crate) fn part(part: Part, ns: u64) {
    if !OPEN.with(|o| o.get()) {
        return;
    }
    let Some(phase) = NOW.with(|n| n.get()) else { return };
    // `shadow::wide` serves other work too: its parts count in the
    // snapshots only.
    if matches!(part, Part::SnapQueue | Part::SnapOwn | Part::SnapWait) && phase != Phase::Snapshots {
        return;
    }
    MINE.with(|m| m.borrow_mut().parts[phase as usize][part as usize] += ns);
}

/// A part from `since` to now.
pub(crate) fn part_since(p: Part, since: Instant) {
    part(p, since.elapsed().as_nanos() as u64);
}

/// Adds to a count of the step running now's submission.
pub(crate) fn count(which: usize, n: u64) {
    if OPEN.with(|o| o.get()) {
        MINE.with(|m| m.borrow_mut().counts[which] += n);
    }
}

/// Times a deferred step of `part`'s kind from `since` until dropped (the
/// end of the loop's turn, `continue` included).
pub(crate) struct StepTimer {
    part: Part,
    since: Instant,
}

impl StepTimer {
    pub(crate) fn new(part: Part, since: Instant) -> Self {
        Self { part, since }
    }
}

impl Drop for StepTimer {
    fn drop(&mut self) {
        part_since(self.part, self.since);
    }
}

/// The submission's timed preparation ends; it counted `total` as
/// `frame.cpu_ms` does. The rest is its time less its steps, worked out
/// here, before anything is summed.
pub(crate) fn end(total: std::time::Duration) {
    if !OPEN.with(|o| o.replace(false)) {
        return;
    }
    NOW.with(|n| n.set(None));
    let open = MINE.with(|m| *m.borrow());
    let total = total.as_nanos() as u64;
    let steps: u64 = open.phases.iter().sum();
    let (rest, over) = (total.saturating_sub(steps), steps.saturating_sub(total));
    for (i, &ns) in open.phases.iter().enumerate() {
        PHASE_NS[i].fetch_add(ns, Relaxed);
        for (j, &p) in open.parts[i].iter().enumerate() {
            if p > 0 {
                PART_NS[i][j].fetch_add(p, Relaxed);
            }
        }
    }
    for (i, &n) in open.counts.iter().enumerate() {
        COUNT[i].fetch_add(n, Relaxed);
    }
    SUM[REST_NS].fetch_add(rest, Relaxed);
    SUM[TOTAL_NS].fetch_add(total, Relaxed);
    SUM[SUBMISSIONS].fetch_add(1, Relaxed);
    // The clock readings of a step and of the whole are taken apart: a
    // step may overrun the whole by their gap. More than 0.1 ms is noted.
    if over > 100_000 {
        SUM[OVER].fetch_add(1, Relaxed);
        SUM[OVER_NS].fetch_add(over, Relaxed);
    }
    SUM[LONGEST_NS].fetch_max(total, Relaxed);
    #[cfg(test)]
    LAST.with(|l| l.set(Some((open, rest, total))));
}

/// Time spent before the counted window (not in `frame.cpu_ms`), on the
/// submitting thread: the sweep (`SWEEP_NS`) or the waiting slots made
/// (`WAITING_SLOTS_NS`).
pub(crate) fn outside(which: usize, since: Instant) {
    if on() {
        SUM[which].fetch_add(since.elapsed().as_nanos() as u64, Relaxed);
    }
}

/// Every total since the last call, reset.
#[cfg(feature = "diagnostics")]
pub(crate) struct Totals {
    pub(crate) phases: [u64; PHASES],
    pub(crate) parts: [[u64; PARTS]; PHASES],
    pub(crate) counts: [u64; COUNTS],
    pub(crate) sums: [u64; SUMS],
}

#[cfg(feature = "diagnostics")]
pub(crate) fn take() -> Totals {
    Totals {
        phases: PHASE_NS.each_ref().map(|v| v.swap(0, Relaxed)),
        parts: PART_NS.each_ref().map(|r| r.each_ref().map(|v| v.swap(0, Relaxed))),
        counts: COUNT.each_ref().map(|v| v.swap(0, Relaxed)),
        sums: SUM.each_ref().map(|v| v.swap(0, Relaxed)),
    }
}

#[cfg(test)]
thread_local! {
    static LAST: Cell<Option<(Open, u64, u64)>> = const { Cell::new(None) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn sleep(ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }

    fn last() -> (Open, u64, u64) {
        LAST.with(|l| l.take()).expect("a submission ended")
    }

    /// Steps and the rest add up to the submission's counted time; a
    /// settle inside a step is left out of it as it is of the total.
    #[test]
    fn steps_and_rest_add_up_to_the_counted_time() {
        let started = Instant::now();
        begin();
        let t = phase_begins(Phase::AskOnce);
        sleep(3);
        // A settle of 4 ms inside the step: out of both.
        let settle = Instant::now();
        sleep(4);
        let settled = settle.elapsed();
        phase_ends(Phase::AskOnce, t, settled);
        sleep(2); // between steps: the rest
        let t = phase_begins(Phase::Snapshots);
        sleep(3);
        phase_ends(Phase::Snapshots, t, Duration::ZERO);
        let total = started.elapsed().saturating_sub(settled);
        end(total);
        let (open, rest, counted) = last();
        let steps: u64 = open.phases.iter().sum();
        assert_eq!(steps + rest, counted);
        assert!(open.phases[Phase::AskOnce as usize] >= 3_000_000);
        assert!(open.phases[Phase::AskOnce as usize] < 7_000_000, "the settle is left out");
        assert!(rest >= 2_000_000, "{rest}");
        assert_eq!(open.phases[Phase::Deferred as usize], 0);
    }

    /// Parts go to the step running when they are timed, and nowhere
    /// outside a step or outside a submission.
    #[test]
    fn parts_go_to_the_step_running() {
        // No submission open: nothing.
        part(Part::Updates, 5);
        begin();
        part(Part::Updates, 7); // outside every step: dropped
        let t = phase_begins(Phase::Deferred);
        {
            let _step = StepTimer::new(Part::StepCopy, Instant::now());
            part(Part::Updates, 11);
            sleep(1);
        }
        phase_ends(Phase::Deferred, t, Duration::ZERO);
        let t = phase_begins(Phase::Flush);
        part(Part::Updates, 13);
        phase_ends(Phase::Flush, t, Duration::ZERO);
        let t = phase_begins(Phase::Snapshots);
        count(SNAP_BYTES, 1 << 20);
        phase_ends(Phase::Snapshots, t, Duration::ZERO);
        end(Duration::from_secs(1));
        let (open, _, _) = last();
        let d = open.parts[Phase::Deferred as usize];
        assert_eq!(d[Part::Updates as usize], 11);
        assert!(d[Part::StepCopy as usize] >= 1_000_000);
        assert!(d[Part::StepCopy as usize] <= open.phases[Phase::Deferred as usize]);
        assert_eq!(open.parts[Phase::Flush as usize][Part::Updates as usize], 13);
        assert_eq!(open.counts[SNAP_BYTES], 1 << 20);
        // After the end: closed.
        part(Part::Updates, 17);
        assert!(LAST.with(|l| l.take()).is_none());
    }

    /// `shadow::wide` also runs fills and refreshes: its parts count only
    /// inside the snapshots.
    #[test]
    fn wide_parts_count_in_the_snapshots_only() {
        begin();
        let t = phase_begins(Phase::Deferred);
        part(Part::SnapOwn, 5);
        phase_ends(Phase::Deferred, t, Duration::ZERO);
        let t = phase_begins(Phase::Snapshots);
        part(Part::SnapOwn, 7);
        part(Part::SnapWait, 3);
        phase_ends(Phase::Snapshots, t, Duration::ZERO);
        end(Duration::from_secs(1));
        let (open, _, _) = last();
        assert_eq!(open.parts[Phase::Deferred as usize][Part::SnapOwn as usize], 0);
        assert_eq!(open.parts[Phase::Snapshots as usize][Part::SnapOwn as usize], 7);
        assert_eq!(open.parts[Phase::Snapshots as usize][Part::SnapWait as usize], 3);
    }

    /// Each submission starts from zero.
    #[test]
    fn each_submission_starts_afresh() {
        begin();
        let t = phase_begins(Phase::Prelude);
        sleep(2);
        phase_ends(Phase::Prelude, t, Duration::ZERO);
        end(Duration::from_millis(10));
        let _ = last();
        begin();
        end(Duration::from_millis(1));
        let (open, rest, total) = last();
        assert_eq!(open.phases, [0; PHASES]);
        assert_eq!((rest, total), (1_000_000, 1_000_000));
    }

    /// A step that overran its submission (clock readings taken apart) is
    /// not taken from the rest of others: the rest stops at zero.
    #[test]
    fn an_overrun_leaves_the_rest_at_zero() {
        begin();
        let t = phase_begins(Phase::CardBefore);
        sleep(2);
        phase_ends(Phase::CardBefore, t, Duration::ZERO);
        end(Duration::from_millis(1));
        let (_, rest, _) = last();
        assert_eq!(rest, 0);
    }
}
