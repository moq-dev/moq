//! The tokio runtime handle passed to moq-net's session entry points.

use std::{marker::PhantomData, pin::Pin, task::Poll};

/// The [`moq_net::Runtime`] for tokio: protocol machines are `tokio::spawn`ed
/// and timers are tokio sleeps.
///
/// One runtime drives one transport type (`S`), so each dial path constructs
/// its own with [`Runtime::new`]; it is a zero-sized handle. The default `S`
/// makes a bare `Runtime::new()` a transportless [`moq_net::Timers`] handle,
/// for work that only arms timers (the origin driver).
pub struct Runtime<S = ()>(PhantomData<fn(S)>);

impl<S> Runtime<S> {
	/// A handle for the transport type this dial produces.
	pub fn new() -> Self {
		Self(PhantomData)
	}
}

impl<S> Default for Runtime<S> {
	fn default() -> Self {
		Self::new()
	}
}

impl<S> Clone for Runtime<S> {
	fn clone(&self) -> Self {
		*self
	}
}

impl<S> Copy for Runtime<S> {}

impl<S> std::fmt::Debug for Runtime<S> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Runtime").finish()
	}
}

// Unbounded: timers don't involve the transport, so any `Runtime<S>` (including
// the transportless default) hands them out.
impl<S> moq_net::Timers for Runtime<S> {
	type Timer = Timer;

	fn timer(&self) -> Self::Timer {
		Timer { at: None, sleep: None }
	}

	fn now(&self) -> moq_net::runtime::Instant {
		// Tokio's clock, so `tokio::time::pause` tests stay coherent; it reads
		// the real clock outside a paused runtime.
		tokio::time::Instant::now().into_std()
	}
}

// Boxable: tokio work-steals, so the machine must be `Send`.
impl<S: moq_net::transport::poll::Boxable> moq_net::Runtime for Runtime<S> {
	type Transport = S;

	fn spawn(&self, machine: moq_net::runtime::Machine<Self>) {
		// The session surfaces the result through `closed()`; the task's own
		// result is nothing to keep.
		use tracing::Instrument;
		tokio::spawn(machine.instrument(tracing::Span::current()));
	}
}

/// A tokio-timer runtime that hands the machine back instead of spawning it.
///
/// For callers that drive the session inline, in their own task, so its
/// lifetime and teardown stay tied to theirs (the relay's WebSocket handler
/// does this). Accept or connect with a clone, then [`take`](Self::take) the
/// machine and poll it yourself.
///
/// One handle serves exactly one session. Clones share its slot, which is how
/// the machine gets back from the clone `accept`/`connect` took, so a second
/// session through the same handle panics rather than silently taking the
/// first one's place: whichever way that resolved, some session would end up
/// with the wrong driver or none at all, and a session nobody drives makes no
/// progress and never processes its shutdown.
pub struct Inline<S: moq_net::transport::poll::Session> {
	slot: std::sync::Arc<std::sync::Mutex<Option<moq_net::runtime::Machine<Self>>>>,
	_transport: PhantomData<fn(S)>,
}

impl<S: moq_net::transport::poll::Session> Inline<S> {
	/// An empty handle; the next accept/connect through it fills the slot.
	pub fn new() -> Self {
		Self {
			slot: Default::default(),
			_transport: PhantomData,
		}
	}

	/// The machine this handle's accept/connect handed over, if any.
	///
	/// The session makes no progress until the caller polls it.
	pub fn take(&self) -> Option<moq_net::runtime::Machine<Self>> {
		self.slot.lock().unwrap().take()
	}
}

impl<S: moq_net::transport::poll::Session> Default for Inline<S> {
	fn default() -> Self {
		Self::new()
	}
}

impl<S: moq_net::transport::poll::Session> Clone for Inline<S> {
	fn clone(&self) -> Self {
		Self {
			slot: self.slot.clone(),
			_transport: PhantomData,
		}
	}
}

impl<S: moq_net::transport::poll::Session> std::fmt::Debug for Inline<S> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Inline").finish()
	}
}

impl<S: moq_net::transport::poll::Session> moq_net::Timers for Inline<S> {
	type Timer = Timer;

	fn timer(&self) -> Self::Timer {
		Timer { at: None, sleep: None }
	}

	fn now(&self) -> moq_net::runtime::Instant {
		tokio::time::Instant::now().into_std()
	}
}

impl<S: moq_net::transport::poll::Session> moq_net::Runtime for Inline<S> {
	type Transport = S;

	fn spawn(&self, machine: moq_net::runtime::Machine<Self>) {
		let mut slot = self.slot.lock().unwrap();
		if slot.is_some() {
			// Unlock first: panicking under the guard would poison the slot and
			// stop the caller who used this handle correctly from ever taking
			// their machine, stranding the session this is meant to protect.
			drop(slot);
			panic!(
				"an Inline runtime drives one session: take() the machine from the previous accept/connect, or use a fresh handle"
			);
		}
		*slot = Some(machine);
	}
}

/// A tokio sleep driven through the [`moq_net::runtime::Timer`] contract.
pub struct Timer {
	at: Option<moq_net::runtime::Instant>,
	// Allocated on the first poll after arming, then re-armed in place via
	// `Sleep::reset`. Construction is deferred because it panics without a live
	// tokio time driver, and only the poll is guaranteed to run inside the
	// runtime.
	sleep: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl moq_net::runtime::Timer for Timer {
	fn set(&mut self, at: Option<moq_net::runtime::Instant>) {
		self.at = at;
		// Reuse the allocation when there is one; `reset` also clears
		// `is_elapsed`.
		if let (Some(at), Some(sleep)) = (at, &mut self.sleep) {
			sleep.as_mut().reset(tokio::time::Instant::from_std(at));
		}
	}

	fn poll(&mut self, waiter: &moq_net::kio::Waiter) -> Poll<()> {
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
