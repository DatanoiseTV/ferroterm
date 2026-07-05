# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/), and the project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

Native app (`apps/native`, versioned independently) — closing parity gaps with
the web renderer:

### Added
- **Underline and strikethrough** in the native wgpu renderer. Decorations are
  emitted as thin solid foreground-colored instances (reusing the no-glyph path)
  positioned to match the web renderer (underline at baseline+2, strike at 55%
  of the cell), thickness scaled to the cell. Underlined blank cells now draw the
  line even with a transparent background. Verified by a headless offscreen
  render asserting exact line colors at the expected rows.
- **Bold, italic and bold-italic** glyphs in the native atlas. The atlas now
  resolves four faces from the system font, classified by their style bits (via
  `ttf-parser`) rather than hardcoded collection indices — so Menlo's real bold
  and italic faces are used on macOS. A missing face degrades to a synthetic
  transform of the regular face (a shear for italic, a one-pixel horizontal
  dilation for bold) so styled text still reads distinctly. Glyphs are cached per
  (codepoint, wide, style). Verified by a headless render asserting bold/italic
  cells differ from regular and from each other, with bold no lighter than
  regular.
- **Inline image rendering** in the native app. A second GPU pass draws placed
  RGBA images (Sixel and Kitty `f=24`/`f=32`) as textured quads over the cells,
  aligned to the grid and scroll-tracked via the core's placement API, with
  per-image textures cached by id and drawn in one pass using dynamic uniform
  offsets. Encoded images (iTerm2, Kitty PNG) carry no RGBA and are skipped —
  the native app links no image codec yet (a follow-up). Verified by headless
  renders: a direct RGBA quad over cells, and a full path feeding a Kitty image
  to the terminal and asserting it rasterizes at its placement.

## [0.7.0] - 2026-07-05

### Added
- **Kitty graphics protocol (APC `_G…`).** The parser now captures APC strings
  (previously consumed and discarded) and routes `_G` commands to a new core
  handler. Supported: direct base64 transmission (`t=d`) of RGB (`f=24`, expanded
  to opaque RGBA in-core), RGBA (`f=32`) and PNG (`f=100`, handed to the browser
  to decode — no codec linked into the WASM); chunked transfers (`m=1`);
  transmit-and-display (`a=T`), store-then-place-by-id (`a=t` then `a=p`), delete
  (`a=d`, scoped to Kitty images so Sixel/iTerm2 are untouched) and query
  (`a=q`, acknowledged). Images reuse the existing renderer-agnostic overlay, so
  they scroll with their text and render on both the Canvas2D and WebGL paths.
  Verified end-to-end in headless Chrome (a transmitted RGBA block rasterizes to
  the expected pixels at its placement).
- File / temp-file / shared-memory transmission (`t=f|t|s`) and zlib-compressed
  payloads (`o=z`) are **refused cleanly** rather than mis-handled: the former
  would let a terminal escape read arbitrary host files; the latter needs an
  inflate the core doesn't link. The byte stream stays in sync in both cases.

## [0.6.2] - 2026-07-05

### Fixed
- **Mouse-wheel scrolling was too sensitive.** The wheel handler scrolled a
  fixed `sign(deltaY) * scrollSensitivity` (default 3) lines per event,
  discarding the delta magnitude and unit — so a trackpad or high-resolution
  wheel, which fires many small events per gesture, snapped a full three rows on
  every one (a gentle nudge could jump ~24 rows). Wheel deltas are now
  normalized to text rows by unit (`DOM_DELTA_LINE`/`PIXEL`/`PAGE`; pixel deltas
  divided by the cell height) and the sub-row remainder is carried between
  events, so scrolling is magnitude-proportional and pixel-accurate. The default
  `scrollSensitivity` is now `1` (was `3`); set it higher for faster scrolling.

## [0.6.1] - 2026-07-05

### Performance
- **WebGL overlay pass no longer scans every cell.** The per-frame overlay
  (cursor, underline/strike decorations, hover-link) previously walked the whole
  grid every frame to find the rare decorated cell — an `O(rows×cols)` scan even
  for a cursor-blink frame that changes nothing. The renderer now tracks which
  rows contain a decoration (updated as rows are regenerated) and the overlay
  pass visits only decorated or hovered rows. Measured at 200×50 under software
  GL: a one-row typing frame drops from 0.010 ms to 0.005 ms (2×) and a
  cursor-blink frame falls below the 100 µs timer resolution; full-frame cost is
  unchanged. Output is pixel-identical (verified by the incremental-parity test,
  which exercises underline and strike). The benchmark page (`examples/
  benchmark.html`) gained `?cols=&rows=` to profile at arbitrary grid sizes.

## [0.6.0] - 2026-07-05

### Added
- **iTerm2 inline images (OSC 1337 `File=`).** ferroterm now renders images sent
  via the iTerm2 protocol, in addition to Sixel. The Rust core parses the
  protocol, base64-decodes the payload, sniffs the pixel dimensions and container
  format from the file header (PNG / JPEG / GIF / BMP / WebP — no full decode),
  and lays the image out in whole cells (honoring `width=`/`height=` in cells,
  pixels, percent or `auto`, with aspect-ratio preservation). The actual pixel
  decode is delegated to the browser (`createImageBitmap`), so **no image decoder
  is linked into the WASM core** — any format the browser supports works, and the
  bundle stays small. Images share the existing renderer-agnostic overlay used by
  Sixel, so they work under both the Canvas2D and WebGL renderers and track
  scrolling. The OSC payload cap is raised for `1337;File=` sequences (to 8 MB,
  still bounded) so real images aren't truncated. Verified end-to-end headlessly:
  a page-generated PNG fed as OSC 1337 decodes to the exact expected pixel on the
  overlay and advances the cursor below the image.
- **Standalone renderer benchmark page** (`examples/benchmark.html`). Reports the
  unmasked WebGL renderer (real GPU vs a software fallback like SwiftShader), then
  runs render-compute, per-frame pipeline, and incremental-vs-full timings as
  tables plus copyable JSON. The measurement helpers are shared with the demo's
  `benchmark` command (`web/src/bench.js`), so both report identical numbers.

### Changed
- The image overlay and cache in the web component now handle both ready-RGBA
  (Sixel) and encoded (iTerm2) images; encoded images decode asynchronously and
  repaint when ready. `Terminal` gains `imageEncoded(id)` / `imageMime(id)`.

## [0.5.0] - 2026-07-05

### Performance
- **Incremental WebGL rendering.** The WebGL renderer previously rebuilt every
  cell's instance data on every frame. It now keeps a persistent GPU buffer with
  one fixed slot per cell and regenerates + re-uploads only the rows that
  actually changed (`bufferSubData` of the changed row span), matching the
  Canvas2D renderer's existing dirty-row behavior. Cursor, underline/strike and
  hover-link decorations live in a small per-frame overlay drawn on top, so they
  never dirty the grid. Selection is baked into the cell background and marks
  only its own rows dirty. Measured on a full 200x50 screen under software GL: a
  full repaint is unchanged (~0.13 ms), a one-row edit drops to ~0.008 ms (18x),
  and a cursor-blink frame (no grid change) to ~0.003 ms (54x). This is a CPU /
  battery win for typical interactive use (typing, cursor, status lines), not a
  full-screen fps change. Output is pixel-identical to a full re-render for
  content edits, cursor movement and selection (verified with 0 pixel
  difference).

### Added
- **Headless renderer regression tests + CI.** `web/test/` renders a fixed
  feature-rich scene through both renderers in headless Chrome and asserts
  semantic per-renderer pixel colors (indexed fg/bg, default, inverse, dim, wide
  CJK, true color) plus same-renderer determinism and — for WebGL — incremental
  vs full-render parity. Wired as `npm test` (`CHROME_BIN` overridable). A CI
  workflow runs `cargo fmt --check` + `clippy -D warnings` + `cargo test` and
  the headless renderer test under Chromium.

## [0.4.1] - 2026-07-05

### Performance
- **Packed-byte instance colors.** The instanced WebGL renderer now packs each
  cell's foreground and background as RGBA8 read by the shader as normalized
  `UNSIGNED_BYTE` attributes, instead of eight floats. This removes the six
  `/255` divides per cell and shrinks the per-cell instance record from 68 to 44
  bytes. Text repaint is ~10% faster (0.11 -> 0.10 ms for a full 200x50 frame
  under software GL); background-heavy frames are unchanged. Output is
  pixel-identical (verified by a 0-difference full-canvas diff against the
  previous renderer).

### Changed
- **Rebuilt the `benchmark` demo command.** The renderer-paint measurement
  previously timed `write()` + `requestAnimationFrame`, so it measured escape-
  sequence parsing and vsync delivery, not paint — capped at ~60 fps regardless
  of renderer speed. It now parses the frame once and times `render()` in a
  tight best-of-N loop, reporting true compute time (e.g. WebGL ~0.1 ms /
  ~9000 fps for a full-screen redraw). A new per-frame pipeline table breaks a
  frame into snapshot / applySnapshot / render with each stage's share, making
  it clear that paint dominates and parse/snapshot are cheap. `runCommand` now
  returns the command's result so async commands can be awaited by the host.

## [0.4.0] - 2026-07-05

### Performance
- **Instanced WebGL renderer.** The WebGL renderer now draws the whole grid
  with a single instanced draw call (WebGL1 + `ANGLE_instanced_arrays`): one
  instance per visible cell, expanded from a shared unit quad in the vertex
  shader. Each instance carries its pixel rect, foreground/background colors and
  glyph atlas coordinates, and the fragment shader composites the glyph over the
  background — so a cell's background and text are one instance instead of two
  separate quads, and default-background cells emit nothing (the clear paints
  them). This writes ~17 floats per cell instead of the 54 floats per quad (up
  to two quads per cell) the previous batched renderer wrote. A full 200x50
  (10k-cell) repaint under software GL (SwiftShader) dropped to ~0.11 ms for
  text and ~0.14 ms for a fully-colored-background screen, from ~0.20 ms and
  ~0.61 ms — 1.7x faster on text and 4.4x on backgrounds, and colored
  backgrounds now cost about the same as plain text. Output is pixel-identical
  to the previous renderer (verified by a full-canvas diff across inverse,
  underline, strike, dim, italic, bold, wide CJK/Hangul, true-color and
  256-color content). If `ANGLE_instanced_arrays` is unavailable the renderer
  falls back to Canvas2D as before.
- **Zero-copy render snapshots across the wasm boundary.** The render loop no
  longer allocates and copies a fresh ~240KB `Uint32Array` per frame (the
  largest per-frame allocation, ~14MB/s of GC garbage at 60fps). The core gained
  `snapshot_into` (fills a reused buffer) and the wasm layer exposes
  `snapshotPtr` / `snapshotLen` over a persistent buffer; JavaScript wraps that
  in a `Uint32Array` view straight over wasm memory. Snapshot stage 0.063 ->
  0.036 ms (43% faster); rendering is unchanged and verified pixel-identical.

## [0.3.2] - 2026-07-04

### Performance
- **WebGL: zero per-quad allocation.** `_quad` previously built a six-element
  `corners` array (each a four-element sub-array) for every quad — roughly
  140k throwaway arrays per full-screen frame, and the GC pressure behind
  periodic frame spikes. It now writes the 54 floats of a quad's six vertices
  straight into the vertex scratch buffer, and hoists the NDC scale into
  precomputed `_invW` / `_invH` reciprocals so the inner loop multiplies
  instead of dividing per vertex.
- **WebGL: skip default-background quads.** The framebuffer is already cleared
  to the theme background before drawing, so cells that keep the default
  background (the majority on a typical screen) no longer emit a background
  quad — less vertex data generated, buffered, and rasterized. Inverse-video
  and explicitly-colored cells still draw their quad.
- **WebGL: tighter inner loop.** Both passes take local references to the
  snapshot typed arrays, compute the row base index directly instead of
  calling `model.index(x, y)` per cell, and hoist per-row / per-cell constants
  out of the innermost expressions.
- **Palette: no allocation on the true-color path.** `resolveRgb` now fills a
  reused scratch triple for 24-bit colors instead of allocating a fresh array
  per cell; the default and indexed cases already returned cached arrays.
- Net: a full 200x50 (10k-cell) WebGL repaint measures ~0.19-0.21 ms for text
  scenes and ~0.52 ms for a fully-colored-background scene under software GL
  (SwiftShader); it is faster still on real GPUs. The Canvas2D renderer holds
  at ~3.2-3.6 ms for the same workload under software rendering (Canvas2D's
  `fillText` is hardware-accelerated in browsers, so this is a worst case).

### Investigated, not shipped
- A Canvas2D glyph atlas (rasterize each glyph once, blit with `drawImage`)
  was prototyped and measured. It regressed the Canvas renderer under software
  rendering (~3.2 -> ~5.8 ms) because `drawImage` from an offscreen atlas is
  slower than batched `fillText` there, and browsers already cache glyph
  rasterization internally, so the win on real GPUs is marginal at best. The
  batched-`fillText` path from 0.3.1 is retained as the measured-faster option.

## [0.3.1] - 2026-07-04

### Performance
- **Render hot path: no per-cell string allocation.** The WebGL glyph atlas is
  now keyed by a small integer for single-scalar cells (the overwhelming
  majority) instead of building a `${text} ${flags}` string per cell per frame;
  only real grapheme clusters take the string path. This removes ~10k string
  allocations per full-screen frame and the GC stutter they caused — WebGL
  full-screen redraw dropped from ~1.23 ms to ~0.97 ms best-case and, more
  importantly, stopped spiking to ~5 ms on GC. The model also interns ASCII
  single-character strings so `clusterAt` never allocates for ASCII.
- **Canvas renderer batching.** Row draws now coalesce runs of identical
  background color into a single `fillRect`, set `ctx.font` / `fillStyle` /
  `globalAlpha` only when the value actually changes (Canvas2D state changes are
  the expensive part), batch selection spans, and draw underlines/strikethroughs
  as `fillRect`s. Full-screen redraw dropped from ~5.3 ms to ~3.1 ms (1.7×).

### Fixed
- Removed a stray NUL byte that had crept into the WebGL glyph cache-key
  template literal.

## [0.3.0] - 2026-07-04

### Added
- **Grapheme-cluster merging.** Combining marks (accents, diacritics), ZWJ
  emoji sequences (family / profession), variation selectors, and
  regional-indicator flags now collapse into a single cell instead of dropping
  zero-width scalars. The core interns cluster strings and exposes them via
  `Terminal::grapheme(id)`; snapshots carry a per-cell grapheme id (cell is now
  6 words), and both renderers draw the full cluster (WebGL keys its atlas by
  the cluster string). Removes the first item from the "known limitations" list.
- **Reflow (rewrap) on resize.** The primary screen and its scrollback are now
  treated as one continuous stream of logical lines and re-split at the new
  width, so auto-wrapped text stays intact when the terminal is resized instead
  of being truncated or padded. Hard line breaks are preserved (rows carry a
  `wrapped` flag set only on auto-wrap), wide glyphs are never split across the
  boundary, the cursor is tracked onto the same character, and overflow moves
  between screen and scrollback. The alternate screen is not reflowed.
- **Dynamic palette (OSC 4/10/11/12/104/110/111/112).** Palette entries and the
  default foreground / background / cursor colors set by the running program are
  now applied live (previously parsed and dropped), and `?` color queries are
  answered with the current value. The core keeps the override state and a
  version counter; the JS component re-themes and repaints when it changes.
  New APIs: `Terminal::palette_version` / `palette_export` / `set_default_colors`
  and their wasm bindings.
- **Sixel graphics.** DCS Sixel images are now decoded (RGB and HLS color
  definitions, run-length, raster attributes) and rendered instead of being
  consumed and dropped. The parser captures DCS payloads and dispatches them
  (`Perform::dcs_dispatch`); the core anchors each image in absolute
  line-serial space so it scrolls with its text and lives in scrollback, and
  advances the cursor below the image. The JS component composites images on a
  renderer-agnostic overlay canvas, so both the Canvas2D and WebGL renderers
  show them. New APIs: `set_cell_pixels`, `images_version`, `image_ids`,
  `image_rgba`, `image_size`, `image_placements` (+ wasm bindings). A `sixel`
  demo command was added to the browser example. iTerm2/Kitty image protocols
  remain unrendered.

## [0.2.0] - 2026-07-04

### Added
- Engine/view split in the web component (`attachView` / `detachView`): a host
  can keep hundreds of terminals alive while only visible ones hold a renderer.
- Mouse selection (drag), double-click word and triple-click line selection,
  right-click context menu (copy/paste/select-all/clear), middle-click paste.
- Buffer search: `findAll`, `lineText`, `totalLines`, `scrollToLine`.
- Desktop app: split panes with drag-resizable dividers, multiple windows,
  find, font zoom, and clear (see the shortcut table in the README).
- Extensible right-click menu (`menuItems` option); the desktop terminal menu
  now includes Split / New Tab / New Window / Close Pane, and the tab bar has
  its own New Tab / New Window / Split / Close context menu.
- A proper built-in `benchmark` command in the demo: per-workload parse
  throughput (plain / 256-color / true color / cursor / scroll) plus a
  Canvas2D-vs-WebGL paint comparison, in formatted tables.
- Web Serial demo (`examples/webserial.html`).

### Changed
- Parser throughput ~130 → ~248 MB/s: release profile tuned for speed
  (`opt-level = 3`) plus an ASCII bulk fast-path.
- WebGL renderer: fixed wide/CJK glyph stretching with a variable-width glyph
  atlas (shelf packer).

### Fixed
- Bulletproof keyboard focus in embedded webviews (focus on pointer-down and on
  window focus).

## [0.1.0] - 2026-07-04

Initial release.

### Added
- `ferroterm-core`: a from-scratch VT100/xterm-compatible terminal core in Rust.
  - DEC/Williams ANSI escape-sequence parser with UTF-8 (incl. astral plane) and
    wide-character handling.
  - Grid buffer with scroll regions, alternate screen, and a scrollback ring.
  - SGR: bold/dim/italic/underline/blink/inverse/invisible/strikethrough,
    16-color, 256-color, and 24-bit true color (both `;` and `:` forms).
  - Cursor motion, erase/insert/delete of lines and characters, DECSET/DECRST
    modes, host replies (DSR, Device Attributes).
  - OSC 0/2 window title and OSC 8 hyperlinks.
  - Keyboard and mouse input encoding (application cursor keys, xterm modifiers,
    X10 and SGR mouse protocols).
  - Compact `Uint32Array` render snapshots with per-row dirty tracking.
  - 20+ behavioral conformance tests and a throughput benchmark.
- `ferroterm` (WASM + JS): a reusable, dependency-free web component.
  - Swappable **Canvas2D** and **WebGL** renderers (WebGL uses a dynamic glyph
    atlas; automatic fallback to Canvas2D).
  - Keyboard, mouse, wheel, selection, clipboard and bracketed-paste handling.
  - OSC 8 and auto-detected URL links with hover-underline and click-to-open.
  - Themeable, TypeScript declarations included.
- Browser demo with a local shell (`help`, `ls`, `colors`, `chars`, `links`,
  `loadtest`) and a live FPS / renderer HUD.
- `ferroterm-desktop`: a tabbed terminal application (Tauri) with real PTYs,
  a battery-runtime and performance HUD.
