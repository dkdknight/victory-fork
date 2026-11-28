//! Benchmark frame log, for measurement only: with `VICTORY_BENCH_CSV=<file>`
//! every frame the layer presents adds one line to that file: the frame's
//! number, the seconds since the first present, the milliseconds since the
//! previous present, and the Unix time in seconds (to line it up with
//! samples taken outside the process). Rendered frames only: frames that
//! frame generation adds on the card are not presents of ours and are not in
//! it.
//!
//! The lines are buffered and written out every 32 frames, so a process that
//! is stopped by force loses at most the last 32. Without the variable a
//! present costs one load of a flag.
//!
//! From Shoqapique's benchmark harness (VIctory-Stable `5b7785d`, via
//! VIctory-Stable by LeBonVieuxAxel); `scripts/bench-frames.ps1` sums a log
//! up (1 % and 0.1 % lows, median, p99, frames over 50 / 100 / 200 ms).

use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

struct Log {
    file: std::io::BufWriter<std::fs::File>,
    first: Instant,
    last: Instant,
    lines: u32,
}

static LOG: OnceLock<Option<Mutex<Log>>> = OnceLock::new();

fn open() -> Option<Mutex<Log>> {
    let path = std::env::var_os("VICTORY_BENCH_CSV")?;
    let file = match std::fs::File::create(&path) {
        Ok(file) => file,
        Err(e) => {
            tracing::warn!(error = %e, path = %std::path::Path::new(&path).display(), "benchmark frame log not opened");
            return None;
        }
    };
    let mut file = std::io::BufWriter::new(file);
    let _ = writeln!(file, "frame,t_s,interval_ms,unix_s");
    let now = Instant::now();
    Some(Mutex::new(Log { file, first: now, last: now, lines: 0 }))
}

/// A frame was presented (called right after the swap chain's `Present`).
pub(crate) fn presented(frame: u64) {
    let Some(log) = LOG.get_or_init(open) else {
        return;
    };
    let now = Instant::now();
    let mut log = log.lock().unwrap_or_else(|e| e.into_inner());
    let interval = now.duration_since(log.last).as_secs_f64() * 1e3;
    let t = now.duration_since(log.first).as_secs_f64();
    log.last = now;
    let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
    let _ = writeln!(log.file, "{frame},{t:.6},{interval:.3},{unix:.3}");
    log.lines += 1;
    if log.lines % 32 == 0 {
        let _ = log.file.flush();
    }
}
