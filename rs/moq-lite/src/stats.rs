//! Lightweight application-level statistics hooks.
//!
//! The primary motivation is to measure "usefulness" of a relay in a fanout topology
//! without relying on ambiguous "cache hit" semantics.
//!
//! In particular, callers may want to compute an application-level amplification ratio:
//!   output_bitrate / input_bitrate
//!
//! This intentionally ignores transport-level effects such as retransmissions.

/// A sink for application-level byte accounting.
///
/// Implementations should be fast and non-blocking (e.g., atomics).
pub trait Stats: Send + Sync + 'static {
	/// Record payload bytes received by the MoQ session (from the network).
	fn add_rx_bytes(&self, bytes: u64);

	/// Record payload bytes sent by the MoQ session (to the network).
	fn add_tx_bytes(&self, bytes: u64);
}

/// Default stats sink that does nothing.
#[derive(Default)]
pub struct NoopStats;

impl Stats for NoopStats {
	fn add_rx_bytes(&self, _bytes: u64) {}
	fn add_tx_bytes(&self, _bytes: u64) {}
}
