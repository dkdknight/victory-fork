//! `VICTORY_TARGET_REUSE_REREAD=1` (off by default): a texture the title
//! fills itself whose memory lies in the memory of a texture the host writes
//! (a render target, depth, unordered access: the bakes' results among
//! them), or of one the title released lately, has its staging buffer
//! detiled again from title memory: when it is first found there, then
//! every [`EVERY`] frames while it stays. A line counts the textures found.
//!
//! From kylepelham/El1xr #223 (exp/blackface, batch 5 part O): a
//! character's 512x512 BC1 skin texture moved through four addresses; at the
//! one inside the memory of a bake result the face was black for as long as
//! it stayed there. The view lines up with no target (a block-compressed
//! view is never [`crate::resource::alias_of`]), so the face reads its own
//! host copy, filled from title memory: what can go wrong is when that copy
//! was filled. The title streams the texture into memory a target held, and
//! the staging buffer kept what the memory held before (or the bytes landed
//! after the read and no write to them was seen).
//!
//! shortcut: by time, not by what the title wrote: such a texture is read
//! again every [`EVERY`] frames whether its bytes changed or not (a few
//! hundred kilobytes each).

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

/// Whether `VICTORY_TARGET_REUSE_REREAD=1` (read once).
pub(crate) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var_os("VICTORY_TARGET_REUSE_REREAD").is_some_and(|v| v == "1");
        if on {
            tracing::info!("VICTORY_TARGET_REUSE_REREAD: textures in a target's memory are read again every {EVERY} frames");
        }
        on
    })
}

/// Textures remembered at most (the ones over a target, a few hundred in a
/// scene).
const MOST: usize = 8192;

/// Released targets' memory remembered at most, the newest kept.
const RELEASED: usize = 4096;

/// Frames between reads again of a texture that stays over a target.
const EVERY: u64 = 16;

/// Title memory of targets the title released: start, bytes; oldest first.
static GONE: Mutex<VecDeque<(u64, u64)>> = Mutex::new(VecDeque::new());

/// By texture: the start of the target its memory lay in when it was last
/// read again, and that frame.
static SEEN: Mutex<Option<HashMap<usize, (u64, u64)>>> = Mutex::new(None);

/// Reads again in all; textures found over a target (first time, or over
/// another one); of those, over a released target's memory.
static REREAD: AtomicU64 = AtomicU64::new(0);
static FOUND: AtomicU64 = AtomicU64::new(0);
static FOUND_RELEASED: AtomicU64 = AtomicU64::new(0);

/// What to do with texture `resource`, now (frame `frame`) over the target
/// starting at `over` (None: over none): `None` nothing, `Some(new)` read it
/// again, `new` when it was not over that target before.
fn decide(seen: &mut HashMap<usize, (u64, u64)>, resource: usize, over: Option<u64>, frame: u64) -> Option<bool> {
    let Some(target) = over else {
        seen.remove(&resource);
        return None;
    };
    let was = seen.get(&resource).copied();
    if let Some((start, at)) = was
        && start == target
        && frame.wrapping_sub(at) < EVERY
    {
        return None;
    }
    if seen.len() >= MOST && was.is_none() {
        seen.clear();
    }
    seen.insert(resource, (target, frame));
    Some(was.is_none_or(|(start, _)| start != target))
}

/// The title released a texture the host writes, whose memory began at
/// `start`; `bytes` says how much memory it held ([`on`] only).
pub(crate) fn released(start: u64, bytes: impl FnOnce() -> Option<u64>) {
    if !on() {
        return;
    }
    let Some(len) = bytes().filter(|&l| l > 0) else {
        return;
    };
    remember(&mut GONE.lock().unwrap_or_else(|e| e.into_inner()), start, len);
}

fn remember(gone: &mut VecDeque<(u64, u64)>, start: u64, len: u64) {
    gone.retain(|&(s, _)| s != start);
    if gone.len() >= RELEASED {
        gone.pop_front();
    }
    gone.push_back((start, len));
}

/// The start of the released target's memory `[address, address + len)`
/// overlaps, newest first.
fn released_over(gone: &VecDeque<(u64, u64)>, address: u64, len: u64) -> Option<u64> {
    let end = address.saturating_add(len.max(1));
    gone.iter().rev().find(|&&(s, l)| address < s.saturating_add(l) && s < end).map(|&(s, _)| s)
}

/// For an upload of texture `resource` (one the title fills, not an alias of
/// a target) from title memory `[address, address + len)`: whether its
/// staging buffer is to be detiled again ([`on`] only).
pub(crate) fn reread(resource: usize, address: u64, len: u64) -> bool {
    if !on() || address == 0 || len == 0 {
        return false;
    }
    let live = crate::resource::target_over(address, len);
    let over = live.or_else(|| released_over(&GONE.lock().unwrap_or_else(|e| e.into_inner()), address, len));
    let decided = {
        let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
        decide(seen.get_or_insert_with(HashMap::new), resource, over, crate::frame::current())
    };
    let Some(new) = decided else {
        return false;
    };
    let n = REREAD.fetch_add(1, Relaxed) + 1;
    if new {
        let found = FOUND.fetch_add(1, Relaxed) + 1;
        let released = FOUND_RELEASED.fetch_add(u64::from(live.is_none()), Relaxed) + u64::from(live.is_none());
        if found <= 16 || found.is_power_of_two() {
            tracing::info!(
                found,
                in_released = released,
                rereads = n,
                resource = format_args!("{resource:#x}"),
                address = format_args!("{address:#x}"),
                bytes = len,
                target = format_args!("{:#x}", over.unwrap_or(0)),
                target_released = live.is_none(),
                "target reuse: a texture the title fills lies in a target's memory: read again from title memory (VICTORY_TARGET_REUSE_REREAD)"
            );
        }
    }
    true
}

/// The title released texture `resource`.
pub(crate) fn forget(resource: usize) {
    if !on() {
        return;
    }
    if let Some(seen) = SEEN.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        seen.remove(&resource);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read again the first time it is found over a target, when it is over
    /// another one and every `EVERY` frames while it stays; never while over
    /// none.
    #[test]
    fn a_texture_over_a_target_is_read_again_when_found_and_every_few_frames() {
        let mut seen = HashMap::new();
        assert_eq!(decide(&mut seen, 1, None, 10), None);
        assert_eq!(decide(&mut seen, 1, Some(0x1000), 10), Some(true));
        assert_eq!(decide(&mut seen, 1, Some(0x1000), 11), None);
        assert_eq!(decide(&mut seen, 1, Some(0x1000), 10 + EVERY - 1), None);
        assert_eq!(decide(&mut seen, 1, Some(0x1000), 10 + EVERY), Some(false));
        assert_eq!(decide(&mut seen, 1, Some(0x2000), 10 + EVERY + 1), Some(true));
        assert_eq!(decide(&mut seen, 1, None, 10 + EVERY + 2), None);
        assert_eq!(decide(&mut seen, 1, Some(0x2000), 10 + EVERY + 3), Some(true));
        assert_eq!(decide(&mut seen, 2, Some(0x2000), 10 + EVERY + 3), Some(true));
    }

    /// The skin texture was placed where a bake result had been.
    #[test]
    fn memory_of_a_released_target_is_found_newest_first() {
        let mut gone = VecDeque::new();
        remember(&mut gone, 0x1000, 0x100);
        remember(&mut gone, 0x1080, 0x100);
        assert_eq!(released_over(&gone, 0x1090, 0x10), Some(0x1080));
        assert_eq!(released_over(&gone, 0x1010, 0x10), Some(0x1000));
        assert_eq!(released_over(&gone, 0x1180, 0x10), None);
        remember(&mut gone, 0x1000, 0x100);
        assert_eq!(gone.len(), 2, "the same start is kept once");
        for i in 0..RELEASED as u64 {
            remember(&mut gone, 0x10_0000 + i * 0x1000, 0x1000);
        }
        assert_eq!(gone.len(), RELEASED);
        assert_eq!(released_over(&gone, 0x1000, 0x10), None, "the oldest went");
    }

    /// Off unless the switch is set: nothing is read again.
    #[test]
    fn off_by_default() {
        if std::env::var_os("VICTORY_TARGET_REUSE_REREAD").is_none() {
            assert!(!on());
            assert!(!reread(1, 0x1000, 0x100));
        }
    }
}
