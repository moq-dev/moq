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

/// How long a media track asks its publisher (and, through TRACK_INFO, every relay) to keep a
/// non-latest group fetchable.
///
/// Media is the one thing on a broadcast that is read as HISTORY rather than followed at the live
/// edge: a segmented egress (HLS/DASH) may only advertise segments a FETCH can still reach, and a
/// standard player starts several target durations behind live. `moq_net`'s conservative default
/// is sized for a live-edge follower and leaves such a player addressing groups that are already
/// gone.
///
/// Declared per track rather than by raising that default, so the tracks that do NOT index history
/// keep the cheap default: the catalog is snapshot mode and the timeline is a single never-rolled
/// group, and in both the useful value is the live edge, which is retained unconditionally.
///
/// Raising this does not make anyone play further behind live. It is a retention budget and a
/// CEILING on what a subscriber may ask to wait for; a subscriber's own
/// [`Subscription::latency_max`](moq_net::track::Subscription::latency_max) defaults to zero (skip
/// the moment a newer group arrives) and should be set from that consumer's real latency target.
pub const LATENCY_MAX: std::time::Duration = std::time::Duration::from_secs(30);

/// Track properties for creating a track that carries [`Frame`]s, via
/// [`create_track`](moq_net::broadcast::Producer::create_track) or
/// [`accept`](moq_net::track::Request::accept).
///
/// This pins the track's timescale to [`TIMESCALE`]. `moq_net::track::Info::default()`
/// is milliseconds, which would quantize the net-level frame timestamps that
/// moq-lite-05 and later delta-encode on the wire, even though the container prefix
/// stays at microseconds.
pub fn track_info() -> moq_net::track::Info {
	track_info_at(TIMESCALE)
}

/// [`track_info`] at an explicit timescale, for a container that carries the source's own
/// (CMAF and Matroska both do) rather than normalizing to [`TIMESCALE`].
///
/// Every media track should be created through one of these two rather than
/// `moq_net::track::Info::default()`, so [`LATENCY_MAX`] is declared in one place instead of at
/// each call site -- a track that silently keeps the default is one a segmented egress cannot
/// serve a full playlist window from.
pub fn track_info_at(timescale: Timescale) -> moq_net::track::Info {
	moq_net::track::Info::default()
		.with_timescale(timescale)
		.with_latency_max(LATENCY_MAX)
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
		self.encode_header(buf)?;
		buf.put_slice(&self.payload);
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
		let mut header = BytesMut::new();
		self.encode_header(&mut header)?;
		let header = header.freeze();

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

	/// Write the VarInt timestamp prefix, normalized to [`TIMESCALE`].
	fn encode_header(&self, buf: &mut impl BufMut) -> Result<(), Error> {
		let timestamp = self.timestamp.convert(TIMESCALE)?;
		let value = VarInt::try_from(timestamp.value()).map_err(moq_net::Error::from)?;
		value.encode_quic(buf).map_err(moq_net::Error::from)?;

		Ok(())
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

	#[test]
	fn media_tracks_declare_their_retention() {
		// A media track is read as history (a segmented egress FETCHes segments a playlist
		// advertised), so it declares a retention rather than inheriting the live-edge default.
		// Both constructors must carry it: `track_info_at` exists for the containers that keep
		// the source's timescale, and it is exactly those that would otherwise fall back to
		// `Info::default()` and quietly lose the retention.
		assert_eq!(track_info().latency_max, LATENCY_MAX);
		assert_eq!(track_info_at(Timescale::MILLI).latency_max, LATENCY_MAX);
		assert_eq!(track_info_at(Timescale::MILLI).timescale, Timescale::MILLI);
		assert!(LATENCY_MAX > moq_net::track::DEFAULT_LATENCY_MAX);
	}

	#[test]
	fn non_media_tracks_keep_the_default_retention() {
		// The catalog is snapshot mode and the timeline is a single never-rolled group: in both
		// the useful value is the live edge, which is retained unconditionally, so neither pays
		// for history it never serves.
		assert_eq!(
			crate::Catalog::default_track_info().latency_max,
			moq_net::track::DEFAULT_LATENCY_MAX
		);
	}
}
