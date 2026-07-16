use std::{future::Future, sync::Arc, task::Poll, time::Duration};

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
/// Returned by [`crate::Client::connect`] and [`crate::Server::accept`]. The session
/// owns all of its protocol work: nothing runs unless the caller drives it, either by
/// awaiting [`run`](Self::run) (typically spawned on an executor) or by stepping
/// [`poll`](Self::poll) from inside another [`kio`]-style poll function. Nothing is
/// spawned behind your back.
///
/// There is exactly one `Session` per connection; dropping it closes the transport
/// and cancels the protocol work. For observation from elsewhere (stats, waiting on
/// close, closing remotely), take cheap [`SessionHandle`] clones via
/// [`handle`](Self::handle). A handle can end the session but never keeps it alive.
///
/// Driving still requires a tokio runtime with a time driver; see the crate-level
/// Async docs.
pub struct Session {
	shared: Arc<SessionShared>,
	version: Version,
	send_bandwidth: Option<bandwidth::Consumer>,
	recv_bandwidth: Option<bandwidth::Consumer>,
	driver: Driver,
}

impl Session {
	/// Drive the protocol one step, registering `waiter` for the next wakeup.
	///
	/// The `poll_*` counterpart of [`run`](Self::run), for callers composing the
	/// session into their own [`kio`]-style poll functions. Resolves when the
	/// session ends, and keeps returning that same result if polled again.
	pub fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		self.driver.poll(waiter)
	}

	/// Drive the protocol until the session ends.
	///
	/// Run this for the lifetime of the session: spawn it on an executor, or await
	/// it in place to serve the connection. Cancel-safe (dropping the future just
	/// stops driving; the session itself stays usable), and idempotent after the
	/// session ends.
	pub async fn run(&mut self) -> Result<(), Error> {
		kio::wait(|waiter| self.poll(waiter)).await
	}

	/// Create a cheap cloneable [`SessionHandle`] for observing this session.
	pub fn handle(&self) -> SessionHandle {
		SessionHandle {
			shared: self.shared.clone(),
			version: self.version,
			send_bandwidth: self.send_bandwidth.clone(),
			recv_bandwidth: self.recv_bandwidth.clone(),
		}
	}

	/// Returns the negotiated protocol version.
	pub fn version(&self) -> Version {
		self.version
	}

	/// Close the transport with a specific error, cancelling the protocol work.
	///
	/// Dropping the session does the same with [`Error::Cancel`].
	pub fn close(self, err: Error) {
		self.shared.close(err.to_code(), err.to_string().as_ref());
	}

	/// Drive the session until `ready` resolves, so `connect` can block on the
	/// initial announce set.
	///
	/// A session that dies first still resolves `ready`: the connecting producers
	/// live inside the driver, so its completion drops them (waking `ready`) and
	/// releases the barrier. The error isn't lost, it's cached for whoever drives
	/// the session next.
	pub(super) async fn wait_ready(&mut self, ready: impl Future<Output = ()>) {
		let mut ready = std::pin::pin!(ready);
		kio::wait(|waiter| {
			if waiter.poll_future(ready.as_mut()).is_ready() {
				return Poll::Ready(());
			}
			let _ = self.poll(waiter);
			Poll::Pending
		})
		.await
	}
}

impl Drop for Session {
	fn drop(&mut self) {
		// Close-once in `shared` makes this a no-op after an explicit `close` or a
		// protocol teardown that already closed the transport.
		self.shared
			.close(Error::Cancel.to_code(), Error::Cancel.to_string().as_ref());
	}
}

/// A cheap cloneable observer for a [`Session`].
///
/// Created via [`Session::handle`]. It can read [`stats`](Self::stats), wait for
/// [`closed`](Self::closed), and [`close`](Self::close) the session remotely, but
/// it never keeps the session alive: the unique [`Session`] owns the lifecycle,
/// and dropping a handle does nothing.
#[derive(Clone)]
pub struct SessionHandle {
	shared: Arc<SessionShared>,
	version: Version,
	send_bandwidth: Option<bandwidth::Consumer>,
	recv_bandwidth: Option<bandwidth::Consumer>,
}

impl SessionHandle {
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
		let mut stats = self.shared.inner.stats();
		stats.estimated_recv_rate = self.recv_bandwidth.as_ref().and_then(bandwidth::Consumer::peek);
		stats
	}

	/// Close the transport with the given error. Idempotent: the first close wins.
	pub fn close(&self, err: Error) {
		self.shared.close(err.to_code(), err.to_string().as_ref());
	}

	/// Block until the transport session is closed.
	pub async fn closed(&self) -> Result<(), Error> {
		let err = self.shared.inner.closed().await;
		Err(Error::Transport(err))
	}
}

/// The boxed protocol work driving a [`Session`], stepped by [`Session::poll`].
struct Driver {
	protocol: MaybeSendBox<'static, Result<(), Error>>,
	// Bandwidth sampling, polled alongside the protocol. Its completion never ends
	// the driver: the protocol owns the teardown. `None` once finished (or when the
	// transport reports no send-rate estimate), since a completed future must not be
	// polled again.
	maintenance: Option<MaybeSendBox<'static, ()>>,
	// Cached so a poll after completion (e.g. after `wait_ready` consumed the
	// result) doesn't re-poll a finished future.
	result: Option<Result<(), Error>>,
}

impl Driver {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		if let Some(result) = &self.result {
			return Poll::Ready(result.clone());
		}

		if let Some(maintenance) = &mut self.maintenance
			&& waiter.poll_future(maintenance.as_mut()).is_ready()
		{
			self.maintenance = None;
		}

		let result = std::task::ready!(waiter.poll_future(self.protocol.as_mut()));
		self.result = Some(result.clone());
		// The session is over; release the maintenance future now rather than on
		// Drop, since it holds a transport clone.
		self.maintenance = None;
		Poll::Ready(result)
	}
}

// Close-once state shared by the [`Session`] and its [`SessionHandle`]s: the first
// close wins, whether it comes from an explicit close, the protocol teardown, a
// handle, or the session's own drop.
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
		protocol: MaybeSendBox<'static, Result<(), Error>>,
	) -> Self {
		// Send bandwidth is version-agnostic: it depends on QUIC backend support.
		let (send_bandwidth, maintenance) = if session.stats().estimated_send_rate().is_some() {
			let producer = bandwidth::Producer::new();
			let consumer = producer.consume();

			let mut monitor = SendBandwidth::new(session.clone(), producer);
			let maintenance = async move { kio::wait(|waiter| monitor.poll(waiter)).await }.maybe_boxed();

			(Some(consumer), Some(maintenance))
		} else {
			(None, None)
		};

		Self {
			shared: Arc::new(SessionShared {
				inner: Box::new(session),
				closed: std::sync::atomic::AtomicBool::new(false),
			}),
			version,
			send_bandwidth,
			recv_bandwidth,
			driver: Driver {
				protocol,
				maintenance,
				result: None,
			},
		}
	}
}

/// Samples the QUIC congestion controller's estimated send rate while anyone is
/// consuming it, pausing when nobody is.
///
/// Finishes as soon as the transport or the producer channel closes, so it doesn't
/// pin the underlying connection after the wrapping [`Session`] is dropped.
struct SendBandwidth<S> {
	session: S,
	producer: bandwidth::Producer,
	// The transport close, boxed once so it can be re-polled each step.
	closed: MaybeSendBox<'static, ()>,
	mode: SendBandwidthMode,
}

enum SendBandwidthMode {
	/// No consumers; sampling is paused.
	Idle,
	/// At least one consumer; sample when the sleep elapses.
	Polling { sleep: MaybeSendBox<'static, ()> },
}

impl<S: web_transport_trait::Session> SendBandwidth<S> {
	const POLL_INTERVAL: Duration = Duration::from_millis(100);

	fn new(session: S, producer: bandwidth::Producer) -> Self {
		let closed = {
			let session = session.clone();
			async move {
				session.closed().await;
			}
		}
		.maybe_boxed();

		Self {
			session,
			producer,
			closed,
			mode: SendBandwidthMode::Idle,
		}
	}

	/// Sample the current estimate, arming the next sleep. Errors when the
	/// producer channel is closed.
	fn sample(&mut self) -> Result<(), Error> {
		let bitrate = self.session.stats().estimated_send_rate();
		self.producer.set(bitrate)?;
		self.mode = SendBandwidthMode::Polling {
			sleep: web_async::time::sleep(Self::POLL_INTERVAL).maybe_boxed(),
		};
		Ok(())
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		if waiter.poll_future(self.closed.as_mut()).is_ready() {
			return Poll::Ready(());
		}

		loop {
			match &mut self.mode {
				SendBandwidthMode::Idle => {
					match self.producer.poll_used(waiter) {
						// A consumer appeared: sample immediately, then on the interval.
						Poll::Ready(Ok(())) => {}
						Poll::Ready(Err(_)) => return Poll::Ready(()),
						Poll::Pending => return Poll::Pending,
					}
					if self.sample().is_err() {
						return Poll::Ready(());
					}
				}
				SendBandwidthMode::Polling { sleep } => {
					// Pause before sampling: checked first, like the old biased select.
					match self.producer.poll_unused(waiter) {
						Poll::Ready(Ok(())) => {
							self.mode = SendBandwidthMode::Idle;
							continue;
						}
						Poll::Ready(Err(_)) => return Poll::Ready(()),
						Poll::Pending => {}
					}

					if waiter.poll_future(sleep.as_mut()).is_pending() {
						return Poll::Pending;
					}
					if self.sample().is_err() {
						return Poll::Ready(());
					}
					// Loop so the fresh sleep registers the waiter.
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
