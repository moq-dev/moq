//! # hang: WebCodecs compatible media encoding for MoQ
//!
//! Media-specific library built on [moq_lite] for streaming audio and video with WebCodecs.
//!
//! Each `hang` broadcast consists of:
//!
//! - **Catalog**: A JSON track containing codec info and track metadata, updated live as tracks change.
//! - **Tracks**: Audio or video, supporting one or more renditions.
//!
//! Each track specifies a container format:
//! - **Legacy**: A timestamp followed by the codec payload.
//! - **CMAF**: Fragmented MP4 container (moof+mdat pair)
//!
//! See the [moq-mux](https://crates.io/crates/moq-mux) crate for importing existing media formats into hang broadcasts.
mod error;

/// The catalog is used to describe the available media tracks and codecs.
pub mod catalog;

/// The container is the contents of each media track.
pub mod container;

/// Export the moq-lite version we use.
pub use moq_lite;

pub use catalog::{Catalog, CatalogConsumer, CatalogGuard, CatalogProducer};
pub use error::*;
