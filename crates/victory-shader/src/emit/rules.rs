//! Small correctness rules of the translator, each with its own switch
//! (read once; part of [`super::translation_fingerprint`], since every
//! `"VICTORY_*"` name in this crate's source joins the shader cache key) and
//! its own counters, which the GPU layer logs ([`counts`]).
//!
//! - `VICTORY_SHADER_SEXT64_LITERAL` (on unless `0`): a 32-bit literal read
//!   as a *signed* 64-bit integer source is sign-extended; unsigned (u64,
//!   b64) sources still zero-extend. AMD's "RDNA 2" ISA, §6.2.1 "Literal
//!   Expansion to 64 bits": "64-bit unsigned integer: zero extended to 64
//!   bits; 64-bit signed integer: sign extended to 64 bits". LLVM agrees
//!   (AMDGPUOperandSyntax, "Conversion of Integer Values": i64 gives
//!   `{-1,num.i32}`, u64/b64 `{0,num.u32}`, e.g. `s_bfe_i64 s[0:1],
//!   0xffefffff, s3` reads 0xffffffffffefffff; and SIInstrInfo's
//!   isOperandLegal: a negative 32-bit literal on a 64-bit operand is
//!   "either correctly sign extended or incorrectly zero extended by HW").
//!   Before this the translator zero-extended every literal. Idea from
//!   ubiquitarianism/leonida #1493 (vector half) and #978 (scalar half).
//! - `VICTORY_SHADER_LDS_WRITE2_ONE` (on unless `0`): `ds_write2*` with
//!   equal offsets writes DATA0 only. RDNA 2 ISA §10.4 (LDS access, double
//!   address instructions): "Specify only one address by setting both offsets
//!   to the same value. This causes only one read or write to occur and uses
//!   only the first DATA0." Before this the translator stored DATA0 then
//!   DATA1 at the one address, so DATA1 won. Idea from
//!   ubiquitarianism/leonida #966.
//!
//! Both are default because the ISA manual states the rule; `=0` gives back
//! the old translation, for comparison.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// Whether the signed-literal rule is on (`VICTORY_SHADER_SEXT64_LITERAL`).
pub(super) fn sext64_literal_on() -> bool {
    #[cfg(test)]
    if let Some(on) = TEST_SEXT64.get() {
        return on;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| parse_on(std::env::var("VICTORY_SHADER_SEXT64_LITERAL").ok().as_deref()))
}

/// Whether the one-write rule is on (`VICTORY_SHADER_LDS_WRITE2_ONE`).
pub(super) fn lds_write2_one_on() -> bool {
    #[cfg(test)]
    if let Some(on) = TEST_WRITE2_ONE.get() {
        return on;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| parse_on(std::env::var("VICTORY_SHADER_LDS_WRITE2_ONE").ok().as_deref()))
}

/// A default-on switch's value: off only for `0` (or `off`/`false`).
fn parse_on(value: Option<&str>) -> bool {
    !matches!(value.map(str::trim), Some("0" | "off" | "false"))
}

#[cfg(test)]
thread_local! {
    /// A test's choice for [`sext64_literal_on`], for its thread only.
    pub(super) static TEST_SEXT64: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
    /// A test's choice for [`lds_write2_one_on`], for its thread only.
    pub(super) static TEST_WRITE2_ONE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Whether 64-bit source `k` of instruction `name` is a signed integer
/// (LLVM's operand type i64): the sources a literal is sign-extended for.
pub(super) fn signed_src64(name: &str, k: usize) -> bool {
    match name {
        "v_ashrrev_i64" => k == 1,
        "v_mad_i64_i32" => k == 2,
        "s_ashr_i64" | "s_bfe_i64" | "s_flbit_i32_i64" => k == 0,
        _ => {
            (name.starts_with("v_cmp_") || name.starts_with("v_cmpx_"))
                && name.ends_with("_i64")
                && k < 2
        }
    }
}

/// The high dword a 32-bit literal `lit` gets as 64-bit integer source `k`
/// of `name`, counted ([`counts`]) when the rule changes it.
pub(super) fn literal_high(name: &str, k: usize, lit: u32) -> u32 {
    if !signed_src64(name, k) || (lit as i32) >= 0 {
        return 0;
    }
    COUNTS[0].fetch_add(1, Ordering::Relaxed);
    if sext64_literal_on() {
        COUNTS[1].fetch_add(1, Ordering::Relaxed);
        u32::MAX
    } else {
        0
    }
}

/// Whether a paired LDS write of offsets `offset0` and `offset1` stores
/// DATA0 only, counted ([`counts`]).
pub(super) fn write2_one(offset0: u8, offset1: u8) -> bool {
    if offset0 != offset1 {
        return false;
    }
    COUNTS[2].fetch_add(1, Ordering::Relaxed);
    let on = lds_write2_one_on();
    if on {
        COUNTS[3].fetch_add(1, Ordering::Relaxed);
    }
    on
}

/// Instructions translated since the process started: negative literals
/// read as signed 64-bit sources, of those sign-extended; paired LDS writes
/// at one address, of those that store DATA0 only. Translations only: a
/// shader taken from the cache is not translated again.
static COUNTS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

/// [`COUNTS`]: `[signed literals seen, sign-extended, equal write2 seen,
/// DATA0 only]`.
pub fn counts() -> [u64; 4] {
    COUNTS.each_ref().map(|c| c.load(Ordering::Relaxed))
}

/// The rules and whether each is on, for one log line at start.
pub fn describe() -> String {
    format!(
        "VICTORY_SHADER_SEXT64_LITERAL={} VICTORY_SHADER_LDS_WRITE2_ONE={}",
        u8::from(sext64_literal_on()),
        u8::from(lds_write2_one_on())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switches_are_on_unless_zero() {
        assert!(parse_on(None));
        assert!(parse_on(Some("1")));
        assert!(parse_on(Some("")));
        assert!(!parse_on(Some("0")));
        assert!(!parse_on(Some(" off ")));
    }

    #[test]
    fn both_switches_join_the_shader_cache_key() {
        let keyed: Vec<&str> = env!("VICTORY_SHADER_SWITCHES").split(',').collect();
        for name in ["VICTORY_SHADER_SEXT64_LITERAL", "VICTORY_SHADER_LDS_WRITE2_ONE"] {
            assert!(keyed.contains(&name), "{name} is not in translation_fingerprint");
        }
    }

    #[test]
    fn signed_sources_are_the_i64_operands() {
        for name in ["v_cmp_lt_i64", "v_cmpx_ne_i64", "v_cmp_eq_i64"] {
            assert!(signed_src64(name, 0) && signed_src64(name, 1), "{name}");
        }
        assert!(signed_src64("v_ashrrev_i64", 1));
        assert!(!signed_src64("v_ashrrev_i64", 0), "the shift count is 32 bits");
        assert!(signed_src64("v_mad_i64_i32", 2));
        assert!(signed_src64("s_ashr_i64", 0) && !signed_src64("s_ashr_i64", 1));
        assert!(signed_src64("s_bfe_i64", 0) && signed_src64("s_flbit_i32_i64", 0));
        for name in [
            "v_cmp_lt_u64",
            "v_lshrrev_b64",
            "v_lshlrev_b64",
            "v_mad_u64_u32",
            "s_lshr_b64",
            "s_bfe_u64",
            "s_cmp_eq_u64",
            "v_cmp_lt_f64",
        ] {
            assert!(!(0..3).any(|k| signed_src64(name, k)), "{name}");
        }
    }

    #[test]
    fn negative_literals_of_signed_sources_get_ones_above() {
        TEST_SEXT64.set(Some(true));
        assert_eq!(literal_high("v_cmp_lt_i64", 1, 0xffff_ff9c), u32::MAX);
        assert_eq!(literal_high("s_ashr_i64", 0, 0x8000_0000), u32::MAX);
        assert_eq!(literal_high("v_cmp_lt_i64", 1, 0x7fff_ffff), 0, "positive");
        assert_eq!(literal_high("v_cmp_lt_u64", 1, 0xffff_ff9c), 0, "unsigned");
        TEST_SEXT64.set(Some(false));
        assert_eq!(literal_high("v_cmp_lt_i64", 1, 0xffff_ff9c), 0, "rule off");
        TEST_SEXT64.set(None);
    }

    #[test]
    fn equal_offsets_write_once() {
        TEST_WRITE2_ONE.set(Some(true));
        assert!(write2_one(3, 3));
        assert!(!write2_one(3, 4));
        TEST_WRITE2_ONE.set(Some(false));
        assert!(!write2_one(0, 0));
        TEST_WRITE2_ONE.set(None);
    }
}
