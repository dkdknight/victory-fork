//! Copies that shaders only read, moved to GPU upload heaps
//! (`VICTORY_READONLY_COPIES_ON_CARD=1`; off by default).
//!
//! Title buffers under `VICTORY_CARD_BUFFERS_MIN_KB`, big ones turned down
//! for room, and the windows outside buffers that came too late for
//! `VICTORY_UPLOAD_WINDOWS_MB` (the card was past its line when they were
//! made) are host copies in the PC's memory: shaders read them over the
//! bus, at about a microsecond a load. The frame line "buffer copies asked
//! for per frame by where the card reads them" counted 28-42 k asks a frame
//! to such copies in the owner's runs of 2026-11-25; most of the hottest
//! were 4 MiB windows outside buffers.
//!
//! With the switch, once a second ([`tick`], from the host's sweep):
//! - the copies in the PC's memory that no shader was ever declared to
//!   write, asked for at least `VICTORY_READONLY_COPIES_MIN_ASKS` times a
//!   frame, hottest first, get a twin in a GPU upload heap (the card's
//!   memory, which the CPU writes through Resizable BAR and must not read).
//!   The twin takes the copy's place in the tables, so every ask from then
//!   on finds it; it is filled from title memory by the refreshes of those
//!   asks, as any new copy is;
//! - when the card has no room under its line, cold copies in GPU upload
//!   heaps (asked for at most a quarter as often as the hot one, shown by
//!   no descriptor slot) are put back in the PC's memory to make it: the
//!   card's use stays where it was, so the textures keep their room;
//! - a twin that shaders are then declared to write is moved back to the
//!   PC's memory at once ([`written`]), and that title memory never moves
//!   again (the CPU looks for what shaders wrote by reading the copy, and
//!   reading a GPU upload heap is slow).
//!
//! A copy that was replaced in the tables stays alive while anything holds
//! it: the lists that bound it bring it up to date themselves. A descriptor
//! slot made on it does not, as the pass over the heap brings up to date the
//! copy the tables now hold. So the new copy keeps the older one
//! (`Shadow::forwards`) and everything asked of the new one (refreshes,
//! declared shader writes) is asked of the older one too, until no slot
//! shows the older one's buffer any more ([`cleanup`]).
//!
//! The copies moved are counted against `VICTORY_READONLY_COPIES_MB` (by
//! default a 32nd of the card's budget at the first look, 64 to 512 MB) and
//! are made only while the card's use stays under
//! `VICTORY_READONLY_COPIES_PERCENT` of its budget (by default the windows'
//! line, `VICTORY_CARD_WINDOWS_PERCENT`, 80), unless room was made for them
//! as above.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use super::{BUFFERS, LOOSE, Shadow, buffers, host};

/// Whether copies are moved: the live file's `readonly_copies_on_card`
/// first, then `VICTORY_READONLY_COPIES_ON_CARD`. Read at every look, so
/// that the live file can stop it (the copies moved stay where they are).
fn on() -> bool {
    if let Some(v) = crate::host::live::value("readonly_copies_on_card") {
        return v.trim() == "1";
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_READONLY_COPIES_ON_CARD").is_some_and(|v| v == "1"))
}

/// The settings, read at the first look with the switch on.
struct Settings {
    /// The most bytes of title memory in copies moved.
    most: u64,
    /// The share of the card's budget, in percent, under which a copy is
    /// moved without making room.
    percent: u64,
    /// Copies moved and put back, together, at most per look.
    per_look: usize,
    /// The asks per frame a copy needs to be moved.
    min_asks: f64,
    /// Whether cold copies in GPU upload heaps are put back to make room.
    trade: bool,
    /// The most milliseconds a look spends making buffers.
    look_ms: u64,
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|v| v.trim().parse::<u64>().ok())
}

/// [`Settings::most`] once read (0 before), for the frame line.
static MOST: AtomicU64 = AtomicU64::new(0);

fn settings(budget: Option<u64>) -> &'static Settings {
    static SETTINGS: OnceLock<Settings> = OnceLock::new();
    SETTINGS.get_or_init(|| {
        let most = match env_u64("VICTORY_READONLY_COPIES_MB") {
            Some(mb) => mb << 20,
            None => default_most(budget),
        };
        let s = Settings {
            most,
            percent: env_u64("VICTORY_READONLY_COPIES_PERCENT").map_or_else(super::window_card_percent, |p| p.clamp(50, 97)),
            per_look: env_u64("VICTORY_READONLY_COPIES_PER_LOOK").map_or(16, |n| n.clamp(1, 256)) as usize,
            min_asks: std::env::var("VICTORY_READONLY_COPIES_MIN_ASKS")
                .ok()
                .and_then(|v| v.trim().parse::<f64>().ok())
                .filter(|v| v.is_finite() && *v >= 0.0)
                .unwrap_or(1.0),
            trade: std::env::var_os("VICTORY_READONLY_COPIES_TRADE").is_none_or(|v| v != "0"),
            look_ms: env_u64("VICTORY_READONLY_COPIES_LOOK_MS").map_or(4, |ms| ms.clamp(1, 100)),
        };
        tracing::info!(
            "VICTORY_READONLY_COPIES_ON_CARD=1: copies shaders only read move from the PC's memory to GPU upload heaps, hottest \
             first: at most {} MB (VICTORY_READONLY_COPIES_MB; the card's budget at the first look {} MB), under {}% of the \
             card's budget (VICTORY_READONLY_COPIES_PERCENT), {} moves a look (VICTORY_READONLY_COPIES_PER_LOOK), from {:.1} \
             asks a frame (VICTORY_READONLY_COPIES_MIN_ASKS); cold ones put back to make room: {} (VICTORY_READONLY_COPIES_TRADE)",
            s.most >> 20,
            budget.map_or(0, |b| b >> 20),
            s.percent,
            s.per_look,
            s.min_asks,
            if s.trade { "on" } else { "off" }
        );
        MOST.store(s.most, Ordering::Relaxed);
        s
    })
}

/// A 32nd of the card's budget, 64 to 512 MB; 256 MB when unknown.
fn default_most(budget: Option<u64>) -> u64 {
    budget.map_or(256 << 20, |b| (b / 32).clamp(64 << 20, 512 << 20))
}

/// Copies moved alive, and their bytes of title memory.
static ALIVE: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];

/// A copy moved went (`Shadow`'s drop).
pub(super) fn gone(len: u64) {
    ALIVE[0].fetch_sub(1, Ordering::Relaxed);
    ALIVE[1].fetch_sub(len, Ordering::Relaxed);
}

/// Since the last frame line ([`S`] names them).
static STATS: [AtomicU64; S::N as usize] = [const { AtomicU64::new(0) }; S::N as usize];

/// Since the start: copies moved, put back cold, moved back for shader
/// writes.
static TOTALS: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

#[derive(Clone, Copy)]
#[repr(usize)]
enum S {
    /// Copies moved to GPU upload heaps, and their bytes.
    Moved,
    MovedBytes,
    /// Cold copies in GPU upload heaps put back in the PC's memory to make
    /// room, and their bytes.
    PutBack,
    PutBackBytes,
    /// Copies moved that shaders were then declared to write: moved back.
    MovedBack,
    /// Copies moved that the raw-address directory numbered: moved back.
    MovedBackDirectory,
    /// Asks that found a moved copy standing for an older copy a slot may
    /// still show (in the PC's memory, read over the bus), and asks that
    /// found another copy standing for older ones.
    AsksForwardingMoved,
    AsksForwardingOther,
    /// Snapshots of copies moved (the CPU read a GPU upload heap).
    SnapshotReads,
    /// Hot copies turned down: no room on the card, over the bytes allowed.
    NoRoom,
    NoBudget,
    /// Bytes the refreshes wrote into copies moved (over the bus, into the
    /// card's memory).
    RefreshedBytes,
    /// Refreshes and declared writes passed on to older copies a slot may
    /// show.
    Forwarded,
    /// Copies the tables no longer held when their twin was ready.
    Raced,
    /// Older copies let go once nothing showed them.
    Released,
    /// Microseconds the looks took.
    LookUs,
    /// Copies that could be moved (hot enough, never written), and those
    /// hot enough that shaders write.
    Candidates,
    HotWritten,
    N,
}

fn add(stat: S, n: u64) {
    STATS[stat as usize].fetch_add(n, Ordering::Relaxed);
}

/// Bytes a refresh wrote into a copy moved.
pub(super) fn refreshed(bytes: u64) {
    add(S::RefreshedBytes, bytes);
}

/// The older copies `shadow` stands for: what it is asked, they are too.
pub(super) fn forwards_of(shadow: &Shadow) -> Vec<Arc<Shadow>> {
    let older = shadow.forwards.lock().unwrap_or_else(|e| e.into_inner()).clone();
    add(S::Forwarded, older.len() as u64);
    older
}

/// Title memory (start, bytes) whose copy was moved back for shader
/// writes: never moved again.
static NEVER: Mutex<Option<std::collections::HashSet<(u64, u64)>>> = Mutex::new(None);

fn never(start: u64, len: u64) -> bool {
    NEVER.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|set| set.contains(&(start, len)))
}

/// The copies that stand for older ones (`Shadow::forwards`).
static FORWARDING: Mutex<Vec<Weak<Shadow>>> = Mutex::new(Vec::new());

/// Shaders were declared to write `shadow`, a copy moved, for the first
/// time: it goes back to the PC's memory (a copy there takes its place in
/// the tables, standing for it while a slot shows it), and its title memory
/// is never moved again. Called while a list is recorded.
pub(super) fn written(shadow: &Arc<Shadow>) {
    move_back(shadow, false);
}

/// The raw-address directory (`crate::memory`) is about to number `shadow`
/// for the rest of the run. Every list whose shaders can store declares all
/// numbered copies written, whole, and the directory keeps the copy it
/// numbered whatever the tables hold later. So such a copy is never moved
/// (`Shadow::written`: a twin made later would not see what shaders write
/// through the directory until it came back through title memory), and a
/// copy moved already goes back to the PC's memory first and that one is
/// numbered (else the CPU would read the GPU upload heap after every such
/// submission, for the rest of the run, to find what shaders wrote).
/// Returns the copy to number.
pub(crate) fn for_directory(shadow: Arc<Shadow>) -> Arc<Shadow> {
    shadow.written.store(true, Ordering::Relaxed);
    if !shadow.promoted {
        return shadow;
    }
    match move_back(&shadow, true) {
        Some(back) => back,
        None => {
            static SAID: AtomicBool = AtomicBool::new(false);
            if !SAID.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    copy_start = format_args!("{:#x}", shadow.start),
                    copy_bytes = shadow.len,
                    "VICTORY_READONLY_COPIES_ON_CARD: the raw-address directory numbers a copy in a GPU upload heap that \
                     could not be moved back to the PC's memory: shader writes through the directory have the CPU read \
                     the GPU upload heap (slow there)"
                );
            }
            shadow
        }
    }
}

/// [`written`] and [`for_directory`]: the copy in the PC's memory that took
/// `shadow`'s place, if one could be made.
fn move_back(shadow: &Arc<Shadow>, directory: bool) -> Option<Arc<Shadow>> {
    NEVER
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(Default::default)
        .insert((shadow.start, shadow.len));
    let host = host()?;
    let back = replace(shadow, host, Kind::BackForWrites)?;
    add(if directory { S::MovedBackDirectory } else { S::MovedBack }, 1);
    TOTALS[2].fetch_add(1, Ordering::Relaxed);
    let n = TOTALS[2].load(Ordering::Relaxed);
    if n <= 8 || n.is_power_of_two() {
        tracing::warn!(
            n,
            copy_start = format_args!("{:#x}", shadow.start),
            copy_bytes = shadow.len,
            window = shadow.window,
            directory,
            frame = crate::frame::current(),
            "a copy moved to a GPU upload heap (VICTORY_READONLY_COPIES_ON_CARD) is written by shaders (directory=true: the \
             raw-address directory numbers it): moved back to the PC's memory, and never moved again; the lists recorded \
             with it already have the CPU read it to find what they wrote (slow there)"
        );
    }
    Some(back)
}

/// An ask found `shadow`, which stands for older copies a descriptor slot
/// may still show: what those slots read still crosses the bus, although
/// the frame line of where copies are asked for counts the ask where
/// `shadow` lives.
pub(super) fn asked_forwarding(shadow: &Shadow) {
    add(if shadow.promoted { S::AsksForwardingMoved } else { S::AsksForwardingOther }, 1);
}

/// The snapshot of a copy moved is taken: the CPU reads a GPU upload heap.
pub(super) fn snapshot_read(shadow: &Shadow) {
    add(S::SnapshotReads, 1);
    static SAID: AtomicU64 = AtomicU64::new(0);
    let n = SAID.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= 4 || n.is_power_of_two() {
        tracing::warn!(
            n,
            copy_start = format_args!("{:#x}", shadow.start),
            copy_bytes = shadow.len,
            "shader writes declared in a copy moved to a GPU upload heap (VICTORY_READONLY_COPIES_ON_CARD): the CPU reads it to \
             find them, which is slow there (falls back to the PC's memory for the next lists)"
        );
    }
}

/// What a copy is replaced with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// A twin in a GPU upload heap.
    Moved,
    /// A copy in the PC's memory, for a cold copy moved.
    BackCold,
    /// A copy in the PC's memory, shaders write it.
    BackForWrites,
}

/// Makes the copy that takes `old`'s place and puts it in the tables,
/// standing for `old`. `None` if no buffer was made, or the tables no
/// longer hold `old`.
fn replace(old: &Arc<Shadow>, host: &crate::host::Host, kind: Kind) -> Option<Arc<Shadow>> {
    // No buffer made for a copy the tables let go of already (a write
    // passed on to an older copy, a copy let go since the look).
    if !held(old) {
        add(S::Raced, 1);
        return None;
    }
    // SAFETY: a live buffer.
    let size = unsafe { old.buffer.GetDesc() }.Width;
    let (start, len) = (old.start, old.len);
    let (buffer, data) = match kind {
        Kind::Moved => {
            let made = host.gpu_upload_buffer(size);
            if made.is_none() {
                static SAID: AtomicBool = AtomicBool::new(false);
                if !SAID.swap(true, Ordering::Relaxed) {
                    tracing::warn!(
                        size,
                        "VICTORY_READONLY_COPIES_ON_CARD: the system made no buffer in a GPU upload heap; copies stay in the \
                         PC's memory this look"
                    );
                }
            }
            made?
        }
        Kind::BackCold | Kind::BackForWrites => host.shared_buffer(size)?,
    };
    crate::host::name(&buffer, || match kind {
        Kind::Moved => format!("title memory {start:#x}+{len:#x} moved to a GPU upload heap"),
        _ => format!("title memory {start:#x}+{len:#x} back in the PC's memory"),
    });
    // SAFETY: a live buffer.
    let gpu = unsafe { buffer.GetGPUVirtualAddress() };
    let mut new = Shadow::new(buffer, data, gpu, start, len);
    new.window = old.window;
    new.upload = kind == Kind::Moved;
    new.promoted = kind == Kind::Moved;
    if new.promoted {
        ALIVE[0].fetch_add(1, Ordering::Relaxed);
        ALIVE[1].fetch_add(len, Ordering::Relaxed);
    }
    if kind == Kind::BackForWrites {
        *new.written.get_mut() = true;
    }
    new.used.store(old.used.load(Ordering::Relaxed), Ordering::Relaxed);
    new.asked[0].store(old.asked[0].load(Ordering::Relaxed), Ordering::Relaxed);
    new.asked[1].store(old.asked[1].load(Ordering::Relaxed), Ordering::Relaxed);
    new.unsized_asks.store(old.unsized_asks.load(Ordering::Relaxed), Ordering::Relaxed);
    // Seen: it is judged by its own asks from the next look on.
    *new.seen.get_mut() = true;
    *new.forwards.get_mut().unwrap_or_else(|e| e.into_inner()) = vec![old.clone()];
    *new.forwarding.get_mut() = true;
    let new = Arc::new(new);
    if !put_in_place_of(old, &new) {
        add(S::Raced, 1);
        return None;
    }
    FORWARDING.lock().unwrap_or_else(|e| e.into_inner()).push(Arc::downgrade(&new));
    crate::host::trail(|| {
        let what = match kind {
            Kind::Moved => "moved to a GPU upload heap",
            Kind::BackCold => "put back in the PC's memory (cold)",
            Kind::BackForWrites => "moved back to the PC's memory (shader writes)",
        };
        format!("read-only copies: title memory {start:#x}+{len:#x} {what}")
    });
    Some(new)
}

/// Whether the tables hold `old`.
fn held(old: &Arc<Shadow>) -> bool {
    if old.window {
        LOOSE.read().unwrap_or_else(|e| e.into_inner()).get(&(old.start, old.len)).is_some_and(|held| Arc::ptr_eq(held, old))
    } else {
        BUFFERS
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .range((old.start, 0)..=(old.start, usize::MAX))
            .any(|(_, entry)| entry.shadow.as_ref().is_some_and(|held| Arc::ptr_eq(held, old)))
    }
}

/// Puts `new` in the tables where `old` is: false if `old` is not there
/// any more (let go, or the title released its buffer).
fn put_in_place_of(old: &Arc<Shadow>, new: &Arc<Shadow>) -> bool {
    if old.window {
        let mut map = LOOSE.write().unwrap_or_else(|e| e.into_inner());
        match map.get_mut(&(old.start, old.len)) {
            Some(held) if Arc::ptr_eq(held, old) => {
                *held = new.clone();
                true
            }
            _ => false,
        }
    } else {
        let mut map = buffers();
        for (_, entry) in map.range_mut((old.start, 0)..=(old.start, usize::MAX)) {
            if entry.shadow.as_ref().is_some_and(|held| Arc::ptr_eq(held, old)) {
                entry.shadow = Some(new.clone());
                return true;
            }
        }
        false
    }
}

/// Lets go of the older copies nothing but their newer one holds and whose
/// buffer no descriptor slot shows any more; the older copies those stood
/// for pass to the newer one. Returns how many went.
fn cleanup() -> u64 {
    let mut released = 0;
    let mut list = FORWARDING.lock().unwrap_or_else(|e| e.into_inner());
    list.retain(|weak| {
        let Some(newer) = weak.upgrade() else {
            return false;
        };
        let mut forwards = newer.forwards.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let mut changed = false;
            let mut kept = Vec::with_capacity(forwards.len());
            for older in forwards.drain(..) {
                if Arc::strong_count(&older) == 1 && crate::host::references(&older.buffer) == 1 {
                    kept.append(&mut older.forwards.lock().unwrap_or_else(|e| e.into_inner()));
                    older.forwarding.store(false, Ordering::Relaxed);
                    released += 1;
                    changed = true;
                    crate::host::retire(older);
                } else {
                    kept.push(older);
                }
            }
            *forwards = kept;
            if !changed {
                break;
            }
        }
        let any = !forwards.is_empty();
        newer.forwarding.store(any, Ordering::Relaxed);
        any
    });
    add(S::Released, released);
    released
}

/// Copies standing for older ones now; `None` if another thread holds the
/// list (the device-removed report does not wait for it).
fn forwarding_alive(wait: bool) -> Option<u64> {
    let list = if wait {
        FORWARDING.lock().unwrap_or_else(|e| e.into_inner())
    } else {
        match FORWARDING.try_lock() {
            Ok(list) => list,
            Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        }
    };
    Some(list.iter().filter(|w| w.strong_count() > 0).count() as u64)
}

/// A copy that could be moved, as [`plan`] sees it.
#[derive(Clone, Copy, Debug)]
struct Want {
    asks: u64,
    len: u64,
}

/// A copy in a GPU upload heap that could be put back to make room.
#[derive(Clone, Copy, Debug)]
struct Have {
    asks: u64,
    len: u64,
    /// Moved by this module (counted in its bytes), not a window made in
    /// such a heap (`VICTORY_UPLOAD_WINDOWS_MB`).
    moved: bool,
}

/// A copy put back to make room must be asked for at most a quarter as
/// often as the one it makes room for.
const HOTTER: u64 = 4;

/// What a look does: which wants are moved and which haves put back (by
/// index), and how many wants were turned down for room and for bytes.
#[derive(Debug, Default, PartialEq, Eq)]
struct Plan {
    moved: Vec<usize>,
    put_back: Vec<usize>,
    no_room: u64,
    no_budget: u64,
}

/// Hottest wants first: each is moved if it fits in `room` (bytes of the
/// card under the line) and `budget` (bytes left of the most); otherwise,
/// with `trade`, colder haves (`HOTTER`) are put back, coldest first, until
/// it fits (only moved ones give budget back). At most `per_look` moves
/// and put-backs together.
fn plan(wants: &[Want], haves: &[Have], mut room: u64, mut budget: u64, per_look: usize, trade: bool) -> Plan {
    let mut out = Plan::default();
    let mut order: Vec<usize> = (0..wants.len()).collect();
    order.sort_by(|&a, &b| wants[b].asks.cmp(&wants[a].asks));
    let mut cold: Vec<usize> = (0..haves.len()).collect();
    cold.sort_by_key(|&i| haves[i].asks);
    let mut taken = vec![false; haves.len()];
    for i in order {
        let w = wants[i];
        if out.moved.len() + out.put_back.len() >= per_look {
            break;
        }
        let need_room = w.len.saturating_sub(room);
        let need_budget = w.len.saturating_sub(budget);
        let mut picked = Vec::new();
        let (mut freed_room, mut freed_budget) = (0, 0);
        if trade && (need_room > 0 || need_budget > 0) {
            for &j in &cold {
                if freed_room >= need_room && freed_budget >= need_budget {
                    break;
                }
                let h = haves[j];
                if taken[j] || h.asks.saturating_mul(HOTTER) >= w.asks {
                    continue;
                }
                // Only a moved copy gives bytes of the most back.
                if freed_room >= need_room && !h.moved {
                    continue;
                }
                picked.push(j);
                freed_room += h.len;
                if h.moved {
                    freed_budget += h.len;
                }
            }
        }
        if freed_budget < need_budget {
            out.no_budget += 1;
            continue;
        }
        if freed_room < need_room {
            out.no_room += 1;
            continue;
        }
        if out.moved.len() + out.put_back.len() + picked.len() + 1 > per_look {
            break;
        }
        for &j in &picked {
            taken[j] = true;
        }
        out.put_back.extend(picked);
        out.moved.push(i);
        room = room + freed_room - w.len;
        budget = budget + freed_budget - w.len;
    }
    out
}

/// Frame number at the last look.
static LAST_FRAME: AtomicU64 = AtomicU64::new(0);

/// What the last look did, for the device-removed report: the frame, moved,
/// put back, turned down.
static LAST_LOOK: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

/// Once a second, before a submission starts (the host's sweep): lets go
/// of older copies nothing shows any more, then, with the switch on, moves
/// the hottest copies shaders only read to GPU upload heaps (see the
/// module).
pub(crate) fn tick() {
    cleanup();
    if !on() {
        return;
    }
    let Some(host) = host() else {
        return;
    };
    if !host.gpu_upload_supported() {
        static SAID: AtomicBool = AtomicBool::new(false);
        if !SAID.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                "VICTORY_READONLY_COPIES_ON_CARD=1, but the system has no GPU upload heaps (Resizable BAR off, or an older \
                 system or driver): every copy stays in the PC's memory"
            );
        }
        return;
    }
    let began = std::time::Instant::now();
    let memory = host.video_memory();
    let s = settings(memory.map(|m| m[0]));
    let frame = crate::frame::current();
    let frames = frame.saturating_sub(LAST_FRAME.swap(frame, Ordering::Relaxed)).max(1);
    let least = (s.min_asks * frames as f64).ceil().max(1.0) as u64;

    // Every copy in the tables, once: its asks since the last look.
    let mut seen = std::collections::HashSet::new();
    let mut wants: Vec<(Want, Arc<Shadow>)> = Vec::new();
    let mut haves: Vec<(Have, Arc<Shadow>)> = Vec::new();
    let mut hot_written = 0;
    let mut look = |shadow: &Arc<Shadow>| {
        if !seen.insert(Arc::as_ptr(shadow) as usize) {
            return;
        }
        let asks = shadow.asks.swap(0, Ordering::Relaxed);
        // A copy made since the last look has not had a whole look's asks.
        if !shadow.seen.swap(true, Ordering::Relaxed) || shadow.place.is_some() || shadow.card.is_some() {
            return;
        }
        if shadow.upload {
            if shadow.window || shadow.promoted {
                haves.push((
                    Have {
                        asks,
                        len: shadow.len,
                        moved: shadow.promoted,
                    },
                    shadow.clone(),
                ));
            }
            return;
        }
        if asks < least {
            return;
        }
        if shadow.written.load(Ordering::Relaxed) {
            hot_written += 1;
            return;
        }
        wants.push((Want { asks, len: shadow.len }, shadow.clone()));
    };
    {
        let map = BUFFERS.read().unwrap_or_else(|e| e.into_inner());
        for entry in map.values() {
            if let Some(shadow) = &entry.shadow {
                look(shadow);
            }
        }
    }
    {
        let map = LOOSE.read().unwrap_or_else(|e| e.into_inner());
        for shadow in map.values() {
            look(shadow);
        }
    }
    // Too big for the most, moved back once, or put back only if putting
    // it back frees its room (no slot shows it, it stands for no older
    // copy).
    let cap = (s.most / 4).min(256 << 20);
    wants.retain(|(w, shadow)| w.len <= cap && !never(shadow.start, shadow.len));
    haves.retain(|(_, shadow)| {
        !shadow.forwarding.load(Ordering::Relaxed) && crate::host::references(&shadow.buffer) == 1
    });
    add(S::Candidates, wants.len() as u64);
    add(S::HotWritten, hot_written);
    let room = match memory {
        Some([budget, used, ..]) => (budget / 100 * s.percent).saturating_sub(used),
        None => 0,
    };
    let budget = s.most.saturating_sub(ALIVE[1].load(Ordering::Relaxed));
    let want_list: Vec<Want> = wants.iter().map(|(w, _)| *w).collect();
    let have_list: Vec<Have> = haves.iter().map(|(h, _)| *h).collect();
    let p = plan(&want_list, &have_list, room, budget, s.per_look, s.trade);
    add(S::NoRoom, p.no_room);
    add(S::NoBudget, p.no_budget);
    // Put back first: their room goes when the lists that hold them are
    // done (one frame or two).
    let (mut put_back, mut moved) = (0, 0);
    for &j in &p.put_back {
        let (h, shadow) = &haves[j];
        if replace(shadow, host, Kind::BackCold).is_some() {
            put_back += 1;
            add(S::PutBack, 1);
            add(S::PutBackBytes, h.len);
        }
    }
    for &i in &p.moved {
        if began.elapsed().as_millis() as u64 >= s.look_ms {
            break;
        }
        let (w, shadow) = &wants[i];
        if replace(shadow, host, Kind::Moved).is_some() {
            moved += 1;
            add(S::Moved, 1);
            add(S::MovedBytes, w.len);
            // The first ones said one by one, then every 64th.
            let n = TOTALS[0].load(Ordering::Relaxed) + moved;
            if n <= 16 || n.is_multiple_of(64) {
                tracing::info!(
                    n,
                    start = format_args!("{:#x}", shadow.start),
                    bytes = shadow.len,
                    window = shadow.window,
                    asks_per_frame = format_args!("{:.1}", w.asks as f64 / frames as f64),
                    shown_by_slots = crate::host::references(&shadow.buffer) > 1,
                    frame,
                    "a copy shaders only read moved to a GPU upload heap (VICTORY_READONLY_COPIES_ON_CARD)"
                );
            }
        }
    }
    TOTALS[0].fetch_add(moved, Ordering::Relaxed);
    TOTALS[1].fetch_add(put_back, Ordering::Relaxed);
    let at = [frame, moved, put_back, p.no_room + p.no_budget];
    for (slot, value) in LAST_LOOK.iter().zip(at) {
        slot.store(value, Ordering::Relaxed);
    }
    add(S::LookUs, began.elapsed().as_micros() as u64);
}

/// The frame line, over `frames`: what was moved and why not. Nothing while
/// the switch is off and nothing was ever moved.
pub(crate) fn line(frames: f64) {
    let v: Vec<u64> = STATS.iter().map(|a| a.swap(0, Ordering::Relaxed)).collect();
    if !on() && TOTALS.iter().all(|t| t.load(Ordering::Relaxed) == 0) && v.iter().all(|&x| x == 0) {
        return;
    }
    let f = |s: S| v[s as usize] as f64 / frames;
    let mb = |s: S| v[s as usize] as f64 / frames / (1024.0 * 1024.0);
    let most = MOST.load(Ordering::Relaxed);
    tracing::info!(
        "read-only copies moved to the card's memory per frame (VICTORY_READONLY_COPIES_ON_CARD): {:.2} moved ({:.1} MB), \
         {:.2} cold ones put back in the PC's memory ({:.1} MB), {:.2} moved back for shader writes, {:.2} moved back for the \
         raw-address directory, {:.2} snapshots read from GPU upload heaps; {:.1} MB written into them over the bus by \
         refreshes; turned down {:.2} for room on the card, {:.2} over VICTORY_READONLY_COPIES_MB; {:.1} could move, {:.1} hot \
         ones written by shaders; now {} moved alive ({} MB of at most {} MB), {} copies standing for older ones a slot may \
         show ({:.0} refreshes and writes passed on, {:.2} older let go per frame; of the asks counted under \"a GPU upload \
         heap\" {:.0} found a moved copy whose older copy a slot may still read over the bus, and {:.0} other asks such a \
         copy); looks {:.2} ms per frame, {} raced",
        f(S::Moved),
        mb(S::MovedBytes),
        f(S::PutBack),
        mb(S::PutBackBytes),
        f(S::MovedBack),
        f(S::MovedBackDirectory),
        f(S::SnapshotReads),
        mb(S::RefreshedBytes),
        f(S::NoRoom),
        f(S::NoBudget),
        f(S::Candidates),
        f(S::HotWritten),
        ALIVE[0].load(Ordering::Relaxed),
        ALIVE[1].load(Ordering::Relaxed) >> 20,
        most >> 20,
        forwarding_alive(true).unwrap_or(0),
        f(S::Forwarded),
        f(S::Released),
        f(S::AsksForwardingMoved),
        f(S::AsksForwardingOther),
        v[S::LookUs as usize] as f64 / 1000.0 / frames,
        v[S::Raced as usize],
    );
}

/// For the device-removed report: nothing while the switch is off and
/// nothing was ever moved.
pub(crate) fn removal_note() -> Option<String> {
    if !on() && TOTALS.iter().all(|t| t.load(Ordering::Relaxed) == 0) && ALIVE[0].load(Ordering::Relaxed) == 0 {
        return None;
    }
    Some(format!(
        "read-only copies in GPU upload heaps (VICTORY_READONLY_COPIES_ON_CARD {}): {} alive ({} MB); since the start {} moved, \
         {} put back cold, {} moved back for shader writes or the raw-address directory; {} copies standing for older ones; \
         the last look at frame {} moved {}, put back {}, turned down {}",
        if on() { "on" } else { "off" },
        ALIVE[0].load(Ordering::Relaxed),
        ALIVE[1].load(Ordering::Relaxed) >> 20,
        TOTALS[0].load(Ordering::Relaxed),
        TOTALS[1].load(Ordering::Relaxed),
        TOTALS[2].load(Ordering::Relaxed),
        forwarding_alive(false).map_or_else(|| "(list busy)".to_string(), |n| n.to_string()),
        LAST_LOOK[0].load(Ordering::Relaxed),
        LAST_LOOK[1].load(Ordering::Relaxed),
        LAST_LOOK[2].load(Ordering::Relaxed),
        LAST_LOOK[3].load(Ordering::Relaxed),
    ))
}

#[cfg(test)]
mod tests {
    use super::{FORWARDING, Have, LOOSE, Plan, Shadow, Want, cleanup, default_most, forwards_of, plan, put_in_place_of};
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;
    use windows::Win32::Graphics::Direct3D12::*;
    use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
    use windows::Win32::Graphics::Dxgi::*;

    const MB: u64 = 1 << 20;

    fn want(asks: u64) -> Want {
        Want { asks, len: 4 * MB }
    }

    fn have(asks: u64, moved: bool) -> Have {
        Have {
            asks,
            len: 4 * MB,
            moved,
        }
    }

    #[test]
    fn the_hottest_move_first_while_there_is_room() {
        let p = plan(&[want(10), want(50), want(30)], &[], 8 * MB, 100 * MB, 16, true);
        assert_eq!(p.moved, vec![1, 2]);
        assert!(p.put_back.is_empty());
        assert_eq!(p.no_room, 1);
    }

    #[test]
    fn only_colder_copies_are_put_back_to_make_room() {
        let haves = [have(100, false), have(1, false), have(20, false)];
        let p = plan(&[want(50)], &haves, 0, 100 * MB, 16, true);
        // 1 * 4 < 50: put back; 20 * 4 >= 50 and 100: kept.
        assert_eq!(p.put_back, vec![1]);
        assert_eq!(p.moved, vec![0]);
        // Without trading nothing moves.
        let p = plan(&[want(50)], &haves, 0, 100 * MB, 16, false);
        assert_eq!(
            p,
            Plan {
                moved: vec![],
                put_back: vec![],
                no_room: 1,
                no_budget: 0
            }
        );
    }

    #[test]
    fn a_copy_put_back_makes_room_once() {
        let haves = [have(0, false)];
        let p = plan(&[want(50), want(40)], &haves, 0, 100 * MB, 16, true);
        assert_eq!(p.moved, vec![0]);
        assert_eq!(p.put_back, vec![0]);
        assert_eq!(p.no_room, 1);
    }

    #[test]
    fn only_moved_copies_give_bytes_of_the_most_back() {
        // Room on the card, none left of the most.
        let p = plan(&[want(50)], &[have(0, false)], 100 * MB, 0, 16, true);
        assert!(p.moved.is_empty() && p.put_back.is_empty());
        assert_eq!(p.no_budget, 1);
        let p = plan(&[want(50)], &[have(0, false), have(1, true)], 100 * MB, 0, 16, true);
        assert_eq!(p.put_back, vec![1]);
        assert_eq!(p.moved, vec![0]);
    }

    #[test]
    fn moves_and_put_backs_stay_under_the_look_limit() {
        let wants: Vec<Want> = (0..10).map(|i| want(100 + i)).collect();
        let p = plan(&wants, &[], 1000 * MB, 1000 * MB, 3, true);
        assert_eq!(p.moved.len(), 3);
        let haves: Vec<Have> = (0..10).map(|_| have(0, false)).collect();
        let p = plan(&wants, &haves, 0, 1000 * MB, 3, true);
        // Each move needs a put-back: one pair, and the next pair would pass 3.
        assert_eq!(p.moved.len() + p.put_back.len(), 2);
    }

    /// A plain buffer on the software adapter, `None` without one.
    fn warp_buffer() -> Option<ID3D12Resource> {
        // SAFETY: plain DXGI and D3D12 creation calls on the software adapter.
        unsafe {
            let factory: IDXGIFactory4 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).ok()?;
            let adapter: IDXGIAdapter = factory.EnumWarpAdapter().ok()?;
            let mut device: Option<ID3D12Device> = None;
            D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut device).ok()?;
            let device = device?;
            let heap = D3D12_HEAP_PROPERTIES {
                Type: D3D12_HEAP_TYPE_DEFAULT,
                ..Default::default()
            };
            let desc = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                Width: 64 << 10,
                Height: 1,
                DepthOrArraySize: 1,
                MipLevels: 1,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                ..Default::default()
            };
            let mut buffer: Option<ID3D12Resource> = None;
            device
                .CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &desc, D3D12_RESOURCE_STATE_COMMON, None, &mut buffer)
                .ok()?;
            buffer
        }
    }

    /// A window's copy of `[start, start + 64 KiB)` on `buffer`, standing
    /// for `older`.
    fn window(buffer: ID3D12Resource, memory: &mut [u8], start: u64, older: Vec<Arc<Shadow>>) -> Arc<Shadow> {
        let mut shadow = Shadow::new(buffer, memory.as_mut_ptr(), 0, start, memory.len() as u64);
        shadow.window = true;
        *shadow.forwarding.get_mut() = !older.is_empty();
        *shadow.forwards.get_mut().unwrap() = older;
        Arc::new(shadow)
    }

    #[test]
    fn a_copy_takes_the_place_of_one_the_tables_still_hold() {
        let (Some(a), Some(b)) = (warp_buffer(), warp_buffer()) else {
            return;
        };
        let (mut ma, mut mb) = (vec![0u8; 64 << 10], vec![0u8; 64 << 10]);
        let start = 0x7ff1_0000_0000u64;
        let old = window(a, &mut ma, start, Vec::new());
        LOOSE.write().unwrap().insert((start, old.len), old.clone());
        let new = window(b, &mut mb, start, vec![old.clone()]);
        assert!(put_in_place_of(&old, &new));
        assert!(Arc::ptr_eq(LOOSE.read().unwrap().get(&(start, old.len)).unwrap(), &new));
        // Not there any more: nothing to replace.
        assert!(!put_in_place_of(&old, &new));
        LOOSE.write().unwrap().remove(&(start, old.len));
    }

    #[test]
    fn an_older_copy_stays_while_a_slot_shows_it() {
        let (Some(a), Some(b), Some(c)) = (warp_buffer(), warp_buffer(), warp_buffer()) else {
            return;
        };
        let (mut ma, mut mb, mut mc) = (vec![0u8; 64 << 10], vec![0u8; 64 << 10], vec![0u8; 64 << 10]);
        let start = 0x7ff2_0000_0000u64;
        // oldest <- middle <- newest; a slot shows the oldest only.
        let oldest = window(a, &mut ma, start, Vec::new());
        let slot = oldest.buffer.clone();
        let middle = window(b, &mut mb, start, vec![oldest.clone()]);
        let newest = window(c, &mut mc, start, vec![middle.clone()]);
        FORWARDING.lock().unwrap().push(Arc::downgrade(&newest));
        // (Let go through `crate::host::retire`, which may hold them while
        // another test's submission runs: looked at by the newest's list.)
        let oldest_at = Arc::as_ptr(&oldest);
        drop((oldest, middle));
        cleanup();
        // The middle one went; the oldest passed to the newest.
        let forwards = forwards_of(&newest);
        assert_eq!(forwards.len(), 1);
        assert_eq!(Arc::as_ptr(&forwards[0]), oldest_at);
        drop(forwards);
        assert!(newest.forwarding.load(Ordering::Relaxed));
        // The slot is written again: the oldest goes too.
        drop(slot);
        cleanup();
        assert!(forwards_of(&newest).is_empty());
        assert!(!newest.forwarding.load(Ordering::Relaxed));
    }

    #[test]
    fn the_default_most_follows_the_cards_budget() {
        assert_eq!(default_most(None), 256 * MB);
        assert_eq!(default_most(Some(15209 * MB)), 15209 * MB / 32);
        assert_eq!(default_most(Some(64 * 1024 * MB)), 512 * MB);
        assert_eq!(default_most(Some(1024 * MB)), 64 * MB);
    }

    /// End to end on the software adapter, with the host's own lists: a
    /// title buffer's copy is replaced (by a twin in a GPU upload heap when
    /// the adapter has such heaps, else by a copy in the PC's memory), a
    /// refresh asked of the new copy brings the old one up to date too (a
    /// slot may show it), and the raw-address directory takes a moved copy
    /// back to the PC's memory and never lets it move again. Does nothing
    /// unless the host device is the software adapter:
    /// `VICTORY_D3D12_WARP=1 cargo test -p victory-gpu --features diagnostics
    /// --lib readonly`.
    #[test]
    fn warp_a_refresh_of_the_new_copy_reaches_the_old_one_and_the_directory_moves_it_back() {
        use super::{Kind, for_directory, host, never, replace};
        if !cfg!(feature = "diagnostics") || std::env::var_os("VICTORY_D3D12_WARP").is_none() {
            return;
        }
        let Some(host) = host() else {
            return;
        };
        // Under the card copies' smallest size: a copy in the PC's memory.
        const LEN: usize = 256 << 10;
        let mut title = vec![0u8; LEN];
        let start = title.as_ptr() as u64;
        let fill = |title: &mut [u8], seed: u8| {
            for (i, b) in title.iter_mut().enumerate() {
                *b = (i as u8).wrapping_mul(31).wrapping_add(seed);
            }
        };
        fill(&mut title, 1);
        crate::shadow::register(0x7ead_0417, start, LEN as u64);
        let old = crate::shadow::find(start, LEN as u64).expect("a host copy");
        assert!(!old.upload && old.card.is_none() && old.place.is_none(), "a copy in the PC's memory");
        let kind = if host.gpu_upload_supported() { Kind::Moved } else { Kind::BackCold };
        let new = replace(&old, host, kind).expect("a copy in its place");
        let found = crate::shadow::find(start, LEN as u64).expect("a host copy");
        assert!(Arc::ptr_eq(&found, &new), "the tables hold the new copy");
        drop(found);
        assert_eq!(new.promoted, kind == Kind::Moved);
        let older = forwards_of(&new);
        assert!(older.len() == 1 && Arc::ptr_eq(&older[0], &old), "the new copy stands for the old one");
        drop(older);
        // The title writes; a list asks the new copy only.
        fill(&mut title, 7);
        let list = host.new_list().expect("a host list");
        new.refresh(&list, start, LEN as u64);
        list.close();
        host.execute(&[&list]);
        crate::host::settle_now(crate::host::WHY_SYNC);
        // SAFETY: both mappings hold `LEN` bytes; the one in a GPU upload
        // heap is read here only, in a test.
        let (in_old, in_new) =
            unsafe { (std::slice::from_raw_parts(old.data.cast_const(), LEN), std::slice::from_raw_parts(new.data.cast_const(), LEN)) };
        assert!(in_old == &title[..], "the old copy, which a slot may show, was brought up to date");
        assert!(in_new == &title[..], "the new copy was brought up to date");
        drop(list);
        // The directory numbers the copy: never moved, and a moved one goes
        // back to the PC's memory first.
        let numbered = for_directory(new.clone());
        assert!(numbered.written.load(Ordering::Relaxed));
        if kind == Kind::Moved {
            assert!(!Arc::ptr_eq(&numbered, &new), "a copy in the PC's memory took the moved one's place");
            assert!(!numbered.upload && !numbered.promoted);
            assert!(never(start, LEN as u64), "never moved again");
            let found = crate::shadow::find(start, LEN as u64).expect("a host copy");
            assert!(Arc::ptr_eq(&found, &numbered));
            assert!(forwards_of(&numbered).iter().any(|o| Arc::ptr_eq(o, &new)), "it stands for the moved one");
        } else {
            assert!(Arc::ptr_eq(&numbered, &new), "a copy in the PC's memory is numbered as it is");
        }
        eprintln!("readonly WARP test: the new copy was {kind:?}");
        crate::shadow::unregister(0x7ead_0417, start);
        drop((numbered, new, old));
        drop(title);
    }
}
