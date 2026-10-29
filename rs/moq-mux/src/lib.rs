#![warn(missing_docs)]
#![cfg_attr(
	test,
	expect(clippy::disallowed_methods, reason = "tests read the host clock directly")
)]

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
//! - [`rate`] turns bandwidth grants into stable media bitrate targets shared
//!   by audio, video, and transcode senders.
//! - [`hls`](mod@hls) publishes HLS segment records per rendition by default, so
//!   an edge can build HLS/DASH playlists without downloading media.
//! - [`follow`] follows a broadcast across the publisher instances that
//!   serve it, for a reader that restarts on each one.
//! - [`Linger`] keeps a cleanly ended broadcast up for a fixed time, so a
//!   subscriber that arrives after the end still gets its tail.

pub mod catalog;
mod clock;
pub mod codec;
pub mod container;
mod error;
pub mod follow;
pub mod hls;
pub mod import;
mod jitter;
pub mod json;
mod linger;
mod pace;
pub mod rate;
pub mod select;
mod source;
mod stats;

pub use clock::Clock;
pub use error::*;
pub use linger::Linger;
pub use pace::Pacer;
pub use source::{Binding, Source};
