//! State shared between the [`crate::Worker`] loop and the handles it mints.
//!
//! Everything here is single-threaded (`Rc`, `RefCell`); the only cross-thread
//! surface is [`crate::park::Unpark`]. Handles stage SQEs straight into the
//! ring (submitting inline when the queue is full), while completions are only
//! reaped by the worker loop.

use moq_clock::Instant;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::io;
use std::rc::Rc;
use std::task::Poll;

use io_uring::{IoUring, opcode, squeue};

use crate::metrics::Counters;
use crate::park::Unpark;
use crate::{clock::Clock, udp};

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
	/// Where the worker's time comes from: read once per turn, and between turns.
	pub clock: Clock,
	/// The worker's timers, at the instant of its current turn. Entered as the
	/// ambient clock while the worker polls.
	pub deadlines: moq_clock::Deadlines,
	/// Whether a turn is running, so `deadlines` already holds its instant.
	pub turning: Cell<bool>,
	/// Tasks handed to [`crate::Handle::spawn`], drained by the worker loop.
	pub spawns: RefCell<Vec<Task>>,
	pub unpark: std::sync::Arc<Unpark>,
	/// This worker's counters, also held by its sockets, timers, and park
	/// word.
	pub metrics: std::sync::Arc<Counters>,
	/// Provided-buffer group ids, one per multishot socket.
	pub bgids: RefCell<Bgids>,
	/// A ring failure found where it could not be returned, such as a dropped
	/// socket's cancel. The worker's next turn fails with it.
	pub failed: Cell<Option<io::Error>>,
	/// Set by [`crate::Worker`]'s drop: handles outlive the loop that would
	/// drive them, so operations must fail instead of pending forever.
	pub stopped: Cell<bool>,
	/// Completions copied out of the CQ by [`push`](Self::push) when the
	/// kernel reported it full mid-submit. [`crate::Worker`]'s pump dispatches
	/// these before anything newer in the CQ.
	pub spill: RefCell<VecDeque<Cqe>>,
	/// Errors the next inline enters report instead of submitting.
	#[cfg(test)]
	pub enter_faults: RefCell<VecDeque<i32>>,
}

/// The provided-buffer group ids a worker hands its multishot sockets.
///
/// An id comes back only once the kernel has unregistered its buffer ring,
/// so a worker opening a socket per dial never runs out.
#[derive(Default)]
pub(crate) struct Bgids {
	/// The lowest id never handed out; past `u16::MAX`, only reuse remains.
	pub next: u32,
	pub free: Vec<u16>,
}

impl Bgids {
	/// A free id, or `None` while every id names a live buffer ring.
	pub fn take(&mut self) -> Option<u16> {
		if let Some(id) = self.free.pop() {
			return Some(id);
		}
		let id = u16::try_from(self.next).ok()?;
		self.next += 1;
		Some(id)
	}

	/// Return an id the kernel no longer has registered.
	pub fn give(&mut self, id: u16) {
		self.free.push(id);
	}
}

impl Shared {
	/// The worker's time: the current turn's instant. Between turns the queue
	/// still holds the last turn's, so it catches up to the clock first.
	pub fn now(&self) -> Instant {
		if !self.turning.get() {
			self.deadlines.advance(self.clock.now());
		}
		self.deadlines.now()
	}

	/// The error every operation reports once the worker has been dropped.
	pub fn gone_error() -> io::Error {
		io::Error::new(io::ErrorKind::NotConnected, "the worker was dropped")
	}

	/// Stage one SQE, submitting inline to make room when the queue is full.
	pub fn push(&self, entry: &squeue::Entry) -> io::Result<()> {
		self.push_inner(entry, None)
	}

	/// Stage one SQE before `deadline`, submitting inline to make room.
	fn push_until(&self, entry: &squeue::Entry, deadline: Instant) -> io::Result<()> {
		self.push_inner(entry, Some(deadline))
	}

	fn push_inner(&self, entry: &squeue::Entry, deadline: Option<Instant>) -> io::Result<()> {
		let mut ring = self.ring.borrow_mut();
		loop {
			if deadline.is_some_and(|deadline| self.clock.host_now() >= deadline) {
				return Err(io::Error::new(
					io::ErrorKind::TimedOut,
					"io_uring submission deadline elapsed",
				));
			}
			{
				let mut sq = ring.submission();
				// SAFETY: every entry's referenced memory (headers, buffers,
				// the futex word) is owned by the matching `Op` slab entry or
				// by `Shared` itself, and stays alive until the terminal CQE.
				if unsafe { sq.push(entry) }.is_ok() {
					return Ok(());
				}
			}
			self.metrics.sq_full_enters.add(1);
			let before = ring.completion().len();
			#[cfg(test)]
			let submitted = match self.enter_faults.borrow_mut().pop_front() {
				Some(code) => Err(io::Error::from_raw_os_error(code)),
				None => ring.submit(),
			};
			#[cfg(not(test))]
			let submitted = ring.submit();
			self.metrics.entered(
				submitted.as_ref().map_or(0, |count| *count),
				ring.completion().len().saturating_sub(before),
			);
			if let Err(err) = submitted {
				// A signal interrupted the enter before it consumed anything;
				// the ring is fine, so try again.
				if err.raw_os_error() == Some(libc::EINTR) {
					continue;
				}
				// EBUSY: the CQ is full, the kernel could not flush its
				// overflow backlog into it, and it consumed none of our SQEs.
				// Only kernels before 5.13 report this (newer ones just grow
				// the backlog), but it must never surface as an error: it is
				// ring pressure, not a failure of the operation. Acting on the
				// completions here would re-enter dispatch (receive re-arms,
				// cancels) under our ring borrow, so copy them aside for the
				// worker's pump instead; the freed CQ slots let the next
				// submit flush the backlog and take our SQEs.
				if err.raw_os_error() != Some(libc::EBUSY) {
					return Err(err);
				}
				let mut spill = self.spill.borrow_mut();
				let before = spill.len();
				spill.extend(ring.completion().map(|entry| Cqe {
					user_data: entry.user_data(),
					result: entry.result(),
					flags: entry.flags(),
				}));
				if spill.len() == before {
					// EBUSY with an empty CQ breaks the kernel's contract;
					// bail out rather than spin on it.
					return Err(err);
				}
			}
		}
	}

	/// Stage a cancellation after the operation it targets.
	pub fn cancel(&self, target: u64) -> io::Result<()> {
		self.cancel_inner(target, None)
	}

	/// Stage a cancellation after its target before `deadline`.
	pub fn cancel_until(&self, target: u64, deadline: Instant) -> io::Result<()> {
		self.cancel_inner(target, Some(deadline))
	}

	fn cancel_inner(&self, target: u64, deadline: Option<Instant>) -> io::Result<()> {
		let key = self.insert(Op::Cancel);
		let entry = opcode::AsyncCancel::new(target).build().user_data(key);
		let result = match deadline {
			Some(deadline) => self.push_until(&entry, deadline),
			None => self.push(&entry),
		};
		if let Err(err) = result {
			self.ops.borrow_mut().remove(key as usize);
			return Err(err);
		}
		Ok(())
	}

	/// Record a ring failure for the worker's next turn, keeping the first.
	pub fn fail(&self, err: io::Error) {
		let first = self.failed.take().unwrap_or(err);
		self.failed.set(Some(first));
	}

	/// Insert an op and return its `user_data` key.
	pub fn insert(&self, op: Op) -> u64 {
		self.ops.borrow_mut().insert(op) as u64
	}
}
