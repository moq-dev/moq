use std::{
	future::Future,
	pin::Pin,
	sync::Arc,
	task::{Context, Poll},
	time::Duration,
};

use web_transport_trait::Stats;

use crate::{
	Error, Version, bandwidth,
	util::{MaybeBoxedExt, MaybeSendBox},
};

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
/// Borrowed or cloned from the [`Connection`] returned by [`crate::Client::connect`]
/// or [`crate::Server::accept`].
#[derive(Clone)]
pub struct Session {
	session: Arc<SessionShared>,
	version: Version,
	send_bandwidth: Option<bandwidth::Consumer>,
	recv_bandwidth: Option<bandwidth::Consumer>,
}

/// A connected session and the future that drives its protocol state.
///
/// Poll this future for the lifetime of the session. Dropping it cancels protocol
/// work and closes the session.
pub struct Connection {
	session: Session,
	inner: MaybeSendBox<'static, Result<(), Error>>,
	result: Option<Result<(), Error>>,
}

impl Connection {
	pub(super) fn new<S: web_transport_trait::Session>(
		transport: S,
		version: Version,
		recv_bandwidth: Option<bandwidth::Consumer>,
		protocol: MaybeSendBox<'static, Result<(), Error>>,
	) -> Self {
		let (session, maintenance) = Session::new(transport, version, recv_bandwidth);
		let inner = async move {
			let mut protocol = protocol;
			tokio::select! {
				result = &mut protocol => result,
				_ = maintenance => protocol.await,
			}
		}
		.maybe_boxed();

		Self {
			session,
			inner,
			result: None,
		}
	}

	/// Borrow the connected session handle.
	pub fn session(&self) -> &Session {
		&self.session
	}

	pub(super) async fn wait_ready(&mut self, ready: impl Future<Output = ()>) -> Result<(), Error> {
		tokio::pin!(ready);
		tokio::select! {
			biased;
			_ = &mut ready => Ok(()),
			result = &mut *self => {
				// Connecting producers live inside the driver. Its completion drops
				// them and releases the barrier on an early session error. The cached
				// result remains available when the caller polls the connection.
				let _ = result;
				ready.await;
				Ok(())
			}
		}
	}
}

impl Future for Connection {
	type Output = Result<(), Error>;

	fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		if let Some(result) = &self.result {
			return Poll::Ready(result.clone());
		}

		let result = std::task::ready!(self.inner.as_mut().poll(cx));
		self.result = Some(result.clone());
		Poll::Ready(result)
	}
}

impl Drop for Connection {
	fn drop(&mut self) {
		self.session.close(Error::Cancel);
	}
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
	fn new<S: web_transport_trait::Session>(
		session: S,
		version: Version,
		recv_bandwidth: Option<bandwidth::Consumer>,
	) -> (Self, MaybeSendBox<'static, ()>) {
		// Send bandwidth is version-agnostic: it depends on QUIC backend support.
		let send_bandwidth = if session.stats().estimated_send_rate().is_some() {
			let producer = bandwidth::Producer::new();
			let consumer = producer.consume();

			let session = session.clone();
			let maintenance = async move {
				run_send_bandwidth(&session, producer).await;
			}
			.maybe_boxed();

			(Some(consumer), maintenance)
		} else {
			(None, std::future::pending().maybe_boxed())
		};
		let (send_bandwidth, maintenance) = send_bandwidth;

		let session = Self {
			session: Arc::new(SessionShared {
				inner: Box::new(session),
				closed: std::sync::atomic::AtomicBool::new(false),
			}),
			version,
			send_bandwidth,
			recv_bandwidth,
		};

		(session, maintenance)
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
