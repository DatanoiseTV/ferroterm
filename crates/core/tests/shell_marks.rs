//! OSC 133 shell-integration command marks and OSC 7 cwd reports: scripted
//! sessions asserting the exact contents of [`Terminal::blocks`] and
//! [`Terminal::cwd`].

use ferroterm_core::{Block, Terminal};

fn block(
    prompt_line: usize,
    cmd_line: usize,
    output_line: usize,
    end_line: usize,
    exit: Option<i32>,
    done: bool,
) -> Block {
    Block {
        prompt_line,
        cmd_line,
        output_line,
        end_line,
        exit,
        done,
    }
}

#[test]
fn two_completed_commands_and_one_running() {
    let mut t = Terminal::new(20, 5, 100);
    // Command 1: prompt on line 0, output on line 1, D on line 2, exit 0.
    t.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n");
    t.feed(b"\x1b]133;C\x07file1\r\n");
    t.feed(b"\x1b]133;D;0\x07");
    // Command 2: prompt on line 2, no output, D on line 3, exit 1.
    t.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07false\r\n");
    t.feed(b"\x1b]133;C\x07\x1b]133;D;1\x07");
    // Command 3: still running — no D; end_line tracks the cursor.
    t.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07sleep 99\r\n");
    t.feed(b"\x1b]133;C\x07working...");
    assert_eq!(
        t.blocks(),
        &[
            block(0, 0, 1, 2, Some(0), true),
            block(2, 2, 3, 3, Some(1), true),
            block(3, 3, 4, 4, None, false),
        ]
    );
}

#[test]
fn d_without_code_finishes_with_no_exit() {
    let mut t = Terminal::new(20, 5, 100);
    t.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\r\n");
    t.feed(b"\x1b]133;C\x07out\r\n");
    t.feed(b"\x1b]133;D\x1b\\"); // ST-terminated, no code
    assert_eq!(t.blocks(), &[block(0, 0, 1, 2, None, true)]);
}

#[test]
fn marks_ignored_on_alt_screen() {
    let mut t = Terminal::new(20, 5, 100);
    t.feed(b"\x1b]133;A\x07$ vi\r\n");
    assert_eq!(t.blocks().len(), 1);
    t.feed(b"\x1b[?1049h"); // enter alt screen
    t.feed(b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\x1b]133;D;9\x07");
    // No new block, and the open primary-screen block is untouched.
    assert_eq!(t.blocks(), &[block(0, 0, 0, 1, None, false)]);
    t.feed(b"\x1b[?1049l");
    t.feed(b"\x1b]133;D;0\x07"); // back on primary: D lands
    let b = t.blocks()[0];
    assert_eq!((b.exit, b.done), (Some(0), true));
}

#[test]
fn blocks_shift_and_evict_as_scrollback_overflows() {
    // 3 rows + 2 scrollback lines = 5 retained lines.
    let mut t = Terminal::new(20, 3, 2);
    t.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07a\r\n\x1b]133;C\x07x\r\n\x1b]133;D;0\x07");
    t.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07b\r\n\x1b]133;C\x07y\r\n\x1b]133;D;0\x07");
    assert_eq!(
        t.blocks(),
        &[
            block(0, 0, 1, 2, Some(0), true),
            block(2, 2, 3, 4, Some(0), true),
        ]
    );
    // Each newline at the bottom scrolls one line off the top of retained
    // history; block lines shift down and fully-scrolled-out blocks evict.
    t.feed(b"\r\n\r\n\r\n");
    assert_eq!(t.blocks(), &[block(0, 0, 0, 1, Some(0), true)]);
    t.feed(b"\r\n\r\n");
    assert!(t.blocks().is_empty());
}

#[test]
fn block_count_is_capped_at_2048() {
    let mut t = Terminal::new(10, 4, 4096);
    for _ in 0..2050 {
        t.feed(b"\x1b]133;A\x07\r\n");
    }
    assert_eq!(t.blocks().len(), 2048);
    // The two oldest blocks (prompts on lines 0 and 1) were dropped.
    assert_eq!(t.blocks()[0].prompt_line, 2);
}

#[test]
fn row_only_resize_keeps_blocks_column_resize_discards() {
    let mut t = Terminal::new(20, 5, 100);
    t.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07f\r\n\x1b]133;D;0\x07");
    let before = t.blocks().to_vec();
    assert_eq!(before, vec![block(0, 0, 1, 2, Some(0), true)]);
    t.resize(20, 4); // rows only: same column count, numbering preserved
    assert_eq!(t.blocks(), &before[..]);
    t.resize(19, 4); // column change rewraps and renumbers: blocks dropped
    assert!(t.blocks().is_empty());
}

#[test]
fn osc7_sets_percent_decoded_cwd() {
    let mut t = Terminal::new(20, 5, 100);
    assert_eq!(t.cwd(), "");
    t.feed(b"\x1b]7;file://myhost/Users/me/My%20Docs\x07");
    assert_eq!(t.cwd(), "/Users/me/My Docs");
    // UTF-8 percent-escapes, ST-terminated, empty host.
    t.feed(b"\x1b]7;file:///tmp/\xc3\xbc-%C3%A4%C3%B6\x1b\\");
    assert_eq!(t.cwd(), "/tmp/\u{fc}-\u{e4}\u{f6}");
    // Non-file schemes are ignored.
    t.feed(b"\x1b]7;kitty-shell-cwd://host/elsewhere\x07");
    assert_eq!(t.cwd(), "/tmp/\u{fc}-\u{e4}\u{f6}");
    // An empty payload clears the cwd.
    t.feed(b"\x1b]7;\x07");
    assert_eq!(t.cwd(), "");
}
