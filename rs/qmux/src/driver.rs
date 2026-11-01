//! The driver: one future that owns the transport and moves the session's records over it.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use moq_clock::Instant;
use moq_quic::{TransportError, TransportErrorCode};

use crate::Error;
use crate::session::{Inner, State};
use crate::transport::Transport;

/// How long a closing session keeps the transport open to deliver its close and finish graceful
/// shutdown, like QUIC's draining period. A peer that stopped reading or never finishes its send
/// half cannot hold the transport open past it.
pub(crate) const CLOSE_TIMEOUT: Duration = Duration::from_secs(3);

/// The most bytes read, or queued for writing, before the driver turns to the other direction.
const BATCH_BYTES: usize = 64 * 1024;

/// Rounds of reading and writing in one poll before the driver yields to its executor.
const ROUNDS: usize = 16;

/// Runs a [`Session`](crate::Session) over the transport it owns.
///
/// Returned beside the session by every constructor. Spawn it on any executor, such as
/// `tokio::spawn`, or a thread-local task set since it needs no `Send` beyond what the
/// transport needs. It reads and writes the transport, runs the session's timers, and resolves
/// once the session has closed and the transport shut down. Dropping it first ends the session
/// with [`Error::Closed`].
#[must_use = "the session makes no progress unless its driver is polled"]
pub struct Driver<T> {
	park: kio::Park,
	run: Run<T>,
}

// The transport is only ever borrowed mutably, never pinned.
impl<T> Unpin for Driver<T> {}

impl<T: Transport> Driver<T> {
	pub(crate) fn new(inner: Arc<Inner>, transport: T) -> Self {
		Self {
			park: kio::Park::default(),
			run: Run {
				inner,
				transport,
				timer: Timer::default(),
				inbound: Vec::new(),
				reading: true,
				phase: Phase::Open,
				sent_bytes: 0,
				received_bytes: 0,
			},
		}
	}
}

impl<T: Transport> Future for Driver<T> {
	type Output = ();

	fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
		let this = self.get_mut();
		let waiter = this.park.hold(cx);
		waiter.batch(|| this.run.poll(waiter))
	}
}

impl<T> Drop for Driver<T> {
	fn drop(&mut self) {
		if !matches!(self.run.phase, Phase::Done) {
			finish(&self.run.inner, Err(Error::Closed));
		}
	}
}

impl<T> std::fmt::Debug for Driver<T> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Driver").finish_non_exhaustive()
	}
}

enum Phase {
	/// Moving records both ways.
	Open,
	/// The connection said everything; closing the transport until this deadline.
	Closing(Instant),
	/// The transport is closed and the session's end published.
	Done,
}

/// What one round of reading and writing came to.
enum Step {
	/// Did something; go again.
	Progress,
	/// Parked on the transport, a handle, or a timer.
	Idle,
	/// The connection has nothing left to say and all of it was flushed.
	Drained,
	/// The transport failed, or a closing session ran out of time.
	Failed(Error),
}

struct Run<T> {
	inner: Arc<Inner>,
	transport: T,
	timer: Timer,
	/// Records read but not yet handed to the connection, kept to reuse its allocation.
	inbound: Vec<Result<Bytes, Error>>,
	/// The transport's receive side has not ended.
	reading: bool,
	phase: Phase,
	sent_bytes: u64,
	received_bytes: u64,
}

impl<T: Transport> Run<T> {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		let mut rounds = 0;
		loop {
			match self.phase {
				Phase::Done => return Poll::Ready(()),
				Phase::Closing(deadline) => {
					// A close that fails or runs out of time still ends the session; the close
					// frame, if any, is already out.
					if self.transport.poll_close(waiter).is_pending() && !self.timer.poll(waiter, Some(deadline)) {
						return Poll::Pending;
					}
					self.finish(Ok(()));
				}
				Phase::Open => match self.step(waiter) {
					Step::Progress => {
						rounds += 1;
						if rounds == ROUNDS {
							waiter.waker().wake_by_ref();
							return Poll::Pending;
						}
					}
					Step::Idle => return Poll::Pending,
					Step::Drained => {
						let deadline = self.timer.closing.unwrap_or_else(|| moq_clock::now() + CLOSE_TIMEOUT);
						self.phase = Phase::Closing(deadline);
					}
					Step::Failed(err) => self.finish(Err(err)),
				},
			}
		}
	}

	/// Read a batch into the connection, write a batch out of it, and flush.
	fn step(&mut self, waiter: &kio::Waiter) -> Step {
		let mut read = 0;
		while self.reading && read < BATCH_BYTES {
			match self.transport.poll_recv(waiter) {
				Poll::Ready(Ok(record)) => {
					read += record.len().max(1);
					self.inbound.push(Ok(record));
				}
				Poll::Ready(Err(err)) => {
					self.reading = false;
					self.inbound.push(Err(err));
				}
				Poll::Pending => break,
			}
		}
		let received = !self.inbound.is_empty();

		let now = moq_clock::now();
		let mut state = self.inner.lock();
		// Register first: a handle mutating the connection after this re-polls us.
		waiter.register(&mut state.driver);
		for record in self.inbound.drain(..) {
			if let Ok(record) = &record {
				self.received_bytes += record.len() as u64;
			}
			deliver(&mut state, record, now);
			// Publish what this record did before the next one: a close followed by the
			// transport ending must report the close.
			state.sweep();
		}
		if state.conn.poll_timeout().is_some_and(|at| now >= at) {
			state.conn.handle_timeout(now);
		}

		let mut written = 0;
		let mut blocked = false;
		let mut failed = None;
		while written < BATCH_BYTES {
			match self.transport.poll_ready(waiter) {
				Poll::Ready(Ok(())) => {}
				Poll::Ready(Err(err)) => {
					failed = Some(err);
					break;
				}
				Poll::Pending => {
					blocked = true;
					break;
				}
			}
			let Some(record) = state.conn.poll_transmit(now) else {
				break;
			};
			written += record.len();
			if let Err(err) = self.transport.send(record) {
				failed = Some(err);
				break;
			}
		}
		state.sweep();
		if state.conn.is_closed() && self.timer.closing.is_none() {
			self.timer.closing = Some(now + CLOSE_TIMEOUT);
		}
		let drained = state.conn.is_drained();
		let deadline = state.conn.poll_timeout();
		drop(state);

		self.inner.wire.set_received_bytes(self.received_bytes);
		self.sent_bytes += written as u64;
		self.inner.wire.set_sent_bytes(self.sent_bytes);
		if let Some(err) = failed {
			return Step::Failed(err);
		}

		// Flush even with nothing new queued, which also sends what the transport queued on its
		// own, such as a WebSocket Pong.
		let flushed = match self.transport.poll_flush(waiter) {
			Poll::Ready(Ok(())) => true,
			Poll::Ready(Err(err)) => return Step::Failed(err),
			Poll::Pending => false,
		};
		self.set_stalled(blocked || !flushed);

		if drained && flushed && !blocked {
			return Step::Drained;
		}
		if self.timer.closing.is_some_and(|at| now >= at) {
			// The close never got through a stalled transport.
			return Step::Failed(Error::Closed);
		}
		if received || (written > 0 && flushed) {
			return Step::Progress;
		}
		let deadline = match (deadline, self.timer.closing) {
			(Some(a), Some(b)) => Some(a.min(b)),
			(a, b) => a.or(b),
		};
		match self.timer.poll(waiter, deadline) {
			true => Step::Progress,
			false => Step::Idle,
		}
	}

	/// Track whether writes are stalled on the transport. The peer is evidently there but slow,
	/// which defers the idle timeout.
	fn set_stalled(&mut self, stalled: bool) {
		if self.timer.stalled != stalled {
			self.timer.stalled = stalled;
			self.inner.lock().conn.set_backpressured(stalled);
		}
	}

	fn finish(&mut self, result: Result<(), Error>) {
		self.phase = Phase::Done;
		finish(&self.inner, result);
	}
}

/// Feed one received record, or the transport's error, into the connection.
fn deliver(state: &mut State, record: Result<Bytes, Error>, now: Instant) {
	let violation = |code, reason: &str| TransportError::new(code, reason.to_string());
	match record {
		Ok(record) => state.conn.handle_record(record, now),
		Err(Error::FrameTooLarge) => state.conn.handle_oversized(),
		Err(Error::FlowControlError) => state.conn.handle_violation(violation(
			TransportErrorCode::FLOW_CONTROL_ERROR,
			"transport buffer overrun",
		)),
		Err(Error::FrameEncoding) => state
			.conn
			.handle_violation(violation(TransportErrorCode::FRAME_ENCODING_ERROR, "transport framing")),
		Err(Error::ProtocolViolation) => state
			.conn
			.handle_violation(violation(TransportErrorCode::PROTOCOL_VIOLATION, "transport framing")),
		Err(err) => {
			// The transport's own error says more than the connection's reset, unless this side
			// is closing, when the peer hanging up is the expected answer and the driver
			// publishes the close it asked for.
			if state.local_close.is_none() {
				state.fail(err);
			}
			state.conn.transport_closed();
		}
	}
}

/// The transport is done: publish a local close, or the failure that ended it.
fn finish(inner: &Inner, result: Result<(), Error>) {
	let mut state = inner.lock();
	if let Some(err) = state.local_close.take() {
		state.fail(err);
	}
	if let Err(err) = result {
		state.fail(err);
	}
	state.conn.transport_closed();
	state.sweep();
	// A drained connection ended with a close from either side, already published; this only
	// lands when the transport died under a live one.
	state.fail(Error::Closed);
}

/// The driver's timers.
#[derive(Default)]
struct Timer {
	sleep: Option<moq_clock::Sleep>,
	/// When a closing session gives up on delivering its close frame.
	closing: Option<Instant>,
	/// Writes are stalled on the transport.
	stalled: bool,
}

impl Timer {
	/// Wait for `deadline`, returning whether it already passed.
	fn poll(&mut self, waiter: &kio::Waiter, deadline: Option<Instant>) -> bool {
		let Some(deadline) = deadline else {
			return false;
		};
		if moq_clock::now() >= deadline {
			return true;
		}
		let sleep = match &mut self.sleep {
			Some(sleep) => {
				if sleep.deadline() != deadline {
					sleep.reset(deadline);
				}
				sleep
			}
			None => self.sleep.insert(moq_clock::sleep_until(deadline)),
		};
		waiter.poll_future(Pin::new(sleep)).is_ready()
	}
}
