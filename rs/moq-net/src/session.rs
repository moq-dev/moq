use std::{
	future::Future,
	pin::Pin,
	sync::{Arc, atomic::Ordering},
	task::{Context, Poll},
	time::Duration,
};

use web_transport_trait::Stats;

use crate::{
	Error, Version, bandwidth, goaway,
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
/// Returned by [`crate::Client::connect`] and [`crate::Server::accept`], paired with
/// the [`Driver`] that runs its protocol work. Nothing is spawned behind your back:
/// the session makes no progress unless its driver is polled.
///
/// Like every handle in this library, the lifecycle is reference counted: clones
/// share the connection, the transport closes when the last clone drops, and
/// [`abort`](Self::abort) closes it explicitly with an error. The [`Driver`] holds
/// no `Session` clone, so handing it to an executor never keeps the session alive.
#[derive(Clone)]
pub struct Session {
	shared: Arc<SessionShared>,
	version: Version,
	send_bandwidth: Option<bandwidth::Consumer>,
	recv_bandwidth: Option<bandwidth::Consumer>,
	goaway: Arc<goaway::Handle>,
}

impl Session {
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

	/// Close the transport with an explicit error, instead of waiting for the last
	/// clone to drop. Idempotent: the first close wins.
	pub fn abort(&self, err: Error) {
		self.shared.close(err.to_code(), err.to_string().as_ref());
	}

	/// Block until the transport session is closed, returning the reason.
	pub async fn closed(&self) -> Error {
		Error::Transport(self.shared.inner.closed().await)
	}

	/// Initiate a graceful GOAWAY drain of this session.
	///
	/// Returns a [`Drain`] handle; call [`Drain::start`] (or
	/// [`start_with_timeout`](Drain::start_with_timeout)) to send the GOAWAY frame
	/// and transition into the [`Draining`] state, then await
	/// [`Draining::complete`] for the peer to disconnect.
	///
	/// Returns `None` when the negotiated version has no GOAWAY message
	/// (moq-lite-03 and earlier), or when a drain is already in progress (only
	/// one GOAWAY per session). The claim is released if the returned [`Drain`]
	/// is dropped before starting, so a caller that bails out can retry later.
	pub fn drain(&self) -> Option<Drain> {
		// Pre-GOAWAY versions have no send path listening on the trigger, so a
		// Drain would silently no-op and Draining::complete could hang.
		if !self.version.has_goaway() {
			return None;
		}
		// Atomically claim the drain so two concurrent callers can't both get a handle.
		if self
			.goaway
			.draining
			.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
			.is_err()
		{
			return None;
		}
		Some(Drain {
			session: Some(self.clone()),
		})
	}

	/// Wait until a GOAWAY is received from the peer.
	///
	/// Resolves with the payload (redirect URI and optional deadline) once the
	/// remote endpoint signals that this session should reconnect elsewhere.
	/// Returns `None` if the session closes before a GOAWAY arrives.
	pub async fn goaway(&self) -> Option<crate::GoawayReceived> {
		kio::wait(|waiter| {
			match self.goaway.received.poll(waiter, |state| match &**state {
				Some(v) => std::task::Poll::Ready(v.clone()),
				None => std::task::Poll::Pending,
			}) {
				std::task::Poll::Ready(Ok(v)) => std::task::Poll::Ready(Some(v)),
				std::task::Poll::Ready(Err(_)) => std::task::Poll::Ready(None),
				std::task::Poll::Pending => std::task::Poll::Pending,
			}
		})
		.await
	}

	/// Whether a GOAWAY has been received from the peer.
	///
	/// Once this returns `true`, new subscribe and announce-interest requests on
	/// this session are rejected with [`Error::GoingAway`]; existing
	/// subscriptions keep flowing until the session closes.
	pub fn is_going_away(&self) -> bool {
		self.goaway.going_away.is_set()
	}
}

/// A claimed but not yet started GOAWAY drain, from [`Session::drain`].
///
/// Call [`start`](Self::start) to send the GOAWAY frame; dropping instead
/// releases the claim so a later [`Session::drain`] can retry.
#[must_use = "call start() to send the GOAWAY; dropping releases the drain claim"]
pub struct Drain {
	// `Some` until start consumes it; `Drop` uses the remainder to release the claim.
	session: Option<Session>,
}

impl Drain {
	/// Send the GOAWAY frame with no deadline.
	///
	/// `uri` is the new session URI the peer should reconnect to; empty tells the
	/// peer to reconnect to the same endpoint.
	pub fn start(self, uri: impl Into<Arc<str>>) -> Draining {
		self.start_inner(uri, None)
	}

	/// Send the GOAWAY frame with a deadline for the peer to disconnect.
	///
	/// [`Draining::complete`] force-closes the session with
	/// [`Error::GoawayTimeout`] when `timeout` elapses. The deadline also rides
	/// the wire on versions with a timeout field (IETF draft-17+) so the peer can
	/// observe it; the wire encodes 0 as "no deadline", so pass a non-zero
	/// duration for a deadline the peer can see.
	pub fn start_with_timeout(self, uri: impl Into<Arc<str>>, timeout: Duration) -> Draining {
		self.start_inner(uri, Some(timeout))
	}

	fn start_inner(mut self, uri: impl Into<Arc<str>>, timeout: Option<Duration>) -> Draining {
		let session = self.session.take().expect("start consumes the drain");
		let payload = goaway::Payload {
			uri: uri.into(),
			timeout,
		};
		if let Ok(mut state) = session.goaway.trigger.write() {
			*state = Some(payload);
		}
		// The drain claim stays set: only one GOAWAY per session.
		Draining { session, timeout }
	}
}

impl Drop for Drain {
	fn drop(&mut self) {
		// Not started: release the claim so a later drain() can retry.
		if let Some(session) = &self.session {
			session.goaway.draining.store(false, Ordering::Release);
		}
	}
}

/// An in-flight GOAWAY drain, from [`Drain::start`].
///
/// The session must be kept driven (its [`Driver`] polled) for the GOAWAY to
/// actually reach the wire and for the drain to progress.
#[must_use = "await complete() to observe the drain finishing"]
pub struct Draining {
	session: Session,
	timeout: Option<Duration>,
}

impl Draining {
	/// Wait for the peer to close the session after receiving the GOAWAY.
	///
	/// With a deadline (from [`Drain::start_with_timeout`]), the session is
	/// force-closed with [`Error::GoawayTimeout`] when it expires; the timer is
	/// cancelled if the peer closes first.
	pub async fn complete(self) {
		let mut closed = std::pin::pin!(self.session.closed());
		let mut deadline = self.timeout.map(|timeout| Box::pin(web_async::time::sleep(timeout)));

		kio::wait(|waiter| {
			if waiter.poll_future(closed.as_mut()).is_ready() {
				return std::task::Poll::Ready(());
			}
			if let Some(sleep) = &mut deadline
				&& waiter.poll_future(sleep.as_mut()).is_ready()
			{
				self.session.abort(Error::GoawayTimeout);
				// Keep polling: the abort resolves `closed` on the next pass.
				deadline = None;
			}
			std::task::Poll::Pending
		})
		.await
	}
}

/// The future driving a [`Session`]'s protocol state.
///
/// Poll it for the lifetime of the session, either by `.await`ing it (typically
/// spawned on an executor) or by stepping [`poll`](Self::poll) from inside another
/// [`kio`]-style poll function. It holds no [`Session`] clone, so it never keeps
/// the session alive: once the last session clone drops (or [`Session::abort`]
/// fires), the transport closes and the driver finishes on its own. Dropping the
/// driver cancels the protocol work without closing the session. It resolves when
/// the session ends, and keeps returning that same result if polled again.
///
/// On native, driving requires a tokio runtime with a time driver (timers go
/// through `web_async::time`); see the crate-level Async docs.
pub struct Driver {
	protocol: MaybeSendBox<'static, Result<(), Error>>,
	// Bandwidth sampling, polled alongside the protocol. Its completion never ends
	// the driver: the protocol owns the teardown. `None` once finished (or when the
	// transport reports no send-rate estimate), since a completed future must not be
	// polled again.
	maintenance: Option<MaybeSendBox<'static, ()>>,
	// Cached so a poll after completion (e.g. after `wait_ready` consumed the
	// result) doesn't re-poll a finished future.
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
		// The session is over; release the maintenance future now rather than on
		// Drop, since it holds a transport clone.
		self.maintenance = None;
		Poll::Ready(result)
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

// Close-once state shared by every [`Session`] clone: the first close wins,
// whether it comes from an [`Session::abort`], the protocol teardown, or the
// last clone dropping.
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
		goaway: goaway::Handle,
	) -> (Self, Driver) {
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

		let session = Self {
			shared: Arc::new(SessionShared {
				inner: Box::new(session),
				closed: std::sync::atomic::AtomicBool::new(false),
			}),
			version,
			send_bandwidth,
			recv_bandwidth,
			goaway: Arc::new(goaway),
		};
		let driver = Driver {
			protocol,
			maintenance,
			result: None,
			waiter: None,
		};

		(session, driver)
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
		Box::pin(async move {
			let err = S::closed(self).await;
			// Surface the application close code and reason when the transport
			// carries them: Display alone often drops both (e.g. quinn reports a
			// bare "connection error: closed"), and the reason is how a peer
			// distinguishes a GOAWAY-timeout force-close from a network failure.
			match web_transport_trait::Error::session_error(&err) {
				Some((code, reason)) if !reason.is_empty() => format!("code={code}: {reason}"),
				Some((code, _)) => format!("code={code}: {err}"),
				None => err.to_string(),
			}
		})
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
