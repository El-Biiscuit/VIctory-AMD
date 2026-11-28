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
- **With the relay**, the `card_wait.*` classes stay at zero, because those fence waits are not classed on that path. Read `late.wait_ms` and `settle_wait.*` instead.
- **Card copy fetch waits.** `card.extra_wait_ms` (`CARD_STATS[6]`) is fed by two code paths:
  - `Host::relay_card_back`, on the relay thread. These microseconds also go into `relay.fetch_wait_ms` (`RELAY_STATS[3]`).
  - `Host::card_back`, on the submitting thread, for synchronous submissions and for settles without the relay.

  Its timing class is therefore `mixed`. The tool splits it:
  - `card.extra_wait_relay_ms` = min(total, `relay.fetch_wait_ms`), concurrent.
  - `card.extra_wait_sync_ms` = the rest, on the submitting thread.

  Without the relay, the whole total is the submitting thread's. Only the `_sync_` part is added to `frame.submit_path_ms`. Both parts belong to the copy back, not to the fence waits: `frame.gpu_ms` is measured before them. With the relay, `frame.back_ms` also includes the copy back of synchronous submissions, which runs on the submitting thread. The counters cannot separate that copy back from the relay's, except for its card copy fetch waits.
- **Derived metrics.** `frame.cpu_other_ms` = preparing - copies - textures. It holds the recorded steps, refresh batching, snapshots and write-watch askings on the submit path. `frame.submit_path_ms`, `back.compared_total_mib`, `back.written_pct`, `copies.changed_mb` = checked - unchanged, `copies.unchanged_pct` and `vram.local_used_pct` are also derived. The report labels them.
- **The cost ranking** lists durations per frame (medians) with their parent aggregate and timing class. "Largest parts" takes the three deepest entries that have data. A thread-sum or concurrent entry is not on the frame's critical path as such, and the ranking says so.
- **Verdicts** in comparisons compare medians against a noise band. They are indications. A gain counts only once the responsible tester has confirmed it in the game on the target machine.

## Guard protections (`protect.*`, `guard_waits.*`)

Title memory that the kernel cannot write-watch (views of physical pages) is guarded instead. After a copy takes a block, the block is made read-only with `VirtualProtect`. The first write faults into the handler in `crates/victory-gpu/src/watch.rs`, which opens the block again. Diagnostics builds count every one of these calls by cause, over the copystat window:

| Cause | Keys | Where it runs |
| --- | --- | --- |
| Guarding blocks a copy has just taken (one call per run), then block by block if the run's call failed | `protect.guard_*`, `protect.retry_*` | the thread checking a copy, usually the submitting thread |
| Guarding the other views of those pages (one call per block) | `protect.alias_*` | same |
| Opening for a write that faulted | `protect.fault_*` | the title's faulting thread |
| Opening for a write told (`writing`: the runtime's kernel reads, our writes into title memory) | `protect.write_*` | the title's thread, or ours |
| Opening the other views of an opened block | `protect.other_*` | with the opening above |
| Opening blocks of which a new view was mapped | `protect.mapped_*` | the runtime's mapping thread |

For each cause, the line gives:
- calls, blocks, failed calls and time, all summed over threads;
- the time spent on a thread inside a submission (`*_submitting_ms`). That thread holds the submission lock, so the frame waits for this part.
- for the causes that make one call per block, the runs of contiguous blocks. A run is what one call could cover.

`protect.calls_if_joined` adds up these runs, plus the runs of each check joined across blocks that are already guarded (`guard_waits.check_joined`). It is an upper bound on what joining calls could save: it does not say that joining is always allowed.

`guard_waits.*` gives:
- the write faults the handler took and their total time on the faulting threads;
- the waits for the guard lock: by the handler (the title's thread stands still), by checks, and by writes told.

The counting changes nothing in what the guards do. It costs two clock readings per call. `VICTORY_PROTECT_STATS=0` leaves it out. The Windows tests in `watch.rs` (`protection_changes_are_counted_by_cause`, `joinable_runs_and_submission_time_are_counted`, `failed_protections_are_counted`) check the counts against real views written through two aliases. They run with `cargo test -p victory-gpu --lib` on Windows and need no graphics card.

## Lock waits and holds (`lock_guards.*`, `lock_jv.*`, `lock_roles.*`, `lock_kinds.*`, `lock_extra.*`)

Diagnostics builds count, over the copystat window, how long three locks are waited for and held:
- `GUARDS`, the guard state of view memory (`watch.rs`);
- `JOURNAL`, the write-watch journal (`watch.rs`);
- `VIEWS`, the title's views (`inplace.rs`).

The counts are split by who takes the lock (`crate::lockstat`):

| Site | Keys | Who |
| --- | --- | --- |
| `GUARDS`, checks | `lock_guards.check_*` | `guard()`: guarding the blocks a copy has taken, and their other views |
| `GUARDS`, looks before checks | `lock_guards.known_*` | the test whether a range's first block is known |
| `GUARDS`, write faults | `lock_guards.fault_*` | the write-fault handler, on the faulting thread |
| `GUARDS`, writes told | `lock_guards.write_*` | `writing()`: the title's kernel reads (files, DirectStorage), our writes into title memory (relay, writebacks, copies) |
| `GUARDS`, views mapped and remaps | `lock_guards.mapped_*`, `lock_guards.remap_*` | mapping changes |
| `JOURNAL` | `lock_jv.jw_*` (write-watch askings, held across the kernel call and the guard check), `lock_jv.ja_*` (looks at askings ahead), `lock_jv.jk_*` (askings ahead), `lock_jv.jo_*` (other) | `watch.rs` |
| `VIEWS` | `lock_jv.vg_*` (taken while the thread holds `GUARDS`: alias lookups, part of that `GUARDS` hold), `lock_jv.vo_*` (other) | `inplace.rs` |

Per site, the line gives:
- times taken;
- time waiting, with the longest wait;
- time held, with the longest hold;
- how many waits went over 0.1 ms, and how many holds over 0.1 ms and over 1 ms.

Times are per frame. Longest waits and holds are for the whole window, not per frame. `*_queue_mean` is the mean number of threads already waiting when a thread asked for the lock, and `*_queue_max` the most seen at once.

`lock_roles.*` gives the same wait and hold times by kind of thread:

| Role | Keys | Meaning |
| --- | --- | --- |
| In submissions | `sub_*` | The submission lock is held: the frame waits for this time. |
| Title submitting threads outside their submissions | `title_*` | The title's render threads. A stall here delays the next submission. This is the title's critical path. |
| The relay | `relay_*` | The thread named `victory-relay`. Settles wait for it. |
| Other threads | `other_*` | Every other thread: the sum of the four kinds of `lock_kinds.*` below. |

`lock_kinds.*` splits the "other threads" into four kinds (line `lock time per frame by kind of other thread`):

| Kind | Keys | How the runtime knows it |
| --- | --- | --- |
| Title recording threads | `rec_*`, `fault_rec_*`, `write_rec_*` | The thread has made a list call (marked at its first one, in `copystat::recorded`) and has never submitted. Certain: only the title's code makes list calls. |
| Runtime workers | `run_*`, `fault_run_*`, `write_run_*` | The thread has a name the runtime gave it (`dstorage {i}`, `victory-wide-{i}`, `victory-pipelines-{i}`, `texture copy queue`, `card accounting`, `victory-late-wait`, `victory-memory-map`, …), other than `title-main`. Certain: the title's threads have no Rust name. |
| Other title threads | `tother_*`, `fault_tother_*`, `write_tother_*` | No runtime name (the title creates its threads itself, nameless), or `title-main`, and neither a list call nor a submission so far: the title's workers, file and streaming threads. Three nameless runtime threads would land here too (the input poller, the thread running the title's suspend routines, the `VICTORY_STACKS` sampler); none of them is expected to take these locks. |
| Unclassified | `unc_*`, `fault_unc_*`, `write_unc_*` | The kind was not known when the lock was taken. That only happens in the write-fault handler, which does not look up thread names (the lookup can allocate on a thread the runtime did not start). A thread lands here when its first lock taking is a fault, before any list call, submission or other lock taking. Title threads that only ever fault (they write guarded memory and never take another lock) stay here. |

For each kind the line gives:
- `GUARDS`, `JOURNAL` and `VIEWS` time waiting and held (`<kind>_guards_wait_ms`, …);
- for write faults and for writes told, the `GUARDS` takings and the wait (`fault_<kind>_taken`, `fault_<kind>_wait_ms`, `write_<kind>_taken`, `write_<kind>_wait_ms`).

**The kind is taken at each taking.** A thread's kind can change once: a nameless thread becomes a recording thread at its first list call, or a title submitter at its first submission. Its takings before that are counted under the kind it had then ("other title thread", or "unclassified" in the fault handler). To see how much that moves:
- each thread adds up its `GUARDS` waits counted as "other title thread" or "unclassified";
- at its first list call or submission, `lock_kinds.took_title_role` counts the thread, and `lock_kinds.waited_before_ms` adds those waits.

These two are totals since the previous line, not per frame. In steady play they should be 0: the recording threads start with the first frame. A non-zero value marks the windows where part of "other title threads" or "unclassified" belongs to a recording or submitting thread.

**Check that the split adds up.** `lock_kinds.guards_wait_gap_ms` is derived: `lock_roles.other_guards_wait_ms` minus the four `<kind>_guards_wait_ms`. It should only be the rounding of the printed values (a few hundredths of a millisecond).

`lock_extra.*` gives:
- the time checks spend asking Windows whether a range is view memory, before taking `GUARDS`;
- the time checks spend asking about uncommitted blocks, while holding `GUARDS`;
- the write faults that found their block already open (another thread opened it while they waited);
- the write faults' waits split into those on title submitting threads and those on other threads.

**Reading the lines.**
- **Wait** is time a thread could not run.
- **Hold** is time it kept others out.
- Long waits on one site with long holds on another site show who blocks whom.
- `title_*` and `sub_*` waits are the ones on the critical path. `other_*` waits may run beside the frame.
- Among them, `lock_kinds.rec_*` waits are on threads whose lists the next submission needs. `tother_*` and `run_*` waits only delay the frame if the title waits for those threads.

**Cost and switch.**
- The counting allocates, sorts and logs nothing while a lock is held or in the fault handler.
- Inside a lock it costs two clock readings. All the counts are added after the lock is released.
- The kind of thread is a few thread-local flags, read before the lock is taken. The name is looked up once per thread, outside the fault handler. A list call adds one thread-local read.
- `copystat` formats them at its own line.
- Measured outside Windows, in a Linux virtual machine: about 125 ns per take and release, most of it the clock. The test `lockstat::tests::counting_costs_little` prints the figure on any machine with `-- --nocapture`.
- `VICTORY_LOCK_STATS=0` turns this counting off. `VICTORY_PROTECT_STATS=0` turns off the `protect.*` counting. The two switches are independent.

## Steady periods and bursts (`cargo xtask perf-log regimes`)

`cargo xtask perf-log regimes <logs> [--burst-key KEY] [--burst-min X]` splits the selected copystat windows in two:
- **bursts**: the windows where `KEY` is at least `X` (by default `account.tex_uploads` >= 300, i.e. heavy texture streaming: in the reference log, quiet scenes record 5 to 45 uploads per frame, ordinary play 100 to 260, and streaming bursts 400 to 1,100);
- **steady**: the others.

Ticks, gauges and present events go with the window that covers them. The filters are the same as for `summary`.

For each headline metric, the report gives on both sides the median, the interquartile range (P75 - P25), the P95 and the coefficient of variation (standard deviation / mean), plus the change of the median from steady to bursts. It also gives each window metric's correlation (Pearson's r) with the frame time of the frame-rate line the window covers.

A correlation does not say which way the cause runs. Read it together with the tables by thread:
- if `title_*` and `sub_*` waits grow in bursts, the locks are on the critical path;
- if only `other_*` waits grow, the waits follow the streaming without holding up the frame.

`summary` and `compare` also give `p25`, `p75`, `iqr`, `stdev` and `cv` for every metric in their JSON and CSV output.

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
| `lock_guards.*`, `lock_jv.*`, `lock_roles.*`, `lock_kinds.*`, `lock_extra.*` | `GUARDS lock per frame`, `JOURNAL and VIEWS locks per frame`, `lock time per frame by thread`, `lock time per frame by kind of other thread`, `guard lock around checks and faults per frame` | `copystat.rs`, `log_locks` (counted in `lockstat.rs`) |
| `protect.*`, `guard_waits.*` | `guard protection changes per frame (VirtualProtect)`, `guard waits per frame` | `copystat.rs`, `log_protections` (counted in `watch.rs`, `stats`) |
| `vram.*`, `memory.*`, `tex_memory.*`, `let_go.*`, `staging_pool.*`, `budget_change.*` | gauges | `host.rs`, `copystat.rs`, `log_memory` |

## Maintenance

The patterns are in `xtask/src/perflog/catalog.rs`. Each is the literal text of a `tracing::info!` format string with its placeholders named. The test `patterns_match_the_engine_messages` checks that every literal still appears in `crates/victory-gpu/src/{host,copystat,shadow}.rs`. Rewording a message without updating its pattern fails `cargo test --workspace`. If a log comes from a build whose messages differ, the report lists the affected lines under "Warnings" instead of silently leaving them out.

To add a line, add an `Entry` with the right `Scope`. If it should appear in the headline tables, the cost ranking or the comparison verdicts, also add its metrics to `META`, `SECTIONS` or `COSTS`.
