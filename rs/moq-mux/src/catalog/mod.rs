//! Hang catalog publish/subscribe.
//!
//! The hang catalog is a JSON track describing a broadcast's audio/video tracks
//! (codec info, init segments, container format). [`Producer`] manages the catalog
//! tracks (both hang-style and MSF-style) and is shared across every codec demuxer
//! in [`crate::import`]. [`Consumer`] subscribes to the hang catalog track and
//! deserializes incoming updates.

mod consumer;
mod msf_consumer;
mod producer;

pub use consumer::*;
pub use msf_consumer::*;
pub use producer::*;
