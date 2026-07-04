//! The cell matrix: a fixed `rows x cols` buffer with a cursor, a vertical
//! scroll region, and the low-level editing primitives the terminal drives
//! (scroll, erase, insert/delete lines and characters).
//!
//! Scrollback and alt-screen switching live one level up in
//! [`crate::terminal::Terminal`]; a `Buffer` only knows about its own visible
//! area.

use crate::cell::{Cell, Pen};

/// A physical row of cells plus a `wrapped` flag: `wrapped == true` means the
/// row was produced by an auto-wrap and its content logically continues on the
/// next physical row. Reflow-on-resize uses this to rejoin and re-split lines.
///
/// `Deref`s to its `Vec<Cell>` so `line[x]`, `line.iter()`, `line.len()`,
/// `line.resize(..)` etc. all work as before the flag existed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    cells: Vec<Cell>,
    pub wrapped: bool,
}

impl Line {
    /// A row of `cols` default (theme-default) blank cells.
    pub fn blank(cols: usize) -> Self {
        Line {
            cells: vec![Cell::default(); cols],
            wrapped: false,
        }
    }

    /// A row of `cols` blanks that keep `pen`'s background (erase semantics).
    pub fn filled(cols: usize, pen: Pen) -> Self {
        Line {
            cells: vec![Cell::blank(pen); cols],
            wrapped: false,
        }
    }

    /// Wrap an existing cell vector (used by reflow).
    pub fn from_cells(cells: Vec<Cell>, wrapped: bool) -> Self {
        Line { cells, wrapped }
    }
}

impl std::ops::Deref for Line {
    type Target = Vec<Cell>;
    fn deref(&self) -> &Vec<Cell> {
        &self.cells
    }
}

impl std::ops::DerefMut for Line {
    fn deref_mut(&mut self) -> &mut Vec<Cell> {
        &mut self.cells
    }
}

/// Cursor position and the pending-wrap flag (deferred auto-wrap at the last
/// column, per DEC/VT behavior).
#[derive(Clone, Copy, Debug, Default)]
pub struct Cursor {
    pub x: usize,
    pub y: usize,
    /// Set after printing into the last column; the next printable char wraps
    /// first instead of the char that filled the column.
    pub pending_wrap: bool,
}

/// A saved cursor (DECSC/DECRC) — position plus the pen in effect.
#[derive(Clone, Copy, Debug, Default)]
pub struct SavedCursor {
    pub x: usize,
    pub y: usize,
    pub pen: Pen,
    pub link: u32,
}

pub struct Buffer {
    pub cols: usize,
    pub rows: usize,
    lines: Vec<Line>,
    pub cursor: Cursor,
    pub saved: SavedCursor,
    /// Inclusive scroll region [top, bottom] in row coordinates.
    pub scroll_top: usize,
    pub scroll_bottom: usize,
    /// Per-row dirty flags consumed by the renderer.
    dirty: Vec<bool>,
}

impl Buffer {
    pub fn new(cols: usize, rows: usize) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        Buffer {
            cols,
            rows,
            lines: vec![Line::blank(cols); rows],
            cursor: Cursor::default(),
            saved: SavedCursor::default(),
            scroll_top: 0,
            scroll_bottom: rows - 1,
            dirty: vec![true; rows],
        }
    }

    // --- access -------------------------------------------------------------

    #[inline]
    pub fn line(&self, y: usize) -> &Line {
        &self.lines[y]
    }

    #[inline]
    pub fn line_mut(&mut self, y: usize) -> &mut Line {
        self.dirty[y] = true;
        &mut self.lines[y]
    }

    #[inline]
    pub fn cell(&self, x: usize, y: usize) -> &Cell {
        &self.lines[y][x]
    }

    #[inline]
    pub fn is_dirty(&self, y: usize) -> bool {
        self.dirty[y]
    }

    pub fn mark_dirty(&mut self, y: usize) {
        if y < self.rows {
            self.dirty[y] = true;
        }
    }

    /// Mark row `y` as auto-wrapped (its content continues on row `y+1`).
    pub fn mark_wrapped(&mut self, y: usize) {
        if y < self.rows {
            self.lines[y].wrapped = true;
        }
    }

    #[inline]
    pub fn is_wrapped(&self, y: usize) -> bool {
        self.lines[y].wrapped
    }

    pub fn mark_all_dirty(&mut self) {
        for d in &mut self.dirty {
            *d = true;
        }
    }

    pub fn clear_dirty(&mut self) {
        for d in &mut self.dirty {
            *d = false;
        }
    }

    // --- cursor -------------------------------------------------------------

    /// Move the cursor to an absolute position, clamped to the buffer, and
    /// clear the pending-wrap flag.
    pub fn goto(&mut self, x: usize, y: usize) {
        self.cursor.x = x.min(self.cols - 1);
        self.cursor.y = y.min(self.rows - 1);
        self.cursor.pending_wrap = false;
    }

    pub fn goto_col(&mut self, x: usize) {
        self.cursor.x = x.min(self.cols - 1);
        self.cursor.pending_wrap = false;
    }

    pub fn goto_row(&mut self, y: usize) {
        self.cursor.y = y.min(self.rows - 1);
        self.cursor.pending_wrap = false;
    }

    // --- resize -------------------------------------------------------------

    /// Reflow-free resize: rows/cols are grown or truncated, cursor clamped.
    /// (Line rewrap is intentionally out of scope for the core; hosts that want
    /// it can rewrap their scrollback.)
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(1);
        let rows = rows.max(1);

        for line in &mut self.lines {
            line.resize(cols, Cell::default());
        }
        if rows > self.rows {
            for _ in self.rows..rows {
                self.lines.push(Line::blank(cols));
            }
        } else {
            self.lines.truncate(rows);
        }

        self.cols = cols;
        self.rows = rows;
        self.dirty = vec![true; rows];
        self.scroll_top = 0;
        self.scroll_bottom = rows - 1;
        self.cursor.x = self.cursor.x.min(cols - 1);
        self.cursor.y = self.cursor.y.min(rows - 1);
        self.cursor.pending_wrap = false;
    }

    /// Replace the entire visible grid with `lines` (exactly `rows` rows, each
    /// padded/truncated to `cols`), reset the scroll region and dirty state, and
    /// place the cursor. Used by reflow-on-resize to install the rewrapped
    /// screen. `saved` is preserved by the caller if needed.
    pub fn set_grid(
        &mut self,
        cols: usize,
        rows: usize,
        mut lines: Vec<Line>,
        cx: usize,
        cy: usize,
    ) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        lines.truncate(rows);
        for l in &mut lines {
            l.resize(cols, Cell::default());
        }
        while lines.len() < rows {
            lines.push(Line::blank(cols));
        }
        self.cols = cols;
        self.rows = rows;
        self.lines = lines;
        self.dirty = vec![true; rows];
        self.scroll_top = 0;
        self.scroll_bottom = rows - 1;
        self.cursor.x = cx.min(cols - 1);
        self.cursor.y = cy.min(rows - 1);
        self.cursor.pending_wrap = false;
    }

    // --- scrolling ----------------------------------------------------------

    /// Scroll the region up by `n` lines (content moves up; blank lines appear
    /// at the bottom). If `evicted` is `Some`, the top lines that scroll out of
    /// the region are moved into it (used to feed scrollback).
    pub fn scroll_up(&mut self, n: usize, pen: Pen, mut evicted: Option<&mut Vec<Line>>) {
        let top = self.scroll_top;
        let bottom = self.scroll_bottom;
        let region = bottom - top + 1;
        let n = n.min(region);
        if n == 0 {
            return;
        }
        for i in 0..n {
            let blank = self.blank_line(pen);
            let removed = std::mem::replace(&mut self.lines[top + i], blank);
            if let Some(ev) = evicted.as_deref_mut() {
                ev.push(removed);
            }
        }
        // Rotate the region so the surviving lines move up into place.
        self.lines[top..=bottom].rotate_left(n);
        for y in top..=bottom {
            self.dirty[y] = true;
        }
    }

    /// Scroll the region down by `n` lines (content moves down; blank lines
    /// appear at the top).
    pub fn scroll_down(&mut self, n: usize, pen: Pen) {
        let top = self.scroll_top;
        let bottom = self.scroll_bottom;
        let region = bottom - top + 1;
        let n = n.min(region);
        if n == 0 {
            return;
        }
        self.lines[top..=bottom].rotate_right(n);
        for i in 0..n {
            self.lines[top + i] = self.blank_line(pen);
        }
        for y in top..=bottom {
            self.dirty[y] = true;
        }
    }

    fn blank_line(&self, pen: Pen) -> Line {
        Line::filled(self.cols, pen)
    }

    // --- line editing -------------------------------------------------------

    /// Insert `n` blank lines at the cursor row, within the scroll region.
    pub fn insert_lines(&mut self, n: usize, pen: Pen) {
        let y = self.cursor.y;
        if y < self.scroll_top || y > self.scroll_bottom {
            return;
        }
        let bottom = self.scroll_bottom;
        let n = n.min(bottom - y + 1);
        self.lines[y..=bottom].rotate_right(n);
        for i in 0..n {
            self.lines[y + i] = self.blank_line(pen);
        }
        for r in y..=bottom {
            self.dirty[r] = true;
        }
    }

    /// Delete `n` lines at the cursor row, within the scroll region.
    pub fn delete_lines(&mut self, n: usize, pen: Pen) {
        let y = self.cursor.y;
        if y < self.scroll_top || y > self.scroll_bottom {
            return;
        }
        let bottom = self.scroll_bottom;
        let n = n.min(bottom - y + 1);
        self.lines[y..=bottom].rotate_left(n);
        for i in 0..n {
            self.lines[bottom - i] = self.blank_line(pen);
        }
        for r in y..=bottom {
            self.dirty[r] = true;
        }
    }

    // --- character editing --------------------------------------------------

    /// Insert `n` blank cells at the cursor, shifting the rest of the line right.
    pub fn insert_chars(&mut self, n: usize, pen: Pen) {
        let y = self.cursor.y;
        let x = self.cursor.x;
        let n = n.min(self.cols - x);
        let line = &mut self.lines[y];
        for i in (x..self.cols).rev() {
            if i >= x + n {
                line[i] = line[i - n];
            } else {
                line[i] = Cell::blank(pen);
            }
        }
        self.dirty[y] = true;
    }

    /// Delete `n` cells at the cursor, shifting the rest of the line left.
    pub fn delete_chars(&mut self, n: usize, pen: Pen) {
        let y = self.cursor.y;
        let x = self.cursor.x;
        let n = n.min(self.cols - x);
        let line = &mut self.lines[y];
        for i in x..self.cols {
            if i + n < self.cols {
                line[i] = line[i + n];
            } else {
                line[i] = Cell::blank(pen);
            }
        }
        self.dirty[y] = true;
    }

    /// Erase `n` cells at the cursor (replace with blanks, no shifting).
    pub fn erase_chars(&mut self, n: usize, pen: Pen) {
        let y = self.cursor.y;
        let x = self.cursor.x;
        let end = (x + n).min(self.cols);
        for i in x..end {
            self.lines[y][i] = Cell::blank(pen);
        }
        self.dirty[y] = true;
    }

    // --- erasing ------------------------------------------------------------

    pub fn erase_line_to_right(&mut self, pen: Pen) {
        let y = self.cursor.y;
        let x = self.cursor.x;
        for i in x..self.cols {
            self.lines[y][i] = Cell::blank(pen);
        }
        // Content is cut here, so the row no longer continues onto the next.
        self.lines[y].wrapped = false;
        self.dirty[y] = true;
    }

    pub fn erase_line_to_left(&mut self, pen: Pen) {
        let y = self.cursor.y;
        let x = self.cursor.x;
        for i in 0..=x.min(self.cols - 1) {
            self.lines[y][i] = Cell::blank(pen);
        }
        self.dirty[y] = true;
    }

    pub fn erase_whole_line(&mut self, pen: Pen) {
        let y = self.cursor.y;
        for i in 0..self.cols {
            self.lines[y][i] = Cell::blank(pen);
        }
        self.lines[y].wrapped = false;
        self.dirty[y] = true;
    }

    pub fn erase_below(&mut self, pen: Pen) {
        self.erase_line_to_right(pen);
        for y in (self.cursor.y + 1)..self.rows {
            for i in 0..self.cols {
                self.lines[y][i] = Cell::blank(pen);
            }
            self.lines[y].wrapped = false;
            self.dirty[y] = true;
        }
    }

    pub fn erase_above(&mut self, pen: Pen) {
        for y in 0..self.cursor.y {
            for i in 0..self.cols {
                self.lines[y][i] = Cell::blank(pen);
            }
            self.lines[y].wrapped = false;
            self.dirty[y] = true;
        }
        self.erase_line_to_left(pen);
    }

    pub fn erase_all(&mut self, pen: Pen) {
        for y in 0..self.rows {
            for i in 0..self.cols {
                self.lines[y][i] = Cell::blank(pen);
            }
            self.lines[y].wrapped = false;
            self.dirty[y] = true;
        }
    }
}
