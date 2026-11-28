//! CTS hang container: QUIC varint composition time offset (in track timescale) plus codec payload.
//!
//! Each MoQ frame begins with an unsigned QUIC varint CTS measured in the track's timescale,
//! followed by the raw codec payload. The transport timestamp is the decode time, which never
//! decreases within a group, and the presentation timestamp is `transport_timestamp + CTS`.
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

	fn decode_order(&self) -> bool {
		true
	}

	// Not advertised yet: a draft-18..22 hop stamps every object at its Group ID, losing the
	// decode time the offset counts from, and the binding frame APIs carry no decode time.
	fn writable(&self) -> Result<(), Self::Error> {
		Err(moq_net::Error::Unsupported.into())
	}

	fn check(&self, frame: &Frame) -> Result<(), Self::Error> {
		// The offset is unsigned: a frame never presents before it decodes.
		match frame.decode.is_some_and(|decode| frame.timestamp < decode) {
			true => Err(moq_net::Error::TimestampMismatch.into()),
			false => Ok(()),
		}
	}

	fn finish_group(
		&self,
		group: &mut moq_net::group::Producer,
		end: Option<moq_net::Timestamp>,
	) -> Result<(), Self::Error> {
		if self.0 == Kind::Video
			&& let Some(end) = end
		{
			// An empty codec payload at the end, decoding and presenting there.
			group.write_frame(end, bytes::Bytes::from_static(&[0]))?;
		}
		Ok(())
	}

	fn write(&self, group: &mut moq_net::group::Producer, frames: &[Frame]) -> Result<(), Self::Error> {
		let scale = group.timescale().ok_or(moq_net::Error::TimestampMismatch)?;
		// Checked up front so a refused batch writes nothing.
		let offsets = frames
			.iter()
			.map(|frame| offset(frame, scale))
			.collect::<Result<Vec<_>, _>>()?;
		for (frame, (decode, offset)) in frames.iter().zip(offsets) {
			let mut buf = bytes::BytesMut::with_capacity(8 + frame.payload.len());
			moq_net::varint::encode_quic(offset, &mut buf).map_err(moq_net::Error::from)?;
			buf.extend_from_slice(&frame.payload);
			group.write_frame(decode, buf.freeze())?;
		}
		Ok(())
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
			decode: (cts != 0).then_some(frame.timestamp),
			payload: frame.payload,
			keyframe: false,
			duration: None,
		}])
	}
}

/// The bytes preceding `frame`'s payload in a track at `scale`: its composition offset.
pub(crate) fn prefix(frame: &Frame, scale: moq_net::Timescale) -> crate::Result<bytes::Bytes> {
	let (_, offset) = offset(frame, scale)?;
	let mut buf = bytes::BytesMut::new();
	moq_net::varint::encode_quic(offset, &mut buf).map_err(moq_net::Error::from)?;
	Ok(buf.freeze())
}

/// A frame's decode time in `scale`, and how far after it the frame presents.
fn offset(frame: &Frame, scale: moq_net::Timescale) -> crate::Result<(moq_net::Timestamp, u64)> {
	let at = |timestamp: moq_net::Timestamp| timestamp.convert(scale).map_err(hang::Error::from);
	let presentation = at(frame.timestamp)?;
	let decode = match frame.decode {
		Some(decode) => at(decode)?,
		None => presentation,
	};
	let offset = presentation
		.value()
		.checked_sub(decode.value())
		.ok_or(moq_net::Error::TimestampMismatch)?;
	Ok((decode, offset))
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

	/// A reordered stream writes each frame at its decode time and reads its presentation time
	/// back, and a frame presenting before it decodes is refused.
	#[kio::test]
	async fn reordered_frames_round_trip() {
		let scale = Timescale::new(90_000).unwrap();
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("video", moq_net::track::Info::default().with_timescale(scale))
			.unwrap();
		let at = |ticks| Timestamp::new(ticks, scale).unwrap();
		// Decode order I P B with presentation 3000, 12000, 6000.
		let frames = [(0, 3_000), (3_000, 12_000), (6_000, 6_000)].map(|(decode, presentation)| Frame {
			timestamp: at(presentation),
			decode: Some(at(decode)),
			payload: Bytes::from_static(b"pic"),
			keyframe: false,
			duration: None,
		});
		let mut group = track.append_group(at(0)).unwrap();
		Wire(Kind::Video).write(&mut group, &frames).unwrap();
		Wire(Kind::Video).finish_group(&mut group, Some(at(9_000))).unwrap();
		let mut consumer = group.consume();
		group.finish().unwrap();

		for (decode, presentation) in [(0, 3_000), (3_000, 12_000), (6_000, 6_000)] {
			let frame = consumer.read_frame().await.unwrap().unwrap();
			assert_eq!(frame.timestamp, at(decode));
			let decoded = Wire(Kind::Video).decode(frame).unwrap();
			assert_eq!(decoded[0].timestamp, at(presentation));
			assert_eq!(decoded[0].decode, (decode != presentation).then_some(at(decode)));
		}
		let marker = Wire(Kind::Video)
			.decode(consumer.read_frame().await.unwrap().unwrap())
			.unwrap();
		assert_eq!(Wire(Kind::Video).end(&marker[0]), Some(at(9_000)));

		let early = Frame {
			timestamp: at(0),
			decode: Some(at(3_000)),
			payload: Bytes::from_static(b"pic"),
			keyframe: false,
			duration: None,
		};
		assert!(Wire(Kind::Video).check(&early).is_err());
	}
}
