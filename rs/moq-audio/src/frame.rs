use bytes::Bytes;
use moq_net::Timestamp;

use crate::Activity;

/// One unit of raw PCM crossing the codec boundary: what
/// [`encode::Producer::write`](crate::encode::Producer::write) takes and what
/// [`decode::Consumer::read`](crate::decode::Consumer::read) returns.
///
/// Just a payload, a presentation timestamp, and whether the packet these
/// samples came from coded any audio. PCM layout (format / sample rate / channel count)
/// is fixed by the producer or consumer at construction time, never per frame,
/// so callers can't accidentally drift the format mid-stream.
#[derive(Clone, Debug)]
pub struct Frame {
	/// Presentation timestamp of the first sample.
	pub timestamp: Timestamp,
	/// The samples, in the layout the producer or consumer was built with.
	pub data: Bytes,
	/// Whether the packet these samples came out of coded audio, or none at all.
	/// Set on the way out of [`decode`](crate::decode) and ignored on the way
	/// into [`encode`](crate::encode), which classifies what it encodes rather
	/// than what it was told.
	pub activity: Activity,
}

impl Frame {
	/// PCM shown at `timestamp`, classified [`Activity::Active`].
	pub fn new(data: Bytes, timestamp: Timestamp) -> Self {
		Self {
			timestamp,
			data,
			activity: Activity::Active,
		}
	}
}
