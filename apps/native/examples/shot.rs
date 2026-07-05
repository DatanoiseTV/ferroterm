//! Render a curated scene through the native renderer to an offscreen texture
//! and write it out as a PNG — an authentic screenshot of the wgpu renderer
//! without needing a window (or macOS screen-recording permission).
//!
//!   cargo run --release --example shot -- out.png
//!
//! The scene shows what the native renderer draws today (colors, plain text,
//! box-drawing, bold/italic, underline/strikethrough and an inline image); it
//! avoids CJK/emoji, which the bundled monospace lacks.

use ferroterm_core::Terminal;
use ferroterm_native::atlas::Atlas;
use ferroterm_native::images::{ImageLayer, ImageQuad};
use ferroterm_native::palette::{Palette, Theme};
use ferroterm_native::renderer::Renderer;
use ferroterm_native::snapshot::Grid;

const FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Standard base64 (Kitty's wire encoding).
fn b64(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18 & 63) as usize] as char);
        out.push(T[(n >> 12 & 63) as usize] as char);
        out.push(if c.len() > 1 {
            T[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// A Kitty transmit-and-display escape for a `w`x`h` RGBA gradient image.
fn kitty_gradient(w: u32, h: u32) -> String {
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let r = (255 * x / w.max(1)) as u8;
            let g = (255 * y / h.max(1)) as u8;
            px.extend_from_slice(&[r, g, 200, 255]);
        }
    }
    format!("\x1b_Ga=T,f=32,s={w},v={h};{}\x1b\\", b64(&px))
}

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
    // Text styles the native renderer now supports.
    s.push_str(
        "  \x1b[1mbold\x1b[0m  \x1b[3mitalic\x1b[0m  \x1b[1;3mbold-italic\x1b[0m  \x1b[4munderline\x1b[0m  \x1b[9mstrike\x1b[0m\r\n\r\n",
    );
    // An inline image (Kitty RGBA), which the image layer composites over cells.
    s.push_str(&format!(
        "{p}:\x1b[38;5;111m~\x1b[0m$ icat gradient.png\r\n"
    ));
    s.push_str(&kitty_gradient(120, 48));
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
    let (cols, rows) = (66usize, 16usize);

    let mut term = Terminal::new(cols, rows, 100);
    // Tell the core the cell size so the inline image lays out in whole cells.
    term.set_cell_pixels(atlas.cell_w as usize, atlas.cell_h as usize);
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

    // Inline images: draw the scene's RGBA placements over the cells.
    let mut images = ImageLayer::new(&device, FMT);
    images.set_screen(w as f32, h as f32, margin as f32, margin as f32);
    let cw = atlas.cell_w as f32;
    let ch = atlas.cell_h as f32;
    let placements = term.image_placements();
    let mut owned: Vec<(u32, u32, u32, f32, f32, Vec<u8>)> = Vec::new();
    for p in placements.chunks(5) {
        let (id, row, col, iw, ih) = (p[0] as u32, p[1], p[2], p[3] as u32, p[4] as u32);
        let rgba = term.image_rgba(id);
        if rgba.is_empty() {
            continue;
        }
        let x = margin as f32 + col as f32 * cw;
        let y = margin as f32 + row as f32 * ch;
        owned.push((id, iw, ih, x, y, rgba));
    }
    let quads: Vec<ImageQuad> = owned
        .iter()
        .map(|(id, iw, ih, x, y, rgba)| ImageQuad {
            id: *id,
            src_w: *iw,
            src_h: *ih,
            x: *x,
            y: *y,
            w: *iw as f32,
            h: *ih as f32,
            rgba,
        })
        .collect();
    images.render(&device, &queue, &view, &quads);

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

    let img = image::RgbaImage::from_raw(w, h, rgba).expect("rgba buffer");
    img.save_with_format(&out, image::ImageFormat::Png)
        .expect("write png");
    println!("wrote {out} ({w}x{h})");
}
