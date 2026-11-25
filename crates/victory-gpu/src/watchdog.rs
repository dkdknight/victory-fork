//! The card's watchdog: what a submission held when it has not finished
//! after some seconds, said in the log while the card is still ours.
//!
//! The owner's gameplay session of 2026-11-25 ended in
//! `DXGI_ERROR_DEVICE_HUNG` after ~6 minutes, with no device-removed data
//! (a release build) and nothing in the log that said which of the four
//! submissions in the air was the one the card stood in. Windows resets a
//! card that has not answered for about two seconds (`TdrDelay`), so a
//! submission that is still running after that long names the culprit of
//! the next hang before the device is gone; one that finishes late but
//! finishes shows a near miss.
//!
//! - `VICTORY_GPU_WATCHDOG=<ms>` (live key `gpu_watchdog`): how long a
//!   submission may run on the card before it is named; 2000 when unset, 0
//!   off. Each submission sent to the direct queue (and each job of the
//!   copy queues, `crate::queue2`) is noted with its fence value, its lists
//!   (our serial numbers, the names device-removed data shows, and the
//!   pipelines each binds) and the waits it carries (the direct queue's
//!   waits for the copy queue). A thread looks every 100 ms. The time "on
//!   the card" of a submission starts when the one before it on the same
//!   fence is seen finished (or when it is sent, if none was in the air).
//!   Logging only: nothing waits or changes for it.
//! - `VICTORY_GPU_PROGRESS=1` (off by default; read once): around every
//!   list of a submission a list of ours of two `WriteBufferImmediate`
//!   marks (`MARKER_IN` when the card reaches it, `MARKER_OUT` when
//!   everything before it is done) into a buffer the CPU reads. The
//!   watchdog then names the list the card stands in, not only the
//!   submission. The marks are 1,024 lists recorded once and given out in
//!   turn; a submission that would reuse one still in the air goes without
//!   (counted, warned once).
//!
//! Per-frame line: "GPU watchdog per frame (...)"; at device removal the
//! submissions still unfinished go into the "when the host device went"
//! report ([`removal_note`]).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use windows::Win32::Graphics::Direct3D12::*;
use windows::core::Interface;

/// The limit when the switch is unset.
const DEFAULT_MS: u64 = 2000;
/// Pipelines kept per list for the report.
const PIPELINES_KEPT: usize = 24;
/// Full reports in a run; later ones are one line each.
const REPORTS_MOST: u64 = 16;

/// `VICTORY_GPU_WATCHDOG`'s value (or the live `gpu_watchdog`): the
/// milliseconds after which a submission still running is named; 0 off.
pub(crate) fn limit_ms() -> u64 {
    if let Some(v) = crate::host::live::value("gpu_watchdog") {
        return parse_ms(Some(&v));
    }
    static ENV: OnceLock<u64> = OnceLock::new();
    *ENV.get_or_init(|| {
        let ms = parse_ms(std::env::var("VICTORY_GPU_WATCHDOG").ok().as_deref());
        if ms == 0 {
            tracing::info!("the GPU watchdog is off (VICTORY_GPU_WATCHDOG=0)");
        } else {
            tracing::info!(
                ms,
                "the GPU watchdog names a submission still on the card after {ms} ms (VICTORY_GPU_WATCHDOG; 0 turns it off)"
            );
        }
        ms
    })
}

/// A switch's value in milliseconds; [`DEFAULT_MS`] when unset or
/// unreadable. Pure, for the tests.
pub(crate) fn parse_ms(value: Option<&str>) -> u64 {
    match value.map(str::trim) {
        None | Some("") => DEFAULT_MS,
        Some(v) => v.parse().unwrap_or(DEFAULT_MS),
    }
}

/// Whether submissions are noted (not without the watchdog's thread: no
/// one would take them out again).
pub(crate) fn on() -> bool {
    !NO_THREAD.load(Ordering::Relaxed) && limit_ms() > 0
}

/// The watchdog's thread could not be made.
static NO_THREAD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether progress marks go around lists (`VICTORY_GPU_PROGRESS=1`).
pub(crate) fn progress_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var_os("VICTORY_GPU_PROGRESS").is_some_and(|v| v == "1");
        if on {
            tracing::info!(
                "VICTORY_GPU_PROGRESS=1: a progress mark goes before every list and after the last one; the GPU watchdog names the list the card stands in"
            );
        }
        on
    })
}

/// A list of a submission, as the report names it.
pub(crate) struct ListNote {
    /// `HostList::serial` (the device-removed data's "victory list #N").
    pub(crate) serial: u32,
    /// Pipelines it binds (the first [`PIPELINES_KEPT`]) and how many.
    pub(crate) pipelines: Vec<String>,
    pub(crate) pipeline_count: usize,
}

impl ListNote {
    pub(crate) fn new(serial: u32, pipelines: &std::collections::BTreeSet<String>) -> ListNote {
        ListNote {
            serial,
            pipelines: pipelines.iter().take(PIPELINES_KEPT).cloned().collect(),
            pipeline_count: pipelines.len(),
        }
    }

    fn text(&self, whole: bool) -> String {
        if self.pipeline_count == 0 {
            return format!("#{} (no pipeline)", self.serial);
        }
        if !whole {
            return format!("#{} ({} pipelines)", self.serial, self.pipeline_count);
        }
        let more = self.pipeline_count.saturating_sub(self.pipelines.len());
        let more = if more > 0 { format!(" and {more} more") } else { String::new() };
        format!("#{} (pipelines {:?}{more})", self.serial, self.pipelines)
    }
}

/// A submission (or a copy queue's job) in the air.
struct Entry {
    queue: &'static str,
    fence: ID3D12Fence,
    value: u64,
    /// The submission's number (`host::STARTED`) or the job's.
    number: u64,
    sent: Instant,
    /// Since when it is the oldest unfinished one on its fence.
    front: Option<Instant>,
    lists: Vec<ListNote>,
    waits: Vec<String>,
    /// With progress marks: the first mark of the submission and how many
    /// (one per list, one after the last).
    marks: Option<(u32, u32)>,
    reported: bool,
}

// SAFETY: a fence is free-threaded; the rest is plain data.
unsafe impl Send for Entry {}

static WATCHED: Mutex<VecDeque<Entry>> = Mutex::new(VecDeque::new());
/// Waits for the next submission noted on the direct queue to carry.
static CARRIED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Since the last frame line: submissions watched, the longest time one
/// was on the card (µs), those past the limit; since the start: reports,
/// submissions sent without marks (the ring was in the air).
pub(crate) static STATS: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];

/// A wait the next submission sent to the direct queue carries (the
/// direct queue told to wait for a copy queue's job, `Second::hold`).
pub(crate) fn carry(wait: impl FnOnce() -> String) {
    if !on() {
        return;
    }
    let mut carried = CARRIED.lock().unwrap_or_else(|e| e.into_inner());
    if carried.len() < 64 {
        carried.push(wait());
    }
}

/// Notes a submission just sent to `queue` and followed by `fence`'s
/// `value`: `lists` names its lists, built only when the watchdog is on.
/// The direct queue's submissions take the waits carried so far.
pub(crate) fn sent(
    queue: &'static str,
    fence: &ID3D12Fence,
    value: u64,
    number: u64,
    marks: Option<(u32, u32)>,
    lists: impl FnOnce() -> Vec<ListNote>,
    waits: Vec<String>,
) {
    if !on() {
        return;
    }
    start();
    if NO_THREAD.load(Ordering::Relaxed) {
        return;
    }
    let mut waits = waits;
    if queue == DIRECT {
        waits.append(&mut CARRIED.lock().unwrap_or_else(|e| e.into_inner()));
    }
    let lists = lists();
    let now = Instant::now();
    let mut watched = WATCHED.lock().unwrap_or_else(|e| e.into_inner());
    let raw = fence.as_raw();
    let alone = !watched.iter().any(|e| e.fence.as_raw() == raw);
    STATS[0].fetch_add(1, Ordering::Relaxed);
    watched.push_back(Entry {
        queue,
        fence: fence.clone(),
        value,
        number,
        sent: now,
        front: alone.then_some(now),
        lists,
        waits,
        marks,
        reported: false,
    });
}

/// The name of the direct queue in reports.
pub(crate) const DIRECT: &str = "the direct queue";

fn start() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        if let Err(e) = std::thread::Builder::new().name("victory-gpu-watchdog".into()).spawn(watch) {
            NO_THREAD.store(true, Ordering::Relaxed);
            tracing::warn!(error = %e, "no thread for the GPU watchdog: submissions are not watched");
        }
    });
}

/// The watchdog's thread.
fn watch() {
    loop {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let limit = limit_ms();
        let reports = look(&mut WATCHED.lock().unwrap_or_else(|e| e.into_inner()), Instant::now(), limit);
        for report in reports {
            let n = STATS[3].fetch_add(1, Ordering::Relaxed);
            if n < REPORTS_MOST {
                tracing::warn!("GPU watchdog: {report}");
            } else if n == REPORTS_MOST {
                tracing::warn!("GPU watchdog: more submissions past the limit; from now on only counted (the frame line)");
            }
        }
    }
}

/// One look: entries their fence has passed go (a reported one says how
/// long it took in the end); the oldest unfinished one of each fence
/// starts its time on the card; those past `limit` ms are reported, once.
fn look(watched: &mut VecDeque<Entry>, now: Instant, limit: u64) -> Vec<String> {
    let mut reports = Vec::new();
    let mut completed: Vec<(usize, u64)> = Vec::new();
    let mut finished_at: Vec<usize> = Vec::new();
    watched.retain(|e| {
        let raw = e.fence.as_raw() as usize;
        let done = match completed.iter().find(|(f, _)| *f == raw) {
            Some(&(_, v)) => v,
            None => {
                // SAFETY: a plain query on a live fence.
                let v = unsafe { e.fence.GetCompletedValue() };
                completed.push((raw, v));
                v
            }
        };
        // A removed device reads all ones: nothing is finished then.
        if done == u64::MAX || done < e.value {
            return true;
        }
        let on_card = now.saturating_duration_since(e.front.unwrap_or(e.sent));
        STATS[1].fetch_max(on_card.as_micros() as u64, Ordering::Relaxed);
        if e.reported {
            tracing::warn!(
                "GPU watchdog: submission {} on {} finished after all, {:.0} ms on the card ({:.0} ms after it was sent)",
                e.number,
                e.queue,
                on_card.as_secs_f64() * 1e3,
                now.saturating_duration_since(e.sent).as_secs_f64() * 1e3
            );
        }
        finished_at.push(raw);
        false
    });
    // The oldest left of each fence is on the card from now (or from its
    // send, if that was later).
    let mut seen: Vec<usize> = Vec::new();
    for e in watched.iter_mut() {
        let raw = e.fence.as_raw() as usize;
        if seen.contains(&raw) {
            continue;
        }
        seen.push(raw);
        if e.front.is_none() {
            e.front = Some(if finished_at.contains(&raw) { now } else { e.sent.max(now.checked_sub(std::time::Duration::from_millis(100)).unwrap_or(now)) });
        }
    }
    if limit == 0 {
        return reports;
    }
    let marks = marks_now();
    let others = fence_states(watched, &completed);
    let behind_of = |queue: &str, number: u64, watched: &VecDeque<Entry>| {
        watched.iter().filter(|o| o.queue == queue && o.number > number).count()
    };
    let mut over: Vec<usize> = Vec::new();
    for (i, e) in watched.iter().enumerate() {
        let Some(front) = e.front else { continue };
        if e.reported || now.saturating_duration_since(front).as_millis() < u128::from(limit) {
            continue;
        }
        over.push(i);
    }
    for i in over {
        let behind = behind_of(watched[i].queue, watched[i].number, watched);
        let e = &mut watched[i];
        e.reported = true;
        STATS[2].fetch_add(1, Ordering::Relaxed);
        let done = completed.iter().find(|(f, _)| *f == e.fence.as_raw() as usize).map_or(0, |&(_, v)| v);
        reports.push(report(e, now, limit, done, behind, marks, &others));
    }
    reports
}

/// What each fence watched has reached and what it waits for.
fn fence_states(watched: &VecDeque<Entry>, completed: &[(usize, u64)]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut seen: Vec<usize> = Vec::new();
    for e in watched {
        let raw = e.fence.as_raw() as usize;
        if seen.contains(&raw) {
            continue;
        }
        seen.push(raw);
        let left: Vec<&Entry> = watched.iter().filter(|o| o.fence.as_raw() as usize == raw).collect();
        let done = completed.iter().find(|(f, _)| *f == raw).map_or(0, |&(_, v)| v);
        parts.push(format!(
            "{}: fence at {done}, {} unfinished (values {}..={})",
            e.queue,
            left.len(),
            left.first().map_or(0, |l| l.value),
            left.last().map_or(0, |l| l.value)
        ));
    }
    parts.join("; ")
}

fn report(e: &Entry, now: Instant, limit: u64, done: u64, behind: usize, marks: Option<[u32; 2]>, others: &str) -> String {
    let on_card = now.saturating_duration_since(e.front.unwrap_or(e.sent)).as_secs_f64() * 1e3;
    let sent = now.saturating_duration_since(e.sent).as_secs_f64() * 1e3;
    let mut text = format!(
        "submission {} on {} has been on the card {on_card:.0} ms (limit {limit} ms; sent {sent:.0} ms ago), its fence at {done} of {}; {behind} sent after it wait behind it; {} lists",
        e.number,
        e.queue,
        e.value,
        e.lists.len()
    );
    // Where the marks say the card is in it.
    let mut standing: Option<(usize, usize)> = None;
    match (e.marks, marks) {
        (Some((first, count)), Some([began, ended])) => {
            let lists = count as usize - 1;
            match (mark_in(began, first, count), mark_in(ended, first, count)) {
                (Some(b), ended_at) => {
                    let all_before = ended_at.unwrap_or(0);
                    if b >= lists {
                        text.push_str(&format!(
                            "; progress marks: the card has begun all {lists} of its lists and finished everything before list {all_before}"
                        ));
                    } else {
                        text.push_str(&format!(
                            "; progress marks: the card has reached list {b} of {lists} and finished everything before list {all_before}"
                        ));
                    }
                    standing = Some((all_before, b.min(lists.saturating_sub(1))));
                }
                (None, Some(e2)) => {
                    text.push_str(&format!(
                        "; progress marks: the card has begun what was sent after it and finished everything before its list {e2}"
                    ));
                    standing = Some((e2, lists.saturating_sub(1)));
                }
                (None, None) => text.push_str(&format!(
                    "; progress marks: none of this submission's (marks {}..={}) reached, the last reached {began}, everything done before {ended}",
                    first + 1,
                    first + count
                )),
            }
        }
        (None, Some(_)) => text.push_str("; no progress marks (the ring of marks was in the air)"),
        _ => {}
    }
    // The lists: in full where the card stands (all if the marks do not
    // say), by name and pipeline count elsewhere.
    let named: Vec<String> = e
        .lists
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let whole = standing.is_none_or(|(from, to)| i >= from && i <= to);
            format!("{i}: {}", l.text(whole))
        })
        .collect();
    text.push_str(&format!("; its lists: [{}]", named.join(", ")));
    if !e.waits.is_empty() {
        text.push_str(&format!("; the waits it carries: [{}]", e.waits.join("; ")));
    }
    text.push_str(&format!("; queues: {others}"));
    text
}

/// Which of a submission's marks (`first`, `count` of them, ring numbers)
/// the card wrote as `mark` (a ring number plus one; 0: none yet).
fn mark_in(mark: u32, first: u32, count: u32) -> Option<usize> {
    let k = mark.checked_sub(1)?;
    let i = (k + RING - first % RING) % RING;
    (i < count).then_some(i as usize)
}

/// For the "when the host device went" report: what was still on the
/// card, oldest first, by fence.
pub(crate) fn removal_note() -> String {
    if !on() {
        return String::new();
    }
    let now = Instant::now();
    let watched = WATCHED.lock().unwrap_or_else(|e| e.into_inner());
    if watched.is_empty() {
        return "; GPU watchdog: nothing of ours unfinished".to_owned();
    }
    let marks = marks_now();
    let mut text = format!("; GPU watchdog: {} unfinished", watched.len());
    if let Some([began, ended]) = marks {
        text.push_str(&format!(" (progress marks: last reached {began}, all done before {ended})"));
    }
    for e in watched.iter().take(8) {
        let on_card = e.front.map_or(0.0, |f| now.saturating_duration_since(f).as_secs_f64() * 1e3);
        let lists: Vec<String> = e.lists.iter().map(|l| l.text(false)).collect();
        text.push_str(&format!(
            "; submission {} on {} (fence value {}, sent {:.0} ms before, {on_card:.0} ms on the card, marks {:?}): lists [{}]",
            e.number,
            e.queue,
            e.value,
            now.saturating_duration_since(e.sent).as_secs_f64() * 1e3,
            e.marks,
            lists.join(", ")
        ));
        if !e.waits.is_empty() {
            text.push_str(&format!(", waits [{}]", e.waits.join("; ")));
        }
    }
    text
}

/// The per-frame line.
pub(crate) fn line(frames: f64) {
    let limit = limit_ms();
    if limit == 0 {
        return;
    }
    let watched = STATS[0].swap(0, Ordering::Relaxed) as f64 / frames;
    let longest = STATS[1].swap(0, Ordering::Relaxed) as f64 / 1e3;
    let over = STATS[2].swap(0, Ordering::Relaxed);
    let unmarked = STATS[4].load(Ordering::Relaxed);
    let in_air = WATCHED.lock().unwrap_or_else(|e| e.into_inner()).len();
    tracing::info!(
        "GPU watchdog per frame (VICTORY_GPU_WATCHDOG={limit} ms): {watched:.1} submissions watched, the longest {longest:.1} ms on the card, \
         {over} past the limit since the last line ({} reported in all), {in_air} in the air now; progress marks {} ({unmarked} submissions without, so far)",
        STATS[3].load(Ordering::Relaxed),
        if progress_on() { "on" } else { "off (VICTORY_GPU_PROGRESS=1)" },
    );
}

/// Marks in the ring ([`Marks`]).
const RING: u32 = 1024;

/// The progress marks: [`RING`] lists of ours, each two writes into
/// `buffer` (dword 0: the mark the card has reached, `MARKER_IN`; dword 1:
/// the mark before which everything is done, `MARKER_OUT`), the mark being
/// the list's ring number plus one (0: none written yet).
struct Marks {
    lists: Vec<ID3D12CommandList>,
    _allocator: ID3D12CommandAllocator,
    _buffer: ID3D12Resource,
    data: *const u32,
    /// Marks given out so far, and per ring place the fence value the
    /// last submission that used it signals.
    state: Mutex<(u32, Vec<u64>)>,
}

// SAFETY: D3D12 objects are free-threaded; the mapping is read only.
unsafe impl Send for Marks {}
// SAFETY: as above.
unsafe impl Sync for Marks {}

static MARKS: OnceLock<Option<Marks>> = OnceLock::new();

fn marks(device: &ID3D12Device) -> Option<&'static Marks> {
    MARKS
        .get_or_init(|| match make_marks(device) {
            Ok(m) => Some(m),
            Err(e) => {
                tracing::warn!(error = %e, "no progress marks (VICTORY_GPU_PROGRESS): the GPU watchdog names submissions only");
                None
            }
        })
        .as_ref()
}

fn make_marks(device: &ID3D12Device) -> windows::core::Result<Marks> {
    // SAFETY: plain object creation; the buffer is mapped once and only
    // read; each list is recorded once and closed.
    unsafe {
        let heap = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_READBACK,
            ..Default::default()
        };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Width: 256,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            ..Default::default()
        };
        let mut buffer: Option<ID3D12Resource> = None;
        device.CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &desc, D3D12_RESOURCE_STATE_COPY_DEST, None, &mut buffer)?;
        let buffer = buffer.ok_or_else(windows::core::Error::empty)?;
        let _ = buffer.SetName(&windows::core::HSTRING::from("victory progress marks"));
        let mut data = std::ptr::null_mut();
        buffer.Map(0, None, Some(&mut data))?;
        let address = buffer.GetGPUVirtualAddress();
        let allocator: ID3D12CommandAllocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)?;
        let mut lists = Vec::with_capacity(RING as usize);
        for k in 0..RING {
            let list: ID3D12GraphicsCommandList2 = device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None)?;
            let params = [
                D3D12_WRITEBUFFERIMMEDIATE_PARAMETER { Dest: address, Value: k + 1 },
                D3D12_WRITEBUFFERIMMEDIATE_PARAMETER { Dest: address + 4, Value: k + 1 },
            ];
            let modes = [D3D12_WRITEBUFFERIMMEDIATE_MODE_MARKER_IN, D3D12_WRITEBUFFERIMMEDIATE_MODE_MARKER_OUT];
            list.WriteBufferImmediate(2, params.as_ptr(), Some(modes.as_ptr()));
            list.Close()?;
            let _ = list.SetName(&windows::core::HSTRING::from(format!("victory progress mark {}", k + 1)));
            lists.push(list.cast()?);
        }
        Ok(Marks {
            lists,
            _allocator: allocator,
            _buffer: buffer,
            data: data.cast(),
            state: Mutex::new((0, vec![0; RING as usize])),
        })
    }
}

/// For a submission of `lists` lists that `value` of the fence will
/// follow, `completed` being the value the fence has reached: the mark
/// lists to put before each list and after the last (`lists + 1`), and the
/// first mark's ring number. `None` with the switch off, or when a mark
/// needed is still in the air (counted).
pub(crate) fn take_marks(device: &ID3D12Device, lists: usize, value: u64, completed: u64) -> Option<(Vec<ID3D12CommandList>, u32)> {
    if !progress_on() || !on() {
        return None;
    }
    let marks = marks(device)?;
    let count = lists as u32 + 1;
    if count > RING / 2 {
        STATS[4].fetch_add(1, Ordering::Relaxed);
        return None;
    }
    let mut state = marks.state.lock().unwrap_or_else(|e| e.into_inner());
    let first = state.0;
    if !ring_free(&state.1, first, count, completed) {
        if STATS[4].fetch_add(1, Ordering::Relaxed) == 0 {
            tracing::warn!("progress marks: a submission went without (the ring of marks was in the air; VICTORY_GPU_PROGRESS)");
        }
        return None;
    }
    let mut taken = Vec::with_capacity(count as usize);
    for i in 0..count {
        let k = ((first + i) % RING) as usize;
        state.1[k] = value;
        taken.push(marks.lists[k].clone());
    }
    state.0 = (first + count) % RING;
    drop(state);
    Some((taken, first))
}

/// Whether the `count` mark lists from ring place `first` on (across the
/// ring's end) may be sent again: the fence (at `completed`) has passed the
/// value of the last submission that sent each (`last`, by ring place; 0:
/// never sent). A list still in the air is never sent a second time. Pure,
/// for the tests.
fn ring_free(last: &[u64], first: u32, count: u32, completed: u64) -> bool {
    (0..count).all(|i| last[((first + i) % RING) as usize] <= completed)
}

/// Puts the mark lists of [`take_marks`] around `lists` (whose first list
/// of each run starts at `firsts`): a mark before each run's first list
/// and one after the last.
pub(crate) fn interleave<T>(lists: Vec<Option<T>>, firsts: &[usize], marks: Vec<T>) -> Vec<Option<T>> {
    let mut out = Vec::with_capacity(lists.len() + marks.len());
    let mut marks = marks.into_iter();
    for (i, list) in lists.into_iter().enumerate() {
        if firsts.contains(&i) {
            out.extend(marks.next().map(Some));
        }
        out.push(list);
    }
    out.extend(marks.map(Some));
    out
}

/// The marks the card has written: [reached, everything done before];
/// ring numbers plus one, 0 none yet. `None` without marks.
fn marks_now() -> Option<[u32; 2]> {
    let marks = MARKS.get()?.as_ref()?;
    // SAFETY: a mapped readback buffer of 256 bytes, written by the card.
    unsafe {
        Some([
            std::ptr::read_volatile(marks.data),
            std::ptr::read_volatile(marks.data.add(1)),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_limit_parses_or_falls_back() {
        assert_eq!(parse_ms(None), DEFAULT_MS);
        assert_eq!(parse_ms(Some("")), DEFAULT_MS);
        assert_eq!(parse_ms(Some(" 500 ")), 500);
        assert_eq!(parse_ms(Some("0")), 0);
        assert_eq!(parse_ms(Some("two")), DEFAULT_MS);
    }

    #[test]
    fn marks_go_before_each_run_and_after_the_last() {
        // Runs start at 0 (a prelude and its list) and 2; one mark more
        // than runs, the last after everything.
        let lists = vec![Some(10), Some(11), Some(20)];
        let out = interleave(lists, &[0, 2], vec![1, 2, 3]);
        assert_eq!(out, vec![Some(1), Some(10), Some(11), Some(2), Some(20), Some(3)]);
        // No marks: the lists alone.
        assert_eq!(interleave(vec![Some(5)], &[0], Vec::new()), vec![Some(5)]);
    }

    #[test]
    fn a_mark_is_placed_in_its_submission_across_the_ring_end() {
        // Marks 1022, 1023, 0 of the ring (written as 1023, 1024, 1).
        assert_eq!(mark_in(1023, 1022, 3), Some(0));
        assert_eq!(mark_in(1024, 1022, 3), Some(1));
        assert_eq!(mark_in(1, 1022, 3), Some(2));
        assert_eq!(mark_in(2, 1022, 3), None);
        assert_eq!(mark_in(0, 0, 3), None, "nothing written yet");
        assert_eq!(mark_in(1, 0, 3), Some(0));
    }

    #[test]
    fn a_mark_list_still_in_the_air_is_not_sent_again() {
        let mut last = vec![0u64; RING as usize];
        assert!(ring_free(&last, 0, 3, 0), "never sent");
        // Places 1022, 1023 and 0 sent by the submission the fence's 7
        // follows.
        for k in [1022, 1023, 0] {
            last[k] = 7;
        }
        assert!(!ring_free(&last, 1022, 3, 6), "the card has not passed 7");
        assert!(!ring_free(&last, 1023, 1, 6));
        assert!(!ring_free(&last, 0, 2, 6), "across the ring's end");
        assert!(ring_free(&last, 1, 4, 6), "other places are free");
        assert!(ring_free(&last, 1022, 3, 7), "the fence passed it");
        assert!(ring_free(&last, 1022, 3, u64::MAX), "a removed device reads all ones");
    }

    #[test]
    fn a_list_note_keeps_the_first_pipelines_and_the_count() {
        let set: std::collections::BTreeSet<String> = (0..30).map(|i| format!("p{i:02}")).collect();
        let note = ListNote::new(7, &set);
        assert_eq!(note.pipelines.len(), PIPELINES_KEPT);
        assert_eq!(note.pipeline_count, 30);
        assert!(note.text(true).contains("and 6 more"));
        assert_eq!(note.text(false), "#7 (30 pipelines)");
        assert_eq!(ListNote::new(1, &Default::default()).text(true), "#1 (no pipeline)");
    }

    /// A fence of a device on the software adapter, signalled from the CPU
    /// in the tests; `None` without one.
    fn warp_fence() -> Option<ID3D12Fence> {
        let adapter = crate::host::warp_adapter().ok()?;
        let mut device: Option<ID3D12Device> = None;
        // SAFETY: plain device and fence creation.
        unsafe {
            windows::Win32::Graphics::Direct3D12::D3D12CreateDevice(
                &adapter,
                windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_12_0,
                &mut device,
            )
            .ok()?;
            device?.CreateFence(0, D3D12_FENCE_FLAG_NONE).ok()
        }
    }

    fn entry(fence: &ID3D12Fence, value: u64, number: u64, sent: Instant, front: Option<Instant>) -> Entry {
        let pipelines: std::collections::BTreeSet<String> = ["[compute 559d9f0184e15fe3]".to_owned()].into();
        Entry {
            queue: DIRECT,
            fence: fence.clone(),
            value,
            number,
            sent,
            front,
            lists: vec![ListNote::new(40 + number as u32, &pipelines)],
            waits: vec!["the direct queue waits for job 4 of the copy queue (up)".to_owned()],
            marks: None,
            reported: false,
        }
    }

    #[test]
    fn a_submission_past_the_limit_is_named_once_and_the_next_starts_its_time_when_it_ends() {
        let Some(fence) = warp_fence() else {
            eprintln!("no software adapter: skipped");
            return;
        };
        let ms = std::time::Duration::from_millis;
        let t0 = Instant::now();
        let mut watched = VecDeque::new();
        watched.push_back(entry(&fence, 1, 10, t0, Some(t0)));
        watched.push_back(entry(&fence, 2, 11, t0, None));
        assert!(look(&mut watched, t0 + ms(50), 2000).is_empty(), "under the limit");
        let reports = look(&mut watched, t0 + ms(2100), 2000);
        assert_eq!(reports.len(), 1, "{reports:?}");
        let r = &reports[0];
        assert!(r.contains("submission 10 on the direct queue has been on the card 2100 ms"), "{r}");
        assert!(r.contains("its fence at 0 of 1; 1 sent after it wait behind it"), "{r}");
        assert!(r.contains("#50 (pipelines [\"[compute 559d9f0184e15fe3]\"])"), "{r}");
        assert!(r.contains("the waits it carries: [the direct queue waits for job 4"), "{r}");
        assert!(look(&mut watched, t0 + ms(2200), 2000).is_empty(), "named once");
        // SAFETY: a CPU-side signal of our fence.
        unsafe { fence.Signal(1) }.expect("signal");
        assert!(look(&mut watched, t0 + ms(2300), 2000).is_empty());
        assert_eq!(watched.len(), 1, "the first is done");
        assert_eq!(watched[0].front, Some(t0 + ms(2300)), "the second is on the card from the first's end");
        assert!(look(&mut watched, t0 + ms(4200), 2000).is_empty(), "1,900 ms on the card");
        let reports = look(&mut watched, t0 + ms(4400), 2000);
        assert_eq!(reports.len(), 1);
        assert!(reports[0].contains("submission 11"), "{}", reports[0]);
        // SAFETY: as above.
        unsafe { fence.Signal(2) }.expect("signal");
        assert!(look(&mut watched, t0 + ms(4500), 2000).is_empty());
        assert!(watched.is_empty());
    }

    #[test]
    fn with_the_limit_at_zero_nothing_is_named() {
        let Some(fence) = warp_fence() else {
            return;
        };
        let t0 = Instant::now();
        let mut watched = VecDeque::new();
        watched.push_back(entry(&fence, 1, 1, t0, Some(t0)));
        assert!(look(&mut watched, t0 + std::time::Duration::from_secs(60), 0).is_empty());
        assert_eq!(watched.len(), 1);
    }
}
