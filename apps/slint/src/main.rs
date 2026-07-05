//! ferroterm-slint: a native terminal built on `ferroterm-core` with a Slint UI.
//!
//! Slint owns the window and keyboard; a PTY runs the shell; the terminal grid
//! is software-rasterized (see [`raster`]) into an RGBA buffer shown as a single
//! Slint `Image`. The terminal engine, key encoding and colors are shared with
//! the web and wgpu front-ends via `ferroterm-core`.
//!
//! Threading: the PTY reader runs on its own thread and pushes bytes over an
//! `mpsc` channel. A ~60 Hz `slint::Timer` on the UI thread drains the channel,
//! feeds the terminal, and repaints only when something changed.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

use ferroterm_core::{Mods, Terminal};
use ferroterm_slint::keymap::map_key;
use ferroterm_slint::palette::{Palette, Theme};
use ferroterm_slint::pty::{Pty, PtyMsg};
use ferroterm_slint::raster::Raster;
use ferroterm_slint::snapshot::Grid;
use slint::{ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer, Timer, TimerMode};

slint::include_modules!();

const SCROLLBACK: usize = 5000;
/// Base font size in logical pixels; scaled by the window's device pixel ratio.
const BASE_PX: f32 = 16.0;
/// Cursor blink half-period in frames (~530 ms at 60 Hz).
const BLINK_FRAMES: u32 = 32;

struct App {
    term: Terminal,
    grid: Grid,
    raster: Raster,
    palette: Palette,
    pty: Pty,
    rx: Receiver<PtyMsg>,
    snap: Vec<u32>,
    cols: usize,
    rows: usize,
    px_w: usize,
    px_h: usize,
    scale: f32,
    dirty: bool,
    blink_frames: u32,
    blink_on: bool,
    exited: bool,
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = MainWindow::new()?;

    let raster = Raster::new(BASE_PX).expect("no monospace font found");
    let (cols, rows) = (80usize, 24usize);
    let term = Terminal::new(cols, rows, SCROLLBACK);
    let (tx, rx) = channel();
    let pty = Pty::spawn(cols as u16, rows as u16, tx).expect("spawn pty");

    let app = Rc::new(RefCell::new(App {
        term,
        grid: Grid::default(),
        raster,
        palette: Palette::new(Theme::default()),
        pty,
        rx,
        snap: Vec::new(),
        cols,
        rows,
        px_w: 0,
        px_h: 0,
        scale: 1.0,
        dirty: true,
        blink_frames: 0,
        blink_on: true,
        exited: false,
    }));

    // Keyboard: encode via ferroterm-core and write straight to the PTY.
    {
        let app = app.clone();
        ui.on_key(move |text, shift, ctrl, alt, meta| {
            let mut a = app.borrow_mut();
            let m = Mods {
                shift,
                alt,
                ctrl,
                meta,
            };
            let app_cursor = a.term.modes().app_cursor_keys;
            let bytes = map_key(text.as_str(), m, app_cursor);
            if !bytes.is_empty() {
                a.pty.write(&bytes);
            }
        });
    }

    // Frame timer: drain PTY, track window size, repaint when dirty.
    let timer = Timer::default();
    {
        let app = app.clone();
        let weak = ui.as_weak();
        timer.start(TimerMode::Repeated, Duration::from_millis(16), move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let mut a = app.borrow_mut();

            // Rebuild the font atlas if the device pixel ratio changed.
            let scale = ui.window().scale_factor().max(1.0);
            if (scale - a.scale).abs() > 0.01 {
                if let Some(r) = Raster::new(BASE_PX * scale) {
                    a.raster = r;
                    a.scale = scale;
                    a.px_w = 0; // force a resize + repaint below
                }
            }

            // Physical window size drives the grid dimensions.
            let sz = ui.window().size();
            let (pw, ph) = (sz.width as usize, sz.height as usize);
            if pw == 0 || ph == 0 {
                return;
            }
            if pw != a.px_w || ph != a.px_h {
                a.px_w = pw;
                a.px_h = ph;
                let cols = (pw / a.raster.cell_w).max(1);
                let rows = (ph / a.raster.cell_h).max(1);
                if cols != a.cols || rows != a.rows {
                    a.cols = cols;
                    a.rows = rows;
                    a.term.resize(cols, rows);
                    a.pty.resize(cols as u16, rows as u16);
                }
                a.dirty = true;
            }

            // Drain everything the shell produced this frame.
            let mut got = false;
            loop {
                match a.rx.try_recv() {
                    Ok(PtyMsg::Data(bytes)) => {
                        a.term.feed(&bytes);
                        got = true;
                    }
                    Ok(PtyMsg::Exit) => {
                        a.exited = true;
                        break;
                    }
                    Err(_) => break,
                }
            }
            if a.exited {
                let _ = slint::quit_event_loop();
                return;
            }
            if got {
                let reply = a.term.take_output();
                if !reply.is_empty() {
                    a.pty.write(&reply);
                }
                a.dirty = true;
            }

            // Cursor blink.
            a.blink_frames += 1;
            if a.blink_frames >= BLINK_FRAMES {
                a.blink_frames = 0;
                a.blink_on = !a.blink_on;
                a.dirty = true;
            }

            if !a.dirty {
                return;
            }
            a.dirty = false;

            // Snapshot -> grid -> pixels -> Image.
            let mut snap = std::mem::take(&mut a.snap);
            a.term.snapshot_into(false, &mut snap);
            a.grid.apply(&snap);
            a.snap = snap;

            let (pw, ph) = (a.px_w, a.px_h);
            let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(pw as u32, ph as u32);
            let blink_on = a.blink_on;
            {
                let App {
                    raster,
                    grid,
                    palette,
                    ..
                } = &mut *a;
                raster.draw(grid, palette, buf.make_mut_bytes(), pw, ph, blink_on);
            }
            ui.set_frame(Image::from_rgba8(buf));
        });
    }

    ui.run()?;
    app.borrow_mut().pty.kill();
    Ok(())
}
