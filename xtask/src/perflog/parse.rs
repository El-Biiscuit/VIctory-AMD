//! Reads a runtime log into samples: one per tick (about a second), one per
//! `copystat::log` window (about five seconds), one per group of gauge
//! lines, plus the per-range lists and the present events.

use std::collections::BTreeMap;

use super::catalog::{ENTRIES, HotKind, Scope};
use super::template::{Captures, Template, normalize};

/// The numbers of one tick, window or gauge group, keyed `line.capture`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sample {
    /// The log line (from 1) that opened the sample.
    pub line: usize,
    /// Seconds since the Unix epoch of that line, when it has a timestamp.
    pub t: Option<f64>,
    pub values: BTreeMap<String, f64>,
}

impl Sample {
    pub fn get(&self, key: &str) -> Option<f64> {
        self.values.get(key).copied()
    }
}

/// One line of a per-range list.
#[derive(Debug, Clone, PartialEq)]
pub struct HotLine {
    pub line: usize,
    pub kind: HotKind,
    /// The range as printed after `0x` (`1a2b0000+0x100000`).
    pub range: String,
    /// The memory type, for copies.
    pub memory: Option<String>,
    pub values: BTreeMap<String, f64>,
    /// The copystat window it belongs to (index into `Run::windows`).
    pub window: Option<usize>,
}

/// A parsed log.
#[derive(Debug, Default)]
pub struct Run {
    pub path: String,
    pub lines: usize,
    /// Launcher lines and settings seen in the log (no file paths).
    pub header: Vec<(String, String)>,
    pub ticks: Vec<Sample>,
    pub windows: Vec<Sample>,
    pub gauges: Vec<Sample>,
    pub hot: Vec<HotLine>,
    /// Present events (`victory_gpu::frame=debug`): line and time.
    pub presents: Vec<(usize, f64)>,
    /// Lines that held a known prefix but did not match its pattern.
    pub unparsed: BTreeMap<&'static str, usize>,
    /// Matched lines by pattern.
    pub matched: BTreeMap<&'static str, usize>,
}

impl Run {
    /// Whether the relay ran (its line is written every tick when it does).
    pub fn relay(&self) -> bool {
        self.matched.contains_key("relay")
    }

    /// Whether submissions were sent ahead (`VICTORY_LATE_WAIT`).
    pub fn late(&self) -> bool {
        self.matched.contains_key("late")
    }
}

/// Removes ANSI escape sequences (`ESC [ ... letter`).
pub fn strip_ansi(line: &str) -> std::borrow::Cow<'_, str> {
    if !line.contains('\u{1b}') {
        return std::borrow::Cow::Borrowed(line);
    }
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    std::borrow::Cow::Owned(out)
}

/// Days from 1970-01-01 to the given civil date (proleptic Gregorian).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The timestamp that starts a `tracing` line (RFC 3339:
/// `2026-11-27T21:03:11.123456Z` or with an offset), in seconds since the
/// Unix epoch.
pub fn timestamp(line: &str) -> Option<f64> {
    let word = line.split_whitespace().next()?;
    let b = word.as_bytes();
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || !(b[10] == b'T' || b[10] == b' ')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let num = |r: std::ops::Range<usize>| word.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, s) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let mut rest = &word[19..];
    let mut frac = 0.0;
    if let Some(r) = rest.strip_prefix('.') {
        let digits = r.find(|c: char| !c.is_ascii_digit()).unwrap_or(r.len());
        frac = format!("0.{}", &r[..digits]).parse::<f64>().ok()?;
        rest = &r[digits..];
    }
    let offset = match rest {
        "" | "Z" => 0,
        _ => {
            let sign = match rest.as_bytes()[0] {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let hm = rest[1..].replace(':', "");
            let oh = hm.get(0..2)?.parse::<i64>().ok()?;
            let om = hm.get(2..4).unwrap_or("0").parse::<i64>().ok()?;
            sign * (oh * 3600 + om * 60)
        }
    };
    let secs = days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + s - offset;
    Some(secs as f64 + frac)
}

/// Launcher lines kept in the header (by prefix). Paths are left out.
const HEADER_PREFIXES: &[&str] = &[
    "Ray tracing requested: ",
    "Expected scene rendering ",
    "Texture copy queue requested: ",
    "Sparse texture mips requested",
    "Dirty-page readback capture requested",
    "FSR ",
    "VIctory exited with ",
];

struct Compiled {
    entry: &'static super::catalog::Entry,
    template: Template,
}

fn compiled() -> &'static [Compiled] {
    static ALL: std::sync::OnceLock<Vec<Compiled>> = std::sync::OnceLock::new();
    ALL.get_or_init(|| {
        ENTRIES
            .iter()
            .map(|entry| Compiled {
                entry,
                template: Template::compile(entry.pattern),
            })
            .collect()
    })
}

/// Anchors shorter than this are too common to call a failed match a
/// parse problem (`memory: ` is in many lines).
const DISTINCT_ANCHOR: usize = 16;

/// The key prefix of a line's numbers: its id, then its qualifiers.
fn prefix(id: &str, caps: &Captures) -> String {
    if caps.qualifiers.is_empty() {
        id.to_owned()
    } else {
        format!("{id}.{}", caps.qualifiers.join("."))
    }
}

/// Adds a line's numbers to `sample` under `prefix.name`.
fn put(sample: &mut Sample, prefix: &str, caps: &Captures) {
    for (name, value) in &caps.nums {
        sample.values.insert(format!("{prefix}.{name}"), *value);
    }
}

/// Whether `sample` holds a key under `prefix` already (the line came
/// again: a new sample begins).
fn has_line(sample: &Sample, prefix: &str) -> bool {
    let prefix = format!("{prefix}.");
    sample
        .values
        .range(prefix.clone()..)
        .next()
        .is_some_and(|(k, _)| k.starts_with(&prefix))
}

/// Parses the text of one log.
pub fn parse(path: &str, text: &str) -> Run {
    let mut run = Run {
        path: path.to_owned(),
        ..Run::default()
    };
    let entries = compiled();
    for (index, raw) in text.lines().enumerate() {
        let number = index + 1;
        run.lines = number;
        let clean = strip_ansi(raw);
        let line = normalize(&clean);
        if line.is_empty() {
            continue;
        }
        let t = timestamp(&line);
        if t.is_none()
            && let Some(prefix) = HEADER_PREFIXES.iter().find(|p| line.starts_with(**p))
        {
            let key = prefix.trim_end_matches([' ', ':']).to_owned();
            let value = line[prefix.len()..].trim().to_owned();
            run.header.push((key, value));
            continue;
        }
        if line.contains("victory_gpu::frame") && line.contains(" present") {
            if let Some(t) = t {
                run.presents.push((number, t));
            }
            continue;
        }
        for c in entries {
            let anchor = c.template.anchor();
            if !line.contains(anchor) {
                continue;
            }
            let Some(caps) = c.template.matches(&line) else {
                if anchor.trim().len() >= DISTINCT_ANCHOR {
                    *run.unparsed.entry(c.entry.id).or_default() += 1;
                }
                continue;
            };
            *run.matched.entry(c.entry.id).or_default() += 1;
            let id = c.entry.id;
            let id = &prefix(id, &caps);
            let fresh = || Sample {
                line: number,
                t,
                values: BTreeMap::new(),
            };
            match c.entry.scope {
                Scope::TickStart => {
                    let mut s = fresh();
                    put(&mut s, id, &caps);
                    run.ticks.push(s);
                }
                Scope::Tick | Scope::Report => {
                    if run.ticks.last().is_none_or(|s| has_line(s, id)) {
                        run.ticks.push(fresh());
                    }
                    put(run.ticks.last_mut().expect("pushed above"), id, &caps);
                }
                Scope::CopyStart => {
                    let mut s = fresh();
                    put(&mut s, id, &caps);
                    run.windows.push(s);
                }
                Scope::Copy => {
                    if run.windows.last().is_none_or(|s| has_line(s, id)) {
                        run.windows.push(fresh());
                    }
                    put(run.windows.last_mut().expect("pushed above"), id, &caps);
                }
                Scope::Gauge => {
                    if run.gauges.last().is_none_or(|s| has_line(s, id)) {
                        run.gauges.push(fresh());
                    }
                    put(run.gauges.last_mut().expect("pushed above"), id, &caps);
                }
                Scope::Hot(kind) => {
                    let text = |name: &str| {
                        caps.texts
                            .iter()
                            .find(|(n, _)| n == name)
                            .map(|(_, v)| v.clone())
                    };
                    run.hot.push(HotLine {
                        line: number,
                        kind,
                        range: text("range").unwrap_or_default(),
                        memory: text("kind"),
                        values: caps.nums.iter().cloned().collect(),
                        window: run.windows.len().checked_sub(1),
                    });
                }
            }
            if c.entry.id == "joins"
                && let Some((_, state)) = caps.texts.iter().find(|(n, _)| n == "state")
            {
                let setting = ("VICTORY_JOIN_COPIES".to_owned(), state.clone());
                if !run.header.contains(&setting) {
                    run.header.push(setting);
                }
            }
            break;
        }
    }
    derive(&mut run);
    run
}

/// Adds the derived metrics to each sample.
fn derive(run: &mut Run) {
    let relay = run.relay();
    for s in &mut run.ticks {
        if let (Some(cpu), Some(copy), Some(tex)) = (
            s.get("frame.cpu_ms"),
            s.get("frame.copy_ms"),
            s.get("frame.texture_ms"),
        ) {
            s.values
                .insert("frame.cpu_other_ms".into(), (cpu - copy - tex).max(0.0));
        }
        // The card copy fetch waits: with the relay, those of its thread
        // are counted twice (`card.extra_wait_ms` and `relay.fetch_wait_ms`,
        // the same microseconds); the rest is the submitting thread's
        // (`Host::card_back`, synchronous submissions). The two counters
        // are taken a few microseconds apart in a tick, so the relay's part
        // is capped at the total.
        if let Some(all) = s.get("card.extra_wait_ms") {
            let relayed = if relay {
                s.get("relay.fetch_wait_ms").unwrap_or(0.0).min(all)
            } else {
                0.0
            };
            if relay {
                s.values.insert("card.extra_wait_relay_ms".into(), relayed);
            }
            s.values
                .insert("card.extra_wait_sync_ms".into(), (all - relayed).max(0.0));
        }
        if let (Some(cpu), Some(gpu)) = (s.get("frame.cpu_ms"), s.get("frame.gpu_ms")) {
            // With the relay the copy back runs on its own thread, beside
            // the submitting thread: only the part known to be the
            // submitting thread's (its card copy fetch waits) is on this
            // path. Without it the whole copy back is.
            let back = if relay {
                s.get("card.extra_wait_sync_ms").unwrap_or(0.0)
            } else {
                s.get("frame.back_ms").unwrap_or(0.0)
            };
            s.values
                .insert("frame.submit_path_ms".into(), cpu + gpu + back);
        }
        if let (Some(w), Some(b)) = (
            s.get("back.windows_compared_mib"),
            s.get("back.buffers_compared_mib"),
        ) {
            s.values.insert("back.compared_total_mib".into(), w + b);
            if let Some(written) = s.get("frame.back_mb") {
                let compared_mb = (w + b) * 1.048_576;
                if compared_mb > 0.0 {
                    s.values
                        .insert("back.written_pct".into(), 100.0 * written / compared_mb);
                }
            }
        }
        if let (Some(w), Some(b)) = (
            s.get("back.windows_snapshot_mib"),
            s.get("back.buffers_snapshot_mib"),
        ) {
            s.values.insert("back.snapshot_total_mib".into(), w + b);
        }
    }
    for s in &mut run.windows {
        // What the guards' calls would be if each contiguous run were one
        // call: checks' runs joined across guarded blocks, the block-by-
        // block retries as they are, and per-block kinds by their runs.
        let keys = [
            "guard_waits.check_joined",
            "protect.retry_calls",
            "protect.alias_runs",
            "protect.fault_runs",
            "protect.write_runs",
            "protect.other_runs",
            "protect.mapped_runs",
        ];
        if keys.iter().all(|k| s.values.contains_key(*k)) {
            let joined: f64 = keys.iter().map(|k| s.values[*k]).sum();
            s.values.insert("protect.calls_if_joined".into(), joined);
        }
        // The kinds of other thread must add up to "other threads": what
        // is left is the rounding of the printed values.
        let kinds = [
            "lock_kinds.rec_guards_wait_ms",
            "lock_kinds.run_guards_wait_ms",
            "lock_kinds.tother_guards_wait_ms",
            "lock_kinds.unc_guards_wait_ms",
        ];
        if let Some(other) = s.get("lock_roles.other_guards_wait_ms")
            && kinds.iter().all(|k| s.values.contains_key(*k))
        {
            let parts: f64 = kinds.iter().map(|k| s.values[*k]).sum();
            s.values
                .insert("lock_kinds.guards_wait_gap_ms".into(), other - parts);
        }
        if let (Some(checked), Some(unchanged)) =
            (s.get("copies.checked_mb"), s.get("copies.unchanged_mb"))
        {
            s.values
                .insert("copies.changed_mb".into(), (checked - unchanged).max(0.0));
            if checked > 0.0 {
                s.values
                    .insert("copies.unchanged_pct".into(), 100.0 * unchanged / checked);
            }
        }
    }
    for s in &mut run.gauges {
        if let (Some(used), Some(budget)) =
            (s.get("vram.local_used_mb"), s.get("vram.local_budget_mb"))
            && budget > 0.0
        {
            s.values
                .insert("vram.local_used_pct".into(), 100.0 * used / budget);
        }
    }
}

/// Which part of a run to look at.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Selection {
    /// Seconds to leave out after the start (below).
    pub skip_secs: f64,
    /// Seconds to keep after that; all when `None`.
    pub duration_secs: Option<f64>,
    /// Frame-rate lines to leave out after the start.
    pub skip_reports: usize,
    /// The start is the first frame-rate line with at least this many
    /// draws a frame (the scene is there); the first one when `None`.
    pub min_draws: Option<f64>,
}

impl Selection {
    pub fn is_default(&self) -> bool {
        *self == Selection::default()
    }

    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(d) = self.min_draws {
            parts.push(format!(
                "start at the first frame line with >= {d} draws/frame"
            ));
        }
        if self.skip_reports > 0 {
            parts.push(format!("skip {} frame lines", self.skip_reports));
        }
        if self.skip_secs > 0.0 {
            parts.push(format!("skip {} s", self.skip_secs));
        }
        if let Some(d) = self.duration_secs {
            parts.push(format!("keep {d} s"));
        }
        if parts.is_empty() {
            "whole log".to_owned()
        } else {
            parts.join(", ")
        }
    }
}

/// The selected part of a run.
#[derive(Debug, Default)]
pub struct Selected<'a> {
    pub run: Option<&'a Run>,
    pub ticks: Vec<&'a Sample>,
    pub windows: Vec<&'a Sample>,
    pub gauges: Vec<&'a Sample>,
    pub hot: Vec<&'a HotLine>,
    /// Frames of the selected copystat windows, by window index.
    pub window_frames: BTreeMap<usize, f64>,
    pub presents: Vec<f64>,
    /// Seconds between the first and last selected tick.
    pub span_secs: Option<f64>,
    pub warnings: Vec<String>,
}

/// Picks the samples of `run` that `sel` asks for. The selection is a range
/// of log lines set by the ticks; windows count only when every frame they
/// cover lies inside it (unless nothing is left out at the start).
pub fn select<'a>(run: &'a Run, sel: &Selection) -> Selected<'a> {
    let mut out = Selected {
        run: Some(run),
        ..Selected::default()
    };
    let reports: Vec<&Sample> = run
        .ticks
        .iter()
        .filter(|s| s.get("frame.fps").is_some())
        .collect();
    // The start: the first tick, or a frame-rate line when asked for by
    // draws or by count.
    let mut start_line = if sel.min_draws.is_none() && sel.skip_reports == 0 {
        run.ticks.first().map_or(0, |s| s.line)
    } else {
        let first = match sel.min_draws {
            Some(min) => reports
                .iter()
                .position(|s| s.get("frame.draws").is_some_and(|d| d >= min)),
            None => Some(0),
        };
        let Some(first) = first else {
            out.warnings.push(format!(
                "{}: no frame line reaches the draw threshold; nothing selected",
                run.path
            ));
            return out;
        };
        match reports.get(first + sel.skip_reports) {
            Some(s) => s.line,
            None => {
                out.warnings.push(format!(
                    "{}: fewer frame lines than skipped; nothing selected",
                    run.path
                ));
                return out;
            }
        }
    };
    let mut start_t = run
        .ticks
        .iter()
        .find(|s| s.line >= start_line)
        .and_then(|s| s.t);
    let timed = run.ticks.iter().all(|s| s.t.is_some());
    if (sel.skip_secs > 0.0 || sel.duration_secs.is_some()) && !timed {
        out.warnings.push(format!(
            "{}: lines without timestamps; --skip-secs and --duration-secs are ignored",
            run.path
        ));
    }
    if timed
        && sel.skip_secs > 0.0
        && let Some(t0) = start_t
    {
        match run
            .ticks
            .iter()
            .find(|s| s.t.is_some_and(|t| t >= t0 + sel.skip_secs))
        {
            Some(s) => {
                start_line = s.line;
                start_t = s.t;
            }
            None => {
                out.warnings.push(format!(
                    "{}: the log is shorter than the skipped time; nothing selected",
                    run.path
                ));
                return out;
            }
        }
    }
    let end_line = match (timed, sel.duration_secs, start_t) {
        (true, Some(d), Some(t0)) => run
            .ticks
            .iter()
            .find(|s| s.t.is_some_and(|t| t > t0 + d))
            .map_or(usize::MAX, |s| s.line.saturating_sub(1)),
        _ => usize::MAX,
    };
    let inside = |line: usize| line >= start_line && line <= end_line;
    let trimmed = !sel.is_default() || start_line > run.ticks.first().map_or(0, |s| s.line);
    out.ticks = run.ticks.iter().filter(|s| inside(s.line)).collect();
    for (i, w) in run.windows.iter().enumerate() {
        let covered = if trimmed {
            i > 0 && run.windows[i - 1].line >= start_line
        } else {
            true
        };
        if inside(w.line) && covered {
            out.windows.push(w);
            out.window_frames
                .insert(i, w.get("recording.frames").unwrap_or(0.0));
        }
    }
    out.gauges = run.gauges.iter().filter(|s| inside(s.line)).collect();
    out.hot = run
        .hot
        .iter()
        .filter(|h| h.window.is_some_and(|w| out.window_frames.contains_key(&w)))
        .collect();
    out.presents = run
        .presents
        .iter()
        .filter(|(l, _)| inside(*l))
        .map(|(_, t)| *t)
        .collect();
    let times: Vec<f64> = out.ticks.iter().filter_map(|s| s.t).collect();
    if let (Some(a), Some(b)) = (times.first(), times.last()) {
        out.span_secs = Some(b - a);
    }
    if out.ticks.is_empty() {
        out.warnings
            .push(format!("{}: no tick lines selected", run.path));
    }
    out
}

/// Splits a selection into the copystat windows where `key` is below
/// `min` (steady) and those where it is at or above it (bursts, e.g. of
/// texture streaming). Ticks, gauges and present events go with the window
/// written after them, which covers them; those after the last window are
/// left out of both.
pub fn split<'a>(sel: &Selected<'a>, key: &str, min: f64) -> (Selected<'a>, Selected<'a>) {
    let mut steady = Selected {
        run: sel.run,
        ..Selected::default()
    };
    let mut bursts = Selected {
        run: sel.run,
        ..Selected::default()
    };
    let Some(run) = sel.run else {
        return (steady, bursts);
    };
    // The selected windows in log order, with their run index and class.
    let mut windows: Vec<(usize, &Sample, bool)> = Vec::new();
    for (i, w) in run.windows.iter().enumerate() {
        if let Some(&frames) = sel.window_frames.get(&i) {
            let burst = w.get(key).is_some_and(|v| v >= min);
            windows.push((i, w, burst));
            let side = if burst { &mut bursts } else { &mut steady };
            side.windows.push(w);
            side.window_frames.insert(i, frames);
        }
    }
    let class_of = |line: usize| {
        windows
            .iter()
            .find(|(_, w, _)| w.line >= line)
            .map(|&(_, _, b)| b)
    };
    for t in &sel.ticks {
        match class_of(t.line) {
            Some(true) => bursts.ticks.push(t),
            Some(false) => steady.ticks.push(t),
            None => {}
        }
    }
    for g in &sel.gauges {
        match class_of(g.line) {
            Some(true) => bursts.gauges.push(g),
            Some(false) => steady.gauges.push(g),
            None => {}
        }
    }
    for h in &sel.hot {
        if let Some(i) = h.window {
            if bursts.window_frames.contains_key(&i) {
                bursts.hot.push(h);
            } else if steady.window_frames.contains_key(&i) {
                steady.hot.push(h);
            }
        }
    }
    // Present events have no line kept with them: by time, against the
    // windows' times.
    for &t in &sel.presents {
        let class = windows
            .iter()
            .find(|(_, w, _)| w.t.is_some_and(|wt| wt >= t))
            .map(|&(_, _, b)| b);
        match class {
            Some(true) => bursts.presents.push(t),
            Some(false) => steady.presents.push(t),
            None => {}
        }
    }
    for side in [&mut steady, &mut bursts] {
        let times: Vec<f64> = side.ticks.iter().filter_map(|s| s.t).collect();
        side.span_secs = times.first().zip(times.last()).map(|(a, b)| b - a);
        side.warnings = sel.warnings.clone();
    }
    (steady, bursts)
}

/// Per selected copystat window, its values with the frame time of the
/// frame-rate line written just before it in the same window (the last
/// tick it covers), for correlations.
pub fn window_frames_paired<'a>(sel: &Selected<'a>) -> Vec<(f64, &'a Sample)> {
    let mut out = Vec::new();
    let Some(run) = sel.run else { return out };
    let mut previous_line = 0;
    for (i, w) in run.windows.iter().enumerate() {
        if sel.window_frames.contains_key(&i)
            && let Some(ms) = sel
                .ticks
                .iter()
                .filter(|t| t.line > previous_line && t.line < w.line)
                .filter_map(|t| t.get("frame.ms"))
                .next_back()
        {
            out.push((ms, w));
        }
        previous_line = w.line;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{strip_ansi, timestamp};

    #[test]
    fn timestamps_parse_with_and_without_offsets() {
        let a = timestamp("2026-11-27T21:03:11.250000Z  INFO x: y").unwrap();
        let b = timestamp("2026-11-27T22:03:11.250+01:00 INFO x: y").unwrap();
        assert!((a - b).abs() < 1e-6);
        // 2026-11-27 is day 20,784 after 1970-01-01.
        assert!((a - (20_784.0 * 86_400.0 + 21.0 * 3600.0 + 3.0 * 60.0 + 11.25)).abs() < 1e-6);
        assert!(timestamp("Runtime C:\\x").is_none());
        assert!(timestamp("INFO no time").is_none());
    }

    #[test]
    fn ansi_sequences_are_removed() {
        let line = "\u{1b}[2m2026-11-27T21:03:11Z\u{1b}[0m \u{1b}[32m INFO\u{1b}[0m x";
        assert_eq!(strip_ansi(line), "2026-11-27T21:03:11Z  INFO x");
        assert!(timestamp(&strip_ansi(line)).is_some());
    }
}
