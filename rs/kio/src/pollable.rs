use std::{
	future::Future,
	ops::{Deref, DerefMut},
	pin::Pin,
	task::{Context, Poll},
};

use crate::Waiter;

/// A pollable computation backed by kio channels.
///
/// Implementors write only [`Self::poll`], registering the [`Waiter`] with the
/// channels they read. Wrap the value in [`Awaitable`] to get a real [`Future`].
///
/// This exists because a kio [`Waiter`] holds the strong `Arc<Waker>` while the
/// channel's [`crate::WaiterList`] keeps only a `Weak`. A bare [`Future`] would have
/// to stash the strong `Waiter` in a field and replace it every poll (or lose its
/// wakeup); [`Awaitable`] does that once so each implementor doesn't have to.
pub trait Pollable: Unpin {
	/// The value the computation resolves to.
	type Output;

	/// Poll for the output, registering `waiter` with the relevant channels if not
	/// yet ready.
	///
	/// Takes `&self`: kio channels poll immutably, so a pollable can be driven
	/// through a shared borrow (e.g. while it lives inside an `&self`-borrowed enum).
	/// Carry any per-poll mutable state in a kio channel or a [`std::cell`] type.
	fn poll(&self, waiter: &Waiter) -> Poll<Self::Output>;
}

/// Adapts a [`Pollable`] into a [`Future`], retaining the strong [`Waiter`] between
/// polls so its weak registration stays live.
///
/// Derefs to the inner value, so any inherent methods you define on it are
/// reachable through the awaitable handle (e.g. a non-blocking `poll`, or an
/// `update`).
pub struct Awaitable<P> {
	inner: P,
	// Retain the previous waiter so its Weak registration survives until the next
	// poll replaces it (see [`crate::WaiterList`]).
	waiter: Option<Waiter>,
}

impl<P> Awaitable<P> {
	/// Wrap a [`Pollable`] so it can be `.await`ed.
	pub fn new(inner: P) -> Self {
		Self { inner, waiter: None }
	}

	/// Consume the wrapper, returning the inner value.
	pub fn into_inner(self) -> P {
		self.inner
	}
}

impl<P> Deref for Awaitable<P> {
	type Target = P;

	fn deref(&self) -> &P {
		&self.inner
	}
}

impl<P> DerefMut for Awaitable<P> {
	fn deref_mut(&mut self) -> &mut P {
		&mut self.inner
	}
}

impl<P: Pollable> Future for Awaitable<P> {
	type Output = P::Output;

	fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<P::Output> {
		// Replacing drops the previous waiter, killing its Weak ref in the list so
		// the inner poll's register call can recycle the slot (see `WaiterList`).
		// `Awaitable<P>` is `Unpin` (P is, via the trait bound), so this deref is sound.
		let this = &mut *self;
		this.waiter = Some(Waiter::new(cx.waker().clone()));
		Pollable::poll(&this.inner, this.waiter.as_ref().unwrap())
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use crate::Producer;

	/// A pollable that waits for the channel value to reach a threshold, with an
	/// inherent method reachable through `Awaitable`'s `DerefMut`.
	struct AtLeast {
		consumer: crate::Consumer<u64>,
		threshold: u64,
	}

	impl AtLeast {
		fn bump_threshold(&mut self) {
			self.threshold += 1;
		}
	}

	impl Pollable for AtLeast {
		type Output = u64;

		fn poll(&self, waiter: &Waiter) -> Poll<u64> {
			let threshold = self.threshold;
			match self.consumer.poll(waiter, |v| {
				let current = **v;
				if current >= threshold {
					Poll::Ready(current)
				} else {
					Poll::Pending
				}
			}) {
				Poll::Ready(Ok(v)) => Poll::Ready(v),
				_ => Poll::Pending,
			}
		}
	}

	#[test]
	fn awaitable_derefs_and_drives() {
		use std::task::Waker;

		let producer = Producer::new(0u64);
		let mut awaitable = Awaitable::new(AtLeast {
			consumer: producer.consume(),
			threshold: 5,
		});

		// Inherent method on the inner reached via DerefMut.
		awaitable.bump_threshold(); // threshold now 6

		// The kio-level poll (reached through Deref) is pending until the value catches up.
		assert!(Pollable::poll(&*awaitable, &Waiter::noop()).is_pending());

		if let Ok(mut v) = producer.write() {
			*v = 6;
		}

		// The std Future resolves once the threshold is met.
		let mut cx = Context::from_waker(Waker::noop());
		let mut awaitable = std::pin::pin!(awaitable);
		assert_eq!(Future::poll(awaitable.as_mut(), &mut cx), Poll::Ready(6));
	}
}
