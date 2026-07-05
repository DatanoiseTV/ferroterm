//! ferroterm-native: a GPU terminal on `ferroterm-core`, no webview.
//!
//! winit owns the window + input; wgpu (Metal / Vulkan / DX12 / GL) draws the
//! grid; a PTY runs the shell. The terminal engine, key encoding and colors are
//! shared verbatim with the web component via `ferroterm-core`.

mod input;
mod pty;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ferroterm_core::Terminal;
use ferroterm_native::atlas::Atlas;
use ferroterm_native::images::{decode_image, ImageLayer, ImageQuad};
use ferroterm_native::palette::{Palette, Theme};
use ferroterm_native::renderer::Renderer;
use ferroterm_native::selection::{selected_text, word_range, Selection};
use ferroterm_native::snapshot::Grid;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

/// Events delivered to the loop from the PTY reader thread.
#[derive(Debug)]
pub enum UserEvent {
    PtyData(Vec<u8>),
    PtyExit,
}

/// Cursor blink half-period (on for this long, then off for this long).
const BLINK: Duration = Duration::from_millis(530);
/// Max gap between clicks to count as a double/triple click.
const MULTICLICK: Duration = Duration::from_millis(400);

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
    /// Decoded encoded images by image id → `Some((w, h, rgba))`, or `None` if
    /// the bytes couldn't be decoded — cached either way so a given image is
    /// decoded (or rejected) once, not on every frame.
    decoded: HashMap<u32, Option<(u32, u32, Vec<u8>)>>,

    /// Last known cursor position in device pixels (for hit-testing to a cell).
    cursor_px: (f64, f64),
    /// Active text selection (viewport cells), if any.
    selection: Option<Selection>,
    /// Mouse-drag selection anchor cell, set on press while dragging.
    sel_anchor: Option<(usize, usize)>,
    /// Last mouse press (time, cell, click count) for double/triple detection.
    last_click: Option<(Instant, usize, usize, u32)>,
    /// System clipboard, created lazily on first copy/paste.
    clipboard: Option<arboard::Clipboard>,
    /// OSC 8 hyperlink id under the mouse (0 = none), for hover underline.
    hover_link: u32,

    /// Whether the window is focused (cursor is solid, not blinking, unfocused).
    focused: bool,
    /// Current cursor blink phase (true = the block is shown this half-period).
    blink_on: bool,
    /// When the next blink toggle is due.
    next_blink: Instant,
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
            cursor_px: (0.0, 0.0),
            selection: None,
            sel_anchor: None,
            last_click: None,
            clipboard: None,
            hover_link: 0,
            focused: true,
            blink_on: true,
            next_blink: Instant::now() + BLINK,
        }
    }

    /// Update the hovered hyperlink from the cell under the mouse, adjusting the
    /// pointer icon and repainting when it changes.
    fn update_hover(&mut self, px: f64, py: f64) {
        let (x, y) = self.px_to_cell(px, py);
        let link = if x < self.grid.cols && y < self.grid.rows {
            self.grid.cell(x, y).link
        } else {
            0
        };
        if link != self.hover_link {
            self.hover_link = link;
            let icon = if link != 0 {
                winit::window::CursorIcon::Pointer
            } else {
                winit::window::CursorIcon::Text
            };
            self.window.set_cursor(icon);
            self.window.request_redraw();
        }
    }

    /// Open the hovered hyperlink's URI in the OS default handler.
    fn open_hovered_link(&self) {
        if self.hover_link == 0 {
            return;
        }
        let Some(uri) = self.term.link_uri(self.hover_link) else {
            return;
        };
        // Only http(s)/file/mailto — never hand an arbitrary scheme to the shell.
        let ok = ["http://", "https://", "mailto:", "file://"]
            .iter()
            .any(|p| uri.starts_with(p));
        if !ok {
            return;
        }
        let uri = uri.to_string();
        #[cfg(target_os = "macos")]
        let _ = std::process::Command::new("open").arg(&uri).spawn();
        #[cfg(all(unix, not(target_os = "macos")))]
        let _ = std::process::Command::new("xdg-open").arg(&uri).spawn();
        #[cfg(target_os = "windows")]
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", &uri])
            .spawn();
    }

    /// Reset the cursor to solid and restart the blink timer — called on any
    /// activity (keypress, output) so the cursor doesn't blink off mid-action.
    fn poke_cursor(&mut self) {
        self.blink_on = true;
        self.next_blink = Instant::now() + BLINK;
    }

    /// Hit-test a device-pixel position to a viewport cell, clamped to the grid.
    fn px_to_cell(&self, px: f64, py: f64) -> (usize, usize) {
        let cx = ((px - self.origin_x as f64) / self.atlas.cell_w as f64).floor();
        let cy = ((py - self.origin_y as f64) / self.atlas.cell_h as f64).floor();
        let x = (cx.max(0.0) as usize).min(self.cols.saturating_sub(1));
        let y = (cy.max(0.0) as usize).min(self.rows.saturating_sub(1));
        (x, y)
    }

    /// Copy the current selection to the system clipboard.
    fn copy_selection(&mut self) {
        let Some(sel) = self.selection else { return };
        let text = selected_text(&self.grid, &sel);
        if text.is_empty() {
            return;
        }
        if self.clipboard.is_none() {
            self.clipboard = arboard::Clipboard::new().ok();
        }
        if let Some(cb) = self.clipboard.as_mut() {
            let _ = cb.set_text(text);
        }
    }

    /// Select the whole visible viewport (Cmd/Ctrl+A).
    fn select_all(&mut self) {
        let last = (self.cols.saturating_sub(1), self.rows.saturating_sub(1));
        self.selection = Some(Selection::new((0, 0), last));
        self.sel_anchor = None;
    }

    /// Paste clipboard text into the PTY (bracketed if the app enabled it).
    fn paste_clipboard(&mut self) {
        if self.clipboard.is_none() {
            self.clipboard = arboard::Clipboard::new().ok();
        }
        let Some(text) = self.clipboard.as_mut().and_then(|c| c.get_text().ok()) else {
            return;
        };
        if text.is_empty() {
            return;
        }
        self.term.scroll_to_bottom();
        if self.term.modes().bracketed_paste {
            self.pty.write(b"\x1b[200~");
            self.pty.write(text.as_bytes());
            self.pty.write(b"\x1b[201~");
        } else {
            self.pty.write(text.as_bytes());
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
        // Cell geometry changed under the selection; drop it.
        self.selection = None;
        self.sel_anchor = None;
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
        self.poke_cursor();
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
        // The cursor is solid while unfocused, and blinks while focused.
        let cursor_on = !self.focused || self.blink_on;
        self.renderer.render(
            &self.device,
            &self.queue,
            &view,
            &self.grid,
            &self.palette,
            &mut self.atlas,
            cursor_on,
            self.palette.theme.bg,
            self.selection.as_ref(),
            self.hover_link,
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
            // Encoded image (iTerm2 / Kitty f=100): decode it natively. Any
            // format the `image` crate understands works, regardless of the
            // core's MIME hint; a decode failure just leaves it unshown.
            if !self.decoded.contains_key(&id) {
                let enc = self.term.image_encoded(id);
                self.decoded.insert(id, decode_image(&enc));
            }
            if let Some(Some((dw, dh, drgba))) = self.decoded.get(&id) {
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

    /// Drive the cursor blink: when a scheduled toggle time is reached, flip the
    /// blink phase and repaint.
    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: winit::event::StartCause) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        if let winit::event::StartCause::ResumeTimeReached { .. } = cause {
            if state.focused && Instant::now() >= state.next_blink {
                state.blink_on = !state.blink_on;
                state.next_blink = Instant::now() + BLINK;
                state.window.request_redraw();
            }
        }
    }

    /// Idle when unfocused; otherwise wake for the next cursor-blink toggle.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(state) = self.state.as_ref() else {
            return;
        };
        if state.focused {
            event_loop
                .set_control_flow(winit::event_loop::ControlFlow::WaitUntil(state.next_blink));
        } else {
            event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);
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
            WindowEvent::Focused(f) => {
                state.focused = f;
                state.poke_cursor();
                state.window.request_redraw();
            }
            WindowEvent::ModifiersChanged(m) => state.mods = m.state(),
            WindowEvent::RedrawRequested => state.render(),
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / state.atlas.cell_h as f32,
                };
                let n = lines.abs().round() as usize;
                if n > 0 {
                    // Scrolling invalidates the viewport-anchored selection.
                    state.selection = None;
                    state.sel_anchor = None;
                    if lines > 0.0 {
                        state.term.scroll_up_view(n);
                    } else {
                        state.term.scroll_down_view(n);
                    }
                    state.window.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                state.cursor_px = (position.x, position.y);
                if let Some(anchor) = state.sel_anchor {
                    let cell = state.px_to_cell(position.x, position.y);
                    let sel = Selection::new(anchor, cell);
                    state.selection = (!sel.is_empty()).then_some(sel);
                    state.window.request_redraw();
                } else {
                    state.update_hover(position.x, position.y);
                }
            }
            WindowEvent::MouseInput {
                state: btn,
                button: MouseButton::Left,
                ..
            } => {
                // Leave the mouse to the application when it enabled reporting.
                if state.term.modes().mouse_mode != 0 {
                    return;
                }
                match btn {
                    ElementState::Pressed => {
                        // Cmd/Ctrl-click on a hyperlink opens it instead of
                        // starting a selection.
                        if (state.mods.super_key() || state.mods.control_key())
                            && state.hover_link != 0
                        {
                            state.open_hovered_link();
                            return;
                        }
                        let (px, py) = state.cursor_px;
                        let cell = state.px_to_cell(px, py);
                        // Shift-click extends the existing selection from its far
                        // anchor to the clicked cell instead of starting a new one.
                        if state.mods.shift_key() {
                            let anchor = state
                                .selection
                                .map(|s| s.start)
                                .or(state.sel_anchor)
                                .unwrap_or(cell);
                            let sel = Selection::new(anchor, cell);
                            state.selection = (!sel.is_empty()).then_some(sel);
                            state.sel_anchor = Some(anchor);
                            state.last_click = None;
                            state.window.request_redraw();
                            return;
                        }
                        // Count consecutive clicks on the same cell: 1=drag,
                        // 2=word, 3=line (then cycles).
                        let now = Instant::now();
                        let count = match state.last_click {
                            Some((t, cx, cy, n))
                                if cx == cell.0
                                    && cy == cell.1
                                    && now.duration_since(t) < MULTICLICK =>
                            {
                                (n % 3) + 1
                            }
                            _ => 1,
                        };
                        state.last_click = Some((now, cell.0, cell.1, count));
                        state.selection = None;
                        state.sel_anchor = None;
                        match count {
                            1 => state.sel_anchor = Some(cell),
                            2 => {
                                let (lo, hi) = word_range(&state.grid, cell.0, cell.1);
                                state.selection = Some(Selection::new((lo, cell.1), (hi, cell.1)));
                            }
                            _ => {
                                let last = state.cols.saturating_sub(1);
                                state.selection = Some(Selection::new((0, cell.1), (last, cell.1)));
                            }
                        }
                        state.window.request_redraw();
                    }
                    ElementState::Released => state.sel_anchor = None,
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed() => {
                // Copy/paste shortcuts intercepted before PTY encoding: Cmd on
                // macOS, Ctrl+Shift elsewhere (plain Ctrl+C must reach the shell).
                let copy_mod =
                    state.mods.super_key() || (state.mods.control_key() && state.mods.shift_key());
                if copy_mod {
                    if let winit::keyboard::Key::Character(s) = &event.logical_key {
                        if s.eq_ignore_ascii_case("c") {
                            state.copy_selection();
                            return;
                        }
                        if s.eq_ignore_ascii_case("v") {
                            state.paste_clipboard();
                            state.window.request_redraw();
                            return;
                        }
                        if s.eq_ignore_ascii_case("a") {
                            state.select_all();
                            state.window.request_redraw();
                            return;
                        }
                    }
                }
                // Shift+PageUp/PageDown pages the scrollback viewport (a nearly
                // full screen at a time), staying out of the shell's own keys.
                if state.mods.shift_key() {
                    use winit::keyboard::{Key, NamedKey};
                    if let Key::Named(named) = &event.logical_key {
                        let page = state.rows.saturating_sub(1).max(1);
                        let scrolled = match named {
                            NamedKey::PageUp => {
                                state.term.scroll_up_view(page);
                                true
                            }
                            NamedKey::PageDown => {
                                state.term.scroll_down_view(page);
                                true
                            }
                            _ => false,
                        };
                        if scrolled {
                            state.selection = None;
                            state.sel_anchor = None;
                            state.window.request_redraw();
                            return;
                        }
                    }
                }
                let m = input::mods(state.mods);
                let bytes =
                    input::encode(&event.logical_key, m, state.term.modes().app_cursor_keys);
                if !bytes.is_empty() {
                    // Typing clears the selection highlight.
                    if state.selection.take().is_some() {
                        state.window.request_redraw();
                    }
                    state.poke_cursor();
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
