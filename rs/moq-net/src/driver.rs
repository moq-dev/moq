//! Drive a session on the caller's executor.

use std::task::Poll;

use crate::Error;
use crate::time::{Clock, Instant};

/// Drives a session with caller-supplied time.
///
/// Returned by [`crate::Client::connect`] and [`crate::Server::accept`]. Call
/// [`poll`](Self::poll) when external activity wakes the waiter or when
/// [`timeout`](Self::timeout) is reached. Supply nondecreasing instants.
/// Completion is cached, so subsequent polls return the same result.
///
/// It holds no session handle: dropping the last [`crate::Session`] requests
/// closure on the next poll. Dropping the driver cancels the session, and
/// [`crate::Session::closed`] resolves with [`Error::Cancel`]. Its `Send`-ness
/// follows its transport.
#[must_use = "the session makes no progress unless its driver is polled"]
pub struct Driver<S: crate::transport::poll::Session> {
	state: State<S>,
	clock: Clock,
}

/// The protocol half of a machine, one variant per negotiated wire protocol.
///
/// The lite driver is a named machine, so the machine's `Send`-ness follows
/// the transport (a pinned `!Send` transport yields a `!Send` machine that
/// stays on its thread). The ietf driver is still a boxed future; the box
/// demands `Send` on native, which is why the ietf path requires a
/// [`Boxable`](crate::transport::poll::Boxable) transport until it too becomes
/// a named machine.
pub(crate) enum Protocol<S: crate::transport::poll::Session> {
	/// Boxed for size only: a concrete box, so `Send` stays inferred.
	Lite(Box<crate::lite::Driver<S>>),
	Ietf(crate::util::MaybeSendBox<'static, Result<(), Error>>),
}

/// Protocol and lifecycle work owned by the driver.
pub(crate) struct State<S: crate::transport::poll::Session> {
	pub(crate) protocol: Protocol<S>,
	// The session supervisor, polled alongside the protocol: it executes the
	// handles' close requests, publishes the transport's terminal error, and
	// samples stats. It finishes once the transport reports closed, and the
	// machine is not done until it has: the protocol's terminal transport close
	// is what `Session::closed` observes, so resolving before it is published
	// would leave waiters parked on a machine nobody polls again. `None` once
	// finished, since a completed machine must not be polled again.
	pub(crate) supervisor: Option<crate::session::Supervisor<S>>,
	// Cached so a poll after completion doesn't re-poll a finished protocol.
	pub(crate) result: Option<Result<(), Error>>,
}

impl<S: crate::transport::poll::Session> Driver<S> {
	pub(crate) fn new(clock: Clock, state: State<S>) -> Self {
		Self { state, clock }
	}

	/// Process ready work at `now`, registering for external activity.
	///
	/// Panics if `now` is earlier than the previous poll or construction time.
	pub fn poll(&mut self, now: Instant, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		self.clock.advance(now);
		self.state.poll(waiter)
	}
}

impl<S: crate::transport::poll::Session> Protocol<S> {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		match self {
			Self::Lite(driver) => driver.poll(waiter),
			Self::Ietf(driver) => waiter.poll_future(driver.as_mut()),
		}
	}
}

impl<S: crate::transport::poll::Session> State<S> {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		if let Some(supervisor) = &mut self.supervisor
			&& supervisor.poll(waiter).is_ready()
		{
			self.supervisor = None;
		}

		if self.result.is_none()
			&& let Poll::Ready(result) = self.protocol.poll(waiter)
		{
			self.result = Some(result);
			// The protocol's last act was closing the transport, which wakes the
			// supervisor's close watch; poll it now instead of waiting a turn.
			if let Some(supervisor) = &mut self.supervisor
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

impl<S: crate::transport::poll::Session> crate::time::Driver for Driver<S> {
	type Output = Result<(), Error>;
	fn poll(&mut self, now: Instant, waiter: &kio::Waiter) -> Poll<Self::Output> {
		self.poll(now, waiter)
	}
	fn timeout(&self) -> Option<Instant> {
		self.timeout()
	}
}

impl<S: crate::transport::poll::Session> Driver<S> {
	/// The next instant the caller must poll at, or none while no timer is armed.
	pub fn timeout(&self) -> Option<Instant> {
		self.clock.timeout()
	}
}

impl<S: crate::transport::poll::Session> std::fmt::Debug for Driver<S> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Driver")
			.field("done", &self.state.result.is_some())
			.finish()
	}
}
