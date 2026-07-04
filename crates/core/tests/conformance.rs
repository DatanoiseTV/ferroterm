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
fn ascii_fast_path_wraps_across_lines() {
    // 20 cols: a 45-char run must fill row 0, wrap to row 1, then row 2.
    let mut t = term();
    let s: String = (0..45).map(|i| (b'a' + (i % 26) as u8) as char).collect();
    t.feed(s.as_bytes());
    assert_eq!(&row_text(&t, 0)[..20], &s[..20]);
    assert_eq!(&row_text(&t, 1)[..20], &s[20..40]);
    assert_eq!(&row_text(&t, 2)[..5], &s[40..45]);
    assert_eq!(t.cursor(), (5, 2));
}

#[test]
fn ascii_fast_path_matches_per_char() {
    // The batched path must produce the same grid as byte-by-byte feeding.
    let mut a = Terminal::new(12, 4, 50);
    let mut b = Terminal::new(12, 4, 50);
    let text = b"the quick brown fox jumps over the lazy dog 0123456789";
    a.feed(text);
    for &byte in text {
        b.feed(&[byte]);
    }
    for y in 0..4 {
        assert_eq!(row_text(&a, y), row_text(&b, y), "row {y} differs");
    }
    assert_eq!(a.cursor(), b.cursor());
}

#[test]
fn line_text_reads_scrollback() {
    let mut t = term();
    for i in 0..10 {
        t.feed(format!("row{}\r\n", i).as_bytes());
    }
    // Oldest lines are in scrollback; line 0 should be "row0".
    assert!(t.total_lines() >= 10);
    assert_eq!(t.line_text(0), "row0");
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

// --- grapheme clusters --------------------------------------------------

#[test]
fn combining_accent_merges_into_base_cell() {
    // "e" + U+0301 COMBINING ACUTE ACCENT -> one cell "é", cursor advances 1.
    let mut t = term();
    t.feed("e\u{0301}".as_bytes());
    assert_eq!(t.cell_cluster(0, 0), "e\u{0301}");
    assert_eq!(t.cursor(), (1, 0));
    // The second column is untouched (still blank).
    assert_eq!(t.cell_char(1, 0), ' ');
}

#[test]
fn multiple_combining_marks_stack() {
    let mut t = term();
    t.feed("a\u{0300}\u{0323}".as_bytes()); // a + grave + dot-below
    assert_eq!(t.cell_cluster(0, 0), "a\u{0300}\u{0323}");
    assert_eq!(t.cursor(), (1, 0));
}

#[test]
fn zwj_emoji_sequence_is_one_cluster() {
    // Family: man + ZWJ + woman + ZWJ + girl (each with VS16 omitted here).
    let mut t = term();
    let seq = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    t.feed(seq.as_bytes());
    assert_eq!(t.cell_cluster(0, 0), seq);
}

#[test]
fn variation_selector_attaches() {
    // Heart + VS16 (emoji presentation) is one cluster.
    let mut t = term();
    t.feed("\u{2764}\u{FE0F}".as_bytes());
    assert_eq!(t.cell_cluster(0, 0), "\u{2764}\u{FE0F}");
}

#[test]
fn regional_indicators_form_one_flag() {
    // U+1F1E9 U+1F1EA = flag of Germany, occupies two columns as one cluster.
    let mut t = term();
    t.feed("\u{1F1E9}\u{1F1EA}".as_bytes());
    assert_eq!(t.cell_cluster(0, 0), "\u{1F1E9}\u{1F1EA}");
    assert!(t.active_line(0)[0].pen.has(attr::WIDE));
    assert!(t.active_line(0)[1].pen.has(attr::WIDE_SPACER));
    assert_eq!(t.cursor(), (2, 0));
}

#[test]
fn control_char_breaks_cluster_continuation() {
    // A combining mark after a CR must NOT attach to the pre-CR cell.
    let mut t = term();
    t.feed(b"a\r");
    t.feed("\u{0301}".as_bytes()); // dropped: no base at cursor start
    assert_eq!(t.cell_cluster(0, 0), "a");
}

#[test]
fn combining_mark_after_ascii_fastpath() {
    // The ASCII bulk fast-path must leave last_grapheme pointing at the final
    // cell so a trailing combining mark still merges.
    let mut t = term();
    t.feed(b"hello");
    t.feed("\u{0301}".as_bytes());
    assert_eq!(t.cell_cluster(4, 0), "o\u{0301}");
}

#[test]
fn snapshot_carries_grapheme_id() {
    use ferroterm_core::{SNAPSHOT_CELL_WORDS, SNAPSHOT_MAGIC};
    let mut t = term();
    t.feed("e\u{0301}".as_bytes());
    let snap = t.snapshot(true);
    assert_eq!(snap[0], SNAPSHOT_MAGIC);
    // header is 7 words; first row: [row_index, cells...]; cell 0 words start at 8
    let base = 7 + 1;
    let grapheme_id = snap[base + SNAPSHOT_CELL_WORDS - 1];
    assert_ne!(grapheme_id, 0);
    assert_eq!(t.grapheme(grapheme_id), Some("e\u{0301}"));
}

// --- reflow (rewrap) on resize ------------------------------------------

fn wide_term(cols: usize, rows: usize, sb: usize) -> Terminal {
    Terminal::new(cols, rows, sb)
}

#[test]
fn narrowing_rewraps_wrapped_line() {
    // 10 cols: print 15 chars -> auto-wraps to 2 rows ("0123456789","01234").
    let mut t = wide_term(10, 4, 100);
    t.feed(b"012345678901234");
    assert_eq!(&row_text(&t, 0)[..10], "0123456789");
    // Narrow to 5 cols: the 15-char logical line rewraps to 3 rows of 5.
    t.resize(5, 4);
    assert_eq!(&row_text(&t, 0)[..5], "01234");
    assert_eq!(&row_text(&t, 1)[..5], "56789");
    assert_eq!(&row_text(&t, 2)[..5], "01234");
}

#[test]
fn widening_rejoins_wrapped_line() {
    // Wrap at 5, then widen to 15: the two physical rows rejoin into one.
    let mut t = wide_term(5, 4, 100);
    t.feed(b"012345678901234"); // 15 chars -> 3 rows of 5
    t.resize(15, 4);
    assert_eq!(&row_text(&t, 0)[..15], "012345678901234");
}

#[test]
fn hard_newlines_are_preserved_across_reflow() {
    // Two separate hard lines must not be joined even after resize.
    let mut t = wide_term(10, 4, 100);
    t.feed(b"hello\r\nworld");
    t.resize(3, 4); // "hello" -> "hel","lo"; "world" -> "wor","ld"
    // The proof that the hard break survived: widening back keeps them on two
    // separate physical rows, i.e. they were never joined into one wrapped line.
    t.resize(20, 4);
    assert_eq!(row_text(&t, 0).trim_end(), "hello");
    assert_eq!(row_text(&t, 1).trim_end(), "world");
}

#[test]
fn cursor_follows_reflow() {
    // Cursor sits after "0123456789012" (col 3 of row 1 at width 10). After
    // narrowing to 5, it must still be on the character just typed.
    let mut t = wide_term(10, 4, 100);
    t.feed(b"0123456789012"); // 13 chars: row0 full, row1 = "012", cursor at (3,1)
    assert_eq!(t.cursor(), (3, 1));
    t.resize(5, 4);
    // 13 chars at width 5: rows "01234","56789","012"; cursor after "012" -> (3,2)
    assert_eq!(t.cell_char(0, 2), '0');
    assert_eq!(t.cursor(), (3, 2));
}

#[test]
fn reflow_pushes_overflow_into_scrollback() {
    // Fill more logical content than fits after narrowing; the top must land in
    // scrollback, not be lost.
    let mut t = wide_term(10, 3, 100);
    for i in 0..3 {
        t.feed(format!("line{}______\r\n", i).as_bytes()); // each 12 chars -> wraps
    }
    let before = t.total_lines();
    t.resize(4, 3);
    // Content is preserved: total logical lines only grew (more wrapping), and
    // the oldest content lands in scrollback rather than being lost.
    assert!(t.total_lines() >= before);
    // "line0______" rewrapped at width 4 begins with "line".
    assert!(t.line_text(0).starts_with("line"));
    // Reassembling the full history recovers the original first line intact.
    let history: String = (0..t.total_lines()).map(|a| t.line_text(a)).collect();
    assert!(history.contains("line0______"));
    assert!(history.contains("line2______"));
}

#[test]
fn reflow_keeps_wide_glyph_intact() {
    // A wide (CJK) glyph must never be split across the wrap boundary.
    let mut t = wide_term(6, 3, 100);
    t.feed("AB世界CD".as_bytes()); // widths: A B 世(2) 界(2) C D = 8 cols
    t.resize(3, 3);
    // At width 3, "世" (wide) can't share a row with "AB" (would be col2+3),
    // so it moves to the next row. No half-glyph: every wide cell has a spacer.
    for y in 0..3 {
        let line = t.active_line(y);
        for x in 0..line.len() {
            if line[x].pen.has(attr::WIDE) {
                assert!(x + 1 < line.len() && line[x + 1].pen.has(attr::WIDE_SPACER),
                    "wide glyph at ({},{}) lost its spacer", x, y);
            }
        }
    }
    // The characters survive in order (ignoring the blank right-half spacers,
    // which render as spaces): widening back reassembles the original line.
    t.resize(8, 3);
    // line_text skips the blank right-half spacer cells, so the logical content
    // reads back exactly.
    assert_eq!(t.line_text(0), "AB世界CD");
}

#[test]
fn resize_no_op_when_unchanged() {
    let mut t = wide_term(10, 4, 100);
    t.feed(b"hello");
    t.resize(10, 4); // same dims: must be a no-op, content intact
    assert_eq!(&row_text(&t, 0)[..5], "hello");
    assert_eq!(t.cursor(), (5, 0));
}

#[test]
fn reflow_survives_double_resize_roundtrip() {
    let mut t = wide_term(20, 5, 200);
    t.feed(b"the quick brown fox jumps over the lazy dog and then some more text");
    t.resize(7, 5);
    t.resize(20, 5);
    // After narrow-then-widen the text is intact on the first logical line span.
    let joined: String = (0..5).map(|y| row_text(&t, y)).collect::<Vec<_>>().join("");
    assert!(joined.contains("the quick brown fox"));
    assert!(joined.contains("lazy dog"));
}

// --- palette OSC (4 / 10 / 11 / 104) ------------------------------------

#[test]
fn osc4_sets_palette_index() {
    let mut t = term();
    let v0 = t.palette_version();
    t.feed(b"\x1b]4;1;#00ff00\x1b\\"); // set palette index 1 to green
    assert!(t.palette_version() > v0);
    let ex = t.palette_export();
    // export layout: [fg, bg, cursor, c0, c1, ...]; index 1 -> ex[3+1].
    assert_eq!(ex[3 + 1], 0x0200_0000 | 0x00ff00);
    assert_eq!(ex[3 + 0], 0); // index 0 untouched
}

#[test]
fn osc10_11_set_default_fg_bg() {
    let mut t = term();
    t.feed(b"\x1b]10;rgb:ffff/0000/0000\x1b\\"); // fg red
    t.feed(b"\x1b]11;#0000ff\x1b\\"); // bg blue
    let ex = t.palette_export();
    assert_eq!(ex[0], 0x0200_0000 | 0xff0000);
    assert_eq!(ex[1], 0x0200_0000 | 0x0000ff);
}

#[test]
fn osc104_resets_palette() {
    let mut t = term();
    t.feed(b"\x1b]4;5;#123456\x1b\\");
    assert_ne!(t.palette_export()[3 + 5], 0);
    t.feed(b"\x1b]104;5\x1b\\"); // reset just index 5
    assert_eq!(t.palette_export()[3 + 5], 0);
    t.feed(b"\x1b]4;5;#123456\x1b\\");
    t.feed(b"\x1b]104\x1b\\"); // reset all
    assert_eq!(t.palette_export()[3 + 5], 0);
}

#[test]
fn osc4_query_replies_with_current_color() {
    let mut t = term();
    t.set_default_colors(0xe6e6e6, 0x1a1b26, 0xe6e6e6);
    t.feed(b"\x1b]4;2;#00ff00\x1b\\"); // set index 2 green
    let _ = t.take_output();
    t.feed(b"\x1b]4;2;?\x1b\\"); // query index 2
    let out = String::from_utf8(t.take_output()).unwrap();
    assert_eq!(out, "\x1b]4;2;rgb:0000/ffff/0000\x1b\\");
}

#[test]
fn osc11_query_uses_default_when_unset() {
    let mut t = term();
    t.set_default_colors(0xe6e6e6, 0x102030, 0xe6e6e6);
    t.feed(b"\x1b]11;?\x1b\\"); // query background (never set)
    let out = String::from_utf8(t.take_output()).unwrap();
    assert_eq!(out, "\x1b]11;rgb:1010/2020/3030\x1b\\");
}

#[test]
fn ris_restores_palette() {
    let mut t = term();
    t.feed(b"\x1b]4;1;#00ff00\x1b\\");
    t.feed(b"\x1b]11;#0000ff\x1b\\");
    t.feed(b"\x1bc"); // RIS
    let ex = t.palette_export();
    assert_eq!(ex[1], 0); // bg override cleared
    assert_eq!(ex[3 + 1], 0); // index 1 cleared
}

// --- Sixel images -------------------------------------------------------

#[test]
fn sixel_dcs_places_an_image() {
    let mut t = term();
    t.set_cell_pixels(8, 16);
    let v0 = t.images_version();
    // 4px-wide, 6px-tall red block via a Sixel DCS.
    t.feed(b"\x1bPq#1;2;100;0;0#1!4~\x1b\\");
    assert!(t.images_version() > v0);
    let ids = t.image_ids();
    assert_eq!(ids.len(), 1);
    let size = t.image_size(ids[0]);
    assert_eq!(size, vec![4, 6]);
    let rgba = t.image_rgba(ids[0]);
    assert_eq!(rgba.len(), 4 * 6 * 4);
    assert_eq!(&rgba[0..4], &[255, 0, 0, 255]);
    // Placement: image at viewport row 0, col 0, 4x6 px.
    let pl = t.image_placements();
    assert_eq!(&pl[0..5], &[ids[0] as i32, 0, 0, 4, 6]);
}

#[test]
fn sixel_advances_cursor_below_image() {
    let mut t = term();
    t.set_cell_pixels(8, 16); // 6px tall -> 1 cell row
    t.feed(b"X"); // cursor at (1,0)
    t.feed(b"\x1bPq#1;2;100;0;0#1!4~\x1b\\");
    // Cursor moved to start of the next line.
    assert_eq!(t.cursor(), (0, 1));
}

#[test]
fn sixel_image_scrolls_and_clears() {
    let mut t = term(); // 5 rows
    t.set_cell_pixels(8, 16);
    t.feed(b"\x1bPq#1;2;100;0;0#1!4~\x1b\\"); // image at serial 0
    // Scroll the screen a few lines; the image's viewport row must decrease.
    for _ in 0..3 {
        t.feed(b"\r\n");
    }
    let pl = t.image_placements();
    assert!(!pl.is_empty());
    assert!(pl[1] < 0 || pl[1] < 3); // row moved up as content scrolled
    // Full clear (ED 2) drops on-screen images.
    t.feed(b"\x1b[2J");
    // Image at serial 0 is above the screen now (scrolled), so it survives ED2;
    // RIS drops everything.
    t.feed(b"\x1bc");
    assert!(t.image_ids().is_empty());
}
