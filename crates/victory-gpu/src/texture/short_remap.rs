//! `VICTORY_SHORT_REMAP_LOCK=1` (off by default): a texture read through
//! the copy (a partly mapped one, or every one with `VICTORY_TEXTURE_COPY=1`)
//! lets the runtime remap title memory again as soon as its bytes are in the
//! thread's read buffer, not after they are hashed and detiled. A read in
//! place keeps the guard throughout: it needs the pages to stay mapped.
//! What is hashed and detiled is the copy in the read buffer, never title
//! memory, so the bytes are the same either way.
//! `VICTORY_AB` (or `VICTORY_LIVE_ALT`) may name it: on and off while the
//! title runs (`crate::ab`), for an A/B in one run.
//!
//! Source: kylepelham/El1xr PR #113 (Kyle Pelham), from 3cvil/M1LK
//! b29bcff394, after stellarr-studio/kelvaroq native/patches/0004.

use crate::watch::Reading;

/// Whether the switch is on now.
pub(super) fn on() -> bool {
    static ASKED: std::sync::OnceLock<Result<bool, u32>> = std::sync::OnceLock::new();
    let asked = ASKED.get_or_init(|| {
        if let Some(bit) = crate::ab::bit("VICTORY_SHORT_REMAP_LOCK") {
            tracing::info!(
                "VICTORY_SHORT_REMAP_LOCK: switched while the title runs (VICTORY_AB): texture reads through the copy release the remap lock right after the copy when on"
            );
            return Err(bit);
        }
        let on = std::env::var_os("VICTORY_SHORT_REMAP_LOCK").is_some_and(|v| v == "1");
        if on {
            tracing::info!(
                "VICTORY_SHORT_REMAP_LOCK=1: texture reads through the copy release the remap lock right after the copy"
            );
        }
        Ok(on)
    });
    match *asked {
        Ok(on) => on,
        Err(bit) => crate::ab::on(bit),
    }
}

/// The `size` bytes of title memory at `memory`, copied into this thread's
/// read buffer through the kernel (pages not there read as zeros), given to
/// `consume`. The caller holds `mapping`: with `release_early` it is dropped
/// once the bytes are copied, before `consume` hashes and detiles them;
/// otherwise it is held until `consume` returns, as before.
pub(super) fn with_title_snapshot<R>(
    memory: *const u8,
    size: usize,
    mapping: Reading,
    release_early: bool,
    consume: impl FnOnce(&[u8]) -> R,
) -> R {
    super::with_title(memory, size, |read| {
        if release_early {
            drop(mapping);
            consume(read)
        } else {
            let result = consume(read);
            drop(mapping);
            result
        }
    })
}

#[cfg(test)]
mod tests {
    use super::with_title_snapshot;
    use crate::watch::{title_memory_reading, title_memory_remapping};

    /// The bytes are those of the memory at the copy, partly mapped or not;
    /// with the switch a remap goes on while the bytes are consumed (and
    /// does not change them), without it the remap waits until they are.
    #[test]
    fn the_copy_is_consumed_and_the_remap_waits_only_without_the_switch() {
        use std::sync::mpsc;
        use std::time::Duration;
        use windows_sys::Win32::System::Memory::*;

        const PAGE: usize = 4096;
        for (sparse, release_early) in [(false, false), (false, true), (true, false), (true, true)] {
            // SAFETY: this test owns the whole allocation; only the writer
            // remaps it, and only while it holds the remapping guard.
            unsafe {
                let base = VirtualAlloc(std::ptr::null(), 3 * PAGE, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE)
                    .cast::<u8>();
                assert!(!base.is_null());
                base.write_bytes(0x31, 3 * PAGE);
                if sparse {
                    assert_ne!(VirtualFree(base.add(PAGE).cast(), PAGE, MEM_DECOMMIT), 0);
                }
                let mut original = vec![0x31; 3 * PAGE];
                if sparse {
                    original[PAGE..2 * PAGE].fill(0);
                }
                let address = base as usize;
                std::thread::scope(|scope| {
                    let (begin, wait) = mpsc::channel();
                    let (attempt, started) = mpsc::channel();
                    let (finished, remapped) = mpsc::channel();
                    scope.spawn(move || {
                        wait.recv().unwrap();
                        attempt.send(()).unwrap();
                        let _changing = title_memory_remapping();
                        let first = address as *mut u8;
                        assert_ne!(VirtualFree(first.cast(), PAGE, MEM_DECOMMIT), 0);
                        assert_eq!(VirtualAlloc(first.cast(), PAGE, MEM_COMMIT, PAGE_READWRITE), first.cast());
                        first.write_bytes(0xa7, PAGE);
                        let _ = finished.send(());
                    });
                    with_title_snapshot(base, 3 * PAGE, title_memory_reading(), release_early, |copied| {
                        begin.send(()).unwrap();
                        started.recv().unwrap();
                        if release_early {
                            remapped
                                .recv_timeout(Duration::from_secs(5))
                                .expect("the remap waited for the consumer");
                        } else {
                            std::thread::sleep(Duration::from_millis(20));
                            assert_eq!(remapped.try_recv(), Err(mpsc::TryRecvError::Empty));
                        }
                        // The copy, not title memory: the remap did not
                        // reach it.
                        assert_eq!(copied, original);
                    });
                });
                original[..PAGE].fill(0xa7);
                with_title_snapshot(base, 3 * PAGE, title_memory_reading(), release_early, |fresh| {
                    assert_eq!(fresh, original)
                });
                assert_ne!(VirtualFree(base.cast(), 0, MEM_RELEASE), 0);
            }
        }
    }
}
