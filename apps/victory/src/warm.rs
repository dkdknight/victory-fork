//! `victory warm`: host pipelines made ahead of a run (`known_pipelines`), in
//! processes of their own: a compiler that crashes or hangs costs one pipeline.
//!
//! `victory preload` (from trustno1-coder/VIctory-NVIDIA 29d8dbf, via
//! VIctory-Stable) builds on it: [`into_library`] stores the pipelines a run
//! needs first in the run's pipeline library, and the processes started here
//! stop early when a preload is told to (`crate::preload::stopped`).

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use victory_hle::known_pipelines as known;

use crate::preload;

/// Pipelines a process makes before another takes its place: the compiler's
/// and the translations' memory goes with it.
const AT_ONCE: usize = 40;
/// A pipeline that takes longer is taken for one the compiler hangs on.
const LONGEST: Duration = Duration::from_secs(180);

/// As the runs count one that takes long.
const SLOW_MS: f64 = 1000.0;

pub(crate) fn folder() -> Result<PathBuf> {
    let cfg = victory_core::Config::load()?;
    Ok(cfg.cache_subdir("shaders")?.with_file_name("pipelines"))
}

/// What the pipelines made before are told by in `warm-done.txt`: the
/// translator with its settings and the driver (`known::fingerprint`); the
/// program's path, since the driver keeps what is made here by program path
/// (made by another copy of this program, a pipeline is not made for this
/// one; VIctory-Stable); and that the processes made the static samplers in
/// the order of the runs' list (`known::use_translations`): what was made
/// before they did may sample with other samplers.
pub(crate) fn fingerprint() -> String {
    let program = std::env::current_exe().map(|p| p.to_string_lossy().to_lowercase()).unwrap_or_default();
    let mut h = blake3::Hasher::new();
    h.update(known::fingerprint().as_bytes());
    h.update(program.as_bytes());
    h.update(b"static samplers in the list's order");
    h.finalize().to_hex()[..16].to_owned()
}

/// Reads a list beside the library (a missing file reads as empty).
fn read(dir: &Path, file: &str) -> String {
    std::fs::read_to_string(dir.join(file)).unwrap_or_default()
}

/// The keys of the pipelines that ended a process of their own.
fn bad_keys(text: &str) -> HashSet<&str> {
    text.lines().filter_map(|l| l.split(' ').next()).filter(|k| !k.is_empty()).collect()
}

/// The pipelines a run needs first, by their numbers in the book: those
/// earlier runs drew with (the early list) and those that took a second or
/// more to make, the early ones first, then by key; not those that ended a
/// process of their own.
pub(crate) fn essentials(keys: &[String], early: &str, slow: &str, bad: &str) -> Vec<usize> {
    let bad = bad_keys(bad);
    let early: HashSet<&str> = early.lines().filter(|k| !k.is_empty()).collect();
    let slow: HashSet<&str> = slow.lines().filter_map(|l| l.split(' ').next()).filter(|k| !k.is_empty()).collect();
    let mut all: Vec<usize> = (0..keys.len())
        .filter(|&n| {
            let key = keys[n].as_str();
            !bad.contains(key) && (early.contains(key) || slow.contains(key))
        })
        .collect();
    all.sort_by_key(|&n| (!early.contains(keys[n].as_str()), &keys[n]));
    // (A key the book has twice: once.)
    let mut seen = HashSet::new();
    all.retain(|&n| seen.insert(keys[n].as_str()));
    all
}

/// Every pipeline of the book but those that ended a process of their own,
/// the early ones first.
pub(crate) fn everything(keys: &[String], early: &str, bad: &str) -> Vec<usize> {
    let bad = bad_keys(bad);
    let early: HashSet<&str> = early.lines().filter(|k| !k.is_empty()).collect();
    let mut all: Vec<usize> = (0..keys.len()).filter(|&n| !bad.contains(keys[n].as_str())).collect();
    all.sort_by_key(|&n| (!early.contains(keys[n].as_str()), &keys[n]));
    all
}

/// The pipelines of the book in `dir`, as [`essentials`] and [`everything`] take them.
pub(crate) struct Book {
    pub(crate) keys: Vec<String>,
    pub(crate) early: String,
    pub(crate) slow: String,
    pub(crate) bad: String,
}

impl Book {
    pub(crate) fn read(dir: &Path) -> Book {
        Book {
            keys: known::keys(dir),
            early: read(dir, "early-pipelines.txt"),
            slow: read(dir, "slow-pipelines.txt"),
            bad: read(dir, "warm-bad.txt"),
        }
    }
}

pub fn run(jobs: Option<usize>, limit: Option<usize>, again: bool, used: bool, these: Option<String>) -> Result<()> {
    let dir = folder()?;
    if let Some(these) = these {
        return make(&dir, &these);
    }
    // (A first preload on a PC no run has written pipelines down on yet: none known.)
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    victory_hle::known_pipelines::use_translations(dir.with_file_name("shaders"));
    let book = Book::read(&dir);
    // Those earlier runs drew with first, the others in no order (by key).
    let mut all = everything(&book.keys, &book.early, &book.bad);
    if used {
        let early: HashSet<&str> = book.early.lines().collect();
        all.retain(|&n| early.contains(book.keys[n].as_str()));
    }
    make_in_processes(&dir, &book.keys, all, jobs, limit, again)
}

/// Makes the pipelines `all` (numbers in the book) in processes of their own,
/// for the driver's cache on disk: those made before under the same
/// [`fingerprint`] are left out unless `again`.
pub(crate) fn make_in_processes(
    dir: &Path,
    keys: &[String],
    mut all: Vec<usize>,
    jobs: Option<usize>,
    limit: Option<usize>,
    again: bool,
) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let began = Instant::now();
    let fingerprint = fingerprint();
    let done_file = dir.join("warm-done.txt");
    let bad_file = dir.join("warm-bad.txt");
    let listed = std::fs::read_to_string(&done_file).unwrap_or_default();
    let mut done: HashSet<&str> = HashSet::new();
    for line in listed.lines() {
        if let Some((of, key)) = line.split_once(' ')
            && of == fingerprint
        {
            done.insert(key);
        }
    }
    let bad = std::fs::read_to_string(&bad_file).unwrap_or_default();
    let bad = bad_keys(&bad);
    all.retain(|&n| {
        let key = keys[n].as_str();
        (again || !done.contains(key)) && !bad.contains(key)
    });
    all.truncate(limit.unwrap_or(usize::MAX));
    let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
    let jobs = jobs.or_else(preload::jobs).unwrap_or(cores * 2 / 3).max(1);
    println!(
        "{} pipelines known, {} made before ({fingerprint}), {} bad; {} to make, {jobs} processes",
        keys.len(),
        done.len(),
        bad.len(),
        all.len()
    );
    preload::progress(0, all.len());
    let chunks: Vec<&[usize]> = all.chunks(AT_ONCE).collect();
    let next = AtomicUsize::new(0);
    let state = Mutex::new((Vec::<f64>::new(), 0usize, 0usize));
    let done_out = Mutex::new(std::fs::OpenOptions::new().create(true).append(true).open(&done_file)?);
    // (Read back by the runs: these are made beside the others, not before.)
    let slow_out = Mutex::new(std::fs::OpenOptions::new().create(true).append(true).open(dir.join("slow-pipelines.txt"))?);
    let bad_out = Mutex::new(std::fs::OpenOptions::new().create(true).append(true).open(&bad_file)?);
    std::thread::scope(|s| {
        for _ in 0..jobs.min(chunks.len()) {
            s.spawn(|| {
                while !preload::stopped()
                    && let Some(&chunk) = chunks.get(next.fetch_add(1, Ordering::Relaxed))
                {
                    let mut rest = chunk;
                    while !rest.is_empty() {
                        let (made, ended) = child(rest, |n, ms, there| {
                            if ms >= SLOW_MS {
                                let _ = writeln!(lock(&slow_out), "\n{} {ms:.0}", keys[n]);
                            }
                            let mut state = lock(&state);
                            state.0.push(ms);
                            state.1 += usize::from(!there);
                            let count = state.0.len();
                            if count.is_multiple_of(1000) {
                                println!("{count} of {}, {:.0} s", all.len(), began.elapsed().as_secs_f64());
                            }
                            preload::progress(count, all.len());
                            drop(state);
                            let _ = writeln!(lock(&done_out), "{fingerprint} {}", keys[n]);
                        });
                        rest = &rest[made.min(rest.len())..];
                        // Stopped: the one it was at is not to blame.
                        if preload::stopped() {
                            break;
                        }
                        match (ended, rest.split_first()) {
                            (Some(why), Some((&n, after))) => {
                                // The one it was at.
                                println!("pipeline {n} {}: {why}", keys[n]);
                                let _ = writeln!(lock(&bad_out), "{} {why}", keys[n]);
                                lock(&state).2 += 1;
                                rest = after;
                            }
                            _ => break,
                        }
                    }
                }
            });
        }
    });
    let (mut times, refused, crashed) = state.into_inner().unwrap_or_else(|e| e.into_inner());
    times.sort_by(f64::total_cmp);
    let at = |q: f64| times.get((times.len() as f64 * q) as usize).copied().unwrap_or(0.0);
    println!(
        "{} made in {:.0} s ({refused} have no host pipeline, {crashed} ended their process){}; ms each: median {:.1}, p90 {:.1}, p99 {:.1}, most {:.0}, sum {:.0} s",
        times.len(),
        began.elapsed().as_secs_f64(),
        if preload::stopped() { ", stopped" } else { "" },
        at(0.5),
        at(0.9),
        at(0.99),
        times.last().copied().unwrap_or(0.0),
        times.iter().sum::<f64>() / 1e3
    );
    Ok(())
}

/// `victory preload`'s last step: the pipelines `which` (numbers in the
/// book: [`essentials`]) made again in this process and stored in the host
/// pipeline library the runs load at their start. [`make_in_processes`] made
/// them in processes of their own first, so the driver has them and they come
/// quickly; those that ended a process are left out. Not all the known ones:
/// one process holding the compiler's memory for 88,000 pipelines is what
/// `AT_ONCE` keeps the others from. From trustno1-coder/VIctory-NVIDIA
/// (29d8dbf, via VIctory-Stable); here the pipelines with static samplers go
/// into the library too, since every process makes the samplers in the
/// list's order (`known::list_samplers`). The caller has set the library up
/// (`known::use_library`).
pub(crate) fn into_library(dir: &Path, which: &[usize], jobs: Option<usize>) -> Result<()> {
    let threads = jobs
        .or_else(preload::jobs)
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get()) * 2 / 3)
        .max(1);
    println!("into the pipeline library: {} pipelines (loading and slow ones), {threads} threads", which.len());
    preload::progress(0, which.len());
    let began = Instant::now();
    let (count, made) = (AtomicUsize::new(0), AtomicUsize::new(0));
    // A few at a time, so that a stop is heard soon.
    const STEP: usize = 16;
    std::thread::scope(|s| {
        for t in 0..threads {
            let these: Vec<usize> = which.iter().copied().skip(t).step_by(threads).collect();
            let (count, made, total) = (&count, &made, which.len());
            s.spawn(move || {
                for part in these.chunks(STEP) {
                    if preload::stopped() {
                        return;
                    }
                    known::make(dir, part, |_, came| {
                        let Some((_, there)) = came else { return };
                        made.fetch_add(usize::from(there), Ordering::Relaxed);
                        let n = count.fetch_add(1, Ordering::Relaxed) + 1;
                        if n.is_multiple_of(500) {
                            println!("{n} of {total} in the library, {:.0} s", began.elapsed().as_secs_f64());
                        }
                        preload::progress(n, total);
                    });
                }
            });
        }
    });
    known::keep_library();
    println!(
        "{} of {} in the pipeline library in {:.0} s{}",
        made.load(Ordering::Relaxed),
        which.len(),
        began.elapsed().as_secs_f64(),
        if preload::stopped() { " (stopped)" } else { "" }
    );
    Ok(())
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Has a process make `these`; how many it made, and how it ended if not well.
fn child(these: &[usize], mut each: impl FnMut(usize, f64, bool)) -> (usize, Option<String>) {
    let list: Vec<String> = these.iter().map(usize::to_string).collect();
    let spawned = std::env::current_exe().and_then(|exe| {
        use std::os::windows::process::CommandExt;
        const IDLE_PRIORITY_CLASS: u32 = 0x40;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        Command::new(exe)
            .args(["warm", "--these", &list.join(",")])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(IDLE_PRIORITY_CLASS | CREATE_NO_WINDOW)
            .spawn()
    });
    let Ok(mut process) = spawned else { return (these.len(), None) };
    let Some(out) = process.stdout.take() else { return (these.len(), None) };
    let process = Mutex::new(process);
    let (heard, over) = (Mutex::new(Instant::now()), AtomicBool::new(false));
    let hung = AtomicBool::new(false);
    let mut made = 0;
    std::thread::scope(|s| {
        s.spawn(|| {
            while !over.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(200));
                if preload::stopped() {
                    let _ = lock(&process).kill();
                    return;
                }
                if lock(&heard).elapsed() > LONGEST {
                    hung.store(true, Ordering::Relaxed);
                    let _ = lock(&process).kill();
                    return;
                }
            }
        });
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            *lock(&heard) = Instant::now();
            let mut words = line.split(' ');
            if words.next() == Some("made")
                && let (Some(n), Some(ms), Some(there)) = (words.next(), words.next(), words.next())
                && let (Ok(n), Ok(ms)) = (n.parse(), ms.parse())
            {
                made += 1;
                each(n, ms, there == "1");
            }
        }
        over.store(true, Ordering::Relaxed);
    });
    let status = lock(&process).wait();
    let well = status.as_ref().is_ok_and(|s| s.success());
    if well {
        return (these.len(), None);
    }
    let why = if hung.load(Ordering::Relaxed) {
        format!("hung for {} s", LONGEST.as_secs())
    } else {
        status.map_or_else(|e| e.to_string(), |s| format!("{s}"))
    };
    (made, Some(why))
}

/// What such a process does.
fn make(dir: &Path, these: &str) -> Result<()> {
    let these: Vec<usize> = these.split(',').map(|n| n.parse().context("--these")).collect::<Result<_>>()?;
    let cfg = victory_core::Config::load()?;
    victory_hle::known_pipelines::use_translations(cfg.cache_subdir("shaders")?);
    let mut out = std::io::stdout();
    known::make(dir, &these, |n, came| {
        let told = match came {
            None => writeln!(out, "at {n}"),
            Some((ms, there)) => writeln!(out, "made {n} {ms:.2} {}", u8::from(there)),
        };
        if told.and_then(|()| out.flush()).is_err() {
            // Whoever asked is gone.
            std::process::exit(1);
        }
    });
    // Without what a run's end does (nothing to save, the host's threads).
    let _ = out.flush();
    std::process::exit(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_owned()).collect()
    }

    /// The essentials: early and slow ones, the early first, never a bad
    /// one, each key once.
    #[test]
    fn essentials_are_early_and_slow_ones_early_first() {
        let k = keys(&["e2", "x", "s1", "e1", "bad", "e1", "s2"]);
        let early = "e1\ne2\nbad\n";
        let slow = "\ns1 1200\ns2 3000\ns1 1500\nbad 9000";
        let bad = "bad hung for 180 s\n";
        let got: Vec<&str> = essentials(&k, early, slow, bad).iter().map(|&n| k[n].as_str()).collect();
        assert_eq!(got, ["e1", "e2", "s1", "s2"]);
        assert!(essentials(&k, "", "", "").is_empty());
    }

    /// Everything: all but the bad ones, the early first.
    #[test]
    fn everything_puts_the_early_ones_first() {
        let k = keys(&["c", "a", "b", "bad"]);
        let got: Vec<&str> = everything(&k, "b\n", "bad exit code: 1").iter().map(|&n| k[n].as_str()).collect();
        assert_eq!(got, ["b", "a", "c"]);
    }
}
