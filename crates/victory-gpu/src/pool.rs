//! Host memory for the textures the title fills, and for their staging
//! buffers, cut from blocks made once.
//!
//! Each such texture had an allocation of its own and its staging buffer
//! another. On an RX 7900 XTX an allocation takes about 310 microseconds
//! whatever its size, on one thread (`texture::allocation_costs`), and a
//! texture cut from a block 6: the pass that makes the textures shaders
//! pick by index (`draw.rs` `heap_textures`) got through 350-500 of the
//! 24,000 queued when the world loads in each of its 250 ms turns, some
//! forty seconds of a world without its textures.
//! `VICTORY_TEXTURE_POOL=0` makes every one an allocation of its own again.
//!
//! A piece given back is used again only two seconds later (nothing the card
//! was told before that is still running: Windows resets a card that takes
//! that long over one submission), and by then no list names it (the owners' rule, as for allocations of their own): a
//! texture's piece comes back when the last reference to the host texture
//! goes (an object of ours the texture holds says so), a staging buffer's
//! when the last list that recorded its copy is reset.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

use windows::Win32::Graphics::Direct3D12::*;
use windows::core::{GUID, HRESULT, IUnknown, Interface};

/// Bytes of a block; pieces of more than half of one are not pooled (with
/// blocks made ahead, [`ahead`]: of more than one).
const BLOCK: u64 = 64 << 20;
/// Blocks with nothing in them that are kept.
const SPARE: usize = 2;
/// The same while the card has room ([`room`]): 4 GiB of them.
const SPARE_KEPT: usize = 64;
/// The same of blocks of staging buffers, which are the PC's memory: run
/// crash-1 held 5.2 GB of them.
const STAGING_KEPT: usize = 8;
/// How many are kept now.
static SPARES: AtomicUsize = AtomicUsize::new(SPARE);

/// Sets how many empty blocks stay, by the card's memory (budget and use):
/// [`SPARE_KEPT`] below 85 % ([`crate::host::keep_freed`]), else [`SPARE`].
/// Asked once a second.
pub(crate) fn room(memory: Option<[u64; 4]>) {
    let pressure = crate::residency::pressured();
    let limit = spares(memory, crate::host::keep_freed() && !pressure);
    SPARES.store(limit, Ordering::Relaxed);
    let tick = now();
    // A full pool can become idle while another subsystem needs the VRAM.
    // A busy allocator observes the new limit on its next allocation or tick.
    if let Ok(mut pool) = TEXTURES.try_lock() {
        pool.reclaim(tick, limit, true);
        let gone = std::mem::take(&mut pool.gone);
        drop(pool);
        if !gone.is_empty() {
            blocks_later(gone);
        }
    }
    if let Ok(mut pool) = STAGING.try_lock() {
        let staging_limit = if crate::residency::critical_commit() {
            0
        } else if pressure {
            1
        } else {
            limit
        };
        pool.reclaim(tick, staging_limit, true);
    }
    let buffers_limit = if crate::residency::critical_commit() {
        0
    } else if pressure {
        1
    } else {
        BUFFERS_KEPT
    };
    for buffers in [&UPLOADS, &SHARED] {
        if let Ok(mut pool) = buffers.try_lock() {
            pool.reclaim(tick, buffers_limit, true);
        }
    }
}

fn spares(memory: Option<[u64; 4]>, keep: bool) -> usize {
    match memory {
        Some([budget, used, ..]) if keep && u128::from(used) * 100 < u128::from(budget) * 85 => SPARE_KEPT,
        _ => SPARE,
    }
}

/// Milliseconds until a piece given back is used again. (It was three
/// frames, a tenth of a second while the title shows a loading screen at 30
/// a second, with up to 18 submissions in the air.)
const REST: u64 = 2500;

fn now() -> u64 {
    crate::host::now_ms()
}

pub(crate) fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_TEXTURE_POOL").is_none_or(|v| v != "0"))
}

/// How many blocks of textures are made ahead, so that one has been there
/// for a while when its first piece is taken (`VICTORY_RIPE_BLOCKS`, 12;
/// 0: none, and new blocks are used at once, as before).
///
/// A page fault of lists already on the card, in a texture made the same
/// tenth of a second as the first piece of a block made for it, lost the
/// device in runs n08 and n11 (2026-11-25): the title gives a slot that
/// those lists read by index a new texture. Why the card does not have the
/// new block's addresses yet is not known (n11 waited for `MakeResident`;
/// `indirect::tests::new_blocks_under_readers` never loses it). A block that
/// every submission begun before its making has left behind ([`Ripe`]) has
/// no such lists: pieces are cut from those first.
fn ahead() -> usize {
    static AHEAD: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *AHEAD.get_or_init(|| std::env::var("VICTORY_RIPE_BLOCKS").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(12))
}

/// Submissions begun and finished ([`crate::host::submissions`]), and how
/// many blocks to make ahead (0: every block is ripe at once).
#[derive(Clone, Copy)]
struct Ripe {
    started: u64,
    finished: u64,
    ahead: usize,
}

impl Ripe {
    const ALL: Ripe = Ripe { started: 0, finished: 0, ahead: 0 };

    fn now() -> Ripe {
        let (started, finished) = crate::host::submissions();
        // Not more than are kept ([`room`]): they would go and be made again.
        Ripe { started, finished, ahead: ahead().min(SPARES.load(Ordering::Relaxed)) }
    }

    fn is<B>(self, block: &Block<B>) -> bool {
        self.ahead == 0 || block.made <= self.finished
    }
}

/// Pieces cut from blocks that were not ripe, for want of a ripe one.
static UNRIPE: AtomicU64 = AtomicU64::new(0);

/// The free ranges of a block: start and length, never two that touch.
struct Ranges(BTreeMap<u64, u64>);

impl Ranges {
    fn new(size: u64) -> Ranges {
        Ranges(BTreeMap::from([(0, size)]))
    }

    /// The lowest place for `len` bytes at a multiple of `align`.
    fn take(&mut self, len: u64, align: u64) -> Option<u64> {
        let (start, size, at) = self.0.iter().find_map(|(&start, &size)| {
            let at = start.next_multiple_of(align);
            (at + len <= start + size).then_some((start, size, at))
        })?;
        self.0.remove(&start);
        if at > start {
            self.0.insert(start, at - start);
        }
        if at + len < start + size {
            self.0.insert(at + len, start + size - at - len);
        }
        Some(at)
    }

    fn give(&mut self, mut start: u64, mut len: u64) {
        if let Some((&before, &size)) = self.0.range(..start).next_back()
            && before + size == start
        {
            self.0.remove(&before);
            (start, len) = (before, len + size);
        }
        if let Some(size) = self.0.remove(&(start + len)) {
            len += size;
        }
        self.0.insert(start, len);
    }

    fn whole(&self, size: u64) -> bool {
        self.0.get(&0) == Some(&size)
    }
}

/// A part of a block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Piece {
    block: usize,
    start: u64,
    len: u64,
}

struct Block<B> {
    memory: B,
    free: Ranges,
    /// Bytes of the pieces taken from it and not given back.
    held: u64,
    /// Pieces are cut from it only when no other block has room
    /// ([`mark_draining`]): its textures go one by one and it can go too.
    draining: bool,
    /// The end of the highest piece ever taken: above it the memory is as
    /// it was made (taken to be zeros; written so with
    /// `VICTORY_ZERO_NEW_BLOCKS=1`, [`zero_new_blocks`]).
    used_to: u64,
    /// The newest submission begun when it was made ([`Ripe`]).
    made: u64,
    /// When that was (ms).
    #[cfg_attr(not(feature = "diagnostics"), allow(dead_code))]
    since: u64,
}

struct Pool<B> {
    /// `None`: a block let go, its place to be used again.
    blocks: Vec<Option<Block<B>>>,
    /// Pieces given back, and when each may be used again.
    resting: VecDeque<(u64, Piece)>,
    /// The empty blocks it was last told to keep, and the most it keeps.
    spare: usize,
    most: usize,
    retry_growth_at: u64,
    /// With `hold_gone` ([`block_thread`]): the blocks [`Pool::reclaim`] let
    /// go, to be released on `victory-texture-blocks` instead of under the
    /// lock by the thread that took a piece.
    gone: Vec<B>,
    hold_gone: bool,
}

impl<B: Clone> Pool<B> {
    const fn new(most: usize) -> Pool<B> {
        Pool {
            blocks: Vec::new(),
            resting: VecDeque::new(),
            spare: 0,
            most,
            retry_growth_at: 0,
            gone: Vec::new(),
            hold_gone: false,
        }
    }

    /// A piece of `len` bytes and the memory it lies in, and whether that
    /// part was ever used before; `make` makes a block when none has room.
    fn take(&mut self, len: u64, align: u64, now: u64, make: impl FnMut() -> Option<B>) -> Option<(Piece, B, bool)> {
        self.take_ripe(len, align, now, Ripe::ALL, make)
    }

    /// The same from a block that is ripe, if one has room; `make` makes one
    /// ahead too, when few are empty.
    fn take_ripe(
        &mut self,
        len: u64,
        align: u64,
        now: u64,
        ripe: Ripe,
        mut make: impl FnMut() -> Option<B>,
    ) -> Option<(Piece, B, bool)> {
        // With blocks made ahead, what would be an allocation of its own (and
        // new when the card first reads it) lies in one of those too.
        if len == 0 || len > if ripe.ahead > 0 { BLOCK } else { BLOCK / 2 } {
            return None;
        }
        self.rested(now);
        let mut found = None;
        // Blocks being drained last, before a new one is made.
        for draining in [false, true] {
            for (i, block) in self.blocks.iter_mut().enumerate() {
                if let Some(block) = block
                    && block.draining == draining
                    && ripe.is(block)
                    && let Some(start) = block.free.take(len, align)
                {
                    found = Some((i, start));
                    break;
                }
            }
            if found.is_none() && ripe.ahead > 0 {
                // The one that has been there the longest.
                let mut unripe: Vec<(u64, usize)> = self
                    .blocks
                    .iter()
                    .enumerate()
                    .filter_map(|(i, b)| b.as_ref().filter(|b| b.draining == draining).map(|b| (b.made, i)))
                    .collect();
                unripe.sort_unstable();
                found = unripe.into_iter().find_map(|(_, i)| Some((i, self.blocks[i].as_mut()?.free.take(len, align)?)));
            }
            if found.is_some() {
                break;
            }
        }
        // A failed 64 MiB block must not trigger another driver call for each
        // small texture. Existing free ranges remain usable during backoff.
        let (i, start) = match found {
            Some(found) => found,
            None => {
                let memory = self.grow(now, &mut make)?;
                let i = self.add(memory, ripe.started);
                (i, self.blocks[i].as_mut()?.free.take(len, align)?)
            }
        };
        if ripe.ahead > 0 {
            if !ripe.is(self.blocks[i].as_ref()?) {
                let n = UNRIPE.fetch_add(1, Ordering::Relaxed) + 1;
                if n.is_power_of_two() {
                    tracing::info!("textures cut from blocks made while lists ran that still run: {n}");
                }
            }
            let empty = self.blocks.iter().flatten().filter(|b| b.free.whole(BLOCK)).count();
            if empty < ripe.ahead
                && let Some(memory) = self.grow(now, &mut make)
            {
                self.add(memory, ripe.started);
            }
        }
        let block = self.blocks[i].as_mut()?;
        let again = start < block.used_to;
        block.used_to = block.used_to.max(start + len);
        block.held += len;
        Some((Piece { block: i, start, len }, block.memory.clone(), again))
    }

    fn grow(&mut self, now: u64, make: &mut impl FnMut() -> Option<B>) -> Option<B> {
        if now < self.retry_growth_at {
            return None;
        }
        let block = make();
        if block.is_none() {
            self.retry_growth_at = now.saturating_add(500);
        }
        block
    }

    fn add(&mut self, memory: B, made: u64) -> usize {
        let i = match self.blocks.iter().position(Option::is_none) {
            Some(i) => i,
            None => {
                self.blocks.push(None);
                self.blocks.len() - 1
            }
        };
        self.blocks[i] = Some(Block {
            memory,
            free: Ranges::new(BLOCK),
            held: 0,
            draining: false,
            used_to: 0,
            made,
            since: crate::host::now_ms(),
        });
        i
    }

    fn give(&mut self, piece: Piece, now: u64) {
        if let Some(Some(block)) = self.blocks.get_mut(piece.block) {
            block.held = block.held.saturating_sub(piece.len);
        }
        self.resting.push_back((now + REST, piece));
    }

    /// Marks up to `wanted` blocks that hold `most_held` bytes or fewer,
    /// the emptiest first, as draining, and no other (`wanted` 0: none).
    /// Empty blocks are left out: they go, or are used, as before. The
    /// blocks marked and the bytes they hold.
    fn mark_draining(&mut self, wanted: usize, most_held: u64) -> (usize, u64) {
        let mut sparse: Vec<(u64, usize)> = self
            .blocks
            .iter()
            .enumerate()
            .filter_map(|(i, b)| b.as_ref().filter(|b| b.held > 0 && b.held <= most_held).map(|b| (b.held, i)))
            .collect();
        sparse.sort_unstable();
        sparse.truncate(wanted);
        for block in self.blocks.iter_mut().flatten() {
            block.draining = false;
        }
        let mut held = 0;
        for &(bytes, i) in &sparse {
            if let Some(Some(block)) = self.blocks.get_mut(i) {
                block.draining = true;
                held += bytes;
            }
        }
        (sparse.len(), held)
    }

    /// Whether the block at `index` is being drained.
    fn is_draining(&self, index: usize) -> bool {
        self.blocks.get(index).and_then(Option::as_ref).is_some_and(|b| b.draining)
    }

    /// For the census line: blocks, bytes held, bytes free in them, empty
    /// blocks, blocks under a quarter held and the bytes they hold, the
    /// largest free run, blocks being drained.
    fn census(&self) -> [u64; 8] {
        let mut c = [0u64; 8];
        for b in self.blocks.iter().flatten() {
            c[0] += 1;
            c[1] += b.held;
            let free: u64 = b.free.0.values().sum();
            c[2] += free;
            if b.free.whole(BLOCK) {
                c[3] += 1;
            } else if b.held < BLOCK / 4 {
                c[4] += 1;
                c[5] += b.held;
            }
            c[6] = c[6].max(b.free.0.values().copied().max().unwrap_or(0));
            c[7] += u64::from(b.draining);
        }
        c
    }

    /// Makes the pieces that rested long enough free, and lets go of empty
    /// blocks past [`SPARE`].
    fn rested(&mut self, now: u64) {
        self.reclaim(now, SPARES.load(Ordering::Relaxed), false);
    }

    fn reclaim(&mut self, now: u64, limit: usize, sweep: bool) {
        let mut any = false;
        while let Some(&(ready, piece)) = self.resting.front()
            && ready <= now
        {
            self.resting.pop_front();
            if let Some(block) = self.blocks[piece.block].as_mut() {
                block.free.give(piece.start, piece.len);
                if block.free.whole(BLOCK) {
                    any = true;
                    if std::mem::take(&mut block.draining) {
                        DRAINED[2].fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }
        // (Without a piece given back too, once there is less room.)
        let most = limit.min(self.most);
        let previous = std::mem::replace(&mut self.spare, most);
        if sweep || any || most < previous {
            let mut spare = 0;
            for block in &mut self.blocks {
                if block.as_ref().is_some_and(|b| b.free.whole(BLOCK)) {
                    spare += 1;
                    if spare > most
                        && let Some(b) = block.take()
                        && self.hold_gone
                    {
                        self.gone.push(b.memory);
                    }
                }
            }
        }
    }

    fn blocks(&self) -> usize {
        self.blocks.iter().flatten().count()
    }
}

/// A mapped upload buffer's memory.
#[derive(Clone)]
struct Upload(ID3D12Resource, *mut u8);
// SAFETY: the mapping is written by the owners of its pieces, each its own.
unsafe impl Send for Upload {}

static TEXTURES: Mutex<Pool<ID3D12Heap>> = Mutex::new(Pool::new(SPARE_KEPT));
static STAGING: Mutex<Pool<Upload>> = Mutex::new(Pool::new(STAGING_KEPT));
/// Blocks of upload and shared buffers ([`cpu_buffer`]).
static UPLOADS: Mutex<Pool<ID3D12Heap>> = Mutex::new(Pool::new(BUFFERS_KEPT));
static SHARED: Mutex<Pool<ID3D12Heap>> = Mutex::new(Pool::new(BUFFERS_KEPT));

fn lock<T>(m: &'static Mutex<T>) -> std::sync::MutexGuard<'static, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Bytes of the pieces textures hold now.
static HELD: AtomicU64 = AtomicU64::new(0);

/// Bytes of the texture blocks no texture holds: the card's memory in use
/// by us that new textures take first.
pub(crate) fn texture_room() -> u64 {
    (lock(&TEXTURES).blocks() as u64 * BLOCK).saturating_sub(HELD.load(Ordering::Relaxed))
}

/// For the memory line: blocks of textures and of staging buffers, in MiB.
#[cfg(feature = "diagnostics")]
pub(crate) fn memory() -> [u64; 2] {
    [lock(&TEXTURES).blocks() as u64 * (BLOCK >> 20), lock(&STAGING).blocks() as u64 * (BLOCK >> 20)]
}

/// Part of a block of mapped upload memory, back in the pool when dropped.
pub(crate) struct StagingPiece(Piece);

impl Drop for StagingPiece {
    fn drop(&mut self) {
        lock(&STAGING).give(self.0, now());
    }
}

/// Whether new memory of this module's CPU blocks is written with zeros
/// instead of taken to be zeros (`VICTORY_ZERO_NEW_BLOCKS=1`; off by
/// default: as before): each new block of staging buffers whole when it is
/// made, and with `VICTORY_BUFFER_POOL=1` each piece of a new block of
/// buffers when it is first cut. Pieces used before are zeroed either way.
///
/// Port of kylepelham/El1xr #248 (9e6d5a86, kylepelham). D3D12 promises
/// only that new memory is "almost always" zeros: the memory manager may
/// hand a process memory it freed itself without clearing it. There the
/// buffer of zeros of their card-block reuse came up with a freed buffer's
/// bytes under commit pressure (WARP, 11 of 42 loaded runs), and every
/// piece it zeroed got them; on a card, possibly the white and dark blocks
/// seen on Lucia's hair and a window (not proven). Here what is not detiled
/// into a staging piece (`Block::used_to`: above it the memory is taken to
/// be as made) uploads whatever the new block held. A 64 MiB write once per
/// block made (at most [`STAGING_KEPT`] kept).
fn zero_new_blocks() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| crate::switch::on("VICTORY_ZERO_NEW_BLOCKS"))
}

/// Since the start: new staging blocks zeroed whole, new pieces of blocks
/// of buffers zeroed ([`zero_new_blocks`]).
static ZEROED_NEW: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];

fn zeroed_new(which: usize) {
    let n = ZEROED_NEW[which].fetch_add(1, Ordering::Relaxed) + 1;
    if n.is_power_of_two() {
        tracing::info!(
            "new memory written with zeros (VICTORY_ZERO_NEW_BLOCKS=1), since the start: {} staging blocks of {} MiB, {} new pieces of blocks of buffers",
            ZEROED_NEW[0].load(Ordering::Relaxed),
            BLOCK >> 20,
            ZEROED_NEW[1].load(Ordering::Relaxed),
        );
    }
}

/// `bytes` of upload memory, all zeros: the buffer it lies in, where in
/// that buffer, and its mapping. `None`: not pooled.
pub(crate) fn staging(host: &crate::host::Host, bytes: u64) -> Option<(StagingPiece, ID3D12Resource, u64, *mut u8)> {
    if !on() {
        return None;
    }
    // Copies from a buffer begin at multiples of 512.
    let bytes = bytes.next_multiple_of(512);
    let (piece, memory, again) = lock(&STAGING).take(bytes, 512, now(), || {
        // Only gate speculative bindless prefetch on staging-pool growth
        // headroom; table-bound textures and decoration bakes are sampled
        // immediately and would otherwise read uninitialized/reused VRAM.
        let speculative = crate::texture::PICKING.get().is_some() && !crate::texture::BAKING.get();
        if speculative && !crate::residency::allow_staging_growth(false) {
            return None;
        }
        let (buffer, data) = host.upload_buffer(BLOCK)?;
        crate::host::name(&buffer, || "staging of textures (a block)".into());
        if zero_new_blocks() {
            // SAFETY: the whole mapping of a buffer no one else has yet.
            unsafe { std::ptr::write_bytes(data, 0, BLOCK as usize) };
            zeroed_new(0);
        }
        Some(Upload(buffer, data))
    })?;
    let owned = StagingPiece(piece);
    // SAFETY: the piece lies in the block's mapping.
    let data = unsafe { memory.1.add(piece.start as usize) };
    if again {
        // What is not detiled into it (an unmapped texture) uploads zeros,
        // as from a buffer of its own.
        // SAFETY: the piece is ours alone.
        unsafe { std::ptr::write_bytes(data, 0, bytes as usize) };
    }
    Some((owned, memory.0, piece.start, data))
}

/// An object the host texture holds and releases when it goes: its piece
/// is free then.
#[repr(C)]
struct Reclaim {
    methods: &'static [*const (); 3],
    references: AtomicU32,
    piece: Piece,
    /// The pool the piece goes back to ([`TEXTURES`], or one of buffers).
    to: &'static Mutex<Pool<ID3D12Heap>>,
}

struct Methods([*const (); 3]);
// SAFETY: function addresses only.
unsafe impl Sync for Methods {}

static RECLAIM: Methods = Methods([query as *const (), add_ref as *const (), release as *const ()]);

unsafe extern "system" fn query(this: *mut c_void, iid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    const E_NOINTERFACE: HRESULT = HRESULT(0x8000_4002_u32 as i32);
    // SAFETY: COM's arguments.
    unsafe {
        if out.is_null() {
            return HRESULT(0x8000_4003_u32 as i32);
        }
        if iid.as_ref().is_some_and(|iid| *iid == IUnknown::IID) {
            add_ref(this);
            *out = this;
            return HRESULT(0);
        }
        *out = std::ptr::null_mut();
    }
    E_NOINTERFACE
}

unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
    // SAFETY: one of ours, alive while a reference to it is.
    unsafe { (*this.cast::<Reclaim>()).references.fetch_add(1, Ordering::Relaxed) + 1 }
}

unsafe extern "system" fn release(this: *mut c_void) -> u32 {
    // SAFETY: as above; the last reference frees the box it was made as.
    unsafe {
        let left = (*this.cast::<Reclaim>()).references.fetch_sub(1, Ordering::AcqRel) - 1;
        if left == 0 {
            let gone = Box::from_raw(this.cast::<Reclaim>());
            if std::ptr::eq(gone.to, &TEXTURES) {
                HELD.fetch_sub(gone.piece.len, Ordering::Relaxed);
            }
            lock(gone.to).give(gone.piece, now());
        }
        left
    }
}

/// Textures made in the last second ([`fresh`]), and when.
static RECENT: Mutex<VecDeque<(usize, u64)>> = Mutex::new(VecDeque::new());

/// Whether `texture` was made in a block in the last second.
pub(crate) fn fresh(texture: &ID3D12Resource) -> bool {
    let raw = texture.as_raw() as usize;
    let since = now().saturating_sub(1000);
    RECENT.lock().unwrap_or_else(|e| e.into_inner()).iter().rev().take_while(|r| r.1 >= since).any(|r| r.0 == raw)
}

fn made_now(texture: &ID3D12Resource) {
    let now = now();
    let mut recent = RECENT.lock().unwrap_or_else(|e| e.into_inner());
    while recent.front().is_some_and(|r| r.1 + 1000 < now) {
        recent.pop_front();
    }
    recent.push_back((texture.as_raw() as usize, now));
}

/// The private data a texture cut from a block holds: a [`Reclaim`].
const KEY: GUID = GUID::from_u128(0x6f0e2f0c_5a59_4b1e_9a3f_7f4d2c9be713);

/// The piece `texture` was cut from, if it was cut from a block of ours.
fn piece_of(texture: &ID3D12Resource) -> Option<Piece> {
    let mut size = std::mem::size_of::<*mut c_void>() as u32;
    let mut raw: *mut c_void = std::ptr::null_mut();
    // SAFETY: an interface set with `SetPrivateDataInterface` comes back as
    // its pointer, with a reference of its own, which `object` takes.
    unsafe { texture.GetPrivateData(&KEY, &mut size, Some((&raw mut raw).cast())) }.ok()?;
    if raw.is_null() || size as usize != std::mem::size_of::<*mut c_void>() {
        return None;
    }
    // SAFETY: as above.
    let object = unsafe { IUnknown::from_raw(raw) };
    // Every COM object begins with its methods: ours are those of a
    // `Reclaim`. (The texture holds a reference: this one is not the last.)
    // SAFETY: a live COM object; its fields are read only if it is ours.
    let piece = unsafe {
        let reclaim = raw.cast::<Reclaim>();
        (std::ptr::eq((*reclaim).methods.as_ptr(), RECLAIM.0.as_ptr()) && std::ptr::eq((*reclaim).to, &TEXTURES))
            .then(|| (*reclaim).piece)
    };
    drop(object);
    piece
}

/// Whether textures in blocks that hold little are let go when idle, so
/// that those blocks go back to the card (`VICTORY_POOL_DRAIN=1`; off by
/// default). See `resource.rs` `drain_blocks`.
pub(crate) fn drain_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| on() && std::env::var_os("VICTORY_POOL_DRAIN").is_some_and(|v| v == "1"))
}

/// Since the start: textures let go to drain their blocks, their bytes,
/// blocks drained to the last piece.
pub(crate) static DRAINED: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

/// Marks up to `wanted` of the blocks of textures that hold `most_held`
/// bytes or fewer, the emptiest first, as draining (and no other): no piece
/// is cut from them while another block has room. The blocks and the bytes
/// they hold.
pub(crate) fn mark_draining(wanted: usize, most_held: u64) -> (usize, u64) {
    lock(&TEXTURES).mark_draining(wanted, most_held)
}

/// Whether `texture` lies in a block being drained.
pub(crate) fn in_draining_block(texture: &ID3D12Resource) -> bool {
    // (The piece first, the lock after: no reference of ours is let go
    // under it.)
    piece_of(texture).is_some_and(|p| lock(&TEXTURES).is_draining(p.block))
}

/// For the census line, in bytes but for the counts: blocks of textures,
/// bytes textures hold in them, bytes free in them, empty blocks, blocks
/// under a quarter held and the bytes they hold, the largest free run,
/// blocks being drained.
pub(crate) fn census() -> [u64; 8] {
    lock(&TEXTURES).census()
}

/// Whether `resource` was cut from a block of this module's.
#[cfg(test)]
fn in_block(resource: &ID3D12Resource) -> bool {
    let mut size = 0u32;
    // SAFETY: asks only the size of the private data, if there is any.
    unsafe { resource.GetPrivateData(&KEY, &mut size, None) }.is_ok() && size > 0
}

/// Makes `texture` give `piece` back to [`TEXTURES`] when the last
/// reference to it goes.
fn give_back_with(texture: &ID3D12Resource, piece: Piece) {
    give_back_to(texture, piece, &TEXTURES);
}

/// Makes `texture` (a texture or a buffer) give `piece` back to `to` when
/// the last reference to it goes.
fn give_back_to(texture: &ID3D12Resource, piece: Piece, to: &'static Mutex<Pool<ID3D12Heap>>) {
    let made = Box::into_raw(Box::new(Reclaim { methods: &RECLAIM.0, references: AtomicU32::new(1), piece, to }));
    // SAFETY: a COM object of ours with one reference, which this takes and
    // drops at the end; the texture takes its own.
    unsafe {
        let object = IUnknown::from_raw(made.cast());
        if texture.SetPrivateDataInterface(&KEY, &object).is_err() {
            // Never given back: the piece stays taken, nothing is reused
            // under a texture that may be alive.
            std::mem::forget(object);
        }
    }
}

/// A block of texture memory (64 MiB), resident and mapped: tens of
/// milliseconds.
fn texture_block(device: &ID3D12Device) -> Option<ID3D12Heap> {
    let desc = D3D12_HEAP_DESC {
        SizeInBytes: BLOCK,
        Properties: D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_DEFAULT, ..Default::default() },
        Alignment: 0,
        Flags: D3D12_HEAP_FLAG_ALLOW_ONLY_NON_RT_DS_TEXTURES | crate::host::not_resident(),
    };
    let mut heap: Option<ID3D12Heap> = None;
    // SAFETY: plain creation.
    if let Err(e) = unsafe { device.CreateHeap(&desc, &mut heap) } {
        crate::residency::allocation_error(&e);
        tracing::warn!(error = %e, "no block of texture memory");
    }
    if let Some(h) = &heap
        && let Err(e) = crate::host::resident(device, h).and_then(|()| crate::host::mapped(device))
    {
        tracing::warn!(error = %e, "a block of texture memory is not resident");
        return None;
    }
    heap
}

/// Whether the texture pool's blocks are made, and let go, by a thread of
/// their own, `victory-texture-blocks` (below normal), never by a thread
/// that places a texture (`VICTORY_TEXTURE_BLOCK_THREAD=1`; off by default:
/// as before).
///
/// Port of kylepelham/El1xr #233 (55a3a677, kylepelham; worldstream, #26).
/// A thread that placed a texture made the block when none had room, and
/// the one made ahead (`VICTORY_RIPE_BLOCKS`) after its cut: a
/// `CreateHeap`, `MakeResident` and a wait for the card to map it, 11-45 ms
/// each there, under the lock of [`TEXTURES`], so every other thread placing
/// a texture waited too (the recording thread, the prepare and bake
/// threads). Blocks let go (empty past the spares) were released under it
/// as well. Here the thread keeps `max(VICTORY_RIPE_BLOCKS, 4)` blocks
/// empty ahead (never more than the pool keeps, [`SPARES`]); a take that
/// finds no room rings it and makes a committed texture this once (the
/// caller's fallback): it never waits for a block.
///
/// What does not change: a block goes only when every piece of it is back
/// and rested, pieces come back with their texture's last reference, and
/// are cut from blocks the submissions begun before them have left behind.
fn block_thread() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| on() && crate::switch::on("VICTORY_TEXTURE_BLOCK_THREAD"))
}

/// Empty blocks kept ahead at least by `victory-texture-blocks`.
const BLOCKS_LEAST: usize = 4;

/// How many empty blocks the thread keeps: [`ahead`] (at least
/// [`BLOCKS_LEAST`]), never more than the pool keeps empty now.
fn blocks_wanted(ahead: usize, spares: usize) -> usize {
    ahead.max(BLOCKS_LEAST).min(spares)
}

/// Empty blocks in `pool`.
fn empties<B: Clone>(pool: &Pool<B>) -> usize {
    pool.blocks.iter().flatten().filter(|b| b.free.whole(BLOCK)).count()
}

/// Since the last line: takes that found no room, blocks made on the
/// thread and their microseconds, blocks let go on it and theirs.
static BLOCK_STATS: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];

/// The thread's mailbox: rung, and blocks to let go.
struct BlockMail {
    rung: bool,
    gone: Vec<ID3D12Heap>,
}

static BLOCK_MAIL: Mutex<BlockMail> = Mutex::new(BlockMail { rung: false, gone: Vec::new() });
static BLOCK_BELL: std::sync::Condvar = std::sync::Condvar::new();

/// A piece for a texture of `len` bytes, no block made or let go here; the
/// thread is rung when no block had room or few are empty.
fn take_behind(len: u64, align: u64, ripe: Ripe) -> Option<(Piece, ID3D12Heap, bool)> {
    let mut asked = false;
    let mut pool = lock(&TEXTURES);
    pool.hold_gone = true;
    let taken = pool.take_ripe(len, align, now(), ripe, || {
        asked = true;
        None
    });
    let gone = std::mem::take(&mut pool.gone);
    let low = empties(&pool) < blocks_wanted(ahead(), SPARES.load(Ordering::Relaxed));
    drop(pool);
    if taken.is_none() && len <= BLOCK {
        BLOCK_STATS[0].fetch_add(1, Ordering::Relaxed);
    }
    if !gone.is_empty() {
        blocks_later(gone);
    } else if asked || low {
        ring_block_thread();
    }
    taken
}

/// Rings `victory-texture-blocks` (started on first use).
fn ring_block_thread() {
    start_block_thread();
    lock(&BLOCK_MAIL).rung = true;
    BLOCK_BELL.notify_one();
}

/// Blocks the pool let go of, released on the thread.
fn blocks_later(gone: Vec<ID3D12Heap>) {
    start_block_thread();
    let mut mail = lock(&BLOCK_MAIL);
    mail.gone.extend(gone);
    mail.rung = true;
    drop(mail);
    BLOCK_BELL.notify_one();
}

fn start_block_thread() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        match std::thread::Builder::new().name("victory-texture-blocks".into()).spawn(block_thread_run) {
            Ok(_) => tracing::info!(
                "texture blocks made and let go on victory-texture-blocks (VICTORY_TEXTURE_BLOCK_THREAD=1), {} kept empty ahead at least",
                BLOCKS_LEAST
            ),
            Err(e) => tracing::warn!(error = %e, "victory-texture-blocks did not start: takes without room make committed textures"),
        }
    });
}

fn block_thread_run() {
    use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};
    // SAFETY: plain call on this thread's pseudo handle.
    let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL) };
    let mut held_back_until = 0;
    loop {
        let gone = {
            let mut mail = lock(&BLOCK_MAIL);
            if !mail.rung && mail.gone.is_empty() {
                mail = BLOCK_BELL
                    .wait_timeout(mail, std::time::Duration::from_millis(250))
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
            mail.rung = false;
            std::mem::take(&mut mail.gone)
        };
        if !gone.is_empty() {
            let began = std::time::Instant::now();
            let n = gone.len() as u64;
            drop(gone);
            BLOCK_STATS[3].fetch_add(n, Ordering::Relaxed);
            BLOCK_STATS[4].fetch_add(began.elapsed().as_micros() as u64, Ordering::Relaxed);
        }
        if let Some(host) = crate::host::host()
            && now() >= held_back_until
        {
            // One at a time, outside the lock, until enough are empty.
            loop {
                {
                    let pool = lock(&TEXTURES);
                    if empties(&pool) >= blocks_wanted(ahead(), SPARES.load(Ordering::Relaxed)) {
                        break;
                    }
                }
                let started = crate::host::submissions().0;
                let began = std::time::Instant::now();
                let Some(block) = texture_block(&host.device) else {
                    // (The host refused: not asked again for a while.)
                    held_back_until = now() + 500;
                    break;
                };
                let us = began.elapsed().as_micros() as u64;
                let mut pool = lock(&TEXTURES);
                pool.hold_gone = true;
                pool.add(block, started);
                drop(pool);
                BLOCK_STATS[1].fetch_add(1, Ordering::Relaxed);
                BLOCK_STATS[2].fetch_add(us, Ordering::Relaxed);
            }
        }
        block_thread_line();
    }
}

/// Every 5 s (from the thread), what it did and the pool as it stands.
fn block_thread_line() {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let at = now();
    if at < NEXT.load(Ordering::Relaxed) {
        return;
    }
    NEXT.store(at + 5000, Ordering::Relaxed);
    let s: [u64; 5] = std::array::from_fn(|i| BLOCK_STATS[i].swap(0, Ordering::Relaxed));
    if s.iter().all(|&v| v == 0) {
        return;
    }
    let (blocks, empty) = {
        let pool = lock(&TEXTURES);
        (pool.blocks(), empties(&pool))
    };
    tracing::info!(
        "texture blocks on victory-texture-blocks (VICTORY_TEXTURE_BLOCK_THREAD=1), over 5 s: {} made in {:.1} ms, {} let go in {:.1} ms; \
         {} takes found no room (committed instead); {blocks} blocks, {empty} empty",
        s[1],
        s[2] as f64 / 1e3,
        s[3],
        s[4] as f64 / 1e3,
        s[0],
    );
}

/// A texture of `desc` (no target, depth or unordered access) in a block;
/// `None`: not pooled, or the host refused.
///
/// # Safety
/// `desc` is a valid description.
pub(crate) unsafe fn texture(
    device: &ID3D12Device,
    desc: *const D3D12_RESOURCE_DESC,
    state: D3D12_RESOURCE_STATES,
) -> Option<ID3D12Resource> {
    if !on() {
        return None;
    }
    // Small textures lie at multiples of 4 KiB where the host allows it
    // (the top mip is 64 KiB at most): 64 KiB each otherwise.
    // SAFETY: the caller's description.
    let mut d = unsafe { *desc };
    d.Alignment = u64::from(D3D12_SMALL_RESOURCE_PLACEMENT_ALIGNMENT);
    // SAFETY: plain queries.
    let mut info = unsafe { device.GetResourceAllocationInfo(0, &[d]) };
    if info.SizeInBytes == u64::MAX || info.Alignment != d.Alignment {
        d.Alignment = 0;
        info = unsafe { device.GetResourceAllocationInfo(0, &[d]) };
    }
    if info.SizeInBytes == u64::MAX {
        return None;
    }
    // (`VICTORY_TEXTURE_BLOCK_THREAD=1`: no block made here, [`block_thread`].)
    let taken = if block_thread() {
        take_behind(info.SizeInBytes, info.Alignment.max(1), Ripe::now())
    } else {
        lock(&TEXTURES).take_ripe(info.SizeInBytes, info.Alignment.max(1), now(), Ripe::now(), || texture_block(device))
    };
    #[cfg_attr(not(feature = "diagnostics"), allow(unused_variables))]
    let (piece, heap, again) = taken?;
    let mut made: Option<ID3D12Resource> = None;
    // SAFETY: the piece is no one else's; the heap lives while a piece of
    // it is taken.
    let result = unsafe { device.CreatePlacedResource(&heap, piece.start, &d, state, None, &mut made) };
    match (result, made) {
        (Ok(()), Some(made)) => {
            HELD.fetch_add(piece.len, Ordering::Relaxed);
            give_back_with(&made, piece);
            made_now(&made);
            #[cfg(feature = "diagnostics")]
            {
                crate::book::made(&made, || "a texture in a block".into());
                crate::book::block(&heap, &made, piece.start, BLOCK);
                crate::book::note(made.as_raw() as usize, || {
                    let block = lock(&TEXTURES).blocks.get(piece.block).and_then(Option::as_ref).map(|b| (b.made, b.since));
                    let (submission, since) = block.unwrap_or_default();
                    format!(
                        "; piece {:#x}+{:#x} of block {} {:#x} made {:.1} s before it at submission {submission}, used before: {again}, made at submissions {:?}",
                        piece.start,
                        piece.len,
                        piece.block,
                        heap.as_raw() as usize,
                        now().saturating_sub(since) as f64 / 1e3,
                        crate::host::submissions()
                    )
                });
            }
            Some(made)
        }
        _ => {
            lock(&TEXTURES).give(piece, now());
            None
        }
    }
}

/// Whether upload buffers and shared buffers (the PC's memory, which the
/// CPU maps) of up to [`BUFFER_MOST`] are cut from blocks
/// (`VICTORY_BUFFER_POOL=1`; off by default: each an allocation of its own,
/// as before).
///
/// Port of trustno1-coder/VIctory-NVIDIA #991 (95509b9a, trustno1-coder).
/// Each was a committed resource made resident on the thread that asked
/// for it, about a millisecond each and up to 22 ms while the system pages.
/// A play log there counted in each 5 seconds of play 300 to 441 upload
/// buffers made (355 to 693 ms of waiting) and 220 to 382 shared buffers
/// (180 to 790 ms), on the threads that translate and submit the title's
/// lists. Cut from a block, a buffer is one placement and one mapping; the
/// block's residency is waited for once per 64 MiB.
///
/// A piece used before is zeroed by the CPU before it is handed out: a
/// committed buffer starts as zeros, and callers count on that (the host
/// copies of title buffers are compared against their card side, which
/// began as zeros). The card's own buffers are not pooled: no CPU can zero
/// them. A piece comes back when the last reference to its buffer goes and
/// rests [`REST`] ms, as a texture's.
pub(crate) fn buffers_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| crate::switch::on("VICTORY_BUFFER_POOL"))
}

/// The largest buffer [`cpu_buffer`] cuts from a block; larger ones are
/// allocations of their own.
const BUFFER_MOST: u64 = 4 << 20;

/// Empty blocks of buffers kept ([`room`]): they are the PC's memory, and
/// commit charge.
const BUFFERS_KEPT: usize = 2;

/// The piece a buffer of `size` bytes takes in a block: buffers lie at
/// multiples of 64 KiB. `None`: larger than [`BUFFER_MOST`], not cut from
/// a block.
fn buffer_piece(size: u64) -> Option<u64> {
    (size <= BUFFER_MOST).then(|| size.max(1).next_multiple_of(u64::from(D3D12_DEFAULT_RESOURCE_PLACEMENT_ALIGNMENT)))
}

/// Where a buffer of [`cpu_buffer`] lies.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum CpuBuffer {
    /// An upload buffer (`crate::host::Host::upload_buffer`): the card
    /// reads it, the CPU writes it (write-combined).
    Upload,
    /// A shared buffer (`crate::host::Host::shared_buffer_now`): the PC's
    /// memory, cached, which shaders may read and write.
    Shared,
}

/// Buffers cut from blocks since the start, by kind (upload, shared), and
/// of those the pieces used before (zeroed by the CPU).
pub(crate) static CUT_BUFFERS: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

/// A buffer of `size` bytes in a block of `kind`, mapped, all zeros: what
/// `Host::upload_buffer` or `Host::shared_buffer_now` would have made.
/// `None`: not pooled (switched off, or larger than [`BUFFER_MOST`]) or the
/// host refused; the caller makes an allocation of its own then.
pub(crate) fn cpu_buffer(device: &ID3D12Device, kind: CpuBuffer, size: u64) -> Option<(ID3D12Resource, *mut u8)> {
    if !buffers_on() {
        return None;
    }
    const ALIGN: u64 = D3D12_DEFAULT_RESOURCE_PLACEMENT_ALIGNMENT as u64;
    let len = buffer_piece(size)?;
    let pool = match kind {
        CpuBuffer::Upload => &UPLOADS,
        CpuBuffer::Shared => &SHARED,
    };
    let (piece, heap, again) = lock(pool).take(len, ALIGN, now(), || {
        let properties = match kind {
            CpuBuffer::Upload => D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_UPLOAD, ..Default::default() },
            CpuBuffer::Shared => D3D12_HEAP_PROPERTIES {
                Type: D3D12_HEAP_TYPE_CUSTOM,
                CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_WRITE_BACK,
                MemoryPoolPreference: D3D12_MEMORY_POOL_L0,
                ..Default::default()
            },
        };
        let desc = D3D12_HEAP_DESC {
            SizeInBytes: BLOCK,
            Properties: properties,
            Alignment: 0,
            Flags: D3D12_HEAP_FLAG_ALLOW_ONLY_BUFFERS | crate::host::not_resident(),
        };
        let mut heap: Option<ID3D12Heap> = None;
        // SAFETY: plain creation.
        if let Err(e) = unsafe { device.CreateHeap(&desc, &mut heap) } {
            crate::residency::allocation_error(&e);
            tracing::warn!(error = %e, ?kind, "no block of buffers");
            return None;
        }
        let heap = heap?;
        let waited = crate::host::resident(device, &heap).and_then(|()| match kind {
            CpuBuffer::Upload => Ok(()),
            CpuBuffer::Shared => crate::host::mapped(device),
        });
        if let Err(e) = waited {
            tracing::warn!(error = %e, ?kind, "a block of buffers is not resident");
            return None;
        }
        Some(heap)
    })?;
    let (flags, state) = match kind {
        CpuBuffer::Upload => (D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_GENERIC_READ),
        CpuBuffer::Shared => (D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COMMON),
    };
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Width: size.max(1),
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        Flags: flags,
        ..Default::default()
    };
    let mut made: Option<ID3D12Resource> = None;
    // SAFETY: the piece is no one else's; the heap lives while a piece of
    // it is taken.
    let placed = unsafe { device.CreatePlacedResource(&heap, piece.start, &desc, state, None, &mut made) };
    let Some(made) = placed.ok().and(made) else {
        lock(pool).give(piece, now());
        return None;
    };
    let mut data = std::ptr::null_mut();
    // SAFETY: the whole of a CPU-visible buffer.
    if unsafe { made.Map(0, None, Some(&mut data)) }.is_err() || data.is_null() {
        // (The piece comes back with the buffer, once it is told how.)
        give_back_to(&made, piece, pool);
        return None;
    }
    if again {
        // SAFETY: the mapping holds `size` bytes, ours alone.
        unsafe { std::ptr::write_bytes(data.cast::<u8>(), 0, size as usize) };
        CUT_BUFFERS[2].fetch_add(1, Ordering::Relaxed);
    } else if zero_new_blocks() {
        // (A heap cannot be mapped whole: each piece of a new block is
        // zeroed as it is first cut.)
        // SAFETY: as above.
        unsafe { std::ptr::write_bytes(data.cast::<u8>(), 0, size as usize) };
        zeroed_new(1);
    }
    give_back_to(&made, piece, pool);
    let n = CUT_BUFFERS[kind as usize].fetch_add(1, Ordering::Relaxed) + 1;
    if n.is_power_of_two() {
        tracing::info!(
            "buffers cut from blocks (VICTORY_BUFFER_POOL=1), since the start: {} upload, {} shared, {} of them zeroed for use again; blocks {} upload, {} shared",
            CUT_BUFFERS[0].load(Ordering::Relaxed),
            CUT_BUFFERS[1].load(Ordering::Relaxed),
            CUT_BUFFERS[2].load(Ordering::Relaxed),
            lock(&UPLOADS).blocks(),
            lock(&SHARED).blocks(),
        );
    }
    #[cfg(feature = "diagnostics")]
    crate::book::made(&made, || match kind {
        CpuBuffer::Upload => "an upload buffer in a block".into(),
        CpuBuffer::Shared => "a shared buffer in a block".into(),
    });
    Some((made, data.cast()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `victory-texture-blocks` keeps at least four empty, never more than
    /// the pool keeps.
    #[test]
    fn the_block_thread_keeps_a_few_ahead_within_the_spares() {
        assert_eq!(blocks_wanted(12, 64), 12);
        assert_eq!(blocks_wanted(0, 64), BLOCKS_LEAST);
        assert_eq!(blocks_wanted(12, 2), 2);
        assert_eq!(blocks_wanted(1, 3), 3);
    }

    /// With `VICTORY_TEXTURE_BLOCK_THREAD=1` and a card: a take that finds
    /// no room rings the thread, which makes blocks, and a texture is cut
    /// from one of them soon after without its thread making any.
    #[test]
    fn the_block_thread_makes_the_blocks_textures_are_cut_from() {
        let Some(host) = crate::host::host() else { return };
        if !block_thread() {
            return;
        }
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Width: 256,
            Height: 256,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            ..Default::default()
        };
        let began = std::time::Instant::now();
        loop {
            // SAFETY: a valid description.
            let cut = unsafe { texture(&host.device, &desc, D3D12_RESOURCE_STATE_COMMON) };
            if cut.is_some() {
                break;
            }
            assert!(began.elapsed() < std::time::Duration::from_secs(5), "no block made by the thread");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(empties(&lock(&TEXTURES)) > 0 || lock(&TEXTURES).blocks() > 0);
    }

    /// With `hold_gone`, the blocks a reclaim lets go are handed out, not
    /// dropped under the lock; without it, dropped as before.
    #[test]
    fn a_held_pool_hands_out_the_blocks_it_lets_go() {
        for hold in [false, true] {
            let block = std::sync::Arc::new(());
            let mut pool: Pool<std::sync::Arc<()>> = Pool::new(8);
            pool.hold_gone = hold;
            let (piece, _, _) = pool.take(1 << 20, 1, 0, || Some(std::sync::Arc::clone(&block))).expect("a piece");
            pool.give(piece, 0);
            pool.reclaim(REST, 0, true);
            assert_eq!(pool.blocks(), 0);
            assert_eq!(pool.gone.len(), usize::from(hold));
            assert_eq!(std::sync::Arc::strong_count(&block), 1 + usize::from(hold));
        }
    }

    /// What size of buffer is cut from a block, and the piece it takes.
    #[test]
    fn buffers_up_to_four_mib_are_cut_at_multiples_of_64_kib() {
        assert_eq!(buffer_piece(0), Some(64 << 10));
        assert_eq!(buffer_piece(1), Some(64 << 10));
        assert_eq!(buffer_piece(64 << 10), Some(64 << 10));
        assert_eq!(buffer_piece((64 << 10) + 1), Some(128 << 10));
        assert_eq!(buffer_piece(BUFFER_MOST), Some(BUFFER_MOST));
        assert_eq!(buffer_piece(BUFFER_MOST + 1), None);
        assert_eq!(buffer_piece(64 << 20), None);
    }

    /// Upload and shared buffers cut from blocks: apart, mapped, all zeros
    /// (also a piece used again after its rest), known as cut from a block.
    /// (With `VICTORY_BUFFER_POOL=1` and a card only.)
    #[test]
    fn buffers_cut_from_blocks_start_as_zeros_and_come_back() {
        let Some(host) = crate::host::host() else { return };
        if !buffers_on() {
            return;
        }
        for kind in [CpuBuffer::Upload, CpuBuffer::Shared] {
            let a = cpu_buffer(&host.device, kind, 1000).expect("a buffer");
            let b = cpu_buffer(&host.device, kind, 70_000).expect("a buffer");
            assert!(in_block(&a.0) && in_block(&b.0));
            // SAFETY: plain queries of live buffers.
            let (at_a, at_b) = unsafe { (a.0.GetGPUVirtualAddress(), b.0.GetGPUVirtualAddress()) };
            assert!(at_a.abs_diff(at_b) >= 1000, "{kind:?}: the buffers overlap");
            // SAFETY: the buffers' mappings.
            unsafe {
                assert!(std::slice::from_raw_parts(a.1, 1000).iter().all(|&x| x == 0));
                std::ptr::write_bytes(a.1, 0xab, 1000);
                std::ptr::write_bytes(b.1, 0xcd, 70_000);
            }
            drop((a, b));
            // Past the rest, the same pieces are cut again, and zeroed.
            std::thread::sleep(std::time::Duration::from_millis(REST + 100));
            let mut again = Vec::new();
            for _ in 0..4 {
                let c = cpu_buffer(&host.device, kind, 70_000).expect("a buffer");
                // SAFETY: the buffer's mapping.
                assert!(unsafe { std::slice::from_raw_parts(c.1, 70_000) }.iter().all(|&x| x == 0), "{kind:?}: not zeros");
                again.push(c);
            }
        }
    }

    #[test]
    fn ranges_are_cut_aligned_and_join_again() {
        let mut r = Ranges::new(1024);
        assert_eq!(r.take(100, 64), Some(0));
        assert_eq!(r.take(100, 64), Some(128));
        assert_eq!(r.take(28, 4), Some(100));
        assert_eq!(r.take(2000, 4), None);
        r.give(128, 100);
        r.give(0, 100);
        assert!(!r.whole(1024));
        r.give(100, 28);
        assert!(r.whole(1024));
    }

    #[test]
    fn empty_blocks_stay_while_the_card_has_room() {
        assert_eq!(spares(Some([100, 84, 0, 0]), true), SPARE_KEPT);
        assert_eq!(spares(Some([100, 85, 0, 0]), true), SPARE);
        assert_eq!(spares(Some([100, 10, 0, 0]), false), SPARE);
        assert_eq!(spares(None, true), SPARE);
    }

    #[test]
    fn a_failed_block_is_not_retried_for_each_small_upload() {
        let mut pool: Pool<u32> = Pool::new(2);
        let calls = std::cell::Cell::new(0);
        let refuse = || { calls.set(calls.get() + 1); None };
        for size in [704008, 179720, 179720, 24080, 48648, 524288, 89616, 351760] {
            assert!(pool.take(size, 512, 12, refuse).is_none());
        }
        assert_eq!(calls.get(), 1);
        assert!(pool.take(24080, 512, 1012, || Some(1)).is_some());
    }

    #[test]
    fn budget_tick_reclaims_idle_blocks_without_reusing_live_or_resting_pieces() {
        let mut pool = Pool::new(STAGING_KEPT);
        let (held, _, _) = pool.take(BLOCK / 2, 1, 0, || Some(1)).unwrap();
        let (resting, _, _) = pool.take(BLOCK / 2, 1, 0, || Some(2)).unwrap();
        pool.give(resting, 0);
        for i in 0..12 {
            pool.add(i, 0);
        }
        pool.reclaim(0, SPARE_KEPT, true);
        assert_eq!(pool.blocks(), STAGING_KEPT + 1);
        pool.reclaim(REST - 1, SPARE, true);
        assert_eq!(pool.blocks(), SPARE + 1);
        assert_eq!(pool.resting.len(), 1);
        pool.reclaim(REST, 0, true);
        assert_eq!(pool.blocks(), 1);
        assert!(pool.resting.is_empty());
        pool.give(held, REST);
        pool.reclaim(2 * REST, 0, true);
        assert_eq!(pool.blocks(), 0);
    }

    #[test]
    fn pieces_never_overlap_and_rest_before_they_are_used_again() {
        let mut pool: Pool<u32> = Pool::new(SPARE);
        let made = std::cell::Cell::new(0);
        let make = || {
            made.set(made.get() + 1);
            Some(made.get())
        };
        let mut taken: Vec<Piece> = Vec::new();
        for i in 0..500u64 {
            let (p, _, again) = pool.take(65536 * (1 + i % 7), 65536, 0, make).unwrap();
            assert!(!again);
            assert!(taken.iter().all(|q| q.block != p.block || p.start + p.len <= q.start || q.start + q.len <= p.start));
            taken.push(p);
        }
        let blocks = pool.blocks();
        assert!(blocks >= 2);
        // Given back at 10: not used again before 10 + REST.
        let first = taken[0];
        pool.give(first, 10);
        let (p, _, _) = pool.take(first.len, 65536, 10 + REST - 1, make).unwrap();
        assert_ne!((p.block, p.start), (first.block, first.start));
        let (q, _, again) = pool.take(first.len, 65536, 10 + REST, make).unwrap();
        assert_eq!((q.block, q.start, again), (first.block, first.start, true));
        // Everything back: the empty blocks past the spare ones go.
        for p in taken.drain(1..).chain([p, q]) {
            pool.give(p, 20);
        }
        pool.rested(20 + REST);
        assert_eq!(pool.blocks(), blocks.min(SPARE));
        // Too big for a block, and a block that cannot be made.
        assert!(pool.take(BLOCK, 65536, 30, make).is_none());
        let mut none: Pool<u32> = Pool::new(SPARE);
        assert!(none.take(65536, 65536, 0, || None).is_none());
    }

    #[test]
    fn draining_blocks_are_cut_from_last_and_go_when_empty() {
        let mut pool: Pool<u32> = Pool::new(SPARE);
        let made = std::cell::Cell::new(0);
        let make = || {
            made.set(made.get() + 1);
            Some(made.get())
        };
        // Two blocks: the first full, the second with one small piece left.
        assert!(pool.take(BLOCK, 65536, 0, make).is_none(), "a whole block is not pooled without blocks made ahead");
        let a = pool.take(BLOCK / 2, 65536, 0, make).unwrap().0;
        let b = pool.take(BLOCK / 2, 65536, 0, make).unwrap().0;
        let small = pool.take(BLOCK / 2, 65536, 0, make).unwrap().0;
        let half = pool.take(BLOCK / 2, 65536, 0, make).unwrap().0;
        assert_eq!((a.block, b.block, small.block, half.block), (0, 0, 1, 1));
        pool.give(half, 0);
        pool.rested(REST);
        let c = pool.take(BLOCK / 4, 65536, REST, make).unwrap().0;
        assert_eq!(c.block, 1);
        pool.give(c, REST);
        pool.give(small, REST);
        let tiny = pool.take(65536, 65536, REST, make).unwrap().0;
        assert_eq!(tiny.block, 1);
        pool.rested(2 * REST);
        // Block 1 holds 64 KiB: drained; block 0 is full, not.
        assert_eq!(pool.mark_draining(4, BLOCK / 4), (1, 65536));
        assert!(pool.is_draining(1) && !pool.is_draining(0));
        let census = pool.census();
        assert_eq!((census[0], census[1], census[4], census[7]), (2, BLOCK + 65536, 1, 1));
        // No other block has room: the draining one, before a new block.
        let n = made.get();
        let d = pool.take(BLOCK / 4, 65536, 2 * REST, make).unwrap().0;
        assert_eq!((d.block, made.get()), (1, n));
        pool.give(d, 2 * REST);
        // With room elsewhere, not from the draining block.
        pool.give(a, 2 * REST);
        pool.rested(3 * REST);
        let e = pool.take(BLOCK / 4, 65536, 3 * REST, make).unwrap().0;
        assert_eq!(e.block, 0);
        // Its last piece back: no more draining, and it goes past the spares.
        pool.give(tiny, 3 * REST);
        let before = DRAINED[2].load(Ordering::Relaxed);
        pool.rested(4 * REST);
        assert!(!pool.is_draining(1));
        assert!(DRAINED[2].load(Ordering::Relaxed) > before);
        // None wanted: none marked.
        assert_eq!(pool.mark_draining(0, BLOCK / 4), (0, 0));
    }

    #[test]
    fn pieces_come_from_blocks_older_than_the_lists_that_run() {
        let mut pool: Pool<u64> = Pool::new(SPARE_KEPT);
        let made = std::cell::Cell::new(0);
        let make = || {
            made.set(made.get() + 1);
            Some(made.get())
        };
        let at = |started, finished| Ripe { started, finished, ahead: 2 };
        let take = |pool: &mut Pool<u64>, ripe| pool.take_ripe(BLOCK / 2, 65536, 0, ripe, make).unwrap().1;
        // Nothing runs: at once, and two are made ahead, one a time.
        assert_eq!(take(&mut pool, at(0, 0)), 1);
        assert_eq!(made.get(), 2);
        assert_eq!(take(&mut pool, at(0, 0)), 1);
        assert_eq!(made.get(), 3);
        // 4 is made while submission 5 runs.
        assert_eq!(take(&mut pool, at(5, 4)), 2);
        assert_eq!(made.get(), 4);
        assert_eq!(take(&mut pool, at(5, 4)), 2);
        assert_eq!(take(&mut pool, at(6, 4)), 3);
        assert_eq!(take(&mut pool, at(6, 4)), 3);
        // None is ripe: the oldest; it is ripe once 5 has finished.
        assert_eq!(take(&mut pool, at(7, 4)), 4);
        assert_eq!(take(&mut pool, at(7, 5)), 4);
        // A whole block's worth too.
        assert_eq!(pool.take_ripe(BLOCK, 65536, 0, at(8, 7), make).unwrap().1, 5);
        // Off: the first with room, whatever its age, none ahead.
        let n = made.get();
        assert_eq!(pool.take(BLOCK / 2, 65536, 0, make).unwrap().1, 6);
        assert_eq!(made.get(), n);
        assert!(pool.take(BLOCK, 65536, 0, make).is_none());
    }

    /// A pooled texture's piece comes back when its last reference goes, not
    /// before; its staging piece begins as zeros.
    #[test]
    fn a_texture_gives_its_piece_back_when_it_goes() {
        let Some(host) = crate::host::host() else { return };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Width: 256,
            Height: 256,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            ..Default::default()
        };
        // SAFETY: a valid description.
        let Some(t) = (unsafe { texture(&host.device, &desc, D3D12_RESOURCE_STATE_COMMON) }) else { return };
        assert_eq!(crate::host::references(&t), 1, "what the texture holds is no reference to it");
        let resting = || lock(&TEXTURES).resting.len();
        let before = resting();
        let other = t.clone();
        drop(t);
        assert_eq!(resting(), before);
        drop(other);
        assert_eq!(resting(), before + 1);
        // SAFETY: the piece's mapping, 4096 bytes of it ours.
        unsafe {
            let (piece, _, at, data) = staging(host, 4096).unwrap();
            assert!(at.is_multiple_of(512));
            std::ptr::write_bytes(data, 0xAB, 4096);
            drop(piece);
            lock(&STAGING).rested(u64::MAX);
            let (_piece, _, _, data) = staging(host, 4096).unwrap();
            assert!((0..4096).all(|i| *data.add(i) == 0));
            // SAFETY: plain query.
            assert!(host.device.GetDeviceRemovedReason().is_ok());
        }
    }
}
