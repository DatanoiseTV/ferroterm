// WebGL renderer. Rasterizes each unique glyph once into a texture atlas, then
// draws the whole grid as batched quads: one solid-color quad per cell
// background and one textured quad per glyph, in a single draw call. Text glyphs
// are stored as alpha masks and tinted by the cell's foreground; emoji are
// stored in full color and drawn untinted.
//
// Targets WebGL1 for broad compatibility; falls back gracefully (the host
// catches a throw from the constructor and uses the Canvas2D renderer).

import { ATTR } from './palette.js';

const VERT_SRC = `
attribute vec2 aPos;      // clip space
attribute vec4 aColor;    // rgba 0..1
attribute vec2 aTex;      // atlas uv, or (-1,-1) for solid
attribute float aTint;    // 1 = alpha-mask tint, 0 = color glyph
varying vec4 vColor;
varying vec2 vTex;
varying float vTint;
void main() {
  gl_Position = vec4(aPos, 0.0, 1.0);
  vColor = aColor;
  vTex = aTex;
  vTint = aTint;
}`;

const FRAG_SRC = `
precision mediump float;
varying vec4 vColor;
varying vec2 vTex;
varying float vTint;
uniform sampler2D uAtlas;
void main() {
  if (vTex.x < 0.0) {
    gl_FragColor = vColor;               // solid quad (background / cursor)
  } else {
    vec4 t = texture2D(uAtlas, vTex);
    if (vTint > 0.5) {
      gl_FragColor = vec4(vColor.rgb, t.a * vColor.a);  // tinted alpha mask
    } else {
      gl_FragColor = t;                  // color glyph (emoji)
    }
  }
}`;

const FLOATS_PER_VERT = 9; // pos(2) color(4) tex(2) tint(1)
const VERTS_PER_QUAD = 6;

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

    const gl = this.canvas.getContext('webgl', { alpha: false, antialias: false }) ||
      this.canvas.getContext('experimental-webgl', { alpha: false, antialias: false });
    if (!gl) {
      this.canvas.remove();
      throw new Error('WebGL not available');
    }
    this.gl = gl;
    this._initGL();

    // Offscreen atlas canvas.
    this.atlasCanvas = document.createElement('canvas');
    this.atlasCtx = this.atlasCanvas.getContext('2d', { willReadFrequently: false });
    this.glyphCache = new Map();
    this.verts = new Float32Array(0);
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
      aPos: gl.getAttribLocation(prog, 'aPos'),
      aColor: gl.getAttribLocation(prog, 'aColor'),
      aTex: gl.getAttribLocation(prog, 'aTex'),
      aTint: gl.getAttribLocation(prog, 'aTint'),
      uAtlas: gl.getUniformLocation(prog, 'uAtlas'),
    };
    this.buffer = gl.createBuffer();
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

    // Atlas: integer device-pixel cell, packed into a 2048 texture.
    this.gcw = Math.ceil(cellW * dpr);
    this.gch = Math.ceil(cellH * dpr);
    this.atlasSize = 2048;
    this.cols_atlas = Math.max(1, Math.floor(this.atlasSize / this.gcw));
    this.atlasCanvas.width = this.atlasSize;
    this.atlasCanvas.height = this.atlasSize;
    this._resetAtlas();

    // Vertex scratch: max 2 quads/cell + cursor.
    const maxQuads = model.cols * model.rows * 2 + 8;
    this.verts = new Float32Array(maxQuads * VERTS_PER_QUAD * FLOATS_PER_VERT);
  }

  _resetAtlas() {
    const ctx = this.atlasCtx;
    ctx.clearRect(0, 0, this.atlasSize, this.atlasSize);
    const { fontFamily, fontSize, dpr, baseline } = this.metrics;
    ctx.textBaseline = 'alphabetic';
    this._atlasFont = { fontFamily, fontSize: fontSize * dpr, baseline: baseline * dpr };
    this.glyphCache.clear();
    this.nextSlot = 0;
    this._uploadAtlas();
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

  _glyph(cp, bold, italic, color) {
    const key = (cp << 3) | (bold ? 1 : 0) | (italic ? 2 : 0) | (color ? 4 : 0);
    let g = this.glyphCache.get(key);
    if (g) return g;

    const slotsPerRow = this.cols_atlas;
    const maxSlots = slotsPerRow * Math.floor(this.atlasSize / this.gch);
    if (this.nextSlot >= maxSlots) {
      // Atlas full: reset (rare; only with a huge glyph variety).
      this._resetAtlas();
    }
    const slot = this.nextSlot++;
    const col = slot % slotsPerRow;
    const row = Math.floor(slot / slotsPerRow);
    const x = col * this.gcw;
    const y = row * this.gch;

    const ctx = this.atlasCtx;
    ctx.clearRect(x, y, this.gcw, this.gch);
    let font = '';
    if (italic) font += 'italic ';
    if (bold) font += 'bold ';
    font += `${this._atlasFont.fontSize}px ${this._atlasFont.fontFamily}`;
    ctx.font = font;
    ctx.fillStyle = color ? '#ffffff' : '#ffffff'; // color glyphs render native
    ctx.fillText(String.fromCodePoint(cp), x, y + this._atlasFont.baseline);

    g = {
      u0: x / this.atlasSize,
      v0: y / this.atlasSize,
      u1: (x + this.gcw) / this.atlasSize,
      v1: (y + this.gch) / this.atlasSize,
      tint: color ? 0 : 1,
    };
    this.glyphCache.set(key, g);
    this._dirtyAtlas = true;
    return g;
  }

  // Append one quad's six vertices to the scratch buffer at `this._o`.
  _quad(x, y, w, h, r, g, b, a, tex, tint) {
    const v = this.verts;
    let o = this._o;
    const x0 = (x / this.W) * 2 - 1;
    const y0 = 1 - (y / this.H) * 2;
    const x1 = ((x + w) / this.W) * 2 - 1;
    const y1 = 1 - ((y + h) / this.H) * 2;
    const tx0 = tex ? tex.u0 : -1;
    const ty0 = tex ? tex.v0 : -1;
    const tx1 = tex ? tex.u1 : -1;
    const ty1 = tex ? tex.v1 : -1;
    const corners = [
      [x0, y0, tx0, ty0], [x1, y0, tx1, ty0], [x0, y1, tx0, ty1],
      [x1, y0, tx1, ty0], [x1, y1, tx1, ty1], [x0, y1, tx0, ty1],
    ];
    for (const c of corners) {
      v[o++] = c[0]; v[o++] = c[1];
      v[o++] = r; v[o++] = g; v[o++] = b; v[o++] = a;
      v[o++] = c[2]; v[o++] = c[3];
      v[o++] = tint;
    }
    this._o = o;
  }

  render(model, _dirtyRows, _full, cursor, selection, hoverLink) {
    const gl = this.gl;
    this._o = 0;
    this._dirtyAtlas = false;

    const dpr = this.metrics.dpr;
    const cw = this.metrics.cellW * dpr;
    const ch = this.metrics.cellH * dpr;

    // Pass 1: cell backgrounds.
    for (let y = 0; y < model.rows; y++) {
      for (let x = 0; x < model.cols; x++) {
        const i = model.index(x, y);
        const flags = model.flags[i];
        if (flags & ATTR.WIDE_SPACER) continue;
        const inverse = (flags & ATTR.INVERSE) !== 0;
        const w = flags & ATTR.WIDE ? cw * 2 : cw;
        const rgb = inverse
          ? this.palette.resolveRgb(model.fg[i], true, false)
          : this.palette.resolveRgb(model.bg[i], false, false);
        this._quad(x * cw, y * ch, w, ch, rgb[0] / 255, rgb[1] / 255, rgb[2] / 255, 1, null, -1);
      }
    }

    // Pass 1b: selection overlay (translucent).
    if (selection) {
      const sc = this._selCss();
      for (let y = selection.sy; y <= selection.ey && y < model.rows; y++) {
        const [x0, x1] = this._selSpan(selection, y, model.cols);
        if (x1 > x0) {
          this._quad(x0 * cw, y * ch, (x1 - x0) * cw, ch, sc[0], sc[1], sc[2], sc[3], null, -1);
        }
      }
    }

    // Pass 2: glyphs.
    for (let y = 0; y < model.rows; y++) {
      for (let x = 0; x < model.cols; x++) {
        const i = model.index(x, y);
        const flags = model.flags[i];
        if (flags & (ATTR.WIDE_SPACER | ATTR.INVISIBLE)) continue;
        const cp = model.cp[i];
        if (cp === 0x20 || cp === 0) continue;
        const bold = (flags & ATTR.BOLD) !== 0;
        const italic = (flags & ATTR.ITALIC) !== 0;
        const inverse = (flags & ATTR.INVERSE) !== 0;
        const color = isColorGlyph(cp);
        const g = this._glyph(cp, bold, italic, color);
        const fg = inverse
          ? this.palette.resolveRgb(model.bg[i], false, false)
          : this.palette.resolveRgb(model.fg[i], true, bold);
        const a = flags & ATTR.DIM ? 0.6 : 1;
        const w = flags & ATTR.WIDE ? cw * 2 : cw;
        this._quad(x * cw, y * ch, w, ch, fg[0] / 255, fg[1] / 255, fg[2] / 255, a, g, g.tint);
        // Underline / hover-link / strikethrough as thin quads.
        const hovered = hoverLink && hoverLink.y === y && x >= hoverLink.x0 && x <= hoverLink.x1;
        const t = Math.max(1, Math.round(dpr));
        if (flags & ATTR.UNDERLINE || hovered) {
          this._quad(x * cw, y * ch + ch - t * 2, w, t, fg[0] / 255, fg[1] / 255, fg[2] / 255, a, null, -1);
        }
        if (flags & ATTR.STRIKETHROUGH) {
          this._quad(x * cw, y * ch + ch * 0.55, w, t, fg[0] / 255, fg[1] / 255, fg[2] / 255, a, null, -1);
        }
      }
    }

    // Cursor.
    if (cursor.show && cursor.y < model.rows) {
      this._pushCursor(model, cursor, cw, ch);
    }

    if (this._dirtyAtlas) this._uploadAtlas();

    const o = this._o;
    gl.clearColor(
      this.palette.bgRgb[0] / 255,
      this.palette.bgRgb[1] / 255,
      this.palette.bgRgb[2] / 255,
      1
    );
    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.useProgram(this.prog);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.buffer);
    gl.bufferData(gl.ARRAY_BUFFER, this.verts.subarray(0, o), gl.STREAM_DRAW);
    const stride = FLOATS_PER_VERT * 4;
    gl.enableVertexAttribArray(this.loc.aPos);
    gl.vertexAttribPointer(this.loc.aPos, 2, gl.FLOAT, false, stride, 0);
    gl.enableVertexAttribArray(this.loc.aColor);
    gl.vertexAttribPointer(this.loc.aColor, 4, gl.FLOAT, false, stride, 8);
    gl.enableVertexAttribArray(this.loc.aTex);
    gl.vertexAttribPointer(this.loc.aTex, 2, gl.FLOAT, false, stride, 24);
    gl.enableVertexAttribArray(this.loc.aTint);
    gl.vertexAttribPointer(this.loc.aTint, 1, gl.FLOAT, false, stride, 32);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.texture);
    gl.uniform1i(this.loc.uAtlas, 0);
    gl.drawArrays(gl.TRIANGLES, 0, o / FLOATS_PER_VERT);
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
      this._quad(px, py, w, t, c[0], c[1], c[2], 1, null, -1);
      this._quad(px, py + ch - t, w, t, c[0], c[1], c[2], 1, null, -1);
      this._quad(px, py, t, ch, c[0], c[1], c[2], 1, null, -1);
      this._quad(px + w - t, py, t, ch, c[0], c[1], c[2], 1, null, -1);
    } else if (style === 'bar') {
      this._quad(px, py, Math.max(1, Math.round(2 * dpr)), ch, c[0], c[1], c[2], 1, null, -1);
    } else if (style === 'underline') {
      const t = Math.max(1, Math.round(2 * dpr));
      this._quad(px, py + ch - t, w, t, c[0], c[1], c[2], 1, null, -1);
    } else {
      this._quad(px, py, w, ch, c[0], c[1], c[2], 1, null, -1);
      const cp = model.cp[i];
      if (cp !== 0x20 && cp !== 0) {
        const ca = this._cursorAccentRgb();
        const g = this._glyph(cp, (flags & ATTR.BOLD) !== 0, (flags & ATTR.ITALIC) !== 0, false);
        this._quad(px, py, w, ch, ca[0], ca[1], ca[2], 1, g, 1);
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
    // selection is an rgba() string; parse to [r,g,b,a] 0..1.
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
    gl.deleteBuffer(this.buffer);
    gl.deleteTexture(this.texture);
    gl.deleteProgram(this.prog);
    this.canvas.remove();
  }
}
