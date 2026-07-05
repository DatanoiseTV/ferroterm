//! ferroterm-native: a GPU terminal on `ferroterm-core`, no webview.
//!
//! winit owns the window + input; wgpu (Metal / Vulkan / DX12 / GL) draws the
//! grid; a PTY runs the shell. The terminal engine, key encoding and colors are
//! shared verbatim with the web component via `ferroterm-core`.

mod input;
mod pty;

use std::collections::HashMap;
use std::sync::Arc;

use ferroterm_core::Terminal;
use ferroterm_native::atlas::Atlas;
use ferroterm_native::images::{decode_png, ImageLayer, ImageQuad};
use ferroterm_native::palette::{Palette, Theme};
use ferroterm_native::renderer::Renderer;
use ferroterm_native::snapshot::Grid;
use winit::application::ApplicationHandler;
use winit::event::{MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

/// Events delivered to the loop from the PTY reader thread.
#[derive(Debug)]
pub enum UserEvent {
    PtyData(Vec<u8>),
    PtyExit,
}

struct State {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
    images: ImageLayer,
    atlas: Atlas,
    palette: Palette,
    term: Terminal,
    grid: Grid,
    snap: Vec<u32>,
    pty: pty::Pty,
    mods: ModifiersState,
    cols: usize,
    rows: usize,
    /// Inset (device px) kept clear on every side so the grid doesn't run under
    /// the window's rounded corners / title bar.
    pad: u32,
    /// Grid origin (device px) — the top-left inset where cell (0,0) is drawn.
    /// Kept in sync with the renderer so inline images align with the text.
    origin_x: f32,
    origin_y: f32,
    /// Decoded encoded images (iTerm2 / Kitty PNG) by image id → (w, h, rgba),
    /// so a PNG is decoded once and not on every frame.
    decoded: HashMap<u32, (u32, u32, Vec<u8>)>,
}

/// One image resolved for this frame: id, source (texture) size, on-screen
/// position and draw size, and owned RGBA pixels.
struct ImgFrame {
    id: u32,
    sw: u32,
    sh: u32,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    rgba: Vec<u8>,
}

struct App {
    state: Option<State>,
    proxy: winit::event_loop::EventLoopProxy<UserEvent>,
}

impl State {
    fn new(window: Arc<Window>, proxy: winit::event_loop::EventLoopProxy<UserEvent>) -> State {
        let size = window.inner_size();
        let scale = window.scale_factor() as f32;

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY | wgpu::Backends::GL,
            ..Default::default()
        });
        let surface = instance.create_surface(window.clone()).expect("surface");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .expect("no GPU adapter");
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("ferroterm"),
                required_features: wgpu::Features::empty(),
                required_limits:
                    wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        ))
        .expect("device");

        let caps = surface.get_capabilities(&adapter);
        // A non-sRGB format so shader-written colors round-trip unchanged.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        let atlas = Atlas::new((15.0 * scale).round().max(8.0));
        let palette = Palette::new(Theme::default());
        let renderer = Renderer::new(&device, &queue, format, &atlas);
        let mut images = ImageLayer::new(&device, format);

        let pad = (8.0 * scale).round() as u32;
        let (cols, rows, ox, oy) =
            grid_layout(config.width, config.height, atlas.cell_w, atlas.cell_h, pad);
        renderer.set_screen(
            &queue,
            config.width as f32,
            config.height as f32,
            ox as f32,
            oy as f32,
        );
        images.set_screen(
            config.width as f32,
            config.height as f32,
            ox as f32,
            oy as f32,
        );

        let mut term = Terminal::new(cols, rows, 5000);
        term.set_default_colors(
            rgb(palette.theme.fg),
            rgb(palette.theme.bg),
            rgb(palette.theme.cursor),
        );
        let pty = pty::Pty::spawn(cols as u16, rows as u16, proxy).expect("spawn shell");

        State {
            window,
            surface,
            device,
            queue,
            config,
            renderer,
            images,
            atlas,
            palette,
            term,
            grid: Grid::default(),
            snap: Vec::new(),
            pty,
            mods: ModifiersState::empty(),
            cols,
            rows,
            pad,
            origin_x: ox as f32,
            origin_y: oy as f32,
            decoded: HashMap::new(),
        }
    }

    fn resize(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 {
            return;
        }
        self.config.width = w;
        self.config.height = h;
        self.surface.configure(&self.device, &self.config);

        let (cols, rows, ox, oy) =
            grid_layout(w, h, self.atlas.cell_w, self.atlas.cell_h, self.pad);
        self.renderer
            .set_screen(&self.queue, w as f32, h as f32, ox as f32, oy as f32);
        self.images
            .set_screen(w as f32, h as f32, ox as f32, oy as f32);
        self.origin_x = ox as f32;
        self.origin_y = oy as f32;
        if cols != self.cols || rows != self.rows {
            self.cols = cols;
            self.rows = rows;
            self.term.resize(cols, rows);
            self.pty.resize(cols as u16, rows as u16);
        }
        self.render();
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.term.feed(bytes);
        let reply = self.term.take_output();
        if !reply.is_empty() {
            self.pty.write(&reply);
        }
        self.window.request_redraw();
    }

    fn render(&mut self) {
        self.term.snapshot_into(false, &mut self.snap);
        self.grid.apply(&self.snap);
        let frame = match self.surface.get_current_texture() {
            Ok(f) => f,
            Err(_) => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.renderer.render(
            &self.device,
            &self.queue,
            &view,
            &self.grid,
            &self.palette,
            &mut self.atlas,
            true,
            self.palette.theme.bg,
        );

        // Inline images: draw RGBA placements over the cells. Raw Sixel / Kitty
        // pixels are used directly; encoded PNGs (iTerm2, Kitty f=100) are decoded
        // once and cached.
        let cw = self.atlas.cell_w as f32;
        let ch = self.atlas.cell_h as f32;
        let placements = self.term.image_placements(); // [id, row, col, w, h] * n
        let live: std::collections::HashSet<u32> =
            placements.chunks(5).map(|p| p[0] as u32).collect();
        self.decoded.retain(|id, _| live.contains(id));

        let mut frames: Vec<ImgFrame> = Vec::new();
        for p in placements.chunks(5) {
            let (id, row, col, iw, ih) = (p[0] as u32, p[1], p[2], p[3] as u32, p[4] as u32);
            // Skip images fully above/below the viewport.
            let y = self.origin_y + row as f32 * ch;
            if y + ih as f32 <= 0.0 || y >= self.config.height as f32 {
                continue;
            }
            let x = self.origin_x + col as f32 * cw;
            let rgba = self.term.image_rgba(id);
            if !rgba.is_empty() {
                // Raw image: source size equals the (native) display box.
                frames.push(ImgFrame {
                    id,
                    sw: iw,
                    sh: ih,
                    x,
                    y,
                    w: iw as f32,
                    h: ih as f32,
                    rgba,
                });
                continue;
            }
            // Encoded image: decode PNG (only format we handle natively yet).
            if self.term.image_mime(id) != "image/png" {
                continue;
            }
            if !self.decoded.contains_key(&id) {
                let enc = self.term.image_encoded(id);
                if let Some(dec) = decode_png(&enc) {
                    self.decoded.insert(id, dec);
                }
            }
            if let Some((dw, dh, drgba)) = self.decoded.get(&id) {
                frames.push(ImgFrame {
                    id,
                    sw: *dw,
                    sh: *dh,
                    x,
                    y,
                    w: iw as f32, // scale the decoded PNG into the cell box
                    h: ih as f32,
                    rgba: drgba.clone(),
                });
            }
        }
        let quads: Vec<ImageQuad> = frames
            .iter()
            .map(|f| ImageQuad {
                id: f.id,
                src_w: f.sw,
                src_h: f.sh,
                x: f.x,
                y: f.y,
                w: f.w,
                h: f.h,
                rgba: &f.rgba,
            })
            .collect();
        self.images.render(&self.device, &self.queue, &view, &quads);

        frame.present();
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("ferroterm")
            .with_inner_size(winit::dpi::LogicalSize::new(900.0, 560.0));
        let window = Arc::new(event_loop.create_window(attrs).expect("window"));
        self.state = Some(State::new(window, self.proxy.clone()));
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        match event {
            UserEvent::PtyData(bytes) => state.feed(&bytes),
            UserEvent::PtyExit => event_loop.exit(),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => {
                state.pty.kill();
                event_loop.exit();
            }
            WindowEvent::Resized(size) => state.resize(size.width, size.height),
            WindowEvent::ModifiersChanged(m) => state.mods = m.state(),
            WindowEvent::RedrawRequested => state.render(),
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / state.atlas.cell_h as f32,
                };
                let n = lines.abs().round() as usize;
                if n > 0 {
                    if lines > 0.0 {
                        state.term.scroll_up_view(n);
                    } else {
                        state.term.scroll_down_view(n);
                    }
                    state.window.request_redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed() => {
                let m = input::mods(state.mods);
                let bytes =
                    input::encode(&event.logical_key, m, state.term.modes().app_cursor_keys);
                if !bytes.is_empty() {
                    state.term.scroll_to_bottom();
                    state.pty.write(&bytes);
                }
            }
            _ => {}
        }
    }
}

fn rgb((r, g, b): (u8, u8, u8)) -> u32 {
    ((r as u32) << 16) | ((g as u32) << 8) | b as u32
}

/// Fit as many whole cells as possible inside a `pad`-inset area, then center
/// them so the leftover pixels become balanced margins (and the grid clears the
/// window's rounded corners on every side). Returns `(cols, rows, origin_x,
/// origin_y)` with the origin in device pixels.
fn grid_layout(w: u32, h: u32, cw: u32, ch: u32, pad: u32) -> (usize, usize, u32, u32) {
    let avail_w = w.saturating_sub(2 * pad);
    let avail_h = h.saturating_sub(2 * pad);
    let cols = (avail_w / cw).max(1);
    let rows = (avail_h / ch).max(1);
    let ox = pad + avail_w.saturating_sub(cols * cw) / 2;
    let oy = pad + avail_h.saturating_sub(rows * ch) / 2;
    (cols as usize, rows as usize, ox, oy)
}

fn main() {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .expect("event loop");
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);
    let mut app = App {
        state: None,
        proxy: event_loop.create_proxy(),
    };
    event_loop.run_app(&mut app).expect("run");
}
