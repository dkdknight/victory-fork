//! `VICTORY_SHADER_HOT_DIR=<dir>`: a shader's host code from a file, swapped
//! in while the title runs, to measure what a change to a translation costs
//! or saves without the minutes of loading a restart takes.
//!
//! A thread looks at `<dir>` every half second for `<hash>.dxil` files (the
//! 16 lowercase hex digits the logs name a shader by). When one appears or
//! changes, its bytes take the place of that shader's translation, and every
//! host pipeline made with the shader is made again with them: same root
//! signature (made from the translation's bindings, which the replacement
//! has to keep), same other stages and graphics state, same amplified mesh
//! variant. When the file goes, they are made again from the translation.
//! A pipeline the host refuses keeps the one it had.
//!
//! Pipelines made with a replacement never go into the pipeline library, so
//! a run without the switch never loads one. Replaced host pipelines are
//! kept (never released): a list being recorded may still hold the old one.
//!
//! `<dir>/want.txt` (one hash per line, read again at each look) asks for a
//! starting point: once a shader it names has been translated, its
//! translation is written to `<dir>/orig/<hash>.dxil`.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};
use std::time::{Duration, SystemTime};

use super::make::Pending;

/// The folder, if the switch is on.
pub(crate) fn dir() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        std::env::var_os("VICTORY_SHADER_HOT_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    })
    .as_deref()
}

/// The replacements, by shader hash.
static OVERRIDES: RwLock<Option<HashMap<String, Arc<Vec<u8>>>>> = RwLock::new(None);

/// Every title pipeline created while the switch is on.
static PIPELINES: Mutex<Vec<Weak<Pending>>> = Mutex::new(Vec::new());

thread_local! {
    /// This thread is making a pipeline with replacements: they are looked
    /// up, and the pipeline library is left alone.
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    /// The last pipeline creation error of this thread.
    static ERROR: Cell<Option<i32>> = const { Cell::new(None) };
}

/// The hash a file of the folder replaces: `<16 lowercase hex digits>.dxil`.
pub(crate) fn hash_of_file(name: &str) -> Option<&str> {
    let hash = name.strip_suffix(".dxil")?;
    (hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))).then_some(hash)
}

/// The hashes of a `want.txt`: one a line, blank lines and `#` comments
/// left out, upper case taken as lower.
pub(crate) fn parse_wanted(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_ascii_lowercase)
        .filter(|h| h.len() == 16 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        .collect()
}

/// What tells a file's versions apart.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Stamp {
    modified: Option<SystemTime>,
    size: u64,
}

/// The replacement files of `dir`, by hash.
pub(crate) fn scan(dir: &Path) -> HashMap<String, Stamp> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return HashMap::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let hash = hash_of_file(name.to_str()?)?.to_owned();
            let meta = e.metadata().ok().filter(std::fs::Metadata::is_file)?;
            Some((hash, Stamp { modified: meta.modified().ok(), size: meta.len() }))
        })
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Change {
    /// A file appeared or changed.
    Replaced(String),
    /// A file went.
    Removed(String),
}

/// What changed from one look to the next, by hash.
pub(crate) fn changes(old: &HashMap<String, Stamp>, new: &HashMap<String, Stamp>) -> Vec<Change> {
    let mut out: Vec<Change> = new
        .iter()
        .filter(|(h, s)| old.get(*h) != Some(*s))
        .map(|(h, _)| Change::Replaced(h.clone()))
        .chain(old.keys().filter(|h| !new.contains_key(*h)).map(|h| Change::Removed(h.clone())))
        .collect();
    out.sort_by(|a, b| {
        let key = |c: &Change| match c {
            Change::Replaced(h) | Change::Removed(h) => h.clone(),
        };
        key(a).cmp(&key(b))
    });
    out
}

fn lookup(hash: &str) -> Option<Arc<Vec<u8>>> {
    OVERRIDES.read().unwrap_or_else(|e| e.into_inner()).as_ref()?.get(hash).cloned()
}

fn set_override(hash: &str, code: Option<Vec<u8>>) {
    let mut map = OVERRIDES.write().unwrap_or_else(|e| e.into_inner());
    let map = map.get_or_insert_with(HashMap::new);
    match code {
        Some(code) => map.insert(hash.to_owned(), Arc::new(code)),
        None => map.remove(hash),
    };
}

/// The replacement of a stage's code, while this thread makes a pipeline
/// with replacements ([`with_overrides`]).
pub(crate) fn override_for(hash: &str) -> Option<Arc<Vec<u8>>> {
    if !ACTIVE.get() {
        return None;
    }
    lookup(hash)
}

/// Whether one of `hashes` has a replacement (always false with the switch off).
pub(crate) fn overridden<'a>(mut hashes: impl Iterator<Item = &'a str>) -> bool {
    dir().is_some() && hashes.any(|h| lookup(h).is_some())
}

/// Whether this thread makes a pipeline with replacements: the pipeline
/// library is then neither read nor written.
pub(crate) fn active() -> bool {
    ACTIVE.get()
}

/// Runs `make` with replacements looked up (`on`) or not; and the last
/// pipeline creation error it met.
pub(crate) fn with_overrides<T>(on: bool, make: impl FnOnce() -> T) -> (T, Option<i32>) {
    let before = ACTIVE.replace(on);
    ERROR.set(None);
    let made = make();
    ACTIVE.set(before);
    (made, ERROR.take())
}

/// Notes a pipeline creation error of this thread (for [`with_overrides`]).
pub(crate) fn note_error(e: &windows::core::Error) {
    ERROR.set(Some(e.code().0));
}

/// At the title's creation of a pipeline, with the switch on: remembers it,
/// and starts the thread that looks at the folder.
pub(crate) fn register(p: &Arc<Pending>) {
    let Some(dir) = dir() else { return };
    PIPELINES.lock().unwrap_or_else(|e| e.into_inner()).push(Arc::downgrade(p));
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        tracing::info!(dir = %dir.display(), "hot shaders: <hash>.dxil files in this folder replace translations (VICTORY_SHADER_HOT_DIR)");
        let spawned = std::thread::Builder::new()
            .name("victory-hot-shaders".into())
            .spawn(move || watch(dir));
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "no hot shader thread");
        }
    });
}

fn watch(dir: &'static Path) {
    let mut seen: HashMap<String, Stamp> = HashMap::new();
    let mut written: HashSet<String> = HashSet::new();
    loop {
        let now = scan(dir);
        for change in changes(&seen, &now) {
            apply(dir, &change);
        }
        seen = now;
        write_wanted(dir, &mut written);
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn apply(dir: &Path, change: &Change) {
    let (hash, from) = match change {
        Change::Replaced(hash) => {
            let file = dir.join(format!("{hash}.dxil"));
            let code = match std::fs::read(&file) {
                Ok(code) => code,
                Err(e) => {
                    tracing::warn!(hash = %hash, error = %e, "hot shader {hash}: not read");
                    return;
                }
            };
            if !code.starts_with(b"DXBC") {
                tracing::warn!("hot shader {hash}: {} is not a DXIL container (no DXBC header); ignored", file.display());
                return;
            }
            set_override(hash, Some(code));
            (hash, file.display().to_string())
        }
        Change::Removed(hash) => {
            if lookup(hash).is_none() {
                return;
            }
            set_override(hash, None);
            (hash, "its translation".to_owned())
        }
    };
    let pipelines: Vec<Arc<Pending>> = {
        let mut all = PIPELINES.lock().unwrap_or_else(|e| e.into_inner());
        all.retain(|p| p.strong_count() > 0);
        all.iter().filter_map(Weak::upgrade).filter(|p| p.is_made() && p.uses(hash)).collect()
    };
    let mut made = 0;
    for p in &pipelines {
        match p.remake() {
            Ok(()) => made += 1,
            Err(code) => tracing::warn!(
                shaders = ?p.shaders(),
                "hot shader {hash}: a pipeline is refused ({}); it keeps the one it had",
                code.map_or("no HRESULT, see the warning above".to_owned(), |c| format!("HRESULT {:#010x}", c as u32))
            ),
        }
    }
    tracing::info!("hot shader {hash}: {made} pipelines made again (from {from})");
}

/// Writes the translation of each shader `want.txt` names, once it has one.
fn write_wanted(dir: &Path, written: &mut HashSet<String>) {
    let Ok(text) = std::fs::read_to_string(dir.join("want.txt")) else { return };
    let wanted: Vec<String> = parse_wanted(&text).into_iter().filter(|h| !written.contains(h)).collect();
    if wanted.is_empty() {
        return;
    }
    let found: Vec<(String, Arc<super::Shader>)> = {
        let shaders = super::SHADERS.lock().unwrap_or_else(|e| e.into_inner());
        shaders
            .iter()
            .flat_map(|m| m.values())
            .filter(|s| wanted.contains(&s.hash))
            .map(|s| (s.hash.clone(), s.clone()))
            .collect()
    };
    for (hash, shader) in found {
        let Some(Some(t)) = shader.translated.get() else { continue };
        let orig = dir.join("orig");
        let file = orig.join(format!("{hash}.dxil"));
        match std::fs::create_dir_all(&orig).and_then(|()| std::fs::write(&file, &t.host.dxil)) {
            Ok(()) => tracing::info!("hot shader {hash}: its translation ({}) is written to {}", shader.stage, file.display()),
            Err(e) => tracing::warn!(error = %e, "hot shader {hash}: its translation is not written"),
        }
        written.insert(hash);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names() {
        assert_eq!(hash_of_file("cb6adcc6da2613ff.dxil"), Some("cb6adcc6da2613ff"));
        assert_eq!(hash_of_file("CB6ADCC6DA2613FF.dxil"), None);
        assert_eq!(hash_of_file("cb6adcc6da2613f.dxil"), None);
        assert_eq!(hash_of_file("cb6adcc6da2613ff.pixel.dxil"), None);
        assert_eq!(hash_of_file("cb6adcc6da2613ff.dxil.tmp"), None);
        assert_eq!(hash_of_file("cb6adcc6da2613fg.dxil"), None);
        assert_eq!(hash_of_file("want.txt"), None);
    }

    #[test]
    fn wanted_list() {
        assert_eq!(
            parse_wanted("# mine\n cb6adcc6da2613ff \n\nCB6ADCC6DA2613FE\r\nshort\n"),
            ["cb6adcc6da2613ff", "cb6adcc6da2613fe"]
        );
    }

    #[test]
    fn changes_over_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let a = "00112233445566aa";
        let b = "00112233445566bb";
        std::fs::write(dir.path().join("want.txt"), "x").unwrap();
        std::fs::write(dir.path().join(format!("{a}.dxil")), b"DXBC1").unwrap();
        std::fs::create_dir(dir.path().join(format!("{b}.dxil"))).unwrap();
        let first = scan(dir.path());
        assert_eq!(first.len(), 1, "files only, by their names");
        assert_eq!(changes(&HashMap::new(), &first), [Change::Replaced(a.into())]);
        assert!(changes(&first, &scan(dir.path())).is_empty());

        std::fs::write(dir.path().join(format!("{a}.dxil")), b"DXBC12").unwrap();
        std::fs::write(dir.path().join(format!("{b}.pixel.dxil")), b"DXBC").unwrap();
        let second = scan(dir.path());
        assert_eq!(changes(&first, &second), [Change::Replaced(a.into())]);

        std::fs::remove_file(dir.path().join(format!("{a}.dxil"))).unwrap();
        assert_eq!(changes(&second, &scan(dir.path())), [Change::Removed(a.into())]);
    }

    #[test]
    fn overrides_only_while_making_with_them() {
        let hash = "ffeeddccbbaa0099";
        assert!(lookup(hash).is_none());
        set_override(hash, Some(b"DXBCcode".to_vec()));
        assert!(override_for(hash).is_none(), "not outside a hot make");
        let (got, error) = with_overrides(true, || {
            assert!(active());
            override_for(hash)
        });
        assert_eq!(got.as_deref().map(Vec::as_slice), Some(&b"DXBCcode"[..]));
        assert_eq!(error, None);
        assert!(!active());
        assert!(with_overrides(true, || override_for("ffeeddccbbaa0098")).0.is_none());
        set_override(hash, None);
        assert!(with_overrides(true, || override_for(hash)).0.is_none());
    }
}
