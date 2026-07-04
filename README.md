# ferroterm

A fast, secure terminal emulator **core** written from scratch in Rust, compiled
to WebAssembly, and wrapped in a small, dependency-free web component with both
**Canvas2D** and **WebGL** renderers.

It is a clean-room reimplementation of the functionality of a browser terminal
(the problem [xterm.js](https://xtermjs.org) solves), not a fork: the escape
parser, grid model, scrollback and input encoding are all new Rust code. The
heavy lifting — parsing an untrusted byte stream into a styled grid — runs in
Rust/WASM for throughput and memory safety; the JavaScript layer only renders
and captures input.

```
┌─────────────┐   bytes    ┌──────────────────────────┐  Uint32Array   ┌───────────────┐
│  PTY / host │ ─────────▶ │  ferroterm-core (Rust →   │  snapshot ───▶ │  renderer     │
│  (or socket)│ ◀───────── │  WASM): parser + grid +   │ ◀── input ──   │  Canvas2D /   │
└─────────────┘  replies   │  scrollback + state       │   encoding     │  WebGL        │
                           └──────────────────────────┘                └───────────────┘
```

## Highlights

- **From-scratch ANSI/VT parser** following the DEC (Williams) state machine:
  CSI/OSC/DCS, SGR incl. 256-color and 24-bit true color, cursor motion, erase /
  insert / delete, scroll regions, alternate screen, DECSET/DECRST modes, and
  host replies (DSR, Device Attributes).
- **Unicode**: UTF-8 decoding with astral-plane support (emoji, CJK extensions),
  wide-character (double-width) cells, and a compact East-Asian-width table.
- **Links**: OSC 8 hyperlinks *and* automatic URL detection, with hover-underline
  and click-to-open.
- **Two renderers, swappable at runtime**: a Canvas2D renderer that redraws only
  dirty rows, and a WebGL renderer with a dynamic glyph atlas and batched quads.
  WebGL falls back to Canvas2D when unavailable.
- **Reusable component**: `Ferroterm.create(el, opts)`, `onData` / `write`,
  theming, selection, clipboard, bracketed paste, scrollback. Ships TypeScript
  types. No runtime dependencies.
- **Measured**: ~130 MB/s parse throughput (native release), sub-millisecond
  render snapshots. See [Benchmarks](#benchmarks).
- **Desktop app**: a tabbed Tauri terminal with real PTYs and a battery + FPS HUD.

## Repository layout

| Path | What |
| --- | --- |
| `crates/core` | `ferroterm-core` — the pure-Rust terminal engine + tests + bench |
| `crates/wasm` | `ferroterm-wasm` — `wasm-bindgen` bindings |
| `web/` | the JS/TS component (renderers, input, links) + `.d.ts` |
| `examples/` | a no-backend browser demo |
| `apps/desktop/` | the Tauri tabbed-terminal application |
| `build.sh` | builds the WASM module into `web/pkg` |

## Quick start (web component)

```bash
# 1. Build the WASM module (needs the wasm32 target + wasm-bindgen-cli).
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.122
./build.sh                 # -> web/pkg/

# 2. Serve the repo and open the demo.
python3 -m http.server 8080
# visit http://localhost:8080/examples/
```

In your own app:

```js
import { Ferroterm } from 'ferroterm';

const term = await Ferroterm.create(document.getElementById('term'), {
  cols: 80, rows: 24, renderer: 'webgl', // or 'canvas'
});

// Wire it to a PTY over a WebSocket (your backend):
const sock = new WebSocket('wss://example/pty');
sock.binaryType = 'arraybuffer';
term.onData(bytes => sock.send(bytes));            // keystrokes -> PTY
sock.onmessage = e => term.write(new Uint8Array(e.data)); // PTY -> screen

term.onTitleChange(t => (document.title = t));
term.fit();
term.focus();
```

Switch renderers live: `term.setRenderer('canvas')`. Re-theme:
`term.setTheme({ background: '#000', ansi: [...] })`.

### Options

`cols`, `rows`, `scrollback`, `fontFamily`, `fontSize`, `lineHeight`,
`renderer` (`'webgl'`|`'canvas'`), `theme`, `cursorStyle`
(`'block'`|`'bar'`|`'underline'`), `cursorBlink`, `scrollSensitivity`,
`autoFit`, `copyOnSelect`, `onLink`, `wasmUrl`. See `web/ferroterm.d.ts`.

## Using the core directly (Rust)

```rust
use ferroterm_core::Terminal;

let mut term = Terminal::new(80, 24, 1000);
term.feed(b"\x1b[31mhello\x1b[0m");
assert_eq!(term.cell_char(0, 0), 'h');

// The front-end consumes packed snapshots:
let snapshot: Vec<u32> = term.snapshot(/* force = */ false);
```

## Desktop app (Tauri)

A tabbed terminal that spawns real shells and shows a battery-runtime + FPS HUD.

```bash
./build.sh                       # build WASM first
cd apps/desktop
npm install                      # @tauri-apps/cli
npm run dev                      # sync component + `tauri dev`
```

Features: multiple tabs (Cmd/Ctrl+T / +W, Cmd/Ctrl+1..9), per-tab PTY via
`portable-pty`, live title updates, battery percentage + time-remaining, FPS and
throughput readouts, runtime renderer selection.

## Benchmarks

```bash
cargo run --release -p ferroterm-core --example bench
```

On an Apple-silicon laptop (native release build):

```
80x24:  parse 130.3 MB/s  (34 MB in 257 ms)   snapshot 0.01 ms/frame
200x50: parse 126.6 MB/s  (34 MB in 265 ms)   snapshot 0.07 ms/frame
```

The browser `loadtest` command measures end-to-end (parse + render) MB/s and
prints it the way the xterm.js demo does.

## Testing

```bash
cargo test -p ferroterm-core          # 20+ behavioral conformance tests
```

The tests assert against the *visible grid state* (what a renderer would draw),
which is the strongest check for an emulator: printing/wrapping, cursor motion,
erase/scroll regions, SGR (incl. true color), alt-screen isolation, wide chars,
astral emoji, OSC titles, OSC 8 links, DSR/DA replies, and a fuzz-style
"malicious input must not panic/hang" case.

## Design notes

- **One snapshot per frame.** Crossing the WASM boundary per cell is slow, so the
  core serializes changed rows into a single `Uint32Array`
  (`[magic, cols, rows, curX, curY, curFlags, nRows, {rowIndex, cells…}…]`,
  5 words per cell) that the JS model decodes once. Only dirty rows are emitted.
- **Deferred wrap.** Printing into the last column sets a pending-wrap flag rather
  than wrapping immediately, matching real DEC terminals.
- **Bounded input.** Parameter counts, intermediates and OSC payloads are capped
  so hostile sequences can't exhaust memory; every parser loop advances.
- **Safety at the boundary.** All untrusted bytes are parsed in memory-safe Rust;
  the JS layer never interprets escape sequences itself.

## Known limitations (v0.1, honestly)

- **Combining marks / grapheme clusters** (ZWJ emoji sequences, flags, base +
  combining accents) are not merged into a single cell — each cell holds one
  Unicode scalar, so zero-width combining code points are dropped. Most text and
  standalone emoji render correctly; complex clusters do not.
- **No reflow on resize.** Resizing truncates/pads lines rather than rewrapping
  wrapped content. Scrollback is preserved but not rewrapped.
- **DCS / Sixel / iTerm images** are recognized and consumed but not rendered.
- **Palette OSC (4/10/11) and some rare modes** are parsed but not applied.

These are deliberate scope choices for a first release, not accidental gaps.

## License

MIT — see [LICENSE](LICENSE).
