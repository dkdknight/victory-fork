//! Diagnostics (`VICTORY_IMAGE_WATCH`; off by default): words of the title's
//! image, logged once a second, for what a launch's own counters say (how
//! much of its streaming memory it uses, whether an allocation failed) where
//! two launches end up with different pictures.
//!
//! `VICTORY_IMAGE_WATCH=used=98d26d8:8,failed=98d26d0:1`: a name, the offset
//! in the image (hexadecimal) and the bytes (1, 2, 4 or 8).
//!
//! `VICTORY_IMAGE_HOLD=hour=1234:4=12,minute=1238:4=0` (also off by default)
//! writes such words back ten times a second, as a trainer does: a value the
//! title keeps moving (a world clock) held still, so that launches compared
//! against each other see the same scene. Words as above, then `=` and the
//! value (decimal, or hexadecimal with `0x`). Nothing of the title's is in
//! this file: the offsets come from the user's own settings.

use std::time::Duration;

struct Word {
    name: String,
    offset: usize,
    bytes: usize,
}

/// The words `text` names; what cannot be read of it is left out.
fn words(text: &str) -> Vec<Word> {
    text.split(',')
        .filter_map(|part| {
            let (name, rest) = part.trim().split_once('=')?;
            let (offset, bytes) = rest.split_once(':').unwrap_or((rest, "8"));
            let offset = usize::from_str_radix(offset.trim().trim_start_matches("0x"), 16).ok()?;
            let bytes = bytes.trim().parse().ok().filter(|n| matches!(n, 1 | 2 | 4 | 8))?;
            Some(Word { name: name.trim().to_owned(), offset, bytes })
        })
        .collect()
}

/// The words and values `text` names for [`start`]'s hold.
fn held(text: &str) -> Vec<(Word, u64)> {
    text.split(',')
        .filter_map(|part| {
            let (word, value) = part.trim().rsplit_once('=')?;
            let value = value.trim();
            let value = match value.strip_prefix("0x") {
                Some(hex) => u64::from_str_radix(hex, 16).ok()?,
                None => value.parse().ok()?,
            };
            Some((words(word).pop()?, value))
        })
        .collect()
}

/// Writes `value` into `word` of the image at `base`; false if it cannot be written.
fn write(base: usize, word: &Word, value: u64) -> bool {
    use windows_sys::Win32::System::Diagnostics::Debug::WriteProcessMemory;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let bytes = value.to_le_bytes();
    // SAFETY: the kernel copies at most eight bytes from `bytes`, and fails
    // where nothing is writable; the title reads the word as before.
    let ok = unsafe {
        WriteProcessMemory(GetCurrentProcess(), (base + word.offset) as *mut _, bytes.as_ptr().cast(), word.bytes, std::ptr::null_mut())
    };
    ok != 0
}

/// Holds the words `VICTORY_IMAGE_HOLD` names at their values, from a thread
/// of its own, ten times a second.
fn hold(base: usize) {
    let Ok(text) = std::env::var("VICTORY_IMAGE_HOLD") else {
        return;
    };
    let held = held(&text);
    if base == 0 || held.is_empty() {
        return;
    }
    let names: Vec<String> = held.iter().map(|(word, value)| format!("{}={value}", word.name)).collect();
    tracing::info!("the title's image: holding {} (VICTORY_IMAGE_HOLD)", names.join(" "));
    let _ = std::thread::Builder::new().name("victory-image-hold".into()).spawn(move || {
        let mut failed = false;
        loop {
            for (word, value) in &held {
                if !write(base, word, *value) && !failed {
                    failed = true;
                    tracing::warn!(word = %word.name, "the title's image: a held word cannot be written (VICTORY_IMAGE_HOLD)");
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    });
}

/// `word` of the image at `base`; `None` if it cannot be read.
fn read(base: usize, word: &Word) -> Option<u64> {
    use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut value = [0u8; 8];
    // SAFETY: the kernel copies at most eight bytes into `value`, and fails
    // where there is nothing to read.
    let ok = unsafe {
        ReadProcessMemory(
            GetCurrentProcess(),
            (base + word.offset) as *const _,
            value.as_mut_ptr().cast(),
            word.bytes,
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then(|| u64::from_le_bytes(value))
}

/// Starts watching the image at `base`, if asked for.
pub(crate) fn start(base: usize) {
    static HOLDING: std::sync::Once = std::sync::Once::new();
    HOLDING.call_once(|| hold(base));
    let Ok(text) = std::env::var("VICTORY_IMAGE_WATCH") else {
        return;
    };
    let words = words(&text);
    if base == 0 || words.is_empty() {
        return;
    }
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let _ = std::thread::Builder::new().name("victory-image-watch".into()).spawn(move || {
            let mut last = Vec::new();
            loop {
                std::thread::sleep(Duration::from_secs(1));
                let now: Vec<Option<u64>> = words.iter().map(|word| read(base, word)).collect();
                if now == last {
                    continue;
                }
                let values: Vec<String> = words
                    .iter()
                    .zip(&now)
                    .map(|(word, value)| match value {
                        Some(value) => format!("{}={value:#x}", word.name),
                        None => format!("{}=?", word.name),
                    })
                    .collect();
                tracing::info!("the title's image: {}", values.join(" "));
                last = now;
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_are_read_where_they_are_named() {
        let all = words("used=98d26d8:8, failed = 0x10:1,odd=3:3,none");
        assert_eq!(all.len(), 2);
        assert_eq!((all[0].name.as_str(), all[0].offset, all[0].bytes), ("used", 0x98d_26d8, 8));
        assert_eq!((all[1].name.as_str(), all[1].offset, all[1].bytes), ("failed", 0x10, 1));
        let memory = [0x11u8, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
        let word = |offset, bytes| Word { name: String::new(), offset, bytes };
        let base = memory.as_ptr() as usize;
        assert_eq!(read(base, &word(0, 4)), Some(0x4433_2211));
        assert_eq!(read(base, &word(2, 1)), Some(0x33));
        assert_eq!(read(8, &word(0, 8)), None);
    }

    #[test]
    fn held_words_are_written() {
        let all = held("hour=10:4=12, minute = 14:1=0x2a,bad=1:4,odd=3:3=1");
        assert_eq!(all.len(), 2);
        assert_eq!((all[0].0.name.as_str(), all[0].0.offset, all[0].0.bytes, all[0].1), ("hour", 0x10, 4, 12));
        assert_eq!((all[1].0.name.as_str(), all[1].0.offset, all[1].0.bytes, all[1].1), ("minute", 0x14, 1, 42));
        let mut memory = [0u8; 8];
        let base = memory.as_mut_ptr() as usize;
        assert!(write(base, &Word { name: String::new(), offset: 2, bytes: 2 }, 0xbeef));
        assert_eq!(memory, [0, 0, 0xef, 0xbe, 0, 0, 0, 0]);
    }
}
