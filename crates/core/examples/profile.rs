//! Granular pipeline profile: isolates plain-text, SGR, truecolor, scroll and
//! unicode parsing plus snapshot serialization, so we can see which stage is
//! the bottleneck. Run: `cargo run --release -p ferroterm-core --example profile`.

use std::time::Instant;

use ferroterm_core::Terminal;

fn gen(kind: &str, target: usize) -> Vec<u8> {
    let words = [
        "ferroterm", "wasm", "rust", "render", "parser", "vt100", "grid", "scroll", "buffer",
        "atlas",
    ];
    let mut out = Vec::with_capacity(target + 4096);
    let mut seed = 0x9e3779b9u32;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    while out.len() < target {
        match kind {
            "plain" => {
                let mut line = String::new();
                for _ in 0..12 {
                    line.push_str(words[(next() as usize) % words.len()]);
                    line.push(' ');
                }
                out.extend_from_slice(line.as_bytes());
                out.extend_from_slice(b"\r\n");
            }
            "sgr" => {
                for _ in 0..10 {
                    out.extend_from_slice(
                        format!("\x1b[38;5;{}m", next() % 256).as_bytes(),
                    );
                    out.extend_from_slice(words[(next() as usize) % words.len()].as_bytes());
                    out.push(b' ');
                }
                out.extend_from_slice(b"\x1b[0m\r\n");
            }
            "truecolor" => {
                for _ in 0..8 {
                    out.extend_from_slice(
                        format!("\x1b[38;2;{};{};{}m", next() % 256, next() % 256, next() % 256)
                            .as_bytes(),
                    );
                    out.extend_from_slice(words[(next() as usize) % words.len()].as_bytes());
                    out.push(b' ');
                }
                out.extend_from_slice(b"\x1b[0m\r\n");
            }
            "scroll" => {
                let mut line = String::new();
                while line.len() < 80 {
                    line.push_str(words[(next() as usize) % words.len()]);
                    line.push(' ');
                }
                out.extend_from_slice(line[..80].as_bytes());
                out.extend_from_slice(b"\r\n");
            }
            "unicode" => {
                let glyphs = ["世界", "café", "naïve", "🦀", "日本語", "e\u{0301}"];
                for _ in 0..8 {
                    out.extend_from_slice(glyphs[(next() as usize) % glyphs.len()].as_bytes());
                    out.push(b' ');
                }
                out.extend_from_slice(b"\r\n");
            }
            _ => unreachable!(),
        }
    }
    out
}

fn bench_parse(payload: &[u8], cols: usize, rows: usize) -> f64 {
    let mut term = Terminal::new(cols, rows, 5000);
    term.feed(&payload[..payload.len() / 16]); // warm
    let mut term = Terminal::new(cols, rows, 5000);
    let start = Instant::now();
    for chunk in payload.chunks(64 * 1024) {
        term.feed(chunk);
    }
    let s = start.elapsed().as_secs_f64();
    (payload.len() as f64 / 1e6) / s
}

fn bench_snapshot(payload: &[u8], cols: usize, rows: usize) -> (f64, usize) {
    let mut term = Terminal::new(cols, rows, 5000);
    term.feed(payload);
    let iters = 2000;
    // force=true every frame (worst case: whole screen dirty).
    let start = Instant::now();
    let mut words = 0usize;
    for _ in 0..iters {
        let snap = term.snapshot(true);
        words += snap.len();
    }
    let us = start.elapsed().as_secs_f64() * 1e6 / iters as f64;
    (us, words / iters)
}

fn main() {
    let target = 16 * 1024 * 1024;
    let (cols, rows) = (200usize, 50usize);
    println!("grid {cols}x{rows}, {} MB payloads\n", target / 1024 / 1024);
    println!("{:<12} {:>12}", "scenario", "parse MB/s");
    for kind in ["plain", "sgr", "truecolor", "scroll", "unicode"] {
        let p = gen(kind, target);
        let mbps = bench_parse(&p, cols, rows);
        println!("{kind:<12} {mbps:>12.1}");
    }
    println!();
    let p = gen("sgr", target);
    let (us, words) = bench_snapshot(&p, cols, rows);
    println!("snapshot(full): {us:.1} us/frame ({words} words, {:.1} ns/word)", us * 1000.0 / words as f64);
}
