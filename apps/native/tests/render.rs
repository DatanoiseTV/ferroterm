//! Headless render test: drive the wgpu renderer against an offscreen texture
//! (no window) and pixel-sample the result, the same way the web component's
//! render test asserts semantic colors. Requires a working GPU/adapter (Metal /
//! Vulkan / GL); it is skipped with a clear message if none is available.

use ferroterm_core::Terminal;
use ferroterm_native::atlas::Atlas;
use ferroterm_native::palette::{Palette, Theme};
use ferroterm_native::renderer::Renderer;
use ferroterm_native::snapshot::Grid;

const FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

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
