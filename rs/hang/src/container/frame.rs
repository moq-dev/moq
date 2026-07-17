use bytes::{Buf, BufMut, Bytes, BytesMut};
use derive_more::Debug;
use moq_net::VarInt;

use crate::Error;

pub use moq_net::{Timescale, Timestamp};

/// Canonical timescale for the hang legacy wire format: microseconds.
///
/// The legacy container's on-wire timestamp is a single VarInt with no scale tag,
/// so encoders normalize to this scale and decoders attach it.
pub const TIMESCALE: Timescale = Timescale::MICRO;

/// Track properties for creating a track that carries [`Frame`]s, via
/// [`create_track`](moq_net::broadcast::Producer::create_track) or
/// [`accept`](moq_net::track::Request::accept).
///
/// This pins the track's timescale to [`TIMESCALE`]. `moq_net::track::Info::default()`
/// is milliseconds, which would quantize the net-level frame timestamps that
/// moq-lite-05 and later delta-encode on the wire, even though the container prefix
/// stays at microseconds.
pub fn track_info() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(TIMESCALE)
}

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
	/// Encode the frame: VarInt timestamp prefix followed by the raw codec payload.
	///
	/// The timestamp is normalized to [`TIMESCALE`] (microseconds) so peers using a
	/// different source scale (e.g. nanoseconds from MKV) can decode without knowing
	/// the producer's internal scale. Inverse of [`Self::decode`].
	pub fn encode(&self, buf: &mut impl BufMut) -> Result<(), Error> {
		buf.put(self.header()?);
		buf.put(self.payload.clone());
		Ok(())
	}

	/// Decode a frame from raw bytes (VarInt timestamp prefix + payload).
	///
	/// Attaches [`TIMESCALE`] (microseconds) to the decoded timestamp, matching what
	/// [`Self::encode`] writes. Inverse of [`Self::encode`].
	pub fn decode(mut buf: impl Buf) -> Result<Self, Error> {
		let value: u64 = VarInt::decode_quic(&mut buf).map_err(moq_net::Error::from)?.into();
		let timestamp = Timestamp::new(value, TIMESCALE)?;
		let payload = buf.copy_to_bytes(buf.remaining());

		Ok(Self { timestamp, payload })
	}

	/// Write the frame to `group` as a single moq-lite frame, in the [`Self::encode`] format.
	///
	/// Prefer this over [`Self::encode`] when writing to a group: it streams the header and
	/// payload as separate chunks rather than copying the payload into one buffer, and stamps
	/// the moq-net frame timestamp so moq-lite-05 and later can delta-encode it on the wire
	/// independently of the container-level prefix.
	pub fn write_to(&self, group: &mut moq_net::group::Producer) -> Result<(), Error> {
		let header = self.header()?;
		let size = (header.len() + self.payload.len()) as u64;

		// `create_frame` converts the timestamp into the track's timescale; older drafts
		// simply don't put it on the wire.
		let info = moq_net::frame::Info {
			size,
			timestamp: self.timestamp,
		};
		let mut chunked = group.create_frame(info)?;
		chunked.write(header)?;
		chunked.write(self.payload.clone())?;
		chunked.finish()?;

		Ok(())
	}

	/// The VarInt timestamp prefix, normalized to [`TIMESCALE`].
	fn header(&self) -> Result<Bytes, Error> {
		let timestamp = self.timestamp.convert(TIMESCALE)?;
		let value = VarInt::try_from(timestamp.value()).map_err(moq_net::Error::from)?;

		let mut header = BytesMut::new();
		value.encode_quic(&mut header).map_err(moq_net::Error::from)?;

		Ok(header.freeze())
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn encode_decode_roundtrip() {
		let frame = Frame {
			timestamp: Timestamp::from_micros(1_234_567).expect("timestamp"),
			payload: Bytes::from_static(b"hello"),
		};

		let mut buf = BytesMut::new();
		frame.encode(&mut buf).expect("encode");

		let decoded = Frame::decode(buf.freeze()).expect("decode");
		assert_eq!(decoded.timestamp, frame.timestamp);
		assert_eq!(decoded.payload, frame.payload);
	}

	#[test]
	fn encode_normalizes_timescale() {
		// A nanosecond-scale source (e.g. MKV) still lands on the wire as microseconds.
		let frame = Frame {
			timestamp: Timestamp::new(1_234_567_000, Timescale::NANO).expect("timestamp"),
			payload: Bytes::from_static(b"hello"),
		};

		let mut buf = BytesMut::new();
		frame.encode(&mut buf).expect("encode");

		let decoded = Frame::decode(buf.freeze()).expect("decode");
		assert_eq!(decoded.timestamp, Timestamp::from_micros(1_234_567).expect("timestamp"));
	}

	#[test]
	fn track_info_uses_container_timescale() {
		assert_eq!(track_info().timescale, TIMESCALE);
	}
}
