//! A CPU rasterizer that turns a decoded [`Grid`] into an RGBA pixel buffer.
//!
//! Slint has no raw per-pixel canvas element, so instead of one live element
//! per cell (1920 for an 80x24 grid — the wrong shape) the whole terminal is
//! drawn here into a flat `[u8]` RGBA buffer that the app wraps in a
//! `SharedPixelBuffer` and displays as a single `Image`. Deliberately kept free
//! of any Slint type so it can be exercised headlessly in tests.
//!
//! Font handling mirrors the native app's atlas: a system monospace face plus
//! its bold / italic / bold-italic variants (real where the font provides them,
//! e.g. Menlo.ttc; synthetic shear / dilation otherwise). Monochrome coverage
//! only — color-emoji is a follow-up, same as the native renderer.

use std::collections::HashMap;

use fontdue::{Font, FontSettings};

use ferroterm_core::attr;

use crate::palette::Palette;
use crate::snapshot::Grid;

/// Synthetic-italic shear (x-shift per pixel above the glyph baseline).
const SHEAR: f32 = 0.2;

type Rgb = (u8, u8, u8);

/// One resolved face plus whether to apply synthetic italic / bold on top.
struct Face {
    font: Font,
    shear: bool,
    embolden: bool,
}

/// A rasterized glyph: coverage bitmap plus where it sits within the cell.
struct Glyph {
    w: usize,
    h: usize,
    left: i32, // x offset from the cell's left edge
    top: i32,  // y offset from the cell's top edge
    cov: Vec<u8>,
}

pub struct Raster {
    faces: [Face; 4], // indexed by style: 0=regular 1=bold 2=italic 3=bold-italic
    px: f32,
    pub cell_w: usize,
    pub cell_h: usize,
    baseline: i32,
    cache: HashMap<u64, Glyph>,
}

impl Raster {
    /// Build a rasterizer at `px` pixels, discovering a system monospace font.
    /// Returns `None` when no usable font is found (so callers can degrade or a
    /// headless test can skip pixel assertions rather than panic).
    pub fn new(px: f32) -> Option<Raster> {
        let faces = load_faces()?;

        // Metrics come from the regular face; monospace styles share them.
        let font = &faces[0].font;
        let lm = font.horizontal_line_metrics(px)?;
        let advance = font.metrics('M', px).advance_width;
        let cell_w = advance.ceil().max(1.0) as usize;
        let cell_h = (lm.ascent - lm.descent + lm.line_gap).ceil().max(1.0) as usize;
        let baseline = lm.ascent.round() as i32;

        Some(Raster {
            faces,
            px,
            cell_w,
            cell_h,
            baseline,
            cache: HashMap::new(),
        })
    }

    fn glyph(&mut self, cp: u32, style: usize) -> &Glyph {
        let key = (cp as u64) | ((style as u64) << 32);
        if !self.cache.contains_key(&key) {
            let g = self.rasterize(cp, style);
            self.cache.insert(key, g);
        }
        self.cache.get(&key).unwrap()
    }

    fn rasterize(&self, cp: u32, style: usize) -> Glyph {
        let empty = Glyph {
            w: 0,
            h: 0,
            left: 0,
            top: 0,
            cov: Vec::new(),
        };
        let Some(ch) = char::from_u32(cp) else {
            return empty;
        };
        let face = &self.faces[style];
        let (m, bitmap) = face.font.rasterize(ch, self.px);
        if m.width == 0 || m.height == 0 {
            return empty;
        }

        // Synthetic italic shears each row right in proportion to its height
        // above the glyph bottom; synthetic bold dilates one pixel right.
        let extra = usize::from(face.embolden);
        let slant_max = if face.shear {
            (SHEAR * (m.height as f32 - 1.0)).round() as usize
        } else {
            0
        };
        let w = m.width + extra + slant_max;
        let h = m.height;
        let mut cov = vec![0u8; w * h];
        for r in 0..m.height {
            let slant = if face.shear {
                (SHEAR * (m.height - 1 - r) as f32).round() as usize
            } else {
                0
            };
            for c in 0..m.width {
                let v = bitmap[r * m.width + c];
                if v == 0 {
                    continue;
                }
                for dx in 0..=extra {
                    let o = r * w + (c + slant + dx);
                    if v > cov[o] {
                        cov[o] = v;
                    }
                }
            }
        }

        Glyph {
            w,
            h,
            left: m.xmin,
            top: self.baseline - m.ymin - m.height as i32,
            cov,
        }
    }

    /// Draw `grid` into `out` (a `width`x`height` RGBA buffer). `cursor_on` is
    /// the cursor's current blink phase (drawn only when the grid also reports
    /// the cursor visible and on-screen).
    pub fn draw(
        &mut self,
        grid: &Grid,
        palette: &Palette,
        out: &mut [u8],
        width: usize,
        height: usize,
        cursor_on: bool,
    ) {
        let bg = palette.theme.bg;
        fill(out, width, height, 0, 0, width, height, bg);
        if grid.cols == 0 || grid.rows == 0 {
            return;
        }

        let cw = self.cell_w;
        let ch = self.cell_h;
        let baseline = self.baseline as usize;

        for y in 0..grid.rows {
            let oy = y * ch;
            if oy >= height {
                break;
            }
            for x in 0..grid.cols {
                let cell = grid.cell(x, y);
                if cell.flags & attr::WIDE_SPACER != 0 {
                    continue; // covered by the wide glyph to its left
                }
                let ox = x * cw;
                if ox >= width {
                    continue;
                }
                let wide = cell.flags & attr::WIDE != 0;
                let slot_w = cw * if wide { 2 } else { 1 };
                let bold = cell.flags & attr::BOLD != 0;

                let mut fg = palette.resolve(cell.fg, true, bold);
                let mut bg = palette.resolve(cell.bg, false, false);
                if cell.flags & attr::INVERSE != 0 {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if cell.flags & attr::DIM != 0 {
                    fg = dim(fg);
                }

                let is_cursor = cursor_on
                    && grid.cursor_on_screen
                    && grid.cursor_visible
                    && x == grid.cursor_x
                    && y == grid.cursor_y;
                if is_cursor {
                    bg = palette.theme.cursor;
                    fg = palette.theme.cursor_text;
                }

                let cell_w_clamped = slot_w.min(width - ox);
                let cell_h_clamped = ch.min(height - oy);
                fill(
                    out,
                    width,
                    height,
                    ox,
                    oy,
                    cell_w_clamped,
                    cell_h_clamped,
                    bg,
                );

                let printable =
                    cell.flags & attr::INVISIBLE == 0 && cell.cp != 0 && cell.cp != 0x20;
                if printable {
                    let style =
                        usize::from(bold) | (usize::from(cell.flags & attr::ITALIC != 0) << 1);
                    let g = self.glyph(cell.cp, style);
                    blit(
                        out,
                        width,
                        height,
                        ox as i32 + g.left,
                        oy as i32 + g.top,
                        g.w,
                        g.h,
                        &g.cov,
                        fg,
                    );
                }

                if cell.flags & attr::UNDERLINE != 0 {
                    let ly = oy + (baseline + 1).min(ch.saturating_sub(1));
                    fill(out, width, height, ox, ly, cell_w_clamped, 1, fg);
                }
                if cell.flags & attr::STRIKETHROUGH != 0 {
                    let ly = oy + ch / 2;
                    fill(out, width, height, ox, ly, cell_w_clamped, 1, fg);
                }
            }
        }
    }
}

/// Fill an opaque `w`x`h` rectangle at `(x, y)`, clipped to the buffer.
#[allow(clippy::too_many_arguments)]
fn fill(
    out: &mut [u8],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    c: Rgb,
) {
    for yy in y..(y + h).min(height) {
        let row = yy * width;
        for xx in x..(x + w).min(width) {
            let o = (row + xx) * 4;
            out[o] = c.0;
            out[o + 1] = c.1;
            out[o + 2] = c.2;
            out[o + 3] = 255;
        }
    }
}

/// Alpha-composite a coverage bitmap tinted `fg` over the existing buffer.
#[allow(clippy::too_many_arguments)]
fn blit(
    out: &mut [u8],
    width: usize,
    height: usize,
    x: i32,
    y: i32,
    gw: usize,
    gh: usize,
    cov: &[u8],
    fg: Rgb,
) {
    for r in 0..gh {
        let py = y + r as i32;
        if py < 0 || py as usize >= height {
            continue;
        }
        let row = py as usize * width;
        for c in 0..gw {
            let a = cov[r * gw + c] as u32;
            if a == 0 {
                continue;
            }
            let px = x + c as i32;
            if px < 0 || px as usize >= width {
                continue;
            }
            let o = (row + px as usize) * 4;
            let ia = 255 - a;
            out[o] = ((fg.0 as u32 * a + out[o] as u32 * ia) / 255) as u8;
            out[o + 1] = ((fg.1 as u32 * a + out[o + 1] as u32 * ia) / 255) as u8;
            out[o + 2] = ((fg.2 as u32 * a + out[o + 2] as u32 * ia) / 255) as u8;
            out[o + 3] = 255;
        }
    }
}

/// Dim a color to ~60% (SGR 2).
fn dim(c: Rgb) -> Rgb {
    (
        (c.0 as u16 * 3 / 5) as u8,
        (c.1 as u16 * 3 / 5) as u8,
        (c.2 as u16 * 3 / 5) as u8,
    )
}

/// Discover the regular monospace face plus bold / italic / bold-italic,
/// filling any gap with a synthetic transform of the regular face. Mirrors the
/// native app's `atlas::load_faces`.
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
