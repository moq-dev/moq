//! Native playback for a MoQ broadcast.
//!
//! The `play` feature gates the device stacks: the winit event loop, the wgpu
//! window, and the cpal speaker. Everything the player decides rather than
//! draws (argument validation, rendition selection, media timing) compiles
//! without them, so a default build typechecks it and runs its tests.
//!
//! The media tasks need the decoders, so they stay behind `play`, but they
//! reach the devices only through `output`: tests drive them against a
//! recorder (`fake`), and `just rs play` runs those on every PR that reaches
//! this crate.

// With `play` off the event loop is gone and nothing calls the modules below.
// They are still compiled and tested, which is the point.
#![cfg_attr(not(feature = "play"), allow(dead_code))]

mod args;
mod buffer;
mod layout;
mod playback;
mod source;
mod timeline;

#[cfg(all(test, feature = "play"))]
mod fake;
#[cfg(feature = "play")]
mod media;
#[cfg(feature = "play")]
mod output;
#[cfg(feature = "play")]
mod video;
#[cfg(feature = "play")]
mod window;

#[cfg(feature = "play")]
pub use args::Args;
#[cfg(feature = "play")]
pub use window::run;
