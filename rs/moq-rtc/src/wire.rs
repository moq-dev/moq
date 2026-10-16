//! Cumulative UDP payload bytes for one WebRTC peer.

use moq_tokio::webrtc::ice;

/// A peer's cumulative datagram bytes, excluding IP and UDP headers.
pub use ice::Stats;

/// A read-only source whose counters survive the WebRTC session ending.
#[derive(Clone, Debug)]
pub struct Source(ice::Wire);

impl Source {
	pub(crate) fn new(wire: ice::Wire) -> Self {
		Self(wire)
	}

	/// Read cumulative sent and received UDP payload bytes; neither read consumes them.
	pub fn stats(&self) -> Stats {
		self.0.stats()
	}
}
