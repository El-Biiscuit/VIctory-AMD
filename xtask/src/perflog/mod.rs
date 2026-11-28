//! `cargo xtask perf-log`: reads the runtime's diagnostics logs and says
//! where frame time and memory traffic go, for one version or two.
//!
//! The runtime writes its counters as text every second and every five
//! seconds (see `catalog`). This tool turns those lines into samples,
//! keeps per-frame averages, gauges and totals apart, tells durations on the
//! submitting thread from those on other threads, and writes medians and
//! 95th percentiles as Markdown, JSON or CSV. It changes nothing in the
//! runtime. See docs/perf-log-analysis.md.

mod catalog;
mod parse;
mod report;
mod stats;
mod template;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

pub use parse::Selection;

#[derive(Subcommand)]
pub enum Cmd {
    /// Summarise one version: one or more logs of comparable sequences,
    /// pooled.
    Summary {
        /// Runtime logs (`logs\run-*.log` of the launcher, or stderr).
        #[arg(required = true)]
        logs: Vec<PathBuf>,
        #[command(flatten)]
        opts: Opts,
    },
    /// Compare two versions: the logs of each side are pooled.
    Compare {
        /// Logs of the reference version.
        #[arg(long, required = true, num_args = 1..)]
        before: Vec<PathBuf>,
        /// Logs of the changed version.
        #[arg(long, required = true, num_args = 1..)]
        after: Vec<PathBuf>,
        /// Median changes smaller than this (percent) are reported as "same".
        #[arg(long, default_value_t = 5.0)]
        threshold_pct: f64,
        #[command(flatten)]
        opts: Opts,
    },
}

#[derive(Args, Clone)]
pub struct Opts {
    /// Seconds to leave out after the start (warm-up, loading).
    #[arg(long, default_value_t = 0.0)]
    skip_secs: f64,
    /// Seconds to keep after the skipped part (all when absent).
    #[arg(long)]
    duration_secs: Option<f64>,
    /// Frame-rate lines (one per 5 s) to leave out after the start.
    #[arg(long, default_value_t = 0)]
    skip_reports: usize,
    /// Start at the first frame-rate line with at least this many draws a
    /// frame (the scene is loaded).
    #[arg(long)]
    min_draws: Option<f64>,
    /// Also write the Markdown report to this file.
    #[arg(long)]
    md: Option<PathBuf>,
    /// Write the summary as JSON to this file.
    #[arg(long)]
    json: Option<PathBuf>,
    /// Write one CSV row per metric to this file.
    #[arg(long)]
    csv: Option<PathBuf>,
    /// Write every selected sample value (long form CSV) to this file.
    #[arg(long)]
    samples_csv: Option<PathBuf>,
    /// List every metric seen, not only the headline ones.
    #[arg(long)]
    all: bool,
    /// Ranges listed per hot-range table.
    #[arg(long, default_value_t = 8)]
    top: usize,
}

impl Opts {
    fn selection(&self) -> Selection {
        Selection {
            skip_secs: self.skip_secs,
            duration_secs: self.duration_secs,
            skip_reports: self.skip_reports,
            min_draws: self.min_draws,
        }
    }
}

/// A log as text: UTF-8, or UTF-16 with a byte-order mark (what Windows
/// PowerShell 5 writes when output is redirected with `>`).
pub fn decode(bytes: &[u8]) -> String {
    let utf16 = |le: bool| {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| {
                if le {
                    u16::from_le_bytes([c[0], c[1]])
                } else {
                    u16::from_be_bytes([c[0], c[1]])
                }
            })
            .collect();
        String::from_utf16_lossy(&units)
    };
    match bytes {
        [0xFF, 0xFE, ..] => utf16(true),
        [0xFE, 0xFF, ..] => utf16(false),
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

fn load(paths: &[PathBuf]) -> Result<Vec<parse::Run>> {
    paths
        .iter()
        .map(|p| {
            let bytes = std::fs::read(p).with_context(|| format!("reading {}", p.display()))?;
            Ok(parse::parse(&p.display().to_string(), &decode(&bytes)))
        })
        .collect()
}

fn write(path: &Path, text: &str) -> Result<()> {
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
    eprintln!("wrote {}", path.display());
    Ok(())
}

fn summarise<'a>(
    runs: &'a [parse::Run],
    sel: &Selection,
) -> (report::Summary, Vec<parse::Selected<'a>>) {
    let selected: Vec<parse::Selected> = runs.iter().map(|r| parse::select(r, sel)).collect();
    (report::Summary::build(sel, &selected), selected)
}

pub fn run(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Summary { logs, opts } => {
            let runs = load(&logs)?;
            if runs.iter().all(|r| r.ticks.is_empty()) {
                bail!(
                    "no runtime counter lines found in the given logs (is victory_gpu::host logging at info?)"
                );
            }
            let (summary, selected) = summarise(&runs, &opts.selection());
            let md = summary.markdown(opts.all, opts.top);
            print!("{md}");
            if let Some(p) = &opts.md {
                write(p, &md)?;
            }
            if let Some(p) = &opts.json {
                write(p, &format!("{}\n", summary.json()))?;
            }
            if let Some(p) = &opts.csv {
                write(p, &summary.csv())?;
            }
            if let Some(p) = &opts.samples_csv {
                write(p, &report::samples_csv(&selected))?;
            }
            Ok(())
        }
        Cmd::Compare {
            before,
            after,
            threshold_pct,
            opts,
        } => {
            let (b_runs, a_runs) = (load(&before)?, load(&after)?);
            for (side, runs) in [("before", &b_runs), ("after", &a_runs)] {
                if runs.iter().all(|r| r.ticks.is_empty()) {
                    bail!("no runtime counter lines found in the {side} logs");
                }
            }
            let sel = opts.selection();
            let (b, b_sel) = summarise(&b_runs, &sel);
            let (a, a_sel) = summarise(&a_runs, &sel);
            let cmp = report::Comparison {
                before: &b,
                after: &a,
                threshold_pct,
            };
            let md = cmp.markdown(opts.all);
            print!("{md}");
            if let Some(p) = &opts.md {
                write(p, &md)?;
            }
            if let Some(p) = &opts.json {
                write(p, &format!("{}\n", cmp.json()))?;
            }
            if let Some(p) = &opts.csv {
                write(p, &cmp.csv())?;
            }
            if let Some(p) = &opts.samples_csv {
                let mut all = b_sel;
                all.extend(a_sel);
                write(p, &report::samples_csv(&all))?;
            }
            Ok(())
        }
    }
}
