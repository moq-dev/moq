//! Grouped-failure signal shared by one subscriber and the groups it handed out.
//!
//! A parked read has registered on the net object, which stays silent after a bad
//! frame. The flag is what wakes it. Dropping the subscription there is what ends
//! publisher demand while those handles are still held.

use std::sync::{Arc, Mutex};
use std::task::Poll;

use crate::error::{Error, Result};

pub(crate) struct Terminal {
	flag: kio::Producer<bool>,
	watch: kio::Consumer<bool>,
	subscription: Mutex<Slot>,
}

struct Slot {
	inner: Option<moq_net::track::Subscriber>,
	/// Set before the subscriber is dropped. A poll that took it out must discard
	/// it instead of putting demand back.
	released: bool,
}

impl Terminal {
	pub(crate) fn new(subscription: Option<moq_net::track::Subscriber>) -> Arc<Self> {
		let flag = kio::Producer::new(false);
		let watch = flag.consume();
		Arc::new(Self {
			flag,
			watch,
			subscription: Mutex::new(Slot {
				inner: subscription,
				released: false,
			}),
		})
	}

	pub(crate) fn is_failed(&self) -> bool {
		*self.watch.read()
	}

	/// Whether grouped authentication has ended this subscriber.
	///
	/// Registers `waiter` until it has, so a later [`Self::fail`] wakes the park.
	pub(crate) fn poll_failed(&self, waiter: &kio::Waiter) -> bool {
		self.watch
			.poll(waiter, |failed| if **failed { Poll::Ready(()) } else { Poll::Pending })
			.is_ready()
	}

	/// End the subscriber: wake every parked read and release publisher demand.
	pub(crate) fn fail(&self) {
		// Write the flag before taking the subscription. A reader woken inline must
		// observe the failure without waiting on this lock.
		if let Ok(mut failed) = self.flag.write()
			&& !*failed
		{
			*failed = true;
		}
		let mut slot = self.subscription.lock().expect("terminal");
		slot.released = true;
		slot.inner.take();
	}

	/// Drop the subscription without failing, so held groups keep reading.
	pub(crate) fn unsubscribe(&self) {
		self.subscription.lock().expect("terminal").inner.take();
	}

	/// Poll `body` against the live subscription.
	///
	/// The subscriber is taken out for the poll so [`Self::fail`] does not nest
	/// this lock under a net read. Once grouped failure has released it, the poll
	/// is [`Error::Authentication`].
	pub(crate) fn poll_subscription<T>(
		&self,
		waiter: &kio::Waiter,
		body: impl FnOnce(&mut moq_net::track::Subscriber, &kio::Waiter) -> Poll<Result<T>>,
	) -> Poll<Result<T>> {
		if self.poll_failed(waiter) {
			return Poll::Ready(Err(Error::Authentication));
		}

		let mut taken = {
			let mut slot = self.subscription.lock().expect("terminal");
			if slot.released {
				return Poll::Ready(Err(Error::Authentication));
			}
			slot.inner.take()
		};
		let Some(inner) = taken.as_mut() else {
			return Poll::Ready(Err(Error::Authentication));
		};
		let result = body(inner, waiter);

		let mut slot = self.subscription.lock().expect("terminal");
		if slot.released {
			return Poll::Ready(Err(Error::Authentication));
		}
		slot.inner = taken;
		result
	}
}
