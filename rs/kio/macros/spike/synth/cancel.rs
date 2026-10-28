#![allow(clippy::all, unused_imports)]
use std::task::{Poll, ready};

#[derive(Debug)]
pub struct Error;

/// Ready on every other poll.
#[derive(Default)]
pub struct Source {
	n: u64,
}

impl Source {
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<u64, Error>> {
		self.n += 1;
		if self.n % 2 == 1 {
			waiter.waker().wake_by_ref();
			return Poll::Pending;
		}
		Poll::Ready(Ok(self.n))
	}
}

pub fn assert_send<T: Send>(_: &T) {}

#[allow(unused_macros)]
macro_rules! block_on {
	($poll:expr) => {{
		let waiter = kio::Waiter::noop();
		loop {
			if let Poll::Ready(value) = $poll(&waiter) {
				break value;
			}
		}
	}};
}

pub struct Guard(pub &'static str);

impl Drop for Guard {
	fn drop(&mut self) {
		println!("dropped guard {}", self.0);
	}
}

pub struct M {
	src: Source,
	state: MState,
}

#[kio::machine(self.state: MState)]
impl M {
	pub fn new() -> Self {
		Self { src: Source::default(), state: MState::Start }
	}

	pub fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<u64, Error>> {
		let guard: Guard = Guard("held across B");
		let a: u64 = wait!(A => self.src.poll_next(waiter))?;
		let b: u64 = wait!(B => self.src.poll_next(waiter))?;
		println!("still holding {}", guard.0);
		Poll::Ready(Ok(a + b))
	}
}

impl Drop for M {
	fn drop(&mut self) {
		println!("dropping M at {}", self.state.name());
	}
}

#[test]
fn cancel() {
	let waiter = kio::Waiter::noop();
	let mut m = M::new();
	assert!(m.poll(&waiter).is_pending());
	println!("after poll 1: {}", m.state.name());
	assert!(m.poll(&waiter).is_pending());
	println!("after poll 2: {}", m.state.name());
	drop(m);
}
