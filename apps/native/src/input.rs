//! Map winit keyboard events onto `ferroterm-core`'s key encoder, so the native
//! app produces byte-for-byte the same PTY input as the web front-end.

use ferroterm_core::{encode_char, encode_key, Key, Mods};
use winit::keyboard::{Key as WKey, ModifiersState, NamedKey};

pub fn mods(m: ModifiersState) -> Mods {
    Mods {
        shift: m.shift_key(),
        alt: m.alt_key(),
        ctrl: m.control_key(),
        meta: m.super_key(),
    }
}

/// Encode a key press into PTY bytes. Returns an empty vec for keys that
/// produce no input (e.g. a bare modifier, or a Cmd/Super shortcut we leave for
/// the app to handle). `app_cursor` is the terminal's DECCKM state.
pub fn encode(logical: &WKey, m: Mods, app_cursor: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let named = |k: Key| {
        let mut v = Vec::new();
        encode_key(k, m, app_cursor, &mut v);
        v
    };
    match logical {
        WKey::Named(n) => match n {
            NamedKey::ArrowUp => return named(Key::Up),
            NamedKey::ArrowDown => return named(Key::Down),
            NamedKey::ArrowLeft => return named(Key::Left),
            NamedKey::ArrowRight => return named(Key::Right),
            NamedKey::Home => return named(Key::Home),
            NamedKey::End => return named(Key::End),
            NamedKey::PageUp => return named(Key::PageUp),
            NamedKey::PageDown => return named(Key::PageDown),
            NamedKey::Insert => return named(Key::Insert),
            NamedKey::Delete => return named(Key::Delete),
            NamedKey::Enter => return named(Key::Enter),
            NamedKey::Backspace => return named(Key::Backspace),
            NamedKey::Tab => return named(Key::Tab),
            NamedKey::Escape => return named(Key::Escape),
            NamedKey::Space => {
                encode_char(' ', m, &mut out);
                return out;
            }
            NamedKey::F1 => return named(Key::F(1)),
            NamedKey::F2 => return named(Key::F(2)),
            NamedKey::F3 => return named(Key::F(3)),
            NamedKey::F4 => return named(Key::F(4)),
            NamedKey::F5 => return named(Key::F(5)),
            NamedKey::F6 => return named(Key::F(6)),
            NamedKey::F7 => return named(Key::F(7)),
            NamedKey::F8 => return named(Key::F(8)),
            NamedKey::F9 => return named(Key::F(9)),
            NamedKey::F10 => return named(Key::F(10)),
            NamedKey::F11 => return named(Key::F(11)),
            NamedKey::F12 => return named(Key::F(12)),
            _ => {}
        },
        WKey::Character(s) => {
            // Leave Cmd/Super combos for the app (copy/paste, etc.).
            if m.meta {
                return out;
            }
            if let Some(c) = s.chars().next() {
                encode_char(c, m, &mut out);
                return out;
            }
        }
        _ => {}
    }
    out
}
