//! A subscription's tail: the group streams still owed once the publisher has ended it.
//!
//! A publisher ends a subscription only after every group stream it opened has finished,
//! but QUIC does not order streams, so one opened before the end can still reach us after
//! it. The subscriber keeps the subscription routable until each owed group is accounted
//! for, or a grace expires for a group whose stream was reset before its header arrived.
//! A reset that keeps the header (reliable reset) would make the grace unnecessary.
//!
//! A stream whose header arrived is read to its end whatever the grace says: a group ends
//! on its own stream's FIN or reset, never because its track ended. A lost datagram is not
//! owed: it leaves a hole that waits out the grace like a stream reset before its header.

use std::{ops::Range, task::Poll, time::Duration};

pub(crate) use crate::ledger::{GRACE, grace};
use crate::{
	ledger::Ledger,
	time::{Deadline, Instant},
};

/// The data streams a subscription has received, and the groups they account for.
#[derive(Debug)]
pub(crate) struct Tail {
	ledger: Ledger,
	streams: u64,
	// Streams whose header arrived and that are still being read.
	active: u64,
	// The publisher ended the subscription, so the track is over even if the session
	// dies before the tail settles.
	ended: bool,
}

impl Default for Tail {
	fn default() -> Self {
		Self::new(GRACE)
	}
}

impl Tail {
	/// A tail that gives up on a missing group after `grace`.
	pub fn new(grace: Duration) -> Self {
		Self {
			ledger: Ledger::new(grace),
			streams: 0,
			active: 0,
			ended: false,
		}
	}

	/// Record that the publisher ended the subscription.
	pub fn end(&mut self) {
		self.ended = true;
	}

	/// Whether the publisher ended the subscription.
	pub fn is_ended(&self) -> bool {
		self.ended
	}

	/// Change the grace, for a subscription whose max delay changed.
	pub fn set_grace(&mut self, grace: Duration) {
		self.ledger.set_grace(grace);
	}

	/// Record groups as accounted for without a stream: dropped, or sent as a datagram.
	pub fn account(&mut self, groups: Range<u64>, now: Instant) {
		if groups.is_empty() {
			return;
		}
		self.ledger.account(groups, now);
		self.expire(now);
	}

	/// Restart the age of every gap reaching into `groups`, which the demand newly asks for.
	pub fn demand(&mut self, groups: Range<u64>, now: Instant) {
		self.ledger.demand(groups, now);
	}

	/// Fold every gap older than the grace into the runs around it.
	pub fn expire(&mut self, now: Instant) {
		self.ledger.expire(now);
	}

	/// See [`Ledger::covers`].
	pub fn covers(&self, groups: Range<u64>) -> bool {
		self.ledger.covers(groups)
	}

	/// See [`Ledger::folds_at`].
	#[cfg(test)]
	pub fn folds_at(&self, group: u64) -> Option<Instant> {
		self.ledger.folds_at(group, self.ledger.grace())
	}

	/// Data streams received, whether they finished or were reset.
	pub fn streams(&self) -> u64 {
		self.streams
	}
}

/// A data stream whose header arrived, holding its subscription's end open until dropped.
pub(crate) struct Reading {
	tail: Option<kio::Producer<Tail>>,
	active: bool,
}

impl Reading {
	/// Count a data stream, accounting for `group` when it names one (a fill does not).
	pub fn open(tail: &kio::Producer<Tail>, group: Option<u64>, now: Instant) -> Self {
		let Ok(mut state) = tail.write() else {
			return Self {
				tail: None,
				active: false,
			};
		};
		state.streams += 1;
		state.active += 1;
		if let Some(group) = group {
			state.account(group..group.saturating_add(1), now);
		}
		Self {
			tail: Some(tail.clone()),
			active: true,
		}
	}

	/// Stop holding the end open while the stream waits on another one, which holds it
	/// open itself if its header arrived.
	pub fn park(&mut self) {
		self.set_active(false);
	}

	/// Hold the end open again once the stream resumes reading.
	pub fn resume(&mut self) {
		self.set_active(true);
	}

	fn set_active(&mut self, active: bool) {
		if self.active == active {
			return;
		}
		self.active = active;
		if let Some(tail) = &self.tail
			&& let Ok(mut state) = tail.write()
		{
			match active {
				true => state.active += 1,
				false => state.active -= 1,
			}
		}
	}
}

impl Drop for Reading {
	fn drop(&mut self) {
		self.set_active(false);
	}
}

/// Waits out a subscription's tail: until the owed streams are accounted for, or the grace.
pub(crate) struct Settle {
	tail: kio::Consumer<Tail>,
	grace: Deadline,
}

impl Settle {
	/// Start waiting on `tail`, giving up on missing streams after its grace.
	pub fn new(runtime: &crate::time::Deadlines, tail: kio::Consumer<Tail>) -> Self {
		let grace = tail.read().ledger.grace();
		Self {
			tail,
			grace: Deadline::after(runtime, grace),
		}
	}

	/// Ready once no stream is being read and either `complete` holds or the grace
	/// expired, or once the subscription is gone.
	///
	/// `complete` runs whenever no stream is being read, the grace notwithstanding, so a
	/// caller recording what it found sees a stream that finished after the grace.
	pub fn poll(&mut self, waiter: &kio::Waiter, mut complete: impl FnMut(&Tail) -> bool) -> Poll<()> {
		let expired = self.grace.poll(waiter).is_ready();
		self.tail
			.poll(waiter, |tail| match tail.active == 0 && (complete(tail) || expired) {
				true => Poll::Ready(()),
				false => Poll::Pending,
			})
			.map(|_| ())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The grace gives up on streams that never arrived, never on one still being read:
	/// its group, or the END_OF_TRACK it carries, still belongs to the track.
	#[kio::test]
	async fn the_grace_waits_for_a_stream_being_read() {
		let runtime = crate::time::sim();
		let tail = kio::Producer::new(Tail::default());
		let reading = Reading::open(&tail, Some(0), runtime.now());
		let mut settle = Settle::new(&runtime, tail.consume());
		let mut settled = std::pin::pin!(kio::wait(|waiter| settle.poll(waiter, |_| false)));

		kio::clock::sleep(GRACE * 2).await;
		assert!(
			futures::poll!(settled.as_mut()).is_pending(),
			"a stream is still being read"
		);

		drop(reading);
		settled.await;
	}

	/// A stream that outlives the grace still reaches `complete` once it finishes.
	#[kio::test]
	async fn completion_is_judged_after_the_grace() {
		let runtime = crate::time::sim();
		let tail = kio::Producer::new(Tail::default());
		let reading = Reading::open(&tail, Some(0), runtime.now());
		let mut settle = Settle::new(&runtime, tail.consume());
		kio::clock::sleep(GRACE * 2).await;

		drop(reading);
		let mut complete = false;
		kio::wait(|waiter| {
			settle.poll(waiter, |tail| {
				complete = tail.streams() == 1;
				complete
			})
		})
		.await;
		assert!(complete);
	}

	/// A stream parked on another one does not hold the end open.
	#[kio::test]
	async fn a_parked_stream_does_not_hold_the_end() {
		let runtime = crate::time::sim();
		let tail = kio::Producer::new(Tail::default());
		let mut reading = Reading::open(&tail, None, runtime.now());
		reading.park();
		let mut settle = Settle::new(&runtime, tail.consume());
		kio::wait(|waiter| settle.poll(waiter, |tail| tail.streams() == 1)).await;

		reading.resume();
		assert_eq!(tail.read().active, 1);
		drop(reading);
		assert_eq!(tail.read().active, 0);
	}
}
