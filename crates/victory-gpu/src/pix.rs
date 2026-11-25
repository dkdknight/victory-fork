//! `VICTORY_PIX=1`: PIX for Windows' GPU capturer (`WinPixGpuCapturer.dll`
//! of the newest PIX under `C:\Program Files\Microsoft PIX`) is loaded
//! before the device is made, so PIX can capture what the card runs.
//!
//! `VICTORY_PIX_TRIGGER=<file>`: whenever that file appears it is removed
//! and the next presented frame is captured (`CaptureNextFrame`, what PIX's
//! own `PIXGpuCaptureNextFrames` calls). The file's first line names the
//! capture (a `.wpix` path); empty, the capture goes beside the trigger as
//! `capture-<unix seconds>.wpix`. Open it in PIX for the card's timing of
//! every draw and dispatch, occupancy and the barriers between them.

use std::path::PathBuf;
use std::sync::OnceLock;

use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

type CaptureNextFrame = unsafe extern "system" fn(*const u16, u32) -> i32;

static CAPTURE: OnceLock<Option<CaptureNextFrame>> = OnceLock::new();

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The newest PIX's capturer, if PIX is installed.
fn capturer() -> Option<PathBuf> {
    let root = PathBuf::from(r"C:\Program Files\Microsoft PIX");
    let mut versions: Vec<PathBuf> = std::fs::read_dir(&root)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("WinPixGpuCapturer.dll").is_file())
        .collect();
    // Version folders are named like 2611.19: newest last by name.
    versions.sort();
    versions.pop().map(|p| p.join("WinPixGpuCapturer.dll"))
}

/// Loads the capturer (`VICTORY_PIX=1`); call before the device is made.
pub(crate) fn load() {
    if std::env::var_os("VICTORY_PIX").is_none_or(|v| v != "1") {
        return;
    }
    CAPTURE.get_or_init(|| {
        let Some(dll) = capturer() else {
            tracing::warn!("VICTORY_PIX=1, but no WinPixGpuCapturer.dll under C:\\Program Files\\Microsoft PIX");
            return None;
        };
        let name = wide(&dll.to_string_lossy());
        // SAFETY: a library path of PIX's own, and an export of it with
        // the signature PIX's header gives it.
        unsafe {
            let module = LoadLibraryW(name.as_ptr());
            if module.is_null() {
                tracing::warn!(dll = %dll.display(), "PIX's GPU capturer did not load");
                return None;
            }
            let f = GetProcAddress(module, c"CaptureNextFrame".as_ptr().cast());
            tracing::info!(dll = %dll.display(), capture = f.is_some(), "PIX's GPU capturer loaded");
            f.map(|f| std::mem::transmute::<_, CaptureNextFrame>(f))
        }
    });
}

/// At a present: a capture of the next frame if the trigger file appeared.
pub(crate) fn frame() {
    let Some(Some(capture)) = CAPTURE.get() else {
        return;
    };
    static TRIGGER: OnceLock<Option<PathBuf>> = OnceLock::new();
    let Some(trigger) = TRIGGER.get_or_init(|| std::env::var_os("VICTORY_PIX_TRIGGER").map(PathBuf::from)) else {
        return;
    };
    if !trigger.is_file() {
        return;
    }
    let named = std::fs::read_to_string(trigger).unwrap_or_default();
    let _ = std::fs::remove_file(trigger);
    let file = match named.lines().next().map(str::trim).filter(|l| !l.is_empty()) {
        Some(path) => PathBuf::from(path),
        None => {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            trigger
                .parent()
                .map_or_else(|| PathBuf::from("."), PathBuf::from)
                .join(format!("capture-{secs}.wpix"))
        }
    };
    let name = wide(&file.to_string_lossy());
    // SAFETY: PIX's export, a NUL-terminated path, one frame.
    let hr = unsafe { capture(name.as_ptr(), 1) };
    tracing::info!(file = %file.display(), hr = format_args!("{hr:#x}"), "PIX capture of the next frame asked for");
}
