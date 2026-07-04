//! Behavioral tests for the terminal core. These assert against the *visible
//! grid state*, which is what a renderer would draw — the strongest kind of
//! check for an emulator.

use ferroterm_core::{attr, Color, Terminal};

fn term() -> Terminal {
    Terminal::new(20, 5, 100)
}

/// Read a row's text as a trimmed string.
fn row_text(t: &Terminal, y: usize) -> String {
    let line = t.active_line(y);
    line.iter().map(|c| c.ch).collect::<String>()
}

#[test]
fn plain_text_prints() {
    let mut t = term();
    t.feed(b"hello");
    assert_eq!(&row_text(&t, 0)[..5], "hello");
    assert_eq!(t.cursor(), (5, 0));
}

#[test]
fn carriage_return_and_newline() {
    let mut t = term();
    t.feed(b"abc\r\ndef");
    assert_eq!(&row_text(&t, 0)[..3], "abc");
    assert_eq!(&row_text(&t, 1)[..3], "def");
    assert_eq!(t.cursor(), (3, 1));
}

#[test]
fn autowrap_defers_at_last_column() {
    // 20 cols: print exactly 20 chars, cursor should be pending-wrap at col 19,
    // still on row 0. The 21st char lands on row 1.
    let mut t = term();
    t.feed(b"01234567890123456789"); // 20 chars
    assert_eq!(t.cursor(), (19, 0));
    t.feed(b"X");
    assert_eq!(t.cell_char(0, 1), 'X');
    assert_eq!(t.cursor(), (1, 1));
}

#[test]
fn cursor_position_absolute() {
    let mut t = term();
    t.feed(b"\x1b[3;5Hz"); // row 3, col 5 (1-based)
    assert_eq!(t.cell_char(4, 2), 'z');
}

#[test]
fn erase_in_line() {
    let mut t = term();
    t.feed(b"abcdef\r");
    t.feed(b"\x1b[2C"); // to col 2
    t.feed(b"\x1b[0K"); // erase to right
    assert_eq!(t.cell_char(0, 0), 'a');
    assert_eq!(t.cell_char(1, 0), 'b');
    assert_eq!(t.cell_char(2, 0), ' ');
    assert_eq!(t.cell_char(5, 0), ' ');
}

#[test]
fn sgr_colors_and_attrs() {
    let mut t = term();
    t.feed(b"\x1b[1;31mA\x1b[0mB");
    let a = t.active_line(0)[0];
    assert_eq!(a.ch, 'A');
    assert_eq!(a.pen.fg, Color::Indexed(1));
    assert!(a.pen.has(attr::BOLD));
    let b = t.active_line(0)[1];
    assert_eq!(b.pen.fg, Color::Default);
    assert!(!b.pen.has(attr::BOLD));
}

#[test]
fn sgr_truecolor_semicolon_and_colon() {
    let mut t = term();
    t.feed(b"\x1b[38;2;10;20;30mA");
    assert_eq!(t.active_line(0)[0].pen.fg, Color::Rgb(10, 20, 30));
    t.feed(b"\x1b[38:2::40:50:60mB");
    assert_eq!(t.active_line(0)[1].pen.fg, Color::Rgb(40, 50, 60));
}

#[test]
fn sgr_256_color() {
    let mut t = term();
    t.feed(b"\x1b[38;5;123mA");
    assert_eq!(t.active_line(0)[0].pen.fg, Color::Indexed(123));
}

#[test]
fn scroll_pushes_to_scrollback() {
    let mut t = term(); // 5 rows
    for i in 0..8 {
        t.feed(format!("line{}\r\n", i).as_bytes());
    }
    // 8 lines fed into a 5-row screen -> at least 3 lines in scrollback.
    assert!(t.scrollback_len() >= 3);
    // Bottom of screen shows the most recent content.
    assert!(row_text(&t, 3).starts_with("line7"));
}

#[test]
fn alt_screen_isolation() {
    let mut t = term();
    t.feed(b"primary");
    t.feed(b"\x1b[?1049h"); // enter alt screen
    assert_eq!(t.cell_char(0, 0), ' '); // alt starts blank
    t.feed(b"alt");
    assert_eq!(t.cell_char(0, 0), 'a');
    t.feed(b"\x1b[?1049l"); // back to primary
    assert_eq!(&row_text(&t, 0)[..7], "primary");
}

#[test]
fn wide_char_occupies_two_cells() {
    let mut t = term();
    t.feed("世界".as_bytes());
    let c0 = t.active_line(0)[0];
    assert_eq!(c0.ch, '世');
    assert!(c0.pen.has(attr::WIDE));
    assert!(t.active_line(0)[1].pen.has(attr::WIDE_SPACER));
    assert_eq!(t.active_line(0)[2].ch, '界');
    assert_eq!(t.cursor(), (4, 0));
}

#[test]
fn astral_plane_emoji_is_single_scalar() {
    let mut t = term();
    t.feed("🦀".as_bytes()); // U+1F980, wide
    assert_eq!(t.active_line(0)[0].ch, '🦀');
    assert!(t.active_line(0)[0].pen.has(attr::WIDE));
}

#[test]
fn osc_title() {
    let mut t = term();
    t.feed(b"\x1b]0;My Title\x07");
    assert_eq!(t.title(), "My Title");
    assert!(t.take_title_dirty());
}

#[test]
fn osc8_hyperlink() {
    let mut t = term();
    t.feed(b"\x1b]8;;https://example.com\x07link\x1b]8;;\x07");
    let cell = t.active_line(0)[0];
    assert_eq!(cell.ch, 'l');
    assert!(cell.link != 0);
    assert_eq!(t.link_uri(cell.link), Some("https://example.com"));
    // After closing, further text has no link.
    t.feed(b"X");
    assert_eq!(t.active_line(0)[4].link, 0);
}

#[test]
fn dsr_cursor_position_report() {
    let mut t = term();
    t.feed(b"\x1b[3;7H"); // row 3 col 7
    t.feed(b"\x1b[6n");
    let out = t.take_output();
    assert_eq!(out, b"\x1b[3;7R");
}

#[test]
fn device_attributes() {
    let mut t = term();
    t.feed(b"\x1b[c");
    assert_eq!(t.take_output(), b"\x1b[?1;2c");
}

#[test]
fn scroll_region_and_insert_delete_lines() {
    let mut t = term();
    // region rows 2..4 (1-based) => indices 1..3
    t.feed(b"\x1b[2;4r");
    t.feed(b"\x1b[2;1H"); // to row 2
    t.feed(b"AAA\r\nBBB\r\nCCC");
    // Now delete one line at row 2 -> BBB moves up.
    t.feed(b"\x1b[2;1H\x1b[1M");
    assert!(row_text(&t, 1).starts_with("BBB"));
}

#[test]
fn snapshot_header_is_well_formed() {
    use ferroterm_core::{SNAPSHOT_CELL_WORDS, SNAPSHOT_MAGIC};
    let mut t = term();
    t.feed(b"hi");
    let snap = t.snapshot(true);
    assert_eq!(snap[0], SNAPSHOT_MAGIC);
    assert_eq!(snap[1], 20); // cols
    assert_eq!(snap[2], 5); // rows
    let n_rows = snap[6] as usize;
    assert_eq!(n_rows, 5); // force => all rows
    // First row block: index then cols*words.
    assert_eq!(snap[7], 0); // row index 0
    let first_cp = snap[8]; // codepoint of cell (0,0)
    assert_eq!(first_cp, 'h' as u32);
    // Total length sanity.
    let expected = 7 + n_rows * (1 + 20 * SNAPSHOT_CELL_WORDS);
    assert_eq!(snap.len(), expected);
}

#[test]
fn backspace_and_tab() {
    let mut t = term();
    t.feed(b"ab\x08X"); // backspace over 'b'
    assert_eq!(t.cell_char(1, 0), 'X');
    let mut t2 = term();
    t2.feed(b"\tX");
    assert_eq!(t2.cursor(), (9, 0)); // tab to col 8, then X advances to 9
}

#[test]
fn malicious_long_params_do_not_panic() {
    let mut t = term();
    let mut s = b"\x1b[".to_vec();
    for _ in 0..1000 {
        s.extend_from_slice(b"9;");
    }
    s.push(b'm');
    t.feed(&s); // must not panic or hang
    t.feed(b"ok");
    assert_eq!(t.cell_char(0, 0), 'o');
}
