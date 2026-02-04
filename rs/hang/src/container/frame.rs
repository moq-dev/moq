use bytes::{Buf, BytesMut};
use derive_more::Debug;

pub use buf_list::BufList;
use moq_lite::{coding::Encode, lite};

use crate::Error;

pub type Timestamp = moq_lite::Timescale<1_000_000>;

/// A media frame with a timestamp and codec-specific payload.
///
/// Frames are the fundamental unit of media data in hang. Each frame contains:
/// - A timestamp when they should be rendered.
/// - A keyframe flag indicating whether this frame can be decoded independently
/// - A codec-specific payload.
#[derive(Clone, Debug)]
pub struct Frame {
	/// The presentation timestamp for this frame.
	///
	/// This indicates when the frame should be displayed relative to the
	/// start of the stream or some other reference point.
	/// This is NOT a wall clock time.
	pub timestamp: Timestamp,

	/// Whether this frame is a keyframe (can be decoded independently).
	///
	/// Keyframes are used to start new groups for efficient seeking and caching.
	pub keyframe: bool,

	/// The encoded media data for this frame, split into chunks.
	///
	/// The format depends on the codec being used (H.264, AV1, Opus, etc.).
	/// The debug implementation shows only the payload length for brevity.
	#[debug("{} bytes", payload.num_bytes())]
	pub payload: BufList,
}

impl Frame {
	/// Encode the frame to the given group.
	///
	/// NOTE: The [Self::keyframe] flag is ignored for this method; you need to create a new group manually.
	pub fn encode(&self, group: &mut moq_lite::GroupProducer) -> Result<(), Error> {
		let mut header = BytesMut::new();
		self.timestamp.encode(&mut header, lite::Version::Draft02);

		let size = header.len() + self.payload.remaining();

		let mut chunked = group.create_frame(size.into());
		chunked.write_chunk(header.freeze());
		for chunk in &self.payload {
			chunked.write_chunk(chunk.clone());
		}
		chunked.close();

		Ok(())
	}
}
