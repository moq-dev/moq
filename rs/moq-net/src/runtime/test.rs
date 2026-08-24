//! The deterministic test runtime: virtual time, manual machine stepping.

use std::{
	collections::HashMap,
	marker::PhantomData,
	sync::{Arc, Mutex},
	task::Poll,
};

use super::{Instant, Machine, Runtime, Timer, Timers};

/// A deterministic [`Runtime`] for tests: no thread runs machines and no time
/// passes unless the test says so.
///
/// - [`tick`](Self::tick) polls every spawned machine once.
/// - [`advance`](Self::advance) moves the virtual clock, firing elapsed timers.
/// - [`advance_to_timer`](Self::advance_to_timer) jumps straight to the
///   earliest armed timer.
///
/// [`Runtime::now`] reads the virtual clock, so deadlines armed relative to
/// "now" stay coherent with the advances a test performs. Clones share one
/// clock and machine queue.
///
/// The transport parameter defaults to [`Never`] for tests that only exercise
/// timers.
pub struct Test<S: crate::transport::poll::Session = Never> {
	shared: Arc<Mutex<Shared<S>>>,
	_transport: PhantomData<fn(S)>,
}

struct Shared<S: crate::transport::poll::Session> {
	now: Instant,
	timers: HashMap<u64, Entry>,
	next_id: u64,
	machines: Vec<Machine<Test<S>>>,
}

struct Entry {
	at: Option<Instant>,
	waiters: kio::WaiterList,
}

impl<S: crate::transport::poll::Session> Test<S> {
	/// A fresh runtime whose virtual clock starts at the real current instant.
	pub fn new() -> Self {
		Self {
			shared: Arc::new(Mutex::new(Shared {
				now: Instant::now(),
				timers: HashMap::new(),
				next_id: 0,
				machines: Vec::new(),
			})),
			_transport: PhantomData,
		}
	}

	/// Move the virtual clock forward, waking every timer that elapses.
	pub fn advance(&self, duration: std::time::Duration) {
		let mut woken = Vec::new();
		{
			let mut shared = self.shared.lock().unwrap();
			shared.now += duration;
			let now = shared.now;
			for entry in shared.timers.values_mut() {
				if entry.at.is_some_and(|at| at <= now) {
					woken.push(entry.waiters.take());
				}
			}
		}
		// Wake outside the lock: a woken task's first move may be to arm a timer.
		for mut waiters in woken {
			waiters.wake();
		}
	}

	/// Jump the virtual clock to the earliest armed timer and fire it.
	///
	/// Returns `false` (moving nothing) when no timer is armed in the future.
	pub fn advance_to_timer(&self) -> bool {
		let next = {
			let shared = self.shared.lock().unwrap();
			let now = shared.now;
			shared
				.timers
				.values()
				.filter_map(|entry| entry.at)
				.filter(|at| *at > now)
				.min()
		};
		match next {
			Some(at) => {
				// Route through `advance` so the wake happens outside the lock.
				let now = self.shared.lock().unwrap().now;
				self.advance(at - now);
				true
			}
			None => false,
		}
	}

	/// Drop every spawned machine without polling it, like a runtime shutting
	/// down mid-session.
	///
	/// Machines hold runtime clones (for timers), so dropping every external
	/// handle alone never drops them; this is the explicit teardown.
	pub fn shutdown(&self) {
		let machines = std::mem::take(&mut self.shared.lock().unwrap().machines);
		drop(machines);
	}

	/// Poll every spawned machine once, dropping the finished ones.
	///
	/// Returns how many machines remain. Polling is unconditional (no waker
	/// bookkeeping): a test advances state, ticks, and asserts.
	pub fn tick(&self) -> usize {
		// Take the machines out so their polls can reach the timers without
		// deadlocking on the shared lock.
		let mut machines = std::mem::take(&mut self.shared.lock().unwrap().machines);
		let waiter = kio::Waiter::noop();
		machines.retain_mut(|machine| machine.poll(&waiter).is_pending());

		let mut shared = self.shared.lock().unwrap();
		// A machine spawned by a machine mid-tick landed in the queue already;
		// keep both.
		shared.machines.extend(machines);
		shared.machines.len()
	}
}

impl<S: crate::transport::poll::Session> Timers for Test<S> {
	type Timer = TestTimer<S>;

	fn timer(&self) -> Self::Timer {
		let id = {
			let mut shared = self.shared.lock().unwrap();
			let id = shared.next_id;
			shared.next_id += 1;
			shared.timers.insert(
				id,
				Entry {
					at: None,
					waiters: kio::WaiterList::new(),
				},
			);
			id
		};
		TestTimer {
			shared: self.shared.clone(),
			id,
		}
	}

	fn now(&self) -> Instant {
		self.shared.lock().unwrap().now
	}
}

impl<S: crate::transport::poll::Session> Runtime for Test<S> {
	type Transport = S;

	fn spawn(&self, machine: Machine<Self>) {
		self.shared.lock().unwrap().machines.push(machine);
	}
}

impl<S: crate::transport::poll::Session> Clone for Test<S> {
	fn clone(&self) -> Self {
		Self {
			shared: self.shared.clone(),
			_transport: PhantomData,
		}
	}
}

impl<S: crate::transport::poll::Session> Default for Test<S> {
	fn default() -> Self {
		Self::new()
	}
}

impl<S: crate::transport::poll::Session> std::fmt::Debug for Test<S> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		let shared = self.shared.lock().unwrap();
		f.debug_struct("Test")
			.field("now", &shared.now)
			.field("timers", &shared.timers.len())
			.field("machines", &shared.machines.len())
			.finish()
	}
}

/// The [`Timer`] handed out by [`Test`]: elapsed exactly when its armed instant
/// is at or before the shared virtual clock.
pub struct TestTimer<S: crate::transport::poll::Session> {
	shared: Arc<Mutex<Shared<S>>>,
	id: u64,
}

impl<S: crate::transport::poll::Session> Timer for TestTimer<S> {
	fn set(&mut self, at: Option<Instant>) {
		let mut shared = self.shared.lock().unwrap();
		if let Some(entry) = shared.timers.get_mut(&self.id) {
			entry.at = at;
		}
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		let mut shared = self.shared.lock().unwrap();
		let now = shared.now;
		let Some(entry) = shared.timers.get_mut(&self.id) else {
			return Poll::Pending;
		};
		match entry.at {
			Some(at) if at <= now => Poll::Ready(()),
			_ => {
				waiter.register(&mut entry.waiters);
				Poll::Pending
			}
		}
	}
}

impl<S: crate::transport::poll::Session> Drop for TestTimer<S> {
	fn drop(&mut self) {
		self.shared.lock().unwrap().timers.remove(&self.id);
	}
}

/// An uninhabited transport, for [`Test`] runtimes that never open a session.
#[derive(Debug, Clone, Copy)]
pub enum Never {}

impl std::fmt::Display for Never {
	fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match *self {}
	}
}

impl std::error::Error for Never {}

impl web_transport_trait::Error for Never {
	fn session_error(&self) -> Option<(u32, String)> {
		match *self {}
	}
}

impl web_transport_trait::poll::Session for Never {
	type SendStream = Never;
	type RecvStream = Never;
	type Error = Never;

	fn poll_accept_uni(&mut self, _: &mut std::task::Context<'_>) -> Poll<Result<Self::RecvStream, Self::Error>> {
		match *self {}
	}

	fn poll_accept_bi(
		&mut self,
		_: &mut std::task::Context<'_>,
	) -> Poll<Result<web_transport_trait::poll::BiStreams<Self>, Self::Error>> {
		match *self {}
	}

	fn poll_open_uni(&mut self, _: &mut std::task::Context<'_>) -> Poll<Result<Self::SendStream, Self::Error>> {
		match *self {}
	}

	fn poll_open_bi(
		&mut self,
		_: &mut std::task::Context<'_>,
	) -> Poll<Result<web_transport_trait::poll::BiStreams<Self>, Self::Error>> {
		match *self {}
	}

	fn poll_send_datagram(&mut self, _: &mut std::task::Context<'_>, _: &[u8]) -> Poll<Result<(), Self::Error>> {
		match *self {}
	}

	fn poll_recv_datagram(&mut self, _: &mut std::task::Context<'_>) -> Poll<Result<bytes::Bytes, Self::Error>> {
		match *self {}
	}

	fn max_datagram_size(&self) -> usize {
		match *self {}
	}

	fn protocol(&self) -> Option<&str> {
		match *self {}
	}

	fn close(&mut self, _: u32, _: &str) {
		match *self {}
	}

	fn poll_closed(&mut self, _: &mut std::task::Context<'_>) -> Poll<Self::Error> {
		match *self {}
	}

	fn stats(&self) -> impl web_transport_trait::Stats {
		// Uninhabited, so this is never called; a concrete type keeps the
		// opaque return type nameable.
		web_transport_trait::StatsUnavailable
	}
}

impl web_transport_trait::poll::SendStream for Never {
	type Error = Never;

	fn poll_write(&mut self, _: &mut std::task::Context<'_>, _: &[u8]) -> Poll<Result<usize, Self::Error>> {
		match *self {}
	}

	fn set_priority(&mut self, _: u8) {
		match *self {}
	}

	fn finish(&mut self) -> Result<(), Self::Error> {
		match *self {}
	}

	fn reset(&mut self, _: u32) {
		match *self {}
	}

	fn poll_closed(&mut self, _: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
		match *self {}
	}
}

impl web_transport_trait::poll::RecvStream for Never {
	type Error = Never;

	fn poll_read(&mut self, _: &mut std::task::Context<'_>, _: &mut [u8]) -> Poll<Result<Option<usize>, Self::Error>> {
		match *self {}
	}

	fn stop(&mut self, _: u32) {
		match *self {}
	}

	fn poll_closed(&mut self, _: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
		match *self {}
	}
}

#[cfg(all(test, not(loom)))]
mod tests {
	use std::time::Duration;

	use super::*;
	use crate::runtime::Deadline;

	fn poll_once(rt: &Test, deadline: &mut Deadline<Test>) -> Poll<()> {
		let _ = rt;
		let waiter = kio::Waiter::noop();
		deadline.poll(&waiter)
	}

	#[test]
	fn fires_only_when_advanced_past() {
		let rt: Test = Test::new();
		let mut deadline = Deadline::after(&rt, Duration::from_secs(5));

		assert!(poll_once(&rt, &mut deadline).is_pending());
		rt.advance(Duration::from_secs(4));
		assert!(poll_once(&rt, &mut deadline).is_pending());
		rt.advance(Duration::from_secs(1));
		assert!(poll_once(&rt, &mut deadline).is_ready());
		// Fused: still ready on a re-poll.
		assert!(poll_once(&rt, &mut deadline).is_ready());
	}

	#[test]
	fn rearm_after_advance_lands_in_the_future() {
		// The reason Runtime::now exists: a deadline armed relative to "now"
		// after a big advance must not sit in the virtual past.
		let rt: Test = Test::new();
		rt.advance(Duration::from_secs(3600));

		let mut deadline = Deadline::after(&rt, Duration::from_secs(1));
		assert!(poll_once(&rt, &mut deadline).is_pending());
		rt.advance(Duration::from_secs(1));
		assert!(poll_once(&rt, &mut deadline).is_ready());
	}

	#[test]
	fn disarmed_never_fires() {
		let rt: Test = Test::new();
		let mut deadline = Deadline::new(&rt);
		assert!(poll_once(&rt, &mut deadline).is_pending());
		rt.advance(Duration::from_secs(3600));
		assert!(poll_once(&rt, &mut deadline).is_pending());
	}

	#[test]
	fn disarming_a_live_countdown_stops_it() {
		let rt: Test = Test::new();
		let mut deadline = Deadline::after(&rt, Duration::from_secs(1));
		deadline.set(None);
		rt.advance(Duration::from_secs(10));
		assert!(poll_once(&rt, &mut deadline).is_pending());

		// And re-arming fires again.
		let at = rt.now() + Duration::from_secs(3);
		deadline.set(Some(at));
		rt.advance(Duration::from_secs(3));
		assert!(poll_once(&rt, &mut deadline).is_ready());
	}

	#[test]
	fn advance_wakes_a_parked_waiter() {
		let rt: Test = Test::new();
		let mut deadline = Deadline::after(&rt, Duration::from_secs(1));

		let woken = Arc::new(std::sync::atomic::AtomicBool::new(false));
		struct Flag(Arc<std::sync::atomic::AtomicBool>);
		impl std::task::Wake for Flag {
			fn wake(self: Arc<Self>) {
				self.0.store(true, std::sync::atomic::Ordering::SeqCst);
			}
		}
		let waker = std::task::Waker::from(Arc::new(Flag(woken.clone())));
		let waiter = kio::Waiter::new(waker);

		assert!(deadline.poll(&waiter).is_pending());
		rt.advance(Duration::from_secs(1));
		assert!(
			woken.load(std::sync::atomic::Ordering::SeqCst),
			"the timer wake was lost"
		);
		assert!(deadline.poll(&waiter).is_ready());
	}

	#[test]
	fn advance_to_timer_jumps_to_the_earliest() {
		let rt: Test = Test::new();
		let start = rt.now();
		let mut near = Deadline::after(&rt, Duration::from_secs(2));
		let mut far = Deadline::after(&rt, Duration::from_secs(9));

		assert!(rt.advance_to_timer());
		assert_eq!(rt.now(), start + Duration::from_secs(2));
		assert!(poll_once(&rt, &mut near).is_ready());
		assert!(poll_once(&rt, &mut far).is_pending());

		assert!(rt.advance_to_timer());
		assert_eq!(rt.now(), start + Duration::from_secs(9));
		assert!(poll_once(&rt, &mut far).is_ready());

		// Nothing armed in the future: the clock stays put.
		assert!(!rt.advance_to_timer());
		assert_eq!(rt.now(), start + Duration::from_secs(9));
	}

	#[test]
	fn dropped_timers_release_their_slot() {
		let rt: Test = Test::new();
		let deadline = Deadline::after(&rt, Duration::from_secs(1));
		drop(deadline);
		assert!(!rt.advance_to_timer(), "a dropped timer still counted as armed");
	}
}
