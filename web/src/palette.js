// Color resolution: turns the packed u32 colors from the WASM snapshot into
// concrete RGB, honoring the theme, the 256-color xterm palette, bold-as-bright
// and inverse video.

// Packed-color kinds (must match `Color::pack` in the Rust core).
export const COLOR_DEFAULT = 0x00;
export const COLOR_INDEXED = 0x01;
export const COLOR_RGB = 0x02;

// Attribute flag bits (must match `cell::attr` in the Rust core).
export const ATTR = {
  BOLD: 1 << 0,
  DIM: 1 << 1,
  ITALIC: 1 << 2,
  UNDERLINE: 1 << 3,
  BLINK: 1 << 4,
  INVERSE: 1 << 5,
  INVISIBLE: 1 << 6,
  STRIKETHROUGH: 1 << 7,
  WIDE: 1 << 8,
  WIDE_SPACER: 1 << 9,
};

/** The default dark theme (16 ANSI colors + fg/bg/cursor). */
export const DEFAULT_THEME = {
  foreground: '#e6e6e6',
  background: '#1a1b26',
  cursor: '#e6e6e6',
  cursorAccent: '#1a1b26',
  selection: 'rgba(122, 162, 247, 0.35)',
  // 0-7 normal, 8-15 bright.
  ansi: [
    '#15161e', '#f7768e', '#9ece6a', '#e0af68',
    '#7aa2f7', '#bb9af7', '#7dcfff', '#a9b1d6',
    '#414868', '#f7768e', '#9ece6a', '#e0af68',
    '#7aa2f7', '#bb9af7', '#7dcfff', '#c0caf5',
  ],
};

function hexToRgb(hex) {
  if (hex[0] === '#') hex = hex.slice(1);
  if (hex.length === 3) {
    hex = hex[0] + hex[0] + hex[1] + hex[1] + hex[2] + hex[2];
  }
  const n = parseInt(hex, 16);
  return [(n >> 16) & 0xff, (n >> 8) & 0xff, n & 0xff];
}

/** Build the 256-entry xterm palette as `[r,g,b]` triples. */
function buildXterm256(ansi16) {
  const table = new Array(256);
  for (let i = 0; i < 16; i++) table[i] = hexToRgb(ansi16[i]);
  // 6x6x6 color cube (16..231).
  const steps = [0, 95, 135, 175, 215, 255];
  let idx = 16;
  for (let r = 0; r < 6; r++)
    for (let g = 0; g < 6; g++)
      for (let b = 0; b < 6; b++)
        table[idx++] = [steps[r], steps[g], steps[b]];
  // Grayscale ramp (232..255).
  for (let i = 0; i < 24; i++) {
    const v = 8 + i * 10;
    table[idx++] = [v, v, v];
  }
  return table;
}

/**
 * Resolves snapshot colors to CSS strings. Cheap and allocation-light: it
 * caches formatted strings for the palette entries.
 */
export class Palette {
  constructor(theme = DEFAULT_THEME, brightenBold = true) {
    this.setTheme(theme);
    this.brightenBold = brightenBold;
  }

  setTheme(theme) {
    this.theme = { ...DEFAULT_THEME, ...theme };
    this.table = buildXterm256(this.theme.ansi);
    this.css = this.table.map((c) => `rgb(${c[0]},${c[1]},${c[2]})`);
    this.fg = this.theme.foreground;
    this.bg = this.theme.background;
    this.cursor = this.theme.cursor;
    this.cursorAccent = this.theme.cursorAccent;
    this.selection = this.theme.selection;
    this.fgRgb = hexToRgb(this.theme.foreground);
    this.bgRgb = hexToRgb(this.theme.background);
  }

  /**
   * Resolve a packed color to a CSS string.
   * @param packed u32 from the snapshot
   * @param isDefaultFg whether the *default* here means foreground vs background
   * @param bold apply bold-as-bright for indexed 0-7
   */
  resolveCss(packed, isDefaultFg, bold) {
    const kind = (packed >>> 24) & 0xff;
    if (kind === COLOR_DEFAULT) {
      return isDefaultFg ? this.fg : this.bg;
    }
    if (kind === COLOR_INDEXED) {
      let i = packed & 0xff;
      if (bold && this.brightenBold && i < 8) i += 8;
      return this.css[i];
    }
    // RGB
    const r = (packed >> 16) & 0xff;
    const g = (packed >> 8) & 0xff;
    const b = packed & 0xff;
    return `rgb(${r},${g},${b})`;
  }

  /** Resolve to an `[r,g,b]` triple (used by the WebGL renderer). */
  resolveRgb(packed, isDefaultFg, bold) {
    const kind = (packed >>> 24) & 0xff;
    if (kind === COLOR_DEFAULT) {
      return isDefaultFg ? this.fgRgb : this.bgRgb;
    }
    if (kind === COLOR_INDEXED) {
      let i = packed & 0xff;
      if (bold && this.brightenBold && i < 8) i += 8;
      return this.table[i];
    }
    return [(packed >> 16) & 0xff, (packed >> 8) & 0xff, packed & 0xff];
  }
}
