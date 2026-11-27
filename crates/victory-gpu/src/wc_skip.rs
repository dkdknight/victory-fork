//! Shader writes to copies of write-combining title memory left on the card,
//! except small ranges (off by default; an experiment).
//!
//! Port of kylepelham/El1xr 4d0660454 (`wc_skip.rs`, live), itself from
//! Peppacodes/playablegames-peppa 6035addd51, 3cc0a8d63e and ca2432bd64,
//! originally fxceless/victory-fork #868 and #1379. An earlier port of ours
//! (799d2819, Chipsrain, branch integ/fps-15) had no small-range keep.
//!
//! - `VICTORY_SKIP_WC_BACK=1`: what shaders wrote in a declared range that
//!   lies wholly in one `XMemVirtualAlloc` reservation the title asked for
//!   with `PAGE_WRITECOMBINE` (0x400) is not fetched back into title memory
//!   (the console's CPU does not read such memory back either). The card
//!   still compares the range.
//! - `VICTORY_SKIP_WC_COMPARE=1` (implies the above): such ranges are not
//!   hashed and compared on the card at all.
//! - `VICTORY_SKIP_WC_KEEP=<bytes>` (1 MiB by default): declared ranges of
//!   at most this many bytes are fetched back all the same (small results
//!   the CPU reads, such as exposure and occlusion).
//!
//! Both switches go by `crate::ab::pick` too (`VICTORY_AB=30,VICTORY_SKIP_WC_BACK`):
//! reservations are recorded whether they are on or not.
//!
//! On an RTX 5080 (Chipsrain, 799d2819) about 100 MB a frame was fetched
//! back, 97 to 99 % of it from write-combining memory. Expect wrong bytes
//! where the CPU writes part of a piece the card wrote, or reads what the
//! card wrote in a range over the keep.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

/// The console's write-combining protection bit, as the title asked for it
/// (before `HOST_PROTECTION` masking).
const PAGE_WRITECOMBINE: u32 = 0x400;

/// `VICTORY_SKIP_WC_KEEP`'s default: 1 MiB (fxceless #1379).
const DEFAULT_KEEP_BYTES: usize = 1 << 20;

/// A reservation of `XMemVirtualAlloc`, as the title asked for it.
struct Reservation {
    len: usize,
    protect: u32,
}

/// The reservations, by their start.
static RESERVATIONS: Mutex<BTreeMap<usize, Reservation>> = Mutex::new(BTreeMap::new());

/// Ranges left on the card: count and bytes.
static SKIPPED: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];

/// Whether `VICTORY_SKIP_WC_BACK=1` (or `VICTORY_SKIP_WC_COMPARE=1`) is on now.
pub(crate) fn skip_back_on() -> bool {
    crate::ab::pick("VICTORY_SKIP_WC_BACK").unwrap_or_else(|| launched("VICTORY_SKIP_WC_BACK")) || skip_compare_on()
}

/// Whether `VICTORY_SKIP_WC_COMPARE=1` is on now.
pub(crate) fn skip_compare_on() -> bool {
    if let Some(on) = crate::ab::pick("VICTORY_SKIP_WC_COMPARE") {
        return on;
    }
    launched("VICTORY_SKIP_WC_COMPARE")
}

/// Whether switch `name` (one of the two) was set at start; logged once.
fn launched(name: &'static str) -> bool {
    static BACK: OnceLock<bool> = OnceLock::new();
    static COMPARE: OnceLock<bool> = OnceLock::new();
    let cell = if name == "VICTORY_SKIP_WC_BACK" { &BACK } else { &COMPARE };
    *cell.get_or_init(|| {
        let on = crate::switch::on(name);
        if on {
            tracing::warn!(
                keep_bytes = keep_bytes(),
                "{name}=1: shader writes to write-combining title memory in declared ranges over \
                 VICTORY_SKIP_WC_KEEP bytes are not fetched back (experiment)"
            );
        }
        on
    })
}

fn parse_keep(raw: Option<&str>) -> usize {
    raw.and_then(|v| v.trim().parse().ok()).unwrap_or(DEFAULT_KEEP_BYTES)
}

/// `VICTORY_SKIP_WC_KEEP=<bytes>`: declared ranges of at most this many
/// bytes are fetched back even in write-combining memory.
fn keep_bytes() -> usize {
    static KEEP: OnceLock<usize> = OnceLock::new();
    *KEEP.get_or_init(|| parse_keep(std::env::var("VICTORY_SKIP_WC_KEEP").ok().as_deref()))
}

/// `XMemVirtualAlloc` reserved `[at, at + len)` with the title's own console
/// `protect` flags. Recorded with the switches off too, for a live change.
pub fn title_memory_reserved(at: usize, len: usize, protect: u32) {
    if at == 0 || len == 0 {
        return;
    }
    let mut reservations = RESERVATIONS.lock().unwrap_or_else(|e| e.into_inner());
    insert_reservation(&mut reservations, at, len, protect);
}

fn insert_reservation(reservations: &mut BTreeMap<usize, Reservation>, at: usize, len: usize, protect: u32) {
    let Some(end) = at.checked_add(len) else {
        return;
    };
    // A stale reservation that overlaps the new one (its memory was freed
    // and the addresses given again) is forgotten.
    let stale: Vec<usize> = reservations
        .range(..end)
        .rev()
        .take_while(|&(&base, r)| base.saturating_add(r.len) > at)
        .map(|(&base, _)| base)
        .collect();
    for base in stale {
        reservations.remove(&base);
    }
    reservations.insert(at, Reservation { len, protect });
}

/// Whether `[from, to)` lies wholly in one write-combining reservation.
fn wc_in(reservations: &BTreeMap<usize, Reservation>, (from, to): (usize, usize)) -> bool {
    // Seeking from the exclusive end also finds a reservation that begins
    // inside the range. One that ends at `to` holds it.
    from < to
        && reservations.range(..to).next_back().is_some_and(|(&base, r)| {
            base <= from && r.protect & PAGE_WRITECOMBINE != 0 && base.checked_add(r.len).is_some_and(|end| to <= end)
        })
}

fn should_skip_in(reservations: &BTreeMap<usize, Reservation>, declared: (usize, usize), keep: usize) -> bool {
    declared.1.saturating_sub(declared.0) > keep && wc_in(reservations, declared)
}

/// Whether what shaders wrote in the declared title range `declared` stays
/// on the card (not fetched back), with the switches as they are now.
/// Counted when it does.
pub(crate) fn skip_back(declared: (usize, usize)) -> bool {
    if !skip_back_on() {
        return false;
    }
    let skip = {
        let reservations = RESERVATIONS.lock().unwrap_or_else(|e| e.into_inner());
        should_skip_in(&reservations, declared, keep_bytes())
    };
    if skip {
        let bytes = declared.1.saturating_sub(declared.0) as u64;
        let n = SKIPPED[0].fetch_add(1, Relaxed) + 1;
        let b = SKIPPED[1].fetch_add(bytes, Relaxed) + bytes;
        if n.is_power_of_two() {
            tracing::info!(
                "VICTORY_SKIP_WC_BACK: {n} declared ranges ({} MB) of write-combining memory left on the card so far",
                b >> 20
            );
        }
    }
    skip
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: usize = 1 << 20;

    fn reservation(len: usize, protect: u32) -> Reservation {
        Reservation { len, protect }
    }

    #[test]
    fn a_write_in_the_ordinary_half_of_a_window_still_comes_back() {
        let window = 8 * MIB;
        let middle = window + 2 * MIB;
        let map = BTreeMap::from([(window, reservation(2 * MIB, 0x404)), (middle, reservation(2 * MIB, 0x04))]);
        assert!(wc_in(&map, (middle - 65536, middle)));
        assert!(!wc_in(&map, (middle, middle + 65536)));
        assert!(!wc_in(&map, (middle - 65536, middle + 65536)));
    }

    #[test]
    fn a_range_must_fit_one_known_reservation() {
        let map = BTreeMap::from([
            (0x10000, reservation(0x10000, 0x404)),
            (0x20000, reservation(0x10000, 0x404)),
            (usize::MAX - 8, reservation(16, 0x404)),
        ]);
        assert!(wc_in(&map, (0x10000, 0x20000)));
        assert!(wc_in(&map, (0x20000, 0x30000)));
        assert!(!wc_in(&map, (0x1ffff, 0x20001)));
        assert!(!wc_in(&map, (0x30000, 0x30001)));
        assert!(!wc_in(&map, (0x10000, 0x10000)));
        assert!(!wc_in(&map, (usize::MAX - 8, 1)));
        assert!(!wc_in(&map, (usize::MAX - 8, usize::MAX)));
    }

    #[test]
    fn ranges_up_to_the_keep_still_come_back() {
        let base = 16 * MIB;
        let map = BTreeMap::from([(base, reservation(8 * MIB, 0x404)), (base + 8 * MIB, reservation(8 * MIB, 0x04))]);
        assert!(!should_skip_in(&map, (base, base + 65536), DEFAULT_KEEP_BYTES));
        assert!(!should_skip_in(&map, (base, base + MIB), DEFAULT_KEEP_BYTES));
        assert!(should_skip_in(&map, (base, base + MIB + 1), DEFAULT_KEEP_BYTES));
        assert!(should_skip_in(&map, (base, base + 4 * MIB), DEFAULT_KEEP_BYTES));
        assert!(!should_skip_in(&map, (base + 8 * MIB, base + 12 * MIB), DEFAULT_KEEP_BYTES));
        assert!(should_skip_in(&map, (base, base + 4096), 0));
    }

    #[test]
    fn a_new_reservation_forgets_an_overlapping_one() {
        let base = 32 * MIB;
        let mut map = BTreeMap::new();
        insert_reservation(&mut map, base, 4 * MIB, 0x404);
        assert!(should_skip_in(&map, (base, base + 2 * MIB), DEFAULT_KEEP_BYTES));
        insert_reservation(&mut map, base, 4 * MIB, 0x04);
        assert!(!should_skip_in(&map, (base, base + 2 * MIB), DEFAULT_KEEP_BYTES));
    }

    #[test]
    fn the_keep_is_one_mib_unless_set() {
        assert_eq!(parse_keep(None), 1_048_576);
        assert_eq!(parse_keep(Some("0")), 0);
        assert_eq!(parse_keep(Some(" 2097152 ")), 2_097_152);
        assert_eq!(parse_keep(Some("invalid")), 1_048_576);
    }
}
