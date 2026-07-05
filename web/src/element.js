// <ferro-term> — a Custom Element wrapper around the Ferroterm engine, so the
// terminal can be used declaratively in HTML:
//
//   import { defineFerroTermElement } from 'ferroterm';
//   defineFerroTermElement();
//   // <ferro-term rows="24" cols="80" renderer="webgl"></ferro-term>
//   const el = document.querySelector('ferro-term');
//   await el.ready;                 // WASM + view are up
//   el.addEventListener('data', (e) => socket.send(e.detail));
//   socket.onmessage = (m) => el.write(new Uint8Array(m.data));
//
// The imperative factory API (`Ferroterm.create(el, opts)`) is unchanged; this
// element uses it under the hood and mounts into its own light DOM, so the
// host's ferroterm stylesheet applies exactly as it does for a plain container.
// Registration is an explicit call (not an import side effect) to keep the
// package tree-shakeable (`"sideEffects": false`).

import { Ferroterm } from './ferroterm.js';

const num = (v) => Number(v);
const str = (v) => v;
// A boolean attribute is true when present and not literally "false".
const bool = (v) => v !== null && v !== 'false';

// Declarative attribute -> create() option, with a parser. Kebab-case in HTML,
// camelCase in the options object.
const ATTR_OPTIONS = {
  cols: ['cols', num],
  rows: ['rows', num],
  renderer: ['renderer', str],
  'font-size': ['fontSize', num],
  'font-family': ['fontFamily', str],
  'line-height': ['lineHeight', num],
  scrollback: ['scrollback', num],
  'cursor-style': ['cursorStyle', str],
  'cursor-blink': ['cursorBlink', bool],
  'copy-on-select': ['copyOnSelect', bool],
  'right-click': ['rightClick', str],
};

export class FerroTermElement extends HTMLElement {
  static get observedAttributes() {
    return ['renderer'];
  }

  constructor() {
    super();
    this._term = null;
    this._ready = null;
    // Writes issued before WASM finishes loading are buffered, then flushed in
    // order once the engine is live.
    this._pending = [];
  }

  /** The underlying `Ferroterm` instance, or `null` until `ready` resolves. */
  get terminal() {
    return this._term;
  }

  /** Resolves with the `Ferroterm` instance once WASM and the view are up. */
  get ready() {
    return this._ready || Promise.resolve(this._term);
  }

  _readOptions() {
    const opts = {};
    for (const [attr, [key, parse]] of Object.entries(ATTR_OPTIONS)) {
      if (this.hasAttribute(attr)) opts[key] = parse(this.getAttribute(attr));
    }
    // A fixed grid by default; opt into fitting the element's box with `fit`.
    opts.autoFit = this.hasAttribute('fit');
    return opts;
  }

  connectedCallback() {
    // connectedCallback can fire more than once (e.g. a DOM move); only mount
    // when we aren't already mounting or mounted.
    if (this._term || this._ready) return;
    if (!this.style.display) this.style.display = 'block';
    this._ready = Ferroterm.create(this, this._readOptions()).then((t) => {
      this._term = t;
      // Bridge engine callbacks to DOM CustomEvents so a host can wire things
      // up declaratively (el.addEventListener('data', ...)).
      t.onData((bytes) => this.dispatchEvent(new CustomEvent('data', { detail: bytes })));
      t.onTitleChange((title) => this.dispatchEvent(new CustomEvent('title', { detail: title })));
      t.onResize((cols, rows) =>
        this.dispatchEvent(new CustomEvent('resize', { detail: { cols, rows } })));
      for (const d of this._pending) t.write(d);
      this._pending = [];
      this.dispatchEvent(new CustomEvent('ready', { detail: t }));
      return t;
    });
  }

  disconnectedCallback() {
    if (this._term) this._term.dispose();
    this._term = null;
    this._ready = null;
  }

  attributeChangedCallback(name, _old, value) {
    if (name === 'renderer' && this._term && value) this._term.setRenderer(value);
  }

  // --- convenience delegation so the element behaves like a terminal --------

  /** Feed bytes/text to the screen. Buffered until `ready` if called early. */
  write(data) {
    if (this._term) this._term.write(data);
    else this._pending.push(data);
    return this;
  }

  focus() {
    this._term?.focus();
  }

  blur() {
    this._term?.blur();
  }

  fit() {
    this._term?.fit();
  }
}

/**
 * Register the `<ferro-term>` element (idempotent, and a no-op outside a browser
 * or when the tag is already defined). Returns the tag name.
 */
export function defineFerroTermElement(tag = 'ferro-term') {
  if (typeof customElements !== 'undefined' && !customElements.get(tag)) {
    customElements.define(tag, FerroTermElement);
  }
  return tag;
}
