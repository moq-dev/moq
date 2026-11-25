//! CTS hang container: QUIC varint composition time offset (in track timescale) plus codec payload.
//!
//! Each MoQ frame begins with an unsigned QUIC varint CTS measured in the track's timescale,
//! followed by the raw codec payload. Presentation timestamp is `transport_timestamp + CTS`.
//! Requires a timed track.

use crate::container::{Container, Frame, Kind, legacy};

/// CTS hang wire format configured for the track's media role.
pub struct Wire(
	/// The kind of content carried by the track.
	pub Kind,
);

impl Container for Wire {
	type Error = crate::Error;

	fn end(&self, frame: &Frame) -> Option<moq_net::Timestamp> {
		(self.0 != Kind::Data && frame.payload.is_empty()).then_some(frame.timestamp)
	}

	fn kind(&self) -> Kind {
		self.0
	}

	fn writable(&self) -> Result<(), Self::Error> {
		Err(moq_net::Error::Unsupported.into())
	}

	fn finish_group(
		&self,
		_group: &mut moq_net::group::Producer,
		_end: Option<moq_net::Timestamp>,
	) -> Result<(), Self::Error> {
		// CTS encoding requires a separate DTS per frame (not yet carried by `container::Frame`).
		Err(moq_net::Error::Unsupported.into())
	}

	fn write(&self, _group: &mut moq_net::group::Producer, _frames: &[Frame]) -> Result<(), Self::Error> {
		// CTS needs each frame's DTS as the transport timestamp, which `container::Frame`
		// does not carry yet; writing CTS = 0 with the PTS would mislabel reordered video.
		Err(moq_net::Error::Unsupported.into())
	}

	fn decode(&self, mut frame: moq_net::frame::Frame) -> Result<Vec<Frame>, Self::Error> {
		// CTS is relative to the transport timestamp in the track's timescale, so an untimed track is malformed.
		let Some(scale) = frame.timestamp.scale() else {
			return Err(moq_net::Error::TimestampMismatch.into());
		};

		// On a data track an object with no bytes is its discontinuity marker, not a frame.
		// Elsewhere it is malformed, so it fails to decode below.
		if self.0 == Kind::Data && frame.payload.is_empty() {
			return Ok(Vec::new());
		}

		let cts = moq_net::varint::decode_quic(&mut frame.payload).map_err(moq_net::Error::from)?;
		let pts = frame
			.timestamp
			.value()
			.checked_add(cts)
			.ok_or(moq_net::Error::Decode(moq_net::DecodeError::BoundsExceeded))?;
		let timestamp = moq_net::Timestamp::new(pts, scale).map_err(hang::Error::from)?;

		Ok(vec![Frame {
			timestamp,
			payload: frame.payload,
			keyframe: false,
			duration: None,
		}])
	}
}

/// Decode a CTS audio frame while attaching codec-derived packet durations.
pub(crate) fn decode_audio(audio: &legacy::Audio, frame: moq_net::frame::Frame) -> crate::Result<Vec<Frame>> {
	let mut frames = Wire(Kind::Audio).decode(frame)?;
	for frame in &mut frames {
		frame.duration = audio.duration(&frame.payload);
	}
	Ok(frames)
}

#[cfg(test)]
mod tests {
	use super::*;
	use bytes::{Bytes, BytesMut};
	use moq_net::{Timescale, Timestamp};

	fn cts_payload(cts: u64, body: &[u8]) -> Bytes {
		let mut buf = BytesMut::new();
		moq_net::varint::encode_quic(cts, &mut buf).unwrap();
		buf.extend_from_slice(body);
		buf.freeze()
	}

	#[test]
	fn decodes_cts_in_track_timescale() {
		let scale = Timescale::new(90_000).unwrap();
		// DTS = 3000 (33.3ms), CTS = 6000 (66.7ms) -> PTS = 9000 (100ms).
		let wire = moq_net::frame::Frame::new(Timestamp::new(3_000, scale).unwrap(), cts_payload(6_000, b"bframe"));
		let frames = Wire(Kind::Video).decode(wire).unwrap();
		assert_eq!(frames.len(), 1);
		assert_eq!(frames[0].timestamp, Timestamp::new(9_000, scale).unwrap());
		assert_eq!(frames[0].payload, Bytes::from_static(b"bframe"));
	}

	#[test]
	fn rejects_untimed_track_and_empty_media_object() {
		let untimed = moq_net::frame::Frame::new(Timestamp::unscaled(0), cts_payload(0, b"frame"));
		assert!(Wire(Kind::Video).decode(untimed).is_err());

		let empty = moq_net::frame::Frame::new(Timestamp::from_millis(100).unwrap(), Bytes::new());
		assert!(Wire(Kind::Video).decode(empty).is_err());
	}

	#[test]
	fn empty_codec_payload_and_data_discontinuity() {
		let ts = Timestamp::from_millis(500).unwrap();
		let marker = Wire(Kind::Video)
			.decode(moq_net::frame::Frame::new(ts, cts_payload(33, b"")))
			.unwrap();
		assert_eq!(marker.len(), 1);
		assert_eq!(
			Wire(Kind::Video).end(&marker[0]),
			Some(Timestamp::from_millis(533).unwrap())
		);

		let data = Wire(Kind::Data)
			.decode(moq_net::frame::Frame::new(ts, Bytes::new()))
			.unwrap();
		assert!(data.is_empty());
	}

	#[test]
	fn write_and_finish_group_return_unsupported() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track(
				"video",
				moq_net::track::Info::default().with_timescale(Timescale::MILLI),
			)
			.unwrap();
		let mut group = track.append_group(Timestamp::from_millis(0).unwrap()).unwrap();
		let frame = Frame {
			timestamp: Timestamp::from_millis(33).unwrap(),
			payload: Bytes::from_static(b"pkt"),
			keyframe: true,
			duration: None,
		};
		assert!(
			Wire(Kind::Video)
				.write(&mut group, std::slice::from_ref(&frame))
				.is_err()
		);
		assert!(
			Wire(Kind::Video)
				.finish_group(&mut group, Some(frame.timestamp))
				.is_err()
		);
	}
}
