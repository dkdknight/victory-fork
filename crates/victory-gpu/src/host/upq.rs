//! What a submission sends up to copies in the card's memory comes over the
//! bus on a copy queue of its own (`VICTORY_UP_QUEUE=1`; off by default;
//! A/B-able with `VICTORY_AB=<s>,VICTORY_UP_QUEUE` in diagnostics builds).
//!
//! Ported from avatarmanz/VIctory-7900XTX (`d3a6c2c`, "Copies in the card's
//! memory brought up on a copy queue", and `53ed64e`, "Up queue for the sends
//! of a submission's own memory").
//!
//! Without it the drawing queue copies the bytes from the PC's memory to the
//! card's sides at the head of a submission's lists ([`Host::card_before`]'s
//! list): PCIe-bound, and the drawing queue does nothing else meanwhile (a
//! GPU trace at the boat scene: one `CopyBufferRegion` of 43 MB, 6.2 ms).
//! With it the copy queue brings the same bytes into a buffer of ours in the
//! card's memory as soon as the submission is prepared, while the drawing
//! queue still runs earlier submissions; the drawing queue waits for that
//! copy (a fence, `ID3D12CommandQueue::Wait`) and then copies from our buffer
//! to the card's sides, card to card, at the same place in its order as
//! before. So every list of the submission and of later ones finds the new
//! bytes, and no earlier one does: the card's sides are still written only
//! by the drawing queue, behind the lists that were before.
//!
//! With the sends of [`send`] (`VICTORY_SEND_OWN`, on by default) the bytes
//! are in the submission's own memory in the PC's: that comes up whole, in
//! one copy, and the sends (copies, and the pairs of offset and word the
//! scatter reads) read it from our buffer instead.
//!
//! The copy queue reads the PC's side after the bytes were put there (it is
//! sent after that). Our buffer and lists belong to the submission's plan
//! (`CardPlan::up`) and come back with it when its submission has run
//! (`Host::card_done`), so they are not written again while in use.
//!
//! [`Host::card_before`]: super::Host
//! [`send`]: super::send

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use windows::Win32::Graphics::Direct3D12::*;

/// Whether it is on now (`VICTORY_UP_QUEUE=1`, or its phase of `VICTORY_AB`).
pub(super) fn on() -> bool {
    #[cfg(feature = "diagnostics")]
    if let Some(on) = crate::ab::pick("VICTORY_UP_QUEUE") {
        return on;
    }
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_UP_QUEUE").is_some_and(|v| v == "1"))
}

/// The most bytes one submission sends this way; more go as before.
const MOST: u64 = 512 << 20;

/// Our buffers are made in sizes of powers of two from this on.
const LEAST: u64 = 4 << 20;

/// At most this much of our buffers is kept among the free slots.
const KEPT: u64 = 256 << 20;

/// One submission's: a copy list and our buffer in the card's memory.
pub(super) struct Slot {
    allocator: ID3D12CommandAllocator,
    list: ID3D12GraphicsCommandList,
    bounce: Option<(ID3D12Resource, u64)>,
}

// SAFETY: free-threaded D3D12 objects, used by one submission at a time.
unsafe impl Send for Slot {}

impl Slot {
    /// Our buffer, which holds what went up.
    pub(super) fn bounce(&self) -> Option<&ID3D12Resource> {
        self.bounce.as_ref().map(|b| &b.0)
    }
}

/// The copy queue, its fence and the free slots.
pub(super) struct Up {
    queue: ID3D12CommandQueue,
    fence: ID3D12Fence,
    sent: u64,
    free: Vec<Slot>,
}

// SAFETY: as `Slot`.
unsafe impl Send for Up {}

/// `None` inside: none could be made (then nothing goes this way).
static QUEUE: Mutex<Option<Option<Up>>> = Mutex::new(None);

/// Since the last frame-rate line: submissions, bytes, ranges sent this way;
/// submissions that went as before (too big, or something could not be
/// made or sent).
pub(super) static STATS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

/// Bytes of our buffers in the card's memory alive now (in use or kept),
/// for the census line.
pub(crate) static BOUNCE: AtomicU64 = AtomicU64::new(0);

/// One range: the card side, its offset, the PC's side, the bytes (the same
/// offset on both sides).
pub(super) type Range<'a> = (&'a ID3D12Resource, u64, &'a ID3D12Resource, u64);

/// Where range `i` of `ranges` lies in our buffer.
fn places<'a>(ranges: &'a [Range<'_>]) -> impl Iterator<Item = u64> + 'a {
    ranges.iter().scan(0u64, |at, r| {
        let here = *at;
        *at += r.3.next_multiple_of(256);
        Some(here)
    })
}

impl Up {
    pub(super) fn new(device: &ID3D12Device) -> Option<Up> {
        // SAFETY: plain object creation.
        let made = unsafe {
            (|| -> windows::core::Result<Up> {
                let queue: ID3D12CommandQueue = device.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                    Type: D3D12_COMMAND_LIST_TYPE_COPY,
                    Priority: D3D12_COMMAND_QUEUE_PRIORITY_HIGH.0,
                    ..Default::default()
                })?;
                let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE)?;
                Ok(Up { queue, fence, sent: 0, free: Vec::new() })
            })()
        };
        match made {
            Ok(up) => {
                super::name(&up.queue, || "copies in the card's memory: up queue".into());
                Some(up)
            }
            Err(e) => {
                tracing::warn!(error = %e, "no copy queue for what goes up to copies in the card's memory: it goes on the drawing queue");
                None
            }
        }
    }

    fn slot(device: &ID3D12Device) -> Option<Slot> {
        // SAFETY: plain object creation.
        unsafe {
            (|| -> windows::core::Result<Slot> {
                let allocator: ID3D12CommandAllocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_COPY)?;
                let list: ID3D12GraphicsCommandList = device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_COPY, &allocator, None)?;
                list.Close()?;
                Ok(Slot { allocator, list, bounce: None })
            })()
            .ok()
        }
    }

    /// Sends `ranges` up on the copy queue into our buffer and makes the
    /// drawing queue wait for that; then `record` gets, for each range, our
    /// buffer and the range's offset in it, to copy from there on the
    /// drawing queue. `make` makes our buffer (in the card's memory, in the
    /// common state). `None`: nothing was sent (nothing to send, too many
    /// bytes, or something could not be made or sent), and nothing waits;
    /// the caller copies as before.
    ///
    /// # Safety
    /// Live buffers, the ranges lie in them; the PC's sides hold the bytes
    /// to send; `drawing` is the queue the lists that read the card sides go
    /// to, and what `record` records goes to it after this.
    pub(super) unsafe fn send(
        &mut self,
        device: &ID3D12Device,
        make: impl FnOnce(u64) -> Option<ID3D12Resource>,
        drawing: &ID3D12CommandQueue,
        ranges: &[Range<'_>],
        mut record: impl FnMut(usize, &ID3D12Resource, u64),
    ) -> Option<Slot> {
        let total: u64 = ranges.iter().map(|r| r.3.next_multiple_of(256)).sum();
        if total == 0 || total > MOST {
            STATS[3].fetch_add(1, Relaxed);
            return None;
        }
        let Some(mut slot) = self.free.pop().or_else(|| Self::slot(device)) else {
            STATS[3].fetch_add(1, Relaxed);
            return None;
        };
        if slot.bounce.as_ref().is_none_or(|b| b.1 < total) {
            let size = total.next_power_of_two().max(LEAST);
            if let Some((_, old)) = slot.bounce.take() {
                BOUNCE.fetch_sub(old, Relaxed);
            }
            let Some(made) = make(size) else {
                self.free.push(slot);
                STATS[3].fetch_add(1, Relaxed);
                return None;
            };
            super::name(&made, || format!("copies in the card's memory: what goes up, {} MB", size >> 20));
            slot.bounce = Some((made, size));
            BOUNCE.fetch_add(size, Relaxed);
        }
        let (bounce, _) = slot.bounce.clone()?;
        // SAFETY: the slot's last submission has run (it came back with its
        // plan); the caller's for the buffers.
        let mut executed = false;
        let sent = unsafe {
            (|| -> windows::core::Result<()> {
                slot.allocator.Reset()?;
                slot.list.Reset(&slot.allocator, None)?;
                for (&(_, offset, cpu, bytes), at) in ranges.iter().zip(places(ranges)) {
                    slot.list.CopyBufferRegion(&bounce, at, cpu, offset, bytes);
                }
                slot.list.Close()?;
                let list = windows::core::Interface::cast(&slot.list)?;
                self.queue.ExecuteCommandLists(&[Some(list)]);
                executed = true;
                self.queue.Signal(&self.fence, self.sent + 1)?;
                self.sent += 1;
                Ok(())
            })()
        };
        if let Err(e) = sent {
            // Nothing waits for a copy that was not signalled, and the
            // caller copies as before. A copy that was executed only writes
            // our buffer, but may still run: its slot is never used or
            // given up again (it is not known when the copy is done).
            tracing::warn!(error = %e, "what goes up to copies in the card's memory could not go on the copy queue: as before");
            if executed {
                std::mem::forget(slot);
            } else {
                self.free.push(slot);
            }
            STATS[3].fetch_add(1, Relaxed);
            return None;
        }
        // SAFETY: a fence of ours that the copy queue signals.
        if let Err(e) = unsafe { drawing.Wait(&self.fence, self.sent) } {
            // The drawing queue cannot be made to wait: wait here, so that
            // what follows still finds the bytes.
            tracing::warn!(error = %e, "the drawing queue could not wait for the up queue: waited on the CPU");
            // SAFETY: as above; a null event waits until the value is
            // reached.
            let _ = unsafe { self.fence.SetEventOnCompletion(self.sent, windows::Win32::Foundation::HANDLE::default()) };
        }
        for (i, at) in places(ranges).enumerate() {
            record(i, &bounce, at);
        }
        STATS[0].fetch_add(1, Relaxed);
        STATS[1].fetch_add(ranges.iter().map(|r| r.3).sum::<u64>(), Relaxed);
        STATS[2].fetch_add(ranges.len() as u64, Relaxed);
        Some(slot)
    }

    /// A slot, back once its submission has run. What is kept of our
    /// buffers is bounded: the largest go first.
    pub(super) fn give_back(&mut self, slot: Slot) {
        self.free.push(slot);
        let size = |s: &Slot| s.bounce.as_ref().map_or(0, |b| b.1);
        while self.free.iter().map(size).sum::<u64>() > KEPT {
            let Some(largest) = self.free.iter_mut().max_by_key(|s| size(s)) else {
                break;
            };
            if let Some((_, old)) = largest.bounce.take() {
                BOUNCE.fetch_sub(old, Relaxed);
            }
        }
    }
}

/// [`Up::send`] on the host's copy queue, made the first time.
///
/// # Safety
/// As [`Up::send`].
pub(super) unsafe fn send(
    host: &super::Host,
    drawing: &ID3D12CommandQueue,
    ranges: &[Range<'_>],
    record: impl FnMut(usize, &ID3D12Resource, u64),
) -> Option<Slot> {
    let mut guard = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(up) = guard.get_or_insert_with(|| Up::new(&host.device)).as_mut() else {
        STATS[3].fetch_add(1, Relaxed);
        return None;
    };
    // SAFETY: the caller's.
    unsafe { up.send(&host.device, |size| host.uav_buffer(size), drawing, ranges, record) }
}

/// A plan's slot, back once its submission has run.
pub(super) fn give_back(slot: Slot) {
    let mut guard = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(Some(up)) = guard.as_mut() {
        up.give_back(slot);
    }
}

/// For the frame-rate line.
pub(super) fn line(frames: f64) {
    let [subs, bytes, ranges, before] = [0, 1, 2, 3].map(|i| STATS[i].swap(0, Relaxed) as f64 / frames);
    if subs + before > 0.0 {
        tracing::info!(
            "sends to copies in the card's memory brought up on a copy queue per frame (VICTORY_UP_QUEUE): {subs:.1} submissions, \
             {:.1} MB in {ranges:.0} ranges; {before:.1} submissions fell back to the drawing queue (too big, or nothing to send with)",
            bytes / 1048576.0
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
    use windows::Win32::Graphics::Dxgi::*;
    use windows::core::Interface;

    fn buffer(device: &ID3D12Device, kind: D3D12_HEAP_TYPE, size: u64, state: D3D12_RESOURCE_STATES) -> ID3D12Resource {
        let heap = D3D12_HEAP_PROPERTIES { Type: kind, ..Default::default() };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Width: size,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            ..Default::default()
        };
        let mut out: Option<ID3D12Resource> = None;
        // SAFETY: plain creation.
        unsafe { device.CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &desc, state, None, &mut out) }.unwrap();
        out.unwrap()
    }

    fn map(b: &ID3D12Resource) -> *mut u8 {
        let mut p = std::ptr::null_mut();
        // SAFETY: a mappable buffer of ours.
        unsafe { b.Map(0, None, Some(&mut p)) }.unwrap();
        p.cast()
    }

    fn list(device: &ID3D12Device) -> (ID3D12CommandAllocator, ID3D12GraphicsCommandList) {
        // SAFETY: plain creation.
        unsafe {
            let a: ID3D12CommandAllocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT).unwrap();
            let l: ID3D12GraphicsCommandList = device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &a, None).unwrap();
            (a, l)
        }
    }

    /// Bytes sent up through the up queue are seen by a list behind the one
    /// that copies them from our buffer, and not by a list sent before it
    /// that runs only after the copy queue has finished; ranges land at
    /// their offsets.
    #[test]
    fn warp_up_queue_bytes_seen_by_later_lists_only() {
        let _turn = crate::indirect::tests::warp_turn();
        // SAFETY: a D3D12 device and objects of the test's own on WARP.
        unsafe {
            let Ok(factory) = CreateDXGIFactory2::<IDXGIFactory4>(DXGI_CREATE_FACTORY_FLAGS(0)) else {
                return;
            };
            let Ok(adapter) = factory.EnumWarpAdapter::<IDXGIAdapter>() else {
                return;
            };
            let mut device: Option<ID3D12Device> = None;
            if windows::Win32::Graphics::Direct3D12::D3D12CreateDevice(
                &adapter,
                windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0,
                &mut device,
            )
            .is_err()
            {
                return;
            }
            let device = device.unwrap();
            let drawing: ID3D12CommandQueue = device
                .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC { Type: D3D12_COMMAND_LIST_TYPE_DIRECT, ..Default::default() })
                .unwrap();
            const SIZE: u64 = 64 << 10;
            let card = buffer(&device, D3D12_HEAP_TYPE_DEFAULT, SIZE, D3D12_RESOURCE_STATE_COMMON);
            let cpu = buffer(&device, D3D12_HEAP_TYPE_UPLOAD, SIZE, D3D12_RESOURCE_STATE_GENERIC_READ);
            let old = buffer(&device, D3D12_HEAP_TYPE_UPLOAD, SIZE, D3D12_RESOURCE_STATE_GENERIC_READ);
            let early = buffer(&device, D3D12_HEAP_TYPE_READBACK, SIZE, D3D12_RESOURCE_STATE_COPY_DEST);
            let late = buffer(&device, D3D12_HEAP_TYPE_READBACK, SIZE, D3D12_RESOURCE_STATE_COPY_DEST);
            let (cpu_p, old_p) = (map(&cpu), map(&old));
            std::ptr::write_bytes(old_p, 0x11, SIZE as usize);
            std::ptr::write_bytes(cpu_p, 0xee, SIZE as usize);
            // The card side starts as 0x11.
            let (a0, l0) = list(&device);
            l0.CopyBufferRegion(&card, 0, &old, 0, SIZE);
            l0.Close().unwrap();
            drawing.ExecuteCommandLists(&[Some(l0.cast().unwrap())]);
            // A gate keeps the drawing queue from running the earlier list
            // until the copy queue has run.
            let gate: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE).unwrap();
            drawing.Wait(&gate, 1).unwrap();
            let (a1, l1) = list(&device);
            l1.CopyBufferRegion(&early, 0, &card, 0, SIZE);
            l1.Close().unwrap();
            drawing.ExecuteCommandLists(&[Some(l1.cast().unwrap())]);
            // Ranges of odd sizes and places, not at our buffer's offsets.
            let ranges: Vec<Range<'_>> = vec![(&card, 16, &cpu, 100), (&card, 4096, &cpu, 300), (&card, 40000, &cpu, 7)];
            let mut up = Up::new(&device).expect("a copy queue on WARP");
            // Nothing to send: nothing goes up and nothing waits.
            let none = up.send(&device, |_| unreachable!("no buffer for nothing"), &drawing, &[], |_, _, _| unreachable!());
            assert!(none.is_none());
            assert_eq!(up.sent, 0);
            let (a2, l2) = list(&device);
            let mut got = Vec::new();
            let slot = up
                .send(
                    &device,
                    |size| Some(buffer(&device, D3D12_HEAP_TYPE_DEFAULT, size, D3D12_RESOURCE_STATE_COMMON)),
                    &drawing,
                    &ranges,
                    |i, bounce, at| {
                        got.push(at);
                        let (card, offset, _, bytes) = ranges[i];
                        l2.CopyBufferRegion(card, offset, bounce, at, bytes);
                    },
                )
                .expect("sent up");
            assert_eq!(got, [0, 256, 768]);
            assert!(slot.bounce().is_some());
            l2.CopyBufferRegion(&late, 0, &card, 0, SIZE);
            l2.Close().unwrap();
            drawing.ExecuteCommandLists(&[Some(l2.cast().unwrap())]);
            // Let the copy queue finish first, then the drawing queue.
            let done: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE).unwrap();
            up.fence.SetEventOnCompletion(up.sent, windows::Win32::Foundation::HANDLE::default()).unwrap();
            gate.Signal(1).unwrap();
            drawing.Signal(&done, 1).unwrap();
            done.SetEventOnCompletion(1, windows::Win32::Foundation::HANDLE::default()).unwrap();
            let (e, l) = (map(&early), map(&late));
            let early = std::slice::from_raw_parts(e, SIZE as usize);
            let late = std::slice::from_raw_parts(l, SIZE as usize);
            assert!(early.iter().all(|&b| b == 0x11), "an earlier list saw bytes sent up after it");
            for (i, &b) in late.iter().enumerate() {
                let sent = ranges.iter().any(|r| (r.1..r.1 + r.3).contains(&(i as u64)));
                assert_eq!(b, if sent { 0xee } else { 0x11 }, "byte {i}");
            }
            up.give_back(slot);
            assert_eq!(up.free.len(), 1);
            drop((a0, a1, a2));
        }
    }

    /// Kept buffers are bounded: past the bound the largest is given up,
    /// its slot kept.
    #[test]
    fn warp_up_queue_keeps_buffers_bounded() {
        let _turn = crate::indirect::tests::warp_turn();
        // SAFETY: objects of the test's own on WARP.
        unsafe {
            let Ok(factory) = CreateDXGIFactory2::<IDXGIFactory4>(DXGI_CREATE_FACTORY_FLAGS(0)) else {
                return;
            };
            let Ok(adapter) = factory.EnumWarpAdapter::<IDXGIAdapter>() else {
                return;
            };
            let mut device: Option<ID3D12Device> = None;
            if D3D12CreateDevice(&adapter, windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0, &mut device).is_err() {
                return;
            }
            let device = device.unwrap();
            let mut up = Up::new(&device).expect("a copy queue on WARP");
            let small = buffer(&device, D3D12_HEAP_TYPE_DEFAULT, 256, D3D12_RESOURCE_STATE_COMMON);
            // (Sizes are only counted: one small buffer stands for each.)
            for size in [KEPT / 2, KEPT / 4, KEPT / 2] {
                let mut slot = Up::slot(&device).unwrap();
                slot.bounce = Some((small.clone(), size));
                up.give_back(slot);
            }
            assert_eq!(up.free.len(), 3);
            let kept: Vec<u64> = up.free.iter().map(|s| s.bounce.as_ref().map_or(0, |b| b.1)).collect();
            assert_eq!(kept.iter().sum::<u64>(), KEPT * 3 / 4);
            assert_eq!(kept.iter().filter(|&&k| k == 0).count(), 1);
        }
    }
}
