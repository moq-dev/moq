//! What [`crate::moq_decode_audio`] and [`crate::moq_decode_video`] deliver besides frames.
//!
//! Both decoders hand over the same three kinds of read as `moq_audio::decode::Read` and
//! `moq_video::decode::Read`, so a C player handles loss and ends alike for every media type.

use crate::Error;

/// Which kind of read a decoder delivered: a decoded frame, or a break between frames.
#[repr(C)]
#[expect(non_camel_case_types, reason = "C ABI type names match moq.h")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum moq_decode_read_kind {
	/// A decoded frame.
	MOQ_DECODE_READ_FRAME = 0,
	/// Media after what was already read was lost: a group was skipped or never arrived.
	/// What follows does not continue what came before.
	MOQ_DECODE_READ_GAP = 1,
	/// The publisher stopped its encoder: it paused, switched source, or finished. What
	/// follows comes from a fresh encoder.
	MOQ_DECODE_READ_END = 2,
}

/// Lost media, for a [moq_decode_read_kind::MOQ_DECODE_READ_GAP] read.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct moq_decode_gap {
	/// The furthest presentation point already read, in microseconds.
	pub timestamp_us: u64,
	/// An upper bound on how long until the next frame resumes, in microseconds.
	pub duration_us: u64,
}

/// A delivered read held by its id, with its timestamps already in the C ABI's microseconds.
#[derive(Clone)]
pub(crate) enum Read<F> {
	Frame(F),
	Gap(moq_decode_gap),
	End { end_us: u64 },
}

impl<F> Read<F> {
	pub(crate) fn gap(timestamp: moq_net::Timestamp, duration: moq_net::Timestamp) -> Result<Self, Error> {
		Ok(Self::Gap(moq_decode_gap {
			timestamp_us: micros(timestamp)?,
			duration_us: micros(duration)?,
		}))
	}

	pub(crate) fn end(end: moq_net::Timestamp) -> Result<Self, Error> {
		Ok(Self::End { end_us: micros(end)? })
	}

	/// The C kind, plus the gap and end fields a non-frame read fills.
	pub(crate) fn info(&self) -> (moq_decode_read_kind, moq_decode_gap, u64) {
		match self {
			Self::Frame(_) => (
				moq_decode_read_kind::MOQ_DECODE_READ_FRAME,
				moq_decode_gap::default(),
				0,
			),
			Self::Gap(gap) => (moq_decode_read_kind::MOQ_DECODE_READ_GAP, *gap, 0),
			Self::End { end_us } => (
				moq_decode_read_kind::MOQ_DECODE_READ_END,
				moq_decode_gap::default(),
				*end_us,
			),
		}
	}
}

fn micros(timestamp: moq_net::Timestamp) -> Result<u64, Error> {
	timestamp
		.as_micros()
		.try_into()
		.map_err(|_| Error::TimestampOverflow(moq_net::TimeOverflow))
}
