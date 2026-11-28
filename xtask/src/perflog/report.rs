//! Summaries of selected runs, their comparison, and the Markdown, JSON
//! and CSV they are written as.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use super::catalog::{self, Better, COSTS, HotKind, SECTIONS, Timing};
use super::parse::{Selected, Selection};
use super::stats::{Stats, change_pct, stats};

/// What one run contributed.
#[derive(Debug, Clone)]
pub struct RunInfo {
    pub path: String,
    pub lines: usize,
    pub ticks: usize,
    pub reports: usize,
    pub windows: usize,
    pub window_frames: f64,
    pub gauges: usize,
    pub presents: usize,
    pub span_secs: Option<f64>,
    pub relay: bool,
    pub late: bool,
    pub header: Vec<(String, String)>,
    pub unparsed: Vec<(String, usize)>,
    /// The median of each metric in this run alone.
    pub medians: BTreeMap<String, f64>,
}

/// One address range of the per-range lists, over the selection.
#[derive(Debug, Clone)]
pub struct HotRow {
    pub kind: HotKind,
    pub range: String,
    pub memory: Option<String>,
    /// Per frame of the selected windows: times, MB, MB written back, ms.
    pub per_frame: f64,
    pub mb: f64,
    pub written_mb: Option<f64>,
    pub ms: Option<f64>,
    /// Windows that listed it, of those selected.
    pub listed: usize,
}

/// One cost of the ranking.
#[derive(Debug, Clone)]
pub struct CostRow {
    pub key: String,
    pub parent: Option<String>,
    /// How the cost relates to the frame (`submit-path`, `concurrent`...).
    pub nature: String,
    pub median: f64,
    pub p95: f64,
    /// The median as a share of the median frame time.
    pub share_pct: Option<f64>,
    /// Whether it has no child with data (the ranking compares these).
    pub leaf: bool,
}

/// Selected runs, summarised together.
#[derive(Debug, Clone)]
pub struct Summary {
    pub selection: String,
    pub runs: Vec<RunInfo>,
    pub metrics: BTreeMap<String, Stats>,
    /// Intervals between present events, in ms.
    pub frame_intervals: Option<Stats>,
    pub hot: Vec<HotRow>,
    pub costs: Vec<CostRow>,
    pub relay: bool,
    pub warnings: Vec<String>,
}

/// Values of every metric in `sel`, in log order, with their weights.
fn collect(sel: &Selected) -> BTreeMap<String, Vec<(f64, f64)>> {
    let mut all: BTreeMap<String, Vec<(f64, f64)>> = BTreeMap::new();
    for s in &sel.ticks {
        for (k, v) in &s.values {
            all.entry(k.clone()).or_default().push((*v, 0.0));
        }
    }
    for s in &sel.windows {
        let w = s.get("recording.frames").unwrap_or(0.0);
        for (k, v) in &s.values {
            all.entry(k.clone()).or_default().push((*v, w));
        }
    }
    for s in &sel.gauges {
        for (k, v) in &s.values {
            all.entry(k.clone()).or_default().push((*v, 0.0));
        }
    }
    all
}

fn intervals(sel: &Selected) -> Vec<(f64, f64)> {
    sel.presents
        .windows(2)
        .map(|w| ((w[1] - w[0]) * 1e3, 0.0))
        .collect()
}

fn hot_rows(selected: &[Selected]) -> Vec<HotRow> {
    // Sums over windows of value x frames, then divided by all selected
    // frames: a range missing from a window's list counts as zero there,
    // so these are lower bounds (each list holds the largest few only).
    type Key = (HotKind, String);
    let mut sums: BTreeMap<Key, (f64, f64, f64, f64, usize, Option<String>, bool, bool)> =
        BTreeMap::new();
    let mut frames = 0.0;
    for sel in selected {
        frames += sel.window_frames.values().sum::<f64>();
        for h in &sel.hot {
            let w = h
                .window
                .and_then(|i| sel.window_frames.get(&i))
                .copied()
                .unwrap_or(0.0);
            let e = sums
                .entry((h.kind, h.range.clone()))
                .or_insert((0.0, 0.0, 0.0, 0.0, 0, None, false, false));
            let v = |k: &str| h.values.get(k).copied().unwrap_or(0.0);
            e.0 += v("per_frame") * w;
            e.1 += v("mb") * w;
            if h.values.contains_key("written_mb") {
                e.2 += v("written_mb") * w;
                e.6 = true;
            }
            if h.values.contains_key("ms") {
                e.3 += v("ms") * w;
                e.7 = true;
            }
            e.4 += 1;
            if e.5.is_none() {
                e.5 = h.memory.clone();
            }
        }
    }
    if frames <= 0.0 {
        return Vec::new();
    }
    let mut rows: Vec<HotRow> = sums
        .into_iter()
        .map(
            |((kind, range), (n, mb, written, ms, listed, memory, has_w, has_ms))| HotRow {
                kind,
                range,
                memory,
                per_frame: n / frames,
                mb: mb / frames,
                written_mb: has_w.then_some(written / frames),
                ms: has_ms.then_some(ms / frames),
                listed,
            },
        )
        .collect();
    rows.sort_by(|a, b| a.kind.cmp(&b.kind).then(b.mb.total_cmp(&a.mb)));
    rows
}

fn cost_rows(metrics: &BTreeMap<String, Stats>, relay: bool) -> Vec<CostRow> {
    let frame = metrics.get("frame.ms").map(|s| s.median);
    let has = |k: &str| metrics.contains_key(k);
    let mut rows: Vec<CostRow> = COSTS
        .iter()
        .filter(|(key, _)| has(key))
        .map(|(key, parent)| {
            let s = metrics[*key];
            let leaf = !COSTS.iter().any(|(k, p)| *p == Some(*key) && has(k));
            let timing = catalog::timing(key, catalog::scope_of(key));
            let nature = nature(timing, relay).to_owned();
            CostRow {
                key: (*key).to_owned(),
                parent: parent.map(str::to_owned),
                nature,
                median: s.median,
                p95: s.p95,
                share_pct: frame.filter(|f| *f > 0.0).map(|f| 100.0 * s.median / f),
                leaf,
            }
        })
        .collect();
    rows.sort_by(|a, b| b.median.total_cmp(&a.median));
    rows
}

impl Summary {
    pub fn build(selection: &Selection, selected: &[Selected]) -> Summary {
        let mut pooled: BTreeMap<String, Vec<(f64, f64)>> = BTreeMap::new();
        let mut runs = Vec::new();
        let mut warnings = Vec::new();
        let mut all_intervals = Vec::new();
        for sel in selected {
            let Some(run) = sel.run else { continue };
            let values = collect(sel);
            let medians = values
                .iter()
                .filter_map(|(k, v)| stats(v).map(|s| (k.clone(), s.median)))
                .collect();
            for (k, v) in values {
                pooled.entry(k).or_default().extend(v);
            }
            all_intervals.extend(intervals(sel));
            warnings.extend(sel.warnings.iter().cloned());
            if !run.unparsed.is_empty() {
                warnings.push(format!(
                    "{}: lines with a known prefix that did not parse (message changed?): {}",
                    run.path,
                    run.unparsed
                        .iter()
                        .map(|(k, n)| format!("{k} x{n}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            runs.push(RunInfo {
                path: run.path.clone(),
                lines: run.lines,
                ticks: sel.ticks.len(),
                reports: sel
                    .ticks
                    .iter()
                    .filter(|s| s.get("frame.fps").is_some())
                    .count(),
                windows: sel.windows.len(),
                window_frames: sel.window_frames.values().sum(),
                gauges: sel.gauges.len(),
                presents: sel.presents.len(),
                span_secs: sel.span_secs,
                relay: run.relay(),
                late: run.late(),
                header: run.header.clone(),
                unparsed: run
                    .unparsed
                    .iter()
                    .map(|(k, n)| ((*k).to_owned(), *n))
                    .collect(),
                medians,
            });
        }
        let relay = runs.iter().any(|r| r.relay);
        if relay && runs.iter().any(|r| !r.relay) {
            warnings.push(
                "some runs had the relay and some did not: copy back times are not comparable"
                    .to_owned(),
            );
        }
        let metrics: BTreeMap<String, Stats> = pooled
            .iter()
            .filter_map(|(k, v)| stats(v).map(|s| (k.clone(), s)))
            .collect();
        let costs = cost_rows(&metrics, relay);
        Summary {
            selection: selection.describe(),
            runs,
            frame_intervals: stats(&all_intervals),
            hot: hot_rows(selected),
            costs,
            relay,
            metrics,
            warnings,
        }
    }

    fn median(&self, key: &str) -> Option<f64> {
        self.metrics.get(key).map(|s| s.median)
    }

    /// The three largest costs among the deepest parts that have data.
    pub fn top_costs(&self) -> Vec<&CostRow> {
        self.costs.iter().filter(|c| c.leaf).take(3).collect()
    }
}

/// A number for a table: fewer decimals as it grows.
pub fn num(v: f64) -> String {
    if !v.is_finite() {
        return "-".to_owned();
    }
    let a = v.abs();
    if a >= 1000.0 {
        format!("{v:.0}")
    } else if a >= 10.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    }
}

fn pct(v: Option<f64>) -> String {
    match v {
        Some(v) if v.is_finite() => format!("{v:+.1}%"),
        _ => "-".to_owned(),
    }
}

fn runs_table(out: &mut String, runs: &[RunInfo]) {
    out.push_str("| log | lines | span (s) | ticks | frame lines | copystat windows (frames) | presents | relay | late wait |\n");
    out.push_str("|---|---:|---:|---:|---:|---:|---:|---|---|\n");
    for r in runs {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} ({}) | {} | {} | {} |",
            file_name(&r.path),
            r.lines,
            r.span_secs.map_or("-".into(), num),
            r.ticks,
            r.reports,
            r.windows,
            num(r.window_frames),
            r.presents,
            if r.relay { "on" } else { "off" },
            if r.late { "on" } else { "off" },
        );
    }
    let settings: Vec<String> = runs
        .iter()
        .flat_map(|r| {
            r.header
                .iter()
                .map(move |(k, v)| format!("{}: {k} = {v}", file_name(&r.path)))
        })
        .collect();
    if !settings.is_empty() {
        out.push_str("\nSettings seen in the logs:\n\n");
        for s in settings {
            let _ = writeln!(out, "- {s}");
        }
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// How a cost of `timing` relates to the frame, given the relay's state.
pub fn nature(timing: Timing, relay: bool) -> &'static str {
    match timing {
        Timing::CopyBack if relay => "concurrent (relay thread)",
        Timing::Concurrent => "concurrent (relay thread)",
        Timing::CopyBack => "submit-path",
        Timing::Mixed if relay => "mixed (relay thread + submitting thread)",
        Timing::Mixed => "submit-path",
        t => t.name(),
    }
}

fn timing_label(key: &str, relay: bool) -> String {
    nature(catalog::timing(key, catalog::scope_of(key)), relay).to_owned()
}

impl Summary {
    /// The frame time split into what adds up and what overlaps.
    fn decomposition(&self, out: &mut String) {
        let Some(frame) = self.median("frame.ms") else {
            out.push_str("No frame-rate line was selected.\n");
            return;
        };
        let get = |k: &str| self.median(k).map_or("-".into(), num);
        let _ = writeln!(
            out,
            "Medians of the 1 s windows logged with the frame-rate line (a median of each figure, so the parts need not add up exactly):\n"
        );
        let _ = writeln!(
            out,
            "- Frame time: **{} ms** ({} fps).",
            num(frame),
            get("frame.fps")
        );
        let submit = self.median("frame.submit_path_ms");
        let _ = writeln!(
            out,
            "- Sequential on the submitting thread (submission lock held): CPU preparing {} ms + waiting for the card {} ms{} = {} ms{}.",
            get("frame.cpu_ms"),
            get("frame.gpu_ms"),
            if self.relay {
                format!(
                    " + card copy fetch waits of synchronous submissions {} ms",
                    get("card.extra_wait_sync_ms")
                )
            } else {
                " + copy back".to_owned()
            },
            submit.map_or("-".into(), num),
            submit.map_or(String::new(), |s| format!(
                ", {:.0}% of the frame",
                100.0 * s / frame
            )),
        );
        if self.relay {
            let _ = writeln!(
                out,
                "- Concurrent, on the relay thread: copy back {} ms (`VICTORY_RELAY` on). It overlaps the line above and is not added to it; \
                 the submitting thread waits for it only through settles ({} ms of fence waits in settles). \
                 The figure also holds the copy back of synchronous submissions, done on the submitting thread: \
                 of the card copy fetch waits, {} ms were the relay thread's and {} ms the submitting thread's.",
                get("frame.back_ms"),
                get("late.wait_ms"),
                get("card.extra_wait_relay_ms"),
                get("card.extra_wait_sync_ms"),
            );
        } else {
            let _ = writeln!(
                out,
                "- Copy back {} ms, on the submitting thread (relay off), included above.",
                get("frame.back_ms")
            );
        }
        let _ = writeln!(
            out,
            "- The card's clock: busy {}%, idle {} ms of which {} ms while the submitting thread was working (CPU bound); the thread stood waiting for the card {} ms.",
            get("clock.busy_pct"),
            get("clock.idle_ms"),
            get("clock.behind_ms"),
            get("clock.stood_ms"),
        );
        out.push_str("- \"Waiting for the card\" is the CPU blocked on fences, not the card's own work time; the card's clock above is the better measure of that.\n");
        out.push_str("- Copystat times (`copies.*`, `recording.*`, `account.*`) are summed over every thread and cover about 5 s each: compare them with each other, not with the frame time.\n");
        if self.relay
            && self
                .metrics
                .get("card_wait.under2")
                .is_some_and(|s| s.max == 0.0)
        {
            out.push_str("- With the relay, the `card_wait.*` classes stay at zero (those fence waits are not classed on that path): read `late.wait_ms` and `settle_wait.*` instead.\n");
        }
    }

    fn metric_rows(&self, out: &mut String, keys: &[&str]) {
        out.push_str("| metric | key | unit | timing | n | median | P95 | mean | min | max | last - first |\n");
        out.push_str("|---|---|---|---|---:|---:|---:|---:|---:|---:|---:|\n");
        for key in keys {
            let Some(s) = self.metrics.get(*key) else {
                continue;
            };
            let mean = s.weighted_mean.unwrap_or(s.mean);
            // Growth only means something for states and totals.
            let growth = match catalog::timing(key, catalog::scope_of(key)) {
                Timing::Gauge | Timing::Cumulative => num(s.last - s.first),
                _ => "-".to_owned(),
            };
            let _ = writeln!(
                out,
                "| {} | `{key}` | {} | {} | {} | {} | {} | {} | {} | {} | {growth} |",
                catalog::label(key).trim(),
                catalog::unit(key),
                timing_label(key, self.relay),
                s.n,
                num(s.median),
                num(s.p95),
                num(mean),
                num(s.min),
                num(s.max),
            );
        }
    }

    /// The summary as Markdown. `all` adds every metric seen.
    pub fn markdown(&self, all: bool, top: usize) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "# VIctory log summary\n\nSelection: {}.\n",
            self.selection
        );
        runs_table(&mut out, &self.runs);
        if !self.warnings.is_empty() {
            out.push_str("\nWarnings:\n\n");
            for w in &self.warnings {
                let _ = writeln!(out, "- {w}");
            }
        }
        out.push_str("\n## Frame time decomposition\n\n");
        self.decomposition(&mut out);
        if let Some(f) = &self.frame_intervals {
            let _ = writeln!(
                out,
                "\n## Frame intervals from present events\n\n{} intervals: median {} ms, P95 {} ms, P99 {} ms, max {} ms, mean {} ms.",
                f.n,
                num(f.median),
                num(f.p95),
                num(f.p99),
                num(f.max),
                num(f.mean)
            );
        } else {
            out.push_str("\nNo present events: P95 figures below are over 1 s window means, not single frames \
                          (log with `VICTORY_LOG=info,victory_gpu::frame=debug` for per-frame intervals).\n");
        }
        out.push_str("\n## Costs ranked (ms per frame, median)\n\n");
        out.push_str("| cost | key | part of | nature | median | P95 | share of frame |\n|---|---|---|---|---:|---:|---:|\n");
        for c in &self.costs {
            let _ = writeln!(
                out,
                "| {}{} | `{}` | {} | {} | {} | {} | {} |",
                catalog::label(&c.key).trim(),
                if c.leaf { "" } else { " (aggregate)" },
                c.key,
                c.parent.as_deref().unwrap_or("-"),
                c.nature,
                num(c.median),
                num(c.p95),
                c.share_pct.map_or("-".into(), |p| format!("{p:.0}%")),
            );
        }
        let top3 = self.top_costs();
        if !top3.is_empty() {
            out.push_str("\nLargest parts (deepest parts with data; thread sums and concurrent work are not on the frame's critical path as such):\n\n");
            for (i, c) in top3.iter().enumerate() {
                let _ = writeln!(
                    out,
                    "{}. {} (`{}`): {} ms median, {}.",
                    i + 1,
                    catalog::label(&c.key).trim(),
                    c.key,
                    num(c.median),
                    c.nature
                );
            }
        }
        for (title, keys) in SECTIONS {
            let _ = writeln!(out, "\n## {title}\n");
            self.metric_rows(&mut out, keys);
        }
        if !self.hot.is_empty() {
            out.push_str("\n## Hottest ranges (per frame over the selected copystat windows; lower bounds)\n");
            for kind in [HotKind::Compared, HotKind::Copied, HotKind::Whole] {
                let rows: Vec<&HotRow> = self
                    .hot
                    .iter()
                    .filter(|h| h.kind == kind)
                    .take(top)
                    .collect();
                if rows.is_empty() {
                    continue;
                }
                let _ = writeln!(
                    out,
                    "\n### {}\n\n| range | memory | times | MB | MB written back | ms | windows listed |\n|---|---|---:|---:|---:|---:|---:|",
                    kind.name()
                );
                for h in rows {
                    let _ = writeln!(
                        out,
                        "| 0x{} | {} | {} | {} | {} | {} | {} |",
                        h.range,
                        h.memory.as_deref().unwrap_or("-"),
                        num(h.per_frame),
                        num(h.mb),
                        h.written_mb.map_or("-".into(), num),
                        h.ms.map_or("-".into(), num),
                        h.listed
                    );
                }
            }
        }
        if all {
            out.push_str("\n## Every metric\n\n");
            let keys: Vec<&str> = self.metrics.keys().map(String::as_str).collect();
            self.metric_rows(&mut out, &keys);
        }
        out
    }
}

/// Two summaries side by side.
pub struct Comparison<'a> {
    pub before: &'a Summary,
    pub after: &'a Summary,
    /// Changes of the median smaller than this (percent) are called noise.
    pub threshold_pct: f64,
}

/// One metric before and after.
#[derive(Debug, Clone)]
pub struct Delta {
    pub key: String,
    pub before: Stats,
    pub after: Stats,
    pub median_pct: Option<f64>,
    pub p95_pct: Option<f64>,
    pub verdict: &'static str,
}

impl Comparison<'_> {
    pub fn deltas(&self) -> Vec<Delta> {
        let mut out = Vec::new();
        for (key, b) in &self.before.metrics {
            let Some(a) = self.after.metrics.get(key) else {
                continue;
            };
            let median_pct = change_pct(b.median, a.median);
            let verdict = match (median_pct, catalog::better(key)) {
                (Some(p), _) if p.abs() < self.threshold_pct => "same",
                (Some(p), Better::Lower) => {
                    if p < 0.0 {
                        "better"
                    } else {
                        "worse"
                    }
                }
                (Some(p), Better::Higher) => {
                    if p > 0.0 {
                        "better"
                    } else {
                        "worse"
                    }
                }
                (Some(_), Better::Neither) => "changed",
                (None, _) => "-",
            };
            out.push(Delta {
                key: key.clone(),
                before: *b,
                after: *a,
                median_pct,
                p95_pct: change_pct(b.p95, a.p95),
                verdict,
            });
        }
        out
    }

    pub fn markdown(&self, all: bool) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "# VIctory log comparison\n\nSelection: {}.\n\n## Before\n",
            self.before.selection
        );
        runs_table(&mut out, &self.before.runs);
        out.push_str("\n## After\n\n");
        runs_table(&mut out, &self.after.runs);
        let warnings: Vec<&String> = self
            .before
            .warnings
            .iter()
            .chain(&self.after.warnings)
            .collect();
        if self.before.relay != self.after.relay {
            out.push_str("\n**The relay was on in one side only: the copy back is not measured the same way.**\n");
        }
        if !warnings.is_empty() {
            out.push_str("\nWarnings:\n\n");
            for w in warnings {
                let _ = writeln!(out, "- {w}");
            }
        }
        let _ = writeln!(
            out,
            "\nVerdicts compare medians with a {}% noise band; they are indications, not proof. \
             Synthetic or single runs do not establish an FPS gain.\n",
            self.threshold_pct
        );
        let deltas = self.deltas();
        let by_key: BTreeMap<&str, &Delta> = deltas.iter().map(|d| (d.key.as_str(), d)).collect();
        let table = |out: &mut String, keys: &[&str]| {
            out.push_str("| metric | key | unit | before median | after median | change | before P95 | after P95 | change | n before/after | verdict |\n");
            out.push_str("|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|\n");
            for key in keys {
                let Some(d) = by_key.get(key) else { continue };
                let _ = writeln!(
                    out,
                    "| {} | `{key}` | {} | {} | {} | {} | {} | {} | {} | {}/{} | {} |",
                    catalog::label(key).trim(),
                    catalog::unit(key),
                    num(d.before.median),
                    num(d.after.median),
                    pct(d.median_pct),
                    num(d.before.p95),
                    num(d.after.p95),
                    pct(d.p95_pct),
                    d.before.n,
                    d.after.n,
                    d.verdict
                );
            }
        };
        if let (Some(b), Some(a)) = (&self.before.frame_intervals, &self.after.frame_intervals) {
            let _ = writeln!(
                out,
                "## Frame intervals from present events\n\n| | before | after | change |\n|---|---:|---:|---:|\n\
                 | median ms | {} | {} | {} |\n| P95 ms | {} | {} | {} |\n| P99 ms | {} | {} | {} |\n| intervals | {} | {} | |",
                num(b.median),
                num(a.median),
                pct(change_pct(b.median, a.median)),
                num(b.p95),
                num(a.p95),
                pct(change_pct(b.p95, a.p95)),
                num(b.p99),
                num(a.p99),
                pct(change_pct(b.p99, a.p99)),
                b.n,
                a.n
            );
        }
        for (title, keys) in SECTIONS {
            let _ = writeln!(out, "\n## {title}\n");
            table(&mut out, keys);
        }
        if self.before.runs.len() > 1 || self.after.runs.len() > 1 {
            out.push_str("\n## Medians per log (spread between runs)\n\n| key | before | after |\n|---|---|---|\n");
            for key in [
                "frame.ms",
                "frame.cpu_ms",
                "frame.gpu_ms",
                "frame.back_ms",
                "clock.busy_pct",
            ] {
                let side = |s: &Summary| {
                    s.runs
                        .iter()
                        .map(|r| r.medians.get(key).map_or("-".into(), |v| num(*v)))
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                let _ = writeln!(
                    out,
                    "| `{key}` | {} | {} |",
                    side(self.before),
                    side(self.after)
                );
            }
        }
        if all {
            out.push_str("\n## Every metric\n\n");
            let keys: Vec<&str> = deltas.iter().map(|d| d.key.as_str()).collect();
            table(&mut out, &keys);
        }
        out
    }
}

/// A JSON value, written by `Display`.
#[derive(Debug, Clone)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn num(v: f64) -> Json {
        if v.is_finite() {
            Json::Num(v)
        } else {
            Json::Null
        }
    }
    fn opt(v: Option<f64>) -> Json {
        v.map_or(Json::Null, Json::num)
    }
    fn str(s: impl Into<String>) -> Json {
        Json::Str(s.into())
    }
    fn obj(pairs: Vec<(&str, Json)>) -> Json {
        Json::Obj(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
    }
}

impl std::fmt::Display for Json {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Json::Null => f.write_str("null"),
            Json::Bool(b) => write!(f, "{b}"),
            Json::Num(v) => write!(f, "{v}"),
            Json::Str(s) => {
                f.write_str("\"")?;
                for c in s.chars() {
                    match c {
                        '"' => f.write_str("\\\"")?,
                        '\\' => f.write_str("\\\\")?,
                        '\n' => f.write_str("\\n")?,
                        '\r' => f.write_str("\\r")?,
                        '\t' => f.write_str("\\t")?,
                        c if (c as u32) < 0x20 => write!(f, "\\u{:04x}", c as u32)?,
                        c => write!(f, "{c}")?,
                    }
                }
                f.write_str("\"")
            }
            Json::Arr(items) => {
                f.write_str("[")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(",")?;
                    }
                    write!(f, "{item}")?;
                }
                f.write_str("]")
            }
            Json::Obj(pairs) => {
                f.write_str("{")?;
                for (i, (k, v)) in pairs.iter().enumerate() {
                    if i > 0 {
                        f.write_str(",")?;
                    }
                    write!(f, "{}:{v}", Json::Str(k.clone()))?;
                }
                f.write_str("}")
            }
        }
    }
}

fn stats_json(s: &Stats) -> Json {
    Json::obj(vec![
        ("n", Json::Num(s.n as f64)),
        ("mean", Json::num(s.mean)),
        ("weighted_mean", Json::opt(s.weighted_mean)),
        ("median", Json::num(s.median)),
        ("p95", Json::num(s.p95)),
        ("p99", Json::num(s.p99)),
        ("min", Json::num(s.min)),
        ("max", Json::num(s.max)),
        ("first", Json::num(s.first)),
        ("last", Json::num(s.last)),
        ("change_first_to_last", Json::num(s.last - s.first)),
    ])
}

impl Summary {
    pub fn json(&self) -> Json {
        let runs = self
            .runs
            .iter()
            .map(|r| {
                Json::obj(vec![
                    ("path", Json::str(r.path.clone())),
                    ("lines", Json::Num(r.lines as f64)),
                    ("ticks", Json::Num(r.ticks as f64)),
                    ("frame_lines", Json::Num(r.reports as f64)),
                    ("copystat_windows", Json::Num(r.windows as f64)),
                    ("copystat_frames", Json::num(r.window_frames)),
                    ("gauge_groups", Json::Num(r.gauges as f64)),
                    ("presents", Json::Num(r.presents as f64)),
                    ("span_secs", Json::opt(r.span_secs)),
                    ("relay", Json::Bool(r.relay)),
                    ("late_wait", Json::Bool(r.late)),
                    (
                        "settings",
                        Json::Obj(
                            r.header
                                .iter()
                                .map(|(k, v)| (k.clone(), Json::str(v.clone())))
                                .collect(),
                        ),
                    ),
                    (
                        "unparsed",
                        Json::Obj(
                            r.unparsed
                                .iter()
                                .map(|(k, n)| (k.clone(), Json::Num(*n as f64)))
                                .collect(),
                        ),
                    ),
                    (
                        "medians",
                        Json::Obj(
                            r.medians
                                .iter()
                                .map(|(k, v)| (k.clone(), Json::num(*v)))
                                .collect(),
                        ),
                    ),
                ])
            })
            .collect();
        let metrics = self
            .metrics
            .iter()
            .map(|(k, s)| {
                let mut o = vec![
                    ("label".to_owned(), Json::str(catalog::label(k).trim())),
                    ("unit".to_owned(), Json::str(catalog::unit(k))),
                    (
                        "scope".to_owned(),
                        Json::str(catalog::scope_name(catalog::scope_of(k))),
                    ),
                    ("timing".to_owned(), Json::str(timing_label(k, self.relay))),
                ];
                if let Json::Obj(pairs) = stats_json(s) {
                    o.extend(pairs);
                }
                (k.clone(), Json::Obj(o))
            })
            .collect();
        let costs = self
            .costs
            .iter()
            .map(|c| {
                Json::obj(vec![
                    ("key", Json::str(c.key.clone())),
                    ("parent", c.parent.clone().map_or(Json::Null, Json::Str)),
                    ("nature", Json::str(c.nature.clone())),
                    ("median_ms", Json::num(c.median)),
                    ("p95_ms", Json::num(c.p95)),
                    ("share_of_frame_pct", Json::opt(c.share_pct)),
                    ("leaf", Json::Bool(c.leaf)),
                ])
            })
            .collect();
        let hot = self
            .hot
            .iter()
            .map(|h| {
                Json::obj(vec![
                    ("list", Json::str(h.kind.name())),
                    ("range", Json::str(format!("0x{}", h.range))),
                    ("memory", h.memory.clone().map_or(Json::Null, Json::Str)),
                    ("times_per_frame", Json::num(h.per_frame)),
                    ("mb_per_frame", Json::num(h.mb)),
                    ("written_back_mb_per_frame", Json::opt(h.written_mb)),
                    ("ms_per_frame", Json::opt(h.ms)),
                    ("windows_listed", Json::Num(h.listed as f64)),
                ])
            })
            .collect();
        Json::obj(vec![
            ("tool", Json::str("cargo xtask perf-log")),
            ("format", Json::Num(1.0)),
            ("selection", Json::str(self.selection.clone())),
            ("relay", Json::Bool(self.relay)),
            ("runs", Json::Arr(runs)),
            (
                "warnings",
                Json::Arr(self.warnings.iter().map(|w| Json::str(w.clone())).collect()),
            ),
            (
                "frame_intervals_ms",
                self.frame_intervals.as_ref().map_or(Json::Null, stats_json),
            ),
            ("costs", Json::Arr(costs)),
            (
                "top_costs",
                Json::Arr(
                    self.top_costs()
                        .iter()
                        .map(|c| Json::str(c.key.clone()))
                        .collect(),
                ),
            ),
            ("metrics", Json::Obj(metrics)),
            ("hot_ranges", Json::Arr(hot)),
        ])
    }

    /// One row per metric.
    pub fn csv(&self) -> String {
        let mut out = String::from(
            "key,label,unit,scope,timing,n,mean,weighted_mean,median,p95,p99,min,max,first,last\n",
        );
        for (k, s) in &self.metrics {
            let row = [
                k.clone(),
                catalog::label(k).trim().to_owned(),
                catalog::unit(k).to_owned(),
                catalog::scope_name(catalog::scope_of(k)).to_owned(),
                timing_label(k, self.relay),
                s.n.to_string(),
                csv_num(s.mean),
                s.weighted_mean.map_or(String::new(), csv_num),
                csv_num(s.median),
                csv_num(s.p95),
                csv_num(s.p99),
                csv_num(s.min),
                csv_num(s.max),
                csv_num(s.first),
                csv_num(s.last),
            ];
            push_csv_row(&mut out, &row);
        }
        out
    }
}

impl Comparison<'_> {
    pub fn json(&self) -> Json {
        let deltas = self
            .deltas()
            .into_iter()
            .map(|d| {
                (
                    d.key.clone(),
                    Json::obj(vec![
                        ("before_median", Json::num(d.before.median)),
                        ("after_median", Json::num(d.after.median)),
                        ("median_change_pct", Json::opt(d.median_pct)),
                        ("before_p95", Json::num(d.before.p95)),
                        ("after_p95", Json::num(d.after.p95)),
                        ("p95_change_pct", Json::opt(d.p95_pct)),
                        ("verdict", Json::str(d.verdict)),
                    ]),
                )
            })
            .collect();
        Json::obj(vec![
            ("tool", Json::str("cargo xtask perf-log compare")),
            ("format", Json::Num(1.0)),
            ("threshold_pct", Json::num(self.threshold_pct)),
            ("before", self.before.json()),
            ("after", self.after.json()),
            ("deltas", Json::Obj(deltas)),
        ])
    }

    /// One row per metric present on both sides.
    pub fn csv(&self) -> String {
        let mut out = String::from(
            "key,label,unit,timing,before_n,before_median,before_p95,before_mean,after_n,after_median,after_p95,after_mean,median_change_pct,p95_change_pct,verdict\n",
        );
        for d in self.deltas() {
            let row = [
                d.key.clone(),
                catalog::label(&d.key).trim().to_owned(),
                catalog::unit(&d.key).to_owned(),
                timing_label(&d.key, self.after.relay),
                d.before.n.to_string(),
                csv_num(d.before.median),
                csv_num(d.before.p95),
                csv_num(d.before.weighted_mean.unwrap_or(d.before.mean)),
                d.after.n.to_string(),
                csv_num(d.after.median),
                csv_num(d.after.p95),
                csv_num(d.after.weighted_mean.unwrap_or(d.after.mean)),
                d.median_pct.map_or(String::new(), csv_num),
                d.p95_pct.map_or(String::new(), csv_num),
                d.verdict.to_owned(),
            ];
            push_csv_row(&mut out, &row);
        }
        out
    }
}

fn csv_num(v: f64) -> String {
    if v.is_finite() {
        format!("{v}")
    } else {
        String::new()
    }
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}

fn push_csv_row(out: &mut String, row: &[String]) {
    let fields: Vec<String> = row.iter().map(|f| csv_field(f)).collect();
    out.push_str(&fields.join(","));
    out.push('\n');
}

/// Every selected value in long form: one row per sample and metric.
pub fn samples_csv(selected: &[Selected]) -> String {
    let mut out = String::from("log,kind,line,time_s,key,value,frames\n");
    for sel in selected {
        let Some(run) = sel.run else { continue };
        let t0 = sel.ticks.iter().find_map(|s| s.t);
        let groups: [(&str, &Vec<&super::parse::Sample>); 3] = [
            ("tick", &sel.ticks),
            ("copystat", &sel.windows),
            ("gauge", &sel.gauges),
        ];
        for (kind, samples) in groups {
            for s in samples.iter() {
                let time = match (s.t, t0) {
                    (Some(t), Some(t0)) => csv_num(t - t0),
                    _ => String::new(),
                };
                let frames = if kind == "copystat" {
                    s.get("recording.frames").map_or(String::new(), csv_num)
                } else {
                    String::new()
                };
                for (k, v) in &s.values {
                    let row = [
                        run.path.clone(),
                        kind.to_owned(),
                        s.line.to_string(),
                        time.clone(),
                        k.clone(),
                        csv_num(*v),
                        frames.clone(),
                    ];
                    push_csv_row(&mut out, &row);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Json, csv_field, num};

    #[test]
    fn json_escapes_and_drops_non_finite_numbers() {
        let j = Json::obj(vec![
            ("a\"b", Json::str("x\\y\n\u{1}")),
            ("n", Json::num(f64::NAN)),
            (
                "v",
                Json::Arr(vec![Json::Num(1.5), Json::Bool(true), Json::Null]),
            ),
        ]);
        assert_eq!(
            j.to_string(),
            r#"{"a\"b":"x\\y\n\u0001","n":null,"v":[1.5,true,null]}"#
        );
    }

    #[test]
    fn csv_fields_are_quoted_when_needed() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn numbers_lose_decimals_as_they_grow() {
        assert_eq!(num(1234.4), "1234");
        assert_eq!(num(201.84), "201.8");
        assert_eq!(num(5.0), "5.00");
        assert_eq!(num(f64::NAN), "-");
    }
}
