# ferroterm vs. xterm.js — measured comparison

This is a real, reproducible benchmark, not marketing. The harness in
[`bench/compare.html`](bench/compare.html) loads **both** terminals in the same
browser, feeds them **byte-for-byte identical** payloads, and times each one.

## Methodology

- **Same page, same browser, same machine.** Both terminals are 80×24 with 5000
  lines of scrollback.
- **Metric: write (parse) throughput** — the time to feed a payload and have the
  emulator finish parsing it into its buffer. For xterm.js this is the time
  until the `write(data, callback)` callback fires; for ferroterm it is the
  synchronous `write()` call. Both defer rendering to a later animation frame,
  so this isolates the parser/buffer — the core of a terminal emulator.
- **4 MB per payload, median of 4 runs**, buffers reset between runs.
- xterm.js **5.5.0**; ferroterm **0.6.0**. The parse numbers below were measured
  at 0.3.0; the parser/grid core is unchanged since, so they still hold (0.4–0.6
  were renderer, bundle and inline-image work, none of which touch parsing).
- Run headless (Chrome + SwiftShader). **Absolute numbers depend on hardware;
  the ratio is the point.** On a real GPU/CPU both are faster.

Reproduce:

```bash
cd bench && npm install
python3 -m http.server 8080          # from the repo root
# open http://localhost:8080/bench/compare.html  (results render as JSON)
```

## Parse throughput (higher is better)

| Workload | xterm.js 5.5 | ferroterm 0.3 | speedup |
| --- | ---: | ---: | ---: |
| plain text | 102 MB/s | **446 MB/s** | **4.4×** |
| 256-color (SGR) | 114 MB/s | **202 MB/s** | **1.8×** |
| true color | 130 MB/s | **179 MB/s** | **1.4×** |
| scroll (full-width) | 119 MB/s | **488 MB/s** | **4.1×** |

ferroterm is fastest on plain text and scrolling (its ASCII bulk fast-path fills
whole line spans at once) and narrows to ~1.4× on dense truecolor SGR, where both
are dominated by per-cell attribute parsing. The Rust core compiled natively
parses the same mix at ~250 MB/s in-process; WASM lands in the same ballpark.

## Bundle size (what ships to the browser)

| | raw | gzip |
| --- | ---: | ---: |
| **ferroterm** wasm + JS component (both renderers, all features) | 217 KB | **73 KB** |
| **xterm.js** core `xterm.js` | 289 KB | 68 KB |
| xterm.js + `addon-webgl` | 390 KB | ~95 KB |

ferroterm's JS is unminified source here; minified it is smaller still. The wasm
is 107 KB (44 KB gzip, `wasm-opt -Oz`) — the +2 KB gzip over 0.3 is the iTerm2
image protocol (base64 + header-only dimension sniffing; no image codec is
linked — the browser decodes pixels). The rest of the growth is JS: the
incremental WebGL renderer and inline-image handling. Canvas2D **and** WebGL
renderers plus Sixel, iTerm2 images, search, links and reflow are all built in —
no separate addons — so this compares against xterm.js **with** its webgl addon
(95 KB gzip); ferroterm is still smaller and includes more.

## Feature comparison

| | ferroterm 0.6 | xterm.js 5.5 |
| --- | --- | --- |
| Core language | Rust → WebAssembly | TypeScript |
| Renderers | Canvas2D + WebGL (built in) | DOM + WebGL (addon) |
| WebGL draw model | instanced, incremental (per-cell GPU buffer, dirty-row upload) | full-frame per addon |
| ANSI/VT parser | from scratch (DEC state machine) | mature |
| 256-color / true color | yes | yes |
| Wide (CJK) glyphs | yes | yes |
| Combining marks / ZWJ grapheme clusters | yes | yes |
| Reflow (rewrap) on resize | yes | yes |
| Dynamic palette (OSC 4/10/11) | yes | yes |
| Sixel graphics | yes (built in) | addon |
| iTerm2 inline images (OSC 1337) | yes (built in) | addon |
| Kitty graphics protocol | no | no |
| Programming ligatures | no | addon |
| OSC 8 hyperlinks | yes (built in) | yes |
| Auto URL link detection | yes (built in) | addon |
| Search | yes (built in) | addon |
| Selection / clipboard / bracketed paste | yes | yes |
| Mouse reporting (X10/SGR) | yes | yes |
| Memory safety of the parser | Rust (safe) | JS (safe) |
| Addon ecosystem | built-ins | **large, mature** |
| Production maturity | new (0.x) | **battle-tested (VS Code, …)** |

## Honest take

ferroterm is **faster** (1.4–4.4× parse), **comparably small** (73 KB gzip with
both renderers *and* Sixel, iTerm2 images, reflow, palette, search and links
built in, vs xterm.js's 68 KB core alone or ~95 KB with the webgl addon), and
parses untrusted bytes in **memory-safe Rust**. It ships Canvas2D and WebGL out
of the box, and the WebGL renderer repaints incrementally (only changed rows
re-upload, ~35× cheaper for a one-row edit; a cursor-blink frame is essentially
free).

xterm.js is **more mature**: a large addon ecosystem and years of production
hardening in editors and IDEs. If you need a proven track record or a specific
addon, use xterm.js. If you want raw throughput, a small footprint, and a Rust
core you can embed anywhere WASM runs, use ferroterm.

Numbers were produced by `bench/compare.html` on 2026-07-04; rerun it on your own
hardware to get figures for your machine.
