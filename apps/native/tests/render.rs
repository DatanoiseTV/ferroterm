//! Headless render test: drive the wgpu renderer against an offscreen texture
//! (no window) and pixel-sample the result, the same way the web component's
//! render test asserts semantic colors. Requires a working GPU/adapter (Metal /
//! Vulkan / GL); it is skipped with a clear message if none is available.

use ferroterm_core::Terminal;
use ferroterm_native::atlas::Atlas;
use ferroterm_native::images::{ImageLayer, ImageQuad};
use ferroterm_native::palette::{Palette, Theme};
use ferroterm_native::renderer::Renderer;
use ferroterm_native::snapshot::Grid;

const FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Standard base64 (with padding), the encoding Kitty uses on the wire.
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

/// Encode RGBA pixels as a PNG file (via the `png` crate).
fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(rgba).unwrap();
    }
    out
}

fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY | wgpu::Backends::GL,
        ..Default::default()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))?;
    pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("test"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::Performance,
        },
        None,
    ))
    .ok()
}

/// Render the grid to an offscreen texture and read it back as tightly-packed
/// RGBA rows (`width * height * 4`).
#[allow(clippy::too_many_arguments)]
fn render_readback(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut Renderer,
    atlas: &mut Atlas,
    pal: &Palette,
    grid: &Grid,
    w: u32,
    h: u32,
) -> Vec<u8> {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("target"),
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
    renderer.set_screen(queue, w as f32, h as f32, 0.0, 0.0);
    renderer.render(device, queue, &view, grid, pal, atlas, true, pal.theme.bg);

    // Copy to a buffer with a 256-aligned row stride, then repack tightly.
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
    let mut out = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        let src = (y * padded) as usize;
        let dst = (y * w * 4) as usize;
        out[dst..dst + (w * 4) as usize].copy_from_slice(&data[src..src + (w * 4) as usize]);
    }
    drop(data);
    buf.unmap();
    out
}

#[test]
fn semantic_colors_render() {
    let Some((device, queue)) = gpu() else {
        eprintln!("SKIP: no GPU adapter available");
        return;
    };

    let mut atlas = Atlas::new(16.0);
    let pal = Palette::new(Theme::default());
    let (cw, ch) = (atlas.cell_w, atlas.cell_h);
    let (cols, rows) = (10usize, 2usize);

    let mut term = Terminal::new(cols, rows, 100);
    // col0 red full-block, col1 default space, col2 blue-bg space,
    // col4 truecolor(255,128,0) bg space, col6 green full-block.
    term.feed(
        "\x1b[31m\u{2588}\x1b[0m \x1b[44m \x1b[0m \x1b[48;2;255;128;0m \x1b[0m \x1b[32m\u{2588}\x1b[0m"
            .as_bytes(),
    );
    let mut grid = Grid::default();
    let mut snap = Vec::new();
    term.snapshot_into(true, &mut snap);
    grid.apply(&snap);

    let mut r = Renderer::new(&device, &queue, FMT, &atlas);
    let (w, h) = (cw * cols as u32, ch * rows as u32);
    let px = render_readback(&device, &queue, &mut r, &mut atlas, &pal, &grid, w, h);

    let sample = |cx: usize, cy: usize| -> [u8; 3] {
        let x = cx as u32 * cw + cw / 2;
        let y = cy as u32 * ch + ch / 2;
        let o = ((y * w + x) * 4) as usize;
        [px[o], px[o + 1], px[o + 2]]
    };
    let near = |got: [u8; 3], exp: (u8, u8, u8)| {
        let d = |a: u8, b: u8| (a as i32 - b as i32).abs();
        d(got[0], exp.0) <= 42 && d(got[1], exp.1) <= 42 && d(got[2], exp.2) <= 42
    };

    type Check = (&'static str, [u8; 3], (u8, u8, u8));
    let checks: &[Check] = &[
        ("red block fg", sample(0, 0), (0xf7, 0x76, 0x8e)),
        ("default bg", sample(1, 0), (0x1a, 0x1b, 0x26)),
        ("blue bg", sample(2, 0), (0x7a, 0xa2, 0xf7)),
        ("truecolor bg", sample(4, 0), (255, 128, 0)),
        ("green block fg", sample(6, 0), (0x9e, 0xce, 0x6a)),
    ];
    let mut failed = 0;
    for (name, got, exp) in checks {
        let ok = near(*got, *exp);
        eprintln!(
            "  {} {}: got {:?} expected ~{:?}",
            if ok { "PASS" } else { "FAIL" },
            name,
            got,
            exp
        );
        if !ok {
            failed += 1;
        }
    }

    // Determinism: a second render is byte-identical.
    let px2 = render_readback(&device, &queue, &mut r, &mut atlas, &pal, &grid, w, h);
    let det_ok = px == px2;
    eprintln!("  {} determinism", if det_ok { "PASS" } else { "FAIL" });
    assert!(det_ok, "second render differed");
    assert_eq!(failed, 0, "{failed} color check(s) failed");
}

#[test]
fn underline_and_strikethrough_draw_lines() {
    let Some((device, queue)) = gpu() else {
        eprintln!("SKIP: no GPU adapter available");
        return;
    };

    let mut atlas = Atlas::new(16.0);
    let pal = Palette::new(Theme::default());
    let (cw, ch) = (atlas.cell_w, atlas.cell_h);
    let baseline = atlas.baseline();
    let (cols, rows) = (4usize, 1usize);

    let mut term = Terminal::new(cols, rows, 100);
    // col0: red underlined space (SGR 4). col1: green strikethrough space (SGR 9).
    // Spaces so the only fg-colored pixels come from the decoration lines.
    term.feed("\x1b[38;2;255;0;0;4m \x1b[0m\x1b[38;2;0;255;0;9m \x1b[0m".as_bytes());
    let mut grid = Grid::default();
    let mut snap = Vec::new();
    term.snapshot_into(true, &mut snap);
    grid.apply(&snap);

    let mut r = Renderer::new(&device, &queue, FMT, &atlas);
    let (w, h) = (cw * cols as u32, ch * rows as u32);
    let px = render_readback(&device, &queue, &mut r, &mut atlas, &pal, &grid, w, h);

    let at = |x: u32, y: u32| -> [u8; 3] {
        let o = ((y * w + x) * 4) as usize;
        [px[o], px[o + 1], px[o + 2]]
    };
    let near = |got: [u8; 3], exp: (u8, u8, u8)| {
        let d = |a: u8, b: u8| (a as i32 - b as i32).abs();
        d(got[0], exp.0) <= 42 && d(got[1], exp.1) <= 42 && d(got[2], exp.2) <= 42
    };

    let underline_y = ((baseline + 2).min(ch as i32 - 1)) as u32;
    let strike_y = (ch as f32 * 0.55).round() as u32;
    let bg = (0x1a, 0x1b, 0x26);

    // col0 underline row is red; the top of col0 is still background.
    let ul = at(cw / 2, underline_y);
    let ul_top = at(cw / 2, 1);
    // col1 strike row is green; the top of col1 is still background.
    let st = at(cw + cw / 2, strike_y);
    let st_top = at(cw + cw / 2, 1);

    type Check = (&'static str, [u8; 3], (u8, u8, u8));
    let checks: &[Check] = &[
        ("underline line = fg red", ul, (255, 0, 0)),
        ("above underline = bg", ul_top, bg),
        ("strike line = fg green", st, (0, 255, 0)),
        ("above strike = bg", st_top, bg),
    ];
    let mut failed = 0;
    for (name, got, exp) in checks {
        let ok = near(*got, *exp);
        eprintln!(
            "  {} {}: got {:?} expected ~{:?}",
            if ok { "PASS" } else { "FAIL" },
            name,
            got,
            exp
        );
        if !ok {
            failed += 1;
        }
    }
    assert_eq!(failed, 0, "{failed} decoration check(s) failed");
}

#[test]
fn bold_and_italic_rasterize_differently() {
    let Some((device, queue)) = gpu() else {
        eprintln!("SKIP: no GPU adapter available");
        return;
    };

    let mut atlas = Atlas::new(24.0); // larger cell → clearer style differences
    let pal = Palette::new(Theme::default());
    let (cw, ch) = (atlas.cell_w, atlas.cell_h);
    let (cols, rows) = (3usize, 1usize);

    let mut term = Terminal::new(cols, rows, 100);
    // The same glyph in three styles: regular, bold (SGR 1), italic (SGR 3).
    term.feed("R\x1b[1mR\x1b[0m\x1b[3mR\x1b[0m".as_bytes());
    let mut grid = Grid::default();
    let mut snap = Vec::new();
    term.snapshot_into(true, &mut snap);
    grid.apply(&snap);

    let mut r = Renderer::new(&device, &queue, FMT, &atlas);
    let (w, h) = (cw * cols as u32, ch * rows as u32);
    let px = render_readback(&device, &queue, &mut r, &mut atlas, &pal, &grid, w, h);

    // Collect one cell's RGB block and count its lit (non-background) pixels.
    let bg = (0x1a, 0x1b, 0x26);
    let cell = |cx: usize| -> (Vec<u8>, usize) {
        let mut bytes = Vec::with_capacity((cw * ch * 3) as usize);
        let mut lit = 0usize;
        for yy in 0..ch {
            for xx in 0..cw {
                let o = (((yy) * w + (cx as u32 * cw + xx)) * 4) as usize;
                let (rr, gg, bb) = (px[o], px[o + 1], px[o + 2]);
                bytes.extend_from_slice(&[rr, gg, bb]);
                let d =
                    (rr as i32 - bg.0).abs() + (gg as i32 - bg.1).abs() + (bb as i32 - bg.2).abs();
                if d > 60 {
                    lit += 1;
                }
            }
        }
        (bytes, lit)
    };
    let (reg, reg_lit) = cell(0);
    let (bold, bold_lit) = cell(1);
    let (ital, ital_lit) = cell(2);

    eprintln!("  lit px — regular {reg_lit}, bold {bold_lit}, italic {ital_lit}");
    assert!(reg_lit > 0, "regular glyph rendered nothing");
    assert_ne!(reg, bold, "bold cell must differ from regular");
    assert_ne!(reg, ital, "italic cell must differ from regular");
    assert_ne!(bold, ital, "bold and italic must differ from each other");
    // Bold is heavier: at least as many lit pixels as regular.
    assert!(
        bold_lit >= reg_lit,
        "bold ({bold_lit}) should not be lighter than regular ({reg_lit})"
    );
}

/// Read an offscreen texture back as tightly-packed RGBA rows.
fn readback(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tex: &wgpu::Texture,
    w: u32,
    h: u32,
) -> Vec<u8> {
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
            texture: tex,
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
    let mut out = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        let src = (y * padded) as usize;
        let dst = (y * w * 4) as usize;
        out[dst..dst + (w * 4) as usize].copy_from_slice(&data[src..src + (w * 4) as usize]);
    }
    drop(data);
    buf.unmap();
    out
}

#[test]
fn inline_rgba_image_draws_over_cells() {
    let Some((device, queue)) = gpu() else {
        eprintln!("SKIP: no GPU adapter available");
        return;
    };

    let mut atlas = Atlas::new(16.0);
    let pal = Palette::new(Theme::default());
    let (cw, ch) = (atlas.cell_w, atlas.cell_h);
    let (cols, rows) = (8usize, 4usize);
    let (w, h) = (cw * cols as u32, ch * rows as u32);

    // A blank grid: cells clear to the theme background.
    let term = Terminal::new(cols, rows, 100);
    let mut grid = Grid::default();
    let mut snap = Vec::new();
    let mut term = term;
    term.snapshot_into(true, &mut snap);
    grid.apply(&snap);

    // Offscreen target that both passes render into.
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("target"),
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

    // Cells first (clears to bg), then the image over them.
    let mut r = Renderer::new(&device, &queue, FMT, &atlas);
    r.set_screen(&queue, w as f32, h as f32, 0.0, 0.0);
    r.render(
        &device,
        &queue,
        &view,
        &grid,
        &pal,
        &mut atlas,
        false,
        pal.theme.bg,
    );

    let mut layer = ImageLayer::new(&device, FMT);
    layer.set_screen(w as f32, h as f32, 0.0, 0.0);
    // A 20x20 opaque magenta image at pixel (12, 8).
    let (iw, ih) = (20u32, 20u32);
    let magenta: Vec<u8> = (0..iw * ih).flat_map(|_| [255u8, 0, 255, 255]).collect();
    let quad = ImageQuad {
        id: 1,
        src_w: iw,
        src_h: ih,
        x: 12.0,
        y: 8.0,
        w: iw as f32,
        h: ih as f32,
        rgba: &magenta,
    };
    layer.render(&device, &queue, &view, &[quad]);

    let px = readback(&device, &queue, &tex, w, h);
    let at = |x: u32, y: u32| -> [u8; 3] {
        let o = ((y * w + x) * 4) as usize;
        [px[o], px[o + 1], px[o + 2]]
    };
    let near = |got: [u8; 3], exp: (u8, u8, u8)| {
        let d = |a: u8, b: u8| (a as i32 - b as i32).abs();
        d(got[0], exp.0) <= 12 && d(got[1], exp.1) <= 12 && d(got[2], exp.2) <= 12
    };

    // Inside the image → magenta. Well outside (bottom-right) → theme bg.
    let inside = at(20, 16);
    let outside = at(w - 4, h - 4);
    eprintln!("  inside {inside:?} (want magenta), outside {outside:?} (want bg)");
    assert!(
        near(inside, (255, 0, 255)),
        "image pixel not magenta: {inside:?}"
    );
    assert!(
        near(outside, (0x1a, 0x1b, 0x26)),
        "outside the image should be background: {outside:?}"
    );
}

#[test]
fn kitty_image_renders_end_to_end() {
    let Some((device, queue)) = gpu() else {
        eprintln!("SKIP: no GPU adapter available");
        return;
    };

    // Feed a real Kitty RGBA image to the terminal, then build image quads the
    // same way the app's render loop does (placements -> rgba -> quads).
    let mut atlas = Atlas::new(16.0);
    let pal = Palette::new(Theme::default());
    let (cw, ch) = (atlas.cell_w, atlas.cell_h);
    let (cols, rows) = (10usize, 6usize);
    let (w, h) = (cw * cols as u32, ch * rows as u32);

    let mut term = Terminal::new(cols, rows, 100);
    // 12x12 solid cyan RGBA, transmit-and-display at the cursor (row 0, col 0).
    let (iw, ih) = (12u32, 12u32);
    let cyan: Vec<u8> = (0..iw * ih).flat_map(|_| [0u8, 255, 255, 255]).collect();
    term.feed(format!("\x1b_Ga=T,f=32,s={iw},v={ih};{}\x1b\\", b64(&cyan)).as_bytes());

    let mut grid = Grid::default();
    let mut snap = Vec::new();
    term.snapshot_into(true, &mut snap);
    grid.apply(&snap);

    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("target"),
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

    let mut r = Renderer::new(&device, &queue, FMT, &atlas);
    r.set_screen(&queue, w as f32, h as f32, 0.0, 0.0);
    r.render(
        &device,
        &queue,
        &view,
        &grid,
        &pal,
        &mut atlas,
        false,
        pal.theme.bg,
    );

    // Mirror main.rs: turn placements into quads.
    let placements = term.image_placements();
    let mut rgbas: Vec<(u32, u32, u32, f32, f32, Vec<u8>)> = Vec::new();
    for p in placements.chunks(5) {
        let (id, row, col, pw, ph) = (p[0] as u32, p[1], p[2], p[3] as u32, p[4] as u32);
        let rgba = term.image_rgba(id);
        if rgba.is_empty() {
            continue;
        }
        rgbas.push((
            id,
            pw,
            ph,
            col as f32 * cw as f32,
            row as f32 * ch as f32,
            rgba,
        ));
    }
    assert_eq!(rgbas.len(), 1, "one raw image should be placed");
    let quads: Vec<ImageQuad> = rgbas
        .iter()
        .map(|(id, pw, ph, x, y, rgba)| ImageQuad {
            id: *id,
            src_w: *pw,
            src_h: *ph,
            x: *x,
            y: *y,
            w: *pw as f32,
            h: *ph as f32,
            rgba,
        })
        .collect();

    let mut layer = ImageLayer::new(&device, FMT);
    layer.set_screen(w as f32, h as f32, 0.0, 0.0);
    layer.render(&device, &queue, &view, &quads);

    let px = readback(&device, &queue, &tex, w, h);
    let at = |x: u32, y: u32| -> [u8; 3] {
        let o = ((y * w + x) * 4) as usize;
        [px[o], px[o + 1], px[o + 2]]
    };
    let inside = at(6, 6); // inside the 12x12 image at (0,0)
    eprintln!("  kitty image inside pixel {inside:?} (want cyan)");
    let d = |a: u8, b: u8| (a as i32 - b as i32).abs();
    assert!(
        d(inside[0], 0) <= 12 && d(inside[1], 255) <= 12 && d(inside[2], 255) <= 12,
        "kitty image did not render cyan: {inside:?}"
    );
}

#[test]
fn decode_png_roundtrips_rgba() {
    // 4x4 solid RGBA encoded then decoded should come back byte-identical.
    let rgba: Vec<u8> = (0..16).flat_map(|_| [200u8, 40, 60, 255]).collect();
    let png = encode_png(4, 4, &rgba);
    let (w, h, out) = ferroterm_native::images::decode_png(&png).expect("decode");
    assert_eq!((w, h), (4, 4));
    assert_eq!(out, rgba, "decoded RGBA must match the source");
}

#[test]
fn kitty_png_image_decodes_and_renders() {
    let Some((device, queue)) = gpu() else {
        eprintln!("SKIP: no GPU adapter available");
        return;
    };

    let mut atlas = Atlas::new(16.0);
    let pal = Palette::new(Theme::default());
    let (cw, ch) = (atlas.cell_w, atlas.cell_h);
    let (cols, rows) = (10usize, 6usize);
    let (w, h) = (cw * cols as u32, ch * rows as u32);

    // A solid-orange PNG transmitted as a Kitty f=100 image (encoded path).
    let (iw, ih) = (24u32, 24u32);
    let orange: Vec<u8> = (0..iw * ih).flat_map(|_| [255u8, 128, 0, 255]).collect();
    let png = encode_png(iw, ih, &orange);
    let mut term = Terminal::new(cols, rows, 100);
    term.feed(format!("\x1b_Ga=T,f=100;{}\x1b\\", b64(&png)).as_bytes());

    // The core stores it as an encoded image (no RGBA); the native app decodes.
    let ids = term.image_ids();
    assert_eq!(ids.len(), 1);
    assert!(
        term.image_rgba(ids[0]).is_empty(),
        "PNG has no core-side RGBA"
    );
    assert_eq!(term.image_mime(ids[0]), "image/png");

    let mut grid = Grid::default();
    let mut snap = Vec::new();
    term.snapshot_into(true, &mut snap);
    grid.apply(&snap);

    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("target"),
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

    let mut r = Renderer::new(&device, &queue, FMT, &atlas);
    r.set_screen(&queue, w as f32, h as f32, 0.0, 0.0);
    r.render(
        &device,
        &queue,
        &view,
        &grid,
        &pal,
        &mut atlas,
        false,
        pal.theme.bg,
    );

    // Mirror main.rs: decode the encoded PNG, scale it into the placement box.
    let placements = term.image_placements();
    let p = &placements[0..5];
    let (id, _row, _col, pw, ph) = (p[0] as u32, p[1], p[2], p[3] as u32, p[4] as u32);
    let (dw, dh, drgba) =
        ferroterm_native::images::decode_png(&term.image_encoded(id)).expect("decode png");
    let quad = ImageQuad {
        id,
        src_w: dw,
        src_h: dh,
        x: 0.0,
        y: 0.0,
        w: pw as f32,
        h: ph as f32,
        rgba: &drgba,
    };

    let mut layer = ImageLayer::new(&device, FMT);
    layer.set_screen(w as f32, h as f32, 0.0, 0.0);
    layer.render(&device, &queue, &view, &[quad]);

    let px = readback(&device, &queue, &tex, w, h);
    let o = ((4 * w + 4) * 4) as usize; // inside the image near the top-left
    let got = [px[o], px[o + 1], px[o + 2]];
    eprintln!("  kitty PNG inside pixel {got:?} (want orange 255,128,0)");
    let d = |a: u8, b: u8| (a as i32 - b as i32).abs();
    assert!(
        d(got[0], 255) <= 16 && d(got[1], 128) <= 16 && d(got[2], 0) <= 16,
        "decoded PNG did not render orange: {got:?}"
    );
}
