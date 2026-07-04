//! A Sixel (DEC VT340) graphics decoder: turns a DCS Sixel payload into an
//! RGBA bitmap. Supports raster attributes, RGB and HLS color definitions,
//! run-length (`!`), carriage-return (`$`) and new-line (`-`) controls.
//!
//! Unset pixels are left transparent so the image composites over the terminal
//! background.

/// A decoded Sixel image as tightly-packed RGBA (row-major, `width*height*4`).
pub struct SixelImage {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

/// Hard caps so a hostile payload can't allocate unbounded memory.
const MAX_DIM: usize = 4096;

/// Decode a DCS payload (everything after `ESC P`) as Sixel. Returns `None` if
/// the payload is not a Sixel sequence (final selector byte is not `q`).
pub fn decode(data: &[u8]) -> Option<SixelImage> {
    // Split leading params/intermediates from the body at the `q` selector.
    let mut i = 0;
    // Leading parameters are digits and ';'; intermediates 0x20..0x2f. The
    // selector for Sixel is 'q' (0x71).
    while i < data.len() {
        let b = data[i];
        if b == b'q' {
            break;
        }
        if b.is_ascii_digit() || b == b';' || (0x20..=0x2f).contains(&b) {
            i += 1;
        } else {
            return None; // unexpected byte before selector -> not sixel
        }
    }
    if i >= data.len() || data[i] != b'q' {
        return None;
    }
    let body = &data[i + 1..];

    let mut palette = default_palette();
    let mut color: usize = 0;

    // Growable pixel rows; each row is padded to the final width at the end.
    let mut rows: Vec<Vec<[u8; 4]>> = Vec::new();
    let mut cur_x: usize = 0;
    let mut band: usize = 0; // each band is 6 pixel rows tall
    let mut max_width: usize = 0;

    let mut j = 0;
    while j < body.len() {
        let b = body[j];
        match b {
            b'#' => {
                // Color introducer: #Pc  or  #Pc;Pu;Px;Py;Pz
                j += 1;
                let (pc, adv) = parse_num(&body[j..]);
                j += adv;
                let pc = pc as usize % 256;
                if body.get(j) == Some(&b';') {
                    j += 1;
                    let (pu, a) = parse_num(&body[j..]);
                    j += a;
                    let px = read_semi_num(body, &mut j);
                    let py = read_semi_num(body, &mut j);
                    let pz = read_semi_num(body, &mut j);
                    palette[pc] = match pu {
                        2 => rgb_from_percent(px, py, pz),
                        1 => hls_to_rgb(px, py, pz),
                        _ => palette[pc],
                    };
                }
                color = pc;
            }
            b'"' => {
                // Raster attributes "Pan;Pad;Ph;Pv — consumed (size is dynamic).
                j += 1;
                while j < body.len() && (body[j].is_ascii_digit() || body[j] == b';') {
                    j += 1;
                }
            }
            b'!' => {
                // Run-length: !Pn <sixel>
                j += 1;
                let (mut n, adv) = parse_num(&body[j..]);
                j += adv;
                if n == 0 {
                    n = 1;
                }
                if let Some(&s) = body.get(j) {
                    if (0x3f..=0x7e).contains(&s) {
                        put_sixel(
                            &mut rows,
                            &mut cur_x,
                            &mut max_width,
                            band,
                            s - 0x3f,
                            palette[color],
                            n as usize,
                        )?;
                        j += 1;
                    }
                }
            }
            b'$' => {
                // Graphics carriage return.
                cur_x = 0;
                j += 1;
            }
            b'-' => {
                // Graphics new line: next band.
                cur_x = 0;
                band += 1;
                if band * 6 >= MAX_DIM {
                    break;
                }
                j += 1;
            }
            0x3f..=0x7e => {
                put_sixel(
                    &mut rows,
                    &mut cur_x,
                    &mut max_width,
                    band,
                    b - 0x3f,
                    palette[color],
                    1,
                )?;
                j += 1;
            }
            _ => {
                // Whitespace / newlines between tokens are ignored.
                j += 1;
            }
        }
    }

    let width = max_width;
    let height = rows.len();
    if width == 0 || height == 0 {
        return None;
    }

    let mut rgba = vec![0u8; width * height * 4];
    for (y, row) in rows.iter().enumerate() {
        for (x, px) in row.iter().enumerate() {
            let o = (y * width + x) * 4;
            rgba[o..o + 4].copy_from_slice(px);
        }
    }
    Some(SixelImage {
        width,
        height,
        rgba,
    })
}

/// Paint sixel column `bits` (6 vertical pixels) `repeat` times starting at
/// `cur_x`, growing `rows` as needed. Returns `None` if it would exceed caps.
fn put_sixel(
    rows: &mut Vec<Vec<[u8; 4]>>,
    cur_x: &mut usize,
    max_width: &mut usize,
    band: usize,
    bits: u8,
    color: [u8; 4],
    repeat: usize,
) -> Option<()> {
    if *cur_x + repeat > MAX_DIM {
        return None;
    }
    let y0 = band * 6;
    // Ensure rows exist for this band (6 pixel rows).
    if y0 + 6 > rows.len() {
        rows.resize(y0 + 6, Vec::new());
    }
    for _ in 0..repeat {
        let x = *cur_x;
        for i in 0..6 {
            if bits & (1 << i) != 0 {
                let row = &mut rows[y0 + i];
                if x + 1 > row.len() {
                    row.resize(x + 1, [0, 0, 0, 0]);
                }
                row[x] = color;
            }
        }
        *cur_x += 1;
    }
    if *cur_x > *max_width {
        *max_width = *cur_x;
    }
    Some(())
}

/// Read a `;`-prefixed number (advancing past the `;`); 0 if absent.
fn read_semi_num(body: &[u8], j: &mut usize) -> u16 {
    if body.get(*j) == Some(&b';') {
        *j += 1;
        let (n, adv) = parse_num(&body[*j..]);
        *j += adv;
        n
    } else {
        0
    }
}

/// Parse a leading decimal number; returns (value, bytes_consumed).
fn parse_num(s: &[u8]) -> (u16, usize) {
    let mut n: u32 = 0;
    let mut k = 0;
    while k < s.len() && s[k].is_ascii_digit() {
        n = (n * 10 + (s[k] - b'0') as u32).min(65535);
        k += 1;
    }
    (n as u16, k)
}

fn rgb_from_percent(r: u16, g: u16, b: u16) -> [u8; 4] {
    let s = |v: u16| ((v.min(100) as u32 * 255 + 50) / 100) as u8;
    [s(r), s(g), s(b), 255]
}

/// DEC sixel HLS: H 0..360, L 0..100, S 0..100. Note DEC's hue origin differs
/// from HSL by 120°, handled here.
fn hls_to_rgb(h: u16, l: u16, s: u16) -> [u8; 4] {
    let h = (h % 360) as f32;
    let l = (l.min(100) as f32) / 100.0;
    let s = (s.min(100) as f32) / 100.0;
    // DEC hue 0 = blue; rotate to standard HSL where 0 = red.
    let hh = (h + 240.0) % 360.0 / 360.0;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((hh * 6.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (hh * 6.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let to = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    [to(r), to(g), to(b), 255]
}

/// The VT340 default 16-color sixel palette (indices 0..15); the rest black.
fn default_palette() -> Vec<[u8; 4]> {
    let base: [[u8; 3]; 16] = [
        [0, 0, 0],
        [20, 20, 80],
        [80, 13, 13],
        [20, 80, 20],
        [80, 20, 80],
        [13, 80, 80],
        [80, 80, 20],
        [53, 53, 53],
        [26, 26, 26],
        [33, 33, 60],
        [60, 26, 26],
        [33, 60, 33],
        [60, 33, 60],
        [26, 60, 60],
        [60, 60, 33],
        [80, 80, 80],
    ];
    let mut p = vec![[0u8, 0, 0, 255]; 256];
    for (i, c) in base.iter().enumerate() {
        p[i] = rgb_from_percent(c[0] as u16, c[1] as u16, c[2] as u16);
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_a_simple_block() {
        // Select color 1 (RGB red), one sixel column `~` (all 6 bits) repeated 4.
        // "1;2;100;0;0" defines color 1 = red.
        let data = b"q#1;2;100;0;0#1!4~";
        let img = decode(data).expect("sixel");
        assert_eq!(img.width, 4);
        assert_eq!(img.height, 6);
        // Top-left pixel is red, opaque.
        assert_eq!(&img.rgba[0..4], &[255, 0, 0, 255]);
    }

    #[test]
    fn rejects_non_sixel_dcs() {
        assert!(decode(b"0;1|17/ab").is_none()); // DECUDK etc.
    }

    #[test]
    fn newline_advances_band() {
        // Two bands of one column each.
        let img = decode(b"q#0!1~-#0!1~").unwrap();
        assert_eq!(img.height, 12);
        assert_eq!(img.width, 1);
    }
}
