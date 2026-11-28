//! Frame pacing: the title sets a frame interval and waits for each frame
//! to begin. Ours is a clock started by the first interval set, ticking
//! once per interval; the token is the frame number. Each event kind
//! (origin, vblank, flip) happens once per tick, so two waits for the same
//! kind never end on the same tick: the title polls the origin
//! with "token on time-out" (starting its next frame at once) and is paced
//! by a blocking wait for the flip before each present.
//!
//! shortcut: event offsets are ignored (every event is at the tick); tie
//! the clock to presentation if a title paces by them.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::object::{E_INVALIDARG, HRESULT, S_FALSE, S_OK, This};

/// What a wait returns when its time is up first.
const TIMEOUT: HRESULT = S_FALSE;
/// The frame-origin event: the one that starts frames and makes tokens.
const ORIGIN: u32 = 0;
/// Make the frame's token even if the wait times out (the title starts the
/// frame early).
const TOKEN_ON_TIMEOUT: u32 = 2;
const SIXTIETH: Duration = Duration::from_micros(16_667);

struct Clock {
    start: Instant,
    interval: Duration,
    /// The interval the title set (ours differs under a frame rate cap,
    /// [`capped`]).
    title: Duration,
    /// The tick number at `start` (ticks go on across interval changes).
    first: u64,
}

static CLOCK: Mutex<Option<Clock>> = Mutex::new(None);
static FRAME: AtomicU64 = AtomicU64::new(0);

fn clock() -> std::sync::MutexGuard<'static, Option<Clock>> {
    CLOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// `VICTORY_FRAME_INTERVAL_US=<n>`: frames begin every `n` microseconds
/// whatever interval the title sets (16667: 60 a second, 8333: 120, 1000:
/// as fast as frames are made); the F1 menu's FRAME RATE CAP saves it for
/// the next start. The title sets 33,333 us (30 a second) against the 60 Hz
/// mode it is offered; it times its own frames with the performance counter,
/// so a shorter interval is a higher frame rate, not a faster game. Values
/// outside 1,000..=1,000,000 are ignored. Unset: the title's own interval.
///
/// (From trustno1-coder/VIctory-NVIDIA 22c6be2, "no frame rate cap".)
fn interval_override() -> Option<Duration> {
    static OVERRIDE: std::sync::OnceLock<Option<Duration>> = std::sync::OnceLock::new();
    *OVERRIDE.get_or_init(|| {
        let us = std::env::var("VICTORY_FRAME_INTERVAL_US")
            .ok()?
            .trim()
            .parse::<u64>()
            .ok()?;
        let interval = (1_000..=1_000_000)
            .contains(&us)
            .then(|| Duration::from_micros(us));
        match interval {
            Some(interval) => {
                tracing::info!(?interval, "VICTORY_FRAME_INTERVAL_US: frame interval overridden")
            }
            None => tracing::warn!(us, "VICTORY_FRAME_INTERVAL_US outside 1000..=1000000: ignored"),
        }
        interval
    })
}

/// The interval frames begin at, given the one the title set:
/// [`interval_override`], or the live menus' frame rate cap (`crate::live`,
/// as Corgansm/victory-corgi 482a81c7 has it: frames a second, 30 the
/// title's own interval) once a menu has moved it.
fn capped(title: Duration) -> Duration {
    #[cfg(windows)]
    if let Some(cap) = crate::live::number(crate::live::FRAME_CAP) {
        return if cap > 30.5 { Duration::from_secs_f32(1.0 / cap.clamp(30.0, 240.0)) } else { title };
    }
    interval_override().unwrap_or(title)
}

/// The run's own frame rate cap in frames a second (30: the title's own;
/// `VICTORY_FRAME_INTERVAL_US=1000` shows as 240, the live menus' most),
/// the live menus' start (`crate::live`).
#[cfg(windows)]
pub(crate) fn cap_start() -> f32 {
    interval_override().map_or(30.0, |i| (1.0 / i.as_secs_f32()).clamp(30.0, 240.0))
}

/// `VICTORY_FRAME_PERIOD=1` (off by default; port of xcl-dev/xcl #3146):
/// the period the title sets with its interval (1 to 4) bounds the frames
/// whose token was handed out and that were not presented yet: with that
/// many in flight an origin tick is held, and the next comes a whole
/// interval later. After [`MOST_HELD`] held in a row one goes anyway, so a
/// token the title never presents does not stop the frames.
fn period_switch() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_FRAME_PERIOD").is_some_and(|v| v == "1"))
}

/// The title's period under [`period_switch`] (0: not honoured).
static PERIOD: AtomicU32 = AtomicU32::new(0);
/// The newest token presented ([`presented`]).
static FLIPPED: AtomicU64 = AtomicU64::new(0);
/// Origin ticks held in a row now; the last one held; held in all.
static HELD: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
/// Origins held in a row before one goes anyway.
const MOST_HELD: u64 = 4;

/// The title presented the frame of `token` (`queue.rs` `present`).
pub(crate) fn presented(token: u64) {
    FLIPPED.fetch_max(token, Ordering::Relaxed);
}

/// Whether an origin may go with `issued` tokens handed out, the newest
/// presented `flipped`, `period` allowed in flight (0: always) and `held`
/// origins held in a row so far.
fn origin_allowed(issued: u64, flipped: u64, period: u32, held: u64) -> bool {
    period == 0 || held >= MOST_HELD || issued.saturating_sub(flipped) < u64::from(period)
}

/// One origin tick (`tick`) held for the period: counted once, with a line
/// at the first and at every power of two after.
fn hold(tick: u64, period: u32) {
    if HELD[1].fetch_max(tick, Ordering::Relaxed) < tick {
        HELD[0].fetch_add(1, Ordering::Relaxed);
        let n = HELD[2].fetch_add(1, Ordering::Relaxed) + 1;
        if n.is_power_of_two() {
            tracing::info!(
                held = n,
                period,
                issued = FRAME.load(Ordering::Relaxed),
                flipped = FLIPPED.load(Ordering::Relaxed),
                "VICTORY_FRAME_PERIOD: a frame origin held, that many frames handed out and not presented (xcl #3146)"
            );
        }
    }
}

/// Device method: a frame every `length` microseconds. The period is how
/// many intervals a frame may take to reach the display (pipeline depth),
/// not part of the rate (honoured under [`period_switch`]).
pub(crate) extern "system" fn set_interval(
    _: This,
    _output: This,
    length: u32,
    period: u32,
    _flags: u32,
) -> HRESULT {
    if period_switch() {
        if !(1..=4).contains(&period) {
            tracing::warn!(period, "VICTORY_FRAME_PERIOD: SetFrameIntervalX period not 1 to 4");
            return E_INVALIDARG;
        }
        if PERIOD.swap(period, Ordering::Relaxed) != period {
            tracing::info!(period, "VICTORY_FRAME_PERIOD: at most this many frames handed out and not presented");
        }
    }
    let asked_by_title = match length {
        0 => SIXTIETH,
        us => Duration::from_micros(u64::from(us)),
    };
    let interval = capped(asked_by_title);
    tracing::info!(?interval, title = ?asked_by_title, "frame interval");
    // A new interval starts from now, after the last tick handed out.
    let first = LAST
        .iter()
        .map(|t| t.load(Ordering::Relaxed))
        .max()
        .unwrap_or(0);
    *clock() = Some(Clock {
        start: Instant::now(),
        interval,
        title: asked_by_title,
        first,
    });
    S_OK
}

/// Device method: events are scheduled at every tick.
pub(crate) extern "system" fn schedule(
    _: This,
    _kind: u32,
    _offset: u32,
    _list: *const u8,
    _flags: u32,
) -> HRESULT {
    S_OK
}

/// Frames begun so far (frame waits that ended).
pub(crate) fn current() -> u64 {
    FRAME.load(Ordering::Relaxed)
}

/// For tests of what is done once a frame.
#[cfg(test)]
pub(crate) fn test_advance() {
    FRAME.fetch_add(1, Ordering::Relaxed);
}

/// Per event kind (origin, vblank, flip and later kinds): the last tick a
/// wait for it ended on.
static LAST: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

fn last(kind: u32) -> &'static AtomicU64 {
    &LAST[(kind as usize).min(LAST.len() - 1)]
}

/// The next tick after now and after the last one a wait for `kind` ended
/// on (an event happens once per tick: a wait woken a little early must not
/// let the next one end on the same tick), and how long until it.
fn next_tick(kind: u32) -> (u64, Duration) {
    let mut clock = clock();
    let c = clock.get_or_insert_with(|| Clock {
        start: Instant::now(),
        interval: SIXTIETH,
        title: SIXTIETH,
        first: 0,
    });
    // A cap changed in the live menus: a new interval starts from now, after
    // the last tick handed out, as when the title sets one.
    let wanted = capped(c.title);
    if wanted != c.interval {
        tracing::info!(interval = ?wanted, title = ?c.title, "frame interval: the frame rate cap changed");
        c.first = LAST.iter().map(|t| t.load(Ordering::Relaxed)).max().unwrap_or(0);
        c.start = Instant::now();
        c.interval = wanted;
    }
    let elapsed = c.start.elapsed().as_nanos();
    let interval = c.interval.as_nanos().max(1);
    let tick =
        (c.first + (elapsed / interval + 1) as u64).max(last(kind).load(Ordering::Relaxed) + 1);
    let at = u128::from(tick - c.first) * interval;
    (
        tick,
        Duration::from_nanos(at.saturating_sub(elapsed) as u64),
    )
}

/// Device method: blocks until the next tick (or the time-out); an origin
/// wait then hands out the new frame's token. Other events keep the token
/// the title passes. Ticks are absolute (start plus whole intervals), so
/// lateness never accumulates.
pub(crate) extern "system" fn wait(
    _: This,
    kind: u32,
    timeout_ms: u32,
    _list: *const u8,
    flags: u32,
    token: *mut u64,
) -> HRESULT {
    let origin = kind == ORIGIN;
    let (mut tick, mut wait) = next_tick(kind);
    let limit = Duration::from_millis(u64::from(timeout_ms));
    // A blocking wait for an event that has happened since the last wait
    // for it ended (a frame took longer than a tick) ends at once, on the
    // tick that passed: sleeping on to the next one cost 17 ms of every
    // 730 ms frame (run 58: "2.0 frame waits sleeping 16.8 ms"). Polls (a
    // time-out shorter than a tick) answer as before.
    // `VICTORY_LATE_WAITS=1` sleeps to the next tick, as before.
    static SLEEP: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*SLEEP.get_or_init(|| std::env::var_os("VICTORY_LATE_WAITS").is_some_and(|v| v == "1"))
        && limit >= wait
        && tick > last(kind).load(Ordering::Relaxed) + 1
    {
        tick -= 1;
        wait = Duration::ZERO;
    }
    let _called = crate::timeline::span("the title's frame wait");
    // `VICTORY_FRAME_PERIOD`: with the period's frames not presented the
    // origin tick is held; the wait goes on to the next one if its time-out
    // allows, else it times out.
    let period = PERIOD.load(Ordering::Relaxed);
    let mut held_out = false;
    let mut waited = Duration::ZERO;
    if origin && period != 0 {
        let interval = clock().as_ref().map_or(SIXTIETH, |c| c.interval);
        loop {
            let held = HELD[0].load(Ordering::Relaxed);
            if origin_allowed(FRAME.load(Ordering::Relaxed), FLIPPED.load(Ordering::Relaxed), period, held) {
                if held >= MOST_HELD {
                    static N: AtomicU64 = AtomicU64::new(0);
                    if N.fetch_add(1, Ordering::Relaxed) < 8 {
                        tracing::warn!(period, held, "VICTORY_FRAME_PERIOD: an origin goes after that many held with the period's frames not presented");
                    }
                }
                HELD[0].store(0, Ordering::Relaxed);
                break;
            }
            hold(tick, period);
            if waited + wait + interval > limit {
                held_out = true;
                break;
            }
            std::thread::sleep(wait);
            last(kind).fetch_max(tick, Ordering::Relaxed);
            waited += wait;
            tick += 1;
            wait = interval;
        }
    }
    let hr = if held_out {
        std::thread::sleep(limit.saturating_sub(waited));
        TIMEOUT
    } else if wait > limit {
        std::thread::sleep(limit);
        TIMEOUT
    } else {
        std::thread::sleep(wait);
        last(kind).fetch_max(tick, Ordering::Relaxed);
        S_OK
    };
    #[cfg(all(windows, feature = "diagnostics"))]
    {
        use crate::copystat::{C, add};
        add(C::PaceWaits, 1);
        add(C::PaceUs, wait.min(limit).as_micros() as u64);
    }
    // A frame begun on a time-out takes no tick: its flip still does.
    if origin && (hr == S_OK || flags & TOKEN_ON_TIMEOUT != 0) {
        // Tokens count frames (consecutive), not ticks.
        let frame = FRAME.fetch_add(1, Ordering::Relaxed) + 1;
        if !token.is_null() {
            // SAFETY: checked non-null; the title passes its token.
            unsafe { token.write(frame) };
        }
    }
    #[cfg(feature = "diagnostics")]
    tracing::debug!(
        kind,
        timeout_ms,
        flags,
        hr,
        tick,
        wait_us = wait.as_micros() as u64,
        frame = current(),
        "frame wait"
    );
    hr
}

/// Device method: no statistics are kept, so none are returned.
pub(crate) extern "system" fn statistics(
    _: This,
    _token: u64,
    _kinds: u32,
    count: *mut u32,
    _out: *mut u8,
) -> HRESULT {
    if !count.is_null() {
        // SAFETY: checked non-null.
        unsafe { count.write(0) };
    }
    S_OK
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `VICTORY_FRAME_PERIOD` (xcl #3146): an origin waits while the
    /// period's newest frames are not presented, and goes after
    /// `MOST_HELD` held in a row.
    #[test]
    fn the_period_bounds_frames_not_presented() {
        assert!(origin_allowed(1, 0, 2, 0), "one in flight of two");
        assert!(!origin_allowed(2, 0, 2, 0), "two in flight: held");
        assert!(origin_allowed(2, 1, 2, 0), "frame 1 presented");
        assert!(!origin_allowed(3, 1, 2, 0), "2 and 3 in flight");
        assert!(!origin_allowed(5, 4, 1, 0), "period 1: origin 6 waits for 5");
        assert!(origin_allowed(5, 5, 1, 0));
        assert!(origin_allowed(2, 0, 2, MOST_HELD), "held long enough");
        assert!(origin_allowed(9, 0, 0, 0), "not honoured");
    }

    #[test]
    fn waits_end_on_ticks_and_count_frames() {
        let null = std::ptr::null();
        assert_eq!(set_interval(null, null, 2_000, 1, 0), S_OK);
        let mut first = 0;
        assert_eq!(wait(null, 0, 1_000, std::ptr::null(), 0, &mut first), S_OK);
        let before = Instant::now();
        let mut second = 0;
        assert_eq!(wait(null, 0, 1_000, std::ptr::null(), 0, &mut second), S_OK);
        assert!(second > first);
        // At the tick, not at the time-out (a busy PC may wake it late).
        assert!(before.elapsed() <= Duration::from_millis(500));
        assert_eq!(wait(null, 0, 0, std::ptr::null(), 0, &mut second), TIMEOUT);
        // Tokens count frames, a frame started early on a time-out too.
        let mut early = 0;
        let flags = TOKEN_ON_TIMEOUT;
        assert_eq!(
            wait(null, 0, 0, std::ptr::null(), flags, &mut early),
            TIMEOUT
        );
        let mut next = 0;
        assert_eq!(wait(null, 0, 1_000, std::ptr::null(), 0, &mut next), S_OK);
        assert!(early == second + 1 && next == early + 1);
        assert!(LAST[0].load(Ordering::Relaxed) >= 2);

        // The title's loop (the clock is global, so in the same test): an
        // origin poll that times out (and starts the frame), then a
        // blocking flip wait. Each flip takes its own tick.
        const FLIP: u32 = 2;
        // (The ticks are far apart, and it may take a few tries: a busy PC
        // wakes a wait tens of milliseconds late at times.)
        assert_eq!(set_interval(null, null, 60_000, 3, 0), S_OK);
        let mut token = 0;
        let mut four = || -> Vec<u64> {
            (0..4)
                .map(|_| {
                    let _ = wait(null, 0, 0, std::ptr::null(), TOKEN_ON_TIMEOUT, &mut token);
                    assert_eq!(
                        wait(null, FLIP, u32::MAX, std::ptr::null(), 0, &mut token),
                        S_OK
                    );
                    LAST[2].load(Ordering::Relaxed)
                })
                .collect()
        };
        let mut flips = four();
        for _ in 0..10 {
            if flips.windows(2).all(|w| w[1] == w[0] + 1) {
                break;
            }
            flips = four();
        }
        assert!(flips.windows(2).all(|w| w[1] == w[0] + 1), "{flips:?}");
        // Even when a flip wait begins on the tick another just ended on.
        let (tick, _) = next_tick(FLIP);
        assert!(tick > flips[3]);
    }
}
