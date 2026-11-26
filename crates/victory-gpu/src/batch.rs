//! Title dispatches one after the other with the same host pipeline, sent
//! as one `ExecuteIndirect` whose records set each one's root arguments
//! and dispatch it (`VICTORY_BATCH_DISPATCH=1`, live `batch_dispatch`).
//!
//! A frame of the opening scene starts some 16,000 title dispatches on the
//! card, 82% of them in runs with the same pipeline and no barrier between
//! (about eight to a run), each with its own root constants and root views.
//! While a run lasts, the root arguments the bind of each dispatch sets
//! are kept here with its group counts instead of being recorded; the run
//! is recorded as one indirect execution when anything else is recorded in
//! the list: a bind for another command, a barrier of ours, the list's end.
//!
//! Root descriptor tables cannot be set by an indirect record: a dispatch
//! whose bind sets one ends the run and is recorded as before.
//!
//! The one-record indirect mesh draws drawn through our amplification
//! stage (`VICTORY_INDIRECT_AMPLIFY=1`) go the same way
//! (`VICTORY_BATCH_MESH=1`, live `batch_mesh`): some 18,000 a frame, one
//! group each, 79% in runs with the same pipeline; their records set the
//! graphics root arguments and end with `DISPATCH_MESH`.

use std::cell::RefCell;
use std::sync::Mutex;
use windows::Win32::Graphics::Direct3D12::*;
use windows::core::Interface;

/// A root argument the bind of a dispatch sets.
#[derive(Clone)]
pub(crate) enum Arg {
    Consts(u32, Vec<u32>),
    /// Parameter, view kind (0 constant buffer, 1 shader resource,
    /// 2 unordered access), GPU address.
    View(u32, u8, u64),
    Table(u32, u64),
}

impl Arg {
    fn shape(&self) -> (u32, u8, u32) {
        match self {
            Arg::Consts(p, v) => (*p, 3, v.len() as u32),
            Arg::View(p, k, _) => (*p, *k, 2),
            Arg::Table(p, _) => (*p, 4, 0),
        }
    }

    /// # Safety
    /// An open list whose root signature has this parameter.
    unsafe fn record(&self, l: &ID3D12GraphicsCommandList, graphics: bool) {
        // SAFETY: the caller's.
        unsafe {
            if graphics {
                match self {
                    Arg::Consts(p, v) => l.SetGraphicsRoot32BitConstants(*p, v.len() as u32, v.as_ptr().cast(), 0),
                    Arg::View(p, 0, a) => l.SetGraphicsRootConstantBufferView(*p, *a),
                    Arg::View(p, 1, a) => l.SetGraphicsRootShaderResourceView(*p, *a),
                    Arg::View(p, _, a) => l.SetGraphicsRootUnorderedAccessView(*p, *a),
                    Arg::Table(p, h) => l.SetGraphicsRootDescriptorTable(*p, D3D12_GPU_DESCRIPTOR_HANDLE { ptr: *h }),
                }
                return;
            }
            match self {
                Arg::Consts(p, v) => l.SetComputeRoot32BitConstants(*p, v.len() as u32, v.as_ptr().cast(), 0),
                Arg::View(p, 0, a) => l.SetComputeRootConstantBufferView(*p, *a),
                Arg::View(p, 1, a) => l.SetComputeRootShaderResourceView(*p, *a),
                Arg::View(p, _, a) => l.SetComputeRootUnorderedAccessView(*p, *a),
                Arg::Table(p, h) => l.SetComputeRootDescriptorTable(*p, D3D12_GPU_DESCRIPTOR_HANDLE { ptr: *h }),
            }
        }
    }
}

struct Run {
    list: ID3D12GraphicsCommandList,
    /// The host list the run is recorded in: alive while it records, and
    /// every run is recorded before the list is closed.
    holder: *const crate::host::HostList,
    root: ID3D12RootSignature,
    pso: usize,
    /// Mesh draws (graphics root arguments, `DISPATCH_MESH`), else title
    /// dispatches.
    mesh: bool,
    shape: Vec<(u32, u8, u32)>,
    /// Every record's root arguments and group counts, the first record's
    /// arguments kept whole for a run of one.
    words: Vec<u32>,
    first: Vec<Arg>,
    first_groups: [u32; 3],
    count: u32,
}

thread_local! {
    /// The root arguments the bind under way sets, while it is a title
    /// dispatch's that may join a run.
    static CAPTURE: RefCell<Option<Vec<Arg>>> = const { RefCell::new(None) };
    /// Whether the kept bind is a graphics one (a mesh draw's).
    static CAPTURE_GRAPHICS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static RUN: RefCell<Option<Run>> = const { RefCell::new(None) };
}

/// Whether title dispatches are batched now.
pub(crate) fn on() -> bool {
    static AT_START: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    match live("batch_dispatch") {
        Some(v) => v != 0,
        None => *AT_START.get_or_init(|| std::env::var_os("VICTORY_BATCH_DISPATCH").is_some_and(|v| v == "1")),
    }
}

/// Whether mesh draws through our amplification stage are batched now.
pub(crate) fn mesh_on() -> bool {
    static AT_START: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    match live("batch_mesh") {
        Some(v) => v != 0,
        None => *AT_START.get_or_init(|| std::env::var_os("VICTORY_BATCH_MESH").is_some_and(|v| v == "1")),
    }
}

/// Starts keeping the root arguments the next bind sets.
pub(crate) fn capture_begin() {
    CAPTURE_GRAPHICS.with(|g| g.set(false));
    CAPTURE.with(|c| *c.borrow_mut() = Some(Vec::new()));
}

/// Starts keeping the graphics root arguments the next bind sets.
pub(crate) fn capture_begin_graphics() {
    CAPTURE_GRAPHICS.with(|g| g.set(true));
    CAPTURE.with(|c| *c.borrow_mut() = Some(Vec::new()));
}

/// Whether a root argument of this kind is kept now.
fn keeps(compute: bool) -> bool {
    CAPTURE.with(|c| c.borrow().is_some()) && CAPTURE_GRAPHICS.with(|g| g.get()) != compute
}

/// Stops keeping them: what was kept.
pub(crate) fn capture_end() -> Option<Vec<Arg>> {
    CAPTURE.with(|c| c.borrow_mut().take())
}

fn kept(arg: Arg) -> Option<Arg> {
    CAPTURE.with(|c| match c.borrow_mut().as_mut() {
        Some(v) => {
            v.push(arg);
            None
        }
        None => Some(arg),
    })
}

/// `SetCompute/GraphicsRoot32BitConstants`, or kept for a run.
///
/// # Safety
/// As the call it stands for.
pub(crate) unsafe fn consts(l: &ID3D12GraphicsCommandList, compute: bool, param: u32, values: &[u32]) {
    if keeps(compute) {
        kept(Arg::Consts(param, values.to_vec()));
        return;
    }
    // SAFETY: the caller's.
    unsafe {
        if compute {
            l.SetComputeRoot32BitConstants(param, values.len() as u32, values.as_ptr().cast(), 0);
        } else {
            l.SetGraphicsRoot32BitConstants(param, values.len() as u32, values.as_ptr().cast(), 0);
        }
    }
}

/// A root view set (kind 0 constant buffer, 1 shader resource, 2 unordered
/// access), or kept for a run.
///
/// # Safety
/// As the call it stands for.
pub(crate) unsafe fn view(l: &ID3D12GraphicsCommandList, compute: bool, kind: u8, param: u32, address: u64) {
    if keeps(compute) && kept(Arg::View(param, kind, address)).is_none() {
        return;
    }
    // SAFETY: the caller's.
    unsafe {
        match (compute, kind) {
            (true, 0) => l.SetComputeRootConstantBufferView(param, address),
            (true, 1) => l.SetComputeRootShaderResourceView(param, address),
            (true, _) => l.SetComputeRootUnorderedAccessView(param, address),
            (false, 0) => l.SetGraphicsRootConstantBufferView(param, address),
            (false, 1) => l.SetGraphicsRootShaderResourceView(param, address),
            (false, _) => l.SetGraphicsRootUnorderedAccessView(param, address),
        }
    }
}

/// A root descriptor table set, or kept (it then ends the run).
///
/// # Safety
/// As the call it stands for.
pub(crate) unsafe fn table(l: &ID3D12GraphicsCommandList, compute: bool, param: u32, handle: D3D12_GPU_DESCRIPTOR_HANDLE) {
    if keeps(compute) && kept(Arg::Table(param, handle.ptr)).is_none() {
        return;
    }
    // SAFETY: the caller's.
    unsafe {
        if compute {
            l.SetComputeRootDescriptorTable(param, handle);
        } else {
            l.SetGraphicsRootDescriptorTable(param, handle);
        }
    }
}

/// Records the run kept for `l`, if any, before anything else goes in.
pub(crate) fn flush(l: &ID3D12GraphicsCommandList) {
    let run = RUN.with(|r| {
        let mut r = r.borrow_mut();
        if r.as_ref().is_some_and(|run| run.list.as_raw() == l.as_raw()) { r.take() } else { None }
    });
    if let Some(run) = run {
        record_run(run);
    }
}

/// Records any run kept on this thread (a list ends, another begins).
pub(crate) fn flush_any() {
    if let Some(run) = RUN.with(|r| r.borrow_mut().take()) {
        record_run(run);
    }
}

/// Whether a run is kept for `l` with another pipeline than `pso`.
pub(crate) fn flush_unless(l: &ID3D12GraphicsCommandList, pso: usize) {
    let other = RUN.with(|r| r.borrow().as_ref().is_some_and(|run| run.list.as_raw() != l.as_raw() || run.pso != pso));
    if other {
        flush_any();
    }
}

/// A title dispatch whose bind kept `args`: joins the run, or starts one.
pub(crate) fn dispatch(host_list: &crate::host::HostList, root: &ID3D12RootSignature, pso: usize, args: Vec<Arg>, groups: [u32; 3]) {
    push(host_list, root, pso, args, groups, false);
}

/// A mesh draw through our amplification stage whose bind kept `args`
/// (with the stage's record buffer and offset last): joins the run, or
/// starts one. The amplified pipeline is bound.
pub(crate) fn mesh(host_list: &crate::host::HostList, root: &ID3D12RootSignature, pso: usize, args: Vec<Arg>) {
    push(host_list, root, pso, args, [1, 1, 1], true);
}

fn push(host_list: &crate::host::HostList, root: &ID3D12RootSignature, pso: usize, args: Vec<Arg>, groups: [u32; 3], mesh: bool) {
    let l = &host_list.list;
    let shape: Vec<_> = args.iter().map(Arg::shape).collect();
    if shape.iter().any(|s| s.1 == 4) {
        flush(l);
        // SAFETY: an open list with the command's pipeline bound.
        unsafe {
            args.iter().for_each(|a| a.record(l, mesh));
            launch(l, mesh, groups);
        }
        return;
    }
    let joins = RUN.with(|r| {
        r.borrow().as_ref().is_some_and(|run| {
            run.list.as_raw() == l.as_raw() && run.pso == pso && run.mesh == mesh && run.shape == shape && run.count < 4096
        })
    });
    if !joins {
        flush_any();
    }
    RUN.with(|r| {
        let mut r = r.borrow_mut();
        let run = r.get_or_insert_with(|| Run {
            list: l.clone(),
            holder: std::ptr::from_ref(host_list),
            root: root.clone(),
            pso,
            mesh,
            shape: shape.clone(),
            words: Vec::new(),
            first: args.clone(),
            first_groups: groups,
            count: 0,
        });
        for a in &args {
            match a {
                Arg::Consts(_, v) => run.words.extend_from_slice(v),
                Arg::View(_, _, at) => run.words.extend_from_slice(&[*at as u32, (*at >> 32) as u32]),
                Arg::Table(..) => {}
            }
        }
        run.words.extend_from_slice(&groups);
        run.count += 1;
    });
    // The list holds the root signature while the run waits.
    host_list.keep(root);
}

/// # Safety
/// An open list with the command's pipeline and root arguments set.
unsafe fn launch(l: &ID3D12GraphicsCommandList, mesh: bool, g: [u32; 3]) {
    // SAFETY: the caller's.
    unsafe {
        if mesh {
            if let Ok(l6) = l.cast::<ID3D12GraphicsCommandList6>() {
                l6.DispatchMesh(g[0], g[1], g[2]);
            }
        } else {
            l.Dispatch(g[0], g[1], g[2]);
        }
    }
}

static SIGNATURES: Mutex<Vec<((usize, bool, Vec<(u32, u8, u32)>), ID3D12CommandSignature)>> = Mutex::new(Vec::new());

/// Runs recorded as one indirect execution, the dispatches in them, and
/// runs of one.
static STATS: [std::sync::atomic::AtomicU64; 6] = [const { std::sync::atomic::AtomicU64::new(0) }; 6];

fn signature(root: &ID3D12RootSignature, shape: &[(u32, u8, u32)], mesh: bool) -> Option<ID3D12CommandSignature> {
    let key = (root.as_raw() as usize, mesh, shape.to_vec());
    let mut made = SIGNATURES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, s)) = made.iter().find(|(k, _)| *k == key) {
        return Some(s.clone());
    }
    let mut descs = Vec::new();
    let mut stride = 0u32;
    for &(param, kind, dwords) in shape {
        let mut d = D3D12_INDIRECT_ARGUMENT_DESC::default();
        match kind {
            3 => {
                d.Type = D3D12_INDIRECT_ARGUMENT_TYPE_CONSTANT;
                d.Anonymous.Constant = D3D12_INDIRECT_ARGUMENT_DESC_0_1 {
                    RootParameterIndex: param,
                    DestOffsetIn32BitValues: 0,
                    Num32BitValuesToSet: dwords,
                };
            }
            k => {
                d.Type = match k {
                    0 => D3D12_INDIRECT_ARGUMENT_TYPE_CONSTANT_BUFFER_VIEW,
                    1 => D3D12_INDIRECT_ARGUMENT_TYPE_SHADER_RESOURCE_VIEW,
                    _ => D3D12_INDIRECT_ARGUMENT_TYPE_UNORDERED_ACCESS_VIEW,
                };
                d.Anonymous.ConstantBufferView = D3D12_INDIRECT_ARGUMENT_DESC_0_2 { RootParameterIndex: param };
            }
        }
        stride += dwords * 4;
        descs.push(d);
    }
    descs.push(D3D12_INDIRECT_ARGUMENT_DESC {
        Type: if mesh { D3D12_INDIRECT_ARGUMENT_TYPE_DISPATCH_MESH } else { D3D12_INDIRECT_ARGUMENT_TYPE_DISPATCH },
        ..Default::default()
    });
    stride += 12;
    let desc = D3D12_COMMAND_SIGNATURE_DESC {
        ByteStride: stride,
        NumArgumentDescs: descs.len() as u32,
        pArgumentDescs: descs.as_ptr(),
        NodeMask: 0,
    };
    let mut sig: Option<ID3D12CommandSignature> = None;
    // SAFETY: a description of live arguments, for this root signature.
    let made_now = unsafe { crate::host::host()?.device.CreateCommandSignature(&desc, root, &mut sig) };
    if let Err(e) = made_now {
        tracing::warn!(error = %e, "a command signature for batched dispatches refused: those dispatches are recorded one by one");
        return None;
    }
    let sig = sig?;
    made.push((key, sig.clone()));
    Some(sig)
}

fn record_run(run: Run) {
    let l = &run.list;
    let one = |run: &Run| {
        // SAFETY: an open list with the run's pipeline bound.
        unsafe {
            run.first.iter().for_each(|a| a.record(l, run.mesh));
            launch(l, run.mesh, run.first_groups);
        }
    };
    let st = if run.mesh { &STATS[3..] } else { &STATS[..3] };
    if run.count == 1 {
        st[2].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        one(&run);
        return;
    }
    let Some(sig) = signature(&run.root, &run.shape, run.mesh) else {
        // Recorded one by one from the words.
        replay_words(&run);
        return;
    };
    let bytes = (run.words.len() * 4) as u64;
    // SAFETY: see `Run::holder`.
    let holder = unsafe { &*run.holder };
    let Some((buffer, offset, data)) = holder.scratch_at(bytes) else {
        replay_words(&run);
        return;
    };
    // SAFETY: `bytes` of mapped upload memory of the list's own; an open
    // list with the run's pipeline and root signature bound.
    unsafe {
        std::ptr::copy_nonoverlapping(run.words.as_ptr().cast::<u8>(), data, bytes as usize);
        l.ExecuteIndirect(&sig, run.count, &buffer, offset, None, 0);
    }
    holder.keep(&sig);
    st[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    st[1].fetch_add(u64::from(run.count), std::sync::atomic::Ordering::Relaxed);
}

/// A run recorded one by one, as without the switch.
fn replay_words(run: &Run) {
    let l = &run.list;
    let mut at = 0usize;
    for _ in 0..run.count {
        for &(param, kind, dwords) in &run.shape {
            let w = &run.words[at..at + dwords as usize];
            let arg = match kind {
                3 => Arg::Consts(param, w.to_vec()),
                k => Arg::View(param, k, u64::from(w[0]) | u64::from(w[1]) << 32),
            };
            // SAFETY: an open list with the run's pipeline bound.
            unsafe { arg.record(l, run.mesh) };
            at += dwords as usize;
        }
        // SAFETY: as above.
        unsafe { launch(l, run.mesh, [run.words[at], run.words[at + 1], run.words[at + 2]]) };
        at += 3;
    }
}

/// The batches since the last line, per frame.
pub(crate) fn say(frames: f64) {
    use std::sync::atomic::Ordering::Relaxed;
    let (runs, dispatches, ones) = (STATS[3].swap(0, Relaxed), STATS[4].swap(0, Relaxed), STATS[5].swap(0, Relaxed));
    if runs + ones > 0 {
        tracing::info!(
            "  batched amplified mesh draws per frame (VICTORY_BATCH_MESH / live batch_mesh): {:.0} in {:.0} indirect executions, {:.0} alone",
            dispatches as f64 / frames,
            runs as f64 / frames,
            ones as f64 / frames
        );
    }
    let (runs, dispatches, ones) = (STATS[0].swap(0, Relaxed), STATS[1].swap(0, Relaxed), STATS[2].swap(0, Relaxed));
    if runs + ones > 0 {
        tracing::info!(
            "  batched title dispatches per frame (VICTORY_BATCH_DISPATCH / live batch_dispatch): {:.0} in {:.0} indirect executions, {:.0} alone",
            dispatches as f64 / frames,
            runs as f64 / frames,
            ones as f64 / frames
        );
    }
}

/// Whether the bind under way keeps root arguments for a run.
pub(crate) fn capturing() -> bool {
    CAPTURE.with(|c| c.borrow().is_some())
}

thread_local! {
    /// The pipeline the run of the bind under way is keyed by, when it is
    /// not the bind's own (a mesh draw's amplified pipeline).
    static RUN_PSO: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Keys the bind under way's run by `pso` (0: the bind's own pipeline).
pub(crate) fn set_run_pso(pso: usize) {
    RUN_PSO.with(|p| p.set(pso));
}

/// The pipeline the bind under way's run is keyed by, if not its own.
pub(crate) fn run_pso() -> usize {
    RUN_PSO.with(|p| p.get())
}

/// A number from `VICTORY_LIVE_FILE` (`key=value` lines), read at most
/// every 250 ms.
fn live(key: &str) -> Option<usize> {
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};
    static PATH: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
    static READ: Mutex<Option<(Instant, Vec<(String, usize)>)>> = Mutex::new(None);
    let path = PATH.get_or_init(|| std::env::var_os("VICTORY_LIVE_FILE").map(Into::into)).as_ref()?;
    let mut read = READ.lock().unwrap_or_else(|e| e.into_inner());
    if read.as_ref().is_none_or(|(at, _)| at.elapsed() > Duration::from_millis(250)) {
        let pairs = std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let (k, v) = l.split_once('=')?;
                Some((k.trim().to_owned(), v.trim().parse().ok()?))
            })
            .collect();
        *read = Some((Instant::now(), pairs));
    }
    read.as_ref()?.1.iter().find(|(k, _)| k == key).map(|(_, v)| *v)
}
