//! Minimal metrics for the relay demo.
//!
//! Intentionally tiny and low-cardinality: active sessions + application payload bytes.

use std::sync::{
	Arc,
	atomic::{AtomicU64, Ordering},
};

/// Transport used by a MoQ session.
#[derive(Clone, Copy, Debug)]
pub enum Transport {
	WebTransport,
	WebSocket,
}

impl Transport {
	pub fn as_str(&self) -> &'static str {
		match self {
			Self::WebTransport => "webtransport",
			Self::WebSocket => "websocket",
		}
	}
}

/// Thread-safe counters for basic relay metrics.
#[derive(Clone, Default)]
pub struct MetricsTracker {
	active_sessions_webtransport: Arc<AtomicU64>,
	active_sessions_websocket: Arc<AtomicU64>,
	app_bytes_sent_webtransport: Arc<AtomicU64>,
	app_bytes_sent_websocket: Arc<AtomicU64>,
	app_bytes_received_webtransport: Arc<AtomicU64>,
	app_bytes_received_websocket: Arc<AtomicU64>,
}

impl MetricsTracker {
	pub fn new() -> Self {
		Self::default()
	}

	pub fn inc_active_sessions(&self, transport: Transport) {
		match transport {
			Transport::WebTransport => self.active_sessions_webtransport.fetch_add(1, Ordering::Relaxed),
			Transport::WebSocket => self.active_sessions_websocket.fetch_add(1, Ordering::Relaxed),
		};
	}

	pub fn dec_active_sessions(&self, transport: Transport) {
		match transport {
			Transport::WebTransport => self.active_sessions_webtransport.fetch_sub(1, Ordering::Relaxed),
			Transport::WebSocket => self.active_sessions_websocket.fetch_sub(1, Ordering::Relaxed),
		};
	}

	pub fn record_app_bytes_sent(&self, transport: Transport, bytes: u64) {
		match transport {
			Transport::WebTransport => self.app_bytes_sent_webtransport.fetch_add(bytes, Ordering::Relaxed),
			Transport::WebSocket => self.app_bytes_sent_websocket.fetch_add(bytes, Ordering::Relaxed),
		};
	}

	pub fn record_app_bytes_received(&self, transport: Transport, bytes: u64) {
		match transport {
			Transport::WebTransport => self
				.app_bytes_received_webtransport
				.fetch_add(bytes, Ordering::Relaxed),
			Transport::WebSocket => self.app_bytes_received_websocket.fetch_add(bytes, Ordering::Relaxed),
		};
	}

	pub fn active_sessions(&self, transport: Transport) -> u64 {
		match transport {
			Transport::WebTransport => self.active_sessions_webtransport.load(Ordering::Relaxed),
			Transport::WebSocket => self.active_sessions_websocket.load(Ordering::Relaxed),
		}
	}

	pub fn app_bytes_sent(&self, transport: Transport) -> u64 {
		match transport {
			Transport::WebTransport => self.app_bytes_sent_webtransport.load(Ordering::Relaxed),
			Transport::WebSocket => self.app_bytes_sent_websocket.load(Ordering::Relaxed),
		}
	}

	pub fn app_bytes_received(&self, transport: Transport) -> u64 {
		match transport {
			Transport::WebTransport => self.app_bytes_received_webtransport.load(Ordering::Relaxed),
			Transport::WebSocket => self.app_bytes_received_websocket.load(Ordering::Relaxed),
		}
	}
}

/// `moq-lite` stats sink that attributes payload bytes by transport.
pub struct TransportStats {
	metrics: MetricsTracker,
	transport: Transport,
}

impl TransportStats {
	pub fn new(metrics: MetricsTracker, transport: Transport) -> Self {
		Self { metrics, transport }
	}
}

impl moq_lite::Stats for TransportStats {
	fn add_rx_bytes(&self, bytes: u64) {
		self.metrics.record_app_bytes_received(self.transport, bytes);
	}

	fn add_tx_bytes(&self, bytes: u64) {
		self.metrics.record_app_bytes_sent(self.transport, bytes);
	}
}

