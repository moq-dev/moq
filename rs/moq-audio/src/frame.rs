use bytes::Bytes;
use moq_net::Timestamp;

/// One unit of raw PCM crossing the codec boundary: what
/// [`encode::Producer::write`](crate::encode::Producer::write) takes and what
/// [`decode::Consumer::read`](crate::decode::Consumer::read) returns.
///
/// Just a payload and a presentation timestamp. PCM layout (format / sample rate
/// / channel count) is fixed by the producer or consumer at construction time,
/// never per frame, so callers can't accidentally drift the format mid-stream.
#[derive(Clone, Debug)]
pub struct Frame {
	/// Presentation timestamp of the first sample.
	pub timestamp: Timestamp,
	/// The samples, in the layout the producer or consumer was built with.
	pub data: Bytes,
}
