//! A glyph atlas rasterized with `fontdue`. Each glyph is drawn into a
//! cell-sized RGBA slot (coverage in the alpha channel; RGB left white so the
//! shader tints it with the cell's foreground), exactly like the web
//! component's Canvas2D atlas — so one instance per cell composites the glyph
//! over its background in a single pass.
//!
//! Bold / italic / bold-italic use real font faces when the system font
//! provides them (e.g. Menlo.ttc carries all four); a missing face degrades to
//! a synthetic transform of the regular face — a shear for italic, a one-pixel
//! horizontal dilation for bold — so styled text still reads distinctly.
//!
//! Monochrome only for now: color-emoji rasterization (COLR/sbix) needs a
//! richer text stack (swash/cosmic-text) and is a follow-up.

use std::collections::HashMap;

use fontdue::{Font, FontSettings};

const ATLAS: usize = 1024;

/// Style index: bit 0 = bold, bit 1 = italic. Values 0..=3.
pub const STYLE_REGULAR: u8 = 0;
pub const STYLE_BOLD: u8 = 1;
pub const STYLE_ITALIC: u8 = 2;

/// Synthetic-italic shear (x-shift per pixel above the baseline).
const SHEAR: f32 = 0.2;

pub struct Glyph {
    pub u0: f32,
    pub v0: f32,
    pub u1: f32,
    pub v1: f32,
    pub wide: bool,
}

/// One resolved face: the fontdue font to rasterize from, plus whether to apply
/// a synthetic italic shear and/or synthetic bold dilation on top of it.
struct Face {
    font: Font,
    shear: bool,
    embolden: bool,
}

pub struct Atlas {
    /// Faces indexed by style (0=regular, 1=bold, 2=italic, 3=bold-italic).
    faces: [Face; 4],
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
    /// Build an atlas at `px` pixels, discovering a system monospace font and
    /// its bold / italic faces (real where available, synthetic otherwise).
    pub fn new(px: f32) -> Self {
        let faces = load_faces().expect("no monospace font found");

        // Metrics come from the regular face; monospace bold/italic share them.
        let font = &faces[0].font;
        let lm = font
            .horizontal_line_metrics(px)
            .expect("font has no horizontal metrics");
        let advance = font.metrics('M', px).advance_width;
        let cell_w = advance.ceil().max(1.0) as u32;
        let cell_h = (lm.ascent - lm.descent + lm.line_gap).ceil().max(1.0) as u32;
        let baseline = lm.ascent.round() as i32;

        Atlas {
            faces,
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

    /// Font baseline in pixels from the cell top (for underline placement).
    pub fn baseline(&self) -> i32 {
        self.baseline
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Get (rasterizing on first use) the glyph for a scalar codepoint in the
    /// given `style` (0..=3; see the `STYLE_*` constants).
    pub fn glyph(&mut self, cp: u32, wide: bool, style: u8) -> &Glyph {
        let style = (style & 3) as u64;
        let key = (cp as u64) | ((wide as u64) << 32) | (style << 34);
        if !self.cache.contains_key(&key) {
            let g = self.rasterize(cp, wide, style as usize);
            self.cache.insert(key, g);
        }
        self.cache.get(&key).unwrap()
    }

    fn rasterize(&mut self, cp: u32, wide: bool, style: usize) -> Glyph {
        let slot_w = self.cell_w as usize * if wide { 2 } else { 1 };
        let cell_h = self.cell_h as usize;
        let (sx, sy) = self.alloc(slot_w);

        if let Some(ch) = char::from_u32(cp) {
            let face = &self.faces[style];
            let (shear, embolden) = (face.shear, face.embolden);
            let (m, bitmap) = face.font.rasterize(ch, self.px);
            if m.width > 0 && m.height > 0 {
                let gx = m.xmin.max(0) as usize;
                let gy = (self.baseline - m.ymin - m.height as i32).max(0) as usize;
                for r in 0..m.height {
                    let ay = sy + gy + r;
                    if ay >= ATLAS {
                        break;
                    }
                    // Synthetic italic: shift each row right in proportion to its
                    // height above the glyph's bottom.
                    let slant = if shear {
                        (SHEAR * (m.height - 1 - r) as f32).round() as usize
                    } else {
                        0
                    };
                    for c in 0..m.width {
                        let cov = bitmap[r * m.width + c];
                        if cov == 0 {
                            continue;
                        }
                        // Synthetic bold: also stamp one pixel to the right,
                        // taking the max coverage (a light dilation).
                        let reach = if embolden { 1 } else { 0 };
                        for dx in 0..=reach {
                            let col = gx + c + slant + dx;
                            if col >= slot_w {
                                continue;
                            }
                            let ax = sx + col;
                            if ax >= ATLAS {
                                continue;
                            }
                            let o = (ay * ATLAS + ax) * 4;
                            if cov > self.pixels[o + 3] {
                                self.pixels[o] = 255;
                                self.pixels[o + 1] = 255;
                                self.pixels[o + 2] = 255;
                                self.pixels[o + 3] = cov;
                            }
                        }
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

/// Discover the regular monospace face plus bold / italic / bold-italic,
/// filling any gap with a synthetic transform of the regular face.
fn load_faces() -> Option<[Face; 4]> {
    // One byte-blob per style, with the collection index to parse from it.
    let mut found: [Option<(Vec<u8>, u32)>; 4] = [None, None, None, None];

    // Font collections that ship every style in one file.
    const COLLECTIONS: &[&str] = &["/System/Library/Fonts/Menlo.ttc"];
    for path in COLLECTIONS {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let n = ttf_parser::fonts_in_collection(&bytes).unwrap_or(1);
        for i in 0..n {
            let Ok(face) = ttf_parser::Face::parse(&bytes, i) else {
                continue;
            };
            let style = (face.is_bold() as usize) | ((face.is_italic() as usize) << 1);
            if found[style].is_none() {
                found[style] = Some((bytes.clone(), i));
            }
        }
        if found[0].is_some() {
            break;
        }
    }

    // Single-face fallbacks (regular only) if no collection was found.
    if found[0].is_none() {
        const REGULAR: &[&str] = &[
            "/System/Library/Fonts/SFNSMono.ttf",
            "/System/Library/Fonts/Monaco.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
            "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
            "/usr/share/fonts/dejavu/DejaVuSansMono.ttf",
            "C:\\Windows\\Fonts\\consola.ttf",
            "C:\\Windows\\Fonts\\cour.ttf",
        ];
        for path in REGULAR {
            if let Ok(bytes) = std::fs::read(path) {
                found[0] = Some((bytes, 0));
                break;
            }
        }
    }
    // Style-specific sibling files (best-effort; common on Linux).
    const SIBLINGS: &[(usize, &str)] = &[
        (
            1,
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf",
        ),
        (
            2,
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Oblique.ttf",
        ),
        (
            3,
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-BoldOblique.ttf",
        ),
        (1, "C:\\Windows\\Fonts\\consolab.ttf"),
        (2, "C:\\Windows\\Fonts\\consolai.ttf"),
        (3, "C:\\Windows\\Fonts\\consolaz.ttf"),
    ];
    for (style, path) in SIBLINGS {
        if found[*style].is_none() {
            if let Ok(bytes) = std::fs::read(path) {
                found[*style] = Some((bytes, 0));
            }
        }
    }

    found[0].as_ref()?; // a regular face is mandatory

    // Build each style: real face if present, else synthesize from regular.
    let build = |style: usize| -> Face {
        if let Some((bytes, idx)) = &found[style] {
            let font = Font::from_bytes(
                bytes.as_slice(),
                FontSettings {
                    collection_index: *idx,
                    ..Default::default()
                },
            )
            .expect("font parse");
            Face {
                font,
                shear: false,
                embolden: false,
            }
        } else {
            let (bytes, idx) = found[0].as_ref().unwrap();
            let font = Font::from_bytes(
                bytes.as_slice(),
                FontSettings {
                    collection_index: *idx,
                    ..Default::default()
                },
            )
            .expect("font parse");
            Face {
                font,
                shear: style & 2 != 0,    // italic
                embolden: style & 1 != 0, // bold
            }
        }
    };

    Some([build(0), build(1), build(2), build(3)])
}
