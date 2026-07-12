use std::{sync::Arc, time::Duration};

use web_transport_trait::Stats;

use crate::{Error, Version, bandwidth, util::MaybeSendBox};

/// A snapshot of connection statistics for a [`Session`].
///
/// Every field is optional: availability depends on the transport backend (native QUIC
/// reports all of them, the browser WebTransport reports few or none) and on the
/// connection state (e.g. `estimated_send_rate` is `None` until the congestion controller
/// has a window). `None` means "not reported", not "zero".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConnectionStats {
	/// Smoothed round-trip time estimate.
	pub rtt: Option<Duration>,

	/// Estimated send bandwidth from the congestion controller, in bits per second.
	pub estimated_send_rate: Option<u64>,

	/// Estimated receive bandwidth from MoQ PROBE, in bits per second.
	///
	/// `None` unless the negotiated version supports PROBE (moq-lite-03+).
	pub estimated_recv_rate: Option<u64>,

	/// Total bytes sent over the connection, including retransmissions and overhead.
	pub bytes_sent: Option<u64>,

	/// Total bytes received over the connection, including duplicates and overhead.
	pub bytes_received: Option<u64>,

	/// Total bytes lost (detected via retransmission or acknowledgement).
	pub bytes_lost: Option<u64>,

	/// Total datagrams sent.
	pub packets_sent: Option<u64>,

	/// Total datagrams received.
	pub packets_received: Option<u64>,

	/// Total datagrams detected as lost.
	pub packets_lost: Option<u64>,
}

/// A MoQ transport session, wrapping a WebTransport connection.
///
/// Created via:
/// - [`crate::Client::connect`] for clients.
/// - [`crate::Server::accept`] for servers.
#[derive(Clone)]
pub struct Session {
	session: Arc<SessionShared>,
	version: Version,
	send_bandwidth: Option<bandwidth::Consumer>,
	recv_bandwidth: Option<bandwidth::Consumer>,
}

// Close-once state shared by every clone: the transport closes when [`Session::close`]
// is first called, or when the last clone drops, whichever comes first.
struct SessionShared {
	inner: Box<dyn SessionInner>,
	closed: std::sync::atomic::AtomicBool,
}

impl SessionShared {
	fn close(&self, code: u32, reason: &str) {
		if !self.closed.swap(true, std::sync::atomic::Ordering::SeqCst) {
			self.inner.close(code, reason);
		}
	}
}

impl Drop for SessionShared {
	fn drop(&mut self) {
		self.close(Error::Cancel.to_code(), "dropped");
	}
}

impl Session {
	pub(super) fn new<S: web_transport_trait::Session>(
		session: S,
		version: Version,
		recv_bandwidth: Option<bandwidth::Consumer>,
	) -> Self {
		// Send bandwidth is version-agnostic: it depends on QUIC backend support.
		let send_bandwidth = if session.stats().estimated_send_rate().is_some() {
			let producer = bandwidth::Producer::new();
			let consumer = producer.consume();

			let session = session.clone();
			web_async::spawn(async move {
				run_send_bandwidth(&session, producer).await;
			});

			Some(consumer)
		} else {
			None
		};

		Self {
			session: Arc::new(SessionShared {
				inner: Box::new(session),
				closed: std::sync::atomic::AtomicBool::new(false),
			}),
			version,
			send_bandwidth,
			recv_bandwidth,
		}
	}

	/// Returns the negotiated protocol version.
	pub fn version(&self) -> Version {
		self.version
	}

	/// Returns a consumer for the estimated send bitrate (from the congestion controller).
	///
	/// Returns `None` if the QUIC backend doesn't support bandwidth estimation.
	pub fn send_bandwidth(&self) -> Option<bandwidth::Consumer> {
		self.send_bandwidth.clone()
	}

	/// Returns a consumer for the estimated receive bitrate (from PROBE).
	///
	/// Returns `None` if the MoQ version doesn't support PROBE (requires moq-lite-03+).
	pub fn recv_bandwidth(&self) -> Option<bandwidth::Consumer> {
		self.recv_bandwidth.clone()
	}

	/// Returns a snapshot of the current connection statistics.
	///
	/// This is a cheap, non-blocking read of the underlying transport's counters; see
	/// [`ConnectionStats`] for which metrics each backend reports.
	pub fn stats(&self) -> ConnectionStats {
		let mut stats = self.session.inner.stats();
		stats.estimated_recv_rate = self.recv_bandwidth.as_ref().and_then(bandwidth::Consumer::peek);
		stats
	}

	/// Close the underlying transport session.
	///
	/// The close state is shared across clones: the first close wins, and the
	/// transport also closes automatically when the last clone drops.
	pub fn close(&self, err: Error) {
		self.session.close(err.to_code(), err.to_string().as_ref());
	}

	/// Block until the transport session is closed.
	pub async fn closed(&self) -> Result<(), Error> {
		let err = self.session.inner.closed().await;
		Err(Error::Transport(err))
	}
}

/// Polls the QUIC congestion controller for estimated send rate.
///
/// Exits as soon as the session closes so we don't pin the underlying connection
/// after the wrapping [`Session`] is dropped.
async fn run_send_bandwidth<S: web_transport_trait::Session>(session: &S, producer: bandwidth::Producer) {
	tokio::select! {
		_ = session.closed() => {}
		_ = producer.closed() => {}
		_ = run_send_bandwidth_inner(session, &producer) => {}
	}
}

/// Toggles between waiting for a consumer and polling stats while one exists.
/// Returns when the producer channel errors (closed by the consumer side).
async fn run_send_bandwidth_inner<S: web_transport_trait::Session>(session: &S, producer: &bandwidth::Producer) {
	const POLL_INTERVAL: Duration = Duration::from_millis(100);

	loop {
		if producer.used().await.is_err() {
			return;
		}

		let mut interval = web_async::time::interval(POLL_INTERVAL);
		loop {
			tokio::select! {
				biased;
				res = producer.unused() => {
					if res.is_err() {
						return;
					}
					// No more consumers, pause polling.
					break;
				}
				_ = interval.tick() => {
					let bitrate = session.stats().estimated_send_rate();
					if producer.set(bitrate).is_err() {
						return;
					}
				}
			}
		}
	}
}

// We use a wrapper type that is dyn-compatible to remove the generic bounds from Session.
// MaybeSend/MaybeSync keep this Send+Sync on native (where transports are) while
// allowing the !Send browser WebTransport on wasm.
trait SessionInner: web_transport_trait::MaybeSend + web_transport_trait::MaybeSync {
	fn close(&self, code: u32, reason: &str);
	fn closed(&self) -> MaybeSendBox<'_, String>;
	fn stats(&self) -> ConnectionStats;
}

impl<S: web_transport_trait::Session> SessionInner for S {
	fn close(&self, code: u32, reason: &str) {
		S::close(self, code, reason);
	}

	fn closed(&self) -> MaybeSendBox<'_, String> {
		Box::pin(async move { S::closed(self).await.to_string() })
	}

	fn stats(&self) -> ConnectionStats {
		// estimated_recv_rate is filled in at the Session level (it comes from MoQ PROBE,
		// not the transport), so leave it at the Default `None` here.
		let stats = S::stats(self);
		ConnectionStats {
			rtt: stats.rtt(),
			estimated_send_rate: stats.estimated_send_rate(),
			bytes_sent: stats.bytes_sent(),
			bytes_received: stats.bytes_received(),
			bytes_lost: stats.bytes_lost(),
			packets_sent: stats.packets_sent(),
			packets_received: stats.packets_received(),
			packets_lost: stats.packets_lost(),
			..Default::default()
		}
	}
}
