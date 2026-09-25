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

use crate::{Closed, Lock, Producer, Queue, Ref, Shared, WaiterList, wait};

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

/// The invariant `write_unused` exists for: a consumer and a teardown racing must
/// never both win. Either the consumer is minted and the teardown is declined, or
/// the teardown closes the channel and the weak handle mints nothing.
///
/// Without the count moving under the state lock, the "closed and minted" corner
/// is reachable: the bump lands after the teardown read zero, and the consumer is
/// left holding a channel that was torn down for being unused.
#[test]
fn a_teardown_and_a_consumer_never_both_win() {
	loom::model(|| {
		let producer = Producer::new(0u32);
		let weak = producer.weak();

		let consumer = thread::spawn(move || weak.try_consume());

		let committed = match producer.write_unused() {
			crate::Unused::Idle(guard) => {
				guard.close();
				true
			}
			crate::Unused::Used => false,
			crate::Unused::Closed => unreachable!("nothing else closes this channel"),
		};

		let consumer = consumer.join().unwrap();
		assert!(
			!(committed && consumer.is_some()),
			"the teardown cancelled a consumer that had already been handed out"
		);
		assert!(
			committed || consumer.is_some(),
			"the consumer was refused by a teardown that never happened"
		);
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

/// Drain the list under its lock, then wake it outside, as the channels do.
fn wake(list: &Lock<WaiterList>) {
	let mut waiters = list.lock().take();
	waiters.wake();
}

/// A partial wake retires the waiter still parked on the quiet list. The new
/// registration must hear a terminal wake on the drained list or the task hangs.
#[test]
fn a_replaced_waiter_hears_the_drained_list_again() {
	loom::model(|| {
		let woken = Lock::new(WaiterList::new());
		let parked = Lock::new(WaiterList::new());
		let done = Arc::new(AtomicBool::new(false));

		let waker = {
			let woken = woken.clone();
			let done = done.clone();

			thread::spawn(move || {
				// Drains `woken`; a re-poll now re-registers while still parked on
				// `parked`.
				wake(&woken);
				done.store(true, Ordering::SeqCst);
				// Must reach the re-registered waiter, or the model deadlocks.
				wake(&woken);
			})
		};

		// Register before reading `done`: the other order has a window where the
		// store and both wakes land between the read and the registration.
		block_on(wait(|waiter| {
			waiter.register(&mut woken.lock());
			waiter.register(&mut parked.lock());
			match done.load(Ordering::SeqCst) {
				true => Poll::Ready(()),
				false => Poll::Pending,
			}
		}));

		waker.join().unwrap();
	});
}

#[test]
fn consumer_creation_cannot_cross_an_idle_guard() {
	loom::model(|| {
		let producer = Producer::new(0u32);
		let existing = producer.consume();
		let weak = producer.weak();
		let minting = thread::spawn(move || weak.consume());
		drop(existing);
		if let crate::Unused::Idle(guard) = producer.write_unused() {
			thread::yield_now();
			assert!(!producer.is_used(), "consumer appeared while idle guard was held");
			guard.close();
		}
		drop(minting.join().unwrap());
	});
}
