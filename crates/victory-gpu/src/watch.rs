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
use std::sync::Mutex;

/// The unit tracked and copied: 64 KiB, the console's page (the kernel's
/// 4 KiB pages fold into it). Finer units made the checks of the world's
/// big buffers cost more than the copies they saved.
pub(crate) const PAGE: usize = 64 << 10;
/// The kernel's page, the unit of its answers.
const KERNEL_PAGE: usize = 4096;

/// A title-memory range whose changed pages another subsystem needs to read.
pub(crate) struct Tracked {
    start: usize,
    end: usize,
    seen: Mutex<BTreeMap<usize, u64>>,
}

/// Tracks a runtime GPU-memory range, including physical-page views when
/// `views` is true. Other process memory is not tracked.
pub(crate) fn track(start: usize, len: usize, views: bool) -> Option<Tracked> {
    let end = start.checked_add(len)?;
    if len == 0 || writes(start, end, views).is_none() {
        return None;
    }
    Some(Tracked {
        start,
        end,
        seen: Mutex::default(),
    })
}

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

// (A lock for readers and writers: during a submission every refresh asks
// the journal, from six threads at once, and nearly every answer is read from
// it; under one mutex they stood in line.)
static JOURNAL: std::sync::RwLock<Journal> = std::sync::RwLock::new(Journal {
    asked: 0,
    written: BTreeMap::new(),
    found: Vec::new(),
    ahead: Vec::new(),
    ahead_asked: 0,
    kept: false,
});

/// Experiment (`VICTORY_WATCH_STATS=1`): the kernel askings by where they
/// come from and how they end: ahead ok, ahead failed, `writes` ok,
/// `writes` failed (then the guards), the pages asked in each, and the
/// microseconds of each.
static ASKS: [std::sync::atomic::AtomicU64; 12] = [const { std::sync::atomic::AtomicU64::new(0) }; 12];
/// Of the askings that were answered: those that found no written page
/// (count, pages, microseconds), the pages found written in the others,
/// and askings spared by [`cold`] (count, pages).
static CLEAN: [std::sync::atomic::AtomicU64; 6] = [const { std::sync::atomic::AtomicU64::new(0) }; 6];

fn asks_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_WATCH_STATS").is_some_and(|v| v == "1"))
}

fn asked_kernel(kind: usize, pages: usize, started: std::time::Instant) {
    use std::sync::atomic::Ordering::Relaxed;
    ASKS[kind].fetch_add(1, Relaxed);
    ASKS[4 + kind].fetch_add(pages as u64, Relaxed);
    ASKS[8 + kind].fetch_add(started.elapsed().as_nanos() as u64 / 1000, Relaxed);
}

/// The counts since the last call, for the frame line's neighbour.
pub(crate) fn asks_say(frames: f64) {
    if !asks_on() || frames <= 0.0 {
        return;
    }
    let v: Vec<f64> = ASKS.iter().map(|c| c.swap(0, std::sync::atomic::Ordering::Relaxed) as f64 / frames).collect();
    let c: Vec<f64> = CLEAN.iter().map(|c| c.swap(0, std::sync::atomic::Ordering::Relaxed) as f64 / frames).collect();
    tracing::info!(
        "write-watch askings per frame, answered: {:.0} found nothing ({:.0} MB, {:.1} ms); the others found {:.0} pages written; spared as cold {:.0} ({:.0} MB)",
        c[0],
        c[1] * PAGE as f64 / 1048576.0,
        c[2] / 1000.0,
        c[3],
        c[4],
        c[5] * PAGE as f64 / 1048576.0
    );
    tracing::info!(
        "write-watch askings per frame (VICTORY_WATCH_STATS=1): ahead ok {:.0} ({:.0} MB, {:.1} ms), ahead failed {:.0} ({:.0} MB, {:.1} ms), single ok {:.0} ({:.0} MB, {:.1} ms), single failed {:.0} ({:.0} MB, {:.1} ms)",
        v[0], v[4] * PAGE as f64 / 1048576.0, v[8] / 1000.0,
        v[1], v[5] * PAGE as f64 / 1048576.0, v[9] / 1000.0,
        v[2], v[6] * PAGE as f64 / 1048576.0, v[10] / 1000.0,
        v[3], v[7] * PAGE as f64 / 1048576.0, v[11] / 1000.0,
    );
}

/// Whether pages `[first, last)` begin in a view of the title's physical
/// pages (`crate::inplace`'s record of what the runtime mapped). View
/// memory is not write-watched: the kernel refuses the asking, and the
/// guards answer instead. Knowing it beforehand saves the refused call:
/// in gameplay 25,000 of the 26,700 askings of a frame were refused, 62 ms
/// of the threads' time under the journal's lock (run 51).
/// `VICTORY_WATCH_VIEWS=0` asks the kernel all the same, as before.
fn in_view(first: usize, last: usize) -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ON.get_or_init(|| std::env::var_os("VICTORY_WATCH_VIEWS").is_none_or(|v| v != "0")) {
        return false;
    }
    let _ = last;
    crate::inplace::cover(first * PAGE, 1) != crate::inplace::Cover::None
}

/// `VICTORY_WATCH_COLD=<n>` (experiment, 0: off): a run of pages whose
/// last `n` askings found nothing written is asked once a frame, not at
/// every submission; the submissions between take it as not written. Half
/// of a gameplay frame's askings found nothing (771 of 1,187, 4.1 GB,
/// 27 ms on the submitting thread, run 53): buffers the title filled once.
///
/// What it gives up: a page of such a run written between two submissions
/// of one frame reaches the card with the next frame's first submission.
/// The first asking that finds a write makes the run asked every time
/// again.
fn cold_after() -> u32 {
    static N: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *N.get_or_init(|| std::env::var("VICTORY_WATCH_COLD").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(0))
}

/// Per run of pages: askings in a row that found nothing, and the frame of
/// the last one.
static COLD: Mutex<Option<std::collections::HashMap<(usize, usize), (u32, u64)>>> = Mutex::new(None);
/// The pages the last answered asking found written.
static LAST_FOUND: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// [`ask_kernel`], unless the run is cold and was asked this frame
/// ([`cold_after`]): then nothing is asked and it counts as answered.
fn ask_or_spare(j: &mut Journal, first: usize, last: usize) -> bool {
    use std::sync::atomic::Ordering::Relaxed;
    let after = cold_after();
    if after == 0 {
        return ask_kernel(j, first, last);
    }
    let frame = crate::frame::current();
    let mut cold = COLD.lock().unwrap_or_else(|e| e.into_inner());
    let cold = cold.get_or_insert_with(Default::default);
    if let Some(&(streak, at)) = cold.get(&(first, last))
        && streak >= after
        && at == frame
    {
        CLEAN[4].fetch_add(1, Relaxed);
        CLEAN[5].fetch_add((last - first) as u64, Relaxed);
        return true;
    }
    let ok = ask_kernel(j, first, last);
    if ok {
        if cold.len() > 200_000 {
            cold.clear();
        }
        let e = cold.entry((first, last)).or_insert((0, 0));
        e.0 = if LAST_FOUND.load(Relaxed) == 0 { e.0.saturating_add(1) } else { 0 };
        e.1 = frame;
    }
    ok
}

/// Ranges of one submission closer than this are asked about together:
/// walking the pages between them costs less than another kernel call (a
/// call that resets written pages costs 30-70 microseconds whatever the
/// range).
const AHEAD_GAP: usize = (8 << 20) / PAGE;
/// [`AHEAD_GAP`], or `VICTORY_WATCH_GAP_MB` (experiment: an asking that
/// resets written pages makes every core running the process drop them
/// from its address cache, which costs by the asking and by the cores:
/// 365 such askings took 47 ms of a gameplay frame, run 54).
fn ahead_gap() -> usize {
    static GAP: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *GAP.get_or_init(|| {
        std::env::var("VICTORY_WATCH_GAP_MB").ok().and_then(|v| v.trim().parse::<usize>().ok()).map_or(AHEAD_GAP, |mb| (mb << 20) / PAGE)
    })
}

/// The most one asking covers (its answer needs 8 bytes per 4 KiB page).
const AHEAD_MAX: usize = (1 << 30) / PAGE;

/// Asks the kernel about many ranges `(start, end)` at once, before the
/// [`writes`] of each: ranges close together are one asking, and `writes`
/// then answers from the journal until [`ask_ahead_done`]. The world's
/// submissions refresh thousands of host copies (6,664 askings a frame,
/// 456 ms, in run 14). Ranges that are not write-watched, or lie alone, are
/// left to `writes`.
pub(crate) fn ask_ahead(ranges: &[(usize, usize)]) {
    let mut pieces: Vec<(usize, usize)> = ranges
        .iter()
        .filter(|r| r.0 < r.1)
        .map(|&(start, end)| (start / PAGE, end.div_ceil(PAGE)))
        // View memory is the guards' (`in_view`).
        .filter(|&(first, last)| !in_view(first, last))
        .collect();
    if pieces.len() < 2 {
        return;
    }
    pieces.sort_unstable();
    let mut j = JOURNAL.write().unwrap_or_else(|e| e.into_inner());
    let j = &mut *j;
    // The submission's one asking stands ([`ask_once`]): what it does not
    // hold is left to `writes`.
    if j.kept {
        return;
    }
    // View memory the guards hold is left to `writes`, which answers it
    // from the guards ([`known_view`]): the kernel's write watch can only
    // fail there. Asked here with the others, a run holding such a range
    // failed, was asked again by halves down to single ranges, failing
    // each time, and the range then went to `writes` all the same (after
    // triippywhale d0581a55). `VICTORY_VIEW_ASK=1` asks as before.
    #[cfg(feature = "diagnostics")]
    let all = pieces.len();
    if skip_views_ahead() {
        pieces.retain(|&(first, _)| !known_view(first * PAGE));
    }
    #[cfg(feature = "diagnostics")]
    AHEAD_VIEWS.fetch_add((all - pieces.len()) as u64, std::sync::atomic::Ordering::Relaxed);
    j.asked += 1;
    j.ahead_asked = j.asked;
    j.ahead.clear();
    let mut i = 0;
    while i < pieces.len() {
        let first = pieces[i].0;
        let mut last = pieces[i].1;
        let mut k = i + 1;
        while k < pieces.len()
            && pieces[k].0 <= last + ahead_gap()
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

/// Since the last frame line: ranges of view memory the guards hold that a
/// submission's askings ahead ([`ask_ahead`], [`ask_once`]) left out. Each
/// was a kernel asking that failed, or several, before.
/// Whether the askings ahead leave out view memory the guards hold
/// (fxceless, after triippywhale cc368c7c / d0581a55; on there, off here
/// unless `VICTORY_AHEAD_SKIP_VIEWS=1`). The answers are the same either
/// way: the kernel's asking fails there and `writes` answers from the
/// guards.
fn skip_views_ahead() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_AHEAD_SKIP_VIEWS").is_some_and(|v| v == "1"))
}

#[cfg(feature = "diagnostics")]
pub(crate) static AHEAD_VIEWS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// [`SLOW`], taken.
#[cfg(feature = "diagnostics")]
pub(crate) fn slow_asks() -> [u64; 3] {
    [0, 1, 2].map(|i| SLOW[i].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// The submission's refreshes are done: `writes` asks the kernel again.
pub(crate) fn ask_ahead_done() {
    let mut j = JOURNAL.write().unwrap_or_else(|e| e.into_inner());
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
    let submits = TITLE_SUBMITS.load(std::sync::atomic::Ordering::Acquire);
    if ask_shared() {
        use std::sync::atomic::Ordering::Relaxed;
        let stamp = CURRENT_STAMP.load(Relaxed);
        let mut j = JOURNAL.write().unwrap_or_else(|e| e.into_inner());
        let j = &mut *j;
        // The title gave this submission before the asking that stands
        // was made: what it wrote before giving it is in that asking.
        if stamp != 0 && stamp <= ASK_STAMP.load(Relaxed) && !j.ahead.is_empty() {
            for &(start, end) in ranges {
                if start >= end {
                    continue;
                }
                let (first, last) = (start / PAGE, end.div_ceil(PAGE));
                if !j.ahead.iter().any(|&(a, b)| a <= first && last <= b) && !in_view(first, last) && ask_or_spare(j, first, last) {
                    j.ahead.push((first, last));
                }
            }
            j.kept = true;
            SHARED[0].fetch_add(1, Relaxed);
            return;
        }
        SHARED[1].fetch_add(1, Relaxed);
        let n = SHARED[1].load(Relaxed);
        if n.is_power_of_two() && n >= 256 {
            tracing::info!(shared = SHARED[0].load(Relaxed), asked = n, "submissions that took the asking before them, and that asked anew (VICTORY_ASK_SHARED=1)");
        }
        j.kept = false;
        j.ahead.clear();
    }
    ask_once_end();
    ask_ahead(ranges);
    ASK_STAMP.store(submits, std::sync::atomic::Ordering::Relaxed);
    let mut j = JOURNAL.write().unwrap_or_else(|e| e.into_inner());
    let j = &mut *j;
    if j.ahead.is_empty() {
        // `ask_ahead` asked nothing (fewer than two ranges).
        j.asked += 1;
        j.ahead_asked = j.asked;
    }
    // `ask_ahead` leaves a range that lies alone to `writes`, which asks
    // once per flush: with one asking for the submission that was every
    // such range asked again at each of its 70 flushes (run 76: 2,300
    // askings a frame with the switch on, as without it). They are asked
    // here, once; what is not write-watched stays with `writes`.
    for &(start, end) in ranges {
        if start >= end {
            continue;
        }
        // View memory the guards hold is not asked about: the asking can
        // only fail there, one failed kernel call for each such copy at
        // every submission, and `writes` answers it from the guards
        // (after triippywhale cc368c7c).
        if skip_views_ahead() && known_view(start) {
            #[cfg(feature = "diagnostics")]
            AHEAD_VIEWS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            continue;
        }
        let (first, last) = (start / PAGE, end.div_ceil(PAGE));
        if !j.ahead.iter().any(|&(a, b)| a <= first && last <= b) && !in_view(first, last) && ask_or_spare(j, first, last) {
            j.ahead.push((first, last));
        }
    }
    j.kept = !j.ahead.is_empty();
}

/// The submission's one asking stands no longer: its refreshes are done,
/// or title memory was written in a way the journal was not told.
pub(crate) fn ask_once_end() {
    // (With `ask_shared` the asking stands for the submissions the title
    // gave before it; `ask_once_void` ends it.)
    if ask_shared() {
        return;
    }
    let mut j = JOURNAL.write().unwrap_or_else(|e| e.into_inner());
    if j.kept {
        j.kept = false;
        j.ahead.clear();
    }
}

/// Title memory was written in a way the journal was not told: no asking
/// stands, with [`ask_shared`] too.
pub(crate) fn ask_once_void() {
    ASK_STAMP.store(0, std::sync::atomic::Ordering::Relaxed);
    let mut j = JOURNAL.write().unwrap_or_else(|e| e.into_inner());
    j.kept = false;
    j.ahead.clear();
}

/// Submissions the title has given, counted when it gives them.
pub(crate) static TITLE_SUBMITS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// [`TITLE_SUBMITS`] as it was when the submission being prepared was
/// given (0: not known).
pub(crate) static CURRENT_STAMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// [`TITLE_SUBMITS`] when the asking that stands was made.
static ASK_STAMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static SHARED: [std::sync::atomic::AtomicU64; 2] = [const { std::sync::atomic::AtomicU64::new(0) }; 2];

/// `VICTORY_ASK_SHARED=1` (experiment, with `VICTORY_ASYNC_SUBMIT`): one
/// asking of the kernel stands for every submission the title gave before
/// it was made. The title gives a frame's submissions in a burst and the
/// thread that runs the lines prepares them after it; each asked the
/// kernel for itself (35% of that thread's time, sampled), about what the
/// title wrote before giving it, which the asking made for the first of
/// the burst holds already. A submission names ranges the standing asking
/// lacks: those are asked and added.
/// shortcut: what we write into title memory ourselves between two such
/// submissions (the copy back of what shaders wrote) is not seen by the
/// second through another copy of that memory.
fn ask_shared() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_ASK_SHARED").is_some_and(|v| v == "1"))
}

/// We wrote title memory `[at, at + len)` while a submission's one asking
/// stands (the title's buffer copies, done at submission): its pages count
/// as written after that asking, so the refreshes that follow copy them.
pub(crate) fn wrote(at: usize, len: usize) {
    let mut j = JOURNAL.write().unwrap_or_else(|e| e.into_inner());
    if !j.kept || len == 0 {
        return;
    }
    j.asked += 1;
    let now = j.asked;
    // The asking that stands moves with it: `asked` and `writes` give it as
    // "now", and a page noted written after "now" stays stale for every
    // copy that takes it (copied at `now`, written at `now + 1`): copied
    // again at every refresh, and a reason to settle flights
    // (fxceless/victory-fork#410).
    j.ahead_asked = now;
    for page in at / PAGE..at.saturating_add(len).div_ceil(PAGE) {
        j.written.insert(page, now);
    }
}

/// We wrote the 64 KiB page numbered `page` of title memory through a
/// view of our own (`crate::inplace::write_own`): no guard was opened and
/// no kernel write-watch bit set, so the journal is told here. The page,
/// and the same physical page at its other title addresses (`others`,
/// page numbers), count as written at a new asking, which every host copy
/// that holds them takes at its next refresh. Returns the asking that had
/// last found `page` written (0: never) and the new one, for the copy
/// that wrote to note itself current.
pub(crate) fn wrote_own(page: usize, others: &[usize]) -> (u64, u64) {
    let mut j = JOURNAL.write().unwrap_or_else(|e| e.into_inner());
    let j = &mut *j;
    j.asked += 1;
    let now = j.asked;
    // As in [`wrote`]: an asking that stands for a submission, or for a
    // flush, moves with it, or a copy taking the page at the standing
    // asking would find it written "later" at every refresh.
    if j.kept || !j.ahead.is_empty() {
        j.ahead_asked = now;
    }
    let before = j.written.insert(page, now).unwrap_or(0);
    for &other in others {
        if other != page {
            j.written.insert(other, now);
        }
    }
    (before, now)
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
    if ask_or_spare(j, first, last) {
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
    if asks_on() {
        asked_kernel(usize::from(failed), last - first, started);
    }
    if failed {
        return false;
    }
    LAST_FOUND.store(count, std::sync::atomic::Ordering::Relaxed);
    if asks_on() {
        use std::sync::atomic::Ordering::Relaxed;
        if count == 0 {
            CLEAN[0].fetch_add(1, Relaxed);
            CLEAN[1].fetch_add((last - first) as u64, Relaxed);
            CLEAN[2].fetch_add(started.elapsed().as_nanos() as u64 / 1000, Relaxed);
        } else {
            CLEAN[3].fetch_add(count as u64, Relaxed);
        }
    }
    #[cfg(feature = "diagnostics")]
    {
        use crate::copystat::{C, add};
        add(C::Watches, 1);
        add(C::WatchUs, started.elapsed().as_micros() as u64);
        slow(started.elapsed().as_micros() as u64);
    }
    let now = j.ahead_asked;
    for k in 0..count {
        let page = j.found[k] / PAGE;
        j.written.insert(page, now);
    }
    true
}

/// What [`writes`] would say of `[start, end)` from the journal alone, if
/// the range lies in one asked ahead ([`ask_ahead`], [`ask_once`]): the
/// pages written since tracking began and the asking that last found each
/// written, with the number of the asking that stands (as [`writes`] gives
/// it). `None` if the kernel would have to be asked.
pub(crate) fn asked(start: usize, end: usize) -> Option<(u64, Vec<(usize, u64)>)> {
    let (first, last) = (start / PAGE, end.div_ceil(PAGE));
    let j = JOURNAL.read().unwrap_or_else(|e| e.into_inner());
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
    // Asked ahead with the submission's other ranges: read from the journal,
    // beside the other threads that read it.
    {
        let j = JOURNAL.read().unwrap_or_else(|e| e.into_inner());
        if j.ahead.iter().any(|&(a, b)| a <= first && last <= b) {
            return Some((j.ahead_asked, j.written.range(first..last).map(|(&p, &w)| (p, w)).collect()));
        }
    }
    // View memory is the guards' (`in_view`), which have a lock of their
    // own: asked without the journal's, which is taken to write only if
    // they found something. Every refresh of a copy of view memory took the
    // journal to write, and the six threads of a submission stood in line
    // behind it (45% of the submitting thread's samples, in a cutscene).
    if guarded && in_view(first, last) {
        let found = guard(first * PAGE, last * PAGE)?;
        if found.is_empty() {
            let j = JOURNAL.read().unwrap_or_else(|e| e.into_inner());
            return Some((j.asked, j.written.range(first..last).map(|(&p, &w)| (p, w)).collect()));
        }
        let mut j = JOURNAL.write().unwrap_or_else(|e| e.into_inner());
        j.asked += 1;
        let now = j.asked;
        for page in found {
            j.written.insert(page, now);
        }
        return Some((now, j.written.range(first..last).map(|(&p, &w)| (p, w)).collect()));
    }
    let mut j = JOURNAL.write().unwrap_or_else(|e| e.into_inner());
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
    let started = std::time::Instant::now();
    // View memory: the kernel would refuse (`in_view`, and the views the
    // guards already hold).
    let known_view = guarded && known_view(first * PAGE);
    let view = known_view || in_view(first, last);
    // SAFETY: the answer buffer holds `count` entries.
    let failed = view
        || unsafe {
            GetWriteWatch(
                WRITE_WATCH_FLAG_RESET,
                (first * PAGE) as *const _,
                (last - first) * PAGE,
                j.found.as_mut_ptr().cast(),
                &mut count,
                &mut granularity,
            )
        } != 0;
    if asks_on() && !view {
        asked_kernel(2 + usize::from(failed), last - first, started);
    }
    #[cfg(feature = "diagnostics")]
    crate::copystat::add(
        if view {
            crate::copystat::C::WatchKnownView
        } else {
            crate::copystat::C::WatchFailed
        },
        failed as u64,
    );
    #[cfg(feature = "diagnostics")]
    if failed && !known_view && guarded {
        failed_asking(first * PAGE, last * PAGE);
    }
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
    let pages = j.found[..count].iter().map(|&at| at / PAGE).chain(guarded);
    for page in pages.collect::<Vec<_>>() {
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

/// Diagnostics: one in 2,048 of the askings the kernel's write watch
/// refused for memory the guards did not know (some 200 a frame in the
/// first rooms, each followed by a question to Windows about the memory):
/// what the memory is and what the guards hold of its first block.
#[cfg(feature = "diagnostics")]
fn failed_asking(start: usize, end: usize) {
    use windows_sys::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, VirtualQuery};
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if !N.fetch_add(1, std::sync::atomic::Ordering::Relaxed).is_multiple_of(2048) {
        return;
    }
    // SAFETY: plain data filled in by the query.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a valid out pointer of the right size.
    let n = unsafe { VirtualQuery(start as *const _, &mut info, size_of_val(&info)) };
    let (protected, clean, absent, viewed, aliased) = {
        let g = guards();
        let b = start / BLOCK;
        (g.protected.contains(&b), g.clean.contains(&b), g.absent.contains(&b), g.viewed.contains(&b), g.aliased.contains(&b))
    };
    tracing::info!(
        "a failed write-watch asking (one in 2048): {start:#x}+{:#x}; Windows says (answered {n}): base {:#x} region {:#x} state {:#x} type {:#x} protect {:#x} allocation protect {:#x}; \
         the guards of its first block: protected {protected} clean {clean} absent {absent} viewed {viewed} aliased {aliased}; is a view: {}",
        end - start,
        info.BaseAddress as usize,
        info.RegionSize,
        info.State,
        info.Type,
        info.Protect,
        info.AllocationProtect,
        is_view(start, end)
    );
}

impl Tracked {
    /// Takes changed pages of `[from, to)`, cut to the tracked range. A
    /// partial page stays changed so a later take can read its other bytes.
    pub(crate) fn take(&self, from: usize, to: usize) -> Vec<(usize, usize)> {
        let (from, to) = (from.max(self.start), to.min(self.end));
        if from >= to {
            return Vec::new();
        }
        let Some((_, pages)) = writes(from, to, true) else {
            return Vec::new();
        };
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        for (page, write) in pages {
            if seen.get(&page).is_some_and(|&last| last >= write) {
                continue;
            }
            let (start, end) = ((page * PAGE).max(from), ((page + 1) * PAGE).min(to));
            if start >= end {
                continue;
            }
            // Keep partial pages dirty: the caller has not taken the rest.
            if (page * PAGE).max(self.start) >= from
                && ((page + 1) * PAGE).min(self.end) <= to
            {
                seen.insert(page, write);
            }
            match ranges.last_mut() {
                Some((_, last)) if *last == start => *last = end,
                _ => ranges.push((start, end)),
            }
        }
        ranges
    }
}

/// Guarded blocks of view memory.
const BLOCK: usize = PAGE;

/// Whether the block holding `start` is view memory the guards already
/// hold (read-only by our hand, or noted absent): asking the kernel's write
/// watch about it can only fail. `VICTORY_VIEW_ASK=1` (or
/// `VICTORY_GUARD_ASK=1`) asks the kernel anyway, as before.
fn known_view(start: usize) -> bool {
    static ASK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let ask = *ASK.get_or_init(|| {
        ["VICTORY_VIEW_ASK", "VICTORY_GUARD_ASK"]
            .iter()
            .any(|n| std::env::var_os(n).is_some_and(|v| v == "1"))
    });
    if ask {
        return false;
    }
    // By the bits, without the guards' lock (this runs with the journal's
    // held alone: taking the guards' lock alone here stood every asker of
    // the journal behind the fault handler). A hot block counts too: the
    // kernel refuses view memory, hot or not, and the guards answer.
    known_view_block(start / BLOCK)
        // fxceless's form (`VICTORY_VIEW_KNOWN=1`, off): blocks once guarded
        // stay known as view memory while their guard is open.
        || (view_known() && guards().viewed.contains(&(start / BLOCK)))
}

struct Guards {
    /// Blocks we made read-only.
    protected: BTreeSet<usize>,
    /// Of those, the ones not written since a copy took them.
    clean: BTreeSet<usize>,
    /// Blocks already copied as zeros. While they remain wholly uncommitted,
    /// the copy can skip hundreds of MB of failing kernel reads per world frame.
    absent: BTreeSet<usize>,
    /// Blocks whose physical page was guarded together with its other views
    /// (those views' blocks are in it too): a write to one opens them all.
    /// A write range can be huge (`fread`), so only these are looked up.
    aliased: BTreeSet<usize>,
    /// Faults that opened each block, for [`hot_after`]: counted until the
    /// block turns hot.
    heat: BTreeMap<usize, u32>,
    /// Hot blocks ([`hot_after`]): left writable and reported written at
    /// every asking, with how many askings have done so since the block
    /// turned hot (at [`HOT_COOL`] it is guarded again, once, to see).
    hot: BTreeMap<usize, u32>,
    /// Absent blocks found still uncommitted, "parked" ([`park_on`]): their
    /// clean bit is set while they stay so, and the walk passes them by.
    parked: BTreeSet<usize>,
    /// With [`view_known`]: blocks made read-only at least once since the
    /// runtime last mapped or unmapped them. They are view memory whether
    /// their guard stands now or a write has opened it.
    viewed: BTreeSet<usize>,
}

/// Whether a block once guarded stays known as view memory while its
/// guard is open (`VICTORY_VIEW_KNOWN=1`; off by default).
///
/// A range whose first block the title wrote since the last check has
/// that block neither read-only nor noted absent, so nothing says it is a
/// view: the kernel's write watch is asked and fails, and then Windows is
/// asked what the memory is (`is_view`), a walk of the page tables to the
/// end of the pages that share the first one's state. In runs 222 and 225
/// that was 338-450 failed askings a frame and 6.5-7.8 ms a frame of the
/// submitting thread in `VirtualQuery`. With the switch the block is
/// remembered from the time it was first made read-only until the runtime
/// maps or unmaps it ([`title_memory_remapped`]).
///
/// `VICTORY_VIEW_KNOWN=alt`: twenty seconds with, twenty without, in turn,
/// each change said in the log, for measuring it in one scene. (Blocks
/// noted in one phase are forgotten at a remapping in either.)
fn view_known() -> bool {
    /// 0 off, 1 on, 2 in turn.
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    match *MODE.get_or_init(|| match std::env::var("VICTORY_VIEW_KNOWN").as_deref() {
        Ok("1") => 1,
        Ok("alt") => 2,
        _ => 0,
    }) {
        0 => false,
        1 => true,
        _ => {
            static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
            let on = (EPOCH.get_or_init(std::time::Instant::now).elapsed().as_secs() / 20) % 2 == 1;
            static LAST: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(2);
            if LAST.swap(u8::from(on), std::sync::atomic::Ordering::Relaxed) != u8::from(on) {
                tracing::info!(
                    "view memory stays known while its guard is open from now (VICTORY_VIEW_KNOWN=alt): {}",
                    if on { "ON" } else { "off, as usual" }
                );
            }
            on
        }
    }
}

// (Readers and writers: nearly every asking finds every block of its range
// clean, and six threads ask at once during a submission.)
static GUARDS: std::sync::RwLock<Guards> = std::sync::RwLock::new(Guards {
    protected: BTreeSet::new(),
    clean: BTreeSet::new(),
    absent: BTreeSet::new(),
    aliased: BTreeSet::new(),
    heat: BTreeMap::new(),
    hot: BTreeMap::new(),
    parked: BTreeSet::new(),
    viewed: BTreeSet::new(),
});

/// Whether blocks found uncommitted are parked (experiment,
/// `VICTORY_GUARD_PARK=1`; off by default: with it on, the opening
/// cutscene's lights were wrong, off they were right, same build. It freed
/// 15-20 ms of the submitting thread's frame, copies 40 -> 25 ms, but the
/// card then stood 87-97% busy and the frame rate stayed ~7.7). The walk over a range's blocks that are not clean asked
/// Windows about every absent block at every asking (`uncommitted_end`):
/// tens of thousands of 64 KiB placeholders, each its own region, some
/// 88,000 questions a frame (found by Peppacodes, fxceless/victory-fork PR
/// #812: guards 26 -> 14 ms a frame on an RTX 5080). Parked, such a block
/// counts as clean by the bits until the runtime maps memory there
/// ([`title_memory_remapped`]), the title commits it
/// ([`title_memory_committed`]), or every [`PARK_FRAMES`] frames, when all
/// are looked at again (a commit no one told of).
fn park_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_GUARD_PARK").is_some_and(|v| v == "1") && clean_bits().is_some())
}

/// Frames between two looks at every parked block.
const PARK_FRAMES: u64 = 64;

/// Parks absent block `block`, still uncommitted ([`park_on`]).
fn park(g: &mut Guards, block: usize) {
    if block < CLEAN_BLOCKS && g.parked.insert(block) {
        clean_bit(block, true);
    }
}

/// Takes the parked blocks of `blocks` back: walked again at the next asking.
fn unpark(g: &mut Guards, blocks: std::ops::Range<usize>) {
    if g.parked.is_empty() {
        return;
    }
    let back: Vec<usize> = g.parked.range(blocks).copied().collect();
    for block in back {
        g.parked.remove(&block);
        if !g.clean.contains(&block) {
            clean_bit(block, false);
        }
    }
}

/// Every [`PARK_FRAMES`] frames, every parked block is looked at again.
fn unpark_epoch() {
    use std::sync::atomic::Ordering::Relaxed;
    static EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let epoch = crate::frame::current() / PARK_FRAMES + 1;
    if EPOCH.load(Relaxed) == epoch || EPOCH.swap(epoch, Relaxed) == epoch {
        return;
    }
    let mut g = guards();
    unpark(&mut g, 0..usize::MAX);
}

/// The title committed `[at, at + len)` through the runtime: blocks found
/// uncommitted there are no longer so.
pub fn title_memory_committed(at: usize, len: usize) {
    if !park_on() {
        return;
    }
    let mut g = guards();
    let range = at / BLOCK..at.saturating_add(len).div_ceil(BLOCK);
    if g.parked.range(range.clone()).next().is_none() {
        return;
    }
    unpark(&mut g, range.clone());
    let absent: Vec<usize> = g.absent.range(range).copied().collect();
    for block in absent {
        g.absent.remove(&block);
        known_bit(&g, block);
    }
}

/// After how many faults a block is left unguarded ("hot"):
/// `VICTORY_GUARD_HOT` (experiment, off unless set: 0 never). With it at 3
/// in the opening cutscene faults fell from ~370 to ~95 a frame but the
/// refreshes copied 633 MB a frame instead of ~125 (every copy over a hot
/// block takes it whole at each refresh), 3.8 fps instead of ~7.5, and
/// textures were missing. The title writes some
/// blocks every frame (constants): guarding them cost a fault, a
/// `VirtualProtect` to open and one to guard again, each under the guards'
/// lock, only to learn every time that they changed (opening cutscene:
/// ~370 faults and ~1,150 blocks guarded again a frame, the title's threads
/// waiting 26-39 ms a frame in all for the lock in the fault handler). A
/// hot block is copied at every refresh instead: 64 KiB, cheaper than one
/// fault.
fn hot_after() -> u32 {
    static N: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *N.get_or_init(|| std::env::var("VICTORY_GUARD_HOT").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(0))
}

/// Askings a hot block is reported written before it is guarded again once,
/// to see whether the title still writes it.
const HOT_COOL: u32 = 512;

/// `Guards::clean` again as bits, one a block, for blocks under
/// [`CLEAN_BLOCKS`]: read without the lock by [`guard`]'s first look
/// (`VICTORY_GUARD_BITS=0`: under the lock, walking the set, as before).
/// Written only with the lock held alone, with the set.
const CLEAN_BLOCKS: usize = 1 << 26;

fn clean_bits() -> Option<&'static [std::sync::atomic::AtomicU64]> {
    static BITS: std::sync::OnceLock<Option<Box<[std::sync::atomic::AtomicU64]>>> = std::sync::OnceLock::new();
    BITS.get_or_init(|| {
        std::env::var_os("VICTORY_GUARD_BITS")
            .is_none_or(|v| v != "0")
            .then(|| (0..CLEAN_BLOCKS / 64).map(|_| std::sync::atomic::AtomicU64::new(0)).collect())
    })
    .as_deref()
}

fn clean_bit(block: usize, on: bool) {
    use std::sync::atomic::Ordering::SeqCst;
    if block < CLEAN_BLOCKS && let Some(bits) = clean_bits() {
        let (word, bit) = (&bits[block / 64], 1u64 << (block % 64));
        if on {
            word.fetch_or(bit, SeqCst);
        } else {
            word.fetch_and(!bit, SeqCst);
        }
    }
}

/// `Guards::protected` or `Guards::absent` holds the block, as bits, one a
/// block under [`CLEAN_BLOCKS`]: [`guard`] asks whether a range's first
/// block is a known view at every refresh, and that look took the guards'
/// lock beside the threads that hold it alone for `VirtualProtect` (a
/// fifth of the submitting thread's time in contention, sampled in the
/// opening cutscene). Written only with the lock held alone, after the sets
/// (`known_bit`); `VICTORY_GUARD_KNOWN_BITS=0` looks under the lock as before.
fn known_bits() -> Option<&'static [std::sync::atomic::AtomicU64]> {
    static BITS: std::sync::OnceLock<Option<Box<[std::sync::atomic::AtomicU64]>>> = std::sync::OnceLock::new();
    BITS.get_or_init(|| {
        std::env::var_os("VICTORY_GUARD_KNOWN_BITS")
            .is_none_or(|v| v != "0")
            .then(|| (0..CLEAN_BLOCKS / 64).map(|_| std::sync::atomic::AtomicU64::new(0)).collect())
    })
    .as_deref()
}

/// `Guards::absent` again as bits, one a block under [`CLEAN_BLOCKS`]:
/// [`drop_uncommitted`] reads them without the lock. Written only with the
/// lock held alone, after the set, by [`known_bit`] (called at every change
/// of `absent`). `VICTORY_GUARD_ABSENT_BITS=0`: every absent block is asked
/// about under the lock, as before.
fn absent_bits() -> Option<&'static [std::sync::atomic::AtomicU64]> {
    static BITS: std::sync::OnceLock<Option<Box<[std::sync::atomic::AtomicU64]>>> = std::sync::OnceLock::new();
    BITS.get_or_init(|| {
        std::env::var_os("VICTORY_GUARD_ABSENT_BITS")
            .is_none_or(|v| v != "0")
            .then(|| (0..CLEAN_BLOCKS / 64).map(|_| std::sync::atomic::AtomicU64::new(0)).collect())
    })
    .as_deref()
}

/// Sets `block`'s bits of [`known_bits`] and [`absent_bits`] to what the
/// sets say now.
fn known_bit(g: &Guards, block: usize) {
    use std::sync::atomic::Ordering::SeqCst;
    if block < CLEAN_BLOCKS && let Some(bits) = absent_bits() {
        let (word, bit) = (&bits[block / 64], 1u64 << (block % 64));
        if g.absent.contains(&block) {
            word.fetch_or(bit, SeqCst);
        } else {
            word.fetch_and(!bit, SeqCst);
        }
    }
    if block < CLEAN_BLOCKS && let Some(bits) = known_bits() {
        let (word, bit) = (&bits[block / 64], 1u64 << (block % 64));
        if g.protected.contains(&block) || g.absent.contains(&block) || g.hot.contains_key(&block) {
            word.fetch_or(bit, SeqCst);
        } else {
            word.fetch_and(!bit, SeqCst);
        }
    }
}

/// Whether `block` is a known view (protected or noted absent): by the
/// bits without the lock, else under it.
fn known_view_block(block: usize) -> bool {
    use std::sync::atomic::Ordering::SeqCst;
    if block < CLEAN_BLOCKS && let Some(bits) = known_bits() {
        return bits[block / 64].load(SeqCst) & (1u64 << (block % 64)) != 0;
    }
    let g = GUARDS.read().unwrap_or_else(|e| e.into_inner());
    g.protected.contains(&block) || g.absent.contains(&block) || g.hot.contains_key(&block)
}

/// Whether every block of `first..last` is clean by the bits; `None` if
/// the bits do not say (off, or blocks past them).
fn clean_by_bits(first: usize, last: usize) -> Option<bool> {
    use std::sync::atomic::Ordering::SeqCst;
    let bits = clean_bits()?;
    if last > CLEAN_BLOCKS || last <= first {
        return None;
    }
    let (w0, w1) = (first / 64, (last - 1) / 64);
    for w in w0..=w1 {
        let mut mask = u64::MAX;
        if w == w0 {
            mask &= u64::MAX << (first % 64);
        }
        if w == w1 {
            mask &= u64::MAX >> (63 - (last - 1) % 64);
        }
        if bits[w].load(SeqCst) & mask != mask {
            return Some(false);
        }
    }
    Some(true)
}

/// Since the last frame-rate line: title writes that faulted on a guarded
/// block and opened it, blocks made read-only (again), the microseconds the
/// fault handler waited for the guards' lock, blocks opened ([`open_here`])
/// and the calls that opened them.
pub(crate) static GUARD_STATS: [std::sync::atomic::AtomicU64; 5] = [const { std::sync::atomic::AtomicU64::new(0) }; 5];

fn guards() -> std::sync::RwLockWriteGuard<'static, Guards> {
    GUARDS.write().unwrap_or_else(|e| e.into_inner())
}

/// Whether the block `page` is read-only by our hand and unwritten since a
/// copy took it (diagnostics of `VICTORY_BACK_OWN_CHECK`).
pub(crate) fn clean(page: usize) -> bool {
    guards().clean.contains(&page)
}

/// For view memory: the pages of `[start, end)`'s blocks written since
/// they were last guarded (all of a block's pages), guarding them again
/// before the caller copies them. `None` if the memory is not a view.
fn guard(start: usize, end: usize) -> Option<Vec<usize>> {
    // Every block of the range guarded and clean: nothing written, said
    // beside the other threads that ask (and without a list of the blocks).
    {
        let (first, last) = (start / BLOCK, end.div_ceil(BLOCK));
        match clean_by_bits(first, last) {
            Some(true) => return Some(Vec::new()),
            Some(false) => {}
            None => {
                let g = GUARDS.read().unwrap_or_else(|e| e.into_inner());
                if last > first && g.protected.contains(&first) && g.clean.range(first..last).count() == last - first {
                    return Some(Vec::new());
                }
            }
        }
    }
    // A range whose first block the guards hold already (read-only by our
    // hand, or noted absent) is a view: no question to Windows. The
    // question walks the page tables to the end of the pages that share the
    // first one's state, and a guarded view is one long run of read-only
    // pages: 0.6 ms for the title's 174 MB heap, 16 times a frame, and most
    // of the 68 ms a frame of "guards" (run 58).
    // `VICTORY_GUARD_ASK=1` asks every time, as before.
    static ASK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let known = !*ASK.get_or_init(|| std::env::var_os("VICTORY_GUARD_ASK").is_some_and(|v| v == "1"))
        && (known_view_block(start / BLOCK) || (view_known() && guards().viewed.contains(&(start / BLOCK))));
    if !known && !is_view(start, end) {
        return None;
    }
    install();
    let (first, last) = (start / BLOCK, end.div_ceil(BLOCK));
    // By the bits: the runs of blocks that are not clean, found without
    // the lock; only those are walked with it held (a range of 174 MB with
    // a few written pages held it for all its 42,000 blocks, and the
    // threads that refresh at once stood behind one another).
    if let Some(bits) = clean_bits()
        && last <= CLEAN_BLOCKS
    {
        use std::sync::atomic::Ordering::SeqCst;
        if park_on() {
            unpark_epoch();
        }
        let mut runs: Vec<(usize, usize)> = Vec::new();
        let mut block = first;
        while block < last {
            let word = bits[block / 64].load(SeqCst);
            if block % 64 == 0 && word == u64::MAX && block + 64 <= last {
                block += 64;
                continue;
            }
            if word & (1 << (block % 64)) == 0 {
                match runs.last_mut() {
                    Some(r) if r.1 == block => r.1 = block + 1,
                    _ => runs.push((block, block + 1)),
                }
            }
            block += 1;
        }
        // Absent blocks still uncommitted: `guard_run` would pass them by
        // with nothing done, after a question to Windows each, with the
        // lock held alone. Asked here, without it.
        if !park_on()
            && let Some(absent) = absent_bits()
        {
            runs = drop_uncommitted(runs, absent);
        }
        let mut written = Vec::new();
        if runs.is_empty() {
            return Some(written);
        }
        let mut g = guards();
        for (from, to) in runs {
            guard_run(&mut g, from, to, &mut written);
        }
        return Some(written);
    }
    let mut g = guards();
    let mut written = Vec::new();
    guard_run(&mut g, first, last, &mut written);
    Some(written)
}

/// `runs` (blocks, in order) less their absent blocks that are still
/// uncommitted, by [`absent_bits`] and without the guards' lock.
///
/// `guard_run` does nothing with such a block (park off) but ask Windows
/// whether it is still uncommitted, one 64 KiB placeholder, one region,
/// one question at a time: some 88,000 a frame in the opening cutscene
/// (fxceless/victory-fork PR #812), every one of them with the lock held
/// alone, so the threads that refresh and the title's threads in the fault
/// handler stood in line behind them. The answer is the same asked here: a
/// block that is committed meanwhile is seen at the next asking either way
/// (it stays absent and not clean until a check finds it committed). A
/// block whose bit is stale is either kept (`guard_run` decides under the
/// lock, as before) or was dropped as if this asking had come just before
/// the change. Nothing is parked, nothing is noted clean.
fn drop_uncommitted(runs: Vec<(usize, usize)>, absent: &[std::sync::atomic::AtomicU64]) -> Vec<(usize, usize)> {
    use std::sync::atomic::Ordering::SeqCst;
    let mut kept: Vec<(usize, usize)> = Vec::with_capacity(runs.len());
    // One question answers for the rest of its region (runs are in order).
    let mut uncommitted_until = 0;
    for (from, to) in runs {
        for block in from..to {
            if absent[block / 64].load(SeqCst) & (1u64 << (block % 64)) != 0 {
                if block >= uncommitted_until {
                    uncommitted_until = uncommitted_end(block);
                }
                if block < uncommitted_until {
                    continue;
                }
            }
            match kept.last_mut() {
                Some(r) if r.1 == block => r.1 = block + 1,
                _ => kept.push((block, block + 1)),
            }
        }
    }
    kept
}

/// [`guard`] for the blocks `first..last`, with the lock held alone.
fn guard_run(g: &mut Guards, first: usize, last: usize, written: &mut Vec<usize>) {
    let mut run: Option<usize> = None;
    // The range's clean blocks, in order: read off the set in one pass,
    // not looked up one block at a time. A frame asks about some 10 GB of
    // view memory, 150,000 blocks, nearly all clean: the lookups were most
    // of the 60-90 ms a frame of "guards" (run 54). What the loop adds to
    // the set meanwhile is the block it stands at or ones before it.
    let clean: Vec<usize> = g.clean.range(first..last).copied().collect();
    if clean.len() == last - first {
        // Nothing written, mapped or new since the range was last taken.
        return;
    }
    let mut next_clean = 0;
    let mut uncommitted_until = first;
    for block in first..=last {
        while next_clean < clean.len() && clean[next_clean] < block {
            next_clean += 1;
        }
        let is_clean = next_clean < clean.len() && clean[next_clean] == block;
        let mut open = block < last && !is_clean;
        if open && g.absent.contains(&block) {
            // Ordinary commits need no runtime mapping notification. Reuse
            // this asking's query for the following blocks in the same region.
            if block >= uncommitted_until {
                uncommitted_until = uncommitted_end(block);
            }
            open = block >= uncommitted_until;
            if open {
                g.absent.remove(&block);
                known_bit(g, block);
            } else if park_on() {
                park(g, block);
            }
        }
        if open {
            written.extend(block * BLOCK / PAGE..(block + 1) * BLOCK / PAGE);
            if g.protected.contains(&block) {
                // Written through the runtime, or through another view of
                // its pages (`open`), still read-only. That view may be one
                // no copy checks: guarded again here, with the others.
                g.clean.insert(block);
                clean_bit(block, true);
                if view_guard() {
                    protect_aliases(g, block);
                }
            } else {
                // A hot block stays writable: reported written (above) at
                // every asking, and not guarded, until it has been so for
                // `HOT_COOL` askings; then guarded once more to see.
                let stays_hot = match g.hot.get_mut(&block) {
                    Some(asked) if *asked + 1 < HOT_COOL => {
                        *asked += 1;
                        true
                    }
                    Some(_) => {
                        g.hot.remove(&block);
                        false
                    }
                    None => false,
                };
                if !stays_hot {
                    run.get_or_insert(block);
                    continue;
                }
            }
        }
        if let Some(from) = run.take() {
            protect(g, from, block);
        }
    }
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
    for (base, bytes) in crate::inplace::views_between(from.saturating_mul(BLOCK), to.saturating_mul(BLOCK)) {
        let first = (base / BLOCK).max(from);
        let last = (base.saturating_add(bytes).div_ceil(BLOCK)).min(to);
        for block in first..last {
            protect_aliases(g, block);
        }
    }
}

/// Makes the other views of `block`'s pages read-only and notes them all
/// guarded together, `block` included (see [`protect`]).
fn protect_aliases(g: &mut Guards, block: usize) {
    let shown = crate::inplace::pages_of(block * BLOCK);
    // One view (nothing else to guard), or the page the title shows
    // wherever it has nothing (thousands of views): each view of that one
    // is guarded by itself.
    if shown.len() < 2 || shown.len() > MANY_VIEWS {
        return;
    }
    #[cfg(feature = "diagnostics")]
    ALIASES[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    for alias in shown {
        let alias = alias / BLOCK;
        g.aliased.insert(alias);
        if alias != block {
            protect_alias(g, alias);
        }
    }
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
fn protect_alias(g: &mut Guards, block: usize) {
    use windows_sys::Win32::System::Memory::{PAGE_READONLY, VirtualProtect};
    if g.protected.contains(&block) {
        return;
    }
    let mut old = 0;
    // SAFETY: title view memory; only its protection changes.
    if unsafe { VirtualProtect((block * BLOCK) as *const _, BLOCK, PAGE_READONLY, &mut old) } != 0 {
        g.protected.insert(block);
        known_bit(g, block);
        if view_known() {
            g.viewed.insert(block);
        }
    }
}

/// [`protect`] for one range, without the other views of its pages.
fn protect_here(g: &mut Guards, from: usize, to: usize) {
    use windows_sys::Win32::System::Memory::{PAGE_READONLY, VirtualProtect};
    let set = |from: usize, to: usize| {
        let mut old = 0;
        // SAFETY: title view memory; only its protection changes.
        unsafe {
            VirtualProtect(
                (from * BLOCK) as *const _,
                (to - from) * BLOCK,
                PAGE_READONLY,
                &mut old,
            ) != 0
        }
    };
    let note = |g: &mut Guards, from: usize, to: usize| {
        GUARD_STATS[1].fetch_add((to - from) as u64, std::sync::atomic::Ordering::Relaxed);
        g.protected.extend(from..to);
        g.clean.extend(from..to);
        (from..to).for_each(|b| clean_bit(b, true));
        (from..to).for_each(|b| known_bit(g, b));
        if view_known() {
            g.viewed.extend(from..to);
        }
    };
    if set(from, to) {
        note(g, from, to);
        return;
    }
    for block in from..to {
        if set(block, block + 1) {
            note(g, block, block + 1);
        } else {
            #[cfg(feature = "diagnostics")]
            let error = std::io::Error::last_os_error();
            if block < uncommitted_end(block) {
                g.absent.insert(block);
                known_bit(g, block);
            } else {
                // Diagnostics: a committed block that will not be made
                // read-only is reported written at every asking and never
                // known to the guards (one in 1,024 said).
                #[cfg(feature = "diagnostics")]
                {
                    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                    if N.fetch_add(1, std::sync::atomic::Ordering::Relaxed).is_multiple_of(1024) {
                        tracing::info!("a block of title memory could not be made read-only (one in 1024): {:#x}: {error}", block * BLOCK);
                    }
                }
            }
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
/// every other view of them are noted written too: the write that faulted
/// (or is ours, `crate::host::copy_mapped`) goes through one view, and
/// every guard over the pages must copy them again.
///
/// Those other views stay read-only (fxceless/victory-fork PR #853):
/// `guard` reports a block that is read-only and not clean as written, and
/// a write through one of them still faults and opens it. Opening them all
/// cost two changes of protection for every other view of every page
/// written, each under the guards' lock.
fn open(g: &mut Guards, start: usize, end: usize) -> bool {
    let any = open_here(g, start, end);
    if start >= end || g.aliased.is_empty() {
        return any;
    }
    let aliased: Vec<usize> = g.aliased.range(start / BLOCK..end.div_ceil(BLOCK)).copied().collect();
    for block in aliased {
        // Guarded with the others again when its own check, or the next
        // check of one of them, finds the pages written.
        g.aliased.remove(&block);
        for alias in crate::inplace::pages_of(block * BLOCK) {
            let alias = alias / BLOCK;
            // Left read-only, noted written: the clean bit goes with the
            // set, or `guard`'s look by the bits would pass it by.
            if alias != block && g.protected.contains(&alias) && g.clean.remove(&alias) {
                clean_bit(alias, false);
                #[cfg(feature = "diagnostics")]
                ALIASES[1].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }
    any
}

/// [`open`] for one range, without the other views of its pages.
fn open_here(g: &mut Guards, start: usize, end: usize) -> bool {
    use windows_sys::Win32::System::Memory::{PAGE_READWRITE, VirtualProtect};
    let blocks: Vec<usize> = g
        .protected
        .range(start / BLOCK..end.div_ceil(BLOCK))
        .copied()
        .collect();
    // The blocks are in order, so each run of them that follow one another
    // is given back in one call (after upstream #1059): a long write
    // (`fread`) opens hundreds of blocks, and every call walks the page
    // tables. Each block's own notes are as before, all of a run's before
    // its call (a block noted not clean a moment early is only looked at).
    let mut calls = 0u64;
    let mut at = 0;
    while at < blocks.len() {
        let first = blocks[at];
        let mut end = first + 1;
        at += 1;
        while at < blocks.len() && blocks[at] == end {
            end += 1;
            at += 1;
        }
        for block in first..end {
            g.protected.remove(&block);
            g.clean.remove(&block);
            clean_bit(block, false);
            known_bit(g, block);
        }
        let mut old = 0;
        calls += 1;
        // SAFETY: blocks we made read-only.
        let set = unsafe { VirtualProtect((first * BLOCK) as *const _, (end - first) * BLOCK, PAGE_READWRITE, &mut old) };
        if set == 0 && end - first > 1 {
            // A run over two views (or a placeholder and a view) is not one
            // region the kernel changes in one call: one block at a time,
            // as before.
            for block in first..end {
                let mut old = 0;
                calls += 1;
                // SAFETY: a block we made read-only.
                unsafe { VirtualProtect((block * BLOCK) as *const _, BLOCK, PAGE_READWRITE, &mut old) };
            }
        }
    }
    GUARD_STATS[3].fetch_add(blocks.len() as u64, std::sync::atomic::Ordering::Relaxed);
    GUARD_STATS[4].fetch_add(calls, std::sync::atomic::Ordering::Relaxed);
    !blocks.is_empty()
}

/// Before our own write into title memory `[at, at + len)` through the
/// kernel (which does not fault into our handler): opens its guarded blocks.
/// The runtime calls it (as `kernel_will_write`) before the title's kernel
/// reads into its own memory.
pub fn writing(at: usize, len: usize) {
    // The length is the title's (`fread`'s size times count, saturated).
    open(&mut guards(), at, at.saturating_add(len));
    // A placed texture there changes with no descriptor written.
    crate::descriptor::host::remapped(at, len);
}

/// What `VICTORY_PAGE_WRITES` asks to be done with a write the guards do
/// not see once its bytes are there ([`title_memory_written`]), as bits:
/// 1, the guarded blocks under it are opened and count as written (host
/// copies of buffers and textures read through tables take them again);
/// 2, the placed textures picked by index that lie under it are due again;
/// 4, both also for reads to plain addresses when they have finished.
/// Unset or `guards`: bit 1 alone. `0`: nothing, counted only (the default
/// until run 199). `1`: bits 1 and 2. `slots`: bit 2 alone. `all`:
/// everything.
/// (Bit 1 by default: in every run 10,000 to 50,000 guarded blocks of host
/// copies lay under such writes and kept the bytes from before; runs 197
/// and 198 opened them, 9,766 and 10,796 blocks, with nothing worse for
/// it. Bit 2 makes texture slots due and is the one that flooded the
/// queue in an earlier form: still asked for by name.)
pub(crate) fn page_writes() -> u8 {
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| match std::env::var("VICTORY_PAGE_WRITES").as_deref().map(str::trim) {
        Ok("0") => 0,
        Ok("1") => 3,
        Ok("slots") => 2,
        Ok("all") => 7,
        _ => 1,
    })
}

/// Since the start: writes into physical pages through the runtime's own
/// views that landed at addresses the title has mapped (one for each run
/// of addresses); their bytes; guarded blocks that lay under them (host
/// copies that would otherwise keep the bytes from before); reads to
/// plain addresses that finished.
pub(crate) static WRITTEN: [std::sync::atomic::AtomicU64; 4] = [const { std::sync::atomic::AtomicU64::new(0) }; 4];

/// The bytes of a write the guards do not see are there: title memory
/// `[at, at + len)`. `pages`: DirectStorage wrote them into physical pages
/// through a view of the runtime's own, and this is an address where the
/// title has those pages mapped (no title address faulted, nothing was
/// mapped or unmapped; the shortcut this file's header names). Otherwise a
/// read to a plain address has finished, of which [`writing`] was told
/// before it began.
///
/// Upstream's c75fdba does the same for its trackers (its `Writing` marks
/// the pages once more when dropped; page destinations are reported at
/// each address showing them). Here it is counted always and acted on as
/// [`page_writes`] says. Called without the runtime's page-table lock;
/// takes the guards' lock, then, alone, the lock of the ranges kept for
/// the texture pass.
pub fn title_memory_written(at: usize, len: usize, pages: bool) {
    use std::sync::atomic::Ordering::Relaxed;
    if len == 0 {
        return;
    }
    let mode = page_writes();
    let end = at.saturating_add(len);
    if pages {
        WRITTEN[0].fetch_add(1, Relaxed);
        WRITTEN[1].fetch_add(len as u64, Relaxed);
    } else {
        WRITTEN[3].fetch_add(1, Relaxed);
        if mode & 4 == 0 {
            return;
        }
    }
    {
        let mut g = guards();
        let guarded = g.protected.range(at / BLOCK..end.div_ceil(BLOCK)).count() as u64;
        if pages {
            WRITTEN[2].fetch_add(guarded, Relaxed);
        }
        if mode & 1 != 0 && guarded != 0 {
            open(&mut g, at, end);
        }
    }
    // A placed texture there changed with no descriptor written.
    crate::descriptor::host::page_written(at, len);
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
    for &(at, n) in &ranges {
        if open_here(&mut g, at, at.saturating_add(n)) {
            #[cfg(feature = "diagnostics")]
            ALIASES[2].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// Held shared by a read of title memory in place, and alone by the runtime
/// while it changes what title memory shows.
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
static MAPPING: std::sync::RwLock<()> = std::sync::RwLock::new(());

/// What the runtime holds while it changes what title memory shows
/// ([`title_memory_remapping`]).
pub struct Remapping(#[allow(dead_code)] std::sync::RwLockWriteGuard<'static, ()>, std::time::Instant);

impl Drop for Remapping {
    fn drop(&mut self) {
        // From asking for the lock until it is let go (the frame log).
        let us = self.1.elapsed().as_micros() as u64;
        crate::framelog::add(crate::framelog::T::RemapUs, us);
        REMAPS[1].fetch_add(us, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Since the last frame-rate line, the runtime's changes of what title
/// memory shows ([`title_memory_remapping`]) and the reads in place that
/// hold them off ([`title_memory_reading`]): changes, microseconds from
/// asking for the lock to letting it go, of that the wait for reads in
/// place to end, the calls that changed several pages under one take of
/// the lock (`VICTORY_REMAP_BATCH=1`, [`remap_batched`]) and their pages;
/// reads in place, the microseconds they held the lock, the longest such
/// hold, and the reads that found the lock taken or asked for and did not
/// wait ([`title_memory_reading_now`] answering `None`).
///
/// The lock is the standard library's, which lets no new reader in while
/// a change waits for the readers under way: a change of one page waits
/// for the longest read in place (a texture hashed and detiled whole), and
/// every read asked for meanwhile waits behind it. In the runs of
/// 2026-11-25 the frame lines of 0.1-1.6 fps while the world streams in are
/// all "waiting for title memory to hold still" (4,565 of 4,689 ms of
/// texture refreshes in one frame); these counters say how many changes
/// there were and who held whom.
pub(crate) static REMAPS: [std::sync::atomic::AtomicU64; 9] = [const { std::sync::atomic::AtomicU64::new(0) }; 9];

/// A call of the runtime's that changed `pages` pages under one take of
/// the lock ([`REMAPS`]).
pub fn remap_batched(pages: usize) {
    use std::sync::atomic::Ordering::Relaxed;
    REMAPS[3].fetch_add(1, Relaxed);
    REMAPS[4].fetch_add(pages as u64, Relaxed);
}

/// With the frame line: [`REMAPS`] per frame over `frames`.
pub(crate) fn remaps_line(frames: f64) {
    use std::sync::atomic::Ordering::Relaxed;
    let [changes, held, waited, batches, pages, reads, read_us, longest, refused] =
        std::array::from_fn::<u64, 9, _>(|i| REMAPS[i].swap(0, Relaxed));
    if changes + reads == 0 {
        return;
    }
    let f = |n: u64| n as f64 / frames;
    tracing::info!(
        "title memory remaps per frame (the runtime changing what it shows): {:.1} changes, {:.1} ms holding the lock, \
         {:.1} ms of that waiting for reads in place to end; {:.1} calls changed {:.1} pages under one take \
         (VICTORY_REMAP_BATCH=1: {}); reads in place {:.0} per frame holding the lock {:.1} ms, the longest {:.1} ms; \
         {:.1} reads found it taken and did not wait",
        f(changes),
        f(held) / 1e3,
        f(waited) / 1e3,
        f(batches),
        f(pages),
        if remap_batch() { "on" } else { "off" },
        f(reads),
        f(read_us) / 1e3,
        longest as f64 / 1e3,
        f(refused),
    );
}

/// Whether the runtime's scatter mapping (`XMemMapPhysicalPagesScatter`:
/// one page at each of many addresses) changes all its pages under one
/// take of the lock instead of one take per page (`VICTORY_REMAP_BATCH=1`;
/// off by default). Each take waits for every read in place under way and
/// holds off every read asked for meanwhile; a scatter of hundreds of
/// pages made as many such waits, each one a stall of the texture
/// refreshes of the frame being prepared. Read by the runtime
/// (`victory-hle`'s `xmem`) once per process.
pub fn remap_batch() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var_os("VICTORY_REMAP_BATCH").is_some_and(|v| v == "1");
        if on {
            tracing::info!(
                "VICTORY_REMAP_BATCH=1: a scatter mapping of title memory changes all its pages under one take of the \
                 lock that reads in place hold"
            );
        }
        on
    })
}

/// The runtime is about to map, unmap or move physical pages of title
/// memory: until what it returns is dropped, no read of title memory in
/// place is under way and none begins ([`title_memory_reading`]). Taken
/// around the whole change, the unmapping and the mapping again.
pub fn title_memory_remapping() -> Remapping {
    let asked = std::time::Instant::now();
    let held = MAPPING.write().unwrap_or_else(|e| e.into_inner());
    let waited = asked.elapsed().as_micros() as u64;
    crate::framelog::add(crate::framelog::T::Remaps, 1);
    crate::framelog::add(crate::framelog::T::RemapWaitUs, waited);
    REMAPS[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    REMAPS[2].fetch_add(waited, std::sync::atomic::Ordering::Relaxed);
    Remapping(held, asked)
}

/// What a read of title memory in place holds ([`title_memory_reading`]).
pub struct Reading(#[allow(dead_code)] std::sync::RwLockReadGuard<'static, ()>, std::time::Instant);

impl Drop for Reading {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering::Relaxed;
        let us = self.1.elapsed().as_micros() as u64;
        REMAPS[5].fetch_add(1, Relaxed);
        REMAPS[6].fetch_add(us, Relaxed);
        REMAPS[7].fetch_max(us, Relaxed);
    }
}

/// For reading title memory in place: until what it returns is dropped,
/// the runtime changes no mapping, so memory found mapped stays mapped.
/// Not to be taken twice by one thread, nor held across anything that
/// maps title memory.
pub fn title_memory_reading() -> Reading {
    let guard = MAPPING.read().unwrap_or_else(|e| e.into_inner());
    Reading(guard, std::time::Instant::now())
}

/// [`title_memory_reading`] if it can be had at once: `None` while the
/// runtime changes a mapping (or waits to), for a caller that has another
/// way to read and would rather not wait.
pub fn title_memory_reading_now() -> Option<Reading> {
    match MAPPING.try_read() {
        Ok(guard) => Some(Reading(guard, std::time::Instant::now())),
        Err(std::sync::TryLockError::Poisoned(e)) => Some(Reading(e.into_inner(), std::time::Instant::now())),
        Err(std::sync::TryLockError::WouldBlock) => {
            REMAPS[8].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            None
        }
    }
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
    let range_hot = range.clone();
    unpark(&mut g, range.clone());
    let absent: Vec<usize> = g.absent.range(range.clone()).copied().collect();
    for block in absent {
        g.absent.remove(&block);
        known_bit(&g, block);
    }
    let blocks: Vec<usize> = g.protected.range(range).copied().collect();
    for block in blocks {
        g.protected.remove(&block);
        g.clean.remove(&block);
        clean_bit(block, false);
        known_bit(&g, block);
    }
    let hot: Vec<usize> = g.hot.range(range_hot.clone()).map(|(&b, _)| b).collect();
    for block in hot {
        g.hot.remove(&block);
        known_bit(&g, block);
    }
    let heat: Vec<usize> = g.heat.range(range_hot).map(|(&b, _)| b).collect();
    for block in heat {
        g.heat.remove(&block);
    }
    let aliased: Vec<usize> = g.aliased.range(at / BLOCK..at.saturating_add(len).div_ceil(BLOCK)).copied().collect();
    for block in aliased {
        g.aliased.remove(&block);
    }
    if view_known() {
        // The runtime calls this for two things only (`xmem`: `map` and
        // `unmap`): a range it has just mapped a section's pages into,
        // and a view it is about to make a placeholder again. Either way
        // what stands there is view memory or a hole in it, which is what
        // `is_view` would answer for a range that begins there; and the
        // write watch's asking can only fail there. The title remaps some
        // 83 times a frame, and each time the blocks were forgotten, so
        // that the next asking about each range went to the kernel twice
        // (run 235: 266 to 565 failed askings a frame with the blocks
        // remembered from their guards alone, no fewer than without).
        g.viewed.extend(at / BLOCK..at.saturating_add(len).div_ceil(BLOCK));
        return;
    }
    // What the range shows is new: no longer known to be a view.
    let viewed: Vec<usize> = g.viewed.range(at / BLOCK..at.saturating_add(len).div_ceil(BLOCK)).copied().collect();
    for block in viewed {
        g.viewed.remove(&block);
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
        // Held across both checks: another thread may have opened the block
        // since this one faulted, and a copy may have guarded it again since
        // (then it is ours again, not read-only for some other reason).
        let waited = std::time::Instant::now();
        let mut g = guards();
        GUARD_STATS[2].fetch_add(waited.elapsed().as_micros() as u64, std::sync::atomic::Ordering::Relaxed);
        if open(&mut g, at, at + 1) {
            GUARD_STATS[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let n = hot_after();
            if n > 0 {
                let block = at / BLOCK;
                let heat = g.heat.entry(block).or_insert(0);
                *heat += 1;
                if *heat >= n {
                    g.heat.remove(&block);
                    g.hot.insert(block, 0);
                    known_bit(&g, block);
                }
            }
            #[cfg(feature = "diagnostics")]
            crate::copystat::add(crate::copystat::C::GuardFaults, 1);
            crate::framelog::add(crate::framelog::T::GuardFaults, 1);
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
        if park_on() {
            // Parked blocks: found at the next look at all of them, every
            // `PARK_FRAMES` frames (frames do not advance in a test).
            let mut g = guards();
            unpark(&mut g, 0..usize::MAX);
        }
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

    /// A write through a view that no copy checks: the checked view is
    /// noted written and left read-only, its check reports the pages and
    /// guards the written view again, so a second write through it is seen
    /// as well (fxceless/victory-fork PR #853).
    #[test]
    fn a_view_no_copy_checks_is_guarded_again_by_the_checked_one() {
        let (section, checked, other) = two_views(BLOCK);
        assert_eq!(guard(checked, checked + BLOCK), Some(vec![checked / PAGE]));
        assert!(!writable(other), "guarded with the checked view");
        // SAFETY: the other view, read-write once the handler has opened it.
        unsafe { (other as *mut u8).write(1) };
        assert!(writable(other), "opened by the handler");
        assert!(!writable(checked), "noted written, left read-only");
        assert_eq!(guard(checked, checked + BLOCK), Some(vec![checked / PAGE]));
        assert!(!writable(other), "guarded again by the check");
        // SAFETY: as above.
        unsafe { (other as *mut u8).write(2) };
        assert_eq!(guard(checked, checked + BLOCK), Some(vec![checked / PAGE]));
        assert_eq!(guard(checked, checked + BLOCK), Some(Vec::new()));
        // SAFETY: the checked view, opened by the handler.
        unsafe { (checked as *mut u8).write(3) };
        assert_eq!(guard(checked, checked + BLOCK), Some(vec![checked / PAGE]));
        drop_views(section, &[checked, other], BLOCK);
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
    use super::{title_memory_reading, title_memory_remapping};
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
                let _changing = title_memory_remapping();
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
                let _reading = title_memory_reading();
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
