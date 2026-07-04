//! WebAssembly bindings for `ferroterm-core`.
//!
//! Exposes a single [`Terminal`] handle to JavaScript. The JS component
//! ([`../../web`]) drives it: `feed()` in bytes from a PTY, `snapshot()` out a
//! packed `Uint32Array` for the renderer, and `key()` / `char()` / `mouse()` to
//! turn user input into the bytes a host program expects.

use ferroterm_core::{encode_char, encode_key, Key, Mods, Terminal as CoreTerminal};
use wasm_bindgen::prelude::*;

/// A terminal instance usable from JavaScript.
#[wasm_bindgen]
pub struct Terminal {
    inner: CoreTerminal,
    // Scratch buffer reused for key/char/mouse encoding to avoid allocations.
    scratch: Vec<u8>,
}

#[wasm_bindgen]
impl Terminal {
    /// Create a terminal of `cols` x `rows` with `scrollback` lines of history.
    #[wasm_bindgen(constructor)]
    pub fn new(cols: usize, rows: usize, scrollback: usize) -> Terminal {
        console_error_panic_hook();
        Terminal {
            inner: CoreTerminal::new(cols, rows, scrollback),
            scratch: Vec::with_capacity(16),
        }
    }

    /// Feed raw bytes received from the host / PTY.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.inner.feed(bytes);
    }

    /// Feed a UTF-16 JS string (encoded to UTF-8 on the boundary).
    #[wasm_bindgen(js_name = feedStr)]
    pub fn feed_str(&mut self, s: &str) {
        self.inner.feed(s.as_bytes());
    }

    /// Produce a render snapshot as a packed `Uint32Array`.
    /// Pass `force = true` to emit every row (e.g. after a theme change).
    pub fn snapshot(&mut self, force: bool) -> Vec<u32> {
        self.inner.snapshot(force)
    }

    /// Resize the grid.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        self.inner.resize(cols, rows);
    }

    pub fn cols(&self) -> usize {
        self.inner.cols()
    }
    pub fn rows(&self) -> usize {
        self.inner.rows()
    }

    /// Drain bytes the terminal wants to send back to the host (reply to DSR,
    /// device attributes, etc.). Send these to the PTY.
    #[wasm_bindgen(js_name = takeOutput)]
    pub fn take_output(&mut self) -> Vec<u8> {
        self.inner.take_output()
    }

    // --- title & bell -------------------------------------------------------

    /// The window title (OSC 0/2). Read [`title_changed`] to know when to poll.
    pub fn title(&self) -> String {
        self.inner.title().to_string()
    }

    #[wasm_bindgen(js_name = titleChanged)]
    pub fn title_changed(&mut self) -> bool {
        self.inner.take_title_dirty()
    }

    #[wasm_bindgen(js_name = bellCount)]
    pub fn bell_count(&self) -> u32 {
        self.inner.bell_count
    }

    // --- links --------------------------------------------------------------

    /// Resolve an OSC 8 hyperlink id (from a cell's `link` field) to its URI.
    #[wasm_bindgen(js_name = linkUri)]
    pub fn link_uri(&self, id: u32) -> Option<String> {
        self.inner.link_uri(id).map(|s| s.to_string())
    }

    /// Resolve a grapheme-cluster id (from a cell's `grapheme` field in the
    /// snapshot) to the full cluster string (base + combining marks, a ZWJ
    /// emoji sequence, or a regional-indicator flag).
    #[wasm_bindgen(js_name = grapheme)]
    pub fn grapheme(&self, id: u32) -> Option<String> {
        self.inner.grapheme(id).map(|s| s.to_string())
    }

    /// Counter bumped whenever the dynamic palette (OSC 4/10/11/12/104…)
    /// changes; the front-end re-reads `paletteExport` when it differs.
    #[wasm_bindgen(js_name = paletteVersion)]
    pub fn palette_version(&self) -> u32 {
        self.inner.palette_version()
    }

    /// Current palette overrides: `[fg, bg, cursor, c0..c255]` (259 words), each
    /// `0` for "no override" or a packed `0x02_RRGGBB`.
    #[wasm_bindgen(js_name = paletteExport)]
    pub fn palette_export(&self) -> Vec<u32> {
        self.inner.palette_export()
    }

    /// Provide the theme's default fg/bg/cursor (packed RGB, low 24 bits) so the
    /// core can answer OSC color queries for un-overridden colors.
    #[wasm_bindgen(js_name = setDefaultColors)]
    pub fn set_default_colors(&mut self, fg: u32, bg: u32, cursor: u32) {
        self.inner.set_default_colors(fg, bg, cursor);
    }

    // --- Sixel images -------------------------------------------------------

    /// Cell size in device pixels, so Sixel images lay out in whole cells.
    #[wasm_bindgen(js_name = setCellPixels)]
    pub fn set_cell_pixels(&mut self, w: usize, h: usize) {
        self.inner.set_cell_pixels(w, h);
    }

    /// Counter bumped when the image set changes; re-sync textures on change.
    #[wasm_bindgen(js_name = imagesVersion)]
    pub fn images_version(&self) -> u32 {
        self.inner.images_version()
    }

    /// Live image ids (oldest first).
    #[wasm_bindgen(js_name = imageIds)]
    pub fn image_ids(&self) -> Vec<u32> {
        self.inner.image_ids()
    }

    /// RGBA bytes of image `id` (`width*height*4`), empty if gone.
    #[wasm_bindgen(js_name = imageRgba)]
    pub fn image_rgba(&self, id: u32) -> Vec<u8> {
        self.inner.image_rgba(id)
    }

    /// `[width, height]` in pixels of image `id`.
    #[wasm_bindgen(js_name = imageSize)]
    pub fn image_size(&self, id: u32) -> Vec<u32> {
        self.inner.image_size(id)
    }

    /// Per-frame placements: flat `[id, viewportRow, col, widthPx, heightPx] …`.
    #[wasm_bindgen(js_name = imagePlacements)]
    pub fn image_placements(&self) -> Vec<i32> {
        self.inner.image_placements()
    }

    // --- scrollback viewport ------------------------------------------------

    #[wasm_bindgen(js_name = scrollLines)]
    pub fn scroll_lines(&mut self, delta: i32) {
        if delta < 0 {
            self.inner.scroll_up_view((-delta) as usize);
        } else if delta > 0 {
            self.inner.scroll_down_view(delta as usize);
        }
    }

    #[wasm_bindgen(js_name = scrollToBottom)]
    pub fn scroll_to_bottom(&mut self) {
        self.inner.scroll_to_bottom();
    }

    #[wasm_bindgen(js_name = scrollToLine)]
    pub fn scroll_to_line(&mut self, abs: usize) {
        self.inner.scroll_to_line(abs);
    }

    // --- text access for search --------------------------------------------

    #[wasm_bindgen(js_name = totalLines)]
    pub fn total_lines(&self) -> usize {
        self.inner.total_lines()
    }

    #[wasm_bindgen(js_name = lineText)]
    pub fn line_text(&self, abs: usize) -> String {
        self.inner.line_text(abs)
    }

    #[wasm_bindgen(js_name = scrollbackLen)]
    pub fn scrollback_len(&self) -> usize {
        self.inner.scrollback_len()
    }

    #[wasm_bindgen(js_name = displayOffset)]
    pub fn display_offset(&self) -> usize {
        self.inner.display_offset()
    }

    // --- mode getters (so the host encodes input correctly) -----------------

    #[wasm_bindgen(js_name = appCursorKeys)]
    pub fn app_cursor_keys(&self) -> bool {
        self.inner.modes().app_cursor_keys
    }

    #[wasm_bindgen(js_name = bracketedPaste)]
    pub fn bracketed_paste(&self) -> bool {
        self.inner.modes().bracketed_paste
    }

    #[wasm_bindgen(js_name = mouseMode)]
    pub fn mouse_mode(&self) -> u16 {
        self.inner.modes().mouse_mode
    }

    #[wasm_bindgen(js_name = mouseSgr)]
    pub fn mouse_sgr(&self) -> bool {
        self.inner.modes().mouse_sgr
    }

    #[wasm_bindgen(js_name = cursorVisible)]
    pub fn cursor_visible(&self) -> bool {
        self.inner.modes().cursor_visible
    }

    // --- input encoding -----------------------------------------------------

    /// Encode a special key press to the bytes a host program expects.
    ///
    /// `key` is the [`KeyCode`] discriminant; `mods` is a bitmask
    /// (1=shift, 2=alt, 4=ctrl, 8=meta). Returns the bytes to send to the PTY.
    pub fn key(&mut self, key: u32, mods: u32) -> Vec<u8> {
        let Some(k) = decode_keycode(key) else {
            return Vec::new();
        };
        self.scratch.clear();
        encode_key(k, decode_mods(mods), self.inner.modes().app_cursor_keys, &mut self.scratch);
        self.scratch.clone()
    }

    /// Encode a printable character press (with Ctrl/Alt folding).
    #[wasm_bindgen(js_name = char)]
    pub fn char_input(&mut self, code_point: u32, mods: u32) -> Vec<u8> {
        let Some(c) = char::from_u32(code_point) else {
            return Vec::new();
        };
        self.scratch.clear();
        encode_char(c, decode_mods(mods), &mut self.scratch);
        self.scratch.clone()
    }

    /// Encode a mouse event to the current mouse protocol, or return empty if
    /// mouse reporting is off. `button`: 0=left,1=middle,2=right,64=wheel-up,
    /// 65=wheel-down. `action`: 0=press,1=release,2=move.
    pub fn mouse(&mut self, button: u32, col: u32, row: u32, action: u32, mods: u32) -> Vec<u8> {
        let modes = self.inner.modes();
        if modes.mouse_mode == 0 {
            return Vec::new();
        }
        // Only report motion when the mode asks for it.
        if action == 2 && modes.mouse_mode != 1002 && modes.mouse_mode != 1003 {
            return Vec::new();
        }
        let m = decode_mods(mods);
        let mut cb = button & 0xff;
        if m.shift {
            cb |= 4;
        }
        if m.alt {
            cb |= 8;
        }
        if m.ctrl {
            cb |= 16;
        }
        if action == 2 {
            cb |= 32; // motion flag
        }
        let mut out = Vec::new();
        let (c1, r1) = (col + 1, row + 1);
        if modes.mouse_sgr {
            let final_ = if action == 1 { 'm' } else { 'M' };
            out.extend_from_slice(format!("\x1b[<{};{};{}{}", cb, c1, r1, final_).as_bytes());
        } else {
            // X10 encoding: byte offset by 32, clamped to the legacy range.
            let enc = |v: u32| -> u8 { (v.min(223) + 32) as u8 };
            let b = if action == 1 { 3 + 32 } else { (cb + 32) as u8 };
            out.extend_from_slice(&[0x1b, b'[', b'M', b, enc(c1), enc(r1)]);
        }
        out
    }
}

fn decode_mods(m: u32) -> Mods {
    Mods {
        shift: m & 1 != 0,
        alt: m & 2 != 0,
        ctrl: m & 4 != 0,
        meta: m & 8 != 0,
    }
}

/// Numeric key codes shared with the JS side (see `web/src/keycodes.js`).
fn decode_keycode(k: u32) -> Option<Key> {
    Some(match k {
        1 => Key::Up,
        2 => Key::Down,
        3 => Key::Right,
        4 => Key::Left,
        5 => Key::Home,
        6 => Key::End,
        7 => Key::Insert,
        8 => Key::Delete,
        9 => Key::PageUp,
        10 => Key::PageDown,
        11 => Key::Enter,
        12 => Key::Backspace,
        13 => Key::Tab,
        14 => Key::Escape,
        100..=111 => Key::F((k - 99) as u8), // 100 => F1 .. 111 => F12
        _ => return None,
    })
}

/// A minimal panic hook that logs to the browser console, so a Rust panic in
/// the parser surfaces as a readable error instead of "unreachable".
fn console_error_panic_hook() {
    use std::sync::Once;
    static SET: Once = Once::new();
    SET.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            error(info.to_string());
        }));
    });
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn error(msg: String);
}
