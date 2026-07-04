# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/), and the project adheres to
[Semantic Versioning](https://semver.org/).

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
