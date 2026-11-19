//! A seeded task schedule for the simulator.
//!
//! The simulator polls ready tasks in the order they woke, so two sessions' tasks always
//! run in the order the test happened to wake them. Separate threads give no such
//! order. While a schedule is [`install`]ed, every task [`spawn`] starts (the harness's
//! session and origin drivers included) may be held back for a few executor passes
//! before a poll, letting every other ready task run first, the way a thread descheduled
//! for a moment would. Holds are drawn from the run's [`kio::clock::seed`], so the
//! run's seed replays its schedule.
//!
//! It reorders whole task polls. An interleaving inside one poll, between two threads
//! taking the same lock, is out of its reach.

#![allow(dead_code, reason = "shared by several test binaries, each using only part of it")]

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

thread_local! {
	static SCHEDULE: RefCell<Option<(Schedule, u64)>> = const { RefCell::new(None) };
}

/// How often and how long a poll is held back.
pub struct Schedule {
	/// The chance, in 1/1024ths, that a poll is held back at all.
	pub chance: u64,
	/// The most executor passes one hold lasts.
	pub longest: u64,
}

/// Holds the schedule installed until dropped.
pub struct Installed(());

impl Drop for Installed {
	fn drop(&mut self) {
		SCHEDULE.with(|schedule| schedule.borrow_mut().take());
	}
}

/// Hold back the polls of every task [`spawn`] starts on this thread, as `schedule`
/// draws from the running simulator's seed.
pub fn install(schedule: Schedule) -> Installed {
	SCHEDULE.with(|installed| {
		let mut installed = installed.borrow_mut();
		assert!(installed.is_none(), "a schedule is already installed");
		// Salted, so a test drawing from the same seed draws other numbers.
		*installed = Some((schedule, kio::clock::seed() ^ 0x5CED_0000_5CED));
	});
	Installed(())
}

/// Spawn on the simulator, under the installed schedule if there is one.
pub fn spawn<F>(future: F) -> kio::clock::sim::JoinHandle<F::Output>
where
	F: Future + 'static,
	F::Output: 'static,
{
	match SCHEDULE.with(|schedule| schedule.borrow().is_some()) {
		true => kio::clock::sim::spawn(Held {
			future: Box::pin(future),
			hold: None,
		}),
		false => kio::clock::sim::spawn(future),
	}
}

/// The executor passes the next poll waits, 0 to run it now.
fn draw() -> u64 {
	SCHEDULE.with(|schedule| {
		let mut schedule = schedule.borrow_mut();
		let Some((schedule, state)) = schedule.as_mut() else {
			return 0;
		};
		match next(state) % 1024 < schedule.chance {
			true => 1 + next(state) % schedule.longest,
			false => 0,
		}
	})
}

/// splitmix64.
fn next(state: &mut u64) -> u64 {
	*state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
	let mut z = *state;
	z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
	z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
	z ^ (z >> 31)
}

struct Held<F: Future> {
	future: Pin<Box<F>>,
	/// Passes left before the held poll runs.
	hold: Option<u64>,
}

impl<F: Future> Future for Held<F> {
	type Output = F::Output;

	fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
		let left = match self.hold.take() {
			Some(left) => left,
			None => draw(),
		};
		if left > 0 {
			self.hold = Some(left - 1);
			// Woken now, it goes behind every task already ready.
			cx.waker().wake_by_ref();
			return Poll::Pending;
		}
		self.future.as_mut().poll(cx)
	}
}
