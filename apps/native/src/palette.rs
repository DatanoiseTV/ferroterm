//! Color resolution for the native renderer. Mirrors the web component's
//! palette: the same Tokyo Night default theme for the 16 ANSI slots, the
//! standard xterm cube/greys for 16..=255, and the packed-`u32` cell color
//! encoding produced by `ferroterm-core`'s snapshot.

use ferroterm_core::xterm256;

/// Tokyo Night 16-color ANSI ramp (matches `DEFAULT_THEME.ansi` in the web
/// component), so a shell looks identical native vs. in the browser.
const ANSI16: [(u8, u8, u8); 16] = [
    (0x15, 0x16, 0x1e),
    (0xf7, 0x76, 0x8e),
    (0x9e, 0xce, 0x6a),
    (0xe0, 0xaf, 0x68),
    (0x7a, 0xa2, 0xf7),
    (0xbb, 0x9a, 0xf7),
    (0x7d, 0xcf, 0xff),
    (0xa9, 0xb1, 0xd6),
    (0x41, 0x48, 0x68),
    (0xf7, 0x76, 0x8e),
    (0x9e, 0xce, 0x6a),
    (0xe0, 0xaf, 0x68),
    (0x7a, 0xa2, 0xf7),
    (0xbb, 0x9a, 0xf7),
    (0x7d, 0xcf, 0xff),
    (0xc0, 0xca, 0xf5),
];

#[derive(Clone, Copy)]
pub struct Theme {
    pub fg: (u8, u8, u8),
    pub bg: (u8, u8, u8),
    pub cursor: (u8, u8, u8),
    pub cursor_text: (u8, u8, u8),
    /// Background of selected cells (Tokyo Night selection blue).
    pub selection: (u8, u8, u8),
}

impl Default for Theme {
    fn default() -> Self {
        Theme {
            fg: (0xe6, 0xe6, 0xe6),
            bg: (0x1a, 0x1b, 0x26),
            cursor: (0xe6, 0xe6, 0xe6),
            cursor_text: (0x1a, 0x1b, 0x26),
            selection: (0x28, 0x3b, 0x5c),
        }
    }
}

/// The 256-entry table: ANSI16 for 0..16, xterm cube/greys for the rest.
pub struct Palette {
    pub theme: Theme,
    table: [(u8, u8, u8); 256],
}

impl Palette {
    pub fn new(theme: Theme) -> Self {
        let mut table = [(0u8, 0u8, 0u8); 256];
        for (i, slot) in table.iter_mut().enumerate() {
            *slot = if i < 16 { ANSI16[i] } else { xterm256(i as u8) };
        }
        Palette { theme, table }
    }

    /// Resolve a packed cell color (`Color::pack` encoding) to RGB. `bold`
    /// brightens the low 8 ANSI colors (aixterm convention). `is_fg` chooses
    /// which default the `Default` color maps to.
    pub fn resolve(&self, packed: u32, is_fg: bool, bold: bool) -> (u8, u8, u8) {
        match packed >> 24 {
            0x00 => {
                if is_fg {
                    self.theme.fg
                } else {
                    self.theme.bg
                }
            }
            0x01 => {
                let mut i = (packed & 0xff) as usize;
                if bold && i < 8 {
                    i += 8;
                }
                self.table[i]
            }
            _ => (
                ((packed >> 16) & 0xff) as u8,
                ((packed >> 8) & 0xff) as u8,
                (packed & 0xff) as u8,
            ),
        }
    }
}
