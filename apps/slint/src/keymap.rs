//! Translate Slint key events into PTY bytes via `ferroterm-core`'s encoder, so
//! the Slint app produces byte-for-byte the same input as the web and wgpu
//! front-ends.
//!
//! Slint delivers key presses as `event.text` plus modifier flags. Printable
//! keys arrive as their character; named keys arrive as the fixed private-use /
//! control codepoints Slint documents (the macOS `NSEvent` function-key range
//! plus a handful of C0 controls). We map those codepoints onto
//! [`ferroterm_core::Key`]; everything else is treated as text.

use ferroterm_core::{encode_char, encode_key, Key, Mods};

/// Encode one Slint key press. `app_cursor` is the terminal's DECCKM state.
/// Returns an empty vec for keys that produce no PTY input (bare modifiers, or
/// a Cmd/Super combo left for the app).
pub fn map_key(text: &str, m: Mods, app_cursor: bool) -> Vec<u8> {
    let mut out = Vec::new();

    // A single codepoint may be a named key (Slint's documented mapping).
    let mut chars = text.chars();
    if let (Some(ch), None) = (chars.next(), chars.clone().next()) {
        let named = match ch {
            '\u{F700}' => Some(Key::Up),
            '\u{F701}' => Some(Key::Down),
            '\u{F702}' => Some(Key::Left),
            '\u{F703}' => Some(Key::Right),
            '\u{F729}' => Some(Key::Home),
            '\u{F72B}' => Some(Key::End),
            '\u{F72C}' => Some(Key::PageUp),
            '\u{F72D}' => Some(Key::PageDown),
            '\u{F727}' => Some(Key::Insert),
            '\u{007F}' => Some(Key::Delete),
            '\u{0008}' => Some(Key::Backspace),
            '\u{0009}' => Some(Key::Tab),
            '\u{000A}' | '\u{000D}' => Some(Key::Enter),
            '\u{001B}' => Some(Key::Escape),
            // Slint packs F1..=F12 contiguously from U+F704.
            c if ('\u{F704}'..='\u{F70F}').contains(&c) => {
                Some(Key::F(1 + (c as u32 - 0xF704) as u8))
            }
            _ => None,
        };
        if let Some(k) = named {
            encode_key(k, m, app_cursor, &mut out);
            return out;
        }
        // Bare modifier keys (Control/ControlR/Meta/MetaR) produce no input.
        if matches!(ch, '\u{0011}' | '\u{0016}' | '\u{0017}' | '\u{0018}') {
            return out;
        }
    }

    // Printable text. Leave Cmd/Super combos for the app (copy/paste, etc.).
    if m.meta {
        return out;
    }
    for c in text.chars() {
        encode_char(c, m, &mut out);
    }
    out
}
