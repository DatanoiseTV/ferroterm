//! Window-independent pieces of the Slint terminal: color palette, snapshot
//! decoder, key mapping, PTY session and the CPU rasterizer. The binary
//! (`main.rs`) wires these to a Slint window; the integration test drives the
//! decoder + rasterizer without opening one.

pub mod keymap;
pub mod palette;
pub mod pty;
pub mod raster;
pub mod snapshot;
