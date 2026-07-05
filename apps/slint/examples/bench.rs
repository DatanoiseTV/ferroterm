//! Headless benchmark for the Slint front-end's rasterizer (the `Image`
//! renderer). For that mode the entire per-frame cost is CPU work — snapshot
//! decode + software raster into the RGBA buffer — after which Slint just
//! uploads the finished buffer as a texture. So this measures the real thing:
//! parse throughput, snapshot-decode cost, and full-frame raster time (warm and
//! cold glyph cache) across grid sizes.
//!
//!   cargo run --release --example bench
//!
//! No window / GPU is involved; the numbers are the render loop's CPU budget.

use std::time::Instant;

use ferroterm_core::Terminal;
use ferroterm_slint::palette::{Palette, Theme};
use ferroterm_slint::raster::Raster;
use ferroterm_slint::snapshot::Grid;

/// Dense worst case: every cell a distinct printable glyph on a colored
/// background with a colored foreground, so both the parser and the glyph cache
/// are exercised (not one glyph repeated).
fn full_screen(cols: usize, rows: usize) -> Vec<u8> {
    let mut s = String::from("\x1b[H");
    for y in 0..rows {
        for x in 0..cols {
            let c = (x + y) % 216 + 16;
            let glyph = char::from_u32(0x21 + ((x * 3 + y * 7) % 94) as u32).unwrap(); // '!'..'~'
            s.push_str(&format!(
                "\x1b[48;5;{}m\x1b[38;5;{}m{}",
                c,
                (c + 108) % 216 + 16,
                glyph
            ));
        }
        s.push_str("\x1b[0m");
        if y < rows - 1 {
            s.push_str("\r\n");
        }
    }
    s.into_bytes()
}

fn best_ms(iters: usize, trials: usize, mut f: impl FnMut()) -> f64 {
    let mut best = f64::INFINITY;
    for _ in 0..trials {
        let t = Instant::now();
        for _ in 0..iters {
            f();
        }
        best = best.min(t.elapsed().as_secs_f64() * 1000.0 / iters as f64);
    }
    best
}

fn bench_grid(px: f32, cols: usize, rows: usize) {
    let mut term = Terminal::new(cols, rows, 5000);
    let payload = full_screen(cols, rows);

    // Parse throughput: feed ~8 MB of full-screen frames.
    let target = 8 * 1024 * 1024;
    let reps = (target / payload.len()).max(1);
    let t = Instant::now();
    for _ in 0..reps {
        term.feed(&payload);
    }
    let parse_ms = t.elapsed().as_secs_f64() * 1000.0;
    let mb = (reps * payload.len()) as f64 / 1e6;
    let mbps = mb / (parse_ms / 1000.0);

    let mut grid = Grid::default();
    let mut snap = Vec::new();
    term.snapshot_into(true, &mut snap);
    grid.apply(&snap);

    let mut raster = Raster::new(px).expect("font");
    let (w, h) = (raster.cell_w * cols, raster.cell_h * rows);
    let mut buf = vec![0u8; w * h * 4];

    // Cold cache: a fresh raster rasterizes every unique glyph on the first
    // frame. Time exactly one such frame (best of a few fresh rasters).
    let cold_ms = best_ms(1, 5, || {
        let mut r = Raster::new(px).expect("font");
        r.draw(&grid, &Palette::new(Theme::default()), &mut buf, w, h, true);
    });

    let pal = Palette::new(Theme::default());
    // Warm the glyph cache, then measure steady-state cost.
    raster.draw(&grid, &pal, &mut buf, w, h, true);
    let decode_us = best_ms(200, 5, || grid.apply(&snap)) * 1000.0;
    let draw_ms = best_ms(60, 5, || raster.draw(&grid, &pal, &mut buf, w, h, true));

    let cpu_ms = decode_us / 1000.0 + draw_ms;
    let mpix = (w * h) as f64 / 1e6;
    let fillrate = mpix / (draw_ms / 1000.0) / 1000.0; // Gpx/s
    println!(
        "  {:>4}x{:<3} ({:>6} cells, {:>5}x{:<5}px)  parse {:>4.0} MB/s   decode {:>5.3} ms   raster {:>6.3} ms (warm) / {:>6.3} ms (cold)   frame {:>6.3} ms = {:>5.0} fps   fill {:>4.1} Gpx/s",
        cols, rows, cols * rows, w, h, mbps, decode_us / 1000.0, draw_ms, cold_ms, cpu_ms, 1000.0 / cpu_ms, fillrate,
    );
}

fn main() {
    if Raster::new(16.0).is_none() {
        eprintln!("no monospace font found; cannot benchmark the rasterizer");
        return;
    }
    // 2x px so the numbers reflect a HiDPI (Retina) physical buffer, the common
    // case; the buffer is rasterized at physical resolution.
    let px = 32.0;
    println!("ferroterm-slint rasterizer (Image renderer), font {px:.0}px physical:");
    for &(c, r) in &[(80, 24), (120, 40), (200, 50), (300, 80)] {
        bench_grid(px, c, r);
    }
}
