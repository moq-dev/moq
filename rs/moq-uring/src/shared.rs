//! State shared between the [`crate::Worker`] loop and the handles it mints.
//!
//! Everything here is single-threaded (`Rc`, `RefCell`); the only cross-thread
//! surface is [`crate::park::Unpark`]. Handles stage SQEs straight into the
//! ring (submitting inline when the queue is full), while completions are only
//! reaped by the worker loop.

use std::cell::{Cell, RefCell};
use std::io;
use std::rc::Rc;
use std::task::Poll;

use io_uring::{IoUring, squeue};
use moq_net::runtime::Instant;

use crate::park::Unpark;
use crate::{timer, udp};

/// One instant shared by everything the worker polls in a turn.
///
/// `Instant::now` is a vDSO `clock_gettime`, and a busy turn polls a deadline
/// per connection plus whatever those arm, all wanting the same answer. The
/// worker samples once and freezes the clock for the turn, so the whole pass
/// agrees on the time instead of paying for its own read.
#[derive(Default)]
pub(crate) struct Clock {
	/// The frozen instant, or `None` outside a turn.
	now: Cell<Option<Instant>>,
}

impl Clock {
	/// Freeze the clock at `now` until the returned guard drops.
	pub fn turn(&self, now: Instant) -> Turn<'_> {
		self.now.set(Some(now));
		Turn(self)
	}

	/// The turn's instant, or the real clock outside one.
	///
	/// Reads before the first turn and while the worker is parked hit the real
	/// clock, so a snapshot is never served past the turn that took it.
	pub fn now(&self) -> Instant {
		self.now.get().unwrap_or_else(Instant::now)
	}
}

/// Holds a [`Clock`] frozen; dropping it, including on unwind, thaws it.
pub(crate) struct Turn<'a>(&'a Clock);

impl Drop for Turn<'_> {
	fn drop(&mut self) {
		self.0.now.set(None);
	}
}

/// A spawned task: the plain poll-closure shape `kio::Tasks` drives.
pub(crate) type Task = Box<dyn FnMut(&kio::Waiter) -> Poll<()>>;

/// A completion, copied out of the CQ so dispatch can borrow the ring again.
#[derive(Clone, Copy)]
pub(crate) struct Cqe {
	pub user_data: u64,
	pub result: i32,
	pub flags: u32,
}

/// An in-flight operation, keyed by its slab index (the SQE `user_data`).
///
/// The entry owns everything the kernel may still touch: a recv keeps its
/// socket (and so its provided buffers) alive, a send keeps its header and
/// staging buffer lease. An entry is only removed once its terminal CQE
/// arrives, which is what makes teardown safe.
pub(crate) enum Op {
	/// An armed (possibly multishot) receive on a socket.
	Recv {
		sock: Rc<udp::SockShared>,
		/// Oneshot mode: the receive owns its own header and buffer claim.
		one: Option<Box<udp::OneshotRecv>>,
	},
	/// An in-flight `sendmsg`.
	Send(udp::SendOp),
	/// The armed `FUTEX_WAIT` on the park word.
	FutexWait,
	/// A fire-and-forget cancellation; only its own CQE to consume.
	Cancel,
}

/// The worker's shared core, `Rc`ed into every handle.
pub(crate) struct Shared {
	pub ring: RefCell<IoUring>,
	pub ops: RefCell<slab::Slab<Op>>,
	/// The monotonic clock sampled once at the start of each worker turn.
	pub clock: Rc<Clock>,
	/// Its own `Rc` so a [`crate::Timer`] holds just the heap, not the ring.
	pub timers: Rc<RefCell<timer::Heap>>,
	/// Tasks handed to [`crate::Handle::spawn`], drained by the worker loop.
	pub spawns: RefCell<Vec<Task>>,
	pub unpark: std::sync::Arc<Unpark>,
	/// The next provided-buffer group id; one per socket.
	pub next_bgid: Cell<u16>,
	/// Set by [`crate::Worker`]'s drop: handles outlive the loop that would
	/// drive them, so operations must fail instead of pending forever.
	pub stopped: Cell<bool>,
}

impl Shared {
	/// The error every operation reports once the worker has been dropped.
	pub fn gone_error() -> io::Error {
		io::Error::new(io::ErrorKind::NotConnected, "the worker was dropped")
	}

	/// Stage one SQE, submitting inline to make room when the queue is full.
	pub fn push(&self, entry: &squeue::Entry) -> io::Result<()> {
		let mut ring = self.ring.borrow_mut();
		loop {
			{
				let mut sq = ring.submission();
				// SAFETY: every entry's referenced memory (headers, buffers,
				// the futex word) is owned by the matching `Op` slab entry or
				// by `Shared` itself, and stays alive until the terminal CQE.
				if unsafe { sq.push(entry) }.is_ok() {
					return Ok(());
				}
			}
			ring.submit()?;
		}
	}

	/// Insert an op and return its `user_data` key.
	pub fn insert(&self, op: Op) -> u64 {
		self.ops.borrow_mut().insert(op) as u64
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::time::Duration;

	#[test]
	fn the_turn_freezes_and_the_guard_thaws() {
		let clock = Clock::default();
		let start = Instant::now() - Duration::from_secs(60);

		let turn = clock.turn(start);
		assert_eq!(clock.now(), start);
		assert_eq!(clock.now(), start);
		drop(turn);

		// A minute-old snapshot must not outlive the turn that took it.
		assert!(clock.now() > start);
	}
}
