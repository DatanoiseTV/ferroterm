// Instanced WebGL renderer. Draws the whole grid with a single instanced draw
// call: one instance per visible cell, expanded from a shared unit quad in the
// vertex shader. Each instance carries its pixel rect, foreground and
// background colors and glyph atlas coords; the fragment shader composites the
// glyph over the background, so a cell's background and text are one instance
// (no separate background pass). This writes ~17 floats per cell instead of the
// 54 floats per quad (and up to two quads per cell) the batched renderer wrote.
//
// Glyphs are rasterized once into a texture atlas (alpha masks for text, full
// color for emoji), exactly as the batched renderer does.
//
// Requires WebGL1 + ANGLE_instanced_arrays (universally available where WebGL
// is). The constructor throws otherwise, and the host falls back to Canvas2D.

import { ATTR } from './palette.js';

const VERT_SRC = `
attribute vec2 aCorner;   // static unit quad, 0..1
attribute vec4 aRect;     // instance: x, y, w, h in device px
attribute vec4 aFg;       // instance: glyph rgba
attribute vec4 aBg;       // instance: background rgba (a=0 => no fill)
attribute vec4 aTex;      // instance: atlas u0,v0,u1,v1 (u0<0 => no glyph)
attribute float aTint;    // instance: 1 = alpha-mask tint, 0 = color glyph
uniform vec2 uInv;        // 2/W, 2/H
varying vec2 vTex;
varying vec4 vFg;
varying vec4 vBg;
varying float vTint;
varying float vHasGlyph;
void main() {
  vec2 p = aRect.xy + aCorner * aRect.zw;
  gl_Position = vec4(p.x * uInv.x - 1.0, 1.0 - p.y * uInv.y, 0.0, 1.0);
  vTex = mix(aTex.xy, aTex.zw, aCorner);
  vHasGlyph = step(0.0, aTex.x);
  vFg = aFg;
  vBg = aBg;
  vTint = aTint;
}`;

const FRAG_SRC = `
precision mediump float;
varying vec2 vTex;
varying vec4 vFg;
varying vec4 vBg;
varying float vTint;
varying float vHasGlyph;
uniform sampler2D uAtlas;
void main() {
  vec3 brgb = vBg.rgb;
  float ba = vBg.a;
  float ga = 0.0;
  vec3 grgb = vec3(0.0);
  if (vHasGlyph > 0.5) {
    vec4 t = texture2D(uAtlas, vTex);
    // Tinted alpha-mask (text): glyph rgb = fg, coverage = t.a * fg.a (dim).
    // Color glyph (emoji): use the texel directly.
    grgb = mix(t.rgb, vFg.rgb, vTint);
    ga = mix(t.a, t.a * vFg.a, vTint);
  }
  // Composite glyph over the (possibly transparent) background in straight
  // alpha, so the result blends against the cleared default background exactly
  // as a separate glyph-over-background pass would.
  float outA = ga + ba * (1.0 - ga);
  if (outA <= 0.0) discard;
  vec3 outRGB = (grgb * ga + brgb * ba * (1.0 - ga)) / outA;
  gl_FragColor = vec4(outRGB, outA);
}`;

const FLOATS_PER_INSTANCE = 17; // rect(4) fg(4) bg(4) tex(4) tint(1)

function isColorGlyph(cp) {
  return (
    (cp >= 0x1f300 && cp <= 0x1faff) ||
    (cp >= 0x2600 && cp <= 0x27bf) ||
    (cp >= 0x1f000 && cp <= 0x1f0ff)
  );
}

export class WebGLRenderer {
  static get name() {
    return 'webgl';
  }

  constructor(container, metrics, palette) {
    this.palette = palette;
    this.metrics = metrics;
    this.canvas = document.createElement('canvas');
    this.canvas.className = 'ft-canvas';
    this.canvas.style.display = 'block';
    container.appendChild(this.canvas);

    const gl =
      this.canvas.getContext('webgl', { alpha: false, antialias: false }) ||
      this.canvas.getContext('experimental-webgl', { alpha: false, antialias: false });
    if (!gl) {
      this.canvas.remove();
      throw new Error('WebGL not available');
    }
    this.gl = gl;
    this.ext = gl.getExtension('ANGLE_instanced_arrays');
    if (!this.ext) {
      this.canvas.remove();
      throw new Error('ANGLE_instanced_arrays not available');
    }
    this._initGL();

    this.atlasCanvas = document.createElement('canvas');
    this.atlasCtx = this.atlasCanvas.getContext('2d', { willReadFrequently: false });
    this.glyphCache = new Map();
    this.inst = new Float32Array(0);
  }

  get element() {
    return this.canvas;
  }

  _initGL() {
    const gl = this.gl;
    const prog = this._program(VERT_SRC, FRAG_SRC);
    this.prog = prog;
    gl.useProgram(prog);
    this.loc = {
      aCorner: gl.getAttribLocation(prog, 'aCorner'),
      aRect: gl.getAttribLocation(prog, 'aRect'),
      aFg: gl.getAttribLocation(prog, 'aFg'),
      aBg: gl.getAttribLocation(prog, 'aBg'),
      aTex: gl.getAttribLocation(prog, 'aTex'),
      aTint: gl.getAttribLocation(prog, 'aTint'),
      uAtlas: gl.getUniformLocation(prog, 'uAtlas'),
      uInv: gl.getUniformLocation(prog, 'uInv'),
    };
    // Static unit quad shared by every instance (two triangles).
    this.cornerBuf = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, this.cornerBuf);
    gl.bufferData(
      gl.ARRAY_BUFFER,
      new Float32Array([0, 0, 1, 0, 0, 1, 1, 0, 1, 1, 0, 1]),
      gl.STATIC_DRAW
    );
    this.instBuf = gl.createBuffer();
    this.texture = gl.createTexture();
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);
  }

  _program(vsrc, fsrc) {
    const gl = this.gl;
    const compile = (type, src) => {
      const s = gl.createShader(type);
      gl.shaderSource(s, src);
      gl.compileShader(s);
      if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) {
        throw new Error('shader: ' + gl.getShaderInfoLog(s));
      }
      return s;
    };
    const p = gl.createProgram();
    gl.attachShader(p, compile(gl.VERTEX_SHADER, vsrc));
    gl.attachShader(p, compile(gl.FRAGMENT_SHADER, fsrc));
    gl.linkProgram(p);
    if (!gl.getProgramParameter(p, gl.LINK_STATUS)) {
      throw new Error('link: ' + gl.getProgramInfoLog(p));
    }
    return p;
  }

  resize(model, metrics) {
    this.metrics = metrics;
    this.cols = model.cols;
    this.rows = model.rows;
    const { cellW, cellH, dpr } = metrics;
    const W = Math.round(model.cols * cellW * dpr);
    const H = Math.round(model.rows * cellH * dpr);
    this.canvas.width = W;
    this.canvas.height = H;
    this.canvas.style.width = `${model.cols * cellW}px`;
    this.canvas.style.height = `${model.rows * cellH}px`;
    this.gl.viewport(0, 0, W, H);
    this.W = W;
    this.H = H;

    this.gcw = Math.ceil(cellW * dpr);
    this.gch = Math.ceil(cellH * dpr);
    this.atlasSize = 2048;
    this.atlasCanvas.width = this.atlasSize;
    this.atlasCanvas.height = this.atlasSize;
    this._resetAtlas();

    // One instance per cell, plus a margin for cursor / decoration instances.
    const maxInstances = model.cols * model.rows + model.cols + 8;
    this.inst = new Float32Array(maxInstances * FLOATS_PER_INSTANCE);
  }

  _resetAtlas() {
    const ctx = this.atlasCtx;
    ctx.clearRect(0, 0, this.atlasSize, this.atlasSize);
    const { fontFamily, fontSize, dpr, baseline } = this.metrics;
    ctx.textBaseline = 'alphabetic';
    this._atlasFont = { fontFamily, fontSize: fontSize * dpr, baseline: baseline * dpr };
    this.glyphCache.clear();
    this._shelfX = 0;
    this._shelfY = 0;
    this._uploadAtlas();
  }

  _allocSlot(w) {
    if (this._shelfX + w > this.atlasSize) {
      this._shelfX = 0;
      this._shelfY += this.gch;
    }
    if (this._shelfY + this.gch > this.atlasSize) {
      this._resetAtlas();
    }
    const x = this._shelfX;
    const y = this._shelfY;
    this._shelfX += w;
    return { x, y };
  }

  _uploadAtlas() {
    const gl = this.gl;
    gl.bindTexture(gl.TEXTURE_2D, this.texture);
    gl.pixelStorei(gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, false);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, this.atlasCanvas);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  }

  _glyph(cp, cluster, styleBits, cells) {
    const key = cluster === null ? cp * 16 + styleBits : cluster + '\x00' + styleBits;
    let g = this.glyphCache.get(key);
    if (g !== undefined) return g;

    const bold = (styleBits & 1) !== 0;
    const italic = (styleBits & 2) !== 0;
    const color = (styleBits & 4) !== 0;
    const text = cluster === null ? String.fromCodePoint(cp) : cluster;
    const slotW = this.gcw * cells;
    const { x, y } = this._allocSlot(slotW);

    const ctx = this.atlasCtx;
    ctx.clearRect(x, y, slotW, this.gch);
    let font = '';
    if (italic) font += 'italic ';
    if (bold) font += 'bold ';
    font += `${this._atlasFont.fontSize}px ${this._atlasFont.fontFamily}`;
    ctx.font = font;
    ctx.fillStyle = '#ffffff';
    ctx.fillText(text, x, y + this._atlasFont.baseline);

    g = {
      u0: x / this.atlasSize,
      v0: y / this.atlasSize,
      u1: (x + slotW) / this.atlasSize,
      v1: (y + this.gch) / this.atlasSize,
      tint: color ? 0 : 1,
    };
    this.glyphCache.set(key, g);
    this._dirtyAtlas = true;
    return g;
  }

  // Append one instance. `tex` null => background-only (no glyph). Colors are
  // 0..1. Writes 17 floats at `this._o`.
  _inst(x, y, w, h, fr, fg, fb, fa, br, bg, bb, ba, tex, tint) {
    const v = this.inst;
    let o = this._o;
    v[o] = x; v[o + 1] = y; v[o + 2] = w; v[o + 3] = h;
    v[o + 4] = fr; v[o + 5] = fg; v[o + 6] = fb; v[o + 7] = fa;
    v[o + 8] = br; v[o + 9] = bg; v[o + 10] = bb; v[o + 11] = ba;
    if (tex) {
      v[o + 12] = tex.u0; v[o + 13] = tex.v0; v[o + 14] = tex.u1; v[o + 15] = tex.v1;
    } else {
      v[o + 12] = -1; v[o + 13] = -1; v[o + 14] = -1; v[o + 15] = -1;
    }
    v[o + 16] = tint;
    this._o = o + FLOATS_PER_INSTANCE;
  }

  render(model, _dirtyRows, _full, cursor, selection, hoverLink) {
    const gl = this.gl;
    this._o = 0;
    this._dirtyAtlas = false;

    const dpr = this.metrics.dpr;
    const cw = this.metrics.cellW * dpr;
    const ch = this.metrics.cellH * dpr;
    const pal = this.palette;
    const cols = model.cols;
    const bgRgb = pal.bgRgb;
    const dbr = bgRgb[0] / 255, dbg = bgRgb[1] / 255, dbb = bgRgb[2] / 255;
    const sel = selection ? this._selCss() : null;
    const t = Math.max(1, Math.round(dpr)); // decoration thickness (px)

    const cpA = model.cp, fgA = model.fg, bgA = model.bg, flagsA = model.flags,
      graphemeA = model.grapheme;

    for (let y = 0; y < model.rows; y++) {
      const base = y * cols;
      const yc = y * ch;
      const selRange =
        sel && y >= selection.sy && y <= selection.ey ? this._selSpan(selection, y, cols) : null;
      for (let x = 0; x < cols; x++) {
        const i = base + x;
        const flags = flagsA[i];
        if (flags & ATTR.WIDE_SPACER) continue;
        const inverse = (flags & ATTR.INVERSE) !== 0;
        const cp = cpA[i];
        const hasGlyph = cp !== 0x20 && cp !== 0 && !(flags & ATTR.INVISIBLE);
        const selected = selRange && x >= selRange[0] && x < selRange[1];

        // Resolve background (rgb always; alpha 1 only when it must be filled).
        const bgKind = bgA[i] >>> 24;
        let br, bg, bb, ba;
        if (inverse) {
          const c = pal.resolveRgb(fgA[i], true, false);
          br = c[0] / 255; bg = c[1] / 255; bb = c[2] / 255; ba = 1;
        } else if (bgKind !== 0) {
          const c = pal.resolveRgb(bgA[i], false, false);
          br = c[0] / 255; bg = c[1] / 255; bb = c[2] / 255; ba = 1;
        } else {
          br = dbr; bg = dbg; bb = dbb; ba = 0; // default bg -> clear shows it
        }
        if (selected) {
          // Selection tints the background (under the glyph), matching the
          // batched renderer's translucent overlay.
          const sa = sel[3];
          br = br * (1 - sa) + sel[0] * sa;
          bg = bg * (1 - sa) + sel[1] * sa;
          bb = bb * (1 - sa) + sel[2] * sa;
          ba = 1;
        }

        // Foreground / glyph.
        let fr = 0, fgc = 0, fb = 0, fa = 1, glyph = null, tint = 1;
        if (hasGlyph) {
          const bold = (flags & ATTR.BOLD) !== 0;
          const fc = inverse
            ? pal.resolveRgb(bgA[i], false, false)
            : pal.resolveRgb(fgA[i], true, bold);
          fr = fc[0] / 255; fgc = fc[1] / 255; fb = fc[2] / 255;
          fa = flags & ATTR.DIM ? 0.6 : 1;
          const cells = flags & ATTR.WIDE ? 2 : 1;
          const styleBits =
            (bold ? 1 : 0) | (flags & ATTR.ITALIC ? 2 : 0) | (isColorGlyph(cp) ? 4 : 0);
          const cluster = graphemeA[i] !== 0 ? model.clusterAt(i) : null;
          glyph = this._glyph(cp, cluster, styleBits, cells);
          tint = glyph.tint;
        }

        if (ba === 0 && !hasGlyph) continue; // nothing to draw
        const w = flags & ATTR.WIDE ? cw * 2 : cw;
        this._inst(x * cw, yc, w, ch, fr, fgc, fb, fa, br, bg, bb, ba, glyph, tint);

        // Underline / strike / hover-link as thin background-only instances,
        // drawn after the cell (on top of its glyph).
        const hovered =
          hoverLink && hoverLink.y === y && x >= hoverLink.x0 && x <= hoverLink.x1;
        if ((flags & ATTR.UNDERLINE || hovered) && hasGlyph) {
          const dc = fr, dg = fgc, db = fb;
          this._inst(x * cw, yc + ch - t * 2, w, t, 0, 0, 0, 1, dc, dg, db, 1, null, 0);
        } else if (flags & ATTR.UNDERLINE || hovered) {
          const c = pal.resolveRgb(fgA[i], true, false);
          this._inst(x * cw, yc + ch - t * 2, w, t, 0, 0, 0, 1, c[0] / 255, c[1] / 255, c[2] / 255, 1, null, 0);
        }
        if (flags & ATTR.STRIKETHROUGH) {
          const c = hasGlyph ? [fr * 255, fgc * 255, fb * 255] : pal.resolveRgb(fgA[i], true, false);
          this._inst(x * cw, yc + ch * 0.55, w, t, 0, 0, 0, 1, c[0] / 255, c[1] / 255, c[2] / 255, 1, null, 0);
        }
      }
    }

    if (cursor.show && cursor.y < model.rows) {
      this._pushCursor(model, cursor, cw, ch);
    }

    if (this._dirtyAtlas) this._uploadAtlas();

    // Draw.
    gl.clearColor(dbr, dbg, dbb, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.useProgram(this.prog);
    gl.uniform2f(this.loc.uInv, 2 / this.W, 2 / this.H);

    // Static unit quad.
    gl.bindBuffer(gl.ARRAY_BUFFER, this.cornerBuf);
    gl.enableVertexAttribArray(this.loc.aCorner);
    gl.vertexAttribPointer(this.loc.aCorner, 2, gl.FLOAT, false, 0, 0);
    this.ext.vertexAttribDivisorANGLE(this.loc.aCorner, 0);

    // Instance attributes.
    const nInst = this._o / FLOATS_PER_INSTANCE;
    gl.bindBuffer(gl.ARRAY_BUFFER, this.instBuf);
    gl.bufferData(gl.ARRAY_BUFFER, this.inst.subarray(0, this._o), gl.STREAM_DRAW);
    const stride = FLOATS_PER_INSTANCE * 4;
    this._bindInstanceAttr(this.loc.aRect, 4, stride, 0);
    this._bindInstanceAttr(this.loc.aFg, 4, stride, 16);
    this._bindInstanceAttr(this.loc.aBg, 4, stride, 32);
    this._bindInstanceAttr(this.loc.aTex, 4, stride, 48);
    this._bindInstanceAttr(this.loc.aTint, 1, stride, 64);

    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.texture);
    gl.uniform1i(this.loc.uAtlas, 0);
    this.ext.drawArraysInstancedANGLE(gl.TRIANGLES, 0, 6, nInst);
  }

  _bindInstanceAttr(loc, size, stride, offset) {
    const gl = this.gl;
    gl.enableVertexAttribArray(loc);
    gl.vertexAttribPointer(loc, size, gl.FLOAT, false, stride, offset);
    this.ext.vertexAttribDivisorANGLE(loc, 1);
  }

  _pushCursor(model, cursor, cw, ch) {
    const i = model.index(cursor.x, cursor.y);
    const flags = model.flags[i];
    const w = flags & ATTR.WIDE ? cw * 2 : cw;
    const px = cursor.x * cw;
    const py = cursor.y * ch;
    const c = this._cursorRgb();
    const style = cursor.style || 'block';
    const dpr = this.metrics.dpr;
    if (!cursor.focused) {
      const t = Math.max(1, Math.round(dpr));
      this._inst(px, py, w, t, 0, 0, 0, 1, c[0], c[1], c[2], 1, null, 0);
      this._inst(px, py + ch - t, w, t, 0, 0, 0, 1, c[0], c[1], c[2], 1, null, 0);
      this._inst(px, py, t, ch, 0, 0, 0, 1, c[0], c[1], c[2], 1, null, 0);
      this._inst(px + w - t, py, t, ch, 0, 0, 0, 1, c[0], c[1], c[2], 1, null, 0);
    } else if (style === 'bar') {
      const t = Math.max(1, Math.round(2 * dpr));
      this._inst(px, py, t, ch, 0, 0, 0, 1, c[0], c[1], c[2], 1, null, 0);
    } else if (style === 'underline') {
      const t = Math.max(1, Math.round(2 * dpr));
      this._inst(px, py + ch - t, w, t, 0, 0, 0, 1, c[0], c[1], c[2], 1, null, 0);
    } else {
      // Block: fill the cell with the cursor color, then re-draw the glyph in
      // the accent color on top.
      const cp = model.cp[i];
      const cells = flags & ATTR.WIDE ? 2 : 1;
      if (cp !== 0x20 && cp !== 0) {
        const ca = this._cursorAccentRgb();
        const styleBits =
          (flags & ATTR.BOLD ? 1 : 0) | (flags & ATTR.ITALIC ? 2 : 0) | (isColorGlyph(cp) ? 4 : 0);
        const cluster = model.grapheme[i] !== 0 ? model.clusterAt(i) : null;
        const g = this._glyph(cp, cluster, styleBits, cells);
        this._inst(px, py, w, ch, ca[0], ca[1], ca[2], 1, c[0], c[1], c[2], 1, g, g.tint);
      } else {
        this._inst(px, py, w, ch, 0, 0, 0, 1, c[0], c[1], c[2], 1, null, 0);
      }
    }
  }

  _cursorRgb() {
    return this._css2rgb(this.palette.cursor);
  }
  _cursorAccentRgb() {
    return this._css2rgb(this.palette.cursorAccent);
  }
  _selCss() {
    const m = /rgba?\(([^)]+)\)/.exec(this.palette.selection);
    if (!m) return [0.4, 0.6, 1, 0.35];
    const p = m[1].split(',').map((s) => parseFloat(s));
    return [p[0] / 255, p[1] / 255, p[2] / 255, p[3] === undefined ? 1 : p[3]];
  }
  _selSpan(sel, y, cols) {
    if (sel.sy === sel.ey) return [sel.sx, sel.ex];
    if (y === sel.sy) return [sel.sx, cols];
    if (y === sel.ey) return [0, sel.ex];
    return [0, cols];
  }
  _css2rgb(css) {
    if (css[0] === '#') {
      let h = css.slice(1);
      if (h.length === 3) h = h[0] + h[0] + h[1] + h[1] + h[2] + h[2];
      const n = parseInt(h, 16);
      return [((n >> 16) & 255) / 255, ((n >> 8) & 255) / 255, (n & 255) / 255];
    }
    const m = /rgba?\(([^)]+)\)/.exec(css);
    if (m) {
      const p = m[1].split(',').map((s) => parseFloat(s));
      return [p[0] / 255, p[1] / 255, p[2] / 255];
    }
    return [1, 1, 1];
  }

  dispose() {
    const gl = this.gl;
    gl.deleteBuffer(this.instBuf);
    gl.deleteBuffer(this.cornerBuf);
    gl.deleteTexture(this.texture);
    gl.deleteProgram(this.prog);
    this.canvas.remove();
  }
}
