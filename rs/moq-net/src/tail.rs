//! A subscription's tail: the group streams still owed once the publisher has ended it.
//!
//! A publisher ends a subscription only after every group stream it opened has finished,
//! but QUIC does not order streams, so one opened before the end can still reach us after
//! it. The subscriber keeps the subscription routable until each owed group is accounted
//! for, or a grace expires for a group whose stream was reset before its header arrived.
//! A lite-07 publisher names every start: a group's GROUP_END proves its span, SUBSCRIBE_SKIP
//! names any other start it declared empty, a reset delivers the header (reliable reset) or
//! names the group in a drop, and SUBSCRIBE_END names every start past the last group. So a lite-07 tail waits for no
//! grace: it settles once everything owed is accounted for.
//!
//! A stream whose header arrived is read to its end whatever the grace says: a group ends
//! on its own stream's FIN or reset, never because its track ended. A lost datagram is not
//! owed: it leaves a hole that waits out the grace like a stream reset before its header.

use std::{ops::Range, task::Poll, time::Duration};

pub(crate) use crate::ledger::{GRACE, grace};
use crate::{
	cache,
	ledger::{Ledger, RUN_OVERHEAD},
	time::{Deadline, Instant},
	track,
};

/// Heap a lost group holds until an update reports it: its entry, doubled for half-full
/// `BTreeSet` nodes, as [`RUN_OVERHEAD`] is.
const LOST_OVERHEAD: u64 = 2 * size_of::<u64>() as u64;

/// The data streams a subscription has received, and the groups they account for.
#[derive(Debug)]
pub(crate) struct Tail {
	ledger: Ledger,
	// What the ledger's gaps bill to the copy's account, once it has one. A publisher's drops
	// and skips name any range, so under pool pressure the oldest gaps fold early rather
	// than grow with a grace that never passes.
	bill: Option<cache::Reader>,
	streams: u64,
	// The lowest group a stream's header named, which proves the group exists.
	first: Option<u64>,
	// Streams whose header arrived and that are still being read.
	active: u64,
	// The publisher's FIN ended the track, its only end before lite-05, so a session that
	// dies before the tail settles declares that end before aborting.
	ended: bool,
	// Lite-07: the publisher names every start, so no gap ages out.
	proven: bool,
	// Lite-07: groups whose stream was discarded before its end, which leaves the rest of the
	// span its GROUP_END told unnamed, for a SUBSCRIBE_UPDATE to ask the publisher to name again.
	// Billed with the ledger, since a publisher can keep resetting streams while the update
	// waits on flow control.
	lost: std::collections::BTreeSet<u64>,
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
			bill: None,
			streams: 0,
			first: None,
			active: 0,
			ended: false,
			proven: false,
			lost: Default::default(),
		}
	}

	/// A lite-07 tail, whose publisher names every start: no gap ages out, whatever max delay
	/// its subscription asks, so it settles only once everything owed is accounted for.
	pub fn proven() -> Self {
		Self {
			proven: true,
			..Self::new(Duration::MAX)
		}
	}

	/// Record that the publisher's FIN ended the track.
	pub fn end(&mut self) {
		self.ended = true;
	}

	/// Whether the publisher's FIN ended the track.
	pub fn is_ended(&self) -> bool {
		self.ended
	}

	/// Change the grace, for a subscription whose max delay changed. A proven tail keeps none.
	pub fn set_grace(&mut self, grace: Duration) {
		if !self.proven {
			self.ledger.set_grace(grace);
		}
	}

	/// Follow the copy `track`: wait on its max age and `budget`, and bill the ledger to its
	/// account. Call it with no lock on `track`'s state held, as [`Self::account`] and
	/// [`Self::expire`] must be: billing the ledger can take that lock to settle the debt.
	pub fn follow(&mut self, track: &track::Producer, budget: Duration) {
		self.set_grace(track.tail_grace(budget));
		if self.bill.is_none() {
			self.bill = Some(track.tail_bill());
			self.rebill();
		}
	}

	/// Bill the ledger's gaps, then pay what the pool is owed from them: fold the oldest gaps
	/// early, giving up on them as the grace passing would, until the debt is paid or none is
	/// left. The run they split is the subscription's own, so an in-order feed bills nothing.
	fn rebill(&mut self) {
		let Some(bill) = &mut self.bill else {
			return;
		};
		let lost = self.lost.len() as u64 * LOST_OVERHEAD;
		let bytes = |ledger: &Ledger| ledger.len().saturating_sub(1) as u64 * RUN_OVERHEAD + lost;
		bill.set(bytes(&self.ledger));
		while bill.debt() > 0 && self.ledger.fold_oldest().is_some() {
			bill.set(bytes(&self.ledger));
		}
	}

	/// Record groups as accounted for without a stream: dropped, or sent as a datagram.
	pub fn account(&mut self, groups: Range<u64>, now: Instant) {
		if groups.is_empty() {
			return;
		}
		self.ledger.account(groups, now);
		self.expire(now);
	}

	/// Account every gap of a proven tail that no range of `owed` reaches, as when a range
	/// retires: its publisher names nothing outside its live ranges, so with no grace to fold
	/// them, those gaps would otherwise outlive the ranges that left them.
	pub fn retire(&mut self, owed: &rangemap::RangeSet<u64>, now: Instant) {
		if self.proven {
			self.ledger.retire(owed, now);
			self.rebill();
		}
	}

	/// Restart the age of every gap reaching into `groups`, which the demand newly asks for.
	pub fn demand(&mut self, groups: Range<u64>, now: Instant) {
		self.ledger.demand(groups, now);
	}

	/// Fold every gap older than the grace into the runs around it.
	pub fn expire(&mut self, now: Instant) {
		self.ledger.expire(now);
		self.rebill();
	}

	/// See [`Ledger::covers`].
	pub fn covers(&self, groups: Range<u64>) -> bool {
		self.ledger.covers(groups)
	}

	/// See [`Ledger::accounted`].
	pub fn accounted(&self, groups: Range<u64>) -> impl Iterator<Item = Range<u64>> + '_ {
		self.ledger.accounted(groups)
	}

	/// See [`Ledger::folds_at`].
	#[cfg(test)]
	pub fn folds_at(&self, group: u64) -> Option<Instant> {
		self.ledger.folds_at(group, self.ledger.grace())
	}

	/// Whether a discarded stream left a group for the publisher to name the rest of again.
	pub fn has_lost(&self) -> bool {
		!self.lost.is_empty()
	}

	/// Take the groups a SUBSCRIBE_UPDATE asks the publisher to name the rest of again.
	pub fn take_lost(&mut self) -> Vec<u64> {
		let lost = std::mem::take(&mut self.lost).into_iter().collect();
		self.rebill();
		lost
	}

	/// Record that the stream of `group` was discarded before its end. A proven tail asks the
	/// publisher to name the rest of its span again: only the publisher knows where it ends, as
	/// the next start being accounted for proves nothing when a skip or drop named it.
	fn lose(&mut self, group: u64) {
		if self.proven && self.lost.insert(group) {
			self.rebill();
		}
	}

	/// Whether a data stream whose header arrived is still being read.
	pub fn is_reading(&self) -> bool {
		self.active > 0
	}

	/// Data streams received, whether they finished or were reset.
	pub fn streams(&self) -> u64 {
		self.streams
	}

	/// The lowest group whose stream's header arrived.
	pub fn first_group(&self) -> Option<u64> {
		self.first
	}
}

/// A data stream whose header arrived, holding its subscription's end open until dropped.
pub(crate) struct Reading {
	tail: Option<kio::Producer<Tail>>,
	active: bool,
	/// The group the stream's header named, until its end is read.
	unread: Option<u64>,
}

impl Reading {
	/// Count a data stream, accounting for `group` when it names one (a fill does not).
	pub fn open(tail: &kio::Producer<Tail>, group: Option<u64>, now: Instant) -> Self {
		let Ok(mut state) = tail.write() else {
			return Self {
				tail: None,
				active: false,
				unread: None,
			};
		};
		state.streams += 1;
		state.active += 1;
		if let Some(group) = group {
			state.first = Some(state.first.map_or(group, |first| first.min(group)));
			state.account(group..group.saturating_add(1), now);
		}
		Self {
			tail: Some(tail.clone()),
			active: true,
			unread: group,
		}
	}

	/// Record that the stream's end was read, or its span accounted for another way: dropped
	/// before then, the stream asks its publisher to name the rest of its span again.
	pub fn finish(&mut self) {
		self.unread = None;
	}

	/// Record `groups` as accounted for by this stream, such as the span its GROUP_END closed.
	pub fn account(&self, groups: Range<u64>, now: Instant) {
		if let Some(tail) = &self.tail
			&& let Ok(mut state) = tail.write()
		{
			state.account(groups, now);
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
		if let Some(group) = self.unread
			&& let Some(tail) = &self.tail
			&& let Ok(mut state) = tail.write()
		{
			state.lose(group);
		}
	}
}

/// Waits out a subscription's tail: until the owed streams are accounted for, or the grace.
pub(crate) struct Settle {
	runtime: crate::time::Deadlines,
	tail: kio::Consumer<Tail>,
	grace: Deadline,
}

impl Settle {
	/// Start waiting on `tail`, giving up on missing streams after its grace.
	pub fn new(runtime: &crate::time::Deadlines, tail: kio::Consumer<Tail>) -> Self {
		let grace = tail.read().ledger.grace();
		Self {
			runtime: runtime.clone(),
			tail,
			grace: Deadline::after(runtime, grace),
		}
	}

	/// Give up on missing streams after `grace` at the latest.
	pub fn at_most(mut self, grace: Duration) -> Self {
		let limit = self.runtime.now().checked_add(grace);
		if let Some(limit) = limit
			&& self.grace.deadline().is_none_or(|at| limit < at)
		{
			self.grace.set(Some(limit));
		}
		self
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

	/// A proven tail waits for what it owes however long that takes, and settles once it has it.
	#[kio::test]
	async fn a_proven_tail_waits_out_no_grace() {
		let runtime = crate::time::sim();
		let tail = kio::Producer::new(Tail::proven());
		if let Ok(mut tail) = tail.write() {
			tail.set_grace(GRACE);
			tail.account(0..1, runtime.now());
		}
		let mut settle = Settle::new(&runtime, tail.consume());
		let mut settled = std::pin::pin!(kio::wait(|waiter| settle.poll(waiter, |tail| tail.covers(0..3))));

		kio::clock::sleep(GRACE * 100).await;
		if let Ok(mut tail) = tail.write() {
			tail.expire(runtime.now());
		}
		assert!(futures::poll!(settled.as_mut()).is_pending(), "1 and 2 are still owed");

		if let Ok(mut tail) = tail.write() {
			tail.account(1..3, runtime.now());
		}
		settled.await;
	}

	/// A proven tail keeps a run only per gap a live range still owes: the gaps a retired range
	/// left fold into their neighbors, so moving a range keeps the ledger bounded.
	#[kio::test]
	async fn a_proven_tail_folds_the_gaps_no_live_range_owes() {
		let runtime = crate::time::sim();
		let mut tail = Tail::proven();
		let now = runtime.now();
		// A fixed range over 0..10 owes 5, and a second range moved through 20..22 and 30..32.
		for groups in [0..5, 6..10, 20..22, 30..32] {
			tail.account(groups, now);
		}
		let mut owed = rangemap::RangeSet::new();
		owed.insert(0..10);
		owed.insert(30..u64::MAX);
		tail.retire(&owed, now);
		assert!(tail.accounted(0..40).eq([0..5, 6..32]), "5 is still owed");
		assert!(!tail.covers(5..6));

		// A tail with a grace folds its gaps when the grace passes instead.
		let mut graced = Tail::new(GRACE);
		graced.account(0..1, now);
		graced.account(2..3, now);
		graced.retire(&rangemap::RangeSet::new(), now);
		assert!(!graced.covers(1..2));
	}

	/// A proven tail reports a stream discarded before its end even when the next start is
	/// accounted for: a skip naming it says nothing about where the group's span ends.
	#[kio::test]
	async fn a_stream_lost_before_its_end_is_reported_past_a_skipped_start() {
		let runtime = crate::time::sim();
		let tail = kio::Producer::new(Tail::proven());
		let reading = Reading::open(&tail, Some(0), runtime.now());
		// Start 1 was skipped while group 0 was open, which then finished at 3.
		if let Ok(mut tail) = tail.write() {
			tail.account(1..2, runtime.now());
		}
		drop(reading);
		drop(Reading::open(&tail, Some(0), runtime.now()));
		assert_eq!(tail.write().ok().unwrap().take_lost(), [0], "reported once");

		// A tail with a grace reports none: the grace gives up on what it owes.
		let graced = kio::Producer::new(Tail::new(GRACE));
		drop(Reading::open(&graced, Some(0), runtime.now()));
		assert!(!graced.read().has_lost());
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

	/// A copy with no max age and a subscription with no max delay never ages a gap out, so a
	/// publisher's alternating single-group drops bill the pool instead, and the oldest gaps
	/// fold once it is short of room.
	#[kio::test]
	async fn drops_are_billed_and_fold_under_pressure() {
		const DROPS: u64 = 1000;
		let runtime = crate::time::sim();
		let capacity = 64 * RUN_OVERHEAD;
		let pool = cache::Pool::new(cache::Config::default().with_capacity(capacity));
		let broadcast = crate::broadcast::Info {
			pool: pool.clone(),
			..Default::default()
		};
		let track = track::Producer::new(std::sync::Arc::new(broadcast), "test", track::Info::timed());
		let base = pool.used();

		let mut tail = Tail::default();
		tail.follow(&track, Duration::MAX);
		assert_eq!(tail.ledger.grace(), Duration::MAX, "nothing ever ages a gap out");
		let now = runtime.now();
		for drop in 0..DROPS {
			tail.account(2 * drop..2 * drop + 1, now);
		}
		assert!(tail.ledger.len() < DROPS as usize, "the oldest gaps folded");
		assert!(
			pool.used() <= base + capacity + 2 * RUN_OVERHEAD,
			"the ledger stayed within the pool: {}",
			pool.used()
		);
		assert_eq!(
			pool.used(),
			base + (tail.ledger.len() - 1) as u64 * RUN_OVERHEAD,
			"what is left is billed"
		);

		drop(tail);
		assert_eq!(pool.used(), base, "the bill went with the tail");
	}

	/// Without pressure nothing folds early: the pool bills the gaps and keeps them.
	#[kio::test]
	async fn drops_are_kept_while_the_pool_has_room() {
		let runtime = crate::time::sim();
		let pool = cache::Pool::unbounded();
		let broadcast = crate::broadcast::Info {
			pool: pool.clone(),
			..Default::default()
		};
		let track = track::Producer::new(std::sync::Arc::new(broadcast), "test", track::Info::timed());
		let base = pool.used();

		let mut tail = Tail::default();
		tail.follow(&track, Duration::MAX);
		for drop in 0..100 {
			tail.account(2 * drop..2 * drop + 1, runtime.now());
		}
		assert_eq!(tail.ledger.len(), 100);
		assert_eq!(pool.used(), base + 99 * RUN_OVERHEAD);
	}
}
