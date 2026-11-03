//! Drive a session on the caller's executor.

use std::task::Poll;

use crate::Error;
use crate::time::{Deadlines, Instant};

/// Drives a session with caller-supplied time.
///
/// Returned by [`crate::Client::connect`] and [`crate::Server::accept`]. Call
/// [`poll`](Self::poll) when external activity wakes the waiter or when the
/// returned deadline is reached, supplying nondecreasing instants; see
/// [`crate::time::Driver`] for the contract. Completion is cached, so
/// subsequent polls return the same error.
///
/// It holds no session handle: dropping the last [`crate::Session`] requests
/// closure on the next poll. Dropping the driver cancels the session, and
/// [`crate::Session::closed`] resolves with [`Error::Cancel`]. It is `Send` on
/// native, like the [`Boxable`](crate::transport::poll::Boxable) transport it drives.
/// A panic during polling is published as a transport failure before unwinding to the caller.
#[must_use = "the session makes no progress unless its driver is polled"]
pub struct Driver {
	state: State,
	clock: Deadlines,
}

// An executor may catch the panic and only drop the future after unwinding ended. Publish at
// the actual poll boundary, while the failure is still distinguishable from cancellation.
struct Polling<'a>(&'a mut State);

impl Drop for Polling<'_> {
	fn drop(&mut self) {
		if !std::thread::panicking() {
			return;
		}
		let error = Error::Transport("session driver panicked".into());
		self.0.result = Some(Err(error.clone()));
		if let Some(mut supervisor) = self.0.supervisor.take() {
			supervisor.panicked(error);
		}
	}
}

/// The protocol half of a machine, one variant per negotiated wire protocol.
pub(crate) enum Protocol {
	/// Boxed for size only.
	Lite(Box<crate::lite::Driver>),
	Ietf(crate::ietf::Driver),
}

/// The transport every session runs on, boxed once by `Client` or `Server`.
pub(crate) type Transport = crate::transport::boxed::Session;

/// Protocol and lifecycle work owned by the driver.
pub(crate) struct State {
	pub(crate) protocol: Protocol,
	// The session supervisor, polled alongside the protocol: it executes the
	// handles' close requests, publishes the transport's terminal error, and
	// samples stats. It finishes once the transport reports closed, and the
	// machine is not done until it has: the protocol's terminal transport close
	// is what `Session::closed` observes, so resolving before it is published
	// would leave waiters parked on a machine nobody polls again. `None` once
	// finished, since a completed machine must not be polled again.
	pub(crate) supervisor: Option<crate::session::Supervisor>,
	// Cached so a poll after completion doesn't re-poll a finished protocol.
	pub(crate) result: Option<Result<(), Error>>,
}

impl Driver {
	pub(crate) fn new(clock: Deadlines, state: State) -> Self {
		Self { state, clock }
	}

	/// Process ready work at `now`, registering for external activity.
	///
	/// Panics if `now` is earlier than the previous poll or construction time.
	pub fn poll(&mut self, now: Instant, waiter: &kio::Waiter) -> Result<Option<Instant>, Error> {
		let state = Polling(&mut self.state);
		waiter.batch(|| {
			self.clock.advance(now);
			match state.0.poll(waiter) {
				Poll::Ready(Ok(())) => Err(Error::Closed),
				Poll::Ready(Err(err)) => Err(err),
				Poll::Pending => Ok(self.clock.park(waiter)),
			}
		})
	}
}

impl Protocol {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		match self {
			Self::Lite(driver) => driver.poll(waiter),
			Self::Ietf(driver) => waiter.poll_future(std::pin::Pin::new(driver)),
		}
	}

	fn close(&self) {
		match self {
			Self::Lite(driver) => driver.close(),
			Self::Ietf(driver) => driver.withdrawal.begin(),
		}
	}

	/// Whether the protocol owes the peer no queued data, so a draining close can
	/// proceed.
	fn drained(&self) -> bool {
		match self {
			Self::Lite(driver) => driver.drained(),
			Self::Ietf(driver) => driver.drained(),
		}
	}
}

impl State {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		if let Some(supervisor) = &mut self.supervisor
			&& supervisor.poll(waiter).is_ready()
		{
			self.supervisor = None;
		}

		if self.supervisor.as_ref().is_some_and(|supervisor| supervisor.draining()) {
			self.protocol.close();
		}

		if self.result.is_none() {
			// The protocol's last act is closing the transport, and so is a drain
			// once the protocol that just ran owes the peer nothing. Either wakes
			// the supervisor's close watch; poll it now instead of waiting a turn.
			let closed = match self.protocol.poll(waiter) {
				Poll::Ready(result) => {
					self.result = Some(result);
					true
				}
				Poll::Pending => self
					.supervisor
					.as_mut()
					.is_some_and(|supervisor| supervisor.poll_drain(self.protocol.drained(), waiter)),
			};
			if closed
				&& let Some(supervisor) = &mut self.supervisor
				&& supervisor.poll(waiter).is_ready()
			{
				self.supervisor = None;
			}
		}

		match (&self.result, &self.supervisor) {
			(Some(result), None) => Poll::Ready(result.clone()),
			_ => Poll::Pending,
		}
	}
}

impl crate::time::Driver for Driver {
	fn poll(&mut self, now: Instant, waiter: &kio::Waiter) -> Result<Option<Instant>, Error> {
		self.poll(now, waiter)
	}
}

impl std::fmt::Debug for Driver {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Driver")
			.field("done", &self.state.result.is_some())
			.finish()
	}
}

// The relay and the bindings spawn drivers on multi-threaded runtimes.
const _: () = {
	const fn assert_send<T: Send>() {}
	assert_send::<Driver>();
};
