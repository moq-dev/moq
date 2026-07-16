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

/// A connected session bundled with the future that drives its protocol state.
///
/// Poll this for the lifetime of every [`Session`] clone taken from it: nothing
/// else drives the protocol, so a session whose connection isn't polled makes no
/// progress. Either `.await` it, or call [`poll`](Self::poll) with a
/// [`kio::Waiter`] to step it from inside another `poll_*` function. Dropping it
/// cancels protocol work and closes the session. It resolves when the session
/// ends, and keeps returning that same result if polled again.
///
/// To run the driver elsewhere (an executor task) while handing the session to a
/// caller, use [`split`](Self::split) rather than moving the whole connection: a
/// [`Connection`] holds a [`Session`] clone, so a detached one would keep the
/// session's close-on-last-drop from ever firing.
///
/// Polling still requires a tokio runtime with a time driver; see the crate-level
/// Async docs.
pub struct Connection {
	// Both are `Some` until `split` takes them, which also suppresses `Drop`.
	session: Option<Session>,
	driver: Option<Driver>,
}

impl Connection {
	pub(super) fn new<S: web_transport_trait::Session>(
		transport: S,
		version: Version,
		recv_bandwidth: Option<bandwidth::Consumer>,
		protocol: MaybeSendBox<'static, Result<(), Error>>,
	) -> Self {
		let (session, maintenance) = Session::new(transport, version, recv_bandwidth);

		Self {
			session: Some(session),
			driver: Some(Driver {
				protocol,
				maintenance,
				result: None,
				waiter: None,
			}),
		}
	}

	/// Drive the connection one step, registering `waiter` for the next wakeup.
	///
	/// The `poll_*` counterpart of `.await`ing the connection, for callers composing
	/// it into their own [`kio`]-style poll functions. See [`Driver::poll`].
	pub fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		self.driver_mut().poll(waiter)
	}

	/// Borrow the connected session handle.
	pub fn session(&self) -> &Session {
		self.session.as_ref().expect("connection split")
	}

	/// Split into the session handle and the future driving it.
	///
	/// The [`Driver`] holds no [`Session`] clone, so the session still closes once
	/// the caller drops its last handle, and the driver then finishes on its own.
	/// That makes this the right way to hand the driver to an executor.
	pub fn split(mut self) -> (Session, Driver) {
		let session = self.session.take().expect("connection split");
		let driver = self.driver.take().expect("connection split");
		(session, driver)
	}

	fn driver_mut(&mut self) -> &mut Driver {
		self.driver.as_mut().expect("connection split")
	}

	/// Drive the connection until `ready` resolves, so `connect` can block on the
	/// initial announce set.
	///
	/// A session that dies first still resolves `ready`: the connecting producers
	/// live inside the driver, so its completion drops them (waking `ready`) and
	/// releases the barrier. The error isn't lost, it's cached for whoever polls
	/// the connection next.
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

impl Future for Connection {
	type Output = Result<(), Error>;

	fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		Pin::new(self.driver_mut()).poll(cx)
	}
}

impl Drop for Connection {
	fn drop(&mut self) {
		// `split` handed the session off, so there's nothing to cancel.
		if let Some(session) = &self.session {
			session.close(Error::Cancel);
		}
	}
}

/// The future driving a [`Session`]'s protocol state, split off from its
/// [`Connection`].
///
/// Poll it for the lifetime of the session, either by `.await`ing it or via
/// [`poll`](Self::poll). Unlike a [`Connection`], dropping it cancels protocol
/// work without closing the session, since it holds no session handle of its
/// own. It resolves when the session ends, and keeps returning that same result
/// if polled again.
pub struct Driver {
	protocol: MaybeSendBox<'static, Result<(), Error>>,
	// Bandwidth sampling, polled alongside the protocol. Its completion never ends
	// the driver: the protocol owns the teardown. `None` once finished (or when the
	// transport reports no send-rate estimate), since a completed future must not be
	// polled again.
	maintenance: Option<MaybeSendBox<'static, ()>>,
	// Cached so a poll after `wait_ready` consumed the result doesn't re-poll a
	// completed future.
	result: Option<Result<(), Error>>,
	// Retains the previous poll's waiter so its kio registrations stay live until
	// the next poll replaces it (same dance as `kio::wait`).
	waiter: Option<kio::Waiter>,
}

impl Driver {
	/// Drive the protocol one step, registering `waiter` for the next wakeup.
	///
	/// The `poll_*` counterpart of `.await`ing the driver, for callers composing it
	/// into their own [`kio`]-style poll functions.
	pub fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
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
		Poll::Ready(result)
	}
}

impl Future for Driver {
	type Output = Result<(), Error>;

	fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		let this = &mut *self;
		// Replacing drops the previous waiter, keeping this one live until the next
		// poll so any kio registrations it made survive (see `kio::wait`).
		let waiter = kio::Waiter::new(cx.waker().clone());
		let result = this.poll(&waiter);
		this.waiter = Some(waiter);
		result
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
	) -> (Self, Option<MaybeSendBox<'static, ()>>) {
		// Send bandwidth is version-agnostic: it depends on QUIC backend support.
		let (send_bandwidth, maintenance) = if session.stats().estimated_send_rate().is_some() {
			let producer = bandwidth::Producer::new();
			let consumer = producer.consume();

			let session = session.clone();
			let maintenance = async move {
				run_send_bandwidth(&session, producer).await;
			}
			.maybe_boxed();

			(Some(consumer), Some(maintenance))
		} else {
			(None, None)
		};

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
