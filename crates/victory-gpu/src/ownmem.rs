//! The card's memory our layer keeps for itself, by kind, for one log line
//! that shows it whenever it moves (`card memory of ours by kind`), and for
//! the device-removed report.
//!
//! The card's budget on the owner's 16 GB card is ~12 GB and ~10.9 GB of it
//! is in use in the opening cutscene; textures need the room (a bigger
//! budget for buffer copies made the lights wrong). Most of the card is the
//! title's textures; this line says how much of the rest is ours: depth
//! twins, the lists' indirect buffers, our tables, the swap chain, the
//! lists' scratch memory when it is in GPU upload heaps
//! (`VICTORY_SCRATCH_ON_CARD`), and, read from their own counters, the
//! buffer copies and windows in the card's memory and the tiles of levels
//! on demand.
//!
//! Logging only, except [`BudgetSlabs`]: the clamps' budgets of 256 bytes,
//! which each took a 64 KiB buffer of their own (the smallest committed
//! resource), share 64 KiB slabs with `VICTORY_BUDGET_SLAB=1`.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// What of ours is kept on the card, counted here as it is made and let go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Own {
    /// The lists' scratch chunks in GPU upload heaps
    /// (`VICTORY_SCRATCH_ON_CARD`), the spare ones among them.
    Scratch = 0,
    /// The lists' buffers of clamped indirect arguments (one each, grown
    /// to the largest execution; the ones they replaced live to the reset).
    IndirectArgs,
    /// The clamps' budgets: a 64 KiB buffer per list, or slabs of 256 of
    /// them with `VICTORY_BUDGET_SLAB=1`.
    IndirectBudget,
    /// The tables our translated shaders read in GPU upload heaps
    /// (`VICTORY_TABLES_ON_CARD`).
    Tables,
    /// Depth twins: the colour textures shaders write for depth textures
    /// (the console needs none), their depth-plane twins and the buffers a
    /// stencil plane is copied through.
    DepthTwins,
    /// The swap chain's buffers.
    SwapChain,
}

const KINDS: usize = 6;

/// Alive now, by [`Own`]: count and bytes.
static ALIVE: [[AtomicU64; 2]; KINDS] = [const { [AtomicU64::new(0), AtomicU64::new(0)] }; KINDS];

pub(crate) fn add(kind: Own, bytes: u64) {
    ALIVE[kind as usize][0].fetch_add(1, Relaxed);
    ALIVE[kind as usize][1].fetch_add(bytes, Relaxed);
}

pub(crate) fn sub(kind: Own, bytes: u64) {
    // Never below zero, whatever the order of a race between two threads.
    let _ = ALIVE[kind as usize][0].fetch_update(Relaxed, Relaxed, |n| Some(n.saturating_sub(1)));
    let _ = ALIVE[kind as usize][1].fetch_update(Relaxed, Relaxed, |b| Some(b.saturating_sub(bytes)));
}

/// Count and bytes of `kind` alive now.
pub(crate) fn alive(kind: Own) -> (u64, u64) {
    (ALIVE[kind as usize][0].load(Relaxed), ALIVE[kind as usize][1].load(Relaxed))
}

/// One thing of `kind` counted in [`ALIVE`] for as long as this lives:
/// kept beside the resource it stands for.
pub(crate) struct Held {
    kind: Own,
    bytes: u64,
}

impl Held {
    pub(crate) fn new(kind: Own, bytes: u64) -> Held {
        add(kind, bytes);
        Held { kind, bytes }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        sub(self.kind, self.bytes);
    }
}

impl std::fmt::Debug for Held {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Held({:?}, {})", self.kind, self.bytes)
    }
}

/// Whether the line is due: the total moved by `step` bytes or more since
/// it was last said, or `every_s` seconds went by.
pub(crate) fn due(total: u64, last: Option<u64>, since_s: f64, step: u64, every_s: f64) -> bool {
    match last {
        None => true,
        Some(last) => total.abs_diff(last) >= step || since_s >= every_s,
    }
}

/// Bytes of a texel of the swap chain's formats.
pub(crate) fn swap_texel_bytes(format: u32) -> u64 {
    match format {
        // R16G16B16A16 float.
        10 => 8,
        _ => 4,
    }
}

/// What the line reads beside [`ALIVE`]: their own counters elsewhere.
#[derive(Default, Clone, Copy, Debug)]
struct Elsewhere {
    /// Host textures alive (the title's, render targets, twins) and bytes.
    textures: [u64; 2],
    /// Of them, the ones the title fills and their bytes.
    filled: [u64; 2],
    /// Levels on demand: the bytes of tiles given, of the pool's heaps.
    tiles: [u64; 2],
    /// Buffer copies in the card's memory, their bytes from before, windows
    /// in the card's memory, windows in GPU upload heaps.
    copies: [u64; 4],
    /// The card: budget and use, as the system says.
    card: [u64; 2],
    /// Lists whose budget is in a slab ([`budget_slab`]).
    in_slabs: u64,
}

#[cfg(windows)]
fn elsewhere() -> Elsewhere {
    let textures = [0, 1].map(|i| crate::resource::HOST_TEXTURES[i].load(Relaxed));
    let filled = [0, 1].map(|i| crate::resource::KINDS[i].load(Relaxed));
    let tiles = [crate::tiles::given_total(), crate::tiles::heap_total()];
    let copies = crate::shadow::card_memory();
    let card = crate::host::host().and_then(|h| h.video_memory()).map_or([0, 0], |m| [m[0], m[1]]);
    let in_slabs = BUDGET_SLABS.lock().unwrap_or_else(|p| p.into_inner()).used() as u64;
    Elsewhere { textures, filled, tiles, copies, card, in_slabs }
}

/// The line's words for `e` and what is [`ALIVE`]: (text, our bytes in all).
fn words(e: &Elsewhere, alive: &[(u64, u64); KINDS]) -> (String, u64) {
    let mb = |b: u64| b as f64 / (1u64 << 20) as f64;
    let [scratch, args, budget, tables, twins, swap] = *alive;
    // The host textures counted here hold the twins' textures; the buffers
    // stencil planes go through are not in them (counted in the twins).
    let written = e.textures[1].saturating_sub(e.filled[1]).saturating_sub(twins.1);
    let written_n = e.textures[0].saturating_sub(e.filled[0]);
    let ours = scratch.1 + args.1 + budget.1 + tables.1 + twins.1 + swap.1;
    let text = format!(
        "textures the title fills {:.0} MB ({}), textures the host renders to or writes {:.0} MB ({} with the twins), \
         level tiles given {:.0} MB of {:.0} MB of heaps; buffer copies {:.0} MB (their bytes from before {:.0} MB), \
         windows in the card's memory {:.0} MB, windows in GPU upload heaps {:.0} MB; ours alone: depth twins {:.0} MB \
         ({}), lists' scratch in GPU upload heaps {:.0} MB ({} chunks), indirect arguments {:.0} MB ({}), indirect \
         budgets {:.0} MB ({} buffers; {} lists in slabs), tables {:.0} MB ({}), swap chain {:.0} MB; ours alone in all {:.0} MB; the card {:.0} \
         MB in use of {:.0} MB",
        mb(e.filled[1]),
        e.filled[0],
        mb(written),
        written_n,
        mb(e.tiles[0]),
        mb(e.tiles[1]),
        mb(e.copies[0]),
        mb(e.copies[1]),
        mb(e.copies[2]),
        mb(e.copies[3]),
        mb(twins.1),
        twins.0,
        mb(scratch.1),
        scratch.0,
        mb(args.1),
        args.0,
        mb(budget.1),
        budget.0,
        e.in_slabs,
        mb(tables.1),
        tables.0,
        mb(swap.1),
        mb(ours),
        mb(e.card[1]),
        mb(e.card[0]),
    );
    (text, ours)
}

fn alive_all() -> [(u64, u64); KINDS] {
    std::array::from_fn(|k| (ALIVE[k][0].load(Relaxed), ALIVE[k][1].load(Relaxed)))
}

/// Said with the frame-rate lines: when what is on the card moved by 32 MB
/// or more since it was last said, or every 30 seconds.
#[cfg(windows)]
pub(crate) fn line() {
    use std::sync::Mutex;
    static LAST: Mutex<Option<(u64, std::time::Instant)>> = Mutex::new(None);
    let e = elsewhere();
    let alive = alive_all();
    let (text, ours) = words(&e, &alive);
    // All of it: ours, and the host copies of the title's memory.
    let total = ours + e.textures[1] + e.tiles[1] + e.copies.iter().sum::<u64>();
    let mut last = LAST.lock().unwrap_or_else(|p| p.into_inner());
    let since = last.map_or(f64::MAX, |(_, at)| at.elapsed().as_secs_f64());
    if !due(total, last.map(|(t, _)| t), since, 32 << 20, 30.0) {
        return;
    }
    *last = Some((total, std::time::Instant::now()));
    drop(last);
    tracing::info!("card memory of ours by kind: {text}");
}

/// For the device-removed report.
#[cfg(windows)]
pub(crate) fn summary() -> String {
    words(&elsewhere(), &alive_all()).0
}

/// `VICTORY_BUDGET_SLAB=1`: the clamps' budgets (256 bytes a list, zeroed
/// by the list's first clamp) share buffers of 64 KiB, 256 a buffer,
/// instead of a committed 64 KiB buffer each (the smallest the card gives).
/// Read once; said once.
pub(crate) fn budget_slab() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var_os("VICTORY_BUDGET_SLAB").is_some_and(|v| v == "1");
        if on {
            tracing::info!("VICTORY_BUDGET_SLAB=1: the indirect clamps' budgets share 64 KiB buffers, 256 lists a buffer");
        }
        on
    })
}

/// Bytes of a budget, and of a slab.
pub(crate) const BUDGET: u64 = 256;
pub(crate) const SLAB: u64 = 64 << 10;

/// The slabs of the lists' budgets with [`budget_slab`] (host.rs
/// `CardBudget::Slab`): never let go, 64 KiB for 256 lists; the buffer and
/// its GPU address.
#[cfg(windows)]
pub(crate) static BUDGET_SLABS: std::sync::Mutex<
    BudgetSlabs<(windows::Win32::Graphics::Direct3D12::ID3D12Resource, u64)>,
> = std::sync::Mutex::new(BudgetSlabs::new());

// SAFETY: D3D12 resources are free-threaded.
#[cfg(windows)]
unsafe impl Send for BudgetSlabs<(windows::Win32::Graphics::Direct3D12::ID3D12Resource, u64)> {}

/// Slabs of budgets: the buffers (never let go) and the free places
/// (slab, offset). Generic over the buffer so the logic is tested without
/// a card.
pub(crate) struct BudgetSlabs<B> {
    pub(crate) slabs: Vec<B>,
    free: Vec<(usize, u64)>,
}

impl<B: Clone> BudgetSlabs<B> {
    pub(crate) const fn new() -> Self {
        BudgetSlabs { slabs: Vec::new(), free: Vec::new() }
    }

    /// A free place: (buffer, slab, offset), from a new slab `make` gives
    /// when none is free. `None` if it gives none.
    pub(crate) fn take(&mut self, make: impl FnOnce() -> Option<B>) -> Option<(B, usize, u64)> {
        if self.free.is_empty() {
            let slab = make()?;
            let at = self.slabs.len();
            self.slabs.push(slab);
            // Handed out from the front: the last pushed is taken first.
            self.free.extend((0..SLAB / BUDGET).rev().map(|i| (at, i * BUDGET)));
        }
        let (slab, offset) = self.free.pop()?;
        Some((self.slabs[slab].clone(), slab, offset))
    }

    /// The place is free again: its list is gone (and with it every
    /// submission that charged it had run).
    pub(crate) fn give(&mut self, slab: usize, offset: u64) {
        debug_assert!(!self.free.contains(&(slab, offset)));
        self.free.push((slab, offset));
    }

    /// Places in use.
    pub(crate) fn used(&self) -> usize {
        self.slabs.len() * (SLAB / BUDGET) as usize - self.free.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_counts_while_it_lives() {
        let before = alive(Own::SwapChain);
        {
            let _a = Held::new(Own::SwapChain, 1000);
            let _b = Held::new(Own::SwapChain, 24);
            let now = alive(Own::SwapChain);
            assert_eq!(now.0, before.0 + 2);
            assert_eq!(now.1, before.1 + 1024);
        }
        assert_eq!(alive(Own::SwapChain), before);
    }

    #[test]
    fn sub_never_goes_below_zero() {
        // A kind no other test touches.
        sub(Own::Tables, 1 << 40);
        let (_, bytes) = alive(Own::Tables);
        assert!(bytes < 1 << 40);
    }

    #[test]
    fn due_on_a_move_or_a_while() {
        assert!(due(5, None, 0.0, 32, 30.0));
        assert!(!due(100, Some(90), 1.0, 32, 30.0));
        assert!(due(200, Some(100), 1.0, 32, 30.0));
        assert!(due(100, Some(200), 1.0, 32, 30.0));
        assert!(due(100, Some(100), 31.0, 32, 30.0));
    }

    #[test]
    fn words_split_the_twins_out_of_the_written_textures() {
        let e = Elsewhere {
            textures: [10, 1000 << 20],
            filled: [6, 600 << 20],
            tiles: [0, 0],
            copies: [0; 4],
            card: [12 << 30, 10 << 30],
            in_slabs: 0,
        };
        let mut alive = [(0, 0); KINDS];
        alive[Own::DepthTwins as usize] = (2, 100 << 20);
        alive[Own::SwapChain as usize] = (1, 64 << 20);
        let (text, ours) = words(&e, &alive);
        assert_eq!(ours, 164 << 20);
        assert!(text.contains("textures the host renders to or writes 300 MB (4 with the twins)"), "{text}");
        assert!(text.contains("depth twins 100 MB (2)"), "{text}");
        assert!(text.contains("ours alone in all 164 MB"), "{text}");
    }

    #[test]
    fn slabs_hand_out_256_places_each_and_take_them_back() {
        let mut made = 0;
        let mut slabs = BudgetSlabs::<u32>::new();
        let mut places = Vec::new();
        for _ in 0..300 {
            let (b, slab, offset) = slabs
                .take(|| {
                    made += 1;
                    Some(made)
                })
                .unwrap();
            assert_eq!(b as usize, slab + 1);
            assert_eq!(offset % BUDGET, 0);
            assert!(offset < SLAB);
            places.push((slab, offset));
        }
        assert_eq!(made, 2);
        assert_eq!(slabs.used(), 300);
        let mut sorted = places.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 300, "no place handed out twice");
        for (slab, offset) in places.drain(..100) {
            slabs.give(slab, offset);
        }
        assert_eq!(slabs.used(), 200);
        for _ in 0..100 {
            slabs.take(|| panic!("a free place was there")).unwrap();
        }
        assert_eq!(made, 2);
    }

    #[test]
    fn slabs_without_a_buffer_give_none() {
        let mut slabs = BudgetSlabs::<u32>::new();
        assert!(slabs.take(|| None).is_none());
        assert_eq!(slabs.used(), 0);
    }
}
