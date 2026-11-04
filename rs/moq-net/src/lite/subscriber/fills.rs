//! Fills a lite-07 copy asks of its upstream through a subscription rather than FETCH.

use std::{
	collections::BTreeMap,
	ops::Range,
	task::{Poll, ready},
	time::Duration,
};

use rangemap::{RangeMap, RangeSet};

use super::{Establish, SubStream, Tail, TrackServe};
use crate::{
	Error, StreamError, group, lite,
	track::{self, Delay, Order, Position, Subscription},
};

/// What a lite-07 copy's held fills ask of its upstream subscription, beside the demand.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct FillAsk {
	/// The groups to ask for, empty when no fill is held.
	pub groups: RangeSet<u64>,
	/// The most urgent priority a held fill asks with.
	pub priority: i8,
	/// The most tolerant budget a held fill asks with, `None` for the publisher's max age.
	pub max_delay: Option<Duration>,
	pub order: Order,
}

impl Default for FillAsk {
	fn default() -> Self {
		Self {
			groups: Default::default(),
			priority: i8::MIN,
			max_delay: Some(Duration::ZERO),
			order: Order::Ascending,
		}
	}
}

impl FillAsk {
	pub fn is_empty(&self) -> bool {
		self.groups.is_empty()
	}

	/// Count `fill`'s preferences in.
	fn add(&mut self, fill: &track::Fill) {
		self.priority = self.priority.max(fill.priority());
		self.max_delay = match (self.max_delay, fill.max_delay()) {
			(Some(ours), Some(theirs)) => Some(ours.max(theirs)),
			_ => None,
		};
		if fill.order() == Order::Descending {
			self.order = Order::Descending;
		}
	}

	/// The bounded ranges the groups take on the wire, each ending before a group's first
	/// frame, as `wire_ranges` ends an absolute one.
	pub fn wire(&self) -> impl Iterator<Item = lite::Range> + '_ {
		self.groups.iter().map(|groups| lite::Range {
			start: Some(lite::Anchor::Absolute(Position::group(groups.start))),
			end: Some(lite::Anchor::Absolute(Position::group(groups.end))),
		})
	}

	/// `demand` asked with the fills' priority too. The demand's budget caps every range, so
	/// the live feed never loosens for a fill: a group it drops as stale is recovered on its
	/// own (see [`Recovery`]).
	pub fn merge(&self, demand: Subscription) -> Subscription {
		let priority = demand.priority.max(self.priority);
		demand.with_priority(priority)
	}

	/// A subscription asking for the fills alone: no range of its own, so no floor.
	pub fn subscription(&self) -> Subscription {
		Subscription::default()
			.with_ranges([])
			.with_priority(self.priority)
			.with_delay(Delay::default().with_max(self.max_delay))
			.with_order(self.order)
	}
}

/// Fills a lite-07 upstream serves through the subscription rather than FETCH: each held
/// fill's next owed run joins the upstream ranges until its groups arrive or the publisher
/// names them given up, and the fill answers itself from the cache.
///
/// A fill is judged against the subscription that asked for it alone. The track's arrivals
/// still account for a group an earlier subscription cut, which this one may still deliver,
/// so only what this subscription says it will not deliver (a SUBSCRIBE_DROP, or a range
/// start past the range) refuses a fill. A group it drops as stale is officially dropped,
/// and one it already accounted for (delivered before and since evicted here) it never
/// sends again, so a [`Recovery`] asks for either on its own subscription with no max delay,
/// once.
///
/// A group stays asked until its stream finishes, since narrowing the subscription under a
/// stream still arriving would reset it.
#[derive(Default)]
pub(super) struct RangeFills {
	held: Vec<track::Fill>,
	/// What the upstream subscription asks for the fills now.
	pub ask: FillAsk,
	/// Asked groups no fill owes any more whose stream is still being read.
	reading: BTreeMap<u64, group::Consumer>,
	/// Asked groups no fill owes any more, already searched for streams still being read.
	draining: RangeSet<u64>,
	/// Groups the subscription said it will not deliver, and why.
	refused: RangeMap<u64, StreamError>,
	/// Groups dropped as stale, which a [`Recovery`] has yet to ask for.
	recover: RangeSet<u64>,
	/// Groups a [`Recovery`] asks for now, which the subscription leaves out meanwhile.
	recovering: RangeSet<u64>,
	/// Groups already recovered once, which a second stale drop refuses.
	recovered: RangeSet<u64>,
	/// The subscriptions recovering them.
	recoveries: Vec<Recovery>,
}

impl RangeFills {
	/// Whether no fill is held, asked for, or recovering.
	pub fn is_empty(&self) -> bool {
		self.held.is_empty() && self.ask.is_empty() && self.recoveries.is_empty()
	}

	pub fn push(&mut self, fill: track::Fill) {
		self.held.push(fill);
	}

	/// The subscription will not deliver `groups`: a fill owing one is refused with `err`,
	/// or recovered once when it was dropped as stale.
	pub fn refuse(&mut self, groups: Range<u64>, err: StreamError) {
		// Only what a fill asked for: a fill asks for nothing the subscription dropped
		// before it.
		let asked: Vec<_> = self
			.ask
			.groups
			.overlapping(&groups)
			.map(|run| run.start.max(groups.start)..run.end.min(groups.end))
			.collect();
		for groups in asked {
			if err == StreamError::Old {
				for fresh in self.recovered.gaps(&groups).collect::<Vec<_>>() {
					self.recover.insert(fresh);
				}
				// Recovered once already: this drop stands.
				for again in self.recovered.overlapping(&groups).cloned().collect::<Vec<_>>() {
					let again = again.start.max(groups.start)..again.end.min(groups.end);
					self.refused.insert(again, err.clone());
				}
				continue;
			}
			self.refused.insert(groups, err.clone());
		}
	}

	/// A fresh subscription may deliver what the last one refused.
	pub fn forget_refused(&mut self) {
		self.refused = Default::default();
	}

	/// Settle each held fill against what the subscription refused, open a [`Recovery`] for
	/// each run it will not deliver, and recompute what to ask. `accounted` is the tail of
	/// the subscription the fills ride, if one is open, and `read` whether anyone still reads
	/// the track. Returns whether the ask changed.
	pub fn poll(
		&mut self,
		serve: &TrackServe,
		serving: &track::Producer,
		accounted: Option<&kio::Producer<Tail>>,
		read: bool,
		waiter: &kio::Waiter,
	) -> bool {
		// The groups the subscription already accounted for, which it never sends again, are
		// recovered on their own, once.
		if let Some(accounted) = accounted {
			let tail = accounted.read();
			let mut asked = RangeSet::new();
			for fill in &self.held {
				if let Some(run) = fill.next_run() {
					asked.insert(run);
				}
			}
			for run in asked.iter() {
				for gone in tail.accounted(run.clone()) {
					for gap in self.recovering.gaps(&gone).collect::<Vec<_>>() {
						for again in self.recovered.overlapping(&gap).cloned().collect::<Vec<_>>() {
							let again = again.start.max(gap.start)..again.end.min(gap.end);
							if !self.refused.contains_key(&again.start) {
								self.refused.insert(again, StreamError::NotFound);
							}
						}
						for fresh in self.recovered.gaps(&gap).collect::<Vec<_>>() {
							if !self.refused.contains_key(&fresh.start) {
								self.recover.insert(fresh);
							}
						}
					}
				}
			}
		}
		// Each stale run goes out on its own subscription with no max delay, once.
		for run in std::mem::take(&mut self.recover).iter() {
			self.recovered.insert(run.clone());
			self.recovering.insert(run.clone());
			self.recoveries
				.push(Recovery::new(serve, serving, run.clone(), &self.ask));
		}
		// What a finished recovery did not deliver stands refused.
		let mut done = Vec::new();
		self.recoveries.retain_mut(|recovery| match recovery.poll(waiter) {
			Poll::Ready(refused) => {
				done.push((recovery.groups.clone(), refused));
				false
			}
			Poll::Pending => true,
		});
		for (groups, refused) in done {
			self.recovering.remove(groups);
			for (groups, err) in refused.iter() {
				self.refused.insert(groups.clone(), err.clone());
			}
		}

		let mut ask = FillAsk::default();
		// Every run a held fill still owes, recovered or not.
		let mut owed = RangeSet::new();
		let refused = &self.refused;
		let recovering = &self.recovering;
		self.held.retain_mut(|fill| {
			if fill.poll_withdrawn(waiter).is_ready() {
				return false;
			}
			// Arms every input of the owed groups: readers, answers, and the cache.
			let _ = fill.poll_wanted_changed(waiter);
			loop {
				let Some(run) = fill.next_run() else {
					return false;
				};
				// `next_run` extends from the group owed first, which the subscription may have
				// refused already. Refusing the run answers it, so the loop moves on.
				let first = match fill.order() {
					Order::Ascending => run.start,
					Order::Descending => run.end - 1,
				};
				if let Some((groups, err)) = refused.get_key_value(&first) {
					let groups = run.start.max(groups.start)..run.end.min(groups.end);
					refuse_fill(fill, groups, err.clone());
					continue;
				}
				owed.insert(run.clone());
				// A run being recovered waits on its own subscription.
				for gap in recovering.gaps(&run) {
					ask.groups.insert(gap);
				}
				ask.add(fill);
				return true;
			}
		});

		// A recovery no held fill owes any group of is cancelled, once none of its streams is
		// still being read or nobody reads the track, and a group is recovered once for the
		// fills owing it now: a later request for it may recover it again.
		self.recoveries
			.retain(|recovery| owed.overlaps(&recovery.groups) || (read && recovery.reading()));
		let still: RangeSet<u64> = self.recoveries.iter().map(|recovery| recovery.groups.clone()).collect();
		self.recovering = still;
		let recovered: Vec<_> = self.recovered.iter().cloned().collect();
		self.recovered = RangeSet::new();
		for run in recovered {
			for kept in owed.overlapping(&run) {
				self.recovered.insert(kept.start.max(run.start)..kept.end.min(run.end));
			}
		}

		// A run stays asked whole while a fill still owes any of it, so the subscription
		// changes once as a run is asked and once as it is done, and the streams of its
		// answered groups keep arriving. One no fill owes stays while any of its streams is
		// still being read, each group searched for once.
		self.reading.retain(|_, group| group.poll_finished(waiter).is_pending());
		let edge = serving.consume();
		let mut groups = ask.groups.clone();
		let mut draining = RangeSet::new();
		for run in self.ask.groups.iter() {
			if ask.groups.overlaps(run) {
				groups.insert(run.clone());
				continue;
			}
			// Only the groups the cache holds, so a wide run costs what it holds.
			for gap in self.draining.gaps(run) {
				for sequence in edge.cached_groups(gap) {
					if let Some(mut group) = edge.peek_group(sequence)
						&& group.poll_finished(waiter).is_pending()
					{
						self.reading.insert(sequence, group);
					}
				}
			}
			if self.reading.range(run.clone()).next().is_some() {
				groups.insert(run.clone());
				draining.insert(run.clone());
			}
		}
		self.reading.retain(|sequence, _| draining.contains(sequence));
		self.draining = draining;
		// What finishes a stream still asks with what its fills asked.
		if self.held.is_empty() && !groups.is_empty() {
			ask.priority = self.ask.priority;
			ask.max_delay = self.ask.max_delay;
			ask.order = self.ask.order;
		}
		ask.groups = groups;
		if ask == self.ask {
			return false;
		}
		self.ask = ask;
		true
	}
}

/// Answer `groups` of `fill` as the subscription that asked for them will not deliver them,
/// with why: a miss names them dropped, as a FETCH's miss does.
fn refuse_fill(fill: &track::Fill, groups: Range<u64>, err: StreamError) {
	match err {
		StreamError::NotFound => fill.drop_groups(groups),
		StreamError::Old => fill.reject_groups(groups, Error::Old),
		err => fill.refuse_groups(groups, Error::Stream(err)),
	}
}

/// A subscription asking again, with no max delay, for groups another one dropped as stale,
/// which a fill still owes. It lives until each group arrives and its stream is read, or the
/// publisher gives it up, then FINs and waits for the publisher's FIN in reply.
struct Recovery {
	serve: TrackServe,
	id: u64,
	groups: Range<u64>,
	state: RecoveryState,
	/// The groups this subscription said it will not deliver, and why.
	refused: RangeMap<u64, StreamError>,
	/// Whether our FIN went out.
	finished: bool,
}

enum RecoveryState {
	Establish(Box<Establish>),
	Active(Box<SubStream>),
	/// The subscription failed to open or was reset: everything left is refused.
	Done,
}

impl Recovery {
	fn new(serve: &TrackServe, serving: &track::Producer, groups: Range<u64>, ask: &FillAsk) -> Self {
		let subscription = Subscription::default()
			.with_ranges([track::Range::groups(groups.clone())])
			.with_priority(ask.priority)
			.with_delay(Delay::default().with_max(None))
			.with_order(Order::Ascending);
		let ranges = vec![lite::Range {
			start: Some(lite::Anchor::Absolute(Position::group(groups.start))),
			end: Some(lite::Anchor::Absolute(Position::group(groups.end))),
		}];
		let copy = super::Accepted {
			producer: serving.clone(),
			framing: lite::Framing::new(serving.info(), serve.subscriber.version),
		};
		tracing::debug!(track = %serve.name, ?groups, "recovering stale groups");
		let establish = serve.register(subscription, ranges, Some(copy));
		Self {
			serve: serve.clone(),
			id: establish.id,
			groups,
			state: RecoveryState::Establish(Box::new(establish)),
			refused: Default::default(),
			finished: false,
		}
	}

	/// Whether a stream of the subscription is still being read, which cancelling would cut.
	fn reading(&self) -> bool {
		match &self.state {
			RecoveryState::Active(active) => active.tail.read().is_reading(),
			_ => false,
		}
	}

	/// End the subscription, refusing what is left of the groups with `err`, as nothing more
	/// will arrive on it.
	fn end(&mut self, err: StreamError) {
		let state = std::mem::replace(&mut self.state, RecoveryState::Done);
		let mut owed = RangeSet::new();
		owed.insert(self.groups.clone());
		if let RecoveryState::Active(active) = &state {
			for arrived in active.tail.read().accounted(self.groups.clone()) {
				owed.remove(arrived);
			}
		}
		for (refused, _) in self.refused.iter() {
			owed.remove(refused.clone());
		}
		for gap in owed.iter() {
			self.refused.insert(gap.clone(), err.clone());
		}
		self.serve.subscriber.remove_subscribe(self.id);
		match state {
			RecoveryState::Establish(est) => est.abort(&Error::Cancel),
			RecoveryState::Active(active) if !self.finished => active.stream.writer.abort(&Error::Cancel),
			_ => {}
		}
	}

	/// Ready once the recovery is over, with the groups it refused.
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<RangeMap<u64, StreamError>> {
		loop {
			match &mut self.state {
				RecoveryState::Establish(est) => match ready!(est.poll(waiter)) {
					Ok(active) => self.state = RecoveryState::Active(Box::new(active)),
					Err(err) => {
						tracing::debug!(%err, "recovery subscribe failed");
						self.end(StreamError::Cancel);
					}
				},
				RecoveryState::Active(active) => {
					// FIN once every group is accounted for and read, as a subscriber does once
					// its tail settles, and the publisher FINs in reply.
					if !self.finished {
						let groups = self.groups.clone();
						let settled = active
							.tail
							.consume()
							.poll(waiter, |tail| match tail.covers(groups.clone()) && !tail.is_reading() {
								true => Poll::Ready(()),
								false => Poll::Pending,
							})
							.is_ready();
						if settled {
							let _ = active.stream.writer.finish();
							self.finished = true;
						}
					}
					if let Poll::Ready(Err(err)) = active.stream.writer.poll_flush(waiter) {
						tracing::debug!(%err, "recovery subscribe write failed");
					}
					let res = ready!(
						active
							.stream
							.reader
							.poll_decode_maybe::<lite::SubscribeResponse>(waiter)
					);
					let now = self.serve.subscriber.runtime.now();
					let version = self.serve.subscriber.version;
					let groups = self.groups.clone();
					// What the publisher gives up of the groups, accounted for in the tail.
					let mut given_up = |given: Range<u64>, err: StreamError| {
						if let Ok(mut tail) = active.tail.write() {
							tail.account(given.clone(), now);
						}
						let given = given.start.max(groups.start)..given.end.min(groups.end);
						if !given.is_empty() {
							self.refused.insert(given, err);
						}
					};
					match res {
						Ok(Some(lite::SubscribeResponse::Start(start))) => {
							// Below its start the publisher delivers nothing.
							let from = start.starts.first().copied().flatten().unwrap_or(groups.end);
							given_up(
								groups.start..from.clamp(groups.start, groups.end),
								StreamError::NotFound,
							);
						}
						Ok(Some(lite::SubscribeResponse::Drop(dropped))) => {
							given_up(dropped.start..dropped.end.saturating_add(1), dropped.error(version));
						}
						// Nothing at or past the track's end will be produced.
						Ok(Some(lite::SubscribeResponse::Finish(finish))) => {
							given_up(finish.group.max(groups.start)..groups.end, StreamError::NotFound);
						}
						Ok(Some(msg)) => {
							tracing::warn!(?msg, "unexpected recovery subscribe response");
							self.end(StreamError::Cancel);
						}
						Ok(None) => self.end(StreamError::NotFound),
						Err(err) => {
							tracing::debug!(%err, "recovery subscribe ended");
							self.end(StreamError::Cancel);
						}
					}
				}
				RecoveryState::Done => return Poll::Ready(std::mem::take(&mut self.refused)),
			}
		}
	}
}

impl Drop for Recovery {
	fn drop(&mut self) {
		if !matches!(self.state, RecoveryState::Done) {
			self.end(StreamError::Cancel);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A stale drop of groups a fill asked for is recovered once; a second one stands, and a
	/// drop of groups no fill asked for refuses nothing.
	#[test]
	#[expect(clippy::single_range_in_vec_init, reason = "the recovered runs are a list, here one")]
	fn a_stale_drop_is_recovered_once() {
		let mut fills = RangeFills::default();
		fills.ask.groups.insert(2..6);
		fills.refuse(3..5, StreamError::Old);
		assert_eq!(fills.recover.iter().cloned().collect::<Vec<_>>(), [3..5]);
		assert!(fills.refused.is_empty(), "recovered, not refused");

		// Once recovered, the same groups dropped stale again stay dropped.
		fills.recovered.insert(3..5);
		fills.recover = Default::default();
		fills.refuse(2..5, StreamError::Old);
		assert_eq!(fills.recover.iter().cloned().collect::<Vec<_>>(), [2..3]);
		assert_eq!(fills.refused.get(&3), Some(&StreamError::Old));

		fills.refuse(7..9, StreamError::NotFound);
		assert_eq!(fills.refused.get(&7), None, "no fill asked for it");
		fills.refuse(5..6, StreamError::NotFound);
		assert_eq!(fills.refused.get(&5), Some(&StreamError::NotFound));
	}
}
