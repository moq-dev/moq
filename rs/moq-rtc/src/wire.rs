//! Cumulative UDP payload bytes for one WebRTC peer.

/// A peer's cumulative datagram bytes, excluding IP and UDP headers.
pub use moq_ice::Stats;

/// A read-only source whose counters survive the WebRTC session ending.
#[derive(Clone, Debug)]
pub struct Source(moq_ice::Wire);

impl Source {
	pub(crate) fn new(wire: moq_ice::Wire) -> Self {
		Self(wire)
	}

	/// Read cumulative sent and received UDP payload bytes; neither read consumes them.
	pub fn stats(&self) -> Stats {
		self.0.stats()
	}
}
