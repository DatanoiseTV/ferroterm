//! Render a curated scene through the native renderer to an offscreen texture
//! and write it out as a PNG — an authentic screenshot of the wgpu renderer
//! without needing a window (or macOS screen-recording permission).
//!
//!   cargo run --release --example shot -- out.png
//!
//! The scene is limited to what the native renderer draws today (colors, plain
//! text, box-drawing, underline/strikethrough); it avoids CJK/emoji, which the
//! bundled monospace lacks.

use std::io::BufWriter;

use ferroterm_core::Terminal;
use ferroterm_native::atlas::Atlas;
use ferroterm_native::palette::{Palette, Theme};
use ferroterm_native::renderer::Renderer;
use ferroterm_native::snapshot::Grid;

const FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

fn scene(cols: usize) -> String {
    let mut s = String::from("\x1b[H");
    let p = "\x1b[38;5;114mferro\x1b[38;5;111mterm\x1b[0m";
    s.push_str(&format!(
        "{p}:\x1b[38;5;111m~\x1b[0m$ native terminal \x1b[38;5;245m// Rust + wgpu (Metal)\x1b[0m\r\n\r\n"
    ));
    // A colored ls.
    s.push_str("\x1b[38;5;111msrc/\x1b[0m  \x1b[38;5;111mpkg/\x1b[0m  \x1b[38;5;150mREADME.md\x1b[0m  \x1b[38;5;150mCargo.toml\x1b[0m  \x1b[38;5;114mbuild.sh\x1b[0m  \x1b[38;5;180mLICENSE\x1b[0m\r\n\r\n");
    // 256-color strip.
    let n = (cols - 2).min(216);
    for i in 0..n {
        s.push_str(&format!("\x1b[48;5;{}m \x1b[0m", 16 + i));
    }
    s.push_str("\r\n");
    // Truecolor gradient.
    for x in 0..(cols - 2) {
        let t = x as f32 / (cols - 2) as f32;
        let r = (128.0 + 127.0 * (std::f32::consts::TAU * t).sin()) as u8;
        let g = (128.0 + 127.0 * (std::f32::consts::TAU * (t + 0.33)).sin()) as u8;
        let b = (128.0 + 127.0 * (std::f32::consts::TAU * (t + 0.66)).sin()) as u8;
        s.push_str(&format!("\x1b[48;2;{r};{g};{b}m \x1b[0m"));
    }
    s.push_str("\r\n\r\n");
    // Box-drawing + basic ANSI colors.
    s.push_str("  \x1b[38;5;111m\u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}\x1b[0m  ");
    for i in 1..8 {
        s.push_str(&format!("\x1b[3{i}m\u{2588}\u{2588}\x1b[0m"));
    }
    s.push_str(
        "\r\n  \x1b[38;5;111m\u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f}\x1b[0m\r\n\r\n",
    );
    s.push_str(&format!("{p}:\x1b[38;5;111m~\x1b[0m$ "));
    s
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "shot.png".into());

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
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("shot"),
            required_features: wgpu::Features::empty(),
            required_limits: adapter.limits(),
            memory_hints: wgpu::MemoryHints::Performance,
        },
        None,
    ))
    .expect("device");

    let mut atlas = Atlas::new(28.0); // large for a crisp screenshot
    let pal = Palette::new(Theme::default());
    let (cols, rows) = (66usize, 13usize);

    let mut term = Terminal::new(cols, rows, 100);
    term.feed(scene(cols).as_bytes());
    let mut grid = Grid::default();
    let mut snap = Vec::new();
    term.snapshot_into(true, &mut snap);
    grid.apply(&snap);

    let margin = 28u32;
    let gw = atlas.cell_w * cols as u32;
    let gh = atlas.cell_h * rows as u32;
    let (w, h) = (gw + margin * 2, gh + margin * 2);

    let mut r = Renderer::new(&device, &queue, FMT, &atlas);
    r.set_screen(&queue, w as f32, h as f32, margin as f32, margin as f32);
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("shot-target"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FMT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    r.render(
        &device,
        &queue,
        &view,
        &grid,
        &pal,
        &mut atlas,
        true,
        pal.theme.bg,
    );

    // Read back and repack to tight RGBA rows.
    let padded = (w * 4).div_ceil(256) * 256;
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (padded * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture: &tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &buf,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(enc.finish()));
    let slice = buf.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::Maintain::Wait);
    let data = slice.get_mapped_range();
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        let src = (y * padded) as usize;
        let dst = (y * w * 4) as usize;
        rgba[dst..dst + (w * 4) as usize].copy_from_slice(&data[src..src + (w * 4) as usize]);
    }
    drop(data);
    buf.unmap();

    let file = std::fs::File::create(&out).expect("create png");
    let mut enc = png::Encoder::new(BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .expect("png header")
        .write_image_data(&rgba)
        .expect("png data");
    println!("wrote {out} ({w}x{h})");
}
