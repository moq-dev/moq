//! Container formats.
//!
//! A container decides how a media frame is laid out inside a moq-lite
//! frame: framing overhead, whether multiple samples can share one moq
//! frame, and whether the same encoding doubles as a file format on disk.
//!
//! Each submodule implements one format. The wire-level ones implement
//! the [`Container`] trait, so [`Producer<C>`] and [`Consumer<C>`] can
//! be generic over the choice. The catalog announces a container per
//! track; [`catalog::hang::Container`](crate::catalog::hang::Container)
//! dispatches the right implementation at runtime.

use std::task::Poll;

use bytes::Bytes;

mod consumer;
pub(crate) mod jitter;
mod producer;
mod source;

pub mod flv;
pub mod fmp4;
pub mod legacy;
pub mod loc;
pub mod mkv;
pub mod ts;

pub use consumer::Consumer;
pub use producer::Producer;
pub(crate) use source::ExportSource;

/// A decoded media frame: timestamp, payload bytes, keyframe flag.
///
/// `payload` is the raw codec bitstream that gets handed to the decoder.
/// The exact shape depends on the codec (Annex B for H.264/H.265, OBU for
/// AV1, and so on).
#[derive(Clone, Debug)]
pub struct Frame {
	/// Presentation timestamp.
	///
	/// Each container picks its own native scale: fmp4 uses the source
	/// `mdhd.timescale`, mkv uses nanoseconds, legacy is fixed at microseconds.
	/// LOC defaults to microseconds but a decoded frame keeps whatever per-frame
	/// timescale the wire carried, so an exporter can re-emit without forcing
	/// micros. Frames within a track must be in *decode* order, not display
	/// order. B-frames may have non-monotonic presentation timestamps.
	pub timestamp: moq_net::Timestamp,

	/// How long this frame occupies the presentation timeline, in the frame's
	/// own scale, when the container reports it.
	///
	/// CMAF carries a per-sample duration (trun sample-duration); containers
	/// that don't (Legacy, LOC) leave this `None`. The [`Consumer`] adds it to
	/// `timestamp` to learn how far a group has presented, so it can advance to
	/// a newer group as soon as the gap is covered instead of waiting out the
	/// latency budget.
	pub duration: Option<moq_net::Timestamp>,

	/// Encoded codec payload.
	pub payload: Bytes,

	/// Whether this frame is a keyframe.
	///
	/// Containers that carry the bit on the wire (CMAF reads it from
	/// trun sample-flags) should set it; containers that don't (Legacy,
	/// LOC) leave it `false`. The wrapping [`Consumer`] still asserts
	/// "first frame in a group is a keyframe" as a fallback, so the
	/// Legacy/LOC case lands correctly without anyone having to know.
	pub keyframe: bool,
}

/// A non-keyframe frame arrived with no open group.
///
/// A track must open with a keyframe (and so must the frame after
/// [`finish_group`](Producer::finish_group) / [`seek`](Producer::seek)).
/// [`Producer::write`] returns this so a caller joining mid-stream can skip
/// frames until the first keyframe instead of treating it as fatal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("missing keyframe: a group must open on a keyframe")]
pub struct MissingKeyframe;

/// Encode and decode media frames over a moq-lite group.
///
/// Implementors decide how many [`Frame`]s map onto one moq-lite frame:
/// Legacy and LOC write one media frame per moq-lite frame; CMAF can
/// pack many samples into a single moof+mdat fragment.
pub trait Container {
	/// Container-specific error. Must be convertible from [`moq_net::Error`]
	/// (so IO errors propagate) and [`MissingKeyframe`] (so the producer can
	/// reject a group that doesn't open on a keyframe).
	type Error: std::error::Error + Send + Sync + Unpin + From<moq_net::Error> + From<MissingKeyframe>;

	/// Encode one or more frames into a single moq-lite frame appended to `group`.
	fn write(&self, group: &mut moq_net::GroupProducer, frames: &[Frame]) -> Result<(), Self::Error>;

	/// Poll the next moq-lite frame from `group` and decode it into media
	/// frames. Returns `Ok(None)` when the group has ended. A single call
	/// may produce multiple media frames (e.g. all samples in a CMAF
	/// fragment).
	fn poll_read(
		&self,
		group: &mut moq_net::GroupConsumer,
		waiter: &kio::Waiter,
	) -> Poll<Result<Option<Vec<Frame>>, Self::Error>>;

	/// Async wrapper around [`Self::poll_read`].
	fn read(
		&self,
		group: &mut moq_net::GroupConsumer,
	) -> impl std::future::Future<Output = Result<Option<Vec<Frame>>, Self::Error>>
	where
		Self: Sync,
	{
		async { kio::wait(|waiter| self.poll_read(group, waiter)).await }
	}
}
