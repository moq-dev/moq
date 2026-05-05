//! Wire-level container abstraction shared by the import/export pipelines.
//!
//! A moq-lite group carries a sequence of frames. *How* a media frame is encoded inside
//! each moq-lite frame depends on the container format:
//!
//! - **Hang Legacy**: a VarInt timestamp prefix followed by the raw codec bitstream.
//!   One media frame per moq-lite frame.
//! - **CMAF**: ISO-BMFF moof+mdat atoms. A single moq-lite frame can carry multiple
//!   samples (one fragment).
//!
//! [`Container`] abstracts these into a shared write/read interface. The [`Hang`] enum
//! is a runtime-dispatched [`Container`] that picks the format based on a hang catalog,
//! so callers don't need to thread a generic parameter through user code.

use std::task::Poll;

use bytes::Bytes;

pub(crate) mod cmaf;
mod consumer;
mod hang;
mod producer;

pub use cmaf::{Cmaf, Error as CmafError};
pub use consumer::Consumer;
pub use hang::Hang;
pub use producer::Producer;

/// Microsecond presentation timestamp, the canonical timebase for media frames in moq-mux.
pub type Timestamp = moq_lite::Timescale<1_000_000>;

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
/// - The Hang Legacy implementation writes one media frame per moq-lite frame
///   (timestamp + payload).
/// - The CMAF implementation packs N samples into a single moof+mdat moq-lite frame.
///
/// Most callers should use [`Hang`] (catalog-driven) rather than picking a concrete
/// container directly.
pub trait Container {
	/// Container-specific error. All variants must be convertible from [`moq_lite::Error`]
	/// so the IO layer's errors propagate cleanly.
	type Error: std::error::Error + Send + Sync + Unpin + From<moq_lite::Error>;

	/// Encode one or more frames into a single moq-lite frame appended to `group`.
	fn write(&self, group: &mut moq_lite::GroupProducer, frames: &[Frame]) -> Result<(), Self::Error>;

	/// Poll the next moq-lite frame from `group` and decode it into media frames.
	///
	/// Returns `Ok(None)` when the group has ended. A single call may decode multiple
	/// media frames (e.g. all samples in a CMAF fragment).
	fn poll_read(
		&self,
		group: &mut moq_lite::GroupConsumer,
		waiter: &conducer::Waiter,
	) -> Poll<Result<Option<Vec<Frame>>, Self::Error>>;

	/// Async wrapper around [`Self::poll_read`].
	fn read(
		&self,
		group: &mut moq_lite::GroupConsumer,
	) -> impl std::future::Future<Output = Result<Option<Vec<Frame>>, Self::Error>>
	where
		Self: Sync,
	{
		async { conducer::wait(|waiter| self.poll_read(group, waiter)).await }
	}
}
