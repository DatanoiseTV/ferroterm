//! Keyboard input encoding: turn a logical key press into the byte sequence a
//! host program expects, respecting DECCKM (application cursor keys) and
//! modifier combinations (xterm-style `CSI 1 ; mod <final>`).
//!
//! The web/desktop front-ends map DOM/native key events onto [`Key`] and call
//! [`encode_key`]; keeping this in the core means every front-end encodes keys
//! identically.

/// Modifier bitmask matching xterm's convention (the value is `code + 1`,
/// where `code` is the bitfield below).
#[derive(Clone, Copy, Debug, Default)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub meta: bool,
}

impl Mods {
    fn xterm_code(&self) -> u8 {
        let mut m = 0;
        if self.shift {
            m |= 1;
        }
        if self.alt {
            m |= 2;
        }
        if self.ctrl {
            m |= 4;
        }
        if self.meta {
            m |= 8;
        }
        m + 1
    }

    fn any(&self) -> bool {
        self.shift || self.alt || self.ctrl || self.meta
    }
}

/// A logical key. Printable text is handled by the caller (encoded as UTF-8);
/// this enum covers the keys that need escape sequences.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Right,
    Left,
    Home,
    End,
    Insert,
    Delete,
    PageUp,
    PageDown,
    Enter,
    Backspace,
    Tab,
    Escape,
    F(u8),
}

/// Encode `key` under `mods` into an output byte sequence.
///
/// `app_cursor` is DECCKM state (arrows/Home/End use `SS3`/application form).
pub fn encode_key(key: Key, mods: Mods, app_cursor: bool, out: &mut Vec<u8>) {
    use Key::*;
    let code = mods.xterm_code();

    // Cursor / edit keys: `CSI <final>` normally, `SS3 <final>` in app mode
    // with no modifiers, `CSI 1 ; mod <final>` when modified.
    let csi_letter = |out: &mut Vec<u8>, letter: u8| {
        if mods.any() {
            out.extend_from_slice(format!("\x1b[1;{}{}", code, letter as char).as_bytes());
        } else if app_cursor {
            out.extend_from_slice(&[0x1b, b'O', letter]);
        } else {
            out.extend_from_slice(&[0x1b, b'[', letter]);
        }
    };

    // Edit keys use the numeric `CSI n ~` form.
    let csi_tilde = |out: &mut Vec<u8>, n: u8| {
        if mods.any() {
            out.extend_from_slice(format!("\x1b[{};{}~", n, code).as_bytes());
        } else {
            out.extend_from_slice(format!("\x1b[{}~", n).as_bytes());
        }
    };

    match key {
        Up => csi_letter(out, b'A'),
        Down => csi_letter(out, b'B'),
        Right => csi_letter(out, b'C'),
        Left => csi_letter(out, b'D'),
        Home => csi_letter(out, b'H'),
        End => csi_letter(out, b'F'),
        Insert => csi_tilde(out, 2),
        Delete => csi_tilde(out, 3),
        PageUp => csi_tilde(out, 5),
        PageDown => csi_tilde(out, 6),
        Enter => {
            if mods.alt {
                out.push(0x1b);
            }
            out.push(b'\r');
        }
        Backspace => {
            if mods.alt {
                out.push(0x1b);
            }
            // Send DEL (0x7f); ctrl+backspace sends BS (0x08).
            out.push(if mods.ctrl { 0x08 } else { 0x7f });
        }
        Tab => {
            if mods.shift {
                out.extend_from_slice(b"\x1b[Z"); // back-tab
            } else {
                out.push(b'\t');
            }
        }
        Escape => out.push(0x1b),
        F(n) => encode_fn(n, code, mods.any(), out),
    }
}

fn encode_fn(n: u8, code: u8, modified: bool, out: &mut Vec<u8>) {
    // F1-F4 use SS3 P/Q/R/S; F5-F12 use CSI n ~.
    let ss3 = |out: &mut Vec<u8>, letter: u8| {
        if modified {
            out.extend_from_slice(format!("\x1b[1;{}{}", code, letter as char).as_bytes());
        } else {
            out.extend_from_slice(&[0x1b, b'O', letter]);
        }
    };
    let tilde = |out: &mut Vec<u8>, num: u8| {
        if modified {
            out.extend_from_slice(format!("\x1b[{};{}~", num, code).as_bytes());
        } else {
            out.extend_from_slice(format!("\x1b[{}~", num).as_bytes());
        }
    };
    match n {
        1 => ss3(out, b'P'),
        2 => ss3(out, b'Q'),
        3 => ss3(out, b'R'),
        4 => ss3(out, b'S'),
        5 => tilde(out, 15),
        6 => tilde(out, 17),
        7 => tilde(out, 18),
        8 => tilde(out, 19),
        9 => tilde(out, 20),
        10 => tilde(out, 21),
        11 => tilde(out, 23),
        12 => tilde(out, 24),
        _ => {}
    }
}

/// Encode a printable character with an optional Alt (meta) prefix and Ctrl
/// folding. Returns the bytes to send. Handles the common `Ctrl+letter -> C0`
/// mapping so front-ends don't each reimplement it.
pub fn encode_char(c: char, mods: Mods, out: &mut Vec<u8>) {
    if mods.ctrl && !mods.alt {
        // Ctrl+A..Ctrl+_ fold to 0x01..0x1f.
        let b = c as u32;
        let ctl = match c {
            'a'..='z' => Some((b - 0x60) as u8),
            'A'..='Z' => Some((b - 0x40) as u8),
            '@' => Some(0),
            '[' => Some(0x1b),
            '\\' => Some(0x1c),
            ']' => Some(0x1d),
            '^' => Some(0x1e),
            '_' => Some(0x1f),
            ' ' => Some(0),
            _ => None,
        };
        if let Some(byte) = ctl {
            if mods.alt {
                out.push(0x1b);
            }
            out.push(byte);
            return;
        }
    }
    if mods.alt {
        out.push(0x1b);
    }
    let mut buf = [0u8; 4];
    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
}
