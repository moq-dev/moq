//! Cumulative UDP payload bytes for one WebRTC peer.

use std::sync::{
	Arc,
	atomic::{AtomicU64, Ordering},
};

/// A peer's cumulative datagram bytes, excluding IP and UDP headers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
	/// Bytes successfully handed to the UDP socket, including control and retransmissions.
	pub tx: u64,
	/// Bytes routed to this peer, including control and packets dropped by its full inbox.
	pub rx: u64,
}

/// A read-only source whose counters survive the WebRTC session ending.
#[derive(Clone, Debug)]
pub struct Source(Arc<Counters>);

#[derive(Debug, Default)]
struct Counters {
	tx: AtomicU64,
	rx: AtomicU64,
}

impl Source {
	pub(crate) fn new() -> Self {
		Self(Arc::default())
	}

	/// Read cumulative sent and received UDP payload bytes; neither read consumes them.
	pub fn stats(&self) -> Stats {
		Stats {
			tx: self.0.tx.load(Ordering::Relaxed),
			rx: self.0.rx.load(Ordering::Relaxed),
		}
	}

	pub(crate) fn sent(&self, bytes: usize) {
		self.0.tx.fetch_add(bytes as u64, Ordering::Relaxed);
	}

	pub(crate) fn received(&self, bytes: usize) {
		self.0.rx.fetch_add(bytes as u64, Ordering::Relaxed);
	}
}
