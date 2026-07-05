//! Text selection over the visible grid: a normalized inclusive cell range, a
//! containment test for highlighting, and flow-style text extraction for copy.
//!
//! Coordinates are viewport cells (column, row). Selection currently covers only
//! what is on screen; extending it across scrollback is a follow-up.

use ferroterm_core::attr;

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

/// Extract the selected text as a flow (first row from the start column to the
/// row end, full middle rows, last row up to the end column), with trailing
/// blanks trimmed per line and rows joined by `\n`.
pub fn selected_text(grid: &Grid, sel: &Selection) -> String {
    let (sx, sy) = sel.start;
    let (ex, ey) = sel.end;
    let mut lines = Vec::new();
    for y in sy..=ey.min(grid.rows.saturating_sub(1)) {
        let x0 = if y == sy { sx } else { 0 };
        let x1 = if y == ey {
            ex
        } else {
            grid.cols.saturating_sub(1)
        };
        let mut line = String::new();
        let mut x = x0;
        while x <= x1 && x < grid.cols {
            let c = grid.cell(x, y);
            // Skip the trailing half of a wide character (its glyph is on the
            // preceding cell).
            if c.flags & attr::WIDE_SPACER != 0 {
                x += 1;
                continue;
            }
            let ch = if c.cp == 0 {
                ' '
            } else {
                char::from_u32(c.cp).unwrap_or(' ')
            };
            line.push(ch);
            x += 1;
        }
        while line.ends_with(' ') {
            line.pop();
        }
        lines.push(line);
    }
    lines.join("\n")
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
