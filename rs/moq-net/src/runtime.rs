//! Executor integration: who runs session machines and arms their timers.
//!
//! This crate never spawns onto a global executor and never reaches for ambient
//! state (no thread-locals, no globals). Instead, [`crate::Client::connect`] and
//! [`crate::Server::accept`] take a [`Runtime`], which supplies the two things a
//! session cannot do alone: run its protocol [`Machine`] to completion, and wake
//! a [`Timer`] when a deadline passes.
//!
//! Each transport ships its runtime: `moq-tokio` spawns machines onto tokio,
//! `moq-wasm` onto the browser microtask queue, and a thread-per-core runtime
//! keeps them on its own thread. `Test` (behind the `test-runtime` feature) is
//! the deterministic one for tests: nothing runs and no time passes unless the
//! test says so.
//!
//! There is deliberately no `Send` bound anywhere here. A runtime whose
//! [`Runtime::Transport`] is `Send` gets a `Send` [`Machine`] and may move it
//! across threads (tokio does); a pinned `!Send` transport stays on its thread.

use std::task::Poll;

use crate::Error;

/// The instant type used for deadlines and annotations.
///
/// [`std::time::Instant`] on native. The browser has no monotonic std clock, so
/// wasm substitutes an equivalent backed by `performance.now()`.
#[cfg(not(target_family = "wasm"))]
pub type Instant = std::time::Instant;
/// The instant type used for deadlines and annotations (wasm shim).
#[cfg(target_family = "wasm")]
pub type Instant = web_async::time::Instant;

/// A single re-armable timer registration, produced by [`Runtime::timer`].
///
/// This is the primitive [`Deadline`] wraps; implement it, use `Deadline`.
/// Arming is synchronous and in-memory: no I/O submission, no async
/// cancellation. Implementations typically keep a slot in the runtime's timer
/// wheel (or wrap a tokio `Sleep`).
pub trait Timer {
	/// Arm, re-arm, or disarm (`None`) the timer.
	///
	/// Re-arming an elapsed timer for a later instant makes it pend again;
	/// re-arming for an instant already in the past leaves it elapsed.
	fn set(&mut self, at: Option<Instant>);

	/// Ready once the armed instant has passed, registering `waiter` otherwise.
	///
	/// Fused: an elapsed timer keeps reporting `Ready` until re-armed. A
	/// disarmed timer never fires.
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()>;
}

/// The executor a session runs on.
///
/// Passed to [`crate::Client::connect`] / [`crate::Server::accept`], which hand
/// their protocol [`Machine`] to [`spawn`](Self::spawn). Cloned into the session
/// internals that arm timers, so clones must be cheap (a ZST or a reference
/// count).
pub trait Runtime: Clone {
	/// The transport this runtime can drive. Pinning the transport per runtime
	/// is what lets each implementation know the concrete `Machine` type it
	/// spawns, including whether it is `Send`.
	type Transport: crate::transport::poll::Session;

	/// The timer registration this runtime hands out.
	type Timer: Timer;

	/// A new, disarmed timer.
	fn timer(&self) -> Self::Timer;

	/// The current instant.
	///
	/// Defaults to the real clock, which every production runtime should keep.
	/// It exists so `Test` can substitute a virtual clock: code that arms
	/// relative deadlines (`now + interval`) or compares against armed instants
	/// must read *this* clock, or a test that advances virtual time leaves those
	/// instants in the past. Pure annotations (activity stamps, logs) may read
	/// [`Instant::now`] directly.
	fn now(&self) -> Instant {
		Instant::now()
	}

	/// Take ownership of a session's protocol machine and poll it to completion.
	///
	/// The machine holds no [`crate::Session`] clone, so running it never keeps
	/// the session alive; once the last session handle drops, the transport
	/// closes and the machine finishes on its own. Dropping the machine instead
	/// cancels the protocol work without closing the session.
	fn spawn(&self, machine: Machine<Self>);
}

/// A wall-clock deadline: the ergonomic layer over [`Timer`].
///
/// Mirrors the old `kio::time::Deadline` surface: arm it with an [`Instant`],
/// poll it from a `poll_*` function, re-arm or disarm as the deadline moves.
/// Re-setting the instant it already holds does nothing, so a poll loop can
/// recompute its deadline every turn without restarting the countdown.
pub struct Deadline<R: Runtime> {
	at: Option<Instant>,
	timer: R::Timer,
}

impl<R: Runtime> Deadline<R> {
	/// A disarmed deadline, which never fires until [`set`](Self::set) arms it.
	pub fn new(runtime: &R) -> Self {
		Self {
			at: None,
			timer: runtime.timer(),
		}
	}

	/// A deadline armed for `at`.
	pub fn at(runtime: &R, at: Instant) -> Self {
		let mut deadline = Self::new(runtime);
		deadline.set(Some(at));
		deadline
	}

	/// A deadline armed for `duration` past the runtime's [`now`](Runtime::now).
	///
	/// A duration the clock cannot represent (e.g. [`std::time::Duration::MAX`])
	/// leaves the deadline disarmed, so it never fires rather than panicking on
	/// the overflow.
	pub fn after(runtime: &R, duration: std::time::Duration) -> Self {
		let mut deadline = Self::new(runtime);
		deadline.set(runtime.now().checked_add(duration));
		deadline
	}

	/// Arm, re-arm, or disarm (`None`) the deadline.
	pub fn set(&mut self, at: Option<Instant>) {
		if self.at == at {
			return;
		}
		self.at = at;
		self.timer.set(at);
	}

	/// The instant this fires at, or `None` while disarmed.
	pub fn deadline(&self) -> Option<Instant> {
		self.at
	}

	/// Poll the deadline, registering `waiter` so the poll re-fires once it
	/// elapses. `Ready` once the instant has passed, `Pending` before then and
	/// while disarmed.
	pub fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		if self.at.is_none() {
			return Poll::Pending;
		}
		self.timer.poll(waiter)
	}

	/// Wait for the deadline to elapse. Parks forever while disarmed.
	pub async fn wait(&mut self) {
		kio::wait(|waiter| self.poll(waiter)).await
	}
}

impl<R: Runtime> std::fmt::Debug for Deadline<R> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Deadline").field("at", &self.at).finish()
	}
}

/// The future driving a [`crate::Session`]'s protocol state.
///
/// Created by [`crate::Client::connect`] / [`crate::Server::accept`] and handed
/// straight to [`Runtime::spawn`]; the only thing to do with one is poll it,
/// either as a [`Future`] or by stepping [`poll`](Self::poll) from a
/// [`kio`]-style poll function. It resolves when the session ends, and keeps
/// returning that same result if polled again.
pub struct Machine<R: Runtime> {
	state: MachineState,
	// Retains the waiter across `Future` polls so its kio registrations stay
	// live. Kept out of `MachineState` so the borrow `hold` hands back doesn't
	// collide with the `&mut` that polling the state needs.
	park: kio::Park,
	// Ties the machine to the runtime that spawns it without affecting
	// auto-traits: those follow the actual state (type-erased boxes today).
	_runtime: std::marker::PhantomData<fn(R) -> R>,
}

/// Everything the machine polls, split from the park so the two borrow
/// disjointly.
pub(crate) struct MachineState {
	pub(crate) protocol: crate::util::MaybeSendBox<'static, Result<(), Error>>,
	// Bandwidth sampling, polled alongside the protocol. Its completion never
	// ends the machine: the protocol owns the teardown. `None` once finished
	// (or when the transport reports no send-rate estimate), since a completed
	// future must not be polled again.
	pub(crate) maintenance: Option<crate::util::MaybeSendBox<'static, ()>>,
	// Cached so a poll after completion doesn't re-poll a finished future.
	pub(crate) result: Option<Result<(), Error>>,
}

impl<R: Runtime> Machine<R> {
	pub(crate) fn new(state: MachineState) -> Self {
		Self {
			state,
			park: kio::Park::default(),
			_runtime: std::marker::PhantomData,
		}
	}

	/// Drive the protocol one step, registering `waiter` for the next wakeup.
	///
	/// The `poll_*` counterpart of `.await`ing the machine, for runtimes
	/// composing it into their own [`kio`]-style poll loops.
	pub fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		self.state.poll(waiter)
	}
}

impl MachineState {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
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
		// The session is over; release the maintenance future now rather than
		// on Drop, since it holds a transport clone.
		self.maintenance = None;
		Poll::Ready(result)
	}
}

impl<R: Runtime> Future for Machine<R> {
	type Output = Result<(), Error>;

	fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
		let this = &mut *self;
		// Disjoint field borrows: `hold` borrows the park for as long as the
		// waiter lives, while the state is polled through its own `&mut`.
		let waiter = this.park.hold(cx);
		this.state.poll(waiter)
	}
}

impl<R: Runtime> std::fmt::Debug for Machine<R> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Machine")
			.field("done", &self.state.result.is_some())
			.finish()
	}
}

#[cfg(feature = "test-runtime")]
mod test;
#[cfg(feature = "test-runtime")]
pub use test::{Never, Test};

/// A tokio-backed runtime for this crate's own unit tests, so the existing
/// `tokio::time::pause`/`advance` tests keep their semantics: `now` reads
/// tokio's (pausable) clock and timers are tokio sleeps, which paused tests
/// auto-advance. Production code uses `moq_tokio::Runtime` instead; this one is
/// compiled only into the test harness (integration tests carry their own copy
/// in `tests/support`).
#[cfg(all(test, not(target_family = "wasm")))]
pub(crate) mod tokio_test {
	use std::{marker::PhantomData, pin::Pin, task::Poll};

	use super::{Instant, Machine, Runtime, Timer};

	pub(crate) struct Tokio<S>(PhantomData<fn(S)>);

	impl<S> Tokio<S> {
		pub fn new() -> Self {
			Self(PhantomData)
		}
	}

	impl<S> Clone for Tokio<S> {
		fn clone(&self) -> Self {
			Self(PhantomData)
		}
	}

	impl<S: crate::transport::poll::Session> Runtime for Tokio<S> {
		type Transport = S;
		type Timer = TokioTimer;

		fn timer(&self) -> Self::Timer {
			TokioTimer { at: None, sleep: None }
		}

		fn now(&self) -> Instant {
			tokio::time::Instant::now().into_std()
		}

		fn spawn(&self, machine: Machine<Self>) {
			tokio::spawn(machine);
		}
	}

	pub(crate) struct TokioTimer {
		at: Option<Instant>,
		// Allocated on the first poll after arming, then re-armed in place via
		// `Sleep::reset`. Construction is deferred because it panics without a
		// live tokio time driver, and only the poll is guaranteed to run inside
		// the runtime.
		sleep: Option<Pin<Box<tokio::time::Sleep>>>,
	}

	impl Timer for TokioTimer {
		fn set(&mut self, at: Option<Instant>) {
			self.at = at;
			// Reuse the allocation when there is one; `reset` also clears
			// `is_elapsed`.
			if let (Some(at), Some(sleep)) = (at, &mut self.sleep) {
				sleep.as_mut().reset(tokio::time::Instant::from_std(at));
			}
		}

		fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
			let Some(at) = self.at else { return Poll::Pending };
			let sleep = self
				.sleep
				.get_or_insert_with(|| Box::pin(tokio::time::sleep_until(tokio::time::Instant::from_std(at))));
			if sleep.is_elapsed() {
				return Poll::Ready(());
			}
			waiter.poll_future(sleep.as_mut())
		}
	}
}
