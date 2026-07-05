// Shared benchmark helpers, used by the demo's `benchmark` command and the
// standalone examples/benchmark.html page. All timings isolate real compute:
// they parse / snapshot once up front and then time the operation in a tight
// best-of-N loop, so nothing here is limited by requestAnimationFrame / vsync.

/** Best-of-`trials` average ms/call over `iters` calls. */
export function bestMs(fn, trials = 5, iters = 40) {
  let best = Infinity;
  for (let t = 0; t < trials; t++) {
    const t0 = performance.now();
    for (let i = 0; i < iters; i++) fn();
    best = Math.min(best, (performance.now() - t0) / iters);
  }
  return best;
}

/** A full-screen frame with a background + glyph in every cell (worst case). */
export function fullScreenFrame(cols, rows, frame = 0) {
  let s = '\x1b[H';
  for (let y = 0; y < rows; y++) {
    let line = '';
    for (let x = 0; x < cols; x++) {
      const cc = (x + y + frame) % 256;
      line += `\x1b[48;5;${cc}m\x1b[38;5;${(cc + 128) % 256}m*`;
    }
    s += line + '\x1b[0m';
    if (y < rows - 1) s += '\r\n';
  }
  return s;
}

/**
 * The unmasked GPU renderer string, so results can be read in context (a real
 * GPU vs a software fallback like SwiftShader/llvmpipe behave very differently).
 * Returns `{ renderer, vendor, software }` or null if WebGL is unavailable.
 */
export function gpuInfo() {
  const c = document.createElement('canvas');
  const gl = c.getContext('webgl') || c.getContext('experimental-webgl');
  if (!gl) return null;
  const dbg = gl.getExtension('WEBGL_debug_renderer_info');
  const renderer = dbg ? gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL) : gl.getParameter(gl.RENDERER);
  const vendor = dbg ? gl.getParameter(dbg.UNMASKED_VENDOR_WEBGL) : gl.getParameter(gl.VENDOR);
  const software = /swiftshader|llvmpipe|software|microsoft basic/i.test(renderer || '');
  return { renderer: renderer || 'unknown', vendor: vendor || 'unknown', software };
}

const nextFrame = () => new Promise((r) => requestAnimationFrame(r));

/**
 * Render-compute time (ms) for each renderer over an identical, already-parsed
 * full-screen frame. Parsing/snapshot happen once; only render() is timed.
 * Returns `[{ kind, best }]`. Restores the original renderer afterward.
 */
export async function measureRenderers(term) {
  const original = term.rendererName || 'webgl';
  const content = fullScreenFrame(term.cols, term.rows, 0);
  const out = [];
  for (const kind of ['canvas', 'webgl', 'dom']) {
    term.setRenderer(kind);
    await nextFrame();
    term.write(content);
    await nextFrame();
    const R = term.renderer, model = term.model;
    const cursor = { x: model.cursorX, y: model.cursorY, show: false, style: 'block', focused: true };
    for (let i = 0; i < 10; i++) R.render(model, [], true, cursor, null, null);
    out.push({ kind: term.rendererName, best: bestMs(() => R.render(model, [], true, cursor, null, null)) });
  }
  term.setRenderer(original);
  await nextFrame();
  term.write('\x1b[2J\x1b[H');
  return out;
}

/** snapshot / applySnapshot / render breakdown for the active renderer. */
export function measurePipeline(term) {
  term.write(fullScreenFrame(term.cols, term.rows, 7));
  const snap = bestMs(() => term._snapshot(true), 5, 50);
  const s = term._snapshot(true);
  const apply = bestMs(() => term.model.applySnapshot(s), 5, 50);
  term.model.applySnapshot(s);
  const R = term.renderer, model = term.model;
  const cursor = { x: model.cursorX, y: model.cursorY, show: false, style: 'block', focused: true };
  for (let i = 0; i < 10; i++) R.render(model, [], true, cursor, null, null);
  const render = bestMs(() => R.render(model, [], true, cursor, null, null));
  term.write('\x1b[2J\x1b[H');
  return { kind: term.rendererName, snap, apply, render, frame: snap + apply + render };
}

/**
 * Full vs incremental render cost for the WebGL renderer: a full repaint, a
 * single dirty row (typing), and a no-grid-change frame (cursor blink). Only
 * meaningful for the incremental WebGL path; returns null for Canvas2D.
 */
export function measureIncremental(term) {
  if (!term.rendererName || term.rendererName.toLowerCase().includes('canvas')) return null;
  term.write(fullScreenFrame(term.cols, term.rows, 3));
  const R = term.renderer, model = term.model;
  model.applySnapshot(term._snapshot(true));
  const cursor = { x: 0, y: 0, show: false, style: 'block', focused: true };
  for (let i = 0; i < 10; i++) R.render(model, [], true, cursor, null, null);
  const mid = (term.rows / 2) | 0;
  const full = bestMs(() => R.render(model, [], true, cursor, null, null));
  const oneRow = bestMs(() => R.render(model, [mid], false, cursor, null, null));
  const cursorOnly = bestMs(() => R.render(model, [], false, cursor, null, null));
  term.write('\x1b[2J\x1b[H');
  return { full, oneRow, cursorOnly };
}
