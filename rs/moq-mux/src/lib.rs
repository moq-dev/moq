//! Media muxers and demuxers for MoQ.
//!
//! Sits between [`moq_net`] (pub/sub transport) and [`hang`] (media
//! catalog). Takes containerized media in, produces a moq broadcast,
//! and the other way around.
//!
//! - [`container`](mod@container) holds one submodule per container
//!   format. Each describes how media frames are packaged on the wire,
//!   and some also handle the corresponding file or stream format.
//! - [`codec`] holds one submodule per codec. Each parses the codec's
//!   configuration record and provides an importer that publishes a
//!   raw bitstream to a broadcast.
//! - [`catalog`] publishes and subscribes to the broadcast catalog,
//!   the JSON manifest listing every track and how to decode it.
//! - [`import`](mod@import) is the front door for callers who only have
//!   a format string. It picks the right concrete importer for you.
//! - [`select`] picks which renditions of a broadcast to keep, on either
//!   the import or the consume side.

pub mod catalog;
mod clock;
pub mod codec;
pub mod container;
mod error;
pub mod import;
pub mod select;

pub use clock::Clock;
pub use error::*;

/// Re-export of the [`mp4_atom`] crate, whose types appear in the public CMAF
/// surface ([`container::fmp4`]). A major version bump of `mp4_atom` is a
/// breaking change for moq-mux.
pub use mp4_atom;
