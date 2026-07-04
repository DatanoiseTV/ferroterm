//! Cell contents and text attributes.

/// A terminal color. Kept deliberately small (`Copy`) so cells stay cheap to move.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[derive(Default)]
pub enum Color {
    /// The terminal's default foreground/background (theme-defined).
    #[default]
    Default,
    /// A palette index into the 256-color table (0..=255).
    Indexed(u8),
    /// A 24-bit true color.
    Rgb(u8, u8, u8),
}


impl Color {
    /// Pack into a `u32` for the render snapshot.
    ///
    /// Encoding (top byte = kind):
    /// - `0x00_000000` default
    /// - `0x01_0000II` indexed, `II` = palette index
    /// - `0x02_RRGGBB` true color
    pub fn pack(self) -> u32 {
        match self {
            Color::Default => 0x0000_0000,
            Color::Indexed(i) => 0x0100_0000 | i as u32,
            Color::Rgb(r, g, b) => {
                0x0200_0000 | ((r as u32) << 16) | ((g as u32) << 8) | b as u32
            }
        }
    }
}

/// Text attribute flags. Stored as a bitset on every [`Cell`].
pub mod attr {
    pub const BOLD: u16 = 1 << 0;
    pub const DIM: u16 = 1 << 1;
    pub const ITALIC: u16 = 1 << 2;
    pub const UNDERLINE: u16 = 1 << 3;
    pub const BLINK: u16 = 1 << 4;
    pub const INVERSE: u16 = 1 << 5;
    pub const INVISIBLE: u16 = 1 << 6;
    pub const STRIKETHROUGH: u16 = 1 << 7;
    /// Left cell of a double-width glyph (e.g. CJK, wide emoji).
    pub const WIDE: u16 = 1 << 8;
    /// Right-hand spacer that trails a [`WIDE`] cell; carries no glyph.
    pub const WIDE_SPACER: u16 = 1 << 9;
}

/// The visual style shared by printed cells: colors plus attribute flags.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Pen {
    pub fg: Color,
    pub bg: Color,
    pub flags: u16,
}

impl Default for Pen {
    fn default() -> Self {
        Pen {
            fg: Color::Default,
            bg: Color::Default,
            flags: 0,
        }
    }
}

impl Pen {
    #[inline]
    pub fn set(&mut self, flag: u16) {
        self.flags |= flag;
    }
    #[inline]
    pub fn clear(&mut self, flag: u16) {
        self.flags &= !flag;
    }
    #[inline]
    pub fn has(&self, flag: u16) -> bool {
        self.flags & flag != 0
    }
}

/// One character cell in the grid.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    /// The primary displayed character. `' '` for a blank cell. Full Unicode
    /// scalar value, so astral-plane code points (emoji, CJK ext) are one cell.
    /// For a multi-scalar grapheme cluster (base + combining marks, a ZWJ emoji
    /// sequence, a flag), this is the first scalar and [`grapheme`] holds the
    /// full cluster.
    pub ch: char,
    pub pen: Pen,
    /// OSC 8 hyperlink id (0 = none). Resolves to a URI in the terminal's link
    /// registry.
    pub link: u32,
    /// Grapheme-cluster id (0 = the cell is just `ch`). Non-zero ids resolve to
    /// the full cluster string in the terminal's grapheme table.
    pub grapheme: u32,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            pen: Pen::default(),
            link: 0,
            grapheme: 0,
        }
    }
}

impl Cell {
    /// A blank cell that keeps `pen`'s background — used when erasing so that
    /// e.g. a set background color fills the cleared region (matches xterm).
    pub fn blank(pen: Pen) -> Self {
        // Erased cells keep background but drop glyph-level attributes and fg.
        Cell {
            ch: ' ',
            pen: Pen {
                fg: Color::Default,
                bg: pen.bg,
                flags: pen.flags & attr::INVERSE, // preserve inverse-fill semantics
            },
            link: 0,
            grapheme: 0,
        }
    }

    #[inline]
    pub fn is_wide(&self) -> bool {
        self.pen.has(attr::WIDE)
    }

    #[inline]
    pub fn is_spacer(&self) -> bool {
        self.pen.has(attr::WIDE_SPACER)
    }
}
