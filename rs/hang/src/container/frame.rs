use bytes::{Buf, Bytes, BytesMut};
use derive_more::Debug;
use moq_net::coding::VarInt;

use crate::Error;

pub use moq_net::Timestamp;

/// Canonical timescale for hang frame timestamps: microseconds.
pub const TIMESCALE: moq_net::Timescale = moq_net::Timescale::MICRO;

/// Re-export so callers don't need a direct `moq_net` import to refer to the
/// hang container timescale by type.
pub type Timescale = moq_net::Timescale;

/// A media frame with a timestamp and codec-specific payload.
///
/// Frames are the fundamental unit of media data in hang. Each frame contains:
/// - A timestamp when they should be rendered.
/// - A codec-specific payload.
#[derive(Clone, Debug)]
pub struct Frame {
	/// The presentation timestamp for this frame.
	///
	/// This indicates when the frame should be displayed relative to the
	/// start of the stream or some other reference point.
	/// This is NOT a wall clock time.
	pub timestamp: Timestamp,

	/// The encoded media data for this frame.
	///
	/// The format depends on the codec being used (H.264, AV1, Opus, etc.).
	/// The debug implementation shows only the payload length for brevity.
	#[debug("{} bytes", payload.len())]
	pub payload: Bytes,
}

impl Frame {
	/// Encode the frame to the given group as a single moq-lite frame:
	/// VarInt timestamp prefix followed by the raw codec payload. Also stamps
	/// the moq-net [`moq_net::Frame::timestamp`] so the wire layer can
	/// delta-encode it independently on Lite05+ (the container-level prefix
	/// stays as a duplicate for now).
	pub fn encode(&self, group: &mut moq_net::GroupProducer) -> Result<(), Error> {
		// Normalize to the hang container timescale on the wire so peers using a
		// different source scale (e.g. nanoseconds from MKV) can decode without
		// knowing the producer's internal scale.
		let timestamp = self.timestamp.convert(TIMESCALE)?;

		let mut header = BytesMut::new();
		let value = VarInt::try_from(timestamp.value()).map_err(moq_net::Error::from)?;
		value.encode_quic(&mut header).map_err(moq_net::Error::from)?;

		let size = (header.len() + self.payload.len()) as u64;

		let net_frame = moq_net::Frame { size, timestamp };
		let mut chunked = group.create_frame(net_frame)?;
		chunked.write(header.freeze())?;
		chunked.write(self.payload.clone())?;
		chunked.finish()?;

		Ok(())
	}

	/// Decode a frame from raw bytes (VarInt timestamp prefix + payload).
	pub fn decode(mut buf: impl Buf) -> Result<Self, Error> {
		let value: u64 = VarInt::decode_quic(&mut buf).map_err(moq_net::Error::from)?.into();
		let timestamp = Timestamp::from_micros(value)?;
		let payload = buf.copy_to_bytes(buf.remaining());

		Ok(Self { timestamp, payload })
	}
}
