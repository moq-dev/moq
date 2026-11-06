//! Waiting for a joiner to want a fresh group.

use std::task::Poll;

use crate::Result;

/// Watches a track's [`Demand`](moq_net::track::Demand) for a group past the largest one written.
///
/// A join judged by the track's [`refresh`](moq_net::track::Info::refresh) ratio asks for the next
/// free sequence, so a producer restating its value in a new group answers it.
pub(crate) struct Refresh {
	demand: moq_net::track::Demand,
}

impl Refresh {
	pub fn new(track: &moq_net::track::Producer) -> Self {
		Self { demand: track.demand() }
	}

	/// Ready once consumers want a group past `largest`, the newest group written, arming `waiter`
	/// for the next demand change otherwise. Never ready before a group is written (`None`), since
	/// there is nothing to restate yet. Errors once the track closes.
	pub fn poll(&mut self, largest: Option<u64>, waiter: &kio::Waiter) -> Poll<Result<()>> {
		// Drain the changes so `waiter` is armed for the next one; the decision reads the level.
		loop {
			match self.demand.poll_wanted_changed(waiter) {
				Poll::Ready(Ok(_)) => continue,
				Poll::Ready(Err(err)) => return Poll::Ready(Err(err.into())),
				Poll::Pending => break,
			}
		}

		match (self.demand.wanted(), largest) {
			(Some(wanted), Some(largest)) if wanted > largest => Poll::Ready(Ok(())),
			_ => Poll::Pending,
		}
	}
}
