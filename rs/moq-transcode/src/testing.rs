//! Source media for tests: gray 320x240 H.264 at 30 fps, in groups created by their first frame.

use bytes::{Bytes, BytesMut};

/// One frame at 30 fps, in microseconds.
pub(crate) const FRAME: u64 = 33_333;

/// `count` gray frames from index `first`, opening on a keyframe, each at its index in microseconds.
pub(crate) fn frames(first: u64, count: u64) -> Vec<hang::container::Frame> {
	let mut encoder = moq_video::encode::Encoder::new(&{
		let mut config = moq_video::encode::Config::new(320, 240, moq_video::FrameRate::new(30, 1).unwrap());
		config.kind = moq_video::encode::Kind::Software;
		config
	})
	.unwrap();
	encoder.cut().unwrap();
	let gray = vec![0x80u8; 320 * 240 * 4];
	let mut frames = Vec::new();
	let mut drain = |encoder: &mut moq_video::encode::Encoder| {
		while let Some(encoded) = encoder.receive().unwrap() {
			frames.push(hang::container::Frame {
				timestamp: encoded.timestamp,
				payload: encoded.payload,
			});
		}
	};
	for index in first..first + count {
		let surface = moq_video::Surface::rgba(&gray, moq_video::Size::new(320, 240)).unwrap();
		let timestamp = moq_net::Timestamp::from_micros(index * FRAME).unwrap();
		encoder.encode(&moq_video::Frame::new(surface, timestamp)).unwrap();
		drain(&mut encoder);
	}
	encoder.flush().unwrap();
	drain(&mut encoder);
	frames
}

/// How the first frame opens its group.
#[derive(Clone, Copy)]
pub(crate) enum Open {
	/// `create_group`, leaving the starts around it unproduced.
	Create,
	/// `append_group`, skipping every start since the newest group.
	Append,
}

/// A group on `track` holding `frames`, keyed by the first one's timestamp.
pub(crate) fn media_group(
	track: &moq_net::track::Producer,
	open: Open,
	frames: Vec<hang::container::Frame>,
) -> moq_net::group::Producer {
	let mut frames = frames.into_iter();
	let first = frames.next().expect("a group needs its first frame");
	let mut buf = BytesMut::new();
	first.encode(&mut buf).unwrap();
	let mut group = match open {
		Open::Create => track.create_group(first.timestamp, buf.freeze()),
		Open::Append => track.append_group(first.timestamp, buf.freeze()),
	}
	.unwrap();
	extend(&mut group, frames);
	group
}

/// Write `frames` on after what `group` holds.
pub(crate) fn extend(group: &mut moq_net::group::Producer, frames: impl IntoIterator<Item = hang::container::Frame>) {
	for frame in frames {
		frame.write_to(group).unwrap();
	}
}

/// A group on `track` at `start` in the track's timescale, holding one empty frame: a stand-in
/// whose media nothing reads.
pub(crate) fn stand_in(track: &moq_net::track::Producer, start: u64) -> moq_net::group::Producer {
	let timescale = track.info().timescale.expect("a timed track");
	let timestamp = moq_net::Timestamp::new(start, timescale).unwrap();
	track.create_group(timestamp, Bytes::new()).unwrap()
}
