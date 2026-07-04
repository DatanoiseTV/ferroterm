//! Reflow (rewrap) of the primary screen + scrollback on a width change.
//!
//! Terminals that don't reflow simply truncate or pad each physical line when
//! the window is resized, which mangles any text that had auto-wrapped. Reflow
//! instead treats the scrollback and screen as one continuous stream of
//! *logical* lines — a logical line being a maximal run of physical rows joined
//! across soft (auto-)wraps — and re-splits each logical line at the new width.
//!
//! Hard line breaks (explicit CR/LF) are preserved because only soft-wrapped
//! rows carry [`Line::wrapped`]. The cursor's position is tracked through the
//! rewrap so it lands on the same character.

use crate::cell::Cell;
use crate::grid::Line;

/// The rewrapped stream plus the cursor's new position within it.
pub struct Reflowed {
    /// All physical rows after rewrap, oldest first (scrollback then screen).
    pub rows: Vec<Line>,
    /// Index into [`rows`] of the row the cursor is on.
    pub cursor_row: usize,
    /// Column of the cursor within that row.
    pub cursor_col: usize,
}

/// Rewrap `phys` (a stream of physical rows, oldest first) to `new_cols`.
/// `cursor_row`/`cursor_col` locate the cursor within `phys`.
pub fn reflow(phys: &[Line], cursor_row: usize, cursor_col: usize, new_cols: usize) -> Reflowed {
    let new_cols = new_cols.max(1);
    let default = Cell::default();

    // --- 1. join physical rows into logical lines --------------------------
    // Each logical line is its concatenated cells plus, when the cursor falls
    // inside it, the cursor's offset within that concatenation.
    let mut logicals: Vec<(Vec<Cell>, Option<usize>)> = Vec::new();
    let n = phys.len();
    let mut i = 0;
    while i < n {
        let mut cells: Vec<Cell> = Vec::new();
        let mut cur_off: Option<usize> = None;
        loop {
            let row = &phys[i];
            if i == cursor_row {
                cur_off = Some(cells.len() + cursor_col.min(row.len()));
            }
            cells.extend_from_slice(row);
            let wrapped = row.wrapped;
            i += 1;
            if !wrapped || i >= n {
                break;
            }
            // A wrapped row whose last column is blank and whose successor
            // begins with a wide glyph was padded by the wide-glyph split guard
            // (see below): that pad cell is not real content, so drop it before
            // rejoining or it would accrete a spurious space on every reflow.
            if cells.last() == Some(&default) && phys[i].first().is_some_and(|c| c.is_wide()) {
                cells.pop();
            }
        }
        // Trim trailing default cells (padding) on the hard end of the logical
        // line, but never below the cursor so its column survives.
        let floor = cur_off.map(|o| o + 1).unwrap_or(0);
        let mut end = cells.len();
        while end > floor && cells[end - 1] == default {
            end -= 1;
        }
        cells.truncate(end);
        logicals.push((cells, cur_off));
    }

    // --- 2. re-split each logical line at new_cols -------------------------
    let mut out: Vec<Line> = Vec::new();
    let mut cursor_row_out = 0usize;
    let mut cursor_col_out = 0usize;

    for (cells, cur_off) in logicals {
        let base = out.len();

        if cells.is_empty() {
            if cur_off.is_some() {
                cursor_row_out = base;
                cursor_col_out = 0;
            }
            out.push(Line::from_cells(vec![default; new_cols], false));
            continue;
        }

        // Split into rows of `new_cols`, never separating a wide glyph from its
        // spacer (break the row one column early instead).
        let mut rows: Vec<Vec<Cell>> = vec![Vec::with_capacity(new_cols)];
        let mut cpos: Option<(usize, usize)> = None;
        for (k, cell) in cells.iter().enumerate() {
            if cell.is_wide() && rows.last().unwrap().len() == new_cols - 1 {
                let last = rows.last_mut().unwrap();
                last.push(default); // pad the odd last column
                rows.push(Vec::with_capacity(new_cols));
            }
            if cur_off == Some(k) {
                let r = rows.len() - 1;
                cpos = Some((r, rows[r].len()));
            }
            rows.last_mut().unwrap().push(*cell);
            if rows.last().unwrap().len() == new_cols {
                rows.push(Vec::with_capacity(new_cols));
            }
        }
        // Drop a trailing empty row left by an exact-width fill.
        if rows.len() > 1 && rows.last().unwrap().is_empty() {
            rows.pop();
        }
        // Cursor at or past the end of content (e.g. pending-wrap state).
        if let Some(o) = cur_off {
            if o >= cells.len() {
                let r = rows.len() - 1;
                cpos = Some((r, rows[r].len().min(new_cols - 1)));
            }
        }

        let last = rows.len() - 1;
        for (ri, mut r) in rows.into_iter().enumerate() {
            if r.len() < new_cols {
                r.resize(new_cols, default);
            }
            out.push(Line::from_cells(r, ri != last));
        }
        if let Some((r, c)) = cpos {
            cursor_row_out = base + r;
            cursor_col_out = c;
        }
    }

    if out.is_empty() {
        out.push(Line::from_cells(vec![default; new_cols], false));
    }

    Reflowed {
        rows: out,
        cursor_row: cursor_row_out,
        cursor_col: cursor_col_out,
    }
}
