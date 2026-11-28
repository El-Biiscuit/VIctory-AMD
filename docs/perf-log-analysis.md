# Performance log analysis

`cargo xtask perf-log` reads the runtime's diagnostics logs and reports where frame time and memory traffic go. It can summarise one version or compare two. It only reads logs. It does not change the runtime and does not need the game or a graphics card.

The tool covers the memory measurements of the CPU/GPU memory work: CPU preparation, title memory copies, host copy refreshes, write-watch checks, GPU-to-CPU copy back, fence waits, settles, the relay, video memory and process memory.

## Capturing logs on Windows

1. Build the diagnostics release as usual:

   ```powershell
   cargo build --locked --release -p victory --features diagnostics --bins
   ```

   `--features diagnostics` turns on the copystat lines (`victory-hle/diagnostics` enables `victory-gpu/diagnostics`). A plain release build writes only the per-second lines and the frame-rate line.

2. Optional: log every present to get per-frame intervals as well as the 1 s window means. Set this in the PowerShell session that starts the launcher:

   ```powershell
   $env:VICTORY_LOG = "info,victory_gpu::frame=debug"
   ```

   The launcher passes the environment to the runtime. It sets its own `VICTORY_LOG` only for FSR or the texture copy queue. Neither is used in the Series X 1080p-to-1440p profile.

3. Keep these variables unset in both versions of a comparison, unless the comparison is about them:
   - `VICTORY_COPY_MODE`. Its `none` value skips copies, so its timings are not valid results.
   - `VICTORY_GPU_TIMING`. It turns off `VICTORY_LATE_WAIT` and the relay, which changes what the copy-back figures mean.
   - `VICTORY_BARRIER_TEST`. It removes waits and is for timing only.

   For `VICTORY_RELAY`, `VICTORY_LATE_WAIT` and `VICTORY_MEMORY_MAP`, use the same values in both runs.

4. Play the same sequence in each run: same save, same place, same camera path. Stay for at least 60 seconds after the scene has loaded. Do at least three runs per version.

5. Collect the logs from `%LOCALAPPDATA%\DexNom Runtime\logs`. `latest.txt` names the newest one. Do not commit logs to the repository.

## Running the tool

```powershell
# One version, one or more runs pooled
cargo xtask perf-log summary run-A1.log run-A2.log run-A3.log --min-draws 1000 --skip-secs 20 --duration-secs 60

# Two versions
cargo xtask perf-log compare --before run-A1.log run-A2.log run-A3.log `
                             --after  run-B1.log run-B2.log run-B3.log `
                             --min-draws 1000 --skip-secs 20 --duration-secs 60 `
                             --md compare.md --json compare.json --csv compare.csv
```

| Option | Meaning |
| --- | --- |
| `--min-draws N` | Start at the first frame-rate line with at least N draws per frame, once the scene is loaded. Without it, the selection starts at the first tick. |
| `--skip-reports N` | Leave out N frame-rate lines (one every 5 s) after the start. |
| `--skip-secs S` | Leave out S seconds after the start. This needs timestamps, which the default log format has. |
| `--duration-secs D` | Keep D seconds after the skipped part. |
| `--md`, `--json`, `--csv` | Write the report as Markdown, as JSON (all statistics, costs and hot ranges), or as CSV with one row per metric. |
| `--samples-csv` | Write every selected value in long form (log, kind, line, time, key, value, frames) for a spreadsheet. |
| `--all` | List every metric seen, not only the headline tables. |
| `--threshold-pct P` | Compare only. A median change smaller than P% (default 5) is reported as "same". |
| `--top N` | List N ranges per hot-range table. |

The tool reads UTF-8 and UTF-16 logs. Windows PowerShell 5 writes UTF-16 when output is redirected with `>`. ANSI colour codes and CRLF line endings are handled.

To try the tool without the game, write two synthetic logs and compare them:

```powershell
$env:VICTORY_PERFLOG_SYNTHETIC = "$env:TEMP\perflog"
cargo test -p xtask -- --ignored write_synthetic_logs
cargo xtask perf-log compare --before $env:TEMP\perflog\before.log --after $env:TEMP\perflog\after.log
```

## How the runtime writes its counters

Read the figures with this in mind (`crates/victory-gpu/src/host.rs`, `Meter::frame`):

- **Tick (about 1 s).** The per-second lines are the card's clock, settles, the relay, Direct3D queue calls, shader writes looked for on the CPU and title buffer copies. Their counters are reset every tick and divided by the tick's frames. A tick starts with the `indirect mesh dispatches` line.
- **Frame-rate line (every 5 s or more).** This is the `fps, ms/frame (… preparing … on the GPU … copying back …)` line, written with the card-copy and write-watch lines. Its per-frame figures cover **only the last tick**, not the 5 s since the previous frame-rate line.
- **Copystat window (diagnostics builds).** `copystat::log` follows the frame-rate line. Its counters run since the previous `copystat::log` (about 5 s) and are divided by the frames presented in that time. The `recording per frame (N frames)` line gives N, and the tool uses N to weight these windows.
- **Gauges** (`memory: …`, `video memory as the system counts it …`) are values at the moment of the line. "Made so far" and "since the start" are cumulative totals. For these the report also shows the last value minus the first over the selection.

With `--skip-secs`, `--min-draws` or `--skip-reports`, a copystat window is kept only if every frame it covers lies inside the selection.

## What adds up and what overlaps

Each duration has a timing class:

| Class | Meaning |
| --- | --- |
| `wall` | The frame's wall clock (`frame.ms`) and the card's clock (`clock.*`). |
| `submit-path` | On the thread that holds the submission lock, one after the other. `frame.cpu_ms` (preparing) and `frame.gpu_ms` (blocked on fences) add up, and their sum cannot exceed the frame time. |
| `copy-back` | `frame.back_ms`. With the relay (on by default, `VICTORY_RELAY`), the relay's own thread does it, **concurrently** with the submitting thread, so it is not added to the line above. Without the relay it is part of the submit path. The report says which case applies. |
| `concurrent` | The relay thread's own figures. |
| `thread-sum` | Copystat times summed over every thread that did the work. They can exceed the frame time. Compare them with each other or between versions, not with `frame.ms`. |
| `volume`, `gauge`, `cumulative` | Bytes or counts per frame, states, and totals since the start. |

Two consequences for the reference measurement (201.8 ms per frame; 147.1 ms preparing, 24.8 ms on the GPU, 133.6 ms copying back):

- The three parts add up to 305.5 ms, more than the frame. With the relay on, the submit path is 147.1 + 24.8 = 171.9 ms, or 85% of the frame. The 133.6 ms copy back overlaps it on the relay thread. The submitting thread waits for that copy back only through settles (`late.wait_ms`).
- "On the GPU" (`frame.gpu_ms`) is the time the CPU is blocked on fences inside submissions. It is not the card's work time. `clock.busy_pct` and `clock.idle_ms` measure the card. `clock.behind_ms` is how long the card sat idle while the submitting thread was still working, which shows the frame is CPU-bound.

## Other points to know

- **Units.** The `shader writes looked for on the CPU` line prints MiB (2^20 bytes) labelled "MB". Its keys end in `_mib`. Everything else labelled MB is 10^6 bytes, as printed. `back.written_pct` converts before dividing.
- **P95.** Without present events, medians and percentiles are taken over 1 s window means, not over single frames. Log the presents (step 2 above) for real per-frame P95 and P99.
- **Hot ranges.** Each copystat window lists only its largest ranges (6 to 10). Summed over the selection, they are lower bounds. A range missing from a window counts as zero there. The `refreshed_host_copy_ranges_bytes_asked` list (the runtime's `copied from …` lines, written by `Batch::update`) gives the bytes **asked for** per range. The write watch finds most of them unchanged. The bytes actually copied are `copies.changed_mb`.
- **Preparing, copies.** `frame.copy_ms` and `frame.copy_mb` (`copies … MB` in the frame-rate line) cover host copy refreshes (`Batch::update`, write-watch asking included) as well as the title's own buffer copies and fills (`copies.title_copy_mb`).
- **With the relay**, the `card_wait.*` classes stay at zero, because those fence waits are not classed on that path. Read `late.wait_ms` and `settle_wait.*` instead. `card.extra_wait_ms` and `relay.fetch_wait_ms` then count the same waits, on the relay thread.
- **Derived metrics.** `frame.cpu_other_ms` = preparing - copies - textures. It holds the recorded steps, refresh batching, snapshots and write-watch askings on the submit path. `frame.submit_path_ms`, `back.compared_total_mib`, `back.written_pct`, `copies.changed_mb` = checked - unchanged, `copies.unchanged_pct` and `vram.local_used_pct` are also derived. The report labels them.
- **The cost ranking** lists durations per frame (medians) with their parent aggregate and timing class. "Largest parts" takes the three deepest entries that have data. A thread-sum or concurrent entry is not on the frame's critical path as such, and the ranking says so.
- **Verdicts** in comparisons compare medians against a noise band. They are indications. A gain counts only once the responsible tester has confirmed it in the game on the target machine.

## Main keys

| Prefix | Line in the runtime | Source |
| --- | --- | --- |
| `frame.*` | `… fps, … ms/frame (… preparing …)` | `host.rs`, `Meter::frame` |
| `clock.*` | `the card's clock per frame` | `host.rs`, `card_clock_line` |
| `late.*`, `settle_wait.*` | `submissions sent ahead per frame`, `of the waiting for the card in settles` | `host.rs`, `late_line` |
| `relay.*`, `capture.*` | `the relay per frame`, `in-flight readback per frame` | `host.rs`, `late_line` |
| `back.*` | `shader writes looked for on the CPU per frame` | `shadow.rs`, `back_line` |
| `card.*` | `host copies in the card's memory` | `host.rs`, `Meter::frame` |
| `d3d.<queue>.*`, `joins.*`, `texrefresh.*`, `fills.*`, `card_passes.*`, `slow_watch.*` | per-tick lines | `host.rs`, `Meter::frame` |
| `copies.*`, `recording.*`, `account.*`, `whole.*`, `card_wait.*`, `heap.*`, `barrier_waits.*` | copystat lines | `copystat.rs`, `log` |
| `vram.*`, `memory.*`, `tex_memory.*`, `let_go.*`, `staging_pool.*`, `budget_change.*` | gauges | `host.rs`, `copystat.rs`, `log_memory` |

## Maintenance

The patterns are in `xtask/src/perflog/catalog.rs`. Each is the literal text of a `tracing::info!` format string with its placeholders named. The test `patterns_match_the_engine_messages` checks that every literal still appears in `crates/victory-gpu/src/{host,copystat,shadow}.rs`. Rewording a message without updating its pattern fails `cargo test --workspace`. If a log comes from a build whose messages differ, the report lists the affected lines under "Warnings" instead of silently leaving them out.

To add a line, add an `Entry` with the right `Scope`. If it should appear in the headline tables, the cost ranking or the comparison verdicts, also add its metrics to `META`, `SECTIONS` or `COSTS`.
