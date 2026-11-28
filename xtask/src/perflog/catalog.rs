//! The runtime's periodic log lines this tool reads, and what their numbers
//! mean. Every pattern is the literal text of a `tracing::info!` format
//! string in `crates/victory-gpu` with its placeholders named; the test at
//! the bottom checks that the literals still appear in those sources, so a
//! changed message fails here instead of silently dropping out of reports.
//!
//! When these lines are written (`crates/victory-gpu/src/host.rs`,
//! `Meter::frame`): once a second (a *tick*) the per-tick lines, with
//! counters taken since the previous tick and divided by its frames; every
//! five seconds or more, in the same tick, the frame-rate line and the lines
//! next to it, whose per-frame figures are still those of that one tick;
//! then, in diagnostics builds, `copystat::log`, whose counters run since the
//! previous `copystat::log` (about five seconds) and are divided by the
//! frames presented since then.

/// How a line's numbers relate to time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Per-frame averages over the last tick; this line opens a tick.
    TickStart,
    /// Per-frame averages over the last tick (about one second).
    Tick,
    /// Written only with the frame-rate line, per-frame averages over the
    /// last tick all the same (and a few gauges, named in [`META`]).
    Report,
    /// Per-frame averages since the previous `copystat::log`; this line
    /// opens such a window.
    CopyStart,
    /// Per-frame averages since the previous `copystat::log`.
    Copy,
    /// Values at the moment of the line, or totals since the start.
    Gauge,
    /// One line per address range, repeated in each `copystat::log`.
    Hot(HotKind),
}

/// The per-range lists of `copystat::log`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HotKind {
    /// Compared after submissions, with the bytes put back.
    Compared,
    /// Copied into host copies, by memory type.
    Copied,
    /// Copied whole because the memory is not write-watched.
    Whole,
}

impl HotKind {
    pub fn name(self) -> &'static str {
        match self {
            HotKind::Compared => "compared_after_submissions",
            HotKind::Copied => "copied_into_host_copies",
            HotKind::Whole => "copied_whole_unwatched",
        }
    }
}

/// One known line.
pub struct Entry {
    /// The prefix of every metric key from this line.
    pub id: &'static str,
    pub scope: Scope,
    pub pattern: &'static str,
}

/// The lines, most specific first where anchors overlap.
pub const ENTRIES: &[Entry] = &[
    // --- Every tick (host.rs, Meter::frame) ---
    Entry {
        id: "mesh",
        scope: Scope::TickStart,
        pattern: "indirect mesh dispatches: the largest group count asked for {largest} (run: at most 65535), {groups} groups asked per frame",
    },
    Entry {
        id: "late",
        scope: Scope::Tick,
        pattern: "submissions sent ahead per frame: {sent} in {settles} settles ({flights_each} flights each time), {wait_ms} ms waiting for the card in them; \
                  settled for: a synchronous submission or present {why_sync}, the card had run them {why_card_done}, a wait for a memory value {why_value}, \
                  title memory changed under shader writes {why_stale}, title memory read {why_read} (by a buffer copy {why_read_copy}, a fill of upload memory {why_read_fill}, \
                  a texture {why_read_texture}), a step of ours {why_run}, {_} lists in the air {why_most}, \
                  bottom-of-pipe writes (VICTORY_LATE_WAIT_WRITES=0) {why_writes}, a blocking fence wait {why_fence}; \
                  {values_met} waits for memory values met by a flight's bottom-of-pipe write, {signals_behind} fence signals set behind flights",
    },
    Entry {
        id: "settle_wait",
        scope: Scope::Tick,
        pattern: "of the waiting for the card in settles, per frame, by what settled: a synchronous submission or present {sync_ms} ms, \
                  the card had run them {card_done_ms}, a wait for a memory value {value_ms}, title memory changed under shader writes {stale_ms}, \
                  title memory read {read_ms}, a step of ours {run_ms}, lists in the air {most_ms}, bottom-of-pipe writes {writes_ms}, a blocking fence wait {fence_ms}",
    },
    Entry {
        id: "relay",
        scope: Scope::Tick,
        pattern: "the relay per frame (VICTORY_RELAY=1): {flights} flights landed in {landings} landings by the thread that copies back, \
                  beside the submitting thread; that thread waited {idle_wait_ms} ms for the card with nothing to land and {fetch_wait_ms} ms for \
                  fetches of card copies' pieces, and was {landing_ms} ms landing in all",
    },
    Entry {
        id: "capture",
        scope: Scope::Tick,
        pattern: "in-flight readback per frame: {landings} captured landings, {fetches} queued fetches, {published_mib} MiB published",
    },
    Entry {
        id: "clock",
        scope: Scope::Tick,
        pattern: "the card's clock per frame: nothing of ours queued {idle_ms} ms (the card about {busy_pct}% busy by this count), \
                  {behind_ms} ms of that while the submitting thread was working (the card idle behind us), the rest while the title \
                  had not submitted; the submitting thread stood waiting for the card {stood_ms} ms",
    },
    Entry {
        id: "d3d",
        scope: Scope::Tick,
        pattern: "Direct3D {=queue} queue calls: {calls}/frame, {execute_ms} ms/frame in ExecuteCommandLists, longest call {longest_ms} ms",
    },
    Entry {
        id: "back",
        scope: Scope::Tick,
        pattern: "shader writes looked for on the CPU per frame: {windows_compared_mib} MB compared in {windows_ranges} ranges of windows outside buffers ({windows_snapshot_mib} MB \
                  snapshotted), {buffers_compared_mib} MB in {buffers_ranges} ranges of title buffers' copies ({buffers_snapshot_mib} MB snapshotted), of them {big_copies_mib} MB in copies \
                  of 1 MiB or more",
    },
    Entry {
        id: "card_passes",
        scope: Scope::Tick,
        pattern: "our passes over copies in the card's memory per frame: {hash_passes} hash passes over {pieces} pieces after ranges were sent up, \
                  {saved} redundant hash dispatches avoided, {compares} compare passes",
    },
    Entry {
        id: "texrefresh",
        scope: Scope::Tick,
        pattern: "texture refreshes per frame, all threads: {all_ms} ms, of them {other_thread_ms} ms waiting for a texture another thread is refreshing, \
                  {asking_ms} ms asking the write watch and the guards, {still_ms} ms waiting for title memory to hold still",
    },
    Entry {
        id: "joins",
        scope: Scope::Tick,
        pattern: "title buffer copies recorded per frame: {copies}, of them {joined} continue the one before (VICTORY_JOIN_COPIES=1 makes those one copy with it: now {$state})",
    },
    Entry {
        id: "fills",
        scope: Scope::Tick,
        pattern: "parallel upload preparation per frame: {batches} batches, {fills} fills",
    },
    // --- With the frame-rate line, every five seconds or more ---
    Entry {
        id: "slow_watch",
        scope: Scope::Report,
        pattern: "write-watch askings of a millisecond or more: {asks} a frame, {total_ms} ms a frame; the longest asking {longest_ms} ms",
    },
    Entry {
        id: "card",
        scope: Scope::Report,
        pattern: "host copies in the card's memory: {copies} copies {alive_mb} MB, their bytes from before {before_mb} MB; per frame: \
                  {sent_mb} MB sent up in {sent_ranges} ranges, {pieces_compared} pieces compared on the card, {dirty_fetched} dirty ranges fetched \
                  ({fetched_mb} MB) with {extra_waits} more waits for the card ({extra_wait_ms} ms), {readback_copies} readback copy commands; \
                  relay joined {joined_ranges} dirty ranges and avoided {avoided_mb} MB of overlapping reads",
    },
    Entry {
        id: "frame",
        scope: Scope::Report,
        pattern: "{fps} fps, {ms} ms/frame ({cpu_ms} preparing: copies {copy_ms} ms {copy_mb} MB, textures {texture_ms} ms {texture_mb} MB; {gpu_ms} on the \
                  GPU; {back_ms} copying back {back_mb} MB), {draws} draws/frame, dropped {dropped} without a host pipeline ({making} being made) and \
                  {indirect} indirect; heap buffers {heap_passes} passes {heap_ms} ms;{**}cpu {cpu_pct}% gpu {gpu_pct}% busy ram {ram_mb} MB",
    },
    // --- copystat::log (diagnostics builds), since the previous one ---
    Entry {
        id: "copies",
        scope: Scope::CopyStart,
        pattern: "copies per frame: {submissions} submissions; {title_copies} title copies {title_copy_mb} MB; {refreshes} refreshes {refresh_mb} MB, \
                  {skipped} skipped {skipped_mb} MB; {runs} runs {run_ms} ms; {queries} queries {query_ms} ms, moves {move_ms} ms, \
                  {unmapped_mb} MB unmapped; {page_faults} page faults; {watches} watches {watch_ms} ms, {checked_mb} MB checked, {unchanged_mb} MB \
                  unchanged; {fallbacks} fallbacks; {guard_faults} guard faults; textures {tex_watched_mb} MB watched and \
                  changed, {tex_hashed_mb} MB hashed, {tex_changed_mb} MB of them changed",
    },
    Entry {
        id: "recording",
        scope: Scope::Copy,
        pattern: "recording per frame ({frames} frames): {list_calls} list calls {list_ms} ms (all recording \
                  threads); {pipeline_wait_ms} ms waiting for pipelines; dynamic slots: {dyn_asked} tables asked, {dyn_refused} \
                  refused, {dyn_ms} ms searching; notes {notes_ms} ms; shader-written ranges: {gpu_write_steps} steps in \
                  {back_submissions} submissions, snapshots {snapshot_ms} ms, {back_compared_mb} MB compared back; sparse reads {sparse_ms} ms; \
                  guards {guard_ms} ms; {whole_mb} MB copied whole (memory not watched); {frame_waits} frame waits \
                  sleeping {pace_ms} ms (all threads); ranges of shaders that cannot store to buffers: \
                  {left_out_steps} steps {left_out_mb} MB left out",
    },
    Entry {
        id: "account",
        scope: Scope::Copy,
        pattern: "frame account per frame: after a present {until_record_ms} ms until the first list call and {until_submit_ms} ms \
                  until the first submission; {tail_ms} ms from the last submission to the present call; \
                  lists reset in {reset_ms} ms; the process ran {user_ms} ms user + {kernel_ms} ms kernel on \
                  all threads; what shaders wrote: {back_scan_ms} ms finding {back_differ} pieces that differ, {back_sync_ms} ms \
                  putting them back; texture uploads: {tex_uploads} recorded, {tex_skipped} left out; {forced} held \
                  submissions run at the present with their waits not met; waits for memory values: \
                  {wait_looks} looks {wait_look_ms} ms; {host_lists} host lists made {host_list_ms} ms; {parts_alone} parts of held submissions \
                  ran alone",
    },
    Entry {
        id: "whole",
        scope: Scope::Copy,
        pattern: "copied whole (memory not watched): {whole_ms} ms per frame in {ranges} ranges",
    },
    Entry {
        id: "card_wait",
        scope: Scope::Copy,
        pattern: "waiting for the card per frame: {under2} submissions under 2 ms ({under2_ms} ms), {from2to8} of 2-8 ms \
                  ({from2to8_ms} ms), {from8to32} of 8-32 ms ({from8to32_ms} ms), {over32} longer ({over32_ms} ms); {nothing_back} submissions hand \
                  nothing back to the title ({nothing_back_ms} ms of the waiting); the longest since the last line: \
                  {longest_ms} ms",
    },
    Entry {
        id: "barrier_waits",
        scope: Scope::Copy,
        pattern: "waits for every earlier shader write, per frame: {groups} for the title's barrier groups, {buffers} \
                  for its barriers on buffers; {indirect_bounded} indirect commands bounded",
    },
    Entry {
        id: "heap",
        scope: Scope::Copy,
        pattern: "heap streaming per frame: {view_resolve_ms} ms resolving buffer views, {buffer_refresh_ms} ms preparing buffer refreshes, \
                  {texture_stage_ms} ms staging textures",
    },
    Entry {
        id: "compared_ranges",
        scope: Scope::Copy,
        pattern: "compared after submissions: {ranges} ranges",
    },
    Entry {
        id: "hot_whole",
        scope: Scope::Hot(HotKind::Whole),
        pattern: "copied whole from 0x{$range}: {per_frame}x/frame, {mb} MB/frame, {ms} ms/frame",
    },
    Entry {
        id: "hot_compared",
        scope: Scope::Hot(HotKind::Compared),
        pattern: "compared 0x{$range}: {per_frame}x/frame, {mb} MB/frame, {written_mb} MB/frame of it written back",
    },
    Entry {
        id: "hot_copied",
        scope: Scope::Hot(HotKind::Copied),
        pattern: "copied from 0x{$range} (memory {$kind}): {per_frame}x/frame, {mb} MB/frame, {ms} ms/frame",
    },
    // --- Gauges: the state at the moment of the line ---
    Entry {
        id: "vram",
        scope: Scope::Gauge,
        pattern: "video memory as the system counts it, {_}: the card's own: budget {local_budget_mb} MB, in use {local_used_mb} MB, \
                  {local_available_mb} MB could still be reserved, {local_reserved_mb} MB reserved; the PC's shared with the card: budget \
                  {shared_budget_mb} MB, in use {shared_used_mb} MB",
    },
    Entry {
        id: "budget_change",
        scope: Scope::Gauge,
        pattern: "the system changed the budget of the card's memory from {from_mb} MB to {to_mb} MB",
    },
    Entry {
        id: "tex_memory",
        scope: Scope::Gauge,
        pattern: "memory of textures: {no_copy} title textures have no host copy yet ({no_copy_mb} MB of title memory);{**}the card: \
                  {card_used_mb} MB in use of a budget of {card_budget_mb} MB, {card_shared_mb} MB of the PC's memory shared with it in use",
    },
    Entry {
        id: "memory",
        scope: Scope::Gauge,
        pattern: "memory: {private_mb} MB private ({working_mb} MB of it in memory; the PC has {free_mb} MB free and {commit_left_mb} MB \
                  left to commit); host copies of title memory: {host_copies} alive {host_copies_mb} MB, of them {windows} windows \
                  outside buffers {windows_mb} MB; snapshot buffers kept: {snapshots} reserving {snapshots_mb} MB; staging of \
                  textures: {stagings} alive {stagings_mb} MB, {stagings_kept} kept by their texture; host textures: {host_textures} alive {host_textures_mb} MB, \
                  {placed_textures} placed in title memory; made so far: {upload_buffers} upload buffers {upload_buffers_mb} MB, {shared_buffers} shared buffers \
                  {shared_buffers_mb} MB, {card_buffers} card buffers {card_buffers_mb} MB",
    },
    Entry {
        id: "let_go",
        scope: Scope::Gauge,
        pattern: "memory let go since the start: {windows} windows outside buffers {windows_mb} MB, {stagings} staging buffers \
                  {stagings_mb} MB",
    },
    Entry {
        id: "staging_pool",
        scope: Scope::Gauge,
        pattern: "texture staging pool since startup: {created} chunks made, {reused} allocations served by existing chunks",
    },
];

/// How a duration relates to the frame's wall clock. Durations of different
/// kinds must not be added up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timing {
    /// The frame's wall clock.
    Wall,
    /// On the thread that holds the submission lock, one after another:
    /// these add up, and their sum cannot exceed the frame time.
    SubmitPath,
    /// The copy back: on the submitting thread without the relay, on the
    /// relay's own thread (beside the submitting thread) with it.
    CopyBack,
    /// On the relay's thread, beside the submitting thread.
    Concurrent,
    /// Summed over every thread that did it: may exceed the frame time.
    ThreadSum,
    /// Bytes or counts per frame.
    Volume,
    /// A value at a moment.
    Gauge,
    /// A total since the start: its change over the selection matters.
    Cumulative,
}

impl Timing {
    pub fn name(self) -> &'static str {
        match self {
            Timing::Wall => "wall",
            Timing::SubmitPath => "submit-path",
            Timing::CopyBack => "copy-back",
            Timing::Concurrent => "concurrent",
            Timing::ThreadSum => "thread-sum",
            Timing::Volume => "volume",
            Timing::Gauge => "gauge",
            Timing::Cumulative => "cumulative",
        }
    }
}

/// Which way is an improvement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Better {
    Lower,
    Higher,
    Neither,
}

/// What is known of a metric beyond its key.
pub struct Meta {
    pub key: &'static str,
    pub label: &'static str,
    pub timing: Timing,
    pub better: Better,
}

const fn m(key: &'static str, label: &'static str, timing: Timing, better: Better) -> Meta {
    Meta {
        key,
        label,
        timing,
        better,
    }
}

use Better::{Higher, Lower, Neither};
use Timing::{Concurrent, CopyBack, Cumulative, Gauge, SubmitPath, ThreadSum, Volume, Wall};

/// Labels and kinds of the metrics the reports name. Keys not listed here
/// still appear in the full tables, with their unit read from their name.
pub const META: &[Meta] = &[
    // Frame (last tick, written every five seconds or more).
    m("frame.fps", "Frame rate (1 s window)", Wall, Higher),
    m("frame.ms", "Frame time (mean of a 1 s window)", Wall, Lower),
    m(
        "frame.cpu_ms",
        "CPU preparing submissions",
        SubmitPath,
        Lower,
    ),
    m(
        "frame.copy_ms",
        "  of it: title memory copies",
        SubmitPath,
        Lower,
    ),
    m(
        "frame.texture_ms",
        "  of it: texture refreshes (read, hash, detile)",
        SubmitPath,
        Lower,
    ),
    m(
        "frame.cpu_other_ms",
        "  of it: the rest (derived: cpu - copies - textures)",
        SubmitPath,
        Lower,
    ),
    m(
        "frame.gpu_ms",
        "Blocked waiting for the card (fence waits)",
        SubmitPath,
        Lower,
    ),
    m(
        "frame.back_ms",
        "GPU to CPU copy back (compare and publish)",
        CopyBack,
        Lower,
    ),
    m(
        "frame.submit_path_ms",
        "Submit path total (derived, sequential parts)",
        SubmitPath,
        Lower,
    ),
    m(
        "frame.copy_mb",
        "Title memory copied while preparing",
        Volume,
        Lower,
    ),
    m(
        "frame.texture_mb",
        "Texture bytes hashed while preparing",
        Volume,
        Lower,
    ),
    m(
        "frame.back_mb",
        "Bytes that differed and went back to title memory",
        Volume,
        Neither,
    ),
    m("frame.draws", "Draws per frame", Volume, Neither),
    m(
        "frame.cpu_pct",
        "Process CPU share of the machine",
        Gauge,
        Neither,
    ),
    m(
        "frame.gpu_pct",
        "Fence waits as a share of the frame",
        Gauge,
        Neither,
    ),
    m("frame.ram_mb", "Working set", Gauge, Lower),
    // The card's clock (every tick).
    m(
        "clock.busy_pct",
        "Card busy (by the card's clock)",
        Wall,
        Neither,
    ),
    m(
        "clock.idle_ms",
        "Card idle: nothing of ours queued",
        Wall,
        Lower,
    ),
    m(
        "clock.behind_ms",
        "  of it while the submitting thread worked (CPU bound)",
        Wall,
        Lower,
    ),
    m(
        "clock.stood_ms",
        "Submitting thread stood waiting for the card",
        Wall,
        Lower,
    ),
    // Flights and the relay (every tick).
    m(
        "late.settles",
        "Settles (waits for flights) per frame",
        Volume,
        Lower,
    ),
    m(
        "late.wait_ms",
        "Waiting for the card in settles (part of the fence waits)",
        SubmitPath,
        Lower,
    ),
    m(
        "relay.landing_ms",
        "Relay thread landing (its copy back)",
        Concurrent,
        Lower,
    ),
    m(
        "relay.fetch_wait_ms",
        "Relay thread waiting for card copy fetches",
        Concurrent,
        Lower,
    ),
    m(
        "relay.idle_wait_ms",
        "Relay thread idle, waiting for the card",
        Concurrent,
        Neither,
    ),
    // Readback volumes (every tick; MiB as the engine counts them).
    m(
        "back.compared_total_mib",
        "Shader-written ranges compared on the CPU (derived)",
        Volume,
        Lower,
    ),
    m(
        "back.windows_compared_mib",
        "  in windows outside buffers",
        Volume,
        Lower,
    ),
    m(
        "back.buffers_compared_mib",
        "  in title buffers' copies",
        Volume,
        Lower,
    ),
    m(
        "back.snapshot_total_mib",
        "Shader-written ranges snapshotted (derived)",
        Volume,
        Lower,
    ),
    m(
        "back.written_pct",
        "Share of compared bytes that differed (derived)",
        Volume,
        Higher,
    ),
    // Host copies in the card's memory (with the frame-rate line).
    m("card.sent_mb", "Sent up to card copies", Volume, Lower),
    m(
        "card.fetched_mb",
        "Dirty ranges fetched back from card copies",
        Volume,
        Lower,
    ),
    m(
        "card.extra_wait_ms",
        "Extra waits for the card for those fetches",
        SubmitPath,
        Lower,
    ),
    m(
        "card.readback_copies",
        "Readback copy commands",
        Volume,
        Lower,
    ),
    m(
        "card.alive_mb",
        "Host copies in the card's memory",
        Gauge,
        Neither,
    ),
    m("card.before_mb", "  their 'before' twins", Gauge, Neither),
    m(
        "card.copies",
        "Host copies in the card's memory (count)",
        Gauge,
        Neither,
    ),
    // copystat (since the previous copystat line).
    m(
        "copies.title_copy_mb",
        "Title copies and fills done on the CPU",
        Volume,
        Lower,
    ),
    m(
        "copies.refresh_mb",
        "Host copy refreshes asked for",
        Volume,
        Lower,
    ),
    m(
        "copies.checked_mb",
        "  write-watched bytes checked",
        Volume,
        Lower,
    ),
    m(
        "copies.unchanged_mb",
        "  of them unchanged",
        Volume,
        Neither,
    ),
    m(
        "copies.changed_mb",
        "  of them copied again (derived)",
        Volume,
        Lower,
    ),
    m(
        "copies.unchanged_pct",
        "Share of checked bytes found unchanged (derived)",
        Volume,
        Neither,
    ),
    m("copies.watch_ms", "Write-watch askings", ThreadSum, Lower),
    m("copies.query_ms", "Mapping queries", ThreadSum, Lower),
    m(
        "copies.run_ms",
        "Runs of title work at submission",
        ThreadSum,
        Lower,
    ),
    m(
        "copies.tex_hashed_mb",
        "Texture bytes hashed",
        Volume,
        Lower,
    ),
    m(
        "copies.tex_changed_mb",
        "  of them changed",
        Volume,
        Neither,
    ),
    m("copies.page_faults", "Page faults", Volume, Lower),
    m("copies.guard_faults", "Guard faults", Volume, Lower),
    m(
        "recording.frames",
        "Frames in the copystat window",
        Volume,
        Neither,
    ),
    m(
        "recording.list_ms",
        "List calls (all recording threads)",
        ThreadSum,
        Lower,
    ),
    m(
        "recording.pipeline_wait_ms",
        "Waiting for pipelines",
        ThreadSum,
        Lower,
    ),
    m(
        "recording.snapshot_ms",
        "Snapshots of shader-written ranges",
        ThreadSum,
        Lower,
    ),
    m(
        "recording.back_compared_mb",
        "Bytes compared back after submissions",
        Volume,
        Lower,
    ),
    m(
        "recording.whole_mb",
        "Copied whole (memory not write-watched)",
        Volume,
        Lower,
    ),
    m(
        "recording.guard_ms",
        "Guarding view memory",
        ThreadSum,
        Lower,
    ),
    m(
        "recording.sparse_ms",
        "Reads around unmapped pages",
        ThreadSum,
        Lower,
    ),
    m(
        "account.back_scan_ms",
        "Finding the 64 KiB pieces that differ",
        ThreadSum,
        Lower,
    ),
    m(
        "account.back_sync_ms",
        "Putting them into title memory",
        ThreadSum,
        Lower,
    ),
    m("account.back_differ", "Pieces that differ", Volume, Neither),
    m(
        "account.user_ms",
        "Process CPU time, user (all threads)",
        ThreadSum,
        Lower,
    ),
    m(
        "account.kernel_ms",
        "Process CPU time, kernel (all threads)",
        ThreadSum,
        Lower,
    ),
    m(
        "whole.whole_ms",
        "Copying unwatched memory whole",
        ThreadSum,
        Lower,
    ),
    m(
        "card_wait.nothing_back_ms",
        "Fence waits for submissions that give nothing back",
        SubmitPath,
        Lower,
    ),
    m(
        "card_wait.over32_ms",
        "Fence waits longer than 32 ms",
        SubmitPath,
        Lower,
    ),
    m(
        "card_wait.longest_ms",
        "Longest fence wait in the window",
        SubmitPath,
        Lower,
    ),
    m(
        "heap.buffer_refresh_ms",
        "Heap streaming: preparing buffer refreshes",
        ThreadSum,
        Lower,
    ),
    m(
        "heap.texture_stage_ms",
        "Heap streaming: staging textures",
        ThreadSum,
        Lower,
    ),
    m(
        "heap.view_resolve_ms",
        "Heap streaming: resolving buffer views",
        ThreadSum,
        Lower,
    ),
    m(
        "texrefresh.all_ms",
        "Texture refreshes, all threads",
        ThreadSum,
        Lower,
    ),
    m(
        "slow_watch.total_ms",
        "Write-watch askings of 1 ms or more",
        ThreadSum,
        Lower,
    ),
    // Gauges.
    m(
        "vram.local_used_mb",
        "Dedicated video memory in use",
        Gauge,
        Lower,
    ),
    m(
        "vram.local_budget_mb",
        "Dedicated video memory budget",
        Gauge,
        Neither,
    ),
    m(
        "vram.shared_used_mb",
        "Shared (system) memory used by the card",
        Gauge,
        Lower,
    ),
    m(
        "vram.local_used_pct",
        "Dedicated video memory in use, share of budget (derived)",
        Gauge,
        Lower,
    ),
    m("memory.private_mb", "Process private bytes", Gauge, Lower),
    m(
        "memory.host_copies_mb",
        "Host copies of title memory",
        Gauge,
        Lower,
    ),
    m(
        "memory.windows_mb",
        "  of them windows outside buffers",
        Gauge,
        Lower,
    ),
    m(
        "memory.snapshots_mb",
        "Snapshot buffers reserved",
        Gauge,
        Lower,
    ),
    m(
        "memory.upload_buffers",
        "Upload buffers made so far",
        Cumulative,
        Lower,
    ),
    m(
        "memory.upload_buffers_mb",
        "Upload buffer bytes made so far",
        Cumulative,
        Lower,
    ),
    m(
        "memory.card_buffers_mb",
        "Card buffer bytes made so far",
        Cumulative,
        Lower,
    ),
    m(
        "let_go.windows_mb",
        "Windows let go so far",
        Cumulative,
        Neither,
    ),
    m(
        "staging_pool.created",
        "Texture staging chunks made so far",
        Cumulative,
        Lower,
    ),
];

/// Metadata for `key`, if listed.
pub fn meta(key: &str) -> Option<&'static Meta> {
    META.iter().find(|m| m.key == key)
}

/// The unit of a metric, read from its key.
pub fn unit(key: &str) -> &'static str {
    let last = key.rsplit('.').next().unwrap_or(key);
    if last.ends_with("_ms") {
        "ms"
    } else if last.ends_with("_mib") {
        "MiB"
    } else if last.ends_with("_mb") {
        "MB"
    } else if last.ends_with("_pct") {
        "%"
    } else if last == "fps" {
        "fps"
    } else if last == "ms" {
        "ms"
    } else {
        "count"
    }
}

/// The timing kind of a metric: listed, or read from its unit and scope.
pub fn timing(key: &str, scope: Option<Scope>) -> Timing {
    if let Some(m) = meta(key) {
        return m.timing;
    }
    match (unit(key), scope) {
        (_, Some(Scope::Gauge)) => Gauge,
        ("ms", Some(Scope::Copy | Scope::CopyStart)) => ThreadSum,
        ("ms", _) => ThreadSum,
        _ => Volume,
    }
}

/// Which way is better for a metric.
pub fn better(key: &str) -> Better {
    if let Some(m) = meta(key) {
        return m.better;
    }
    match unit(key) {
        "ms" | "MB" | "MiB" => Lower,
        _ => Neither,
    }
}

/// The human label of a metric (its key when not listed).
pub fn label(key: &str) -> &str {
    meta(key).map_or(key, |m| m.label)
}

/// The scope of the line a key comes from, by its prefix.
pub fn scope_of(key: &str) -> Option<Scope> {
    let id = key.split('.').next()?;
    ENTRIES.iter().find(|e| e.id == id).map(|e| e.scope)
}

/// A short description of a scope, for tables.
pub fn scope_name(scope: Option<Scope>) -> &'static str {
    match scope {
        Some(Scope::TickStart | Scope::Tick) => "per frame, 1 s tick",
        Some(Scope::Report) => "per frame, 1 s tick (logged every 5 s)",
        Some(Scope::CopyStart | Scope::Copy) => "per frame, copystat window (~5 s)",
        Some(Scope::Gauge) => "instant",
        Some(Scope::Hot(_)) => "per range",
        None => "derived",
    }
}

/// The costs ranked by the summary: (key, parent key). A parent is an
/// aggregate that holds its children; ranking looks at the deepest parts
/// that have data.
pub const COSTS: &[(&str, Option<&str>)] = &[
    ("frame.cpu_ms", None),
    ("frame.copy_ms", Some("frame.cpu_ms")),
    ("frame.texture_ms", Some("frame.cpu_ms")),
    ("frame.cpu_other_ms", Some("frame.cpu_ms")),
    ("frame.gpu_ms", None),
    ("late.wait_ms", Some("frame.gpu_ms")),
    ("frame.back_ms", None),
    ("account.back_scan_ms", Some("frame.back_ms")),
    ("account.back_sync_ms", Some("frame.back_ms")),
    ("relay.fetch_wait_ms", Some("frame.back_ms")),
    ("recording.snapshot_ms", None),
    ("copies.watch_ms", None),
    ("whole.whole_ms", None),
    ("texrefresh.all_ms", None),
    ("heap.buffer_refresh_ms", None),
    ("heap.texture_stage_ms", None),
    ("card.extra_wait_ms", None),
];

/// The metrics of the headline tables, in order, by section.
pub const SECTIONS: &[(&str, &[&str])] = &[
    (
        "Frame time and where it goes",
        &[
            "frame.fps",
            "frame.ms",
            "frame.cpu_ms",
            "frame.copy_ms",
            "frame.texture_ms",
            "frame.cpu_other_ms",
            "frame.gpu_ms",
            "late.wait_ms",
            "frame.back_ms",
            "relay.landing_ms",
            "frame.submit_path_ms",
            "clock.busy_pct",
            "clock.idle_ms",
            "clock.behind_ms",
            "clock.stood_ms",
            "frame.draws",
        ],
    ),
    (
        "CPU to GPU: host copies and uploads",
        &[
            "frame.copy_mb",
            "copies.title_copy_mb",
            "copies.refresh_mb",
            "copies.checked_mb",
            "copies.unchanged_mb",
            "copies.changed_mb",
            "copies.unchanged_pct",
            "recording.whole_mb",
            "whole.whole_ms",
            "copies.watch_ms",
            "card.sent_mb",
            "frame.texture_mb",
            "copies.tex_hashed_mb",
            "copies.tex_changed_mb",
            "texrefresh.all_ms",
        ],
    ),
    (
        "GPU to CPU: shader writes brought back",
        &[
            "back.compared_total_mib",
            "back.windows_compared_mib",
            "back.buffers_compared_mib",
            "back.snapshot_total_mib",
            "frame.back_mb",
            "back.written_pct",
            "recording.snapshot_ms",
            "account.back_scan_ms",
            "account.back_sync_ms",
            "account.back_differ",
            "card.fetched_mb",
            "card.readback_copies",
            "relay.fetch_wait_ms",
        ],
    ),
    (
        "Synchronisation",
        &[
            "late.settles",
            "late.wait_ms",
            "card_wait.nothing_back_ms",
            "card_wait.over32_ms",
            "card_wait.longest_ms",
            "card.extra_wait_ms",
        ],
    ),
    (
        "Memory",
        &[
            "vram.local_used_mb",
            "vram.local_budget_mb",
            "vram.local_used_pct",
            "vram.shared_used_mb",
            "memory.private_mb",
            "memory.host_copies_mb",
            "memory.windows_mb",
            "memory.snapshots_mb",
            "card.alive_mb",
            "frame.ram_mb",
            "memory.upload_buffers_mb",
            "staging_pool.created",
        ],
    ),
];

#[cfg(test)]
mod tests {
    use super::{COSTS, ENTRIES, META, SECTIONS};
    use crate::perflog::template::{Template, normalize};

    /// The engine's sources with Rust's line continuations joined and
    /// whitespace collapsed, as the strings print.
    fn engine_text() -> String {
        let root = crate::workspace_root().join("crates/victory-gpu/src");
        let mut text = String::new();
        for name in ["host.rs", "copystat.rs", "shadow.rs"] {
            // A checkout with CRLF endings (a source ZIP) joins the same.
            let source = std::fs::read_to_string(root.join(name))
                .expect("engine source")
                .replace("\r\n", "\n");
            let mut joined = String::with_capacity(source.len());
            let mut chars = source.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '\\' && chars.peek() == Some(&'\n') {
                    chars.next();
                    while chars.peek().is_some_and(|c| c.is_whitespace()) {
                        chars.next();
                    }
                    continue;
                }
                joined.push(c);
            }
            text.push_str(&normalize(&joined));
            text.push(' ');
        }
        text
    }

    /// Every literal of every pattern is still in the engine's messages:
    /// a reworded message must be reworded here too.
    #[test]
    fn patterns_match_the_engine_messages() {
        let text = engine_text();
        let mut missing = Vec::new();
        for entry in ENTRIES {
            let t = Template::compile(entry.pattern);
            for lit in t.literals() {
                // The engine prints addresses with `{:#x}`: the `0x` is
                // part of the value there, a literal here.
                let lit = lit.strip_suffix("0x").unwrap_or(lit);
                if lit.trim().len() >= 3 && !text.contains(lit) {
                    missing.push(format!("{}: {lit:?}", entry.id));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "literals not found in the engine sources:\n{}",
            missing.join("\n")
        );
    }

    #[test]
    fn ids_are_unique_and_patterns_compile() {
        let mut ids: Vec<&str> = ENTRIES.iter().map(|e| e.id).collect();
        ids.sort_unstable();
        let n = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), n);
        for e in ENTRIES {
            let _ = Template::compile(e.pattern);
        }
    }

    /// Every key the reports name is produced by a pattern or derived.
    #[test]
    fn named_keys_exist() {
        let derived = [
            "frame.cpu_other_ms",
            "frame.submit_path_ms",
            "back.compared_total_mib",
            "back.snapshot_total_mib",
            "back.written_pct",
            "copies.changed_mb",
            "copies.unchanged_pct",
            "vram.local_used_pct",
        ];
        let mut known: Vec<String> = derived.iter().map(|s| (*s).to_owned()).collect();
        for e in ENTRIES {
            for name in Template::compile(e.pattern).number_names() {
                known.push(format!("{}.{name}", e.id));
            }
        }
        let named = META
            .iter()
            .map(|m| m.key)
            .chain(COSTS.iter().map(|c| c.0))
            .chain(SECTIONS.iter().flat_map(|s| s.1.iter().copied()));
        for key in named {
            assert!(
                known.iter().any(|k| k == key),
                "{key} is named but never produced"
            );
        }
    }
}
