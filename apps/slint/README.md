# ferroterm-slint

A native terminal built on [`ferroterm-core`](../../crates/core) with a
[Slint](https://slint.dev) UI. It's a compact reference for embedding the
terminal engine in a GUI toolkit that isn't the web platform or wgpu.

```bash
cargo run --release                    # opens a Slint window running your $SHELL
cargo test                             # headless decode + rasterize pipeline (pixel-asserted)
cargo run --release --example bench    # headless rasterizer throughput benchmark
```

## How it works

`ferroterm-core` has no rendering and no I/O: you `feed()` it bytes from a PTY
and read a packed grid `snapshot()` back. This crate wires that to Slint:

- **Rendering.** Slint has no raw per-pixel canvas element. One live element per
  cell (1920 for an 80×24 grid) is the wrong shape, so instead the whole grid is
  **software-rasterized** on the CPU (`src/raster.rs`, using `fontdue`) into a
  flat RGBA buffer, wrapped in a `SharedPixelBuffer`, and displayed as a single
  Slint `Image`. Font-face discovery and synthetic bold/italic mirror the wgpu
  app's atlas. The rasterizer holds no Slint types, so it's testable headlessly.
- **Input.** A `FocusScope` (`ui/main.slint`) captures every key press and hands
  the raw `event.text` plus modifier flags to Rust, which maps them onto
  `ferroterm-core`'s key encoder (`src/keymap.rs`). Named keys arrive as the
  private-use / control codepoints Slint documents and are decoded there.
- **Shell.** `portable-pty` spawns `$SHELL` on a background thread that streams
  output over a channel (`src/pty.rs`); a ~60 Hz `slint::Timer` drains it, feeds
  the terminal, and repaints only when something changed.

Colors, key encoding and the Tokyo Night theme are shared with the web and wgpu
front-ends, so a shell looks the same across all of them.

## Scope

Working: shell I/O, keyboard (incl. arrows / F-keys / Home / End / PageUp-Down),
256-color + truecolor, wide/CJK cells, bold/italic (real faces where the system
provides them, synthetic shear/dilation otherwise), underline/strikethrough,
inverse/dim, a blinking block cursor, live resize and HiDPI.

Intentionally left to the fuller [`apps/native`](../native) app: mouse
selection + clipboard, scrollback viewing, inline images, OSC 8 hyperlinks, and
color emoji.
