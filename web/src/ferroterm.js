// Ferroterm — the public web component.
//
// Orchestrates the WASM core, a pluggable renderer (Canvas2D or WebGL), input
// handling, links, selection and scrollback into a single embeddable terminal.
//
//   import { Ferroterm } from 'ferroterm';
//   const term = await Ferroterm.create(document.getElementById('term'), {
//     cols: 80, rows: 24, renderer: 'webgl',
//   });
//   term.onData(bytes => socket.send(bytes));   // user input -> PTY
//   socket.onmessage = e => term.write(new Uint8Array(e.data)); // PTY -> screen

import init, { Terminal as WasmTerminal } from '../pkg/ferroterm_wasm.js';
import { Palette, DEFAULT_THEME } from './palette.js';
import { GridModel } from './model.js';
import { CanvasRenderer } from './renderer-canvas.js';
import { WebGLRenderer } from './renderer-webgl.js';
import { KEY, modMask } from './keycodes.js';
import { linkAt } from './links.js';

const DEFAULTS = {
  cols: 80,
  rows: 24,
  scrollback: 2000,
  fontFamily: 'Menlo, Monaco, "DejaVu Sans Mono", "Cascadia Code", Consolas, monospace',
  fontSize: 14,
  lineHeight: 1.2,
  renderer: 'webgl', // 'webgl' | 'canvas'
  theme: DEFAULT_THEME,
  cursorStyle: 'block', // 'block' | 'bar' | 'underline'
  cursorBlink: true,
  scrollSensitivity: 3,
};

let wasmReady = null;

/** Initialize the WASM module once. `wasmUrl` overrides the default location. */
export function initWasm(wasmUrl) {
  if (!wasmReady) {
    wasmReady = wasmUrl ? init(wasmUrl) : init();
  }
  return wasmReady;
}

export class Ferroterm {
  /** Async factory: initializes WASM, then constructs the terminal. */
  static async create(container, options = {}) {
    await initWasm(options.wasmUrl);
    return new Ferroterm(container, options);
  }

  constructor(container, options = {}) {
    this.opts = { ...DEFAULTS, ...options };
    this.container = container;
    this.container.classList.add('ferroterm');
    this._encoder = new TextEncoder();
    this._decoder = new TextDecoder();

    this.palette = new Palette(this.opts.theme);
    this.term = new WasmTerminal(this.opts.cols, this.opts.rows, this.opts.scrollback);
    this.model = new GridModel(this.opts.cols, this.opts.rows);

    this._dataCbs = [];
    this._titleCbs = [];
    this._bellCbs = [];
    this._resizeCbs = [];
    this._lastBell = 0;
    this._focused = false;
    this._cursorOn = true;
    this._selection = null;
    this._selecting = false;
    this._selAnchor = null;
    this._hoverLink = null;
    this._renderScheduled = false;
    this._forceNext = true;

    this._measure();
    this._buildDom();
    this._makeRenderer(this.opts.renderer);
    this._bindInput();
    this._startBlink();
    this._scheduleRender(true);
    this._observeResize();
  }

  // --- public API ---------------------------------------------------------

  /** Feed bytes (Uint8Array) or a string received from the host / PTY. */
  write(data) {
    if (typeof data === 'string') {
      this.term.feedStr(data);
    } else {
      this.term.feed(data);
    }
    this._drainOutput();
    this._maybeBell();
    this._maybeTitle();
    this._scheduleRender();
  }

  onData(cb) {
    this._dataCbs.push(cb);
    return () => this._off(this._dataCbs, cb);
  }
  onTitleChange(cb) {
    this._titleCbs.push(cb);
    return () => this._off(this._titleCbs, cb);
  }
  onBell(cb) {
    this._bellCbs.push(cb);
    return () => this._off(this._bellCbs, cb);
  }
  onResize(cb) {
    this._resizeCbs.push(cb);
    return () => this._off(this._resizeCbs, cb);
  }

  /** Programmatically resize the grid to `cols` x `rows`. */
  resize(cols, rows) {
    cols = Math.max(1, cols | 0);
    rows = Math.max(1, rows | 0);
    if (cols === this.model.cols && rows === this.model.rows) return;
    this.term.resize(cols, rows);
    this.model.resize(cols, rows);
    this.renderer.resize(this.model, this.metrics);
    this._forceNext = true;
    this._scheduleRender(true);
    for (const cb of this._resizeCbs) cb(cols, rows);
  }

  /** Resize to fill the container based on the measured cell size. */
  fit() {
    const rect = this.container.getBoundingClientRect();
    const cols = Math.max(1, Math.floor(rect.width / this.metrics.cellW));
    const rows = Math.max(1, Math.floor(rect.height / this.metrics.cellH));
    this.resize(cols, rows);
  }

  focus() {
    this._input.focus();
  }
  blur() {
    this._input.blur();
  }

  /** Switch renderer backend at runtime ('canvas' | 'webgl'). */
  setRenderer(kind) {
    if (this.renderer && this.renderer.constructor.name.toLowerCase().includes(kind)) return;
    const old = this.renderer;
    this._makeRenderer(kind);
    if (old) old.dispose();
    this._forceNext = true;
    this._scheduleRender(true);
  }

  get rendererName() {
    return this.renderer ? this.renderer.constructor.name : null;
  }

  get cols() {
    return this.model.cols;
  }
  get rows() {
    return this.model.rows;
  }

  setTheme(theme) {
    this.palette.setTheme(theme);
    this._forceNext = true;
    this._scheduleRender(true);
  }

  /** The current selection text, or ''. */
  getSelection() {
    if (!this._selection) return '';
    return this._selectionText(this._selection);
  }

  clearSelection() {
    if (this._selection) {
      this._selection = null;
      this._forceNext = true;
      this._scheduleRender(true);
    }
  }

  dispose() {
    this._disposed = true;
    if (this._blinkTimer) clearInterval(this._blinkTimer);
    if (this._resizeObserver) this._resizeObserver.disconnect();
    this.renderer.dispose();
    this._input.remove();
  }

  // --- rendering ----------------------------------------------------------

  _measure() {
    const { fontFamily, fontSize, lineHeight } = this.opts;
    const c = document.createElement('canvas').getContext('2d');
    c.font = `${fontSize}px ${fontFamily}`;
    const m = c.measureText('M');
    const cellW = Math.max(1, Math.ceil(m.width));
    const ascent = m.actualBoundingBoxAscent || fontSize * 0.75;
    const descent = m.actualBoundingBoxDescent || fontSize * 0.25;
    const cellH = Math.max(1, Math.ceil(fontSize * lineHeight));
    const baseline = Math.round(ascent + (cellH - (ascent + descent)) / 2);
    this.metrics = {
      cellW,
      cellH,
      baseline,
      fontFamily,
      fontSize,
      dpr: window.devicePixelRatio || 1,
    };
  }

  _buildDom() {
    const c = this.container;
    if (getComputedStyle(c).position === 'static') c.style.position = 'relative';
    c.style.overflow = 'hidden';
    c.style.background = this.palette.bg;
    // Hidden textarea captures keyboard + IME composition + paste.
    const ta = document.createElement('textarea');
    ta.className = 'ft-input';
    ta.setAttribute('autocorrect', 'off');
    ta.setAttribute('autocapitalize', 'off');
    ta.setAttribute('spellcheck', 'false');
    Object.assign(ta.style, {
      position: 'absolute',
      opacity: '0',
      left: '0',
      top: '0',
      width: '1px',
      height: '1px',
      padding: '0',
      border: '0',
      margin: '0',
      resize: 'none',
      outline: 'none',
      overflow: 'hidden',
      zIndex: '-5',
    });
    c.appendChild(ta);
    this._input = ta;
  }

  _makeRenderer(kind) {
    let R = kind === 'canvas' ? CanvasRenderer : WebGLRenderer;
    try {
      this.renderer = new R(this.container, this.metrics, this.palette);
    } catch (e) {
      // WebGL unavailable -> fall back to Canvas2D.
      console.warn('ferroterm: renderer', kind, 'failed, falling back to canvas:', e.message);
      this.renderer = new CanvasRenderer(this.container, this.metrics, this.palette);
    }
    this.renderer.resize(this.model, this.metrics);
    this.renderer.element.style.cursor = 'text';
  }

  _scheduleRender() {
    if (this._renderScheduled || this._disposed) return;
    this._renderScheduled = true;
    requestAnimationFrame(() => {
      this._renderScheduled = false;
      this._frame();
    });
  }

  _frame() {
    const snap = this.term.snapshot(this._forceNext);
    this._forceNext = false;
    const { dirtyRows, full } = this.model.applySnapshot(snap);
    if (full) {
      // Dimensions changed underneath us (rare); make renderer match.
      this.renderer.resize(this.model, this.metrics);
    }
    const blink = this.opts.cursorBlink && this.model.cursorBlink;
    const cursor = {
      x: this.model.cursorX,
      y: this.model.cursorY,
      show: this.model.cursorVisible && this.model.cursorOnScreen && (!blink || this._cursorOn),
      style: this.opts.cursorStyle,
      focused: this._focused,
    };
    this.renderer.render(this.model, dirtyRows, full, cursor, this._selection, this._hoverLink);
  }

  _startBlink() {
    this._blinkTimer = setInterval(() => {
      if (!this.opts.cursorBlink) return;
      this._cursorOn = !this._cursorOn;
      this._scheduleRender();
    }, 530);
  }

  _observeResize() {
    if (typeof ResizeObserver === 'undefined') return;
    this._resizeObserver = new ResizeObserver(() => {
      if (this.opts.autoFit !== false) this.fit();
    });
    this._resizeObserver.observe(this.container);
  }

  // --- host output --------------------------------------------------------

  _drainOutput() {
    const out = this.term.takeOutput();
    if (out && out.length) this._emitData(out);
  }
  _emitData(bytes) {
    for (const cb of this._dataCbs) cb(bytes);
  }
  _maybeBell() {
    const n = this.term.bellCount();
    if (n !== this._lastBell) {
      this._lastBell = n;
      for (const cb of this._bellCbs) cb();
    }
  }
  _maybeTitle() {
    if (this.term.titleChanged()) {
      const t = this.term.title();
      for (const cb of this._titleCbs) cb(t);
    }
  }

  _off(arr, cb) {
    const i = arr.indexOf(cb);
    if (i >= 0) arr.splice(i, 1);
  }

  // --- input --------------------------------------------------------------

  _bindInput() {
    const ta = this._input;
    ta.addEventListener('focus', () => {
      this._focused = true;
      this._scheduleRender();
    });
    ta.addEventListener('blur', () => {
      this._focused = false;
      this._scheduleRender();
    });
    ta.addEventListener('keydown', (e) => this._onKeyDown(e));
    ta.addEventListener('compositionstart', () => {
      this._composing = true;
    });
    ta.addEventListener('compositionend', (e) => {
      this._composing = false;
      if (e.data) this._sendText(e.data);
      ta.value = '';
    });
    ta.addEventListener('input', (e) => {
      // Non-composition text input (e.g. dictation) -> send raw.
      if (this._composing) return;
      if (ta.value) {
        this._sendText(ta.value);
        ta.value = '';
      }
      void e;
    });
    ta.addEventListener('paste', (e) => this._onPaste(e));

    // Mouse & selection on the renderer surface.
    const el = this.container;
    el.addEventListener('mousedown', (e) => this._onMouseDown(e));
    window.addEventListener('mousemove', (e) => this._onMouseMove(e));
    window.addEventListener('mouseup', (e) => this._onMouseUp(e));
    el.addEventListener('wheel', (e) => this._onWheel(e), { passive: false });
    el.addEventListener('click', (e) => this._onClick(e));
  }

  _onKeyDown(e) {
    if (this._composing) return;
    const mods = modMask(e);
    const key = e.key;

    // Copy / paste shortcuts.
    const primary = e.metaKey || (e.ctrlKey && e.shiftKey);
    if (primary && (key === 'c' || key === 'C') && this._selection) {
      this._copySelection();
      e.preventDefault();
      return;
    }
    if (primary && (key === 'v' || key === 'V')) {
      // Let the paste event fire; also try async clipboard.
      this._tryClipboardPaste();
      e.preventDefault();
      return;
    }

    // Special keys.
    const code = KEY[key];
    if (code !== undefined) {
      const bytes = this.term.key(code, mods);
      if (bytes.length) {
        this._emitData(bytes);
        this._scrollToBottomOnInput();
        e.preventDefault();
      }
      return;
    }

    // Printable single character (including Ctrl/Alt combos).
    if (key.length === 1 || key.codePointAt(0) > 0xffff) {
      const cp = key.codePointAt(0);
      const bytes = this.term.char(cp, mods);
      if (bytes.length) {
        this._emitData(bytes);
        this._scrollToBottomOnInput();
        e.preventDefault();
      }
    }
  }

  _sendText(text) {
    // Encode each code point through the core so Ctrl/Alt folding is uniform;
    // here there are no modifiers (already-composed text).
    const bytes = this._encoder.encode(text);
    this._emitData(bytes);
    this._scrollToBottomOnInput();
  }

  _scrollToBottomOnInput() {
    if (this.term.displayOffset() !== 0) {
      this.term.scrollToBottom();
      this._forceNext = true;
      this._scheduleRender(true);
    }
  }

  _onPaste(e) {
    const text = e.clipboardData && e.clipboardData.getData('text');
    if (text) {
      this._pasteText(text);
      e.preventDefault();
    }
  }

  async _tryClipboardPaste() {
    if (navigator.clipboard && navigator.clipboard.readText) {
      try {
        const text = await navigator.clipboard.readText();
        if (text) this._pasteText(text);
      } catch {
        /* clipboard blocked; the paste event path still works */
      }
    }
  }

  _pasteText(text) {
    text = text.replace(/\r\n/g, '\r').replace(/\n/g, '\r');
    let bytes;
    if (this.term.bracketedPaste()) {
      const payload = this._encoder.encode(text);
      bytes = new Uint8Array(payload.length + 12);
      bytes.set(this._encoder.encode('\x1b[200~'), 0);
      bytes.set(payload, 6);
      bytes.set(this._encoder.encode('\x1b[201~'), 6 + payload.length);
    } else {
      bytes = this._encoder.encode(text);
    }
    this._emitData(bytes);
  }

  async _copySelection() {
    const text = this.getSelection();
    if (!text) return;
    if (navigator.clipboard && navigator.clipboard.writeText) {
      try {
        await navigator.clipboard.writeText(text);
      } catch {
        /* ignore */
      }
    }
  }

  // --- mouse / selection / links ------------------------------------------

  _cellAt(e) {
    const rect = this.renderer.element.getBoundingClientRect();
    const x = Math.floor((e.clientX - rect.left) / this.metrics.cellW);
    const y = Math.floor((e.clientY - rect.top) / this.metrics.cellH);
    return {
      x: Math.max(0, Math.min(this.model.cols - 1, x)),
      y: Math.max(0, Math.min(this.model.rows - 1, y)),
    };
  }

  _onMouseDown(e) {
    this.focus();
    const { x, y } = this._cellAt(e);
    // App mouse reporting (unless Shift bypasses it for local selection).
    if (this.term.mouseMode() !== 0 && !e.shiftKey) {
      const bytes = this.term.mouse(this._btn(e), x, y, 0, modMask(e));
      if (bytes.length) this._emitData(bytes);
      return;
    }
    if (e.button !== 0) return;
    this._selecting = true;
    this._selAnchor = { x, y };
    this._selection = { sx: x, sy: y, ex: x, ey: y };
    this._forceNext = true;
    this._scheduleRender(true);
    e.preventDefault();
  }

  _onMouseMove(e) {
    if (this._selecting) {
      const { x, y } = this._cellAt(e);
      this._selection = this._normalizeSel(this._selAnchor, { x: x + 1, y });
      this._forceNext = true;
      this._scheduleRender(true);
      return;
    }
    // Link hover.
    if (e.target === this.renderer.element || this.container.contains(e.target)) {
      const { x, y } = this._cellAt(e);
      const link = linkAt(this.model, x, y, (id) => this.term.linkUri(id));
      const changed =
        !!link !== !!this._hoverLink ||
        (link && this._hoverLink && (link.y !== this._hoverLink.y || link.x0 !== this._hoverLink.x0));
      if (changed) {
        this._hoverLink = link;
        this.renderer.element.style.cursor = link ? 'pointer' : 'text';
        this._forceNext = true;
        this._scheduleRender(true);
      }
    }
  }

  _onMouseUp(e) {
    if (this._selecting) {
      this._selecting = false;
      const sel = this.getSelection();
      if (sel && this.opts.copyOnSelect) this._copySelection();
    }
    if (this.term.mouseMode() !== 0 && !e.shiftKey) {
      const { x, y } = this._cellAt(e);
      const bytes = this.term.mouse(this._btn(e), x, y, 1, modMask(e));
      if (bytes.length) this._emitData(bytes);
    }
  }

  _onClick(e) {
    if (this._hoverLink) {
      const uri = this._hoverLink.uri;
      if (this.opts.onLink) {
        this.opts.onLink(uri, e);
      } else {
        window.open(uri, '_blank', 'noopener,noreferrer');
      }
    }
  }

  _onWheel(e) {
    // In app mouse mode, forward wheel as buttons 64/65.
    if (this.term.mouseMode() !== 0 && !e.shiftKey) {
      const { x, y } = this._cellAt(e);
      const btn = e.deltaY < 0 ? 64 : 65;
      const bytes = this.term.mouse(btn, x, y, 0, modMask(e));
      if (bytes.length) {
        this._emitData(bytes);
        e.preventDefault();
      }
      return;
    }
    const lines = Math.sign(e.deltaY) * this.opts.scrollSensitivity;
    this.term.scrollLines(lines);
    this._forceNext = true;
    this._scheduleRender(true);
    e.preventDefault();
  }

  _btn(e) {
    return e.button === 1 ? 1 : e.button === 2 ? 2 : 0;
  }

  _normalizeSel(a, b) {
    // a is a cell {x,y}; b is an end {x (exclusive), y}.
    if (a.y < b.y || (a.y === b.y && a.x <= b.x)) {
      return { sx: a.x, sy: a.y, ex: b.x, ey: b.y };
    }
    return { sx: b.x, sy: b.y, ex: a.x + 1, ey: a.y };
  }

  _selectionText(sel) {
    let out = '';
    for (let y = sel.sy; y <= sel.ey; y++) {
      const full = this.model.rowText(y);
      let x0 = 0;
      let x1 = this.model.cols;
      if (sel.sy === sel.ey) {
        x0 = sel.sx;
        x1 = sel.ex;
      } else if (y === sel.sy) {
        x0 = sel.sx;
      } else if (y === sel.ey) {
        x1 = sel.ex;
      }
      out += full.slice(x0, x1).replace(/\s+$/, '');
      if (y !== sel.ey) out += '\n';
    }
    return out;
  }
}

export { Palette, DEFAULT_THEME };
export default Ferroterm;
