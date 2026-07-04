// A persistent, renderer-agnostic view of the terminal grid, updated from the
// packed Uint32Array snapshots the WASM core produces. Both the Canvas2D and
// WebGL renderers read this model; link hit-testing and selection read it too.

const SNAPSHOT_MAGIC = 0xf3e70001;
const CELL_WORDS = 6; // [codepoint, fg, bg, flags, link, grapheme]

// Interned single-character strings for ASCII, so the render hot path never
// allocates via String.fromCodePoint for the overwhelmingly common case.
const ASCII = new Array(128);
for (let c = 0; c < 128; c++) ASCII[c] = String.fromCharCode(c);

export class GridModel {
  constructor(cols, rows) {
    this.resize(cols, rows);
    this.cursorX = 0;
    this.cursorY = 0;
    this.cursorVisible = true;
    this.cursorBlink = true;
    this.cursorOnScreen = true;
    // Resolves a non-zero grapheme id to its cluster string (base + combining
    // marks / ZWJ sequence / flag). Set by the component to `engine.grapheme`.
    this.graphemeResolver = null;
    // id -> cluster string cache, so we cross the WASM boundary once per id.
    this._clusterCache = new Map();
  }

  resize(cols, rows) {
    const n = cols * rows;
    this.cols = cols;
    this.rows = rows;
    this.cp = new Uint32Array(n).fill(0x20); // space
    this.fg = new Uint32Array(n);
    this.bg = new Uint32Array(n);
    this.flags = new Uint16Array(n);
    this.link = new Uint32Array(n);
    this.grapheme = new Uint32Array(n);
  }

  /**
   * Apply a snapshot. Returns `{ dirtyRows, full }` where `dirtyRows` is the
   * list of row indices whose contents changed. If the snapshot's dimensions
   * disagree with the model (a resize raced the frame) the model is resized and
   * every row is treated as dirty.
   */
  applySnapshot(u32) {
    if (u32.length < 7 || u32[0] !== SNAPSHOT_MAGIC) {
      return { dirtyRows: [], full: false };
    }
    const cols = u32[1];
    const rows = u32[2];
    let full = false;
    if (cols !== this.cols || rows !== this.rows) {
      this.resize(cols, rows);
      full = true;
    }
    this.cursorX = u32[3];
    this.cursorY = u32[4];
    const cflags = u32[5];
    this.cursorVisible = (cflags & 1) !== 0;
    this.cursorBlink = (cflags & 2) !== 0;
    this.cursorOnScreen = (cflags & 4) !== 0;

    const nRows = u32[6];
    let p = 7;
    const dirtyRows = [];
    for (let i = 0; i < nRows; i++) {
      const y = u32[p++];
      if (y >= rows) {
        p += cols * CELL_WORDS;
        continue;
      }
      let off = y * cols;
      for (let x = 0; x < cols; x++) {
        this.cp[off] = u32[p];
        this.fg[off] = u32[p + 1];
        this.bg[off] = u32[p + 2];
        this.flags[off] = u32[p + 3];
        this.link[off] = u32[p + 4];
        this.grapheme[off] = u32[p + 5];
        p += CELL_WORDS;
        off++;
      }
      dirtyRows.push(y);
    }
    return { dirtyRows, full };
  }

  index(x, y) {
    return y * this.cols + x;
  }

  /**
   * The full text to draw / copy for cell `i`: a single scalar for ordinary
   * cells, or the merged grapheme cluster (accented base, ZWJ emoji, flag) when
   * the cell carries a non-zero grapheme id. Cluster strings are cached by id.
   */
  clusterAt(i) {
    const gid = this.grapheme[i];
    if (gid !== 0 && this.graphemeResolver) {
      let s = this._clusterCache.get(gid);
      if (s === undefined) {
        s = this.graphemeResolver(gid);
        if (s == null) s = '';
        this._clusterCache.set(gid, s);
      }
      if (s) return s;
    }
    const cp = this.cp[i];
    if (cp < 128) return cp === 0 ? ' ' : ASCII[cp]; // hot path, no allocation
    return String.fromCodePoint(cp);
  }

  /** Extract the text of row `y` as a string (for selection / link scanning). */
  rowText(y) {
    let s = '';
    const off = y * this.cols;
    for (let x = 0; x < this.cols; x++) {
      // Skip wide spacers so double-width glyphs aren't doubled.
      if (this.flags[off + x] & (1 << 9)) continue;
      s += this.clusterAt(off + x);
    }
    return s;
  }
}
