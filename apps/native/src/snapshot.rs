//! Decode `ferroterm-core`'s packed `u32` snapshot into a persistent grid.
//!
//! The core emits only the header plus the rows that changed since the last
//! snapshot (or every row when forced). We keep the full cell grid here and
//! patch the emitted rows, exactly as the web component's `GridModel` does, so
//! the renderer can upload just the dirty rows.

use ferroterm_core::{SNAPSHOT_CELL_WORDS, SNAPSHOT_MAGIC};

#[derive(Clone, Copy, Default)]
pub struct GCell {
    pub cp: u32,
    pub fg: u32,
    pub bg: u32,
    pub flags: u16,
    pub link: u32,
    pub grapheme: u32,
}

#[derive(Default)]
pub struct Grid {
    pub cols: usize,
    pub rows: usize,
    pub cells: Vec<GCell>,
    pub cursor_x: usize,
    pub cursor_y: usize,
    pub cursor_visible: bool,
    pub cursor_blink: bool,
    pub cursor_on_screen: bool,
    /// Rows changed by the most recent `apply` (empty on a full frame).
    pub dirty_rows: Vec<usize>,
    /// True when the whole grid changed (resize or forced snapshot).
    pub full: bool,
}

impl Grid {
    #[inline]
    pub fn index(&self, x: usize, y: usize) -> usize {
        y * self.cols + x
    }

    #[inline]
    pub fn cell(&self, x: usize, y: usize) -> GCell {
        self.cells[self.index(x, y)]
    }

    /// Patch the grid from a snapshot buffer. Returns silently on a malformed
    /// buffer (wrong magic / truncated), leaving the grid unchanged.
    pub fn apply(&mut self, snap: &[u32]) {
        self.dirty_rows.clear();
        self.full = false;
        if snap.len() < 7 || snap[0] != SNAPSHOT_MAGIC {
            return;
        }
        let cols = snap[1] as usize;
        let rows = snap[2] as usize;
        self.cursor_x = snap[3] as usize;
        self.cursor_y = snap[4] as usize;
        let cflags = snap[5];
        self.cursor_visible = cflags & 1 != 0;
        self.cursor_blink = cflags & 2 != 0;
        self.cursor_on_screen = cflags & 4 != 0;
        let n_rows = snap[6] as usize;

        if cols != self.cols || rows != self.rows {
            self.cols = cols;
            self.rows = rows;
            self.cells = vec![GCell::default(); cols * rows];
            self.full = true;
        }

        let mut p = 7;
        for _ in 0..n_rows {
            if p >= snap.len() {
                break;
            }
            let y = snap[p] as usize;
            p += 1;
            if y >= rows || p + cols * SNAPSHOT_CELL_WORDS > snap.len() {
                break;
            }
            let base = y * cols;
            for x in 0..cols {
                let o = p + x * SNAPSHOT_CELL_WORDS;
                self.cells[base + x] = GCell {
                    cp: snap[o],
                    fg: snap[o + 1],
                    bg: snap[o + 2],
                    flags: snap[o + 3] as u16,
                    link: snap[o + 4],
                    grapheme: snap[o + 5],
                };
            }
            p += cols * SNAPSHOT_CELL_WORDS;
            if !self.full {
                self.dirty_rows.push(y);
            }
        }
    }
}
