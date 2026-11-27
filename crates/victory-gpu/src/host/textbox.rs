//! White dot letters on a black box, copied onto the swap chain's buffer:
//! what the stats panel (`panel.rs`, F3) and the settings menu (`menu.rs`,
//! F1, with its frame-rate line) are written with. The text is painted into
//! an upload buffer when it changes, copied into a buffer in the card's
//! memory, and a frame only copies the box out of that one.
//!
//! (The letters and the painting were the panel's; trustno1-coder's
//! VIctory-NVIDIA moved them into `host/font.rs` for its F1 menu, which
//! added the menu's characters.)

use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;

use super::{Host, HostList, transition};

/// A character's dots, and with the space beside and below it.
pub(super) const GLYPH: (u32, u32) = (5, 7);
pub(super) const CELL: (u32, u32) = (6, 9);

/// The texts are put into one of these in turn: the card reads the others.
const UPLOADS: usize = 4;

/// Seven rows of five dots, the leftmost the highest bit; small letters
/// are shown as capitals by the callers.
#[rustfmt::skip]
pub(super) const fn glyph(c: u8) -> [u8; 7] {
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
        // The settings menu's (after trustno1-coder's font.rs).
        b'>' => [0b01000, 0b00100, 0b00010, 0b00001, 0b00010, 0b00100, 0b01000],
        b'<' => [0b00010, 0b00100, 0b01000, 0b10000, 0b01000, 0b00100, 0b00010],
        b'*' => [0, 0b00100, 0b10101, 0b01110, 0b10101, 0b00100, 0],
        b':' => [0, 0b01100, 0b01100, 0, 0b01100, 0b01100, 0],
        b'(' => [0b00010, 0b00100, 0b01000, 0b01000, 0b01000, 0b00100, 0b00010],
        b')' => [0b01000, 0b00100, 0b00010, 0b00010, 0b00010, 0b00100, 0b01000],
        b'+' => [0, 0b00100, 0b00100, 0b11111, 0b00100, 0b00100, 0],
        b',' => [0, 0, 0, 0, 0b01100, 0b00100, 0b01000],
        b'!' => [0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0, 0b00100],
        b'?' => [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0, 0b00100],
        b' ' => [0; 7],
        _ => [0b11111, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11111],
    }
}

/// Whether `c` has a picture of its own (as a capital).
pub(super) fn drawable(c: u8) -> bool {
    c == b'#' || glyph(c.to_ascii_uppercase()) != glyph(b'#')
}

/// The bytes of a black and a white pixel of a swap chain's format.
pub(super) fn colours(format: DXGI_FORMAT) -> Option<[&'static [u8]; 2]> {
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

/// Pixels to a dot for a picture that high: a 720-line screenshot of it
/// shows one and a third.
pub(super) fn scale(height: u32) -> u32 {
    (height / 540).max(1)
}

/// A box's characters a line, lines, and rim in dots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Shape {
    pub(super) columns: u32,
    pub(super) lines: u32,
    pub(super) rim: u32,
}

impl Shape {
    /// The box's size in pixels, a dot being `scale` of them each way.
    pub(super) const fn size(self, scale: u32) -> (u32, u32) {
        (
            (self.columns * CELL.0 + 2 * self.rim) * scale,
            (self.lines * CELL.1 + 2 * self.rim) * scale,
        )
    }

    /// The size of the part that shows `lines` (as long as the longest of
    /// them, as high as there are of them).
    pub(super) fn part(self, lines: &[String], scale: u32) -> (u32, u32) {
        let columns = lines.iter().map(|l| l.len() as u32).max().unwrap_or(0).min(self.columns);
        let rows = (lines.len() as u32).min(self.lines);
        ((columns * CELL.0 + 2 * self.rim) * scale, (rows * CELL.1 + 2 * self.rim) * scale)
    }
}

/// The box as rows of `pitch` bytes, into `out` (kept from one text to the
/// next: no new memory to fault in). Black rows copied whole, then each
/// character's lit dots: about 0.1 ms at 1080 lines.
pub(super) fn paint(out: &mut Vec<u8>, shape: Shape, lines: &[String], scale: u32, colours: [&[u8]; 2], pitch: usize) {
    let (width, height) = shape.size(scale);
    let rim = shape.rim as usize;
    let (bytes, scale) = (colours[0].len(), scale as usize);
    let row = width as usize * bytes;
    out.resize(pitch * height as usize, 0);
    let black = colours[0].repeat(width as usize);
    let white = colours[1].repeat(scale);
    for y in 0..height as usize / scale {
        let top = pitch * y * scale;
        out[top..top + row].copy_from_slice(&black);
        let (line, dot) = (y.wrapping_sub(rim) / CELL.1 as usize, y.wrapping_sub(rim) % CELL.1 as usize);
        if let Some(text) = lines.get(line).filter(|_| y >= rim && dot < GLYPH.1 as usize) {
            for (column, c) in text.bytes().take(shape.columns as usize).enumerate() {
                let bits = glyph(c.to_ascii_uppercase())[dot];
                for i in (0..GLYPH.0 as usize).filter(|i| bits >> (GLYPH.0 as usize - 1 - i) & 1 != 0) {
                    let at = top + (rim + column * CELL.0 as usize + i) * scale * bytes;
                    out[at..at + white.len()].copy_from_slice(&white);
                }
            }
        }
        for i in 1..scale {
            out.copy_within(top..top + row, top + i * pitch);
        }
    }
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
    fn new(host: &Host, shape: Shape, format: DXGI_FORMAT, scale: u32) -> Option<Buffers> {
        let (width, height) = shape.size(scale);
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

    fn write(&mut self, shape: Shape, lines: &[String]) {
        let Some(colours) = colours(self.format) else { return };
        let picture = &mut self.picture;
        paint(picture, shape, lines, self.scale, colours, self.pitch as usize);
        self.turn = (self.turn + 1) % UPLOADS;
        // SAFETY: a mapped upload buffer of the picture's size, which the card
        // read frames ago (a text changes twice a second at most, or on a key).
        unsafe { std::ptr::copy_nonoverlapping(picture.as_ptr(), self.uploads[self.turn].1, picture.len()) };
        self.fresh = true;
    }
}

/// Where a box goes on the picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Place {
    TopLeft,
    Middle,
}

/// A box of text of one shape, and its buffers.
pub(super) struct TextBox {
    shape: Shape,
    lines: Vec<String>,
    changed: bool,
    buffers: Option<Buffers>,
    failed: bool,
}

impl TextBox {
    pub(super) fn new(shape: Shape) -> TextBox {
        TextBox { shape, lines: Vec::new(), changed: false, buffers: None, failed: false }
    }

    /// The text from the next draw on (painted again only when it differs).
    pub(super) fn set(&mut self, lines: Vec<String>) {
        if lines != self.lines {
            self.lines = lines;
            self.changed = true;
        }
    }

    /// Records the copy of the box (of it the part its text fills) onto
    /// `back`, a buffer of the swap chain of that size and format in the
    /// state to be copied into.
    pub(super) fn draw(
        &mut self,
        host: &Host,
        list: &HostList,
        back: &ID3D12Resource,
        (width, height): (u32, u32),
        format: DXGI_FORMAT,
        place: Place,
    ) {
        if self.lines.is_empty() || self.failed {
            return;
        }
        // Not the title's: the frame recorder leaves it out.
        #[cfg(feature = "diagnostics")]
        let _ours = crate::replay::record::unrecorded();
        let scale = scale(height);
        if self.buffers.as_ref().is_none_or(|b| (b.format, b.scale) != (format, scale)) {
            // (What the card may still read stays: a picture's size changes once or twice in a run.)
            std::mem::forget(self.buffers.take());
            self.buffers = Buffers::new(host, self.shape, format, scale);
            self.failed = self.buffers.is_none();
            self.changed = true;
        }
        let Some(buffers) = &mut self.buffers else { return };
        if std::mem::take(&mut self.changed) {
            buffers.write(self.shape, &self.lines);
        }
        let (w, h) = self.shape.size(scale);
        let (pw, ph) = self.shape.part(&self.lines, scale);
        let (pw, ph) = (pw.min(width), ph.min(height));
        let (x, y) = match place {
            Place::TopLeft => (0, 0),
            Place::Middle => ((width - pw) / 2, (height - ph) / 2),
        };
        let list = &list.list;
        // SAFETY: an open list and live resources; the box lies within both.
        // The card's buffer is a buffer: it is back in the common state after
        // each list, promoted to the copy's destination, then to its source.
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
                right: pw,
                bottom: ph,
                back: 1,
            };
            list.CopyTextureRegion(&to, x, y, 0, &from, Some(&part));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHAPE: Shape = Shape { columns: 40, lines: 14, rim: 3 };

    fn painted(lines: &[String], scale: u32, colours: [&[u8]; 2], pitch: usize) -> Vec<u8> {
        let mut out = Vec::new();
        paint(&mut out, SHAPE, lines, scale, colours, pitch);
        out
    }

    /// Every character the texts use has its own picture, and no two look
    /// alike.
    #[test]
    fn characters_differ() {
        let used: Vec<u8> = (b'0'..=b'9')
            .chain(b'A'..=b'Z')
            .chain(*b"./- %=[]_<>*:()+,!?")
            .collect();
        for (i, a) in used.iter().enumerate() {
            assert!(drawable(*a), "{} has a picture", *a as char);
            assert!(glyph(*a).iter().all(|row| row >> GLYPH.0 == 0));
            for b in &used[i + 1..] {
                assert_ne!(glyph(*a), glyph(*b), "{} {}", *a as char, *b as char);
            }
        }
        assert!(!drawable(b'&'));
    }

    #[test]
    fn a_box_is_painted() {
        let lines = vec!["I".to_owned(), " i".to_owned()];
        let rim = SHAPE.rim;
        for scale in [1, 2, 4] {
            let (width, height) = SHAPE.size(scale);
            let pitch = (width * 4).next_multiple_of(256) as usize;
            let colours = colours(DXGI_FORMAT_R10G10B10A2_UNORM).unwrap();
            let picture = painted(&lines, scale, colours, pitch);
            assert_eq!(picture.len(), pitch * height as usize);
            let at = |x: u32, y: u32| &picture[y as usize * pitch + x as usize * 4..][..4];
            // The rim is black, the first letter's top row lit in its middle.
            assert_eq!(at(0, 0), colours[0]);
            assert_eq!(at((rim + 2) * scale, rim * scale), colours[1]);
            assert_eq!(at((rim + 2) * scale + scale - 1, rim * scale + scale - 1), colours[1]);
            assert_eq!(at(rim * scale, rim * scale), colours[0]);
            // Small letters are shown as capitals, a line and a cell on.
            assert_eq!(at((rim + CELL.0 + 2) * scale, (rim + CELL.1) * scale), colours[1]);
            // Nothing past a line's end.
            assert_eq!(at((rim + CELL.0 * 2 + 2) * scale, (rim + CELL.1) * scale), colours[0]);
        }
    }

    #[test]
    fn the_part_shown_fits_the_text() {
        let lines = vec!["FPS: 21.5".to_owned()];
        assert_eq!(SHAPE.part(&lines, 2), ((9 * CELL.0 + 6) * 2, (CELL.1 + 6) * 2));
        let long = vec!["X".repeat(99); 99];
        assert_eq!(SHAPE.part(&long, 1), SHAPE.size(1));
    }
}
