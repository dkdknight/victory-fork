//! Textures shaders write that the title's own code then reads on the CPU
//! (`VICTORY_TEXTURE_BACK`; off by default). From fxceless/victory-fork
//! f5c03f3b (pull request 999: Geckomacabre 895c0b8f, 04df4350), by way of
//! avatarmanz/VIctory-7900XTX 23925a4, where it is on by default.
//!
//! On the console the CPU and the GPU share memory, so what a compute pass
//! stores into a texture is in title memory for the CPU to read. Here it
//! stays in the texture's host copy. The title's ocean is one: its wave
//! passes write a 256x256 height map (R32_FLOAT) and a displacement map
//! (R32G32_FLOAT), and its CPU code samples both, which is what boats float
//! on; it read memory nothing had written, and boats sank (theirs: in every
//! run without it, in none with it).
//!
//! For textures of the kinds the switch names (comma-separated
//! `<width>x<height>f<format>`; `1`: the four water kinds of the upstream
//! trace; unset or `0`: none), a list in which shaders write one through
//! unordered access copies it into a read-back buffer of ours when it closes
//! (`HostList::close`), and once the list has run the texels are tiled into
//! the texture's title memory in the console's layout, as the console's GPU
//! would have left them (`crate::host`'s read-backs, also for flights the
//! relay lands). With the relay that title memory counts as written by the
//! flight until it lands, as declared buffer writes do.
//!
//! A read-back is dropped when the title memory under it changed meanwhile:
//! the texture was released, another one put at its address, or the runtime
//! mapped or unmapped memory there.
//!
//! shortcut: by kind, not by texture: textures of those kinds the title
//! never reads are copied back too (about a megabyte per such list). The
//! copy shows what the list's last write left; a title read in between sees
//! the frame before, as it would before the console's GPU has run.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use windows::Win32::Graphics::Direct3D12::*;

use crate::object::This;

/// The four kinds of the upstream trace (`VICTORY_TEXTURE_BACK=1`).
const WATER: &str = "256x256f41,256x256f16,64x64f16,128x128f36";

/// `<width>x<height>f<format>`, comma-separated, as (width, height, format).
fn parse_kinds(spec: &str) -> Vec<(u64, u32, u32)> {
    spec.split(',')
        .filter_map(|k| {
            let (size, format) = k.trim().split_once('f')?;
            let (w, h) = size.split_once('x')?;
            Some((w.trim().parse().ok()?, h.trim().parse().ok()?, format.trim().parse().ok()?))
        })
        .collect()
}

/// The kinds copied back: (width, height, format).
fn kinds() -> &'static [(u64, u32, u32)] {
    static KINDS: OnceLock<Vec<(u64, u32, u32)>> = OnceLock::new();
    KINDS.get_or_init(|| {
        let kinds = match std::env::var("VICTORY_TEXTURE_BACK").as_deref() {
            Err(_) | Ok("0") | Ok("") => Vec::new(),
            Ok("1") => parse_kinds(WATER),
            Ok(spec) => parse_kinds(spec),
        };
        if !kinds.is_empty() {
            tracing::info!(
                ?kinds,
                "VICTORY_TEXTURE_BACK: textures of these kinds (width, height, format) that shaders write are copied back into title memory"
            );
        }
        kinds
    })
}

/// Whether title texture `resource` is of a kind copied back.
pub(crate) fn wanted(resource: This) -> bool {
    let kinds = kinds();
    !kinds.is_empty() && crate::resource::texture_kind(resource).is_some_and(|k| kinds.contains(&k))
}

/// A read-back still to land into title memory `[at, end)` of texture
/// `owner`: no longer valid once that memory changes.
struct Ticket {
    owner: usize,
    at: usize,
    end: usize,
    valid: AtomicBool,
}

static TICKETS: Mutex<Vec<Weak<Ticket>>> = Mutex::new(Vec::new());
/// A ticket was ever made: before that, nothing to look at.
static TRACKING: AtomicBool = AtomicBool::new(false);

fn track(owner: usize, at: usize, end: usize) -> Arc<Ticket> {
    let ticket = Arc::new(Ticket { owner, at, end, valid: AtomicBool::new(true) });
    let mut tickets = TICKETS.lock().unwrap_or_else(|e| e.into_inner());
    tickets.retain(|weak| weak.strong_count() != 0);
    tickets.push(Arc::downgrade(&ticket));
    TRACKING.store(true, Ordering::Relaxed);
    ticket
}

fn invalidate(matches: impl Fn(&Ticket) -> bool) {
    if !TRACKING.load(Ordering::Relaxed) {
        return;
    }
    TICKETS.lock().unwrap_or_else(|e| e.into_inner()).retain(|weak| {
        let Some(ticket) = weak.upgrade() else {
            return false;
        };
        if matches(&ticket) {
            ticket.valid.store(false, Ordering::Release);
        }
        true
    });
}

/// Title texture `owner` is released.
pub(crate) fn released(owner: usize) {
    invalidate(|ticket| ticket.owner == owner);
}

/// A texture was made at title address `at`.
pub(crate) fn replaced(at: usize) {
    invalidate(|ticket| ticket.at == at);
}

/// The runtime mapped or unmapped title memory `[at, at + len)`.
pub(crate) fn remapped(at: usize, len: usize) {
    let end = at.saturating_add(len);
    invalidate(|ticket| at < ticket.end && ticket.at < end);
}

/// A texture a list's shaders write, to be copied back when it closes.
pub(crate) struct Target {
    pub(crate) copy: Arc<crate::resource::HostTexture>,
    layout: victory_formats::texture::Layout,
    ticket: Arc<Ticket>,
}

impl Target {
    pub(crate) fn new(resource: This) -> Option<Self> {
        // The host copy, held whatever the title does with its resource.
        let copy = crate::resource::host_copy(resource)?;
        let _mapping = crate::watch::title_memory_reading();
        let (layout, at, size) = crate::resource::title_texture(resource)?;
        let end = at.checked_add(size).filter(|&end| at != 0 && end > at)?;
        let ticket = track(resource as usize, at, end);
        Some(Self { copy, layout, ticket })
    }

    pub(crate) fn owner(&self) -> usize {
        self.ticket.owner
    }
}

/// A texture copied into a read-back buffer by a list, to land once it ran.
pub(crate) struct Back {
    buffer: ID3D12Resource,
    footprints: Vec<D3D12_PLACED_SUBRESOURCE_FOOTPRINT>,
    rows: Vec<u32>,
    mips: u32,
    layout: victory_formats::texture::Layout,
    ticket: Arc<Ticket>,
}

impl Back {
    /// The title memory it writes when it lands.
    pub(crate) fn range(&self) -> (usize, usize) {
        (self.ticket.at, self.ticket.end)
    }
}

/// Since the start: read-backs recorded, landed, their bytes, dropped (the
/// memory changed), not tiled.
pub(crate) static STATS: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];

/// Records into `list` the copy of `target`'s host texture (in
/// `COPY_SOURCE`) into a read-back buffer.
pub(crate) fn record(list: &ID3D12GraphicsCommandList, target: Target) -> Option<Back> {
    if !target.ticket.valid.load(Ordering::Acquire) {
        return None;
    }
    let host = crate::host::host()?;
    let texture = &target.copy.texture;
    // SAFETY: a live texture, held by the target.
    let desc = unsafe { texture.GetDesc() };
    let mips = u32::from(desc.MipLevels.max(1));
    let subresources = mips * u32::from(desc.DepthOrArraySize.max(1));
    let n = subresources as usize;
    let mut footprints = vec![D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default(); n];
    let mut rows = vec![0u32; n];
    let mut total = 0u64;
    // SAFETY: out arrays of `n` entries.
    unsafe {
        host.device.GetCopyableFootprints(
            &desc,
            0,
            subresources,
            0,
            Some(footprints.as_mut_ptr()),
            Some(rows.as_mut_ptr()),
            None,
            Some(&mut total),
        )
    };
    // The last footprint's extent may leave out the padding at a row's end;
    // the tiling reads whole pitched slices, so the buffer holds them.
    for (footprint, &rows) in footprints.iter().zip(&rows) {
        let format = footprint.Footprint;
        let pitched = u64::from(format.RowPitch).checked_mul(u64::from(rows))?.checked_mul(u64::from(format.Depth))?;
        total = total.max(footprint.Offset.checked_add(pitched)?);
    }
    let buffer = host.read_back_buffer(total)?;
    for (i, footprint) in footprints.iter().enumerate() {
        let to = D3D12_TEXTURE_COPY_LOCATION {
            // SAFETY: a borrowed pointer the location never releases.
            pResource: unsafe { std::mem::transmute_copy(&buffer) },
            Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { PlacedFootprint: *footprint },
        };
        let from = D3D12_TEXTURE_COPY_LOCATION {
            // SAFETY: as above.
            pResource: unsafe { std::mem::transmute_copy(texture) },
            Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: i as u32 },
        };
        // SAFETY: an open list; the texture is in the copy-source state.
        unsafe { list.CopyTextureRegion(&to, 0, 0, 0, &from, None) };
    }
    STATS[0].fetch_add(1, Ordering::Relaxed);
    Some(Back { buffer, footprints, rows, mips, layout: target.layout, ticket: target.ticket })
}

/// Once the list has run: its texels, tiled, into the texture's title
/// memory, unless that memory changed meanwhile.
pub(crate) fn land(back: &Back) {
    let _mapping = crate::watch::title_memory_reading();
    let _tickets = TICKETS.lock().unwrap_or_else(|e| e.into_inner());
    if !back.ticket.valid.load(Ordering::Acquire) {
        let dropped = STATS[3].fetch_add(1, Ordering::Relaxed) + 1;
        if dropped.is_power_of_two() {
            tracing::info!(dropped, "texture read-backs dropped since the start: their title memory changed meanwhile (VICTORY_TEXTURE_BACK)");
        }
        return;
    }
    let (at, end) = back.range();
    let size = end - at;
    let mut tiled = vec![0u8; size];
    // Title bytes the subresources do not cover stay as they are.
    if !crate::host::read_into(at as *const u8, &mut tiled) {
        return;
    }
    let mut data = std::ptr::null_mut();
    // SAFETY: our read-back buffer, written by the list that has run.
    unsafe {
        if back.buffer.Map(0, None, Some(&mut data)).is_err() || data.is_null() {
            return;
        }
    }
    for (i, (footprint, &rows)) in back.footprints.iter().zip(&back.rows).enumerate() {
        let (mip, slice) = (i as u32 % back.mips, i as u32 / back.mips);
        let f = footprint.Footprint;
        let pitch = f.RowPitch as usize;
        let len = pitch * rows as usize * f.Depth as usize;
        // SAFETY: the subresource's part of the mapped buffer.
        let linear = unsafe { std::slice::from_raw_parts(data.cast::<u8>().add(footprint.Offset as usize), len) };
        if let Err(e) = back.layout.tile(&mut tiled, mip, slice, linear, pitch, pitch * rows as usize) {
            if STATS[4].fetch_add(1, Ordering::Relaxed) == 0 {
                tracing::warn!(?e, mip, slice, "a texture copied back could not be tiled (VICTORY_TEXTURE_BACK)");
            }
            // SAFETY: mapped above.
            unsafe { back.buffer.Unmap(0, None) };
            return;
        }
    }
    // SAFETY: mapped above.
    unsafe { back.buffer.Unmap(0, None) };
    crate::host::copy_mapped(at as *mut u8, tiled.as_ptr(), size, true);
    STATS[2].fetch_add(size as u64, Ordering::Relaxed);
    let landed = STATS[1].fetch_add(1, Ordering::Relaxed) + 1;
    if landed == 1 || landed.is_multiple_of(1024) {
        tracing::info!(
            "texture read-backs landed in title memory since the start (VICTORY_TEXTURE_BACK): {landed} of {} recorded, {} MB; \
             {} dropped (memory changed), {} not tiled",
            STATS[0].load(Ordering::Relaxed),
            STATS[2].load(Ordering::Relaxed) >> 20,
            STATS[3].load(Ordering::Relaxed),
            STATS[4].load(Ordering::Relaxed)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_read_backs_of_changed_memory_are_dropped() {
        let first = track(91, 0x1000, 0x2000);
        let next = track(92, 0x2000, 0x3000);
        let far = track(93, 0x4000, 0x5000);
        remapped(0x0f00, 0x200);
        assert!(!first.valid.load(Ordering::Acquire));
        assert!(next.valid.load(Ordering::Acquire));
        replaced(0x2000);
        assert!(!next.valid.load(Ordering::Acquire));
        assert!(far.valid.load(Ordering::Acquire));
        released(93);
        assert!(!far.valid.load(Ordering::Acquire));
        assert!(track(94, 0x2000, 0x3000).valid.load(Ordering::Acquire));
    }

    #[test]
    fn kinds_are_width_height_and_format() {
        assert_eq!(parse_kinds(WATER), vec![(256, 256, 41), (256, 256, 16), (64, 64, 16), (128, 128, 36)]);
        assert_eq!(parse_kinds(" 8x4f2 , x, 3x3fz,16x16f10"), vec![(8, 4, 2), (16, 16, 10)]);
        assert!(parse_kinds("").is_empty());
    }
}
