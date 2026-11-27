//! The run's numbers on the picture (`VICTORY_PANEL=1`, every build; off by
//! default; F3 shows and hides it while our window has the focus, and so
//! does the live menus' switch, `crate::live`): white text on a black box in
//! the upper left corner of what is shown, so that a screenshot carries
//! them, with what it costs itself on the submitting thread. Made again
//! twice a second from the counters of the frame-rate line, memory and
//! textures; a frame itself only copies the box out of a buffer in the
//! card's memory. Copied onto the swap chain's buffer, behind the title's
//! picture: saved frames and the frame recorder see the title's own. Off, it
//! costs a frame one look at the key's state.
//!
//! (After avatarmanz/VIctory-7900XTX, where it is a diagnostics build's.
//! The letters and the box are `textbox.rs`'s, shared with the F1 menu.)

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_F3};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

use super::textbox::{Place, Shape, TextBox};
use super::{Host, HostList};

/// How often the numbers change.
const EVERY: Duration = Duration::from_millis(500);

/// The box: characters in a line, lines, and its rim in dots.
const SHAPE: Shape = Shape { columns: 40, lines: 15, rim: 3 };
const COLUMNS: u32 = SHAPE.columns;

fn asked() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| crate::switch::on("VICTORY_PANEL"))
}

/// Whether it is shown now (the F1 menu's frame-rate line gives way to it).
pub(super) fn shown() -> &'static AtomicBool {
    static SHOWN: OnceLock<AtomicBool> = OnceLock::new();
    SHOWN.get_or_init(|| AtomicBool::new(asked()))
}

/// F3 was pressed since the last call, with one of our windows in front.
fn pressed() -> bool {
    static DOWN: AtomicBool = AtomicBool::new(false);
    // SAFETY: a plain query.
    let down = unsafe { GetAsyncKeyState(i32::from(VK_F3)) } < 0;
    if DOWN.swap(down, Ordering::Relaxed) || !down {
        return false;
    }
    let mut process = 0;
    // SAFETY: plain queries; room for the answer.
    unsafe { GetWindowThreadProcessId(GetForegroundWindow(), &mut process) };
    process == std::process::id()
}

/// The counters read for it. They grow, and go back to 0 with the frame-rate
/// line (right behind a look of ours).
#[derive(Clone, Copy, Default)]
struct Counters([u64; COUNTERS]);

const COUNTERS: usize = 15;

/// Where each is in [`Counters`].
mod at {
    pub(super) const WORKED: usize = 0;
    pub(super) const CARD_IDLE: usize = 1;
    pub(super) const IDLE_BEHIND: usize = 2;
    pub(super) const STOOD: usize = 3;
    pub(super) const TEXTURE_BYTES: usize = 4;
    pub(super) const BACK_US: usize = 5;
    pub(super) const BACK_BYTES: usize = 6;
    pub(super) const UP_BYTES: usize = 7;
    pub(super) const DROPPED: usize = 8;
    /// Recording, in nanoseconds (diagnostics builds; 0 otherwise).
    pub(super) const RECORDING: usize = 9;
    pub(super) const DRAWN: usize = 10;
    pub(super) const PREPARING: usize = 11;
    pub(super) const ON_THE_GPU: usize = 12;
    pub(super) const COPIES_US: usize = 13;
    pub(super) const TEXTURES_US: usize = 14;
}

impl Counters {
    fn now() -> Counters {
        use super::{CARD_CLOCK_US, COPY_BACK, PREPARING, SUBMISSION_US};
        let get = |a: &std::sync::atomic::AtomicU64| a.load(Ordering::Relaxed);
        #[cfg(feature = "diagnostics")]
        let recording = crate::copystat::peek(crate::copystat::C::RecordNs);
        #[cfg(not(feature = "diagnostics"))]
        let recording = 0;
        Counters([
            super::worked(),
            get(&CARD_CLOCK_US[0]),
            get(&CARD_CLOCK_US[1]),
            get(&CARD_CLOCK_US[2]),
            get(&PREPARING[3]),
            get(&COPY_BACK[0]),
            get(&COPY_BACK[1]),
            get(&crate::shadow::CARD_STATS[0]),
            crate::draw::dropped_now(),
            recording,
            crate::draw::drawn(),
            get(&SUBMISSION_US[0]),
            get(&SUBMISSION_US[1]),
            get(&PREPARING[0]),
            get(&PREPARING[2]),
        ])
    }

    /// What came since `before`, per frame.
    fn since(self, before: Counters, frames: f64) -> [f64; COUNTERS] {
        let mut out = [0.0; COUNTERS];
        for (i, out) in out.iter_mut().enumerate() {
            let (was, is) = (before.0[i], self.0[i]);
            *out = if is >= was { is - was } else { is } as f64 / frames;
        }
        out
    }
}

/// What the text is made of, besides [`Counters`].
struct Said {
    /// A frame's time and the longest frame's, in milliseconds.
    ms: f64,
    worst: f64,
    /// What this takes of a frame, in microseconds.
    cost: f64,
    /// The card's memory: budget, in use, and the PC's memory shared with it
    /// in use. Bytes.
    video: [u64; 3],
    /// The process's memory: committed, and in physical memory. Bytes.
    process: [u64; 2],
    /// The settings changing while the title runs (`crate::live::tag`).
    tag: Option<String>,
    /// Frames shown for each one rendered (frame generation; 1 without).
    shown_per_frame: u32,
}

/// The text. `c`: [`Counters`] per frame.
fn text(c: &[f64; COUNTERS], s: &Said) -> Vec<String> {
    use at::*;
    let gb = |bytes: u64| bytes as f64 / (1u64 << 30) as f64;
    let ms = s.ms;
    let idle = (c[CARD_IDLE] / 1e3).min(ms);
    let busy = if ms > 0.0 { (100.0 * (1.0 - idle / ms)).clamp(0.0, 100.0) } else { 0.0 };
    let [budget, used, shared] = s.video;
    let [private, working] = s.process;
    let mut lines = vec![
        format!("FPS {:.1}  FRAME {ms:.1} MS  LONGEST {:.0}", 1e3 / ms.max(0.001), s.worst),
        format!("PREPARING {:.1} MS  ON THE GPU {:.1}", c[PREPARING] / 1e3, c[ON_THE_GPU] / 1e3),
        format!(" COPIES {:.1}  TEXTURES {:.1}  BACK {:.1}", c[COPIES_US] / 1e3, c[TEXTURES_US] / 1e3, c[BACK_US] / 1e3),
        format!(" MB UP {:.0}  TEXTURES {:.0}  BACK {:.0}", c[UP_BYTES] / 1e6, c[TEXTURE_BYTES] / 1e6, c[BACK_BYTES] / 1e6),
        format!("GPU BUSY {busy:.0}%  IDLE BEHIND US {:.1} MS", c[IDLE_BEHIND] / 1e3),
        format!("SUBMIT BUSY {:.1}  WAITS FOR GPU {:.1}", c[WORKED] / 1e3, c[STOOD] / 1e3),
        format!("DRAWS {:.0}  DROPPED {:.0}", c[DRAWN], c[DROPPED]),
        format!("VRAM {:.1}/{:.1} GB  SHARED {:.1}", gb(used), gb(budget), gb(shared)),
        format!("RAM {:.1} GB  IN MEMORY {:.1}", gb(private), gb(working)),
    ];
    if s.shown_per_frame > 1 {
        // (After trustno1-coder/VIctory-NVIDIA's panel: frame generation's line.)
        let shown = 1e3 / ms.max(0.001) * f64::from(s.shown_per_frame);
        lines.insert(1, format!("FRAME GEN {}X  SHOWN {shown:.1} FPS", s.shown_per_frame));
    }
    if cfg!(feature = "diagnostics") {
        lines.push(format!("RECORDING {:.0} MS", c[RECORDING] / 1e6));
    }
    lines.push(format!("THIS PANEL {:.0} US A FRAME", s.cost));
    if let Some(tag) = &s.tag {
        // (Two lines at most, without the names' common start.)
        let tag = tag.replace("VICTORY_", "").replace(',', "");
        for part in tag.as_bytes().chunks(COLUMNS as usize).take(2) {
            lines.push(String::from_utf8_lossy(part).into_owned());
        }
    }
    lines
}

/// The panel of a presenter.
pub(super) struct Panel {
    since: Instant,
    frames: u32,
    last: Instant,
    worst: f64,
    /// What this took since the last text.
    cost: Duration,
    before: Counters,
    text: TextBox,
}

impl Panel {
    pub(super) fn new() -> Panel {
        let now = Instant::now();
        Panel {
            since: now,
            frames: 0,
            last: now,
            worst: 0.0,
            cost: Duration::ZERO,
            before: Counters::default(),
            text: TextBox::new(SHAPE),
        }
    }

    /// A frame was presented; `line`: the counters go back to 0 right behind
    /// this call. (Not shown it asks for the key's state and no more.)
    pub(super) fn frame(&mut self, line: bool) {
        if pressed() {
            shown().fetch_xor(true, Ordering::Relaxed);
        }
        // The live menus' switch (`crate::live`), when it changes.
        static SEEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        if let Some(v) = crate::live::fresh(crate::live::PANEL, &SEEN) {
            shown().store(v >= 0.5, Ordering::Relaxed);
        }
        if !shown().load(Ordering::Relaxed) {
            self.frames = 0;
            return;
        }
        let now = Instant::now();
        if self.frames == 0 && now - self.last > 2 * EVERY {
            // (Shown from here on.)
            (self.since, self.last, self.worst, self.before) = (now, now, 0.0, Counters::now());
            self.frames = 0;
        }
        self.frames += 1;
        self.worst = self.worst.max((now - self.last).as_secs_f64() * 1e3);
        self.last = now;
        let elapsed = now - self.since;
        if elapsed < EVERY && !(line && elapsed * 2 >= EVERY) {
            return;
        }
        let Some(host) = super::host() else { return };
        let frames = f64::from(self.frames);
        let counters = Counters::now();
        let c = counters.since(self.before, frames);
        self.before = counters;
        let [budget, used, _, shared] = host.video_memory().unwrap_or_default();
        let (working, private) = super::process_memory();
        let said = Said {
            ms: elapsed.as_secs_f64() * 1e3 / frames,
            worst: self.worst,
            cost: self.cost.as_secs_f64() * 1e6 / frames,
            video: [budget, used, shared],
            process: [private, working],
            tag: crate::live::tag(),
            shown_per_frame: super::menu::shown_per_frame(),
        };
        self.text.set(text(&c, &said));
        (self.since, self.frames, self.worst) = (now, 0, 0.0);
        self.cost = now.elapsed();
    }

    /// Records the copy of the box onto `back`, a buffer of the swap chain of
    /// that size and format in the state to be copied into.
    pub(super) fn draw(
        &mut self,
        host: &Host,
        list: &HostList,
        back: &ID3D12Resource,
        size: (u32, u32),
        format: DXGI_FORMAT,
    ) {
        if !shown().load(Ordering::Relaxed) {
            return;
        }
        let began = Instant::now();
        self.text.draw(host, list, back, size, format, Place::TopLeft);
        self.cost += began.elapsed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every character the lines may hold has its own picture.
    #[test]
    fn every_character_has_a_picture() {
        let c = [1.0; COUNTERS];
        let said = Said {
            ms: 45.0,
            worst: 80.0,
            cost: 20.0,
            video: [16 << 30, 9 << 30, 1 << 30],
            process: [20 << 30, 12 << 30],
            tag: Some("ab 2/3 VICTORY_UP_QUEUE=on, live [VICTORY_FILL_ON_CARD=0]".to_owned()),
            shown_per_frame: 2,
        };
        let lines = text(&c, &said);
        assert!(lines.len() <= SHAPE.lines as usize, "{lines:?}");
        for line in lines {
            assert!(line.bytes().all(super::super::textbox::drawable), "{line}");
        }
    }

    #[test]
    fn counters_that_went_back_to_nothing_count_from_there() {
        let (mut a, mut b) = (Counters::default(), Counters::default());
        (a.0[0], b.0[0]) = (10, 30);
        (a.0[1], b.0[1]) = (50, 20);
        let c = b.since(a, 10.0);
        assert_eq!((c[0], c[1]), (2.0, 2.0));
    }
}
