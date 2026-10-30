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

use bytes::Bytes;

mod consumer;
mod declared;
mod decode;
mod group;
mod producer;
mod source;
#[cfg(test)]
pub(crate) mod test_util;
mod text;
mod ttml;

pub mod flv;
pub mod fmp4;
pub mod legacy;
pub mod loc;
pub mod mkv;
pub mod ts;

pub use consumer::{Consumer, DEFAULT_MAX_BUFFER};
pub use group::GroupConsumer;
pub use producer::Producer;
pub(crate) use source::{ExportSource, Resume};

/// The media role that determines how a container represents frame durations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
	/// Audio packets carry codec-defined durations.
	Audio,
	/// Video frames can need a duration marker at group end.
	Video,
	/// Opaque data has no sample-duration semantics.
	Data,
}

/// One item of a [`Consumer`] read, in delivery order.
#[derive(Clone, Debug)]
pub enum Read {
	/// A media frame from `group`.
	Frame {
		/// The group's sequence number. Only equality means anything: numbers may skip.
		group: u64,
		/// The media frame delivered by the group.
		frame: Frame,
	},
	/// Media after an item already read will never reach this reader: a group was skipped,
	/// evicted, or never arrived. A decoder resumes from here without the references or
	/// state that media built. Joining mid-track is not a gap: nothing was read before it.
	///
	/// The span is an upper bound, in the scale of the item that follows.
	Gap {
		/// The furthest presentation point already read. Without a per-frame duration this
		/// is the last frame's timestamp, one frame early.
		timestamp: moq_net::Timestamp,
		/// How long until the next item resumes, so it also covers any declared pause.
		duration: moq_net::Timestamp,
	},
	/// The publisher declared that its media stops at `end`: it paused, switched source, or
	/// finished. Frames that follow from the same `group` are the stopped encoder's final
	/// packets (an audio codec's drain), whose output at or past `end` is padding. The next
	/// group starts fresh.
	End {
		/// The group's sequence number. Only equality means anything: numbers may skip.
		group: u64,
		/// Where the media stops, at the scale the marker was encoded with.
		end: moq_net::Timestamp,
	},
}

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

	/// Sample duration in the frame's own scale, when the container reports it.
	///
	/// CMAF carries a per-sample duration (trun sample-duration). Legacy and LOC
	/// can fill it from a duration marker that has already landed when a
	/// [`GroupConsumer`] reads the frame, as in a fetched group. Streaming
	/// muxers receive the later endpoint separately, so media stays immediately available.
	/// The [`Consumer`] adds it to `timestamp` to learn how far a group has
	/// presented, so it can advance to a newer group as soon as the gap is
	/// covered instead of waiting out the max delay budget.
	pub duration: Option<moq_net::Timestamp>,

	/// Encoded codec payload.
	pub payload: Bytes,

	/// Whether this frame opens a group, or is a video keyframe.
	///
	/// Containers that carry the bit on the wire (CMAF reads it from
	/// trun sample-flags) set it for video; containers that don't (Legacy,
	/// LOC) leave it `false`. The wrapping [`Consumer`] still asserts
	/// "first frame in a group is a keyframe" as a fallback, so the
	/// Legacy/LOC case lands correctly without anyone having to know. For
	/// audio, whose samples are all independently decodable, that fallback
	/// is the only source: the bit marks the group boundary the publisher
	/// drew, never a per-sample sync flag.
	pub keyframe: bool,
}

/// Stamp `frame` with the duration that ends at `bound`, unless it already has one.
pub(crate) fn close_duration(frame: &mut Frame, bound: moq_net::Timestamp) {
	if frame.duration.is_some() {
		return;
	}
	let Some(delta) = bound.as_micros().checked_sub(frame.timestamp.as_micros()) else {
		return;
	};
	if delta == 0 {
		return;
	}
	let Ok(delta) = u64::try_from(delta) else {
		return;
	};
	if let Ok(duration) = moq_net::Timestamp::from_micros(delta) {
		frame.duration = Some(duration);
	}
}

/// A non-keyframe frame arrived with no open group.
///
/// A track must open with a keyframe (and so must the frame after
/// [`cut`](Producer::cut) / [`seek`](Producer::seek)).
/// [`Producer::write`] returns this so a caller joining mid-stream can skip
/// frames until the first keyframe instead of treating it as fatal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("missing keyframe: a group must open on a keyframe")]
pub struct MissingKeyframe;

/// An explicit endpoint precedes the last frame of an ordered video group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("video group endpoint precedes its last frame")]
pub struct InvalidEnd;

/// A group starts before the previous group did: the source restarted, which is a new broadcast.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
	"frame timestamp {} µs is below the previous group's start {} µs",
	.timestamp.as_micros(),
	.floor.as_micros()
)]
pub struct TimestampRewind {
	/// The refused frame's timestamp.
	pub timestamp: moq_net::Timestamp,
	/// The lowest allowed timestamp: the previous group's start.
	pub floor: moq_net::Timestamp,
}

/// Encode and decode media frames over a moq-lite group.
///
/// Implementors decide how many [`Frame`]s map onto one moq-lite frame:
/// Legacy and LOC write one media frame per moq-lite frame; CMAF can
/// pack many samples into a single moof+mdat fragment.
pub trait Container {
	/// Container-specific error. Must be convertible from [`moq_net::Error`]
	/// (so IO errors propagate), [`MissingKeyframe`], [`InvalidEnd`], and
	/// [`TimestampRewind`] (so the producer can reject invalid group boundaries).
	type Error: std::error::Error
		+ Send
		+ Sync
		+ Unpin
		+ From<moq_net::Error>
		+ From<MissingKeyframe>
		+ From<InvalidEnd>
		+ From<TimestampRewind>;

	/// Encode one or more frames into a single moq-lite frame appended to `group`.
	fn write(&self, group: &mut moq_net::group::Producer, frames: &[Frame]) -> Result<(), Self::Error>;

	/// Decode one moq-lite frame read from a group into media frames.
	///
	/// A single moq-lite frame may carry several media frames (e.g. all samples in a
	/// CMAF fragment), or none (a CMAF fragment with zero samples), which is not the
	/// end of the group.
	///
	/// On a data track an empty batch is the discontinuity marker, so a data
	/// container returns one only for an object with no bytes, and refuses any
	/// other object that decodes to no frames.
	fn decode(&self, frame: moq_net::frame::Frame) -> Result<Vec<Frame>, Self::Error>;

	/// Return the endpoint timestamp when `frame` carries empty-payload metadata.
	///
	/// For video this bounds the preceding frame; for audio it bounds source samples
	/// before terminal codec packets. A consumer never submits the marker to a decoder.
	/// Formats without endpoint metadata use the default.
	fn end(&self, _frame: &Frame) -> Option<moq_net::Timestamp> {
		None
	}

	/// The media role, when this format represents an audio or video track.
	fn kind(&self) -> Kind {
		Kind::Data
	}

	/// Write any format-specific endpoint before the producer closes the group.
	fn finish_group(
		&self,
		_group: &mut moq_net::group::Producer,
		_end: Option<moq_net::Timestamp>,
	) -> Result<(), Self::Error> {
		Ok(())
	}
}
