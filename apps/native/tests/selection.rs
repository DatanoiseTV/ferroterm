//! Word-range detection over a real decoded grid (no GPU needed). Selection
//! text extraction is tested in the core crate (`Terminal::selection_text`),
//! which owns the scrollback buffer the extraction spans.

use ferroterm_core::Terminal;
use ferroterm_native::selection::word_range;
use ferroterm_native::snapshot::Grid;

fn grid_of(cols: usize, rows: usize, feed: &str) -> Grid {
    let mut term = Terminal::new(cols, rows, 100);
    term.feed(feed.as_bytes());
    let mut grid = Grid::default();
    let mut snap = Vec::new();
    term.snapshot_into(true, &mut snap);
    grid.apply(&snap);
    grid
}

#[test]
fn word_range_spans_a_word() {
    let grid = grid_of(20, 1, "foo bar");
    // Clicking anywhere in "foo" (cols 0..=2) selects the whole word.
    assert_eq!(word_range(&grid, 1, 0), (0, 2));
    // Clicking in "bar" (cols 4..=6).
    assert_eq!(word_range(&grid, 5, 0), (4, 6));
    // A URL-ish token stays one word across the punctuation we treat as word.
    let grid = grid_of(40, 1, "see https://a.b/c ok");
    assert_eq!(word_range(&grid, 8, 0), (4, 16));
}
