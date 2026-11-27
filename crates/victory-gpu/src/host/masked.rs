//! The masked patch (`VICTORY_MASKED_PATCH=1`, off by default; see
//! [`crate::shadow::masked_patch_now`]): a page of a copy in the card's
//! memory with more changed runs under flights' shader writes than the
//! merge's cap leaves goes up as one dispatch of (offset, mask, value)
//! words, after the flights, instead of the flights being settled.
//!
//! Ported from fxceless/victory-fork 3221ac4 (on by default there since
//! 0e006f0) onto this tree's sends (`super::send`, `super::upq`), merge
//! (`Shadow::merge_under_writes`, its pages on several threads) and card
//! steps: the words come from the same pool of memory a submission's sends
//! do, and the patch is the first thing of the list of ours in front of the
//! submission (`Host::card_before`), so that the sends and the card steps'
//! reads come after it on the queue.

use super::*;
use crate::shadow::{PatchWord, Patched, Shadow};

/// Per dword (12 bytes from word `first` of `words`): the title's changed
/// bytes stored, the others kept.
const MASKED_PATCH: &str = r#"
#define RS "RootConstants(num32BitConstants=2, b0), SRV(t0), UAV(u0)"
cbuffer Patch : register(b0) { uint first; uint count; };
ByteAddressBuffer words : register(t0);
RWByteAddressBuffer card : register(u0);
[RootSignature(RS)]
[numthreads(64, 1, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    if (id.x >= count) return;
    uint3 w = words.Load3((first + id.x) * 12u);
    card.Store(w.x, (card.Load(w.x) & ~w.y) | (w.z & w.y));
}
"#;

struct Pass {
    root: ID3D12RootSignature,
    pso: ID3D12PipelineState,
}

/// The patch's code, compiled as the sends' scatter is.
fn code() -> windows::core::Result<Vec<u8>> {
    compile(MASKED_PATCH, windows::core::s!("main"), windows::core::s!("cs_5_1"))
}

/// The patch's root signature and pipeline on `device`.
fn make(device: &ID3D12Device) -> windows::core::Result<Pass> {
    let code = code()?;
    // SAFETY: the code lives through the calls; the root signature is
    // inside it.
    unsafe {
        let root: ID3D12RootSignature = device.CreateRootSignature(0, &code)?;
        let pso: ID3D12PipelineState = device.CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
            pRootSignature: std::mem::transmute_copy(&root),
            CS: D3D12_SHADER_BYTECODE {
                pShaderBytecode: code.as_ptr().cast(),
                BytecodeLength: code.len(),
            },
            ..Default::default()
        })?;
        name(&pso, || "victory: masked patch of title bytes under flights' writes".into());
        Ok(Pass { root, pso })
    }
}

fn pass(host: &Host) -> Option<&'static Pass> {
    static PASS: OnceLock<Option<Pass>> = OnceLock::new();
    PASS.get_or_init(|| match make(&host.device) {
        Ok(pass) => {
            crate::shadow::PATCH_READY.store(true, Ordering::Relaxed);
            tracing::info!(
                "title bytes under flights' shader writes past the merge's cap can go up as a masked patch \
                 (VICTORY_MASKED_PATCH; switched now: {})",
                crate::shadow::masked_patch_now()
            );
            Some(pass)
        }
        Err(e) => {
            tracing::warn!(error = %e, "VICTORY_MASKED_PATCH: the patch's pipeline could not be made; the flights are settled as before");
            None
        }
    })
    .as_ref()
}

/// Before a submission's sends are taken: the patch's pipeline is made if
/// the patch may be on in this run (from then on pages may be patched).
pub(super) fn ready(host: &Host) {
    if crate::shadow::masked_patch_wanted() {
        pass(host);
    }
}

/// A submission's masked patches, their words in memory of the sends' pool.
pub(super) struct Patches {
    memory: send::Memory,
    /// Per copy: the copy, its first word in the memory, its words.
    at: Vec<(std::sync::Arc<Shadow>, u32, u32)>,
}

/// The patches' words in memory the card reads. `None` if there are none;
/// without memory or the pipeline the run ends (the CPU sides already hold
/// the bytes, and nothing else would send them).
pub(super) fn take(host: &Host, patches: &[(std::sync::Arc<Shadow>, Patched)]) -> Option<Patches> {
    if patches.is_empty() {
        return None;
    }
    let words: usize = patches.iter().map(|(_, p)| p.words.len()).sum();
    let size = (words * std::mem::size_of::<PatchWord>()) as u64;
    let (Some(_), Some(memory)) = (pass(host), send::memory(host, size.max(16))) else {
        fail(format_args!("no memory or pipeline for the masked patch of host copies in the card's memory (VICTORY_MASKED_PATCH)"));
    };
    let mut at = Vec::with_capacity(patches.len());
    let mut first = 0u32;
    for (shadow, p) in patches {
        // SAFETY: the mapping holds `words` words of 12 bytes; `PatchWord`
        // is three u32s, as the shader reads them.
        unsafe {
            std::ptr::copy_nonoverlapping(p.words.as_ptr(), memory.data().cast::<PatchWord>().add(first as usize), p.words.len());
        }
        at.push((shadow.clone(), first, p.words.len() as u32));
        first += p.words.len() as u32;
    }
    crate::shadow::PATCHED[3].fetch_add(1, Ordering::Relaxed);
    Some(Patches { memory, at })
}

impl Patches {
    /// Records the patches into `list`, at its head.
    ///
    /// # Safety
    /// As [`record_on`].
    pub(super) unsafe fn record(&self, host: &Host, list: &ID3D12GraphicsCommandList) {
        let Some(pass) = pass(host) else {
            return;
        };
        let at: Vec<(&ID3D12Resource, u32, u32)> = self.at.iter().map(|(s, first, n)| (&s.buffer, *first, *n)).collect();
        // SAFETY: the caller's.
        unsafe { record_on(list, pass, self.memory.buffer(), &at) };
    }

    /// The memory, to be kept until the submission has run.
    pub(super) fn memory(self) -> send::Memory {
        self.memory
    }
}

/// Records the patches of `at` (each copy's buffer, its first word in
/// `words` and how many) into `list`: each copy from the common state to
/// unordered access, its words, then back (what the list does with the
/// copies afterwards finds them as before).
///
/// # Safety
/// An open list of ours on the pass's device, whose compute state is set
/// anew behind this; the copies in the common state; live buffers, the
/// words inside `words` and their offsets inside the copies.
unsafe fn record_on(list: &ID3D12GraphicsCommandList, pass: &Pass, words: &ID3D12Resource, at: &[(&ID3D12Resource, u32, u32)]) {
    let common = D3D12_RESOURCE_STATE_COMMON;
    let write = D3D12_RESOURCE_STATE_UNORDERED_ACCESS;
    let mut copies: Vec<&ID3D12Resource> = Vec::new();
    for (buffer, _, _) in at {
        if !copies.iter().any(|c| c.as_raw() == buffer.as_raw()) {
            copies.push(buffer);
        }
    }
    let there: Vec<_> = copies.iter().map(|c| transition(c, common, write)).collect();
    let back: Vec<_> = copies.iter().map(|c| transition(c, write, common)).collect();
    // SAFETY: the caller's.
    unsafe {
        list.ResourceBarrier(&there);
        list.SetComputeRootSignature(&pass.root);
        list.SetPipelineState(&pass.pso);
        list.SetComputeRootShaderResourceView(1, words.GetGPUVirtualAddress());
        for (buffer, first, count) in at {
            list.SetComputeRootUnorderedAccessView(2, buffer.GetGPUVirtualAddress());
            const MOST: u32 = 64 * D3D12_CS_DISPATCH_MAX_THREAD_GROUPS_PER_DIMENSION;
            let mut done = 0u32;
            while done < *count {
                let n = (*count - done).min(MOST);
                let constants = [first + done, n];
                list.SetComputeRoot32BitConstants(0, 2, constants.as_ptr().cast(), 0);
                list.Dispatch(n.div_ceil(64), 1, 1);
                crate::shadow::PATCHED[2].fetch_add(1, Ordering::Relaxed);
                done += n;
            }
        }
        list.ResourceBarrier(&back);
    }
}

/// The frame line's part.
pub(super) fn line(frames: f64) {
    if !crate::shadow::PATCH_READY.load(Ordering::Relaxed) {
        return;
    }
    let p = [0, 1, 2, 3].map(|i| crate::shadow::PATCHED[i].swap(0, Ordering::Relaxed) as f64 / frames);
    tracing::info!(
        "masked patches per frame (VICTORY_MASKED_PATCH): {:.1} pages past the merge's cap patched instead of a settle, \
         {:.0} dwords, {:.1} dispatches in {:.1} submissions",
        p[0],
        p[1],
        p[2],
        p[3]
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shadow::{folded, patch_words};

    /// The patch's source compiles, with its root signature inside it (no
    /// device needed).
    #[test]
    fn the_masked_patch_compiles() {
        let code = super::code().expect("MASKED_PATCH compiles");
        assert!(code.len() > 100);
    }

    struct Random(u64);

    impl Random {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % n as u64) as usize
        }
    }

    fn buffer(device: &ID3D12Device, kind: D3D12_HEAP_TYPE, size: u64, flags: D3D12_RESOURCE_FLAGS, state: D3D12_RESOURCE_STATES) -> ID3D12Resource {
        let heap = D3D12_HEAP_PROPERTIES {
            Type: kind,
            ..Default::default()
        };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Width: size,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            Flags: flags,
            ..Default::default()
        };
        let mut out: Option<ID3D12Resource> = None;
        // SAFETY: a valid buffer description.
        unsafe { device.CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &desc, state, None, &mut out) }.expect("a test buffer");
        out.unwrap()
    }

    /// On the software adapter (WARP): two copies on the card, their bytes
    /// as the CPU side held them when a flight was prepared; the flight's
    /// shaders store whole dwords into them, some beside the bytes the title
    /// then changes and some over them (a race, as on the console); the
    /// title changes three pages in thousands of runs (more than any merge
    /// cap: every third byte, and a long run). The pages' words are made as
    /// the merge makes them (`patch_words` of the exact changed runs, then
    /// `folded`), and the patch runs after the flight, in one list, as in
    /// `Host::card_before`. Every byte the title changed is then the
    /// title's, every other byte what the flight left there: the merged
    /// bytes, exactly.
    #[test]
    fn a_page_patched_by_masks_after_flights_holds_exactly_the_merged_bytes() {
        let Ok(adapter) = warp_adapter() else {
            eprintln!("no software adapter; skipped");
            return;
        };
        let mut device: Option<ID3D12Device> = None;
        // SAFETY: plain object creation.
        if unsafe { D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_12_0, &mut device) }.is_err() {
            eprintln!("no device on the software adapter; skipped");
            return;
        }
        let device = device.unwrap();
        let pass = make(&device).expect("the patch's pipeline");
        const PAGE: usize = 4096;
        const LEN: usize = 4 * PAGE;
        let mut random = Random(0x9E37_79B9_7F4A_7C15);
        // Per copy: what the CPU side held (and the card, before the
        // flight), the card's bytes after the flight, title memory now.
        let mut held: Vec<Vec<u8>> = Vec::new();
        let mut flown: Vec<Vec<u8>> = Vec::new();
        let mut title: Vec<Vec<u8>> = Vec::new();
        for _ in 0..2 {
            let h: Vec<u8> = (0..LEN).map(|_| random.below(256) as u8).collect();
            let mut f = h.clone();
            for _ in 0..600 {
                let at = 4 * random.below(LEN / 4);
                let word = random.below(1 << 32) as u32;
                f[at..at + 4].copy_from_slice(&word.to_le_bytes());
            }
            held.push(h.clone());
            flown.push(f);
            title.push(h);
        }
        // The pages the title changes (copy, page), and their changes.
        let patched = [(0usize, 0usize), (0, 2), (1, 1)];
        for &(c, page) in &patched {
            let t = &mut title[c][page * PAGE..(page + 1) * PAGE];
            for i in (random.below(3)..PAGE).step_by(3) {
                t[i] = t[i].wrapping_add(1 + random.below(255) as u8);
            }
            for b in &mut t[1000..1300] {
                *b = !*b;
            }
        }
        // The words, per copy, as the merge and the taking make them.
        let mut words: Vec<Vec<PatchWord>> = vec![Vec::new(); 2];
        for &(c, page) in &patched {
            let (lo, hi) = (page * PAGE, (page + 1) * PAGE);
            let runs = crate::shadow::changed_runs_for_tests(&title[c][lo..hi], &held[c][lo..hi]);
            assert!(runs.len() > 1000, "past any cap");
            patch_words(lo, &title[c][lo..hi], &runs, &mut words[c]);
        }
        let words: Vec<Vec<PatchWord>> = words.into_iter().map(folded).collect();
        let all: Vec<PatchWord> = words.concat();
        // SAFETY (all blocks): objects of the test's own device; mapped
        // ranges inside their buffers; the card is waited for before
        // anything it wrote is read.
        unsafe {
            let queue: ID3D12CommandQueue = device
                .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                    Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                    ..Default::default()
                })
                .unwrap();
            let allocator: ID3D12CommandAllocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT).unwrap();
            let list: ID3D12GraphicsCommandList = device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None).unwrap();
            let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE).unwrap();
            let mut fenced = 0u64;
            let mut run = |list: &ID3D12GraphicsCommandList| {
                list.Close().unwrap();
                queue.ExecuteCommandLists(&[Some(list.cast().unwrap())]);
                fenced += 1;
                queue.Signal(&fence, fenced).unwrap();
                while fence.GetCompletedValue() < fenced {
                    std::thread::yield_now();
                }
                assert_ne!(fence.GetCompletedValue(), u64::MAX, "the device was removed");
            };
            let upload = |bytes: &[u8]| {
                let b = buffer(&device, D3D12_HEAP_TYPE_UPLOAD, bytes.len().max(16) as u64, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_GENERIC_READ);
                let mut data = std::ptr::null_mut();
                b.Map(0, None, Some(&mut data)).unwrap();
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), data.cast::<u8>(), bytes.len());
                b.Unmap(0, None);
                b
            };
            let copies: Vec<ID3D12Resource> = (0..2)
                .map(|_| {
                    buffer(&device, D3D12_HEAP_TYPE_DEFAULT, LEN as u64, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COMMON)
                })
                .collect();
            // The card's sides as they stand after the flight (the copies
            // put them there; the buffers are back in the common state when
            // the list has run).
            let sources: Vec<ID3D12Resource> = flown.iter().map(|f| upload(f)).collect();
            for (copy, source) in copies.iter().zip(&sources) {
                list.CopyBufferRegion(copy, 0, source, 0, LEN as u64);
            }
            run(&list);
            // The patch, as the next submission's list of ours records it.
            let bytes: Vec<u8> = all.iter().flat_map(|w| w.iter().flat_map(|v| v.to_le_bytes())).collect();
            let memory = upload(&bytes);
            allocator.Reset().unwrap();
            list.Reset(&allocator, None).unwrap();
            let at = [(&copies[0], 0u32, words[0].len() as u32), (&copies[1], words[0].len() as u32, words[1].len() as u32)];
            record_on(&list, &pass, &memory, &at);
            let back = buffer(&device, D3D12_HEAP_TYPE_READBACK, 2 * LEN as u64, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COPY_DEST);
            for (i, copy) in copies.iter().enumerate() {
                list.CopyBufferRegion(&back, (i * LEN) as u64, copy, 0, LEN as u64);
            }
            run(&list);
            let mut data = std::ptr::null_mut();
            back.Map(0, None, Some(&mut data)).unwrap();
            let card = std::slice::from_raw_parts(data.cast::<u8>(), 2 * LEN).to_vec();
            back.Unmap(0, None);
            for c in 0..2 {
                let mut changed = 0;
                for i in 0..LEN {
                    let want = if title[c][i] != held[c][i] {
                        changed += 1;
                        title[c][i]
                    } else {
                        flown[c][i]
                    };
                    assert_eq!(card[c * LEN + i], want, "copy {c}, byte {i:#x}");
                }
                assert!(changed > 1000);
            }
        }
    }
}
