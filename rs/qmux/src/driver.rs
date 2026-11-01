//! The driver: one future that owns the transport and moves the session's records over it.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

use bytes::Bytes;
use kio::clock::Instant;
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
	/// Polls the transport's receive side, so it is polled again only once it asks to be.
	recv: kio::Waiter,
	recv_wake: Arc<RecvWake>,
	run: Run<T>,
}

// The transport is only ever borrowed mutably, never pinned.
impl<T> Unpin for Driver<T> {}

impl<T: Transport> Driver<T> {
	pub(crate) fn new(inner: Arc<Inner>, transport: T) -> Self {
		let recv_wake = Arc::new(RecvWake {
			// Nothing is known about the receive side until it is polled once.
			woken: AtomicBool::new(true),
			task: Mutex::new(None),
		});
		Self {
			park: kio::Park::default(),
			recv: kio::Waiter::new(Waker::from(recv_wake.clone())),
			recv_wake,
			run: Run {
				inner,
				transport,
				timer: Timer::default(),
				inbound: Vec::new(),
				reading: true,
				unflushed: false,
				room: true,
				outbound: VecDeque::new(),
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
		this.recv_wake.track(cx.waker());
		let waiter = this.park.hold(cx);
		let recv = Recv {
			waiter: &this.recv,
			wake: &this.recv_wake,
		};
		waiter.batch(|| this.run.poll(waiter, &recv))
	}
}

/// Wakes the driver's task for the transport's receive side, and records that it did.
///
/// Polling the receive side is not free on every transport: tungstenite zeroes its whole
/// read buffer on each attempt, even one that finds nothing. So the driver polls it only
/// after it asked to be woken, rather than on every wakeup a write or a handle caused.
struct RecvWake {
	woken: AtomicBool,
	/// The task that polls the driver.
	task: Mutex<Option<Waker>>,
}

impl RecvWake {
	/// Wake `waker` from now on.
	fn track(&self, waker: &Waker) {
		let mut task = self.task.lock().unwrap();
		if !task.as_ref().is_some_and(|task| task.will_wake(waker)) {
			*task = Some(waker.clone());
		}
	}
}

impl Wake for RecvWake {
	fn wake(self: Arc<Self>) {
		self.wake_by_ref();
	}

	fn wake_by_ref(self: &Arc<Self>) {
		self.woken.store(true, Ordering::Release);
		if let Some(task) = self.task.lock().unwrap().as_ref() {
			task.wake_by_ref();
		}
	}
}

/// The receive side's waiter, and whether it was woken.
struct Recv<'a> {
	waiter: &'a kio::Waiter,
	wake: &'a RecvWake,
}

impl Recv<'_> {
	/// Whether the receive side was woken since it was last found pending, clearing it.
	fn take(&self) -> bool {
		self.wake.woken.swap(false, Ordering::AcqRel)
	}

	/// Poll the receive side again on the next round, since it stopped short of pending.
	fn again(&self) {
		self.wake.woken.store(true, Ordering::Release);
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
	/// The last flush did not finish.
	unflushed: bool,
	/// The transport took everything and flushed it, so the connection may hand over more.
	room: bool,
	/// Records taken from the connection but not yet handed to the transport, which had no room.
	outbound: VecDeque<Bytes>,
	phase: Phase,
	sent_bytes: u64,
	received_bytes: u64,
}

impl<T: Transport> Run<T> {
	fn poll(&mut self, waiter: &kio::Waiter, recv: &Recv<'_>) -> Poll<()> {
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
				Phase::Open => match self.step(waiter, recv) {
					Step::Progress => {
						rounds += 1;
						if rounds == ROUNDS {
							waiter.waker().wake_by_ref();
							return Poll::Pending;
						}
					}
					Step::Idle => return Poll::Pending,
					Step::Drained => {
						let deadline = self.timer.closing.unwrap_or_else(|| kio::clock::now() + CLOSE_TIMEOUT);
						self.phase = Phase::Closing(deadline);
					}
					Step::Failed(err) => self.finish(Err(err)),
				},
			}
		}
	}

	/// Read a batch into the connection, write a batch out of it, and flush.
	fn step(&mut self, waiter: &kio::Waiter, recv: &Recv<'_>) -> Step {
		// Whether this step polled the receive side, and whether it stopped short of pending.
		let polled = self.reading && recv.take();
		let mut more_to_read = false;
		if polled {
			let mut read = 0;
			loop {
				if read >= BATCH_BYTES {
					recv.again();
					more_to_read = true;
					break;
				}
				match self.transport.poll_recv(recv.waiter) {
					Poll::Ready(Ok(record)) => {
						read += record.len().max(1);
						self.inbound.push(Ok(record));
					}
					Poll::Ready(Err(err)) => {
						self.reading = false;
						self.inbound.push(Err(err));
						break;
					}
					Poll::Pending => break,
				}
			}
		}
		let now = kio::clock::now();
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

		// Take a batch from the connection only while the transport has room for it, so a
		// stalled transport leaves unreliable datagrams in the connection to be shed.
		let mut gathered = 0;
		if self.room && self.outbound.is_empty() {
			while gathered < BATCH_BYTES {
				let Some(record) = state.conn.poll_transmit(now) else {
					break;
				};
				gathered += record.len();
				self.outbound.push_back(record);
			}
		}
		state.sweep();
		if state.conn.is_closed() && self.timer.closing.is_none() {
			self.timer.closing = Some(now + CLOSE_TIMEOUT);
		}
		let drained = state.conn.is_drained();
		let deadline = state.conn.poll_timeout();
		// Hand the batch to the transport outside the lock: it copies each record, and
		// tungstenite may write to the socket, while the handles on other threads wait.
		drop(state);

		let mut written = 0;
		let mut blocked = false;
		while let Some(record) = self.outbound.front() {
			match self.transport.poll_ready(waiter) {
				Poll::Ready(Ok(())) => {}
				Poll::Ready(Err(err)) => return Step::Failed(err),
				Poll::Pending => {
					blocked = true;
					break;
				}
			}
			written += record.len();
			let record = self.outbound.pop_front().expect("peeked above");
			if let Err(err) = self.transport.send(record) {
				return Step::Failed(err);
			}
		}

		self.inner.wire.set_received_bytes(self.received_bytes);
		self.sent_bytes += written as u64;
		self.inner.wire.set_sent_bytes(self.sent_bytes);

		// Flush what this step queued, what an earlier flush left, and what the transport may
		// have queued on its own while receiving, such as a WebSocket Pong. Skipping it
		// otherwise matters: a flush through TLS and tungstenite is not free.
		let flushed = match written > 0 || blocked || self.unflushed || polled {
			true => match self.transport.poll_flush(waiter) {
				Poll::Ready(Ok(())) => true,
				Poll::Ready(Err(err)) => return Step::Failed(err),
				Poll::Pending => false,
			},
			false => true,
		};
		self.unflushed = !flushed;
		let had_room = std::mem::replace(&mut self.room, flushed && !blocked);
		self.set_stalled(!self.room);

		if drained && self.outbound.is_empty() && self.room {
			return Step::Drained;
		}
		if self.timer.closing.is_some_and(|at| now >= at) {
			// The close never got through a stalled transport.
			return Step::Failed(Error::Closed);
		}
		// Each step already wrote what the records it read made the connection say, so go again
		// only when a batch cut a direction short, or the transport has room again for what the
		// connection held back meanwhile.
		if more_to_read || (self.room && (gathered >= BATCH_BYTES || !had_room)) {
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
	sleep: Option<kio::clock::Sleep>,
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
		if kio::clock::now() >= deadline {
			return true;
		}
		let sleep = match &mut self.sleep {
			Some(sleep) => {
				if sleep.deadline() != deadline {
					sleep.reset(deadline);
				}
				sleep
			}
			None => self.sleep.insert(kio::clock::sleep_until(deadline)),
		};
		waiter.poll_future(Pin::new(sleep)).is_ready()
	}
}
