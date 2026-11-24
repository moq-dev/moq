//! Raw hang container: zero-byte header, presentation timestamp equals the MoQ transport timestamp.
//!
//! Each MoQ frame carries the raw codec payload directly with no container prefix. Suitable for
//! tracks without frame reordering (audio, text, data, or I/P-only video). Requires a timed track.

use crate::container::{Container, Frame, Kind, legacy};

/// Raw hang wire format configured for the track's media role.
pub struct Wire(
	/// The kind of content carried by the track.
	pub Kind,
);

impl Wire {
	/// The bytes preceding a frame's payload: none.
	pub(crate) fn prefix(&self) -> bytes::Bytes {
		bytes::Bytes::new()
	}
}

impl Container for Wire {
	type Error = crate::Error;

	fn end(&self, frame: &Frame) -> Option<moq_net::Timestamp> {
		(self.0 != Kind::Data && frame.payload.is_empty()).then_some(frame.timestamp)
	}

	fn kind(&self) -> Kind {
		self.0
	}

	fn finish_group(
		&self,
		group: &mut moq_net::group::Producer,
		end: Option<moq_net::Timestamp>,
	) -> Result<(), Self::Error> {
		if self.0 == Kind::Video
			&& let Some(timestamp) = end
		{
			group.write_frame(timestamp, bytes::Bytes::new())?;
		}
		Ok(())
	}

	fn write(&self, group: &mut moq_net::group::Producer, frames: &[Frame]) -> Result<(), Self::Error> {
		for frame in frames {
			group.write_frame(frame.timestamp, frame.payload.clone())?;
		}
		Ok(())
	}

	fn decode(&self, frame: moq_net::frame::Frame) -> Result<Vec<Frame>, Self::Error> {
		// Raw relies on the transport timestamp, so an untimed track is malformed.
		if frame.timestamp.scale().is_none() {
			return Err(moq_net::Error::TimestampMismatch.into());
		}

		// On a data track an object with no bytes is its discontinuity marker, not a frame.
		if self.0 == Kind::Data && frame.payload.is_empty() {
			return Ok(Vec::new());
		}

		Ok(vec![Frame {
			timestamp: frame.timestamp,
			payload: frame.payload,
			keyframe: false,
			duration: None,
		}])
	}
}

/// Decode a raw audio frame while attaching codec-derived packet durations.
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
	use bytes::Bytes;
	use moq_net::{Timescale, Timestamp};

	#[test]
	fn decodes_transport_timestamp_and_payload() {
		let wire = moq_net::frame::Frame::new(
			Timestamp::new(90_000, Timescale::new(90_000).unwrap()).unwrap(),
			Bytes::from_static(b"opus"),
		);
		let frames = Wire(Kind::Audio).decode(wire).unwrap();
		assert_eq!(frames.len(), 1);
		assert_eq!(
			frames[0].timestamp,
			Timestamp::new(90_000, Timescale::new(90_000).unwrap()).unwrap()
		);
		assert_eq!(frames[0].payload, Bytes::from_static(b"opus"));
	}

	#[test]
	fn rejects_untimed_track() {
		let wire = moq_net::frame::Frame::new(Timestamp::unscaled(7), Bytes::from_static(b"opus"));
		assert!(Wire(Kind::Audio).decode(wire).is_err());
	}

	#[test]
	fn empty_payload_semantics() {
		let ts = Timestamp::from_millis(500).unwrap();
		// Audio/video: empty payload decodes as an endpoint/discontinuity marker frame.
		let video = Wire(Kind::Video)
			.decode(moq_net::frame::Frame::new(ts, Bytes::new()))
			.unwrap();
		assert_eq!(video.len(), 1);
		assert_eq!(Wire(Kind::Video).end(&video[0]), Some(ts));

		// Data: empty object is the discontinuity marker (decodes to no frames).
		let data = Wire(Kind::Data)
			.decode(moq_net::frame::Frame::new(ts, Bytes::new()))
			.unwrap();
		assert!(data.is_empty());
	}
}
