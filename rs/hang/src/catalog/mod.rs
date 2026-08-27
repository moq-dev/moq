//! The catalog describes the tracks a broadcast publishes.
//!
//! This is a JSON blob that can be live updated like any other track in MoQ.
//! It describes the available audio and video tracks, including codec information,
//! resolution, bitrates, and other metadata, plus the `json` and `binary` sections
//! listing the data tracks that aren't media.

mod audio;
mod binary;
mod compression;
mod container;
mod hex;
mod json;
mod mode;
mod priority;
mod root;
mod timeline;
mod video;

pub use audio::*;
pub use binary::*;
pub use compression::*;
pub use container::*;
pub use json::*;
pub use mode::*;
pub use priority::*;
pub use root::*;
pub use timeline::*;
pub use video::*;
