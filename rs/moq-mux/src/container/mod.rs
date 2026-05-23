//! Container formats.
//!
//! A moq-lite group is a sequence of frames. Each container submodule
//! decides how a media frame gets encoded into one of those frames:
//!
//! - [`legacy`] — the original hang wire format. Timestamp + payload.
//! - [`loc`] — Low Overhead Container, the IETF draft replacement for Legacy.
//! - [`fmp4`] — ISO-BMFF moof+mdat fragments.
//! - [`mkv`] — Matroska / WebM.
//! - [`hls`] — HLS playlist ingest.
//!
//! Wire-level containers implement the [`Container`] trait. [`Hang`] is a
//! runtime-dispatched enum that picks one based on a hang catalog entry,
//! so most callers stay generic.

use std::task::Poll;

use bytes::Bytes;

mod consumer;
mod hang;
pub(crate) mod jitter;
mod producer;
mod source;

pub mod fmp4;
pub mod hls;
pub mod legacy;
pub mod loc;
pub mod mkv;

pub use consumer::Consumer;
pub use hang::Hang;
pub use producer::Producer;
pub(crate) use source::{CatalogSource, ExportSource};

/// Microsecond presentation timestamp, the canonical timebase for media frames in moq-mux.
pub type Timestamp = moq_net::Timescale<1_000_000>;

/// A decoded media frame: timestamp, payload bytes, keyframe flag.
///
/// `payload` is the raw codec bitstream — what gets decoded by the eventual player.
/// The exact format depends on the codec (Annex B for H.264 / H.265, OBU for AV1, etc.).
#[derive(Clone, Debug)]
pub struct Frame {
	/// Presentation timestamp.
	///
	/// Microsecond precision. Frames within a track must be in *decode* order (i.e. the
	/// order the decoder consumes them); B-frames may have non-monotonic presentation
	/// timestamps.
	pub timestamp: Timestamp,

	/// Encoded codec payload.
	pub payload: Bytes,

	/// Whether this frame is a keyframe.
	///
	/// In the Legacy wire format, keyframes are inferred from group boundaries (the first
	/// frame of a group is a keyframe). In CMAF, the trun sample-flags carry the truth.
	pub keyframe: bool,
}

/// Encode/decode media frames over a moq-lite group.
///
/// Implementors choose how multiple [`Frame`]s map onto moq-lite frames:
///
/// - The [`legacy`] implementation writes one media frame per moq-lite frame.
/// - The [`fmp4`] implementation packs N samples into a single moof+mdat moq-lite frame.
/// - The [`loc`] implementation writes one LOC-framed media frame per moq-lite frame.
///
/// Most callers should use [`Hang`] (catalog-driven) rather than picking a concrete
/// container directly.
pub trait Container {
	/// Container-specific error. All variants must be convertible from [`moq_net::Error`]
	/// so the IO layer's errors propagate cleanly.
	type Error: std::error::Error + Send + Sync + Unpin + From<moq_net::Error>;

	/// Encode one or more frames into a single moq-lite frame appended to `group`.
	fn write(&self, group: &mut moq_net::GroupProducer, frames: &[Frame]) -> Result<(), Self::Error>;

	/// Poll the next moq-lite frame from `group` and decode it into media frames.
	///
	/// Returns `Ok(None)` when the group has ended. A single call may decode multiple
	/// media frames (e.g. all samples in a CMAF fragment).
	fn poll_read(
		&self,
		group: &mut moq_net::GroupConsumer,
		waiter: &conducer::Waiter,
	) -> Poll<Result<Option<Vec<Frame>>, Self::Error>>;

	/// Async wrapper around [`Self::poll_read`].
	fn read(
		&self,
		group: &mut moq_net::GroupConsumer,
	) -> impl std::future::Future<Output = Result<Option<Vec<Frame>>, Self::Error>>
	where
		Self: Sync,
	{
		async { conducer::wait(|waiter| self.poll_read(group, waiter)).await }
	}
}
