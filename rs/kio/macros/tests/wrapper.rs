//! What `#[kio::wrapper]` generates for each poll shape it accepts.

use std::{
	pin::pin,
	task::{Context, Poll, Waker},
};

/// One poll of `fut` on a waker that does nothing.
fn poll_once<F: Future>(fut: F) -> Poll<F::Output> {
	pin!(fut).poll(&mut Context::from_waker(Waker::noop()))
}

struct Values {
	queue: kio::Queue<u64>,
	taken: Vec<u64>,
}

impl Values {
	fn new(values: &[u64]) -> Self {
		let queue = kio::Queue::new();
		for value in values {
			queue.try_push(*value).unwrap();
		}
		Self {
			queue,
			taken: Vec::new(),
		}
	}

	/// The next value.
	#[kio::wrapper]
	fn poll_next(&self, waiter: &kio::Waiter) -> Poll<Result<u64, kio::Closed>> {
		self.queue.poll_pop(waiter)
	}

	/// The next value at or above `min`, keeping the smaller ones it skips.
	#[kio::wrapper]
	pub fn poll_at_least(&mut self, w: &kio::Waiter, mut min: u64) -> Poll<Result<u64, kio::Closed>> {
		min = min.max(1);
		loop {
			match self.queue.poll_pop(w) {
				Poll::Ready(Ok(value)) if value < min => self.taken.push(value),
				other => return other,
			}
		}
	}

	/// Moves the next value into `into`, with the waiter last.
	#[kio::wrapper]
	fn poll_move_into(&self, into: &mut Vec<u64>, waiter: &kio::Waiter) -> Poll<()> {
		if let Poll::Ready(Ok(value)) = self.queue.poll_pop(waiter) {
			into.push(value);
		}
		Poll::Ready(())
	}

	/// The next value, converted: `T` appears only in the return type.
	#[kio::wrapper]
	fn poll_convert<T: From<u64>>(&self, waiter: &kio::Waiter) -> std::task::Poll<Option<T>> {
		self.queue.poll_pop(waiter).map(|value| value.ok().map(T::from))
	}

	/// Kept for compatibility.
	#[deprecated = "use `poll_next`"]
	#[kio::wrapper]
	fn poll_old(&self, waiter: &kio::Waiter) -> Poll<Result<u64, kio::Closed>> {
		self.queue.poll_pop(waiter)
	}

	/// The next value, under a keyword's name.
	#[kio::wrapper]
	fn poll_match(&self, waiter: &kio::Waiter) -> Poll<Result<u64, kio::Closed>> {
		self.queue.poll_pop(waiter)
	}

	/// Only compiled where the poll is.
	#[cfg(test)]
	#[kio::wrapper]
	fn poll_conditional(&self, waiter: &kio::Waiter) -> Poll<Result<u64, kio::Closed>> {
		self.queue.poll_pop(waiter)
	}

	/// Never compiled: a wrapper without the poll's `cfg` would call a missing method.
	#[cfg(not(test))]
	#[kio::wrapper]
	fn poll_missing(&self, waiter: &kio::Waiter) -> Poll<Result<u64, kio::Closed>> {
		self.queue.poll_pop(waiter)
	}
}

#[test]
fn awaits_the_poll() {
	let values = Values::new(&[1]);
	assert_eq!(poll_once(values.next()), Poll::Ready(Ok(1)));
	assert_eq!(poll_once(values.next()), Poll::Pending);
	values.queue.close();
	assert_eq!(poll_once(values.next()), Poll::Ready(Err(kio::Closed)));
}

#[test]
fn passes_arguments_to_every_poll() {
	let mut values = Values::new(&[1, 2]);
	let mut at_least = Box::pin(values.at_least(3));
	assert_eq!(poll_once(at_least.as_mut()), Poll::Pending);
	drop(at_least);
	assert_eq!(values.taken, [1, 2]);

	values.queue.try_push(4).unwrap();
	assert_eq!(poll_once(values.at_least(3)), Poll::Ready(Ok(4)));

	let mut into = Vec::new();
	values.queue.try_push(5).unwrap();
	assert_eq!(poll_once(values.move_into(&mut into)), Poll::Ready(()));
	assert_eq!(into, [5]);
}

#[test]
fn names_the_generic_parameters() {
	let values = Values::new(&[7]);
	assert_eq!(poll_once(values.convert::<u128>()), Poll::Ready(Some(7u128)));
}

#[test]
#[allow(deprecated, reason = "awaits the deprecated wrapper")]
fn keeps_attributes() {
	let values = Values::new(&[1, 2]);
	assert_eq!(poll_once(values.old()), Poll::Ready(Ok(1)));
	assert_eq!(poll_once(values.conditional()), Poll::Ready(Ok(2)));
}

#[test]
fn escapes_a_keyword_name() {
	let values = Values::new(&[3]);
	assert_eq!(poll_once(values.r#match()), Poll::Ready(Ok(3)));
}

/// A wrapper on a generic impl, beside a machine that waits on it.
struct Pair<T> {
	queue: kio::Queue<T>,
}

impl<T> Pair<T> {
	/// The next item.
	#[kio::wrapper]
	fn poll_item(&self, waiter: &kio::Waiter) -> Poll<Result<T, kio::Closed>> {
		self.queue.poll_pop(waiter)
	}
}

struct Sum {
	pair: Pair<u64>,
	state: SumState,
}

#[kio::machine(self.state: SumState)]
impl Sum {
	/// Two items, added.
	#[kio::wrapper]
	fn poll_sum(&mut self, waiter: &kio::Waiter) -> Poll<Result<u64, kio::Closed>> {
		let first: u64 = wait!(First => self.pair.poll_item(waiter))?;
		let second = wait!(Second => self.pair.poll_item(waiter))?;
		Poll::Ready(Ok(first + second))
	}
}

#[test]
fn composes_with_a_machine() {
	let mut sum = Sum {
		pair: Pair {
			queue: kio::Queue::new(),
		},
		state: SumState::Start,
	};
	sum.pair.queue.try_push(1).unwrap();
	assert_eq!(poll_once(sum.sum()), Poll::Pending);
	assert_eq!(sum.state.name(), "Second");
	sum.pair.queue.try_push(2).unwrap();
	assert_eq!(poll_once(sum.sum()), Poll::Ready(Ok(3)));
}

#[test]
fn is_send_when_its_borrows_are() {
	fn send<F: Future + Send>(fut: F) -> F {
		fut
	}
	let pair = Pair {
		queue: kio::Queue::new(),
	};
	pair.queue.try_push(1u64).unwrap();
	assert_eq!(poll_once(send(pair.item())), Poll::Ready(Ok(1)));
}
