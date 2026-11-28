//! Big buffers let go on the layer's busy threads are dropped on a thread
//! of their own, `victory-reaper`, once the card has finished the
//! submissions begun when they were let go (`VICTORY_DEFERRED_RELEASE=1`;
//! off by default; also by `crate::ab::pick`; `2`: also every lot the
//! landing drops, [`super::bury`]).
//!
//! Idea from kylepelham/El1xr #224 (`VICTORY_PREPARE_FENCED`), where the
//! staging blocks of its texture prepare queue are dropped on its reaper two
//! submissions later: a 64 MiB block cost 20-26 ms to the thread that took
//! the next piece. That queue is not ours; what is: a list's upload buffers
//! made per copy (`HostList::upload` without `VICTORY_SOURCE_POOL`, the big
//! scratch of `HostList::scratch_at`), freed when the list is reset, on the
//! title's thread that resets it or on the one that recycles lists; and the
//! pooled copy sources and buffers on the card a full pool frees
//! (`give_uploads`, `give_uavs`, with `VICTORY_SPARE_EVICT` the oldest kept
//! ones). Of those, the ones of [`BIG`] bytes or more come here; the others
//! are dropped where they are let go, as before.
//!
//! Whatever comes here was safe to drop where it was let go (every
//! submission of its list had run); the wait only adds the card's order on
//! top, as `super::retire` does, with no bury lag. Off, the big ones are
//! still timed where they are dropped (grep `big releases`).

use std::any::Any;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Condvar, Mutex, OnceLock};

/// Bytes from which a buffer let go is a big one.
pub(crate) const BIG: u64 = 16 << 20;

/// The switch: 0 off, 1 big buffers, 2 big buffers and the landing's lots.
pub(crate) fn level() -> u8 {
    if let Some(on) = crate::ab::pick("VICTORY_DEFERRED_RELEASE") {
        return u8::from(on);
    }
    static LEVEL: OnceLock<u8> = OnceLock::new();
    *LEVEL.get_or_init(|| level_of(std::env::var("VICTORY_DEFERRED_RELEASE").ok().as_deref()))
}

fn level_of(value: Option<&str>) -> u8 {
    match value.map(str::trim) {
        Some("1") => 1,
        Some("2") => 2,
        _ => 0,
    }
}

pub(crate) fn on() -> bool {
    level() > 0
}

/// What was let go, by the submission number it waits for.
pub(crate) struct Graves<T> {
    held: VecDeque<(u64, T)>,
}

impl<T> Graves<T> {
    pub(crate) const fn new() -> Self {
        Graves { held: VecDeque::new() }
    }

    /// `object`, to wait until submission `until` has finished.
    pub(crate) fn bury(&mut self, until: u64, object: T) {
        self.held.push_back((until, object));
    }

    /// What waited for submissions up to `finished`, in the order buried.
    /// (Numbers come in order but for threads that overtake each other: one
    /// of those waits a little longer than it must.)
    pub(crate) fn ripe(&mut self, finished: u64) -> Vec<T> {
        let n = self.held.iter().take_while(|(until, _)| *until <= finished).count();
        self.held.drain(..n).map(|(_, object)| object).collect()
    }

    pub(crate) fn len(&self) -> usize {
        self.held.len()
    }
}

/// What becomes of a thing let go.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Fate {
    /// Dropped where it is let go, as before (small, or switched off).
    Here,
    /// Dropped where it is let go, timed: a big one, switched off.
    HereTimed,
    /// Waits for the card, then goes to `victory-reaper`.
    Reaper,
}

pub(crate) fn fate(on: bool, bytes: u64) -> Fate {
    match (bytes >= BIG, on) {
        (false, _) => Fate::Here,
        (true, false) => Fate::HereTimed,
        (true, true) => Fate::Reaper,
    }
}

struct Lot(#[allow(dead_code)] Box<dyn Any>);
// SAFETY: D3D12 resources (free-threaded) and the wrappers holding them,
// dropped once; they were dropped by whichever thread let them go before.
unsafe impl Send for Lot {}

static GRAVES: Mutex<Graves<Lot>> = Mutex::new(Graves::new());
static WAKE: Condvar = Condvar::new();
/// Lots waiting, to skip the lock and the wake when there are none.
static WAITING: AtomicU64 = AtomicU64::new(0);

/// Since the last frame-rate line: big drops where let go, their
/// microseconds, the longest; big things handed to the reaper, their bytes;
/// the reaper's drops, their microseconds, the longest; the landing's lots
/// dropped where they ripened, their microseconds, the longest.
static STATS: [AtomicU64; 11] = [const { AtomicU64::new(0) }; 11];
const HERE: usize = 0;
const HERE_US: usize = 1;
const HERE_LONGEST: usize = 2;
const HANDED: usize = 3;
const HANDED_BYTES: usize = 4;
const REAPED: usize = 5;
const REAPED_US: usize = 6;
const REAPED_LONGEST: usize = 7;
const LANDING: usize = 8;
const LANDING_US: usize = 9;
const LANDING_LONGEST: usize = 10;

/// The submission a thing let go now waits for: the newest begun, and those
/// the title made that the host has not begun.
fn until() -> u64 {
    super::submissions().0 + crate::queue::queued()
}

fn timed(began: std::time::Instant, [count, total, longest]: [usize; 3]) {
    let us = began.elapsed().as_micros() as u64;
    STATS[count].fetch_add(1, Relaxed);
    STATS[total].fetch_add(us, Relaxed);
    STATS[longest].fetch_max(us, Relaxed);
}

/// Lets go of `things` (each with its bytes): the big ones to the reaper
/// with the switch, the others dropped here (the big ones timed).
pub(crate) fn let_go_sized<T: 'static>(things: impl IntoIterator<Item = (T, u64)>) {
    let on = on();
    let mut handed: Vec<Lot> = Vec::new();
    let mut bytes = 0;
    for (thing, size) in things {
        match fate(on, size) {
            Fate::Here => drop(thing),
            Fate::HereTimed => {
                let began = std::time::Instant::now();
                drop(thing);
                timed(began, [HERE, HERE_US, HERE_LONGEST]);
            }
            Fate::Reaper => {
                bytes += size;
                handed.push(Lot(Box::new(thing)));
            }
        }
    }
    if handed.is_empty() {
        return;
    }
    STATS[HANDED].fetch_add(handed.len() as u64, Relaxed);
    STATS[HANDED_BYTES].fetch_add(bytes, Relaxed);
    hand(handed);
}

/// Buries `lots` until the submissions begun now have finished, and wakes
/// the reaper.
fn hand(lots: Vec<Lot>) {
    reaper();
    let until = until();
    let n = lots.len() as u64;
    {
        let mut graves = GRAVES.lock().unwrap_or_else(|e| e.into_inner());
        for lot in lots {
            graves.bury(until, lot);
        }
    }
    WAITING.fetch_add(n, Relaxed);
    WAKE.notify_one();
}

/// The landing's ripe lots ([`super::bury`]): with `VICTORY_DEFERRED_RELEASE=2`
/// handed to the reaper (dropped at once there: they are ripe), else dropped
/// here, timed.
pub(super) fn landing_drop<T: 'static>(done: Vec<T>) {
    if done.is_empty() {
        return;
    }
    if level() >= 2 {
        hand(vec![Lot(Box::new(done))]);
        return;
    }
    let began = std::time::Instant::now();
    drop(done);
    timed(began, [LANDING, LANDING_US, LANDING_LONGEST]);
}

/// Submissions finished ([`super::bury`]): the reaper looks if anything
/// waits.
pub(super) fn poke() {
    if WAITING.load(Relaxed) > 0 {
        WAKE.notify_one();
    }
}

/// Starts `victory-reaper` once.
fn reaper() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let spawned = std::thread::Builder::new().name("victory-reaper".into()).spawn(reap);
        if let Err(e) = spawned {
            tracing::warn!("victory-reaper did not start ({e}): big buffers wait for the next landing's poke");
        }
    });
}

fn reap() {
    let mut graves = GRAVES.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        let ripe = graves.ripe(super::submissions().1);
        if ripe.is_empty() {
            // Woken by a hand or a landing; the timeout covers a wake lost
            // between the look and the wait.
            graves = WAKE
                .wait_timeout(graves, std::time::Duration::from_millis(20))
                .unwrap_or_else(|e| e.into_inner())
                .0;
            continue;
        }
        drop(graves);
        WAITING.fetch_sub(ripe.len() as u64, Relaxed);
        for lot in ripe {
            let began = std::time::Instant::now();
            drop(lot);
            timed(began, [REAPED, REAPED_US, REAPED_LONGEST]);
        }
        graves = GRAVES.lock().unwrap_or_else(|e| e.into_inner());
    }
}

/// The frame-rate line (grep `big releases`), when anything happened.
pub(super) fn line(frames: f64) {
    let c = STATS.each_ref().map(|s| s.swap(0, Relaxed));
    let waiting = GRAVES.lock().unwrap_or_else(|e| e.into_inner()).len();
    if c.iter().all(|&v| v == 0) && waiting == 0 {
        return;
    }
    tracing::info!("{}", text(c, waiting, frames, level()));
}

fn text(c: [u64; 11], waiting: usize, frames: f64, level: u8) -> String {
    let ms = |us: u64| us as f64 / 1e3;
    format!(
        "big releases (>= {} MiB), over {frames:.0} frames: where let go {} in {:.1} ms (longest {} us); \
         handed to victory-reaper {} ({:.0} MiB), dropped there {} in {:.1} ms (longest {} us), {waiting} waiting; \
         landing drops {} in {:.1} ms (longest {} us) (VICTORY_DEFERRED_RELEASE={level})",
        BIG >> 20,
        c[HERE],
        ms(c[HERE_US]),
        c[HERE_LONGEST],
        c[HANDED],
        c[HANDED_BYTES] as f64 / (1u64 << 20) as f64,
        c[REAPED],
        ms(c[REAPED_US]),
        c[REAPED_LONGEST],
        c[LANDING],
        ms(c[LANDING_US]),
        c[LANDING_LONGEST],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// A buffer let go while submission 5 may still use it is not dropped
    /// before submission 5 has finished.
    #[test]
    fn a_lot_waits_for_the_submission_begun_when_let_go() {
        let buffer = Arc::new(());
        let mut graves = Graves::new();
        graves.bury(5, Arc::clone(&buffer));
        for finished in [3, 4] {
            assert!(graves.ripe(finished).is_empty(), "dropped at {finished}");
            assert_eq!(Arc::strong_count(&buffer), 2);
        }
        let ripe = graves.ripe(5);
        assert_eq!(ripe.len(), 1);
        drop(ripe);
        assert_eq!(Arc::strong_count(&buffer), 1);
        assert_eq!(graves.len(), 0);
    }

    /// Lots ripen in the order they were buried.
    #[test]
    fn lots_ripen_in_order() {
        let mut graves = Graves::new();
        graves.bury(1, 'a');
        graves.bury(4, 'b');
        graves.bury(4, 'c');
        assert_eq!(graves.ripe(3), ['a']);
        assert_eq!(graves.ripe(3), [] as [char; 0]);
        assert_eq!(graves.ripe(9), ['b', 'c']);
    }

    /// Only big things go to the reaper, and only with the switch; off, the
    /// big ones are timed where they go.
    #[test]
    fn only_big_things_go_to_the_reaper() {
        assert_eq!(fate(true, BIG - 1), Fate::Here);
        assert_eq!(fate(false, BIG - 1), Fate::Here);
        assert_eq!(fate(false, BIG), Fate::HereTimed);
        assert_eq!(fate(true, BIG), Fate::Reaper);
        assert_eq!(fate(true, 64 << 20), Fate::Reaper);
    }

    /// The switch is off unless 1 or 2.
    #[test]
    fn the_switch_is_off_unless_set() {
        assert_eq!(level_of(None), 0);
        assert_eq!(level_of(Some("0")), 0);
        assert_eq!(level_of(Some("yes")), 0);
        assert_eq!(level_of(Some("1")), 1);
        assert_eq!(level_of(Some(" 2 ")), 2);
        if std::env::var_os("VICTORY_DEFERRED_RELEASE").is_none() && crate::ab::pick("VICTORY_DEFERRED_RELEASE").is_none() {
            assert!(!on());
        }
    }

    /// Off, small things are dropped at once and nothing waits.
    #[test]
    fn small_things_are_dropped_at_once() {
        let buffer = Arc::new(());
        let_go_sized([(Arc::clone(&buffer), 1u64 << 20)]);
        assert_eq!(Arc::strong_count(&buffer), 1);
    }

    #[test]
    fn the_line_names_the_switch() {
        let text = text([2, 45_000, 26_000, 3, 192 << 20, 3, 1500, 900, 7, 300, 120], 1, 100.0, 1);
        assert!(text.starts_with("big releases (>= 16 MiB), over 100 frames: where let go 2 in 45.0 ms (longest 26000 us)"));
        assert!(text.contains("handed to victory-reaper 3 (192 MiB), dropped there 3 in 1.5 ms"));
        assert!(text.ends_with("(VICTORY_DEFERRED_RELEASE=1)"));
    }
}
