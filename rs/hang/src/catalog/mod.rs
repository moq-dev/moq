//! The catalog describes available media tracks and codecs.
//!
//! This is a JSON blob that can be live updated like any other track in MoQ.
//! It describes the available audio and video tracks, including codec information,
//! resolution, bitrates, and other metadata.

mod audio;
mod chat;
mod consumer;
mod container;
mod preview;
mod producer;
mod root;
mod user;
mod video;

pub use audio::*;
pub use chat::*;
pub use consumer::*;
pub use container::*;
pub use preview::*;
pub use producer::*;
pub use root::*;
pub use user::*;
pub use video::*;
