//! Throughput benchmark for the terminal core. Feeds a representative mix of
//! plain text, SGR color changes, cursor moves and scrolling, and reports
//! parse+apply MB/s. Run with:
//!
//!   cargo run --release -p ferroterm-core --example bench
//!
//! This measures the core only (no rendering) — the same work a real terminal
//! does on the receiving end of a PTY.

use std::time::Instant;

use ferroterm_core::Terminal;

fn build_payload(target_bytes: usize) -> Vec<u8> {
    // A deterministic pseudo-random stream (no rng dependency).
    let words = [
        "ferroterm", "wasm", "rust", "render", "parser", "vt100", "grid", "scroll", "buffer",
    ];
    let mut out = Vec::with_capacity(target_bytes + 4096);
    let mut seed = 0x9e3779b9u32;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    while out.len() < target_bytes {
        let color = 31 + (next() % 7);
        out.extend_from_slice(format!("\x1b[{}m", color).as_bytes());
        let w = words[(next() as usize) % words.len()];
        out.extend_from_slice(w.as_bytes());
        out.extend_from_slice(b"\x1b[0m ");
        if next() % 8 == 0 {
            out.extend_from_slice(b"\r\n");
        }
        if next() % 64 == 0 {
            // Occasional cursor move + erase, like a progress redraw.
            out.extend_from_slice(b"\x1b[1;1H\x1b[K");
        }
    }
    out
}

fn main() {
    let target = 32 * 1024 * 1024; // 32 MB
    let payload = build_payload(target);
    let mb = payload.len() as f64 / 1e6;

    // A couple of grid sizes to show scaling.
    for &(cols, rows) in &[(80usize, 24usize), (200, 50)] {
        let mut term = Terminal::new(cols, rows, 5000);
        // Warm up.
        term.feed(&payload[..payload.len() / 16]);

        let mut term = Terminal::new(cols, rows, 5000);
        let start = Instant::now();
        // Feed in 64KB chunks, as a PTY would deliver it.
        for chunk in payload.chunks(64 * 1024) {
            term.feed(chunk);
        }
        let elapsed = start.elapsed().as_secs_f64();
        let throughput = mb / elapsed;

        // Also measure a render snapshot pass (what a front-end consumes).
        let snap_start = Instant::now();
        let iters = 240;
        let mut total_words = 0usize;
        for _ in 0..iters {
            let s = term.snapshot(true);
            total_words += s.len();
        }
        let snap_ms = snap_start.elapsed().as_secs_f64() * 1000.0 / iters as f64;

        println!(
            "{cols}x{rows}: parse {throughput:7.1} MB/s  ({mb:.0} MB in {ms:.0} ms)   \
             snapshot {snap_ms:.2} ms/frame ({kw} KWords)",
            ms = elapsed * 1000.0,
            kw = total_words / iters / 1000,
        );
    }
}
