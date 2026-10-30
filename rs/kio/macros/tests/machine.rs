//! How a `#[kio::machine]` runs: where it resumes, what each call re-runs, and when
//! its stored locals drop.

use std::{
	sync::{
		Arc,
		atomic::{AtomicUsize, Ordering},
	},
	task::Poll,
};

/// Counts its drops.
struct Guard(Arc<AtomicUsize>);

impl Drop for Guard {
	fn drop(&mut self) {
		self.0.fetch_add(1, Ordering::Relaxed);
	}
}

struct Steps {
	values: kio::Queue<u64>,
	polls: usize,
	bail: bool,
	drops: Arc<AtomicUsize>,
	state: StepsState,
}

#[kio::machine(self.state: StepsState)]
impl Steps {
	fn new(values: &kio::Queue<u64>) -> Self {
		Self {
			values: values.clone(),
			polls: 0,
			bail: false,
			drops: Arc::default(),
			state: StepsState::Start,
		}
	}

	fn poll_steps(&mut self, waiter: &kio::Waiter) -> Poll<Result<u64, kio::Closed>> {
		#[every_poll]
		self.polls += 1;

		let guard: Guard = Guard(self.drops.clone());
		let first: u64 = wait!(First => self.values.poll_pop(waiter))?;
		if first == 0 {
			return Poll::Ready(Ok(0));
		}
		let mut total: u64 = first;
		// Leaves the machine at the wait, and runs again on the next call.
		let second = wait!(Second => {
			if self.bail {
				return Poll::Ready(Ok(100));
			}
			self.values.poll_pop(waiter)
		})?;
		// The guard's last use, so it drops on leaving this wait.
		drop(guard);
		total += second;
		wait!(Third => self.values.poll_pop(waiter).map_ok(|third| *total += third))?;
		Poll::Ready(Ok(total))
	}
}

#[test]
fn resumes_at_each_wait() {
	let values = kio::Queue::new();
	let mut steps = Steps::new(&values);
	let waiter = kio::Waiter::noop();
	assert_eq!(steps.state.name(), "Start");

	assert!(steps.poll_steps(&waiter).is_pending());
	assert_eq!(steps.state.name(), "First");
	assert!(steps.poll_steps(&waiter).is_pending());
	assert_eq!(steps.polls, 2);

	values.try_push(1).unwrap();
	assert!(steps.poll_steps(&waiter).is_pending());
	assert_eq!(steps.state.name(), "Second");
	assert_eq!(steps.drops.load(Ordering::Relaxed), 0);

	values.try_push(2).unwrap();
	assert!(steps.poll_steps(&waiter).is_pending());
	assert_eq!(steps.state.name(), "Third");
	assert_eq!(steps.drops.load(Ordering::Relaxed), 1);

	values.try_push(3).unwrap();
	assert_eq!(steps.poll_steps(&waiter), Poll::Ready(Ok(6)));
	assert_eq!(steps.state.name(), "Done");
	assert_eq!(steps.polls, 5);
}

#[test]
fn returns_early_from_run_once_code() {
	let values = kio::Queue::new();
	let mut steps = Steps::new(&values);
	values.try_push(0).unwrap();
	assert_eq!(steps.poll_steps(&kio::Waiter::noop()), Poll::Ready(Ok(0)));
	assert_eq!(steps.state.name(), "Done");
	assert_eq!(steps.drops.load(Ordering::Relaxed), 1);
}

#[test]
fn returns_from_a_wait() {
	let values = kio::Queue::new();
	let mut steps = Steps::new(&values);
	values.try_push(1).unwrap();
	steps.bail = true;
	assert_eq!(steps.poll_steps(&kio::Waiter::noop()), Poll::Ready(Ok(100)));
	// A `return` inside the poll leaves the machine where it was.
	assert_eq!(steps.state.name(), "Second");
}

#[test]
fn propagates_errors() {
	let values = kio::Queue::new();
	let mut steps = Steps::new(&values);
	values.close();
	assert_eq!(steps.poll_steps(&kio::Waiter::noop()), Poll::Ready(Err(kio::Closed)));
	assert_eq!(steps.state.name(), "Done");
	assert_eq!(steps.drops.load(Ordering::Relaxed), 1);
}

#[test]
fn drops_stored_locals_when_dropped() {
	let values = kio::Queue::new();
	let mut steps = Steps::new(&values);
	values.try_push(1).unwrap();
	assert!(steps.poll_steps(&kio::Waiter::noop()).is_pending());
	assert_eq!(steps.state.name(), "Second");

	let drops = steps.drops.clone();
	drop(steps);
	assert_eq!(drops.load(Ordering::Relaxed), 1);
}

#[test]
#[should_panic(expected = "polled after returning `Poll::Ready`")]
fn panics_when_polled_after_ready() {
	let values = kio::Queue::new();
	let mut steps = Steps::new(&values);
	values.try_push(0).unwrap();
	let waiter = kio::Waiter::noop();
	assert!(steps.poll_steps(&waiter).is_ready());
	let _ = steps.poll_steps(&waiter);
}

#[test]
fn is_send_when_its_locals_are() {
	fn assert_send<T: Send>() {}
	assert_send::<Steps>();
}

/// A conditional wait: one that is ready at once when it has nothing to wait for.
struct Maybe {
	values: kio::Queue<u64>,
	state: MaybeState,
}

#[kio::machine(self.state: MaybeState)]
impl Maybe {
	fn poll_maybe(&mut self, waiter: &kio::Waiter) -> Poll<Result<u64, kio::Closed>> {
		let first: u64 = wait!(First => self.values.poll_pop(waiter))?;
		let more: bool = first > 1;
		let second = wait!(Second => match more {
			true => self.values.poll_pop(waiter),
			false => Poll::Ready(Ok(0)),
		})?;
		Poll::Ready(Ok(first + second))
	}
}

#[test]
fn skips_a_conditional_wait() {
	let values = kio::Queue::new();
	let waiter = kio::Waiter::noop();

	let mut maybe = Maybe {
		values: values.clone(),
		state: MaybeState::Start,
	};
	values.try_push(1).unwrap();
	assert_eq!(maybe.poll_maybe(&waiter), Poll::Ready(Ok(1)));

	let mut maybe = Maybe {
		values: values.clone(),
		state: MaybeState::Start,
	};
	values.try_push(2).unwrap();
	assert!(maybe.poll_maybe(&waiter).is_pending());
	values.try_push(3).unwrap();
	assert_eq!(maybe.poll_maybe(&waiter), Poll::Ready(Ok(5)));
}

/// A typed tuple binding, and a macro's named argument that shares a moved local's name.
struct Pair {
	values: kio::Queue<u64>,
	state: PairState,
}

#[kio::machine(self.state: PairState)]
impl Pair {
	fn poll_pair(&mut self, waiter: &kio::Waiter) -> Poll<Result<String, kio::Closed>> {
		let (low, high): (u64, u64) = wait!(First => self.values.poll_pop(waiter)).map(|first| (first, first + 1))?;
		let label: String = format!("{low}");
		// Moved before the wait, so only the named argument below says `label`.
		let moved: String = label;
		wait!(Second => self.values.poll_pop(waiter))?;
		Poll::Ready(Ok(format!("{moved}-{high}-{label}", label = low)))
	}
}

#[test]
fn stores_typed_tuples() {
	let values = kio::Queue::new();
	let mut pair = Pair {
		values: values.clone(),
		state: PairState::Start,
	};
	values.try_push(1).unwrap();
	values.try_push(0).unwrap();
	assert_eq!(
		pair.poll_pair(&kio::Waiter::noop()),
		Poll::Ready(Ok("1-2-1".to_string()))
	);
}
