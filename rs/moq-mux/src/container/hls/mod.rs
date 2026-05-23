//! HLS playlist ingest.
//!
//! Follows the playlist, downloads each fMP4 segment, and feeds it
//! through the fMP4 importer.

mod import;

pub use import::*;
