//! Regression tests for the value/closed/consumer waiter split.
//!
//! Each non-close event must wake only the waiters that care about it: a value
//! modification wakes neither `closed()` nor `used`/`unused`, and consumer
//! create/drop wakes neither value nor `closed()` waiters. We drive the public
//! async futures by hand with a counting waker so we can assert that a waiter
//! was *not* notified.

use std::{
	future::Future,
	sync::{
		Arc,
		atomic::{AtomicUsize, Ordering},
	},
	task::{Context, Poll, Wake, Waker},
};

use crate::Producer;

/// A waker that counts how many times it was woken.
struct CountWaker(AtomicUsize);

impl CountWaker {
	fn new() -> Arc<Self> {
		Arc::new(Self(AtomicUsize::new(0)))
	}

	fn count(&self) -> usize {
		self.0.load(Ordering::SeqCst)
	}
}

impl Wake for CountWaker {
	fn wake(self: Arc<Self>) {
		self.0.fetch_add(1, Ordering::SeqCst);
	}

	fn wake_by_ref(self: &Arc<Self>) {
		self.0.fetch_add(1, Ordering::SeqCst);
	}
}

/// Bundle a counting waker with a `Context` borrowing it.
fn counting() -> (Arc<CountWaker>, Waker) {
	let waker = CountWaker::new();
	let w = Waker::from(waker.clone());
	(waker, w)
}

#[test]
fn value_modification_does_not_wake_consumer() {
	let producer = Producer::new(0u32);
	let consumer = producer.consume();

	let (waker, w) = counting();
	let mut cx = Context::from_waker(&w);

	let mut fut = Box::pin(producer.unused());
	assert!(matches!(fut.as_mut().poll(&mut cx), Poll::Pending));

	// `unused()` only cares about the consumer count, so value churn shouldn't wake it.
	for i in 1..=100 {
		*producer.write().ok().expect("open") = i;
	}
	assert_eq!(waker.count(), 0, "value modification spuriously woke unused()");
	assert!(matches!(fut.as_mut().poll(&mut cx), Poll::Pending));

	// Dropping the last consumer is what should wake it.
	drop(consumer);
	assert!(waker.count() >= 1, "dropping the last consumer did not wake unused()");
	assert!(matches!(fut.as_mut().poll(&mut cx), Poll::Ready(Ok(()))));
}

#[test]
fn value_modification_does_not_wake_closed() {
	let producer = Producer::new(0u32);
	let _consumer = producer.consume();

	let (waker, w) = counting();
	let mut cx = Context::from_waker(&w);

	let mut fut = Box::pin(producer.closed());
	assert!(matches!(fut.as_mut().poll(&mut cx), Poll::Pending));

	// The hot path: a parked `closed()` must survive per-modification churn untouched.
	for i in 1..=100 {
		*producer.write().ok().expect("open") = i;
	}
	assert_eq!(waker.count(), 0, "value modification spuriously woke closed()");
	assert!(matches!(fut.as_mut().poll(&mut cx), Poll::Pending));

	// Closing is what should wake it.
	producer.close().ok().expect("open");
	assert!(waker.count() >= 1, "close did not wake closed()");
	assert!(matches!(fut.as_mut().poll(&mut cx), Poll::Ready(())));
}

#[test]
fn consumer_churn_does_not_wake_value_or_closed() {
	let producer = Producer::new(0u32);

	let (value_waker, vw) = counting();
	let mut value_cx = Context::from_waker(&vw);
	let (closed_waker, cw) = counting();
	let mut closed_cx = Context::from_waker(&cw);

	// Wait for a value we never set, so the future stays pending throughout.
	let mut value_fut = Box::pin(producer.wait(|m| if **m == 99 { Poll::Ready(()) } else { Poll::Pending }));
	let mut closed_fut = Box::pin(producer.closed());
	assert!(matches!(value_fut.as_mut().poll(&mut value_cx), Poll::Pending));
	assert!(matches!(closed_fut.as_mut().poll(&mut closed_cx), Poll::Pending));

	// Cross the 0<->1 consumer boundary repeatedly. Each transition wakes the
	// consumer-count waiters, but must leave the value and closed waiters alone.
	for _ in 0..100 {
		let consumer = producer.consume();
		drop(consumer);
	}
	assert_eq!(value_waker.count(), 0, "consumer churn spuriously woke a value waiter");
	assert_eq!(
		closed_waker.count(),
		0,
		"consumer churn spuriously woke a closed waiter"
	);
	assert!(matches!(value_fut.as_mut().poll(&mut value_cx), Poll::Pending));
	assert!(matches!(closed_fut.as_mut().poll(&mut closed_cx), Poll::Pending));

	// A real value change still wakes the value waiter. `wait` now hands back a
	// writable `Mut` on success, so just match `Ok(_)`.
	*producer.write().ok().expect("open") = 99;
	assert!(value_waker.count() >= 1, "value change did not wake the value waiter");
	assert!(matches!(value_fut.as_mut().poll(&mut value_cx), Poll::Ready(Ok(_))));
}

#[test]
fn close_wakes_value_closed_and_consumer() {
	let producer = Producer::new(0u32);
	let _consumer = producer.consume();

	let (value_waker, vw) = counting();
	let mut value_cx = Context::from_waker(&vw);
	let (closed_waker, cw) = counting();
	let mut closed_cx = Context::from_waker(&cw);
	let (unused_waker, uw) = counting();
	let mut unused_cx = Context::from_waker(&uw);

	let mut value_fut = Box::pin(producer.wait(|m| if **m == 99 { Poll::Ready(()) } else { Poll::Pending }));
	let mut closed_fut = Box::pin(producer.closed());
	let mut unused_fut = Box::pin(producer.unused());
	assert!(matches!(value_fut.as_mut().poll(&mut value_cx), Poll::Pending));
	assert!(matches!(closed_fut.as_mut().poll(&mut closed_cx), Poll::Pending));
	assert!(matches!(unused_fut.as_mut().poll(&mut unused_cx), Poll::Pending));

	// Closing resolves all three: value/unused with `Err`, closed with `()`.
	producer.close().ok().expect("open");
	assert!(value_waker.count() >= 1, "close did not wake the value waiter");
	assert!(closed_waker.count() >= 1, "close did not wake the closed waiter");
	assert!(
		unused_waker.count() >= 1,
		"close did not wake the consumer-count waiter"
	);
	assert!(matches!(value_fut.as_mut().poll(&mut value_cx), Poll::Ready(Err(_))));
	assert!(matches!(closed_fut.as_mut().poll(&mut closed_cx), Poll::Ready(())));
	assert!(matches!(unused_fut.as_mut().poll(&mut unused_cx), Poll::Ready(Err(_))));
}
