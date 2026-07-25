//! [`Encoded`]: one encoded access unit on its way to a track.

use bytes::Bytes;
use moq_net::Timestamp;

/// One encoded video frame: a whole access unit in the codec's wire framing
/// (Annex-B with in-band parameter sets), plus the presentation time of the raw
/// [`Frame`](crate::Frame) it came from.
///
/// Named for what it holds rather than mirroring [`Frame`](crate::Frame), so the
/// two never read alike at a call site that handles both.
///
/// The encoder carries the timestamp through the codec rather than leaving the
/// caller to guess one at publish time, so a backend that buffers a frame or
/// drains a tail from [`Encoder::finish`](super::Encoder::finish) still stamps
/// each access unit with the time of the picture it encoded.
#[derive(Clone, Debug)]
pub struct Encoded {
	/// Presentation timestamp, from the raw frame this was encoded from.
	pub timestamp: Timestamp,
	/// The access unit, in the framing the matching catalog importer expects.
	pub payload: Bytes,
}

impl Encoded {
	/// An encoded access unit shown at `timestamp`.
	pub fn new(payload: Bytes, timestamp: Timestamp) -> Self {
		Self { timestamp, payload }
	}
}
