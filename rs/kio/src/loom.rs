//! Loom model checks for the kio channel primitives.
//!
//! kio is the shared state under every moq-net handle, so a lost wakeup or a
//! resurrected channel here surfaces as a stalled subscription several layers up.
//! These tests permute the thread interleavings exhaustively instead of hoping a
//! stress loop hits the bad one.
//!
//! Two failure modes are in scope:
//!
//! - **Lost wakeup**: a parked [`loom::future::block_on`] that is never woken leaves
//!   every thread blocked, which loom reports as a deadlock. So "the assertion" for
//!   these is simply that the test terminates.
//! - **Bad state**: a handle observing something the API promises it can't, asserted
//!   directly.
//!
//! Run with `just rs loom`; `cfg(loom)` is never set in a normal build.

use std::task::Poll;

use loom::{
	future::block_on,
	sync::{
		Arc,
		atomic::{AtomicBool, Ordering},
	},
	thread,
};

use crate::{Closed, Fan, Lock, Producer, Queue, Ref, Shared, Waiter, WaiterList, wait};

/// Ready once the value equals `n`.
fn equals(n: u32) -> impl FnMut(&Ref<'_, u32>) -> Poll<()> + Unpin {
	move |v: &Ref<'_, u32>| if **v == n { Poll::Ready(()) } else { Poll::Pending }
}

/// A write must reach a parked consumer no matter how it interleaves with the
/// producer drop that closes the channel behind it.
#[test]
fn write_wakes_a_parked_consumer() {
	loom::model(|| {
		let producer = Producer::new(0u32);
		let consumer = producer.consume();

		let writer = thread::spawn(move || {
			*producer.write().ok().expect("open") = 1;
			// The producer drops here, closing the channel right behind the write.
		});

		assert_eq!(block_on(consumer.wait(equals(1))), Ok(()), "the write was lost");
		writer.join().unwrap();
	});
}

/// `Mut::drop` drains the waiter list under the lock and wakes after releasing it.
/// Two writers racing through that window must still leave the consumer woken.
#[test]
fn concurrent_writes_never_lose_a_wakeup() {
	loom::model(|| {
		let producer = Producer::new(0u32);
		let consumer = producer.consume();
		let second = producer.clone();

		let a = thread::spawn(move || *producer.write().ok().expect("open") += 1);
		let b = thread::spawn(move || *second.write().ok().expect("open") += 1);

		assert_eq!(block_on(consumer.wait(equals(2))), Ok(()), "a write was lost");
		a.join().unwrap();
		b.join().unwrap();
	});
}

/// Both producers race to be the last one out. Exactly one must run the close, and
/// the consumer parked on `closed()` must be woken by whichever it was.
#[test]
fn racing_last_producer_drops_still_close() {
	loom::model(|| {
		let producer = Producer::new(0u32);
		let second = producer.clone();
		let consumer = producer.consume();

		let a = thread::spawn(move || drop(producer));
		let b = thread::spawn(move || drop(second));

		block_on(consumer.closed());
		a.join().unwrap();
		b.join().unwrap();
	});
}

/// `ProducerWeak::produce` promises `None` once the channel is closed, so a `Some`
/// must be a producer that can actually write.
#[test]
fn weak_upgrade_never_resurrects_a_closed_channel() {
	loom::model(|| {
		let producer = Producer::new(0u32);
		let weak = producer.weak();

		let closer = thread::spawn(move || drop(producer));

		let upgraded = weak.produce();
		closer.join().unwrap();

		if let Some(upgraded) = upgraded {
			assert!(
				upgraded.write().is_ok(),
				"produce() handed back a producer on a closed channel"
			);
		}
	});
}

/// `Weak` owns nothing, not even the state allocation, so an upgrade races the
/// deallocation as well as the close. Same contract: a `Some` must be writable.
#[test]
fn weak_downgrade_upgrade_races_the_last_drop() {
	loom::model(|| {
		let producer = Producer::new(0u32);
		let weak = producer.downgrade();

		let closer = thread::spawn(move || drop(producer));

		let upgraded = weak.upgrade();
		closer.join().unwrap();

		if let Some(upgraded) = upgraded {
			assert!(
				upgraded.write().is_ok(),
				"upgrade() handed back a producer on a closed channel"
			);
		}
	});
}

/// The first consumer appearing must wake a producer parked on `used()`, however the
/// `fetch_add` interleaves with the waiter registration.
#[test]
fn first_consumer_wakes_used() {
	loom::model(|| {
		let producer = Producer::new(0u32);
		let second = producer.clone();

		// The consumer rides back on the join handle so it outlives the `used()` poll.
		let maker = thread::spawn(move || second.consume());

		assert_eq!(block_on(producer.used()), Ok(()), "the new consumer was missed");
		drop(maker.join().unwrap());
	});
}

/// The mirror image: the last consumer leaving must wake a producer parked on
/// `unused()`.
#[test]
fn last_consumer_wakes_unused() {
	loom::model(|| {
		let producer = Producer::new(0u32);
		let consumer = producer.consume();

		let dropper = thread::spawn(move || drop(consumer));

		assert_eq!(block_on(producer.unused()), Ok(()), "the last drop was missed");
		dropper.join().unwrap();
	});
}

/// A consumer created and dropped while `unused()` is parked must not leave it
/// parked on a stale count.
#[test]
fn consumer_churn_resolves_unused() {
	loom::model(|| {
		let producer = Producer::new(0u32);
		let second = producer.clone();

		let churn = thread::spawn(move || drop(second.consume()));

		assert_eq!(block_on(producer.unused()), Ok(()), "unused() stalled on churn");
		churn.join().unwrap();
	});
}

/// `Shared` has no producer/consumer split: any handle can mutate, and every
/// mutation must wake every other handle parked on a predicate.
#[test]
fn shared_mutation_wakes_a_parked_handle() {
	loom::model(|| {
		let shared = Shared::new(0u32);
		let other = shared.clone();

		let writer = thread::spawn(move || *other.lock() = 1);

		drop(block_on(shared.wait(equals(1))));
		writer.join().unwrap();
	});
}

/// A push must reach a pop parked on an empty queue no matter how the two
/// interleave.
#[test]
fn queue_push_wakes_a_parked_pop() {
	loom::model(|| {
		let queue = Queue::new();
		let pusher = queue.clone();

		let push = thread::spawn(move || pusher.try_push(1u32).unwrap());

		assert_eq!(block_on(queue.pop()), Ok(1), "the push was lost");
		push.join().unwrap();
	});
}

/// A pop freeing the only slot of a bounded queue must wake a push parked on it.
#[test]
fn queue_pop_wakes_a_parked_push() {
	loom::model(|| {
		let queue = Queue::bounded(1);
		queue.try_push(1u32).unwrap();
		let popper = queue.clone();

		let pop = thread::spawn(move || assert_eq!(popper.try_pop().unwrap(), Some(1)));

		assert_eq!(block_on(queue.push(2)), Ok(()), "the freed slot was missed");
		pop.join().unwrap();
	});
}

/// A close racing a parked pop must still wake it; drained-then-closed is the
/// only acceptable outcome.
#[test]
fn queue_close_wakes_a_parked_pop() {
	loom::model(|| {
		let queue = Queue::<u32>::new();
		let closer = queue.clone();

		let close = thread::spawn(move || closer.close());

		assert_eq!(block_on(queue.pop()), Err(Closed), "the close was lost");
		close.join().unwrap();
	});
}

/// `Fan` holds wakes back while a guard is out and delivers them when the last
/// one drops. A wake racing that window must still reach the parked waiter, whichever
/// side of the hold it lands on.
#[test]
fn a_held_wake_is_never_lost() {
	loom::model(|| {
		let fan = Fan::new();
		let ready = Arc::new(AtomicBool::new(false));

		let waker = {
			let fan = fan.clone();
			let ready = ready.clone();

			thread::spawn(move || {
				let hold = fan.hold();
				ready.store(true, Ordering::SeqCst);
				fan.wake();
				// The deferred wake is delivered here.
				drop(hold);
			})
		};

		// Register before reading `ready`: the other order has a window where the store
		// and the wake both land between the read and the registration.
		block_on(wait(|waiter| {
			fan.register(waiter);

			match ready.load(Ordering::SeqCst) {
				true => Poll::Ready(()),
				false => Poll::Pending,
			}
		}));

		waker.join().unwrap();
	});
}

/// A wake blocked on the projected list must recheck the hold after it acquires the
/// list lock. Otherwise it can pass the deferral check, stall on the list, and deliver
/// while a hold created in that window is still live.
#[test]
fn a_hold_covers_a_wake_waiting_for_the_list() {
	loom::model(|| {
		use std::sync::Arc as StdArc;

		struct Flag(AtomicBool);

		impl std::task::Wake for Flag {
			fn wake(self: StdArc<Self>) {
				self.wake_by_ref();
			}

			fn wake_by_ref(self: &StdArc<Self>) {
				self.0.store(true, Ordering::SeqCst);
			}
		}

		let state = Lock::new(WaiterList::new());
		let fan = Fan::project(&state, |waiters| waiters);
		let flag = StdArc::new(Flag(AtomicBool::new(false)));
		let waiter = Waiter::new(std::task::Waker::from(flag.clone()));
		waiter.register(&mut state.lock());

		let state_guard = state.lock();
		let waking = thread::spawn({
			let fan = fan.clone();
			move || fan.wake()
		});

		let hold = fan.hold();
		drop(state_guard);
		waking.join().unwrap();
		assert!(!flag.0.load(Ordering::SeqCst), "wake escaped a live hold");

		drop(hold);
		assert!(flag.0.load(Ordering::SeqCst), "deferred wake never arrived");
	});
}
