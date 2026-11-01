use std::{fmt, task::Poll};

use crate::{Consumer, Ref, Waiter};

/// No version [`Watch::judged`] can match: odd, so never an open one, and a closed one
/// only after 2^63 publishes.
const UNJUDGED: u64 = u64::MAX;

/// A [`Consumer`] that remembers the version its predicate was last pending at, so a
/// re-poll the channel hasn't changed since skips the predicate and the value lock.
///
/// A task re-polls every channel it waits on whenever any of them wakes it, and most of
/// those re-polls find nothing changed. [`Consumer::poll`] still locks the value to
/// judge it again. [`Self::poll`] instead compares the version: unchanged, it parks the
/// waiter again and returns [`Poll::Pending`] without calling `f`, and a waiter still
/// parked from the last poll usually takes no lock at all.
///
/// The skip is only sound while `f` would answer the same, so a watch serves one
/// predicate, and that predicate may read only `T` and caller state that changes only
/// after it returns [`Poll::Ready`]. A predicate that reads a clock, another channel, or
/// an argument that moves while it is pending would be skipped past the change it is
/// waiting for. Use [`Consumer::poll`] for those.
///
/// The version is stored only when `f` returns [`Poll::Pending`], so a ready predicate is
/// judged again on every poll: being level-triggered, it stays ready. Cloning a watch
/// clones the consumer and starts it unjudged.
pub struct Watch<T> {
	consumer: Consumer<T>,
	/// The version `f` last returned [`Poll::Pending`] at, or [`UNJUDGED`].
	judged: u64,
}

impl<T> Watch<T> {
	/// Watch `consumer`, judging the predicate on the first poll.
	pub fn new(consumer: Consumer<T>) -> Self {
		Self {
			consumer,
			judged: UNJUDGED,
		}
	}

	/// Poll the shared state with a closure, as [`Consumer::poll`] does, unless nothing
	/// changed since `f` was last pending.
	///
	/// Then `f` is not called: the waiter is registered for the next change and this
	/// returns [`Poll::Pending`]. Any waiter will do, not only the one the last poll used.
	#[cfg_attr(feature = "sim", track_caller)]
	pub fn poll<F, R>(&mut self, waiter: &Waiter, f: F) -> Poll<Result<R, Ref<'_, T>>>
	where
		F: FnOnce(&Ref<'_, T>) -> Poll<R>,
	{
		let channel = &*self.consumer.state;
		if channel.repoll(waiter, self.judged) {
			return Poll::Pending;
		}
		channel.poll_judged(waiter, f, &mut self.judged)
	}

	/// Wait for the closure to return [`Poll::Ready`], as [`Consumer::wait`] does, judging
	/// it only after the channel changed.
	#[cfg(feature = "async")]
	pub async fn wait<F, R>(&mut self, mut f: F) -> Result<R, crate::Closed>
	where
		F: FnMut(&Ref<'_, T>) -> Poll<R> + Unpin,
	{
		crate::wait(move |waiter| self.poll(waiter, &mut f).map(|res| res.map_err(|_| crate::Closed))).await
	}

	/// The consumer being watched, for reads and polls of other predicates.
	pub fn consumer(&self) -> &Consumer<T> {
		&self.consumer
	}
}

impl<T> From<Consumer<T>> for Watch<T> {
	fn from(consumer: Consumer<T>) -> Self {
		Self::new(consumer)
	}
}

impl<T> Clone for Watch<T> {
	fn clone(&self) -> Self {
		Self::new(self.consumer.clone())
	}
}

impl<T: fmt::Debug> fmt::Debug for Watch<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Watch")
			.field("consumer", &self.consumer)
			.field("judged", &self.judged)
			.finish()
	}
}

#[cfg(test)]
mod tests {
	use std::{
		sync::{
			Arc,
			atomic::{AtomicUsize, Ordering},
		},
		task::{Wake, Waker},
	};

	use super::*;
	use crate::Producer;

	struct Count(AtomicUsize);

	impl Wake for Count {
		fn wake(self: Arc<Self>) {
			self.0.fetch_add(1, Ordering::SeqCst);
		}
	}

	fn counting() -> (Arc<Count>, Waiter) {
		let count = Arc::new(Count(AtomicUsize::new(0)));
		(count.clone(), Waiter::new(Waker::from(count)))
	}

	fn woken(count: &Count) -> usize {
		count.0.load(Ordering::SeqCst)
	}

	/// Poll for the value to reach `target`, counting the calls of the predicate.
	fn poll_at_least(watch: &mut Watch<u32>, waiter: &Waiter, target: u32, calls: &mut usize) -> Poll<u32> {
		watch
			.poll(waiter, |value| {
				*calls += 1;
				match **value >= target {
					true => Poll::Ready(**value),
					false => Poll::Pending,
				}
			})
			.map(|res| res.ok().expect("open"))
	}

	/// An unchanged re-poll skips the predicate, and a publish brings it back.
	#[test]
	fn an_unchanged_repoll_skips_the_predicate() {
		let producer = Producer::new(0u32);
		let mut watch = Watch::new(producer.consume());
		let (count, waiter) = counting();
		let mut calls = 0;

		assert!(poll_at_least(&mut watch, &waiter, 2, &mut calls).is_pending());
		assert!(poll_at_least(&mut watch, &waiter, 2, &mut calls).is_pending());
		assert_eq!(calls, 1, "the re-poll judged an unchanged value");

		*producer.write().ok().expect("open") = 1;
		assert_eq!(woken(&count), 1);
		assert!(poll_at_least(&mut watch, &waiter, 2, &mut calls).is_pending());
		assert_eq!(calls, 2, "the publish was skipped");

		*producer.write().ok().expect("open") = 2;
		assert_eq!(woken(&count), 2, "the skipped re-poll lost the registration");
		assert_eq!(poll_at_least(&mut watch, &waiter, 2, &mut calls), Poll::Ready(2));
		// Ready stores no version: a level-triggered predicate stays ready.
		assert_eq!(poll_at_least(&mut watch, &waiter, 2, &mut calls), Poll::Ready(2));
		assert_eq!(calls, 4);
	}

	/// A skipped re-poll parks whichever waiter it is given, not only the last one.
	#[test]
	fn a_skipped_repoll_parks_a_new_waiter() {
		let producer = Producer::new(0u32);
		let mut watch = Watch::new(producer.consume());
		let mut calls = 0;

		assert!(poll_at_least(&mut watch, &Waiter::noop(), 1, &mut calls).is_pending());
		let (count, waiter) = counting();
		assert!(poll_at_least(&mut watch, &waiter, 1, &mut calls).is_pending());
		assert_eq!(calls, 1);

		*producer.write().ok().expect("open") = 1;
		assert_eq!(woken(&count), 1, "the new waiter was never parked");
	}

	/// Closing bumps the version, so a watch hands the closed value back.
	#[test]
	fn a_close_is_never_skipped() {
		let producer = Producer::new(0u32);
		let mut watch = Watch::new(producer.consume());
		let waiter = Waiter::noop();

		assert!(watch.poll(&waiter, |_| Poll::<()>::Pending).is_pending());
		drop(producer);
		let Poll::Ready(Err(closed)) = watch.poll(&waiter, |_| Poll::<()>::Pending) else {
			panic!("the close was skipped");
		};
		assert!(closed.is_closed());
	}

	/// A clone judges its own predicate, whatever the original last saw.
	#[test]
	fn a_clone_starts_unjudged() {
		let producer = Producer::new(0u32);
		let mut watch = Watch::new(producer.consume());
		let waiter = Waiter::noop();
		let mut calls = 0;

		assert!(poll_at_least(&mut watch, &waiter, 1, &mut calls).is_pending());
		let mut clone = watch.clone();
		assert_eq!(poll_at_least(&mut clone, &waiter, 0, &mut calls), Poll::Ready(0));
		assert_eq!(calls, 2);
	}

	/// `wait` resolves on the change it waits for, past the ones it skips.
	#[tokio::test]
	async fn wait_resolves_on_a_later_change() {
		let producer = Producer::new(0u32);
		let mut watch = Watch::new(producer.consume());
		let writer = tokio::spawn(async move {
			for value in 1..=3 {
				*producer.write().ok().expect("open") = value;
				tokio::task::yield_now().await;
			}
		});

		let reached = |value: &Ref<'_, u32>| match **value {
			3 => Poll::Ready(3),
			_ => Poll::Pending,
		};
		assert_eq!(watch.wait(reached).await, Ok(3));
		writer.await.expect("the writer panicked");
	}
}
