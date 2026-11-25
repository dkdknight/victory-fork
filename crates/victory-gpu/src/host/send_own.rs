//! What goes up to copies in the card's memory goes up as it was when its
//! submission was prepared (`VICTORY_SEND_OWN=1`, live key `send_own`; off
//! by default). From avatarmanz/VIctory-7900XTX 5b422ba and 18c0129.
//!
//! The sends to a copy's card side (`Host::card_before`) were copies out of
//! its CPU side, made when the card comes to the submission's first list.
//! With the relay that is a frame or two after the submission was prepared,
//! and the submissions prepared meanwhile have written the same CPU side
//! with their refreshes: the earlier submission's shaders read a later
//! one's bytes. For what the title's CPU writes that is as on the console
//! (it does not write what the card still reads). But the title's own
//! buffer copies are commands in its lists, done by us when a submission is
//! prepared: "a value to a buffer, a draw that reads it" in one submission
//! and the same with another value in the next is ordinary, and both draws
//! read the second value.
//!
//! With the switch each submission's changed ranges are taken out of the
//! CPU sides into upload memory of the submission's own, in the order the
//! landing buffer of `VICTORY_QUEUE2` bit 2 has them, when they are taken
//! from the dirty lists; the card copies them from there. The memory comes
//! back when the submission has run (`Host::card_done`).
//!
//! Not taken from theirs: the scatter pass that stores runs of a few words
//! as pairs of offset and value (we keep one copy per range, as before), and
//! the flag that skips copies whose dirty list stayed empty.

use super::*;
use crate::shadow::Shadow;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

/// Whether sends come from memory of the submission's own: the live file's
/// `send_own` first, then `VICTORY_SEND_OWN`. Read per submission; the memory
/// a plan holds goes back whatever the switch says by then.
pub(super) fn on() -> bool {
    if let Some(v) = live::value("send_own") {
        return v.trim() == "1";
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var_os("VICTORY_SEND_OWN").is_some_and(|v| v == "1");
        if on {
            tracing::info!(
                "VICTORY_SEND_OWN=1: what goes up to copies in the card's memory is taken out of their CPU sides when the \
                 submission is prepared, into upload memory of its own"
            );
        }
        on
    })
}

/// Since the last line of the copies in the card's memory: bytes taken,
/// ranges taken, microseconds of taking them, buffers made.
pub(super) static STATS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

/// Upload memory a submission's sends come from, mapped.
pub(super) struct Memory {
    pub(super) buffer: ID3D12Resource,
    data: usize,
    size: u64,
}

// SAFETY: a D3D12 object (free-threaded) and its mapping, with one owner.
unsafe impl Send for Memory {}

/// What submissions that have run gave back, biggest first.
static SPARE: Mutex<Vec<Memory>> = Mutex::new(Vec::new());

/// At most this much is kept spare.
const KEPT: u64 = 512 << 20;

fn memory(host: &Host, size: u64) -> Option<Memory> {
    {
        let mut spare = SPARE.lock().unwrap_or_else(|e| e.into_inner());
        let fit = spare.iter().enumerate().filter(|(_, m)| m.size >= size).min_by_key(|(_, m)| m.size).map(|(i, _)| i);
        if let Some(i) = fit {
            return Some(spare.swap_remove(i));
        }
    }
    let size = size.next_multiple_of(4 << 20);
    let (buffer, data) = host.upload_buffer(size)?;
    name(&buffer, || "victory: bytes a submission sends up to card copies".into());
    STATS[3].fetch_add(1, Ordering::Relaxed);
    Some(Memory {
        buffer,
        data: data as usize,
        size,
    })
}

/// The submission that held `memory` has run.
pub(super) fn give_back(memory: Memory) {
    let mut spare = SPARE.lock().unwrap_or_else(|e| e.into_inner());
    spare.push(memory);
    spare.sort_by_key(|m| std::cmp::Reverse(m.size));
    while spare.iter().map(|m| m.size).sum::<u64>() > KEPT {
        spare.remove(0);
    }
}

/// The bytes `dirty` sends up, packed in its order (each copy in the card's
/// memory, each of its ranges), as `Host::card_before` lays them out in the
/// landing buffer. `None` when there is nothing to send or no memory.
pub(super) fn take(host: &Host, dirty: &[(Arc<Shadow>, Vec<(usize, usize)>)]) -> Option<Memory> {
    let began = std::time::Instant::now();
    let mut moves: Vec<(usize, usize, usize)> = Vec::new();
    let mut at = 0usize;
    for (shadow, ranges) in dirty {
        if shadow.card_sides().is_none() {
            continue;
        }
        let from = shadow.card_data() as usize;
        for &(a, b) in ranges {
            moves.push((from + a, at, b - a));
            at += b - a;
        }
    }
    if at == 0 {
        return None;
    }
    let memory = memory(host, at as u64)?;
    let ranges = moves.len();
    for m in &mut moves {
        m.1 += memory.data;
    }
    // (Several threads when they are many bytes: the first send of a big
    // copy is all of it.)
    crate::shadow::copy_all(moves);
    STATS[0].fetch_add(at as u64, Ordering::Relaxed);
    STATS[1].fetch_add(ranges as u64, Ordering::Relaxed);
    STATS[2].fetch_add(began.elapsed().as_micros() as u64, Ordering::Relaxed);
    Some(memory)
}

/// Per frame, for the line of the copies in the card's memory.
pub(super) fn line(frames: f64) -> Option<String> {
    let [bytes, ranges, us, made] = [0, 1, 2, 3].map(|i| STATS[i].swap(0, Ordering::Relaxed) as f64 / frames);
    if bytes + ranges + made == 0.0 {
        return None;
    }
    let spare: u64 = SPARE.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|m| m.size).sum();
    Some(format!(
        "; sent from the submissions' own memory (VICTORY_SEND_OWN): {:.1} MB in {ranges:.0} ranges, {:.2} ms taking them, \
         {made:.2} buffers made, {} MB spare",
        bytes / 1e6,
        us / 1e3,
        spare >> 20
    ))
}
