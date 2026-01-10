//! The catalog describes available media tracks and codecs.
//!
//! This module provides JSON-based catalog functionality that allows broadcasters
//! to describe their available audio and video tracks, including codec information,
//! resolution, bitrates, and other metadata. Consumers can subscribe to catalog
//! tracks to discover and choose appropriate tracks for their capabilities.

mod audio;
mod chat;
mod preview;
mod produce;
mod root;
mod track;
mod user;
mod video;

pub use audio::*;
pub use chat::*;
pub use preview::*;
pub use produce::*;
pub use root::*;
pub use track::*;
pub use user::*;
pub use video::*;
