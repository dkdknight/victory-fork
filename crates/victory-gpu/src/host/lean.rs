//! A small flush of refreshes done on the submitting thread before any kept
//! thread is woken: two settings, both off by default, and their counters.
//!
//! Ported by hand from Kemerd/VIctory-NVIDIA-RT (`host/lean.rs`, 42c41ef and
//! 4749cd7, by Kemerd / D Everett Hinton): only the probe and its textures
//! part, not the declarations across lists nor the step counters.
//!
//! One thread prepares every submission (`queue::behind`), so what it does
//! between a list closing and the lists going to the card is the frame. In
//! the city upstream measured about 70 flushes a frame and 8,000 ranges, of
//! which 6,800 copy nothing: most flushes are a few dozen refreshes, and
//! handing those to the kept threads costs a wake for each and a wait at the
//! end whether or not they had anything to do.
//!
//! - [`spread`] (`VICTORY_REFRESH_PROBE_US`): a small flush is tried here first.
//! - [`textures_small`] (`VICTORY_REFRESH_PROBE_TEXTURES=1`): a flush with only
//!   light textures due still counts as small.
//!
//! Both can be changed while the title runs (`crate::ab`): see [`probe`] and
//! [`probe_textures`].

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// The probe `VICTORY_AB` turns on when the run's own setting has none.
const AB_PROBE: Duration = Duration::from_micros(300);

/// How long a small flush of refreshes is worked on this thread alone before
/// the rest goes to the kept threads (`VICTORY_REFRESH_PROBE_US=<microseconds>`,
/// 1 to 5000; unset, 0 or anything else: every flush goes to the threads at
/// once, as before).
///
/// With the probe a flush goes through its refreshes one after the other here
/// until the probe has run out and only then hands the rest over, so a small
/// flush is done without any wake and one that is not loses at most the probe
/// and the one refresh it was in. Where the bytes go does not change: title
/// memory is read into the copy by the same refresh, once; only which thread
/// calls it does. Flushes with textures due (long, hashed whole, unless
/// [`probe_textures`]) and the flush of the heap's thousands of copies go to
/// the threads at once, as before.
///
/// `VICTORY_LIVE_ALT=VICTORY_REFRESH_PROBE_US=0|300,<seconds>` alternates
/// two probes; `VICTORY_AB=<seconds>,VICTORY_REFRESH_PROBE_US` alternates off
/// and the run's own probe (300 µs without one).
fn probe() -> Option<Duration> {
    static PROBE: OnceLock<Option<Duration>> = OnceLock::new();
    let own = *PROBE.get_or_init(|| probe_of(std::env::var("VICTORY_REFRESH_PROBE_US").ok().as_deref()));
    match crate::ab::value("VICTORY_REFRESH_PROBE_US") {
        None => own,
        Some(v) => probe_ab(v, own),
    }
}

/// [`probe`] for a value of `crate::ab`: 0 or less off, 1 (a switch of
/// `VICTORY_AB` turned on) the run's own probe or [`AB_PROBE`], more than
/// that so many microseconds.
fn probe_ab(value: f32, own: Option<Duration>) -> Option<Duration> {
    if value.is_nan() || value <= 0.0 {
        return None;
    }
    if value < 1.5 {
        return Some(own.unwrap_or(AB_PROBE));
    }
    Some(Duration::from_micros((value as u64).min(5_000)))
}

/// [`probe`] for a setting.
fn probe_of(setting: Option<&str>) -> Option<Duration> {
    let us: u64 = setting?.trim().parse().ok()?;
    // A probe past a few milliseconds is a flush done here whatever it is.
    (us > 0).then(|| Duration::from_micros(us.min(5_000)))
}

/// Since the last frame-rate line: flushes that were done on this thread
/// alone, those that went on to the threads after the probe, the nanoseconds
/// spent in these calls, and the refreshes done here.
static PROBED: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

/// Bytes of title memory a due texture may read and still count as light for
/// [`textures_small`].
const SMALL_TEXTURE: usize = 256 << 10;

/// Whether a flush with only light textures due still counts as small for
/// [`spread`] (`VICTORY_REFRESH_PROBE_TEXTURES=1`, or what `VICTORY_AB` /
/// `VICTORY_LIVE_ALT` make of it now; off by default: a flush with any texture
/// due goes to the threads at once, as before). Only meaningful with
/// [`probe`] set, so it is off without it.
///
/// A wake and a barrier for work of a few dozen microseconds is what the probe
/// is for, and a texture that reads under 256 KiB, or was read ahead already,
/// costs about as little as a few copies. The probe still bounds the time on
/// this thread (its microseconds, plus the one refresh in progress). A texture
/// refresh that blocks on another thread's hold of the same texture could run
/// past the probe, so a texture another thread holds is not light
/// (`crate::texture::Staging::cheap`).
fn probe_textures() -> bool {
    if probe().is_none() {
        return false;
    }
    if let Some(on) = crate::ab::pick("VICTORY_REFRESH_PROBE_TEXTURES") {
        return on;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| crate::switch::on("VICTORY_REFRESH_PROBE_TEXTURES"))
}

/// Since the last frame-rate line: flushes with textures due that were taken
/// as small only because the textures were light.
static TEXTURES_PROBED: AtomicU64 = AtomicU64::new(0);

/// Whether every texture in `due` is light enough ([`SMALL_TEXTURE`]) for the
/// flush to be tried on this thread first. False with the switch off, so the
/// caller's own test stands as it was. Counts a yes for a non-empty `due` when
/// with the `spans` the flush has two or more pieces: with fewer it never went
/// to the threads, so the textures did not change where it ran.
pub(super) fn textures_small(due: &[std::sync::Arc<crate::texture::Staging>], spans: usize) -> bool {
    if !probe_textures() || !due.iter().all(|t| t.cheap(SMALL_TEXTURE)) {
        return false;
    }
    if !due.is_empty() && spans + due.len() >= 2 {
        TEXTURES_PROBED.fetch_add(1, Ordering::Relaxed);
    }
    true
}

/// What [`spread_with`] did.
#[derive(Debug, PartialEq, Eq)]
struct Spread {
    /// How many of the `n` were done on this thread before the rest left it.
    here: usize,
    /// Whether the rest went to the kept threads.
    went: bool,
}

/// Calls `work` with every number below `n`, on this thread while the clock
/// says less than `probe` has passed since the first, and with what is left on
/// the kept threads and this one (`crate::shadow::wide_each`).
///
/// With no `probe`, or `small` false, or fewer than two to do: all of it goes
/// to `wide_each` as it did.
fn spread_with(n: usize, small: bool, probe: Option<Duration>, work: impl Fn(usize) + Send + Sync + 'static) -> Spread {
    let Some(probe) = probe.filter(|_| small && n >= 2) else {
        crate::shadow::wide_each(n, work);
        return Spread { here: 0, went: true };
    };
    let started = std::time::Instant::now();
    let here = serial_first(n, probe, || started.elapsed(), &work);
    if here >= n {
        return Spread { here, went: false };
    }
    // What the thread did not get to: numbers from `here` on.
    crate::shadow::wide_each(n - here, move |i| work(here + i));
    Spread { here, went: true }
}

/// Calls `work` with 0, 1, ... while `elapsed` says the probe has not run out;
/// at least once. How many it called, `n` if it called all.
fn serial_first(n: usize, probe: Duration, mut elapsed: impl FnMut() -> Duration, work: &impl Fn(usize)) -> usize {
    let mut i = 0;
    while i < n {
        work(i);
        i += 1;
        if i < n && elapsed() >= probe {
            break;
        }
    }
    i
}

/// [`spread_with`] under [`probe`], counted. `small`: the caller's flush is one
/// the probe is for (few spans, and no textures due, or with
/// `VICTORY_REFRESH_PROBE_TEXTURES=1` only light ones: [`textures_small`]).
/// With the probe off it is `crate::shadow::wide_each`, nothing more.
pub(super) fn spread(n: usize, small: bool, work: impl Fn(usize) + Send + Sync + 'static) {
    let Some(probe) = probe().filter(|_| small && n >= 2) else {
        crate::shadow::wide_each(n, work);
        return;
    };
    let started = std::time::Instant::now();
    let done = spread_with(n, small, Some(probe), work);
    // (The time is the whole call: the part on this thread is what the probe
    // bounds, and when the rest went out the call is longer. The line says so.)
    let k = usize::from(done.went);
    PROBED[k].fetch_add(1, Ordering::Relaxed);
    PROBED[2].fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
    PROBED[3].fetch_add(done.here as u64, Ordering::Relaxed);
}

/// The counters, per frame, for the frame-rate line; nothing when the probe
/// did not run since the last one.
pub(super) fn line(frames: f64) {
    let frames = frames.max(1.0);
    let p: [f64; 4] = std::array::from_fn(|i| PROBED[i].swap(0, Ordering::Relaxed) as f64 / frames);
    let textures = TEXTURES_PROBED.swap(0, Ordering::Relaxed) as f64 / frames;
    if p[0] + p[1] <= 0.0 {
        return;
    }
    let probe = probe().map_or(0, |p| p.as_micros());
    tracing::info!(
        "small refresh flushes, per frame (VICTORY_REFRESH_PROBE_US={probe}): {:.1} done on the submitting thread alone and {:.1} \
         that went on to the threads, {:.2} ms in all these calls, {:.0} refreshes made there; {textures:.1} taken as small only \
         because their due textures were light (VICTORY_REFRESH_PROBE_TEXTURES)",
        p[0],
        p[1],
        p[2] / 1e6,
        p[3],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    /// A table of how many times each number was called.
    fn tally(n: usize) -> Arc<Vec<AtomicUsize>> {
        Arc::new((0..n).map(|_| AtomicUsize::new(0)).collect())
    }

    #[test]
    fn the_probe_is_a_number_of_microseconds_or_nothing() {
        assert_eq!(probe_of(None), None);
        assert_eq!(probe_of(Some("")), None);
        assert_eq!(probe_of(Some("0")), None);
        assert_eq!(probe_of(Some("fast")), None);
        assert_eq!(probe_of(Some("-5")), None);
        assert_eq!(probe_of(Some(" 150 ")), Some(Duration::from_micros(150)));
        // A probe is bounded.
        assert_eq!(probe_of(Some("90000000")), Some(Duration::from_micros(5_000)));
    }

    #[test]
    fn an_ab_value_is_off_the_runs_own_or_microseconds() {
        let own = Some(Duration::from_micros(150));
        assert_eq!(probe_ab(0.0, own), None);
        assert_eq!(probe_ab(-3.0, own), None);
        assert_eq!(probe_ab(f32::NAN, own), None);
        assert_eq!(probe_ab(1.0, own), own);
        assert_eq!(probe_ab(1.0, None), Some(AB_PROBE));
        assert_eq!(probe_ab(300.0, own), Some(Duration::from_micros(300)));
        assert_eq!(probe_ab(1e9, None), Some(Duration::from_micros(5_000)));
    }

    #[test]
    fn the_thread_works_until_the_probe_has_run_out_and_at_least_once() {
        let called = tally(10);
        let work = {
            let called = called.clone();
            move |i: usize| {
                called[i].fetch_add(1, Ordering::Relaxed);
            }
        };
        // A clock that has run out at once: the first is still done.
        let here = serial_first(10, Duration::from_micros(1), || Duration::from_secs(1), &work);
        assert_eq!(here, 1);
        // A clock that never does.
        let here = serial_first(10, Duration::from_secs(1), || Duration::ZERO, &work);
        assert_eq!(here, 10);
        // One that runs out after three looks.
        let mut looks = 0;
        let here = serial_first(
            10,
            Duration::from_micros(30),
            || {
                looks += 1;
                Duration::from_micros(10 * looks)
            },
            &work,
        );
        assert_eq!(here, 3);
        assert_eq!(called[0].load(Ordering::Relaxed), 3);
        assert_eq!(called[3].load(Ordering::Relaxed), 1);
        assert_eq!(called[9].load(Ordering::Relaxed), 1);
    }

    #[test]
    fn every_number_is_called_once_whoever_does_it() {
        for (n, small, probe) in [
            (0, true, Some(Duration::from_secs(1))),
            (1, true, Some(Duration::from_secs(1))),
            (2, true, None),
            (37, false, Some(Duration::from_secs(1))),
            // All here: the probe is long.
            (37, true, Some(Duration::from_secs(60))),
            // Next to none here: the probe has run out after the first.
            (37, true, Some(Duration::from_nanos(1))),
            (500, true, Some(Duration::from_nanos(1))),
        ] {
            let called = tally(n);
            let work = {
                let called = called.clone();
                move |i: usize| {
                    called[i].fetch_add(1, Ordering::Relaxed);
                }
            };
            let done = spread_with(n, small, probe, work);
            assert!(called.iter().all(|c| c.load(Ordering::Relaxed) == 1), "n {n}, small {small}, probe {probe:?}");
            // Where it went.
            match (n, small, probe) {
                (37, true, Some(p)) if p >= Duration::from_secs(60) => assert_eq!(done, Spread { here: 37, went: false }),
                (37 | 500, true, Some(_)) => assert!(done.went && done.here >= 1),
                _ => assert!(done.went && done.here == 0),
            }
        }
    }
}
