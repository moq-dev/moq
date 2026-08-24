use std::{sync::Arc, task::Poll, time::Duration};

use web_transport_trait::Stats;

use crate::{Error, SessionError, Version, bandwidth, goaway};

/// A close requested by a session handle, executed by the machine.
#[derive(Clone)]
struct Close {
	code: u32,
	reason: String,
}

/// The stats cell shared between the machine's sampler and the handles.
struct StatsState {
	/// The latest sample the machine took (or the construction-time snapshot).
	sample: ConnectionStats,
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
/// Returned by [`crate::Client::connect`] and [`crate::Server::accept`], which hand
/// the session's protocol [`runtime::Machine`](crate::runtime::Machine) to the
/// [`Runtime`](crate::runtime::Runtime) they were given: that runtime is the only
/// thing driving the session.
///
/// Like every handle in this library, the lifecycle is reference counted: clones
/// share the connection, the transport closes when the last clone drops, and
/// [`abort`](Self::abort) closes it explicitly with an error. The handle and the
/// machine are severed in both directions: the machine holds no `Session` clone,
/// so the runtime running it never keeps the session alive, and the `Session`
/// holds no transport, so the handle is `Send + Sync` whatever transport the
/// runtime drives. Everything transport-shaped (the close, the close reason,
/// the stats sample) is relayed through the machine.
#[derive(Clone)]
pub struct Session {
	/// Handle side to machine: `Some` once [`abort`](Self::abort) ran; the
	/// channel closing (the last handle dropping) is the implicit Cancel.
	close: kio::Producer<Option<Close>>,
	/// Machine to handle side: the transport's terminal error.
	closed: kio::Consumer<Option<Error>>,
	stats: kio::Shared<StatsState>,
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
	/// Cheap and non-blocking: this reads the latest sample the session's
	/// machine took, and schedules a refresh, so periodic polling observes
	/// fresh counters (100ms cadence). See [`ConnectionStats`] for which
	/// metrics each backend reports.
	pub fn stats(&self) -> ConnectionStats {
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
	/// clone to drop. Idempotent: the first close wins.
	///
	/// The close is executed by the session's machine, so it reaches the wire
	/// once the runtime polls it (immediately on a live runtime).
	pub fn abort(&self, err: Error) {
		if let Ok(mut close) = self.close.write()
			&& close.is_none()
		{
			*close = Some(Close {
				code: SessionError::from(&err).to_code(),
				reason: err.to_string(),
			});
		}
	}

	/// Block until the transport session is closed, returning the reason.
	///
	/// A close code the peer sent is decoded through the session registry (so an auth
	/// rejection arrives as [`Error::Unauthorized`]); an unregistered code is kept
	/// verbatim as [`Error::Remote`], and a close carrying no application code surfaces
	/// as [`Error::Transport`]. See [`Error::from_transport`]. If the runtime drops
	/// the machine instead of running it to completion, this resolves with
	/// [`Error::Cancel`].
	pub async fn closed(&self) -> Error {
		match self
			.closed
			.wait(|state| match &**state {
				Some(err) => Poll::Ready(err.clone()),
				None => Poll::Pending,
			})
			.await
		{
			Ok(err) => err,
			// The machine was dropped before it could observe the close.
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
	pub(super) fn new<R>(
		runtime: R,
		session: R::Transport,
		version: Version,
		recv_bandwidth: Option<bandwidth::Consumer>,
		protocol: crate::runtime::Protocol<R>,
		goaway: goaway::Handle,
	) -> (Self, crate::runtime::Machine<R>)
	where
		R: crate::runtime::Runtime + 'static,
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
			runtime,
			closed_watch: session.clone(),
			session,
			close: Some(close.consume()),
			closed,
			stats: stats.clone(),
			send_bandwidth: send_producer,
			mode: SamplerMode::Idle,
		};

		let session = Self {
			close,
			closed: closed_consumer,
			stats,
			version,
			send_bandwidth,
			recv_bandwidth,
			goaway: Arc::new(goaway),
		};
		let machine = crate::runtime::Machine::new(crate::runtime::MachineState {
			protocol,
			supervisor: Some(supervisor),
			result: None,
		});

		(session, machine)
	}

	/// Build the session, hand its machine to the runtime, and return the handle.
	pub(super) fn spawn<R>(
		runtime: R,
		session: R::Transport,
		version: Version,
		recv_bandwidth: Option<bandwidth::Consumer>,
		protocol: crate::runtime::Protocol<R>,
		goaway: goaway::Handle,
	) -> Self
	where
		R: crate::runtime::Runtime + 'static,
	{
		let (session, machine) = Self::new(runtime.clone(), session, version, recv_bandwidth, protocol, goaway);
		runtime.spawn(machine);
		session
	}
}

/// The machine's transport-facing half of a [`Session`]: it executes the
/// handles' close requests, publishes the transport's terminal error, and
/// samples the connection stats (including the send-bandwidth estimate) while
/// anyone is consuming them.
///
/// Finishes once the transport reports closed; everything else is moot then.
pub(crate) struct Supervisor<S, R: crate::runtime::Timers> {
	runtime: R,
	session: S,
	// A dedicated clone for the close watch, since each pending poll operation
	// needs its own handle.
	closed_watch: S,
	/// Handle-side close requests; `None` once one was executed (only the
	/// first close matters, and the channel closing is the last handle
	/// dropping).
	close: Option<kio::Consumer<Option<Close>>>,
	/// Where the transport's terminal error is published for [`Session::closed`].
	closed: kio::Producer<Option<Error>>,
	stats: kio::Shared<StatsState>,
	/// The send-rate estimate channel, when the backend reports one. `None`
	/// also once every consumer is gone for good.
	send_bandwidth: Option<bandwidth::Producer>,
	mode: SamplerMode<R>,
}

enum SamplerMode<R: crate::runtime::Timers> {
	/// Nobody wants stats; sampling is paused.
	Idle,
	/// Someone does; sample when the deadline elapses.
	Polling { deadline: crate::runtime::Deadline<R> },
}

impl<S: crate::transport::poll::Session, R: crate::runtime::Timers> Supervisor<S, R> {
	const POLL_INTERVAL: Duration = Duration::from_millis(100);

	pub(crate) fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		let mut cx = std::task::Context::from_waker(waiter.waker());

		// The transport's terminal error ends the supervisor.
		if let Poll::Ready(err) = self.closed_watch.poll_closed(&mut cx) {
			// Nothing samples once this returns, but `stats()` keeps serving
			// this cell, so leave it holding the session's final counters
			// rather than whichever sample the last demand happened to catch.
			self.stats.lock().sample = snapshot(&self.session);
			if let Ok(mut closed) = self.closed.write() {
				*closed = Some(Error::from_transport(err));
			}
			return Poll::Ready(());
		}

		// Execute the first handle-side close request. The channel closing is
		// the last handle dropping, with an abort written just before winning
		// over the implicit cancel.
		if let Some(close) = &self.close {
			let request = match close.poll(waiter, |state| match &**state {
				Some(request) => Poll::Ready(request.clone()),
				None => Poll::Pending,
			}) {
				Poll::Ready(Ok(request)) => Some(request),
				Poll::Ready(Err(last)) => Some(last.clone().unwrap_or_else(|| Close {
					code: SessionError::Cancel.to_code(),
					reason: "dropped".to_string(),
				})),
				Poll::Pending => None,
			};
			if let Some(request) = request {
				self.session.close(request.code, &request.reason);
				self.close = None;
			}
		}

		self.poll_sampler(waiter);
		Poll::Pending
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
			deadline: crate::runtime::Deadline::after(&self.runtime, Self::POLL_INTERVAL),
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

/// A [`ConnectionStats`] snapshot of the transport's counters.
///
/// `estimated_recv_rate` is filled in at the [`Session`] level (it comes from
/// MoQ PROBE, not the transport), so it stays `None` here.
fn snapshot<S: crate::transport::poll::Session>(session: &S) -> ConnectionStats {
	let stats = session.stats();
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

// The point of the sever: the handle's auto-traits no longer depend on which
// transport the runtime drives, so every consumer (moq-ffi needs Send + Sync)
// works over every transport, pinned `!Send` ones included.
const _: () = {
	const fn assert_send_sync<T: Send + Sync>() {}
	assert_send_sync::<Session>();
};
