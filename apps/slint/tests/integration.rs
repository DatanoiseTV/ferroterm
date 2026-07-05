//! Headless integration test for the Slint front-end's rendering seam.
//!
//! Opening a Slint window needs a display backend, which CI doesn't have — so
//! instead of driving the UI we exercise the pipeline that feeds it: bytes ->
//! `Terminal::feed` -> `snapshot` -> `Grid::apply` -> `Raster::draw` -> pixels,
//! and assert on the decoded grid and the rasterized buffer. This is the same
//! code the running app calls every frame, minus the window.

use ferroterm_core::{attr, Terminal};
use ferroterm_slint::palette::{Palette, Theme};
use ferroterm_slint::raster::Raster;
use ferroterm_slint::snapshot::Grid;

/// Font-independent: feeding SGR + text must decode to the right cells.
#[test]
fn grid_decodes_colored_text() {
    let mut t = Terminal::new(20, 3, 100);
    t.feed(b"\x1b[31mAB\x1b[0m");
    let snap = t.snapshot(true);

    let mut g = Grid::default();
    g.apply(&snap);

    assert_eq!(g.cols, 20);
    assert_eq!(g.rows, 3);

    let a = g.cell(0, 0);
    assert_eq!(char::from_u32(a.cp).unwrap(), 'A');
    // Red = ANSI index 1 => packed 0x01_000001 (see Color::pack).
    assert_eq!(a.fg, 0x0100_0001);
    assert_eq!(a.flags & attr::WIDE_SPACER, 0);

    let b = g.cell(1, 0);
    assert_eq!(char::from_u32(b.cp).unwrap(), 'B');
    assert_eq!(b.fg, 0x0100_0001);
}

/// A wide (double-width) glyph must mark its trailing cell as a spacer.
#[test]
fn wide_char_marks_spacer() {
    let mut t = Terminal::new(20, 1, 100);
    t.feed("世".as_bytes());
    let snap = t.snapshot(true);
    let mut g = Grid::default();
    g.apply(&snap);

    assert_ne!(g.cell(0, 0).flags & attr::WIDE, 0, "cell 0 should be WIDE");
    assert_ne!(
        g.cell(1, 0).flags & attr::WIDE_SPACER,
        0,
        "cell 1 should be a WIDE_SPACER"
    );
}

/// Font-dependent: rasterizing must paint the correct background and draw the
/// glyph in its foreground color. Skips (rather than fails) when the host has
/// no monospace font, so the suite still runs on a minimal CI image.
#[test]
fn rasterizes_fg_over_bg() {
    let Some(mut r) = Raster::new(16.0) else {
        eprintln!("no monospace font available; skipping pixel assertions");
        return;
    };
    let (cw, ch) = (r.cell_w, r.cell_h);

    // Green 'X' at (0,0); cursor advances to (1,0), leaving cells 2,3 empty.
    let mut t = Terminal::new(4, 1, 100);
    t.feed(b"\x1b[32mX");
    let snap = t.snapshot(true);
    let mut g = Grid::default();
    g.apply(&snap);

    let pal = Palette::new(Theme::default());
    let (w, h) = (cw * 4, ch);
    let mut buf = vec![0u8; w * h * 4];
    r.draw(&g, &pal, &mut buf, w, h, false); // cursor off, so cell 1 stays bg

    let px = |x: usize, y: usize| {
        let o = (y * w + x) * 4;
        (buf[o], buf[o + 1], buf[o + 2], buf[o + 3])
    };

    // An empty cell reads as the opaque theme background.
    let bg = pal.theme.bg;
    assert_eq!(px(cw * 3 + cw / 2, ch / 2), (bg.0, bg.1, bg.2, 255));

    // Somewhere in cell 0 a green glyph pixel exists: green dominates and is
    // clearly above the dark background (bg = (26,27,38)).
    let green = ANSI_GREEN;
    let mut found = false;
    for y in 0..ch {
        for x in 0..cw {
            let (pr, pg, pb, pa) = px(x, y);
            assert_eq!(pa, 255, "buffer must stay opaque");
            if pg > 100 && pg as i16 - pb as i16 > 40 && pg as i16 - pr as i16 > 20 {
                found = true;
            }
        }
    }
    assert!(
        found,
        "expected green glyph pixels (fg={green:?}) in cell 0"
    );
}

/// Tokyo Night ANSI green (index 2), for the assertion message above.
const ANSI_GREEN: (u8, u8, u8) = (0x9e, 0xce, 0x6a);
