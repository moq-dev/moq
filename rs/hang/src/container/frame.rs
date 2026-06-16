use bytes::{Buf, Bytes, BytesMut};
use derive_more::Debug;
use moq_net::VarInt;

use crate::Error;

pub use moq_net::{Timescale, Timestamp};

/// Canonical timescale for the hang legacy wire format: microseconds.
///
/// The legacy container's on-wire timestamp is a single VarInt with no scale tag,
/// so encoders normalize to this scale and decoders attach it.
pub const TIMESCALE: Timescale = Timescale::MICRO;

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
	/// VarInt timestamp prefix followed by the raw codec payload.
	///
	/// The timestamp is normalized to [`TIMESCALE`] (microseconds) on the wire so
	/// peers using a different source scale (e.g. nanoseconds from MKV) can decode
	/// without knowing the producer's internal scale.
	pub fn encode(&self, group: &mut moq_net::GroupProducer) -> Result<(), Error> {
		let timestamp = self.timestamp.convert(TIMESCALE)?;
		let value = VarInt::try_from(timestamp.value()).map_err(moq_net::Error::from)?;

		let mut header = BytesMut::new();
		value.encode_quic(&mut header).map_err(moq_net::Error::from)?;

		let size = (header.len() + self.payload.len()) as u64;

		// Stamp the moq-net frame timestamp too so Lite05+ can delta-encode it on the
		// wire independently of the container-level prefix.
		let net_frame = moq_net::Frame {
			size,
			timestamp: Some(timestamp),
		};
		let mut chunked = group.create_frame(net_frame)?;
		chunked.write(header.freeze())?;
		chunked.write(self.payload.clone())?;
		chunked.finish()?;

		Ok(())
	}

	/// Decode a frame from raw bytes (VarInt timestamp prefix + payload).
	///
	/// Attaches [`TIMESCALE`] (microseconds) to the decoded timestamp, matching what
	/// [`Self::encode`] writes.
	pub fn decode(mut buf: impl Buf) -> Result<Self, Error> {
		let value: u64 = VarInt::decode_quic(&mut buf).map_err(moq_net::Error::from)?.into();
		let timestamp = Timestamp::new(value, TIMESCALE)?;
		let payload = buf.copy_to_bytes(buf.remaining());

		Ok(Self { timestamp, payload })
	}
}
