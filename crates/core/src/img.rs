//! Helpers for inline-image escape sequences (iTerm2 OSC 1337 `File=`).
//!
//! The actual pixel decode is deliberately left to the front-end (the browser
//! decodes PNG/JPEG/GIF/WebP natively via `createImageBitmap`), so this module
//! only base64-decodes the payload and sniffs the pixel dimensions and format
//! from the file header — enough to lay the image out in whole cells and hand
//! the raw bytes on. No image decoder is linked into the WASM core.

/// A recognised container format for an inline image.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Png,
    Jpeg,
    Gif,
    Bmp,
    Webp,
}

impl Format {
    /// The MIME type handed to the front-end's `Blob`, so the browser decodes
    /// with the right container hint.
    pub fn mime(self) -> &'static str {
        match self {
            Format::Png => "image/png",
            Format::Jpeg => "image/jpeg",
            Format::Gif => "image/gif",
            Format::Bmp => "image/bmp",
            Format::Webp => "image/webp",
        }
    }
}

/// Decode standard-alphabet base64, skipping any ASCII whitespace (iTerm2 may
/// wrap the payload). Returns `None` on a malformed alphabet character.
pub fn decode_base64(input: &[u8]) -> Option<Vec<u8>> {
    // Reverse alphabet: value for each byte, 0xFF = invalid, 0xFE = skip.
    const INV: [u8; 256] = build_inv();
    let mut out = Vec::with_capacity(input.len() / 4 * 3 + 3);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    let mut pad = 0usize;
    for &b in input {
        let v = INV[b as usize];
        match v {
            0xFE => continue, // whitespace
            0xFD => {
                pad += 1; // '=' padding
                continue;
            }
            0xFF => return None, // invalid
            _ => {}
        }
        // A data char after padding began is malformed.
        if pad != 0 {
            return None;
        }
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

const fn build_inv() -> [u8; 256] {
    let mut t = [0xFFu8; 256];
    let mut i = 0;
    // A-Z, a-z, 0-9, +, /
    while i < 26 {
        t[b'A' as usize + i] = i as u8;
        t[b'a' as usize + i] = 26 + i as u8;
        i += 1;
    }
    let mut d = 0;
    while d < 10 {
        t[b'0' as usize + d] = 52 + d as u8;
        d += 1;
    }
    t[b'+' as usize] = 62;
    t[b'/' as usize] = 63;
    t[b'=' as usize] = 0xFD;
    // Whitespace to skip.
    t[b' ' as usize] = 0xFE;
    t[b'\t' as usize] = 0xFE;
    t[b'\n' as usize] = 0xFE;
    t[b'\r' as usize] = 0xFE;
    t
}

/// Recognise the container format from the leading magic bytes.
pub fn detect_format(b: &[u8]) -> Option<Format> {
    if b.len() >= 8 && b[..8] == [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'] {
        return Some(Format::Png);
    }
    if b.len() >= 3 && b[..3] == [0xFF, 0xD8, 0xFF] {
        return Some(Format::Jpeg);
    }
    if b.len() >= 6 && (&b[..6] == b"GIF87a" || &b[..6] == b"GIF89a") {
        return Some(Format::Gif);
    }
    if b.len() >= 2 && &b[..2] == b"BM" {
        return Some(Format::Bmp);
    }
    if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        return Some(Format::Webp);
    }
    None
}

/// Sniff `(width, height)` in pixels from a file header without decoding the
/// pixel data. Returns `None` for formats/headers we can't read cheaply.
pub fn sniff_dimensions(b: &[u8]) -> Option<(u32, u32)> {
    match detect_format(b)? {
        Format::Png => png_dimensions(b),
        Format::Gif => gif_dimensions(b),
        Format::Jpeg => jpeg_dimensions(b),
        Format::Bmp => bmp_dimensions(b),
        Format::Webp => webp_dimensions(b),
    }
}

fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn png_dimensions(b: &[u8]) -> Option<(u32, u32)> {
    // IHDR is the first chunk: 8 sig + 4 len + "IHDR" + width(4) + height(4).
    if b.len() < 24 || &b[12..16] != b"IHDR" {
        return None;
    }
    Some((be32(b, 16), be32(b, 20)))
}

fn gif_dimensions(b: &[u8]) -> Option<(u32, u32)> {
    // Logical screen width/height are little-endian u16 at offsets 6 and 8.
    if b.len() < 10 {
        return None;
    }
    let w = u16::from_le_bytes([b[6], b[7]]) as u32;
    let h = u16::from_le_bytes([b[8], b[9]]) as u32;
    Some((w, h))
}

fn bmp_dimensions(b: &[u8]) -> Option<(u32, u32)> {
    // BITMAPINFOHEADER width/height are little-endian i32 at offsets 18 and 22.
    if b.len() < 26 {
        return None;
    }
    let w = i32::from_le_bytes([b[18], b[19], b[20], b[21]]);
    let h = i32::from_le_bytes([b[22], b[23], b[24], b[25]]);
    Some((w.unsigned_abs(), h.unsigned_abs()))
}

fn webp_dimensions(b: &[u8]) -> Option<(u32, u32)> {
    // RIFF....WEBP then a format chunk: VP8 (lossy), VP8L (lossless) or VP8X.
    if b.len() < 30 {
        return None;
    }
    match &b[12..16] {
        b"VP8 " => {
            // Lossy: after the 10-byte frame tag, width/height are 14-bit LE at
            // offsets 26/28 (with a start-code 0x9D012A at 23..26).
            if b.len() < 30 {
                return None;
            }
            let w = (u16::from_le_bytes([b[26], b[27]]) & 0x3FFF) as u32;
            let h = (u16::from_le_bytes([b[28], b[29]]) & 0x3FFF) as u32;
            Some((w, h))
        }
        b"VP8L" => {
            // Lossless: 1 signature byte (0x2F) then 14+14 bits packed LE.
            if b.len() < 25 || b[20] != 0x2F {
                return None;
            }
            let bits = u32::from_le_bytes([b[21], b[22], b[23], b[24]]);
            let w = (bits & 0x3FFF) + 1;
            let h = ((bits >> 14) & 0x3FFF) + 1;
            Some((w, h))
        }
        b"VP8X" => {
            // Extended: 24-bit LE (width-1) at 24, (height-1) at 27.
            if b.len() < 30 {
                return None;
            }
            let w = (b[24] as u32 | (b[25] as u32) << 8 | (b[26] as u32) << 16) + 1;
            let h = (b[27] as u32 | (b[28] as u32) << 8 | (b[29] as u32) << 16) + 1;
            Some((w, h))
        }
        _ => None,
    }
}

fn jpeg_dimensions(b: &[u8]) -> Option<(u32, u32)> {
    // Walk JPEG markers until an SOF (Start Of Frame) carries the dimensions.
    let mut i = 2; // skip SOI (FF D8)
    while i + 1 < b.len() {
        if b[i] != 0xFF {
            i += 1;
            continue;
        }
        // Skip fill bytes.
        let mut marker = b[i + 1];
        let mut m = i + 1;
        while marker == 0xFF && m + 1 < b.len() {
            m += 1;
            marker = b[m];
        }
        let seg = m + 1;
        // SOF0..SOF15 except DHT(C4), DAC(CC), RSTn(D0-D7) carry frame size.
        let is_sof =
            (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xC8 && marker != 0xCC;
        if is_sof {
            if seg + 6 >= b.len() {
                return None;
            }
            let h = u16::from_be_bytes([b[seg + 3], b[seg + 4]]) as u32;
            let w = u16::from_be_bytes([b[seg + 5], b[seg + 6]]) as u32;
            return Some((w, h));
        }
        if seg + 1 >= b.len() {
            return None;
        }
        let len = u16::from_be_bytes([b[seg], b[seg + 1]]) as usize;
        if len < 2 {
            return None;
        }
        i = seg + len;
    }
    None
}

/// A parsed dimension request from an iTerm2 `width=`/`height=` argument.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dim {
    /// `auto` or absent — size from the image's own pixels.
    Auto,
    /// `N` — a count of terminal cells.
    Cells(u32),
    /// `Npx` — pixels.
    Pixels(u32),
    /// `N%` — a percentage of the terminal's width/height.
    Percent(u32),
}

impl Dim {
    /// Parse one iTerm2 dimension token. Unknown/garbage falls back to `Auto`.
    pub fn parse(s: &str) -> Dim {
        let s = s.trim();
        if s.is_empty() || s.eq_ignore_ascii_case("auto") {
            return Dim::Auto;
        }
        if let Some(p) = s.strip_suffix('%') {
            return p.trim().parse().map(Dim::Percent).unwrap_or(Dim::Auto);
        }
        if let Some(p) = s.strip_suffix("px") {
            return p.trim().parse().map(Dim::Pixels).unwrap_or(Dim::Auto);
        }
        s.parse().map(Dim::Cells).unwrap_or(Dim::Auto)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip() {
        assert_eq!(decode_base64(b"aGVsbG8=").unwrap(), b"hello");
        assert_eq!(decode_base64(b"Zm9vYmE=").unwrap(), b"fooba");
        assert_eq!(decode_base64(b"AAECAwQF").unwrap(), &[0, 1, 2, 3, 4, 5]);
        // Whitespace tolerated.
        assert_eq!(decode_base64(b"aGVs\nbG8=").unwrap(), b"hello");
        // Invalid alphabet rejected.
        assert!(decode_base64(b"aGVs*G8=").is_none());
    }

    #[test]
    fn png_header_dimensions() {
        // 3x2 PNG (only the signature + IHDR are needed for a size sniff).
        let mut b = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
        b.extend_from_slice(&[0, 0, 0, 13]); // IHDR length
        b.extend_from_slice(b"IHDR");
        b.extend_from_slice(&3u32.to_be_bytes());
        b.extend_from_slice(&2u32.to_be_bytes());
        assert_eq!(detect_format(&b), Some(Format::Png));
        assert_eq!(sniff_dimensions(&b), Some((3, 2)));
    }

    #[test]
    fn gif_header_dimensions() {
        let mut b = b"GIF89a".to_vec();
        b.extend_from_slice(&10u16.to_le_bytes());
        b.extend_from_slice(&20u16.to_le_bytes());
        assert_eq!(sniff_dimensions(&b), Some((10, 20)));
    }

    #[test]
    fn jpeg_header_dimensions() {
        // SOI, then an APP0 segment, then SOF0 carrying 4x8.
        let mut b = vec![0xFF, 0xD8];
        b.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00]); // APP0 len=4
        b.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]); // SOF0, len, precision
        b.extend_from_slice(&8u16.to_be_bytes()); // height
        b.extend_from_slice(&4u16.to_be_bytes()); // width
        assert_eq!(sniff_dimensions(&b), Some((4, 8)));
    }

    #[test]
    fn dim_parsing() {
        assert_eq!(Dim::parse("auto"), Dim::Auto);
        assert_eq!(Dim::parse(""), Dim::Auto);
        assert_eq!(Dim::parse("10"), Dim::Cells(10));
        assert_eq!(Dim::parse("64px"), Dim::Pixels(64));
        assert_eq!(Dim::parse("50%"), Dim::Percent(50));
        assert_eq!(Dim::parse("garbage"), Dim::Auto);
    }
}
