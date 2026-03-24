use bytes::Bytes;

pub type Timestamp = moq_lite::Timescale<1_000_000>;

/// A media frame with a timestamp and codec-specific payload.
#[derive(Clone, Debug)]
pub struct Frame {
	/// The presentation timestamp for this frame.
	pub timestamp: Timestamp,

	/// The encoded media data for this frame.
	pub payload: Bytes,
}

/// A frame with keyframe context, used by [`crate::consumer::OrderedConsumer`] and [`crate::producer::OrderedProducer`].
#[derive(Clone, Debug)]
pub struct OrderedFrame {
	/// The presentation timestamp for this frame.
	pub timestamp: Timestamp,

	/// The encoded media data for this frame.
	pub payload: Bytes,

	/// Whether this frame is a keyframe (first frame in the group).
	pub keyframe: bool,
}

impl From<OrderedFrame> for Frame {
	fn from(f: OrderedFrame) -> Self {
		Frame {
			timestamp: f.timestamp,
			payload: f.payload,
		}
	}
}
