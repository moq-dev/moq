//! Opaque binary payloads over [`moq-net`](moq_net) tracks, in two modes:
//!
//! - [`snapshot`]: **lossy**. One value updated over time; a consumer only gets the most recent
//!   one. Older values are superseded and dropped.
//! - [`stream`]: **lossless**. An ordered append-log of self-contained payloads, delivered in order
//!   with nothing superseded. Bounded by the group cache: see [`stream`] for what that costs a
//!   consumer that falls behind.
//!
//! Pick [`snapshot`] when consumers care about "what is the value now" (a poster image, a
//! serialized state blob) and [`stream`] when they care about every payload (an event log, a
//! sequence of samples).
//!
//! The bytes are opaque: this crate frames them onto a track and optionally compresses them, and
//! never looks inside. For JSON documents reach for [`moq-json`](https://docs.rs/moq-json) instead,
//! which adds RFC 7396 merge-patch deltas on top of the same two modes.
//!
//! Compression is [`moq-flate`](moq_flate), the same group-scoped DEFLATE moq-json uses, so the two
//! agree on the wire: each group is one raw DEFLATE stream, sync-flushed at every frame boundary. A
//! [`stream`] therefore compresses each payload against the earlier ones in its group, while a
//! [`snapshot`] group holds a single self-contained value.

// The browser transport is `!Send`, so on wasm the shared state behind these `Arc`s is
// too and clippy suggests `Rc`. The same code is genuinely cross-thread on native, so
// `Arc` stays and the lint is unactionable here.
#![cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]

pub mod snapshot;
pub mod stream;

/// Errors produced while publishing or consuming binary payloads.
#[derive(thiserror::Error, Debug, Clone)]
#[non_exhaustive]
pub enum Error {
	/// An error from the underlying track.
	#[error(transparent)]
	Net(#[from] moq_net::Error),

	/// A compressed frame could not be decoded (malformed, truncated, or oversized).
	#[error(transparent)]
	Flate(#[from] moq_flate::Error),

	/// A [`stream`] track carried a second group, which a lossless log cannot do.
	///
	/// A stream is a single group by construction: a publisher that cannot write a payload ends
	/// the track rather than rolling. A second group therefore means the records that would have
	/// completed the first one are gone, so the read fails instead of presenting the remainder as
	/// a continuous log.
	#[error("stream rolled to a second group")]
	Rolled,
}

/// A [`Result`](std::result::Result) using this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
