//! Indirect execution's arguments, bounded before the GPU reads them. The
//! title's own compute passes write most of them; a pass we drop or
//! translate wrongly leaves whatever the memory held, and a draw of four
//! billion vertices or a dispatch past the host's limits runs until the
//! host resets the display driver. So active records are copied and clamped
//! on the GPU into a device-local scratch buffer, just before execution, by a
//! compute shader of ours (HLSL compiled by the system's compiler; a 5.1
//! shader with its root signature inside), leaving the title's argument and
//! count buffers untouched.
//!
//! When a count buffer is given, threads at or past `min(max, count)` return
//! immediately without charging the list's budget or reading uninitialized
//! tail slots. Each list has a budget (2^27 vertex instances or threads,
//! groups counted as 64 threads, mesh groups as 128): active records past it
//! run empty, saturating without 32-bit wraparound so a list of thousands of
//! garbage records cannot run for seconds either.
//!
//! Where the clamped records go is chosen per list when it begins
//! recording ([`Form`], all off by default): the list's one scratch buffer,
//! with a change of state before and after each clamp (each a wait for all
//! the card's work before it); with `VICTORY_INDIRECT_RING=1`, room of its
//! own in a ring of the list's behind one enhanced barrier; with
//! `VICTORY_INDIRECT_PRELUDE=1` as well, records the list did not write are
//! clamped in its prelude. After fxceless/victory-fork PR #1000.
//!
//! shortcut: the bounds (2^24 vertices, 2^26 vertex instances, 65,535
//! groups per dimension and 2^20 per dispatch, one-dimensional mesh
//! dispatches, the budget) cut legitimate work beyond them; raise them if
//! the title needs more.

use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::ID3DBlob;
use windows::Win32::Graphics::Direct3D12::*;
use windows::core::s;

use crate::host::{HostList, host, transition};

const SOURCE: &str = r#"
#define RS "RootConstants(num32BitConstants=6, b0), SRV(t0), SRV(t1), UAV(u0), UAV(u1)"
cbuffer Record : register(b0) {
    uint kind;
    uint stride;
    uint records;
    uint args_offset;
    // Bit 0: a count buffer bounds the records. Bit 1: the records are a
    // window clamped ahead of their draws: one that asks for more than can
    // run is left empty and costs nothing.
    uint has_count;
    uint count_offset;
};
ByteAddressBuffer src_args : register(t0);
ByteAddressBuffer count_buf : register(t1);
RWByteAddressBuffer dst_args : register(u0);
// The list's work so far (vertex instances, threads): past the budget,
// records run empty.
RWByteAddressBuffer budget : register(u1);
static const uint BUDGET = 1u << 27;
// Groups of one dispatch; more run empty.
static const uint GROUPS = 1u << 20;
[RootSignature(RS)]
// One lane for the clamp of a single record (`VICTORY_INDIRECT_SINGLE=1`):
// each lane charges the budget on its own, so the code is the same.
#ifdef SINGLE_RECORD
[numthreads(1, 1, 1)]
#else
[numthreads(64, 1, 1)]
#endif
void main(uint3 id : SV_DispatchThreadID) {
    uint eff_count = records;
    bool ahead = (has_count & 2u) != 0;
    if ((has_count & 1u) != 0) {
        eff_count = min(eff_count, count_buf.Load(count_offset));
    }
    if (id.x >= eff_count) return;
    uint src_at = args_offset + id.x * stride;
    uint dst_at = id.x * stride;
    uint cost;
    uint2 draw_counts = uint2(0, 0);
    uint3 groups = uint3(0, 0, 0);
    if (kind <= 1) {
        // Draws: vertex or index count, then instances.
        uint2 asked = src_args.Load2(src_at);
        if (ahead && (asked.x > (1u << 24) || asked.y > (1u << 24))) {
            asked = uint2(0, 0);
        }
        uint count = min(asked.x, 1u << 24);
        uint instances = min(asked.y, (1u << 26) / max(count, 1u));
        cost = count * instances;
        draw_counts = uint2(count, instances);
    } else if (kind == 2) {
        uint3 asked = src_args.Load3(src_at);
        // Diagnostics, in the budget buffer's words after the mesh ones:
        // the largest x a compute record asked for, and the records that
        // asked for more groups in x than one dispatch runs (they are cut
        // to 65,535 below, so the groups past it never run). Not the
        // garbage of a slot nothing wrote.
        if (asked.x <= (1u << 24) && asked.y <= 64u && asked.z <= 64u) {
            uint noted;
            budget.InterlockedMax(12, asked.x, noted);
            if (asked.x > 65535u) {
                budget.InterlockedAdd(16, 1u, noted);
            }
        }
        uint3 g = min(asked, uint3(65535u, 65535u, 65535u));
        // x * y fits 32 bits; the whole count may not.
        if (g.z != 0 && g.x * g.y > GROUPS / g.z) {
            g = uint3(0, 0, 0);
        }
        cost = g.x * g.y * g.z * 64u;
        groups = g;
    } else {
        // Mesh dispatches: the console's are one-dimensional.
        uint3 g = src_args.Load3(src_at);
        // Ahead of its draw, a record over the most a dispatch runs is a
        // slot the title has not filled (the title leaves all
        // ones there): empty, and not counted below either.
        if (ahead && g.x > 65535u) {
            g = uint3(0, 0, 0);
        }
        // Diagnostics, in the budget buffer's next words: the largest
        // group count a mesh record asked for, and the groups asked in all.
        uint was;
        budget.InterlockedMax(4, g.x, was);
        budget.InterlockedAdd(8, min(g.x, 1u << 24), was);
        g = uint3(min(g.x, 65535u), min(g.y, 1u), min(g.z, 1u));
        cost = g.x * g.y * g.z * 128u;
        groups = g;
    }
    bool over = false;
    if (cost > 0) {
        uint before;
        budget.InterlockedOr(0, 0, before);
        [allow_uav_condition]
        while (before < BUDGET) {
            uint next = (BUDGET - before <= cost) ? BUDGET : (before + cost);
            uint prev;
            budget.InterlockedCompareExchange(0, before, next, prev);
            if (prev == before) break;
            before = prev;
        }
        over = before >= BUDGET || BUDGET - before < cost;
    }
    if (kind <= 1) {
        if (over) {
            draw_counts = uint2(0, 0);
        }
        dst_args.Store2(dst_at, draw_counts);
        for (uint off = 8; off < stride; off += 4) {
            dst_args.Store(dst_at + off, src_args.Load(src_at + off));
        }
    } else {
        if (over) {
            groups = uint3(0, 0, 0);
        }
        dst_args.Store3(dst_at, groups);
        for (uint off = 12; off < stride; off += 4) {
            dst_args.Store(dst_at + off, src_args.Load(src_at + off));
        }
    }
}
// The budget and the notes after it, zeroed on the card before a list's
// first clamp when the budget is in the card's memory.
[RootSignature(RS)]
[numthreads(1, 1, 1)]
void zero() {
    budget.Store4(0, uint4(0, 0, 0, 0));
    budget.Store(16, 0u);
}
"#;

struct Clamp {
    root: ID3D12RootSignature,
    pso: ID3D12PipelineState,
    /// The `zero` entry: a budget in the card's memory zeroed.
    zero: ID3D12PipelineState,
    /// The lanes of a group of `main`: 64, or 1 for the single-record clamp.
    lanes: u32,
}

// SAFETY: D3D12 objects are free-threaded.
unsafe impl Send for Clamp {}
// SAFETY: as above.
unsafe impl Sync for Clamp {}

fn clamp() -> Option<&'static Clamp> {
    static CLAMP: std::sync::OnceLock<Option<Clamp>> = std::sync::OnceLock::new();
    CLAMP
        .get_or_init(|| match make() {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!(error = %e, "no argument clamp: indirect execution dropped");
                None
            }
        })
        .as_ref()
}

fn make() -> windows::core::Result<Clamp> {
    let device = &host().ok_or_else(windows::core::Error::empty)?.device;
    make_for(device)
}

fn make_for(device: &ID3D12Device) -> windows::core::Result<Clamp> {
    make_from(device, SOURCE, 64)
}

/// The clamp of one record in one lane ([`clamp_for`]), as fxceless/
/// victory-fork PR #1016 has it. Our clamp charges the budget lane by lane
/// (no sums over the group), so the one-lane clamp is the same code in a
/// group of one.
fn make_single_for(device: &ID3D12Device) -> windows::core::Result<Clamp> {
    make_from(device, &format!("#define SINGLE_RECORD\n{SOURCE}"), 1)
}

/// The clamp compiled from `source`, with `lanes` lanes to a group of `main`.
fn make_from(device: &ID3D12Device, source: &str, lanes: u32) -> windows::core::Result<Clamp> {
    let main = compile(source, s!("main"))?;
    let zero = compile(source, s!("zero"))?;
    // SAFETY: the blobs' own bytes, alive through the calls; the root
    // signature is inside the shader.
    unsafe {
        let (main, zero) = (blob_bytes(&main), blob_bytes(&zero));
        let root: ID3D12RootSignature = device.CreateRootSignature(0, main)?;
        let pso: ID3D12PipelineState = device.CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
            pRootSignature: std::mem::transmute_copy(&root),
            CS: D3D12_SHADER_BYTECODE {
                pShaderBytecode: main.as_ptr().cast(),
                BytecodeLength: main.len(),
            },
            ..Default::default()
        })?;
        let zero: ID3D12PipelineState = device.CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
            pRootSignature: std::mem::transmute_copy(&root),
            CS: D3D12_SHADER_BYTECODE {
                pShaderBytecode: zero.as_ptr().cast(),
                BytecodeLength: zero.len(),
            },
            ..Default::default()
        })?;
        Ok(Clamp { root, pso, zero, lanes })
    }
}

/// The one-lane clamp ([`make_single_for`]), made at its first use; `None`
/// if it does not compile (the clamp of 64 lanes is used then).
fn single_clamp() -> Option<&'static Clamp> {
    static SINGLE: std::sync::OnceLock<Option<Clamp>> = std::sync::OnceLock::new();
    SINGLE
        .get_or_init(|| match make_single_for(&host()?.device) {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!(error = %e, "no single-record argument clamp (VICTORY_INDIRECT_SINGLE=1): the clamp of 64 lanes is used");
                None
            }
        })
        .as_ref()
}

/// The clamp for `records` records on `list`: the one-lane clamp for a
/// single record when the list was begun with [`Form::single`].
fn clamp_for(list: &HostList, records: u32) -> Option<&'static Clamp> {
    if records == 1
        && list.indirect_form().single
        && let Some(one) = single_clamp()
    {
        tally(S_SINGLE, 1);
        return Some(one);
    }
    clamp()
}

/// The bytes of a compiled blob.
///
/// # Safety
/// The slice must not outlive `code`.
unsafe fn blob_bytes(code: &ID3DBlob) -> &[u8] {
    // SAFETY: the blob's own buffer, as long as the caller keeps it.
    unsafe { std::slice::from_raw_parts(code.GetBufferPointer().cast::<u8>(), code.GetBufferSize()) }
}

/// `source`'s entry `entry`, compiled.
fn compile(source: &str, entry: windows::core::PCSTR) -> windows::core::Result<ID3DBlob> {
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    // SAFETY: the source and names live through the call; out pointers ours.
    let compiled = unsafe {
        D3DCompile(
            source.as_ptr().cast(),
            source.len(),
            s!("clamp"),
            None,
            None,
            entry,
            s!("cs_5_1"),
            0,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(e) = compiled {
        if let Some(errors) = errors {
            // SAFETY: the blob's own bytes.
            let text = unsafe {
                std::slice::from_raw_parts(
                    errors.GetBufferPointer().cast::<u8>(),
                    errors.GetBufferSize(),
                )
            };
            tracing::warn!("argument clamp: {}", String::from_utf8_lossy(text));
        }
        return Err(e);
    }
    code.ok_or_else(windows::core::Error::empty)
}

/// Minimum record byte stride for a supported PC indirect argument `kind`.
fn min_stride(kind: u32) -> Option<u32> {
    match kind {
        0 => Some(16),
        1 => Some(20),
        2 | 10 => Some(12),
        _ => None,
    }
}

/// Parses `VICTORY_INDIRECT_CLAMP`: enabled by default; explicit `"0"`,
/// `"false"`, `"no"`, or `"off"` (ASCII case-insensitive) bypasses the
/// argument-clamp compute pass so `ExecuteIndirect` reads the title's
/// argument buffer directly.
pub(crate) fn parse_clamp_enabled(value: Option<&std::ffi::OsStr>) -> bool {
    let Some(value) = value else {
        return true;
    };
    let Some(text) = value.to_str() else {
        return true;
    };
    let trimmed = text.trim();
    !(trimmed == "0"
        || trimmed.eq_ignore_ascii_case("false")
        || trimmed.eq_ignore_ascii_case("no")
        || trimmed.eq_ignore_ascii_case("off"))
}

/// Whether indirect arguments are copied and clamped by our compute shader
/// before `ExecuteIndirect` (`VICTORY_INDIRECT_CLAMP=0` disables it).
pub(crate) fn clamp_enabled() -> bool {
    // `clamp=0` in the live file (experiment): for timing only, the title's
    // own fatal path can follow (see the performance notes).
    if let Some(v) = crate::host::live::value("clamp") {
        return parse_clamp_enabled(Some(std::ffi::OsStr::new(&v)));
    }
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| parse_clamp_enabled(std::env::var_os("VICTORY_INDIRECT_CLAMP").as_deref()))
}

/// `VICTORY_MESH_GUARD=1` (off by default; read once, logged once): an
/// indirect mesh record that asks for more than 65,535 groups is not one
/// the title wrote (no dispatch of the console runs more; the title leaves
/// all ones in a slot it has not filled) and runs empty, in the clamp pass
/// (as for a window clamped ahead) and in the amplification stage
/// (`crate::pipeline::host::amplifier`), instead of 65,535 groups.
///
/// Why: the three losses of the card on 2026-11-25 (01:48, 17:09 and
/// 21:06 UTC) were all shaders' page faults ("MMU Fault" on SMs of every
/// GPC, the same SM error registers each time), all with
/// `VICTORY_INDIRECT_AMPLIFY=1`, and each while the layer was making
/// pipelines it did not have (draws and dispatches dropped meanwhile). A
/// mesh draw whose record the dropped culling pass never wrote runs 65,535
/// groups of a mesh shader that reads its meshlets by group number, far
/// past the ends of its buffers, through root views that have no bounds.
pub(crate) fn mesh_guard() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var_os("VICTORY_MESH_GUARD").is_some_and(|v| v == "1");
        if on {
            tracing::info!("VICTORY_MESH_GUARD=1: an indirect mesh record of more than 65,535 groups runs empty (clamp pass and amplification stage)");
        }
        on
    })
}

/// Whether the clamp treats records of `kind` as a window clamped ahead
/// (bit 1 of its `has_count`): asked by the caller, or, with `guard`
/// ([`mesh_guard`]), every mesh record (kind 3 and up). Pure, for the tests.
fn clamped_as_ahead(kind: u32, ahead: bool, guard: bool) -> bool {
    ahead || (guard && kind >= 3)
}

/// Validates record alignment and returns the scratch byte size `stride * records`.
pub(crate) fn validate_layout(
    (kind, stride, records): (u32, u32, u32),
    src_offset: u64,
    count_offset: Option<u64>,
) -> Option<u64> {
    let min = min_stride(kind)?;
    if stride < min || !stride.is_multiple_of(4) || !src_offset.is_multiple_of(4) {
        return None;
    }
    if count_offset.is_some_and(|off| !off.is_multiple_of(4)) {
        return None;
    }
    let bytes = u64::from(stride).checked_mul(u64::from(records))?;
    (bytes <= u64::from(u32::MAX)).then_some(bytes)
}

/// Records, in `list`, the non-destructive clamp of up to `records` records of
/// `kind` (PC argument numbering) `stride` bytes apart from `src` (and bounded
/// by the 32-bit count in `count` when given) into device-local memory of the
/// list's: room of its own in the list's ring when the list was begun with
/// [`Form::ring`] (behind one enhanced barrier, [`record_clamp_ring`]), or
/// else its one scratch buffer at offset `0`, left in `INDIRECT_ARGUMENT`.
/// Returns the buffer and where the records start in it. Replaces the list's
/// compute root signature and pipeline (callers bind theirs after). `None`
/// if the clamp or alignment is not available.
pub(crate) fn bound(
    list: &HostList,
    src: (&ID3D12Resource, u64),
    count: Option<(&ID3D12Resource, u64)>,
    spec: (u32, u32, u32),
) -> Option<(ID3D12Resource, u64)> {
    clamp_to(list, src, count, spec, false)
}

/// [`bound`] and [`bound_ahead`] (`ahead`).
fn clamp_to(
    list: &HostList,
    src: (&ID3D12Resource, u64),
    count: Option<(&ID3D12Resource, u64)>,
    spec: (u32, u32, u32),
    ahead: bool,
) -> Option<(ID3D12Resource, u64)> {
    let bytes = validate_layout(spec, src.1, count.map(|(_, off)| off))?;
    let c = clamp_for(list, spec.2)?;
    let budget = budget_of(c, list)?;
    if list.indirect_form().ring
        && let Some(l7) = list.list7()
    {
        match list.indirect_ring(bytes) {
            Some((dst, at)) => {
                return record_clamp_ring(c, &list.list, l7, (&dst, at), budget, src, count, spec, ahead)
                    .then_some((dst, at));
            }
            // No ring buffer (the card's memory is full): this clamp goes
            // to the scratch buffer, with its changes of state, as without
            // the ring. The two buffers are apart, so neither sees barriers
            // of the other kind.
            None => count_fallback(F_NO_RING_BUFFER),
        }
    }
    let dst = list.indirect_scratch(bytes)?;
    tally(S_SCRATCH, 1);
    let recorded = from_state_of(list, &dst, || record_clamp_as(c, &list.list, &dst, budget, src, count, spec, ahead));
    recorded.then_some((dst, 0))
}

/// Whether the scratch buffer stays in `INDIRECT_ARGUMENT` after an
/// execution instead of going back to `COMMON` (`VICTORY_INDIRECT_LAZY=1`,
/// experiment, off: no fps difference measured, 7.4-9.7 on against 8.5-9.6 off). Each of the ~1,300 indirect
/// dispatches a frame had three barrier calls of ours around it (before the
/// clamp, after it, after the execution); on NVIDIA a GPU Trace of the
/// opening cutscene counted 5,000-6,100 pipeline drains ("go idle") a frame
/// with the shader units 5-8% busy. Buffers decay to `COMMON` when their
/// submission ends, so a list that ends with the buffer readable needs no
/// barrier of its own; within the list the next clamp starts from the state
/// noted on the list.
fn lazy() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_INDIRECT_LAZY").is_some_and(|v| v == "1"))
}

thread_local! {
    /// The state the scratch buffer is in when [`record_clamp_as`] starts.
    static CLAMP_FROM: std::cell::Cell<D3D12_RESOURCE_STATES> = const { std::cell::Cell::new(D3D12_RESOURCE_STATE_COMMON) };
}

/// Runs `record` with the clamp starting from the state `dst` is in on `list`.
fn from_state_of(list: &HostList, dst: &ID3D12Resource, record: impl FnOnce() -> bool) -> bool {
    let readable = list.take_indirect_readable(dst);
    CLAMP_FROM.set(if readable { D3D12_RESOURCE_STATE_INDIRECT_ARGUMENT } else { D3D12_RESOURCE_STATE_COMMON });
    let recorded = record();
    CLAMP_FROM.set(D3D12_RESOURCE_STATE_COMMON);
    if !recorded && readable {
        // Nothing recorded: still readable.
        list.indirect_left_readable(dst);
    }
    recorded
}

/// Whether the clamp's budget is in the card's own memory
/// (`VICTORY_INDIRECT_CARD_BUDGET=0`: in the PC's, as before). There every
/// clamp's atomics on it (one to four a record, a compare-and-swap loop
/// among them) cross the bus, and the draw or dispatch after the clamp
/// waits for them: in a PIX capture of the opening cutscene ~1,300 clamps
/// a frame took ~4.3 ms, 1.6 us for a dispatch record, 7 us for a draw.
/// As fxceless/victory-fork PR #220 (`d5c15cb`).
pub(crate) fn card_budget() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_INDIRECT_CARD_BUDGET").is_none_or(|v| v != "0"))
}

/// The budget the clamps of `list` charge: in the card's memory
/// ([`card_budget`]), zeroed by a dispatch recorded before the list's first
/// clamp, or else in the PC's, zeroed by the CPU when the list is
/// submitted ([`HostList::budget`]).
///
/// A list begun with [`Form::prelude`] has its budget zeroed in its prelude
/// instead, before the clamps there ([`record_early`]): zeroed in the list
/// it would forget what they charged.
fn budget_of(c: &Clamp, list: &HostList) -> Option<u64> {
    if card_budget()
        && let Some((at, first)) = list.card_budget()
    {
        if first {
            if list.indirect_form().prelude {
                list.zero_budget_in_prelude(at);
            } else {
                record_zero(c, &list.list, at);
            }
        }
        return Some(at);
    }
    list.budget()
}

/// Records the zeroing of the budget at `budget` (and its notes), then a
/// wait so that the clamps after it charge the zeroed words. Replaces the
/// list's compute root signature and pipeline, as a clamp does.
fn record_zero(c: &Clamp, l: &ID3D12GraphicsCommandList, budget: u64) {
    let constants = [0u32; 6];
    // SAFETY: an open list; our root signature and pipeline; every root
    // argument names the live budget buffer (the `zero` entry writes only
    // `budget`, 20 bytes of its 256).
    unsafe {
        l.SetComputeRootSignature(&c.root);
        l.SetPipelineState(&c.zero);
        l.SetComputeRoot32BitConstants(0, 6, constants.as_ptr().cast(), 0);
        l.SetComputeRootShaderResourceView(1, budget);
        l.SetComputeRootShaderResourceView(2, budget);
        l.SetComputeRootUnorderedAccessView(3, budget);
        l.SetComputeRootUnorderedAccessView(4, budget);
        l.Dispatch(1, 1, 1);
        l.ResourceBarrier(&[uav_barrier()]);
    }
}

/// Ensures earlier compute writes to indirect argument/count buffers are
/// visible before a direct (unclamped) `ExecuteIndirect`, without running
/// the clamp compute shader or invalidating compute pipeline state.
pub(crate) fn wait_direct(list: &HostList) {
    // SAFETY: an open command list; global UAV barrier has no resource hazards.
    unsafe { list.list.ResourceBarrier(&[uav_barrier()]) };
}

/// [`bound`] for a window of records clamped ahead of the draws that will
/// read them (`crate::draw`'s windows; no count buffer). The records past
/// the ones the title filled are whatever the buffer holds, and each
/// record clamped charges the list's budget when it is clamped: a slot of
/// all ones, clamped to the most a mesh dispatch runs (65,535 groups),
/// charged a sixteenth of the budget, so sixteen of them emptied every
/// draw of the list after them (run 93: "the largest group count asked for
/// 4294967295", 43 to 153 such records a frame). Here a record that asks
/// for more than can run is left empty and charges nothing; a record
/// clamped alone is clamped and charged as before.
pub(crate) fn bound_ahead(
    list: &HostList,
    src: (&ID3D12Resource, u64),
    spec: (u32, u32, u32),
) -> Option<(ID3D12Resource, u64)> {
    clamp_to(list, src, None, spec, true)
}

#[cfg(test)]
fn record_clamp(
    c: &Clamp,
    l: &ID3D12GraphicsCommandList,
    dst: &ID3D12Resource,
    budget: u64,
    src: (&ID3D12Resource, u64),
    count: Option<(&ID3D12Resource, u64)>,
    spec: (u32, u32, u32),
) -> bool {
    record_clamp_as(c, l, dst, budget, src, count, spec, false)
}

/// The clamp; `ahead`: the records are a window clamped ahead of their
/// draws ([`bound_ahead`]). Into the list's one scratch buffer, from
/// the state [`CLAMP_FROM`] says it is in, with the changes of state around
/// the pass (each a wait for all the card's work before it).
#[allow(clippy::too_many_arguments)]
fn record_clamp_as(
    c: &Clamp,
    l: &ID3D12GraphicsCommandList,
    dst: &ID3D12Resource,
    budget: u64,
    src: (&ID3D12Resource, u64),
    count: Option<(&ID3D12Resource, u64)>,
    spec: (u32, u32, u32),
    ahead: bool,
) -> bool {
    // SAFETY: a live D3D12 buffer resource.
    let dst_gpu = unsafe { dst.GetGPUVirtualAddress() };
    let Some((constants, src_gpu, count_gpu)) = clamp_arguments(dst_gpu, budget, src, count, spec, ahead) else {
        return false;
    };
    // SAFETY: an open list; our root signature and pipeline; live source,
    // count, scratch destination, and budget buffers. `src` and `count` stay
    // in `COMMON` / `GENERIC_READ` via implicit buffer promotion while `dst`
    // transitions `COMMON -> UNORDERED_ACCESS -> INDIRECT_ARGUMENT`.
    // Diagnostics: counted; `VICTORY_BARRIER_TEST=indirect` leaves the two
    // waits for every earlier shader write out (the state changes stay).
    #[cfg(feature = "diagnostics")]
    crate::copystat::add(crate::copystat::C::IndirectBounded, 1);
    let first = [
        uav_barrier(),
        transition(
            dst,
            CLAMP_FROM.get(),
            D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
        ),
    ];
    let second = [
        uav_barrier(),
        transition(
            dst,
            D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
            D3D12_RESOURCE_STATE_INDIRECT_ARGUMENT,
        ),
    ];
    let skip = usize::from(!waits_now());
    unsafe {
        // Earlier shader writes (the title's argument and count passes) first.
        l.ResourceBarrier(&first[skip..]);
        dispatch_clamp(c, l, &constants, src_gpu, count_gpu, dst_gpu, budget);
        l.ResourceBarrier(&second[skip..]);
    }
    true
}

/// Whether the clamp being recorded waits for every earlier shader write
/// before its pass: not with `VICTORY_BARRIER_TEST=indirect` (diagnostics),
/// `VICTORY_CLAMP_WAIT=never`, or when `VICTORY_CLAMP_WAIT=auto` found no
/// shader that may have written since the last wait ([`needs_wait`]).
fn waits_now() -> bool {
    #[cfg(feature = "diagnostics")]
    let waits = crate::copystat::barrier_test() & 2 == 0;
    #[cfg(not(feature = "diagnostics"))]
    let waits = true;
    waits && NEEDS_WAIT.get() && !wait_never()
}

/// The clamp pass's root constants and its source and count addresses, if
/// the records can be clamped (4-byte aligned buffers and offsets).
fn clamp_arguments(
    dst_gpu: u64,
    budget: u64,
    (src, src_offset): (&ID3D12Resource, u64),
    count: Option<(&ID3D12Resource, u64)>,
    (kind, stride, records): (u32, u32, u32),
    ahead: bool,
) -> Option<([u32; 6], u64, u64)> {
    let bytes = validate_layout((kind, stride, records), src_offset, count.map(|(_, off)| off))?;
    // SAFETY: a live D3D12 buffer resource.
    let src_base = unsafe { src.GetGPUVirtualAddress() };
    if !src_base.is_multiple_of(4) || !dst_gpu.is_multiple_of(4) || !budget.is_multiple_of(4) {
        return None;
    }
    let (src_gpu, args_offset) = match src_offset.checked_add(bytes).filter(|&end| end <= u64::from(u32::MAX)) {
        Some(_) => (src_base, src_offset as u32),
        None => (src_base.checked_add(src_offset)?, 0),
    };
    let (count_gpu, has_count, count_offset) = match count {
        Some((buf, off)) => {
            // SAFETY: a live D3D12 buffer resource.
            let base = unsafe { buf.GetGPUVirtualAddress() };
            if !base.is_multiple_of(4) {
                return None;
            }
            match off.checked_add(4).filter(|&end| end <= u64::from(u32::MAX)) {
                Some(_) => (base, 1u32, off as u32),
                None => (base.checked_add(off)?, 1u32, 0u32),
            }
        }
        None => (src_gpu, 0u32, 0u32),
    };
    let ahead = clamped_as_ahead(kind, ahead, mesh_guard());
    let constants = [kind, stride, records, args_offset, has_count | (u32::from(ahead) << 1), count_offset];
    Some((constants, src_gpu, count_gpu))
}

/// Records the clamp pass itself (nothing if there are no records).
///
/// # Safety
/// An open list; live buffers behind the addresses.
unsafe fn dispatch_clamp(
    c: &Clamp,
    l: &ID3D12GraphicsCommandList,
    constants: &[u32; 6],
    src_gpu: u64,
    count_gpu: u64,
    dst_gpu: u64,
    budget: u64,
) {
    let records = constants[2];
    if records == 0 {
        return;
    }
    // SAFETY: as the caller vouches.
    unsafe {
        l.SetComputeRootSignature(&c.root);
        l.SetPipelineState(&c.pso);
        l.SetComputeRoot32BitConstants(0, 6, constants.as_ptr().cast(), 0);
        l.SetComputeRootShaderResourceView(1, src_gpu);
        l.SetComputeRootShaderResourceView(2, count_gpu);
        l.SetComputeRootUnorderedAccessView(3, dst_gpu);
        l.SetComputeRootUnorderedAccessView(4, budget);
        l.Dispatch(records.div_ceil(c.lanes), 1, 1);
    }
}

/// After indirect execution (or if preparation failed after [`bound`]):
/// returns the scratch buffer to the common state for the next call. Room
/// in the list's ring needs nothing: no clamp writes it again before the
/// list is reset.
pub(crate) fn done(list: &HostList, scratch: &ID3D12Resource) {
    if list.indirect_form().ring && list.in_ring(scratch) {
        return;
    }
    if lazy() {
        list.indirect_left_readable(scratch);
        return;
    }
    done_on(&list.list, scratch);
}

fn done_on(list: &ID3D12GraphicsCommandList, scratch: &ID3D12Resource) {
    // SAFETY: an open list and a live scratch buffer in INDIRECT_ARGUMENT.
    unsafe {
        list.ResourceBarrier(&[transition(
            scratch,
            D3D12_RESOURCE_STATE_INDIRECT_ARGUMENT,
            D3D12_RESOURCE_STATE_COMMON,
        )])
    };
}

// ---------------------------------------------------------------------------
// The ring, the prelude and the one-lane clamp, as fxceless/victory-fork PRs
// #1000 and #1016 have them, ported onto this tree's clamp (budget on the
// card, `VICTORY_CLAMP_WAIT`, `VICTORY_INDIRECT_LAZY`, the ahead windows).

/// How a host list clamps its indirect records: chosen once, when the list
/// begins recording ([`form_for`]), and kept in the list
/// (`HostList::indirect_form`), so that a switch changed while the game
/// runs never mixes two ways in one list. All three are off by default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Form {
    /// `VICTORY_INDIRECT_RING=1` (live: `indirect_ring=1`): each clamp
    /// writes room of its own in a ring of the list's, behind one enhanced
    /// barrier, instead of the one scratch buffer and its two changes of
    /// state. Only where the host takes enhanced barriers.
    pub(crate) ring: bool,
    /// `VICTORY_INDIRECT_PRELUDE=1` (live: `indirect_prelude=1`), with the
    /// ring: clamps of records no shader of the list may have written
    /// before the command run in the list's prelude.
    pub(crate) prelude: bool,
    /// `VICTORY_INDIRECT_SINGLE=1` (live: `indirect_single=1`): a single
    /// record is clamped by a group of one lane.
    pub(crate) single: bool,
}

impl Form {
    pub(crate) fn bits(self) -> u8 {
        u8::from(self.ring) | (u8::from(self.prelude) << 1) | (u8::from(self.single) << 2)
    }

    pub(crate) fn from_bits(bits: u8) -> Form {
        Form { ring: bits & 1 != 0, prelude: bits & 2 != 0, single: bits & 4 != 0 }
    }
}

/// A switch of [`Form`]'s: the live file's `key` while it has the line
/// (`crate::host::live`), else the variable `var` as the process started
/// (`1`: on).
fn switch(key: &str, var: &'static str, at_start: &std::sync::OnceLock<bool>) -> bool {
    if let Some(v) = crate::host::live::value(key) {
        return v.trim() == "1";
    }
    *at_start.get_or_init(|| std::env::var_os(var).is_some_and(|v| v == "1"))
}

/// The [`Form`] of a list that begins recording now; `list7`: whether the
/// list takes enhanced barriers (`ID3D12GraphicsCommandList7`). What was
/// asked and what the list got are counted for the frame line, and a change
/// of what is asked is said once.
pub(crate) fn form_for(list7: bool) -> Form {
    static RING: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    static PRELUDE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    static SINGLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let asked = Form {
        ring: switch("indirect_ring", "VICTORY_INDIRECT_RING", &RING),
        prelude: switch("indirect_prelude", "VICTORY_INDIRECT_PRELUDE", &PRELUDE),
        single: switch("indirect_single", "VICTORY_INDIRECT_SINGLE", &SINGLE),
    };
    static SAID: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(u8::MAX);
    if SAID.swap(asked.bits(), Relaxed) != asked.bits() {
        tracing::info!(
            ring = asked.ring,
            prelude = asked.prelude,
            single = asked.single,
            "indirect clamps of the lists begun from now: ring (VICTORY_INDIRECT_RING / live indirect_ring), prelude \
             (VICTORY_INDIRECT_PRELUDE / live indirect_prelude), single-record clamp (VICTORY_INDIRECT_SINGLE / live indirect_single)"
        );
    }
    let mut ring = asked.ring;
    if ring && !enhanced_barriers() {
        ring = false;
        count_fallback(F_NO_ENHANCED);
    }
    if ring && !list7 {
        ring = false;
        count_fallback(F_NO_LIST7);
    }
    if asked.prelude && !ring {
        count_fallback(F_PRELUDE_NO_RING);
    }
    tally(if ring { S_LISTS_RING } else { S_LISTS_SCRATCH }, 1);
    Form { ring, prelude: asked.prelude && ring, single: asked.single }
}

/// Whether the host takes barriers of the enhanced kind
/// (`D3D12_FEATURE_D3D12_OPTIONS12.EnhancedBarriersSupported`); asked once.
pub(crate) fn enhanced_barriers() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        let Some(host) = host() else {
            return false;
        };
        enhanced_on(&host.device)
    })
}

/// [`enhanced_barriers`] for `device`, said in the log.
fn enhanced_on(device: &ID3D12Device) -> bool {
    let mut options = D3D12_FEATURE_DATA_D3D12_OPTIONS12::default();
    // SAFETY: a plain query into a struct of the size given.
    let asked = unsafe {
        device.CheckFeatureSupport(
            D3D12_FEATURE_D3D12_OPTIONS12,
            (&raw mut options).cast(),
            size_of::<D3D12_FEATURE_DATA_D3D12_OPTIONS12>() as u32,
        )
    };
    let on = asked.is_ok() && options.EnhancedBarriersSupported.as_bool();
    if on {
        tracing::info!("the host takes enhanced barriers: VICTORY_INDIRECT_RING=1 can clamp indirect records into rings");
    } else {
        tracing::warn!(
            query = ?asked.err(),
            "the host does not take enhanced barriers: VICTORY_INDIRECT_RING=1 and VICTORY_INDIRECT_PRELUDE=1 do nothing, \
             indirect records are clamped into the scratch buffer as before"
        );
    }
    on
}

/// Records one barrier of the enhanced kind on the whole of buffer `dst`:
/// work of stage `before.0` that accessed it as `before.1` is done, and its
/// writes are seen, before work of stage `after.0` accesses it as `after.1`.
fn buffer_barrier(
    l7: &ID3D12GraphicsCommandList7,
    dst: &ID3D12Resource,
    before: (D3D12_BARRIER_SYNC, D3D12_BARRIER_ACCESS),
    after: (D3D12_BARRIER_SYNC, D3D12_BARRIER_ACCESS),
) {
    let barrier = [ring_barrier(dst, before, after)];
    let group = D3D12_BARRIER_GROUP {
        Type: D3D12_BARRIER_TYPE_BUFFER,
        NumBarriers: 1,
        Anonymous: D3D12_BARRIER_GROUP_0 { pBufferBarriers: barrier.as_ptr() },
    };
    // SAFETY: an open list; the barrier names a live buffer.
    unsafe { l7.Barrier(&[group]) };
    let [b] = barrier;
    drop(std::mem::ManuallyDrop::into_inner(b.pResource));
}

/// A buffer barrier on the whole of `dst` (enhanced barriers take offset 0
/// and size `u64::MAX` for a buffer). The caller drops its reference.
fn ring_barrier(
    dst: &ID3D12Resource,
    before: (D3D12_BARRIER_SYNC, D3D12_BARRIER_ACCESS),
    after: (D3D12_BARRIER_SYNC, D3D12_BARRIER_ACCESS),
) -> D3D12_BUFFER_BARRIER {
    D3D12_BUFFER_BARRIER {
        SyncBefore: before.0,
        SyncAfter: after.0,
        AccessBefore: before.1,
        AccessAfter: after.1,
        pResource: std::mem::ManuallyDrop::new(Some(dst.clone())),
        Offset: 0,
        Size: u64::MAX,
    }
}

/// The clamp's writes to the ring, then the indirect read of them.
const CLAMP_WRITES: (D3D12_BARRIER_SYNC, D3D12_BARRIER_ACCESS) =
    (D3D12_BARRIER_SYNC_COMPUTE_SHADING, D3D12_BARRIER_ACCESS_UNORDERED_ACCESS);
const INDIRECT_READ: (D3D12_BARRIER_SYNC, D3D12_BARRIER_ACCESS) =
    (D3D12_BARRIER_SYNC_EXECUTE_INDIRECT, D3D12_BARRIER_ACCESS_INDIRECT_ARGUMENT);

/// The clamp into room `at` of ring `dst`, which nothing recorded before
/// reads ([`Form::ring`]): no change of state before the pass (only the
/// wait for earlier shader writes of the title's, as into the scratch
/// buffer, [`waits_now`]), and one barrier after it from the compute
/// stage's writes to the indirect read.
///
/// The ring is never named by a barrier of the legacy kind: it is made as
/// the scratch buffer is (a buffer of the default heap in `COMMON`, as
/// fxceless/victory-fork PR #1000 makes its ring), and a buffer has no
/// layout, so the enhanced barriers alone order its uses. The scratch
/// buffer keeps its legacy changes of state; the two are apart.
#[allow(clippy::too_many_arguments)]
fn record_clamp_ring(
    c: &Clamp,
    l: &ID3D12GraphicsCommandList,
    l7: &ID3D12GraphicsCommandList7,
    (dst, at): (&ID3D12Resource, u64),
    budget: u64,
    src: (&ID3D12Resource, u64),
    count: Option<(&ID3D12Resource, u64)>,
    spec: (u32, u32, u32),
    ahead: bool,
) -> bool {
    // SAFETY: a live D3D12 buffer resource.
    let dst_gpu = unsafe { dst.GetGPUVirtualAddress() } + at;
    let Some((constants, src_gpu, count_gpu)) = clamp_arguments(dst_gpu, budget, src, count, spec, ahead) else {
        return false;
    };
    #[cfg(feature = "diagnostics")]
    crate::copystat::add(crate::copystat::C::IndirectBounded, 1);
    // SAFETY: an open list; our root signature and pipeline; live source,
    // count, ring and budget buffers.
    unsafe {
        if waits_now() {
            l.ResourceBarrier(&[uav_barrier()]);
        }
        dispatch_clamp(c, l, &constants, src_gpu, count_gpu, dst_gpu, budget);
    }
    buffer_barrier(l7, dst, CLAMP_WRITES, INDIRECT_READ);
    tally(S_RING, 1);
    tally(S_ENHANCED, 1);
    true
}

/// A clamp noted for a list's prelude ([`bound_early`]): the clamp, its root
/// constants, and its source, count, ring and budget addresses.
pub(crate) struct Early {
    clamp: &'static Clamp,
    constants: [u32; 6],
    src: u64,
    count: u64,
    dst: u64,
    budget: u64,
}

/// Why a list begun with [`Form::prelude`] clamps a record in the list
/// rather than in its prelude (counted for the frame line).
pub(crate) enum InList {
    /// A shader recorded before in the list may write the records, by the
    /// writes the list's shaders declare.
    Written,
    /// A count buffer bounds the records (it may be written in the list).
    Counted,
    /// The records are title memory in place (`VICTORY_INPLACE`): what its
    /// shaders write is not declared.
    InPlace,
}

/// Counts a record clamped in the list instead of its prelude.
pub(crate) fn in_list(why: InList) {
    tally(
        match why {
            InList::Written => S_IN_LIST_WRITTEN,
            InList::Counted => S_IN_LIST_COUNTED,
            InList::InPlace => S_IN_LIST_IN_PLACE,
        },
        1,
    );
}

/// [`bound`] or [`bound_ahead`] (`ahead`) of records the list has not
/// written before the command ([`Form::prelude`]): room in the list's
/// ring, and the clamp into it noted for the list's prelude, which runs
/// it at each submission of the list (`HostList::prelude`). Nothing of
/// ours goes into the list: it keeps its pipeline and root bindings.
/// `None` if the records cannot be clamped there (the caller clamps them
/// in the list then).
///
/// The prelude runs before every command of the list, so the clamp sees
/// the records as the card holds them when the list begins: written by
/// the title's CPU (and brought up before the list), or by an earlier
/// list's shaders. That is what the command reads, as long as no shader
/// of this list writes them before it, which the caller checks by the
/// writes the list's shaders declare.
pub(crate) fn bound_early(
    list: &HostList,
    (src, src_offset): (&ID3D12Resource, u64),
    spec: (u32, u32, u32),
    ahead: bool,
) -> Option<(ID3D12Resource, u64)> {
    let bytes = validate_layout(spec, src_offset, None)?;
    let c = clamp_for(list, spec.2)?;
    let budget = budget_of(c, list)?;
    let Some((dst, at)) = list.indirect_ring(bytes) else {
        count_fallback(F_NO_RING_BUFFER);
        return None;
    };
    // SAFETY: a live D3D12 buffer resource.
    let dst_gpu = unsafe { dst.GetGPUVirtualAddress() } + at;
    let (constants, src_gpu, count_gpu) = clamp_arguments(dst_gpu, budget, (src, src_offset), None, spec, ahead)?;
    #[cfg(feature = "diagnostics")]
    crate::copystat::add(crate::copystat::C::IndirectBounded, 1);
    tally(S_PRELUDE, 1);
    if constants[2] > 0 {
        list.keep(src);
        list.early_clamp(Early { clamp: c, constants, src: src_gpu, count: count_gpu, dst: dst_gpu, budget }, &dst);
    }
    Some((dst, at))
}

/// Records a list's prelude clamps ([`bound_early`]) into `l`, a list the
/// card runs just before it: a wait for every earlier write of shaders and
/// copies (the records may come from an earlier list); the zeroing of the
/// list's budget on the card if `zero` (then a wait for it); the clamps with
/// no wait between them (each writes room of its own, and the budget is
/// charged atomically); and one barrier from their writes to the indirect
/// reads of `rings`.
///
/// # Safety
/// An open list; live buffers behind the addresses; `rings` live.
pub(crate) unsafe fn record_early(
    l: &ID3D12GraphicsCommandList,
    zero: Option<u64>,
    clamps: &[Early],
    rings: &[ID3D12Resource],
) {
    use windows::core::Interface;
    let Some(main) = clamp() else {
        return;
    };
    let l7 = l.cast::<ID3D12GraphicsCommandList7>().ok();
    // SAFETY: as the caller vouches; the barriers name live buffers.
    unsafe {
        match &l7 {
            Some(l7) => {
                let before = [D3D12_GLOBAL_BARRIER {
                    SyncBefore: D3D12_BARRIER_SYNC_ALL,
                    SyncAfter: D3D12_BARRIER_SYNC_COMPUTE_SHADING,
                    AccessBefore: D3D12_BARRIER_ACCESS_UNORDERED_ACCESS | D3D12_BARRIER_ACCESS_COPY_DEST,
                    AccessAfter: D3D12_BARRIER_ACCESS_SHADER_RESOURCE | D3D12_BARRIER_ACCESS_UNORDERED_ACCESS,
                }];
                l7.Barrier(&[D3D12_BARRIER_GROUP {
                    Type: D3D12_BARRIER_TYPE_GLOBAL,
                    NumBarriers: 1,
                    Anonymous: D3D12_BARRIER_GROUP_0 { pGlobalBarriers: before.as_ptr() },
                }]);
                tally(S_ENHANCED, 1);
            }
            None => l.ResourceBarrier(&[uav_barrier()]),
        }
        if let Some(budget) = zero {
            record_zero(main, l, budget);
        }
        for e in clamps {
            dispatch_clamp(e.clamp, l, &e.constants, e.src, e.count, e.dst, e.budget);
        }
    }
    tally(S_PRELUDE_RECORDED, clamps.len() as u64);
    if clamps.is_empty() {
        return;
    }
    match &l7 {
        Some(l7) => {
            let after: Vec<D3D12_BUFFER_BARRIER> =
                rings.iter().map(|ring| ring_barrier(ring, CLAMP_WRITES, INDIRECT_READ)).collect();
            // SAFETY: an open list; the barriers name live buffers.
            unsafe {
                l7.Barrier(&[D3D12_BARRIER_GROUP {
                    Type: D3D12_BARRIER_TYPE_BUFFER,
                    NumBarriers: after.len() as u32,
                    Anonymous: D3D12_BARRIER_GROUP_0 { pBufferBarriers: after.as_ptr() },
                }]);
            }
            tally(S_ENHANCED, after.len() as u64);
            for b in after {
                drop(std::mem::ManuallyDrop::into_inner(b.pResource));
            }
        }
        None => {
            // Not seen: the list itself takes enhanced barriers, and the
            // prelude is made on the same device. A wait for every shader
            // write all the same, rather than an indirect read of rooms
            // the clamps may not have written yet.
            // SAFETY: an open list.
            unsafe { l.ResourceBarrier(&[uav_barrier()]) };
            tally(S_PRELUDE_LEGACY, 1);
        }
    }
}

/// Counters for the frame line ([`frame_line`]), by `S_*` index.
static STATS: [std::sync::atomic::AtomicU64; STATS_LEN] = [const { std::sync::atomic::AtomicU64::new(0) }; STATS_LEN];
/// Clamps into a list's ring, in the list.
const S_RING: usize = 0;
/// Clamps into a list's scratch buffer.
const S_SCRATCH: usize = 1;
/// Enhanced barriers recorded (one per ring clamp, a group per prelude).
const S_ENHANCED: usize = 2;
/// Rings made for a list, and rings replaced by one twice as big.
pub(crate) const S_RING_MADE: usize = 3;
pub(crate) const S_RING_GROWN: usize = 4;
/// The largest ring made, and the most room one recording of a list took
/// (bytes; the largest since the last frame line).
pub(crate) const S_RING_LARGEST: usize = 5;
pub(crate) const S_RING_TAKEN: usize = 6;
/// Lists begun with the ring, and without.
const S_LISTS_RING: usize = 7;
const S_LISTS_SCRATCH: usize = 8;
/// Clamps noted for preludes, and recorded in them at submissions.
const S_PRELUDE: usize = 9;
const S_PRELUDE_RECORDED: usize = 10;
/// Preludes whose clamps were followed by a legacy wait (no
/// `ID3D12GraphicsCommandList7` for the prelude).
const S_PRELUDE_LEGACY: usize = 11;
/// Records of prelude lists clamped in the list ([`InList`]).
const S_IN_LIST_WRITTEN: usize = 12;
const S_IN_LIST_COUNTED: usize = 13;
const S_IN_LIST_IN_PLACE: usize = 14;
/// Clamps by the one-lane clamp.
const S_SINGLE: usize = 15;
/// Fallbacks to the scratch buffer, by reason (`F_*`), from here on.
const S_FALLBACK: usize = 16;
/// Lists that asked for the ring on a host without enhanced barriers.
const F_NO_ENHANCED: usize = 0;
/// Lists that asked for the ring and are not `ID3D12GraphicsCommandList7`.
const F_NO_LIST7: usize = 1;
/// Clamps for which no ring buffer could be made (the card's memory).
const F_NO_RING_BUFFER: usize = 2;
/// Lists that asked for the prelude without the ring (it needs it).
const F_PRELUDE_NO_RING: usize = 3;
const FALLBACKS: usize = 4;
const STATS_LEN: usize = S_FALLBACK + FALLBACKS;

use std::sync::atomic::Ordering::Relaxed;

/// Adds `n` to counter `i` of [`STATS`].
pub(crate) fn tally(i: usize, n: u64) {
    STATS[i].fetch_add(n, Relaxed);
}

/// Raises counter `i` of [`STATS`] to `n`.
pub(crate) fn tally_max(i: usize, n: u64) {
    STATS[i].fetch_max(n, Relaxed);
}

fn count_fallback(why: usize) {
    tally(S_FALLBACK + why, 1);
}

/// The frame line's counts of the clamps' forms (`crate::host`'s meter,
/// about once a second): what the switches asked for and what the lists
/// got, so that a run whose switches change in the live file says in its
/// log what each second ran with. Nothing when nothing was clamped.
pub(crate) fn frame_line(frames: f64) {
    let taken: Vec<u64> = STATS.iter().map(|s| s.swap(0, Relaxed)).collect();
    if taken.iter().all(|&n| n == 0) {
        return;
    }
    let f = |i: usize| taken[i] as f64 / frames;
    let kib = |i: usize| taken[i] >> 10;
    tracing::info!(
        "indirect clamps per frame: {:.0} into lists' rings, {:.0} into scratch buffers, {:.0} enhanced barriers; lists begun \
         {:.1} with the ring, {:.1} without (VICTORY_INDIRECT_RING=1 or live indirect_ring=1); rings {:.2} made, {:.2} grown, \
         the largest {} KiB, at most {} KiB taken by one list; fallbacks to the scratch buffer: {:.1} lists without enhanced \
         barriers on the host, {:.1} lists without ID3D12GraphicsCommandList7, {:.1} clamps without a ring buffer, {:.1} lists \
         that asked for the prelude without the ring",
        f(S_RING),
        f(S_SCRATCH),
        f(S_ENHANCED),
        f(S_LISTS_RING),
        f(S_LISTS_SCRATCH),
        f(S_RING_MADE),
        f(S_RING_GROWN),
        kib(S_RING_LARGEST),
        kib(S_RING_TAKEN),
        f(S_FALLBACK + F_NO_ENHANCED),
        f(S_FALLBACK + F_NO_LIST7),
        f(S_FALLBACK + F_NO_RING_BUFFER),
        f(S_FALLBACK + F_PRELUDE_NO_RING),
    );
    let prelude = [S_PRELUDE, S_PRELUDE_RECORDED, S_PRELUDE_LEGACY, S_IN_LIST_WRITTEN, S_IN_LIST_COUNTED, S_IN_LIST_IN_PLACE];
    if prelude.iter().chain([&S_SINGLE]).any(|&i| taken[i] > 0) {
        tracing::info!(
            "indirect clamps in lists' preludes per frame (VICTORY_INDIRECT_PRELUDE=1 or live indirect_prelude=1): {:.0} noted, \
             {:.0} recorded at submissions, {:.1} preludes ended with a legacy wait; of those lists' records clamped in the list \
             instead: {:.0} the list may have written, {:.0} with a count buffer, {:.0} in title memory in place; single records \
             clamped by one lane (VICTORY_INDIRECT_SINGLE=1 or live indirect_single=1): {:.0}",
            f(S_PRELUDE),
            f(S_PRELUDE_RECORDED),
            f(S_PRELUDE_LEGACY),
            f(S_IN_LIST_WRITTEN),
            f(S_IN_LIST_COUNTED),
            f(S_IN_LIST_IN_PLACE),
            f(S_SINGLE),
        );
    }
}

fn uav_barrier() -> D3D12_RESOURCE_BARRIER {
    D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_UAV,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            UAV: std::mem::ManuallyDrop::new(D3D12_RESOURCE_UAV_BARRIER {
                pResource: std::mem::ManuallyDrop::new(None),
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_mesh_guard_clamps_every_mesh_record_as_ahead_and_nothing_else() {
        use super::clamped_as_ahead;
        assert!(clamped_as_ahead(3, false, true));
        assert!(!clamped_as_ahead(3, false, false));
        assert!(clamped_as_ahead(3, true, false));
        for kind in [0, 1, 2] {
            assert!(!clamped_as_ahead(kind, false, true), "kind {kind}");
        }
    }

    use super::*;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_12_0;
    use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
    use windows::Win32::Graphics::Dxgi::*;
    use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
    use windows::core::Interface;

    const WORKER_CS: &str = r#"
#define RS "UAV(u0)"
RWByteAddressBuffer hits : register(u0);
[RootSignature(RS)]
[numthreads(1, 1, 1)]
void main() {
    uint prev;
    hits.InterlockedAdd(0, 1, prev);
}
"#;

    struct WarpHarness {
        device: ID3D12Device,
        queue: ID3D12CommandQueue,
        allocator: ID3D12CommandAllocator,
        list: ID3D12GraphicsCommandList,
        fence: ID3D12Fence,
        submitted: u64,
        event: HANDLE,
        clamp: Clamp,
        worker: Clamp,
    }

    impl WarpHarness {
        fn new() -> Option<Self> {
            // SAFETY: plain DXGI WARP query and D3D12 test harness setup.
            unsafe {
                let factory: IDXGIFactory4 =
                    CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).ok()?;
                let adapter: IDXGIAdapter = factory.EnumWarpAdapter().ok()?;
                let desc = adapter.cast::<IDXGIAdapter1>().ok()?.GetDesc1().ok()?;
                if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 == 0 {
                    return None;
                }
                let mut device: Option<ID3D12Device> = None;
                D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_12_0, &mut device).ok()?;
                let device = device?;
                let queue: ID3D12CommandQueue = device
                    .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                        Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                        ..Default::default()
                    })
                    .ok()?;
                let allocator: ID3D12CommandAllocator = device
                    .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)
                    .ok()?;
                let list: ID3D12GraphicsCommandList = device
                    .CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None)
                    .ok()?;
                let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE).ok()?;
                let event = CreateEventW(None, false, false, None).ok()?;
                let clamp = make_for(&device).expect("clamp shader must compile on WARP");
                let worker = compile_cs(&device, WORKER_CS).expect("worker shader must compile");
                Some(Self {
                    device,
                    queue,
                    allocator,
                    list,
                    fence,
                    submitted: 0,
                    event,
                    clamp,
                    worker,
                })
            }
        }

        /// CPU-mapped L0 write-back buffer matching `Shadow`'s heap type.
        fn shared_buffer(&self, size: u64) -> (ID3D12Resource, *mut u8, u64) {
            let heap = D3D12_HEAP_PROPERTIES {
                Type: D3D12_HEAP_TYPE_CUSTOM,
                CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_WRITE_BACK,
                MemoryPoolPreference: D3D12_MEMORY_POOL_L0,
                ..Default::default()
            };
            let desc = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                Width: size.max(4),
                Height: 1,
                DepthOrArraySize: 1,
                MipLevels: 1,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                Flags: D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
                ..Default::default()
            };
            let mut buffer: Option<ID3D12Resource> = None;
            let mut data = std::ptr::null_mut();
            // SAFETY: plain buffer creation and persistent CPU mapping.
            unsafe {
                self.device
                    .CreateCommittedResource(
                        &heap,
                        D3D12_HEAP_FLAG_NONE,
                        &desc,
                        D3D12_RESOURCE_STATE_COMMON,
                        None,
                        &mut buffer,
                    )
                    .unwrap();
                let buffer = buffer.unwrap();
                buffer.Map(0, None, Some(&mut data)).unwrap();
                let gpu = buffer.GetGPUVirtualAddress();
                (buffer, data.cast(), gpu)
            }
        }

        /// Device-local `DEFAULT`-heap UAV scratch buffer.
        fn default_uav(&self, size: u64) -> ID3D12Resource {
            let heap = D3D12_HEAP_PROPERTIES {
                Type: D3D12_HEAP_TYPE_DEFAULT,
                ..Default::default()
            };
            let desc = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                Width: size.max(64 << 10),
                Height: 1,
                DepthOrArraySize: 1,
                MipLevels: 1,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                Flags: D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
                ..Default::default()
            };
            let mut buffer: Option<ID3D12Resource> = None;
            // SAFETY: plain device-local buffer creation.
            unsafe {
                self.device
                    .CreateCommittedResource(
                        &heap,
                        D3D12_HEAP_FLAG_NONE,
                        &desc,
                        D3D12_RESOURCE_STATE_COMMON,
                        None,
                        &mut buffer,
                    )
                    .unwrap();
            }
            buffer.unwrap()
        }

        fn signature(&self, kind: u32, stride: u32) -> ID3D12CommandSignature {
            let arg = D3D12_INDIRECT_ARGUMENT_DESC {
                Type: D3D12_INDIRECT_ARGUMENT_TYPE(kind as i32),
                ..Default::default()
            };
            let desc = D3D12_COMMAND_SIGNATURE_DESC {
                ByteStride: stride,
                NumArgumentDescs: 1,
                pArgumentDescs: &arg,
                NodeMask: 0,
            };
            let mut sig: Option<ID3D12CommandSignature> = None;
            // SAFETY: valid single-argument command signature description.
            unsafe {
                self.device
                    .CreateCommandSignature(&desc, None::<&ID3D12RootSignature>, &mut sig)
                    .unwrap();
            }
            sig.unwrap()
        }

        fn submit_and_wait(&mut self) {
            self.submitted += 1;
            // SAFETY: closing, executing, waiting on fence, and resetting our list.
            unsafe {
                self.list.Close().unwrap();
                let cmd: ID3D12CommandList = self.list.cast().unwrap();
                self.queue.ExecuteCommandLists(&[Some(cmd)]);
                self.queue.Signal(&self.fence, self.submitted).unwrap();
                self.fence
                    .SetEventOnCompletion(self.submitted, self.event)
                    .unwrap();
                WaitForSingleObject(self.event, INFINITE);
                self.allocator.Reset().unwrap();
                self.list.Reset(&self.allocator, None).unwrap();
            }
        }
    }

    fn compile_cs(device: &ID3D12Device, source: &str) -> windows::core::Result<Clamp> {
        let mut code: Option<ID3DBlob> = None;
        // SAFETY: source and entry names outlive `D3DCompile`.
        unsafe {
            D3DCompile(
                source.as_ptr().cast(),
                source.len(),
                s!("worker"),
                None,
                None,
                s!("main"),
                s!("cs_5_1"),
                0,
                0,
                &mut code,
                None,
            )?;
            let code = code.unwrap();
            let bytes = std::slice::from_raw_parts(
                code.GetBufferPointer().cast::<u8>(),
                code.GetBufferSize(),
            );
            let root: ID3D12RootSignature = device.CreateRootSignature(0, bytes)?;
            let pso: ID3D12PipelineState = device.CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                pRootSignature: std::mem::transmute_copy(&root),
                CS: D3D12_SHADER_BYTECODE {
                    pShaderBytecode: bytes.as_ptr().cast(),
                    BytecodeLength: bytes.len(),
                },
                ..Default::default()
            })?;
            Ok(Clamp { root, zero: pso.clone(), pso, lanes: 1 })
        }
    }

    /// Every one-byte change is found, in the piece it is in, with a copy
    /// that does not start on a piece boundary (`skew`) and a span that
    /// does not start at piece 0 (upstream #1014's test, on our compare of
    /// a group a piece).
    #[test]
    fn warp_card_compare_finds_every_byte_with_skew() {
        let Some(mut warp) = WarpHarness::new() else {
            return;
        };
        let compare = compile_cs(&warp.device, crate::host::CARD_COMPARE).expect("the card compare must compile on WARP");
        const PIECE: usize = 64 << 10;
        const SKEW: u32 = 0x3450;
        const FIRST: u32 = 1;
        const PIECES: u32 = 4;
        let size = PIECE * 6;
        let (_now, now_ptr, now_gpu) = warp.shared_buffer(size as u64);
        let (_hashes, _, hashes_gpu) = warp.shared_buffer(16 * 8);
        let (_differs, differs_ptr, differs_gpu) = warp.shared_buffer(64);
        let content: Vec<u8> = (0..size).map(|i| (i * 131 + i / 7) as u8).collect();
        // SAFETY: the mapping holds `size` bytes.
        unsafe { std::ptr::copy_nonoverlapping(content.as_ptr(), now_ptr, size) };
        let run = |warp: &mut WarpHarness, mode: u32| -> Vec<u32> {
            // SAFETY: our open list, live buffers, the compare's own root
            // signature; the mappings hold what is read and written.
            unsafe {
                for i in 0..PIECES as usize {
                    differs_ptr.cast::<u32>().add(i).write_volatile(7);
                }
                warp.list.SetComputeRootSignature(&compare.root);
                warp.list.SetPipelineState(&compare.pso);
                let constants = [FIRST, SKEW, size as u32, PIECES, 0, mode];
                warp.list.SetComputeRoot32BitConstants(0, 6, constants.as_ptr().cast(), 0);
                warp.list.SetComputeRootShaderResourceView(1, now_gpu);
                warp.list.SetComputeRootUnorderedAccessView(2, hashes_gpu);
                warp.list.SetComputeRootUnorderedAccessView(3, differs_gpu);
                warp.list.Dispatch(PIECES, 1, 1);
            }
            warp.submit_and_wait();
            // SAFETY: as above.
            (0..PIECES as usize).map(|i| unsafe { differs_ptr.cast::<u32>().add(i).read_volatile() }).collect()
        };
        run(&mut warp, 0);
        assert_eq!(run(&mut warp, 1), vec![0; PIECES as usize]);
        // Piece `FIRST + i` of the copy covers offsets [lo, hi) as the
        // compare cuts them; a byte there changed flags that piece alone.
        for (k, step) in [0usize, 1, 15, 16, 1023, 1024, 4095, 30001, 65535].iter().enumerate() {
            let i = (k % PIECES as usize) as u32;
            let lo = ((FIRST + i) as usize * PIECE).saturating_sub(SKEW as usize);
            let at = lo + step % (PIECE - 16);
            // SAFETY: inside the mapping.
            unsafe { now_ptr.add(at).write_volatile(content[at] ^ 0x5a) };
            let flagged = run(&mut warp, 1);
            assert_eq!(flagged[i as usize], 1, "byte {at:#x} of piece {i} not found");
            // SAFETY: inside the mapping.
            unsafe { now_ptr.add(at).write_volatile(content[at]) };
            assert_eq!(run(&mut warp, 1), vec![0; PIECES as usize]);
        }
    }

    /// `VICTORY_CARD_COMPARE_EIGHT=1`: the compare that loads eight places
    /// at once stores the same hash of every piece as the one that loads
    /// one at a time, whole pieces and cut ones (a copy that does not start
    /// on a piece boundary, a size that ends inside a piece).
    #[test]
    fn warp_card_compare_eight_at_once_makes_the_same_hashes() {
        let Some(mut warp) = WarpHarness::new() else {
            return;
        };
        let one = compile_cs(&warp.device, crate::host::CARD_COMPARE).expect("the card compare must compile on WARP");
        let eight = compile_cs(&warp.device, &format!("#define EIGHT 1\n{}", crate::host::CARD_COMPARE))
            .expect("the card compare with eight loads must compile on WARP");
        const PIECE: usize = 64 << 10;
        const PIECES: u32 = 5;
        let buffer = PIECE * 6;
        let (_now, now_ptr, now_gpu) = warp.shared_buffer(buffer as u64);
        let (_hashes, hashes_ptr, hashes_gpu) = warp.shared_buffer(16 * 8);
        let (_differs, _, differs_gpu) = warp.shared_buffer(64);
        let content: Vec<u8> = (0..buffer).map(|i| (i * 167 + i / 13 + i / 4093) as u8).collect();
        // SAFETY: the mapping holds `buffer` bytes.
        unsafe { std::ptr::copy_nonoverlapping(content.as_ptr(), now_ptr, buffer) };
        let hashes = |warp: &mut WarpHarness, compare: &Clamp, skew: u32, size: u32| -> Vec<u32> {
            // SAFETY: our open list, live buffers, the compare's own root
            // signature; the mappings hold what is read and written.
            unsafe {
                std::ptr::write_bytes(hashes_ptr, 0, 16 * 8);
                warp.list.SetComputeRootSignature(&compare.root);
                warp.list.SetPipelineState(&compare.pso);
                let constants = [0, skew, size, PIECES, 0, 0];
                warp.list.SetComputeRoot32BitConstants(0, 6, constants.as_ptr().cast(), 0);
                warp.list.SetComputeRootShaderResourceView(1, now_gpu);
                warp.list.SetComputeRootUnorderedAccessView(2, hashes_gpu);
                warp.list.SetComputeRootUnorderedAccessView(3, differs_gpu);
                warp.list.Dispatch(PIECES, 1, 1);
            }
            warp.submit_and_wait();
            // SAFETY: as above.
            (0..PIECES as usize * 4).map(|i| unsafe { hashes_ptr.cast::<u32>().add(i).read_volatile() }).collect()
        };
        for (skew, size) in [(0u32, (PIECE * 5) as u32), (0x3450, (PIECE * 5) as u32), (0x10, 288), (0x3450, (PIECE * 3 + 1000) as u32)] {
            let a = hashes(&mut warp, &one, skew, size);
            let b = hashes(&mut warp, &eight, skew, size);
            assert!(a.iter().any(|&w| w != 0), "skew {skew:#x} size {size}: no hash stored");
            assert_eq!(a, b, "skew {skew:#x} size {size}: the hashes differ");
        }
    }

    /// The compare of copies in the card's memory (`crate::host`), on the
    /// software adapter. Told 1 it flags the pieces whose bytes are not
    /// what was hashed last and leaves the hashes: a piece a shader changed
    /// and a later one changed back then looks untouched, which is right
    /// only if the piece was fetched after both. Told 2 (submissions sent
    /// ahead with the relay, whose pieces are fetched flight by flight) it
    /// stores the hash it made: each compare flags what changed since the
    /// compare before it, also a change back.
    #[test]
    fn warp_card_compare_stores_the_hash_it_made_when_told() {
        let Some(mut warp) = WarpHarness::new() else {
            return;
        };
        let compare = compile_cs(&warp.device, crate::host::CARD_COMPARE).expect("the card compare must compile on WARP");
        const PIECE: usize = 64 << 10;
        const PIECES: u32 = 3;
        let size = PIECE * PIECES as usize;
        let (_now, now_ptr, now_gpu) = warp.shared_buffer(size as u64);
        let (_hashes, _, hashes_gpu) = warp.shared_buffer(16 * u64::from(PIECES));
        let (_differs, differs_ptr, differs_gpu) = warp.shared_buffer(64);
        let content: Vec<u8> = (0..size).map(|i| (i * 31 + i / 251) as u8).collect();
        // SAFETY: the mapping holds `size` bytes.
        unsafe { std::ptr::copy_nonoverlapping(content.as_ptr(), now_ptr, size) };
        // One pass over the three pieces; what it left in `differs` (7s
        // where it stored nothing).
        let run = |warp: &mut WarpHarness, mode: u32| -> [u32; 3] {
            // SAFETY: our open list, live buffers, the compare's own root
            // signature; the mappings hold what is read and written.
            unsafe {
                for piece in 0..PIECES as usize {
                    differs_ptr.cast::<u32>().add(piece).write_volatile(7);
                }
                warp.list.SetComputeRootSignature(&compare.root);
                warp.list.SetPipelineState(&compare.pso);
                let constants = [0u32, 0, size as u32, PIECES, 0, mode];
                warp.list.SetComputeRoot32BitConstants(0, 6, constants.as_ptr().cast(), 0);
                warp.list.SetComputeRootShaderResourceView(1, now_gpu);
                warp.list.SetComputeRootUnorderedAccessView(2, hashes_gpu);
                warp.list.SetComputeRootUnorderedAccessView(3, differs_gpu);
                warp.list.Dispatch(PIECES, 1, 1);
            }
            warp.submit_and_wait();
            // SAFETY: as above.
            [0, 1, 2].map(|piece| unsafe { differs_ptr.cast::<u32>().add(piece).read_volatile() })
        };
        let at = PIECE + 1234;
        // SAFETY (each): inside the mapping.
        let set = |value: u8| unsafe { now_ptr.add(at).write_volatile(value) };
        // 0: the hashes are stored, nothing is flagged.
        assert_eq!(run(&mut warp, 0), [7, 7, 7]);
        assert_eq!(run(&mut warp, 1), [0, 0, 0]);
        // A change in the second piece: flagged, and with 1 flagged again
        // (the hash stays the old one).
        set(content[at] ^ 0x40);
        assert_eq!(run(&mut warp, 1), [0, 1, 0]);
        assert_eq!(run(&mut warp, 1), [0, 1, 0]);
        // Changed back: with 1 the piece looks untouched.
        set(content[at]);
        assert_eq!(run(&mut warp, 1), [0, 0, 0]);
        // With 2 the change is flagged once, and the hash is the new one.
        set(content[at] ^ 0x40);
        assert_eq!(run(&mut warp, 2), [0, 1, 0]);
        assert_eq!(run(&mut warp, 2), [0, 0, 0]);
        // Changed back: flagged, where 1 saw nothing.
        set(content[at]);
        assert_eq!(run(&mut warp, 2), [0, 1, 0]);
        assert_eq!(run(&mut warp, 2), [0, 0, 0]);
        // Every lane's slice is hashed: first byte of piece 0 (lane 0),
        // last byte of piece 2 (lane 63), and swapping two 16-byte blocks
        // across lanes in piece 1 must all be detected.
        for &(offset, expected) in &[
            (0usize, [1, 0, 0]),
            (PIECE * 3 - 1, [0, 0, 1]),
            (63 * 16 + 7, [1, 0, 0]),
        ] {
            // SAFETY: offset is inside the mapped buffer.
            unsafe { now_ptr.add(offset).write_volatile(content[offset] ^ 0x01) };
            assert_eq!(run(&mut warp, 1), expected);
            // SAFETY: restore original byte.
            unsafe { now_ptr.add(offset).write_volatile(content[offset]) };
            assert_eq!(run(&mut warp, 1), [0, 0, 0]);
        }
        // Swap lane 0's first 16 bytes with lane 1's first 16 bytes in piece 1.
        // SAFETY: both 16-byte blocks are inside piece 1 of the mapped buffer.
        unsafe {
            std::ptr::copy_nonoverlapping(content[PIECE + 16..PIECE + 32].as_ptr(), now_ptr.add(PIECE), 16);
            std::ptr::copy_nonoverlapping(content[PIECE..PIECE + 16].as_ptr(), now_ptr.add(PIECE + 16), 16);
        }
        assert_eq!(run(&mut warp, 1), [0, 1, 0]);
    }

    /// The fetch inside flights (`crate::host::CARD_FETCH`), on the software
    /// adapter, in one list behind the compare as `Host::card_before`
    /// records it: each flagged piece takes a ticket and, while it is
    /// within the slots of the lowest ticket in the air, its bytes go to
    /// its slot; an unflagged piece's entry says so; a ticket past the
    /// slots is written but copies nothing.
    #[test]
    fn warp_card_fetch_copies_flagged_pieces_to_their_slots() {
        use crate::host::{CARD_COMPARE, CARD_FETCH, INFLIGHT_NONE, INFLIGHT_SLOT};
        let Some(mut warp) = WarpHarness::new() else {
            return;
        };
        let compare = compile_cs(&warp.device, CARD_COMPARE).expect("the card compare must compile on WARP");
        let fetch = compile_cs(&warp.device, CARD_FETCH).expect("the card fetch must compile on WARP");
        const PIECE: usize = 64 << 10;
        const PIECES: u32 = 3;
        // The copy starts 40 bytes into its piece 0: piece 0 is short, the
        // others straddle title pieces and start off a multiple of 16.
        const SKEW: u32 = 40;
        let size = PIECE * PIECES as usize;
        let (_now, now_ptr, now_gpu) = warp.shared_buffer(size as u64);
        let (_hashes, _, hashes_gpu) = warp.shared_buffer(16 * u64::from(PIECES + 1));
        let (_differs, _, differs_gpu) = warp.shared_buffer(64);
        let (_table, table_ptr, table_gpu) = warp.shared_buffer(4 + 64);
        let slots = 1u32;
        let (_fetched, fetched_ptr, fetched_gpu) = warp.shared_buffer(INFLIGHT_SLOT * u64::from(slots));
        let content: Vec<u8> = (0..size).map(|i| (i * 31 + i / 251) as u8).collect();
        // SAFETY: the mappings hold what is written.
        unsafe {
            std::ptr::copy_nonoverlapping(content.as_ptr(), now_ptr, size);
            table_ptr.cast::<u32>().write_volatile(0);
        }
        let pieces = PIECES + 1;
        let run = |warp: &mut WarpHarness, mode: u32, landed: u32| -> Vec<u32> {
            // SAFETY: our open list, live buffers, the shaders' own root
            // signatures; the mappings hold what is read and written.
            unsafe {
                warp.list.SetComputeRootSignature(&compare.root);
                warp.list.SetPipelineState(&compare.pso);
                let constants = [0u32, SKEW, size as u32, pieces, 0, mode];
                warp.list.SetComputeRoot32BitConstants(0, 6, constants.as_ptr().cast(), 0);
                warp.list.SetComputeRootShaderResourceView(1, now_gpu);
                warp.list.SetComputeRootUnorderedAccessView(2, hashes_gpu);
                warp.list.SetComputeRootUnorderedAccessView(3, differs_gpu);
                // One group per piece (the 64-thread compare).
                warp.list.Dispatch(pieces, 1, 1);
                if mode == 2 {
                    warp.list.ResourceBarrier(&[uav_barrier()]);
                    warp.list.SetComputeRootSignature(&fetch.root);
                    warp.list.SetPipelineState(&fetch.pso);
                    // Two groups for four pieces: the loop over pieces.
                    let constants = [0u32, SKEW, size as u32, pieces, 0, landed, slots, 2];
                    warp.list.SetComputeRoot32BitConstants(0, 8, constants.as_ptr().cast(), 0);
                    warp.list.SetComputeRootShaderResourceView(1, now_gpu);
                    warp.list.SetComputeRootUnorderedAccessView(2, differs_gpu);
                    warp.list.SetComputeRootUnorderedAccessView(3, table_gpu);
                    warp.list.SetComputeRootUnorderedAccessView(4, fetched_gpu);
                    warp.list.Dispatch(2, 1, 1);
                }
            }
            warp.submit_and_wait();
            // SAFETY: as above.
            (0..=pieces as usize).map(|i| unsafe { table_ptr.cast::<u32>().add(i).read_volatile() }).collect()
        };
        // SAFETY (each): inside the mapping of the copy.
        let set = |at: usize, value: u8| unsafe { now_ptr.add(at).write_volatile(value) };
        // SAFETY (each): the slot's mapping holds `INFLIGHT_SLOT` bytes.
        let slot = |len: usize| unsafe { std::slice::from_raw_parts(fetched_ptr, len).to_vec() };
        run(&mut warp, 0, 0);
        // Piece 2 of the copy (title piece 2: bytes 2 * 64 KiB - 40 on), its
        // slot from the multiple of 16 below to the one above its end.
        let from = 2 * PIECE - SKEW as usize;
        let widened = from & !15;
        assert_ne!(widened, from);
        set(from + 100, content[from + 100] ^ 0x40);
        let table = run(&mut warp, 2, 0);
        assert_eq!(table, [1, INFLIGHT_NONE, INFLIGHT_NONE, 0, INFLIGHT_NONE]);
        let mut wanted = content[widened..widened + INFLIGHT_SLOT as usize].to_vec();
        wanted[from + 100 - widened] ^= 0x40;
        assert_eq!(slot(INFLIGHT_SLOT as usize), wanted, "piece 2 in slot 0");
        // Two pieces flagged with one slot, ticket 0 not landed: tickets 1
        // and 2 get none and the slot keeps what it held.
        set(5, content[5] ^ 1);
        set(PIECE + 7, content[PIECE + 7] ^ 1);
        let table = run(&mut warp, 2, 0);
        assert_eq!(table[0], 3);
        let mut tickets = [table[1], table[2]];
        tickets.sort_unstable();
        assert_eq!(tickets, [1, 2]);
        assert_eq!(&table[3..], [INFLIGHT_NONE, INFLIGHT_NONE]);
        assert_eq!(slot(INFLIGHT_SLOT as usize), wanted);
        // Every ticket up to 2 landed: the next one, 3, gets slot 0. Piece
        // 0 starts at the copy's start.
        set(9, content[9] ^ 2);
        let table = run(&mut warp, 2, 3);
        assert_eq!(table, [4, 3, INFLIGHT_NONE, INFLIGHT_NONE, INFLIGHT_NONE]);
        let mut piece0 = content[..PIECE - SKEW as usize].to_vec();
        piece0[5] ^= 1;
        piece0[9] ^= 2;
        assert_eq!(slot(PIECE - SKEW as usize), piece0);
    }

    /// The clamp compiles with the system's compiler and the host takes it.
    #[test]
    fn argument_clamp_builds() {
        if super::host().is_none() {
            return; // no host GPU here
        }
        assert!(super::clamp().is_some());
    }

    #[test]
    fn rejects_unaligned_or_undersized_layouts() {
        assert_eq!(validate_layout((0, 16, 4), 0, None), Some(64));
        assert_eq!(validate_layout((1, 20, 4), 64, Some(128)), Some(80));
        assert_eq!(validate_layout((2, 12, 0), 0, Some(0)), Some(0));
        assert_eq!(validate_layout((10, 16, 2), 8, None), Some(32));
        // Undersized strides for each kind.
        assert_eq!(validate_layout((0, 12, 1), 0, None), None);
        assert_eq!(validate_layout((1, 16, 1), 0, None), None);
        assert_eq!(validate_layout((2, 8, 1), 0, None), None);
        assert_eq!(validate_layout((10, 8, 1), 0, None), None);
        // Unknown kind.
        assert_eq!(validate_layout((3, 16, 1), 0, None), None);
        // Unaligned stride, source offset, or count offset.
        assert_eq!(validate_layout((0, 18, 1), 0, None), None);
        assert_eq!(validate_layout((0, 16, 1), 2, None), None);
        assert_eq!(validate_layout((0, 16, 1), 0, Some(6)), None);
        // Total byte size exceeding 32-bit ByteAddressBuffer indexing.
        assert_eq!(validate_layout((1, 20, u32::MAX), 0, None), None);
    }

    /// WARP D3D12 execution test proving:
    /// (a) `countBuffer` with a small count (`3`) and `4,093` garbage tail slots
    ///     (`max = 4096`) does NOT blow `BUDGET` or zero valid dispatches in
    ///     this or subsequent `ExecuteIndirect` calls in the same list,
    /// (b) `src_args` (and `count_buf`) are NOT mutated in-place, and
    /// (c) full `stride` bytes (`stride = 24` > `12`) are copied for active
    ///     records into `dst_args` at offset `0`.
    #[test]
    fn warp_count_buffer_ignores_garbage_tail_and_preserves_source() {
        let Some(mut warp) = WarpHarness::new() else {
            return;
        };
        const KIND_DISPATCH: u32 = 2;
        const STRIDE: u32 = 24; // 6 dwords: 3 dispatch group counts + 3 extra payload dwords
        const MAX: u32 = 4096;
        const ARGS_OFFSET: usize = 64; // non-zero offset with sentinel prefix
        const COUNT_OFFSET_1: usize = 128;
        const COUNT_OFFSET_2: usize = 192;

        let src_bytes = ARGS_OFFSET + (STRIDE as usize) * (MAX as usize) + 64;
        let (src_buf, src_ptr, _) = warp.shared_buffer(src_bytes as u64);
        let (count_buf, count_ptr, _) = warp.shared_buffer(512);
        let (_budget_buf, budget_ptr, budget_gpu) = warp.shared_buffer(256);
        let (_hits_buf, hits_ptr, hits_gpu) = warp.shared_buffer(256);
        let (readback_buf, readback_ptr, _) = warp.shared_buffer((STRIDE * 8) as u64);
        let dst_buf = warp.default_uav(u64::from(STRIDE) * u64::from(MAX));
        let sig = warp.signature(KIND_DISPATCH, STRIDE);

        // Fill the entire source buffer with non-zero sentinel bytes, then
        // populate 3 valid records at `ARGS_OFFSET` and 4,093 huge garbage
        // records in `3..MAX` that would exhaust `BUDGET` ~2,000x over if read.
        let mut initial_src = vec![0xA5u8; src_bytes];
        let write_record = |buf: &mut [u8], idx: usize, words: [u32; 6]| {
            let base = ARGS_OFFSET + idx * (STRIDE as usize);
            for (w, val) in words.into_iter().enumerate() {
                buf[base + w * 4..base + (w + 1) * 4].copy_from_slice(&val.to_le_bytes());
            }
        };
        write_record(
            &mut initial_src,
            0,
            [3, 2, 1, 0x1111_2222, 0x3333_4444, 0x5555_6666],
        );
        write_record(
            &mut initial_src,
            1,
            [5, 1, 1, 0x7777_8888, 0x9999_AAAA, 0xBBBB_CCCC],
        );
        // Record 2 exceeds 65,535 in X -> clamped to (65535, 1, 1).
        write_record(
            &mut initial_src,
            2,
            [100_000, 1, 1, 0xDDDD_EEEE, 0xFFFF_0001, 0x0123_4567],
        );
        for slot in 3..MAX as usize {
            // Each garbage slot would clamp to 65535 * 16 * 1 = 1,048,560 groups
            // (`cost = 67,107,840 ~= BUDGET / 2`), exhausting `BUDGET` immediately!
            write_record(
                &mut initial_src,
                slot,
                [0xFFFF_FFFF, 16, 1, 0xDEAD_BEEF, 0xBAD0_C0DE, slot as u32],
            );
        }

        // Second indirect call's source records (at offset 0 of a second buffer,
        // or later in `count_buf`): put 2 active records + garbage tail in a
        // second range to run back-to-back in the SAME command list.
        let mut initial_count = vec![0x5Au8; 512];
        initial_count[COUNT_OFFSET_1..COUNT_OFFSET_1 + 4].copy_from_slice(&3u32.to_le_bytes());
        initial_count[COUNT_OFFSET_2..COUNT_OFFSET_2 + 4].copy_from_slice(&2u32.to_le_bytes());

        // SAFETY: buffers are mapped with exact capacities above.
        unsafe {
            std::ptr::copy_nonoverlapping(initial_src.as_ptr(), src_ptr, src_bytes);
            std::ptr::copy_nonoverlapping(initial_count.as_ptr(), count_ptr, 512);
            (budget_ptr as *mut u32).write_volatile(0);
            (hits_ptr as *mut u32).write_volatile(0);
            std::ptr::write_bytes(readback_ptr, 0, (STRIDE * 8) as usize);
        }

        // Record Call #1 (`eff_count = 3` out of `MAX = 4096`), copy the first
        // 5 records of `dst_buf` out for inspection, then record Call #2
        // (`eff_count = 2` out of `MAX = 4096`) reusing `dst_buf` in the same list!
        assert!(record_clamp(
            &warp.clamp,
            &warp.list,
            &dst_buf,
            budget_gpu,
            (&src_buf, ARGS_OFFSET as u64),
            Some((&count_buf, COUNT_OFFSET_1 as u64)),
            (KIND_DISPATCH, STRIDE, MAX),
        ));
        // SAFETY: open command list, valid worker PSO/root and buffers.
        unsafe {
            warp.list.SetComputeRootSignature(&warp.worker.root);
            warp.list.SetPipelineState(&warp.worker.pso);
            warp.list.SetComputeRootUnorderedAccessView(0, hits_gpu);
            warp.list.ExecuteIndirect(
                &sig,
                MAX,
                &dst_buf,
                0,
                Some(&count_buf),
                COUNT_OFFSET_1 as u64,
            );
        }
        done_on(&warp.list, &dst_buf);

        // Snapshot `dst_buf` after Call #1 (from `COMMON`, copy 5 * STRIDE bytes,
        // then return `dst_buf` to `COMMON` before Call #2).
        // SAFETY: open command list; `dst_buf` returned to `COMMON` by `done_on`.
        unsafe {
            warp.list.ResourceBarrier(&[transition(
                &dst_buf,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            )]);
            warp.list
                .CopyBufferRegion(&readback_buf, 0, &dst_buf, 0, u64::from(STRIDE * 5));
            warp.list.ResourceBarrier(&[transition(
                &dst_buf,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
                D3D12_RESOURCE_STATE_COMMON,
            )]);
        }

        // Call #2 in the SAME command list (`eff_count = 2` -> records 0 and 1:
        // 6 + 5 = 11 groups). Would run empty if Call #1's tail blew `BUDGET`!
        assert!(record_clamp(
            &warp.clamp,
            &warp.list,
            &dst_buf,
            budget_gpu,
            (&src_buf, ARGS_OFFSET as u64),
            Some((&count_buf, COUNT_OFFSET_2 as u64)),
            (KIND_DISPATCH, STRIDE, MAX),
        ));
        // SAFETY: open command list, valid worker PSO/root and buffers.
        unsafe {
            warp.list.SetComputeRootSignature(&warp.worker.root);
            warp.list.SetPipelineState(&warp.worker.pso);
            warp.list.SetComputeRootUnorderedAccessView(0, hits_gpu);
            warp.list.ExecuteIndirect(
                &sig,
                MAX,
                &dst_buf,
                0,
                Some(&count_buf),
                COUNT_OFFSET_2 as u64,
            );
        }
        done_on(&warp.list, &dst_buf);

        warp.submit_and_wait();

        // SAFETY: GPU execution finished; read mapped host results.
        let (hits, spent, after_src, after_count, clamped_words) = unsafe {
            let hits = (hits_ptr as *const u32).read_volatile();
            let spent = (budget_ptr as *const u32).read_volatile();
            let after_src = std::slice::from_raw_parts(src_ptr.cast_const(), src_bytes).to_vec();
            let after_count = std::slice::from_raw_parts(count_ptr.cast_const(), 512).to_vec();
            let words = std::slice::from_raw_parts(
                readback_ptr.cast::<u32>().cast_const(),
                (STRIDE as usize / 4) * 5,
            )
            .to_vec();
            (hits, spent, after_src, after_count, words)
        };

        // Call #1: (3*2*1) + (5*1*1) + (65535*1*1) = 65,546 groups.
        // Call #2: (3*2*1) + (5*1*1) = 11 groups.
        let expected_groups = (6 + 5 + 65_535) + (6 + 5);
        assert_eq!(
            hits, expected_groups,
            "valid indirect dispatches must all execute"
        );
        assert_eq!(
            spent,
            expected_groups * 64,
            "budget must reflect only active records below countBuffer"
        );
        assert_eq!(
            after_src, initial_src,
            "src_args must never be mutated in-place"
        );
        assert_eq!(
            after_count, initial_count,
            "count_buf must never be mutated"
        );

        // Check `dst_args` records 0..3: clamped group counts + preserved extra stride words.
        assert_eq!(
            &clamped_words[0..6],
            &[3, 2, 1, 0x1111_2222, 0x3333_4444, 0x5555_6666]
        );
        assert_eq!(
            &clamped_words[6..12],
            &[5, 1, 1, 0x7777_8888, 0x9999_AAAA, 0xBBBB_CCCC]
        );
        assert_eq!(
            &clamped_words[12..18],
            &[65_535, 1, 1, 0xDDDD_EEEE, 0xFFFF_0001, 0x0123_4567]
        );
        // Inactive slot 3 was never copied from `src_args`.
        assert_ne!(clamped_words[21], 0xDEAD_BEEF);
    }

    /// WARP D3D12 test proving `BUDGET` saturates without 32-bit unsigned
    /// wraparound across >64 huge indexed-draw records (`count = None`), while
    /// preserving `StartIndexLocation`, `BaseVertexLocation`,
    /// `StartInstanceLocation`, and custom stride padding, and leaving
    /// `src_args` untouched.
    /// A window of mesh records clamped ahead: the filled ones come out as
    /// they are and are charged, the slots of all ones come out empty and
    /// charge nothing; clamped as single records the same slots are clamped
    /// to the most and spend the budget.
    #[test]
    fn warp_window_leaves_unfilled_slots_empty_and_uncharged() {
        let Some(mut warp) = WarpHarness::new() else {
            return;
        };
        const KIND_MESH: u32 = 10;
        const STRIDE: u32 = 32;
        const RECORDS: u32 = 16;
        let filled = [5u32, 0, 519];
        let total = (STRIDE * RECORDS) as usize;
        let mut src = vec![0xffu8; total];
        for (i, groups) in filled.into_iter().enumerate() {
            let words = [groups, 1, 1, 0xAB00 + i as u32];
            for (w, word) in words.into_iter().enumerate() {
                let at = i * STRIDE as usize + w * 4;
                src[at..at + 4].copy_from_slice(&word.to_le_bytes());
            }
        }
        for ahead in [true, false] {
            let (src_buf, src_ptr, _) = warp.shared_buffer(total as u64);
            let (_budget_buf, budget_ptr, budget_gpu) = warp.shared_buffer(256);
            let (readback_buf, readback_ptr, _) = warp.shared_buffer(total as u64);
            let dst_buf = warp.default_uav(total as u64);
            // SAFETY: mapped buffers of these sizes.
            unsafe {
                std::ptr::copy_nonoverlapping(src.as_ptr(), src_ptr, total);
                std::ptr::write_bytes(budget_ptr, 0, 256);
                std::ptr::write_bytes(readback_ptr, 0, total);
            }
            assert!(record_clamp_as(
                &warp.clamp,
                &warp.list,
                &dst_buf,
                budget_gpu,
                (&src_buf, 0),
                None,
                (KIND_MESH, STRIDE, RECORDS),
                ahead,
            ));
            done_on(&warp.list, &dst_buf);
            // SAFETY: an open list; the scratch buffer is copied for the check.
            unsafe {
                warp.list.ResourceBarrier(&[transition(
                    &dst_buf,
                    D3D12_RESOURCE_STATE_COMMON,
                    D3D12_RESOURCE_STATE_COPY_SOURCE,
                )]);
                warp.list.CopyBufferRegion(&readback_buf, 0, &dst_buf, 0, total as u64);
                warp.list.ResourceBarrier(&[transition(
                    &dst_buf,
                    D3D12_RESOURCE_STATE_COPY_SOURCE,
                    D3D12_RESOURCE_STATE_COMMON,
                )]);
            }
            warp.submit_and_wait();
            // SAFETY: the work is done; mapped buffers.
            let (spent, largest, out) = unsafe {
                (
                    (budget_ptr as *const u32).read_volatile(),
                    (budget_ptr as *const u32).add(1).read_volatile(),
                    std::slice::from_raw_parts(readback_ptr.cast::<u32>().cast_const(), total / 4).to_vec(),
                )
            };
            for (i, groups) in filled.into_iter().enumerate() {
                let rec = &out[i * 8..i * 8 + 4];
                assert_eq!(rec, [groups, u32::from(groups > 0).max(1).min(1), 1, 0xAB00 + i as u32], "filled record {i}, ahead {ahead}");
            }
            let charged_filled = (5 + 519) * 128;
            if ahead {
                for i in filled.len()..RECORDS as usize {
                    assert_eq!(out[i * 8..i * 8 + 3], [0, 0, 0], "unfilled slot {i} must come out empty");
                    assert_eq!(out[i * 8 + 3], u32::MAX, "the words after the record are copied as they are");
                }
                assert_eq!(spent, charged_filled, "unfilled slots charge nothing");
                assert_eq!(largest, 519, "and are not counted as asked for");
            } else {
                // Thirteen slots at 65,535 groups: sixteen would be all of it.
                assert_eq!(spent, charged_filled + 13 * 65535 * 128, "clamped one by one, each slot is charged the most");
                assert_eq!(largest, u32::MAX);
            }
        }
    }

    #[test]
    fn warp_budget_saturates_without_wraparound_and_preserves_draw_fields() {
        let Some(mut warp) = WarpHarness::new() else {
            return;
        };
        const KIND_INDEXED: u32 = 1;
        const STRIDE: u32 = 24; // 5 indexed-draw words + 1 trailing custom word
        const RECORDS: u32 = 128; // 2 waves of 64: old InterlockedAdd wrapped 2^32 at 64!
        const BUDGET_CAP: u32 = 1 << 27;

        let total_bytes = (STRIDE * RECORDS) as usize;
        let (src_buf, src_ptr, _) = warp.shared_buffer(total_bytes as u64);
        let (_budget_buf, budget_ptr, budget_gpu) = warp.shared_buffer(256);
        let (readback_buf, readback_ptr, _) = warp.shared_buffer(total_bytes as u64);
        let dst_buf = warp.default_uav(total_bytes as u64);

        let mut initial_src = vec![0u8; total_bytes];
        for i in 0..RECORDS as usize {
            let words: [u32; 6] = [
                u32::MAX,                    // IndexCount -> clamps to 1 << 24
                u32::MAX,                    // InstanceCount -> clamps to 4 (cost = 1 << 26)
                1000 + i as u32,             // StartIndexLocation
                (-200i32 + i as i32) as u32, // BaseVertexLocation
                5000 + i as u32,             // StartInstanceLocation
                0xCAFE_0000 | (i as u32),    // Extra stride word
            ];
            let base = i * (STRIDE as usize);
            for (w, val) in words.into_iter().enumerate() {
                initial_src[base + w * 4..base + (w + 1) * 4].copy_from_slice(&val.to_le_bytes());
            }
        }

        // SAFETY: mapped buffers have sufficient capacity.
        unsafe {
            std::ptr::copy_nonoverlapping(initial_src.as_ptr(), src_ptr, total_bytes);
            (budget_ptr as *mut u32).write_volatile(0);
            std::ptr::write_bytes(readback_ptr, 0, total_bytes);
        }

        assert!(record_clamp(
            &warp.clamp,
            &warp.list,
            &dst_buf,
            budget_gpu,
            (&src_buf, 0),
            None,
            (KIND_INDEXED, STRIDE, RECORDS),
        ));
        done_on(&warp.list, &dst_buf);

        // SAFETY: open command list; copy clamped scratch buffer for verification.
        unsafe {
            warp.list.ResourceBarrier(&[transition(
                &dst_buf,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            )]);
            warp.list
                .CopyBufferRegion(&readback_buf, 0, &dst_buf, 0, total_bytes as u64);
            warp.list.ResourceBarrier(&[transition(
                &dst_buf,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
                D3D12_RESOURCE_STATE_COMMON,
            )]);
        }
        warp.submit_and_wait();

        // SAFETY: GPU work completed.
        let (spent, after_src, out_words) = unsafe {
            let spent = (budget_ptr as *const u32).read_volatile();
            let after_src = std::slice::from_raw_parts(src_ptr.cast_const(), total_bytes).to_vec();
            let out_words = std::slice::from_raw_parts(
                readback_ptr.cast::<u32>().cast_const(),
                total_bytes / 4,
            )
            .to_vec();
            (spent, after_src, out_words)
        };

        assert_eq!(
            spent, BUDGET_CAP,
            "budget must saturate at BUDGET without wrapping"
        );
        assert_eq!(after_src, initial_src, "src_args must remain untouched");

        let mut kept = 0usize;
        let mut zeroed = 0usize;
        for i in 0..RECORDS as usize {
            let rec = &out_words[i * 6..(i + 1) * 6];
            if rec[0] == (1 << 24) && rec[1] == 4 {
                kept += 1;
            } else {
                assert_eq!((rec[0], rec[1]), (0, 0), "excess draw must be zeroed");
                zeroed += 1;
            }
            assert_eq!(rec[2], 1000 + i as u32);
            assert_eq!(rec[3], (-200i32 + i as i32) as u32);
            assert_eq!(rec[4], 5000 + i as u32);
            assert_eq!(rec[5], 0xCAFE_0000 | (i as u32));
        }
        assert_eq!((kept, zeroed), (2, (RECORDS as usize) - 2));
    }

    /// Exercises `HostList::indirect_scratch`, `bound`, `done`, `Host::execute`,
    /// and `HostList::reset` end-to-end on the host device.
    #[test]
    fn host_list_reuses_and_grows_scratch_across_submissions() {
        let Some(host) = super::host() else {
            return;
        };
        let Ok(list) = host.new_list() else {
            return;
        };
        // The scratch buffer, whatever the switches in the environment.
        list.set_indirect_form(Form::default());
        let Some((src_buf, src_ptr)) = host.shared_buffer(256 << 10) else {
            return;
        };
        let Some((count_buf, count_ptr)) = host.shared_buffer(256) else {
            return;
        };
        // SAFETY: mapped host buffers.
        unsafe {
            std::ptr::write_bytes(src_ptr, 0x7F, 256 << 10);
            (count_ptr as *mut u32).write_volatile(2);
        }

        // First two calls fit inside the initial 64 KiB scratch buffer and
        // must reuse the exact same `ID3D12Resource`.
        let (b1, _) = bound(&list, (&src_buf, 0), Some((&count_buf, 0)), (0, 16, 64)).unwrap();
        done(&list, &b1);
        let (b2, _) = bound(&list, (&src_buf, 64), Some((&count_buf, 0)), (1, 20, 128)).unwrap();
        done(&list, &b2);
        assert_eq!(
            b1.as_raw(),
            b2.as_raw(),
            "scratch buffer within capacity must be reused"
        );

        // A call needing >64 KiB grows the scratch buffer mid-list while keeping
        // `b1` alive until reset.
        let (b3, _) = bound(&list, (&src_buf, 0), Some((&count_buf, 0)), (1, 20, 5000)).unwrap();
        done(&list, &b3);
        assert_ne!(
            b3.as_raw(),
            b1.as_raw(),
            "larger request must grow scratch buffer"
        );

        list.close();
        host.execute(&[&list]);
        list.reset();
        list.set_indirect_form(Form::default());

        // After reset, requests up to the grown high-water mark reuse `b3` with
        // zero new allocations.
        let (b4, _) = bound(&list, (&src_buf, 0), Some((&count_buf, 0)), (1, 20, 4096)).unwrap();
        done(&list, &b4);
        assert_eq!(
            b4.as_raw(),
            b3.as_raw(),
            "grown scratch buffer must persist across reset"
        );
        list.close();
        host.execute(&[&list]);
        list.reset();
    }

    /// Records clamped through the ring (`Form::ring`), through the ring in
    /// the list's prelude (`Form::prelude`) and by the one-lane clamp
    /// (`Form::single`) come out as through the scratch buffer: the same
    /// words for every active record, on the host device (the software
    /// adapter with `VICTORY_D3D12_WARP=1` in a diagnostics build). A
    /// record the CPU changes between two submissions of a list with the
    /// prelude is clamped anew at the second.
    #[test]
    fn ring_prelude_and_single_clamps_match_the_scratch_buffer() {
        let Some(host) = super::host() else {
            return;
        };
        if !enhanced_barriers() {
            eprintln!("the host takes no enhanced barriers: only the scratch buffer is used there");
            return;
        }
        let Ok(list) = host.new_list() else {
            return;
        };
        let Some(l7) = list.list7().cloned() else {
            return;
        };
        const STRIDE: u32 = 32;
        const ROOM: u64 = 256;
        struct Case {
            kind: u32,
            records: Vec<[u32; 8]>,
            ahead: bool,
            count: Option<u32>,
        }
        let tail = |i: u32| [0xA000 + i, 0xB000 + i, 0xC000 + i, 0xD000 + i, 0xE000 + i];
        let rec = |head: [u32; 3], i: u32| {
            let t = tail(i);
            [head[0], head[1], head[2], t[0], t[1], t[2], t[3], t[4]]
        };
        let cases = [
            // Dispatches: as asked, cut to 65,535 in x, too many groups.
            Case { kind: 2, records: vec![rec([3, 2, 1], 0), rec([70_000, 1, 1], 1), rec([u32::MAX; 3], 2)], ahead: false, count: None },
            // Draws: as asked, and the vertex count cut.
            Case { kind: 0, records: vec![rec([100, 3, 7], 3), rec([u32::MAX, 2, 0], 4)], ahead: false, count: None },
            // One indexed draw and one dispatch: the one-lane clamp's.
            Case { kind: 1, records: vec![rec([36, 4, 1], 5)], ahead: false, count: None },
            Case { kind: 2, records: vec![rec([9, 4, 1], 6)], ahead: false, count: None },
            // Mesh dispatches one by one, and a window clamped ahead whose
            // unfilled slot comes out empty.
            Case { kind: 10, records: vec![rec([5, 1, 1], 7), rec([u32::MAX, 7, 9], 8)], ahead: false, count: None },
            Case { kind: 10, records: vec![rec([6, 1, 1], 9), rec([u32::MAX; 3], 10)], ahead: true, count: None },
            // A count buffer: two of three records (never in the prelude).
            Case { kind: 2, records: vec![rec([2, 2, 2], 11), rec([1, 1, 1], 12), rec([4, 4, 4], 13)], ahead: false, count: Some(2) },
        ];
        let bytes = ROOM * cases.len() as u64;
        let (Some((src_buf, src_ptr)), Some((count_buf, count_ptr)), Some((back_buf, back_ptr))) =
            (host.shared_buffer(bytes), host.shared_buffer(256), host.shared_buffer(bytes))
        else {
            return;
        };
        let write_record = |case: usize, record: usize, words: &[u32; 8]| {
            for (k, w) in words.iter().enumerate() {
                // SAFETY: inside the mapping (a case's records fit its room).
                unsafe { src_ptr.add(case * ROOM as usize + record * STRIDE as usize + k * 4).cast::<u32>().write_volatile(*w) };
            }
        };
        for (i, case) in cases.iter().enumerate() {
            for (r, words) in case.records.iter().enumerate() {
                write_record(i, r, words);
            }
            // SAFETY: inside the 256-byte mapping.
            unsafe { count_ptr.add(i * 4).cast::<u32>().write_volatile(case.count.unwrap_or(0)) };
        }
        // The active records' words of each case, as read back.
        let read = || -> Vec<Vec<u32>> {
            cases
                .iter()
                .enumerate()
                .map(|(i, case)| {
                    let active = case.count.map_or(case.records.len(), |n| n as usize);
                    // SAFETY: the list has run; inside the mapping.
                    (0..active * 8).map(|k| unsafe { back_ptr.add(i * ROOM as usize + k * 4).cast::<u32>().read_volatile() }).collect()
                })
                .collect()
        };
        let copy_source = (D3D12_BARRIER_SYNC_COPY, D3D12_BARRIER_ACCESS_COPY_SOURCE);
        let args = D3D12_RESOURCE_STATE_INDIRECT_ARGUMENT;
        let mut outputs: Vec<(Form, Vec<Vec<u32>>)> = Vec::new();
        let forms = [
            Form::default(),
            Form { ring: true, ..Form::default() },
            Form { ring: true, prelude: true, ..Form::default() },
            Form { ring: true, single: true, ..Form::default() },
            Form { ring: true, prelude: true, single: true },
        ];
        for form in forms {
            list.set_indirect_form(form);
            // SAFETY: nothing runs; the mapping holds `bytes`.
            unsafe { std::ptr::write_bytes(back_ptr, 0, bytes as usize) };
            let mut rooms: Vec<(ID3D12Resource, u64, usize)> = Vec::new();
            for (i, case) in cases.iter().enumerate() {
                let spec = (case.kind, STRIDE, case.records.len() as u32);
                let src = (&src_buf, i as u64 * ROOM);
                let count = case.count.map(|_| (&count_buf, i as u64 * 4));
                let (buf, at) = if form.prelude && count.is_none() {
                    bound_early(&list, src, spec, case.ahead)
                } else if case.ahead {
                    bound_ahead(&list, src, spec)
                } else {
                    bound(&list, src, count, spec)
                }
                .expect("clamped");
                let size = u64::from(STRIDE) * u64::from(spec.2);
                if form.ring {
                    assert!(list.in_ring(&buf), "{form:?}: case {i} went to the ring");
                    assert!(rooms.iter().all(|(_, a, _)| *a != at), "{form:?}: case {i} has room of its own");
                    done(&list, &buf);
                    rooms.push((buf, at, i));
                } else {
                    assert_eq!(at, 0, "the scratch buffer is written from its start");
                    // SAFETY: an open list; the scratch buffer was left
                    // readable as arguments by the clamp.
                    unsafe {
                        list.list.ResourceBarrier(&[transition(&buf, args, D3D12_RESOURCE_STATE_COPY_SOURCE)]);
                        list.list.CopyBufferRegion(&back_buf, i as u64 * ROOM, &buf, 0, size);
                        list.list.ResourceBarrier(&[transition(&buf, D3D12_RESOURCE_STATE_COPY_SOURCE, args)]);
                    }
                    done(&list, &buf);
                }
            }
            if form.prelude {
                assert_eq!(list.deferred_early(), cases.iter().filter(|c| c.count.is_none()).count());
            }
            // The rings' rooms, read as the indirect calls would, copied out.
            let mut seen: Vec<usize> = Vec::new();
            for (buf, _, _) in &rooms {
                if !seen.contains(&(buf.as_raw() as usize)) {
                    seen.push(buf.as_raw() as usize);
                    buffer_barrier(&l7, buf, INDIRECT_READ, copy_source);
                }
            }
            for (buf, at, i) in &rooms {
                let size = u64::from(STRIDE) * cases[*i].records.len() as u64;
                // SAFETY: an open list; the ring is a copy source after the
                // barrier above; the read-back buffer is ours.
                unsafe { list.list.CopyBufferRegion(&back_buf, *i as u64 * ROOM, buf, *at, size) };
            }
            list.close();
            assert_eq!(host.log_messages(), None, "{form:?}: the debug layer found an error");
            host.execute(&[&list]);
            let out = read();
            if form.prelude {
                // The CPU changes a record; the list is submitted again: its
                // prelude clamps it anew.
                write_record(0, 0, &rec([4, 4, 1], 0));
                host.execute(&[&list]);
                assert_eq!(read()[0][..3], [4, 4, 1], "{form:?}: the prelude clamps at each submission");
                write_record(0, 0, &cases[0].records[0]);
            }
            list.reset();
            outputs.push((form, out));
        }
        let (_, scratch) = &outputs[0];
        // What the clamp makes of the records, through the scratch buffer.
        assert_eq!(scratch[0][..3], [3, 2, 1]);
        assert_eq!(scratch[0][8..11], [65_535, 1, 1]);
        assert_eq!(scratch[0][16..19], [0, 0, 0]);
        assert_eq!(scratch[0][19..24], tail(2), "the words after the record are copied");
        assert_eq!(scratch[1][8..10], [1 << 24, 2]);
        assert_eq!(scratch[4][8..11], [65_535, 1, 1]);
        assert_eq!(scratch[5][8..11], [0, 0, 0], "the unfilled slot of a window comes out empty");
        assert_eq!(scratch[6].len(), 16, "two records under the count");
        for (form, out) in &outputs[1..] {
            for (i, case) in out.iter().enumerate() {
                assert_eq!(case, &scratch[i], "{form:?}: case {i} as through the scratch buffer");
            }
        }
    }

    /// Each clamp into the ring takes room of its own, a full ring is
    /// replaced by one twice as big (the old one held until the reset), and
    /// after the list's reset the room is taken from the start again.
    #[test]
    fn ring_gives_each_clamp_room_of_its_own() {
        let Some(host) = super::host() else {
            return;
        };
        if !enhanced_barriers() {
            return;
        }
        let Ok(list) = host.new_list() else {
            return;
        };
        if list.list7().is_none() {
            return;
        }
        list.set_indirect_form(Form { ring: true, ..Form::default() });
        let (Some((src_buf, src_ptr)), Some((count_buf, count_ptr))) = (host.shared_buffer(512 << 10), host.shared_buffer(256)) else {
            return;
        };
        // SAFETY: mapped host buffers.
        unsafe {
            std::ptr::write_bytes(src_ptr, 0, 512 << 10);
            (count_ptr as *mut u32).write_volatile(2);
        }
        let (r1, a1) = bound(&list, (&src_buf, 0), Some((&count_buf, 0)), (0, 16, 64)).unwrap();
        let (r2, a2) = bound(&list, (&src_buf, 64), Some((&count_buf, 0)), (1, 20, 128)).unwrap();
        assert_eq!(r1.as_raw(), r2.as_raw(), "room in the same ring");
        assert_eq!(a1, 0);
        assert_eq!(a2, 64 * 16, "the second clamp's room follows the first's (256-byte aligned)");
        // More than the ring holds: a bigger ring, from its start.
        let (r3, a3) = bound(&list, (&src_buf, 0), Some((&count_buf, 0)), (1, 20, 20_000)).unwrap();
        assert_ne!(r3.as_raw(), r1.as_raw(), "a full ring is replaced");
        assert_eq!(a3, 0);
        assert!(list.in_ring(&r1) && list.in_ring(&r3), "the old ring is held until the reset");
        list.close();
        assert_eq!(host.log_messages(), None, "the debug layer found an error");
        host.execute(&[&list]);
        list.reset();
        list.set_indirect_form(Form { ring: true, ..Form::default() });
        assert!(!list.in_ring(&r1), "the old ring is let go at the reset");
        let (r4, a4) = bound(&list, (&src_buf, 0), Some((&count_buf, 0)), (1, 20, 64)).unwrap();
        assert_eq!(r4.as_raw(), r3.as_raw(), "the ring stays across a reset");
        assert_eq!(a4, 0, "and its room is taken from the start again");
        list.close();
        assert_eq!(host.log_messages(), None, "the debug layer found an error");
        host.execute(&[&list]);
        list.reset();
    }

    /// The one-lane clamp makes of a single record what the clamp of 64
    /// lanes makes of it, budget and its notes included, on the software
    /// adapter (after fxceless/victory-fork PR #1016's test).
    #[test]
    fn warp_single_record_matches_the_group_clamp() {
        let Some(mut warp) = WarpHarness::new() else {
            return;
        };
        let single = make_single_for(&warp.device).expect("the one-lane clamp must compile on WARP");
        const STRIDE: u32 = 32;
        const BUDGET: u32 = 1 << 27;
        // Padding words, an offset source and count, both sides of the
        // budget running out; a zero count leaves the room as it was.
        let cases = [
            (0, [3, 2, 17], None, 0, false),
            (1, [9, 4, u32::MAX], Some(3), BUDGET - 36, false),
            (1, [9, 4, u32::MAX], None, BUDGET - 35, false),
            (2, [u32::MAX, 32, 1], Some(1), 0, false),
            (2, [7, 1, 1], Some(0), 42, false),
            (2, [100_000, 1, 1], None, 0, false),
            (10, [u32::MAX, 5, 2], None, 0, false),
            (10, [u32::MAX, 5, 2], None, 0, true),
            (10, [0, 1, 1], None, BUDGET, false),
            (10, [1, 1, 1], None, BUDGET, false),
        ];
        let (src, src_ptr, _) = warp.shared_buffer(64);
        let (count, count_ptr, _) = warp.shared_buffer(8);
        let (_budget, budget_ptr, budget_gpu) = warp.shared_buffer(256);
        let (back, back_ptr, _) = warp.shared_buffer(u64::from(STRIDE));
        let group = Clamp { root: warp.clamp.root.clone(), pso: warp.clamp.pso.clone(), zero: warp.clamp.zero.clone(), lanes: 64 };
        for (kind, args, count_value, spent, ahead) in cases {
            let mut outcomes = Vec::new();
            for clamp in [&group, &single] {
                let dst = warp.default_uav(u64::from(STRIDE));
                let record = [args[0], args[1], args[2], 4, 5, 6, 7, 8];
                // SAFETY: CPU writes to mapped buffers after the previous
                // submission finished; the record starts at byte 32.
                unsafe {
                    src_ptr.add(32).cast::<[u32; 8]>().write(record);
                    count_ptr.add(4).cast::<u32>().write(count_value.unwrap_or(0));
                    budget_ptr.cast::<[u32; 5]>().write([spent, 17, 19, 23, 29]);
                    warp.list.ResourceBarrier(&[transition(&dst, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_COPY_DEST)]);
                    warp.list.CopyBufferRegion(&dst, 0, &src, 32, u64::from(STRIDE));
                }
                CLAMP_FROM.set(D3D12_RESOURCE_STATE_COPY_DEST);
                let recorded = record_clamp_as(
                    clamp,
                    &warp.list,
                    &dst,
                    budget_gpu,
                    (&src, 32),
                    count_value.map(|_| (&count, 4)),
                    (kind, STRIDE, 1),
                    ahead,
                );
                CLAMP_FROM.set(D3D12_RESOURCE_STATE_COMMON);
                assert!(recorded);
                // SAFETY: an open list; the clamp left dst readable as arguments.
                unsafe {
                    warp.list.ResourceBarrier(&[transition(
                        &dst,
                        D3D12_RESOURCE_STATE_INDIRECT_ARGUMENT,
                        D3D12_RESOURCE_STATE_COPY_SOURCE,
                    )]);
                    warp.list.CopyBufferRegion(&back, 0, &dst, 0, u64::from(STRIDE));
                }
                warp.submit_and_wait();
                // SAFETY: the submission ended; both buffers stay mapped.
                let outcome = unsafe { (budget_ptr.cast::<[u32; 5]>().read(), back_ptr.cast::<[u32; 8]>().read()) };
                if count_value == Some(0) {
                    assert_eq!(outcome, ([spent, 17, 19, 23, 29], record));
                }
                outcomes.push(outcome);
            }
            assert_eq!(outcomes[0], outcomes[1], "{kind} {args:?} {count_value:?} {spent} {ahead}");
        }
    }

    #[test]
    fn parse_clamp_enabled_defaults_on_and_respects_explicit_disable() {
        use std::ffi::OsStr;
        assert!(parse_clamp_enabled(None));
        for on in ["", "1", " 1 ", "true", "TRUE", "yes", "on"] {
            assert!(
                parse_clamp_enabled(Some(OsStr::new(on))),
                "{on:?} must keep indirect clamp enabled"
            );
        }
        for off in ["0", " 0 ", "false", "FALSE", "no", "NO", "off", "OFF"] {
            assert!(
                !parse_clamp_enabled(Some(OsStr::new(off))),
                "{off:?} must disable indirect clamp"
            );
        }
    }

    #[test]
    fn validate_layout_rejects_misaligned_or_overflowing_direct_arguments() {
        assert_eq!(validate_layout((0, 16, 4), 0, None), Some(64));
        assert_eq!(validate_layout((10, 12, 8), 16, Some(4)), Some(96));
        assert_eq!(validate_layout((0, 12, 1), 0, None), None);
        assert_eq!(validate_layout((0, 16, 1), 2, None), None);
        assert_eq!(validate_layout((0, 16, 1), 0, Some(2)), None);
        assert_eq!(validate_layout((0, 16, u32::MAX), 0, None), None);
    }
}

thread_local! {
    /// Whether the next clamp waits for earlier shader writes (`true`
    /// unless the recording thread said otherwise for this one).
    static NEEDS_WAIT: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// Says whether the clamps this thread records from here on wait for
/// every earlier shader write before and after their pass.
pub(crate) fn needs_wait(needed: bool) {
    NEEDS_WAIT.set(needed);
}

/// `VICTORY_CLAMP_WAIT=never`: the clamp records no UAV barrier before or
/// after its pass, only the scratch buffer's transitions. The one before
/// stands behind the title's own barrier between the pass that writes the
/// arguments and its ExecuteIndirect (the clamp is recorded at the
/// ExecuteIndirect); the one after is covered by the transition from
/// `UNORDERED_ACCESS` to `INDIRECT_ARGUMENT`, which waits for the clamp's
/// writes. Diagnostics build, opening cutscene (`VICTORY_BARRIER_TEST=
/// indirect`, the same waits left out): 7.5-8.0 -> 8.0-9.8 fps, picture
/// unchanged in screenshots. An Nsight GPU Trace counted 5,000-6,100
/// pipeline drains a frame with the shader units 5-8% busy.
pub(crate) fn wait_never() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_CLAMP_WAIT").is_some_and(|v| v == "never"))
}

/// `VICTORY_CLAMP_WAIT=auto`.
pub(crate) fn wait_auto() -> bool {
    if let Some(v) = crate::host::live::value("clamp_wait") {
        return v == "auto";
    }
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_CLAMP_WAIT").is_some_and(|v| v == "auto"))
}

/// What one small draw costs this card, bare and with one thing changed
/// between every two draws: a measurement, not a check (it asserts only
/// that the card answered). The timing run of the lit scene (run 187) has
/// some 11,000 small commands a frame at 5 to 16 microseconds each, and no
/// log says whether that is what we put around a command or what the card
/// takes for one. Each variant here is 10,000 one-triangle draws into a
/// 2560x1440 target, timed on the card by one pair of timestamps around
/// the whole batch (not one per command, which would time the stamps).
///
/// Run by name, no game needed:
/// `cargo test -p victory-gpu --release -- --ignored bare_command_cost --nocapture`
#[cfg(test)]
mod bare_cost {
    use windows::Win32::Foundation::{HANDLE, RECT};
    use windows::Win32::Graphics::Direct3D::{D3D_FEATURE_LEVEL_12_0, D3D_PRIMITIVE_TOPOLOGY};
    use windows::Win32::Graphics::Direct3D12::*;
    use windows::Win32::Graphics::Dxgi::Common::*;
    use windows::Win32::Graphics::Dxgi::*;
    use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
    use windows::core::{Interface, s};

    use crate::host::{compile, transition};

    /// Draws in a batch.
    const DRAWS: u32 = 10_000;
    const WIDTH: u32 = 2560;
    const HEIGHT: u32 = 1440;

    /// One small triangle in the middle of the target (about 180 pixels),
    /// and two pixel programs that read one texel of a texture through a
    /// table, so that the view heap set is in use.
    const DRAW: &str = r#"
#define RS "DescriptorTable(SRV(t0))"
Texture2D<float4> image : register(t0);
[RootSignature(RS)]
float4 corner(uint i : SV_VertexID) : SV_Position {
    float2 p = float2((i << 1) & 2, i & 2);
    return float4(p * 0.02 - 0.01, 0.5, 1.0);
}
[RootSignature(RS)]
float4 first(float4 at : SV_Position) : SV_Target {
    return image.Load(int3(0, 0, 0));
}
[RootSignature(RS)]
float4 second(float4 at : SV_Position) : SV_Target {
    return 1.0 - image.Load(int3(0, 0, 0));
}
"#;

    /// A compute pass of next to nothing, as our clamp of an indirect
    /// record is: one group of one thread that writes one word.
    const WORK: &str = r#"
#define RS "UAV(u0)"
RWByteAddressBuffer words : register(u0);
[RootSignature(RS)]
[numthreads(1, 1, 1)]
void work() {
    words.Store(0, words.Load(0) + 1);
}
"#;

    /// What changes between every two draws of a batch.
    #[derive(Clone, Copy, PartialEq)]
    enum Between {
        /// Nothing.
        Nothing,
        /// The pipeline: two alternate.
        Pipeline,
        /// A second texture goes from target to shader read and back.
        BarrierPair,
        /// The depth texture bound for testing goes from write to read or
        /// back.
        DepthMove,
        /// The pair of descriptor heaps: two pairs alternate.
        Heaps,
        /// The colour target: two alternate.
        Targets,
        /// A compute pass of ours of one group, and the pipeline back.
        Compute,
        /// The same between two waits for every earlier shader write, as
        /// the clamp records them.
        ComputeWithWaits,
    }

    struct Rig {
        queue: ID3D12CommandQueue,
        allocator: ID3D12CommandAllocator,
        list: ID3D12GraphicsCommandList,
        fence: ID3D12Fence,
        submitted: u64,
        event: HANDLE,
        root: ID3D12RootSignature,
        pipelines: [ID3D12PipelineState; 2],
        work_root: ID3D12RootSignature,
        work: ID3D12PipelineState,
        words: ID3D12Resource,
        other: ID3D12Resource,
        depth: ID3D12Resource,
        /// Kept alive: the views below name them.
        _kept: Vec<ID3D12Resource>,
        _target_heaps: [ID3D12DescriptorHeap; 2],
        heap_pairs: [[Option<ID3D12DescriptorHeap>; 2]; 2],
        tables: [D3D12_GPU_DESCRIPTOR_HANDLE; 2],
        targets: [D3D12_CPU_DESCRIPTOR_HANDLE; 2],
        /// Kept with the bench: the view the depth clears go through.
        _depth_written: D3D12_CPU_DESCRIPTOR_HANDLE,
        depth_tested: D3D12_CPU_DESCRIPTOR_HANDLE,
        stamps: ID3D12QueryHeap,
        back: ID3D12Resource,
        card: String,
    }

    fn texture(
        device: &ID3D12Device,
        (width, height): (u32, u32),
        format: DXGI_FORMAT,
        flags: D3D12_RESOURCE_FLAGS,
        state: D3D12_RESOURCE_STATES,
    ) -> ID3D12Resource {
        let heap = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_DEFAULT,
            ..Default::default()
        };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Width: u64::from(width),
            Height: height,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Flags: flags,
            ..Default::default()
        };
        let mut out: Option<ID3D12Resource> = None;
        // SAFETY: plain texture creation.
        unsafe {
            device
                .CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &desc, state, None, &mut out)
                .unwrap();
        }
        out.unwrap()
    }

    fn buffer(
        device: &ID3D12Device,
        size: u64,
        kind: D3D12_HEAP_TYPE,
        flags: D3D12_RESOURCE_FLAGS,
        state: D3D12_RESOURCE_STATES,
    ) -> ID3D12Resource {
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
        // SAFETY: plain buffer creation.
        unsafe {
            device
                .CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &desc, state, None, &mut out)
                .unwrap();
        }
        out.unwrap()
    }

    fn heap(device: &ID3D12Device, kind: D3D12_DESCRIPTOR_HEAP_TYPE, count: u32, visible: bool) -> ID3D12DescriptorHeap {
        let flags = if visible {
            D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE
        } else {
            D3D12_DESCRIPTOR_HEAP_FLAG_NONE
        };
        // SAFETY: plain heap creation.
        unsafe {
            device
                .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                    Type: kind,
                    NumDescriptors: count,
                    Flags: flags,
                    NodeMask: 0,
                })
                .unwrap()
        }
    }

    fn pipeline(device: &ID3D12Device, root: &ID3D12RootSignature, vs: &[u8], ps: &[u8]) -> ID3D12PipelineState {
        let code = |bytes: &[u8]| D3D12_SHADER_BYTECODE {
            pShaderBytecode: bytes.as_ptr().cast(),
            BytecodeLength: bytes.len(),
        };
        let mut blend = D3D12_BLEND_DESC::default();
        blend.RenderTarget[0].RenderTargetWriteMask = 0xf;
        let mut formats = [DXGI_FORMAT(0); 8];
        formats[0] = DXGI_FORMAT_R8G8B8A8_UNORM;
        // SAFETY: the description points at live data and never releases
        // its pointer to the root signature.
        unsafe {
            let desc = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
                pRootSignature: std::mem::transmute_copy(root),
                VS: code(vs),
                PS: code(ps),
                BlendState: blend,
                SampleMask: u32::MAX,
                RasterizerState: D3D12_RASTERIZER_DESC {
                    FillMode: D3D12_FILL_MODE_SOLID,
                    CullMode: D3D12_CULL_MODE_NONE,
                    DepthClipEnable: true.into(),
                    ..Default::default()
                },
                // Depth is tested and never written, so the texture may be
                // in the write state or in the read state under one view.
                DepthStencilState: D3D12_DEPTH_STENCIL_DESC {
                    DepthEnable: true.into(),
                    DepthWriteMask: D3D12_DEPTH_WRITE_MASK_ZERO,
                    DepthFunc: D3D12_COMPARISON_FUNC_LESS_EQUAL,
                    ..Default::default()
                },
                PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
                NumRenderTargets: 1,
                RTVFormats: formats,
                DSVFormat: DXGI_FORMAT_D32_FLOAT,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                ..Default::default()
            };
            device.CreateGraphicsPipelineState(&desc).unwrap()
        }
    }

    impl Rig {
        /// The default adapter, as the layer takes it; `None` without one
        /// or on a software adapter.
        fn new() -> Option<Self> {
            let vs = compile(DRAW, s!("corner"), s!("vs_5_1")).ok()?;
            let ps = [
                compile(DRAW, s!("first"), s!("ps_5_1")).ok()?,
                compile(DRAW, s!("second"), s!("ps_5_1")).ok()?,
            ];
            let cs = compile(WORK, s!("work"), s!("cs_5_1")).ok()?;
            // SAFETY: plain object creation on a device of our own; the
            // views are made in heaps that live as long as the rig.
            unsafe {
                let mut device: Option<ID3D12Device> = None;
                D3D12CreateDevice(None, D3D_FEATURE_LEVEL_12_0, &mut device).ok()?;
                let device = device?;
                let desc = CreateDXGIFactory2::<IDXGIFactory4>(DXGI_CREATE_FACTORY_FLAGS(0))
                    .and_then(|factory| factory.EnumAdapterByLuid::<IDXGIAdapter3>(device.GetAdapterLuid()))
                    .ok()?
                    .cast::<IDXGIAdapter1>()
                    .ok()?
                    .GetDesc1()
                    .ok()?;
                if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                    return None;
                }
                let card = String::from_utf16_lossy(&desc.Description)
                    .trim_end_matches('\0')
                    .to_owned();
                let queue: ID3D12CommandQueue = device
                    .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                        Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                        ..Default::default()
                    })
                    .ok()?;
                let allocator: ID3D12CommandAllocator =
                    device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT).ok()?;
                let list: ID3D12GraphicsCommandList = device
                    .CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None)
                    .ok()?;
                let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE).ok()?;
                let event = CreateEventW(None, false, false, None).ok()?;

                let root: ID3D12RootSignature = device.CreateRootSignature(0, &vs[..]).ok()?;
                let pipelines = [
                    pipeline(&device, &root, &vs, &ps[0]),
                    pipeline(&device, &root, &vs, &ps[1]),
                ];
                let work_root: ID3D12RootSignature = device.CreateRootSignature(0, &cs[..]).ok()?;
                let work: ID3D12PipelineState = device
                    .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                        pRootSignature: std::mem::transmute_copy(&work_root),
                        CS: D3D12_SHADER_BYTECODE {
                            pShaderBytecode: cs.as_ptr().cast(),
                            BytecodeLength: cs.len(),
                        },
                        ..Default::default()
                    })
                    .ok()?;

                let size = (WIDTH, HEIGHT);
                let colour = |state| texture(&device, size, DXGI_FORMAT_R8G8B8A8_UNORM, D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET, state);
                let target = [
                    colour(D3D12_RESOURCE_STATE_RENDER_TARGET),
                    colour(D3D12_RESOURCE_STATE_RENDER_TARGET),
                ];
                let other = colour(D3D12_RESOURCE_STATE_RENDER_TARGET);
                let depth = texture(
                    &device,
                    size,
                    DXGI_FORMAT_D32_FLOAT,
                    D3D12_RESOURCE_FLAG_ALLOW_DEPTH_STENCIL,
                    D3D12_RESOURCE_STATE_DEPTH_WRITE,
                );
                let image = texture(
                    &device,
                    (4, 4),
                    DXGI_FORMAT_R8G8B8A8_UNORM,
                    D3D12_RESOURCE_FLAG_NONE,
                    D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
                );
                let words = buffer(
                    &device,
                    64 << 10,
                    D3D12_HEAP_TYPE_DEFAULT,
                    D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
                    D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                );
                let back = buffer(
                    &device,
                    16,
                    D3D12_HEAP_TYPE_READBACK,
                    D3D12_RESOURCE_FLAG_NONE,
                    D3D12_RESOURCE_STATE_COPY_DEST,
                );
                let mut stamps: Option<ID3D12QueryHeap> = None;
                device
                    .CreateQueryHeap(
                        &D3D12_QUERY_HEAP_DESC {
                            Type: D3D12_QUERY_HEAP_TYPE_TIMESTAMP,
                            Count: 2,
                            NodeMask: 0,
                        },
                        &mut stamps,
                    )
                    .ok()?;
                let stamps = stamps?;

                // Target and depth views.
                let rtvs = heap(&device, D3D12_DESCRIPTOR_HEAP_TYPE_RTV, 2, false);
                let rtv_step = device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_RTV) as usize;
                let mut targets = [rtvs.GetCPUDescriptorHandleForHeapStart(); 2];
                targets[1].ptr += rtv_step;
                device.CreateRenderTargetView(&target[0], None, targets[0]);
                device.CreateRenderTargetView(&target[1], None, targets[1]);
                let dsvs = heap(&device, D3D12_DESCRIPTOR_HEAP_TYPE_DSV, 2, false);
                let dsv_step = device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_DSV) as usize;
                let depth_written = dsvs.GetCPUDescriptorHandleForHeapStart();
                let mut depth_tested = depth_written;
                depth_tested.ptr += dsv_step;
                device.CreateDepthStencilView(&depth, None, depth_written);
                let tested = D3D12_DEPTH_STENCIL_VIEW_DESC {
                    Format: DXGI_FORMAT_D32_FLOAT,
                    ViewDimension: D3D12_DSV_DIMENSION_TEXTURE2D,
                    Flags: D3D12_DSV_FLAG_READ_ONLY_DEPTH,
                    ..Default::default()
                };
                device.CreateDepthStencilView(&depth, Some(&tested), depth_tested);

                // Two pairs of shader-visible heaps: views (one view of the
                // small texture each) and samplers (none used).
                let view_heaps = [
                    heap(&device, D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV, 1, true),
                    heap(&device, D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV, 1, true),
                ];
                let sampler_heaps = [
                    heap(&device, D3D12_DESCRIPTOR_HEAP_TYPE_SAMPLER, 1, true),
                    heap(&device, D3D12_DESCRIPTOR_HEAP_TYPE_SAMPLER, 1, true),
                ];
                for views in &view_heaps {
                    device.CreateShaderResourceView(&image, None, views.GetCPUDescriptorHandleForHeapStart());
                }
                let tables = [
                    view_heaps[0].GetGPUDescriptorHandleForHeapStart(),
                    view_heaps[1].GetGPUDescriptorHandleForHeapStart(),
                ];
                let heap_pairs = [
                    [Some(view_heaps[0].clone()), Some(sampler_heaps[0].clone())],
                    [Some(view_heaps[1].clone()), Some(sampler_heaps[1].clone())],
                ];

                // The first list: both targets and the depth texture cleared.
                list.ClearRenderTargetView(targets[0], &[0.0; 4], None);
                list.ClearRenderTargetView(targets[1], &[0.0; 4], None);
                list.ClearDepthStencilView(depth_written, D3D12_CLEAR_FLAG_DEPTH, 1.0, 0, None);
                let [first_target, second_target] = target;
                let mut rig = Rig {
                    queue,
                    allocator,
                    list,
                    fence,
                    submitted: 0,
                    event,
                    root,
                    pipelines,
                    work_root,
                    work,
                    words,
                    other,
                    depth,
                    _kept: vec![first_target, second_target, image],
                    _target_heaps: [rtvs, dsvs],
                    heap_pairs,
                    tables,
                    targets,
                    _depth_written: depth_written,
                    depth_tested,
                    stamps,
                    back,
                    card,
                };
                rig.finish();
                Some(rig)
            }
        }

        /// Closes the open list, runs it, waits for the card and opens the
        /// list again.
        fn finish(&mut self) {
            self.submitted += 1;
            // SAFETY: our list, queue and fence; the allocator is reset
            // only after the card finished everything recorded from it.
            unsafe {
                self.list.Close().unwrap();
                let run: ID3D12CommandList = self.list.cast().unwrap();
                self.queue.ExecuteCommandLists(&[Some(run)]);
                self.queue.Signal(&self.fence, self.submitted).unwrap();
                self.fence.SetEventOnCompletion(self.submitted, self.event).unwrap();
                WaitForSingleObject(self.event, INFINITE);
                self.allocator.Reset().unwrap();
                self.list.Reset(&self.allocator, None).unwrap();
            }
        }

        /// What every list starts with: a new list has nothing set.
        fn begin(&self) {
            let l = &self.list;
            // SAFETY: an open list and live objects and views.
            unsafe {
                l.SetDescriptorHeaps(&self.heap_pairs[0]);
                l.SetGraphicsRootSignature(&self.root);
                l.SetPipelineState(&self.pipelines[0]);
                l.SetGraphicsRootDescriptorTable(0, self.tables[0]);
                l.OMSetRenderTargets(1, Some(&self.targets[0]), false, Some(&self.depth_tested));
                l.RSSetViewports(&[D3D12_VIEWPORT {
                    TopLeftX: 0.0,
                    TopLeftY: 0.0,
                    Width: WIDTH as f32,
                    Height: HEIGHT as f32,
                    MinDepth: 0.0,
                    MaxDepth: 1.0,
                }]);
                l.RSSetScissorRects(&[RECT {
                    left: 0,
                    top: 0,
                    right: WIDTH as i32,
                    bottom: HEIGHT as i32,
                }]);
                // A triangle list.
                l.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY(4));
            }
        }

        /// One batch of [`DRAWS`] draws in `lists` lists run one by one,
        /// with `between` recorded before every draw: the microseconds a
        /// draw took on the card, and the milliseconds recording took.
        fn batch(&mut self, between: Between, lists: u32) -> (f64, f64) {
            let (target, read, written, tested) = (
                D3D12_RESOURCE_STATE_RENDER_TARGET,
                D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
                D3D12_RESOURCE_STATE_DEPTH_WRITE,
                D3D12_RESOURCE_STATE_DEPTH_READ,
            );
            let started = std::time::Instant::now();
            let mut drawn = 0u32;
            for part in 0..lists {
                let l = &self.list;
                // SAFETY: an open list; our query heap; live objects.
                unsafe {
                    if part == 0 {
                        l.EndQuery(&self.stamps, D3D12_QUERY_TYPE_TIMESTAMP, 0);
                    }
                }
                self.begin();
                // SAFETY: an open list with everything set; live objects.
                // Every state moved is moved back within the batch (the
                // draws of a list are an even number).
                unsafe {
                    for _ in 0..DRAWS / lists {
                        let odd = (drawn % 2) as usize;
                        match between {
                            Between::Nothing => {}
                            Between::Pipeline => l.SetPipelineState(&self.pipelines[odd]),
                            Between::BarrierPair => {
                                l.ResourceBarrier(&[transition(&self.other, target, read)]);
                                l.ResourceBarrier(&[transition(&self.other, read, target)]);
                            }
                            Between::DepthMove => {
                                let (from, to) = if odd == 0 { (written, tested) } else { (tested, written) };
                                l.ResourceBarrier(&[transition(&self.depth, from, to)]);
                            }
                            Between::Heaps => {
                                l.SetDescriptorHeaps(&self.heap_pairs[odd]);
                                l.SetGraphicsRootDescriptorTable(0, self.tables[odd]);
                            }
                            Between::Targets => {
                                l.OMSetRenderTargets(1, Some(&self.targets[odd]), false, Some(&self.depth_tested));
                            }
                            Between::Compute | Between::ComputeWithWaits => {
                                let waits = between == Between::ComputeWithWaits;
                                if waits {
                                    l.ResourceBarrier(&[super::uav_barrier()]);
                                }
                                l.SetComputeRootSignature(&self.work_root);
                                l.SetPipelineState(&self.work);
                                l.SetComputeRootUnorderedAccessView(0, self.words.GetGPUVirtualAddress());
                                l.Dispatch(1, 1, 1);
                                if waits {
                                    l.ResourceBarrier(&[super::uav_barrier()]);
                                }
                                l.SetPipelineState(&self.pipelines[0]);
                            }
                        }
                        l.DrawInstanced(3, 1, 0, 0);
                        drawn += 1;
                    }
                    if part + 1 < lists {
                        // The next list of the batch: this one is sent on
                        // its own and the list is opened again at once
                        // (the allocator is not reset before the card is
                        // done with all of them).
                        l.Close().unwrap();
                        let run: ID3D12CommandList = l.cast().unwrap();
                        self.queue.ExecuteCommandLists(&[Some(run)]);
                        l.Reset(&self.allocator, None).unwrap();
                    } else {
                        l.EndQuery(&self.stamps, D3D12_QUERY_TYPE_TIMESTAMP, 1);
                        l.ResolveQueryData(&self.stamps, D3D12_QUERY_TYPE_TIMESTAMP, 0, 2, &self.back, 0);
                    }
                }
            }
            let recording = started.elapsed().as_secs_f64() * 1e3;
            self.finish();
            let mut data = std::ptr::null_mut();
            let range = D3D12_RANGE { Begin: 0, End: 16 };
            // SAFETY: a read-back buffer the finished list resolved two
            // timestamps into.
            let ticks = unsafe {
                self.back.Map(0, Some(&range), Some(&mut data)).unwrap();
                let stamps = std::slice::from_raw_parts(data.cast::<u64>(), 2).to_vec();
                self.back.Unmap(0, None);
                stamps[1].saturating_sub(stamps[0])
            };
            // SAFETY: a plain query of our queue.
            let frequency = unsafe { self.queue.GetTimestampFrequency() }.unwrap_or(1).max(1);
            (ticks as f64 * 1e6 / frequency as f64 / f64::from(drawn.max(1)), recording)
        }
    }

    #[test]
    #[ignore = "a measurement on the card: run it by name with --ignored --nocapture"]
    fn bare_command_cost() {
        let Some(mut rig) = Rig::new() else {
            eprintln!("no hardware adapter, or our shaders did not compile; skipped");
            return;
        };
        eprintln!("bare command cost on {}: {DRAWS} one-triangle draws into {WIDTH}x{HEIGHT}, three batches each", rig.card);
        // The first batch pays for whatever the driver does at first use.
        rig.batch(Between::Nothing, 1);
        let variants = [
            ("(a) nothing changes between draws", Between::Nothing, 1),
            ("(b) another pipeline every draw", Between::Pipeline, 1),
            ("(c) a barrier pair on a second texture every draw", Between::BarrierPair, 1),
            ("(d) the tested depth texture moved between write and read every draw", Between::DepthMove, 1),
            ("(e) another pair of descriptor heaps every draw", Between::Heaps, 1),
            ("(f) another colour target every draw", Between::Targets, 1),
            ("(g) nothing changes, in 100 lists run one by one", Between::Nothing, 100),
            ("(h) a compute pass of ours of one group before every draw", Between::Compute, 1),
            ("(i) the same pass between two waits for earlier shader writes", Between::ComputeWithWaits, 1),
        ];
        let mut least = Vec::new();
        for (name, between, lists) in variants {
            let mut runs: Vec<(f64, f64)> = (0..3).map(|_| rig.batch(between, lists)).collect();
            runs.sort_by(|a, b| a.0.total_cmp(&b.0));
            eprintln!(
                "{name}: {:.2} microseconds a draw on the card (the three: {:.2}, {:.2}, {:.2}); recording took {:.1} ms",
                runs[0].0, runs[0].0, runs[1].0, runs[2].0, runs[0].1
            );
            least.push(runs[0].0);
        }
        assert!(least.iter().all(|us| us.is_finite() && *us > 0.0), "the card gave no time for a batch");
    }
}
