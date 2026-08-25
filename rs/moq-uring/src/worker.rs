//! The per-thread worker: ring ownership, the drive loop, and parking.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::task::{Context, Poll};
use std::time::Instant;

use io_uring::{EnterFlags, IoUring, opcode, types};

use crate::park::{FUTEX_BITSET_MATCH_ANY, FUTEX2_PRIVATE, FUTEX2_SIZE_U32, PARKED, RUNNING, Unpark};
use crate::shared::{Cqe, Op, Shared, Task};
use crate::{Error, timer, udp};

/// The largest submission queue io_uring will set up.
const MAX_ENTRIES: u32 = 32768;

/// Worker construction knobs.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// Submission queue depth, from 1 to 32768 (the kernel rounds it up to a
	/// power of two and sizes the completion queue at twice this).
	pub entries: u32,
}

impl Default for Config {
	fn default() -> Self {
		Self { entries: 256 }
	}
}

/// A thread-pinned io_uring executor: the ring, a timer heap, and a local
/// (`!Send`) task set, driven by a caller-owned loop.
///
/// Create one per thread, keep it on that thread (`!Send`), and drive it with
/// [`block_on`](Self::block_on). Everything else reaches the worker through
/// [`Handle`]: UDP sockets, timers, spawned tasks. Wakes from other threads
/// (any `Waker` this worker minted) are an atomic store plus, only while the
/// worker is parked, one futex syscall.
pub struct Worker {
	shared: Rc<Shared>,
	tasks: kio::Tasks<Task>,
	park: kio::Park,
	/// Whether the park-word `FUTEX_WAIT` SQE is in flight.
	futex_armed: bool,
}

impl Worker {
	/// Set up the ring, refusing kernels below Linux 6.12.
	///
	/// The floor buys incremental provided-buffer consumption, the absolute
	/// park timeout, and batched minimum waits with one code path; there is
	/// deliberately no fallback (use the tokio stack instead).
	pub fn new(config: Config) -> Result<Self, Error> {
		// Checked here so the `EINVAL` below can only mean the kernel refused
		// one of the setup flags, not that the caller asked for a bad depth.
		if config.entries == 0 || config.entries > MAX_ENTRIES {
			return Err(Error::Io(std::io::Error::new(
				std::io::ErrorKind::InvalidInput,
				format!("ring depth must be 1 to {MAX_ENTRIES}, got {}", config.entries),
			)));
		}

		let ring = IoUring::builder()
			.setup_single_issuer()
			.setup_defer_taskrun()
			.setup_coop_taskrun()
			.build(config.entries)
			.map_err(|err| match err.raw_os_error() {
				// EINVAL from setup means the kernel predates one of the
				// requested flags (the depth is already validated), so it never
				// reaches the feature check below.
				Some(libc::ENOSYS) | Some(libc::EPERM) | Some(libc::EACCES) | Some(libc::EINVAL) => {
					Error::Unsupported(format!(
						"io_uring is unavailable ({err}); kernel {} (Linux 6.12+ required, and container seccomp \
						 policies such as Docker's default commonly block io_uring)",
						kernel_release()
					))
				}
				_ => Error::Io(err),
			})?;

		// One feature bit gates the whole floor: MIN_TIMEOUT landed in 6.12
		// alongside everything else this worker assumes.
		if !ring.params().is_feature_min_timeout() {
			return Err(Error::Unsupported(format!(
				"kernel {} is too old: moq-uring requires Linux 6.12+ (io_uring MIN_TIMEOUT feature missing)",
				kernel_release()
			)));
		}

		Ok(Self {
			shared: Rc::new(Shared {
				ring: RefCell::new(ring),
				ops: RefCell::new(slab::Slab::new()),
				timers: Rc::new(RefCell::new(timer::Heap::default())),
				spawns: RefCell::new(Vec::new()),
				unpark: Unpark::new(),
				next_bgid: std::cell::Cell::new(0),
				stopped: std::cell::Cell::new(false),
			}),
			tasks: kio::Tasks::new(),
			park: kio::Park::default(),
			futex_armed: false,
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
	pub fn block_on<F: Future>(&mut self, future: F) -> Result<F::Output, Error> {
		let mut future = std::pin::pin!(future);
		let waker = self.shared.unpark.waker();
		loop {
			// Adopt tasks spawned since the last turn (spawning wakes us).
			let spawns = std::mem::take(&mut *self.shared.spawns.borrow_mut());
			for task in spawns {
				self.tasks.push(task);
			}

			let cx = Context::from_waker(&waker);
			let waiter = self.park.hold(&cx);
			if let Poll::Ready(value) = waiter.poll_future(future.as_mut()) {
				return Ok(value);
			}
			// `Ready` just means the set is drained; the waiter stays
			// registered for the next push.
			let _ = self.tasks.poll(waiter);

			self.shared.timers.borrow_mut().fire(Instant::now());
			self.pump()?;
			self.maybe_park()?;
		}
	}

	/// Submit staged SQEs and dispatch every pending completion.
	fn pump(&mut self) -> Result<(), Error> {
		self.submit()?;
		loop {
			// Copy the completions out so dispatch can borrow the ring (to
			// re-arm receives, push cancels, and so on).
			let cqes: Vec<Cqe> = {
				let mut ring = self.shared.ring.borrow_mut();
				ring.completion()
					.map(|entry| Cqe {
						user_data: entry.user_data(),
						result: entry.result(),
						flags: entry.flags(),
					})
					.collect()
			};
			if cqes.is_empty() {
				return Ok(());
			}
			for cqe in cqes {
				self.dispatch(cqe);
			}
		}
	}

	fn submit(&mut self) -> Result<(), Error> {
		let mut ring = self.shared.ring.borrow_mut();
		if ring.submission().is_empty() {
			return Ok(());
		}
		match ring.submit() {
			Ok(_) => Ok(()),
			// The completion queue overflowed; the caller reaps and retries.
			Err(err) if err.raw_os_error() == Some(libc::EBUSY) => Ok(()),
			Err(err) => Err(err.into()),
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

		let deadline = self.shared.timers.borrow().next();
		let result = {
			let mut ring = self.shared.ring.borrow_mut();
			let to_submit = ring.submission().len() as u32;
			let submitter = ring.submitter();
			match deadline {
				None => submitter.submit_and_wait(1).map(drop),
				Some(at) => {
					// Zero timeout SQEs: the earliest userspace deadline rides
					// the enter call as an absolute CLOCK_MONOTONIC timeout.
					let ts = abs_timespec(at);
					let args = types::SubmitArgs::new().timespec(&ts);
					let flags = EnterFlags::GETEVENTS | EnterFlags::EXT_ARG | EnterFlags::ABS_TIMER;
					// SAFETY: `args` (and the timespec it references) outlive
					// the call, and EXT_ARG matches its type.
					unsafe { submitter.enter(to_submit, 1, flags.bits(), Some(&args)) }.map(drop)
				}
			}
		};
		unpark.word.store(RUNNING, Ordering::Release);

		match result {
			Ok(()) => Ok(()),
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

impl Drop for Worker {
	fn drop(&mut self) {
		// Handles may outlive us; everything they try from here on fails
		// instead of pending on a loop that will never run again.
		self.shared.stopped.set(true);
		// The kernel may still write into provided buffers and read send
		// headers owned by the ops slab. Cancel everything and wait for the
		// terminal completions before any of that memory frees.
		{
			let ring = self.shared.ring.borrow_mut();
			let timeout = types::Timespec::new().sec(1);
			let _ = ring
				.submitter()
				.register_sync_cancel(Some(timeout), types::CancelBuilder::any());
		}
		for _ in 0..64 {
			if self.pump().is_err() {
				break;
			}
			if self.shared.ops.borrow().is_empty() {
				return;
			}
			let ring = self.shared.ring.borrow_mut();
			let ts = types::Timespec::new().nsec(50_000_000);
			let args = types::SubmitArgs::new().timespec(&ts);
			let _ = ring.submitter().submit_with_args(1, &args);
		}
		if !self.shared.ops.borrow().is_empty() {
			// Leak the operations (and what they own) rather than free memory
			// the kernel may still touch.
			tracing::error!("dropping an io_uring worker with operations stuck in flight; leaking them");
			std::mem::forget(std::mem::replace(&mut *self.shared.ops.borrow_mut(), slab::Slab::new()));
		}
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
/// [`spawn`](Self::spawn), [`udp`](Self::udp), and the [`moq_net::Timers`]
/// impl for deadlines. `!Send`, like everything the worker owns.
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
	pub fn udp(&self, socket: std::net::UdpSocket, config: udp::Config) -> Result<udp::Socket, Error> {
		if self.shared.stopped.get() {
			return Err(Shared::gone_error().into());
		}
		udp::Socket::bind(&self.shared, socket, config)
	}
}

impl std::fmt::Debug for Handle {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Handle").finish()
	}
}

impl moq_net::Timers for Handle {
	type Timer = crate::Timer;

	fn timer(&self) -> Self::Timer {
		crate::Timer::new(self.shared.timers.clone())
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

/// Convert a deadline into an absolute `CLOCK_MONOTONIC` timespec (what
/// `IORING_ENTER_ABS_TIMER` expects).
fn abs_timespec(at: Instant) -> types::Timespec {
	// `std::time::Instant` is CLOCK_MONOTONIC on Linux but its origin is
	// opaque, so anchor the difference on a raw clock read.
	let delta = at.saturating_duration_since(Instant::now());
	let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
	// SAFETY: valid out-pointer.
	unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
	let nanos = now.tv_nsec as u64 + delta.subsec_nanos() as u64;
	let secs = (now.tv_sec as u64)
		.saturating_add(delta.as_secs())
		.saturating_add(nanos / 1_000_000_000);
	types::Timespec::new().sec(secs).nsec((nanos % 1_000_000_000) as u32)
}

#[cfg(test)]
mod tests {
	use super::*;
	use moq_net::Timers;
	use moq_net::runtime::Deadline;
	use std::time::Duration;

	/// Kernel-gated: `None` (with a loud skip) below the 6.12 floor, so these
	/// tests pass vacuously on older CI kernels and run everywhere else.
	fn worker() -> Option<Worker> {
		match Worker::new(Config::default()) {
			Ok(worker) => Some(worker),
			Err(Error::Unsupported(reason)) => {
				eprintln!("skipping io_uring test: {reason}");
				None
			}
			Err(err) => panic!("worker setup failed: {err}"),
		}
	}

	#[test]
	fn ready_future() {
		let Some(mut worker) = worker() else { return };
		let value = worker.block_on(async { 7 }).unwrap();
		assert_eq!(value, 7);
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

	#[test]
	fn deadline_fires_at_park() {
		let Some(mut worker) = worker() else { return };
		let handle = worker.handle();
		let start = Instant::now();
		// Nothing else wakes this worker: the park's absolute timeout is the
		// only thing that can fire the deadline.
		worker
			.block_on(async move {
				Deadline::after(&handle, Duration::from_millis(50)).wait().await;
			})
			.unwrap();
		let elapsed = start.elapsed();
		assert!(elapsed >= Duration::from_millis(50), "woke early: {elapsed:?}");
		assert!(elapsed < Duration::from_secs(5), "woke far too late: {elapsed:?}");
	}

	#[test]
	fn timer_rearm_and_disarm() {
		use moq_net::runtime::Timer as _;
		let Some(mut worker) = worker() else { return };
		let handle = worker.handle();
		let mut timer = handle.timer();

		// Disarmed timers never fire.
		assert!(timer.poll(&kio::Waiter::noop()).is_pending());

		// An instant already in the past is immediately elapsed, and stays
		// elapsed (fused) until re-armed.
		timer.set(Some(Instant::now() - Duration::from_millis(1)));
		assert!(timer.poll(&kio::Waiter::noop()).is_ready());
		assert!(timer.poll(&kio::Waiter::noop()).is_ready());

		// Re-arming to the future pends again; disarming stays pending.
		timer.set(Some(Instant::now() + Duration::from_secs(60)));
		assert!(timer.poll(&kio::Waiter::noop()).is_pending());
		timer.set(None);
		assert!(timer.poll(&kio::Waiter::noop()).is_pending());

		// And a short re-arm actually fires through the worker.
		let start = Instant::now();
		worker
			.block_on(async move {
				timer.set(Some(Instant::now() + Duration::from_millis(20)));
				kio::wait(|waiter| timer.poll(waiter)).await;
			})
			.unwrap();
		assert!(start.elapsed() >= Duration::from_millis(20));
	}

	#[test]
	fn dropped_worker_rejects_operations() {
		let Some(worker) = worker() else { return };
		let handle = worker.handle();
		let bind = || std::net::UdpSocket::bind("127.0.0.1:0").expect("bind");
		let sock = handle.udp(bind(), udp::Config::default()).expect("socket");
		let to = sock.local_addr().expect("addr");
		let Poll::Ready(Ok(tx)) = sock.poll_acquire(&kio::Waiter::noop()) else {
			panic!("no tx buffer");
		};
		drop(worker);

		// Every path a retained handle can reach fails instead of pending on
		// a loop that will never run again.
		assert!(handle.udp(bind(), udp::Config::default()).is_err());
		assert!(matches!(sock.poll_recv(&kio::Waiter::noop()), Poll::Ready(Err(_))));
		assert!(matches!(sock.poll_acquire(&kio::Waiter::noop()), Poll::Ready(Err(_))));
		assert!(tx.send(1200, to, 1200).is_err());
		// And a late spawn is dropped rather than parked forever.
		handle.spawn(async {});
	}

	#[test]
	fn invalid_ring_depth_is_not_unsupported() {
		// A caller's bad depth must not read as "this kernel cannot run the
		// worker", which is the signal to fall back to the tokio stack.
		for entries in [0, MAX_ENTRIES + 1] {
			let err = Worker::new(Config { entries }).expect_err("invalid depth");
			assert!(matches!(err, Error::Io(_)), "classified as {err:?}");
		}
	}

	#[test]
	fn oversized_receive_pool_is_rejected() {
		let Some(worker) = worker() else { return };
		let handle = worker.handle();
		// Without validation the power-of-two rounding wraps to a zero-entry
		// ring, which allocates nothing and underflows its mask.
		let config = udp::Config {
			rx_buffers: u16::MAX,
			..Default::default()
		};
		let err = handle
			.udp(std::net::UdpSocket::bind("127.0.0.1:0").expect("bind"), config)
			.expect_err("oversized pool");
		assert!(matches!(err, Error::Io(err) if err.kind() == std::io::ErrorKind::InvalidInput));
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
		let Poll::Ready(Ok(tx)) = sock.poll_acquire(&kio::Waiter::noop()) else {
			panic!("no tx buffer");
		};
		tx.send(64 * 1024, to, 1000).expect("send 66 datagrams");
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
		let Poll::Ready(Ok(tx)) = sock.poll_acquire(&kio::Waiter::noop()) else {
			panic!("no tx buffer");
		};
		let err = tx.send(64 * 1024, to, 1).expect_err("65536 datagrams from one buffer");
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
		let Poll::Ready(Ok(tx)) = sock.poll_acquire(&kio::Waiter::noop()) else {
			panic!("no tx buffer");
		};
		// `UDP_SEGMENT` is a u16: without validation this would truncate to a
		// one-byte stride instead of one segment.
		let err = tx
			.send(60_000, to, usize::from(u16::MAX) + 2)
			.expect_err("oversized segment");
		assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
		drop(worker);
	}

	#[test]
	fn remote_wake_unparks() {
		let Some(mut worker) = worker() else { return };
		let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

		let thread_flag = flag.clone();
		let waker_slot = std::sync::Arc::new(std::sync::Mutex::new(None::<std::task::Waker>));
		let thread_slot = waker_slot.clone();
		let thread = std::thread::spawn(move || {
			// Wait until the worker has parked on the future below.
			std::thread::sleep(Duration::from_millis(50));
			thread_flag.store(true, Ordering::Release);
			if let Some(waker) = thread_slot.lock().unwrap().take() {
				waker.wake();
			}
		});

		let start = Instant::now();
		worker
			.block_on(std::future::poll_fn(move |cx| {
				if flag.load(Ordering::Acquire) {
					return Poll::Ready(());
				}
				*waker_slot.lock().unwrap() = Some(cx.waker().clone());
				Poll::Pending
			}))
			.unwrap();
		assert!(start.elapsed() >= Duration::from_millis(50));
		thread.join().unwrap();
	}
}
