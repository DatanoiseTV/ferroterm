// Ferroterm — the public web component.
//
// Architecture: an **engine** (the WASM core + model + callbacks + input
// encoding) is always alive and cheap. A **view** (renderer + DOM + input
// capture) is attached only while the terminal is visible. Detaching a view
// frees its WebGL context and stops its render loop but preserves all state, so
// a host can keep hundreds of background terminals alive and only pay for the
// handful on screen — browsers cap live WebGL contexts (~16), so per-tab
// renderers would otherwise crash at scale.
//
//   const term = await Ferroterm.create(el, { renderer: 'webgl' });
//   term.onData(bytes => socket.send(bytes));
//   socket.onmessage = e => term.write(new Uint8Array(e.data));

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
  renderer: 'webgl',
  theme: DEFAULT_THEME,
  cursorStyle: 'block',
  cursorBlink: true,
  scrollSensitivity: 3,
  autoFit: true,
  copyOnSelect: false,
  rightClick: 'menu', // 'menu' | 'paste' | 'none'
};

let wasmReady = null;
export function initWasm(wasmUrl) {
  if (!wasmReady) wasmReady = wasmUrl ? init(wasmUrl) : init();
  return wasmReady;
}

// One shared blink timer drives every attached terminal, so N tabs don't spin
// up N intervals.
const blinkSubs = new Set();
let blinkTimer = null;
let blinkOn = true;
function blinkSubscribe(fn) {
  blinkSubs.add(fn);
  if (!blinkTimer) {
    blinkTimer = setInterval(() => {
      blinkOn = !blinkOn;
      for (const f of blinkSubs) f(blinkOn);
    }, 530);
  }
}
function blinkUnsubscribe(fn) {
  blinkSubs.delete(fn);
  if (blinkSubs.size === 0 && blinkTimer) {
    clearInterval(blinkTimer);
    blinkTimer = null;
    blinkOn = true;
  }
}

export class Ferroterm {
  /** Preload the WASM module (idempotent). */
  static ready(wasmUrl) {
    return initWasm(wasmUrl);
  }

  /** Async factory: initializes WASM, constructs the engine, attaches a view. */
  static async create(container, options = {}) {
    await initWasm(options.wasmUrl);
    const t = new Ferroterm(options);
    if (container) t.attachView(container);
    return t;
  }

  /** Construct the engine only (no view). Requires WASM to be ready. */
  constructor(options = {}) {
    this.opts = { ...DEFAULTS, ...options };
    this._encoder = new TextEncoder();
    this.palette = new Palette(this.opts.theme);
    this.term = new WasmTerminal(this.opts.cols, this.opts.rows, this.opts.scrollback);
    this.model = new GridModel(this.opts.cols, this.opts.rows);

    this._dataCbs = [];
    this._titleCbs = [];
    this._bellCbs = [];
    this._resizeCbs = [];
    this._lastBell = 0;
    this._title = '';

    // View state (populated by attachView).
    this.container = null;
    this.renderer = null;
    this._input = null;
    this._focused = false;
    this._cursorOn = true;
    this._selection = null;
    this._selecting = false;
    this._selAnchor = null;
    this._selMode = 'char';
    this._hoverLink = null;
    this._renderScheduled = false;
    this._forceNext = true;
    this._listeners = [];
    this._lastClick = { t: 0, x: -1, y: -1, n: 0 };

    this._measure();
  }

  get attached() {
    return !!this.renderer;
  }
  get cols() {
    return this.model.cols;
  }
  get rows() {
    return this.model.rows;
  }
  get rendererName() {
    return this.renderer ? this.renderer.constructor.name : null;
  }

  // --- engine API (works with or without a view) --------------------------

  write(data) {
    if (typeof data === 'string') this.term.feedStr(data);
    else this.term.feed(data);
    this._drainOutput();
    this._maybeBell();
    this._maybeTitle();
    if (this.attached) this._scheduleRender();
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

  get title() {
    return this._title;
  }

  resize(cols, rows) {
    cols = Math.max(1, cols | 0);
    rows = Math.max(1, rows | 0);
    if (cols === this.model.cols && rows === this.model.rows) return;
    this.term.resize(cols, rows);
    this.model.resize(cols, rows);
    if (this.renderer) this.renderer.resize(this.model, this.metrics);
    this._forceNext = true;
    if (this.attached) this._scheduleRender();
    for (const cb of this._resizeCbs) cb(cols, rows);
  }

  // --- view lifecycle -----------------------------------------------------

  /** Create the renderer + input capture inside `container` and start drawing. */
  attachView(container) {
    if (this.renderer) this.detachView();
    this.container = container;
    container.classList.add('ferroterm');
    this._measure(); // dpr may differ per display
    this._buildDom();
    this._makeRenderer(this.opts.renderer);
    this._bindInput();
    this._blinkCb = (on) => {
      this._cursorOn = on;
      if (this.opts.cursorBlink) this._scheduleRender();
    };
    blinkSubscribe(this._blinkCb);
    if (typeof ResizeObserver !== 'undefined') {
      this._resizeObserver = new ResizeObserver(() => {
        if (this.opts.autoFit) this.fit();
      });
      this._resizeObserver.observe(container);
    }
    this._forceNext = true;
    this._scheduleRender();
    if (document.hasFocus && document.hasFocus()) this.focus();
  }

  /** Tear down the renderer + input but keep all engine state. */
  detachView() {
    if (!this.renderer) return;
    if (this._blinkCb) blinkUnsubscribe(this._blinkCb);
    if (this._resizeObserver) this._resizeObserver.disconnect();
    this._unbindInput();
    this.renderer.dispose();
    this.renderer = null;
    if (this._menu) this._menu.remove();
    if (this._input) this._input.remove();
    this._input = null;
    this.container = null;
    this._renderScheduled = false;
  }

  fit() {
    if (!this.container) return;
    const rect = this.container.getBoundingClientRect();
    const cols = Math.max(1, Math.floor(rect.width / this.metrics.cellW));
    const rows = Math.max(1, Math.floor(rect.height / this.metrics.cellH));
    this.resize(cols, rows);
  }

  focus() {
    if (this._input) this._input.focus({ preventScroll: true });
  }
  blur() {
    if (this._input) this._input.blur();
  }

  setRenderer(kind) {
    if (!this.attached) {
      this.opts.renderer = kind;
      return;
    }
    if (this.rendererName && this.rendererName.toLowerCase().includes(kind)) return;
    const old = this.renderer;
    this._makeRenderer(kind);
    old.dispose();
    this._forceNext = true;
    this._scheduleRender();
  }

  setTheme(theme) {
    this.palette.setTheme(theme);
    if (this.container) this.container.style.background = this.palette.bg;
    this._forceNext = true;
    if (this.attached) this._scheduleRender();
  }

  setFontSize(px) {
    this.opts.fontSize = Math.max(6, px | 0);
    this._measure();
    if (this.renderer) this.renderer.resize(this.model, this.metrics);
    this._forceNext = true;
    if (this.attached) {
      this.fit();
      this._scheduleRender();
    }
  }

  clear() {
    this.write('\x1b[2J\x1b[3J\x1b[H');
  }

  getSelection() {
    return this._selection ? this._selectionText(this._selection) : '';
  }
  selectAll() {
    this._selection = { sx: 0, sy: 0, ex: this.model.cols, ey: this.model.rows - 1 };
    this._forceNext = true;
    this._scheduleRender();
  }
  clearSelection() {
    if (this._selection) {
      this._selection = null;
      this._forceNext = true;
      if (this.attached) this._scheduleRender();
    }
  }

  dispose() {
    this._disposed = true;
    this.detachView();
    this._dataCbs = [];
  }

  // --- measurement / DOM / renderer ---------------------------------------

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
    this.metrics = { cellW, cellH, baseline, fontFamily, fontSize, dpr: window.devicePixelRatio || 1 };
  }

  _buildDom() {
    const c = this.container;
    if (getComputedStyle(c).position === 'static') c.style.position = 'relative';
    c.style.overflow = 'hidden';
    c.style.background = this.palette.bg;
    const ta = document.createElement('textarea');
    ta.className = 'ft-input';
    ta.setAttribute('autocorrect', 'off');
    ta.setAttribute('autocapitalize', 'off');
    ta.setAttribute('spellcheck', 'false');
    ta.tabIndex = 0;
    Object.assign(ta.style, {
      position: 'absolute', opacity: '0', left: '0', top: '0', width: '2px', height: '2px',
      padding: '0', border: '0', margin: '0', resize: 'none', outline: 'none',
      overflow: 'hidden', zIndex: '1', whiteSpace: 'nowrap',
    });
    c.appendChild(ta);
    this._input = ta;
  }

  _makeRenderer(kind) {
    const R = kind === 'canvas' ? CanvasRenderer : WebGLRenderer;
    try {
      this.renderer = new R(this.container, this.metrics, this.palette);
    } catch (e) {
      console.warn('ferroterm: renderer', kind, 'failed, using canvas:', e.message);
      this.renderer = new CanvasRenderer(this.container, this.metrics, this.palette);
    }
    this.renderer.resize(this.model, this.metrics);
    this.renderer.element.style.cursor = 'text';
    // The canvas must sit under the (invisible) textarea for focus/paint order.
    this.renderer.element.style.position = 'absolute';
    this.renderer.element.style.left = '0';
    this.renderer.element.style.top = '0';
    this.renderer.element.style.zIndex = '0';
  }

  _scheduleRender() {
    if (this._renderScheduled || this._disposed || !this.renderer) return;
    this._renderScheduled = true;
    requestAnimationFrame(() => {
      this._renderScheduled = false;
      if (this.renderer) this._frame();
    });
  }

  _frame() {
    const snap = this.term.snapshot(this._forceNext);
    this._forceNext = false;
    const { dirtyRows, full } = this.model.applySnapshot(snap);
    if (full) this.renderer.resize(this.model, this.metrics);
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
      this._title = this.term.title();
      for (const cb of this._titleCbs) cb(this._title);
    }
  }
  _off(arr, cb) {
    const i = arr.indexOf(cb);
    if (i >= 0) arr.splice(i, 1);
  }

  // --- input binding ------------------------------------------------------

  _on(target, type, handler, opts) {
    target.addEventListener(type, handler, opts);
    this._listeners.push([target, type, handler, opts]);
  }
  _unbindInput() {
    for (const [t, type, h, o] of this._listeners) t.removeEventListener(type, h, o);
    this._listeners = [];
  }

  _bindInput() {
    const ta = this._input;
    const el = this.container;
    this._on(ta, 'focus', () => {
      this._focused = true;
      this._scheduleRender();
    });
    this._on(ta, 'blur', () => {
      this._focused = false;
      this._scheduleRender();
    });
    this._on(ta, 'keydown', (e) => this._onKeyDown(e));
    this._on(ta, 'compositionstart', () => (this._composing = true));
    this._on(ta, 'compositionend', (e) => {
      this._composing = false;
      if (e.data) this._sendText(e.data);
      ta.value = '';
    });
    this._on(ta, 'input', () => {
      if (this._composing) return;
      if (ta.value) {
        this._sendText(ta.value);
        ta.value = '';
      }
    });
    this._on(ta, 'paste', (e) => this._onPaste(e));

    // Focus on any pointer press in the terminal (fixes webview focus-on-open).
    this._on(el, 'pointerdown', () => this.focus());
    this._on(el, 'mousedown', (e) => this._onMouseDown(e));
    this._on(window, 'mousemove', (e) => this._onMouseMove(e));
    this._on(window, 'mouseup', (e) => this._onMouseUp(e));
    this._on(el, 'wheel', (e) => this._onWheel(e), { passive: false });
    this._on(el, 'click', (e) => this._onClick(e));
    this._on(el, 'contextmenu', (e) => this._onContextMenu(e));
    this._on(el, 'auxclick', (e) => this._onAuxClick(e));
    // Refocus when the window regains focus (common terminal behavior).
    this._on(window, 'focus', () => {
      if (this._focused) this.focus();
    });
  }

  _onKeyDown(e) {
    if (this._composing) return;
    const mods = modMask(e);
    const key = e.key;
    const primary = e.metaKey || (e.ctrlKey && e.shiftKey);

    if (primary && (key === 'c' || key === 'C') && this._selection) {
      this._copySelection();
      e.preventDefault();
      return;
    }
    if (primary && (key === 'v' || key === 'V')) {
      this._tryClipboardPaste();
      e.preventDefault();
      return;
    }
    if (primary && (key === 'a' || key === 'A')) {
      this.selectAll();
      e.preventDefault();
      return;
    }

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
    if (key.length === 1 || (key.codePointAt && key.codePointAt(0) > 0xffff)) {
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
    this._emitData(this._encoder.encode(text));
    this._scrollToBottomOnInput();
  }

  _scrollToBottomOnInput() {
    if (this.term.displayOffset() !== 0) {
      this.term.scrollToBottom();
      this._forceNext = true;
      this._scheduleRender();
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
        /* fall back to the paste event */
      }
    }
  }
  _pasteText(text) {
    text = text.replace(/\r\n/g, '\r').replace(/\n/g, '\r');
    let bytes;
    if (this.term.bracketedPaste()) {
      const payload = this._encoder.encode(text);
      const pre = this._encoder.encode('\x1b[200~');
      const post = this._encoder.encode('\x1b[201~');
      bytes = new Uint8Array(pre.length + payload.length + post.length);
      bytes.set(pre, 0);
      bytes.set(payload, pre.length);
      bytes.set(post, pre.length + payload.length);
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
    if (this._menu) this._closeMenu();
    const { x, y } = this._cellAt(e);

    // App mouse reporting (Shift bypasses so the user can still select).
    if (this.term.mouseMode() !== 0 && !e.shiftKey && e.button === 0) {
      const bytes = this.term.mouse(this._btn(e), x, y, 0, modMask(e));
      if (bytes.length) this._emitData(bytes);
      return;
    }
    if (e.button !== 0) return;

    // Click counting for word/line selection.
    const now = performance.now();
    const same = now - this._lastClick.t < 400 && this._lastClick.x === x && this._lastClick.y === y;
    this._lastClick = { t: now, x, y, n: same ? this._lastClick.n + 1 : 1 };

    if (this._lastClick.n === 2) {
      this._selection = this._wordSelection(x, y);
      this._selMode = 'word';
    } else if (this._lastClick.n >= 3) {
      this._selection = { sx: 0, sy: y, ex: this.model.cols, ey: y };
      this._selMode = 'line';
    } else {
      this._selecting = true;
      this._selAnchor = { x, y };
      this._selection = { sx: x, sy: y, ex: x, ey: y };
      this._selMode = 'char';
    }
    this._forceNext = true;
    this._scheduleRender();
    e.preventDefault();
  }

  _onMouseMove(e) {
    if (this._selecting) {
      const { x, y } = this._cellAt(e);
      this._selection = this._normalizeSel(this._selAnchor, { x: x + 1, y });
      this._forceNext = true;
      this._scheduleRender();
      return;
    }
    if (!this.container.contains(e.target) && e.target !== this.renderer.element) return;
    const { x, y } = this._cellAt(e);
    const link = linkAt(this.model, x, y, (id) => this.term.linkUri(id));
    const changed =
      !!link !== !!this._hoverLink ||
      (link && this._hoverLink && (link.y !== this._hoverLink.y || link.x0 !== this._hoverLink.x0));
    if (changed) {
      this._hoverLink = link;
      this.renderer.element.style.cursor = link ? 'pointer' : 'text';
      this._forceNext = true;
      this._scheduleRender();
    }
  }

  _onMouseUp(e) {
    if (this._selecting) {
      this._selecting = false;
      if (this.getSelection() && this.opts.copyOnSelect) this._copySelection();
    }
    if (this.term.mouseMode() !== 0 && !e.shiftKey && e.button === 0) {
      const { x, y } = this._cellAt(e);
      const bytes = this.term.mouse(this._btn(e), x, y, 1, modMask(e));
      if (bytes.length) this._emitData(bytes);
    }
  }

  _onClick(e) {
    if (this._hoverLink && !this._selecting) {
      const uri = this._hoverLink.uri;
      if (this.opts.onLink) this.opts.onLink(uri, e);
      else window.open(uri, '_blank', 'noopener,noreferrer');
    }
  }

  _onAuxClick(e) {
    // Middle-click pastes (X11 convention).
    if (e.button === 1) {
      this._tryClipboardPaste();
      e.preventDefault();
    }
  }

  _onContextMenu(e) {
    if (this.opts.rightClick === 'none') return;
    if (this.term.mouseMode() !== 0 && !e.shiftKey) return; // app wants the event
    e.preventDefault();
    if (this.opts.rightClick === 'paste') {
      this._tryClipboardPaste();
      return;
    }
    this._openMenu(e.clientX, e.clientY);
  }

  _openMenu(clientX, clientY) {
    this._closeMenu();
    const menu = document.createElement('div');
    menu.className = 'ft-menu';
    Object.assign(menu.style, {
      position: 'fixed', zIndex: '9999', minWidth: '150px',
      background: '#1f2335', color: '#c0caf5', border: '1px solid #2f334d',
      borderRadius: '8px', padding: '4px', font: '13px system-ui, sans-serif',
      boxShadow: '0 8px 24px rgba(0,0,0,.4)', userSelect: 'none',
    });
    const hasSel = !!this.getSelection();
    const items = [
      ['Copy', hasSel, () => this._copySelection()],
      ['Paste', true, () => this._tryClipboardPaste()],
      ['Select All', true, () => this.selectAll()],
      ['Clear', true, () => this.clear()],
    ];
    for (const [label, enabled, action] of items) {
      const it = document.createElement('div');
      it.textContent = label;
      Object.assign(it.style, {
        padding: '6px 12px', borderRadius: '5px',
        cursor: enabled ? 'pointer' : 'default', opacity: enabled ? '1' : '.4',
      });
      if (enabled) {
        it.addEventListener('mouseenter', () => (it.style.background = '#2a2f45'));
        it.addEventListener('mouseleave', () => (it.style.background = 'transparent'));
        it.addEventListener('mousedown', (ev) => {
          ev.preventDefault();
          ev.stopPropagation();
          this._closeMenu();
          action();
          this.focus();
        });
      }
      menu.appendChild(it);
    }
    document.body.appendChild(menu);
    // Keep on-screen.
    const r = menu.getBoundingClientRect();
    menu.style.left = Math.min(clientX, window.innerWidth - r.width - 8) + 'px';
    menu.style.top = Math.min(clientY, window.innerHeight - r.height - 8) + 'px';
    this._menu = menu;
    this._menuCloser = () => this._closeMenu();
    setTimeout(() => window.addEventListener('mousedown', this._menuCloser, { once: true }), 0);
  }
  _closeMenu() {
    if (this._menu) {
      this._menu.remove();
      this._menu = null;
    }
    if (this._menuCloser) {
      window.removeEventListener('mousedown', this._menuCloser);
      this._menuCloser = null;
    }
  }

  _onWheel(e) {
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
    this._scheduleRender();
    e.preventDefault();
  }

  _btn(e) {
    return e.button === 1 ? 1 : e.button === 2 ? 2 : 0;
  }

  _normalizeSel(a, b) {
    if (a.y < b.y || (a.y === b.y && a.x <= b.x)) return { sx: a.x, sy: a.y, ex: b.x, ey: b.y };
    return { sx: b.x, sy: b.y, ex: a.x + 1, ey: a.y };
  }

  _wordSelection(x, y) {
    const text = this.model.rowText(y);
    const isWord = (ch) => ch && /[\w\-./~:@]/.test(ch);
    if (!isWord(text[x])) return { sx: x, sy: y, ex: x + 1, ey: y };
    let s = x;
    let ee = x;
    while (s > 0 && isWord(text[s - 1])) s--;
    while (ee < this.model.cols - 1 && isWord(text[ee + 1])) ee++;
    return { sx: s, sy: y, ex: ee + 1, ey: y };
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
      } else if (y === sel.sy) x0 = sel.sx;
      else if (y === sel.ey) x1 = sel.ex;
      out += full.slice(x0, x1).replace(/\s+$/, '');
      if (y !== sel.ey) out += '\n';
    }
    return out;
  }
}

export { Palette, DEFAULT_THEME };
export default Ferroterm;
