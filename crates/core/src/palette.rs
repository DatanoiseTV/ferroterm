//! The standard xterm 256-color table and X11 color-spec parsing, used to
//! apply and answer OSC 4 / 10 / 11 / 12 palette control sequences.

/// The default RGB for palette index `i` in the standard xterm 256-color model:
/// 0..15 the ANSI/bright base colors, 16..231 a 6×6×6 cube, 232..255 a grayscale
/// ramp. These are the values a program gets before any OSC 4 override.
pub fn xterm256(i: u8) -> (u8, u8, u8) {
    match i {
        0 => (0, 0, 0),
        1 => (205, 0, 0),
        2 => (0, 205, 0),
        3 => (205, 205, 0),
        4 => (0, 0, 238),
        5 => (205, 0, 205),
        6 => (0, 205, 205),
        7 => (229, 229, 229),
        8 => (127, 127, 127),
        9 => (255, 0, 0),
        10 => (0, 255, 0),
        11 => (255, 255, 0),
        12 => (92, 92, 255),
        13 => (255, 0, 255),
        14 => (0, 255, 255),
        15 => (255, 255, 255),
        16..=231 => {
            const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
            let n = i - 16;
            let r = n / 36;
            let g = (n % 36) / 6;
            let b = n % 6;
            (STEPS[r as usize], STEPS[g as usize], STEPS[b as usize])
        }
        232..=255 => {
            let v = 8 + (i - 232) * 10;
            (v, v, v)
        }
    }
}

/// Parse an X11 / xterm color specification into an 8-bit RGB triple.
///
/// Accepts `rgb:R/G/B` (1–4 hex digits per channel, scaled to 8 bit),
/// `#RGB` / `#RRGGBB` / `#RRRRGGGGBBBB`, and a small set of common X11 color
/// names. Returns `None` for anything unrecognized.
pub fn parse_color_spec(s: &[u8]) -> Option<(u8, u8, u8)> {
    let s = std::str::from_utf8(s).ok()?.trim();

    if let Some(rest) = s.strip_prefix("rgb:") {
        let mut it = rest.split('/');
        let r = scale_hex(it.next()?)?;
        let g = scale_hex(it.next()?)?;
        let b = scale_hex(it.next()?)?;
        if it.next().is_some() {
            return None;
        }
        return Some((r, g, b));
    }

    if let Some(hex) = s.strip_prefix('#') {
        // Equal-length channels: 3, 6, or 12 hex digits (4/8/16 bits each).
        let n = hex.len();
        if n % 3 != 0 {
            return None;
        }
        let w = n / 3;
        if !(1..=4).contains(&w) {
            return None;
        }
        let r = scale_hex(&hex[0..w])?;
        let g = scale_hex(&hex[w..2 * w])?;
        let b = scale_hex(&hex[2 * w..3 * w])?;
        return Some((r, g, b));
    }

    named_color(&s.to_ascii_lowercase())
}

/// Scale a 1–4 digit hex channel to 8 bits (xterm semantics: `v * 255 / max`).
fn scale_hex(h: &str) -> Option<u8> {
    if h.is_empty() || h.len() > 4 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    let max = (1u32 << (4 * h.len())) - 1;
    Some(((v * 255 + max / 2) / max) as u8)
}

fn named_color(name: &str) -> Option<(u8, u8, u8)> {
    Some(match name {
        "black" => (0, 0, 0),
        "red" => (255, 0, 0),
        "green" => (0, 128, 0),
        "yellow" => (255, 255, 0),
        "blue" => (0, 0, 255),
        "magenta" => (255, 0, 255),
        "cyan" => (0, 255, 255),
        "white" => (255, 255, 255),
        "gray" | "grey" => (190, 190, 190),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rgb_forms() {
        assert_eq!(parse_color_spec(b"rgb:ff/00/80"), Some((255, 0, 128)));
        assert_eq!(parse_color_spec(b"rgb:ffff/0000/8000"), Some((255, 0, 128)));
        assert_eq!(parse_color_spec(b"#ff0080"), Some((255, 0, 128)));
        assert_eq!(parse_color_spec(b"#f08"), Some((255, 0, 136)));
        assert_eq!(parse_color_spec(b"red"), Some((255, 0, 0)));
        assert_eq!(parse_color_spec(b"nonsense"), None);
    }

    #[test]
    fn xterm256_cube_and_ramp() {
        assert_eq!(xterm256(16), (0, 0, 0));
        assert_eq!(xterm256(231), (255, 255, 255));
        assert_eq!(xterm256(232), (8, 8, 8));
        assert_eq!(xterm256(255), (238, 238, 238));
    }
}
