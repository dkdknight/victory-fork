//! Opt-in compatibility hooks for known title builds.
//!
//! From Corgansm/victory-corgi (crates/victory-loader/src/hooks.rs, 2026-11-26), which took it
//! from trustno1-coder/VIctory-NVIDIA and vitutu1/victory-perf, which ported it from
//! ZigZagBoi1/ScarlettPort (`runtime/victory`, GPL-3.0-or-later). Left out here: the live
//! menus' render scale (`VICTORY_LIVE_FILE`), which needs their live menus.
//! Only per-build offsets and short byte signatures live here (asset policy:
//! nothing of the title itself). Every hook checks the build's hash and the
//! exact bytes it replaces, and does nothing otherwise.

use windows_sys::Win32::System::Diagnostics::Debug::FlushInstructionCache;
use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE, VirtualAlloc, VirtualProtect};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

/// First 16 hex digits of the BLAKE3 of the Series X 1.0.2.1 executable.
const BUILD_1_0_2_1: &str = "b99f876b74d4a77f";

/// The 1.0.2.1 executable's PE header: its link time stamp and
/// `SizeOfImage`. A copy patched in its code only (the low-spec build of it,
/// `scarlett_lowspec.py`) keeps both while its hash differs.
const BUILD_1_0_2_1_HEADER: (u32, u32) = (0x6af7_83d4, 0x0b06_d000);

/// Whether the image at `base` (whose file's BLAKE3 is `hash`) is the
/// 1.0.2.1 executable or a copy of it patched in its code. The hooks still
/// check every byte they replace, so a site the copy changed is skipped.
///
/// # Safety
/// `base` is a mapped image with its headers.
unsafe fn is_1_0_2_1(base: usize, hash: &blake3::Hash) -> bool {
    if hash.to_hex().starts_with(BUILD_1_0_2_1) {
        return true;
    }
    let at = base as *const u8;
    // SAFETY: the DOS and PE headers of the mapped image (the PE header
    // lies in its first page).
    let (signature, stamp, size) = unsafe {
        let pe = at.add(0x3c).cast::<u32>().read_unaligned() as usize;
        if pe > 0x1000 - 0x60 {
            return false;
        }
        (
            at.add(pe).cast::<u32>().read_unaligned(),
            at.add(pe + 8).cast::<u32>().read_unaligned(),
            at.add(pe + 24 + 56).cast::<u32>().read_unaligned(),
        )
    };
    let copy = signature == 0x4550 && (stamp, size) == BUILD_1_0_2_1_HEADER;
    if copy {
        tracing::info!("a patched copy of the 1.0.2.1 executable: its hooks are checked byte by byte");
    }
    copy
}

/// Environment variable that turns the watchdog hook on.
pub(crate) const NO_WATCHDOG_ENV: &str = "VICTORY_NO_WATCHDOG";

/// One code patch: `rva` must hold exactly `expect` before `new` is written.
struct Patch<'a> {
    what: &'static str,
    rva: usize,
    expect: &'a [u8],
    new: &'a [u8],
}

/// The title's watchdog thread ends the process on purpose when its main
/// thread has not advanced for about 30 one-second ticks. Two places do it:
/// the handler it calls (`mov dword ptr [0], imm32`) and, when the handler
/// returns or is unset, a fallback right after the call (`mov dword ptr [0],
/// 0; int3`). Jumping over the whole block returns the loop to its normal
/// path; the first patch makes the handler harmless if something else calls it.
const WATCHDOG: &[Patch<'static>] = &[
    Patch {
        what: "watchdog: skip the call and the fallback fault",
        rva: 0x2215c27,
        // mov rax, [rip + disp32] (the handler pointer)
        expect: &[0x48, 0x8B, 0x05, 0x1A, 0x07, 0xFA, 0x04],
        // jmp +0x18 (to the loop's normal continuation), then NOPs
        new: &[0xEB, 0x18, 0x90, 0x90, 0x90, 0x90, 0x90],
    },
    Patch {
        what: "watchdog: handler's deliberate fault",
        rva: 0x23cf683,
        expect: &[0xC7, 0x04, 0x25, 0, 0, 0, 0, 0x21, 0xD4, 0x8B, 0x4D],
        // an eleven-byte NOP
        new: &[0x66, 0x66, 0x2E, 0x0F, 0x1F, 0x84, 0, 0, 0, 0, 0],
    },
];

/// Applies the watchdog patches whose bytes match. Returns how many applied.
///
/// # Safety
/// `base` is the mapped image whose original BLAKE3 is `hash`, and no title
/// code is running yet.
pub(crate) unsafe fn disarm_watchdog(base: usize, hash: &blake3::Hash) -> usize {
    // SAFETY: forwarded from this function's contract.
    if !unsafe { is_1_0_2_1(base, hash) } {
        return 0;
    }
    WATCHDOG
        .iter()
        // SAFETY: forwarded from this function's contract.
        .filter(|p| unsafe { apply(base, p) })
        .count()
}

/// Environment variable that pins the title's internal render scale
/// (`0.2..=1.0` of the output size). Testing lever, off by default.
pub(crate) const RENDER_SCALE_ENV: &str = "VICTORY_RENDER_SCALE";
/// Environment variable that makes the title use its own temporal upscaler
/// on every console profile (Series S otherwise picks FSR).
pub(crate) const OWN_UPSCALER_ENV: &str = "VICTORY_OWN_UPSCALER";

/// A patch whose bytes are computed at load time.
struct OwnedPatch {
    what: &'static str,
    rva: usize,
    expect: Vec<u8>,
    new: Vec<u8>,
}

/// The upscaler choice: `mov eax, 6` (own upscaler) then `cmovne eax, ecx`
/// (FSR when the profile asks for it). NOPing the cmovne keeps 6.
const OWN_UPSCALER: Patch<'static> = Patch {
    what: "upscaler: always the title's own",
    rva: 0x7e9426,
    expect: &[0x0F, 0x45, 0xC1],
    new: &[0x0F, 0x1F, 0x00],
};

/// Read-only float constants of this build usable as the FSR scale:
/// value, RVA.
const FSR_SCALES: [(f32, usize); 4] = [
    (0.5, 0x5fc6edc),
    (0.4, 0x5fc6f90),
    (1.0 / 3.0, 0x615a240),
    (0.75, 0x6159b04),
];

/// The render-scale patches for `scale`, with the image at `base`:
/// - the own upscaler's minimum scale (an immediate, 2/3 by default) set to
///   `scale` and its maximum made equal to the minimum (`vmaxss` → `vmovss`),
///   so the scale is fixed;
/// - FSR's scale (a load of 2/3 on Series S) pointed at a constant equal to
///   `scale`, when this build has one, and its maximum size made the scaled
///   size (vtable slot 11 → slot 10's function).
fn render_scale_patches(base: usize, scale: f32, fsr: bool) -> Vec<OwnedPatch> {
    let mut patches = vec![
        OwnedPatch {
            what: "render scale: own upscaler minimum",
            rva: 0x28f1e7e,
            expect: vec![0xAB, 0xAA, 0x2A, 0x3F],
            new: scale.to_le_bytes().to_vec(),
        },
        OwnedPatch {
            what: "render scale: own upscaler maximum = minimum",
            rva: 0x28f1e87,
            expect: vec![0xC5, 0xFA, 0x5F, 0x86, 0x90, 0, 0, 0],
            new: vec![0xC5, 0xFA, 0x10, 0x86, 0x90, 0, 0, 0],
        },
    ];
    if !fsr {
        return patches;
    }
    match FSR_SCALES.iter().find(|(v, _)| (v - scale).abs() < 1e-3) {
        Some(&(_, constant)) => {
            // vmovss xmm0, [rip + disp32]; the next instruction is at +8.
            const LOAD: usize = 0x28ef6b2;
            let disp = (constant as i64 - (LOAD as i64 + 8)) as i32;
            let mut new = vec![0xC5, 0xFA, 0x10, 0x05];
            new.extend_from_slice(&disp.to_le_bytes());
            patches.push(OwnedPatch {
                what: "render scale: FSR scale",
                rva: LOAD,
                expect: vec![0xC5, 0xFA, 0x10, 0x05, 0xA2, 0xE2, 0x86, 0x03],
                new,
            });
            patches.push(OwnedPatch {
                what: "render scale: FSR maximum = scaled size",
                rva: 0x6300ba8,
                expect: ((base + 0x28ef6f0) as u64).to_le_bytes().to_vec(),
                new: ((base + 0x28ef680) as u64).to_le_bytes().to_vec(),
            });
        }
        None => tracing::warn!(
            scale,
            "no FSR constant for this scale (0.5, 0.4, 1/3, 0.75); FSR keeps its own (use {OWN_UPSCALER_ENV}=1)"
        ),
    }
    patches
}

/// The scale `VICTORY_RENDER_SCALE` asks for, if valid.
fn parse_scale(text: &str) -> Option<f32> {
    let v: f32 = text.trim().parse().ok()?;
    (0.2..=1.0).contains(&v).then_some(v)
}

/// Applies the resolution levers asked for in the environment
/// ([`RENDER_SCALE_ENV`], [`OWN_UPSCALER_ENV`]). Returns how many applied.
///
/// # Safety
/// As `disarm_watchdog`.
pub(crate) unsafe fn resolution_levers(base: usize, hash: &blake3::Hash) -> usize {
    let scale = std::env::var(RENDER_SCALE_ENV).ok();
    let own = std::env::var_os(OWN_UPSCALER_ENV).is_some_and(|v| v != "0");
    if scale.is_none() && !own {
        return 0;
    }
    // Every image the loader maps comes here (the title's DLLs too); only
    // the 1.0.2.1 executable has the patched places.
    // SAFETY: forwarded from this function's contract.
    if !unsafe { is_1_0_2_1(base, hash) } {
        tracing::debug!("resolution levers: not the 1.0.2.1 executable; nothing patched");
        return 0;
    }
    let mut applied = 0;
    if own {
        // SAFETY: forwarded from this function's contract.
        applied += usize::from(unsafe { apply(base, &OWN_UPSCALER) });
    }
    if let Some(text) = scale {
        match parse_scale(&text) {
            Some(scale) => {
                // With the own upscaler forced, FSR is left as it is.
                for p in render_scale_patches(base, scale, !own) {
                    let p = Patch {
                        what: p.what,
                        rva: p.rva,
                        expect: &p.expect,
                        new: &p.new,
                    };
                    // SAFETY: forwarded from this function's contract.
                    applied += usize::from(unsafe { apply(base, &p) });
                }
            }
            None => tracing::warn!(text, "{RENDER_SCALE_ENV} wants a number in 0.2..=1.0"),
        }
    }
    applied
}

/// The image the frames-ahead drain works on ([`frames_ahead_drain`]).
static FRAMES_AHEAD_BASE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The title's ring of deferred releases (1.0.2.1): 0x1000 object
/// pointers at +0x75D5B60, the consumer index at +0x75D5B50 and the
/// producer's at +0x75D5B54 (`+0xbfb010` pushes under a lock; a null entry
/// is a frame's end, pushed by the limiter after its drain). The limiter
/// drained up to the first null (each entry's `vtable[1](object)`): the
/// releases of the frame before, safe once that frame's fence has passed.
/// With three frames in flight that fence is one frame further back, so
/// this drains up to the first null only when two are waiting, one frame
/// later than the title's own drain; it never reads past the producer.
extern "system" fn frames_ahead_drain() {
    let base = FRAMES_AHEAD_BASE.load(std::sync::atomic::Ordering::Relaxed);
    if base == 0 {
        return;
    }
    frames_ahead_census(base);
    let consumer = (base + 0x75D5B50) as *mut u32;
    let producer = (base + 0x75D5B54) as *const u32;
    let ring = (base + 0x75D5B60) as *const usize;
    // SAFETY: the title's own globals in its mapped image; only this thread
    // (the title's frame end) moves the consumer, the producer only grows.
    unsafe {
        let (c, p) = (std::ptr::read_volatile(consumer), std::ptr::read_volatile(producer));
        let (mut i, mut nulls) = (c, 0);
        let want = FRAMES_AHEAD_WAIT.load(std::sync::atomic::Ordering::Relaxed);
        while i != p && nulls < want {
            i = (i + 1) & 0xFFF;
            if std::ptr::read_volatile(ring.add(i as usize)) == 0 {
                nulls += 1;
            }
        }
        if nulls < want {
            return;
        }
        let mut i = c;
        loop {
            i = (i + 1) & 0xFFF;
            std::ptr::write_volatile(consumer, i);
            let object = std::ptr::read_volatile(ring.add(i as usize));
            if object == 0 {
                break;
            }
            let vtable = *(object as *const *const usize);
            let release: extern "system" fn(usize) = std::mem::transmute(*vtable.add(1));
            release(object);
        }
    }
}

/// How many frame ends must wait in the ring before our drain empties the
/// oldest: 2 (one frame later than the title), or 1 (the title's own) with
/// `VICTORY_FRAMES_AHEAD_DRAIN=own`.
static FRAMES_AHEAD_WAIT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(2);

/// Allocators per block in each pool (the stub at ring+0x120).
static FRAMES_AHEAD_MORE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(1);

/// Frames the census below has seen ([`frames_ahead_census`]).
static FRAMES_AHEAD_SEEN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Once every 120 frames: the title's command blocks (count at +0x75df1c8,
/// size at +0x75df1cc) and how many of its two pools of command allocators
/// are free (arrays at +0x75df220 and +0x75df240 of 0x30-byte wrappers, one
/// per block; a pool's head index is the low half of the word after the
/// array pointer, each free wrapper's next at +0x2c, the count itself is the
/// empty pool). A pop from an empty pool takes the wrapper one past the
/// array: the crash this was written for.
fn frames_ahead_census(base: usize) {
    let seen = FRAMES_AHEAD_SEEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if seen % 120 != 0 {
        return;
    }
    // SAFETY: the title's own globals in its mapped image; the pools are
    // read racily (a lock-free list), bounded by the count.
    unsafe {
        let count = std::ptr::read_volatile((base + 0x75df1c8) as *const u32);
        let size = std::ptr::read_volatile((base + 0x75df1cc) as *const u32);
        let free = |at: usize| -> Option<u32> {
            let array = std::ptr::read_volatile((base + at) as *const usize);
            if array == 0 || count == 0 {
                return None;
            }
            let mut i = std::ptr::read_volatile((base + at + 8) as *const u32);
            let mut n = 0;
            let all = count * u32::from(FRAMES_AHEAD_MORE.load(std::sync::atomic::Ordering::Relaxed));
            while i < all && n <= all {
                n += 1;
                i = std::ptr::read_volatile((array + i as usize * 0x30 + 0x2c) as *const u32);
            }
            Some(n)
        };
        tracing::info!(frame = seen, count, size, free_first = ?free(0x75df220), free_second = ?free(0x75df240), "frames ahead: the title's command blocks");
    }
}

/// Environment variable that lets the title keep this many frames in flight
/// (`3`; unset or `2`: the title's own two). Experimental.
pub(crate) const FRAMES_AHEAD_ENV: &str = "VICTORY_FRAMES_AHEAD";

/// The title (1.0.2.1) ends each frame in its frame limiter (+0x2474740):
/// it puts the frame's GPU token in a ring of two (`and eax, 1`, at
/// +0x75DDB60, right before the ring's tail and head counters) and, while
/// two frames are in flight (`cmp ebp, 2`), waits for the oldest
/// (+0x4fb0a50, the title's generic fence wait). So the frame period is a
/// whole frame's latency (preparing, the card, the landing): in the city
/// about 33 ms of a ~65 ms frame are this wait (measured by Thibault's
/// agent). Its command allocators come from +0x5c280, which always keeps
/// the two newest (`cmp ebp, 2`, at +0x5c2d0) and recycles older ones whose
/// fence has passed.
///
/// With 3: the ring moves to four slots of ours near the image (`lea rbx`
/// re-pointed, `and eax, 3` twice), the wait starts at three frames in
/// flight and stops at two, and three allocators are kept. Other places
/// may assume two frames (ten double-buffered arrays indexed by
/// `frame & 1`, some age checks against 2 or 3): this is the first step,
/// to find what breaks.
///
/// # Safety
/// As `disarm_watchdog`.
pub(crate) unsafe fn frames_ahead(base: usize, hash: &blake3::Hash) -> usize {
    let asked = std::env::var(FRAMES_AHEAD_ENV).ok().and_then(|v| v.trim().parse::<u32>().ok()).unwrap_or(2);
    if asked != 3 {
        return 0;
    }
    // SAFETY: forwarded from this function's contract.
    if !unsafe { is_1_0_2_1(base, hash) } {
        return 0;
    }
    const LEA: usize = 0x2474779;
    // Four slots of ours within 2 GB of the image (a rip-relative lea).
    // SAFETY: the PE headers of the mapped image.
    let size = unsafe {
        let nt = base + *((base + 0x3c) as *const u32) as usize;
        *((nt + 0x50) as *const u32) as usize
    };
    let mut ring = 0usize;
    let mut at = (base + size).next_multiple_of(0x10000);
    for _ in 0..4096 {
        // SAFETY: a plain reservation at a free address, or a refusal.
        let p = unsafe { VirtualAlloc(at as _, 0x10000, MEM_RESERVE | MEM_COMMIT, PAGE_EXECUTE_READWRITE) };
        if !p.is_null() {
            ring = p as usize;
            break;
        }
        at += 0x10000;
    }
    let next = base + LEA + 7;
    let disp = ring as i64 - next as i64;
    if ring == 0 || disp > i32::MAX as i64 || disp < i32::MIN as i64 {
        tracing::warn!("frames ahead: no memory near the image for the ring; the title keeps two frames");
        return 0;
    }
    // Slots read only between the tail and the head, as the title's: set to
    // "no token" all the same.
    // SAFETY: our own committed page.
    unsafe { std::slice::from_raw_parts_mut(ring as *mut u64, 4).fill(u64::MAX) };
    // More command blocks (and so allocators, one per block): the title
    // sizes them once at start (+0x5c5ff00: the configured count, or 0x20).
    // At +0x5c5ff84 `test eax, eax; cmovne r14d, eax` becomes a call to a
    // stub of ours at ring+0x100 that keeps the same choice and multiplies
    // it (VICTORY_FRAMES_AHEAD_BLOCKS, 2 by default).
    let times = std::env::var("VICTORY_FRAMES_AHEAD_BLOCKS").ok().and_then(|v| v.trim().parse::<u8>().ok()).filter(|t| (1..=8).contains(t)).unwrap_or(1);
    const BLOCKS: usize = 0x5c5ff84;
    let stub = ring + 0x100;
    // test eax,eax; jnz +3; mov eax,r14d; imul eax,eax,times; mov r14d,eax; ret
    let code = [0x85, 0xC0, 0x75, 0x03, 0x44, 0x89, 0xF0, 0x6B, 0xC0, times, 0x41, 0x89, 0xC6, 0xC3];
    // SAFETY: our own committed, executable page.
    unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), stub as *mut u8, code.len()) };
    let mut blocks = vec![0xE8];
    blocks.extend_from_slice(&((stub as i64 - (base + BLOCKS + 5) as i64) as i32).to_le_bytes());
    blocks.push(0x90);
    // The drain at +0x24747e6: `mov rax, imm64; call rax; jmp` to the end
    // of the title's loop (+0x2474824).
    const DRAIN: usize = 0x24747e6;
    const DRAIN_BYTES: [u8; 17] = [0x8B, 0x05, 0x64, 0x13, 0x16, 0x05, 0x48, 0x8D, 0x35, 0x6D, 0x13, 0x16, 0x05, 0xFF, 0xC0, 0x25, 0xFF];
    FRAMES_AHEAD_BASE.store(base, std::sync::atomic::Ordering::Relaxed);
    let mut drain = vec![0x48, 0xB8];
    drain.extend_from_slice(&(frames_ahead_drain as usize as u64).to_le_bytes());
    drain.extend_from_slice(&[0xFF, 0xD0, 0xE9]);
    drain.extend_from_slice(&((0x2474824i64 - (DRAIN as i64 + 17)) as i32).to_le_bytes());
    let mut lea = vec![0x48, 0x8D, 0x1D];
    lea.extend_from_slice(&(disp as i32).to_le_bytes());
    let patches = [
        OwnedPatch { what: "frames ahead: the limiter's ring, four slots of ours", rva: LEA, expect: vec![0x48, 0x8D, 0x1D, 0xE0, 0x93, 0x16, 0x05], new: lea },
        OwnedPatch { what: "frames ahead: the head's slot of four", rva: 0x2474785, expect: vec![0x83, 0xE0, 0x01], new: vec![0x83, 0xE0, 0x03] },
        OwnedPatch { what: "frames ahead: wait from three in flight", rva: 0x2474794, expect: vec![0x83, 0xFD, 0x02], new: vec![0x83, 0xFD, 0x03] },
        OwnedPatch { what: "frames ahead: the tail's slot of four", rva: 0x247479b, expect: vec![0x83, 0xE0, 0x01], new: vec![0x83, 0xE0, 0x03] },
        OwnedPatch { what: "frames ahead: wait down to two in flight", rva: 0x24747b3, expect: vec![0x83, 0xFD, 0x01], new: vec![0x83, 0xFD, 0x02] },
        OwnedPatch { what: "frames ahead: three command allocators kept", rva: 0x5c2d0, expect: vec![0x83, 0xFD, 0x02], new: vec![0x83, 0xFD, 0x03] },
        OwnedPatch { what: "frames ahead: deferred releases one frame later (our drain)", rva: DRAIN, expect: DRAIN_BYTES.to_vec(), new: drain },
        OwnedPatch { what: "frames ahead: more command blocks", rva: BLOCKS, expect: vec![0x85, 0xC0, 0x44, 0x0F, 0x45, 0xF0], new: blocks },
    ];
    if std::env::var("VICTORY_FRAMES_AHEAD_DRAIN").is_ok_and(|v| v.trim() == "own") {
        FRAMES_AHEAD_WAIT.store(1, std::sync::atomic::Ordering::Relaxed);
    }
    // Patches left out by index (comma separated), to find which one breaks.
    let skip: Vec<usize> = std::env::var("VICTORY_FRAMES_AHEAD_SKIP").unwrap_or_default().split(',').filter_map(|v| v.trim().parse().ok()).collect();
    let mut applied = 0;
    for (n, p) in patches.iter().enumerate() {
        if skip.contains(&n) {
            tracing::info!(what = p.what, "frames ahead: patch left out");
            continue;
        }
        let p = Patch { what: p.what, rva: p.rva, expect: &p.expect, new: &p.new };
        // SAFETY: forwarded from this function's contract.
        applied += usize::from(unsafe { apply(base, &p) });
    }
    tracing::info!(applied, times, ring = format_args!("{ring:#x}"), "the title may keep three frames in flight ({FRAMES_AHEAD_ENV}=3)");
    applied
}

/// Environment variable: how many command allocators per command block the
/// title gets (2..=16; unset: 4 with `VICTORY_FRAMES_AHEAD=3`, else the
/// title's own one).
pub(crate) const MORE_ALLOCATORS_ENV: &str = "VICTORY_MORE_ALLOCATORS";

/// The title (1.0.2.1) builds each of its two pools of command allocators
/// (+0x161cef0, called for the arrays at +0x75df220 and +0x75df240) with
/// one 0x30-byte wrapper per command block (the count at +0x75df1c8: 216
/// blocks of 128 KiB here). A wrapper comes back (+0x4e9b530) only when a
/// block that noted it is recycled, a whole turn of the blocks' LRU list
/// (+0x5c280) later; at about one taken a frame the first pool is empty
/// after ~240 frames, even with the title's own two frames in flight. A pop
/// from an empty pool (+0x188430, unchecked) takes the word one past the
/// array, 0x900000000 here (the start of the next region): later
/// `ID3D12CommandAllocator::Reset` through a null pointer at +0x4e9b578,
/// the crash Belobug #583 reports under Linux and three frames in flight
/// hit within a minute. The `mov r13d, [count]` at +0x161cf00 becomes a
/// call to a stub of ours near the image that multiplies the count.
/// Measured: the first pool then stays at 648 of 864 free in the city.
///
/// # Safety
/// As `disarm_watchdog`.
pub(crate) unsafe fn more_allocators(base: usize, hash: &blake3::Hash) -> usize {
    let ahead = std::env::var(FRAMES_AHEAD_ENV).is_ok_and(|v| v.trim() == "3");
    let more = match std::env::var(MORE_ALLOCATORS_ENV).ok().and_then(|v| v.trim().parse::<u8>().ok()) {
        Some(n) if (2..=16).contains(&n) => n,
        Some(_) => return 0,
        None if ahead => 4,
        None => return 0,
    };
    // SAFETY: forwarded from this function's contract.
    if !unsafe { is_1_0_2_1(base, hash) } {
        return 0;
    }
    // SAFETY: as above.
    let Some(stub) = (unsafe { near_page(base) }) else {
        tracing::warn!("more command allocators: no memory near the image; the title keeps its own count");
        return 0;
    };
    const POOL: usize = 0x161cf00;
    let count_at = base + 0x75df1c8;
    // mov r13d, [rip+disp]; imul r13d, r13d, more; ret
    let mut code = vec![0x44, 0x8B, 0x2D];
    code.extend_from_slice(&((count_at as i64 - (stub + 7) as i64) as i32).to_le_bytes());
    code.extend_from_slice(&[0x45, 0x6B, 0xED, more, 0xC3]);
    // SAFETY: our own committed, executable page.
    unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), stub as *mut u8, code.len()) };
    FRAMES_AHEAD_MORE.store(more, std::sync::atomic::Ordering::Relaxed);
    let mut call = vec![0xE8];
    call.extend_from_slice(&((stub as i64 - (base + POOL + 5) as i64) as i32).to_le_bytes());
    call.extend_from_slice(&[0x66, 0x90]);
    let p = Patch { what: "more command allocators than blocks", rva: POOL, expect: &[0x44, 0x8B, 0x2D, 0xC1, 0x22, 0xFC, 0x05], new: &call };
    // SAFETY: forwarded from this function's contract.
    let applied = usize::from(unsafe { apply(base, &p) });
    tracing::info!(applied, more, "{MORE_ALLOCATORS_ENV}: command allocators per block");
    applied
}

/// A committed, executable 64 KiB page within 2 GB after the image (for
/// rip-relative and `call rel32` reach), or none.
///
/// # Safety
/// `base` is the mapped image.
unsafe fn near_page(base: usize) -> Option<usize> {
    // SAFETY: the PE headers of the mapped image.
    let size = unsafe {
        let nt = base + *((base + 0x3c) as *const u32) as usize;
        *((nt + 0x50) as *const u32) as usize
    };
    let mut at = (base + size).next_multiple_of(0x10000);
    for _ in 0..4096 {
        // SAFETY: a plain reservation at a free address, or a refusal.
        let p = unsafe { VirtualAlloc(at as _, 0x10000, MEM_RESERVE | MEM_COMMIT, PAGE_EXECUTE_READWRITE) };
        if !p.is_null() {
            return (p as usize - base < 0x7000_0000).then_some(p as usize);
        }
        at += 0x10000;
    }
    None
}

/// # Safety
/// See `disarm_watchdog`.
unsafe fn apply(base: usize, p: &Patch<'_>) -> bool {
    let at = (base + p.rva) as *mut u8;
    // SAFETY: the offset is inside the mapped .text of this exact build.
    let now = unsafe { std::slice::from_raw_parts(at, p.expect.len()) };
    if now != p.expect || p.new.len() != p.expect.len() {
        tracing::warn!(what = p.what, "hook skipped: the bytes are not the expected ones");
        return false;
    }
    let mut old = 0;
    // SAFETY: changing the protection of those bytes only, restored below.
    if unsafe { VirtualProtect(at as _, p.new.len(), PAGE_EXECUTE_READWRITE, &mut old) } == 0 {
        return false;
    }
    // SAFETY: writable now; the bytes are the ones checked above.
    unsafe {
        std::ptr::copy_nonoverlapping(p.new.as_ptr(), at, p.new.len());
        VirtualProtect(at as _, p.new.len(), old, &mut old);
        FlushInstructionCache(GetCurrentProcess(), at as _, p.new.len());
    }
    tracing::info!(what = p.what, "hook applied");
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_outside_the_floor_are_refused() {
        assert_eq!(parse_scale("0.5"), Some(0.5));
        assert_eq!(parse_scale(" 1 "), Some(1.0));
        assert_eq!(parse_scale("0.1"), None);
        assert_eq!(parse_scale("1.5"), None);
        assert_eq!(parse_scale("half"), None);
    }

    #[test]
    fn half_scale_patches_both_upscalers() {
        let base = 0x7ff0_0000_0000;
        let p = render_scale_patches(base, 0.5, true);
        assert_eq!(p.len(), 4);
        assert_eq!(p[0].new, 0.5f32.to_le_bytes());
        // The FSR load now reads 0.5 at 0x5fc6edc: disp from the next instruction.
        assert_eq!(p[2].new, [0xC5, 0xFA, 0x10, 0x05, 0x22, 0x78, 0x6D, 0x03]);
        assert_eq!(p[3].new, ((base + 0x28ef680) as u64).to_le_bytes());
        assert!(p.iter().all(|p| p.new.len() == p.expect.len()));
    }

    #[test]
    fn odd_scales_patch_only_the_own_upscaler() {
        assert_eq!(render_scale_patches(0, 0.6, true).len(), 2);
        assert_eq!(render_scale_patches(0, 0.5, false).len(), 2);
    }
}

