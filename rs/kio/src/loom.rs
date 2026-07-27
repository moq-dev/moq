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

use loom::{future::block_on, thread};

use crate::{Producer, Ref, Shared};

/// Model a three-thread test with a preemption bound.
///
/// The two-thread tests below search exhaustively in well under a second. Adding a
/// third thread blows that up to minutes, and the bugs these catch are all short
/// interleavings, so bound the search rather than pay for the tail.
fn bounded(f: impl Fn() + Sync + Send + 'static) {
	let mut builder = loom::model::Builder::new();
	builder.preemption_bound = Some(3);
	builder.check(f);
}

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
	bounded(|| {
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
	bounded(|| {
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
