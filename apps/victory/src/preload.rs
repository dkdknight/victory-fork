//! `victory preload`: every shader of the title translated, and the host
//! pipelines the runs need made before the title starts, so that a run does
//! not stop to make them while one plays. From trustno1-coder/VIctory-NVIDIA
//! (29d8dbf, "F1 preload", via VIctory-Stable); the barrier `victory run`
//! puts before the title (`crate::gate`), the stamps and the static samplers
//! listed before any process makes a pipeline are this fork's
//! (`comparaison/preload-shaders.md`, steps 1 to 3).
//!
//! With `--gate` (what the barrier starts): each step is left out while its
//! stamp, a file beside the pipeline library, says what it depends on has not
//! changed; lines starting with `@` tell the barrier how it goes (`@step`,
//! `@progress`, `@reason`, `@done`); the end of standard input (the barrier's
//! "Jouer maintenant" or "Arrêter") stops it, keeping what is done.
//!
//! 1. Shaders: every container of `shaders.rpf`, the title's executable and
//!    its DLLs translated into the shader cache. Stamp: the translator, the
//!    translation's key (the translator, its `VICTORY_*` switches, the host's
//!    facts) and the files' sizes and dates.
//! 2. Essential pipelines: those earlier runs drew with (the early list) and
//!    the slow ones. Their static samplers are added to the runs' list first,
//!    in one order, in this one process (`known::list_samplers`); processes of
//!    their own then make them for the driver's cache (`warm`), each making
//!    the listed samplers in the list's order and never writing the list;
//!    last, this process makes them again (from the driver's cache: quickly)
//!    and stores them in the library the runs load. A sampler numbered
//!    otherwise than in the run would sample with the wrong one
//!    (`victory_gpu::pipeline::samplers`). Stamp: the translation, the host
//!    adapter and its driver, and this program (the driver keeps its cache by
//!    program path).
//! 3. (`--all`, `VICTORY_PRELOAD_ALL=1`) every pipeline the title ever
//!    created (~88,600), for the driver's cache only: 20 to 40 minutes.
//!
//! Without `--gate`, as Stable's: all three, whatever the stamps say.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Machine lines for the barrier (`--gate`).
static REPORTING: AtomicBool = AtomicBool::new(false);
/// Asked to stop: what is done is kept.
static STOP: AtomicBool = AtomicBool::new(false);
/// Processes or threads at once (`--jobs`, `VICTORY_PRELOAD_JOBS`; 0: the default).
static JOBS: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn reporting() -> bool {
    REPORTING.load(Ordering::Relaxed)
}

pub(crate) fn stopped() -> bool {
    STOP.load(Ordering::Relaxed)
}

pub(crate) fn jobs() -> Option<usize> {
    Some(JOBS.load(Ordering::Relaxed)).filter(|&n| n > 0)
}

/// Tells the barrier how far the step is: at most every quarter second,
/// and always at its start and end.
pub(crate) fn progress(done: usize, total: usize) {
    if !reporting() {
        return;
    }
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let edge = done == 0 || done >= total;
    if !edge && last.is_some_and(|t| t.elapsed() < Duration::from_millis(250)) {
        return;
    }
    *last = Some(Instant::now());
    println!("@progress {done} {total}");
}

/// A stamp: what a step depends on, as `name=value` lines.
pub(crate) type Stamp = Vec<(&'static str, String)>;

pub(crate) fn stamp_text(stamp: &Stamp) -> String {
    stamp.iter().map(|(k, v)| format!("{k}={v}\n")).collect()
}

/// Why a step whose last stamp was `old` (`None`: never done) is to be done
/// again with `new`; `None` when nothing changed.
pub(crate) fn stale(old: Option<&str>, new: &Stamp) -> Option<String> {
    let Some(old) = old else { return Some("premier préchargement".into()) };
    let was = |key: &str| old.lines().find_map(|l| l.split_once('=').filter(|(k, _)| *k == key).map(|(_, v)| v));
    new.iter().find(|(k, v)| was(k) != Some(v.as_str())).map(|(k, v)| {
        match *k {
            "translator" => "nouveau traducteur".to_owned(),
            "translation" => "réglages de traduction changés (switches VICTORY_* ou carte)".to_owned(),
            "files" => "fichiers du jeu changés".to_owned(),
            "host" => format!("nouveau pilote ou nouvelle carte : {v}"),
            "program" => "nouveau build de VIctory".to_owned(),
            "library" => "bibliothèque de pipelines absente".to_owned(),
            "book" => "nouveaux pipelines dans le carnet".to_owned(),
            "game" => "nouvelle version du jeu".to_owned(),
            other => format!("{other} changé"),
        }
    })
}

fn read_stamp(dir: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(dir.join(name)).ok()
}

pub(crate) fn write_stamp(dir: &Path, name: &str, stamp: &Stamp) {
    if let Err(e) = std::fs::write(dir.join(name), stamp_text(stamp)) {
        println!("preload stamp {name} not written: {e}");
    }
}

/// A file's size and date, as the stamps tell files.
pub(crate) fn file_id(path: &Path) -> String {
    let meta = std::fs::metadata(path).ok();
    let size = meta.as_ref().map_or(0, std::fs::Metadata::len);
    let date = meta
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    format!("{size}@{date}")
}

/// The title's files with shaders in them: `shaders.rpf` and the programs
/// beside the title's executable (it, its DLLs), by name. The rest of the
/// folder (18 GB of other archives) has none worth a read at every preload.
pub(crate) fn title_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .filter(|p| {
                    let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
                    name == "shaders.rpf" || name.ends_with(".exe") || name.ends_with(".dll")
                })
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

#[cfg(windows)]
pub use command::run;

#[cfg(windows)]
mod command {
    use super::*;
    use crate::{translate, warm};
    use anyhow::Result;
    use victory_hle::known_pipelines as known;

    const SHADERS_STAMP: &str = "preload-shaders.stamp";
    const LIBRARY_STAMP: &str = "preload-library.stamp";
    const ALL_STAMP: &str = "preload-all.stamp";

    pub(crate) fn explain() {
        println!();
        println!("Preloading shaders: every shader of the title is translated, then the pipelines the game");
        println!("made in earlier runs are built, and those it needs while loading are stored where it loads");
        println!("them from at the start, so it does not stop to make them while you play.");
        println!("What is done is kept: stopped, the next preload goes on from there.");
        println!("A later update of the translator, of its switches or of the graphics driver needs a new one.");
        println!();
    }

    /// `victory preload`.
    pub fn run(paths: Vec<PathBuf>, gate: bool, all: bool, jobs: Option<usize>) -> Result<()> {
        let jobs = jobs.or_else(|| std::env::var("VICTORY_PRELOAD_JOBS").ok().and_then(|v| v.trim().parse().ok()));
        JOBS.store(jobs.unwrap_or(0), Ordering::Relaxed);
        if !gate {
            explain();
        } else {
            REPORTING.store(true, Ordering::Relaxed);
            // The barrier closes our input to stop us.
            std::thread::spawn(|| {
                use std::io::Read;
                let mut sink = [0u8; 64];
                let mut input = std::io::stdin();
                while matches!(input.read(&mut sink), Ok(n) if n > 0) {}
                STOP.store(true, Ordering::Relaxed);
            });
        }
        let began = Instant::now();
        let cfg = victory_core::Config::load()?;
        let shaders = cfg.cache_subdir("shaders")?;
        let dir = shaders.with_file_name("pipelines");
        std::fs::create_dir_all(&dir)?;
        // The library and the list of static samplers beside it are this
        // process's from the start (and the shader cache).
        known::use_library(shaders);
        let Some(host) = known::host_label() else {
            anyhow::bail!("no host GPU: nothing preloaded");
        };
        let (translator, translation) = known::translation_ids();
        println!("preload: {host}; translator {translator}, translations {translation}");
        let steps = if all { 3 } else { 2 };
        let step = |n: usize, label: &str, why: &str| {
            if gate {
                println!("@reason {why}");
                println!("@step {n} {steps} {label}");
            } else {
                println!("step {n} of {steps}: {label} ({why})");
            }
        };

        // 1. Shaders.
        let files: Vec<PathBuf> = if gate {
            paths.iter().flat_map(|p| if p.is_dir() { title_files(p) } else { vec![p.clone()] }).collect()
        } else {
            crate::default_paths(paths)?
        };
        let shaders_stamp: Stamp = vec![
            ("translator", translator.clone()),
            ("translation", translation.clone()),
            ("files", files.iter().map(|f| format!("{}:{}", f.display(), file_id(f))).collect::<Vec<_>>().join(";")),
        ];
        // (No file found: nothing to translate, and not the whole folder.)
        let why = if !gate {
            Some("asked".into())
        } else if files.is_empty() {
            println!("preload: no shaders.rpf nor program in the title's folder: no shader translated");
            None
        } else {
            stale(read_stamp(&dir, SHADERS_STAMP).as_deref(), &shaders_stamp)
        };
        if let Some(why) = why {
            step(1, "Traduction des shaders", &why);
            translate::precompile(files, None)?;
            if stopped() {
                return Ok(());
            }
            write_stamp(&dir, SHADERS_STAMP, &shaders_stamp);
        }

        // 2. The essential pipelines.
        let book = warm::Book::read(&dir);
        if book.keys.is_empty() {
            println!("preload: no title pipeline written down yet (runs with VICTORY_PIPELINE_BOOK=1 write them): none to make");
            if gate {
                println!("@done");
            }
            return Ok(());
        }
        let program = std::env::current_exe().map(|p| format!("{}:{}", p.display(), file_id(&p))).unwrap_or_default();
        let essentials = warm::essentials(&book.keys, &book.early, &book.slow, &book.bad);
        let library_stamp: Stamp = vec![
            ("translator", translator.clone()),
            ("translation", translation.clone()),
            ("host", host.clone()),
            ("program", program.clone()),
            ("library", dir.join("host-pipelines.bin").exists().to_string()),
        ];
        let mut library_stamp_written = library_stamp.clone();
        library_stamp_written[4].1 = "true".into();
        let why = if gate {
            stale(read_stamp(&dir, LIBRARY_STAMP).as_deref(), &library_stamp).filter(|_| !essentials.is_empty())
        } else {
            Some("asked".into())
        };
        if let Some(why) = why {
            println!("preload: {} essential pipelines of {} known", essentials.len(), book.keys.len());
            step(2, "Pipelines essentiels : ordre des samplers", &why);
            list_samplers(&dir, &essentials);
            if stopped() {
                return Ok(());
            }
            step(2, "Pipelines essentiels : cache du pilote", &why);
            warm::make_in_processes(&dir, &book.keys, essentials.clone(), None, None, false)?;
            if stopped() {
                return Ok(());
            }
            step(2, "Pipelines essentiels : bibliothèque", &why);
            warm::into_library(&dir, &essentials, None)?;
            if stopped() {
                return Ok(());
            }
            write_stamp(&dir, LIBRARY_STAMP, &library_stamp_written);
        }

        // 3. Every known pipeline, for the driver's cache.
        if all {
            let mut all_stamp = library_stamp_written.clone();
            all_stamp.push(("book", book.keys.len().to_string()));
            let why = if gate { stale(read_stamp(&dir, ALL_STAMP).as_deref(), &all_stamp) } else { Some("asked".into()) };
            if let Some(why) = why {
                let everything = warm::everything(&book.keys, &book.early, &book.bad);
                step(3, "Tous les pipelines : ordre des samplers", &why);
                list_samplers(&dir, &everything);
                if stopped() {
                    return Ok(());
                }
                step(3, "Tous les pipelines : cache du pilote", &why);
                warm::make_in_processes(&dir, &book.keys, everything, None, None, false)?;
                if stopped() {
                    return Ok(());
                }
                write_stamp(&dir, ALL_STAMP, &all_stamp);
            }
        }
        println!("preload done in {:.0} s", began.elapsed().as_secs_f64());
        if gate {
            println!("@done");
        }
        Ok(())
    }

    /// The static samplers of `which` added to the runs' list, in its order.
    fn list_samplers(dir: &Path, which: &[usize]) {
        let began = Instant::now();
        let mut n = 0;
        progress(0, which.len());
        let listed = known::list_samplers(dir, which, |_| {
            n += 1;
            progress(n, which.len());
            !stopped()
        });
        println!(
            "static samplers of {listed} of {} pipelines in the list before any is made ({:.0} s)",
            which.len(),
            began.elapsed().as_secs_f64()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(pairs: &[(&'static str, &str)]) -> Stamp {
        pairs.iter().map(|(k, v)| (*k, (*v).to_owned())).collect()
    }

    /// A stamp read back is fresh; a change names the first thing that
    /// changed; none at all is a first preload.
    #[test]
    fn a_stamp_says_why_a_step_is_done_again() {
        let now = stamp(&[("translator", "aa"), ("translation", "bb"), ("host", "RTX, driver 1")]);
        let text = stamp_text(&now);
        assert_eq!(stale(Some(&text), &now), None);
        assert_eq!(stale(None, &now).as_deref(), Some("premier préchargement"));
        let new_driver = stamp(&[("translator", "aa"), ("translation", "bb"), ("host", "RTX, driver 2")]);
        assert_eq!(stale(Some(&text), &new_driver).as_deref(), Some("nouveau pilote ou nouvelle carte : RTX, driver 2"));
        let new_switch = stamp(&[("translator", "aa"), ("translation", "cc"), ("host", "RTX, driver 1")]);
        assert!(stale(Some(&text), &new_switch).unwrap().starts_with("réglages de traduction"));
        let new_translator = stamp(&[("translator", "ab"), ("translation", "cc"), ("host", "RTX, driver 1")]);
        assert_eq!(stale(Some(&text), &new_translator).as_deref(), Some("nouveau traducteur"));
        // A value with '=' in it, and a line more in the new stamp.
        let odd = stamp(&[("files", "a=b;c")]);
        assert_eq!(stale(Some(&stamp_text(&odd)), &odd), None);
        let more = stamp(&[("translator", "aa"), ("translation", "bb"), ("host", "RTX, driver 1"), ("book", "88567")]);
        assert_eq!(stale(Some(&text), &more).as_deref(), Some("nouveaux pipelines dans le carnet"));
    }

    /// Only `shaders.rpf` and the programs are read, by name, in one order.
    #[test]
    fn the_title_files_are_the_programs_and_the_shader_archive() {
        let dir = std::env::temp_dir().join(format!("victory-preload-files-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub.dll")).unwrap();
        for name in ["shaders.rpf", "game_scarlett_master.exe", "XboxBvhBuilder.dll", "levels.rpf", "data.bin"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        let names: Vec<String> =
            title_files(&dir).iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["XboxBvhBuilder.dll", "game_scarlett_master.exe", "shaders.rpf"]);
        assert!(title_files(&dir.join("missing")).is_empty());
        assert!(file_id(&dir.join("shaders.rpf")).starts_with("1@"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
