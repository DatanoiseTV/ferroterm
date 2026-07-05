//! Text selection: a normalized inclusive range, a containment test for
//! highlighting, and word-range detection for double-click.
//!
//! Endpoints are `(column, line)`. The app stores them in absolute line
//! coordinates (line 0 = oldest scrollback) so a selection survives scrolling
//! and can span history; text extraction for those coordinates lives in the core
//! terminal (`Terminal::selection_text`), which holds the scrollback buffer.
//! `word_range` operates on the visible grid, where the row is a viewport row.

use crate::snapshot::Grid;

/// An inclusive selection range, normalized so `start` is at or before `end` in
/// row-major (reading) order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Selection {
    pub start: (usize, usize),
    pub end: (usize, usize),
}

impl Selection {
    /// Build a normalized selection from two endpoints (anchor and focus).
    pub fn new(a: (usize, usize), b: (usize, usize)) -> Self {
        // Compare by (row, col) so the earlier reading-order cell is `start`.
        if (a.1, a.0) <= (b.1, b.0) {
            Selection { start: a, end: b }
        } else {
            Selection { start: b, end: a }
        }
    }

    /// A single cell (anchor == focus) is treated as no selection: a plain click
    /// clears rather than selects.
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// Whether cell `(x, y)` falls inside the flow selection.
    pub fn contains(&self, x: usize, y: usize) -> bool {
        let ((sx, sy), (ex, ey)) = (self.start, self.end);
        if y < sy || y > ey {
            return false;
        }
        if sy == ey {
            x >= sx && x <= ex
        } else if y == sy {
            x >= sx
        } else if y == ey {
            x <= ex
        } else {
            true
        }
    }
}

/// Character class for word selection: word characters (identifiers and common
/// URL/path punctuation) group together, whitespace groups, and everything else
/// is treated individually-but-grouped as "other".
#[derive(PartialEq, Eq, Clone, Copy)]
enum Class {
    Word,
    Space,
    Other,
}

fn class_of(cp: u32) -> Class {
    match char::from_u32(cp) {
        None => Class::Space,
        Some(c) if c == ' ' || c == '\0' => Class::Space,
        Some(c) if c.is_alphanumeric() || "_-./~:@%+".contains(c) => Class::Word,
        Some(_) => Class::Other,
    }
}

/// The inclusive column range of the "word" (same character class) around
/// `(x, y)` — for double-click selection.
pub fn word_range(grid: &Grid, x: usize, y: usize) -> (usize, usize) {
    if x >= grid.cols || y >= grid.rows {
        return (x, x);
    }
    let class = |cx: usize| class_of(grid.cell(cx, y).cp);
    let here = class(x);
    let mut lo = x;
    while lo > 0 && class(lo - 1) == here {
        lo -= 1;
    }
    let mut hi = x;
    while hi + 1 < grid.cols && class(hi + 1) == here {
        hi += 1;
    }
    (lo, hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_endpoints() {
        // Anchor after focus in reading order → swapped.
        let s = Selection::new((5, 2), (1, 0));
        assert_eq!(s.start, (1, 0));
        assert_eq!(s.end, (5, 2));
    }

    #[test]
    fn contains_flows_across_rows() {
        let s = Selection::new((3, 0), (2, 2));
        assert!(!s.contains(2, 0), "before start on first row");
        assert!(s.contains(3, 0), "start cell");
        assert!(s.contains(9, 0), "rest of first row");
        assert!(s.contains(0, 1), "whole middle row");
        assert!(s.contains(2, 2), "end cell");
        assert!(!s.contains(3, 2), "after end on last row");
        assert!(!s.contains(0, 3), "below selection");
    }

    #[test]
    fn single_cell_is_empty() {
        assert!(Selection::new((4, 1), (4, 1)).is_empty());
        assert!(!Selection::new((4, 1), (5, 1)).is_empty());
    }
}
