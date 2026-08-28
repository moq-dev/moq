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
//! - [`Pacer`] maps each exported frame's media timestamp to the wall-clock
//!   instant it should be delivered at, for byte streams whose spacing is
//!   part of the format (MPEG-TS).
//! - [`timeline`](mod@timeline) publishes the broadcast's segment index: one
//!   record per aligned segment, mapping a span of content time to the group
//!   ranges that carry it on each track, so consumers can seek or build
//!   HLS/DASH playlists without downloading media.

pub mod binary;
pub mod catalog;
mod clock;
pub mod codec;
pub mod container;
mod error;
pub mod import;
pub mod json;
mod pace;
pub mod select;
mod source;
pub mod timeline;

pub use clock::Clock;
pub use error::*;
pub use pace::Pacer;
pub use source::Source;

/// Translate a catalog entry's declared compression into the flag the codecs take.
///
/// An unrecognized algorithm is an error rather than a fallback to plaintext: reading its frames
/// raw would hand the caller garbage.
pub(crate) fn compression(compression: Option<&hang::catalog::Compression>) -> Result<bool> {
	match compression {
		None => Ok(false),
		Some(hang::catalog::Compression::Deflate) => Ok(true),
		Some(other) => Err(Error::UnsupportedCompression(other.to_string())),
	}
}
