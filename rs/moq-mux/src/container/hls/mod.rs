//! HLS playlist ingest.
//!
//! Watches an HLS master or media playlist, downloads each fMP4 segment
//! as it appears, and feeds it through the fMP4 importer. Import-only;
//! moq-mux doesn't emit HLS today.

mod import;

pub use import::*;
