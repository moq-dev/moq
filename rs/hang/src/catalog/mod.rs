//! The catalog describes available media tracks and codecs.
//!
//! This is a JSON blob that can be live updated like any other track in MoQ.
//! The catalog is a flat JSON object where each top-level key is a "section"
//! that can be independently registered, read, and written with typed schemas.

mod audio;
mod consumer;
mod container;
mod reader;
mod section;
mod video;
mod writer;

pub use audio::*;
pub use consumer::*;
pub use container::*;
pub use reader::*;
pub use section::*;
pub use video::*;
pub use writer::*;
