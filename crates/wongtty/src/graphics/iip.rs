//! iTerm2's inline image protocol (`OSC 1337 ; File=… : base64 BEL`) as `@xterm/addon-image` 0.10.0-beta.331 reads it:
//! the header, the strict base64 payload and the size an image takes on screen. PNG, JPEG, GIF, WebP and AVIF are
//! decoded by the host (the browser), so only their sizes are read here; QOI, which the addon decodes itself, is not
//! taken.
//!
//! Ported from xterm.js commit 08829c7eb6279dda2a9e86df901353a1a89858ca (MIT): `addons/addon-image/src/
//! IIPHeaderParser.ts` lines 1-262 (the header), `IIPMetrics.ts` lines 1-142 (image sizes), `IIPHandler.ts` lines
//! 206-237 (`_resize`, `_dim`), and the decoding rules of `xterm-wasm-parts` 0.4.1 `lib/base64/Base64Decoder.wasm.js`
//! (strict RFC 4648 §4 base64: no whitespace, padding only at the end).

/// What an OSC 1337 sequence is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    #[default]
    Invalid,
    File,
    MultipartFile,
    FilePart,
    FileEnd,
    ReportCellSize,
}

/// The header fields the addon reads; the defaults are its `DEFAULT_HEADER`.
#[derive(Clone, Debug, PartialEq)]
pub struct Header {
    pub kind: Kind,
    /// `N` cells, `Npx`, `N%` or `auto`.
    pub width: String,
    pub height: String,
    pub preserve_aspect_ratio: u64,
    pub inline: u64,
    pub size: u64,
}

impl Default for Header {
    fn default() -> Self {
        Self {
            kind: Kind::Invalid,
            width: "auto".into(),
            height: "auto".into(),
            preserve_aspect_ratio: 1,
            inline: 0,
            size: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    Start,
    Abort,
    Key,
    Value,
    End,
}

const MAX_FIELD: usize = 1024;
const FILE: &[u8] = b"File";
const MULTIPART_FILE: &[u8] = b"MultipartFile";
const FILE_PART: &[u8] = b"FilePart";
const FILE_END: &[u8] = b"FileEnd";
const REPORT_CELL_SIZE: &[u8] = b"ReportCellSize";

/// The header up to `:`, fed in pieces (`HeaderParser`).
#[derive(Default)]
pub struct HeaderParser {
    state: State,
    buffer: Vec<u8>,
    position: usize,
    key: Vec<u8>,
    pub header: Header,
}

/// What [`HeaderParser::parse`] found in a piece.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Parsed {
    /// The header is broken.
    Abort,
    /// It goes on in the next piece.
    More,
    /// It ended; the payload starts at this index of the piece.
    Data(usize),
}

impl HeaderParser {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn ended(&self) -> bool {
        self.state == State::End
    }

    /// The buffer as the TypeScript keeps it: a fixed array, so bytes past the field being read are left over
    /// from earlier fields (they are compared too).
    fn at(&self, i: usize) -> u8 {
        self.buffer.get(i).copied().unwrap_or(0)
    }

    fn push(&mut self, pos: &mut usize, c: u8) -> bool {
        if *pos >= MAX_FIELD {
            return false;
        }
        if *pos < self.buffer.len() {
            self.buffer[*pos] = c;
        } else {
            self.buffer.push(c);
        }
        *pos += 1;
        true
    }

    fn abort(&mut self) -> Parsed {
        self.header.kind = Kind::Invalid;
        self.state = State::Abort;
        Parsed::Abort
    }

    fn matches(&self, marker: &[u8], from: usize) -> bool {
        (from..marker.len()).all(|k| self.at(k) == marker[k])
    }

    pub fn parse(&mut self, data: &[u8]) -> Parsed {
        if self.state == State::Abort || self.state == State::End {
            return Parsed::Abort;
        }
        if self.state == State::Start && self.position > 14 {
            return Parsed::Abort;
        }
        let mut state = self.state;
        let mut position = self.position;
        let pos = &mut position;
        for (i, &c) in data.iter().enumerate() {
            match c {
                b';' => {
                    if !self.store_value(*pos) {
                        return self.abort();
                    }
                    state = State::Key;
                    *pos = 0;
                }
                b'=' => match state {
                    State::Start => {
                        if self.at(0) == b'F' {
                            if !self.matches(FILE, 0) {
                                return self.abort();
                            }
                            self.header.kind = Kind::File;
                            if *pos == FILE_PART.len() {
                                if !self.matches(FILE_PART, FILE.len()) {
                                    return self.abort();
                                }
                                self.header.kind = Kind::FilePart;
                                self.state = State::End;
                                return Parsed::Data(i + 1);
                            }
                        } else if self.at(0) == b'M' {
                            if !self.matches(MULTIPART_FILE, 0) {
                                return self.abort();
                            }
                            self.header.kind = Kind::MultipartFile;
                        } else {
                            return self.abort();
                        }
                        state = State::Key;
                        *pos = 0;
                    }
                    State::Key => {
                        if !self.store_key(*pos) {
                            return self.abort();
                        }
                        state = State::Value;
                        *pos = 0;
                    }
                    State::Value if !self.push(pos, c) => return self.abort(),
                    _ => {}
                },
                b':' => {
                    if state == State::Value && !self.store_value(*pos) {
                        return self.abort();
                    }
                    self.state = State::End;
                    return Parsed::Data(i + 1);
                }
                _ => {
                    if !self.push(pos, c) {
                        return self.abort();
                    }
                }
            }
        }
        self.state = state;
        self.position = position;
        Parsed::More
    }

    /// The sequence ended before `:` (`FileEnd`, `ReportCellSize`, a `MultipartFile` header); false if that is wrong.
    pub fn end(&mut self) -> bool {
        let pos = self.position;
        match self.state {
            State::Start => {
                for (marker, kind) in [
                    (FILE_END, Kind::FileEnd),
                    (REPORT_CELL_SIZE, Kind::ReportCellSize),
                ] {
                    if pos == marker.len() {
                        if !self.matches(marker, 0) {
                            break;
                        }
                        self.header.kind = kind;
                        self.state = State::End;
                        return true;
                    }
                }
                self.abort();
                false
            }
            State::End => true,
            State::Value if self.header.kind == Kind::MultipartFile => {
                if !self.store_value(pos) {
                    self.abort();
                    return false;
                }
                self.state = State::End;
                true
            }
            _ => {
                self.abort();
                false
            }
        }
    }

    fn store_key(&mut self, pos: usize) -> bool {
        if pos == 0 {
            return false;
        }
        self.key = self.buffer[..pos].to_vec();
        true
    }

    /// The value of the last key read (it stays the key until the next one, as in the TypeScript).
    fn store_value(&mut self, pos: usize) -> bool {
        if self.key.is_empty() {
            return false;
        }
        let v = &self.buffer[..pos.min(self.buffer.len())];
        let int = || {
            v.iter().try_fold(0u64, |n, &c| {
                c.is_ascii_digit()
                    .then(|| n.wrapping_mul(10).wrapping_add((c - b'0') as u64))
            })
        };
        let h = &mut self.header;
        match &self.key[..] {
            b"inline" => int().map(|n| h.inline = n).is_some(),
            b"size" => int().map(|n| h.size = n).is_some(),
            b"preserveAspectRatio" => int().map(|n| h.preserve_aspect_ratio = n).is_some(),
            b"width" | b"height" => {
                let ok = is_size(v);
                if ok {
                    let s = String::from_utf8_lossy(v).into_owned();
                    if self.key == b"width" {
                        h.width = s;
                    } else {
                        h.height = s;
                    }
                }
                ok
            }
            b"name" => is_forgiving_base64(v),
            _ => true,
        }
    }
}

/// `^((auto)|(\d+?((px)|(%)){0,1}))$`
fn is_size(v: &[u8]) -> bool {
    if v == b"auto" {
        return true;
    }
    let digits = v.iter().take_while(|c| c.is_ascii_digit()).count();
    digits > 0 && matches!(&v[digits..], b"" | b"px" | b"%")
}

/// Whether `atob` takes `v` (the `name` field), following the WHATWG forgiving-base64 decode.
fn is_forgiving_base64(v: &[u8]) -> bool {
    let mut s: Vec<u8> = v
        .iter()
        .copied()
        .filter(|c| !matches!(c, b' ' | b'\t' | b'\n' | b'\x0c' | b'\r'))
        .collect();
    if s.len() % 4 == 0 {
        for _ in 0..2 {
            if s.last() == Some(&b'=') {
                s.pop();
            }
        }
    }
    s.len() % 4 != 1 && s.iter().all(|&c| sextet(c).is_some())
}

fn sextet(c: u8) -> Option<u32> {
    Some(match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    } as u32)
}

/// Streaming strict base64: every block but the last must be four alphabet characters; the last has two to four,
/// `=` padding only as its third and fourth.
#[derive(Default)]
pub struct Base64 {
    block: [u8; 4],
    len: usize,
    total: usize,
    error: bool,
}

impl Base64 {
    /// Decodes `data`, handing the bytes to `out`; false once the data is not base64.
    pub fn put(&mut self, data: &[u8], out: &mut Vec<u8>) -> bool {
        for &c in data {
            if self.error {
                return false;
            }
            if self.len == 4 {
                self.error = !decode_block(&self.block, out);
                self.len = 0;
            }
            self.block[self.len] = c;
            self.len += 1;
            self.total += 1;
        }
        !self.error
    }

    /// Characters taken so far.
    pub fn total(&self) -> usize {
        self.total
    }

    /// Decodes the last block; false if the data was not base64.
    pub fn end(&mut self, out: &mut Vec<u8>) -> bool {
        let mut rem = self.len;
        if self.error || rem < 2 {
            return false;
        }
        if rem == 4 {
            if self.block[3] == b'=' {
                rem -= 1;
            }
            if self.block[2] == b'=' {
                rem -= 1;
            }
        }
        let ok = decode_block(&self.block[..rem], out);
        self.len = 0;
        ok
    }
}

fn decode_block(chars: &[u8], out: &mut Vec<u8>) -> bool {
    let mut accu = 0u32;
    for (i, &c) in chars.iter().enumerate() {
        let Some(v) = sextet(c) else { return false };
        accu |= v << (18 - 6 * i);
    }
    let bytes = [(accu >> 16) as u8, (accu >> 8) as u8, accu as u8];
    out.extend_from_slice(&bytes[..chars.len() - 1]);
    true
}

/// An image format the host can decode and the size its header gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metrics {
    pub mime: &'static str,
    pub width: i64,
    pub height: i64,
}

/// The format and size of `len` bytes read through `at` (`imageType`); `None` for anything else.
pub fn image_type(len: usize, at: impl Fn(usize) -> u8) -> Option<Metrics> {
    if len < 32 {
        return None;
    }
    let d = |i: usize| if i < len { at(i) as i64 } else { 0 };
    let le = |i: usize| (d(i) | d(i + 1) << 8 | d(i + 2) << 16 | d(i + 3) << 24) as u32;
    // `<<` on a JS number is 32-bit and signed
    let be =
        |i: usize| (d(i) << 24 | d(i + 1) << 16 | d(i + 2) << 8 | d(i + 3)) as u32 as i32 as i64;
    let m = |mime, width, height| {
        Some(Metrics {
            mime,
            width,
            height,
        })
    };
    if le(0) == 0x474e_5089 && le(4) == 0x0a1a_0a0d && le(12) == 0x5244_4849 {
        return m("image/png", be(16), be(20));
    }
    if d(0) == 0xff && d(1) == 0xd8 && d(2) == 0xff {
        let (w, h) = jpeg_size(len, d);
        return m("image/jpeg", w, h);
    }
    if le(0) == 0x3846_4947 && (d(4) == 0x37 || d(4) == 0x39) && d(5) == 0x61 {
        return m("image/gif", d(7) << 8 | d(6), d(9) << 8 | d(8));
    }
    if le(0) == 0x4646_4952 && le(8) == 0x5042_4557 && le(12) & 0xff_ffff == 0x38_5056 {
        return match d(15) {
            0x58 => m(
                "image/webp",
                (d(24) | d(25) << 8 | d(26) << 16) + 1,
                (d(27) | d(28) << 8 | d(29) << 16) + 1,
            ),
            0x4c if d(20) == 0x2f => {
                let dim = d(21) | d(22) << 8 | d(23) << 16 | d(24) << 24;
                m("image/webp", (dim & 0x3fff) + 1, (dim >> 14 & 0x3fff) + 1)
            }
            0x20 if d(23) == 0x9d && d(24) == 0x01 && d(25) == 0x2a => m(
                "image/webp",
                (d(26) | d(27) << 8) & 0x3fff,
                (d(28) | d(29) << 8) & 0x3fff,
            ),
            _ => None,
        };
    }
    if le(4) == 0x7079_7466 && (le(8) == 0x6669_7661 || le(8) == 0x7369_7661) {
        let limit = (len - 16).min(1024);
        let pos = (8..limit)
            .find(|&i| d(i) == 0x69 && d(i + 1) == 0x73 && d(i + 2) == 0x70 && d(i + 3) == 0x65)?;
        let (width, height) = (be(pos + 8), be(pos + 12));
        return (width > 0 && height > 0).then_some(Metrics {
            mime: "image/avif",
            width,
            height,
        });
    }
    None
}

/// Width and height from the first SOF0/SOF2 marker (`jpgSize`); 0 when there is none.
fn jpeg_size(len: usize, d: impl Fn(usize) -> i64) -> (i64, i64) {
    let mut i = 4usize;
    let mut block = (d(i) << 8 | d(i + 1)) as usize;
    loop {
        i += block;
        if i >= len || d(i) != 0xff {
            return (0, 0);
        }
        if d(i + 1) == 0xc0 || d(i + 1) == 0xc2 {
            if i + 8 < len {
                return (d(i + 7) << 8 | d(i + 8), d(i + 5) << 8 | d(i + 6));
            }
            return (0, 0);
        }
        i += 2;
        block = (d(i) << 8 | d(i + 1)) as usize;
    }
}

/// CSS pixels of a cell and of the terminal's canvas.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Area {
    pub cell_width: f64,
    pub cell_height: f64,
    pub width: f64,
    pub height: f64,
}

/// The size in CSS pixels a `width` × `height` image is shown at, after the header's `width`/`height` and a fit
/// into the terminal (`_resize`, floored like the handler does).
pub fn fit(width: f64, height: f64, header: &Header, area: Area) -> (f64, f64) {
    let rw = dim(&header.width, area.width, area.cell_width);
    let rh = dim(&header.height, area.height, area.cell_height);
    let (w, h) = if rw == 0.0 && rh == 0.0 {
        let f = (area.width / width).min((area.height - area.cell_height) / height);
        if f < 1.0 {
            (width * f, height * f)
        } else {
            (width, height)
        }
    } else if rw == 0.0 {
        (width * rh / height, rh)
    } else if header.preserve_aspect_ratio != 0 || rh == 0.0 {
        (rw, height * rw / width)
    } else {
        (rw, rh)
    };
    (w.floor(), h.floor())
}

/// `_dim`: `auto` is 0, `N%` of `total`, `Npx`, else `N` cells of `cell`.
fn dim(s: &str, total: f64, cell: f64) -> f64 {
    // `parseInt`; no digits (NaN) counts as 0, as `!rw` treats it
    let int = |s: &str| {
        s.bytes()
            .take_while(u8::is_ascii_digit)
            .fold(0.0, |n, c| n * 10.0 + (c - b'0') as f64)
    };
    if s == "auto" {
        0.0
    } else if let Some(n) = s.strip_suffix('%') {
        int(n) * total / 100.0
    } else if let Some(n) = s.strip_suffix("px") {
        int(n)
    } else {
        int(s) * cell
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(s: &str) -> (Parsed, HeaderParser) {
        let mut p = HeaderParser::default();
        let parsed = p.parse(s.as_bytes());
        (parsed, p)
    }

    #[test]
    fn headers() {
        let (parsed, p) = header("File=name=YS5wbmc=;size=3;width=50%;inline=1:AAAA");
        assert_eq!(parsed, Parsed::Data(45));
        assert_eq!(p.header.kind, Kind::File);
        assert_eq!(
            (p.header.inline, p.header.size, &p.header.width[..]),
            (1, 3, "50%")
        );
        assert_eq!(header("FilePart=abc").0, Parsed::Data(9));
        assert_eq!(header("Fail=inline=1:").0, Parsed::Abort);
        assert_eq!(header("File=width=5em:").0, Parsed::Abort);
        assert_eq!(header("File=name=a:").0, Parsed::Abort);
        assert_eq!(header("File=inline=1").0, Parsed::More);
        let (_, mut p) = header("MultipartFile=inline=1;size=9");
        assert!(p.end());
        assert_eq!((p.header.kind, p.header.size), (Kind::MultipartFile, 9));
        let (_, mut p) = header("ReportCellSize");
        assert!(p.end());
        assert_eq!(p.header.kind, Kind::ReportCellSize);
        assert!(!header("FileEnds").1.end());
    }

    #[test]
    fn strict_base64() {
        let decode = |s: &str| {
            let (mut b, mut out) = (Base64::default(), Vec::new());
            (b.put(s.as_bytes(), &mut out) && b.end(&mut out)).then_some(out)
        };
        assert_eq!(decode("aGVsbG8="), Some(b"hello".to_vec()));
        assert_eq!(decode("aGVsbG8"), Some(b"hello".to_vec()));
        assert_eq!(decode("aGk="), Some(b"hi".to_vec()));
        assert_eq!(decode("aGVsbG8=aGk="), None);
        assert_eq!(decode("aGVs bG8="), None);
        assert_eq!(decode("aGVsb"), None);
        assert_eq!(decode(""), None);
    }

    #[test]
    fn sizes() {
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13];
        png.extend_from_slice(b"IHDR\0\0\x01\x2c\0\0\0\xc8");
        png.resize(40, 0);
        let m = image_type(png.len(), |i| png[i]).unwrap();
        assert_eq!((m.mime, m.width, m.height), ("image/png", 300, 200));
        let mut gif = b"GIF89a\x0a\x00\x05\x00".to_vec();
        gif.resize(32, 0);
        assert_eq!(image_type(gif.len(), |i| gif[i]).unwrap().width, 10);
        let mut jpeg = vec![
            0xff, 0xd8, 0xff, 0xe0, 0, 4, 0, 0, 0xff, 0xc0, 0, 17, 8, 0, 20, 0, 30,
        ];
        jpeg.resize(40, 0);
        let m = image_type(jpeg.len(), |i| jpeg[i]).unwrap();
        assert_eq!((m.width, m.height), (30, 20));
        assert_eq!(image_type(10, |_| 0), None);

        let area = Area {
            cell_width: 10.0,
            cell_height: 20.0,
            width: 800.0,
            height: 480.0,
        };
        let h = |w: &str, hh: &str, keep| Header {
            width: w.into(),
            height: hh.into(),
            preserve_aspect_ratio: keep,
            ..Header::default()
        };
        assert_eq!(
            fit(300.0, 200.0, &h("auto", "auto", 1), area),
            (300.0, 200.0)
        );
        // Taller than the screen less a row: scaled down
        assert_eq!(
            fit(400.0, 920.0, &h("auto", "auto", 1), area),
            (200.0, 460.0)
        );
        assert_eq!(fit(300.0, 200.0, &h("10", "auto", 1), area), (100.0, 66.0));
        assert_eq!(
            fit(300.0, 200.0, &h("50%", "100px", 0), area),
            (400.0, 100.0)
        );
        assert_eq!(fit(300.0, 200.0, &h("auto", "2", 1), area), (60.0, 40.0));
    }
}
