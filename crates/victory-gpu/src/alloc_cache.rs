//! `VICTORY_ALLOC_CACHE=1` (off by default): blocks of 16 bytes to 512 KiB
//! (alignment 16 or less) are kept, once let go, in lists of their size
//! class, a thread's own first (fiber-local storage) and then batches shared
//! by all threads, in front of the system's allocator. The title's 20-odd
//! threads of ours stood in the Windows process heap's lock, and a list
//! grown within its class does not move.
//!
//! Port of SantosFktt/VIctory-RTX5070 da982bd3 (`copystat.rs`
//! `alloc_cache`). Their measurement, with `VICTORY_WATCH_FAST` at 720p in
//! their prison scene: 37.8-38.4 ms -> 35.4 ms a frame (both together).
//!
//! Read once, at the first allocation, through the environment's own call
//! (nothing allocated), so that every block is made and let go by the same
//! rule: not something `VICTORY_AB` can switch. In diagnostics builds the
//! counting allocator (`crate::copystat`) goes through here; in the others
//! [`Cached`] is the program's allocator, the system's unchanged with the
//! switch off.

use std::alloc::{GlobalAlloc, Layout, System};
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};

/// The largest block kept.
const MOST: usize = 1 << 19;
/// Size classes: 16 bytes to 512 KiB, by doubles.
const CLASSES: usize = 16;
/// The most blocks of a class a thread keeps; at that, half go to the
/// shared batches.
const LOCAL_CAP: [u16; CLASSES] = [128, 128, 128, 128, 128, 128, 64, 64, 64, 64, 24, 24, 24, 8, 8, 8];
/// Shared batches a class keeps.
const GLOBAL_BATCHES: usize = 32;

/// 0: not read yet, 1: off, 2: on.
static MODE: AtomicU8 = AtomicU8::new(0);
/// The fiber-local slot: 0 not made, 1 being made, 2 made.
static FLS_STATE: AtomicU32 = AtomicU32::new(0);
static FLS_SLOT: AtomicU32 = AtomicU32::new(u32::MAX);
const FLS_OUT_OF_INDEXES: u32 = u32::MAX;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetEnvironmentVariableW(name: *const u16, buf: *mut u16, size: u32) -> u32;
    fn FlsAlloc(callback: Option<unsafe extern "system" fn(*mut c_void)>) -> u32;
    fn FlsGetValue(index: u32) -> *mut c_void;
    fn FlsSetValue(index: u32, data: *const c_void) -> i32;
}

/// A thread's lists: the first free block of each class (each holds the
/// next one at its start), and how many.
#[repr(C)]
struct LocalCache {
    heads: [*mut u8; CLASSES],
    counts: [u16; CLASSES],
}

/// Blocks of one class handed between threads: a list as in [`LocalCache`].
#[derive(Clone, Copy)]
struct Batch {
    head: *mut u8,
    count: u16,
}

// SAFETY: the blocks are the cache's alone, used under the class's lock or
// by the thread that took them.
unsafe impl Send for Batch {}

struct GlobalBin {
    batches: [Batch; GLOBAL_BATCHES],
    len: usize,
}

static GLOBAL_BINS: [Mutex<GlobalBin>; CLASSES] = [const {
    Mutex::new(GlobalBin {
        batches: [Batch {
            head: std::ptr::null_mut(),
            count: 0,
        }; GLOBAL_BATCHES],
        len: 0,
    })
}; CLASSES];

/// Whether the cache is on.
#[inline]
pub(crate) fn on() -> bool {
    match MODE.load(Ordering::Relaxed) {
        0 => init(),
        m => m == 2,
    }
}

#[cold]
fn init() -> bool {
    const NAME: [u16; 20] = {
        let text = *b"VICTORY_ALLOC_CACHE\0";
        let mut wide = [0u16; 20];
        let mut i = 0;
        while i < 20 {
            wide[i] = text[i] as u16;
            i += 1;
        }
        wide
    };
    let mut buf = [0u16; 16];
    // SAFETY: a name that ends in 0, and room for `buf.len()` characters.
    let n = unsafe { GetEnvironmentVariableW(NAME.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
    let enabled = n > 0 && !(n == 1 && buf[0] == u16::from(b'0'));
    let want = if enabled { 2 } else { 1 };
    match MODE.compare_exchange(0, want, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => want == 2,
        Err(was) => was == 2,
    }
}

/// Whether a block of this size and alignment is kept.
#[inline]
pub(crate) fn eligible(size: usize, align: usize) -> bool {
    align <= 16 && size > 0 && size <= MOST
}

#[inline]
pub(crate) fn class_of(size: usize) -> usize {
    if size <= 16 { 0 } else { (usize::BITS - (size - 1).leading_zeros()) as usize - 4 }
}

#[inline]
fn class_layout(c: usize) -> Layout {
    // SAFETY: 16 << c is a power of two of at most 512 KiB; 16 is one too.
    unsafe { Layout::from_size_align_unchecked(16usize << c, 16) }
}

fn fls_slot() -> u32 {
    if FLS_STATE.load(Ordering::Acquire) == 2 {
        return FLS_SLOT.load(Ordering::Relaxed);
    }
    loop {
        match FLS_STATE.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => {
                // SAFETY: a callback of the right kind.
                let slot = unsafe { FlsAlloc(Some(fls_cleanup)) };
                FLS_SLOT.store(slot, Ordering::Relaxed);
                FLS_STATE.store(2, Ordering::Release);
                return slot;
            }
            Err(2) => return FLS_SLOT.load(Ordering::Relaxed),
            Err(_) => std::hint::spin_loop(),
        }
    }
}

/// A thread ends: its lists go back to the system.
unsafe extern "system" fn fls_cleanup(raw: *mut c_void) {
    if raw.is_null() {
        return;
    }
    let cache = raw.cast::<LocalCache>();
    // SAFETY: the ending thread's own lists, of blocks made by `System`
    // with their class's layout.
    unsafe {
        for c in 0..CLASSES {
            let layout = class_layout(c);
            let mut at = (*cache).heads[c];
            while !at.is_null() {
                let next = at.cast::<*mut u8>().read();
                System.dealloc(at, layout);
                at = next;
            }
        }
        System.dealloc(raw.cast(), Layout::new::<LocalCache>());
    }
}

#[inline]
fn local(make: bool) -> *mut LocalCache {
    let slot = fls_slot();
    if slot == FLS_OUT_OF_INDEXES {
        return std::ptr::null_mut();
    }
    // SAFETY: a slot of ours.
    let at = unsafe { FlsGetValue(slot) }.cast::<LocalCache>();
    if !at.is_null() || !make {
        return at;
    }
    // SAFETY: from the system, not through us.
    let fresh = unsafe { System.alloc_zeroed(Layout::new::<LocalCache>()) }.cast::<LocalCache>();
    if fresh.is_null() {
        return fresh;
    }
    // SAFETY: a slot of ours, and a live block.
    if unsafe { FlsSetValue(slot, fresh.cast()) } == 0 {
        // SAFETY: made just above.
        unsafe { System.dealloc(fresh.cast(), Layout::new::<LocalCache>()) };
        return std::ptr::null_mut();
    }
    fresh
}

/// A block of class `c` (of `size` asked bytes, zeroed if asked).
///
/// # Safety
/// `c` is `class_of(size)` of an `eligible` size.
#[inline]
pub(crate) unsafe fn alloc_class(c: usize, zeroed: bool, size: usize) -> *mut u8 {
    let cache = local(true);
    if !cache.is_null() {
        // SAFETY: the thread's own lists.
        let cache = unsafe { &mut *cache };
        let head = cache.heads[c];
        if !head.is_null() {
            // SAFETY: a free block holds the next at its start.
            cache.heads[c] = unsafe { head.cast::<*mut u8>().read() };
            cache.counts[c] -= 1;
            if zeroed {
                // SAFETY: `size` is within the class's.
                unsafe { std::ptr::write_bytes(head, 0, size) };
            }
            return head;
        }
        if let Ok(mut bin) = GLOBAL_BINS[c].try_lock()
            && bin.len > 0
        {
            bin.len -= 1;
            let batch = bin.batches[bin.len];
            drop(bin);
            // SAFETY: a list of `batch.count` blocks (at least one); the
            // thread's own list of the class is empty.
            cache.heads[c] = unsafe { batch.head.cast::<*mut u8>().read() };
            cache.counts[c] = batch.count - 1;
            if zeroed {
                // SAFETY: as above.
                unsafe { std::ptr::write_bytes(batch.head, 0, size) };
            }
            return batch.head;
        }
    }
    // SAFETY: a layout of non-zero size.
    unsafe { if zeroed { System.alloc_zeroed(class_layout(c)) } else { System.alloc(class_layout(c)) } }
}

/// A block of class `c` is let go.
///
/// # Safety
/// Made by [`alloc_class`] of the same class.
#[inline]
pub(crate) unsafe fn dealloc_class(at: *mut u8, c: usize) {
    let cache = local(false);
    if cache.is_null() {
        // SAFETY: made by `System` with this layout (or kept from such).
        unsafe { System.dealloc(at, class_layout(c)) };
        return;
    }
    // SAFETY: the thread's own lists; a block of 16 bytes or more.
    let cache = unsafe { &mut *cache };
    unsafe { at.cast::<*mut u8>().write(cache.heads[c]) };
    cache.heads[c] = at;
    cache.counts[c] += 1;
    if cache.counts[c] >= LOCAL_CAP[c] {
        // SAFETY: as many as counted.
        unsafe { flush_half(cache, c) };
    }
}

/// Half of a thread's list of class `c` goes to the shared batches (or
/// back to the system if they are full or busy).
#[cold]
unsafe fn flush_half(cache: &mut LocalCache, c: usize) {
    let half = LOCAL_CAP[c] / 2;
    let first = cache.heads[c];
    let mut at = first;
    for _ in 1..half {
        // SAFETY: the list holds at least `2 * half` blocks.
        at = unsafe { at.cast::<*mut u8>().read() };
    }
    // SAFETY: as above.
    let rest = unsafe { at.cast::<*mut u8>().read() };
    unsafe { at.cast::<*mut u8>().write(std::ptr::null_mut()) };
    cache.heads[c] = rest;
    cache.counts[c] -= half;
    if let Ok(mut bin) = GLOBAL_BINS[c].try_lock()
        && bin.len < GLOBAL_BATCHES
    {
        let n = bin.len;
        bin.batches[n] = Batch { head: first, count: half };
        bin.len = n + 1;
        return;
    }
    let layout = class_layout(c);
    let mut at = first;
    while !at.is_null() {
        // SAFETY: the list cut off above, of blocks made by `System`.
        let next = unsafe { at.cast::<*mut u8>().read() };
        unsafe { System.dealloc(at, layout) };
        at = next;
    }
}

/// [`GlobalAlloc::realloc`] through the cache, if either side is kept:
/// `None` if neither is (the caller asks the system). `Some(null)` if no
/// memory.
///
/// # Safety
/// [`GlobalAlloc::realloc`]'s, with the cache on.
#[inline]
pub(crate) unsafe fn realloc(at: *mut u8, layout: Layout, size: usize) -> Option<*mut u8> {
    let was = eligible(layout.size(), layout.align());
    let will = eligible(size, layout.align());
    if !was && !will {
        return None;
    }
    if was && will && class_of(layout.size()) == class_of(size) {
        // Within its class: it stays where it is.
        return Some(at);
    }
    let moved = if will {
        // SAFETY: an eligible size.
        unsafe { alloc_class(class_of(size), false, size) }
    } else {
        let Ok(new) = Layout::from_size_align(size, layout.align()) else {
            return Some(std::ptr::null_mut());
        };
        // SAFETY: a layout of non-zero size.
        unsafe { System.alloc(new) }
    };
    if !moved.is_null() {
        // SAFETY: both live, apart, each at least the bytes copied.
        unsafe { std::ptr::copy_nonoverlapping(at, moved, layout.size().min(size)) };
        if was {
            // SAFETY: made by `alloc_class` of its class.
            unsafe { dealloc_class(at, class_of(layout.size())) };
        } else {
            // SAFETY: made by the system with `layout`.
            unsafe { System.dealloc(at, layout) };
        }
    }
    Some(moved)
}

/// The program's allocator outside diagnostics builds: the system's, through
/// the cache when it is on.
#[cfg(not(feature = "diagnostics"))]
struct Cached;

// SAFETY: every block goes to the system's allocator, or (with the cache on)
// is kept in its class and given out again for a size of that class; a
// block is made and let go by the same rule (`on` is read once).
#[cfg(not(feature = "diagnostics"))]
unsafe impl GlobalAlloc for Cached {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if on() && eligible(layout.size(), layout.align()) {
            // SAFETY: an eligible layout.
            return unsafe { alloc_class(class_of(layout.size()), false, layout.size()) };
        }
        // SAFETY: the caller's contract, passed on.
        unsafe { System.alloc(layout) }
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if on() && eligible(layout.size(), layout.align()) {
            // SAFETY: an eligible layout.
            return unsafe { alloc_class(class_of(layout.size()), true, layout.size()) };
        }
        // SAFETY: the caller's contract, passed on.
        unsafe { System.alloc_zeroed(layout) }
    }

    #[inline]
    unsafe fn dealloc(&self, at: *mut u8, layout: Layout) {
        if on() && eligible(layout.size(), layout.align()) {
            // SAFETY: made by `alloc_class` of its class.
            unsafe { dealloc_class(at, class_of(layout.size())) };
            return;
        }
        // SAFETY: the caller's contract, passed on.
        unsafe { System.dealloc(at, layout) }
    }

    #[inline]
    unsafe fn realloc(&self, at: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if on()
            // SAFETY: the caller's contract, passed on.
            && let Some(moved) = unsafe { realloc(at, layout, size) }
        {
            return moved;
        }
        // SAFETY: the caller's contract, passed on.
        unsafe { System.realloc(at, layout, size) }
    }
}

#[cfg(not(feature = "diagnostics"))]
#[global_allocator]
static HEAP: Cached = Cached;

#[cfg(test)]
mod tests {
    use super::{class_layout, class_of, eligible};

    /// A size goes to the least class that holds it.
    #[test]
    fn sizes_take_the_least_class_that_holds_them() {
        assert_eq!(class_of(1), 0);
        assert_eq!(class_of(16), 0);
        assert_eq!(class_of(17), 1);
        assert_eq!(class_of(32), 1);
        assert_eq!(class_of(33), 2);
        assert_eq!(class_of(1 << 19), 15);
        for size in 1..=(1usize << 19) {
            if size.is_power_of_two() || size % 977 == 0 {
                let c = class_of(size);
                assert!(class_layout(c).size() >= size, "{size}");
                assert!(c == 0 || class_layout(c - 1).size() < size, "{size}");
            }
        }
        assert!(eligible(1 << 19, 16) && !eligible((1 << 19) + 1, 16) && !eligible(64, 32) && !eligible(0, 8));
    }
}
