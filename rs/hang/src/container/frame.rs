use bytes::{Buf, BytesMut};
use derive_more::Debug;

pub use buf_list::BufList;

use crate::Error;

pub type Timestamp = moq_lite::Timescale<1_000_000>;

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

	/// The encoded media data for this frame, split into chunks.
	///
	/// The format depends on the codec being used (H.264, AV1, Opus, etc.).
	/// The debug implementation shows only the payload length for brevity.
	#[debug("{} bytes", payload.num_bytes())]
	pub payload: BufList,
}

impl Frame {
	/// Encode the frame to the given group.
	pub fn encode(&self, group: &mut moq_lite::GroupProducer) -> Result<(), Error> {
		let mut header = BytesMut::new();
		self.timestamp.encode(&mut header).map_err(moq_lite::Error::from)?;

		let size = header.len() + self.payload.remaining();

		let mut chunked = group.create_frame(size.into())?;
		chunked.write(header.freeze())?;
		for chunk in &self.payload {
			chunked.write(chunk.clone())?;
		}
		chunked.finish()?;

		Ok(())
	}

	/// Decode a frame from raw bytes (VarInt timestamp prefix + payload).
	pub fn decode(mut buf: impl Buf) -> Result<Self, Error> {
		let timestamp = Timestamp::decode(&mut buf)?;
		let payload = buf.copy_to_bytes(buf.remaining());

		Ok(Self {
			timestamp,
			payload: payload.into(),
		})
	}
}
