# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/), and the project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- **Grapheme-cluster merging.** Combining marks (accents, diacritics), ZWJ
  emoji sequences (family / profession), variation selectors, and
  regional-indicator flags now collapse into a single cell instead of dropping
  zero-width scalars. The core interns cluster strings and exposes them via
  `Terminal::grapheme(id)`; snapshots carry a per-cell grapheme id (cell is now
  6 words), and both renderers draw the full cluster (WebGL keys its atlas by
  the cluster string). Removes the first item from the "known limitations" list.

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
