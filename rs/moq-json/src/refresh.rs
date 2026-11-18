//! Waiting for a joiner to want a fresh group.

use std::task::Poll;

use crate::Result;

/// Watches a track's [`Demand`](moq_net::track::Demand) for a group past the largest one written.
///
/// A join or refresh judged by the track's [`refresh`](moq_net::track::Info::refresh) asks for a
/// group past every one written, so a producer restating its value in a new group answers it.
pub(crate) struct Refresh {
	demand: moq_net::track::Demand,

	/// Refreshes waiting on a later write, since the one they would restate starts the newest group.
	parked: kio::WaiterList,
}

impl Refresh {
	pub fn new(track: &moq_net::track::Producer) -> Self {
		Self {
			demand: track.demand(),
			parked: kio::WaiterList::new(),
		}
	}

	/// Wait for [`Self::advanced`]: the refresh is wanted, but nothing written yet can start a group.
	pub fn park<T>(&mut self, waiter: &kio::Waiter) -> Poll<T> {
		waiter.register(&mut self.parked);
		Poll::Pending
	}

	/// A write moved the newest timestamp on, which a parked refresh may now restate at.
	pub fn advanced(&mut self) {
		self.parked.wake();
	}

	/// The track ended, which a parked refresh must see: a finished track's demand stays open
	/// while subscribers drain it, so nothing else wakes it.
	pub fn ended(&mut self) {
		self.parked.wake();
	}

	/// Ready once consumers want a group past the newest written, arming `waiter` for the next
	/// demand change otherwise. Never ready before a group is written (`largest` is `None`),
	/// since there is nothing to restate yet: that parks for the next write. Errors once the
	/// track closes.
	pub fn poll(&mut self, largest: Option<u64>, waiter: &kio::Waiter) -> Poll<Result<()>> {
		if largest.is_none() {
			return self.park(waiter);
		}
		self.demand.poll_refresh(waiter).map_err(Into::into)
	}
}

/// A waiter that records whether it was woken, for tests that a parked poll gets a wake.
#[cfg(test)]
pub(crate) fn flagged() -> (kio::Waiter, std::sync::Arc<Flag>) {
	let flag = std::sync::Arc::new(Flag::default());
	(kio::Waiter::new(flag.clone().into()), flag)
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct Flag(std::sync::atomic::AtomicBool);

#[cfg(test)]
impl Flag {
	/// Whether a wake arrived since the last call.
	pub fn take(&self) -> bool {
		self.0.swap(false, std::sync::atomic::Ordering::SeqCst)
	}
}

#[cfg(test)]
impl std::task::Wake for Flag {
	fn wake(self: std::sync::Arc<Self>) {
		self.0.store(true, std::sync::atomic::Ordering::SeqCst);
	}
}
