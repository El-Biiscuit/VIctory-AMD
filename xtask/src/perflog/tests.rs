//! Tests on synthetic logs written by the code below, in the runtime's own
//! line formats (copied from `crates/victory-gpu`; `catalog`'s test checks
//! the patterns against those sources). No data from any game is involved.

use super::parse::{Selection, parse, select};
use super::report::{Comparison, Summary};
use super::{decode, stats};

/// What a synthetic run looks like.
#[derive(Clone)]
struct Spec {
    ticks: usize,
    /// Frame time of tick `i`, in ms.
    frame_ms: fn(usize) -> f64,
    /// Draws per frame of tick `i`.
    draws: fn(usize) -> f64,
    cpu_ms: f64,
    copy_ms: f64,
    texture_ms: f64,
    gpu_ms: f64,
    back_ms: f64,
    /// `card.extra_wait_ms`, and the relay's `fetch_wait_ms`.
    card_wait_ms: f64,
    relay_fetch_ms: f64,
    relay: bool,
    /// Copystat windows written at tick `i` that are a streaming burst.
    burst: fn(usize) -> bool,
    presents: bool,
    ansi: bool,
    crlf: bool,
}

impl Default for Spec {
    fn default() -> Self {
        Spec {
            ticks: 30,
            frame_ms: |i| 200.0 + i as f64,
            draws: |_| 1500.0,
            cpu_ms: 147.1,
            copy_ms: 40.0,
            texture_ms: 30.0,
            gpu_ms: 24.8,
            back_ms: 133.6,
            card_wait_ms: 2.0,
            relay_fetch_ms: 2.5,
            relay: true,
            burst: |_| false,
            presents: false,
            ansi: false,
            crlf: false,
        }
    }
}

/// 2026-11-27T21:00:00Z.
const BASE: f64 = 20_784.0 * 86_400.0 + 21.0 * 3600.0;

fn stamp(t: f64) -> String {
    let secs = t - BASE;
    let whole = secs.floor();
    let micros = ((secs - whole) * 1e6).round() as u64;
    let (m, s) = ((whole as u64) / 60, (whole as u64) % 60);
    format!("2026-11-27T21:{m:02}:{s:02}.{micros:06}Z")
}

fn synthetic(spec: &Spec) -> String {
    let mut out = String::new();
    let mut line = |t: f64, target: &str, msg: String| {
        let level = if target == "victory_gpu::frame" {
            "DEBUG"
        } else {
            " INFO"
        };
        if spec.ansi {
            out.push_str(&format!("\u{1b}[2m{}\u{1b}[0m \u{1b}[32m{level}\u{1b}[0m \u{1b}[2m{target}\u{1b}[0m\u{1b}[2m:\u{1b}[0m {msg}", stamp(t)));
        } else {
            out.push_str(&format!("{} {level} {target}: {msg}", stamp(t)));
        }
        out.push_str(if spec.crlf { "\r\n" } else { "\n" });
    };
    let mut header =
        String::from("Runtime C:\\Users\\someone\\runtime.exe\nGame D:\\games\\title.exe\n");
    header.push_str("Ray tracing requested: off\nExpected scene rendering 1920x1080, output 2560x1440\nTexture copy queue requested: off\n");
    let mut window_frames = 0.0;
    for i in 0..spec.ticks {
        let t = BASE + 10.0 + i as f64;
        let ms = (spec.frame_ms)(i);
        let fps = 1000.0 / ms;
        let frames = (1000.0 / ms).max(1.0).round();
        window_frames += frames;
        if spec.presents {
            for k in 0..frames as usize {
                line(
                    t - 1.0 + (k as f64 + 1.0) * ms / 1000.0,
                    "victory_gpu::frame",
                    format!("present token={}", i * 10 + k),
                );
            }
        }
        line(t, "victory_gpu::host", "indirect mesh dispatches: the largest group count asked for 4096 (run: at most 65535), 120 groups asked per frame".into());
        if spec.relay {
            line(
                t,
                "victory_gpu::host",
                format!(
                    "submissions sent ahead per frame: 12.0 in 3.0 settles (4.0 flights each time), {:.1} ms waiting for the card in them; \
                 settled for: a synchronous submission or present 1.0, the card had run them 0.0, a wait for a memory value 0.5, \
                 title memory changed under shader writes 0.2, title memory read 1.0 (by a buffer copy 0.5, a fill of upload memory 0.3, \
                 a texture 0.2), a step of ours 0.1, 8 lists in the air 0.0, bottom-of-pipe writes (VICTORY_LATE_WAIT_WRITES=0) 0.0, \
                 a blocking fence wait 0.2; 4.0 waits for memory values met by a flight's bottom-of-pipe write, 2.0 fence signals set behind flights",
                    spec.gpu_ms * 0.8
                ),
            );
            line(t, "victory_gpu::host", "of the waiting for the card in settles, per frame, by what settled: a synchronous submission or present 10.0 ms, \
                 the card had run them 0.0, a wait for a memory value 4.0, title memory changed under shader writes 1.0, \
                 title memory read 3.0, a step of ours 0.5, lists in the air 0.0, bottom-of-pipe writes 0.0, a blocking fence wait 1.3".into());
            line(
                t,
                "victory_gpu::host",
                format!(
                    "the relay per frame (VICTORY_RELAY=1): 12.0 flights landed in 6.0 landings by the thread that copies back, \
                 beside the submitting thread; that thread waited 30.0 ms for the card with nothing to land and {:.1} ms for \
                 fetches of card copies' pieces, and was {:.1} ms landing in all (the frame line's \"copying back\" is its time; \
                 \"settles\" above are waits of other threads for it)",
                    spec.relay_fetch_ms, spec.back_ms
                ),
            );
        }
        line(
            t,
            "victory_gpu::host",
            format!(
                "the card's clock per frame: nothing of ours queued {:.1} ms (the card about {:.0}% busy by this count), \
             {:.1} ms of that while the submitting thread was working (the card idle behind us), the rest while the title \
             had not submitted; the submitting thread stood waiting for the card {:.1} ms",
                ms - 30.0,
                100.0 * 30.0 / ms,
                ms - 60.0,
                spec.gpu_ms
            ),
        );
        line(t, "victory_gpu::host", "Direct3D title queue calls: 40.0/frame, 1.200 ms/frame in ExecuteCommandLists, longest call 0.400 ms".into());
        line(t, "victory_gpu::host", "Direct3D upload and readback queue calls: 6.0/frame, 0.300 ms/frame in ExecuteCommandLists, longest call 0.100 ms".into());
        line(t, "victory_gpu::shadow", "shader writes looked for on the CPU per frame: 300.0 MB compared in 50 ranges of windows outside buffers (310.0 MB \
             snapshotted), 100.0 MB in 20 ranges of title buffers' copies (90.0 MB snapshotted), of them 80.0 MB in copies \
             of 1 MiB or more (VICTORY_CARD_BUFFERS_MIN_MB=1 would have the card look there)".into());
        line(t, "victory_gpu::host", "title buffer copies recorded per frame: 30, of them 10 continue the one before (VICTORY_JOIN_COPIES=1 makes those one copy with it: now off)".into());
        if i % 5 != 4 {
            continue;
        }
        line(t, "victory_gpu::host", "write-watch askings of a millisecond or more: 2.0 a frame, 4.0 ms a frame; the longest asking 3.0 ms".into());
        line(
            t,
            "victory_gpu::host",
            format!(
                "host copies in the card's memory: 10 copies 494 MB, their bytes from before 300 MB; per frame: \
             12.5 MB sent up in 40 ranges, 900 pieces compared on the card, 7 dirty ranges fetched (3.5 MB) with 1.0 more waits for the card ({:.1} ms), \
             7 readback copy commands; relay joined 2 dirty ranges and avoided 0.50 MB of overlapping reads",
                spec.card_wait_ms
            ),
        );
        line(
            t,
            "victory_gpu::host",
            format!(
                "{fps:.1} fps, {ms:.1} ms/frame ({:.1} preparing: copies {:.1} ms 50.0 MB, textures {:.1} ms 20.0 MB; {:.1} on the \
             GPU; {:.1} copying back 4.0 MB), {:.0} draws/frame, dropped 3 without a host pipeline (2 being made) and \
             10 indirect; heap buffers 1.0 passes 0.5 ms;                  texture slots written so far: 100 placed, 20 of objects; \
             textures picked by index: 500 uploaded so far (900 MB), 3 waiting, 0 passed over unmapped; depth textures with                  unordered access: 1 made, bound as depth 0x, cleared as depth 0x,                  unordered views refused 0x; buffer views left null as the host would refuse them: 0; depth twins: 2 alive, 0 depth textures left without one; twin copies per frame: 0.0 into depth textures, 0.0 into twins; depth planes: 0.0 drawn from their twins per frame, 0 times not given back to a twin, 0 times used before the pass; \
             cpu 12.0% gpu 12% busy ram 9000 MB",
                spec.cpu_ms,
                spec.copy_ms,
                spec.texture_ms,
                spec.gpu_ms,
                spec.back_ms,
                (spec.draws)(i)
            ),
        );
        // copystat::log
        let c = "victory_gpu::copystat";
        line(t, c, "copies per frame: 40.0 submissions; 300 title copies 50.0 MB; 900 refreshes 400.0 MB, 10 skipped 1.0 MB; 5 runs 2.0 ms; \
             20 queries 0.50 ms, moves 0.10 ms, 0.00 MB unmapped; 30 page faults; 200 watches 6.00 ms, 380.0 MB checked, 360.0 MB \
             unchanged; 0.0 fallbacks; 0 guard faults; textures 5.0 MB watched and changed, 60.0 MB hashed, 6.0 MB of them changed".into());
        line(
            t,
            c,
            format!(
                "recording per frame ({window_frames:.0} frames): 9000 list calls 30.0 ms (all recording threads); 1.0 ms waiting for pipelines; \
             dynamic slots: 50 tables asked, 0 refused, 0.2 ms searching; notes 0.1 ms; shader-written ranges: 70 steps in \
             20.0 submissions, snapshots 25.0 ms, 400.0 MB compared back; sparse reads 0.0 ms; guards 0.5 ms; 8.0 MB copied whole \
             (memory not watched); 1.0 frame waits sleeping 0.0 ms (all threads); ranges of shaders that cannot store to buffers: 0 steps 0.0 MB left out"
            ),
        );
        line(t, c, "guard protection changes per frame (VirtualProtect): 2800 calls 25.00 ms in all, 18.00 ms of it on a submitting thread \
             in a submission; guarding after copies 420 calls over 900 blocks 5.00 ms (5.00 ms in submissions), 3 calls block \
             by block after a failed run (1 failed); guarding other views 790 calls over 790 blocks in 120 runs 6.50 ms \
             (6.50 ms in submissions), 0 failed; opening for faults 418 calls in 400 runs 3.00 ms; for writes told 100 calls \
             in 20 runs 1.00 ms (0.20 ms in submissions); other views of opened blocks 1070 calls in 300 runs 9.00 ms; \
             blocks of views mapped 2 calls in 1 runs 0.05 ms; 0.0 openings failed".into());
        line(t, c, "guard waits per frame: 418 write faults in the handler taking 6.00 ms on the faulting threads, 0.80 ms of it waiting \
             for the guard lock; 900 checks waited 0.40 ms for the guard lock, 100 writes told waited 0.10 ms; 300 checks \
             guarded in 420 calls, 250 if runs with only guarded blocks between them were joined".into());
        let burst = (spec.burst)(i);
        let (write_wait, title_fault_wait, found_open) = if burst {
            (103.81, 12.0, 16.0)
        } else {
            (1.33, 6.0, 16.0)
        };
        line(
            t,
            c,
            format!(
                "GUARDS lock per frame: checks 2070 taken, 0.14 ms waiting (longest 0.05 ms, 0.0 over 0.1 ms), 18.00 ms held \
             (longest 2.50 ms, 40.0 over 0.1 ms, 3.00 over 1 ms); looks before checks 2070 taken, 0.01 ms waiting (longest \
             0.01 ms, 0.0 over 0.1 ms), 0.40 ms held (longest 0.01 ms, 0.0 over 0.1 ms, 0.00 over 1 ms); write faults 356 \
             taken, 23.08 ms waiting (longest 4.10 ms, 60.0 over 0.1 ms), 5.50 ms held (longest 0.30 ms, 2.0 over 0.1 ms, \
             0.00 over 1 ms); writes told 579 taken, {write_wait:.2} ms waiting (longest 9.00 ms, 3.0 over 0.1 ms), 1.10 ms \
             held (longest 0.20 ms, 0.0 over 0.1 ms, 0.00 over 1 ms); views mapped 2 taken, 0.00 ms waiting (longest 0.00 \
             ms, 0.0 over 0.1 ms), 0.02 ms held (longest 0.01 ms, 0.0 over 0.1 ms, 0.00 over 1 ms); remaps 5 taken, 0.01 ms \
             waiting (longest 0.01 ms, 0.0 over 0.1 ms), 0.05 ms held (longest 0.02 ms, 0.0 over 0.1 ms, 0.00 over 1 ms); \
             0.85 threads already waiting on average when taken, 6 at most"
            ),
        );
        line(t, c, "JOURNAL and VIEWS locks per frame: JOURNAL by write-watch askings and guard checks 1500 taken, 0.20 ms waiting \
             (longest 0.10 ms, 0.0 over 0.1 ms), 30.00 ms held (longest 3.00 ms, 50.0 over 0.1 ms, 5.00 over 1 ms); JOURNAL \
             by looks at askings ahead 1500 taken, 0.20 ms waiting (longest 0.10 ms, 0.0 over 0.1 ms), 30.00 ms held \
             (longest 3.00 ms, 50.0 over 0.1 ms, 5.00 over 1 ms); JOURNAL by askings ahead 1500 taken, 0.20 ms waiting \
             (longest 0.10 ms, 0.0 over 0.1 ms), 30.00 ms held (longest 3.00 ms, 50.0 over 0.1 ms, 5.00 over 1 ms); JOURNAL \
             otherwise 1500 taken, 0.20 ms waiting (longest 0.10 ms, 0.0 over 0.1 ms), 30.00 ms held (longest 3.00 ms, 50.0 \
             over 0.1 ms, 5.00 over 1 ms); VIEWS with GUARDS held 1500 taken, 0.20 ms waiting (longest 0.10 ms, 0.0 over \
             0.1 ms), 30.00 ms held (longest 3.00 ms, 50.0 over 0.1 ms, 5.00 over 1 ms); VIEWS otherwise 1500 taken, 0.20 \
             ms waiting (longest 0.10 ms, 0.0 over 0.1 ms), 30.00 ms held (longest 3.00 ms, 50.0 over 0.1 ms, 5.00 over 1 \
             ms); JOURNAL 0.10 threads already waiting on average when taken, 2 at most; VIEWS 0.02, 1 at most".into());
        line(t, c, "lock time per frame by thread: in submissions GUARDS 1.00 ms waiting 21.00 ms held, JOURNAL 2.00 ms waiting \
             22.00 ms held, VIEWS 3.00 ms waiting 23.00 ms held; title submitting threads outside submissions GUARDS 4.00 \
             ms waiting 24.00 ms held, JOURNAL 5.00 ms waiting 25.00 ms held, VIEWS 6.00 ms waiting 26.00 ms held; the \
             relay GUARDS 7.00 ms waiting 27.00 ms held, JOURNAL 8.00 ms waiting 28.00 ms held, VIEWS 9.00 ms waiting 29.00 \
             ms held; other threads GUARDS 10.00 ms waiting 30.00 ms held, JOURNAL 11.00 ms waiting 31.00 ms held, VIEWS \
             12.00 ms waiting 32.00 ms held".into());
        line(
            t,
            c,
            format!(
                "guard lock around checks and faults per frame: 3.20 ms asking whether ranges are views before taking the lock, \
             0.40 ms asking about uncommitted blocks holding it; {found_open:.1} write faults found their block open already; \
             write faults waited {title_fault_wait:.2} ms on title submitting threads and 15.00 ms on other threads"
            ),
        );
        line(t, c, "lock time per frame by kind of other thread: recording threads GUARDS 4.00 ms waiting 0.10 ms held, \
             JOURNAL 0.10 ms waiting 0.10 ms held, VIEWS 0.10 ms waiting 0.10 ms held; runtime workers GUARDS 1.50 ms \
             waiting 0.10 ms held, JOURNAL 0.10 ms waiting 0.10 ms held, VIEWS 0.10 ms waiting 0.10 ms held; other title \
             threads GUARDS 3.25 ms waiting 0.10 ms held, JOURNAL 0.10 ms waiting 0.10 ms held, VIEWS 0.10 ms waiting 0.10 \
             ms held; unclassified GUARDS 1.25 ms waiting 0.10 ms held, JOURNAL 0.10 ms waiting 0.10 ms held, VIEWS 0.10 ms \
             waiting 0.10 ms held; write faults by recording threads 120 taken 2.50 ms waiting, runtime workers 0 taken \
             0.10 ms waiting, other title threads 0 taken 0.10 ms waiting, unclassified 0 taken 0.10 ms waiting; writes \
             told by recording threads 0 taken 0.10 ms waiting, runtime workers 0 taken 0.10 ms waiting, other title \
             threads 0 taken 2.75 ms waiting, unclassified 0 taken 0.10 ms waiting; since the last line 2 threads made \
             their first list call or submission, having waited 0.75 ms for GUARDS before it".into());
        line(t, c, "guards over several views of a page, since the start: 1 blocks guarded with their other views, 2 views opened by a write through another, 3 ranges opened because a view of them was mapped".into());
        line(
            t,
            c,
            format!(
                "frame account per frame: after a present 1.0 ms until the first list call and 2.0 ms until the first submission; \
             3.0 ms from the last submission to the present call; lists reset in 0.5 ms; the process ran 400 ms user + 50 ms kernel on \
             all threads; what shaders wrote: 60.0 ms finding 30 pieces that differ, 5.0 ms putting them back; texture uploads: {} recorded, \
             90 left out; 0.0 held submissions run at the present with their waits not met; waits for memory values: 100 looks 0.3 ms; \
             40 host lists made 0.4 ms; 0 parts of held submissions ran alone that the next part could have run with",
                if burst { 684 } else { 10 }
            ),
        );
        line(
            t,
            c,
            "  copied whole (memory not watched): 3.0 ms per frame in 2 ranges; the largest follow"
                .into(),
        );
        line(
            t,
            c,
            "  copied whole from 0x2000000+0x100000: 4.0x/frame, 4.19 MB/frame, 2.00 ms/frame"
                .into(),
        );
        line(t, c, "  waiting for the card per frame: 30.0 submissions under 2 ms (10.0 ms), 2.0 of 2-8 ms (8.0 ms), 0.5 of 8-32 ms (5.0 ms), \
             0.1 longer (4.0 ms); 25.0 submissions hand nothing back to the title (9.0 ms of the waiting); the longest since the last line: 40.0 ms, 3 lists".into());
        line(t, c, "  the card's time by what ran: 5.0 ms a frame in 2.0 submissions a frame: 3 lists, pipelines {}".into());
        line(t, c, "  waits for every earlier shader write, per frame: 20 for the title's barrier groups, 5 for its barriers on buffers; \
             2 indirect commands bounded (a dispatch of ours between two such waits each), 1 reused clamped commands; barrier experiment 0".into());
        line(t, c, "  heap streaming per frame: 1.5 ms resolving buffer views, 2.5 ms preparing buffer refreshes, 3.5 ms staging textures".into());
        line(
            t,
            c,
            "  compared after submissions: 12 ranges; the largest follow".into(),
        );
        line(t, c, "  compared 0x1000000+0x4000000: 2.0x/frame, 134.22 MB/frame, 0.500 MB/frame of it written back".into());
        line(t, c, "  compared 0x9000000+0x100000: 1.0x/frame, 1.05 MB/frame, 1.000 MB/frame of it written back".into());
        line(t, c, "  thread 1234 submitted 40.0x/frame: between its submissions 30.0 ms (running 20.0 ms user + 2.0 ms kernel; 10.0 ms in list calls), in them running 100.0 ms user + 5.0 ms kernel, 0.1 ms waiting for the submission lock".into());
        line(
            t,
            c,
            "  list calls by thread: 3 threads; thread 1 20 ms in 5000 calls".into(),
        );
        line(t, "victory_gpu::host", "video memory as the system counts it, with the memory line: the card's own: budget 11500 MB, in use 10540 MB, \
             2000 MB could still be reserved, 0 MB reserved; the PC's shared with the card: budget 32000 MB, in use 900 MB, 31000 MB could still be reserved, 0 MB reserved (the layer reserves none)".into());
        line(
            t,
            c,
            format!(
                "texture staging pool since startup: {} chunks made, 100 allocations served by existing chunks",
                10 + i
            ),
        );
        line(t, c, "memory of textures: 5 title textures have no host copy yet (12 MB of title memory); host copies the host renders to or writes: 10 alive 300 MB; \
             host copies the title fills: 100 alive 2000 MB (50 of them placed), 3 let go 10 MB and 1 made again 2 MB since the start; 200 host copies made in all; \
             passes that made none for lack of room: 0; the card: 10540 MB in use of a budget of 11500 MB, 900 MB of the PC's memory shared with it in use".into());
        line(t, c, "memory let go since the start: 4 windows outside buffers 16 MB, 2 staging buffers 8 MB; of the windows alive, 100 MB lie between the lowest and highest address asked for in each".into());
        line(
            t,
            c,
            format!(
                "memory: 18000 MB private (12000 MB of it in memory; the PC has 30000 MB free and 50000 MB left to commit); host copies of title memory: 700 alive 2700 MB, \
             of them 600 windows outside buffers 2400 MB; snapshot buffers kept: 3 reserving 512 MB; staging of textures: 20 alive 100 MB, 5 kept by their texture; \
             host textures: 900 alive 3000 MB, 40 placed in title memory; made so far: {} upload buffers {} MB, 5 shared buffers 50 MB, 30 card buffers 900 MB",
                100 + i,
                400 + 4 * i
            ),
        );
        line(t, c, "  copied from 0x3000000+0x200000 (memory 0x20000): 10.0x/frame, 20.97 MB/frame, 1.50 ms/frame".into());
        window_frames = 0.0;
    }
    header.push_str(&out);
    header
}

fn summary_of(text: &str, sel: &Selection) -> Summary {
    let run = parse("synthetic.log", text);
    let selected = vec![select(&run, sel)];
    Summary::build(sel, &selected)
}

#[test]
fn every_line_of_a_synthetic_run_is_read() {
    let run = parse("synthetic.log", &synthetic(&Spec::default()));
    assert!(run.unparsed.is_empty(), "unparsed: {:?}", run.unparsed);
    assert_eq!(run.ticks.len(), 30);
    assert_eq!(
        run.ticks
            .iter()
            .filter(|s| s.get("frame.fps").is_some())
            .count(),
        6
    );
    assert_eq!(run.windows.len(), 6);
    assert_eq!(run.gauges.len(), 6);
    assert!(run.relay() && run.late());
    let report = &run.ticks[4];
    assert_eq!(report.get("frame.ms"), Some(204.0));
    assert_eq!(report.get("frame.cpu_ms"), Some(147.1));
    assert_eq!(report.get("frame.back_ms"), Some(133.6));
    assert_eq!(report.get("frame.draws"), Some(1500.0));
    assert_eq!(report.get("frame.ram_mb"), Some(9000.0));
    assert_eq!(report.get("frame.cpu_pct"), Some(12.0));
    assert_eq!(report.get("d3d.upload_and_readback.calls"), Some(6.0));
    assert_eq!(report.get("d3d.title.execute_ms"), Some(1.2));
    assert_eq!(report.get("card.sent_mb"), Some(12.5));
    assert_eq!(report.get("late.why_most"), Some(0.0));
    assert_eq!(report.get("late.signals_behind"), Some(2.0));
    assert_eq!(report.get("relay.landing_ms"), Some(133.6));
    let w = &run.windows[0];
    assert_eq!(w.get("recording.frames"), Some(25.0));
    assert_eq!(w.get("copies.checked_mb"), Some(380.0));
    assert_eq!(w.get("account.back_scan_ms"), Some(60.0));
    assert_eq!(w.get("card_wait.longest_ms"), Some(40.0));
    assert_eq!(w.get("heap.texture_stage_ms"), Some(3.5));
    assert_eq!(w.get("whole.whole_ms"), Some(3.0));
    let g = &run.gauges[0];
    assert_eq!(g.get("vram.local_used_mb"), Some(10540.0));
    assert_eq!(g.get("tex_memory.card_used_mb"), Some(10540.0));
    assert_eq!(g.get("memory.snapshots_mb"), Some(512.0));
    assert_eq!(g.get("let_go.windows_mb"), Some(16.0));
    assert_eq!(g.get("staging_pool.created"), Some(14.0));
    assert_eq!(run.hot.len(), 6 * 4);
    assert!(
        run.header
            .contains(&("Ray tracing requested".into(), "off".into()))
    );
    assert!(
        run.header
            .contains(&("VICTORY_JOIN_COPIES".into(), "off".into()))
    );
    assert!(
        !run.header
            .iter()
            .any(|(k, _)| k == "Runtime" || k == "Game"),
        "paths stay out of reports"
    );
}

#[test]
fn derived_metrics_follow_their_parts() {
    let run = parse("synthetic.log", &synthetic(&Spec::default()));
    let r = &run.ticks[4];
    assert!((r.get("frame.cpu_other_ms").unwrap() - (147.1 - 40.0 - 30.0)).abs() < 1e-9);
    // The relay copies back beside the submitting thread: not on its path.
    assert!((r.get("frame.submit_path_ms").unwrap() - (147.1 + 24.8)).abs() < 1e-9);
    assert_eq!(r.get("back.compared_total_mib"), Some(400.0));
    assert!((r.get("back.written_pct").unwrap() - 100.0 * 4.0 / (400.0 * 1.048_576)).abs() < 1e-9);
    assert_eq!(run.windows[0].get("copies.changed_mb"), Some(20.0));
    let pct = run.gauges[0].get("vram.local_used_pct").unwrap();
    assert!((pct - 100.0 * 10540.0 / 11500.0).abs() < 1e-9);

    let without = parse(
        "synthetic.log",
        &synthetic(&Spec {
            relay: false,
            ..Spec::default()
        }),
    );
    assert!(!without.relay());
    let r = &without.ticks[4];
    assert!((r.get("frame.submit_path_ms").unwrap() - (147.1 + 24.8 + 133.6)).abs() < 1e-9);
}

/// The reference sample of the mission (5.0 fps, 201.8 ms; 147.1 ms
/// preparing, 24.8 ms on the GPU, 133.6 ms copying back): with the relay,
/// the parts that add up are 171.9 ms, and the copy back overlaps them.
#[test]
fn the_reference_sample_splits_into_sequential_and_concurrent_parts() {
    let spec = Spec {
        frame_ms: |_| 201.8,
        ..Spec::default()
    };
    let s = summary_of(&synthetic(&spec), &Selection::default());
    assert!((s.metrics["frame.submit_path_ms"].median - 171.9).abs() < 1e-9);
    let md = s.markdown(false, 8);
    assert!(md.contains("= 171.9 ms, 85% of the frame"), "{md}");
    assert!(
        md.contains("Concurrent, on the relay thread: copy back 133.6 ms"),
        "{md}"
    );
    let back = s.costs.iter().find(|c| c.key == "frame.back_ms").unwrap();
    assert_eq!(back.nature, "concurrent (relay thread)");
    assert!(!back.leaf, "its parts (scan, sync, fetch waits) have data");
}

#[test]
fn medians_and_p95_are_over_the_frame_lines() {
    let s = summary_of(&synthetic(&Spec::default()), &Selection::default());
    // Frame lines at ticks 4, 9, ..., 29: 204, 209, ..., 229 ms.
    let ms = &s.metrics["frame.ms"];
    assert_eq!(ms.n, 6);
    assert_eq!(ms.median, 216.5);
    assert!((ms.p95 - (224.0 + 0.75 * 5.0)).abs() < 1e-9);
    // Per-tick lines have a sample every tick.
    assert_eq!(s.metrics["clock.stood_ms"].n, 30);
    // Copystat windows are weighted by their frames.
    assert!(s.metrics["copies.checked_mb"].weighted_mean.is_some());
}

#[test]
fn costs_rank_the_deepest_parts() {
    let s = summary_of(&synthetic(&Spec::default()), &Selection::default());
    let top: Vec<&str> = s.top_costs().iter().map(|c| c.key.as_str()).collect();
    // Leaves: cpu other 77.1, back scan 60, copies 40, textures 30,
    // snapshots 25, settle wait 19.84 ...
    assert_eq!(
        top,
        [
            "frame.cpu_other_ms",
            "account.back_scan_ms",
            "frame.copy_ms"
        ]
    );
    let cpu = s.costs.iter().find(|c| c.key == "frame.cpu_ms").unwrap();
    assert!(!cpu.leaf && cpu.parent.is_none());
    assert_eq!(cpu.nature, "submit-path");
}

#[test]
fn selection_by_time_draws_and_frame_lines() {
    fn draws(i: usize) -> f64 {
        if i < 10 { 100.0 } else { 1500.0 }
    }
    let text = synthetic(&Spec {
        draws,
        ..Spec::default()
    });
    let run = parse("synthetic.log", &text);

    let all = select(&run, &Selection::default());
    assert_eq!((all.ticks.len(), all.windows.len()), (30, 6));

    // First frame line with >= 1000 draws is tick 14.
    let loaded = select(
        &run,
        &Selection {
            min_draws: Some(1000.0),
            ..Selection::default()
        },
    );
    assert_eq!(loaded.ticks.len(), 16);
    // The window written at tick 14 covers ticks 10-14, partly before the
    // start: left out. Those of ticks 19, 24, 29 are inside.
    assert_eq!(loaded.windows.len(), 3);

    let skipped = select(
        &run,
        &Selection {
            skip_secs: 4.0,
            duration_secs: Some(10.0),
            ..Selection::default()
        },
    );
    // Start at tick 0, skip to tick 4, keep ten seconds: ticks 4 to 14.
    assert_eq!(skipped.ticks.len(), 11);
    let reports: Vec<f64> = skipped
        .ticks
        .iter()
        .filter_map(|s| s.get("frame.ms"))
        .collect();
    assert_eq!(reports, [204.0, 209.0, 214.0]);
    // The window written at tick 4 covers ticks 0-4: left out; those of
    // ticks 9 and 14 are inside.
    assert_eq!(skipped.windows.len(), 2);

    let by_lines = select(
        &run,
        &Selection {
            skip_reports: 2,
            ..Selection::default()
        },
    );
    assert_eq!(
        by_lines
            .ticks
            .iter()
            .filter_map(|s| s.get("frame.ms"))
            .next(),
        Some(214.0)
    );

    let none = select(
        &run,
        &Selection {
            min_draws: Some(1e9),
            ..Selection::default()
        },
    );
    assert!(none.ticks.is_empty() && !none.warnings.is_empty());
}

#[test]
fn present_events_give_frame_intervals() {
    let spec = Spec {
        frame_ms: |_| 200.0,
        presents: true,
        ..Spec::default()
    };
    let s = summary_of(&synthetic(&spec), &Selection::default());
    let f = s.frame_intervals.expect("intervals");
    assert!((f.median - 200.0).abs() < 1e-3, "{f:?}");
    assert!(
        s.markdown(false, 8)
            .contains("Frame intervals from present events")
    );
}

#[test]
fn coloured_crlf_logs_read_the_same() {
    let plain = parse("a", &synthetic(&Spec::default()));
    let coloured = parse(
        "b",
        &synthetic(&Spec {
            ansi: true,
            crlf: true,
            ..Spec::default()
        }),
    );
    assert!(coloured.unparsed.is_empty());
    assert_eq!(plain.ticks, coloured.ticks);
    assert_eq!(plain.windows, coloured.windows);
    assert_eq!(plain.gauges, coloured.gauges);
}

#[test]
fn utf16_logs_from_powershell_decode() {
    let text = synthetic(&Spec {
        ticks: 5,
        ..Spec::default()
    });
    let mut bytes = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        bytes.extend(unit.to_le_bytes());
    }
    assert_eq!(decode(&bytes), text);
    let mut bom = vec![0xEF, 0xBB, 0xBF];
    bom.extend(text.as_bytes());
    assert_eq!(decode(&bom), text);
}

/// A reworded message is reported, not silently dropped.
#[test]
fn changed_messages_are_reported() {
    let text = synthetic(&Spec::default()).replace("flights each time), ", "flights each time) ");
    let run = parse("x", &text);
    assert_eq!(run.unparsed.get("late"), Some(&30));
    let s = summary_of(&text, &Selection::default());
    assert!(s.warnings.iter().any(|w| w.contains("late x30")));
}

#[test]
fn hot_ranges_are_weighted_by_window_frames() {
    let s = summary_of(&synthetic(&Spec::default()), &Selection::default());
    let big = s
        .hot
        .iter()
        .find(|h| h.range == "1000000+0x4000000")
        .unwrap();
    assert_eq!(big.listed, 6);
    assert!((big.mb - 134.22).abs() < 1e-9);
    assert!((big.written_mb.unwrap() - 0.5).abs() < 1e-9);
    let copied = s
        .hot
        .iter()
        .find(|h| h.range == "3000000+0x200000")
        .unwrap();
    assert_eq!(copied.memory.as_deref(), Some("0x20000"));
}

#[test]
fn comparison_calls_lower_times_better() {
    let before = summary_of(&synthetic(&Spec::default()), &Selection::default());
    let after_spec = Spec {
        frame_ms: |i| 150.0 + i as f64,
        cpu_ms: 100.0,
        back_ms: 133.0,
        ..Spec::default()
    };
    let after = summary_of(&synthetic(&after_spec), &Selection::default());
    let cmp = Comparison {
        before: &before,
        after: &after,
        threshold_pct: 5.0,
    };
    let deltas = cmp.deltas();
    let get = |k: &str| deltas.iter().find(|d| d.key == k).unwrap();
    assert_eq!(get("frame.ms").verdict, "better");
    assert_eq!(get("frame.fps").verdict, "better");
    assert_eq!(get("frame.back_ms").verdict, "same");
    assert!(
        (get("frame.cpu_ms").median_pct.unwrap() - 100.0 * (100.0 - 147.1) / 147.1).abs() < 1e-9
    );
    let md = cmp.markdown(false);
    assert!(md.contains("| Frame time (mean of a 1 s window) | `frame.ms` |"));
    let csv = cmp.csv();
    assert_eq!(csv.lines().count(), deltas.len() + 1);
    let json = cmp.json().to_string();
    assert!(json.starts_with('{') && json.contains("\"deltas\"") && json.contains("\"frame.ms\""));
    let worse = Comparison {
        before: &after,
        after: &before,
        threshold_pct: 5.0,
    };
    assert_eq!(
        worse
            .deltas()
            .iter()
            .find(|d| d.key == "frame.ms")
            .unwrap()
            .verdict,
        "worse"
    );
}

#[test]
fn summary_outputs_are_complete() {
    let text = synthetic(&Spec::default());
    let run = parse("synthetic.log", &text);
    let selected = vec![select(&run, &Selection::default())];
    let s = Summary::build(&Selection::default(), &selected);
    let csv = s.csv();
    assert_eq!(csv.lines().count(), s.metrics.len() + 1);
    let samples = super::report::samples_csv(&selected);
    let values: usize = run
        .ticks
        .iter()
        .chain(&run.windows)
        .chain(&run.gauges)
        .map(|s| s.values.len())
        .sum();
    assert_eq!(samples.lines().count(), values + 1);
    let json = s.json().to_string();
    for key in [
        "\"metrics\"",
        "\"costs\"",
        "\"top_costs\"",
        "\"hot_ranges\"",
        "\"runs\"",
    ] {
        assert!(json.contains(key), "{key}");
    }
    // Brackets balance (strings hold none in this synthetic log).
    assert_eq!(json.matches('{').count(), json.matches('}').count());
    let md = s.markdown(true, 3);
    assert!(md.contains("## Every metric"));
    assert!(md.contains("### compared_after_submissions"));
}

#[test]
fn two_logs_per_side_pool_their_samples() {
    let a = parse("a.log", &synthetic(&Spec::default()));
    let b = parse(
        "b.log",
        &synthetic(&Spec {
            frame_ms: |i| 300.0 + i as f64,
            ..Spec::default()
        }),
    );
    let sel = Selection::default();
    let s = Summary::build(&sel, &[select(&a, &sel), select(&b, &sel)]);
    assert_eq!(s.runs.len(), 2);
    assert_eq!(s.metrics["frame.ms"].n, 12);
    assert_eq!(s.runs[0].medians["frame.ms"], 216.5);
    assert_eq!(s.runs[1].medians["frame.ms"], 316.5);
    let one = stats::stats(&[(216.5, 0.0)]).unwrap();
    assert_eq!(one.median, 216.5);
}

/// Writes two synthetic logs to try the tool without the game:
/// `VICTORY_PERFLOG_SYNTHETIC=<dir> cargo test -p xtask -- --ignored write_synthetic_logs`
/// then `cargo xtask perf-log compare --before <dir>/before.log --after <dir>/after.log`.
#[test]
#[ignore = "writes files; run by hand"]
fn write_synthetic_logs() {
    let dir = std::path::PathBuf::from(
        std::env::var_os("VICTORY_PERFLOG_SYNTHETIC").expect("VICTORY_PERFLOG_SYNTHETIC=<dir>"),
    );
    std::fs::create_dir_all(&dir).unwrap();
    let before = Spec {
        frame_ms: |i| 201.8 + (i % 7) as f64 * 3.0,
        presents: true,
        ..Spec::default()
    };
    let after = Spec {
        frame_ms: |i| 170.0 + (i % 7) as f64 * 3.0,
        cpu_ms: 120.0,
        copy_ms: 20.0,
        presents: true,
        ..Spec::default()
    };
    std::fs::write(dir.join("before.log"), synthetic(&before)).unwrap();
    std::fs::write(dir.join("after.log"), synthetic(&after)).unwrap();
}

/// `card.extra_wait_ms` (`CARD_STATS[6]`) gets the relay thread's fetch
/// waits (`Host::relay_card_back`, counted in `relay.fetch_wait_ms` too)
/// and the submitting thread's (`Host::card_back`). With the relay only the
/// rest beyond the relay's part is sequential; without it all of it is.
#[test]
fn card_fetch_waits_are_split_by_thread() {
    // The real run's medians: 47.0 ms in all, 46.7 ms on the relay thread.
    let spec = Spec {
        card_wait_ms: 47.0,
        relay_fetch_ms: 46.7,
        frame_ms: |_| 267.9,
        ..Spec::default()
    };
    let s = summary_of(&synthetic(&spec), &Selection::default());
    let get = |k: &str| s.metrics[k].median;
    assert!((get("card.extra_wait_relay_ms") - 46.7).abs() < 1e-9);
    assert!((get("card.extra_wait_sync_ms") - 0.3).abs() < 1e-9);
    // Only the submitting thread's part joins the sequential path.
    assert!((get("frame.submit_path_ms") - (147.1 + 24.8 + 0.3)).abs() < 1e-9);
    let nature = |k: &str| {
        s.costs
            .iter()
            .find(|c| c.key == k)
            .map(|c| c.nature.clone())
    };
    assert_eq!(
        nature("card.extra_wait_relay_ms").as_deref(),
        Some("concurrent (relay thread)")
    );
    assert_eq!(
        nature("card.extra_wait_sync_ms").as_deref(),
        Some("submit-path")
    );
    // The total is not ranked (its parts are), and is labelled mixed.
    assert_eq!(nature("card.extra_wait_ms"), None);
    assert!(
        s.markdown(false, 8)
            .contains("| `card.extra_wait_ms` | ms | mixed (relay thread + submitting thread) |")
            || s.markdown(false, 8)
                .contains("`card.extra_wait_ms` | ms | mixed (relay thread + submitting thread)")
    );
    // The relay's two counters are taken a moment apart: its part is
    // capped at the total, never negative on the other side.
    let skewed = Spec {
        card_wait_ms: 10.0,
        relay_fetch_ms: 10.4,
        ..Spec::default()
    };
    let s = summary_of(&synthetic(&skewed), &Selection::default());
    assert_eq!(s.metrics["card.extra_wait_relay_ms"].median, 10.0);
    assert_eq!(s.metrics["card.extra_wait_sync_ms"].median, 0.0);

    // Without the relay every fetch wait is the submitting thread's.
    let sync = Spec {
        card_wait_ms: 47.0,
        relay: false,
        ..Spec::default()
    };
    let s = summary_of(&synthetic(&sync), &Selection::default());
    assert!(!s.metrics.contains_key("card.extra_wait_relay_ms"));
    assert_eq!(s.metrics["card.extra_wait_sync_ms"].median, 47.0);
    let total = s
        .costs
        .iter()
        .find(|c| c.key == "card.extra_wait_sync_ms")
        .unwrap();
    assert_eq!(total.nature, "submit-path");
    assert!(
        s.markdown(false, 8)
            .contains("`card.extra_wait_ms` | ms | submit-path |")
    );
}

/// The guards' protection lines (`crate::watch::stats`) are read with their
/// window, and the calls joined runs would need are derived.
#[test]
fn guard_protection_lines_are_read() {
    let run = parse("synthetic.log", &synthetic(&Spec::default()));
    assert!(run.unparsed.is_empty(), "{:?}", run.unparsed);
    let w = &run.windows[0];
    assert_eq!(w.get("protect.calls"), Some(2800.0));
    assert_eq!(w.get("protect.submitting_ms"), Some(18.0));
    assert_eq!(w.get("protect.alias_runs"), Some(120.0));
    assert_eq!(w.get("protect.other_calls"), Some(1070.0));
    assert_eq!(w.get("protect.open_failed"), Some(0.0));
    assert_eq!(w.get("guard_waits.fault_lock_ms"), Some(0.8));
    assert_eq!(w.get("guard_waits.check_joined"), Some(250.0));
    assert_eq!(
        w.get("protect.calls_if_joined"),
        Some(250.0 + 3.0 + 120.0 + 400.0 + 20.0 + 300.0 + 1.0)
    );
    let s = summary_of(&synthetic(&Spec::default()), &Selection::default());
    let md = s.markdown(false, 8);
    assert!(md.contains("## Guard protections (VirtualProtect)"));
    assert!(md.contains("`protect.submitting_ms` | ms | submit-path |"));
}

/// The lock lines (`crate::lockstat`) are read with their window: holds
/// and waits by site, the queue, the time by kind of thread, and the
/// counts around checks and faults.
#[test]
fn lock_lines_are_read() {
    let run = parse("synthetic.log", &synthetic(&Spec::default()));
    assert!(run.unparsed.is_empty(), "{:?}", run.unparsed);
    let w = &run.windows[0];
    assert_eq!(w.get("lock_guards.check_hold_ms"), Some(18.0));
    assert_eq!(w.get("lock_guards.check_holds_vlong"), Some(3.0));
    assert_eq!(w.get("lock_guards.fault_wait_ms"), Some(23.08));
    assert_eq!(w.get("lock_guards.fault_wait_max_ms"), Some(4.1));
    assert_eq!(w.get("lock_guards.write_wait_ms"), Some(1.33));
    assert_eq!(w.get("lock_guards.queue_mean"), Some(0.85));
    assert_eq!(w.get("lock_guards.queue_max"), Some(6.0));
    assert_eq!(w.get("lock_jv.vg_hold_ms"), Some(30.0));
    assert_eq!(w.get("lock_jv.v_queue_max"), Some(1.0));
    // In submissions, title submitting threads, the relay, other threads;
    // GUARDS, JOURNAL, VIEWS each: waits 1..12, holds 21..32 in order.
    assert_eq!(w.get("lock_roles.sub_guards_wait_ms"), Some(1.0));
    assert_eq!(w.get("lock_roles.title_guards_wait_ms"), Some(4.0));
    assert_eq!(w.get("lock_roles.title_guards_hold_ms"), Some(24.0));
    assert_eq!(w.get("lock_roles.other_views_hold_ms"), Some(32.0));
    assert_eq!(w.get("lock_extra.fault_wait_title_ms"), Some(6.0));
    assert_eq!(w.get("lock_extra.faults_found_open"), Some(16.0));
    let s = summary_of(&synthetic(&Spec::default()), &Selection::default());
    let md = s.markdown(false, 8);
    assert!(md.contains("## Lock contention (GUARDS, JOURNAL, VIEWS)"));
    assert!(
        md.contains("`lock_extra.fault_wait_title_ms` | ms | title-critical |"),
        "{md}"
    );
    assert!(md.contains("`lock_roles.sub_guards_wait_ms` | ms | submit-path |"));
}

/// The kinds of other thread (PR 01B-0b) are read, and what they leave of
/// "other threads" is derived: the rounding only.
#[test]
fn other_thread_kinds_are_read() {
    let run = parse("synthetic.log", &synthetic(&Spec::default()));
    assert!(run.unparsed.is_empty(), "{:?}", run.unparsed);
    let w = &run.windows[0];
    assert_eq!(w.get("lock_kinds.rec_guards_wait_ms"), Some(4.0));
    assert_eq!(w.get("lock_kinds.tother_guards_wait_ms"), Some(3.25));
    assert_eq!(w.get("lock_kinds.unc_guards_wait_ms"), Some(1.25));
    assert_eq!(w.get("lock_kinds.fault_rec_taken"), Some(120.0));
    assert_eq!(w.get("lock_kinds.fault_rec_wait_ms"), Some(2.5));
    assert_eq!(w.get("lock_kinds.write_tother_wait_ms"), Some(2.75));
    assert_eq!(w.get("lock_kinds.took_title_role"), Some(2.0));
    assert_eq!(w.get("lock_kinds.waited_before_ms"), Some(0.75));
    // other threads 10.00 = 4.00 + 1.50 + 3.25 + 1.25.
    let gap = w.get("lock_kinds.guards_wait_gap_ms").unwrap();
    assert!(gap.abs() < 1e-9, "{gap}");
    let s = summary_of(&synthetic(&Spec::default()), &Selection::default());
    let md = s.markdown(false, 8);
    assert!(md.contains("## Lock time by kind of other thread"), "{md}");
    assert!(
        md.contains("`lock_kinds.fault_rec_wait_ms` | ms | thread-sum |"),
        "{md}"
    );
}

fn bursty() -> Spec {
    Spec {
        ticks: 60,
        frame_ms: |i| {
            if (10..20).contains(&i) {
                700.0 + i as f64
            } else {
                200.0 + (i % 3) as f64
            }
        },
        burst: |i| (10..20).contains(&i),
        ..Spec::default()
    }
}

/// Windows with many texture uploads are bursts, the rest steady; ticks go
/// with the window that covers them; each side has its spread, and the
/// window metrics are correlated with the frame time.
#[test]
fn steady_periods_and_bursts_are_split() {
    use super::parse::split;
    use super::report::{Regimes, correlations};
    let run = parse("synthetic.log", &synthetic(&bursty()));
    let sel = Selection::default();
    let all = select(&run, &sel);
    let (steady, bursts) = split(&all, "account.tex_uploads", 100.0);
    // Windows at ticks 4, 9, ..., 59: those of 14 and 19 cover the burst.
    assert_eq!((steady.windows.len(), bursts.windows.len()), (10, 2));
    assert_eq!(bursts.ticks.len(), 10);
    assert!(
        bursts
            .ticks
            .iter()
            .all(|t| t.get("frame.ms").is_none_or(|ms| ms >= 700.0))
    );
    assert_eq!(steady.ticks.len() + bursts.ticks.len(), 60);
    let (a, b) = (
        Summary::build(&sel, &[steady]),
        Summary::build(&sel, &[bursts]),
    );
    assert_eq!(a.metrics["lock_guards.write_wait_ms"].median, 1.33);
    assert_eq!(b.metrics["lock_guards.write_wait_ms"].median, 103.81);
    assert_eq!(a.metrics["lock_guards.write_wait_ms"].iqr(), 0.0);
    let corr = correlations(&[select(&run, &sel)], 8);
    let r = |k: &str| corr.iter().find(|c| c.0 == k).map(|c| c.1);
    assert!(r("lock_guards.write_wait_ms").unwrap() > 0.9, "{corr:?}");
    // No spread: no correlation.
    assert_eq!(r("lock_guards.check_hold_ms"), None);
    let regimes = Regimes {
        steady: &a,
        bursts: &b,
        key: "account.tex_uploads".into(),
        min: 100.0,
        correlations: corr,
    };
    let md = regimes.markdown(false);
    assert!(md.contains("Copystat windows with `account.tex_uploads` >= 100 are bursts"));
    assert!(
        md.contains("| Writes told waiting for the guard lock |")
            || md.contains("`lock_guards.write_wait_ms`")
    );
    assert!(md.contains("## Correlation with the frame time, per copystat window"));
    let json = regimes.json().to_string();
    assert!(
        json.contains("\"correlations_with_frame_ms\"")
            && json.contains("\"iqr\"")
            && json.contains("\"cv\"")
    );
    assert!(regimes.csv().lines().count() > 10);
}
