//! A MoQ session handle and a snapshot of its connection statistics.

use std::{sync::Arc, task::Poll, time::Duration};

use crate::transport::poll::Session as _;
use crate::transport::{Error as _, Metrics as _};

use crate::{Error, SessionError, Version, auth, bandwidth, coding::StreamCodes, goaway};

/// How long [`Session::close`] waits for queued data before closing anyway.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

/// A close requested by a session handle, executed by the driver.
#[derive(Clone)]
enum Close {
	/// Close now with this error's code.
	Abort(Error),
	/// Close once the protocol owes the peer nothing, or at [`CLOSE_TIMEOUT`].
	Drain,
}

/// How the session ended, published by the driver.
#[derive(Clone)]
struct Ended {
	/// The transport's terminal error, or this side's reason when it closed first.
	err: Error,
	/// The drain's outcome, when a drain is what closed the transport.
	drain: Option<Result<(), Error>>,
}

/// How this side closed the transport, shared by every clone of it.
///
/// Every close goes through the boxed transport, so the first one is recorded whichever
/// task sent it, including one ending the session from inside a single request.
#[derive(Default)]
pub(crate) struct Closing {
	/// The handles closed it by dropping or draining: a clean close, whatever the protocol reports.
	local: std::sync::atomic::AtomicBool,
	/// The first close this side sent: its code, and the error behind it when the closer had one.
	sent: std::sync::Mutex<Option<(u32, Option<Error>)>>,
}

impl Closing {
	/// Remember that this side closed the transport with `code`, over `cause` when known,
	/// unless it already closed it.
	pub(crate) fn sent(&self, code: u32, cause: Option<&Error>) {
		let mut sent = self.sent.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
		sent.get_or_insert_with(|| (code, cause.cloned()));
	}

	/// Whether the handles closed the transport by dropping or draining.
	pub(crate) fn is_local(&self) -> bool {
		self.local.load(std::sync::atomic::Ordering::Relaxed)
	}

	/// Why this side closed the transport, reading a bare code with `version`'s registry.
	fn cause(&self, version: Version) -> Option<Error> {
		let sent = self.sent.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
		let (code, cause) = sent.as_ref()?;
		Some(
			cause
				.clone()
				.unwrap_or_else(|| version.decode_session_code(*code).into()),
		)
	}
}

/// The stats cell shared between the driver's sampler and the handles.
struct StatsState {
	/// The latest sample the driver took (or the construction-time snapshot).
	sample: Stats,
	/// A handle read the stats since the last sample: keep sampling.
	demanded: bool,
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

	/// Total receive streams evicted because incomplete messages exhausted the receive window.
	/// A count above zero means the receive window is too small for what the peer sends.
	pub streams_evicted: Option<u64>,
}

/// The congestion controller's view of the connection, as PROBE reports it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Estimate {
	/// The congestion controller's send rate.
	pub send_rate: Option<bandwidth::Rate>,
	/// The smoothed round-trip time.
	pub rtt: Option<Duration>,
}

impl Estimate {
	/// Read the transport's current estimate.
	pub(crate) fn read<S: crate::transport::poll::Session>(session: &S) -> Self {
		Self::from(&snapshot(session))
	}
}

impl From<&Stats> for Estimate {
	fn from(stats: &Stats) -> Self {
		Self {
			send_rate: stats.estimated_send_rate,
			rtt: stats.rtt,
		}
	}
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
	/// What the peer's SETUP advertised, once it arrives.
	peer_implementation: kio::Consumer<Option<Option<String>>>,
	// Weak, so the sampler runs only while a caller holds a consumer.
	send_bandwidth: Option<bandwidth::Weak>,
	recv_bandwidth: Option<bandwidth::Weak>,
	/// The registry whose reservations this session probes for. Its estimate is
	/// unused: [`bandwidth`](Self::bandwidth) mints one per call, so holding the
	/// session doesn't keep the sampler running.
	bandwidth: bandwidth::Allocator,
	goaway: Arc<goaway::Handle>,
	auth: auth::Handle,
	credential: Option<auth::Credential>,
}

impl Session {
	/// Returns the negotiated protocol version.
	pub fn version(&self) -> Version {
		self.version
	}

	/// The implementation the peer advertised in its SETUP, such as `moq-net-rs/0.3.10`.
	///
	/// Waits for the peer's SETUP, which a dialing client may not have read yet.
	/// `None` when the peer advertised none, its version carries no SETUP (moq-lite
	/// 01-04), or the session ended first. Peer-controlled: use it for logs and
	/// rollout counts, never to authorize.
	pub async fn peer_implementation(&self) -> Option<String> {
		// The slot closes only when the driver drops, which a caller holding a finished
		// one never does, so the transport's end is watched too.
		kio::wait(|waiter| {
			let slot = self.peer_implementation.poll(waiter, |slot| match &**slot {
				Some(value) => Poll::Ready(value.clone()),
				None => Poll::Pending,
			});
			match slot {
				Poll::Ready(Ok(value)) => return Poll::Ready(value),
				Poll::Ready(Err(_)) => return Poll::Ready(None),
				Poll::Pending => {}
			}
			let ended = self.closed.poll(waiter, |ended| match &**ended {
				Some(_) => Poll::Ready(()),
				None => Poll::Pending,
			});
			ended.map(|_| None)
		})
		.await
	}

	/// Returns a consumer for the estimated send bitrate (from the congestion controller).
	///
	/// Returns `None` if the QUIC backend doesn't support bandwidth estimation.
	/// Otherwise it holds an estimate from the start, the session's first sample.
	pub fn send_bandwidth(&self) -> Option<bandwidth::Consumer> {
		self.send_bandwidth.as_ref().map(bandwidth::Weak::consume)
	}

	/// Divides [`send_bandwidth`](Self::send_bandwidth) among this session's senders.
	///
	/// The session probes up to what the reservations add up to, through its
	/// transport's [`Probe`](crate::transport::Probe) when it has one, so reserve
	/// here rather than on an allocator of your own. Clones share one registry: a
	/// [`Client::with_allocator`](crate::Client::with_allocator) allocator's when the
	/// session was dialed with one, so reservations outlive a reconnect. Reservations
	/// report `None` when the transport has no estimate.
	pub fn allocator(&self) -> bandwidth::Allocator {
		self.bandwidth.with_estimate(self.send_bandwidth())
	}

	/// Returns a consumer for the estimated receive bitrate (from PROBE).
	///
	/// Holding the consumer requests PROBE; dropping the last one stops it.
	/// Returns `None` if the MoQ version doesn't support PROBE (requires moq-lite-03+).
	pub fn recv_bandwidth(&self) -> Option<bandwidth::Consumer> {
		self.recv_bandwidth.as_ref().map(bandwidth::Weak::consume)
	}

	/// Returns a snapshot of the current connection statistics.
	///
	/// Cheap and non-blocking: this reads the latest sample the session's
	/// driver took, and schedules a refresh, so periodic polling observes
	/// fresh counters (100ms cadence). See [`Stats`] for which
	/// metrics each backend reports.
	pub fn stats(&self) -> Stats {
		let mut state = self.stats.lock();
		// A read is demand: wake the sampler, but only mutate (and so wake)
		// when the flag actually flips.
		if !state.demanded {
			state.demanded = true;
		}
		state.sample
	}

	/// Close the transport with an explicit error, instead of waiting for the last
	/// clone to drop. Idempotent: the first abort wins, and it cuts short a
	/// [`close`](Self::close) still draining.
	///
	/// The close is executed by the session's driver, so it reaches the wire
	/// once the runtime polls it (immediately on a live runtime).
	pub fn abort(&self, err: Error) {
		if let Ok(mut close) = self.close.write()
			&& !matches!(*close, Some(Close::Abort(_)))
		{
			*close = Some(Close::Abort(err));
		}
	}

	/// Close the session after draining its queued data.
	///
	/// Waits until every stream still serving the peer (a subscription whose track
	/// finished, a fetch, a track info reply) has written its data and FIN and the
	/// transport acknowledged them, then closes the transport. Explicit lite-07-wip
	/// also waits for the subscriber to receive a subscription's tail. Older Lite
	/// versions, including the default lite-06, and IETF wait only for transport ACKs. Returns
	/// [`Error::Timeout`] if that takes longer than one second, closing anyway, or
	/// the session's terminal error if it ended some other way first. A track that
	/// is still live never finishes, so finish or abort tracks before closing.
	///
	/// Both protocols withdraw this session's announcements and wait for their
	/// delivery.
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
	/// The driver keeps draining group streams already delivered to the transport
	/// after this resolves, so keep driving it until it completes.
	///
	/// A clean close is [`Error::Cancel`], whichever side closed. A close code the peer
	/// sent is otherwise decoded through the session registry (so an auth rejection arrives
	/// as `Error::Session(SessionError::Unauthorized)`) and preserved as [`Error::Session`].
	/// When the transport reports that this side closed it, this is this side's reason:
	/// the [`abort`](Self::abort) error, or the protocol failure that ended the session.
	/// A close carrying no application code is otherwise [`Error::IdleTimeout`] for an
	/// idle timeout and [`Error::Transport`] for anything else. See
	/// [`Error::from_transport`]. If the runtime drops the driver instead of running it to
	/// completion, this resolves with [`Error::Cancel`]. A panic while polling the driver is
	/// a transport failure instead.
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
	/// [`poll`](goaway::Consumer::poll) it with a no-op waiter for a synchronous check, or
	/// wait on it for one. Once a GOAWAY arrives, this
	/// session's routes cost [`Cost::DRAIN`](crate::origin::Cost::DRAIN): requests
	/// keep opening on it until a replacement session's route outranks it, and
	/// existing subscriptions keep flowing until the session closes.
	pub fn draining(&self) -> goaway::Consumer {
		self.goaway.consumer()
	}

	/// The tokens this side presented and the grant they earned, plus the tokens
	/// the peer presents. See [`auth`].
	///
	/// The handle passed to [`Client::with_auth`](crate::Client::with_auth), or else one
	/// this session created for itself, which ends with it.
	///
	/// On moq-lite-07-wip, and on moq-transport when both sides negotiate the MoQ Auth
	/// extension, each side presents its connection's credential right after setup.
	/// Older versions, and peers that do not negotiate it, leave the grant `None`.
	pub fn auth(&self) -> auth::Handle {
		self.auth.clone()
	}

	/// The connection credential this side presents on AUTH, or `None` when this session offers no AUTH.
	pub(crate) fn credential(&self) -> Option<&auth::Credential> {
		self.credential.as_ref()
	}
}

/// The handles a protocol driver shares with its [`Session`].
pub(super) struct Handles {
	pub stats: crate::stats::Session,
	pub goaway: goaway::Handle,
	pub auth: auth::Link,
	/// The connection credential the driver presented on `auth`, when its version has AUTH.
	pub credential: Option<auth::Credential>,
	/// Where the driver publishes [`Estimate`] changes, for a protocol that reports
	/// them to the peer (moq-lite PROBE). Sampled only while a consumer exists.
	pub estimate: Option<kio::Producer<Estimate>>,
	/// The reservations the session probes for.
	pub bandwidth: bandwidth::Allocator,
	/// What the peer's SETUP advertised as its implementation.
	pub peer_implementation: crate::setup::PeerImplementation,
}

impl Session {
	pub(super) fn new(
		runtime: crate::time::Deadlines,
		session: crate::driver::Transport,
		version: Version,
		recv_bandwidth: Option<bandwidth::Weak>,
		protocol: crate::driver::Protocol,
		handles: Handles,
	) -> (Self, crate::Driver) {
		let Handles {
			goaway,
			auth,
			credential,
			estimate,
			stats: context,
			bandwidth,
			peer_implementation,
		} = handles;
		let goaway = Arc::new(goaway);
		auth.drain_with(goaway.clone());
		let wire = session.wire().map(|counters| context.wire(counters));
		let sample = snapshot(&session);
		if let Some(estimate) = &estimate
			&& let Ok(mut current) = estimate.write()
		{
			*current = Estimate::from(&sample);
		}

		// Send bandwidth is version-agnostic: it depends on QUIC backend support.
		// Seeded with the first sample, so a consumer taken before the sampler
		// first runs reads the estimate instead of `None`.
		let (send_bandwidth, send_producer) = if sample.estimated_send_rate.is_some() {
			let producer = bandwidth::Producer::new();
			let _ = producer.set(sample.estimated_send_rate);
			(Some(producer.weak()), Some(producer))
		} else {
			(None, None)
		};

		// Without a sender there is nothing to hand the target to, so don't follow it.
		let probe = session.probe().map(|probe| (probe, bandwidth.target()));

		let close = kio::Producer::new(None);
		let closed = kio::Producer::new(None);
		let closed_consumer = closed.consume();
		let stats = kio::Shared::new(StatsState {
			sample,
			demanded: false,
		});

		let supervisor = Supervisor {
			goaway: Some(goaway.deadline(&runtime)),
			version,
			context: context.clone(),
			runtime: runtime.clone(),
			closed_watch: session.clone(),
			session,
			close: Some(close.consume()),
			closed,
			stats: stats.clone(),
			wire,
			send_bandwidth: send_producer,
			estimate,
			probe,
			sampled: None,
			mode: SamplerMode::Idle,
			drain: Drain::Idle,
			auth: auth.clone(),
		};

		let session = Self {
			close,
			closed: closed_consumer,
			stats,
			version,
			peer_implementation: peer_implementation.consume(),
			send_bandwidth,
			recv_bandwidth,
			bandwidth,
			goaway,
			auth: auth.handle().clone(),
			credential,
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
/// anyone is consuming them, and enforces GOAWAY deadlines independently of writes.
///
/// Finishes once the transport reports closed; everything else is moot then.
pub(crate) struct Supervisor {
	goaway: Option<goaway::Deadline>,
	version: Version,
	/// Where the session's outcome is recorded.
	context: crate::stats::Session,
	runtime: crate::time::Deadlines,
	session: crate::driver::Transport,
	// A dedicated clone for the close watch, since each pending poll operation
	// needs its own handle.
	closed_watch: crate::driver::Transport,
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
	/// Published on change while a consumer exists; see [`Self::poll_sampler`].
	estimate: Option<kio::Producer<Estimate>>,
	/// The transport's sender and the target it is handed, when it has one.
	probe: Option<(crate::transport::Probe, bandwidth::Target)>,
	/// When the last sample was taken.
	sampled: Option<crate::time::Instant>,
	mode: SamplerMode,
	drain: Drain,
	/// The transport's wire counters, registered under the session's root; dropping records the last reading.
	wire: Option<crate::stats::Wire>,
	/// This session's side of its auth handle, closed before the end is published so a
	/// replacement session built on the same handle never finds this one still active.
	auth: auth::Link,
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
	// Keep the deadline under a task bit: advancing this driver's clock then
	// queues the sample without waking the already-running driver again.
	Polling {
		deadline: Box<kio::Tasks<crate::time::Deadline>>,
	},
}

impl Supervisor {
	pub(crate) fn panicked(&mut self, error: Error) {
		self.session
			.closing()
			.local
			.store(true, std::sync::atomic::Ordering::Relaxed);
		if let Ok(mut closed) = self.closed.write() {
			*closed = Some(Ended {
				err: error.clone(),
				drain: Some(Err(error.clone())),
			});
		}
		let code = self.version.session_code(&SessionError::Internal);
		self.session.abort(&error, code, "session driver panicked");
	}
	const POLL_INTERVAL: Duration = Duration::from_millis(100);

	pub(crate) fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		// The transport's terminal error ends the supervisor.
		if let Poll::Ready(err) = self.closed_watch.poll_closed(waiter) {
			// Nothing samples once this returns, but `stats()` keeps serving
			// this cell, so leave it holding the session's final counters
			// rather than whichever sample the last demand happened to catch.
			self.stats.lock().sample = snapshot(&self.session);
			self.wire.take();
			let drain = match std::mem::replace(&mut self.drain, Drain::Idle) {
				Drain::Done(res) => Some(res),
				_ => None,
			};
			let err = match err.close_kind() {
				// The transport only knows this side closed it, so the reason is ours.
				crate::transport::CloseKind::Closed if err.session_error().is_none() => {
					self.session.closing().cause(self.version).unwrap_or(Error::Cancel)
				}
				_ => self.version.transport_error(err),
			};
			// Anything the peer is told is "no error" ended the session cleanly, by
			// either side: one spelling for the handles and the stats.
			let err = match SessionError::from(&err) {
				SessionError::Cancel => Error::Cancel,
				_ => err,
			};
			self.auth.close(err.clone());
			self.context.end(&err);
			if let Ok(mut closed) = self.closed.write() {
				*closed = Some(Ended { err, drain });
			}
			return Poll::Ready(());
		}

		if let Some(goaway) = &mut self.goaway
			&& let Poll::Ready(timeout) = goaway.poll(waiter)
		{
			self.goaway = None;
			if let Some(timeout) = timeout {
				tracing::warn!(?timeout, "peer did not leave before the GOAWAY deadline; closing");
				self.close(Error::GoawayTimeout, &Error::GoawayTimeout.to_string());
			}
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
				Poll::Ready(Ok(request)) => (Some(request), false),
				Poll::Ready(Err(last)) => (last.clone(), true),
				Poll::Pending => break,
			};
			match request {
				// The last handle dropped without asking for anything: the implicit cancel.
				None => {
					self.session
						.closing()
						.local
						.store(true, std::sync::atomic::Ordering::Relaxed);
					self.close(Error::Cancel, "dropped");
					self.drain = Drain::Idle;
					self.close = None;
				}
				Some(Close::Abort(err)) => {
					self.close(err.clone(), &err.to_string());
					self.drain = Drain::Idle;
					self.close = None;
				}
				Some(Close::Drain) => {
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

		if let Some((probe, target)) = &mut self.probe {
			while let Poll::Ready(target) = target.poll_changed(waiter) {
				probe.set_target(target);
			}
		}

		Poll::Pending
	}

	pub(crate) fn draining(&self) -> bool {
		matches!(self.drain, Drain::Waiting(_))
	}

	/// Close the transport over `cause`, this side's reason for ending the session.
	fn close(&mut self, cause: Error, reason: &str) {
		let code = self.version.session_code(&SessionError::from(&cause));
		self.session.abort(&cause, code, reason);
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
		self.session
			.closing()
			.local
			.store(true, std::sync::atomic::Ordering::Relaxed);
		self.close(Error::Cancel, "");
		self.drain = Drain::Done(res);
		// The transport is closed, so no later request can change anything.
		self.close = None;
		true
	}

	/// Take one sample, publishing it to the bandwidth and estimate channels.
	fn sample(&mut self) -> Stats {
		let sample = snapshot(&self.session);
		self.sampled = Some(self.runtime.now());
		if let Some(producer) = &self.send_bandwidth {
			// An error means every consumer is gone for good; the stats cell
			// still wants the sample.
			if producer.set(sample.estimated_send_rate).is_err() {
				self.send_bandwidth = None;
			}
		}
		if let Some(estimate) = &self.estimate
			&& let Ok(mut current) = estimate.write()
		{
			let next = Estimate::from(&sample);
			// Only a change wakes the consumers.
			if *current != next {
				*current = next;
			}
		}
		sample
	}

	/// Take one sample for the readers that asked, and arm the next deadline.
	fn sample_demanded(&mut self) {
		let sample = self.sample();
		let mut stats = self.stats.lock();
		stats.sample = sample;
		stats.demanded = false;
		drop(stats);
		let next = crate::time::Deadline::after(&self.runtime, Self::POLL_INTERVAL);
		match &mut self.mode {
			SamplerMode::Polling { deadline } => deadline.push(next),
			SamplerMode::Idle => {
				let mut deadline = kio::Tasks::new();
				deadline.push(next);
				self.mode = SamplerMode::Polling {
					deadline: Box::new(deadline),
				};
			}
		}
	}

	fn poll_sampler(&mut self, waiter: &kio::Waiter) {
		self.poll_demanded(waiter);

		// An estimate consumer (a PROBE stream) samples only when the driver runs
		// anyway, never on a timer of its own: a session nothing wakes is sending
		// nothing, so its estimate has nothing new to say. The driver polls this
		// before the protocol, so the estimate a consumer reads is at most one
		// interval old.
		let Some(estimate) = &self.estimate else { return };
		if estimate.poll_wanted_changed(waiter, false).is_pending() {
			return;
		}
		let now = self.runtime.now();
		if self
			.sampled
			.is_none_or(|at| now.duration_since(at) >= Self::POLL_INTERVAL)
		{
			// Not into the stats cell: writing it would wake this sampler's own
			// registration there, and stats readers demand their own samples.
			self.sample();
		}
	}

	/// Sample on an interval while a bandwidth consumer or a stats reader wants it.
	fn poll_demanded(&mut self, waiter: &kio::Waiter) {
		loop {
			match &mut self.mode {
				SamplerMode::Idle => {
					// Demand is a bandwidth consumer appearing or a stats read.
					let mut demanded = match &self.send_bandwidth {
						Some(producer) => match producer.poll_wanted_changed(waiter, false) {
							Poll::Ready(Ok(_)) => true,
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
					self.sample_demanded();
				}
				SamplerMode::Polling { deadline } => {
					if deadline.poll(waiter).is_pending() {
						return;
					}
					// The interval elapsed: pause unless someone still cares.
					let wanted = self
						.send_bandwidth
						.as_ref()
						.is_some_and(|producer| producer.demand().wanted());
					if !wanted && !self.stats.read().demanded {
						self.mode = SamplerMode::Idle;
						continue;
					}
					self.sample_demanded();
					// Loop so the fresh deadline registers the waiter.
				}
			}
		}
	}
}

/// A [`Stats`] snapshot of the transport's metrics and wire counters.
fn snapshot<S: crate::transport::poll::Session>(session: &S) -> Stats {
	// Metrics first: a transport that polls its byte counts refreshes its wire while reading them.
	let metrics = session.metrics();
	let wire = session.wire();
	Stats {
		rtt: metrics.rtt(),
		estimated_send_rate: metrics.estimated_send_rate().map(bandwidth::Rate::from_bps),
		bytes_sent: wire.as_ref().map(crate::transport::Wire::sent_bytes),
		bytes_received: wire.as_ref().map(crate::transport::Wire::received_bytes),
		bytes_lost: metrics.bytes_lost(),
		packets_sent: metrics.packets_sent(),
		packets_received: metrics.packets_received(),
		packets_lost: metrics.packets_lost(),
		streams_evicted: metrics.streams_evicted(),
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

/// Bounds on what one peer can make a session hold, where its transport does not already.
///
/// Each request holds a stream of its own, so the transport's stream credit bounds those.
/// A peer that announces past `announces` loses the session, closed with
/// `TOO_MANY_REQUESTS`. The stats sessions rows report announcement and subscription peaks.
///
/// ```
/// let mut limits = moq_net::session::Limits::default();
/// limits.announces = 1_000;
/// let client = moq_net::Client::new().with_limits(limits);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Limits {
	/// Broadcasts (moq-lite) or namespaces (moq-transport) the peer may have announced to us.
	pub announces: usize,
}

impl Default for Limits {
	/// Generous enough for a relay mesh carrying a large origin; lower it for untrusted peers.
	fn default() -> Self {
		Self { announces: 100_000 }
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
		stats.track_held(cap, &self.live);
		self.stats = Some((stats.clone(), cap));
		self
	}

	/// How many more slots may be taken right now.
	pub(crate) fn available(&self) -> usize {
		self.max
			.saturating_sub(self.live.load(std::sync::atomic::Ordering::Acquire))
	}

	/// Take one slot until the returned guard drops, or [`Error::TooManyRequests`] at the cap.
	///
	/// A peer whose own request runs past its limits loses the session.
	pub(crate) fn acquire(&self) -> Result<Slot, Error> {
		use std::sync::atomic::Ordering;
		let held = self
			.live
			.fetch_update(Ordering::AcqRel, Ordering::Acquire, |live| {
				(live < self.max).then_some(live + 1)
			})
			.map_err(|_| Error::TooManyRequests)?
			+ 1;
		Ok(self.held(held))
	}

	/// Count one entry the transport already bounds until the returned guard drops, past the
	/// cap too, so entries taken with [`Self::acquire`] see it.
	pub(crate) fn hold(&self) -> Slot {
		let held = self.live.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
		self.held(held)
	}

	fn held(&self, held: usize) -> Slot {
		if let Some((stats, cap)) = &self.stats {
			stats.hold(*cap, held as u64);
		}
		Slot(self.live.clone())
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
