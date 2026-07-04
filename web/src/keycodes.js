// Maps DOM `KeyboardEvent.key` names to the numeric key codes understood by the
// WASM core's `Terminal.key(code, mods)` (see `decode_keycode` in the Rust
// wasm crate). Printable keys are handled separately via `Terminal.char()`.

export const KEY = {
  ArrowUp: 1,
  ArrowDown: 2,
  ArrowRight: 3,
  ArrowLeft: 4,
  Home: 5,
  End: 6,
  Insert: 7,
  Delete: 8,
  PageUp: 9,
  PageDown: 10,
  Enter: 11,
  Backspace: 12,
  Tab: 13,
  Escape: 14,
  F1: 100, F2: 101, F3: 102, F4: 103, F5: 104, F6: 105,
  F7: 106, F8: 107, F9: 108, F10: 109, F11: 110, F12: 111,
};

/** Pack a DOM event's modifier state into the core's bitmask. */
export function modMask(e) {
  let m = 0;
  if (e.shiftKey) m |= 1;
  if (e.altKey) m |= 2;
  if (e.ctrlKey) m |= 4;
  if (e.metaKey) m |= 8;
  return m;
}
