//! # ferroterm-core
//!
//! A VT100 / xterm-compatible terminal emulator core, written from scratch in
//! Rust. It turns a byte stream (typically from a PTY) into a grid of styled
//! cells, and encodes user input back into the byte sequences host programs
//! expect. It has **no rendering and no I/O** — a front-end (canvas, WebGL,
//! native) reads [`Terminal::snapshot`] and feeds bytes via [`Terminal::feed`].
//!
//! Features: a full DEC/Williams escape-sequence [`parser`], UTF-8 decoding
//! with astral-plane and wide-character support, SGR incl. 256-color and true
//! color, scroll regions, alternate screen, scrollback, DECSET/DECRST modes,
//! OSC 0/2 titles and OSC 8 hyperlinks, and host replies (DSR/DA).
//!
//! ```
//! use ferroterm_core::Terminal;
//! let mut term = Terminal::new(80, 24, 1000);
//! term.feed(b"\x1b[31mhello\x1b[0m");
//! assert_eq!(term.cell_char(0, 0), 'h');
//! ```

mod cell;
mod grid;
mod keys;
mod parser;
mod reflow;
mod terminal;
mod width;

pub use cell::{attr, Cell, Color, Pen};
pub use grid::{Buffer, Cursor};
pub use keys::{encode_char, encode_key, Key, Mods};
pub use parser::{Params, Parser, Perform};
pub use terminal::{Modes, Terminal, SNAPSHOT_CELL_WORDS, SNAPSHOT_MAGIC};
pub use width::char_width;

impl Terminal {
    /// Convenience accessor: the character at grid position `(x, y)` of the
    /// active buffer. Out-of-range coordinates return `' '`.
    pub fn cell_char(&self, x: usize, y: usize) -> char {
        if x < self.cols() && y < self.rows() {
            self.active_line(y)[x].ch
        } else {
            ' '
        }
    }

    /// The full grapheme cluster displayed at `(x, y)`: either the single
    /// scalar [`cell_char`](Self::cell_char), or the merged cluster string when
    /// combining marks / a ZWJ sequence / a flag attached to that cell.
    pub fn cell_cluster(&self, x: usize, y: usize) -> String {
        if x < self.cols() && y < self.rows() {
            let cell = self.active_line(y)[x];
            match self.grapheme(cell.grapheme) {
                Some(s) => s.to_string(),
                None => cell.ch.to_string(),
            }
        } else {
            String::new()
        }
    }
}
