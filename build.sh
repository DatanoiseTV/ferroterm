#!/usr/bin/env bash
# Build the ferroterm WASM module and emit an ES-module package into web/pkg.
#
# Requires: rustup with the wasm32-unknown-unknown target, and the wasm-bindgen
# CLI matching the wasm-bindgen crate version pinned in crates/wasm/Cargo.toml.
#   rustup target add wasm32-unknown-unknown
#   cargo install wasm-bindgen-cli --version 0.2.122
set -euo pipefail

cd "$(dirname "$0")"

PROFILE="${1:-release}"
OUT_DIR="web/pkg"

echo "==> building ferroterm-wasm ($PROFILE)"
if [ "$PROFILE" = "release" ]; then
  cargo build -p ferroterm-wasm --target wasm32-unknown-unknown --release
  WASM="target/wasm32-unknown-unknown/release/ferroterm_wasm.wasm"
else
  cargo build -p ferroterm-wasm --target wasm32-unknown-unknown
  WASM="target/wasm32-unknown-unknown/debug/ferroterm_wasm.wasm"
fi

echo "==> generating JS bindings -> $OUT_DIR"
rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR"
wasm-bindgen "$WASM" \
  --out-dir "$OUT_DIR" \
  --target web \
  --typescript

# Optional size optimization if wasm-opt is present.
if command -v wasm-opt >/dev/null 2>&1 && [ "$PROFILE" = "release" ]; then
  echo "==> wasm-opt -Oz"
  wasm-opt -Oz "$OUT_DIR/ferroterm_wasm_bg.wasm" -o "$OUT_DIR/ferroterm_wasm_bg.wasm"
fi

echo "==> done"
ls -la "$OUT_DIR"
