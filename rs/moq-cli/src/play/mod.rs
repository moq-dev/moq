//! Native playback for a MoQ broadcast.
//!
//! The `play` feature gates the device stacks: the winit event loop, the wgpu
//! window, and the cpal speaker. Everything the player decides rather than
//! draws (argument validation, rendition selection, media timing) compiles
//! without them, so a default build typechecks it and runs its tests.

// With `play` off the event loop is gone and nothing calls the modules below.
// They are still compiled and tested, which is the point.
#![cfg_attr(not(feature = "play"), allow(dead_code))]

mod args;
mod layout;
mod playback;
mod source;
mod timeline;

#[cfg(feature = "play")]
mod media;
#[cfg(feature = "play")]
mod window;

#[cfg(feature = "play")]
pub use args::Args;
#[cfg(feature = "play")]
pub use window::run;
