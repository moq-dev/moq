//! A MoQ session handle and a snapshot of its connection statistics.

use std::{sync::Arc, task::Poll, time::Duration};

use crate::transport::Stats as _;

use crate::{Error, SessionError, Version, bandwidth, goaway};

/// How long [`Session::close`] waits for queued data before closing anyway.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

/// A close requested by a session handle, executed by the driver.
#[derive(Clone)]
enum Close {
	/// Close now with this code.
	Abort { code: u32, reason: String },
	/// Close once the protocol owes the peer nothing, or at [`CLOSE_TIMEOUT`].
	Drain,
}

/// How the session ended, published by the driver.
#[derive(Clone)]
struct Ended {
	/// The transport's terminal error.
	err: Error,
	/// The drain's outcome, when a drain is what closed the transport.
	drain: Option<Result<(), Error>>,
}

/// The stats cell shared between the driver's sampler and the handles.
struct StatsState {
	/// The latest sample the driver took (or the construction-time snapshot).
	sample: Stats,
	/// A handle read the stats since the last sample: keep sampling.
	demanded: bool,
}

/// How [`Session::setup`] learns that the peer's SETUP arrived.
#[derive(Clone)]
pub(crate) enum Setup {
	/// The handshake read it before the session started.
	Read,
	/// The lite driver records it from the peer's Setup Stream.
	Lite(crate::lite::PeerSetup),
	/// The moq-transport driver records it from the peer's SETUP stream.
	Ietf(crate::ietf::peer::PeerSetup),
	/// The version carries no SETUP from the peer.
	Never,
}

impl Setup {
	/// `Ready(true)` once the SETUP arrived, `Ready(false)` if it never will.
	fn poll(&self, waiter: &kio::Waiter) -> Poll<bool> {
		match self {
			Self::Read => Poll::Ready(true),
			Self::Lite(setup) => setup.poll_seen(waiter).map(|()| true),
			Self::Ietf(setup) => setup.poll_seen(waiter).map(|()| true),
			Self::Never => Poll::Ready(false),
		}
	}
}

/// A snapshot of connection statistics for a [`Session`].
///
/// Every field is optional: availability depends on the transport backend (native QUIC
/// reports all of them, the browser WebTransport reports few or none) and on the
/// connection state (e.g. `estimated_send_rate` is `None` until the congestion controller
/// has a window). `None` means "not reported", not "zero".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Stats {
	/// Smoothed round-trip time estimate.
	pub rtt: Option<Duration>,

	/// Estimated send bandwidth from the congestion controller.
	pub estimated_send_rate: Option<bandwidth::Rate>,

	/// Estimated receive bandwidth from MoQ PROBE.
	///
	/// `None` unless the negotiated version supports PROBE (moq-lite-03+).
	pub estimated_recv_rate: Option<bandwidth::Rate>,

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
/// Returned with a [`Driver`](crate::Driver) by [`crate::Client::connect`] and
/// [`crate::Server::accept`]. The caller must poll or spawn that driver to run
/// the session.
///
/// Like every handle in this library, the lifecycle is reference counted: clones
/// share the connection, the transport closes when the last clone drops, and
/// [`abort`](Self::abort) closes it explicitly with an error. The handle and the
/// driver are severed in both directions: the driver holds no `Session` clone,
/// so running it never keeps the session alive, and the `Session`
/// holds no transport, so the handle is `Send + Sync` whatever transport the
/// driver uses. Everything transport-shaped (the close, the close reason,
/// the stats sample) is relayed through the driver.
#[derive(Clone)]
pub struct Session {
	/// Handle side to driver: `Some` once [`abort`](Self::abort) or
	/// [`close`](Self::close) ran; the channel closing (the last handle
	/// dropping) is the implicit Cancel, unless a drain was already requested.
	close: kio::Producer<Option<Close>>,
	/// Driver to handle side: how the transport ended.
	closed: kio::Consumer<Option<Ended>>,
	stats: kio::Shared<StatsState>,
	version: Version,
	send_bandwidth: Option<bandwidth::Consumer>,
	recv_bandwidth: Option<bandwidth::Consumer>,
	goaway: Arc<goaway::Handle>,
	setup: Setup,
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
	/// Cheap and non-blocking: this reads the latest sample the session's
	/// driver took, and schedules a refresh, so periodic polling observes
	/// fresh counters (100ms cadence). See [`Stats`] for which
	/// metrics each backend reports.
	pub fn stats(&self) -> Stats {
		let mut stats = {
			let mut state = self.stats.lock();
			// A read is demand: wake the sampler, but only mutate (and so wake)
			// when the flag actually flips.
			if !state.demanded {
				state.demanded = true;
			}
			state.sample
		};
		stats.estimated_recv_rate = self.recv_bandwidth.as_ref().and_then(bandwidth::Consumer::peek);
		stats
	}

	/// Close the transport with an explicit error, instead of waiting for the last
	/// clone to drop. Idempotent: the first abort wins, and it cuts short a
	/// [`close`](Self::close) still draining.
	///
	/// The close is executed by the session's driver, so it reaches the wire
	/// once the runtime polls it (immediately on a live runtime).
	pub fn abort(&self, err: Error) {
		if let Ok(mut close) = self.close.write()
			&& !matches!(*close, Some(Close::Abort { .. }))
		{
			*close = Some(Close::Abort {
				code: SessionError::from(&err).to_code(),
				reason: err.to_string(),
			});
		}
	}

	/// Close the session once the data it queued has been delivered.
	///
	/// Waits until every stream still serving the peer (a subscription whose track
	/// finished, a fetch, a track info reply) has written its data and FIN and the
	/// peer acknowledged them, then closes the transport. Returns
	/// [`Error::Timeout`] if that takes longer than one second, closing anyway, or
	/// the session's terminal error if it ended some other way first. A track that
	/// is still live never finishes, so finish or abort tracks before closing.
	///
	/// Both protocols withdraw this session's announcements and wait for their
	/// delivery. IETF drafts 14 through 16 send withdrawals without waiting.
	pub async fn close(self) -> Result<(), Error> {
		if let Ok(mut close) = self.close.write()
			&& close.is_none()
		{
			*close = Some(Close::Drain);
		}
		// The drain outlives this handle, so dropping it cannot cut the drain short.
		let closed = self.closed.clone();
		drop(self);

		match closed
			.wait(|state| match &**state {
				Some(ended) => Poll::Ready(ended.clone()),
				None => Poll::Pending,
			})
			.await
		{
			Ok(ended) => ended.drain.unwrap_or(Err(ended.err)),
			Err(kio::Closed) => Err(Error::Cancel),
		}
	}

	/// Block until the transport session is closed, returning the reason.
	///
	/// A close code the peer sent is decoded through the session registry (so an auth
	/// rejection arrives as `Error::Session(SessionError::Unauthorized)`); every peer code is
	/// preserved as [`Error::Session`], and a close carrying no application code surfaces as
	/// [`Error::Transport`]. See [`Error::from_transport`]. If the runtime drops
	/// the driver instead of running it to completion, this resolves with
	/// [`Error::Cancel`].
	pub async fn closed(&self) -> Error {
		match self
			.closed
			.wait(|state| match &**state {
				Some(ended) => Poll::Ready(ended.err.clone()),
				None => Poll::Pending,
			})
			.await
		{
			Ok(err) => err,
			// The driver was dropped before it could observe the close.
			Err(kio::Closed) => Error::Cancel,
		}
	}

	/// Wait for the peer's SETUP, or return the session's close reason.
	///
	/// Resolves when the peer's SETUP arrives. This crate's servers send it only once
	/// they admit the client, but neither protocol requires that ordering, so another
	/// server may send SETUP and still refuse the session afterward.
	/// [`crate::Client::connect`] returns before the SETUP on moq-lite-05+ and
	/// moq-transport draft 17+. Older versions read it during the handshake and resolve
	/// at once, except moq-lite-03 and -04, which carry none and return
	/// [`Error::Unsupported`]. A session that already closed returns its close reason.
	pub async fn setup(&self) -> Result<(), Error> {
		kio::wait(|waiter| {
			match self.closed.poll(waiter, |state| match &**state {
				Some(ended) => Poll::Ready(ended.err.clone()),
				None => Poll::Pending,
			}) {
				Poll::Ready(Ok(err)) => return Poll::Ready(Err(err)),
				// The driver was dropped before it could observe the close.
				Poll::Ready(Err(_)) => return Poll::Ready(Err(Error::Cancel)),
				Poll::Pending => {}
			}
			match self.setup.poll(waiter) {
				Poll::Ready(true) => Poll::Ready(Ok(())),
				Poll::Ready(false) => Poll::Ready(Err(Error::Unsupported)),
				Poll::Pending => Poll::Pending,
			}
		})
		.await
	}

	/// Drain the peer gracefully: the handle for sending this session's single
	/// GOAWAY.
	///
	/// The graceful counterpart to [`abort`](Self::abort). Send the message with
	/// [`goaway::Producer::send`], then await [`closed`](Self::closed) to observe
	/// the peer leaving.
	///
	/// Only a [`Goaway`](goaway::Goaway) carrying a [`timeout`](goaway::Goaway::timeout)
	/// schedules a close of our own, so without one this waits for a peer that may
	/// never leave. Set a deadline when the drain has to finish.
	///
	/// Available on every version. A version with no GOAWAY message (moq-lite-03
	/// and earlier) simply carries no explanation to the peer; the deadline is the
	/// sender's own timer either way, so the session still closes on schedule and
	/// the caller does not branch on the negotiated version.
	pub fn drain(&self) -> goaway::Producer {
		self.goaway.producer()
	}

	/// Observe a GOAWAY from the peer, telling us to migrate elsewhere.
	///
	/// [`peek`](goaway::Consumer::peek) is the cheap synchronous check;
	/// [`recv`](goaway::Consumer::recv) waits for one. Once a GOAWAY arrives, new
	/// subscribe and announce-interest requests on this session are refused (both
	/// drafts forbid opening new streams afterward); existing subscriptions keep
	/// flowing until the session closes.
	pub fn draining(&self) -> goaway::Consumer {
		self.goaway.consumer()
	}
}

impl Session {
	pub(super) fn new<S>(
		runtime: crate::time::Clock,
		session: S,
		version: Version,
		recv_bandwidth: Option<bandwidth::Consumer>,
		protocol: crate::driver::Protocol<S>,
		goaway: goaway::Handle,
		setup: Setup,
	) -> (Self, crate::Driver<S>)
	where
		S: crate::transport::poll::Session,
	{
		let sample = snapshot(&session);

		// Send bandwidth is version-agnostic: it depends on QUIC backend support.
		let (send_bandwidth, send_producer) = if sample.estimated_send_rate.is_some() {
			let producer = bandwidth::Producer::new();
			(Some(producer.consume()), Some(producer))
		} else {
			(None, None)
		};

		let close = kio::Producer::new(None);
		let closed = kio::Producer::new(None);
		let closed_consumer = closed.consume();
		let stats = kio::Shared::new(StatsState {
			sample,
			demanded: false,
		});

		let supervisor = Supervisor {
			local_close: protocol.local_close(),
			runtime: runtime.clone(),
			closed_watch: session.clone(),
			session,
			close: Some(close.consume()),
			closed,
			stats: stats.clone(),
			send_bandwidth: send_producer,
			mode: SamplerMode::Idle,
			drain: Drain::Idle,
		};

		let session = Self {
			close,
			closed: closed_consumer,
			stats,
			version,
			send_bandwidth,
			recv_bandwidth,
			goaway: Arc::new(goaway),
			setup,
		};
		let driver = crate::Driver::new(
			runtime.clone(),
			crate::driver::State {
				protocol,
				supervisor: Some(supervisor),
				result: None,
			},
		);

		(session, driver)
	}
}

/// The driver's transport-facing half of a [`Session`]: it executes the
/// handles' close requests, publishes the transport's terminal error, and
/// samples the connection stats (including the send-bandwidth estimate) while
/// anyone is consuming them.
///
/// Finishes once the transport reports closed; everything else is moot then.
pub(crate) struct Supervisor<S> {
	local_close: Arc<std::sync::atomic::AtomicBool>,
	runtime: crate::time::Clock,
	session: S,
	// A dedicated clone for the close watch, since each pending poll operation
	// needs its own handle.
	closed_watch: S,
	/// Handle-side close requests; `None` once one was executed (only the
	/// first close matters, and the channel closing is the last handle
	/// dropping).
	close: Option<kio::Consumer<Option<Close>>>,
	/// Where the transport's end is published for [`Session::closed`].
	closed: kio::Producer<Option<Ended>>,
	stats: kio::Shared<StatsState>,
	/// The send-rate estimate channel, when the backend reports one. `None`
	/// also once every consumer is gone for good.
	send_bandwidth: Option<bandwidth::Producer>,
	mode: SamplerMode,
	drain: Drain,
}

/// A [`Session::close`] waiting for the protocol to deliver what it queued.
enum Drain {
	/// Nobody asked for one.
	Idle,
	/// Requested: close once drained, or at the deadline.
	Waiting(crate::time::Deadline),
	/// The drain closed the transport, with this outcome.
	Done(Result<(), Error>),
}

enum SamplerMode {
	/// Nobody wants stats; sampling is paused.
	Idle,
	/// Someone does; sample when the deadline elapses.
	Polling { deadline: crate::time::Deadline },
}

impl<S: crate::transport::poll::Session> Supervisor<S> {
	const POLL_INTERVAL: Duration = Duration::from_millis(100);

	pub(crate) fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		let mut cx = waiter.context();

		// The transport's terminal error ends the supervisor.
		if let Poll::Ready(err) = self.closed_watch.poll_closed(&mut cx) {
			// Nothing samples once this returns, but `stats()` keeps serving
			// this cell, so leave it holding the session's final counters
			// rather than whichever sample the last demand happened to catch.
			self.stats.lock().sample = snapshot(&self.session);
			let drain = match std::mem::replace(&mut self.drain, Drain::Idle) {
				Drain::Done(res) => Some(res),
				_ => None,
			};
			if let Ok(mut closed) = self.closed.write() {
				*closed = Some(Ended {
					err: Error::from_transport(err),
					drain,
				});
			}
			return Poll::Ready(());
		}

		// Execute the handle-side close requests. The channel closing is the
		// last handle dropping, with a request written just before winning over
		// the implicit cancel. A drain loops back to watch for an abort, which
		// cuts it short.
		while let Some(close) = &self.close {
			let draining = matches!(self.drain, Drain::Waiting(_));
			let (request, last) = match close.poll(waiter, |state| match &**state {
				Some(Close::Drain) if draining => Poll::Pending,
				Some(request) => Poll::Ready(request.clone()),
				None => Poll::Pending,
			}) {
				Poll::Ready(Ok(request)) => (request, false),
				Poll::Ready(Err(last)) => (
					last.clone().unwrap_or_else(|| {
						self.local_close.store(true, std::sync::atomic::Ordering::Relaxed);
						Close::Abort {
							code: SessionError::Cancel.to_code(),
							reason: "dropped".to_string(),
						}
					}),
					true,
				),
				Poll::Pending => break,
			};
			match request {
				Close::Abort { code, reason } => {
					self.session.close(code, &reason);
					self.drain = Drain::Idle;
					self.close = None;
				}
				Close::Drain => {
					if !draining {
						self.drain = Drain::Waiting(crate::time::Deadline::after(&self.runtime, CLOSE_TIMEOUT));
					}
					// No handle is left to abort.
					if last {
						self.close = None;
					}
				}
			}
		}

		self.poll_sampler(waiter);
		Poll::Pending
	}

	pub(crate) fn draining(&self) -> bool {
		matches!(self.drain, Drain::Waiting(_))
	}

	/// Finish a requested drain once the protocol owes the peer nothing, or at
	/// the deadline. Returns whether this closed the transport.
	///
	/// Called after the protocol ran this turn, since only then is `drained`
	/// current.
	pub(crate) fn poll_drain(&mut self, drained: bool, waiter: &kio::Waiter) -> bool {
		let Drain::Waiting(deadline) = &mut self.drain else {
			return false;
		};
		let res = match drained {
			true => Ok(()),
			false if deadline.poll(waiter).is_ready() => Err(Error::Timeout),
			false => return false,
		};
		self.local_close.store(true, std::sync::atomic::Ordering::Relaxed);
		self.session.close(SessionError::Cancel.to_code(), "");
		self.drain = Drain::Done(res);
		// The transport is closed, so no later request can change anything.
		self.close = None;
		true
	}

	/// Take one sample and arm the next deadline.
	fn sample(&mut self) {
		let sample = snapshot(&self.session);
		if let Some(producer) = &self.send_bandwidth {
			// An error means every consumer is gone for good; the stats cell
			// still wants the sample.
			if producer.set(sample.estimated_send_rate).is_err() {
				self.send_bandwidth = None;
			}
		}
		let mut stats = self.stats.lock();
		stats.sample = sample;
		stats.demanded = false;
		drop(stats);
		self.mode = SamplerMode::Polling {
			deadline: crate::time::Deadline::after(&self.runtime, Self::POLL_INTERVAL),
		};
	}

	fn poll_sampler(&mut self, waiter: &kio::Waiter) {
		loop {
			match &mut self.mode {
				SamplerMode::Idle => {
					// Demand is a bandwidth consumer appearing or a stats read.
					let mut demanded = match &self.send_bandwidth {
						Some(producer) => match producer.poll_used(waiter) {
							Poll::Ready(Ok(())) => true,
							Poll::Ready(Err(_)) => {
								self.send_bandwidth = None;
								false
							}
							Poll::Pending => false,
						},
						None => false,
					};
					demanded |= self
						.stats
						.poll(waiter, |state| match state.demanded {
							true => Poll::Ready(()),
							false => Poll::Pending,
						})
						.is_ready();
					if !demanded {
						return;
					}
					self.sample();
				}
				SamplerMode::Polling { deadline } => {
					if deadline.poll(waiter).is_pending() {
						return;
					}
					// The interval elapsed: pause unless someone still cares.
					let used = self.send_bandwidth.as_ref().is_some_and(bandwidth::Producer::is_used);
					if !used && !self.stats.read().demanded {
						self.mode = SamplerMode::Idle;
						continue;
					}
					self.sample();
					// Loop so the fresh deadline registers the waiter.
				}
			}
		}
	}
}

/// A [`Stats`] snapshot of the transport's counters.
///
/// `estimated_recv_rate` is filled in at the [`Session`] level (it comes from
/// MoQ PROBE, not the transport), so it stays `None` here.
fn snapshot<S: crate::transport::poll::Session>(session: &S) -> Stats {
	let stats = session.stats();
	Stats {
		rtt: stats.rtt(),
		estimated_send_rate: stats.estimated_send_rate().map(bandwidth::Rate::from_bps),
		bytes_sent: stats.bytes_sent(),
		bytes_received: stats.bytes_received(),
		bytes_lost: stats.bytes_lost(),
		packets_sent: stats.packets_sent(),
		packets_received: stats.packets_received(),
		packets_lost: stats.packets_lost(),
		..Default::default()
	}
}

// The point of the sever: the handle's auto-traits no longer depend on which
// transport the runtime drives, so every consumer (moq-ffi needs Send + Sync)
// works over every transport, pinned `!Send` ones included.
const _: () = {
	const fn assert_send_sync<T: Send + Sync>() {}
	assert_send_sync::<Session>();
};

/// Session-local advertisement withdrawal; the source origin remains shared.
#[derive(Clone, Default)]
pub(crate) struct Withdrawal(kio::Shared<WithdrawalState>);

#[derive(Default)]
struct WithdrawalState {
	closing: bool,
	active: usize,
}

impl Withdrawal {
	pub(crate) fn begin(&self) {
		self.0.lock().closing = true;
	}

	pub(crate) fn poll(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.0
			.poll(
				waiter,
				|state| if state.closing { Poll::Ready(()) } else { Poll::Pending },
			)
			.map(|_| ())
	}

	pub(crate) fn drained(&self) -> bool {
		self.0.read().active == 0
	}

	pub(crate) fn register(&self) -> Withdrawing {
		self.0.lock().active += 1;
		Withdrawing(self.clone())
	}
}

pub(crate) struct Withdrawing(Withdrawal);

impl Drop for Withdrawing {
	fn drop(&mut self) {
		self.0.0.lock().active -= 1;
	}
}

/// Caps on what one peer can make a session hold at once.
///
/// A peer that goes past a cap loses the session, closed with `TOO_MANY_REQUESTS`. The
/// stats sessions rows report each root's peaks, so an operator sees how close sessions
/// come before one is closed.
///
/// On moq-transport drafts 14 to 16 the caps also size the request window, which counts
/// every request (FETCH, TRACK_STATUS and SUBSCRIBE_UPDATE included), so very low caps
/// can starve it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Limits {
	/// Broadcasts (moq-lite) or namespaces (moq-transport) the peer may have announced to us.
	pub announces: usize,
	/// Subscriptions the peer may hold on our broadcasts.
	pub subscriptions: usize,
}

impl Default for Limits {
	/// Generous enough for a relay mesh carrying a large origin; lower them for untrusted peers.
	fn default() -> Self {
		Self {
			announces: 100_000,
			subscriptions: 10_000,
		}
	}
}

impl Limits {
	/// How many requests a moq-transport peer may hold open at once (drafts 14 to 16).
	///
	/// Twice what the caps admit, so a peer within them is never blocked by request IDs
	/// still waiting to be granted back.
	pub(crate) fn requests(&self) -> u64 {
		let caps = self.announces.saturating_add(self.subscriptions) as u64;
		caps.saturating_mul(2)
	}
}

/// A count of live entries shared across a session, refused past its cap.
#[derive(Clone)]
pub(crate) struct Slots {
	live: Arc<std::sync::atomic::AtomicUsize>,
	max: usize,
	/// Where the most this session has held is reported.
	stats: Option<(crate::stats::Session, crate::stats::Cap)>,
}

impl Slots {
	pub(crate) fn new(max: usize) -> Self {
		Self {
			live: Default::default(),
			max,
			stats: None,
		}
	}

	/// Report how many are held to `stats`, as `cap`'s peak.
	pub(crate) fn with_stats(mut self, stats: &crate::stats::Session, cap: crate::stats::Cap) -> Self {
		self.stats = Some((stats.clone(), cap));
		self
	}

	/// Take one slot until the returned guard drops, or [`Error::TooManyRequests`] at the cap.
	///
	/// The caller closes the session on that error: a peer past its limits loses the session.
	pub(crate) fn acquire(&self) -> Result<Slot, Error> {
		use std::sync::atomic::Ordering;
		let held = self
			.live
			.fetch_update(Ordering::AcqRel, Ordering::Acquire, |live| {
				(live < self.max).then_some(live + 1)
			})
			.map_err(|_| Error::TooManyRequests)?
			+ 1;
		if let Some((stats, cap)) = &self.stats {
			stats.hold(*cap, held as u64);
		}
		Ok(Slot(self.live.clone()))
	}
}

impl Default for Slots {
	/// Unlimited, for sessions and tests that configure nothing.
	fn default() -> Self {
		Self::new(usize::MAX)
	}
}

/// One taken [`Slots`] entry, released on drop.
pub(crate) struct Slot(Arc<std::sync::atomic::AtomicUsize>);

impl Drop for Slot {
	fn drop(&mut self) {
		self.0.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
	}
}
