//! The F1 settings menu, in every build. F1 shows and hides it while our
//! window has the focus; Up/Down pick a line, Left/Right change a setting,
//! Enter runs the line picked (save and restart, save, forget, close), Esc
//! closes. While it is shown the title gets no keyboard
//! (`crate::SETTINGS_MENU_OPEN`, read by the input module); a pad still
//! works.
//!
//! Three kinds of settings:
//! - LIVE: the switches of `crate::live` (those the F8 overlay and
//!   `scripts\live-menu.ps1` change), taken at the next present when the run
//!   has a live file (`VICTORY_LIVE_FILE`), and the menu's own (the frame-rate
//!   line, the stats panel), at once in any run;
//! - frame generation, live when a frame generation port says so
//!   ([`frame_generation_is`]);
//! - the others are read once at the start: a change is saved for the next
//!   start (console profile, picture size, DLSS, frame generation, frame-rate
//!   cap, window and input: fullscreen, mouse capture, English keyboard).
//!
//! "SAVE FOR NEXT START" writes what changed into the file
//! `VICTORY_MENU_FILE` names, `NAME=value` lines (an empty value: unset), over
//! what it held; `scripts\play-coolfox.ps1` reads it at every start.
//! `VICTORY_MENU_CONSOLE` (`series-s`, `series-x`) is the launcher's
//! `--console`; the others are the switches themselves. "SAVE AND RESTART
//! NOW" starts the launcher again (`VICTORY_MENU_PLAY`, with
//! `VICTORY_MENU_ARGS`) and ends this run.
//!
//! The frame-rate line ("FPS: 21.5", top left; `VICTORY_FPS_LINE=0`: not
//! shown) gives way to the stats panel (F3), which says the same.
//!
//! After trustno1-coder/VIctory-NVIDIA (`host/menu.rs`, 6395727, 22c6be2,
//! 35c1560, 06afe63, 48ad785), here with the live settings of this tree.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_DOWN, VK_ESCAPE, VK_F1, VK_LEFT, VK_RETURN, VK_RIGHT, VK_UP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

use super::textbox::{Place, Shape, TextBox, drawable};
use super::{Host, HostList};

const COLUMNS: u32 = 60;
/// Settings and actions shown at a time (the list scrolls).
const ROWS: usize = 20;
/// A title, a help line, a blank, the rows, a blank and a note.
const MENU: Shape = Shape { columns: COLUMNS, lines: ROWS as u32 + 5, rim: 4 };
const FPS: Shape = Shape { columns: 24, lines: 1, rim: 3 };
/// The label's width on a line.
const LABEL: usize = 26;

/// Frame generation, as a port of it tells the menu ([`frame_generation_is`]).
pub(crate) struct FrameGeneration {
    /// Whether it can be switched and changed while the title runs.
    pub(crate) live: fn() -> bool,
    /// Off (0) or `n` frames shown for each one rendered, from the next present on.
    pub(crate) set: fn(u32),
    /// Frames shown for each one rendered now (1 while it is off).
    pub(crate) shown_per_frame: fn() -> u32,
}

static FRAME_GENERATION: OnceLock<FrameGeneration> = OnceLock::new();

/// The frame generation the menu drives: DLSS Frame Generation through
/// Streamline (`crate::streamline`) unless another port said otherwise.
fn frame_generation() -> Option<&'static FrameGeneration> {
    Some(FRAME_GENERATION.get_or_init(|| FrameGeneration {
        live: crate::streamline::framegen_live,
        set: crate::streamline::set_framegen,
        shown_per_frame: crate::streamline::shown_per_frame,
    }))
}

/// Called once by a frame generation port (DLSS Frame Generation through
/// Streamline): the menu's FRAME GENERATION lines then change it while the
/// title runs, and the frame-rate texts say what is shown. Without it they
/// are saved for the next start only.
#[allow(dead_code)]
pub(crate) fn frame_generation_is(hooks: FrameGeneration) {
    let _ = FRAME_GENERATION.set(hooks);
}

/// Frames shown for each one rendered (1 without frame generation).
pub(crate) fn shown_per_frame() -> u32 {
    frame_generation().map_or(1, |f| (f.shown_per_frame)().max(1))
}

fn frame_generation_live() -> bool {
    frame_generation().is_some_and(|f| (f.live)())
}

/// The frame rate the meter last counted, in tenths of a frame a second (0:
/// none yet), for the frame-rate line.
pub(crate) static METER_FPS_TENTHS: AtomicU32 = AtomicU32::new(0);

/// How a setting's change takes effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum How {
    /// At the next start.
    Restart,
    /// The menu's own: at once in any run.
    Own,
    /// One of `crate::live`'s: at the next present with a live file.
    Live(usize),
    /// Frame generation: live when its port says so.
    FrameGeneration,
}

/// A setting: its variable, its name in the menu, the values it takes
/// (`""`: not set, the build's own default) and their names.
struct Item {
    var: &'static str,
    label: String,
    values: Vec<(String, String)>,
    how: How,
}

const OFF_ON: &[(&str, &str)] = &[("", "OFF"), ("1", "ON")];

/// The settings read once at the start (and the menu's own two), in the
/// menu's order: what is played with first.
#[rustfmt::skip]
const CHOICES: &[(&str, &str, &[(&str, &str)], How)] = &[
    ("VICTORY_MENU_CONSOLE", "CONSOLE PROFILE", &[("series-s", "SERIES S (FASTER)"), ("series-x", "SERIES X")], How::Restart),
    ("VICTORY_NATIVE_1440P", "SERIES X NATIVE 1440P", &[("", "OFF (4K OUTPUT)"), ("1", "ON (2560X1440)")], How::Restart),
    ("VICTORY_DLSS", "DLSS", OFF_ON, How::Restart),
    ("VICTORY_DLSS_MODE", "DLSS MODE", &[("", "DEFAULT"), ("quality", "QUALITY"), ("balanced", "BALANCED"), ("performance", "PERFORMANCE"), ("ultraperformance", "ULTRA PERFORMANCE"), ("dlaa", "DLAA (NATIVE)")], How::Restart),
    ("VICTORY_FRAMEGEN", "FRAME GENERATION", OFF_ON, How::FrameGeneration),
    ("VICTORY_FRAMEGEN_X", "FRAME GEN FACTOR", &[("", "DEFAULT (2X)"), ("2", "2X"), ("3", "3X"), ("4", "4X"), ("5", "5X"), ("6", "6X (MOST)"), ("0", "0 (READY, OFF)")], How::FrameGeneration),
    ("VICTORY_FRAME_INTERVAL_US", "FRAME RATE CAP", &[("", "30 (THE GAME)"), ("16667", "60"), ("8333", "120"), ("1000", "NONE")], How::Restart),
    // The relay lands flights on a thread of its own: ~2.5 fps more in the
    // city, but it can put older words over the title's (stretched meshes,
    // black streaks on clothes); off: the picture is clean.
    ("VICTORY_RELAY", "RELAY", &[("1", "ON (FASTER)"), ("0", "OFF (CLEAN PICTURE)")], How::Restart),
    ("VICTORY_FPS_LINE", "FPS AT THE TOP", &[("1", "ON"), ("0", "OFF")], How::Own),
    ("VICTORY_PANEL", "STATS PANEL (F3)", &[("0", "OFF"), ("1", "ON")], How::Own),
    // Window and input (superboynick's, trustno1-coder 06afe63; victory-hle's lifecycle.rs).
    ("VICTORY_FULLSCREEN", "FULLSCREEN (NO BORDER)", OFF_ON, How::Restart),
    ("VICTORY_MOUSE_CAPTURE", "MOUSE CAPTURE", OFF_ON, How::Restart),
    ("VICTORY_IME_ENGLISH", "ENGLISH KEYBOARD", OFF_ON, How::Restart),
];

/// Shorter names than the live settings' own for some of them.
const SHORT: &[(&str, &str)] = &[
    ("VICTORY_WIDE_COPY", "WIDE COPIES (0-2)"),
    ("VICTORY_WIDE_SYNC", "HEAP PAGES IN ONE PASS"),
    ("VICTORY_MERGE_SWAR", "RELAY MERGES 8 BYTES"),
];

/// The lines after the settings.
const ACTIONS: [&str; 4] = ["SAVE AND RESTART NOW", "SAVE FOR NEXT START", "FORGET SAVED SETTINGS", "CLOSE"];

/// A label as the letters can show it: upper case, what is in brackets left
/// out, no wider than the label's column.
fn label_of(label: &str) -> String {
    let head = label.split('(').next().unwrap_or(label).trim();
    let mut out: String = head
        .to_ascii_uppercase()
        .bytes()
        .map(|c| if drawable(c) && c != b'#' { c as char } else { ' ' })
        .collect();
    out.truncate(LABEL);
    out
}

/// A live setting's value as written in a file.
fn number(v: f32) -> String {
    format!("{v}")
}

/// The values a live setting steps through: every step, or for a wide range
/// the run's value and doubles.
fn live_values(n: usize) -> Vec<(String, String)> {
    let spec = &crate::live::SPECS[n];
    if spec.switch {
        return vec![("0".into(), "OFF".into()), ("1".into(), "ON".into())];
    }
    let steps = ((spec.most - spec.least) / spec.step).round() as usize;
    let mut values: Vec<f32> = if steps <= 32 {
        (0..=steps).map(|i| spec.least + i as f32 * spec.step).collect()
    } else {
        let mut v = vec![spec.least];
        let mut at = spec.least.max(spec.step) * 2.0;
        while at < spec.most {
            v.push(at);
            at *= 2.0;
        }
        v.push(spec.most);
        v.push(crate::live::run_value(n));
        v.sort_by(f32::total_cmp);
        v.dedup();
        v
    };
    values.retain(|v| v.is_finite());
    values
        .into_iter()
        .map(|v| (number(v), number(v)))
        .collect()
}

fn items() -> Vec<Item> {
    let mut items: Vec<Item> = CHOICES
        .iter()
        .map(|&(var, label, values, how)| Item {
            var,
            label: label.to_owned(),
            values: values.iter().map(|&(v, s)| (v.to_owned(), s.to_owned())).collect(),
            how,
        })
        .collect();
    for (n, spec) in crate::live::SPECS.iter().enumerate() {
        // (The stats panel is above, as the menu's own.)
        if n == crate::live::PANEL {
            continue;
        }
        // (DLSS and the frame generation factor are already lines of their
        // own above, which also save them for the next start.)
        if items.iter().any(|i| i.var == spec.name) {
            continue;
        }
        let label = SHORT
            .iter()
            .find(|(var, _)| *var == spec.name)
            .map_or_else(|| label_of(spec.label), |(_, s)| (*s).to_owned());
        items.push(Item { var: spec.name, label, values: live_values(n), how: How::Live(n) });
    }
    items
}

/// Whether a change of `item` shows in this run.
fn live(item: &Item) -> bool {
    match item.how {
        How::Restart => false,
        How::Own => true,
        How::Live(_) => crate::live::active(),
        How::FrameGeneration => frame_generation_live(),
    }
}

/// An item's value now: the variable's for those read once, the live
/// setting's as a menu last asked, the menu's own.
fn current(item: &Item) -> String {
    match item.how {
        How::Live(n) => number(crate::live::wanted(n)),
        How::Own if item.var == "VICTORY_PANEL" => if super::panel::shown().load(Ordering::Relaxed) { "1" } else { "0" }.into(),
        How::Own if item.var == "VICTORY_FPS_LINE" => if fps_line_on().load(Ordering::Relaxed) { "1" } else { "0" }.into(),
        _ => start_value(item),
    }
}

/// An item's value at the start of the run.
fn start_value(item: &Item) -> String {
    match item.how {
        How::Live(n) => number(crate::live::run_value(n)),
        _ if item.var == "VICTORY_MENU_CONSOLE" => match crate::console::current() {
            crate::console::Console::SeriesS => "series-s".into(),
            crate::console::Console::SeriesX => "series-x".into(),
        },
        _ if item.var == "VICTORY_PANEL" => if crate::switch::on("VICTORY_PANEL") { "1" } else { "0" }.into(),
        _ if item.var == "VICTORY_FPS_LINE" => if fps_line_wanted() { "1" } else { "0" }.into(),
        _ => std::env::var(item.var).unwrap_or_default().trim().to_owned(),
    }
}

fn fps_line_wanted() -> bool {
    std::env::var_os("VICTORY_FPS_LINE").is_none_or(|v| v != "0")
}

/// Whether the frame-rate line is shown while the menu is closed.
fn fps_line_on() -> &'static AtomicBool {
    static ON: OnceLock<AtomicBool> = OnceLock::new();
    ON.get_or_init(|| AtomicBool::new(fps_line_wanted()))
}

/// That line: the frames rendered a second and, while frame generation
/// shows more, those shown.
fn fps_line() -> Option<String> {
    if !fps_line_on().load(Ordering::Relaxed) || super::panel::shown().load(Ordering::Relaxed) {
        return None;
    }
    let fps = f64::from(METER_FPS_TENTHS.load(Ordering::Relaxed)) / 10.0;
    if fps <= 0.0 {
        return None;
    }
    Some(match shown_per_frame() {
        1 => format!("FPS: {fps:.1}"),
        per => format!("FPS: {fps:.1}  FG: {:.1}", fps * f64::from(per)),
    })
}

/// The index of `value` among the item's values.
fn index_of(item: &Item, value: &str) -> Option<usize> {
    item.values.iter().position(|(v, _)| v == value)
}

/// Keys pressed, gathered on a thread of their own every 10 ms: a frame
/// lasts 40 ms and more, and a short press would fall between two frames.
fn keys() -> &'static Mutex<Vec<u16>> {
    static KEYS: OnceLock<Mutex<Vec<u16>>> = OnceLock::new();
    KEYS.get_or_init(|| {
        let _ = std::thread::Builder::new().name("victory-settings-menu-keys".into()).spawn(|| {
            const WATCHED: [u16; 7] = [VK_F1, VK_UP, VK_DOWN, VK_LEFT, VK_RIGHT, VK_RETURN, VK_ESCAPE];
            let mut was = [false; WATCHED.len()];
            loop {
                std::thread::sleep(Duration::from_millis(10));
                let mut process = 0;
                // SAFETY: plain queries; room for the answer.
                unsafe { GetWindowThreadProcessId(GetForegroundWindow(), &mut process) };
                let ours = process == std::process::id();
                for (i, &vk) in WATCHED.iter().enumerate() {
                    // SAFETY: a plain query.
                    let down = ours && unsafe { GetAsyncKeyState(i32::from(vk)) } < 0;
                    if down && !was[i] {
                        keys().lock().unwrap_or_else(|e| e.into_inner()).push(vk);
                    }
                    was[i] = down;
                }
            }
        });
        Mutex::new(Vec::new())
    })
}

/// The settings file's lines with `changes` put over them (an empty value
/// kept as such: the launcher then unsets the switch).
fn merged(old: &str, changes: &[(&str, String)]) -> String {
    let mut kept: Vec<(String, String)> = old
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|l| l.split_once('=').map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned())))
        .filter(|(k, _)| k.starts_with("VICTORY_"))
        .collect();
    for (var, value) in changes {
        kept.retain(|(k, _)| k != var);
        kept.push(((*var).to_owned(), value.clone()));
    }
    let mut text = String::from(
        "# Saved by the F1 menu; scripts\\play-coolfox.ps1 applies it at every start (an empty value: unset).\r\n",
    );
    for (k, v) in &kept {
        text.push_str(&format!("{k}={v}\r\n"));
    }
    text
}

/// The menu of a presenter.
pub(super) struct Menu {
    items: Vec<Item>,
    /// The values chosen, by item.
    values: Vec<String>,
    /// The values at the start, or as last saved.
    started: Vec<String>,
    /// The line picked: an item, or an action after them.
    at: usize,
    note: String,
    /// The menu's text changed.
    changed: bool,
    shown: bool,
    menu: TextBox,
    fps: TextBox,
}

impl Menu {
    pub(super) fn new() -> Menu {
        // Start the key thread now: F1 must be seen while the menu is closed.
        let _ = keys();
        // `VICTORY_MENU_OPEN=1`: shown from the start (to look at it in a run nobody presses keys in).
        if std::env::var_os("VICTORY_MENU_OPEN").is_some_and(|v| v == "1") {
            crate::SETTINGS_MENU_OPEN.store(true, Ordering::Relaxed);
        }
        let items = items();
        let started: Vec<String> = items.iter().map(start_value).collect();
        Menu {
            values: started.clone(),
            started,
            items,
            at: 0,
            note: String::new(),
            changed: true,
            shown: false,
            menu: TextBox::new(MENU),
            fps: TextBox::new(FPS),
        }
    }

    fn open() -> bool {
        crate::SETTINGS_MENU_OPEN.load(Ordering::Relaxed)
    }

    /// The values that change outside the menu (F3, F8, the live window).
    fn follow(&mut self) {
        for (i, item) in self.items.iter().enumerate() {
            if matches!(item.how, How::Live(_) | How::Own) {
                let now = current(item);
                if now != self.values[i] {
                    self.values[i] = now;
                    self.changed = true;
                }
            }
        }
    }

    fn lines(&self) -> Vec<String> {
        let total = self.items.len() + ACTIONS.len();
        let first = self.at.saturating_sub(ROWS / 2).min(total.saturating_sub(ROWS));
        let mut out = vec![
            "SETTINGS (F1)                          F1 OR ESC CLOSES".to_owned(),
            "UP/DOWN PICK   LEFT/RIGHT CHANGE   ENTER DOES IT".to_owned(),
            String::new(),
        ];
        for row in first..(first + ROWS).min(total) {
            let cursor = if row == self.at { '>' } else { ' ' };
            let line = if let Some(item) = self.items.get(row) {
                let value = &self.values[row];
                let shown = index_of(item, value).map_or_else(|| value.to_ascii_uppercase(), |i| item.values[i].1.clone());
                let mark = match (*value != self.started[row], live(item)) {
                    (true, true) => " *LIVE",
                    (false, true) => " LIVE",
                    (true, false) => " *",
                    (false, false) => "",
                };
                format!("{cursor} {:<LABEL$} < {shown} >{mark}", item.label)
            } else {
                format!("{cursor} {}", ACTIONS[row - self.items.len()])
            };
            out.push(line.chars().take(COLUMNS as usize).collect());
        }
        while out.len() < 3 + ROWS {
            out.push(String::new());
        }
        out.push(String::new());
        let note = if !self.note.is_empty() {
            self.note.as_str()
        } else if self.at == self.items.len() {
            "THE GAME ENDS AT ONCE: SAVE YOUR GAME FIRST"
        } else {
            "* CHANGED: SAVED FOR THE NEXT START UNLESS LIVE"
        };
        out.push(note.to_owned());
        out
    }

    /// Changes item `i` by `step` (+1 / -1) through its values.
    fn change(&mut self, i: usize, step: isize) {
        let item = &self.items[i];
        let n = item.values.len() as isize;
        let now = index_of(item, &self.values[i]).map_or(-1, |x| x as isize);
        let next = if now < 0 {
            if step > 0 { 0 } else { n - 1 }
        } else {
            (now + step).rem_euclid(n)
        };
        self.values[i] = item.values[next as usize].0.clone();
        if live(item) {
            self.apply(i);
        }
        self.note.clear();
    }

    /// A change that shows in this run.
    fn apply(&self, i: usize) {
        let (item, value) = (&self.items[i], &self.values[i]);
        match item.how {
            How::Live(n) => {
                if let Ok(v) = value.parse::<f32>() {
                    crate::live::ask(n, v);
                }
            }
            How::Own if item.var == "VICTORY_FPS_LINE" => fps_line_on().store(value != "0", Ordering::Relaxed),
            How::Own if item.var == "VICTORY_PANEL" => {
                let on = value == "1";
                super::panel::shown().store(on, Ordering::Relaxed);
                // (The live menus follow; at the next present the panel reads the same.)
                crate::live::ask(crate::live::PANEL, f32::from(u8::from(on)));
            }
            How::FrameGeneration => {
                let Some(hooks) = frame_generation() else { return };
                let value_of = |var: &str| {
                    let at = self.items.iter().position(|it| it.var == var)?;
                    Some(self.values[at].clone())
                };
                let on = value_of("VICTORY_FRAMEGEN").is_some_and(|v| v == "1");
                let times = value_of("VICTORY_FRAMEGEN_X")
                    .and_then(|v| v.parse::<u32>().ok())
                    .unwrap_or(2);
                let times = if on { times } else { 0 };
                tracing::info!(times, "settings menu: frame generation");
                (hooks.set)(times);
            }
            _ => {}
        }
    }

    /// What changed since the start or the last save.
    fn changes(&self) -> Vec<(&'static str, String)> {
        (0..self.items.len())
            .filter(|&i| self.values[i] != self.started[i])
            .map(|i| (self.items[i].var, self.values[i].clone()))
            .collect()
    }

    /// Writes the changed values over those in the settings file.
    fn save(&mut self) -> bool {
        let Some(path) = std::env::var_os("VICTORY_MENU_FILE") else {
            self.note = "NO SETTINGS FILE: START WITH PLAY-COOLFOX.PS1".to_owned();
            return false;
        };
        let changes = self.changes();
        let text = merged(&std::fs::read_to_string(&path).unwrap_or_default(), &changes);
        match std::fs::write(&path, text) {
            Ok(()) => {
                tracing::info!(path = %std::path::Path::new(&path).display(), ?changes, "settings menu: saved");
                self.started = self.values.clone();
                self.note = "SAVED: APPLIED AT THE NEXT START".to_owned();
                true
            }
            Err(e) => {
                tracing::warn!(error = %e, "settings menu: not saved");
                self.note = "COULD NOT SAVE THE SETTINGS FILE".to_owned();
                false
            }
        }
    }

    /// Starts the launcher again in a new console, a few seconds on, and ends this run.
    fn restart(&mut self) {
        use std::os::windows::process::CommandExt;
        let Some(play) = std::env::var_os("VICTORY_MENU_PLAY") else {
            self.note = "SAVED. START THE LAUNCHER AGAIN YOURSELF".to_owned();
            return;
        };
        let play = play.to_string_lossy().replace('\'', "''");
        let args = std::env::var("VICTORY_MENU_ARGS").unwrap_or_default();
        let command = format!("Start-Sleep 3; & '{play}' {args}");
        const CREATE_NEW_CONSOLE: u32 = 0x10;
        let mut launcher = std::process::Command::new("powershell.exe");
        launcher
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &command])
            .creation_flags(CREATE_NEW_CONSOLE);
        // The menu's settings come from the launcher and the saved file alone,
        // not from this run's environment (a setting saved once, then
        // forgotten, would stay).
        for item in &self.items {
            launcher.env_remove(item.var);
        }
        match launcher.spawn() {
            Ok(_) => {
                tracing::info!(command, "settings menu: restarting");
                // Ended at once: unloading the DLLs (std::process::exit) did not
                // always come back in trustno1-coder's runs.
                // SAFETY: this process, with nothing of its own left to save.
                unsafe {
                    use windows_sys::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};
                    TerminateProcess(GetCurrentProcess(), 0);
                }
                std::process::exit(0);
            }
            Err(e) => {
                tracing::warn!(error = %e, "settings menu: restart failed");
                self.note = "SAVED, BUT THE RESTART FAILED".to_owned();
            }
        }
    }

    /// Removes the settings file: the launcher's own settings at the next start.
    fn forget(&mut self) {
        if let Some(path) = std::env::var_os("VICTORY_MENU_FILE") {
            let _ = std::fs::remove_file(path);
        }
        self.note = "FORGOTTEN: LAUNCHER DEFAULTS AT THE NEXT START".to_owned();
    }

    /// A frame was presented: the keys pressed since the last one.
    pub(super) fn frame(&mut self) {
        let pressed = std::mem::take(&mut *keys().lock().unwrap_or_else(|e| e.into_inner()));
        if Self::open() {
            self.follow();
        }
        let total = self.items.len() + ACTIONS.len();
        for vk in pressed {
            let open = Self::open();
            if vk == VK_F1 || (open && vk == VK_ESCAPE) {
                crate::SETTINGS_MENU_OPEN.store(!open && vk == VK_F1, Ordering::Relaxed);
                self.note.clear();
                self.changed = true;
                if !open {
                    self.follow();
                }
                continue;
            }
            if !open {
                continue;
            }
            match vk {
                VK_UP => self.at = (self.at + total - 1) % total,
                VK_DOWN => self.at = (self.at + 1) % total,
                VK_LEFT if self.at < self.items.len() => self.change(self.at, -1),
                VK_RIGHT | VK_RETURN if self.at < self.items.len() => self.change(self.at, 1),
                VK_RETURN => match self.at - self.items.len() {
                    0 => {
                        if self.save() {
                            self.restart();
                        }
                    }
                    1 => {
                        self.save();
                    }
                    2 => self.forget(),
                    _ => crate::SETTINGS_MENU_OPEN.store(false, Ordering::Relaxed),
                },
                _ => {}
            }
            self.changed = true;
        }
    }

    /// Records the copy of the menu, or of the frame-rate line, onto `back`
    /// (in the copy-destination state).
    pub(super) fn draw(&mut self, host: &Host, list: &HostList, back: &ID3D12Resource, size: (u32, u32), format: DXGI_FORMAT) {
        if Self::open() {
            if std::mem::replace(&mut self.changed, false) || !self.shown {
                self.menu.set(self.lines());
            }
            self.shown = true;
            self.menu.draw(host, list, back, size, format, Place::Middle);
            return;
        }
        self.shown = false;
        let Some(line) = fps_line() else { return };
        self.fps.set(vec![line]);
        self.fps.draw(host, list, back, size, format, Place::TopLeft);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu() -> Menu {
        let items = items();
        let values: Vec<String> = items.iter().map(|i| i.values[0].0.clone()).collect();
        Menu {
            started: values.clone(),
            values,
            items,
            at: 0,
            note: String::new(),
            changed: false,
            shown: false,
            menu: TextBox::new(MENU),
            fps: TextBox::new(FPS),
        }
    }

    #[test]
    fn every_line_fits_and_every_character_has_a_picture() {
        let mut menu = menu();
        for at in 0..menu.items.len() + ACTIONS.len() {
            menu.at = at;
            let lines = menu.lines();
            assert_eq!(lines.len(), MENU.lines as usize);
            for line in lines {
                assert!(line.len() <= COLUMNS as usize, "{line}");
                assert!(line.bytes().all(|c| drawable(c) && c != b'#'), "{line}");
            }
        }
        for item in &menu.items {
            assert!(item.label.len() <= LABEL, "{}", item.label);
            assert!(!item.values.is_empty(), "{}", item.var);
            for (_, name) in &item.values {
                assert!(name.bytes().all(drawable), "{name}");
            }
        }
    }

    #[test]
    fn values_cycle_both_ways() {
        let mut menu = menu();
        // (The console profile: saved for the next start, nothing applied here.)
        let n = menu.items[0].values.len();
        for _ in 0..n {
            menu.change(0, 1);
        }
        assert_eq!(menu.values[0], menu.items[0].values[0].0);
        menu.change(0, -1);
        assert_eq!(menu.values[0], menu.items[0].values[n - 1].0);
    }

    #[test]
    fn the_asked_settings_are_there() {
        let items = items();
        let has = |var: &str| items.iter().any(|i| i.var == var);
        for var in [
            "VICTORY_MENU_CONSOLE",
            "VICTORY_NATIVE_1440P",
            "VICTORY_DLSS",
            "VICTORY_DLSS_MODE",
            "VICTORY_FRAMEGEN",
            "VICTORY_FRAMEGEN_X",
            "VICTORY_FRAME_INTERVAL_US",
            "VICTORY_PANEL",
            "VICTORY_FPS_LINE",
        ] {
            assert!(has(var), "{var}");
        }
        // Every live setting once, the panel as the menu's own.
        for spec in &crate::live::SPECS {
            assert_eq!(items.iter().filter(|i| i.var == spec.name).count(), 1, "{}", spec.name);
        }
        let console = items.iter().find(|i| i.var == "VICTORY_MENU_CONSOLE").unwrap();
        assert!(index_of(console, "series-s").is_some() && index_of(console, "series-x").is_some());
    }

    #[test]
    fn a_wide_live_range_steps_by_doubles() {
        let values = live_values(crate::live::SUBMIT_DEPTH);
        let numbers: Vec<&str> = values.iter().map(|(v, _)| v.as_str()).collect();
        assert_eq!(numbers.first(), Some(&"1"));
        assert_eq!(numbers.last(), Some(&"256"));
        assert!(numbers.contains(&"64") && numbers.len() < 12, "{numbers:?}");
    }

    #[test]
    fn saving_keeps_the_other_lines() {
        let old = "# saved\r\nVICTORY_DLSS=1\r\nVICTORY_MENU_CONSOLE=series-x\r\nnot a setting\r\n";
        let text = merged(old, &[("VICTORY_MENU_CONSOLE", "series-s".into()), ("VICTORY_NATIVE_1440P", String::new())]);
        let lines: Vec<&str> = text.lines().filter(|l| !l.starts_with('#')).collect();
        assert_eq!(lines, ["VICTORY_DLSS=1", "VICTORY_MENU_CONSOLE=series-s", "VICTORY_NATIVE_1440P="]);
    }
}
