// Instanced WebGL renderer. Draws the grid with per-cell instancing: one
// instance per cell, expanded from a shared unit quad in the vertex shader.
// Each instance carries its pixel rect, packed fg/bg colors and glyph atlas
// coords; the fragment shader composites the glyph over the background, so a
// cell's background and text are one instance (no separate background pass).
//
// Rendering is incremental. A persistent GPU buffer holds one fixed slot per
// cell, so between frames only the rows that actually changed are regenerated
// and re-uploaded (bufferSubData of the changed row span); the whole grid is
// still drawn each frame, but empty/unchanged cells are cheap degenerate
// instances. Cursor, underline/strike decorations and hover-link underlines
// live in a small per-frame overlay buffer drawn on top, so a moving cursor or
// a one-line edit costs a fraction of a full repaint. Selection is baked into
// the cell background (its rows are marked dirty when the selection changes).
//
// Glyphs are rasterized once into a texture atlas (alpha masks for text, full
// color for emoji).
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

// Instance layout, 44 bytes = 11 four-byte words:
//   rect(4 float) tex(4 float) tint(1 float) fg(1 u32) bg(1 u32)
// Colors are packed RGBA8 (0xAABBGGRR) and read by the shader as normalized
// UNSIGNED_BYTE vec4s, so the render loop never divides by 255 and each cell
// writes 11 words instead of 17 floats.
const WORDS_PER_INSTANCE = 11;
const F_RECT = 0, F_TEX = 4, F_TINT = 8, U_FG = 9, U_BG = 10;

// Pack r,g,b (0..255) and a (0..255) into 0xAABBGGRR for a normalized
// UNSIGNED_BYTE vec4 (little-endian -> byte0=r ... byte3=a).
function packRGBA(r, g, b, a) {
  return (r | (g << 8) | (b << 16) | (a << 24)) >>> 0;
}

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
    // Grid + overlay instance buffers (aliased float/u32/byte views) are
    // allocated in resize.
    this.gridAB = this.ovAB = null;
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
    // Two GPU buffers: a persistent per-cell grid (updated incrementally, one
    // fixed slot per cell) and a small per-frame overlay (cursor / decorations
    // / hover), drawn on top.
    this.gridGlBuf = gl.createBuffer();
    this.ovGlBuf = gl.createBuffer();
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

    // Grid: exactly one instance slot per cell (slot y*cols+x). Persistent, so
    // clean rows keep their GPU data between frames and only dirty rows are
    // re-uploaded. Float and u32 views alias the buffer (rect/tex/flags floats,
    // colors packed u32).
    const cells = model.cols * model.rows;
    this.gridAB = new ArrayBuffer(cells * WORDS_PER_INSTANCE * 4);
    this.gridF = new Float32Array(this.gridAB);
    this.gridU = new Uint32Array(this.gridAB);
    this.gridBytes = new Uint8Array(this.gridAB);
    this._rowBytes = model.cols * WORDS_PER_INSTANCE * 4; // one row's byte span

    // Overlay: cursor + per-cell decorations (underline/strike) + hover. Rebuilt
    // every frame; sized for the worst case (two decorations per cell).
    const maxOverlay = cells * 2 + model.cols + 8;
    this.ovAB = new ArrayBuffer(maxOverlay * WORDS_PER_INSTANCE * 4);
    this.ovF = new Float32Array(this.ovAB);
    this.ovU = new Uint32Array(this.ovAB);
    this.ovBytes = new Uint8Array(this.ovAB);

    this._gridFull = true; // force a full grid rebuild + upload next render
    this._prevSel = null;
    this._gridUploaded = false;
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

  // Write one instance record (11 words) into `f`/`u` at word offset `o`.
  // `tex` null => background-only (no glyph); `fgP`/`bgP` are packed RGBA8.
  _writeInst(f, u, o, x, y, w, h, fgP, bgP, tex, tint) {
    f[o + F_RECT] = x; f[o + F_RECT + 1] = y; f[o + F_RECT + 2] = w; f[o + F_RECT + 3] = h;
    if (tex) {
      f[o + F_TEX] = tex.u0; f[o + F_TEX + 1] = tex.v0; f[o + F_TEX + 2] = tex.u1; f[o + F_TEX + 3] = tex.v1;
    } else {
      f[o + F_TEX] = -1; f[o + F_TEX + 1] = -1; f[o + F_TEX + 2] = -1; f[o + F_TEX + 3] = -1;
    }
    f[o + F_TINT] = tint;
    u[o + U_FG] = fgP;
    u[o + U_BG] = bgP;
  }

  // Append an overlay instance (cursor / decoration / hover) at `this._o`.
  _ov(x, y, w, h, fgP, bgP, tex, tint) {
    this._writeInst(this.ovF, this.ovU, this._o, x, y, w, h, fgP, bgP, tex, tint);
    this._o += WORDS_PER_INSTANCE;
  }

  render(model, dirtyRows, full, cursor, selection, hoverLink) {
    const gl = this.gl;
    const dpr = this.metrics.dpr;
    const cw = this.metrics.cellW * dpr;
    const ch = this.metrics.cellH * dpr;
    const cols = model.cols, rows = model.rows;
    const pal = this.palette;
    const bgRgb = pal.bgRgb;
    // Render-scoped context read by _genGridRow.
    this._cwd = cw;
    this._chd = ch;
    this._defBgP = packRGBA(bgRgb[0], bgRgb[1], bgRgb[2], 0);
    this._selArr = selection ? this._selCss255() : null; // [r,g,b] 0..255, a 0..1
    this._selObj = selection || null;
    this._dirtyAtlas = false;

    // --- Regenerate only the grid rows that changed. ---
    const doFull = full || this._gridFull || !this._gridUploaded;
    let minRow = rows, maxRow = -1;
    if (doFull) {
      for (let y = 0; y < rows; y++) this._genGridRow(model, y);
      minRow = 0; maxRow = rows - 1;
    } else {
      const dirty = new Set(dirtyRows);
      for (const y of this._selectionDirtyRows(selection)) dirty.add(y);
      for (const y of dirty) {
        if (y < 0 || y >= rows) continue;
        this._genGridRow(model, y);
        if (y < minRow) minRow = y;
        if (y > maxRow) maxRow = y;
      }
    }
    this._prevSel = selection
      ? { sx: selection.sx, sy: selection.sy, ex: selection.ex, ey: selection.ey }
      : null;

    // --- Overlay: cursor + decorations + hover, rebuilt every frame. ---
    this._o = 0;
    this._genOverlay(model, cw, ch, cursor, hoverLink);

    if (this._dirtyAtlas) this._uploadAtlas();

    // --- Upload grid (whole buffer on a full frame, else just the changed
    // row span via bufferSubData). ---
    gl.bindBuffer(gl.ARRAY_BUFFER, this.gridGlBuf);
    if (doFull) {
      gl.bufferData(gl.ARRAY_BUFFER, this.gridBytes, gl.DYNAMIC_DRAW);
      this._gridUploaded = true;
    } else if (maxRow >= minRow) {
      const off = minRow * this._rowBytes;
      const end = (maxRow + 1) * this._rowBytes;
      gl.bufferSubData(gl.ARRAY_BUFFER, off, this.gridBytes.subarray(off, end));
    }
    this._gridFull = false;

    // --- Draw. ---
    gl.clearColor(bgRgb[0] / 255, bgRgb[1] / 255, bgRgb[2] / 255, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.useProgram(this.prog);
    gl.uniform2f(this.loc.uInv, 2 / this.W, 2 / this.H);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.texture);
    gl.uniform1i(this.loc.uAtlas, 0);

    // Static unit quad (shared, non-instanced).
    gl.bindBuffer(gl.ARRAY_BUFFER, this.cornerBuf);
    gl.enableVertexAttribArray(this.loc.aCorner);
    gl.vertexAttribPointer(this.loc.aCorner, 2, gl.FLOAT, false, 0, 0);
    this.ext.vertexAttribDivisorANGLE(this.loc.aCorner, 0);

    // Grid: one instance per cell (empty cells are degenerate, discarded).
    gl.bindBuffer(gl.ARRAY_BUFFER, this.gridGlBuf);
    this._bindInstanceAttrs();
    this.ext.drawArraysInstancedANGLE(gl.TRIANGLES, 0, 6, cols * rows);

    // Overlay, on top.
    const nOv = this._o / WORDS_PER_INSTANCE;
    if (nOv > 0) {
      gl.bindBuffer(gl.ARRAY_BUFFER, this.ovGlBuf);
      gl.bufferData(gl.ARRAY_BUFFER, this.ovBytes.subarray(0, this._o * 4), gl.STREAM_DRAW);
      this._bindInstanceAttrs();
      this.ext.drawArraysInstancedANGLE(gl.TRIANGLES, 0, 6, nOv);
    }
  }

  // Rows whose selection membership changed since last frame (they must be
  // regenerated because selection is baked into the cell background).
  _selectionDirtyRows(cur) {
    const prev = this._prevSel;
    const same =
      (!prev && !cur) ||
      (prev && cur && prev.sx === cur.sx && prev.sy === cur.sy && prev.ex === cur.ex && prev.ey === cur.ey);
    if (same) return [];
    const out = [];
    if (prev) for (let y = prev.sy; y <= prev.ey; y++) out.push(y);
    if (cur) for (let y = cur.sy; y <= cur.ey; y++) out.push(y);
    return out;
  }

  // Fill row `y`'s grid slots (one instance per cell; empty cells become
  // degenerate zero-area instances). Selection is baked into the background.
  _genGridRow(model, y) {
    const cols = model.cols;
    const pal = this.palette;
    const cw = this._cwd, ch = this._chd, defBgP = this._defBgP;
    const sel = this._selArr, selection = this._selObj;
    const cpA = model.cp, fgA = model.fg, bgA = model.bg, flagsA = model.flags,
      graphemeA = model.grapheme;
    const f = this.gridF, u = this.gridU;
    const base = y * cols;
    const yc = y * ch;
    const selRange =
      sel && selection && y >= selection.sy && y <= selection.ey
        ? this._selSpan(selection, y, cols) : null;

    for (let x = 0; x < cols; x++) {
      const i = base + x;
      const o = i * WORDS_PER_INSTANCE;
      const flags = flagsA[i];
      if (flags & ATTR.WIDE_SPACER) { f[o + F_RECT + 2] = 0; f[o + F_RECT + 3] = 0; continue; }
      const inverse = (flags & ATTR.INVERSE) !== 0;
      const cp = cpA[i];
      const hasGlyph = cp !== 0x20 && cp !== 0 && !(flags & ATTR.INVISIBLE);
      const selected = selRange && x >= selRange[0] && x < selRange[1];

      let bgP, filled;
      if (inverse) {
        const c = pal.resolveRgb(fgA[i], true, false);
        bgP = packRGBA(c[0], c[1], c[2], 255); filled = true;
      } else if (bgA[i] >>> 24 !== 0) {
        const c = pal.resolveRgb(bgA[i], false, false);
        bgP = packRGBA(c[0], c[1], c[2], 255); filled = true;
      } else {
        bgP = defBgP; filled = false;
      }
      if (selected) {
        const sa = sel[3], ia = 1 - sa;
        bgP = packRGBA(
          ((bgP & 0xff) * ia + sel[0] * sa) | 0,
          (((bgP >> 8) & 0xff) * ia + sel[1] * sa) | 0,
          (((bgP >> 16) & 0xff) * ia + sel[2] * sa) | 0,
          255
        );
        filled = true;
      }

      let fgP = 0, glyph = null, tint = 1;
      if (hasGlyph) {
        const bold = (flags & ATTR.BOLD) !== 0;
        const fc = inverse
          ? pal.resolveRgb(bgA[i], false, false)
          : pal.resolveRgb(fgA[i], true, bold);
        fgP = packRGBA(fc[0], fc[1], fc[2], flags & ATTR.DIM ? 153 : 255);
        const cells = flags & ATTR.WIDE ? 2 : 1;
        const styleBits =
          (bold ? 1 : 0) | (flags & ATTR.ITALIC ? 2 : 0) | (isColorGlyph(cp) ? 4 : 0);
        const cluster = graphemeA[i] !== 0 ? model.clusterAt(i) : null;
        glyph = this._glyph(cp, cluster, styleBits, cells);
        tint = glyph.tint;
      }

      if (!filled && !hasGlyph) { f[o + F_RECT + 2] = 0; f[o + F_RECT + 3] = 0; continue; }
      const w = flags & ATTR.WIDE ? cw * 2 : cw;
      this._writeInst(f, u, o, x * cw, yc, w, ch, fgP, bgP, glyph, tint);
    }
  }

  // Cursor + underline/strike + hover-link, drawn over the grid. Decorations are
  // thin background-only instances (exactly as the grid pass drew them before),
  // so scanning for the rare decorated cell is cheap and keeps them pixel-exact.
  _genOverlay(model, cw, ch, cursor, hoverLink) {
    const cols = model.cols, rows = model.rows;
    const pal = this.palette;
    const flagsA = model.flags, fgA = model.fg, bgA = model.bg, cpA = model.cp;
    const t = Math.max(1, Math.round(this.metrics.dpr));
    const DECO = ATTR.UNDERLINE | ATTR.STRIKETHROUGH;
    for (let y = 0; y < rows; y++) {
      const base = y * cols;
      const yc = y * ch;
      const hoverRow = hoverLink && hoverLink.y === y;
      for (let x = 0; x < cols; x++) {
        const i = base + x;
        const flags = flagsA[i];
        if (flags & ATTR.WIDE_SPACER) continue;
        const hovered = hoverRow && x >= hoverLink.x0 && x <= hoverLink.x1;
        if (!(flags & DECO) && !hovered) continue;
        const inverse = (flags & ATTR.INVERSE) !== 0;
        const cp = cpA[i];
        const hasGlyph = cp !== 0x20 && cp !== 0 && !(flags & ATTR.INVISIBLE);
        // Match the previous single-pass decoration colour exactly.
        const c = hasGlyph
          ? (inverse ? pal.resolveRgb(bgA[i], false, false) : pal.resolveRgb(fgA[i], true, (flags & ATTR.BOLD) !== 0))
          : pal.resolveRgb(fgA[i], true, false);
        const dcP = packRGBA(c[0], c[1], c[2], 255);
        const w = flags & ATTR.WIDE ? cw * 2 : cw;
        if (flags & ATTR.UNDERLINE || hovered) this._ov(x * cw, yc + ch - t * 2, w, t, 0, dcP, null, 0);
        if (flags & ATTR.STRIKETHROUGH) this._ov(x * cw, yc + ch * 0.55, w, t, 0, dcP, null, 0);
      }
    }
    if (cursor.show && cursor.y < rows) this._pushCursor(model, cursor, cw, ch);
  }

  _bindInstanceAttrs() {
    const gl = this.gl;
    const stride = WORDS_PER_INSTANCE * 4; // 44
    const F = gl.FLOAT, UB = gl.UNSIGNED_BYTE;
    this._bindInstanceAttr(this.loc.aRect, 4, F, false, stride, F_RECT * 4);
    this._bindInstanceAttr(this.loc.aTex, 4, F, false, stride, F_TEX * 4);
    this._bindInstanceAttr(this.loc.aTint, 1, F, false, stride, F_TINT * 4);
    this._bindInstanceAttr(this.loc.aFg, 4, UB, true, stride, U_FG * 4);
    this._bindInstanceAttr(this.loc.aBg, 4, UB, true, stride, U_BG * 4);
  }

  _bindInstanceAttr(loc, size, type, normalized, stride, offset) {
    const gl = this.gl;
    gl.enableVertexAttribArray(loc);
    gl.vertexAttribPointer(loc, size, type, normalized, stride, offset);
    this.ext.vertexAttribDivisorANGLE(loc, 1);
  }

  _pushCursor(model, cursor, cw, ch) {
    const i = model.index(cursor.x, cursor.y);
    const flags = model.flags[i];
    const w = flags & ATTR.WIDE ? cw * 2 : cw;
    const px = cursor.x * cw;
    const py = cursor.y * ch;
    const cP = packRGBA(...this._css2rgb255(this.palette.cursor), 255);
    const style = cursor.style || 'block';
    const dpr = this.metrics.dpr;
    if (!cursor.focused) {
      const t = Math.max(1, Math.round(dpr));
      this._ov(px, py, w, t, 0, cP, null, 0);
      this._ov(px, py + ch - t, w, t, 0, cP, null, 0);
      this._ov(px, py, t, ch, 0, cP, null, 0);
      this._ov(px + w - t, py, t, ch, 0, cP, null, 0);
    } else if (style === 'bar') {
      const t = Math.max(1, Math.round(2 * dpr));
      this._ov(px, py, t, ch, 0, cP, null, 0);
    } else if (style === 'underline') {
      const t = Math.max(1, Math.round(2 * dpr));
      this._ov(px, py + ch - t, w, t, 0, cP, null, 0);
    } else {
      // Block: fill the cell with the cursor color, then re-draw the glyph in
      // the accent color on top.
      const cp = model.cp[i];
      const cells = flags & ATTR.WIDE ? 2 : 1;
      if (cp !== 0x20 && cp !== 0) {
        const caP = packRGBA(...this._css2rgb255(this.palette.cursorAccent), 255);
        const styleBits =
          (flags & ATTR.BOLD ? 1 : 0) | (flags & ATTR.ITALIC ? 2 : 0) | (isColorGlyph(cp) ? 4 : 0);
        const cluster = model.grapheme[i] !== 0 ? model.clusterAt(i) : null;
        const g = this._glyph(cp, cluster, styleBits, cells);
        this._ov(px, py, w, ch, caP, cP, g, g.tint);
      } else {
        this._ov(px, py, w, ch, 0, cP, null, 0);
      }
    }
  }

  // Selection color as [r, g, b] (0..255) plus alpha (0..1).
  _selCss255() {
    const m = /rgba?\(([^)]+)\)/.exec(this.palette.selection);
    if (!m) return [102, 153, 255, 0.35];
    const p = m[1].split(',').map((s) => parseFloat(s));
    return [p[0] | 0, p[1] | 0, p[2] | 0, p[3] === undefined ? 1 : p[3]];
  }
  _selSpan(sel, y, cols) {
    if (sel.sy === sel.ey) return [sel.sx, sel.ex];
    if (y === sel.sy) return [sel.sx, cols];
    if (y === sel.ey) return [0, sel.ex];
    return [0, cols];
  }
  // CSS color (#hex or rgb()) as [r, g, b] in 0..255.
  _css2rgb255(css) {
    if (css[0] === '#') {
      let h = css.slice(1);
      if (h.length === 3) h = h[0] + h[0] + h[1] + h[1] + h[2] + h[2];
      const n = parseInt(h, 16);
      return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
    }
    const m = /rgba?\(([^)]+)\)/.exec(css);
    if (m) {
      const p = m[1].split(',').map((s) => parseFloat(s));
      return [p[0] | 0, p[1] | 0, p[2] | 0];
    }
    return [255, 255, 255];
  }

  dispose() {
    const gl = this.gl;
    gl.deleteBuffer(this.gridGlBuf);
    gl.deleteBuffer(this.ovGlBuf);
    gl.deleteBuffer(this.cornerBuf);
    gl.deleteTexture(this.texture);
    gl.deleteProgram(this.prog);
    this.canvas.remove();
  }
}
