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

	fn check(&self, frame: &Frame) -> Result<(), Self::Error> {
		// A data track's zero-byte object is its discontinuity marker, so an empty frame has no
		// encoding.
		match self.0 == Kind::Data && frame.payload.is_empty() {
			true => Err(crate::Error::EmptyRawPayload),
			false => Ok(()),
		}
	}

	fn write(&self, group: &mut moq_net::group::Producer, frames: &[Frame]) -> Result<(), Self::Error> {
		// Checked up front so a refused batch writes nothing.
		for frame in frames {
			self.check(frame)?;
		}
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

		// On a data track a zero-byte object is only ever the discontinuity marker (decodes to no
		// frames), so one sharing its group is malformed. On audio/video it is an endpoint frame.
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

	/// An empty frame refused on entry changes nothing: its keyframe bit doesn't cut the open group,
	/// and the cues buffered in that group still flush with the ones that follow.
	#[tokio::test]
	async fn a_refused_empty_payload_leaves_the_group_and_buffer() {
		type Hang = crate::catalog::hang::Container;
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast.create_track("data", hang::container::track_info(0)).unwrap();
		let consumer = broadcast.consume();
		let mut producer = crate::container::Producer::new(track, Hang::Raw(Kind::Data))
			.with_buffer(std::time::Duration::from_secs(1));
		let cue = |millis: u64, payload: &'static [u8], keyframe: bool| Frame {
			timestamp: Timestamp::from_millis(millis).unwrap(),
			payload: Bytes::from_static(payload),
			keyframe,
			duration: None,
		};

		producer.write(cue(0, b"a", true)).unwrap();
		producer.write(cue(10, b"b", false)).unwrap();
		let err = producer.write(cue(20, b"", true));
		assert!(matches!(err, Err(crate::Error::EmptyRawPayload)), "{err:?}");
		producer.write(cue(30, b"c", false)).unwrap();
		let _finished = producer.finish().unwrap();

		let group = consumer.track("data").unwrap().fetch_group(0, None).await.unwrap();
		let mut group = crate::container::GroupConsumer::new(group, Hang::Raw(Kind::Data));
		for payload in [b"a", b"b", b"c"] {
			assert_eq!(group.read().await.unwrap().unwrap().payload, payload.as_slice());
		}
		assert!(group.read().await.unwrap().is_none());
	}
}
