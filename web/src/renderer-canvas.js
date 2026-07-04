// Canvas2D renderer. Draws only the rows the core marked dirty, plus the two
// cells touched by cursor movement, so a blinking cursor or a single changed
// line costs almost nothing.

import { ATTR } from './palette.js';

export class CanvasRenderer {
  static get name() {
    return 'canvas';
  }

  constructor(container, metrics, palette) {
    this.palette = palette;
    this.metrics = metrics;
    this.canvas = document.createElement('canvas');
    this.canvas.className = 'ft-canvas';
    this.canvas.style.display = 'block';
    container.appendChild(this.canvas);
    this.ctx = this.canvas.getContext('2d', { alpha: false });
    this.prevCursor = null;
    this.cols = 0;
    this.rows = 0;
  }

  get element() {
    return this.canvas;
  }

  resize(model, metrics) {
    this.metrics = metrics;
    this.cols = model.cols;
    this.rows = model.rows;
    const { cellW, cellH, dpr } = metrics;
    this.canvas.width = Math.round(model.cols * cellW * dpr);
    this.canvas.height = Math.round(model.rows * cellH * dpr);
    this.canvas.style.width = `${model.cols * cellW}px`;
    this.canvas.style.height = `${model.rows * cellH}px`;
    this.ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    this.ctx.textBaseline = 'alphabetic';
    this.prevCursor = null;
    this._paintBackground();
  }

  _paintBackground() {
    this.ctx.fillStyle = this.palette.bg;
    this.ctx.fillRect(0, 0, this.cols * this.metrics.cellW, this.rows * this.metrics.cellH);
  }

  render(model, dirtyRows, full, cursor, selection, hoverLink) {
    if (full) {
      this._paintBackground();
      for (let y = 0; y < model.rows; y++) this._drawRow(model, y, selection, hoverLink);
    } else {
      for (const y of dirtyRows) this._drawRow(model, y, selection, hoverLink);
    }

    // Erase the previous cursor cell if it isn't already being redrawn.
    const pc = this.prevCursor;
    if (pc && !(full || dirtyRows.includes(pc.y)) && pc.y < model.rows) {
      this._drawCell(model, pc.x, pc.y, selection, hoverLink);
    }
    // Draw the current cursor.
    if (cursor.show && cursor.y < model.rows) {
      if (!(full || dirtyRows.includes(cursor.y))) {
        this._drawCell(model, cursor.x, cursor.y, selection, hoverLink);
      }
      this._drawCursor(model, cursor);
    }
    this.prevCursor = cursor.show ? { x: cursor.x, y: cursor.y } : null;
  }

  _drawRow(model, y, selection, hoverLink) {
    const { cellW, cellH, baseline, fontFamily, fontSize } = this.metrics;
    const ctx = this.ctx;
    const cols = model.cols;
    const rowOff = y * cols;
    const py = y * cellH;
    const pal = this.palette;

    // --- Pass 1: backgrounds, coalescing runs of the same color into one rect.
    let runStart = 0;
    let runColor = null;
    const flush = (end, color) => {
      if (color !== null && end > runStart) {
        ctx.fillStyle = color;
        ctx.fillRect(runStart * cellW, py, (end - runStart) * cellW, cellH);
      }
    };
    for (let x = 0; x < cols; x++) {
      const i = rowOff + x;
      const flags = model.flags[i];
      const inverse = (flags & ATTR.INVERSE) !== 0;
      const bg = inverse
        ? pal.resolveCss(model.fg[i], true, (flags & ATTR.BOLD) !== 0)
        : pal.resolveCss(model.bg[i], false, false);
      if (bg !== runColor) {
        flush(x, runColor);
        runStart = x;
        runColor = bg;
      }
    }
    flush(cols, runColor);

    // --- Selection overlay (batched runs), painted over the backgrounds.
    if (selection) {
      let selStart = -1;
      for (let x = 0; x <= cols; x++) {
        const sel = x < cols && this._selected(selection, x, y);
        if (sel && selStart < 0) selStart = x;
        else if (!sel && selStart >= 0) {
          ctx.fillStyle = pal.selection;
          ctx.fillRect(selStart * cellW, py, (x - selStart) * cellW, cellH);
          selStart = -1;
        }
      }
    }

    // --- Pass 2: glyphs + decorations. Only touch ctx.font / fillStyle when the
    // style actually changes (state changes are the expensive part of Canvas2D).
    let curFont = null;
    let curFill = null;
    let curAlpha = 1;
    for (let x = 0; x < cols; x++) {
      const i = rowOff + x;
      const flags = model.flags[i];
      if (flags & ATTR.WIDE_SPACER) continue;
      const cp = model.cp[i];
      const invisible = (flags & ATTR.INVISIBLE) !== 0;
      const px = x * cellW;
      const w = flags & ATTR.WIDE ? cellW * 2 : cellW;
      const bold = (flags & ATTR.BOLD) !== 0;
      const inverse = (flags & ATTR.INVERSE) !== 0;
      let fg = inverse
        ? pal.resolveCss(model.bg[i], false, false)
        : pal.resolveCss(model.fg[i], true, bold);

      if (cp !== 0x20 && cp !== 0 && !invisible) {
        let font = '';
        if (flags & ATTR.ITALIC) font += 'italic ';
        if (bold) font += 'bold ';
        font += `${fontSize}px ${fontFamily}`;
        if (font !== curFont) {
          ctx.font = font;
          curFont = font;
        }
        if (fg !== curFill) {
          ctx.fillStyle = fg;
          curFill = fg;
        }
        const alpha = flags & ATTR.DIM ? 0.6 : 1;
        if (alpha !== curAlpha) {
          ctx.globalAlpha = alpha;
          curAlpha = alpha;
        }
        ctx.fillText(model.clusterAt(i), px, py + baseline);
      }

      // Underline / strikethrough / link hover.
      const hovered =
        hoverLink && hoverLink.y === y && x >= hoverLink.x0 && x <= hoverLink.x1;
      if (flags & ATTR.UNDERLINE || hovered) {
        if (fg !== curFill) {
          ctx.fillStyle = fg;
          curFill = fg;
        }
        ctx.fillRect(px, py + baseline + 2, w, 1);
      }
      if (flags & ATTR.STRIKETHROUGH) {
        if (fg !== curFill) {
          ctx.fillStyle = fg;
          curFill = fg;
        }
        ctx.fillRect(px, py + cellH * 0.55, w, 1);
      }
    }
    if (curAlpha !== 1) ctx.globalAlpha = 1;
  }

  _drawCell(model, x, y, selection, hoverLink) {
    const { cellW, cellH, baseline, fontFamily, fontSize } = this.metrics;
    const i = model.index(x, y);
    const flags = model.flags[i];
    if (flags & ATTR.WIDE_SPACER) {
      // The lead cell paints the glyph across both columns; nothing to do here
      // except keep the background consistent (handled by lead cell draw).
      return;
    }
    const inverse = (flags & ATTR.INVERSE) !== 0;
    const bold = (flags & ATTR.BOLD) !== 0;
    let fg = this.palette.resolveCss(model.fg[i], true, bold);
    let bg = this.palette.resolveCss(model.bg[i], false, false);
    if (inverse) {
      const t = fg;
      fg = bg;
      bg = t;
    }

    const px = x * cellW;
    const py = y * cellH;
    const w = flags & ATTR.WIDE ? cellW * 2 : cellW;

    const ctx = this.ctx;
    ctx.fillStyle = bg;
    ctx.fillRect(px, py, w, cellH);

    // Selection overlay.
    if (selection && this._selected(selection, x, y)) {
      ctx.fillStyle = this.palette.selection;
      ctx.fillRect(px, py, w, cellH);
    }

    const cp = model.cp[i];
    const invisible = (flags & ATTR.INVISIBLE) !== 0;
    if (cp !== 0x20 && cp !== 0 && !invisible) {
      let font = '';
      if (flags & ATTR.ITALIC) font += 'italic ';
      if (bold) font += 'bold ';
      font += `${fontSize}px ${fontFamily}`;
      ctx.font = font;
      ctx.fillStyle = fg;
      if (flags & ATTR.DIM) ctx.globalAlpha = 0.6;
      ctx.fillText(model.clusterAt(i), px, py + baseline);
      if (flags & ATTR.DIM) ctx.globalAlpha = 1;
    }

    // Underline / strikethrough / link hover.
    const hovered =
      hoverLink && hoverLink.y === y && x >= hoverLink.x0 && x <= hoverLink.x1;
    if (flags & ATTR.UNDERLINE || hovered) {
      ctx.strokeStyle = fg;
      ctx.lineWidth = Math.max(1, this.metrics.dpr === 1 ? 1 : 1);
      ctx.beginPath();
      ctx.moveTo(px, py + baseline + 2);
      ctx.lineTo(px + w, py + baseline + 2);
      ctx.stroke();
    }
    if (flags & ATTR.STRIKETHROUGH) {
      ctx.strokeStyle = fg;
      ctx.beginPath();
      const sy = py + cellH * 0.55;
      ctx.moveTo(px, sy);
      ctx.lineTo(px + w, sy);
      ctx.stroke();
    }
  }

  _drawCursor(model, cursor) {
    const { cellW, cellH, baseline, fontFamily, fontSize } = this.metrics;
    const i = model.index(cursor.x, cursor.y);
    const flags = model.flags[i];
    const w = flags & ATTR.WIDE ? cellW * 2 : cellW;
    const px = cursor.x * cellW;
    const py = cursor.y * cellH;
    const ctx = this.ctx;

    const style = cursor.style || 'block';
    if (!cursor.focused) {
      // Hollow box when unfocused.
      ctx.strokeStyle = this.palette.cursor;
      ctx.lineWidth = 1;
      ctx.strokeRect(px + 0.5, py + 0.5, w - 1, cellH - 1);
      return;
    }
    if (style === 'bar') {
      ctx.fillStyle = this.palette.cursor;
      ctx.fillRect(px, py, 2, cellH);
      return;
    }
    if (style === 'underline') {
      ctx.fillStyle = this.palette.cursor;
      ctx.fillRect(px, py + cellH - 2, w, 2);
      return;
    }
    // Block: fill and redraw glyph in accent color.
    ctx.fillStyle = this.palette.cursor;
    ctx.fillRect(px, py, w, cellH);
    const cp = model.cp[i];
    if (cp !== 0x20 && cp !== 0) {
      let font = '';
      if (flags & ATTR.ITALIC) font += 'italic ';
      if (flags & ATTR.BOLD) font += 'bold ';
      font += `${fontSize}px ${fontFamily}`;
      ctx.font = font;
      ctx.fillStyle = this.palette.cursorAccent;
      ctx.fillText(model.clusterAt(i), px, py + baseline);
    }
  }

  _selected(sel, x, y) {
    if (y < sel.sy || y > sel.ey) return false;
    if (sel.sy === sel.ey) return x >= sel.sx && x < sel.ex;
    if (y === sel.sy) return x >= sel.sx;
    if (y === sel.ey) return x < sel.ex;
    return true;
  }

  dispose() {
    this.canvas.remove();
  }
}
