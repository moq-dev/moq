//! # hang: WebCodecs compatible media encoding for MoQ
//!
//! Media-specific library built on [moq_net] for streaming audio and video with WebCodecs.
//!
//! Each `hang` broadcast consists of:
//!
//! - **Catalog**: A JSON track containing codec info and track metadata, updated live as tracks change.
//! - **Tracks**: Audio or video, supporting one or more renditions.
//! - **HLS records**: An optional JSON track per rendition marking each of its HLS segments' first group
//!   and pts, so an edge can render playlists without downloading media. Advertised by the rendition's `hls` field.
//!
//! Each track specifies a container format:
//! - **Legacy**: A timestamp followed by the codec payload.
//! - **CMAF**: Fragmented MP4 container (moof+mdat pair)
//!
//! See the [moq-mux](https://crates.io/crates/moq-mux) crate for importing existing media formats into hang broadcasts.

#![warn(missing_docs)]

mod error;
mod snapshot;

/// Publisher media and transport snapshots.
pub mod stats;

/// Solicited viewer feedback and echo broadcast names.
pub mod echo;

/// Client health graded from two consecutive media reports.
pub mod health;

/// The catalog is used to describe the available media tracks and codecs.
pub mod catalog;

/// The container is the contents of each media track.
pub mod container;

/// HLS segment records, optional per rendition.
pub mod hls;

/// Export the moq-net version we use.
pub use moq_net;

pub use catalog::Catalog;
pub use error::*;
