//! `VICTORY_EXCHANGE_SITES=1` (off by default; read at translation time, so
//! its value joins the shader cache key): each static place where the two
//! host waves of a console wave64 exchange through LDS (a lane mask's
//! ballot, a reduction's halves, a lane read across the halves) gets dwords
//! of its own, an LDS array `wave_exchange_<n>` laid out as the shared
//! exchange dwords are (per console wave). With one set of dwords for every
//! place, a later place's store can overwrite an earlier one's value when the
//! meetings around it do not pair up as the code says (a host GPU that
//! counts barrier arrivals, two host waves standing at different barriers):
//! an RTX 5090 drew black 16x16 tiles outdoors, the tile classes of
//! `2f03a2df10503cef` (`CS_DepthProcessingHBO`) making `9991e29feaa26e3e`
//! (`CS_ShadowTileClassify`) skip tiles. The meetings stay as they are; a
//! place that no longer fits in the host's 32 KiB uses the shared dwords as
//! before.
//!
//! Ported from Barbital11111's trustno1-coder/VIctory-NVIDIA#246 (24133eec),
//! by way of kylepelham/El1xr#143 (5a0ad670), GPL-3.0.

use super::{Bin, Lower, V};

/// LDS dwords a host thread group has (D3D12: 32 KiB).
const HOST_LDS_DWORDS: u32 = 8192;

/// The prefix of the arrays' names; `bitcode::opt` drops such an array no
/// code reads (the validator refuses a declaration nothing uses).
pub(crate) const PREFIX: &str = "wave_exchange_";

/// `VICTORY_EXCHANGE_SITES=1`; a test can choose for its own thread.
fn enabled() -> bool {
    #[cfg(test)]
    if let Some(on) = TEST_SITES.get() {
        return on;
    }
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_EXCHANGE_SITES").is_some_and(|v| v == "1"))
}

#[cfg(test)]
thread_local! {
    /// A test's choice for [`enabled`], for its thread only.
    pub(crate) static TEST_SITES: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
    /// Arrays made on this thread since a test set it.
    pub(crate) static MADE: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

impl Lower<'_> {
    /// For an exchange whose dwords in the shared exchange region start at
    /// `k` (`exchange_pair(slot)`: the console wave's first) and whose region
    /// starts at `slot`: an array of its own and `k`'s place in it, made in
    /// block `at`; `per_wave` dwords a console wave (2 for the halves' pair,
    /// 1 for a lane read). None with the switch off, or where the array does
    /// not fit: the exchange then uses the shared dwords.
    pub(super) fn exchange_site(&mut self, at: usize, slot: u32, k: V, per_wave: u32) -> Option<(V, V)> {
        if !enabled() {
            return None;
        }
        let threads = self.s.threads.map_or(64, |t| t.iter().product::<u32>());
        let words = threads.div_ceil(64).max(1) * per_wave;
        if self.s.lds_dwords + self.xsites + words > HOST_LDS_DWORDS {
            return None;
        }
        let ty = self.s.m.array(words, self.s.t.i32);
        let g = self.s.m.global(&format!("{PREFIX}{}", self.xsites), ty, 3);
        self.xsites += words;
        #[cfg(test)]
        MADE.set(MADE.get() + 1);
        // `k` is `slot` + 2 x the console wave.
        let base = self.s.m.i32(slot);
        let mut index = self.s.m.bin(at, Bin::Sub, k, base);
        if per_wave == 1 {
            let one = self.s.m.i32(1);
            index = self.s.m.bin(at, Bin::LShr, index, one);
        }
        Some((g, index))
    }
}
