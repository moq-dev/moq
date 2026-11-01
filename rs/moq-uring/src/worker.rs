//! The per-thread worker: ring ownership, the drive loop, and parking.

use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::atomic::Ordering;
use std::task::{Context, Poll};
use std::time::Duration;

use io_uring::{EnterFlags, IoUring, opcode, types};
use moq_clock::Instant;

use crate::clock::{Clock, Wait};
use crate::metrics::Metrics;
use crate::park::{FUTEX_BITSET_MATCH_ANY, FUTEX2_PRIVATE, FUTEX2_SIZE_U32, PARKED, RUNNING, Unpark};
use crate::shared::{Cqe, Op, Shared, Task};
use crate::{Error, udp};

/// Submission queue depth. The SQ only holds SQEs staged between submits,
/// never in-flight operations, so it needs no relation to the socket pools;
/// [`Shared::push`] submits inline whenever it fills.
#[cfg(not(test))]
const SQ_ENTRIES: u32 = 256;
#[cfg(test)]
const SQ_ENTRIES: u32 = 128;

/// Completion queue depth. Every in-flight operation can post a completion
/// (one per send buffer with GSO on, one per provided receive buffer, the
/// park futex, transient cancels), so this covers one socket at the default
/// pool ceilings in [`udp::Config`], the one-socket-per-worker layout the
/// relay runs. Running past it is not fatal: the kernel backlogs completions
/// (`IORING_FEAT_NODROP`) rather than drop them. But the backlog is an
/// allocation-per-CQE slow path and it ends any armed multishot receive, so
/// the CQ is sized to keep it out of steady state.
///
/// No larger: the ring is charged to `RLIMIT_MEMLOCK` at 16 bytes per entry,
/// most of each worker's footprint, and that budget is shared by every
/// io_uring the user runs.
const PRODUCTION_CQ_ENTRIES: u32 = 2048;
#[cfg(not(test))]
const CQ_ENTRIES: u32 = PRODUCTION_CQ_ENTRIES;
// Unit tests exercise overflow handling but do not need production pool capacity.
// Small rings leave room for independent nextest invocations under one user.
// Keep the CQ above twice the SQ so the explicit-sizing regression stays sensitive.
#[cfg(test)]
const CQ_ENTRIES: u32 = 512;

/// Maximum completions copied at once while teardown is deadline-bounded.
const TEARDOWN_CQE_BATCH: usize = 64;

/// Extra mandatory submit attempts allowed after interrupted enters.
const TEARDOWN_EINTR_RETRIES: usize = 8;

/// Maximum time spent staging cancellations and draining completions.
const TEARDOWN_TIMEOUT: Duration = Duration::from_millis(3200);

/// Worker construction knobs.
///
/// The worker sizes its ring internally, with a completion queue that
/// comfortably covers the per-socket pool ceilings in [`udp::Config`].
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct Config {
	/// Where the worker accumulates its counters.
	///
	/// Default gives it a fresh set, still readable through
	/// [`Handle::metrics`]. Pass one in to hold a copy on the thread that
	/// spawned the worker, which is how an ops surface scrapes a worker it
	/// cannot otherwise reach. A [`Metrics`] clone shares one set of counters,
	/// so give each worker its own. Cloning this config starts a fresh set for
	/// the cloned worker.
	pub metrics: Metrics,
}

impl Clone for Config {
	fn clone(&self) -> Self {
		// A cloned construction plan targets a new worker, so its counters must
		// not be folded into the source worker's per-worker series.
		Self {
			metrics: Metrics::default(),
		}
	}
}

/// A thread-pinned io_uring executor: the ring, a timer queue, and a local
/// (`!Send`) task set, driven by a caller-owned loop.
///
/// Create one per thread, keep it on that thread (`!Send`), and drive it with
/// [`block_on`](Self::block_on). Everything else reaches the worker through
/// [`Handle`]: UDP sockets, timers, spawned tasks. Wakes from other threads
/// (any `Waker` this worker minted) are an atomic store plus, only while the
/// worker is parked, one futex syscall.
///
/// Dropping the worker makes a bounded attempt to submit the SQEs its last
/// turn staged and drain their completions. A datagram already handed to a
/// [`udp::Socket`] is included in that submission attempt, while operation
/// storage that the kernel might still access is safely leaked if teardown
/// cannot finish. It runs no tasks, though, so work a task has merely been
/// asked for is not performed: a QUIC close is queued on its connection and
/// framed by the driver task, so keep driving until the close is published
/// rather than stopping the worker on the call that asked for it.
pub struct Worker {
	shared: Rc<Shared>,
	tasks: kio::Tasks<Task>,
	park: kio::Park,
	/// Reused while copying CQEs out of the ring before dispatch.
	cqes: Vec<Cqe>,
	/// Whether the park-word `FUTEX_WAIT` SQE is in flight.
	futex_armed: bool,
	/// Registered on the timer queue, so a deadline armed off this thread while
	/// parked wakes the worker to re-park for it.
	owner: kio::Waiter,
	/// A manual clock's wake for the next deadline; the host clock needs none.
	alarm: Option<moq_clock::Sleep>,
}

impl Worker {
	/// Set up the ring, refusing kernels below Linux 6.12.
	///
	/// The floor buys incremental provided-buffer consumption, the absolute
	/// park timeout, and batched minimum waits with one code path; there is
	/// deliberately no fallback (use the tokio stack instead).
	pub fn new(config: Config) -> Result<Self, Error> {
		let Config { metrics } = config;
		let metrics = metrics.counters().clone();
		let ring = IoUring::builder()
			.setup_single_issuer()
			.setup_defer_taskrun()
			.setup_coop_taskrun()
			.setup_taskrun_flag()
			.setup_cqsize(CQ_ENTRIES)
			.build(SQ_ENTRIES)
			.map_err(|err| match err.raw_os_error() {
				// EINVAL from setup means the kernel predates one of the
				// requested flags (the ring geometry is compile-time valid),
				// so it never reaches the feature check below.
				Some(libc::ENOSYS) | Some(libc::EPERM) | Some(libc::EACCES) | Some(libc::EINVAL) => {
					Error::Unsupported(format!(
						"io_uring is unavailable ({err}); kernel {} (Linux 6.12+ required, and container seccomp \
						 policies such as Docker's default commonly block io_uring)",
						kernel_release()
					))
				}
				_ => Error::ring(err),
			})?;

		// One feature bit gates the whole floor: MIN_TIMEOUT landed in 6.12
		// alongside everything else this worker assumes.
		if !ring.params().is_feature_min_timeout() {
			return Err(Error::Unsupported(format!(
				"kernel {} is too old: moq-uring requires Linux 6.12+ (io_uring MIN_TIMEOUT feature missing)",
				kernel_release()
			)));
		}

		let clock = Clock::new();
		let deadlines = moq_clock::Deadlines::new(clock.now());
		let unpark = Unpark::new(metrics.clone());
		// A wake CQE can join a larger batch after warmup. Reserve the kernel's
		// actual queue size so copying those completions does not allocate.
		let cqes = Vec::with_capacity(ring.params().cq_entries() as usize);
		Ok(Self {
			owner: kio::Waiter::new(unpark.waker()),
			shared: Rc::new(Shared {
				ring: RefCell::new(ring),
				ops: RefCell::new(slab::Slab::new()),
				clock,
				deadlines,
				turning: std::cell::Cell::new(false),
				spawns: RefCell::new(Vec::new()),
				unpark,
				metrics,
				bgids: RefCell::default(),
				failed: std::cell::Cell::new(None),
				stopped: std::cell::Cell::new(false),
				spill: RefCell::new(std::collections::VecDeque::new()),
				#[cfg(test)]
				enter_faults: RefCell::default(),
			}),
			tasks: kio::Tasks::new(),
			park: kio::Park::default(),
			cqes,
			futex_armed: false,
			alarm: None,
		})
	}

	/// A cloneable handle for spawning, binding sockets, and minting timers.
	pub fn handle(&self) -> Handle {
		Handle {
			shared: self.shared.clone(),
		}
	}

	/// Drive the worker until `future` resolves.
	///
	/// Spawned tasks run alongside it and keep running across calls; they do
	/// not keep `block_on` alive. An `Err` means the ring itself failed, which
	/// is fatal to the worker.
	///
	/// While it runs, the worker's clock is the thread's ambient
	/// [`moq_clock`] clock: [`moq_clock::now`] reads the instant the current
	/// turn sampled, and [`moq_clock::sleep`] arms this worker's timers.
	pub fn block_on<F: Future>(&mut self, future: F) -> Result<F::Output, Error> {
		let mut future = std::pin::pin!(future);
		let waker = self.shared.unpark.waker();
		let _turn = Turn::enter(&self.shared);
		loop {
			self.shared.metrics.turns.add(1);
			// The turn's one clock read: every timer due by now fires, and
			// everything polled below sees this instant.
			self.shared.deadlines.advance(self.shared.clock.now());
			// Adopt tasks spawned since the last turn (spawning wakes us).
			let spawns = std::mem::take(&mut *self.shared.spawns.borrow_mut());
			for task in spawns {
				self.tasks.push(task);
			}
			// A failure recorded between calls (or as the last call's future
			// resolved) surfaces before a ready future can return past it.
			if let Some(err) = self.shared.failed.take() {
				return Err(err.into());
			}

			let cx = Context::from_waker(&waker);
			let waiter = self.park.hold(&cx);
			if let Poll::Ready(value) = waiter.poll_future(future.as_mut()) {
				return Ok(value);
			}
			// `Ready` just means the set is drained; the waiter stays
			// registered for the next push.
			let _ = self.tasks.poll(waiter);

			if let Some(err) = self.shared.failed.take() {
				return Err(err.into());
			}
			self.pump()?;
			self.maybe_park()?;
		}
	}

	/// Dispatch completions, carrying parked submissions into the wait enter.
	fn pump(&mut self) -> Result<(), Error> {
		let notified = self.shared.unpark.word.load(Ordering::Acquire) == crate::park::NOTIFIED;
		let full = !notified && !self.futex_armed && self.shared.ring.borrow_mut().submission().is_full();
		if full {
			self.shared.metrics.sq_full_enters.add(1);
		}
		// A notified turn will not park. Otherwise its SQEs can ride the
		// parked enter, unless the futex wait would have no slot left.
		if notified || full {
			self.submit()?;
		}
		self.reap(None)
	}

	/// Submit and reap completions only while the teardown budget remains.
	fn pump_until(&mut self, deadline: Instant) -> Result<(), Error> {
		self.submit()?;
		self.reap(Some(deadline))
	}

	fn reap(&mut self, deadline: Option<Instant>) -> Result<(), Error> {
		let clock = self.shared.clock.clone();
		loop {
			if deadline.is_some_and(|deadline| clock.host_now() >= deadline) {
				return Ok(());
			}
			// Copy the completions out so dispatch can borrow the ring (to
			// re-arm receives, push cancels, and so on). Completions spilled
			// by `Shared::push` predate the CQ's, so they dispatch first.
			self.cqes.clear();
			{
				let mut ring = self.shared.ring.borrow_mut();
				let mut spill = self.shared.spill.borrow_mut();
				let limit = deadline.map_or(usize::MAX, |_| TEARDOWN_CQE_BATCH);
				let spilled = spill.len().min(limit);
				self.cqes.extend(spill.drain(..spilled));
				self.cqes
					.extend(ring.completion().take(limit - spilled).map(|entry| Cqe {
						user_data: entry.user_data(),
						result: entry.result(),
						flags: entry.flags(),
					}));
			}
			self.shared.metrics.completions.add(self.cqes.len() as u64);
			if self.cqes.is_empty() || !self.dispatch_batch(deadline, || clock.host_now()) {
				return Ok(());
			}
		}
	}

	/// Dispatch the collected batch in `self.cqes` while its teardown budget
	/// remains.
	fn dispatch_batch(&mut self, deadline: Option<Instant>, mut now: impl FnMut() -> Instant) -> bool {
		for index in 0..self.cqes.len() {
			if deadline.is_some_and(|deadline| now() >= deadline) {
				// Drop this batch's remaining CQEs. Worker::drop will leak their
				// op state, which is safe even if the kernel already finished it.
				return false;
			}
			let cqe = self.cqes[index];
			self.dispatch(cqe);
		}
		true
	}

	fn submit(&mut self) -> Result<(), Error> {
		let mut ring = self.shared.ring.borrow_mut();
		{
			let sq = ring.submission();
			// Busy turns must flush deferred task work even without new SQEs.
			if sq.is_empty() && !sq.taskrun() && !sq.cq_overflow() {
				return Ok(());
			}
		}
		let before = ring.completion().len();
		let result = ring.submit();
		self.shared.metrics.entered(
			result.as_ref().map_or(0, |count| *count),
			ring.completion().len().saturating_sub(before),
		);
		match result {
			// A partial submit leaves the rest staged for the next pump.
			Ok(_) => Ok(()),
			// A signal interrupted the enter before it consumed anything. The
			// next worker turn retries the same staged SQEs.
			Err(err) if err.raw_os_error() == Some(libc::EINTR) => Ok(()),
			// The completion queue overflowed; the caller reaps and retries.
			Err(err) if err.raw_os_error() == Some(libc::EBUSY) => Ok(()),
			Err(err) => Err(err.into()),
		}
	}

	/// Submit every residual SQE without waiting for completions.
	fn submit_teardown(&mut self) -> Result<(), Error> {
		let mut ring = self.shared.ring.borrow_mut();
		let mut interruptions = 0;
		loop {
			if ring.submission().is_empty() {
				return Ok(());
			}
			let before = ring.completion().len();
			let result = ring.submit();
			self.shared.metrics.entered(
				result.as_ref().map_or(0, |count| *count),
				ring.completion().len().saturating_sub(before),
			);
			match result {
				// Keep submitting after partial progress. Returning zero while SQEs
				// remain would otherwise spin forever.
				Ok(0) => {
					return Err(std::io::Error::other("io_uring teardown submission made no progress").into());
				}
				Ok(_) => {}
				Err(err) => retry_teardown_submit(&mut interruptions, err)?,
			}
		}
	}

	/// Submit residual SQEs, then drain completions within `deadline`.
	fn drain_teardown(&mut self, deadline: Instant) {
		// Cancellation staging can consume the whole deadline. Existing SQEs,
		// especially sends, must still reach the kernel before it gates draining.
		let submission_failed = self.submit_teardown().is_err();
		if !submission_failed {
			let clock = self.shared.clock.clone();
			loop {
				if self.shared.ops.borrow().is_empty() {
					return;
				}
				if clock.host_now() >= deadline || self.pump_until(deadline).is_err() {
					break;
				}
				let Some(remaining) = deadline.checked_duration_since(clock.host_now()) else {
					break;
				};
				let mut ring = self.shared.ring.borrow_mut();
				let wait = remaining.min(std::time::Duration::from_millis(50));
				let ts = types::Timespec::from(wait);
				let args = types::SubmitArgs::new().timespec(&ts);
				let before = ring.completion().len();
				let result = ring.submitter().submit_with_args(1, &args);
				self.shared.metrics.entered(
					result.as_ref().map_or(0, |count| *count),
					ring.completion().len().saturating_sub(before),
				);
			}
		}
		if !self.shared.ops.borrow().is_empty() {
			// Leak the operations (and what they own) rather than free memory
			// the kernel may still touch.
			tracing::error!("dropping an io_uring worker with operations stuck in flight; leaking them");
			std::mem::forget(std::mem::take(&mut *self.shared.ops.borrow_mut()));
		}
	}

	/// Route one completion to its operation.
	fn dispatch(&mut self, cqe: Cqe) {
		let key = cqe.user_data as usize;

		// Terminal completions take their op out of the slab, releasing what
		// the kernel is now done with. A multishot receive with `more` set
		// stays armed, so only its socket is borrowed. The kernel posts
		// nothing for a key after its terminal CQE, so reusing the slot for
		// an op armed during dispatch is sound.
		enum Route {
			Live(Rc<udp::SockShared>),
			Done(Op),
		}

		let route = {
			let mut ops = self.shared.ops.borrow_mut();
			let Some(op) = ops.get(key) else {
				tracing::error!(key, "completion for an unknown operation");
				return;
			};
			let terminal = match op {
				Op::Recv { .. } => cqe.result < 0 || !io_uring::cqueue::more(cqe.flags),
				_ => true,
			};
			if terminal {
				Route::Done(ops.remove(key))
			} else {
				match op {
					Op::Recv { sock, .. } => Route::Live(sock.clone()),
					_ => unreachable!("only receives are non-terminal"),
				}
			}
		};

		match route {
			Route::Live(sock) => udp::on_recv(&self.shared, &sock, None, cqe, false),
			Route::Done(Op::Recv { sock, one }) => udp::on_recv(&self.shared, &sock, one, cqe, true),
			Route::Done(Op::Send(op)) => udp::on_send(op, cqe),
			Route::Done(Op::FutexWait) => self.futex_armed = false,
			Route::Done(Op::Cancel) => {}
		}
	}

	/// Park in `io_uring_enter` until a completion, a timer deadline, or a
	/// remote wake, unless a wake already arrived.
	fn maybe_park(&mut self) -> Result<(), Error> {
		let unpark = self.shared.unpark.clone();
		if unpark
			.word
			.compare_exchange(RUNNING, PARKED, Ordering::AcqRel, Ordering::Acquire)
			.is_err()
		{
			// Notified: consume it and poll again instead of parking.
			unpark.word.store(RUNNING, Ordering::Release);
			return Ok(());
		}

		// Registered while parked: a deadline armed off this thread before this
		// read is in it, and one after wakes the owner, which unparks.
		let deadline = self.shared.deadlines.park(&self.owner);
		let mut cx = self.owner.context();
		let wait = match self.shared.clock.wait(deadline, &mut self.alarm, &mut cx) {
			Wait::Elapsed => {
				unpark.word.store(RUNNING, Ordering::Release);
				return Ok(());
			}
			Wait::Forever => None,
			Wait::Until(at) => Some(types::Timespec::new().sec(at.as_secs()).nsec(at.subsec_nanos())),
		};

		// Keep exactly one FUTEX_WAIT armed. It waits while the word still
		// holds PARKED; a remote unpark stores NOTIFIED and kicks the futex,
		// and if the store lands before this submission the wait completes
		// immediately with EAGAIN. Either way there is a CQE to wake us.
		if !self.futex_armed {
			let key = self.shared.insert(Op::FutexWait);
			let entry = opcode::FutexWait::new(
				unpark.word.as_ptr(),
				PARKED as u64,
				FUTEX_BITSET_MATCH_ANY,
				FUTEX2_SIZE_U32 | FUTEX2_PRIVATE,
			)
			.build()
			.user_data(key);
			if let Err(err) = self.shared.push(&entry) {
				self.shared.ops.borrow_mut().remove(key as usize);
				unpark.word.store(RUNNING, Ordering::Release);
				return Err(err.into());
			}
			self.futex_armed = true;
		}

		self.shared.metrics.parks.add(1);
		let result = {
			let mut ring = self.shared.ring.borrow_mut();
			let to_submit = ring.submission().len() as u32;
			let before = ring.completion().len();
			let submitter = ring.submitter();
			let result = match wait {
				None => submitter.submit_and_wait(1),
				Some(ts) => {
					// Zero timeout SQEs: the earliest userspace deadline rides
					// the enter call as an absolute CLOCK_MONOTONIC timeout.
					let args = types::SubmitArgs::new().timespec(&ts);
					let flags = EnterFlags::GETEVENTS | EnterFlags::EXT_ARG | EnterFlags::ABS_TIMER;
					// SAFETY: `args` (and the timespec it references) outlive
					// the call, and EXT_ARG matches its type.
					unsafe { submitter.enter(to_submit, 1, flags.bits(), Some(&args)) }
				}
			};
			self.shared.metrics.entered(
				result.as_ref().map_or(0, |count| *count),
				ring.completion().len().saturating_sub(before),
			);
			result
		};
		unpark.word.store(RUNNING, Ordering::Release);

		match result {
			Ok(_) => Ok(()),
			Err(err)
				if matches!(
					err.raw_os_error(),
					Some(libc::ETIME) | Some(libc::EINTR) | Some(libc::EBUSY)
				) =>
			{
				Ok(())
			}
			Err(err) => Err(err.into()),
		}
	}
}

/// A running turn: the worker's queue is the ambient clock and holds the
/// current instant until this drops.
struct Turn {
	shared: Rc<Shared>,
	_ambient: moq_clock::Entered,
}

impl Turn {
	fn enter(shared: &Rc<Shared>) -> Self {
		shared.turning.set(true);
		Self {
			shared: shared.clone(),
			_ambient: shared.deadlines.enter(),
		}
	}
}

impl Drop for Turn {
	fn drop(&mut self) {
		self.shared.turning.set(false);
	}
}

impl Drop for Worker {
	fn drop(&mut self) {
		// Handles may outlive us; everything they try from here on fails
		// instead of pending on a loop that will never run again.
		self.shared.stopped.set(true);
		// One deadline bounds cancellation staging and draining together. It is
		// on the host's time, so it passes even on a manual clock.
		let deadline = self.shared.clock.host_now() + TEARDOWN_TIMEOUT;
		// The kernel may still write into provided buffers and read send
		// headers owned by the ops slab. Queue cancels behind every staged
		// receive and the futex, so partial submissions cannot strand an
		// uncancelled operation. Sends are deliberately left alone: a datagram
		// staged by the final worker turn still has to reach the wire.
		let cancel: Vec<u64> = self
			.shared
			.ops
			.borrow()
			.iter()
			.filter_map(|(key, op)| matches!(op, Op::Recv { .. } | Op::FutexWait).then_some(key as u64))
			.collect();
		let mut cancellation_failed = false;
		for key in cancel {
			if self.shared.clock.host_now() >= deadline {
				cancellation_failed = true;
				break;
			}
			cancellation_failed |= self.shared.cancel_until(key, deadline).is_err();
		}
		if cancellation_failed {
			tracing::error!("failed to queue one or more io_uring teardown cancellations");
		}
		self.drain_teardown(deadline);
	}
}

impl std::fmt::Debug for Worker {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Worker").field("tasks", &self.tasks.len()).finish()
	}
}

/// A worker's cloneable, thread-local handle.
///
/// Everything that is not the drive loop goes through this:
/// [`spawn`](Self::spawn), [`udp`](Self::udp), [`timer`](Self::timer), and
/// [`run`](Self::run) for MoQ drivers. `!Send`, like everything the worker owns.
pub struct Handle {
	shared: Rc<Shared>,
}

impl Clone for Handle {
	fn clone(&self) -> Self {
		Self {
			shared: self.shared.clone(),
		}
	}
}

impl Handle {
	/// This worker's counters, readable from any thread.
	pub fn metrics(&self) -> Metrics {
		Metrics::from_counters(self.shared.metrics.clone())
	}

	/// Run a `!Send` future on this worker until completion.
	///
	/// If the worker has already been dropped the future is dropped instead of
	/// running, like a task spawned on a shut-down runtime.
	pub fn spawn(&self, future: impl Future<Output = ()> + 'static) {
		if self.shared.stopped.get() {
			return;
		}
		let mut future = Box::pin(future);
		self.shared
			.spawns
			.borrow_mut()
			.push(Box::new(move |waiter: &kio::Waiter| {
				waiter.poll_future(future.as_mut())
			}));
		// Spawning from another task (or before block_on) must reach the next
		// turn's drain.
		self.shared.unpark.unpark();
	}

	/// Drive `socket` through this worker's ring.
	///
	/// The caller configures and binds the socket (options, addresses); this
	/// takes over receive and send. `config` picks the batching mechanisms.
	///
	/// The socket is what names this worker from here on: an
	/// [`Endpoint`](crate::quic::Endpoint) built on it runs its tasks here,
	/// whichever thread's handle built it. A member of a steered reuseport
	/// group ([`moq_sock::shard::Socket`]) brings its slot along, so the
	/// connection ids issued through it steer back to this socket.
	pub fn udp(&self, socket: impl Into<udp::Bound>, config: udp::Config) -> Result<udp::Socket, Error> {
		if self.shared.stopped.get() {
			return Err(Shared::gone_error().into());
		}
		udp::Socket::bind(&self.shared, socket.into(), config)
	}
}

/// The worker behind a socket, endpoint, or connection, held weakly.
///
/// I/O carries its owner so it cannot be driven through a different worker,
/// but a handle to that I/O must not keep a dropped worker's ring alive, so
/// this holds no strong reference. Once the worker is gone, spawning is a
/// no-op (like [`Handle::spawn`]) and timers never fire, which is what the
/// tasks that would have consumed them expect.
#[derive(Clone)]
pub(crate) struct Owner {
	shared: Weak<Shared>,
	/// Held directly: a timer on a dropped worker still has to exist, since
	/// the driver that owns it is torn down by the same drop that would need
	/// it.
	#[cfg_attr(not(feature = "quic"), allow(dead_code, reason = "only the `quic` feature uses it"))]
	deadlines: moq_clock::Deadlines,
	#[cfg_attr(not(feature = "quic"), allow(dead_code, reason = "only the `quic` feature uses it"))]
	metrics: std::sync::Arc<crate::metrics::Counters>,
}

impl Owner {
	pub(crate) fn new(shared: &Rc<Shared>) -> Self {
		Self {
			shared: Rc::downgrade(shared),
			deadlines: shared.deadlines.clone(),
			metrics: shared.metrics.clone(),
		}
	}

	/// The worker's time: the current turn's instant, or the clock's between turns.
	#[cfg_attr(not(feature = "quic"), allow(dead_code, reason = "only the `quic` feature uses it"))]
	pub fn now(&self) -> Instant {
		self.shared
			.upgrade()
			.map_or_else(|| self.deadlines.now(), |shared| shared.now())
	}

	/// The worker's core while it is still allocated, torn down or not.
	pub fn upgrade(&self) -> Option<Rc<Shared>> {
		self.shared.upgrade()
	}

	/// A strong handle, or `None` once the worker is dropped or torn down.
	pub fn handle(&self) -> Option<Handle> {
		let shared = self.shared.upgrade()?;
		(!shared.stopped.get()).then_some(Handle { shared })
	}

	/// Run a `!Send` future on the worker, or drop it if the worker is gone.
	#[cfg_attr(not(feature = "quic"), allow(dead_code, reason = "only the `quic` feature uses it"))]
	pub fn spawn(&self, future: impl Future<Output = ()> + 'static) {
		if let Some(handle) = self.handle() {
			handle.spawn(future);
		}
	}

	/// A disarmed timer on the worker.
	#[cfg_attr(not(feature = "quic"), allow(dead_code, reason = "only the `quic` feature uses it"))]
	pub fn timer(&self) -> crate::Timer {
		crate::Timer::from_queue(&self.deadlines, self.metrics.clone())
	}

	/// A timer that expires after `duration`.
	#[cfg_attr(not(feature = "quic"), allow(dead_code, reason = "only the `quic` feature uses it"))]
	pub fn after(&self, duration: Duration) -> crate::Timer {
		let mut timer = self.timer();
		timer.set(self.now().checked_add(duration));
		timer
	}
}

impl std::fmt::Debug for Handle {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Handle").finish()
	}
}

impl Handle {
	/// Allocate a disarmed timer on this worker.
	pub fn timer(&self) -> crate::Timer {
		crate::Timer::from_queue(&self.shared.deadlines, self.shared.metrics.clone())
	}

	/// The worker's time: the current turn's instant, or the clock's between turns.
	pub(crate) fn now(&self) -> Instant {
		self.shared.now()
	}

	/// Run a MoQ driver on this worker's timers and clock, resolving with its
	/// terminal error.
	pub async fn run<D: moq_net::time::Driver>(&self, mut driver: D) -> moq_net::Error {
		let mut timer = self.timer();
		kio::wait(|waiter| {
			loop {
				match driver.poll(self.now(), waiter) {
					Ok(at) => timer.set(at),
					Err(err) => return Poll::Ready(err),
				}
				if timer.poll(waiter).is_pending() {
					return Poll::Pending;
				}
			}
		})
		.await
	}
}

/// The running kernel release, for error messages.
fn kernel_release() -> String {
	// SAFETY: all-zero is a valid utsname out-buffer.
	let mut uts: libc::utsname = unsafe { std::mem::zeroed() };
	// SAFETY: valid out-pointer.
	if unsafe { libc::uname(&mut uts) } != 0 {
		return "unknown".into();
	}
	// SAFETY: uname NUL-terminates the release field.
	unsafe { std::ffi::CStr::from_ptr(uts.release.as_ptr()) }
		.to_string_lossy()
		.into_owned()
}

/// Accept a bounded number of interrupted teardown submissions.
fn retry_teardown_submit(interruptions: &mut usize, err: std::io::Error) -> std::io::Result<()> {
	if err.raw_os_error() != Some(libc::EINTR) || *interruptions >= TEARDOWN_EINTR_RETRIES {
		return Err(err);
	}
	*interruptions += 1;
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	use crate::Timer as Deadline;
	use std::time::Duration;

	/// Local unsupported kernels may skip; strict CI must exercise the worker.
	fn worker() -> Option<Worker> {
		worker_with(Config::default())
	}

	fn worker_with(config: Config) -> Option<Worker> {
		worker_result(Worker::new(config), std::env::var_os("CI_STRICT").is_some())
	}

	/// A worker on a manual clock, which only `advance` moves.
	fn manual() -> Option<(Worker, moq_clock::Installed)> {
		let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
		let clock = moq_clock::Manual::new().install(runtime.handle());
		let _entered = runtime.enter();
		Some((worker()?, clock))
	}

	/// Past on the host clock but ahead of the worker's last turn, so a park
	/// for it times out at once.
	fn due(worker: &Worker) -> Instant {
		worker.shared.deadlines.now() + Duration::from_nanos(1)
	}

	fn worker_result(result: Result<Worker, Error>, required: bool) -> Option<Worker> {
		match result {
			Ok(worker) => Some(worker),
			Err(Error::Unsupported(reason)) if !required => {
				eprintln!("skipping io_uring test: {reason}");
				None
			}
			Err(err) => panic!("worker setup failed: {err}"),
		}
	}

	#[test]
	#[should_panic(expected = "worker setup failed")]
	fn strict_worker_refuses_unsupported() {
		worker_result(Err(Error::Unsupported("test capability".into())), true);
	}

	#[test]
	fn optional_worker_skips_unsupported() {
		assert!(worker_result(Err(Error::Unsupported("test capability".into())), false).is_none());
	}

	#[test]
	fn cloned_config_has_fresh_metrics() {
		let config = Config::default();
		let clone = config.clone();
		assert!(!std::sync::Arc::ptr_eq(
			config.metrics.counters(),
			clone.metrics.counters()
		));
	}

	#[test]
	fn a_parked_turn_enters_once() {
		let Some(mut worker) = worker() else { return };
		let key = worker.shared.insert(Op::Cancel);
		worker
			.shared
			.push(&opcode::Nop::new().build().user_data(key))
			.expect("stage NOP");
		let mut timer = worker.handle().timer();
		timer.set(Some(due(&worker)));
		worker.pump().expect("pump");
		worker.maybe_park().expect("park");
		assert_eq!(
			worker.handle().metrics().snapshot().enters,
			1,
			"the parked enter carries the staged SQE"
		);
	}

	/// A wake-before-arm futex CQE can join a send-sized batch after warmup.
	#[test]
	fn a_larger_wake_batch_reuses_completion_storage() {
		let Some(mut worker) = worker() else { return };
		for _ in 0..4 {
			let key = worker.shared.insert(Op::Cancel);
			worker.shared.push(&opcode::Nop::new().build().user_data(key)).unwrap();
		}
		worker.shared.ring.borrow().submitter().submit_and_wait(4).unwrap();
		worker.reap(None).unwrap();
		let capacity = worker.cqes.capacity();
		let before = worker.handle().metrics().snapshot();

		for _ in 0..4 {
			let key = worker.shared.insert(Op::Cancel);
			worker.shared.push(&opcode::Nop::new().build().user_data(key)).unwrap();
		}
		// The wake lands after committing to park but before the kernel arms it.
		// FUTEX_WAIT therefore posts EAGAIN beside the four ordinary completions.
		let unpark = worker.shared.unpark.clone();
		unpark.word.store(PARKED, Ordering::Release);
		unpark.unpark();
		let key = worker.shared.insert(Op::FutexWait);
		let entry = opcode::FutexWait::new(
			unpark.word.as_ptr(),
			PARKED as u64,
			FUTEX_BITSET_MATCH_ANY,
			FUTEX2_SIZE_U32 | FUTEX2_PRIVATE,
		)
		.build()
		.user_data(key);
		worker.shared.push(&entry).unwrap();
		worker.futex_armed = true;
		worker.shared.ring.borrow().submitter().submit_and_wait(5).unwrap();
		worker.reap(None).unwrap();

		let after = worker.handle().metrics().snapshot();
		assert_eq!(after.completions - before.completions, 5);
		assert_eq!(after.wakes - before.wakes, 1);
		assert!(!worker.futex_armed, "the futex completion must be dispatched");
		assert_eq!(
			worker.cqes.capacity(),
			capacity,
			"a larger steady-state batch grew the scratch buffer"
		);
	}

	#[test]
	fn a_full_queue_forces_one_pressure_enter() {
		let Some(mut worker) = worker() else { return };
		for _ in 0..SQ_ENTRIES {
			let key = worker.shared.insert(Op::Cancel);
			worker
				.shared
				.push(&opcode::Nop::new().build().user_data(key))
				.expect("stage NOP");
		}
		worker.pump().expect("full pump");
		let snapshot = worker.handle().metrics().snapshot();
		assert_eq!(snapshot.sq_full_enters, 1);
		assert_eq!(snapshot.enters, 1);
		assert_eq!(snapshot.sqes.sum, u64::from(SQ_ENTRIES));
		assert_eq!(snapshot.sqes.count(), snapshot.enters);
		assert_eq!(snapshot.cqes.count(), snapshot.enters);
	}

	#[test]
	fn an_armed_futex_carries_a_full_queue_in_the_parked_enter() {
		let Some(mut worker) = worker() else { return };
		let mut timer = worker.handle().timer();
		timer.set(Some(due(&worker)));
		worker.maybe_park().expect("arm futex");
		assert!(worker.futex_armed);
		let before = worker.handle().metrics().snapshot();
		for _ in 0..SQ_ENTRIES {
			let key = worker.shared.insert(Op::Cancel);
			worker
				.shared
				.push(&opcode::Nop::new().build().user_data(key))
				.expect("stage NOP");
		}
		worker.pump().expect("full pump");
		worker.maybe_park().expect("park with armed futex");
		let after = worker.handle().metrics().snapshot();
		assert_eq!(after.enters - before.enters, 1);
		assert_eq!(after.sq_full_enters - before.sq_full_enters, 0);
	}

	#[test]
	fn a_notified_turn_flushes_deferred_completions_without_sqes() {
		let Some(mut worker) = worker() else { return };
		let handle = worker.handle();
		let sock = handle
			.udp(
				std::net::UdpSocket::bind("127.0.0.1:0").expect("bind"),
				udp::Config::default(),
			)
			.expect("socket");
		let peer = std::net::UdpSocket::bind("127.0.0.1:0").expect("peer");
		// Arm the receive before the peer sends, so completion happens outside
		// the submit syscall and is deferred to a later GETEVENTS enter.
		worker.shared.ring.borrow_mut().submit().expect("arm receive");
		peer.send_to(b"deferred", sock.local_addr().expect("addr"))
			.expect("send");
		let deadline = worker.shared.clock.now() + Duration::from_secs(5);
		while !worker.shared.ring.borrow_mut().submission().taskrun() {
			assert!(
				worker.shared.clock.now() < deadline,
				"the kernel did not flag deferred task work"
			);
			std::thread::yield_now();
		}
		assert!(worker.shared.ring.borrow_mut().submission().is_empty());
		// A continuously self-waking future never parks. Its next pump must
		// dispatch the receive even though there is nothing new to submit.
		worker.shared.unpark.unpark();
		worker.pump().expect("notified pump");
		let Poll::Ready(Ok(packet)) = sock.poll_recv(&kio::Waiter::noop()) else {
			panic!("one busy turn must publish the receive");
		};
		assert_eq!(packet.payload(), b"deferred");
	}

	#[test]
	fn ready_future() {
		let Some(mut worker) = worker() else { return };
		let value = worker.block_on(async { 7 }).unwrap();
		assert_eq!(value, 7);
		assert_eq!(worker.handle().metrics().snapshot().turns, 1);
	}

	#[test]
	fn spawned_tasks_run() {
		let Some(mut worker) = worker() else { return };
		let handle = worker.handle();
		let flag = Rc::new(std::cell::Cell::new(0));

		for index in 0..3 {
			let flag = flag.clone();
			handle.spawn(async move {
				flag.set(flag.get() + index + 1);
			});
		}
		// Spawned tasks run even while the main future pends on a timer.
		let handle2 = handle.clone();
		worker
			.block_on(async move {
				Deadline::after(&handle2, Duration::from_millis(10)).wait().await;
			})
			.unwrap();
		assert_eq!(flag.get(), 6);
	}

	/// Nothing else wakes this worker: the park's absolute timeout is the only
	/// thing that can fire the deadline, and it fires on the worker's own clock.
	#[test]
	fn deadline_fires_at_park() {
		let Some(mut worker) = worker() else { return };
		let handle = worker.handle();
		let at = worker
			.block_on(async move {
				let at = moq_clock::now() + Duration::from_millis(50);
				let mut timer = handle.timer();
				timer.set(Some(at));
				timer.wait().await;
				assert!(moq_clock::now() >= at, "fired before the turn reached it");
				at
			})
			.unwrap();
		let snap = worker.handle().metrics().snapshot();
		assert!(snap.parks > 0, "the worker never parked: {snap:?}");
		assert_eq!(snap.timers_fired, 1, "{snap:?}");
		assert!(worker.shared.deadlines.now() >= at);
	}

	/// A parked worker on a manual clock wakes when the clock is advanced past
	/// its deadline, and not before.
	#[test]
	fn a_manual_clock_wakes_the_parked_worker() {
		let Some((mut worker, clock)) = manual() else { return };
		let start = clock.now();
		let parks = worker.handle().metrics();
		let advancer = std::thread::spawn(move || {
			// The worker parks once on the armed deadline and stays parked: the
			// clock never moves on its own.
			while parks.snapshot().parks == 0 {
				std::hint::spin_loop();
			}
			clock.advance(Duration::from_secs(5));
		});
		let woke = worker
			.block_on(async move {
				moq_clock::sleep(Duration::from_secs(5)).await;
				moq_clock::now()
			})
			.unwrap();
		advancer.join().unwrap();
		assert_eq!(woke, start + Duration::from_secs(5));
	}

	/// Ambient timers and reads on the worker resolve to its clock.
	#[test]
	fn the_worker_is_the_ambient_clock() {
		let Some((mut worker, clock)) = manual() else { return };
		clock.advance(Duration::from_secs(1));
		let handle = worker.handle();
		let (now, timed_out) = worker
			.block_on(async move {
				let now = moq_clock::now();
				// Due at once, so this needs no advance.
				let timed_out = moq_clock::timeout(Duration::ZERO, std::future::pending::<()>()).await;
				assert_eq!(handle.now(), now);
				(now, timed_out.is_err())
			})
			.unwrap();
		assert_eq!(now, moq_clock::Instant::ZERO + Duration::from_secs(1));
		assert!(timed_out);
	}

	/// Wall time on a worker comes from the manual clock it runs on, so it agrees
	/// with the worker's instants.
	#[test]
	fn the_worker_reads_its_manual_clocks_wall_time() {
		let Some((mut worker, clock)) = manual() else { return };
		clock.advance(Duration::from_secs(5));
		let (wall, origin) = worker
			.block_on(async { (moq_clock::wall(), moq_clock::origin()) })
			.unwrap();
		assert_eq!(wall, clock.wall(), "the worker read the host's wall time");
		assert_eq!(origin.0, clock.now());
		assert_eq!(origin.1, clock.wall());
	}

	/// A timer made between turns counts from the clock, not from the last turn's
	/// instant, so an idle worker does not hand out deadlines already past.
	#[test]
	fn a_timer_between_turns_counts_from_the_clock() {
		let Some((mut worker, clock)) = manual() else { return };
		let handle = worker.handle();
		worker.block_on(async {}).unwrap();
		clock.advance(Duration::from_secs(10));
		let mut timer = crate::Timer::after(&handle, Duration::from_secs(5));

		clock.advance(Duration::from_secs(4));
		worker.block_on(async {}).unwrap();
		assert!(
			timer.poll(&kio::Waiter::noop()).is_pending(),
			"fired 5s after the previous turn"
		);
		clock.advance(Duration::from_secs(1));
		worker.block_on(async {}).unwrap();
		assert!(timer.poll(&kio::Waiter::noop()).is_ready());
	}

	#[test]
	fn timer_rearm_and_disarm() {
		let Some((mut worker, clock)) = manual() else { return };
		let handle = worker.handle();
		let mut timer = handle.timer();
		let now = handle.now();

		// Disarmed timers never fire.
		assert!(timer.poll(&kio::Waiter::noop()).is_pending());

		// An instant already reached is immediately elapsed, and stays elapsed
		// (fused) until re-armed.
		timer.set(Some(now));
		assert!(timer.poll(&kio::Waiter::noop()).is_ready());
		assert!(timer.poll(&kio::Waiter::noop()).is_ready());

		// Re-arming to the future pends again; disarming stays pending.
		timer.set(Some(now + Duration::from_secs(60)));
		assert!(timer.poll(&kio::Waiter::noop()).is_pending());
		timer.set(None);
		assert!(timer.poll(&kio::Waiter::noop()).is_pending());

		// And a re-arm fires through the worker once its clock gets there.
		timer.set(Some(now + Duration::from_millis(20)));
		clock.advance(Duration::from_millis(20));
		worker
			.block_on(async move {
				kio::wait(|waiter| timer.poll(waiter)).await;
			})
			.unwrap();
		assert_eq!(handle.now(), now + Duration::from_millis(20));
	}

	#[test]
	fn dropped_worker_rejects_operations() {
		let Some((worker, clock)) = manual() else { return };
		// Far past the host's time, which bounds teardown: comparing the two
		// would end the drain before it dispatched anything.
		clock.advance(Duration::from_secs(86_400));
		let handle = worker.handle();
		let bind = || std::net::UdpSocket::bind("127.0.0.1:0").expect("bind");
		let sock = handle.udp(bind(), udp::Config::default()).expect("socket");
		let shared = sock.downgrade();
		let to = sock.local_addr().expect("addr");
		let Poll::Ready(Ok(mut tx)) = sock.poll_acquire(&kio::Waiter::noop()) else {
			panic!("no tx buffer");
		};
		tx.resize(1200, 0);
		drop(worker);

		// Every path a retained handle can reach fails instead of pending on
		// a loop that will never run again.
		assert!(handle.udp(bind(), udp::Config::default()).is_err());
		assert!(matches!(sock.poll_recv(&kio::Waiter::noop()), Poll::Ready(Err(_))));
		assert!(matches!(sock.poll_acquire(&kio::Waiter::noop()), Poll::Ready(Err(_))));
		assert!(tx.send(udp::Transmit::new(to, 1200)).is_err());
		// And a late spawn is dropped rather than parked forever.
		handle.spawn(async {});
		drop(sock);
		assert!(shared.upgrade().is_none(), "the worker leaked its staged receive");
	}

	#[test]
	fn teardown_stops_between_completions_at_the_deadline() {
		let Some(mut worker) = worker() else { return };
		let first = worker.shared.insert(Op::Cancel);
		let second = worker.shared.insert(Op::Cancel);
		let cqe = |user_data| Cqe {
			user_data,
			result: 0,
			flags: 0,
		};
		let before = moq_clock::Instant::ZERO;
		let deadline = before + Duration::from_millis(1);
		let mut now = [before, deadline].into_iter();

		worker.cqes = vec![cqe(first), cqe(second)];
		assert!(!worker.dispatch_batch(Some(deadline), || {
			now.next().expect("one deadline check per completion")
		}));
		assert!(!worker.shared.ops.borrow().contains(first as usize));
		assert!(worker.shared.ops.borrow().contains(second as usize));
		worker.shared.ops.borrow_mut().remove(second as usize);
	}

	#[test]
	fn expired_teardown_submits_residual_sqes() {
		let Some(mut worker) = worker() else { return };
		// A NOP needs no slab-owned memory, so it can observe the SQ directly.
		for _ in 0..SQ_ENTRIES {
			worker.shared.push(&opcode::Nop::new().build()).expect("stage NOP");
		}
		assert_eq!(worker.shared.ring.borrow_mut().submission().len(), SQ_ENTRIES as usize);

		worker.drain_teardown(worker.shared.clock.host_now());
		assert!(worker.shared.ring.borrow_mut().submission().is_empty());
	}

	#[test]
	fn teardown_submit_interrupt_budget_is_finite() {
		let interrupted = || std::io::Error::from_raw_os_error(libc::EINTR);
		let mut interruptions = 0;
		for _ in 0..TEARDOWN_EINTR_RETRIES {
			retry_teardown_submit(&mut interruptions, interrupted()).expect("retry interrupted submit");
		}
		assert_eq!(interruptions, TEARDOWN_EINTR_RETRIES);
		assert_eq!(
			retry_teardown_submit(&mut interruptions, interrupted())
				.expect_err("interrupt budget must be finite")
				.raw_os_error(),
			Some(libc::EINTR)
		);
	}

	#[test]
	fn dropped_worker_drains_more_receives_than_the_submission_queue() {
		let Some(worker) = worker() else { return };
		let handle = worker.handle();
		let config = udp::Config {
			gro: false,
			gso: false,
			multishot: false,
			rx_buffers_max: 1,
			rx_buffer_len: 2048,
			tx_buffers_max: 1,
			tx_buffer_len: 2048,
		};
		let mut sockets = Vec::new();
		let mut shared = Vec::new();
		for _ in 0..=SQ_ENTRIES {
			let sock = handle
				.udp(std::net::UdpSocket::bind("127.0.0.1:0").expect("bind"), config.clone())
				.expect("socket");
			shared.push(sock.downgrade());
			sockets.push(sock);
		}

		drop(worker);
		drop(sockets);
		assert!(
			shared.iter().all(|shared| shared.upgrade().is_none()),
			"the worker leaked a receive staged across submission batches"
		);
	}

	#[test]
	fn cq_covers_the_default_pool_ceilings() {
		// The completion queue must cover a socket at its default pool
		// ceilings (plus the futex), or the kernel's overflow slow path
		// becomes steady state for the workload the ceilings exist to serve.
		// Fails when someone raises the udp defaults without revisiting
		// PRODUCTION_CQ_ENTRIES.
		let config = udp::Config::default();
		let per_socket = u32::from(config.tx_buffers_max) + u32::from(config.rx_buffers_max);
		assert!(
			PRODUCTION_CQ_ENTRIES > per_socket,
			"PRODUCTION_CQ_ENTRIES fell behind the pool defaults"
		);
	}

	#[test]
	fn the_ring_honors_the_requested_cq_depth() {
		// The kernel-reported geometry, not the constant: dropping the
		// `setup_cqsize` call would silently fall back to a CQ of twice the SQ.
		// Both builds request more; the overflow test below cannot catch a
		// smaller queue because it expects overflow.
		let Some(worker) = worker() else { return };
		let cq = worker.shared.ring.borrow().params().cq_entries();
		assert!(cq >= CQ_ENTRIES, "kernel granted a {cq}-entry CQ, wanted {CQ_ENTRIES}");
		assert_eq!(
			worker.cqes.capacity(),
			cq as usize,
			"scratch must cover the accepted CQ geometry"
		);
	}

	#[test]
	fn completion_overflow_is_survivable() {
		let Some(mut worker) = worker() else { return };
		let handle = worker.handle();
		// Twice the CQ's worth of sends, staged synchronously so nothing
		// reaps while they complete: the kernel must backlog the completions
		// (`IORING_FEAT_NODROP`) and the worker must drain them without any
		// operation, socket, or the worker itself failing.
		let ceiling = (CQ_ENTRIES * 2) as u16;
		let config = udp::Config {
			tx_buffers_max: ceiling,
			tx_buffer_len: 2048,
			..Default::default()
		};
		let sock = handle
			.udp(std::net::UdpSocket::bind("127.0.0.1:0").expect("bind"), config)
			.expect("socket");
		let to = sock.local_addr().expect("addr");

		let mut held = Vec::new();
		while let Poll::Ready(Ok(tx)) = sock.poll_acquire(&kio::Waiter::noop()) {
			held.push(tx);
		}
		assert_eq!(held.len(), usize::from(ceiling));
		for mut tx in held.drain(..) {
			tx.resize(1200, 0);
			tx.send(udp::Transmit::new(to, 1200)).expect("send");
		}

		// The point of the test is the overflow, so prove it happened: the
		// kernel raises this flag while completions sit in its backlog. It
		// clears once the backlog flushes, so sample it before each sweep.
		let saw_overflow = |worker: &Worker| worker.shared.ring.borrow_mut().submission().cq_overflow();
		let mut overflowed = saw_overflow(&worker);

		// Drive the worker until every completion, backlog included, has been
		// reaped and released its buffer back to the pool.
		let deadline = handle.now() + Duration::from_secs(10);
		loop {
			overflowed = overflowed || saw_overflow(&worker);
			let h = handle.clone();
			worker
				.block_on(async move {
					Deadline::after(&h, Duration::from_millis(10)).wait().await;
				})
				.unwrap();
			let mut free = Vec::new();
			loop {
				match sock.poll_acquire(&kio::Waiter::noop()) {
					Poll::Ready(Ok(tx)) => free.push(tx),
					Poll::Ready(Err(err)) => panic!("send path failed: {err}"),
					Poll::Pending => break,
				}
			}
			if free.len() == usize::from(ceiling) {
				break;
			}
			assert!(
				handle.now() < deadline,
				"buffers stuck in flight: {} of {ceiling} free",
				free.len()
			);
		}
		assert!(overflowed, "the burst never overflowed the CQ; it proves nothing");
		// The receive side rode out the same storm: whatever the loopback
		// delivered drains without a terminal error.
		while let Poll::Ready(result) = sock.poll_recv(&kio::Waiter::noop()) {
			result.expect("receive path failed");
		}
	}

	#[test]
	fn oversized_receive_pool_is_rejected() {
		let Some(worker) = worker() else { return };
		let handle = worker.handle();
		// Without validation the power-of-two rounding wraps to a zero-entry
		// ring, which allocates nothing and underflows its mask.
		let config = udp::Config {
			rx_buffers_max: u16::MAX,
			..Default::default()
		};
		let err = handle
			.udp(std::net::UdpSocket::bind("127.0.0.1:0").expect("bind"), config)
			.expect_err("oversized pool");
		assert!(matches!(err, Error::Io(err) if err.kind() == std::io::ErrorKind::InvalidInput));
	}

	#[test]
	fn the_send_pool_grows_to_its_ceiling() {
		let Some(worker) = worker() else { return };
		let handle = worker.handle();
		// A ceiling is a bound, not a reservation: 65535 default-length buffers
		// would be 4 GiB if the pool were allocated up front.
		let config = udp::Config {
			tx_buffers_max: u16::MAX,
			..Default::default()
		};
		handle
			.udp(std::net::UdpSocket::bind("127.0.0.1:0").expect("bind"), config)
			.expect("socket");

		// Short buffers so the whole ceiling fits in a test.
		let config = udp::Config {
			tx_buffers_max: 200,
			tx_buffer_len: 4096,
			..Default::default()
		};
		let sock = handle
			.udp(std::net::UdpSocket::bind("127.0.0.1:0").expect("bind"), config)
			.expect("socket");

		// Holding every buffer starves the pool, which grows past its initial
		// floor rather than serializing the caller behind it, and stops at the
		// ceiling.
		let mut held = Vec::new();
		while let Poll::Ready(Ok(tx)) = sock.poll_acquire(&kio::Waiter::noop()) {
			held.push(tx);
		}
		assert_eq!(held.len(), 200);
		drop(worker);
	}

	#[test]
	fn ungso_send_is_not_capped_at_a_train() {
		let Some(worker) = worker() else { return };
		let handle = worker.handle();
		// Without GSO each segment rides its own `sendmsg`, so the kernel's
		// 64-segment train limit does not apply.
		let config = udp::Config {
			gso: false,
			..Default::default()
		};
		let sock = handle
			.udp(std::net::UdpSocket::bind("127.0.0.1:0").expect("bind"), config)
			.expect("socket");
		let to = sock.local_addr().expect("addr");
		let Poll::Ready(Ok(mut tx)) = sock.poll_acquire(&kio::Waiter::noop()) else {
			panic!("no tx buffer");
		};
		tx.resize(64 * 1024, 0);
		tx.send(udp::Transmit::new(to, 1000)).expect("send 66 datagrams");
		drop(worker);
	}

	#[test]
	fn ungso_send_is_capped_by_the_ring() {
		let Some(worker) = worker() else { return };
		let handle = worker.handle();
		// Without GSO the segment count is the `sendmsg` count, and `push`
		// submits inline without reaping once the queue is full, so one call
		// must not outrun the ring.
		let config = udp::Config {
			gso: false,
			..Default::default()
		};
		let sock = handle
			.udp(std::net::UdpSocket::bind("127.0.0.1:0").expect("bind"), config)
			.expect("socket");
		let to = sock.local_addr().expect("addr");
		let Poll::Ready(Ok(mut tx)) = sock.poll_acquire(&kio::Waiter::noop()) else {
			panic!("no tx buffer");
		};
		tx.resize(64 * 1024, 0);
		let err = tx
			.send(udp::Transmit::new(to, 1))
			.expect_err("65536 datagrams from one buffer");
		assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
		drop(worker);
	}

	#[test]
	fn a_resized_checkout_comes_back_pool_sized() {
		let Some(worker) = worker() else { return };
		let handle = worker.handle();
		let config = udp::Config {
			tx_buffers_max: 1,
			tx_buffer_len: 2048,
			..Default::default()
		};
		let sock = handle
			.udp(std::net::UdpSocket::bind("127.0.0.1:0").expect("bind"), config)
			.expect("socket");
		let acquire = || {
			let Poll::Ready(Ok(tx)) = sock.poll_acquire(&kio::Waiter::noop()) else {
				panic!("no tx buffer");
			};
			tx
		};

		let mut tx = acquire();
		assert_eq!(tx.capacity(), 2048);
		tx.shrink_to_fit();
		drop(tx);
		let mut tx = acquire();
		assert_eq!((tx.len(), tx.capacity()), (0, 2048));
		tx.resize(8192, 7);
		drop(tx);
		let tx = acquire();
		assert_eq!((tx.len(), tx.capacity()), (0, 2048));
		drop(tx);
		drop(worker);
	}

	#[test]
	fn buffer_grown_past_the_pool_is_rejected() {
		let Some(worker) = worker() else { return };
		let handle = worker.handle();
		let config = udp::Config {
			tx_buffer_len: 2048,
			..Default::default()
		};
		let sock = handle
			.udp(std::net::UdpSocket::bind("127.0.0.1:0").expect("bind"), config)
			.expect("socket");
		let to = sock.local_addr().expect("addr");
		let Poll::Ready(Ok(mut tx)) = sock.poll_acquire(&kio::Waiter::noop()) else {
			panic!("no tx buffer");
		};
		tx.resize(4096, 0);
		let err = tx
			.send(udp::Transmit::new(to, 1024))
			.expect_err("4096 bytes from a 2048 byte buffer");
		assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
		drop(worker);
	}

	#[test]
	fn oversized_gso_segment_is_rejected() {
		let Some(worker) = worker() else { return };
		let handle = worker.handle();
		let sock = handle
			.udp(
				std::net::UdpSocket::bind("127.0.0.1:0").expect("bind"),
				udp::Config::default(),
			)
			.expect("socket");
		let to = sock.local_addr().expect("addr");
		let Poll::Ready(Ok(mut tx)) = sock.poll_acquire(&kio::Waiter::noop()) else {
			panic!("no tx buffer");
		};
		tx.resize(60_000, 0);
		// `UDP_SEGMENT` is a u16: without validation this would truncate to a
		// one-byte stride instead of one segment.
		let err = tx
			.send(udp::Transmit::new(to, usize::from(u16::MAX) + 2))
			.expect_err("oversized segment");
		assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
		drop(worker);
	}

	/// A route or driver that cannot segment fails a GSO train with `EIO` or
	/// `EINVAL`. The train goes out again one datagram at a time, later sends
	/// skip GSO, and the socket stays usable. `SO_NO_CHECK` makes Linux refuse
	/// every `UDP_SEGMENT` send with `EINVAL` while plain sends still go out.
	#[test]
	fn rejected_gso_train_is_resent_unsegmented() {
		let Some(mut worker) = worker() else { return };
		let handle = worker.handle();
		let io = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind");
		let one: libc::c_int = 1;
		// SAFETY: a valid socket and a c_int option value.
		let rc = unsafe {
			libc::setsockopt(
				std::os::fd::AsRawFd::as_raw_fd(&io),
				libc::SOL_SOCKET,
				libc::SO_NO_CHECK,
				(&raw const one).cast(),
				std::mem::size_of_val(&one) as libc::socklen_t,
			)
		};
		assert_eq!(rc, 0, "SO_NO_CHECK: {}", std::io::Error::last_os_error());
		let sock = handle.udp(io, udp::Config::default()).expect("socket");
		let to = sock.local_addr().expect("addr");

		// Two trains of two: the first is rejected and resent, the second
		// never tries GSO.
		for _ in 0..2 {
			let deadline = handle.now() + Duration::from_secs(5);
			let mut received = 0;
			let Poll::Ready(Ok(mut tx)) = sock.poll_acquire(&kio::Waiter::noop()) else {
				panic!("the socket failed after a rejected train");
			};
			tx.resize(2 * 1200, 7);
			tx.send(udp::Transmit::new(to, 1200)).expect("send");
			while received < 2 * 1200 && handle.now() < deadline {
				let handle = handle.clone();
				worker
					.block_on(async move {
						Deadline::after(&handle, Duration::from_millis(10)).wait().await;
					})
					.unwrap();
				while let Poll::Ready(packet) = sock.poll_recv(&kio::Waiter::noop()) {
					received += packet.expect("receive path failed").payload().len();
				}
			}
			assert_eq!(received, 2 * 1200, "the train was dropped");
		}
		// One rejected train, then two single datagrams for each train.
		assert_eq!(handle.metrics().snapshot().tx_sends, 5);
	}

	/// The counters an ops scrape reads have to move for real work, and a
	/// handed-in [`Metrics`] has to be the same set the worker writes: reading
	/// zeros off a worker that is busy is indistinguishable from a healthy idle
	/// one, which is the failure this whole surface exists to prevent.
	#[test]
	fn metrics_record_ring_and_socket_activity() {
		let metrics = Metrics::default();
		let config = Config {
			metrics: metrics.clone(),
			..Default::default()
		};
		let Some(mut worker) = worker_with(config) else { return };
		let handle = worker.handle();
		let sock = handle
			.udp(
				std::net::UdpSocket::bind("127.0.0.1:0").expect("bind"),
				udp::Config::default(),
			)
			.expect("socket");
		let to = sock.local_addr().expect("addr");

		// One GSO train of four datagrams: one `sendmsg`, four packets.
		let Poll::Ready(Ok(mut tx)) = sock.poll_acquire(&kio::Waiter::noop()) else {
			panic!("no tx buffer");
		};
		tx.resize(4 * 1200, 7);
		tx.send(udp::Transmit::new(to, 1200)).expect("send");

		// Drive the worker until the loopback delivers, parking on a timer each
		// turn so the park and timer counters see traffic too.
		let deadline = handle.now() + Duration::from_secs(5);
		let mut received = 0;
		while received == 0 && handle.now() < deadline {
			let handle = handle.clone();
			worker
				.block_on(async move {
					Deadline::after(&handle, Duration::from_millis(10)).wait().await;
				})
				.unwrap();
			while let Poll::Ready(packet) = sock.poll_recv(&kio::Waiter::noop()) {
				let packet = packet.expect("receive path failed");
				received += packet.payload().len();
			}
		}
		assert!(received > 0, "the loopback never delivered the send");

		let snap = metrics.snapshot();
		assert_eq!(snap.tx_sends, 1, "one GSO train is one sendmsg: {snap:?}");
		assert_eq!(snap.tx_datagrams, 4, "four segments: {snap:?}");
		assert!(snap.rx_receives > 0, "no receive completions: {snap:?}");
		assert!(
			snap.rx_datagrams >= snap.rx_receives,
			"fewer datagrams than receives: {snap:?}"
		);
		assert!(snap.submissions > 0, "nothing was submitted: {snap:?}");
		assert!(snap.completions > 0, "nothing completed: {snap:?}");
		assert!(snap.enters > 0, "the ring was never entered: {snap:?}");
		assert!(snap.parks > 0, "the worker never parked: {snap:?}");
		assert!(snap.timers_fired > 0, "the park deadlines never fired: {snap:?}");
		// The worker's own handle reads the same counters as the one passed in,
		// rather than a private set the scraper would never see.
		let own = handle.metrics().snapshot();
		assert_eq!((own.tx_sends, own.tx_datagrams), (snap.tx_sends, snap.tx_datagrams));
	}

	/// The backpressure counters are the first thing to look at when throughput
	/// sags, so both ends of it have to be recorded: a send that found the pool
	/// drained, and a receive that could not be re-armed for want of a buffer.
	#[test]
	fn metrics_record_pool_backpressure() {
		let metrics = Metrics::default();
		let config = Config {
			metrics: metrics.clone(),
			..Default::default()
		};
		let Some(mut worker) = worker_with(config) else { return };
		let handle = worker.handle();
		// One buffer each way, so the pools are at their ceiling immediately.
		// Oneshot receives claim a whole buffer, which is what lets a held
		// packet leave the socket unarmed.
		let sock = handle
			.udp(
				std::net::UdpSocket::bind("127.0.0.1:0").expect("bind"),
				udp::Config {
					gro: false,
					gso: false,
					multishot: false,
					rx_buffers_max: 1,
					rx_buffer_len: 2048,
					tx_buffers_max: 1,
					tx_buffer_len: 2048,
				},
			)
			.expect("socket");
		let to = sock.local_addr().expect("addr");

		let Poll::Ready(Ok(mut tx)) = sock.poll_acquire(&kio::Waiter::noop()) else {
			panic!("no tx buffer");
		};
		tx.resize(1200, 0);
		// The pool is one buffer deep and that one is checked out.
		assert!(sock.poll_acquire(&kio::Waiter::noop()).is_pending());
		assert!(sock.poll_acquire(&kio::Waiter::noop()).is_pending());
		assert_eq!(metrics.snapshot().tx_stalls, 1);
		tx.send(udp::Transmit::new(to, 1200)).expect("send");

		// Hold the received packet: its buffer is the pool, so the re-arm has
		// nowhere to receive into.
		let deadline = handle.now() + Duration::from_secs(5);
		let mut held = None;
		while held.is_none() && handle.now() < deadline {
			let handle = handle.clone();
			worker
				.block_on(async move {
					Deadline::after(&handle, Duration::from_millis(10)).wait().await;
				})
				.unwrap();
			if let Poll::Ready(packet) = sock.poll_recv(&kio::Waiter::noop()) {
				held = Some(packet.expect("receive path failed"));
			}
		}
		assert!(held.is_some(), "the loopback never delivered the send");
		assert!(
			metrics.snapshot().rx_exhausted > 0,
			"a re-arm with every buffer held went unreported: {:?}",
			metrics.snapshot()
		);

		// A completed send ends the first stall. Draining the pool again starts
		// exactly one new episode, however often its waiter is polled.
		let Poll::Ready(Ok(_tx)) = sock.poll_acquire(&kio::Waiter::noop()) else {
			panic!("completed tx buffer was not released");
		};
		assert!(sock.poll_acquire(&kio::Waiter::noop()).is_pending());
		assert!(sock.poll_acquire(&kio::Waiter::noop()).is_pending());
		assert_eq!(metrics.snapshot().tx_stalls, 2);
	}

	/// Timer churn is the thing #3122 needs a baseline for, so an arm, a
	/// re-arm, and a drop each have to land in a different counter, and the
	/// derived heap depth has to come back to zero.
	#[test]
	fn metrics_count_timer_churn() {
		let metrics = Metrics::default();
		let config = Config {
			metrics: metrics.clone(),
			..Default::default()
		};
		let Some(worker) = worker_with(config) else { return };
		let handle = worker.handle();
		let mut timer = handle.timer();
		let now = handle.now();

		timer.set(Some(now + Duration::from_secs(60)));
		assert_eq!(metrics.snapshot().timers_active(), 1);

		// A re-arm is a cancel plus an arm, which is exactly the churn signal.
		timer.set(Some(now + Duration::from_secs(61)));
		let snap = metrics.snapshot();
		assert_eq!((snap.timers_armed, snap.timers_cancelled, snap.timers_fired), (2, 1, 0));
		assert_eq!(snap.timers_active(), 1);

		// An instant already reached fires rather than cancels.
		timer.set(Some(now));
		assert!(timer.poll(&kio::Waiter::noop()).is_ready());
		let snap = metrics.snapshot();
		assert_eq!((snap.timers_armed, snap.timers_cancelled, snap.timers_fired), (3, 2, 1));
		assert_eq!(snap.timers_active(), 0);

		// A dropped armed timer is a cancel, and the queue empties again.
		timer.set(Some(now + Duration::from_secs(60)));
		assert_eq!(metrics.snapshot().timers_active(), 1);
		drop(timer);
		assert_eq!(metrics.snapshot().timers_active(), 0);
	}

	/// A timer whose deadline the worker reached counts as fired even if no poll
	/// saw it: dropped or re-armed afterwards, it is not a cancel.
	#[test]
	fn metrics_count_an_unpolled_expiry_as_fired() {
		let metrics = Metrics::default();
		let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
		let clock = moq_clock::Manual::new().install(runtime.handle());
		let _entered = runtime.enter();
		let Some(mut worker) = worker_with(Config {
			metrics: metrics.clone(),
			..Default::default()
		}) else {
			return;
		};
		let handle = worker.handle();
		let mut rearmed = crate::Timer::after(&handle, Duration::from_secs(1));
		let dropped = crate::Timer::after(&handle, Duration::from_secs(1));
		clock.advance(Duration::from_secs(1));
		worker.block_on(async {}).unwrap();

		drop(dropped);
		rearmed.set(Some(handle.now() + Duration::from_secs(1)));
		let snap = metrics.snapshot();
		assert_eq!((snap.timers_armed, snap.timers_cancelled, snap.timers_fired), (3, 0, 2));
		assert_eq!(snap.timers_active(), 1);
	}

	/// A remote wake has to find the worker parked, then kick the futex. The park
	/// word is that observation: a sleep would pass or fail with the load.
	#[test]
	fn remote_wake_unparks() {
		let metrics = Metrics::default();
		let config = Config {
			metrics: metrics.clone(),
			..Default::default()
		};
		let Some(mut worker) = worker_with(config) else { return };
		let unpark = worker.shared.unpark.clone();
		let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
		let waker_slot = std::sync::Arc::new(std::sync::Mutex::new(None::<std::task::Waker>));

		let thread_flag = flag.clone();
		let thread_slot = waker_slot.clone();
		let thread_metrics = metrics.clone();
		let thread = std::thread::spawn(move || {
			loop {
				while unpark.word.load(Ordering::Acquire) != PARKED {
					std::hint::spin_loop();
				}
				// Park stores the waker first. Cloning leaves it in place so a
				// lost race can try again with whatever the next poll stored.
				let Some(waker) = thread_slot.lock().unwrap().clone() else {
					continue;
				};
				let wakes = thread_metrics.snapshot().wakes;
				waker.wake();
				// EINTR can end the park between the load and this wake. Only a
				// wake that found the word parked counts.
				if thread_metrics.snapshot().wakes == wakes {
					continue;
				}
				thread_flag.store(true, Ordering::Release);
				// The poll after the futex wake may have run before the flag and
				// parked again. Waking whatever it stored lets it observe the flag.
				if let Some(waker) = thread_slot.lock().unwrap().take() {
					waker.wake();
				}
				break;
			}
		});

		worker
			.block_on(std::future::poll_fn(move |cx| {
				if flag.load(Ordering::Acquire) {
					return Poll::Ready(());
				}
				*waker_slot.lock().unwrap() = Some(cx.waker().clone());
				Poll::Pending
			}))
			.unwrap();
		thread.join().unwrap();
		let snap = metrics.snapshot();
		assert!(snap.parks > 0, "the worker never parked: {snap:?}");
		// The futex syscall the wake had to make is the expensive half, and the
		// only counter written from off the worker's thread.
		assert!(snap.wakes > 0, "the remote wake went unreported: {snap:?}");
	}
}
