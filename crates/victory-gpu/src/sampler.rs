//! Samplers as the host describes them, from the console's sampler
//! description (`CreateSamplerX`), which names the hardware's sampler
//! fields. (Immediate samplers in shader code come decoded with the
//! translation.)

use windows::Win32::Graphics::Direct3D12::*;

/// The fields both forms share.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Fields {
    /// Point 0, bilinear 1, anisotropic point 2, anisotropic bilinear 3.
    mag: u32,
    min: u32,
    /// None 0, point 1, linear 2.
    mip: u32,
    /// Plain 0, minimum 1, maximum 2.
    mode: u32,
    /// Host address modes.
    address: [u32; 3],
    anisotropy: u32,
    /// Host comparison function.
    compare: u32,
    min_lod: f32,
    max_lod: f32,
    bias: f32,
    border: [f32; 4],
}

impl Fields {
    fn host(&self) -> D3D12_SAMPLER_DESC {
        let linear = |f: u32| u32::from(f == 1 || f == 3);
        let aniso = self.mag >= 2 || self.min >= 2;
        // The console compares when the instruction says so, with the
        // sampler's function; the host needs a comparison sampler for that.
        // A sampler given a real test (less ... greater-or-equal) is one;
        // never, always and none are plain samplers.
        const LESS: u32 = 2;
        const GREATER_EQUAL: u32 = 7;
        let compares = (LESS..=GREATER_EQUAL).contains(&self.compare);
        let reduction = match self.mode {
            1 => 2,
            2 => 3,
            _ => u32::from(compares),
        };
        let filter = if aniso {
            0x55
        } else {
            linear(self.min) << 4 | linear(self.mag) << 2 | u32::from(self.mip == 2)
        };
        let lods = if self.mip == 0 {
            (self.min_lod, self.min_lod)
        } else {
            lod_order(self.min_lod, self.max_lod)
        };
        D3D12_SAMPLER_DESC {
            Filter: D3D12_FILTER((reduction << 7 | filter) as i32),
            AddressU: D3D12_TEXTURE_ADDRESS_MODE(self.address[0] as i32),
            AddressV: D3D12_TEXTURE_ADDRESS_MODE(self.address[1] as i32),
            AddressW: D3D12_TEXTURE_ADDRESS_MODE(self.address[2] as i32),
            MipLODBias: self.bias,
            MaxAnisotropy: if aniso {
                self.anisotropy.clamp(1, 16)
            } else {
                1
            },
            ComparisonFunc: D3D12_COMPARISON_FUNC(self.compare.max(1) as i32),
            BorderColor: self.border,
            MinLOD: lods.0,
            // Without a mip filter the console samples mip 0 only.
            MaxLOD: lods.1,
        }
    }
}

/// `VICTORY_SAMPLER_LOD_ORDER=1` (or `sampler_lod_order=1` in the live
/// file, for samplers made from then on): a sampler whose MaxLOD is below
/// its MinLOD gets MaxLOD = MinLOD, and a NaN MinLOD becomes 0 (a NaN MaxLOD
/// then MinLOD). D3D12 asks for MaxLOD >= MinLOD (`D3D12_SAMPLER_DESC`:
/// "This value must be greater than or equal to MinLOD"); what a driver
/// does with the inverted pair is not said, and neither is what the
/// console's sampler does with it, so this is off by default. Idea from
/// ubiquitarianism/leonida #2089 (there for Vulkan's
/// `VUID-VkSamplerCreateInfo-maxLod-01973`).
fn lod_order_on() -> bool {
    if let Some(v) = crate::host::live::value("sampler_lod_order") {
        return v == "1";
    }
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var_os("VICTORY_SAMPLER_LOD_ORDER").is_some_and(|v| v == "1");
        if on {
            tracing::info!("VICTORY_SAMPLER_LOD_ORDER=1: a sampler's MaxLOD is raised to its MinLOD when below it");
        }
        on
    })
}

/// Samplers made with MaxLOD below MinLOD (or a NaN LOD), and of those the
/// ones [`lod_order`] put in order; since the last [`frame_line`].
static LOD_ORDER: [std::sync::atomic::AtomicU64; 2] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 2];

/// `(MinLOD, MaxLOD)` in order when [`lod_order_on`], else as given; counted.
pub(crate) fn lod_order(min: f32, max: f32) -> (f32, f32) {
    let out_of_order = min.is_nan() || max.is_nan() || max < min;
    if !out_of_order {
        return (min, max);
    }
    use std::sync::atomic::Ordering;
    LOD_ORDER[0].fetch_add(1, Ordering::Relaxed);
    if !lod_order_on() {
        return (min, max);
    }
    LOD_ORDER[1].fetch_add(1, Ordering::Relaxed);
    ordered_lods(min, max)
}

/// The rule of [`lod_order`], pure.
fn ordered_lods(min: f32, max: f32) -> (f32, f32) {
    let min = if min.is_nan() { 0.0 } else { min };
    let max = if max.is_nan() || max < min { min } else { max };
    (min, max)
}

/// The per-frame line of samplers made with their LODs out of order, when
/// there were some (they are made when the title writes descriptors and
/// makes pipelines, so most frames have none).
pub(crate) fn frame_line(frames: f64) {
    use std::sync::atomic::Ordering;
    let [seen, ordered] = [0, 1].map(|i| LOD_ORDER[i].swap(0, Ordering::Relaxed));
    if seen > 0 {
        tracing::info!(
            "samplers made with MaxLOD below MinLOD (or a NaN LOD): {seen} this second ({:.1} per frame), {ordered} of them given MaxLOD = MinLOD (VICTORY_SAMPLER_LOD_ORDER)",
            seen as f64 / frames
        );
    }
}

/// The console's sampler description (27 words): flags, magnification,
/// minification, mip and depth filters, filter mode, three address modes,
/// minimum and maximum LOD, LOD bias, secondary bias, anisotropy, its bias
/// and threshold, comparison, border color, then tuning words.
pub(crate) fn console(words: &[u32; 27]) -> D3D12_SAMPLER_DESC {
    let f = |i: usize| f32::from_bits(words[i]);
    Fields {
        mag: words[1],
        min: words[2],
        mip: words[3],
        mode: words[5],
        address: [words[6], words[7], words[8]],
        min_lod: f(9),
        max_lod: f(10),
        bias: f(11),
        anisotropy: words[13],
        compare: words[16],
        border: [f(17), f(18), f(19), f(20)],
    }
    .host()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_descriptions_decode() {
        let mut w = [0u32; 27];
        w[1] = 1; // bilinear magnification
        w[2] = 0; // point minification
        w[3] = 2; // linear mips
        w[6..9].copy_from_slice(&[3, 3, 3]);
        w[10] = 1000.0f32.to_bits();
        let d = console(&w);
        assert_eq!(d.Filter.0, 0x5, "min point, mag and mip linear");
        assert_eq!(d.AddressW, D3D12_TEXTURE_ADDRESS_MODE_CLAMP);
        assert_eq!(d.MaxLOD, 1000.0);
        // A less-or-equal test makes a comparison sampler; never does not.
        w[16] = 4;
        assert_eq!(console(&w).Filter.0, 0x85);
        assert_eq!(console(&w).ComparisonFunc, D3D12_COMPARISON_FUNC_LESS_EQUAL);
        w[16] = 1;
        assert_eq!(console(&w).Filter.0, 0x5);
    }

    #[test]
    fn inverted_lods_are_put_in_order() {
        assert_eq!(ordered_lods(3.5, 1.0), (3.5, 3.5));
        assert_eq!(ordered_lods(1.0, 3.5), (1.0, 3.5), "in order: kept");
        assert_eq!(ordered_lods(2.0, 2.0), (2.0, 2.0));
        assert_eq!(ordered_lods(f32::NAN, 4.0), (0.0, 4.0));
        assert_eq!(ordered_lods(2.0, f32::NAN), (2.0, 2.0));
        assert_eq!(ordered_lods(f32::NAN, f32::NAN), (0.0, 0.0));
        // Off by default: the console's words pass through as they are.
        assert_eq!(lod_order(1.0, 3.5), (1.0, 3.5));
        if std::env::var_os("VICTORY_SAMPLER_LOD_ORDER").is_none() {
            assert_eq!(lod_order(3.5, 1.0), (3.5, 1.0));
        }
    }
}
