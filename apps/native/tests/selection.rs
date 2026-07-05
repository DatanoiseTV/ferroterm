//! Selection text-extraction over a real decoded grid (no GPU needed).

use ferroterm_core::Terminal;
use ferroterm_native::selection::{selected_text, word_range, Selection};
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
fn single_row_extracts_and_trims() {
    let grid = grid_of(20, 2, "hello world");
    // Select "hello" (cols 0..=4 on row 0).
    let sel = Selection::new((0, 0), (4, 0));
    assert_eq!(selected_text(&grid, &sel), "hello");
    // Select past the text into trailing blanks — they are trimmed.
    let sel = Selection::new((6, 0), (19, 0));
    assert_eq!(selected_text(&grid, &sel), "world");
}

#[test]
fn multi_row_flows_and_joins_with_newline() {
    let grid = grid_of(20, 3, "abc\r\ndef");
    // From (1,0) to (1,1): rest of row 0 ("bc") + start of row 1 ("de").
    let sel = Selection::new((1, 0), (1, 1));
    assert_eq!(selected_text(&grid, &sel), "bc\nde");
}

#[test]
fn full_rows_in_the_middle_are_whole() {
    let grid = grid_of(10, 4, "one\r\ntwo\r\nthree");
    // Row 0 tail, whole row 1, row 2 head.
    let sel = Selection::new((2, 0), (2, 2)); // (col 2 row 0) .. (col 2 row 2)
    assert_eq!(selected_text(&grid, &sel), "e\ntwo\nthr");
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
