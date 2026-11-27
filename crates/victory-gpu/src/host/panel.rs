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
//! (After avatarmanz/VIctory-7900XTX, where it is a diagnostics build's.)

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_F3};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

use super::{Host, HostList, transition};

/// How often the numbers change.
const EVERY: Duration = Duration::from_millis(500);

/// A character's dots, and with the space beside and below it.
const GLYPH: (u32, u32) = (5, 7);
const CELL: (u32, u32) = (6, 9);
/// Characters in a line, lines, and the box's rim in dots.
const COLUMNS: u32 = 40;
const LINES: u32 = 14;
const RIM: u32 = 3;

/// The texts are put into one of these in turn: the card reads the others,
/// at most half a second old.
const UPLOADS: usize = 4;

fn asked() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| crate::switch::on("VICTORY_PANEL"))
}

/// Whether it is shown now.
fn shown() -> &'static AtomicBool {
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

/// Seven rows of five dots, the leftmost the highest bit.
#[rustfmt::skip]
const fn glyph(c: u8) -> [u8; 7] {
    match c {
        b'0' => [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
        b'1' => [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        b'2' => [0b01110, 0b10001, 0b00001, 0b00110, 0b01000, 0b10000, 0b11111],
        b'3' => [0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110],
        b'4' => [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
        b'5' => [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
        b'6' => [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
        b'7' => [0b11111, 0b00001, 0b00010, 0b00010, 0b00100, 0b00100, 0b00100],
        b'8' => [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
        b'9' => [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
        b'A' => [0b00100, 0b01010, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001],
        b'B' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110],
        b'C' => [0b01111, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b01111],
        b'D' => [0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110],
        b'E' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111],
        b'F' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000],
        b'G' => [0b01111, 0b10000, 0b10000, 0b10011, 0b10001, 0b10001, 0b01111],
        b'H' => [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        b'I' => [0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        b'J' => [0b00111, 0b00010, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100],
        b'K' => [0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001],
        b'L' => [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111],
        b'M' => [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001],
        b'N' => [0b10001, 0b11001, 0b11001, 0b10101, 0b10011, 0b10011, 0b10001],
        b'O' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        b'P' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000],
        b'Q' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101],
        b'R' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001],
        b'S' => [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110],
        b'T' => [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100],
        b'U' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        b'V' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100],
        b'W' => [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001],
        b'X' => [0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001],
        b'Y' => [0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100],
        b'Z' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111],
        b'.' => [0, 0, 0, 0, 0, 0b01100, 0b01100],
        b'/' => [0b00001, 0b00010, 0b00010, 0b00100, 0b01000, 0b01000, 0b10000],
        b'-' => [0, 0, 0, 0b11111, 0, 0, 0],
        b'%' => [0b11001, 0b11010, 0b00010, 0b00100, 0b01000, 0b01011, 0b10011],
        b'=' => [0, 0, 0b11111, 0, 0b11111, 0, 0],
        b'[' => [0b01110, 0b01000, 0b01000, 0b01000, 0b01000, 0b01000, 0b01110],
        b']' => [0b01110, 0b00010, 0b00010, 0b00010, 0b00010, 0b00010, 0b01110],
        b'_' => [0, 0, 0, 0, 0, 0, 0b11111],
        b' ' => [0; 7],
        _ => [0b11111, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11111],
    }
}

/// The box's size in pixels, a dot being `scale` of them each way.
const fn size(scale: u32) -> (u32, u32) {
    ((COLUMNS * CELL.0 + 2 * RIM) * scale, (LINES * CELL.1 + 2 * RIM) * scale)
}

/// Pixels to a dot for a picture that high: a 720-line screenshot of it
/// shows one and a third.
fn scale(height: u32) -> u32 {
    (height / 540).max(1)
}

/// The bytes of a black and a white pixel of a swap chain's format.
fn colours(format: DXGI_FORMAT) -> Option<[&'static [u8]; 2]> {
    Some(match format {
        DXGI_FORMAT_R8G8B8A8_UNORM | DXGI_FORMAT_B8G8R8A8_UNORM => [&[0, 0, 0, 0xFF], &[0xFF; 4]],
        DXGI_FORMAT_R10G10B10A2_UNORM => [&[0, 0, 0, 0xC0], &[0xFF; 4]],
        // Halves of 0 and 1.
        DXGI_FORMAT_R16G16B16A16_FLOAT => [
            &[0, 0, 0, 0, 0, 0, 0, 0x3C],
            &[0, 0x3C, 0, 0x3C, 0, 0x3C, 0, 0x3C],
        ],
        _ => return None,
    })
}

/// The box as rows of `pitch` bytes.
#[cfg(test)]
fn painted(lines: &[String], scale: u32, colours: [&[u8]; 2], pitch: usize) -> Vec<u8> {
    let mut out = Vec::new();
    paint(&mut out, lines, scale, colours, pitch);
    out
}

/// The box as rows of `pitch` bytes, into `out` (kept from one text to the
/// next: no new memory to fault in). Black rows copied whole, then each
/// character's lit dots: about 0.1 ms at 1080 lines.
fn paint(out: &mut Vec<u8>, lines: &[String], scale: u32, colours: [&[u8]; 2], pitch: usize) {
    let (width, height) = size(scale);
    let (bytes, scale) = (colours[0].len(), scale as usize);
    let row = width as usize * bytes;
    out.resize(pitch * height as usize, 0);
    let black = colours[0].repeat(width as usize);
    let white = colours[1].repeat(scale);
    for y in 0..height as usize / scale {
        let top = pitch * y * scale;
        out[top..top + row].copy_from_slice(&black);
        let (line, dot) = (y.wrapping_sub(RIM as usize) / CELL.1 as usize, y.wrapping_sub(RIM as usize) % CELL.1 as usize);
        if let Some(text) = lines.get(line).filter(|_| y >= RIM as usize && dot < GLYPH.1 as usize) {
            for (column, c) in text.bytes().take(COLUMNS as usize).enumerate() {
                let bits = glyph(c.to_ascii_uppercase())[dot];
                for i in (0..GLYPH.0 as usize).filter(|i| bits >> (GLYPH.0 as usize - 1 - i) & 1 != 0) {
                    let at = top + (RIM as usize + column * CELL.0 as usize + i) * scale * bytes;
                    out[at..at + white.len()].copy_from_slice(&white);
                }
            }
        }
        for i in 1..scale {
            out.copy_within(top..top + row, top + i * pitch);
        }
    }
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

/// The buffers of a box that size and format.
struct Buffers {
    format: DXGI_FORMAT,
    scale: u32,
    pitch: u32,
    uploads: Vec<(ID3D12Resource, *mut u8)>,
    card: ID3D12Resource,
    /// The upload written last.
    turn: usize,
    /// It is not in the card's buffer yet.
    fresh: bool,
    /// The picture, made here before it goes into an upload.
    picture: Vec<u8>,
}

// SAFETY: free-threaded resources, and memory only written under the presenter's lock.
unsafe impl Send for Buffers {}

impl Buffers {
    fn new(host: &Host, format: DXGI_FORMAT, scale: u32) -> Option<Buffers> {
        let (width, height) = size(scale);
        let pitch = (width * colours(format)?[0].len() as u32).next_multiple_of(256);
        let bytes = u64::from(pitch) * u64::from(height);
        let uploads = (0..UPLOADS).map(|_| host.upload_buffer(bytes)).collect::<Option<Vec<_>>>()?;
        let heap = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_DEFAULT,
            ..Default::default()
        };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Width: bytes,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            ..Default::default()
        };
        let mut card: Option<ID3D12Resource> = None;
        // SAFETY: plain creation of a buffer.
        unsafe {
            host.device
                .CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &desc, D3D12_RESOURCE_STATE_COMMON, None, &mut card)
                .ok()?;
        }
        Some(Buffers { format, scale, pitch, uploads, card: card?, turn: 0, fresh: false, picture: Vec::new() })
    }

    fn write(&mut self, lines: &[String]) {
        let Some(colours) = colours(self.format) else { return };
        let picture = &mut self.picture;
        paint(picture, lines, self.scale, colours, self.pitch as usize);
        self.turn = (self.turn + 1) % UPLOADS;
        // SAFETY: a mapped upload buffer of the picture's size, which the card read a second or more ago.
        unsafe { std::ptr::copy_nonoverlapping(picture.as_ptr(), self.uploads[self.turn].1, picture.len()) };
        self.fresh = true;
    }
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
    lines: Vec<String>,
    changed: bool,
    buffers: Option<Buffers>,
    failed: bool,
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
            lines: Vec::new(),
            changed: false,
            buffers: None,
            failed: false,
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
        };
        self.lines = text(&c, &said);
        self.changed = true;
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
        (width, height): (u32, u32),
        format: DXGI_FORMAT,
    ) {
        if !shown().load(Ordering::Relaxed) || self.lines.is_empty() || self.failed {
            return;
        }
        let began = Instant::now();
        // Not the title's: the frame recorder leaves it out.
        #[cfg(feature = "diagnostics")]
        let _ours = crate::replay::record::unrecorded();
        let scale = scale(height);
        if self.buffers.as_ref().is_none_or(|b| (b.format, b.scale) != (format, scale)) {
            // (What the card may still read stays: a picture's size changes once or twice in a run.)
            std::mem::forget(self.buffers.take());
            self.buffers = Buffers::new(host, format, scale);
            self.failed = self.buffers.is_none();
            self.changed = true;
        }
        let Some(buffers) = &mut self.buffers else { return };
        if std::mem::take(&mut self.changed) {
            buffers.write(&self.lines);
        }
        let (w, h) = size(scale);
        let list = &list.list;
        // SAFETY: an open list and live resources; the box lies within both.
        unsafe {
            if std::mem::take(&mut buffers.fresh) {
                list.CopyResource(&buffers.card, &buffers.uploads[buffers.turn].0);
                list.ResourceBarrier(&[transition(
                    &buffers.card,
                    D3D12_RESOURCE_STATE_COPY_DEST,
                    D3D12_RESOURCE_STATE_COPY_SOURCE,
                )]);
            }
            let from = D3D12_TEXTURE_COPY_LOCATION {
                pResource: std::mem::transmute_copy(&buffers.card),
                Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                        Offset: 0,
                        Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                            Format: format,
                            Width: w,
                            Height: h,
                            Depth: 1,
                            RowPitch: buffers.pitch,
                        },
                    },
                },
            };
            let to = D3D12_TEXTURE_COPY_LOCATION {
                pResource: std::mem::transmute_copy(back),
                Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: 0 },
            };
            let part = D3D12_BOX {
                left: 0,
                top: 0,
                front: 0,
                right: w.min(width),
                bottom: h.min(height),
                back: 1,
            };
            list.CopyTextureRegion(&to, 0, 0, 0, &from, Some(&part));
        }
        self.cost += began.elapsed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every character the lines use has its own picture, and no two look
    /// alike.
    #[test]
    fn characters_differ() {
        let used: Vec<u8> = (b'0'..=b'9').chain(b'A'..=b'Z').chain(*b"./- %=[]_").collect();
        for (i, a) in used.iter().enumerate() {
            assert_ne!(glyph(*a), glyph(b'#'), "{} has a picture", *a as char);
            assert!(glyph(*a).iter().all(|row| row >> GLYPH.0 == 0));
            for b in &used[i + 1..] {
                assert_ne!(glyph(*a), glyph(*b), "{} {}", *a as char, *b as char);
            }
        }
    }

    #[test]
    fn a_box_is_painted() {
        let lines = vec!["I".to_owned(), " i".to_owned()];
        for scale in [1, 2, 4] {
            let (width, height) = size(scale);
            let pitch = (width * 4).next_multiple_of(256) as usize;
            let colours = colours(DXGI_FORMAT_R10G10B10A2_UNORM).unwrap();
            let picture = painted(&lines, scale, colours, pitch);
            assert_eq!(picture.len(), pitch * height as usize);
            let at = |x: u32, y: u32| &picture[y as usize * pitch + x as usize * 4..][..4];
            // The rim is black, the first letter's top row lit in its middle.
            assert_eq!(at(0, 0), colours[0]);
            assert_eq!(at((RIM + 2) * scale, RIM * scale), colours[1]);
            assert_eq!(at((RIM + 2) * scale + scale - 1, RIM * scale + scale - 1), colours[1]);
            assert_eq!(at(RIM * scale, RIM * scale), colours[0]);
            // Small letters are shown as capitals, a line and a cell on.
            assert_eq!(at((RIM + CELL.0 + 2) * scale, (RIM + CELL.1) * scale), colours[1]);
            // Nothing past a line's end.
            assert_eq!(at((RIM + CELL.0 * 2 + 2) * scale, (RIM + CELL.1) * scale), colours[0]);
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
