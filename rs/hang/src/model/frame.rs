use derive_more::Debug;

pub use buf_list::BufList;

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
	pub timestamp: moq_lite::Time,

	/// Whether this frame is a keyframe (can be decoded independently).
	///
	/// Keyframes are used as group boundaries and entry points for new subscribers.
	/// It's necessary to periodically encode keyframes to support new subscribers.
	pub keyframe: bool,

	/// The encoded media data for this frame, split into chunks.
	///
	/// The format depends on the codec being used (H.264, AV1, Opus, etc.).
	/// The debug implementation shows only the payload length for brevity.
	#[debug("{} bytes", payload.num_bytes())]
	pub payload: BufList,
}
