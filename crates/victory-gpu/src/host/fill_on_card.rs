//! Fills of upload memory that stand behind a copy at its place
//! ([`place`]) and read its bytes, done on the card in the list's order
//! (`VICTORY_FILL_ON_CARD=1`; off by default).
//!
//! What it was: a constant buffer of a draw (a fill: `Step::Fill`, 0xf0
//! bytes) reads title memory that a copy at its place earlier in the same
//! submission writes, on the card (25 such copies a frame, all out of what
//! shaders wrote: ray activation data). With flights in the air that wrote
//! the same bytes the frame before, the fill is asked for by
//! [`card_steps`], whose list goes in front of all of the submission's
//! lists: in front of the copy too, where it would read the bytes before
//! the copy. So the plan refused ([`card_steps`]' `BEHIND_PLACED`, "a copy
//! at its place in a list writes what it reads, or reads or writes what it
//! writes") and the submitting thread waited for every flight on the CPU,
//! once a frame, for the fill's 240 bytes (run coolfox-l64: 185 ms a frame
//! in one wait, the card idle 65 ms of a 114 ms frame). On the console the
//! draw reads the constants on the card, behind the copy.
//!
//! What is done, with the switch:
//!
//! - A fill whose bytes are all written by copies at their place earlier in
//!   its own list is not left to the submission: where it is recorded (the
//!   draw that reads it, before the draw), the list copies them on the card
//!   out of the destination's host copy into a buffer of its own, and the
//!   draw's descriptor slot shows that buffer ([`HostList::fill_at_place`]).
//!   Barriers: everything recorded before (the copy at its place among it)
//!   is done and seen first; the buffer goes from a copy's destination to
//!   what shaders read; what comes after waits for the copy too (a later
//!   shader write of the same bytes must not overtake its read).
//! - A fill behind such a copy in a later list of the submission: the
//!   submission is cut behind the copy's list (`VICTORY_PLACED_ORDER`'s cut,
//!   which this switch turns on), and the fill is done on the card in front
//!   of the second part, out of the host copy the first part wrote.
//! - A fill ahead of the copy is done in front of the lists as before: the
//!   plan only refuses for what stands behind a copy (`VICTORY_PLACED_ORDER`).
//!
//! Why that is the value the CPU would have read after the wait: after the
//! wait, the steps of the submission are done in order on the CPU; the copy
//! at its place puts its source into title memory (`Placed::prepare`), and
//! the fill behind it reads that. On the card the copy wrote the same bytes
//! into its destination's host copy, at its place in the list; the fill's
//! copy is recorded after it with a barrier that waits for all of it. Only
//! where no step between them writes those bytes otherwise:
//!
//! - a copy done at submission on the CPU (its bytes would be under the
//!   copy at its place on the card, over it on the CPU), or a step of ours
//!   that may write anywhere, behind the first copy at its place that
//!   writes them: refused, the fill is left to the submission;
//! - a shader write declared through the same host copy: taken, the card
//!   reads what the shader wrote in order (on the CPU the fill read title
//!   memory as it was before the list: the console's order is this one);
//!   through another host copy: refused;
//! - bytes of the fill that no copy at its place wrote (title bytes, or a
//!   flight's): refused, so nothing is read that the plan of
//!   [`card_steps`] (`holder`) would have to vouch for.
//!
//! What is left to the settle: what is refused here, and what the plan of
//! [`card_steps`] refuses otherwise.

use super::*;

/// Whether it is on (`VICTORY_FILL_ON_CARD=1`).
pub(super) fn on() -> bool {
    #[cfg(feature = "diagnostics")]
    if let Some(on) = crate::ab::pick("VICTORY_FILL_ON_CARD") {
        return on;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| crate::switch::on("VICTORY_FILL_ON_CARD"))
}

/// Since the last frame-rate line: fills done on the card at their place in
/// their list; fills behind a copy at its place left to the submission
/// (refused here); submissions cut behind a copy at its place; fills of
/// upload memory that waited for flights on the CPU (settled).
pub(super) static STATS: [std::sync::atomic::AtomicU64; 4] = [const { std::sync::atomic::AtomicU64::new(0) }; 4];

/// Diagnostics: why the last fill was refused.
#[cfg(feature = "diagnostics")]
static REFUSED: Mutex<&'static str> = Mutex::new("");

/// A step of a list, as the choice sees it. `copy`: a host copy, by address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seen {
    /// A copy at its place, its destination in host copy `copy`.
    Placed { dst: usize, len: usize, copy: usize },
    /// A write of `[from, to)`: done on the CPU at submission (`copy`
    /// `None`), or by shaders through a host copy.
    Writes { from: usize, to: usize, copy: Option<usize> },
    /// A step of ours that may write anywhere.
    Run,
    /// Nothing the fill's bytes depend on here.
    Other,
}

fn seen(step: &Step) -> Seen {
    match step {
        Step::Placed(p) => Seen::Placed {
            dst: p.dst,
            len: p.len,
            copy: std::sync::Arc::as_ptr(&p.shadow) as usize,
        },
        &Step::Copy(dst, _, len) => Seen::Writes {
            from: dst,
            to: dst + len,
            copy: None,
        },
        Step::GpuWrite(shadow, at, len) => Seen::Writes {
            from: *at,
            to: *at + *len,
            copy: Some(std::sync::Arc::as_ptr(shadow) as usize),
        },
        Step::Run(_) => Seen::Run,
        Step::Fill(..) | Step::Refresh(..) | Step::Host(..) | Step::Texture(_) | Step::Plane(..) => Seen::Other,
    }
}

/// For a fill of `[from, to)` recorded behind `steps` (from the first copy
/// at its place that writes some of it on): `None` if no copy at its place
/// writes it; else the place of one that does (whose host copy holds all of
/// it at the fill's place), or why the card cannot be read there.
fn choose(steps: &[Seen], from: usize, to: usize) -> Option<Result<usize, &'static str>> {
    let meets = |a: usize, b: usize| a < to && from < b;
    let first = steps.iter().position(|s| matches!(*s, Seen::Placed { dst, len, .. } if meets(dst, dst + len)))?;
    let Seen::Placed { copy, .. } = steps[first] else {
        return None;
    };
    let mut covered: Vec<(usize, usize)> = Vec::new();
    for s in &steps[first..] {
        match *s {
            Seen::Placed { dst, len, copy: c } if meets(dst, dst + len) => {
                if c != copy {
                    return Some(Err("copies at their place write it through two host copies"));
                }
                join_ranges(&mut covered, [(dst, dst + len)]);
            }
            Seen::Writes { from: a, to: b, copy: None } if meets(a, b) => {
                return Some(Err("a copy done at submission writes it behind the copy at its place"));
            }
            Seen::Writes { from: a, to: b, copy: Some(c) } if c != copy && meets(a, b) => {
                return Some(Err("a shader writes it through another host copy"));
            }
            Seen::Run => return Some(Err("a step of ours runs behind the copy at its place")),
            _ => {}
        }
    }
    if !covered.iter().any(|&(a, b)| a <= from && to <= b) {
        return Some(Err("not all of it is written by copies at their place"));
    }
    Some(Ok(first))
}

/// How shaders read what a fill gives them (as constants, or through the
/// slot's raw twin), and copies (a test reading it back).
const READ: D3D12_RESOURCE_STATES = D3D12_RESOURCE_STATES(
    D3D12_RESOURCE_STATE_VERTEX_AND_CONSTANT_BUFFER.0
        | D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE.0
        | D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE.0
        | D3D12_RESOURCE_STATE_COPY_SOURCE.0,
);

/// The buffer a list's fills are copied into, from its beginning.
const ROOM: u64 = 64 << 10;

/// The most fills of one list done this way (beyond: left to the
/// submission).
const MOST: usize = 1024;

fn refused(_why: &'static str) {
    STATS[1].fetch_add(1, Ordering::Relaxed);
    #[cfg(feature = "diagnostics")]
    {
        *REFUSED.lock().unwrap_or_else(|e| e.into_inner()) = _why;
    }
}

impl HostList {
    /// With [`on`]: a constant buffer of `len` bytes of title memory at
    /// `src` that copies at their place earlier in this list wrote, copied
    /// on the card here (see the file). The buffer and the GPU address of
    /// the bytes, for the slot to show; `None`: the fill is left to the
    /// submission as before.
    pub(crate) fn fill_at_place(&self, src: usize, len: usize) -> Option<(ID3D12Resource, u64)> {
        if !on() || !place::on() {
            return None;
        }
        self.fill_here(src, len)
    }

    fn fill_here(&self, src: usize, len: usize) -> Option<(ID3D12Resource, u64)> {
        if len == 0 {
            return None;
        }
        let host = host()?;
        let shadow = {
            let deferred = self.deferred();
            let first = *deferred.placed.first()?;
            // (Most lists have no copy at its place: nothing is looked at.)
            if !deferred.placed.iter().any(|&i| match &deferred.copies[i] {
                Step::Placed(p) => p.dst < src + len && src < p.dst + p.len,
                _ => false,
            }) {
                return None;
            }
            let steps: Vec<Seen> = deferred.copies[first..].iter().map(seen).collect();
            match choose(&steps, src, src + len)? {
                Ok(i) => match &deferred.copies[first + i] {
                    Step::Placed(p) => p.shadow.clone(),
                    _ => return None,
                },
                Err(why) => {
                    refused(why);
                    return None;
                }
            }
        };
        let (start, span) = shadow.span();
        if shadow.in_place() || (src as u64) < start || (src + len) as u64 > start + span {
            refused("no host copy holds all of it");
            return None;
        }
        let made = self.copy_to_room(host, &shadow, src, len);
        if made.is_some() {
            STATS[0].fetch_add(1, Ordering::Relaxed);
        }
        made
    }

    /// For a constant buffer at `src` (`len` bytes) of title memory that
    /// has a home in the card's memory (`crate::shadow::homes_on`): the
    /// bytes copied out of the home at this place of the list into room of
    /// the list's (aligned as constants must be), the room and its address.
    /// Title memory there holds nothing of what the card wrote, so a fill
    /// of upload memory from it would be wrong. `None` if it is no home.
    pub(crate) fn home_fill(&self, src: usize, len: usize) -> Option<(ID3D12Resource, u64)> {
        if len == 0 || !crate::shadow::homes_alive() {
            return None;
        }
        let shadow = crate::shadow::peek(src as u64, len as u64).filter(|s| s.is_home())?;
        let (start, span) = shadow.span();
        if (src as u64) < start || (src + len) as u64 > start + span {
            return None;
        }
        let host = host()?;
        // The list holds the home until it is reset.
        self.keep(&shadow.buffer);
        let made = self.copy_to_room(host, &shadow, src, len)?;
        crate::shadow::HOME_STATS[6].fetch_add(1, Ordering::Relaxed);
        crate::shadow::HOME_STATS[7].fetch_add(len as u64, Ordering::Relaxed);
        Some(made)
    }

    /// Copies `len` bytes at `src` out of `shadow`'s buffer, at this place
    /// of the list, into room of the list's, readable as constants and
    /// vertices from there on: the room and the copy's address in it.
    fn copy_to_room(&self, host: &Host, shadow: &crate::shadow::Shadow, src: usize, len: usize) -> Option<(ID3D12Resource, u64)> {
        let bytes = (len as u64).next_multiple_of(256);
        let kept = {
            let mut deferred = self.deferred();
            if deferred.on_card_fills >= MOST {
                drop(deferred);
                refused("too many in one list");
                return None;
            }
            deferred.on_card_fills += 1;
            deferred.fill_room.take().filter(|(_, used)| used + bytes <= ROOM)
        };
        let (room, at, new) = match kept {
            Some((room, used)) => (room, used, false),
            None => match self.between(host, bytes.max(ROOM)) {
                Some(room) => (room, 0, true),
                None => {
                    refused("no buffer for it");
                    return None;
                }
            },
        };
        // SAFETY: an open list; live buffers (the list keeps the room, and
        // the host copy through the step of the copy at its place), the
        // ranges inside them. A new room is in the common state and taken
        // to a copy's destination by its first use; an old one was left
        // readable by the fill before.
        unsafe {
            if !new {
                self.list.ResourceBarrier(&[transition(&room, READ, D3D12_RESOURCE_STATE_COPY_DEST)]);
            }
            place::wait_for_all(host, &self.list);
            self.list.CopyBufferRegion(&room, at, &shadow.buffer, shadow.offset(src as u64), len as u64);
            self.list.ResourceBarrier(&[transition(&room, D3D12_RESOURCE_STATE_COPY_DEST, READ)]);
            place::wait_for_all(host, &self.list);
        }
        // SAFETY: a live buffer.
        let gpu = unsafe { room.GetGPUVirtualAddress() } + at;
        let size = if new { bytes.max(ROOM) } else { ROOM };
        if at + bytes < size {
            self.deferred().fill_room = Some((room.clone(), at + bytes));
        }
        Some((room, gpu))
    }
}

/// For the frame-rate line.
pub(super) fn log(frames: f64) {
    let s: Vec<f64> = STATS.iter().map(|r| r.swap(0, Ordering::Relaxed) as f64 / frames).collect();
    if !on() {
        return;
    }
    #[cfg(feature = "diagnostics")]
    let why = std::mem::take(&mut *REFUSED.lock().unwrap_or_else(|e| e.into_inner()));
    #[cfg(not(feature = "diagnostics"))]
    let why = "";
    tracing::info!(
        "fills behind a copy at its place, per frame (VICTORY_FILL_ON_CARD=1): {:.2} done on the card at their place in the \
         list, {:.2} left to the submission{}{}; {:.2} submissions cut behind a copy at its place; {:.2} fills of upload memory \
         that waited for flights on the CPU",
        s[0],
        s[1],
        if why.is_empty() { "" } else { " (the last: " },
        if why.is_empty() { String::new() } else { format!("{why})") },
        s[2],
        s[3],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: usize = 0xa;
    const B: usize = 0xb;

    fn placed(dst: usize, len: usize) -> Seen {
        Seen::Placed { dst, len, copy: A }
    }

    /// Only a fill that a copy at its place earlier in the list wrote is
    /// taken, and only where nothing else writes its bytes between.
    #[test]
    fn a_fill_is_taken_behind_the_copy_that_wrote_it() {
        // No copy at its place, or one elsewhere.
        assert_eq!(choose(&[], 100, 200), None);
        assert_eq!(choose(&[placed(300, 100), Seen::Other], 100, 200), None);
        assert_eq!(choose(&[placed(200, 100)], 100, 200), None);
        // The case of run coolfox-l64: 0xbd0 bytes copied, 0xf0 of them read.
        assert_eq!(choose(&[Seen::Other, placed(0x1000, 0xbd0), Seen::Other], 0x1000, 0x10f0), Some(Ok(1)));
        // Two copies that together hold it.
        assert_eq!(choose(&[placed(100, 50), Seen::Run, placed(150, 50)], 100, 200).map(|r| r.is_ok()), Some(false));
        assert_eq!(choose(&[placed(100, 50), placed(150, 50)], 100, 200), Some(Ok(0)));
        // Bytes no copy at its place wrote.
        assert!(matches!(choose(&[placed(100, 50)], 100, 200), Some(Err(_))));
    }

    #[test]
    fn writes_between_the_copy_and_the_fill_are_refused_or_taken_in_order() {
        let copied = |w: Seen| choose(&[placed(100, 100), w], 120, 140);
        // On the CPU at submission: refused; elsewhere: taken.
        assert!(matches!(copied(Seen::Writes { from: 130, to: 131, copy: None }), Some(Err(_))));
        assert_eq!(copied(Seen::Writes { from: 140, to: 150, copy: None }), Some(Ok(0)));
        // A shader through the same host copy: taken (the card reads it in
        // order); through another: refused.
        assert_eq!(copied(Seen::Writes { from: 100, to: 200, copy: Some(A) }), Some(Ok(0)));
        assert!(matches!(copied(Seen::Writes { from: 100, to: 200, copy: Some(B) }), Some(Err(_))));
        assert_eq!(copied(Seen::Writes { from: 0, to: 100, copy: Some(B) }), Some(Ok(0)));
        // A step of ours.
        assert!(matches!(copied(Seen::Run), Some(Err(_))));
        // Through two host copies.
        assert!(matches!(choose(&[placed(100, 100), Seen::Placed { dst: 100, len: 10, copy: B }], 100, 120), Some(Err(_))));
        // A CPU copy ahead of the copy at its place is under it.
        assert_eq!(choose(&[Seen::Writes { from: 100, to: 200, copy: None }, placed(100, 100)], 120, 140), Some(Ok(1)));
    }

    /// On the card (WARP): a list writes bytes by a "shader", copies them at
    /// their place, a fill behind the copy, a shader write over the copy's
    /// bytes, a second fill behind that. Each fill holds what stood before
    /// it in the list, not what title memory held when the list was
    /// recorded or submitted.
    #[test]
    fn fills_read_what_the_card_wrote_before_them() {
        use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc};
        let Some(host) = host() else {
            return; // no host GPU
        };
        if !place::on() {
            return;
        }
        const LEN: usize = 1 << 20;
        const S: usize = 0x1_0000;
        const D: usize = 0x3_0000;
        const N: usize = 0xf0;
        // SAFETY: memory of our own, never freed.
        let base = unsafe { VirtualAlloc(std::ptr::null(), LEN, MEM_RESERVE | MEM_COMMIT | 0x20_0000, PAGE_READWRITE) }.cast::<u8>();
        assert!(!base.is_null());
        // SAFETY: `LEN` bytes just committed.
        let title = unsafe { std::slice::from_raw_parts_mut(base, LEN) };
        let start = base as usize;
        crate::shadow::register(0xf111, start as u64, LEN as u64, false);
        let shadow = crate::shadow::find(start as u64, 64).expect("a host copy");
        // What the title's CPU holds: not what the fills are to read.
        title[S..S + 0x1000].fill(0x11);
        title[D..D + 0x1000].fill(0x22);
        let (out, out_data) = host.shared_buffer(1 << 16).unwrap();
        let list = host.new_list().unwrap();
        shadow.refresh(&list, start as u64, LEN as u64);
        // A "shader" writes the source on the card.
        shadow.gpu_writes(&list, (start + S) as u64, 0xbd0);
        write(host, &list, &shadow, S, &[0xa7; 0xbd0]);
        // Nothing copied at its place yet: left to the submission.
        assert!(list.fill_here(start + D, N).is_none(), "no copy at its place");
        assert!(list.place_copy(start + D, start + S, 0xbd0), "the copy at its place");
        // Not all of it copied: left to the submission.
        assert!(list.fill_here(start + D + 0xb00, 0x100).is_none(), "past the copy");
        let (first, first_at) = list.fill_here(start + D, N).expect("the fill behind the copy");
        // A shader writes over the copy's bytes, and another fill reads them.
        shadow.gpu_writes(&list, (start + D) as u64, 0x100);
        write(host, &list, &shadow, D, &[0x5b; 0x100]);
        let (second, second_at) = list.fill_here(start + D + 0x10, N).expect("the fill behind the shader");
        // What the draws read.
        // SAFETY: an open list; live buffers, ranges inside them; the rooms
        // are readable by copies.
        unsafe {
            list.list.CopyBufferRegion(&out, 0, &first, first_at - first.GetGPUVirtualAddress(), N as u64);
            list.list.CopyBufferRegion(&out, 0x100, &second, second_at - second.GetGPUVirtualAddress(), N as u64);
        }
        assert_eq!(first_at % 256, 0, "constants begin on 256 bytes");
        assert_eq!(second_at % 256, 0);
        // Nothing for the CPU to fill at submission.
        assert!(!list.deferred().copies.iter().any(|s| matches!(s, Step::Fill(..))));
        list.close();
        host.execute(&[&list]);
        // SAFETY: the mapping; the copies into it have run.
        let read = unsafe { std::slice::from_raw_parts(out_data, 0x200) };
        assert!(read[..N].iter().all(|&b| b == 0xa7), "the first fill reads the copy: {:x?}", &read[..16]);
        assert!(read[0x100..0x100 + N].iter().all(|&b| b == 0x5b), "the second reads the shader: {:x?}", &read[0x100..0x110]);
        // With the debug layer (`VICTORY_D3D12_DEBUG=1`, diagnostics).
        if let Some(said) = host.log_messages() {
            eprintln!("{said}");
        }
    }

    /// The whole way, in a process of its own with the relay on:
    /// `VICTORY_RELAY=2 VICTORY_CARD_STEPS=2 VICTORY_FILL_ON_CARD=1 cargo
    /// test -p victory-gpu --lib --release fill_on_card -- --ignored
    /// --test-threads=1`. A flight in the air writes a source by a "shader",
    /// slowly; the next submission copies it at its place and a draw's
    /// constants read the copy's bytes. Nothing waits for the flight on the
    /// CPU, and each round's constants hold what that round's flight wrote.
    #[test]
    #[ignore = "wants VICTORY_RELAY=2 VICTORY_CARD_STEPS=2 VICTORY_FILL_ON_CARD=1 and a process of its own"]
    fn a_fill_behind_a_flight_and_a_copy_waits_for_nothing() {
        use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc};
        let Some(host) = host() else {
            return;
        };
        assert!(relay_exact() && on() && place::on(), "set VICTORY_RELAY=2 VICTORY_CARD_STEPS=2 VICTORY_FILL_ON_CARD=1");
        const LEN: usize = 1 << 20;
        const S: usize = 0x1_0000;
        const D: usize = 0x3_0000;
        // (The same bytes: the second copy to them stands behind the first,
        // which is in the air, as the frame before's is in the title.)
        const D2: usize = D;
        const N: usize = 0xf0;
        const ROUNDS: usize = 6;
        // SAFETY: memory of our own, never freed.
        let base = unsafe { VirtualAlloc(std::ptr::null(), LEN, MEM_RESERVE | MEM_COMMIT | 0x20_0000, PAGE_READWRITE) }.cast::<u8>();
        assert!(!base.is_null());
        let start = base as usize;
        crate::shadow::register(0xf222, start as u64, LEN as u64, false);
        let shadow = crate::shadow::find(start as u64, 64).expect("a host copy");
        let (slow_from, slow_to) = (host.uav_buffer(64 << 20).unwrap(), host.uav_buffer(64 << 20).unwrap());
        let (out, out_data) = host.shared_buffer(1 << 16).unwrap();
        let pattern = |round: usize, i: usize| (0x40 + round * 29 + i * 3) as u8;
        let (read_settles, done) = (LATE_STATS[WHY_READ].load(Ordering::Relaxed), STATS[0].load(Ordering::Relaxed));
        let mut lists = Vec::new();
        let (mut in_air, mut cut, mut filled) = (0, 0, Vec::new());
        for round in 0..ROUNDS {
            let a = host.new_list().unwrap();
            shadow.refresh(&a, start as u64, LEN as u64);
            shadow.gpu_writes(&a, (start + S) as u64, 0xbd0);
            // SAFETY: an open list; live buffers.
            unsafe {
                for _ in 0..24 {
                    a.list.CopyBufferRegion(&slow_to, 0, &slow_from, 0, 64 << 20);
                }
            }
            let bytes: Vec<u8> = (0..0xbd0).map(|i| pattern(round, i)).collect();
            write(host, &a, &shadow, S, &bytes);
            a.close();
            host.execute_own(&a);
            // With it in the air: the copy at its place and the constants
            // behind it. As they were (a fill at submission) they waited.
            let written = line().written.clone();
            if !written.is_empty() {
                let old = host.new_list().unwrap();
                assert!(old.place_copy(start + D, start + S, 0xbd0));
                let (staging, staged) = old.upload(host, 4096).unwrap();
                old.defer_fill_buffer(&staging, staged, (start + D) as *const u8, N);
                old.close();
                assert_eq!(blocked_by(&written, &[&old], false), Some(WHY_READ), "the fill at submission waits");
                in_air += 1;
            }
            let b = host.new_list().unwrap();
            assert!(b.place_copy(start + D, start + S, 0xbd0), "the copy at its place, round {round}");
            let (room, gpu) = b.fill_at_place(start + D, N).expect("the fill on the card");
            // SAFETY: an open list; live buffers, ranges inside them.
            unsafe {
                b.list.CopyBufferRegion(&out, (round * 256) as u64, &room, gpu - room.GetGPUVirtualAddress(), N as u64);
            }
            b.close();
            assert!(!b.deferred().copies.iter().any(|s| matches!(s, Step::Fill(..))), "nothing to fill on the CPU");
            host.execute_own(&b);
            // A fill behind the copy in a later list of the submission: the
            // submission is cut behind the copy's list (where the flights
            // ask for the fill), and the fill is done on the card in front of
            // the second part, out of the host copy the first part wrote.
            let c = host.new_list().unwrap();
            assert!(c.place_copy(start + D2, start + S, 0xbd0));
            c.close();
            let d = host.new_list().unwrap();
            let (staging, staged) = d.upload(host, 4096).unwrap();
            d.defer_fill_buffer(&staging, staged, (start + D2) as *const u8, N);
            d.close();
            let written = line().written.clone();
            if touches(&written, start + D2, start + D2 + N) {
                assert_eq!(card_steps::cut_for(&[&c, &d]), Some(1), "cut behind the copy, round {round}");
                cut += 1;
            }
            host.execute_own(&c);
            host.execute_own(&d);
            filled.push(staged);
            lists.extend([a, b, c, d]);
        }
        let waited = LATE_STATS[WHY_READ].load(Ordering::Relaxed) - read_settles;
        settle_now(WHY_SYNC);
        // SAFETY: the mapping; the copies into it have run and landed.
        let read = unsafe { std::slice::from_raw_parts(out_data, ROUNDS * 256) };
        for round in 0..ROUNDS {
            let wanted: Vec<u8> = (0..N).map(|i| pattern(round, i)).collect();
            assert!(read[round * 256..round * 256 + N] == wanted[..], "round {round}: {:x?}", &read[round * 256..round * 256 + 16]);
            // SAFETY: the list's upload memory, kept with the list; the fill has run.
            let staged = unsafe { std::slice::from_raw_parts(filled[round], N) };
            assert!(staged == &wanted[..], "the fill behind the cut, round {round}: {:x?}", &staged[..16]);
        }
        // SAFETY: title memory of our own; the flights have landed.
        let title = unsafe { std::slice::from_raw_parts(base, LEN) };
        let last: Vec<u8> = (0..0xbd0).map(|i| pattern(ROUNDS - 1, i)).collect();
        assert!(title[D..D + 0xbd0] == last[..], "title memory holds the last copy");
        assert_eq!(waited, 0, "settles for title memory read");
        assert!(in_air > 0, "no flight was in the air");
        assert!(cut > 0, "no submission would have been cut");
        eprintln!("{in_air} of {ROUNDS} rounds behind a flight in the air, {cut} cut");
        assert_eq!(STATS[0].load(Ordering::Relaxed) - done, ROUNDS as u64);
        if let Some(said) = host.log_messages() {
            eprintln!("{said}");
        }
    }

    /// A shader's stand-in: `bytes` go to offset `at` of the copy.
    fn write(host: &Host, list: &HostList, shadow: &crate::shadow::Shadow, at: usize, bytes: &[u8]) {
        let (upload, data) = host.upload_buffer(bytes.len() as u64).unwrap();
        // SAFETY: the mapping; an open list and live buffers.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
            let at = shadow.offset(shadow.span().0 + at as u64);
            list.list.CopyBufferRegion(&shadow.buffer, at, &upload, 0, bytes.len() as u64);
            list.list
                .ResourceBarrier(&[transition(&shadow.buffer, D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COMMON)]);
        }
        list.keep(&upload);
    }
}
