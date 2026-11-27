//! What shaders wrote goes into title memory on a few threads
//! (`VICTORY_LAND`, on by default; `=0` merges piece by piece on the thread
//! that lands flights, as before).
//!
//! The relay's thread (`crate::host::relay`) was 58-60 ms of a 104 ms frame
//! landing (run record-1): some 1,500 pieces of 64 KiB, 98 MB, fetched from
//! copies in the card's memory, and each of them searched from both ends,
//! cleared room for, read from title memory, copied, merged and compared
//! again while it was put, one after the other; a lock and a protection
//! call to open the guard of each, and the kernel's fault on the first
//! write to each 4 KiB page of memory just made writable again.
//!
//! Here a piece is merged in one pass ([`crate::safecopy::merge`]): what
//! the card holds is compared with what it held 32 bytes at a time, title
//! memory is read only where they differ, just before the dwords that
//! change are stored. The pieces of a landing are cut into jobs for a few
//! threads kept for it ([`Pool`]).
//!
//! What stays in order: pieces in the same 64 KiB of title memory are in
//! one job, in the order they were given (two copies may hold the same
//! title memory); a landing's copies in the card's memory come before its
//! other copies; a landing is done, whole, before the title is told
//! anything and before the next one starts (`Host::relay_land`).
//!
//! With `VICTORY_LAND_BESIDE` (on by default) title memory that is a view of
//! the title's physical pages is written through a view of our own of the
//! same pages ([`crate::inplace::beside`]). The guards (`crate::watch`) are
//! read-only title views: they stay as they are, no protection changes, and
//! [`crate::watch::landed_beside`] notes the blocks written afterwards, so
//! that the next refresh takes them as before.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};

/// The unit pieces are cut in (of title memory), and the guards' blocks.
pub(crate) const BLOCK: usize = 64 << 10;
/// The unit title memory is there or not in.
const PAGE: usize = 4096;
/// About this many bytes to a job.
const JOB: usize = 256 << 10;

/// See the file.
pub(crate) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("VICTORY_LAND").is_none_or(|v| v != "0") && crate::safecopy::on() && crate::shadow::exact_back()
    })
}

/// How many threads land, the relay's own counted (`VICTORY_LAND_THREADS`), as
/// the run began.
pub(crate) fn threads() -> usize {
    static THREADS: OnceLock<usize> = OnceLock::new();
    *THREADS.get_or_init(|| {
        let asked = std::env::var("VICTORY_LAND_THREADS").ok().and_then(|v| v.trim().parse().ok());
        asked.unwrap_or_else(|| std::thread::available_parallelism().map_or(2, |n| n.get() / 4).clamp(1, 4)).clamp(1, 16)
    })
}

/// How many of the pool's threads land now: [`threads`], or what the live
/// menus or `VICTORY_LIVE_ALT` ask (`crate::live`; the pool then has
/// [`LIVE_MOST`] threads, those over the number asleep).
fn width() -> usize {
    crate::live::number(crate::live::LAND_THREADS).map_or_else(threads, |v| (v as usize).clamp(1, LIVE_MOST))
}

/// The pool's threads when the number may change while the title runs.
const LIVE_MOST: usize = 8;

/// Whether views of the title's pages are written through one of ours.
fn beside_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_LAND_BESIDE").is_none_or(|v| v != "0"))
}

/// Whether pieces an earlier fetch holds as they are now are left out
/// ([`Had`]; `VICTORY_LAND_SKIP=1`, off by default: counted either way).
pub(crate) fn skip_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_LAND_SKIP").is_some_and(|v| v == "1"))
}

/// Since the last frame-rate line; microseconds first.
pub(crate) mod stat {
    /// Which pieces to fetch; their list recorded and sent.
    pub(crate) const GATHER: usize = 0;
    /// Looking for what shaders wrote to copies in the PC's memory, the part
    /// done while the fetch was on its way.
    pub(crate) const SCAN_BESIDE: usize = 1;
    /// The same, with nothing else to wait for.
    pub(crate) const SCAN: usize = 2;
    /// Merging what was fetched: until the last job is done, and the jobs'
    /// time on all threads.
    pub(crate) const CARD: usize = 3;
    pub(crate) const CARD_ALL: usize = 4;
    /// Merging what the scan found.
    pub(crate) const BACK: usize = 5;
    pub(crate) const BACK_ALL: usize = 6;
    /// In the jobs: waiting for the lock against the title's bytes going in.
    pub(crate) const LOCK: usize = 7;
    /// In the jobs: guards opened before a write, or noted after one.
    pub(crate) const NOTES: usize = 8;
    /// The flights' ranges done with; reading back, resets; what the title
    /// is told; the rest (the line's account).
    pub(crate) const DONE: usize = 9;
    pub(crate) const READ_BACKS: usize = 10;
    pub(crate) const TOLD: usize = 11;
    pub(crate) const REST: usize = 12;
    /// Counts: pieces merged, those with a new byte, their 4 KiB pages,
    /// those with a new byte, blocks of 32 bytes with a new byte, dwords put.
    pub(crate) const PIECES: usize = 13;
    pub(crate) const NEW_PIECES: usize = 14;
    pub(crate) const PAGES: usize = 15;
    pub(crate) const NEW_PAGES: usize = 16;
    pub(crate) const NEW_BLOCKS: usize = 17;
    pub(crate) const DWORDS: usize = 18;
    /// Pieces put through a view of ours, through the title's; pages where
    /// title memory was not there.
    pub(crate) const BESIDE: usize = 19;
    pub(crate) const DIRECT: usize = 20;
    pub(crate) const MISSING: usize = 21;
    /// Bytes asked for twice in a landing (fetched once); pieces and bytes
    /// an earlier fetch held already.
    pub(crate) const TWICE: usize = 22;
    pub(crate) const HAD: usize = 23;
    pub(crate) const HAD_BYTES: usize = 24;
    /// Jobs.
    pub(crate) const JOBS: usize = 25;
    pub(crate) const COUNT: usize = 26;
}

static STATS: [AtomicU64; stat::COUNT] = [const { AtomicU64::new(0) }; stat::COUNT];

pub(crate) fn count(which: usize, n: u64) {
    STATS[which].fetch_add(n, Ordering::Relaxed);
}

/// Adds the microseconds since `from` to `which`; now.
pub(crate) fn spent(which: usize, from: std::time::Instant) -> std::time::Instant {
    let now = std::time::Instant::now();
    count(which, (now - from).as_micros() as u64);
    now
}

/// For the frame-rate line: where landing went, per frame.
pub(crate) fn log(frames: f64) {
    let t: Vec<f64> = TAKEN.iter().map(|t| t.swap(0, Ordering::Relaxed) as f64 / frames).collect();
    if t[0] > 0.0 {
        tracing::info!(
            "blocks landed in beside the guards per frame: {:.0}, {:.0} of them held by the copy they were landed through, which takes \
             them no more (VICTORY_LAND_TAKEN={})",
            t[0],
            t[1],
            u8::from(taken()),
        );
    }
    let s: Vec<f64> = STATS.iter().map(|s| s.swap(0, Ordering::Relaxed) as f64 / frames).collect();
    if s[stat::PIECES] == 0.0 && s[stat::REST] == 0.0 {
        return;
    }
    let ms = |i: usize| s[i] / 1000.0;
    let mb = |v: f64| v / (1 << 20) as f64;
    tracing::info!(
        "landing per frame (VICTORY_LAND={}, {} threads): which pieces to fetch {:.2} ms, then the fetches (the relay's line); \
         looking through copies in the PC's memory {:.2} ms beside them and {:.2} ms more; merging what was fetched {:.2} ms \
         ({:.2} ms on all threads, {:.0} jobs), merging the others {:.2} ms ({:.2} on all threads); of the threads' time \
         {:.2} ms waiting for the title's bytes going in, {:.2} ms opening or noting guards; then ranges done with {:.2} ms, \
         read-backs and lists {:.2}, the title told {:.2}, the rest {:.2}",
        u8::from(on()),
        width(),
        ms(stat::GATHER),
        ms(stat::SCAN_BESIDE),
        ms(stat::SCAN),
        ms(stat::CARD),
        ms(stat::CARD_ALL),
        s[stat::JOBS],
        ms(stat::BACK),
        ms(stat::BACK_ALL),
        ms(stat::LOCK),
        ms(stat::NOTES),
        ms(stat::DONE),
        ms(stat::READ_BACKS),
        ms(stat::TOLD),
        ms(stat::REST),
    );
    tracing::info!(
        "landed per frame: {:.0} pieces, {:.0} with a new byte; of their {:.0} pages of 4 KiB {:.0} with a new byte ({:.1} MB), \
         {:.1} MB in blocks of 32 bytes with one, {:.2} MB put as dwords; {:.0} pieces through a view of ours \
         (VICTORY_LAND_BESIDE={}), {:.0} through the title's; {:.0} pages not there; asked for twice in a landing {:.1} MB; \
         held already by an earlier fetch {:.0} pieces {:.1} MB (VICTORY_LAND_SKIP={})",
        s[stat::PIECES],
        s[stat::NEW_PIECES],
        s[stat::PAGES],
        s[stat::NEW_PAGES],
        mb(s[stat::NEW_PAGES] * PAGE as f64),
        mb(s[stat::NEW_BLOCKS] * 32.0),
        mb(s[stat::DWORDS] * 4.0),
        s[stat::BESIDE],
        u8::from(beside_on()),
        s[stat::DIRECT],
        s[stat::MISSING],
        mb(s[stat::TWICE]),
        s[stat::HAD],
        mb(s[stat::HAD_BYTES]),
        u8::from(skip_on()),
    );
    if merge_mode().is_some() {
        let c: Vec<f64> = CLASH.iter().map(|c| c.swap(0, Ordering::Relaxed) as f64 / frames).collect();
        let [at, t, n, h] = [0, 1, 2, 3].map(|i| CLASH_LAST[i].swap(0, Ordering::Relaxed));
        tracing::info!(
            "relay guard per frame (VICTORY_RELAY_GUARD={}): {:.1} bytes in {:.2} pages of 4 KiB that the card and the title had \
             both changed since the reference, {:.1} of them in the top 64 bytes of a 128 KiB block, {:.1} kept as the title's; \
             last at {at:#x} (+{:#x} in its 128 KiB): title {t:#010x}, card {n:#010x}, reference {h:#010x}",
            guard(),
            c[0],
            c[1],
            c[3],
            c[2],
            at % (128 << 10),
        );
    }
}

/// Threads kept for landing, and the work they are at.
struct Pool {
    state: Mutex<State>,
    more: Condvar,
    idle: Condvar,
    /// One batch at a time.
    turn: Mutex<()>,
    threads: usize,
    /// Of them, how many take part in a batch (this one counted).
    width: std::sync::atomic::AtomicUsize,
}

struct State {
    /// What to do with each number below `count`.
    work: Option<Work>,
    next: usize,
    count: usize,
    /// Helpers inside a call.
    running: usize,
    /// Helpers that may be inside a call of this batch.
    helpers: usize,
    panicked: bool,
}

#[derive(Clone, Copy)]
struct Work(*const (dyn Fn(usize) + Sync));

// SAFETY: the closure may be called from any thread (`Sync`), and
// `Pool::each` keeps it alive until no helper is inside a call.
unsafe impl Send for Work {}

impl Pool {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn new(name: &'static str, threads: usize) -> &'static Pool {
        let pool: &'static Pool = Box::leak(Box::new(Pool {
            state: Mutex::new(State {
                work: None,
                next: 0,
                count: 0,
                running: 0,
                helpers: threads.saturating_sub(1),
                panicked: false,
            }),
            more: Condvar::new(),
            idle: Condvar::new(),
            turn: Mutex::new(()),
            threads,
            width: std::sync::atomic::AtomicUsize::new(threads),
        }));
        for i in 1..threads {
            // (A helper that is not there is only missed.)
            let _ = std::thread::Builder::new().name(format!("{name}-{i}")).spawn(move || {
                if name == "victory-land" {
                    crate::cores::keep_helper_on_fast_cores(name);
                } else {
                    crate::cores::keep_many_on_fast_cores(name);
                }
                pool.help()
            });
        }
        pool
    }

    fn help(&self) {
        let mut state = self.state();
        loop {
            // (Over the batch's width: asleep until the next one.)
            let taken = if state.running < state.helpers { take(&mut state) } else { None };
            let Some((work, i)) = taken else {
                state = self.more.wait(state).unwrap_or_else(|e| e.into_inner());
                continue;
            };
            state.running += 1;
            drop(state);
            let working = crate::timeline::span("a landing's job");
            // SAFETY: counted in `running`, so `each` has not returned.
            let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe { (*work.0)(i) }));
            working.end();
            state = self.state();
            state.running -= 1;
            state.panicked |= ran.is_err();
            if state.running == 0 {
                self.idle.notify_all();
            }
        }
    }

    /// Calls `f` with every number below `count`, on the pool's threads and
    /// this one, and returns when every call has.
    fn each(&self, count: usize, f: &(dyn Fn(usize) + Sync)) {
        let width = self.width.load(Ordering::Relaxed).clamp(1, self.threads.max(1));
        if count < 2 || width < 2 {
            (0..count).for_each(f);
            return;
        }
        let _spread = crate::timeline::span("landing: spread over threads");
        let _turn = self.turn.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: the lifetime is left out only until this call returns, and
        // it does not before no helper is inside `f` and none can enter.
        let work = Work(unsafe { std::mem::transmute::<*const (dyn Fn(usize) + Sync + '_), *const (dyn Fn(usize) + Sync + 'static)>(f) });
        let mut state = self.state();
        (state.work, state.next, state.count, state.helpers, state.panicked) = (Some(work), 0, count, width - 1, false);
        drop(state);
        if count > width {
            self.more.notify_all();
        } else {
            (1..count).for_each(|_| self.more.notify_one());
        }
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            loop {
                let Some((_, i)) = take(&mut self.state()) else { break };
                f(i);
            }
        }));
        let mut state = self.state();
        // (After a panic here: nothing more is begun.)
        state.work = None;
        while state.running > 0 {
            state = self.idle.wait(state).unwrap_or_else(|e| e.into_inner());
        }
        let panicked = state.panicked;
        drop(state);
        if let Err(panic) = ran {
            std::panic::resume_unwind(panic);
        }
        assert!(!panicked, "a job of landing panicked");
    }
}

/// The next number of the batch, if there is one.
fn take(state: &mut State) -> Option<(Work, usize)> {
    let work = state.work?;
    if state.next >= state.count {
        return None;
    }
    state.next += 1;
    Some((work, state.next - 1))
}

/// [`Pool::each`] on the threads kept for landing.
pub(crate) fn each(count: usize, f: &(dyn Fn(usize) + Sync)) {
    static POOL: OnceLock<&'static Pool> = OnceLock::new();
    let pool = POOL.get_or_init(|| {
        let most = if crate::live::may_change(crate::live::LAND_THREADS) { threads().max(LIVE_MOST) } else { threads() };
        let pool = Pool::new("victory-land", most);
        pool.width.store(threads(), Ordering::Relaxed);
        pool
    });
    pool.width.store(width(), Ordering::Relaxed);
    pool.each(count, f);
}

/// [`Pool::each`] on threads kept for the title's bytes under flights' writes
/// (`VICTORY_UNDER_THREADS`, 6 with this one).
pub(crate) fn each_under(count: usize, f: &(dyn Fn(usize) + Sync)) {
    static POOL: OnceLock<&'static Pool> = OnceLock::new();
    POOL.get_or_init(|| {
        let threads = std::env::var("VICTORY_UNDER_THREADS").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(6usize);
        Pool::new("victory-under", threads.clamp(1, 16))
    })
    .each(count, f);
}

/// Calls `f` with every number below `count`, on this thread and on threads
/// kept for making descriptors (`VICTORY_VIEW_THREADS`, 4 with this one, 6 for records; 1:
/// on this one alone). Those for `records`, the heaps' records copied as
/// bytes, are others than those for the views picked by index: submissions
/// wait for the first, lists for the second, and each kept the other waiting.
pub(crate) fn each_view(records: bool, count: usize, f: &(dyn Fn(usize) + Sync)) {
    static POOLS: [OnceLock<&'static Pool>; 2] = [const { OnceLock::new() }; 2];
    POOLS[usize::from(records)]
        .get_or_init(|| {
            let threads = std::env::var("VICTORY_VIEW_THREADS").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(if records { 6usize } else { 4 });
            Pool::new(if records { "victory-records" } else { "victory-views" }, threads.clamp(1, 16))
        })
        .each(count, f);
}

/// A piece to land: the copy's number, the title address, its bytes.
pub(crate) type Piece = (u64, usize, usize);

/// `pieces` in the order they are landed: by the 64 KiB block of title
/// memory they lie in, those in one block in the order given (each is
/// within one block). Where one copy alone has pieces in a block, those
/// that overlap or touch are one piece: a landing's flights declare parts
/// of the same memory, and what overlapped came down and was merged twice.
/// Each with the number of the piece given that it begins with.
pub(crate) fn ordered(pieces: &[Piece]) -> Vec<(usize, usize, usize)> {
    let mut order: Vec<usize> = (0..pieces.len()).collect();
    order.sort_by_key(|&i| pieces[i].1 / BLOCK);
    let mut out: Vec<(usize, usize, usize)> = Vec::with_capacity(order.len());
    for group in order.chunk_by(|&a, &b| pieces[a].1 / BLOCK == pieces[b].1 / BLOCK) {
        let alone = group.len() > 1 && group.iter().all(|&i| pieces[i].0 == pieces[group[0]].0);
        if !alone {
            out.extend(group.iter().map(|&i| (i, pieces[i].1, pieces[i].2)));
            continue;
        }
        let mut sorted = group.to_vec();
        sorted.sort_by_key(|&i| pieces[i].1);
        let begun = out.len();
        for i in sorted {
            let (_, at, len) = pieces[i];
            match out[begun..].last_mut() {
                Some(last) if at <= last.1 + last.2 => {
                    let end = (last.1 + last.2).max(at + len);
                    count(stat::TWICE, (last.2 + len - (end - last.1)) as u64);
                    last.2 = end - last.1;
                }
                _ => out.push((i, at, len)),
            }
        }
    }
    out
}

/// Pieces (title address, bytes; in [`ordered`]'s order) as jobs: runs of
/// about [`JOB`] bytes that end where a block ends.
pub(crate) fn jobs(pieces: &[(usize, usize)]) -> Vec<std::ops::Range<usize>> {
    let mut out: Vec<std::ops::Range<usize>> = Vec::new();
    let (mut from, mut bytes) = (0, 0);
    for (i, &(at, len)) in pieces.iter().enumerate() {
        let new_block = i == 0 || pieces[i - 1].0 / BLOCK != at / BLOCK;
        if bytes >= JOB && new_block {
            out.push(from..i);
            (from, bytes) = (i, 0);
        }
        bytes += len;
    }
    if from < pieces.len() {
        out.push(from..pieces.len());
    }
    out
}

/// What earlier fetches brought, for as long as a flight they ran behind
/// has not landed: per fetch, the value the host's fence had been given
/// behind the last flight sent before it, and by copy (its number) and
/// piece the offsets it brought.
///
/// A fetch on the drawing queue runs behind everything sent before it, so
/// it brings what those flights' shaders wrote too, and the merge of it put
/// that into title memory. Their own landings flag the same pieces and
/// fetched them again, to find every byte merged. A piece that a fetch held
/// whole, behind the flight that flags it, has nothing more of that flight.
/// (Bytes left out of that merge, the title's that went up behind the
/// fetch, went up behind that flight too, and stand over what it wrote.)
#[derive(Default)]
pub(crate) struct Had(Vec<(u64, std::collections::HashMap<(u64, usize), (usize, usize)>)>);

impl Had {
    /// Flights from the one behind which `fence` is signalled on are landed
    /// next: what is of no use to them is dropped.
    pub(crate) fn landing(&mut self, fence: u64) {
        self.0.retain(|had| had.0 >= fence);
    }

    /// A fetch that ran behind every flight up to `fence` brought (copy,
    /// title address, bytes) `pieces`. They are merged before the next ask.
    pub(crate) fn fetched(&mut self, fence: u64, pieces: impl Iterator<Item = Piece>) {
        let mut all = std::collections::HashMap::new();
        for (copy, at, len) in pieces {
            // (Two parts of a block that do not touch: the longer one.)
            let had = all.entry((copy, at / BLOCK)).or_insert((at, at + len));
            if len > had.1 - had.0 {
                *had = (at, at + len);
            }
        }
        self.0.push((fence, all));
    }

    /// Whether a fetch behind the flight of `fence` brought all of the
    /// piece.
    pub(crate) fn holds(&self, fence: u64, (copy, at, len): Piece) -> bool {
        self.0.iter().any(|(behind, all)| {
            *behind >= fence && all.get(&(copy, at / BLOCK)).is_some_and(|had| had.0 <= at && at + len <= had.1)
        })
    }
}

/// Whether a piece is put through a view of ours while the runtime changes
/// a mapping too (`VICTORY_LAND_ANYTIME=0`: through the title's address, as
/// before).
///
/// While the world streams a change is under way nearly all the time (pages
/// are mapped in groups, eight threads side by side: 28 of 1,620 pieces a
/// frame went through a view of ours in run textures-4, 630 in run n03). Put
/// through the title's address a piece opens the guards of its blocks, 5,000
/// changes of protection a frame, which wait for the address-space lock the
/// mapping wants as well, and every refresh guards them again: the frame went
/// from 105 to 140 ms. The record names a run of a section only while it is
/// mapped there (it is taken out before a view goes and put in when one is
/// there), so the change is one of other memory, or one the title makes of
/// memory its shaders are writing at that moment, which it may not (its
/// fences are signalled after the landing).
fn anytime() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_LAND_ANYTIME").is_none_or(|v| v != "0"))
}

/// What a job wrote beside the guards, to be noted ([`Notes::flush`]).
#[derive(Default)]
pub(crate) struct Notes {
    /// Pages of 4 KiB, by number.
    pages: Vec<usize>,
    /// Per piece, from the first such page to the last: address, bytes.
    spans: Vec<(usize, usize)>,
    /// Per piece put beside the guards, all of it: its 64 KiB block and the
    /// copy it was landed through (which the job holds until the flush).
    landed: Vec<(usize, *const crate::shadow::Shadow)>,
}

// SAFETY: the pointers are used by the job that has the notes, in `flush`.
unsafe impl Send for Notes {}

/// Whether a copy a landing went through is noted as up to date with it
/// (`VICTORY_LAND_TAKEN=0`: it is not, and the next refresh copies the 64
/// KiB again and sends them up to the card, as before).
///
/// What a landing puts into title memory the copy holds already: the card
/// wrote it there, and a copy in the card's memory was given it on the CPU's
/// side by the same merge. But the block counted as written, for that copy
/// as for every other one, so the next refresh copied the whole block, sent
/// it up and had the card hash it again: some 900 blocks a frame landed
/// beside the guards, of the 103 MB a frame that went up (run record-1).
///
/// Why it may be left out ([`crate::shadow::Shadow::landed`]): the copy held
/// title memory's bytes of the block before the landing, if it had taken it
/// after the block's last noted write and no write was to be noted (the
/// block's bit, `crate::watch::block_published`), and the landing put the
/// same bytes into both. Only where one copy alone landed in the block.
fn taken() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_LAND_TAKEN").is_none_or(|v| v != "0"))
}

/// Since the last frame-rate line: blocks landed in beside the guards, and
/// those of them the copy was noted as up to date with.
static TAKEN: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];

impl Notes {
    /// At the end of a job: what it wrote beside the guards is noted.
    pub(crate) fn flush(&mut self) {
        if self.pages.is_empty() {
            return;
        }
        let noting = std::time::Instant::now();
        let (now, blocks) = crate::watch::landed_beside(&self.pages, &self.spans);
        TAKEN[0].fetch_add(blocks.len() as u64, Ordering::Relaxed);
        if taken() {
            for (block, before) in blocks {
                let mut through = self.landed.iter().filter(|l| l.0 == block).map(|l| l.1);
                let Some(copy) = through.next() else {
                    continue;
                };
                if !copy.is_null() && through.all(|other| other == copy) {
                    // SAFETY: the job holds its copies until this is done.
                    let copy = unsafe { &*copy };
                    TAKEN[1].fetch_add(u64::from(copy.landed(block, before, now)), Ordering::Relaxed);
                }
            }
        }
        self.pages.clear();
        self.spans.clear();
        self.landed.clear();
        spent(stat::NOTES, noting);
    }
}

/// The relay's guard (`VICTORY_RELAY_GUARD`, off by default; a mask, in-run
/// A/B as a whole):
///
/// - `1`: a three-way merge. A byte the card changed (it differs from the
///   reference: the snapshot, or the CPU's side of a copy in the card's
///   memory) goes into title memory only if the title has not changed that
///   byte since the reference took it; where both changed it (to different
///   values) the title's byte stays, and the reference takes the card's, so
///   that the next refresh finds the title's byte changed and sends it up.
/// - `2`: what tells the title (fence signals, bottom-of-pipe writes) is
///   told only together with the landing of every flight in the line, as
///   without the relay (`Host::settle` lands the whole line at once); the
///   landings themselves stay on the relay's thread.
///
/// Why. With three frames in flight (`VICTORY_FRAMES_AHEAD=3`) the title is
/// told frame N-1's fence as soon as N-1's flights have landed, while frame
/// N's are still in the air; it recycles N-1's command blocks (128 KiB of
/// the 174 MB heap, which every flight declares written as a whole: root
/// views have no size) and writes new nodes into them at once. Frame N's
/// landings then merge that heap: a byte whose card value differs from the
/// reference for any reason other than a shader of those flights (a later
/// flight's write caught early, a fetch beside sends on the copy queue, a
/// write nobody declared surfacing, two copies of the same memory) is put
/// over the title's newer node. Without the relay nothing is told while a
/// flight is in the air, and the title's writes go into the references at
/// the next refresh before any landing can see them.
pub(crate) fn guard() -> u8 {
    if let Some(on) = crate::ab::pick("VICTORY_RELAY_GUARD") {
        return if on { 3 } else { 0 };
    }
    static MASK: OnceLock<u8> = OnceLock::new();
    *MASK.get_or_init(|| std::env::var("VICTORY_RELAY_GUARD").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(0) & 3)
}

/// Whether the merges count the bytes [`guard`]'s first bit is about, with it
/// off as well (`VICTORY_RELAY_GUARD_COUNT=0`: not, the merge of one pass,
/// [`crate::safecopy::merge`], as before). Counting reads each 4 KiB page of
/// title memory the card changed bytes in, once more.
fn guard_count() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_RELAY_GUARD_COUNT").is_none_or(|v| v != "0"))
}

/// How the relay's merges go: `None`, the merge of one pass; `Some(keep)`,
/// three-way, counted, keeping the title's bytes where both changed if
/// `keep`.
pub(crate) fn merge_mode() -> Option<bool> {
    let keep = guard() & 1 != 0;
    (keep || guard_count()).then_some(keep)
}

/// Since the last frame-rate line: bytes the card and the title had both
/// changed since the reference, the 4 KiB pages they were in, those of the
/// bytes kept as the title's, and those in the top 64 bytes of a 128 KiB
/// block (where the title's command blocks keep their first node).
static CLASH: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
/// The last such byte: its address, and the dword around it in title
/// memory, on the card, in the reference.
static CLASH_LAST: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

/// What [`three_way`] found: bytes the card and the title both changed since
/// the reference, to different values; of them those in the top 64 bytes of
/// a 128 KiB block; and the first (offset; the dword around it in title
/// memory, on the card, in the reference).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Clash {
    pub(crate) bytes: u64,
    pub(crate) top: u64,
    pub(crate) first: Option<(usize, [u32; 3])>,
}

impl Clash {
    /// Counted for the frame line: `at` is the title address of offset 0.
    pub(crate) fn note(self, at: usize, kept: bool) {
        if self.bytes == 0 {
            return;
        }
        CLASH[0].fetch_add(self.bytes, Ordering::Relaxed);
        CLASH[1].fetch_add(1, Ordering::Relaxed);
        if kept {
            CLASH[2].fetch_add(self.bytes, Ordering::Relaxed);
        }
        CLASH[3].fetch_add(self.top, Ordering::Relaxed);
        if let Some((i, [t, n, h])) = self.first {
            for (slot, v) in CLASH_LAST.iter().zip([(at + i) as u64, t.into(), n.into(), h.into()]) {
                slot.store(v, Ordering::Relaxed);
            }
        }
    }
}

/// The three-way merge of a stretch whose title memory begins at `at`: `n`
/// what the card holds, `h` the reference, `t` title memory as read; `m`
/// holds `t` and takes, at each byte the card changed (`n != h`), the card's
/// byte, unless the title changed it too (`t != h`, `t != n`) and `keep`.
/// The reference is the caller's to bring up (it takes `n` either way).
pub(crate) fn three_way(n: &[u8], h: &[u8], t: &[u8], m: &mut [u8], keep: bool, at: usize) -> Clash {
    const CHUNK: usize = 32;
    const BLOCK: usize = 128 << 10;
    let len = n.len().min(h.len()).min(t.len()).min(m.len());
    let mut clash = Clash::default();
    let mut from = 0;
    while from < len {
        let to = (from + CHUNK).min(len);
        if n[from..to] != h[from..to] {
            for i in from..to {
                if n[i] == h[i] {
                    continue;
                }
                if t[i] != h[i] && t[i] != n[i] {
                    clash.bytes += 1;
                    clash.top += u64::from((at + i) % BLOCK >= BLOCK - 64);
                    if clash.first.is_none() {
                        let w = (i - (at + i) % 4).min(len.saturating_sub(4));
                        let word = |s: &[u8]| s.get(w..w + 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
                        clash.first = Some((i, [word(t), word(n), word(h)]));
                    }
                    if keep {
                        continue;
                    }
                }
                m[i] = n[i];
            }
        }
        from = to;
    }
    clash
}

/// [`crate::safecopy::merge`] of a stretch within one 4 KiB page of title
/// memory, three-way ([`three_way`]): the card's bytes (`now`, read once)
/// and title memory are read whole, the merge is made aside, and only the
/// dwords that change are stored ([`crate::safecopy::put_changed`]). Where
/// title memory cannot be read or written, the reference takes the card's
/// bytes if `take`, else it stays (some of the stretch may have been put).
///
/// # Safety
/// [`crate::safecopy::merge`]'s; `len` is at most 4 KiB.
pub(crate) unsafe fn merge_three(title: *mut u8, now: *const u8, held: *mut u8, len: usize, take: bool, keep: bool) -> (crate::safecopy::Merged, Clash) {
    let mut out = crate::safecopy::Merged { there: true, ..Default::default() };
    if len == 0 {
        return (out, Clash::default());
    }
    assert!(len <= PAGE, "a stretch of one page");
    let (mut n, mut t, mut m) = ([0u8; PAGE], [0u8; PAGE], [0u8; PAGE]);
    // SAFETY: the caller's: `now` is `len` bytes, read once (the card may be
    // writing them); `held` is ours alone.
    let h = unsafe {
        std::ptr::copy_nonoverlapping(now, n.as_mut_ptr(), len);
        std::slice::from_raw_parts_mut(held, len)
    };
    let n = &n[..len];
    if n == &h[..] {
        return (out, Clash::default());
    }
    // Blocks of 32 bytes of title memory the card changed bytes in.
    let mut last = usize::MAX;
    for (i, (a, b)) in n.iter().zip(h.iter()).enumerate() {
        if a != b && (title.addr() + i) / 32 != last {
            last = (title.addr() + i) / 32;
            out.new += 1;
        }
    }
    // SAFETY: title memory; a page that is not there fails the copy.
    if !unsafe { crate::safecopy::copy(t.as_mut_ptr(), title, len) } {
        out.there = false;
        if take {
            h.copy_from_slice(n);
        }
        return (out, Clash::default());
    }
    let (t, m) = (&t[..len], &mut m[..len]);
    m.copy_from_slice(t);
    let clash = three_way(n, h, t, m, keep, title.addr());
    let mut at = 0;
    while at < len {
        let end = (at + 4 - (title.addr() + at) % 4).min(len);
        out.dwords += u64::from(m[at..end] != t[at..end]);
        at = end;
    }
    // SAFETY: title memory, `len` bytes; faults fail the put.
    out.there = out.dwords == 0 || unsafe { crate::safecopy::put_changed(title, t, m) };
    if out.there || take {
        h.copy_from_slice(n);
    }
    (out, clash)
}

/// Lands one piece: `parts` (offsets from and to) of the bytes at `now`,
/// which the card wrote, go into title memory from `title` on and into
/// `held` ([`crate::safecopy::merge`] says how, and what `take` is). The
/// piece lies in one 64 KiB block of title memory. Returns the bytes put.
///
/// # Safety
/// `now` and `held` are ours from 0 to the last part's end, and nothing but
/// the card writes `now` there, nothing but this call `held`.
pub(crate) unsafe fn piece(
    title: usize,
    now: *const u8,
    held: *mut u8,
    parts: &[(usize, usize)],
    take: bool,
    notes: &mut Notes,
    copy: *const crate::shadow::Shadow,
) -> u64 {
    // SAFETY: the caller's.
    unsafe { piece_at(title, now, held, parts, take, notes, beside_on(), copy) }
}

/// [`piece`], and whether a view of ours is looked for.
///
/// # Safety
/// [`piece`]'s.
unsafe fn piece_at(
    title: usize,
    now: *const u8,
    held: *mut u8,
    parts: &[(usize, usize)],
    take: bool,
    notes: &mut Notes,
    beside: bool,
    copy: *const crate::shadow::Shadow,
) -> u64 {
    let Some(end) = parts.last().map(|p| p.1) else {
        return 0;
    };
    // From here until the piece is put the runtime changes no mapping: our
    // view's address stays where the title sees `title`. Not waited for: if
    // a change is under way, what the record of the views says now does
    // ([`anytime`]), or the title's own address.
    let reading = beside.then(crate::watch::title_memory_reading_now).flatten();
    let beside = if reading.is_some() || beside && anytime() { crate::inplace::beside(title, end) } else { None };
    count(stat::PIECES, 1);
    count(if beside.is_some() { stat::BESIDE } else { stat::DIRECT }, 1);
    // The pages that differ, before any is written: none, and the guard
    // stays; else it is opened once for the piece.
    let mut differing = [true; BLOCK / PAGE + 1];
    let base = title / PAGE;
    if beside.is_none() {
        drop(reading);
        differing.fill(false);
        for_pages(title, parts, |from, n| {
            // SAFETY: the caller's.
            if unsafe { crate::safecopy::differ(now.add(from), held.add(from), n) } {
                differing[(title + from) / PAGE - base] = true;
            }
        });
        let Some(first) = differing.iter().position(|&d| d) else {
            for_pages(title, parts, |_, _| count(stat::PAGES, 1));
            return 0;
        };
        let last = differing.iter().rposition(|&d| d).unwrap_or(first);
        let (from, to) = (((base + first) * PAGE).max(title), ((base + last + 1) * PAGE).min(title + end));
        let opening = std::time::Instant::now();
        crate::watch::landing(from, to - from);
        spent(stat::NOTES, opening);
    }
    let into = beside.unwrap_or(title);
    let mode = merge_mode();
    let since = crate::watch::changes();
    let (mut pages, mut new_pages, mut blocks, mut dwords, mut missing) = (0, 0u64, 0, 0, 0);
    let mut span: Option<(usize, usize)> = None;
    for_pages(title, parts, |from, n| {
        pages += 1;
        if !differing[(title + from) / PAGE - base] {
            return;
        }
        // SAFETY: the caller's; title memory, by an address that shows it
        // (`beside`) or the title's own.
        let merge = |take| unsafe {
            let (into, now, held) = (into.wrapping_add(from) as *mut u8, now.add(from), held.add(from));
            match mode {
                None => crate::safecopy::merge(into, now, held, n, take),
                Some(keep) => {
                    let (done, clash) = merge_three(into, now, held, n, take, keep);
                    clash.note(title + from, keep);
                    done
                }
            }
        };
        let mut done = merge(take && beside.is_some());
        if !done.there && beside.is_none() {
            // Away only while the runtime changes a mapping beside it: the
            // rest of it when that is done. What is still not there then is
            // taken as the caller says.
            let mut put = done.dwords;
            let again = crate::watch::again_after_change(since, || {
                done = merge(false);
                put += done.dwords;
                done.there
            });
            if !again && take {
                done = merge(true);
                put += done.dwords;
            }
            done.dwords = put;
            // (What was new to the first try was new.)
            done.new = done.new.max(1);
        }
        blocks += done.new;
        dwords += done.dwords;
        missing += u64::from(!done.there);
        if done.new > 0 {
            new_pages += 1;
            if beside.is_some() {
                let page = (title + from) / PAGE;
                // (Two parts in one page.)
                if notes.pages.last() != Some(&page) {
                    notes.pages.push(page);
                }
                let s = span.get_or_insert((title + from, title + from + n));
                s.1 = title + from + n;
            }
        }
    });
    if let Some((from, to)) = span {
        notes.spans.push((from, to - from));
        // Not through a copy that is not to be noted, if that is asked: the
        // block then has two.
        let through = if missing == 0 { copy } else { std::ptr::null() };
        notes.landed.push((title / BLOCK, through));
    }
    count(stat::NEW_PIECES, u64::from(new_pages > 0));
    count(stat::PAGES, pages);
    count(stat::NEW_PAGES, new_pages);
    count(stat::NEW_BLOCKS, blocks);
    count(stat::DWORDS, dwords);
    count(stat::MISSING, missing);
    crate::shadow::PUT[0].fetch_add(u64::from(dwords > 0), Ordering::Relaxed);
    crate::shadow::PUT[2].fetch_add(dwords * 4, Ordering::Relaxed);
    dwords * 4
}

/// `parts` of a piece that begins at `title`, cut where 4 KiB pages of title
/// memory end: offset and bytes.
fn for_pages(title: usize, parts: &[(usize, usize)], mut f: impl FnMut(usize, usize)) {
    for &(from, to) in parts {
        let mut at = from;
        while at < to {
            let n = (PAGE - (title + at) % PAGE).min(to - at);
            f(at, n);
            at += n;
        }
    }
}

#[cfg(test)]
mod tests;
