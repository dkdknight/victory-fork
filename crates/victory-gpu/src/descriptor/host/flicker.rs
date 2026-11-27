//! (After kylepelham/El1xr PR #104, issue #96.) Black one-frame flicker:
//! host slots made null while the title still reads them.
//!
//! `VICTORY_FLICKER_KEEP=N` (frames, 1-64; 0 or unset: off): a slot whose
//! placed texture view a newer view of other memory there made stale
//! ([`super::HostHeap::unmake_stale`], [`super::STALE`]) is not made null at
//! once. Made null at once, the slot reads zeros in the lists already in the
//! air and in the rest of the frame's lists, which the title recorded
//! against the old view (its record in that slot did not change): a surface
//! black for a frame. With the switch the slot forgets what it was made
//! from (a draw or a title write makes it again, as before) but goes on
//! showing the old host copy for `N` frames and until every submission
//! begun or queued when it went stale has finished; then it is made null,
//! unless it was made again meanwhile. Runtime only: no shader or pipeline
//! key changes.
//!
//! `VICTORY_FLICKER_WATCH=1` (diagnostics): a line a frame (frames with
//! any) counting stale slots made null at once (and how many while a
//! submission was in the air), kept, kept then made null, and kept but made
//! again meanwhile.
//!
//! Risks: a stale slot shows the old texture up to `N` frames (late instead
//! of black), and old host copies live `N` frames longer.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use windows::Win32::Graphics::Direct3D12::ID3D12Resource;

/// Frames a stale slot keeps its old view (`VICTORY_FLICKER_KEEP`); 0: off.
pub(super) fn keep_frames() -> u64 {
    static N: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        let n = parse_keep(std::env::var("VICTORY_FLICKER_KEEP").ok().as_deref());
        if n > 0 {
            tracing::info!(frames = n, "stale texture slots keep their old view for a while (VICTORY_FLICKER_KEEP)");
        }
        n
    })
}

fn parse_keep(value: Option<&str>) -> u64 {
    value.and_then(|v| v.trim().parse::<u64>().ok()).map_or(0, |n| n.min(64))
}

/// `VICTORY_FLICKER_WATCH=1`.
fn watch() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_FLICKER_WATCH").is_some_and(|v| v == "1"))
}

/// What a watch line counts, in its order.
#[derive(Clone, Copy)]
enum Count {
    /// Stale slots made null at once.
    StaleNulled,
    /// Of those, while a submission was in the air.
    StaleNulledInAir,
    /// Stale slots that kept their old view (`VICTORY_FLICKER_KEEP`).
    StaleKept,
    /// Kept slots made null once their wait was over.
    KeptNulled,
    /// Kept slots made again meanwhile (left as they were).
    KeptRemade,
}

const COUNTS: usize = 5;
const NAMES: [&str; COUNTS] = ["stale_nulled", "stale_nulled_in_air", "stale_kept", "kept_nulled", "kept_remade"];

static FRAME: AtomicU64 = AtomicU64::new(0);
static NOW: [AtomicU64; COUNTS] = [const { AtomicU64::new(0) }; COUNTS];
static TOTAL: [AtomicU64; COUNTS] = [const { AtomicU64::new(0) }; COUNTS];

/// Counts `what` in the current frame; the frame before is logged first if
/// it had any.
fn count(what: Count) {
    if !watch() {
        return;
    }
    let frame = crate::frame::current();
    let last = FRAME.load(Relaxed);
    if frame != last && FRAME.compare_exchange(last, frame, Relaxed, Relaxed).is_ok() {
        let counts = NOW.each_ref().map(|c| c.swap(0, Relaxed));
        if counts.iter().any(|&c| c > 0) {
            tracing::info!(
                "flicker watch frame={last} {} total_stale_nulled={} total_stale_kept={}",
                line(&counts),
                TOTAL[Count::StaleNulled as usize].load(Relaxed),
                TOTAL[Count::StaleKept as usize].load(Relaxed),
            );
        }
    }
    NOW[what as usize].fetch_add(1, Relaxed);
    TOTAL[what as usize].fetch_add(1, Relaxed);
}

fn line(counts: &[u64; COUNTS]) -> String {
    NAMES.iter().zip(counts).map(|(name, n)| format!("{name}={n}")).collect::<Vec<_>>().join(" ")
}

/// A stale slot waiting to be made null: (frame it may go, submission that
/// must finish first, host heap, slot, the record it was made from, the
/// host copy it showed).
struct Held {
    frame: u64,
    after: u64,
    heap: usize,
    index: u32,
    key: u64,
    raw: usize,
}

static LINE: Mutex<VecDeque<Held>> = Mutex::new(VecDeque::new());

/// Whether a held slot may be made null: its frames are over and every
/// submission begun when it went stale has finished.
fn over(held: &Held, frame: u64, finished: u64) -> bool {
    held.frame <= frame && held.after <= finished
}

fn raw_of(resource: &ID3D12Resource) -> usize {
    windows::core::Interface::as_raw(resource) as usize
}

impl super::HostHeap {
    /// Slot `index`, made from record `key`, went stale; under its lock.
    /// With `VICTORY_FLICKER_KEEP` it forgets the record but keeps showing
    /// what it showed for a while (true: left to [`Self::stale_due`]); else
    /// false and the caller makes it null at once.
    pub(super) fn stale_kept(&self, index: u32, key: u64) -> bool {
        let frames = keep_frames();
        if frames == 0 || self.sampler {
            count(Count::StaleNulled);
            let (started, finished) = crate::host::submissions();
            if started > finished {
                count(Count::StaleNulledInAir);
            }
            return false;
        }
        // (Lists the title made that the host has not begun yet read the
        // slot too: they get the next numbers, as for `host::retire`.)
        let (started, _) = crate::host::submissions();
        let after = started + crate::queue::queued();
        self.hold_stale(index, key, crate::frame::current() + frames, after)
    }

    /// [`Self::stale_kept`] with the switch on: slot `index` keeps its view
    /// until frame `frame` and submission `after`. Under the slot's lock.
    fn hold_stale(&self, index: u32, key: u64, frame: u64, after: u64) -> bool {
        let Some(raw) = self.kept_in(index).as_ref().map(raw_of) else {
            // Nothing of the host's shown: nothing to keep.
            return false;
        };
        // Forgotten: the next draw or title write makes it again.
        self.quick_is(index, 0);
        self.made.set(index, 0);
        LINE.lock().unwrap_or_else(|e| e.into_inner()).push_back(Held {
            frame,
            after,
            heap: std::ptr::from_ref(self) as usize,
            index,
            key,
            raw,
        });
        count(Count::StaleKept);
        true
    }

    /// Makes null the held slots of this heap whose wait is over, if they
    /// still show what they showed when they went stale. Once a texture
    /// pass; nothing with the switch off.
    pub(crate) fn stale_due(&self) {
        if keep_frames() == 0 {
            return;
        }
        let (_, finished) = crate::host::submissions();
        self.release_stale(crate::frame::current(), finished);
    }

    /// Makes null the held slots of this heap whose wait is over at frame
    /// `frame` with submissions up to `finished` done; how many.
    fn release_stale(&self, frame: u64, finished: u64) -> usize {
        let me = std::ptr::from_ref(self) as usize;
        let due: Vec<Held> = {
            let mut line = LINE.lock().unwrap_or_else(|e| e.into_inner());
            if line.is_empty() {
                return 0;
            }
            let (due, left): (Vec<Held>, Vec<Held>) =
                line.drain(..).partition(|h| h.heap == me && over(h, frame, finished));
            line.extend(left);
            due
        };
        let mut nulled = 0;
        for held in due {
            let _made = self.made.lock(held.index);
            let shown = self.kept_in(held.index).as_ref().map(raw_of);
            let still = self.made.get(held.index) == Some(0) && shown == Some(held.raw);
            if !still {
                count(Count::KeptRemade);
                continue;
            }
            self.make_unmade_null(held.index, held.key);
            count(Count::KeptNulled);
            nulled += 1;
        }
        nulled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_is_off_unless_a_number_of_frames() {
        assert_eq!(parse_keep(None), 0);
        assert_eq!(parse_keep(Some("")), 0);
        assert_eq!(parse_keep(Some("0")), 0);
        assert_eq!(parse_keep(Some("x")), 0);
        assert_eq!(parse_keep(Some("3")), 3);
        assert_eq!(parse_keep(Some(" 5 ")), 5);
        assert_eq!(parse_keep(Some("1000")), 64);
    }

    fn held(frame: u64, after: u64) -> Held {
        Held { frame, after, heap: 1, index: 2, key: 3, raw: 4 }
    }

    #[test]
    fn a_kept_slot_waits_for_its_frames_and_its_submissions() {
        let h = held(10, 7);
        assert!(!over(&h, 9, 7), "frames not over");
        assert!(!over(&h, 10, 6), "a submission begun before it is still in the air");
        assert!(over(&h, 10, 7));
        assert!(over(&h, 12, 9));
    }

    #[test]
    fn a_line_names_every_count_in_order() {
        let mut counts = [0; COUNTS];
        counts[Count::StaleKept as usize] = 4;
        counts[Count::KeptRemade as usize] = 2;
        assert_eq!(line(&counts), "stale_nulled=0 stale_nulled_in_air=0 stale_kept=4 kept_nulled=0 kept_remade=2");
    }

    /// A stale slot with the switch keeps its host copy and forgets its
    /// record; it is made null once its frames and submissions are over,
    /// and left alone if it was made again meanwhile.
    #[test]
    fn a_stale_slot_keeps_its_view_until_its_wait_is_over() {
        let Some(heap) = super::super::HostHeap::new(false, 8) else {
            return; // no host GPU
        };
        let Some(host) = crate::host::host() else {
            return;
        };
        let (a, _) = host.upload_buffer(256).unwrap();
        let (b, _) = host.upload_buffer(256).unwrap();
        let key = 7u64 << 32 | 9;
        for i in [1u32, 2] {
            heap.made.set(i, key);
            heap.keep(i, Some(if i == 1 { a.clone() } else { b.clone() }));
        }
        assert!(heap.hold_stale(1, key, 100, 40));
        assert!(heap.hold_stale(2, key, 100, 40));
        assert_eq!(heap.made.get(1), Some(0), "the record is forgotten: a write makes it again");
        assert_eq!(heap.kept_in(1).as_ref().map(raw_of), Some(raw_of(&a)), "the old host copy is still shown");
        // A slot showing nothing of the host's has nothing to keep.
        heap.made.set(3, key);
        assert!(!heap.hold_stale(3, key, 100, 40));
        // Slot 2 made again meanwhile (another host copy).
        let (c, _) = host.upload_buffer(256).unwrap();
        heap.keep(2, Some(c.clone()));
        assert_eq!(heap.release_stale(99, 40), 0, "frames not over");
        assert_eq!(heap.release_stale(100, 39), 0, "a submission still in the air");
        assert_eq!(heap.release_stale(100, 40), 1);
        assert!(heap.kept_in(1).is_none(), "made null after its wait");
        assert_eq!(heap.kept_in(2).as_ref().map(raw_of), Some(raw_of(&c)), "made again: left as it is");
        assert_eq!(heap.release_stale(200, 99), 0, "nothing left");
    }
}
