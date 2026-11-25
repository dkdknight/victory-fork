//! New memory of ours is resident, and mapped into the card's address
//! space, before anything can name it (`VICTORY_WAIT_RESIDENT=1`; off by
//! default; read once). From avatarmanz/VIctory-7900XTX 0322b39 and
//! f1d3eb2, where it is on by default.
//!
//! A committed resource of the default kind is resident once it is made,
//! but its mapping into the card's address space is work of the system's
//! that runs later: only lists sent after it wait for it. Lists sent before
//! run on, and read our descriptor heaps as they are: a slot written now
//! that shows a texture made now is a page fault for them until the
//! texture is mapped. Their losses of the night of 2026-11-25 were that
//! (the fault lay in a texture made in the same tenth of a second).
//!
//! With the switch such memory is made with `CREATE_NOT_RESIDENT`, made
//! resident at once (`MakeResident`, which waits), and then a copy of a few
//! bytes is sent to a copy queue of ours and waited for: the system starts
//! it only when every mapping asked for before it is done (`MakeResident`
//! alone came back before that: their run n11 was lost as n08 was). A tenth
//! of a millisecond to some per object. Where: host textures that are not
//! reserved ones, depth twins, unordered access buffers (card copies, their
//! hash buffers, scratch on the card) and shared buffers (copies in the
//! PC's memory). Not upload buffers: a list's own, which no list already
//! on the card can reach.

use super::*;
use std::sync::atomic::AtomicU64;

/// Whether the switch is on.
pub(crate) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var_os("VICTORY_WAIT_RESIDENT").is_some_and(|v| v == "1");
        if on {
            tracing::info!(
                "VICTORY_WAIT_RESIDENT=1: new textures and buffers of the card are resident and mapped before anything names \
                 them (a copy queue of our own is waited for after each)"
            );
        }
        on
    })
}

/// The heap flag to make such memory with.
pub(crate) fn flag() -> D3D12_HEAP_FLAGS {
    if on() { D3D12_HEAP_FLAG_CREATE_NOT_RESIDENT } else { D3D12_HEAP_FLAG_NONE }
}

/// Times, microseconds in all, the longest, failures.
static STATS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

/// A copy queue of our own that new memory is waited for with.
struct Aside {
    queue: ID3D12CommandQueue,
    list: ID3D12CommandList,
    fence: ID3D12Fence,
    event: HANDLE,
    value: u64,
    _keep: (ID3D12CommandAllocator, [ID3D12Resource; 2]),
}

// SAFETY: D3D12 objects are free-threaded; the event is waited on by one
// thread at a time (the lock).
unsafe impl Send for Aside {}

/// `object`, made with [`flag`], is resident and mapped when this returns.
/// Does nothing without the switch.
pub(crate) fn wait(device: &ID3D12Device, object: &impl Interface) -> windows::core::Result<()> {
    if !on() {
        return Ok(());
    }
    let started = std::time::Instant::now();
    let done = make_resident(device, object).and_then(|()| mapped(device));
    let us = started.elapsed().as_micros() as u64;
    STATS[0].fetch_add(1, Ordering::Relaxed);
    STATS[1].fetch_add(us, Ordering::Relaxed);
    STATS[2].fetch_max(us, Ordering::Relaxed);
    if done.is_err() {
        STATS[3].fetch_add(1, Ordering::Relaxed);
    }
    let n = STATS[0].load(Ordering::Relaxed);
    if n == 1 || n.is_multiple_of(2000) {
        tracing::info!(
            "new card memory waited for until it is resident and mapped (VICTORY_WAIT_RESIDENT=1), since the start: {n} times, \
             {:.1} ms, the longest {:.2} ms, {} failed",
            STATS[1].load(Ordering::Relaxed) as f64 / 1e3,
            STATS[2].load(Ordering::Relaxed) as f64 / 1e3,
            STATS[3].load(Ordering::Relaxed)
        );
    }
    done
}

fn make_resident(device: &ID3D12Device, object: &impl Interface) -> windows::core::Result<()> {
    let object: ID3D12Pageable = object.cast()?;
    // SAFETY: a live heap or resource of the device's.
    unsafe { device.MakeResident(&[Some(object)]) }
}

/// Every mapping asked for so far is done when this returns.
fn mapped(device: &ID3D12Device) -> windows::core::Result<()> {
    static ASIDE: Mutex<Option<Aside>> = Mutex::new(None);
    let mut aside = ASIDE.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: plain object creation; the objects the list names are kept
    // with it.
    unsafe {
        if aside.is_none() {
            let buffer = |kind: D3D12_HEAP_TYPE, state: D3D12_RESOURCE_STATES| -> windows::core::Result<ID3D12Resource> {
                let mut made: Option<ID3D12Resource> = None;
                device.CreateCommittedResource(
                    &D3D12_HEAP_PROPERTIES { Type: kind, ..Default::default() },
                    D3D12_HEAP_FLAG_NONE,
                    &D3D12_RESOURCE_DESC {
                        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                        Width: 256,
                        Height: 1,
                        DepthOrArraySize: 1,
                        MipLevels: 1,
                        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                        ..Default::default()
                    },
                    state,
                    None,
                    &mut made,
                )?;
                made.ok_or_else(windows::core::Error::empty)
            };
            let from = buffer(D3D12_HEAP_TYPE_UPLOAD, D3D12_RESOURCE_STATE_GENERIC_READ)?;
            let to = buffer(D3D12_HEAP_TYPE_READBACK, D3D12_RESOURCE_STATE_COPY_DEST)?;
            let queue: ID3D12CommandQueue = device.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                Type: D3D12_COMMAND_LIST_TYPE_COPY,
                ..Default::default()
            })?;
            name(&queue, || "victory: new memory waited for".into());
            let allocator: ID3D12CommandAllocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_COPY)?;
            let list: ID3D12GraphicsCommandList = device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_COPY, &allocator, None)?;
            list.CopyBufferRegion(&to, 0, &from, 0, 16);
            list.Close()?;
            *aside = Some(Aside {
                queue,
                list: list.cast()?,
                fence: device.CreateFence(0, D3D12_FENCE_FLAG_NONE)?,
                event: CreateEventW(None, false, false, None)?,
                value: 0,
                _keep: (allocator, [from, to]),
            });
        }
        let Some(a) = aside.as_mut() else {
            return Ok(());
        };
        a.queue.ExecuteCommandLists(&[Some(a.list.clone())]);
        a.value += 1;
        a.queue.Signal(&a.fence, a.value)?;
        if a.fence.GetCompletedValue() < a.value {
            a.fence.SetEventOnCompletion(a.value, a.event)?;
            WaitForSingleObject(a.event, INFINITE);
        }
    }
    Ok(())
}
