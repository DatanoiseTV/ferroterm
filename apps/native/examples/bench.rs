//! Headless native-renderer benchmark. Renders full-screen frames of dense SGR
//! content to an offscreen texture on the real GPU and reports parse throughput,
//! the CPU per-frame cost (snapshot decode + instance build) and the off-vsync
//! GPU submit->idle latency.
//!
//!   cargo run --release --example bench
//!
//! The CPU frame cost is the render thread's real per-frame work; GPU raster of
//! the instances is negligible, and submit->idle is command-submission + sync
//! latency that double-buffering hides in the live vsync'd window.

use std::time::Instant;

use ferroterm_core::Terminal;
use ferroterm_native::atlas::Atlas;
use ferroterm_native::palette::{Palette, Theme};
use ferroterm_native::renderer::{build_instances, Renderer};
use ferroterm_native::snapshot::Grid;

const FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

fn full_screen(cols: usize, rows: usize) -> Vec<u8> {
    // Every cell filled with a colored glyph on a colored background (worst case).
    let mut s = String::from("\x1b[H");
    for y in 0..rows {
        for x in 0..cols {
            let c = (x + y) % 216 + 16;
            s.push_str(&format!(
                "\x1b[48;5;{}m\x1b[38;5;{}m*",
                c,
                (c + 108) % 216 + 16
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

fn bench_grid(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    atlas: &mut Atlas,
    pal: &Palette,
    cols: usize,
    rows: usize,
) {
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

    let mut r = Renderer::new(device, queue, FMT, atlas);
    let (w, h) = (atlas.cell_w * cols as u32, atlas.cell_h * rows as u32);
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("bench-target"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FMT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    r.set_screen(queue, w as f32, h as f32, 0.0, 0.0);

    // Warm up (rasterize the atlas, prime pipelines).
    for _ in 0..5 {
        r.render(
            device,
            queue,
            &view,
            &grid,
            pal,
            atlas,
            true,
            pal.theme.bg,
            None,
            0,
            0,
        );
        device.poll(wgpu::Maintain::Wait);
    }

    let decode_us = best_ms(200, 5, || grid.apply(&snap)) * 1000.0;
    let build_us = best_ms(100, 5, || {
        let _ = build_instances(atlas, &grid, pal, true, None, 0, 0);
    }) * 1000.0;
    let frame_ms = best_ms(60, 5, || {
        r.render(
            device,
            queue,
            &view,
            &grid,
            pal,
            atlas,
            true,
            pal.theme.bg,
            None,
            0,
            0,
        );
        device.poll(wgpu::Maintain::Wait);
    });

    // CPU per-frame cost = snapshot decode + instance build. This is what runs
    // on the render thread every frame; GPU raster of the instances is
    // negligible next to it, and the submit->GPU-idle round trip below is
    // latency (hidden by double-buffering in the live vsync'd window).
    let cpu_ms = (decode_us + build_us) / 1000.0;
    println!(
        "  {:>4}x{:<3} ({:>5} cells)  parse {:>4.0} MB/s   cpu frame {:>6.3} ms (= {:>7.0} fps)   gpu submit->idle {:>5.2} ms",
        cols,
        rows,
        cols * rows,
        mbps,
        cpu_ms,
        1000.0 / cpu_ms,
        frame_ms,
    );
}

fn main() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY | wgpu::Backends::GL,
        ..Default::default()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .expect("no GPU adapter");
    let info = adapter.get_info();
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("bench"),
            required_features: wgpu::Features::empty(),
            required_limits: adapter.limits(), // real GPU limits (large textures)
            memory_hints: wgpu::MemoryHints::Performance,
        },
        None,
    ))
    .expect("device");

    println!(
        "\n  GPU: {} ({:?}, {:?})",
        info.name, info.backend, info.device_type
    );

    let mut atlas = Atlas::new(15.0);
    let pal = Palette::new(Theme::default());
    println!("  cell: {}x{} px\n", atlas.cell_w, atlas.cell_h);

    for (c, r) in [(80, 24), (120, 40), (200, 50), (300, 80)] {
        bench_grid(&device, &queue, &mut atlas, &pal, c, r);
    }
    println!();
}
