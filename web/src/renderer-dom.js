// DOM renderer: the terminal drawn as real HTML elements, in the spirit of
// xterm.js's DOM renderer. Each grid row is one <div>; within a row, adjacent
// cells that share a style are coalesced into a single <span> (run-length), so
// a line of same-colored text costs one node, not one-per-cell. The cursor is a
// separate absolutely-positioned overlay.
//
// It is the slowest of the three renderers (mutating the DOM and re-laying out
// text is inherently pricier than a canvas blit or a WebGL draw) — the same
// reason xterm.js defaults away from it. Its upside is a no-canvas, no-WebGL
// fallback that renders text through the browser's own text stack, so it stays
// crisp at any devicePixelRatio with no glyph atlas, and it's trivially
// inspectable / selectable in devtools.
//
// Column alignment relies on the monospace font: each cell advances by one
// character width and wide (CJK) glyphs advance by two, so spacer cells are
// simply skipped. Sub-pixel font drift over very long lines is the known
// trade-off of text-flow rendering (canvas/WebGL place every cell explicitly).

import { ATTR } from './palette.js';

export class DomRenderer {
  static get name() {
    return 'dom';
  }

  constructor(container, metrics, palette) {
    this.palette = palette;
    this.metrics = metrics;

    this.root = document.createElement('div');
    this.root.className = 'ft-dom';
    const s = this.root.style;
    s.position = 'relative';
    s.overflow = 'hidden';
    s.whiteSpace = 'pre';
    s.contain = 'strict'; // isolate layout/paint to this subtree
    s.background = palette.bg;
    container.appendChild(this.root);

    this.rowEls = [];
    this.cursorEl = document.createElement('div');
    this.cursorEl.className = 'ft-dom-cursor';
    this.cursorEl.style.position = 'absolute';
    this.cursorEl.style.pointerEvents = 'none';
    this.cursorEl.style.display = 'none';
    this.root.appendChild(this.cursorEl);

    this.cols = 0;
    this.rows = 0;
    this._sel = parseColor(palette.selection); // [r,g,b,a] for blending
  }

  get element() {
    return this.root;
  }

  resize(model, metrics) {
    this.metrics = metrics;
    this.cols = model.cols;
    this.rows = model.rows;
    this._sel = parseColor(this.palette.selection);

    const { cellW, cellH, fontFamily, fontSize } = metrics;
    const s = this.root.style;
    s.width = `${model.cols * cellW}px`;
    s.height = `${model.rows * cellH}px`;
    // Base (regular) font on the container; spans override weight/style only.
    s.font = `${fontSize}px/${cellH}px ${fontFamily}`;
    s.background = this.palette.bg;

    // (Re)build the row element pool to match the new row count.
    for (const el of this.rowEls) el.remove();
    this.rowEls = [];
    for (let y = 0; y < model.rows; y++) {
      const row = document.createElement('div');
      row.className = 'ft-dom-row';
      row.style.height = `${cellH}px`;
      // Insert rows before the cursor overlay so the cursor stays on top.
      this.root.insertBefore(row, this.cursorEl);
      this.rowEls.push(row);
    }
    for (let y = 0; y < model.rows; y++) this._buildRow(model, y, null, null);
  }

  render(model, dirtyRows, full, cursor, selection, hoverLink) {
    if (full) {
      for (let y = 0; y < model.rows; y++) this._buildRow(model, y, selection, hoverLink);
    } else {
      for (const y of dirtyRows) {
        if (y < model.rows) this._buildRow(model, y, selection, hoverLink);
      }
    }
    this._placeCursor(model, cursor);
  }

  // Rebuild one row's spans, coalescing runs of identical style.
  _buildRow(model, y, selection, hoverLink) {
    const row = this.rowEls[y];
    if (!row) return;
    const { cellW, fontSize, fontFamily } = this.metrics;
    const cols = model.cols;
    const off = y * cols;
    const pal = this.palette;

    const spans = [];
    let runText = '';
    let runKey = null;
    let runStyle = null;

    const flush = () => {
      if (runText.length && runStyle) {
        const span = document.createElement('span');
        span.textContent = runText;
        Object.assign(span.style, runStyle);
        spans.push(span);
      }
      runText = '';
    };

    for (let x = 0; x < cols; x++) {
      const i = off + x;
      const flags = model.flags[i];
      if (flags & ATTR.WIDE_SPACER) continue; // wide glyph to the left covers it

      const bold = (flags & ATTR.BOLD) !== 0;
      const italic = (flags & ATTR.ITALIC) !== 0;
      const inverse = (flags & ATTR.INVERSE) !== 0;
      const invisible = (flags & ATTR.INVISIBLE) !== 0;
      const dim = (flags & ATTR.DIM) !== 0;

      let fg = inverse
        ? pal.resolveCss(model.bg[i], false, false)
        : pal.resolveCss(model.fg[i], true, bold);
      let bg = inverse
        ? pal.resolveCss(model.fg[i], true, bold)
        : pal.resolveCss(model.bg[i], false, false);

      // Dim: fade the text only (translucent color over its own bg), matching
      // the canvas renderer's per-glyph alpha rather than dimming the cell.
      if (dim) {
        const rgb = pal.resolveRgb(inverse ? model.bg[i] : model.fg[i], !inverse, bold);
        fg = `rgba(${rgb[0]},${rgb[1]},${rgb[2]},0.6)`;
      }

      // Selection: blend the translucent selection color over the cell bg to an
      // opaque result, so text stays crisp (as the canvas renderer does).
      if (selection && selected(selection, x, y)) {
        bg = blendCss(this._sel, bg, pal);
      }

      const hovered =
        hoverLink && hoverLink.y === y && x >= hoverLink.x0 && x <= hoverLink.x1;
      const underline = (flags & ATTR.UNDERLINE) !== 0 || hovered;
      const strike = (flags & ATTR.STRIKETHROUGH) !== 0;

      const ch = invisible ? ' ' : model.clusterAt(i);
      const key = `${fg}|${bg}|${bold}|${italic}|${underline}|${strike}`;

      if (key !== runKey) {
        flush();
        runKey = key;
        const decoration =
          (underline ? 'underline ' : '') + (strike ? 'line-through' : '');
        runStyle = {
          color: fg,
          background: bg,
          fontWeight: bold ? 'bold' : 'normal',
          fontStyle: italic ? 'italic' : 'normal',
          textDecoration: decoration.trim() || 'none',
        };
      }
      runText += ch;
    }
    flush();

    row.replaceChildren(...spans);
  }

  _placeCursor(model, cursor) {
    const el = this.cursorEl;
    if (!cursor.show || cursor.y >= model.rows) {
      el.style.display = 'none';
      return;
    }
    const { cellW, cellH, fontFamily, fontSize, baseline } = this.metrics;
    const i = model.index(cursor.x, cursor.y);
    const flags = model.flags[i];
    const w = flags & ATTR.WIDE ? cellW * 2 : cellW;
    const st = el.style;
    st.display = 'block';
    st.left = `${cursor.x * cellW}px`;
    st.top = `${cursor.y * cellH}px`;
    st.height = `${cellH}px`;
    st.width = `${w}px`;
    st.font = `${fontSize}px/${cellH}px ${fontFamily}`;
    st.textAlign = 'left';
    el.textContent = '';
    st.border = 'none';
    st.background = 'transparent';
    st.color = 'transparent';

    const style = cursor.style || 'block';
    if (!cursor.focused) {
      // Hollow box when unfocused; the underlying glyph shows through.
      st.boxSizing = 'border-box';
      st.border = `1px solid ${this.palette.cursor}`;
      return;
    }
    if (style === 'bar') {
      st.width = '2px';
      st.background = this.palette.cursor;
      return;
    }
    if (style === 'underline') {
      st.top = `${cursor.y * cellH + cellH - 2}px`;
      st.height = '2px';
      st.background = this.palette.cursor;
      return;
    }
    // Block: fill and redraw the glyph in the accent color on top.
    st.background = this.palette.cursor;
    const cp = model.cp[i];
    if (cp !== 0x20 && cp !== 0) {
      st.color = this.palette.cursorAccent;
      st.fontWeight = flags & ATTR.BOLD ? 'bold' : 'normal';
      st.fontStyle = flags & ATTR.ITALIC ? 'italic' : 'normal';
      el.textContent = model.clusterAt(i);
    }
  }

  dispose() {
    this.root.remove();
  }
}

function selected(sel, x, y) {
  if (y < sel.sy || y > sel.ey) return false;
  if (sel.sy === sel.ey) return x >= sel.sx && x < sel.ex;
  if (y === sel.sy) return x >= sel.sx;
  if (y === sel.ey) return x < sel.ex;
  return true;
}

// Parse an `rgb()` / `rgba()` / `#rrggbb` string into `[r,g,b,a]`.
function parseColor(css) {
  const m = /rgba?\(([^)]+)\)/.exec(css);
  if (m) {
    const p = m[1].split(',').map((s) => parseFloat(s));
    return [p[0] | 0, p[1] | 0, p[2] | 0, p[3] === undefined ? 1 : p[3]];
  }
  const h = /^#([0-9a-f]{6})$/i.exec(css);
  if (h) {
    const n = parseInt(h[1], 16);
    return [(n >> 16) & 0xff, (n >> 8) & 0xff, n & 0xff, 1];
  }
  return [0, 0, 0, 1];
}

// Blend translucent `top` ([r,g,b,a]) over an opaque `bottomCss` color, using
// the palette to resolve the bottom to concrete RGB. Returns an opaque rgb().
function blendCss(top, bottomCss, pal) {
  const b = parseColor(bottomCss.startsWith('rgb') || bottomCss.startsWith('#') ? bottomCss : pal.bg);
  const a = top[3];
  const r = Math.round(top[0] * a + b[0] * (1 - a));
  const g = Math.round(top[1] * a + b[1] * (1 - a));
  const bl = Math.round(top[2] * a + b[2] * (1 - a));
  return `rgb(${r},${g},${bl})`;
}
