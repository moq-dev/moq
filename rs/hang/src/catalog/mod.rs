//! The catalog describes available media tracks and codecs.
//!
//! This is a JSON blob that can be live updated like any other track in MoQ.
//! It describes the available audio and video tracks, including codec information,
//! resolution, bitrates, and other metadata.

mod audio;
mod container;
mod root;
mod video;

pub use audio::*;
pub use container::*;
pub use root::*;
pub use video::*;
