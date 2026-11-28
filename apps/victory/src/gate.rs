//! The preload barrier: `victory run` holds the title before its entry point
//! until the shaders and the pipelines it needs first are made
//! (`crate::preload`), with a window that says how far it is.
//!
//! Before the title starts there is no watchdog of the title's to kill the
//! process, and the work runs in child processes of this program with this
//! run's environment (the translations depend on ~30 `VICTORY_*` switches,
//! and the driver keeps its cache by program path): a compiler that crashes
//! costs one pipeline, never the run. While the stamps say nothing changed,
//! it costs the time a child takes to open the host GPU (about a second) and
//! shows no window.
//!
//! 0. Recording (only where no run has written the title's pipelines down
//!    yet, or the title's executable changed): the title is started once
//!    with `VICTORY_PIPELINE_BOOK=1` and a save folder of its own, and
//!    stopped once it has created no new pipeline for 10 s (about a minute:
//!    it creates ~88,000 while it loads). `VICTORY_PRELOAD_RECORD=0`: not.
//! 1. and 2. `victory preload --gate`: the shaders, then the essential
//!    pipelines; 3. with `VICTORY_PRELOAD_ALL=1`, every known pipeline for the
//!    driver's cache (20 to 40 minutes the first time).
//!
//! "Jouer maintenant" stops the work and starts the title; "Arrêter" (or
//! closing the window) stops it and does not start the title. What is done
//! is kept either way, and the next start goes on from there.
//! `VICTORY_PRELOAD=0`: no barrier.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use victory_hle::known_pipelines as known;

use crate::preload;

/// What the run does after the barrier.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum After {
    Play,
    Quit,
}

/// The window's buttons: nothing yet, "Jouer maintenant", "Arrêter".
const NONE: u8 = 0;
const PLAY: u8 = 1;
const STOP: u8 = 2;
static CHOICE: AtomicU8 = AtomicU8::new(NONE);

/// What the window shows.
#[derive(Default)]
struct Status {
    heading: String,
    reason: String,
    done: usize,
    total: usize,
    /// What the numbers count ("pipelines notés" for the recording: no
    /// total known).
    counting: Option<&'static str>,
    step_began: Option<Instant>,
}

static STATUS: Mutex<Option<Status>> = Mutex::new(None);

fn status(f: impl FnOnce(&mut Status)) {
    let mut s = STATUS.lock().unwrap_or_else(|e| e.into_inner());
    f(s.get_or_insert_with(Status::default));
}

fn choice() -> u8 {
    CHOICE.load(Ordering::Relaxed)
}

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x4000;

/// Runs the barrier for the title `exe`: whether to start it.
pub(crate) fn barrier(exe: &Path) -> After {
    if std::env::var_os("VICTORY_PRELOAD").is_some_and(|v| v == "0") {
        return After::Play;
    }
    let dir = match crate::warm::folder() {
        Ok(dir) => dir,
        Err(e) => {
            println!("preload        not done: {e:#}");
            return After::Play;
        }
    };
    let began = Instant::now();
    let _ = std::fs::create_dir_all(&dir);
    let after = record(&dir, exe).unwrap_or_else(|| preload(exe));
    window::close();
    let spent = began.elapsed().as_secs_f64();
    match after {
        After::Play if spent >= 5.0 => println!("preload        {:.1} min; the title starts", spent / 60.0),
        After::Play => println!("preload        up to date ({spent:.1} s)"),
        After::Quit => println!("preload        stopped after {:.1} min: the title is not started", spent / 60.0),
    }
    after
}

/// What the buttons asked for, once the work stopped.
fn chosen() -> After {
    if choice() == STOP { After::Quit } else { After::Play }
}

/// Steps 1 to 3 in `victory preload --gate`.
fn preload(exe: &Path) -> After {
    let all = std::env::var_os("VICTORY_PRELOAD_ALL").is_some_and(|v| v == "1");
    let title_dir = exe.parent().unwrap_or(Path::new("."));
    let spawned = std::env::current_exe().and_then(|program| {
        use std::os::windows::process::CommandExt;
        let mut command = Command::new(program);
        command.args(["preload", "--gate"]);
        if all {
            command.arg("--all");
        }
        command
            .arg(title_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .creation_flags(BELOW_NORMAL_PRIORITY_CLASS | CREATE_NO_WINDOW)
            .spawn()
    });
    let mut child = match spawned {
        Ok(child) => child,
        Err(e) => {
            println!("preload        did not start: {e}");
            return After::Play;
        }
    };
    let Some(out) = child.stdout.take() else { return After::Play };
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            if heard(&line) {
                window::show();
            }
        }
    });
    let ended = wait(&mut child, Duration::from_secs(120));
    let _ = reader.join();
    match ended {
        Some(s) if !s.success() => println!("preload        ended with {s}: the title starts anyway"),
        None => println!("preload        did not stop in time: ended"),
        _ => {}
    }
    chosen()
}

/// Waits for `child`, which stops when its input ends: at once when a button
/// was pressed (killed after `grace`); its exit.
fn wait(child: &mut Child, grace: Duration) -> Option<std::process::ExitStatus> {
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Some(status);
        }
        if choice() != NONE {
            // The end of its input stops it, keeping what is done.
            drop(child.stdin.take());
            let asked = Instant::now();
            while asked.elapsed() < grace {
                if let Ok(Some(status)) = child.try_wait() {
                    return Some(status);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A line of `victory preload --gate`: whether a step begins.
fn heard(line: &str) -> bool {
    let Some(rest) = line.strip_prefix('@') else {
        println!("preload        {line}");
        return false;
    };
    let (word, rest) = rest.split_once(' ').unwrap_or((rest, ""));
    match word {
        "step" => {
            let mut words = rest.splitn(3, ' ');
            let (n, of, label) = (words.next().unwrap_or("?"), words.next().unwrap_or("?"), words.next().unwrap_or(""));
            println!("preload        step {n} of {of}: {label}");
            status(|s| {
                s.heading = format!("Étape {n}/{of} — {label}");
                (s.done, s.total, s.counting) = (0, 0, None);
                s.step_began = Some(Instant::now());
            });
            return true;
        }
        "progress" => {
            let mut words = rest.split(' ').map(|w| w.parse::<usize>().unwrap_or(0));
            let (done, total) = (words.next().unwrap_or(0), words.next().unwrap_or(0));
            status(|s| (s.done, s.total) = (done, total));
        }
        "reason" => {
            println!("preload        why: {rest}");
            status(|s| s.reason = rest.to_owned());
        }
        _ => {}
    }
    false
}

/// Step 0: the title's pipelines written down by a run of the title, where
/// none are yet; `None` to go on with the other steps.
fn record(dir: &Path, exe: &Path) -> Option<After> {
    const STAMP: &str = "preload-record.stamp";
    if std::env::var_os("VICTORY_PRELOAD_RECORD").is_some_and(|v| v == "0") {
        return None;
    }
    let game = std::fs::read(exe).map(|b| blake3::hash(&b).to_hex()[..16].to_owned()).unwrap_or_default();
    let shaders = exe.parent().map(|d| preload::file_id(&d.join("shaders.rpf"))).unwrap_or_default();
    let stamp: preload::Stamp = vec![("game", format!("{game} shaders.rpf {shaders}"))];
    let old = std::fs::read_to_string(dir.join(STAMP)).ok();
    let known_before = known::count(dir);
    let why = match (known_before, old.as_deref()) {
        (0, _) => "aucun pipeline du jeu noté pour l'instant".to_owned(),
        (_, Some(old)) => preload::stale(Some(old), &stamp)?,
        (_, None) => {
            // Pipelines written down by runs before the stamps: kept as they are.
            preload::write_stamp(dir, STAMP, &stamp);
            return None;
        }
    };
    println!("preload        step 0: the title runs once to write its pipelines down ({why})");
    status(|s| {
        s.heading = "Étape 0 — Enregistrement des pipelines du jeu (le jeu s'ouvre puis se ferme)".into();
        s.reason = why;
        s.counting = Some("pipelines notés");
        s.step_began = Some(Instant::now());
    });
    window::show();
    let file = dir.join("known-pipelines.bin");
    let size = || std::fs::metadata(&file).map_or(0, |m| m.len());
    let log = std::fs::File::create(dir.join("preload-record.log"));
    // A save folder of its own: the run is stopped from outside.
    let data = dir.with_file_name("preload-record-data");
    let spawned = std::env::current_exe().and_then(|program| {
        let mut command = Command::new(program);
        command
            .args(std::env::args_os().skip(1))
            .env("VICTORY_PRELOAD", "0")
            .env("VICTORY_PIPELINE_BOOK", "1")
            .env("VICTORY_DATA_DIR", &data)
            .stdin(Stdio::null());
        match &log {
            Ok(log) => command.stdout(log.try_clone()?).stderr(log.try_clone()?),
            Err(_) => command.stdout(Stdio::null()).stderr(Stdio::null()),
        };
        command.spawn()
    });
    let mut child = match spawned {
        Ok(child) => child,
        Err(e) => {
            println!("preload        the recording run did not start: {e}");
            return None;
        }
    };
    let began = Instant::now();
    let first = size();
    let (mut last, mut changed) = (first, Instant::now());
    let mut counted = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(500));
        let now = size();
        if now != last {
            (last, changed) = (now, Instant::now());
        }
        if counted.elapsed() >= Duration::from_secs(2) {
            counted = Instant::now();
            let n = known::count(dir);
            status(|s| s.done = n);
        }
        if recorded(began.elapsed(), last > first, changed.elapsed()) || choice() != NONE {
            break;
        }
        if let Ok(Some(status)) = child.try_wait() {
            println!("preload        the recording run ended by itself ({status})");
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    let known_after = known::count(dir);
    println!(
        "preload        recording: {} title pipelines written down ({} before) in {:.0} s",
        known_after,
        known_before,
        began.elapsed().as_secs_f64()
    );
    if known_after > known_before {
        preload::write_stamp(dir, STAMP, &stamp);
    }
    (choice() != NONE).then(chosen)
}

/// Whether the recording run has done its work: `grew` (pipelines were
/// written down) and no new one for 10 s, at least 20 s in; or 2 minutes
/// with none at all; or 5 minutes whatever.
fn recorded(running: Duration, grew: bool, quiet: Duration) -> bool {
    (grew && quiet >= Duration::from_secs(10) && running >= Duration::from_secs(20))
        || (!grew && running >= Duration::from_secs(120))
        || running >= Duration::from_secs(300)
}

/// `12345` as `12 345`.
fn grouped(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

/// `0:52`, `1:04:09`.
fn clock(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

/// What is left of a step `done` of `total` into it after `elapsed`, at the
/// pace so far (after 3 s, to have one).
fn left(done: usize, total: usize, elapsed: Duration) -> Option<Duration> {
    (done > 0 && done < total && elapsed >= Duration::from_secs(3))
        .then(|| elapsed.mul_f64((total - done) as f64 / done as f64))
}

/// The window's second line.
fn detail(s: &Status) -> String {
    let elapsed = s.step_began.map_or(Duration::ZERO, |t| t.elapsed());
    if let Some(what) = s.counting {
        return format!("{} {what} — écoulé {}", grouped(s.done), clock(elapsed));
    }
    if s.total == 0 {
        return format!("préparation — écoulé {}", clock(elapsed));
    }
    let percent = s.done * 100 / s.total.max(1);
    let mut line = format!("{} / {} ({percent} %) — écoulé {}", grouped(s.done), grouped(s.total), clock(elapsed));
    if let Some(rest) = left(s.done, s.total, elapsed) {
        line += &format!(" — reste ~{}", clock(rest));
    }
    line
}

/// How full the bar is, 0 to 1.
fn fraction(s: &Status) -> f64 {
    match s.counting {
        // (About 88,600 while the title loads.)
        Some(_) => (s.done as f64 / 88_600.0).min(0.99),
        None if s.total == 0 => 0.0,
        None => (s.done as f64 / s.total as f64).min(1.0),
    }
}

mod window {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicIsize, Ordering::Relaxed};
    use std::thread::JoinHandle;

    use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::{PCWSTR, w};

    use super::{CHOICE, PLAY, STATUS, STOP, detail, fraction};

    const WIDTH: i32 = 640;
    const HEIGHT: i32 = 250;
    /// Our message: the work is over, the window goes (no choice made).
    const WM_DONE: u32 = WM_APP + 1;
    const PLAY_ID: usize = 1;
    const STOP_ID: usize = 2;

    static WINDOW: AtomicIsize = AtomicIsize::new(0);
    static THREAD: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

    /// Opens the window, once.
    pub(super) fn show() {
        let mut thread = THREAD.lock().unwrap_or_else(|e| e.into_inner());
        if thread.is_none() {
            *thread = std::thread::Builder::new().name("victory-preload-window".into()).spawn(run).ok();
        }
    }

    /// Closes it, if it was opened.
    pub(super) fn close() {
        let Some(thread) = THREAD.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
        // (It may not have made the window yet: 5 s for it, else it is left.)
        for _ in 0..250 {
            if thread.is_finished() {
                let _ = thread.join();
                return;
            }
            let hwnd = WINDOW.load(Relaxed);
            if hwnd != 0 {
                // SAFETY: a message to our own window.
                let _ = unsafe { PostMessageW(Some(HWND(hwnd as _)), WM_DONE, WPARAM(0), LPARAM(0)) };
                let _ = thread.join();
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    fn font(height: i32, weight: i32) -> HFONT {
        // SAFETY: a font of ours, kept for the window's life.
        unsafe {
            CreateFontW(
                height,
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                0,
                w!("Segoe UI"),
            )
        }
    }

    fn run() {
        // SAFETY: our class and window, used on this thread only.
        unsafe {
            let Ok(instance) = GetModuleHandleW(None) else { return };
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(proc),
                hInstance: instance.into(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpszClassName: w!("VIctoryPreload"),
                ..Default::default()
            };
            RegisterClassExW(&class);
            let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN;
            let mut frame = RECT { left: 0, top: 0, right: WIDTH, bottom: HEIGHT };
            let _ = AdjustWindowRectEx(&mut frame, style, false, WINDOW_EX_STYLE(0));
            let (w, h) = (frame.right - frame.left, frame.bottom - frame.top);
            let (sw, sh) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
            let title = wide("VIctory — préchargement des shaders et des pipelines");
            let Ok(hwnd) = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("VIctoryPreload"),
                PCWSTR(title.as_ptr()),
                style,
                (sw - w) / 2,
                (sh - h) / 2,
                w,
                h,
                None,
                None,
                Some(instance.into()),
                None,
            ) else {
                return;
            };
            let small = font(17, FW_NORMAL.0 as i32);
            for (id, label, x, width) in [(PLAY_ID, "Jouer maintenant", 20, 180), (STOP_ID, "Arrêter", 212, 120)] {
                let text = wide(label);
                if let Ok(button) = CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("BUTTON"),
                    PCWSTR(text.as_ptr()),
                    WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32),
                    x,
                    HEIGHT - 52,
                    width,
                    34,
                    Some(hwnd),
                    Some(HMENU(id as _)),
                    Some(instance.into()),
                    None,
                ) {
                    SendMessageW(button, WM_SETFONT, Some(WPARAM(small.0 as usize)), Some(LPARAM(1)));
                }
            }
            WINDOW.store(hwnd.0 as isize, Relaxed);
            let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
            let _ = SetForegroundWindow(hwnd);
            SetTimer(Some(hwnd), 1, 250, None);
            let mut message = MSG::default();
            while GetMessageW(&mut message, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            WINDOW.store(0, Relaxed);
        }
    }

    extern "system" fn proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        // SAFETY: messages of our window, on its thread.
        unsafe {
            match message {
                WM_TIMER => {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                    LRESULT(0)
                }
                WM_COMMAND => {
                    match wparam.0 & 0xffff {
                        PLAY_ID => CHOICE.store(PLAY, Relaxed),
                        STOP_ID => CHOICE.store(STOP, Relaxed),
                        _ => {}
                    }
                    LRESULT(0)
                }
                // Closed: as "Arrêter" (the window goes when the work has stopped).
                WM_CLOSE => {
                    let _ = CHOICE.compare_exchange(super::NONE, STOP, Relaxed, Relaxed);
                    LRESULT(0)
                }
                WM_DONE => {
                    let _ = DestroyWindow(hwnd);
                    LRESULT(0)
                }
                WM_DESTROY => {
                    PostQuitMessage(0);
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

    fn text(dc: HDC, s: &str, r: RECT) {
        let mut wide: Vec<u16> = s.encode_utf16().collect();
        let mut r = r;
        // SAFETY: drawing on a device context of ours.
        unsafe { DrawTextW(dc, &mut wide, &mut r, DT_LEFT | DT_NOPREFIX | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS) };
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
        let (heading, detail, reason, fraction) = {
            let s = STATUS.lock().unwrap_or_else(|e| e.into_inner());
            match s.as_ref() {
                Some(s) => (s.heading.clone(), detail(s), s.reason.clone(), fraction(s)),
                None => (String::new(), String::new(), String::new(), 0.0),
            }
        };
        let mut ps = PAINTSTRUCT::default();
        // SAFETY: painting our window between BeginPaint and EndPaint, into
        // a bitmap of ours first (no flicker).
        unsafe {
            let screen = BeginPaint(hwnd, &mut ps);
            let dc = CreateCompatibleDC(Some(screen));
            let bitmap = CreateCompatibleBitmap(screen, WIDTH, HEIGHT);
            let old_bitmap = SelectObject(dc, bitmap.into());
            fill(dc, RECT { left: 0, top: 0, right: WIDTH, bottom: HEIGHT }, 0x0020_1A16);
            let big = font(21, FW_SEMIBOLD.0 as i32);
            let small = font(17, FW_NORMAL.0 as i32);
            let old_font = SelectObject(dc, big.into());
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, COLORREF(0x00F0_F0F0));
            text(dc, &heading, RECT { left: 20, top: 12, right: WIDTH - 20, bottom: 40 });
            SelectObject(dc, small.into());
            SetTextColor(dc, COLORREF(0x00DC_DCDC));
            text(dc, &detail, RECT { left: 20, top: 44, right: WIDTH - 20, bottom: 68 });
            let bar = RECT { left: 20, top: 76, right: WIDTH - 20, bottom: 96 };
            fill(dc, bar, 0x0040_3A36);
            let filled = bar.left + ((bar.right - bar.left) as f64 * fraction) as i32;
            fill(dc, RECT { right: filled, ..bar }, 0x00D7_7800);
            SetTextColor(dc, COLORREF(0x00B4_B4B4));
            if !reason.is_empty() {
                text(dc, &format!("Raison : {reason}"), RECT { left: 20, top: 104, right: WIDTH - 20, bottom: 128 });
            }
            text(
                dc,
                "Ce qui est fait est gardé : arrêté, le prochain lancement reprend là où il en était.",
                RECT { left: 20, top: 130, right: WIDTH - 20, bottom: 154 },
            );
            text(dc, "Le jeu démarre tout seul à la fin.", RECT { left: 20, top: 154, right: WIDTH - 20, bottom: 178 });
            let _ = BitBlt(screen, 0, 0, WIDTH, HEIGHT, Some(dc), 0, 0, SRCCOPY);
            SelectObject(dc, old_font);
            SelectObject(dc, old_bitmap);
            let _ = DeleteObject(big.into());
            let _ = DeleteObject(small.into());
            let _ = DeleteObject(bitmap.into());
            let _ = DeleteDC(dc);
            let _ = EndPaint(hwnd, &ps);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_times_read_as_in_french() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(18_240), "18 240");
        assert_eq!(grouped(1_088_600), "1 088 600");
        assert_eq!(clock(Duration::from_secs(52)), "0:52");
        assert_eq!(clock(Duration::from_secs(3849)), "1:04:09");
    }

    /// What is left follows the pace so far, once there is one.
    #[test]
    fn the_time_left_follows_the_pace() {
        let s = Duration::from_secs;
        assert_eq!(left(250, 1000, s(10)), Some(s(30)));
        assert_eq!(left(0, 1000, s(10)), None);
        assert_eq!(left(1000, 1000, s(10)), None);
        assert_eq!(left(10, 1000, s(2)), None);
    }

    /// The recording stops 10 s after the last new pipeline, not before 20 s,
    /// and in any case after 5 minutes.
    #[test]
    fn the_recording_stops_once_quiet() {
        let s = Duration::from_secs;
        assert!(!recorded(s(15), true, s(12)));
        assert!(recorded(s(25), true, s(10)));
        assert!(!recorded(s(25), true, s(9)));
        assert!(!recorded(s(100), false, s(100)));
        assert!(recorded(s(120), false, s(120)));
        assert!(recorded(s(300), true, s(0)));
    }

    #[test]
    fn the_lines_of_the_preload_move_the_window() {
        assert!(heard("@step 2 3 Pipelines essentiels : cache du pilote"));
        assert!(!heard("@progress 250 1000"));
        assert!(!heard("@reason nouveau traducteur"));
        assert!(!heard("plain log line"));
        let s = STATUS.lock().unwrap();
        let s = s.as_ref().unwrap();
        assert_eq!(s.heading, "Étape 2/3 — Pipelines essentiels : cache du pilote");
        assert_eq!((s.done, s.total), (250, 1000));
        assert_eq!(s.reason, "nouveau traducteur");
        assert!(detail(s).starts_with("250 / 1 000 (25 %) — écoulé 0:0"));
        assert!((fraction(s) - 0.25).abs() < 1e-9);
    }
}
