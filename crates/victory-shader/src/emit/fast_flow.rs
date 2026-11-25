//! `VICTORY_SHADER_FAST_FLOW`: control-flow changes to the translation of the
//! title's costliest programs, each exact (the same results as without it), off by
//! default. What the switch turns on, by name (`1` or `all`: everything; else a list
//! separated by commas, for runs that isolate one):
//!
//! - `faults`: an access to title memory by address (`global_*`, `s_load*` on a raw
//!   address) records the page it found no copy of in the faults buffer only when it
//!   found none, behind a branch. Before, every access wrote a dword there (the sink,
//!   dword 0, when the page had a copy), branch-free; that buffer is in the PC's memory
//!   (`victory-gpu`'s `memory.rs`), so each access of each wave was a write across the
//!   bus. 36f04a4ed0ad1d9b has 111 such accesses, 559d9f0184e15fe3 13 in its hot loop.
//! - `parked`: the lanes of such an access that EXEC leaves out (a guarded instruction)
//!   look up the directory's last entry, which is never filled, and read the null view:
//!   nothing is fetched for them (before, they looked up and read whatever their stale
//!   address named). Their result was already dropped by the guard.
//! - `paired`: a console wave64 compute program on a host of 32-lane waves whose
//!   translation runs each console wave as two host waves meeting at group barriers
//!   (exchanges of lane reads, masks read as data, LDS accesses) takes the paired form
//!   instead where it translates (fxceless PR #854's form: one host wave per console wave,
//!   two console lanes a thread, no barrier but the entry's). Both forms are exact; the
//!   barriers stop every wave of the group until the slowest arrives, which with the
//!   card's ~1,200-cycle loads is most of such a program's time.
//!
//! Part of the translation: the shader cache keys on the switch's value (the build script
//! lists it in `translation_fingerprint`).

use std::sync::atomic::{AtomicU64, Ordering};

/// The parts of the switch, as bits.
pub const FAULTS: u32 = 1;
pub const PARKED: u32 = 2;
pub const PAIRED: u32 = 4;
pub const ALL: u32 = FAULTS | PARKED | PAIRED;

/// The switch's value parsed: unset, empty or `0` nothing; `1` or `all` everything;
/// otherwise the named parts (unknown names are left out). Pure, for testing.
pub fn parse(value: Option<&str>) -> u32 {
    let Some(v) = value.map(str::trim) else {
        return 0;
    };
    match v {
        "" | "0" => 0,
        "1" | "all" => ALL,
        list => list
            .split(',')
            .map(|n| match n.trim() {
                "faults" => FAULTS,
                "parked" => PARKED,
                "paired" => PAIRED,
                _ => 0,
            })
            .fold(0, |a, b| a | b),
    }
}

/// The parts on in this process (`VICTORY_SHADER_FAST_FLOW`), or a test's choice for its
/// own thread.
pub fn parts() -> u32 {
    #[cfg(test)]
    if let Some(p) = TEST_PARTS.get() {
        return p;
    }
    static PARTS: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *PARTS.get_or_init(|| parse(std::env::var("VICTORY_SHADER_FAST_FLOW").ok().as_deref()))
}

/// Whether `part` is on.
pub fn on(part: u32) -> bool {
    parts() & part != 0
}

#[cfg(test)]
thread_local! {
    /// A test's choice for [`parts`], for its thread only.
    pub(crate) static TEST_PARTS: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

/// Runs `f` with `parts` on for this thread (tests).
#[cfg(test)]
pub(crate) fn with_parts<R>(parts: u32, f: impl FnOnce() -> R) -> R {
    let was = TEST_PARTS.replace(Some(parts));
    let r = f();
    TEST_PARTS.set(was);
    r
}

/// What the switch changed in the programs translated by this process (not those taken
/// from the shader cache, which were translated the same way when they were made): accesses
/// whose fault record is behind a branch, guarded accesses parked, programs in the paired
/// form instead of two host waves that meet, and the group barriers those would have had.
static COUNTS: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];

pub(crate) fn count(what: usize, n: u64) {
    COUNTS[what].fetch_add(n, Ordering::Relaxed);
}

pub(crate) const PROGRAMS: usize = 0;
pub(crate) const BRANCHED: usize = 1;
pub(crate) const PARKED_N: usize = 2;
pub(crate) const PAIRED_N: usize = 3;
pub(crate) const MEETINGS_N: usize = 4;

/// The counts so far: (programs translated with the switch on, accesses whose fault record
/// is branched, guarded accesses parked, wave64 programs moved to the paired form, the group
/// barriers those programs would have had).
pub fn counts() -> [u64; 5] {
    std::array::from_fn(|i| COUNTS[i].load(Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_switch_parses_names_and_all() {
        assert_eq!(parse(None), 0);
        assert_eq!(parse(Some("")), 0);
        assert_eq!(parse(Some("0")), 0);
        assert_eq!(parse(Some("1")), ALL);
        assert_eq!(parse(Some(" all ")), ALL);
        assert_eq!(parse(Some("faults")), FAULTS);
        assert_eq!(parse(Some("parked, paired")), PARKED | PAIRED);
        assert_eq!(parse(Some("bogus")), 0);
        assert_eq!(parse(Some("faults,bogus")), FAULTS);
    }
}
