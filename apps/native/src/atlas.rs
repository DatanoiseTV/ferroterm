//! A glyph atlas rasterized with `fontdue`. Each glyph is drawn into a
//! cell-sized RGBA slot (coverage in the alpha channel; RGB left white so the
//! shader tints it with the cell's foreground), exactly like the web
//! component's Canvas2D atlas — so one instance per cell composites the glyph
//! over its background in a single pass.
//!
//! Monochrome only for now: color-emoji rasterization (COLR/sbix) needs a
//! richer text stack (swash/cosmic-text) and is a follow-up.

use std::collections::HashMap;

use fontdue::{Font, FontSettings};

const ATLAS: usize = 1024;

pub struct Glyph {
    pub u0: f32,
    pub v0: f32,
    pub u1: f32,
    pub v1: f32,
    pub wide: bool,
}

pub struct Atlas {
    font: Font,
    px: f32,
    pub cell_w: u32,
    pub cell_h: u32,
    baseline: i32,
    pixels: Vec<u8>, // ATLAS*ATLAS RGBA
    cache: HashMap<u64, Glyph>,
    shelf_x: usize,
    shelf_y: usize,
    pub dirty: bool,
}

impl Atlas {
    /// Build an atlas at `px` pixels, discovering a system monospace font.
    pub fn new(px: f32) -> Self {
        let data = load_mono_font().expect("no monospace font found");
        let font = Font::from_bytes(data, FontSettings::default()).expect("font parse");

        let lm = font
            .horizontal_line_metrics(px)
            .expect("font has no horizontal metrics");
        // Cell advance from a representative monospace glyph.
        let advance = font.metrics('M', px).advance_width;
        let cell_w = advance.ceil().max(1.0) as u32;
        let cell_h = (lm.ascent - lm.descent + lm.line_gap).ceil().max(1.0) as u32;
        let baseline = lm.ascent.round() as i32;

        Atlas {
            font,
            px,
            cell_w,
            cell_h,
            baseline,
            pixels: vec![0u8; ATLAS * ATLAS * 4],
            cache: HashMap::new(),
            shelf_x: 0,
            shelf_y: 0,
            dirty: true,
        }
    }

    pub fn atlas_size(&self) -> u32 {
        ATLAS as u32
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Get (rasterizing on first use) the glyph for a scalar codepoint.
    pub fn glyph(&mut self, cp: u32, wide: bool) -> &Glyph {
        let key = (cp as u64) | ((wide as u64) << 32);
        if !self.cache.contains_key(&key) {
            let g = self.rasterize(cp, wide);
            self.cache.insert(key, g);
        }
        self.cache.get(&key).unwrap()
    }

    fn rasterize(&mut self, cp: u32, wide: bool) -> Glyph {
        let slot_w = self.cell_w as usize * if wide { 2 } else { 1 };
        let cell_h = self.cell_h as usize;
        let (sx, sy) = self.alloc(slot_w);

        if let Some(ch) = char::from_u32(cp) {
            let (m, bitmap) = self.font.rasterize(ch, self.px);
            if m.width > 0 && m.height > 0 {
                // Top-left of the bitmap within the slot: left bearing, and the
                // baseline minus the glyph's height above it.
                let gx = m.xmin.max(0) as usize;
                let gy = (self.baseline - m.ymin - m.height as i32).max(0) as usize;
                for r in 0..m.height {
                    let ay = sy + gy + r;
                    if ay >= ATLAS {
                        break;
                    }
                    for c in 0..m.width {
                        let ax = sx + gx + c;
                        if ax >= ATLAS || (gx + c) >= slot_w {
                            continue;
                        }
                        let cov = bitmap[r * m.width + c];
                        let o = (ay * ATLAS + ax) * 4;
                        self.pixels[o] = 255;
                        self.pixels[o + 1] = 255;
                        self.pixels[o + 2] = 255;
                        self.pixels[o + 3] = cov;
                    }
                }
                self.dirty = true;
            }
        }

        let s = ATLAS as f32;
        Glyph {
            u0: sx as f32 / s,
            v0: sy as f32 / s,
            u1: (sx + slot_w) as f32 / s,
            v1: (sy + cell_h) as f32 / s,
            wide,
        }
    }

    /// Shelf allocator: rows of `cell_h`, wrapping to a new shelf on overflow.
    fn alloc(&mut self, w: usize) -> (usize, usize) {
        let ch = self.cell_h as usize;
        if self.shelf_x + w > ATLAS {
            self.shelf_x = 0;
            self.shelf_y += ch;
        }
        if self.shelf_y + ch > ATLAS {
            // Atlas full: reset (rare; a proper LRU is a follow-up).
            self.pixels.iter_mut().for_each(|b| *b = 0);
            self.cache.clear();
            self.shelf_x = 0;
            self.shelf_y = 0;
            self.dirty = true;
        }
        let pos = (self.shelf_x, self.shelf_y);
        self.shelf_x += w;
        pos
    }
}

/// Load the first available system monospace font as raw bytes.
fn load_mono_font() -> Option<Vec<u8>> {
    const CANDIDATES: &[&str] = &[
        // macOS
        "/System/Library/Fonts/Menlo.ttc",
        "/System/Library/Fonts/SFNSMono.ttf",
        "/System/Library/Fonts/Monaco.ttf",
        // Linux
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
        "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
        "/usr/share/fonts/dejavu/DejaVuSansMono.ttf",
        // Windows
        "C:\\Windows\\Fonts\\consola.ttf",
        "C:\\Windows\\Fonts\\cour.ttf",
    ];
    for path in CANDIDATES {
        if let Ok(bytes) = std::fs::read(path) {
            return Some(bytes);
        }
    }
    None
}
