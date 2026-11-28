//! Settings changed while the title runs (`VICTORY_LIVE_FILE`; off by
//! default), so that an A/B comparison is made in one run at one spot.
//!
//! Ported from Corgansm/victory-corgi (`crates/victory-gpu/src/live.rs`,
//! `scripts/live-menu.ps1`), with the settings of this tree.
//!
//! `VICTORY_LIVE_FILE=<file>` names the file; `VICTORY_LIVE_FILE=1` takes
//! `live-settings.txt` beside `VICTORY_DATA_DIR` (in its parent folder;
//! without it, `%LOCALAPPDATA%\victory`). The file holds `NAME=value` lines,
//! one for each of [`SPECS`], and is read again whenever it changes (a thread
//! looks every [`POLL`]). At the start of a run it is written with the run's
//! own values, and `<file>.menu` lists the settings
//! (`name|label|least|most|step|switch`, one a line) for what changes them:
//! the window of `scripts\live-menu.ps1`, and the overlay here, over the
//! game's top right corner, shown and hidden with F8 (drag a bar, the wheel
//! steps it, a right click puts back the run's value). Each line also has
//! the label in French, which the window shows; `<file>.render` holds the
//! title's render and output sizes ([`render_sizes`]).
//!
//! The window is victory-corgi's ("VIctory live settings",
//! Corgansm/victory-corgi `scripts/live-menu.ps1`), with those of their
//! settings that this tree has: the ambient light, the creases' shade and
//! radius and the air's limit (`crate::ambient`), the caps of card buffers
//! and BAR windows (`crate::shadow`), the stats panel and the frame rate cap
//! (`crate::frame`). Not here: their render scale (read once by the loader,
//! and DLSS picks the scene's size in this tree's Series X runs), their DXR
//! reader and their ray tracing modes (not in this tree;
//! `VICTORY_BVH_NO_HIT` is decided when shaders are translated).
//!
//! A value read or set by a menu takes effect at the title's next present
//! ([`presented`]), as `VICTORY_AB`'s phases do, never in the middle of a
//! frame. A setting keeps the run's own value (its variable, read once as
//! before) until a menu changes it: [`get`] answers `None` till then. A
//! setting `VICTORY_AB` or `VICTORY_LIVE_ALT` names (`crate::ab`) goes by
//! those instead.
//!
//! Only settings whose code asks again at every use can be here; those read
//! once into a `OnceLock` ask [`number`] (or `crate::ab::pick`) first. Not
//! here, for these reasons:
//! - `VICTORY_CARD_ALL`: decided once per title buffer when its copy is
//!   made, nearly all of them while the world loads; a copy keeps its kind,
//!   so switching it later changes next to nothing;
//! - `VICTORY_ASYNC_SUBMIT`: `VICTORY_THIN=1` lists need it, and leaving it
//!   needs the line drained (`VICTORY_AB` does that at a phase's end);
//! - the translator's switches: shaders are translated once and cached;
//! - the render scale: `victory_loader::hooks` patches the title with it
//!   once, as it starts.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, Once, OnceLock};
use std::time::{Duration, SystemTime};

/// How often the file is looked at.
const POLL: Duration = Duration::from_millis(200);

/// A setting the menus change.
pub(crate) struct Spec {
    pub(crate) name: &'static str,
    pub(crate) label: &'static str,
    /// The label in French, for the window of `scripts\live-menu.ps1` (the
    /// `.menu` file's seventh field).
    pub(crate) fr: &'static str,
    pub(crate) least: f32,
    pub(crate) most: f32,
    pub(crate) step: f32,
    /// On (1) or off (0) rather than a number.
    pub(crate) switch: bool,
    /// The run's own value.
    start: fn() -> f32,
}

// (The switches 0, 4 to 6, 8 and 10 are asked by name, through `crate::ab::pick`.)
pub(crate) const SUBMIT_FRAMES: usize = 1;
pub(crate) const SUBMIT_DEPTH: usize = 2;
pub(crate) const LAND_THREADS: usize = 3;
pub(crate) const PANEL: usize = 7;
/// Asked by name too (`crate::ab::pick`); the number for
/// `crate::shadow::masked_patch_wanted`.
pub(crate) const MASKED_PATCH: usize = 8;
/// How long title copies are read (`crate::host::wide_copy`): 0, 1 or 2,
/// asked through [`number`] at each long copy.
pub(crate) const WIDE_COPY: usize = 9;
/// DLSS taking the title's upscalings (`crate::streamline::dlss_on`): 0 the
/// title's own FidelityFX, 1 DLSS; only with `VICTORY_DLSS=1` at the start.
pub(crate) const DLSS: usize = 12;
/// DLSS Frame Generation's amount (`crate::streamline::set_framegen`): 0
/// off, 2 to 6 frames shown for each one rendered (1 is off); only with
/// `VICTORY_FRAMEGEN=1` at the start.
pub(crate) const FRAMEGEN_X: usize = 13;
// The settings of victory-corgi's window ("VIctory live settings",
// Corgansm/victory-corgi 99fab93e and 482a81c7) this tree has:
/// The stand-in for the indirect light (`crate::ambient`): its level, the
/// creases' shade and radius, and the air's limit.
pub(crate) const AMBIENT: usize = 14;
pub(crate) const SHADE_STRENGTH: usize = 15;
pub(crate) const SHADE_RADIUS: usize = 16;
pub(crate) const AIR: usize = 17;
/// The most bytes of card copies of title buffers (`crate::shadow`).
pub(crate) const CARD_BUFFERS_MB: usize = 18;
/// The most bytes of windows in card memory the CPU maps (`crate::shadow`).
pub(crate) const WINDOWS_BAR_MB: usize = 19;
/// Frames begun a second at most (`crate::frame`; 30: the title's own).
pub(crate) const FRAME_CAP: usize = 20;
/// Narrowed GPU barriers (`crate::host::place::narrow_barriers`,
/// trustno1-coder/VIctory-NVIDIA #799); asked by name (`crate::ab::pick`).
#[allow(dead_code)]
pub(crate) const NARROW_BARRIERS: usize = 21;
const COUNT: usize = 22;

fn env_number(name: &str) -> Option<f32> {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|v| v.is_finite())
}

fn env_is(name: &str, on: fn(&OsStr) -> bool) -> f32 {
    f32::from(u8::from(std::env::var_os(name).is_some_and(|v| on(&v))))
}

pub(crate) const SPECS: [Spec; COUNT] = [
    Spec {
        name: "VICTORY_UP_QUEUE",
        label: "Up queue (copy queue)",
        fr: "File de copie (up queue)",
        least: 0.0,
        most: 1.0,
        step: 1.0,
        switch: true,
        start: || env_is("VICTORY_UP_QUEUE", |v| v == "1"),
    },
    Spec {
        name: "VICTORY_SUBMIT_FRAMES",
        label: "Submit frames ahead",
        fr: "Images soumises d'avance",
        least: 0.0,
        most: 4.0,
        step: 1.0,
        switch: false,
        start: || env_number("VICTORY_SUBMIT_FRAMES").unwrap_or(1.0),
    },
    Spec {
        name: "VICTORY_SUBMIT_DEPTH",
        label: "Submit depth (parts)",
        fr: "Profondeur d'envoi (parts)",
        least: 1.0,
        most: 256.0,
        step: 1.0,
        switch: false,
        start: || env_number("VICTORY_SUBMIT_DEPTH").unwrap_or(64.0).max(1.0),
    },
    Spec {
        name: "VICTORY_LAND_THREADS",
        label: "Landing threads",
        fr: "Threads d'atterrissage",
        least: 1.0,
        most: 8.0,
        step: 1.0,
        switch: false,
        start: || crate::land::threads() as f32,
    },
    Spec {
        name: "VICTORY_UPLOAD_AHEAD",
        label: "Upload ahead",
        fr: "Envoi anticipé",
        least: 0.0,
        most: 1.0,
        step: 1.0,
        switch: true,
        start: || env_is("VICTORY_UPLOAD_AHEAD", |v| v != "0"),
    },
    Spec {
        name: "VICTORY_FILL_ON_CARD",
        label: "Fill on card",
        fr: "Remplissage sur la carte",
        least: 0.0,
        most: 1.0,
        step: 1.0,
        switch: true,
        start: || env_is("VICTORY_FILL_ON_CARD", |v| v != "0"),
    },
    Spec {
        name: "VICTORY_PLACED_ORDER",
        label: "Placed order (fill on card: on)",
        fr: "Ordre des placés (remplissage carte : oui)",
        least: 0.0,
        most: 1.0,
        step: 1.0,
        switch: true,
        start: || env_is("VICTORY_PLACED_ORDER", |v| v != "0"),
    },
    Spec {
        name: "VICTORY_PANEL",
        label: "Stats panel (F3)",
        fr: "Panneau de stats (F3)",
        least: 0.0,
        most: 1.0,
        step: 1.0,
        switch: true,
        start: || env_is("VICTORY_PANEL", |v| v != "0"),
    },
    Spec {
        name: "VICTORY_MASKED_PATCH",
        label: "Masked patch (past the merge cap)",
        fr: "Patch masqué (au-delà du plafond de fusion)",
        least: 0.0,
        most: 1.0,
        step: 1.0,
        switch: true,
        start: || env_is("VICTORY_MASKED_PATCH", |v| v != "0"),
    },
    Spec {
        name: "VICTORY_WIDE_COPY",
        label: "Wide copies (1 host, 2 title too)",
        fr: "Copies larges (1 hôte, 2 titre aussi)",
        least: 0.0,
        most: 2.0,
        step: 1.0,
        switch: false,
        start: || env_number("VICTORY_WIDE_COPY").unwrap_or(0.0).clamp(0.0, 2.0),
    },
    Spec {
        name: "VICTORY_WIDE_SYNC",
        label: "Heap pages of all heaps in one pass",
        fr: "Pages de tous les tas en une passe",
        least: 0.0,
        most: 1.0,
        step: 1.0,
        switch: true,
        start: || env_is("VICTORY_WIDE_SYNC", |v| v == "1"),
    },
    Spec {
        name: "VICTORY_MERGE_SWAR",
        label: "Relay merges 8 bytes at a time (fxceless #1421)",
        fr: "Fusions du relais par 8 octets",
        least: 0.0,
        most: 1.0,
        step: 1.0,
        switch: true,
        start: || env_is("VICTORY_MERGE_SWAR", |v| v == "1"),
    },
    Spec {
        name: "VICTORY_DLSS",
        label: "DLSS (off: the title's FidelityFX)",
        fr: "DLSS (non : FidelityFX du jeu)",
        least: 0.0,
        most: 1.0,
        step: 1.0,
        switch: true,
        start: || env_is("VICTORY_DLSS", |v| v == "1"),
    },
    Spec {
        name: "VICTORY_FRAMEGEN_X",
        label: "Frame generation (0 off, 2-6x)",
        fr: "Génération d'images (0 non, 2-6x)",
        least: 0.0,
        most: 6.0,
        step: 1.0,
        switch: false,
        start: crate::streamline::framegen_start,
    },
    Spec {
        name: "VICTORY_AMBIENT",
        label: "Ambient light (0: the title's)",
        fr: "Lumière ambiante (0 : celle du jeu)",
        least: 0.0,
        most: 2.0,
        step: 0.05,
        switch: false,
        start: crate::ambient::level_start,
    },
    Spec {
        name: "VICTORY_AMBIENT_SHADE_STRENGTH",
        label: "Crease shade",
        fr: "Ombre des plis",
        least: 0.0,
        most: 2.0,
        step: 0.05,
        switch: false,
        start: || crate::ambient::shade_start()[0],
    },
    Spec {
        name: "VICTORY_AMBIENT_SHADE_RADIUS",
        label: "Crease radius (m)",
        fr: "Rayon des plis (m)",
        least: 0.1,
        most: 3.0,
        step: 0.05,
        switch: false,
        start: || crate::ambient::shade_start()[1],
    },
    Spec {
        name: "VICTORY_AMBIENT_AIR",
        label: "Air light limit (0: the title's)",
        fr: "Limite de lumière de l'air (0 : celle du jeu)",
        least: 0.0,
        most: 8.0,
        step: 0.1,
        switch: false,
        start: crate::ambient::air_start,
    },
    Spec {
        name: "VICTORY_CARD_BUFFERS_MB",
        label: "Card buffers cap (MB)",
        fr: "Plafond des tampons sur la carte (Mo)",
        least: 256.0,
        most: 8192.0,
        step: 128.0,
        switch: false,
        start: crate::shadow::card_buffers_mb_start,
    },
    Spec {
        name: "VICTORY_WINDOWS_BAR_MB",
        label: "BAR windows cap (MB)",
        fr: "Plafond des fenêtres BAR (Mo, 0 : aucune)",
        least: 0.0,
        most: 8192.0,
        step: 128.0,
        switch: false,
        start: crate::shadow::windows_bar_mb_start,
    },
    Spec {
        name: "VICTORY_FRAME_CAP",
        label: "Frame rate cap (30: the title's)",
        fr: "Plafond d'images/s (30 : celui du jeu)",
        least: 30.0,
        most: 240.0,
        step: 5.0,
        switch: false,
        start: crate::frame::cap_start,
    },
    Spec {
        name: "VICTORY_NARROW_BARRIERS",
        label: "Narrow GPU barriers (trustno1 #799)",
        fr: "Barrières GPU ciblées (trustno1 #799)",
        least: 0.0,
        most: 1.0,
        step: 1.0,
        switch: true,
        start: || f32::from(u8::from(crate::host::place::narrow_barriers_start())),
    },
];

/// Each setting's value as a menu last asked, as it is now (from the last
/// present on), and the run's own, as bits.
static WANT: [AtomicU32; COUNT] = [const { AtomicU32::new(0) }; COUNT];
static NOW: [AtomicU32; COUNT] = [const { AtomicU32::new(0) }; COUNT];
static START: [AtomicU32; COUNT] = [const { AtomicU32::new(0) }; COUNT];
/// Bumped whenever a setting's value changes (at a present).
static GENERATION: [AtomicU64; COUNT] = [const { AtomicU64::new(0) }; COUNT];
/// A setting was changed at a present once at least.
static CHANGED: AtomicBool = AtomicBool::new(false);
/// Writes of the file, one at a time.
static WRITING: Mutex<()> = Mutex::new(());

/// The file `VICTORY_LIVE_FILE` names: `1` the one beside the data folder,
/// any other value a path.
fn path_from(value: &OsStr, data_dir: Option<&OsStr>, local: Option<&OsStr>) -> Option<PathBuf> {
    if value.is_empty() || value == "0" {
        return None;
    }
    if value != "1" {
        return Some(PathBuf::from(value));
    }
    let data = data_dir.filter(|d| !d.is_empty()).map(Path::new);
    let folder = match data {
        Some(d) => d
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(d)
            .to_path_buf(),
        None => Path::new(local?).join("victory"),
    };
    Some(folder.join("live-settings.txt"))
}

fn file() -> Option<&'static PathBuf> {
    static FILE: OnceLock<Option<PathBuf>> = OnceLock::new();
    FILE.get_or_init(|| {
        let value = std::env::var_os("VICTORY_LIVE_FILE")?;
        path_from(
            &value,
            std::env::var_os("VICTORY_DATA_DIR").as_deref(),
            std::env::var_os("LOCALAPPDATA").as_deref(),
        )
    })
    .as_ref()
}

fn beside(suffix: &str) -> Option<PathBuf> {
    let mut path = file()?.clone().into_os_string();
    path.push(suffix);
    Some(path.into())
}

/// Whether `name` is one of [`SPECS`].
pub(crate) fn known(name: &str) -> bool {
    SPECS.iter().any(|s| s.name == name)
}

/// Starts the settings' thread and the overlay once, if a file is set.
pub(crate) fn start() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let Some(path) = file() else { return };
        for (n, spec) in SPECS.iter().enumerate() {
            let v = round(spec, (spec.start)()).to_bits();
            for at in [&WANT[n], &NOW[n], &START[n]] {
                at.store(v, Relaxed);
            }
        }
        if let Some(folder) = path.parent()
            && !folder.as_os_str().is_empty()
        {
            let _ = std::fs::create_dir_all(folder);
        }
        let mut menu = String::new();
        for s in &SPECS {
            menu += &format!(
                "{}|{}|{}|{}|{}|{}|{}\n",
                s.name,
                s.label,
                s.least,
                s.most,
                s.step,
                u8::from(s.switch),
                s.fr
            );
        }
        // (The last run's sizes are not this one's.)
        if let Some(at) = beside(".render") {
            let _ = std::fs::remove_file(at);
        }
        if let Some(at) = beside(".menu")
            && let Err(e) = std::fs::write(&at, menu)
        {
            tracing::warn!(error = %e, path = %at.display(), "live settings: the menu list not written");
        }
        write_file();
        tracing::info!(
            path = %path.display(),
            "live settings: the menus change them at the next present (scripts\\live-menu.ps1; F8 shows the overlay)"
        );
        let _ = std::thread::Builder::new().name("victory-live-settings".into()).spawn(watch);
        overlay::start();
    });
}

/// A setting's value when a menu has changed it from the run's own.
pub(crate) fn get(n: usize) -> Option<f32> {
    file()?;
    start();
    if GENERATION[n].load(Relaxed) == 0 {
        return None;
    }
    let now = NOW[n].load(Relaxed);
    (now != START[n].load(Relaxed)).then(|| f32::from_bits(now))
}

/// Whether setting `n` may change while the title runs: a live file is set,
/// or `VICTORY_AB` / `VICTORY_LIVE_ALT` names it.
pub(crate) fn may_change(n: usize) -> bool {
    file().is_some() || crate::ab::bit(SPECS[n].name).is_some()
}

/// A setting's value when `VICTORY_AB` / `VICTORY_LIVE_ALT` (`crate::ab`) or
/// a menu has it otherwise than the run's own.
pub(crate) fn number(n: usize) -> Option<f32> {
    crate::ab::value(SPECS[n].name).or_else(|| get(n))
}

/// The switch `name` when a menu has changed it from the run's own (for
/// `crate::ab::pick`).
pub(crate) fn switch_named(name: &str) -> Option<bool> {
    if !CHANGED.load(Relaxed) {
        return None;
    }
    let n = SPECS.iter().position(|s| s.name == name)?;
    get(n).map(|v| v >= 0.5)
}

/// Whether a live file is set: the menus' changes are taken at the next
/// present (for the F1 menu, `crate::host::menu`).
pub(crate) fn active() -> bool {
    if file().is_none() {
        return false;
    }
    start();
    true
}

/// Setting `n`'s value as a menu last asked (without a live file, the run's
/// own).
pub(crate) fn wanted(n: usize) -> f32 {
    if active() {
        f32::from_bits(WANT[n].load(Relaxed))
    } else {
        run_value(n)
    }
}

/// Setting `n`'s value at the start of the run.
pub(crate) fn run_value(n: usize) -> f32 {
    if active() {
        f32::from_bits(START[n].load(Relaxed))
    } else {
        round(&SPECS[n], (SPECS[n].start)())
    }
}

/// Asks for setting `n` at `v` from the next present on, as the overlay
/// does (the file is written: the other menus follow). False without a live
/// file.
pub(crate) fn ask(n: usize, v: f32) -> bool {
    if !active() {
        return false;
    }
    set(n, v);
    true
}

/// As [`get`], once for each change, with `seen` the caller's own count.
pub(crate) fn fresh(n: usize, seen: &AtomicU64) -> Option<f32> {
    file()?;
    let generation = GENERATION[n].load(Relaxed);
    if seen.swap(generation, Relaxed) == generation {
        return None;
    }
    Some(f32::from_bits(NOW[n].load(Relaxed)))
}

/// The title's present ends on this thread: what the menus asked for since
/// the last one is taken from here on.
pub(crate) fn presented() {
    if file().is_none() {
        return;
    }
    start();
    for (n, spec) in SPECS.iter().enumerate() {
        let want = WANT[n].load(Relaxed);
        if NOW[n].swap(want, Relaxed) == want {
            continue;
        }
        GENERATION[n].fetch_add(1, Relaxed);
        CHANGED.store(true, Relaxed);
        let overridden = crate::ab::value(spec.name).is_some();
        tracing::info!(
            setting = spec.name,
            value = f32::from_bits(want),
            run = f32::from_bits(START[n].load(Relaxed)),
            "live settings: changed{}",
            if overridden {
                " (VICTORY_AB / VICTORY_LIVE_ALT has it: no effect)"
            } else {
                ""
            }
        );
    }
}

/// For the frame-rate line: the `crate::ab` phase, and the settings the
/// menus have otherwise than the run's own.
pub(crate) fn tag() -> Option<String> {
    let mut parts: Vec<String> = crate::ab::tag().into_iter().collect();
    if file().is_some() && CHANGED.load(Relaxed) {
        let changed: Vec<String> = (0..COUNT)
            .filter_map(|n| get(n).map(|v| format!("{}={v}", SPECS[n].name)))
            .collect();
        if !changed.is_empty() {
            parts.push(format!("live [{}]", changed.join(" ")));
        }
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

fn round(spec: &Spec, v: f32) -> f32 {
    let v = v.clamp(spec.least, spec.most);
    let steps = ((v - spec.least) / spec.step).round();
    // (Two places past the step's, so 0.05 steps do not print 0.150000006.)
    let v = (spec.least + steps * spec.step).clamp(spec.least, spec.most);
    (v * 1000.0).round() / 1000.0
}

/// Sets setting `n` (a menu of ours) and writes the file.
fn set(n: usize, v: f32) {
    if store(n, v) {
        write_file();
    }
}

/// True if the value asked for changed.
fn store(n: usize, v: f32) -> bool {
    let v = round(&SPECS[n], v);
    WANT[n].swap(v.to_bits(), Relaxed) != v.to_bits()
}

/// The settings a file's text sets: `NAME=value` lines (spaces around
/// either allowed; `#` lines, unknown names and values that are not finite
/// numbers left out), each as the index of its [`Spec`].
fn parse(text: &str) -> Vec<(usize, f32)> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|line| {
            let (name, value) = line.split_once('=')?;
            let n = SPECS.iter().position(|s| s.name == name.trim())?;
            let v = value.trim().parse::<f32>().ok().filter(|v| v.is_finite())?;
            Some((n, v))
        })
        .collect()
}

fn text_of(values: impl Iterator<Item = f32>) -> String {
    let mut text = String::from("# VIctory live settings: NAME=value (VICTORY_LIVE_FILE)\n");
    for (spec, v) in SPECS.iter().zip(values) {
        text += &format!("{}={}\n", spec.name, v);
    }
    text
}

fn write_file() {
    let Some(path) = file() else { return };
    let _one = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let text = text_of(WANT.iter().map(|v| f32::from_bits(v.load(Relaxed))));
    // Whole or not at all: a reader never sees half of it.
    let mut part = path.clone().into_os_string();
    part.push(".part");
    let part = PathBuf::from(part);
    let written = std::fs::write(&part, &text).and_then(|()| std::fs::rename(&part, path));
    if written.is_err() {
        // (Another reader held it open: in place.)
        let _ = std::fs::remove_file(&part);
        if let Err(e) = std::fs::write(path, &text) {
            tracing::warn!(error = %e, "live settings: the file not written");
        }
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// The title's last upscaling: its render size and output size (width in
/// the high half, height in the low one), and whether DLSS took it.
static RENDER: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
static RENDER_DLSS: AtomicBool = AtomicBool::new(false);

/// An upscaling the title asked for (`crate::streamline::upscale`): the
/// sizes go to `<file>.render` (`render_w render_h output_w output_h dlss`),
/// which the window of `scripts\live-menu.ps1` shows, as victory-corgi's
/// loader writes it.
pub(crate) fn render_sizes(render: [u32; 2], output: [u32; 2], dlss: bool) {
    if file().is_none() {
        return;
    }
    let pack = |s: [u32; 2]| (u64::from(s[0]) << 32) | u64::from(s[1]);
    RENDER[0].store(pack(render), Relaxed);
    RENDER[1].store(pack(output), Relaxed);
    RENDER_DLSS.store(dlss, Relaxed);
}

/// The text of `<file>.render` for [`RENDER`], if an upscaling was seen.
fn render_text() -> Option<String> {
    let [render, output] = [&RENDER[0], &RENDER[1]].map(|r| r.load(Relaxed));
    if render == 0 || output == 0 {
        return None;
    }
    let size = |s: u64| format!("{} {}", s >> 32, s & 0xffff_ffff);
    Some(format!(
        "{} {} {}\n",
        size(render),
        size(output),
        u8::from(RENDER_DLSS.load(Relaxed))
    ))
}

/// The settings whose run value is known only once the host is up: worked
/// out again then, while no menu has changed them (the card's budget picks
/// the card buffers' default cap, `crate::shadow::card_buffers_mb_start`).
/// False while the host is not up.
fn late_starts() -> bool {
    if crate::host::host().is_none() {
        return false;
    }
    let mut changed = false;
    for n in [CARD_BUFFERS_MB] {
        let start = START[n].load(Relaxed);
        if GENERATION[n].load(Relaxed) != 0 || WANT[n].load(Relaxed) != start {
            continue;
        }
        let v = round(&SPECS[n], (SPECS[n].start)()).to_bits();
        if v != start {
            for at in [&WANT[n], &NOW[n], &START[n]] {
                at.store(v, Relaxed);
            }
            changed = true;
        }
    }
    if changed {
        write_file();
    }
    true
}

/// The thread: the file, read again when it changes, and the title's render
/// sizes written when they change.
fn watch() {
    let Some(path) = file() else { return };
    let mut seen = modified(path);
    let render = beside(".render");
    let mut render_written = String::new();
    let mut late = false;
    loop {
        std::thread::sleep(POLL);
        if !late {
            late = late_starts();
        }
        if let (Some(at), Some(text)) = (render.as_ref(), render_text())
            && text != render_written
        {
            if let Err(e) = std::fs::write(at, &text) {
                tracing::warn!(error = %e, "live settings: the render sizes not written");
            }
            render_written = text;
        }
        let now = modified(path);
        if now == seen {
            continue;
        }
        seen = now;
        if let Ok(text) = std::fs::read_to_string(path) {
            for (n, v) in parse(&text) {
                store(n, v);
            }
        }
    }
}

/// The overlay: a window of its own over the game's top right corner, on its
/// own thread, shown and hidden with F8. It takes clicks but never the
/// keyboard (the game keeps it).
mod overlay {
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering::Relaxed};

    use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, ReleaseCapture, SetCapture, VK_F8,
    };
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::w;

    use super::{SPECS, set};

    static VISIBLE: AtomicBool = AtomicBool::new(false);
    /// The row a bar is dragged on, plus one (0: none).
    static DRAGGING: AtomicIsize = AtomicIsize::new(0);
    const WIDTH: i32 = 470;
    const TOP: i32 = 52;
    const ROW: i32 = 30;
    const BAR: (i32, i32) = (230, 390);

    fn height() -> i32 {
        TOP + ROW * SPECS.len() as i32 + 30
    }

    pub(super) fn start() {
        let _ = std::thread::Builder::new()
            .name("victory-live-overlay".into())
            .spawn(run);
    }

    fn run() {
        // SAFETY: our class and window, used on this thread only.
        unsafe {
            let Ok(instance) = GetModuleHandleW(None) else {
                return;
            };
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(proc),
                hInstance: instance.into(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpszClassName: w!("VIctoryLiveSettings"),
                ..Default::default()
            };
            RegisterClassExW(&class);
            let Ok(hwnd) = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("VIctoryLiveSettings"),
                w!("VIctory live settings"),
                WS_POPUP,
                0,
                0,
                WIDTH,
                height(),
                None,
                None,
                Some(instance.into()),
                None,
            ) else {
                tracing::warn!("live settings: no overlay window");
                return;
            };
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 230, LWA_ALPHA);
            SetTimer(Some(hwnd), 1, 100, None);
            let mut message = MSG::default();
            while GetMessageW(&mut message, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }

    /// The row under `y` and the value under `x` on its bar.
    fn at(lparam: LPARAM) -> (Option<usize>, i32) {
        let x = i32::from((lparam.0 & 0xffff) as u16 as i16);
        let y = i32::from(((lparam.0 >> 16) & 0xffff) as u16 as i16);
        (row(y), x)
    }

    fn row(y: i32) -> Option<usize> {
        (y >= TOP)
            .then(|| ((y - TOP) / ROW) as usize)
            .filter(|&r| r < SPECS.len())
    }

    fn value_at(n: usize, x: i32) -> f32 {
        let s = &SPECS[n];
        let t = ((x - BAR.0) as f32 / (BAR.1 - BAR.0) as f32).clamp(0.0, 1.0);
        s.least + t * (s.most - s.least)
    }

    /// What a menu asked for last.
    fn want(n: usize) -> f32 {
        f32::from_bits(super::WANT[n].load(Relaxed))
    }

    extern "system" fn proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        // SAFETY: messages of our window, on its thread.
        unsafe {
            match message {
                WM_TIMER => {
                    keys();
                    follow(hwnd);
                    if VISIBLE.load(Relaxed) {
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                    LRESULT(0)
                }
                WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
                WM_LBUTTONDOWN => {
                    if let (Some(n), x) = at(lparam) {
                        if SPECS[n].switch {
                            set(n, if want(n) >= 0.5 { 0.0 } else { 1.0 });
                        } else if x >= BAR.0 - 8 {
                            DRAGGING.store(n as isize + 1, Relaxed);
                            SetCapture(hwnd);
                            set(n, value_at(n, x));
                        }
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                    LRESULT(0)
                }
                WM_MOUSEMOVE => {
                    let dragged = DRAGGING.load(Relaxed);
                    if dragged > 0 {
                        let (_, x) = at(lparam);
                        set(dragged as usize - 1, value_at(dragged as usize - 1, x));
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                    LRESULT(0)
                }
                WM_LBUTTONUP => {
                    if DRAGGING.swap(0, Relaxed) > 0 {
                        let _ = ReleaseCapture();
                    }
                    LRESULT(0)
                }
                WM_RBUTTONDOWN => {
                    if let (Some(n), _) = at(lparam) {
                        set(n, f32::from_bits(super::START[n].load(Relaxed)));
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                    LRESULT(0)
                }
                WM_MOUSEWHEEL => {
                    // (The wheel's message has the point on the screen.)
                    let mut point = POINT {
                        x: i32::from((lparam.0 & 0xffff) as u16 as i16),
                        y: i32::from(((lparam.0 >> 16) & 0xffff) as u16 as i16),
                    };
                    let _ = ScreenToClient(hwnd, &mut point);
                    if let Some(n) = row(point.y) {
                        let turns = ((wparam.0 >> 16) & 0xffff) as u16 as i16;
                        let by = if turns > 0 {
                            SPECS[n].step
                        } else {
                            -SPECS[n].step
                        };
                        set(n, want(n) + by);
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                    LRESULT(0)
                }
                WM_PAINT => {
                    paint(hwnd);
                    LRESULT(0)
                }
                WM_ERASEBKGND => LRESULT(1),
                _ => DefWindowProcW(hwnd, message, wparam, lparam),
            }
        }
    }

    /// F8, with one of our windows in front.
    fn keys() {
        static DOWN: AtomicBool = AtomicBool::new(false);
        // SAFETY: a plain query.
        let down = unsafe { GetAsyncKeyState(i32::from(VK_F8.0)) } < 0;
        if DOWN.swap(down, Relaxed) || !down {
            return;
        }
        let mut process = 0;
        // SAFETY: plain queries.
        unsafe { GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut process)) };
        if process == std::process::id() {
            VISIBLE.fetch_xor(true, Relaxed);
        }
    }

    /// Over the game window's top right corner, shown or hidden.
    unsafe fn follow(hwnd: HWND) {
        let game = if VISIBLE.load(Relaxed) {
            game_window(hwnd)
        } else {
            None
        };
        // SAFETY: our window; the game's is only asked where it is.
        unsafe {
            match game {
                Some(game) => {
                    let mut r = RECT::default();
                    let _ = GetClientRect(game, &mut r);
                    let mut at = POINT {
                        x: r.right - WIDTH - 8,
                        y: 8,
                    };
                    let _ = ClientToScreen(game, &mut at);
                    let _ = SetWindowPos(
                        hwnd,
                        Some(HWND_TOPMOST),
                        at.x,
                        at.y,
                        WIDTH,
                        height(),
                        SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    );
                }
                None => {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
            }
        }
    }

    /// The game's window: the largest visible top-level window of this
    /// process that is not `ours`.
    fn game_window(ours: HWND) -> Option<HWND> {
        struct Found {
            best: Option<(HWND, i64)>,
            skip: HWND,
        }
        extern "system" fn visit(hwnd: HWND, found: LPARAM) -> windows::core::BOOL {
            // SAFETY: `found` is ours; a window handle from the enumeration.
            unsafe {
                let found = &mut *(found.0 as *mut Found);
                let mut process = 0;
                GetWindowThreadProcessId(hwnd, Some(&mut process));
                if process != std::process::id()
                    || !IsWindowVisible(hwnd).as_bool()
                    || hwnd == found.skip
                {
                    return true.into();
                }
                let mut r = RECT::default();
                if GetClientRect(hwnd, &mut r).is_ok() {
                    let area = i64::from(r.right - r.left) * i64::from(r.bottom - r.top);
                    if found.best.is_none_or(|(_, a)| area > a) {
                        found.best = Some((hwnd, area));
                    }
                }
                true.into()
            }
        }
        let mut found = Found {
            best: None,
            skip: ours,
        };
        // SAFETY: `found` outlives the enumeration.
        unsafe {
            let _ = EnumWindows(Some(visit), LPARAM(&mut found as *mut Found as isize));
        }
        found.best.map(|(hwnd, _)| hwnd)
    }

    fn text(dc: HDC, s: &str, r: RECT, flags: DRAW_TEXT_FORMAT) {
        let mut wide: Vec<u16> = s.encode_utf16().collect();
        let mut r = r;
        // SAFETY: drawing on a device context of our window.
        unsafe {
            DrawTextW(
                dc,
                &mut wide,
                &mut r,
                flags | DT_NOPREFIX | DT_SINGLELINE | DT_VCENTER,
            )
        };
    }

    fn fill(dc: HDC, r: RECT, color: u32) {
        // SAFETY: a brush of ours, deleted after use.
        unsafe {
            let brush = CreateSolidBrush(COLORREF(color));
            FillRect(dc, &r, brush);
            let _ = DeleteObject(brush.into());
        }
    }

    unsafe fn paint(hwnd: HWND) {
        let mut ps = PAINTSTRUCT::default();
        let (w, h) = (WIDTH, height());
        // SAFETY: painting our window between BeginPaint and EndPaint, into
        // a bitmap of ours first (no flicker).
        unsafe {
            let screen = BeginPaint(hwnd, &mut ps);
            let dc = CreateCompatibleDC(Some(screen));
            let bitmap = CreateCompatibleBitmap(screen, w, h);
            let old_bitmap = SelectObject(dc, bitmap.into());
            fill(
                dc,
                RECT {
                    left: 0,
                    top: 0,
                    right: w,
                    bottom: h,
                },
                0x0018_1410,
            );
            let font = CreateFontW(
                16,
                0,
                0,
                0,
                FW_NORMAL.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                (FIXED_PITCH.0 | FF_MODERN.0) as u32,
                w!("Consolas"),
            );
            let old_font = SelectObject(dc, font.into());
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, COLORREF(0x0078_FF64));
            text(
                dc,
                "VIctory live settings   F8 hides",
                RECT {
                    left: 10,
                    top: 4,
                    right: w,
                    bottom: 24,
                },
                DT_LEFT,
            );
            SetTextColor(dc, COLORREF(0x00B4_B4B4));
            let status = crate::ab::tag().unwrap_or_else(|| "taken at the next present".to_owned());
            text(
                dc,
                &status,
                RECT {
                    left: 10,
                    top: 24,
                    right: w,
                    bottom: 44,
                },
                DT_LEFT | DT_END_ELLIPSIS,
            );
            for (n, spec) in SPECS.iter().enumerate() {
                let y = TOP + ROW * n as i32;
                let v = want(n);
                let changed = super::WANT[n].load(Relaxed) != super::START[n].load(Relaxed);
                SetTextColor(
                    dc,
                    COLORREF(if changed { 0x0040_D0FF } else { 0x00E6_E6E6 }),
                );
                text(
                    dc,
                    spec.label,
                    RECT {
                        left: 10,
                        top: y,
                        right: BAR.0 - 6,
                        bottom: y + ROW,
                    },
                    DT_LEFT,
                );
                let mid = y + ROW / 2;
                if spec.switch {
                    let on = v >= 0.5;
                    fill(
                        dc,
                        RECT {
                            left: BAR.0,
                            top: mid - 8,
                            right: BAR.0 + 40,
                            bottom: mid + 8,
                        },
                        if on { 0x0050_B040 } else { 0x0050_5050 },
                    );
                    text(
                        dc,
                        if on { "on" } else { "off" },
                        RECT {
                            left: BAR.0 + 48,
                            top: y,
                            right: w,
                            bottom: y + ROW,
                        },
                        DT_LEFT,
                    );
                    continue;
                }
                let t = ((v - spec.least) / (spec.most - spec.least)).clamp(0.0, 1.0);
                let x = BAR.0 + (t * (BAR.1 - BAR.0) as f32) as i32;
                fill(
                    dc,
                    RECT {
                        left: BAR.0,
                        top: mid - 3,
                        right: BAR.1,
                        bottom: mid + 3,
                    },
                    0x0050_5050,
                );
                fill(
                    dc,
                    RECT {
                        left: BAR.0,
                        top: mid - 3,
                        right: x,
                        bottom: mid + 3,
                    },
                    0x00D0_9040,
                );
                fill(
                    dc,
                    RECT {
                        left: x - 4,
                        top: mid - 9,
                        right: x + 4,
                        bottom: mid + 9,
                    },
                    0x00F0_F0F0,
                );
                let shown = if spec.step >= 1.0 {
                    format!("{v:.0}")
                } else if spec.step >= 0.1 {
                    format!("{v:.1}")
                } else {
                    format!("{v:.2}")
                };
                text(
                    dc,
                    &shown,
                    RECT {
                        left: BAR.1 + 10,
                        top: y,
                        right: w,
                        bottom: y + ROW,
                    },
                    DT_LEFT,
                );
            }
            SetTextColor(dc, COLORREF(0x0090_9090));
            text(
                dc,
                "drag or wheel: change   right click: the run's value",
                RECT {
                    left: 10,
                    top: h - 28,
                    right: w,
                    bottom: h - 4,
                },
                DT_LEFT,
            );
            let _ = BitBlt(screen, 0, 0, w, h, Some(dc), 0, 0, SRCCOPY);
            SelectObject(dc, old_font);
            let _ = DeleteObject(font.into());
            SelectObject(dc, old_bitmap);
            let _ = DeleteObject(bitmap.into());
            let _ = DeleteDC(dc);
            let _ = EndPaint(hwnd, &ps);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UP_QUEUE: usize = 0;

    #[test]
    fn the_file_is_read() {
        let text = "# VIctory live settings\n\
                    VICTORY_UP_QUEUE=1\n\
                    \x20 VICTORY_SUBMIT_FRAMES = 2 \n\
                    VICTORY_NONE_SUCH=3\n\
                    VICTORY_LAND_THREADS=x\n\
                    VICTORY_SUBMIT_DEPTH=inf\n\
                    #VICTORY_PANEL=1\n\
                    no equals sign\n\
                    VICTORY_PANEL=0.0\r\n";
        assert_eq!(
            parse(text),
            vec![(UP_QUEUE, 1.0), (SUBMIT_FRAMES, 2.0), (PANEL, 0.0)]
        );
        assert!(parse("").is_empty());
    }

    #[test]
    fn what_is_written_is_read_back() {
        let values: Vec<f32> = (0..COUNT).map(|n| n as f32).collect();
        let read = parse(&text_of(values.iter().copied()));
        assert_eq!(read, values.iter().copied().enumerate().collect::<Vec<_>>());
    }

    #[test]
    fn values_are_kept_within_their_steps() {
        let depth = &SPECS[SUBMIT_DEPTH];
        assert_eq!(round(depth, 0.0), 1.0);
        assert_eq!(round(depth, 9999.0), 256.0);
        assert_eq!(round(depth, 7.4), 7.0);
        assert_eq!(round(&SPECS[UP_QUEUE], 0.7), 1.0);
        assert_eq!(round(&SPECS[LAND_THREADS], 3.5), 4.0);
    }

    #[test]
    fn the_menu_names_are_known_and_unique() {
        for (n, s) in SPECS.iter().enumerate() {
            assert!(known(s.name));
            assert!(s.least < s.most && s.step > 0.0, "{}", s.name);
            assert!(!s.label.contains('|') && !s.name.contains('|'));
            assert!(!s.fr.is_empty() && !s.fr.contains('|') && !s.fr.contains('\n'), "{}", s.name);
            assert!(SPECS[n + 1..].iter().all(|o| o.name != s.name));
        }
        assert!(!known("VICTORY_CARD_ALL"));
        let names = [
            (UP_QUEUE, "VICTORY_UP_QUEUE"),
            (SUBMIT_FRAMES, "VICTORY_SUBMIT_FRAMES"),
            (SUBMIT_DEPTH, "VICTORY_SUBMIT_DEPTH"),
        ];
        let more = [
            (LAND_THREADS, "VICTORY_LAND_THREADS"),
            (4, "VICTORY_UPLOAD_AHEAD"),
            (5, "VICTORY_FILL_ON_CARD"),
        ];
        for (n, name) in names
            .into_iter()
            .chain(more)
            .chain([(6, "VICTORY_PLACED_ORDER"), (PANEL, "VICTORY_PANEL")])
            .chain([(MASKED_PATCH, "VICTORY_MASKED_PATCH")])
            .chain([(WIDE_COPY, "VICTORY_WIDE_COPY"), (10, "VICTORY_WIDE_SYNC")])
        {
            assert_eq!(SPECS[n].name, name);
        }
        // victory-corgi's window's settings, under the names this tree reads.
        let corgi = [
            (AMBIENT, "VICTORY_AMBIENT"),
            (SHADE_STRENGTH, "VICTORY_AMBIENT_SHADE_STRENGTH"),
            (SHADE_RADIUS, "VICTORY_AMBIENT_SHADE_RADIUS"),
            (AIR, "VICTORY_AMBIENT_AIR"),
            (CARD_BUFFERS_MB, "VICTORY_CARD_BUFFERS_MB"),
            (WINDOWS_BAR_MB, "VICTORY_WINDOWS_BAR_MB"),
            (FRAME_CAP, "VICTORY_FRAME_CAP"),
        ];
        for (n, name) in corgi {
            assert_eq!(SPECS[n].name, name);
        }
    }

    #[test]
    fn render_sizes_are_written_as_the_window_reads_them() {
        assert_eq!(render_text(), None);
        // (Without a live file nothing is kept: the test sets them itself.)
        let pack = |s: [u64; 2]| (s[0] << 32) | s[1];
        RENDER[0].store(pack([1706, 960]), Relaxed);
        RENDER[1].store(pack([2560, 1440]), Relaxed);
        RENDER_DLSS.store(true, Relaxed);
        assert_eq!(render_text().as_deref(), Some("1706 960 2560 1440 1\n"));
        for r in &RENDER {
            r.store(0, Relaxed);
        }
    }

    #[test]
    fn the_frame_cap_steps_within_its_range() {
        let cap = &SPECS[FRAME_CAP];
        assert_eq!(round(cap, 1000.0), 240.0);
        assert_eq!(round(cap, 59.0), 60.0);
        assert_eq!(round(&SPECS[AMBIENT], 0.81), 0.8);
        assert_eq!(round(&SPECS[CARD_BUFFERS_MB], 2048.0), 2048.0);
    }

    #[test]
    fn the_file_is_found() {
        let os = |s: &'static str| Some(OsStr::new(s));
        assert_eq!(
            path_from(OsStr::new("D:\\x\\live.txt"), None, None),
            Some(PathBuf::from("D:\\x\\live.txt"))
        );
        assert_eq!(path_from(OsStr::new("0"), os("D:\\d"), None), None);
        assert_eq!(path_from(OsStr::new(""), os("D:\\d"), None), None);
        assert_eq!(
            path_from(
                OsStr::new("1"),
                os("G:\\GTA VI\\victory-gta6-data\\data"),
                os("C:\\L")
            ),
            Some(PathBuf::from(
                "G:\\GTA VI\\victory-gta6-data\\live-settings.txt"
            ))
        );
        assert_eq!(
            path_from(OsStr::new("1"), None, os("C:\\L")),
            Some(Path::new("C:\\L").join("victory").join("live-settings.txt"))
        );
        assert_eq!(path_from(OsStr::new("1"), None, None), None);
    }
}
