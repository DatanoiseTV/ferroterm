//! Reusable, window-independent pieces of the native terminal: the glyph
//! atlas, color palette, snapshot decoder and the wgpu renderer. The binary
//! (`main.rs`) wires these to a winit window + PTY; the render test drives the
//! same renderer against an offscreen texture.

pub mod atlas;
pub mod images;
pub mod palette;
pub mod renderer;
pub mod snapshot;
