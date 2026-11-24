//! Fills a copy asks of its upstream through a subscription rather than FETCH: lite-07's
//! own, or before it, one of its own per run.

use std::{
	collections::BTreeMap,
	ops::Range,
	task::{Poll, ready},
};

use rangemap::{RangeMap, RangeSet};

use super::{Establish, Settle, SubStream, Tail, TrackServe};
use crate::{
	Error, StreamError,
	coding::Recv,
	group, lite,
	track::{self, Order, Position, Subscription},
};

/// What a lite-07 copy's held fills ask of its upstream subscription, beside the demand.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct FillAsk {
	/// The groups to ask for, empty when no fill is held.
	pub groups: RangeSet<u64>,
	/// The most urgent priority a held fill asks with.
	pub priority: i8,
	pub order: Order,
}

impl Default for FillAsk {
	fn default() -> Self {
		Self {
			groups: Default::default(),
			priority: i8::MIN,
			order: Order::Ascending,
		}
	}
}

impl FillAsk {
	pub fn is_empty(&self) -> bool {
		self.groups.is_empty()
	}

	/// Count `fill`'s preferences in.
	fn add(&mut self, fill: &track::FillGroups) {
		self.priority = self.priority.max(fill.priority());
		if fill.order() == Order::Descending {
			self.order = Order::Descending;
		}
	}

	/// The bounded ranges `groups` take on the wire, each ending before a group's first
	/// frame, as `wire_ranges` ends an absolute one.
	pub fn wire(groups: &RangeSet<u64>) -> impl Iterator<Item = lite::Range> + '_ {
		groups.iter().map(|groups| lite::Range {
			start: Some(lite::Anchor::Absolute(Position::group(groups.start))),
			end: Some(lite::Anchor::Absolute(Position::group(groups.end))),
		})
	}

	/// `demand` asked with the fills' priority too. A fill's range is its own, so the demand's
	/// live joins keep their budgets and the live feed never loosens for a fill: a group it
	/// drops as stale is recovered on its own (see [`Recovery`]).
	pub fn merge(&self, demand: Subscription) -> Subscription {
		let priority = demand.priority.max(self.priority);
		demand.with_priority(priority)
	}

	/// A subscription asking for the fills alone: no range of its own, so no floor.
	pub fn subscription(&self) -> Subscription {
		Subscription::default()
			.with_ranges([])
			.with_priority(self.priority)
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
/// start past the range) refuses a fill. A range start speaks only for its range: once the
/// range retires, a later one asking for the same groups may start lower. A group it drops as stale is officially dropped,
/// and one it already accounted for (delivered before and since evicted here) it never
/// sends again, so a [`Recovery`] asks for either on its own subscription with no max delay,
/// once. Nothing newer judges it stale there, so once every hop forgot a run, a narrow reader
/// takes its stale groups.
///
/// A group stays asked until its stream finishes, since narrowing the subscription under a
/// stream still arriving would reset it.
///
/// Before lite-07 a subscription carries one range, so each run goes out on a [`Recovery`]
/// of its own instead. Its publisher names no keys between two group starts (no GROUP_END
/// or SUBSCRIBE_SKIP), and the keys of a timed track are its media time, so FETCHing a run
/// key by key would cost a round trip per microsecond. A SUBSCRIBE from the run's first key
/// answers in one round trip instead: its start is the first group at or past that key, and
/// nothing below it is delivered.
#[derive(Default)]
pub(super) struct RangeFills {
	/// Whether each run goes out on its own subscription, before lite-07.
	per_run: bool,
	held: Vec<track::FillGroups>,
	/// What the upstream subscription asks for the fills now.
	pub ask: FillAsk,
	/// Asked groups no fill owes any more whose stream is still being read.
	reading: BTreeMap<u64, group::Consumer>,
	/// Asked groups no fill owes any more, already searched for streams still being read.
	draining: RangeSet<u64>,
	/// Groups the subscription said it will not deliver, and why.
	refused: RangeMap<u64, Refusal>,
	/// Groups below where a fill's range starts, by the asked run that range carries, which
	/// stand only while that range is live.
	skipped: RangeMap<u64, Range<u64>>,
	/// Groups dropped as stale, which a [`Recovery`] has yet to ask for.
	recover: RangeSet<u64>,
	/// Groups a [`Recovery`] asks for now, which the subscription leaves out meanwhile.
	recovering: RangeSet<u64>,
	/// Groups already recovered once, which a second stale drop refuses.
	recovered: RangeSet<u64>,
	/// The subscriptions recovering them.
	recoveries: Vec<Recovery>,
	/// TEMPORARY MIGRATION, see [`Recovery::walk`]: fills handed back to FETCH, one key at a time.
	walking: Vec<track::FillGroups>,
}

impl RangeFills {
	/// The fills of a copy whose upstream speaks `version`.
	pub fn new(version: lite::Version) -> Self {
		Self {
			per_run: !version.has_ranges(),
			..Default::default()
		}
	}

	/// The fills of a copy whose subscription is over, each run on a subscription of its own,
	/// which any version carries.
	pub fn per_run() -> Self {
		Self {
			per_run: true,
			..Default::default()
		}
	}

	/// Take every held fill, which outlives the subscription that carried it.
	pub fn take_held(&mut self) -> Vec<track::FillGroups> {
		std::mem::take(&mut self.held)
	}

	/// TEMPORARY MIGRATION, see [`Recovery::walk`]: take the fills a stalled recovery handed back,
	/// which FETCH walks a key at a time.
	pub fn take_walks(&mut self) -> Vec<track::FillGroups> {
		std::mem::take(&mut self.walking)
	}

	/// Whether no fill is held, asked for, or recovering.
	pub fn is_empty(&self) -> bool {
		self.held.is_empty() && self.ask.is_empty() && self.recoveries.is_empty()
	}

	pub fn push(&mut self, fill: track::FillGroups) {
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
				// Recovered once already: this drop stands for the fills owing it now, except where
				// a recovery still out asks for it, whose answer it waits on.
				for again in self.recovered.overlapping(&groups).cloned().collect::<Vec<_>>() {
					let again = again.start.max(groups.start)..again.end.min(groups.end);
					for stands in self.recovering.gaps(&again).collect::<Vec<_>>() {
						self.refused.insert(stands, Refusal::dropped(err.clone()));
					}
				}
				continue;
			}
			self.refused.insert(groups, Refusal::dropped(err.clone()));
		}
	}

	/// The subscription's range for the asked run `run` starts past `groups`, which it will not
	/// deliver while that range is live.
	pub fn skip(&mut self, run: Range<u64>, groups: Range<u64>) {
		if !groups.is_empty() {
			self.skipped.insert(groups, run);
		}
	}

	/// A fresh subscription may deliver what the last one refused.
	pub fn forget_refused(&mut self) {
		self.refused = Default::default();
		self.skipped = Default::default();
	}

	/// What the recoveries still out cover: each its groups less what it refused and handed
	/// over.
	fn cover(&mut self) {
		let mut still = RangeSet::new();
		for recovery in &self.recoveries {
			for gap in recovery.released.gaps(&recovery.groups) {
				still.insert(gap);
			}
		}
		self.recovering = still;
	}

	/// Take the refusals a recovery made since the last poll, once: each answers the fills
	/// owing its groups now, and [`Self::forget_answered`] ends it with them, so a later request
	/// for the same groups asks again rather than reading it back.
	/// The groups taken leave the recovery's coverage in `released`.
	fn take_refused(&mut self, fresh: &mut RangeMap<u64, Refusal>, released: &mut RangeSet<u64>) {
		for (groups, refusal) in std::mem::take(fresh) {
			released.insert(groups.clone());
			self.recovering.remove(groups.clone());
			self.refused.insert(groups, refusal);
		}
	}

	/// A refusal answers only the fills owing it now: once no held fill covers its groups, a
	/// later request asks again, and a loss it named stays recorded on the track alone.
	fn forget_answered(&mut self) {
		// Trimmed to the groups a held fill still covers.
		let mut covered = RangeSet::new();
		for fill in &self.held {
			let groups = fill.start().group..fill.end();
			if !groups.is_empty() {
				covered.insert(groups);
			}
		}
		let span = self
			.refused
			.iter()
			.map(|(groups, _)| groups.clone())
			.reduce(|a, b| a.start.min(b.start)..a.end.max(b.end));
		let Some(span) = span else {
			return;
		};
		for gap in covered.gaps(&span).collect::<Vec<_>>() {
			self.refused.remove(gap);
		}
	}

	/// Record `groups` recovered once already.
	#[cfg(test)]
	pub fn recovered_once(&mut self, groups: Range<u64>) {
		self.recovered.insert(groups);
	}

	/// Record `groups` answered skipped by a recovery.
	#[cfg(test)]
	pub fn refused_skipped(&mut self, groups: Range<u64>) {
		self.refused.insert(groups, Refusal::skipped());
	}

	/// Whether a [`Recovery`] is still out, whose groups the subscription's tail does not wait
	/// for.
	pub fn recovering(&self) -> bool {
		!self.recoveries.is_empty()
	}

	/// The keys of `groups` neither the subscription said it will not deliver nor the
	/// subscriber's own range starts past: a refusal answers only its own keys, so a group
	/// past a skipped run is still owed.
	fn unrefused(&self, groups: Range<u64>) -> Vec<Range<u64>> {
		let mut owed = RangeSet::new();
		owed.insert(groups.clone());
		for (refused, _) in self.refused.overlapping(&groups) {
			owed.remove(refused.clone());
		}
		for (skipped, _) in self.skipped.overlapping(&groups) {
			owed.remove(skipped.clone());
		}
		owed.iter().cloned().collect()
	}

	/// Why the subscription will not deliver the run of groups from `first`, if it said so.
	fn refusal(&self, first: u64) -> Option<(Range<u64>, Refusal)> {
		if let Some((groups, refusal)) = self.refused.get_key_value(&first) {
			return Some((groups.clone(), refusal.clone()));
		}
		// The subscriber's own range starting past them says nothing of the source.
		let (groups, _) = self.skipped.get_key_value(&first)?;
		Some((groups.clone(), Refusal::passing(StreamError::NotFound)))
	}

	/// Settle each held fill against what the subscription refused, open a [`Recovery`] for
	/// each run it will not deliver, and recompute what to ask. `accounted` is the tail of
	/// the subscription the fills ride, if one is open, and `read` whether anyone still reads
	/// the track. Returns whether the ask changed, or the protocol violation a recovery's
	/// publisher committed.
	pub fn poll(
		&mut self,
		serve: &TrackServe,
		serving: &track::Producer,
		accounted: Option<&kio::Producer<Tail>>,
		read: bool,
		waiter: &kio::Waiter,
	) -> Result<bool, Error> {
		// What the copy records skipped is never produced, such as the starts inside a whole
		// group's span, which its GROUP_END names rather than a SUBSCRIBE_SKIP: a fill owing one
		// is answered as a SUBSCRIBE_SKIP answers it, rather than asked or recovered again.
		let mut skipped = Vec::new();
		for fill in &self.held {
			if let Some(run) = fill.next_run() {
				skipped.extend(serving.skipped(run).iter().cloned());
			}
		}
		for groups in skipped {
			for gap in self.refused.gaps(&groups).collect::<Vec<_>>() {
				self.refused.insert(gap, Refusal::skipped());
			}
		}
		// The groups the subscription already accounted for, which it never sends again, are
		// recovered on their own, as often as the cache evicts them before a reader takes them
		// while a fill could hold them: a fill holds the first to land until its reader takes
		// it, so each recovery serves at least that one. Past that, the pool would evict a
		// recovered group again as it lands, so one recovered already is refused rather than
		// recovered again without end. A fill whose answer no reader picked up yet asks for
		// nothing more.
		if let Some(accounted) = accounted {
			let tail = accounted.read();
			let mut asked = RangeSet::new();
			// A recovery a fill held that a debt evicted before the fill answered it, too big
			// to hold again: as `unholdable` says of the groups whose eviction the track still
			// records, kept with the fill so neither a grace nor a debt forgets it.
			let mut unfit = RangeSet::new();
			for fill in self.held.iter().filter(|fill| !fill.is_paced()) {
				if let Some(run) = fill.next_run() {
					asked.insert(run);
				}
				unfit.extend(fill.unfit());
			}
			for run in asked.iter() {
				for gone in tail.accounted(run.clone()) {
					for gap in self.recovering.gaps(&gone).collect::<Vec<_>>() {
						for owed in self.unrefused(gap) {
							for once in self.recovered.overlapping(&owed).cloned().collect::<Vec<_>>() {
								let once = once.start.max(owed.start)..once.end.min(owed.end);
								let mut unholdable = serving.unholdable(once.clone());
								unholdable.extend(
									unfit
										.overlapping(&once)
										.map(|run| run.start.max(once.start)..run.end.min(once.end)),
								);
								for unholdable in unholdable.iter() {
									self.refused
										.insert(unholdable.clone(), Refusal::passing(StreamError::NotFound));
								}
							}
							for fresh in self.refused.gaps(&owed).collect::<Vec<_>>() {
								self.recover.insert(fresh);
							}
						}
					}
				}
			}
		}
		// Each stale run goes out on its own subscription with no max delay, once, with the
		// priority of the fills held now: the last ask may predate them.
		let priority = self
			.held
			.iter()
			.map(|fill| fill.priority())
			.fold(self.ask.priority, i8::max);
		for run in std::mem::take(&mut self.recover).iter() {
			self.recovered.insert(run.clone());
			self.recovering.insert(run.clone());
			self.recoveries
				.push(Recovery::new(serve, serving, run.clone(), priority));
		}
		// What a finished recovery did not deliver stands refused.
		let mut done = Vec::new();
		let mut walk = RangeSet::new();
		let mut violation = None;
		self.recoveries.retain_mut(|recovery| match recovery.poll(waiter) {
			Poll::Ready(Ok(refused)) => {
				done.push(refused);
				for keys in recovery.walk.iter() {
					walk.insert(keys.clone());
				}
				false
			}
			Poll::Ready(Err(err)) => {
				violation.get_or_insert(err);
				true
			}
			Poll::Pending => true,
		});
		if let Some(err) = violation {
			return Err(err);
		}
		for refused in done {
			for (groups, refusal) in refused.iter() {
				self.refused.insert(groups.clone(), refusal.clone());
			}
		}
		// What a recovery still out refused since the last poll answers the fills owing it now,
		// rather than waiting on its other streams, including a fill handed to FETCH below.
		let mut recoveries = std::mem::take(&mut self.recoveries);
		for recovery in &mut recoveries {
			self.take_refused(&mut recovery.fresh, &mut recovery.released);
		}
		self.recoveries = recoveries;
		// TEMPORARY MIGRATION, see [`Recovery::walk`]: a fill owing keys a stalled recovery left
		// goes to FETCH whole, as before per-run recoveries, once it takes what was refused
		// already, which it may not have reached yet.
		if !walk.is_empty() {
			let (walking, held): (Vec<_>, _) = std::mem::take(&mut self.held)
				.into_iter()
				.partition(|fill| walk.overlaps(&(fill.start().group..fill.end())));
			self.held = held;
			for fill in &walking {
				let span = fill.start().group..fill.end();
				for (groups, refusal) in self.refused.overlapping(&span) {
					refusal.answer(fill, groups.start.max(span.start)..groups.end.min(span.end));
				}
			}
			self.walking.extend(walking);
		}
		// Rebuilt from those still out, so one ending leaves a later one's coverage of the same
		// groups intact.
		self.cover();
		// A run's own subscription answers only below its start, so what lies past it is asked
		// again rather than counted recovered.
		if self.per_run {
			self.recovered = self.recovering.clone();
		}

		let mut ask = FillAsk::default();
		// Every run a held fill still owes, recovered or not.
		let mut owed = RangeSet::new();
		let mut held = std::mem::take(&mut self.held);
		// Not asked of the subscription: a run being recovered waits on its own, and a group
		// already refused is answered once its fill reaches it.
		let mut waiting = self.recovering.clone();
		for (groups, _) in self.refused.iter() {
			waiting.insert(groups.clone());
		}
		let this = &*self;
		held.retain_mut(|fill| {
			if fill.poll_withdrawn(waiter).is_ready() {
				return false;
			}
			// Arms every input of the owed groups: readers, answers, and the cache.
			let _ = fill.poll_wanted_changed(waiter);
			loop {
				// Answered, it is kept while a reader has yet to take what it holds.
				let Some(run) = fill.next_run() else {
					return fill.is_holding();
				};
				// `next_run` extends from the group owed first, which the subscription may have
				// refused already. Refusing the run answers it, so the loop moves on.
				let first = match fill.order() {
					Order::Ascending => run.start,
					Order::Descending => run.end - 1,
				};
				if let Some((groups, refusal)) = this.refusal(first) {
					let groups = run.start.max(groups.start)..run.end.min(groups.end);
					refusal.answer(fill, groups);
					continue;
				}
				owed.insert(run.clone());
				// With an answer no reader picked up yet, it keeps what it asked but asks for nothing
				// more: what lands meanwhile the cache may evict before the reader gets to it.
				let asked: Vec<Range<u64>> = match fill.is_paced() {
					true => this
						.ask
						.groups
						.overlapping(&run)
						.map(|asked| asked.start.max(run.start)..asked.end.min(run.end))
						.collect(),
					false => vec![run],
				};
				for run in asked {
					for gap in waiting.gaps(&run) {
						ask.groups.insert(gap);
					}
				}
				ask.add(fill);
				return true;
			}
		});
		self.held = held;

		self.forget_answered();

		// Each run goes out on its own subscription, asked again by the next pass, which polls it.
		if self.per_run && !ask.groups.is_empty() {
			self.recover = std::mem::take(&mut ask.groups);
			return self.poll(serve, serving, accounted, read, waiter);
		}

		// A recovery no held fill owes any group of is cancelled, once none of its streams is
		// still being read or nobody reads the track, and a group is recovered once for the
		// fills owing it now: a later request for it may recover it again.
		self.recoveries
			.retain(|recovery| owed.overlaps(&recovery.groups) || (read && recovery.reading()));
		self.cover();
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
		// still being read, each group searched for once. The copy's own outcome says so: a
		// reader's end would ask a flight for what the copy lacks, which nobody here reads.
		self.reading.retain(|_, group| group.poll_closed(waiter).is_pending());
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
				for key in edge.cached_groups(gap) {
					if let Some(group) = edge.peek_group(key)
						&& group.poll_closed(waiter).is_pending()
					{
						self.reading.insert(key, group);
					}
				}
			}
			if self.reading.range(run.clone()).next().is_some() {
				groups.insert(run.clone());
				draining.insert(run.clone());
			}
		}
		self.reading.retain(|key, _| draining.contains(key));
		self.draining = draining;
		// A run being recovered leaves the subscription, even out of a run kept whole: the
		// subscription dropped it for good, so naming it there again only costs the publisher.
		for run in self.recovering.iter() {
			groups.remove(run.clone());
		}
		// What finishes a stream still asks with what its fills asked.
		if self.held.is_empty() && !groups.is_empty() {
			ask.priority = self.ask.priority;
			ask.order = self.ask.order;
		}
		ask.groups = groups;
		if ask == self.ask {
			return Ok(false);
		}
		self.ask = ask;
		Ok(true)
	}

	/// The groups the ask leaves out on purpose: refused, or recovered on their own.
	pub fn avoid(&self) -> RangeSet<u64> {
		let mut avoid = self.recovering.clone();
		for (groups, _) in self.refused.iter() {
			avoid.insert(groups.clone());
		}
		avoid
	}

	/// Asking for `asked` from now on, the runs the subscription carries, retires the range
	/// of each run not asked whole any more, and what its start skipped with it.
	pub fn retire_skipped(&mut self, asked: &RangeSet<u64>) {
		let retired: Vec<_> = self
			.skipped
			.iter()
			.filter(|(_, run)| asked.get(&run.start) != Some(*run))
			.map(|(groups, _)| groups.clone())
			.collect();
		for groups in retired {
			self.skipped.remove(groups);
		}
	}
}

/// Why the subscription that asked for groups will not deliver them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Refusal {
	err: StreamError,
	/// Whether the source no longer has them, which the track records so no reader asks
	/// again: only a genuine miss, which its publisher named. Anything else answers only the
	/// fills owing them now, since another request may still get them: a stale drop judges
	/// a budget, and a range starting past them is the subscriber's own choice.
	lost: bool,
	/// Whether the source never produced them (a SUBSCRIBE_SKIP), which the fill answers as a
	/// skip: its readers pass over them rather than losing them.
	skip: bool,
}

impl Refusal {
	/// The publisher gave the groups up with `err` (a SUBSCRIBE_DROP, or an end before them):
	/// lost only when it never had them.
	fn dropped(err: StreamError) -> Self {
		let lost = err == StreamError::NotFound;
		Self { err, lost, skip: false }
	}

	/// Not delivered on this subscription, which says nothing of the source.
	fn passing(err: StreamError) -> Self {
		Self {
			err,
			lost: false,
			skip: false,
		}
	}

	/// Never produced, as the publisher declared.
	fn skipped() -> Self {
		Self {
			err: StreamError::NotFound,
			lost: false,
			skip: true,
		}
	}

	/// Answer `groups` of `fill`: a lost group is named dropped, as a FETCH's miss is, and
	/// anything else is refused for now, recorded nowhere.
	fn answer(&self, fill: &track::FillGroups, groups: Range<u64>) {
		if self.skip {
			return fill.skip_groups(groups);
		}
		match &self.err {
			StreamError::NotFound if self.lost => fill.drop_groups(groups),
			_ => fill.refuse_groups(groups, self.error()),
		}
	}

	/// The error a reader asking for the groups gets.
	pub(super) fn error(&self) -> Error {
		match &self.err {
			StreamError::NotFound => Error::NotFound,
			StreamError::Old => Error::Old,
			err => Error::Stream(err.clone()),
		}
	}
}

/// A subscription asking again, with no max delay, for groups another one dropped as stale,
/// which a fill still owes, or, on a finished copy, for a time start it can't place. It lives
/// until each group arrives and its stream is read, or the publisher gives it up, then FINs
/// and waits for the publisher's FIN in reply.
pub(super) struct Recovery {
	serve: TrackServe,
	/// The copy the recovered groups land in, which records what the publisher skipped.
	serving: track::Producer,
	id: u64,
	groups: Range<u64>,
	/// Whether its range is final. Before lite-07 the subscription proves only the keys below
	/// its start, so it asks open, then caps once at the first group that arrives.
	capped: bool,
	/// TEMPORARY MIGRATION, delete at the next version cut with [`Self::walk`]: armed once the
	/// cap left keys below the first group unaccounted for.
	stall: Option<Stall>,
	/// TEMPORARY MIGRATION, delete at the next version cut, once no relay of the previous
	/// release is left: the keys a stalled recovery leaves to FETCH. A relay of that release
	/// starts the recovery at its floor, then sends the next group it holds and never accounts
	/// for the keys between: it read a SUBSCRIBE asking no max delay as a 285,000-year grace,
	/// so it never asks its own upstream. Its FETCH answers them, so once [`crate::tail::GRACE`] passes
	/// they are walked a key at a time.
	walk: RangeSet<u64>,
	state: RecoveryState,
	/// The groups this subscription said it will not deliver, and why.
	refused: RangeMap<u64, Refusal>,
	/// Those [`RangeFills`] has yet to take: each refusal is handed over once.
	fresh: RangeMap<u64, Refusal>,
	/// The groups it refused and handed over, which it no longer covers, so a later request
	/// for them asks again rather than waiting on its other streams.
	released: RangeSet<u64>,
	/// Where the publisher started its range, once it said: `None` inside for no group to
	/// deliver.
	start: Option<Option<u64>>,
}

enum RecoveryState {
	Establish(Box<Establish>),
	/// Open, recording its SUBSCRIBE_START and SUBSCRIBE_END as a subscription does.
	Active {
		sub: Box<SubStream>,
		/// Whether our FIN went out.
		finished: bool,
		/// Once the publisher FINs unprompted, as a finished track's does before lite-07, the
		/// tail it leaves: group streams it opened may still be landing.
		draining: Option<Box<Settle>>,
	},
	/// The subscription failed to open or was reset: everything left is refused.
	Done,
}

impl Recovery {
	pub(super) fn new(serve: &TrackServe, serving: &track::Producer, groups: Range<u64>, priority: i8) -> Self {
		// Before lite-07 the start is the answer, which the publisher gives only once a group in
		// range can start it, so the range stays open until the start caps it: only upstream
		// knows which group it still holds past the run.
		let one_start = !serve.subscriber.version.has_group_end();
		let end = (!one_start).then_some(groups.end);
		let subscription = Subscription::default()
			.with_ranges(track::Range::span(Some(groups.start), end))
			.with_priority(priority)
			.with_order(Order::Ascending);
		let ranges = vec![lite::Range {
			start: Some(lite::Anchor::Absolute(Position::group(groups.start))),
			end: end.map(|end| lite::Anchor::Absolute(Position::group(end))),
		}];
		let copy = super::Accepted {
			producer: serving.clone(),
			framing: lite::Framing::new(serving.info(), serve.subscriber.version),
		};
		tracing::debug!(track = %serve.name, ?groups, "recovering stale groups");
		// No max age: the groups it recovers are the ones a max age dropped.
		let request = super::Request {
			subscription,
			ranges,
			max_age: None,
		};
		let establish = serve.register(request, Some(copy));
		Self {
			serve: serve.clone(),
			serving: serving.clone(),
			id: establish.id,
			groups,
			capped: !one_start,
			stall: None,
			walk: RangeSet::new(),
			state: RecoveryState::Establish(Box::new(establish)),
			refused: Default::default(),
			fresh: Default::default(),
			released: Default::default(),
			start: None,
		}
	}

	/// Where the publisher started its range, once readers can read from there: it said no
	/// group is left at or before its first key, or a start past that key, or the stream of
	/// the group it starts at arrived. `None` inside for no group to deliver.
	pub(super) fn landed(&self) -> Option<Option<u64>> {
		let start = self.start?;
		match (start, &self.state) {
			(Some(group), RecoveryState::Active { sub, .. }) if group <= self.groups.start => {
				sub.tail.read().first_group().is_some().then_some(start)
			}
			_ => Some(start),
		}
	}

	/// Whether a stream of the subscription is still being read, which cancelling would cut.
	pub(super) fn reading(&self) -> bool {
		match &self.state {
			RecoveryState::Active { sub, .. } => sub.tail.read().is_reading(),
			_ => false,
		}
	}

	/// End the subscription, refusing what is left of the groups with `err`, as nothing more
	/// will arrive on it.
	pub(super) fn end(&mut self, err: StreamError) {
		let state = std::mem::replace(&mut self.state, RecoveryState::Done);
		let mut owed = RangeSet::new();
		owed.insert(self.groups.clone());
		if let RecoveryState::Active { sub, .. } = &state {
			for arrived in sub.tail.read().accounted(self.groups.clone()) {
				owed.remove(arrived);
			}
		}
		for (refused, _) in self.refused.iter() {
			owed.remove(refused.clone());
		}
		for walked in self.walk.iter() {
			owed.remove(walked.clone());
		}
		for gap in owed.iter() {
			self.refused.insert(gap.clone(), Refusal::passing(err.clone()));
			self.fresh.insert(gap.clone(), Refusal::passing(err.clone()));
		}
		self.serve.subscriber.remove_subscribe(self.id);
		match state {
			RecoveryState::Establish(est) => est.abort(&Error::Cancel),
			RecoveryState::Active {
				sub,
				finished: false,
				draining: None,
			} => sub.stream.writer.abort(&Error::Cancel),
			_ => {}
		}
	}

	/// Ready once the recovery is over, with the groups it refused, or with the protocol
	/// violation its publisher committed, which the subscription's would be too.
	pub(super) fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<RangeMap<u64, Refusal>, Error>> {
		loop {
			match &mut self.state {
				RecoveryState::Establish(est) => match ready!(est.poll(waiter)) {
					Ok(active) => {
						self.state = RecoveryState::Active {
							sub: Box::new(active),
							finished: false,
							draining: None,
						}
					}
					Err(err) => {
						tracing::debug!(%err, "recovery subscribe failed");
						self.end(StreamError::from(&err));
					}
				},
				RecoveryState::Active {
					sub: active,
					finished,
					draining,
				} => {
					if let Poll::Ready(Err(err)) = active.stream.writer.poll_flush(waiter) {
						tracing::debug!(%err, "recovery subscribe write failed");
					}
					// The publisher is done, so only its streams still landing are waited for,
					// at most the grace, before what is left is refused.
					if let Some(settle) = draining {
						let groups = self.groups.clone();
						ready!(settle.poll(waiter, |tail| tail.covers(groups.clone())));
						self.end(StreamError::NotFound);
						continue;
					}
					// TEMPORARY MIGRATION, see [`Recovery::walk`]: what is still owed once the
					// grace passed with no progress is left to FETCH.
					if let Some(stall) = &mut self.stall {
						let tail = active.tail.read();
						if stall.poll(&tail, waiter).is_ready() && !tail.covers(self.groups.clone()) {
							let mut walk = RangeSet::new();
							walk.insert(self.groups.clone());
							for arrived in tail.accounted(self.groups.clone()) {
								walk.remove(arrived);
							}
							drop(tail);
							for (refused, _) in self.refused.iter() {
								walk.remove(refused.clone());
							}
							tracing::debug!(track = %self.serve.name, ?walk, "recovery stalled, fetching");
							self.walk = walk;
							self.end(StreamError::Cancel);
							continue;
						}
					}
					// FIN once every group is accounted for and read, as a subscriber does once
					// its tail settles, and the publisher FINs in reply; after any update, whose
					// flush wakes this again.
					if !*finished && active.stream.writer.is_flushed() {
						let groups = self.groups.clone();
						// Before lite-07 a relay's start may be its source's, raised to the floor, which
						// names no group: only a group that arrives proves one, so the range waits
						// for one past the start, then stops there. What lies between is named by the
						// publisher's drops, or given up with the grace.
						let capping = !self.capped && active.started.is_some();
						let seen = match active.tail.consume().poll(waiter, |tail| {
							if capping
								&& let Some(first) = tail.first_group()
								&& first >= groups.start
							{
								return Poll::Ready(Some(first));
							}
							match tail.covers(groups.clone()) && !tail.is_reading() {
								true => Poll::Ready(None),
								false => Poll::Pending,
							}
						}) {
							Poll::Ready(Ok(first)) => Some(first),
							// The tail is gone, as the subscription ended.
							Poll::Ready(Err(_)) => Some(None),
							Poll::Pending => None,
						};
						match seen {
							Some(Some(first)) => {
								self.groups = groups.start..groups.end.min(first.saturating_add(1));
								self.capped = true;
								cap(&self.groups, active)?;
								// TEMPORARY MIGRATION, see [`Recovery::walk`].
								if !active.tail.read().covers(self.groups.clone()) {
									self.stall = Some(Stall::new(&self.serve.subscriber.runtime, &active.tail.read()));
								}
								continue;
							}
							Some(None) => {
								let _ = active.stream.writer.finish();
								*finished = true;
							}
							None => {}
						}
					}
					let res = ready!(active.stream.reader.poll_recv::<lite::SubscribeResponse>(waiter));
					let now = self.serve.subscriber.runtime.now();
					let version = self.serve.subscriber.version;
					let groups = self.groups.clone();
					// TEMPORARY MIGRATION, see [`Stall`]: the publisher is still answering.
					if let Some(stall) = &mut self.stall {
						stall.restart();
					}
					// What the publisher gives up of the groups, accounted for in the tail.
					let mut given_up = |given: Range<u64>, refusal: Refusal| {
						if let Ok(mut tail) = active.tail.write() {
							tail.account(given.clone(), now);
						}
						let given = given.start.max(groups.start)..given.end.min(groups.end);
						if !given.is_empty() {
							self.refused.insert(given.clone(), refusal.clone());
							self.fresh.insert(given, refusal);
						}
					};
					match res {
						// One start, for its one range.
						Recv::Message(lite::SubscribeResponse::Start(start))
							if active.started.is_none() && start.id == 0 =>
						{
							// Below its start the publisher delivers nothing.
							let from = start.start.unwrap_or(groups.end);
							given_up(
								groups.start..from.clamp(groups.start, groups.end),
								Refusal::passing(StreamError::NotFound),
							);
							active.started = Some(start.start);
							self.start = Some(start.start);
							// Starting past the run answers all of it, so nothing more is asked.
							if !self.capped && from >= groups.end {
								self.capped = true;
								cap(&self.groups, active)?;
							}
						}
						Recv::Message(lite::SubscribeResponse::Drop(dropped)) => {
							given_up(dropped.groups.clone(), Refusal::dropped(dropped.error(version)));
						}
						// Never produced, such as the keys between two sparse group starts: the copy
						// records the skip, as the subscription itself does, and a fill owing one
						// is answered rather than asked again.
						Recv::Message(lite::SubscribeResponse::Skip(skipped)) => {
							if !skipped.groups.is_empty() {
								self.serving.skip_received(skipped.groups.clone());
								given_up(skipped.groups, Refusal::skipped());
							}
						}
						// Nothing at or past the track's end will be produced.
						Recv::Message(lite::SubscribeResponse::End(finish)) if active.end.is_none() => {
							given_up(
								finish.group.max(groups.start)..groups.end,
								Refusal::dropped(StreamError::NotFound),
							);
							active.end = Some(finish);
						}
						Recv::Message(lite::SubscribeResponse::Ok(_)) => {}
						// A second START or END, or a START for a range it never asked.
						Recv::Message(msg) => {
							tracing::warn!(?msg, "unexpected recovery subscribe response");
							return Poll::Ready(Err(Error::ProtocolViolation));
						}
						// The publisher FINs in reply to ours, once the tail settled.
						Recv::Ended if *finished => self.end(StreamError::NotFound),
						// Or once it finished the track, before every stream it opened landed.
						Recv::Ended if active.end.is_some() => {
							// At most the grace: no max delay bounds the tail's own, and before lite-07
							// nothing names a key after the FIN.
							*draining = Some(Box::new(
								Settle::new(&self.serve.subscriber.runtime, active.tail.consume())
									.at_most(crate::tail::GRACE),
							));
						}

						Recv::Ended => {
							tracing::warn!("recovery subscribe ended without SUBSCRIBE_END");
							return Poll::Ready(Err(Error::ProtocolViolation));
						}
						// Malformed bytes are a violation, as on the subscription itself.
						Recv::Malformed(err) => {
							tracing::warn!(%err, "malformed recovery subscribe response");
							return Poll::Ready(Err(Error::ProtocolViolation));
						}
						// A reset, or a FIN inside a message, gives up what is left with its reason.
						Recv::Reset(err) => {
							tracing::debug!(%err, "recovery subscribe ended");
							self.end(StreamError::from(&err));
						}
					}
				}
				// Only what was not handed over while it was out.
				RecoveryState::Done => return Poll::Ready(Ok(std::mem::take(&mut self.fresh))),
			}
		}
	}
}

/// TEMPORARY MIGRATION, delete at the next version cut with [`Recovery::walk`]: how long a
/// capped recovery may go without progress before what it still owes is left to FETCH.
///
/// A relay of the previous release never answers the keys below the first group. A current one
/// answers them once its own upstream does, which may take longer than the grace behind a slow
/// or deep chain, so the grace restarts on every response and data stream instead of running
/// from the cap.
struct Stall {
	runtime: crate::time::Deadlines,
	deadline: crate::time::Deadline,
	/// The tail's streams received and whether one is being read, when last seen.
	seen: (u64, bool),
}

impl Stall {
	fn new(runtime: &crate::time::Deadlines, tail: &Tail) -> Self {
		Self {
			runtime: runtime.clone(),
			deadline: crate::time::Deadline::after(runtime, crate::tail::GRACE),
			seen: (tail.streams(), tail.is_reading()),
		}
	}

	/// Restart the grace, as the publisher made progress.
	fn restart(&mut self) {
		self.deadline.set(self.runtime.now().checked_add(crate::tail::GRACE));
	}

	/// Ready once the grace passed with no stream arriving, being read, or finishing.
	fn poll(&mut self, tail: &Tail, waiter: &kio::Waiter) -> Poll<()> {
		let seen = (tail.streams(), tail.is_reading());
		if seen != self.seen || seen.1 {
			self.seen = seen;
			self.restart();
		}
		self.deadline.poll(waiter)
	}
}

/// Before lite-07, cap a recovery's open range at `groups`, those it waits for: nothing names
/// the keys past a group, so the rest of the run is asked again. A stream already sent past
/// the cap still lands.
fn cap(groups: &Range<u64>, active: &mut SubStream) -> Result<(), Error> {
	let range = lite::Range {
		start: Some(lite::Anchor::Absolute(Position::group(groups.start))),
		end: Some(lite::Anchor::Absolute(Position::group(groups.end))),
	};
	let (edit, _) = active.live.edit_keyed(&[((), range)], |_| ())?;
	super::buffer_update(active, edit)
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
		assert_eq!(fills.refused.get(&3), Some(&Refusal::dropped(StreamError::Old)));
		assert!(!fills.refused.get(&3).unwrap().lost, "a stale drop is never a loss");

		// A stale drop repeated while a recovery still asks for the groups waits on it.
		fills.recovering.insert(3..5);
		fills.refuse(3..5, StreamError::Old);
		assert_eq!(
			fills.refused.get(&3),
			Some(&Refusal::dropped(StreamError::Old)),
			"kept from before"
		);
		fills.refused = Default::default();
		fills.refuse(3..5, StreamError::Old);
		assert_eq!(fills.refused.get(&3), None, "a recovery still asks for it");
		fills.recovering = Default::default();

		fills.refuse(7..9, StreamError::NotFound);
		assert_eq!(fills.refused.get(&7), None, "no fill asked for it");
		fills.refuse(5..6, StreamError::NotFound);
		assert_eq!(fills.refused.get(&5), Some(&Refusal::dropped(StreamError::NotFound)));
		assert!(fills.refused.get(&5).unwrap().lost, "the publisher never had it");
	}

	/// A refusal stands while a held fill covers its groups, and ends with them, so a later
	/// request asks again and a loss stays recorded on the track alone.
	#[test]
	fn a_refusal_ends_with_its_fills() {
		let (_producer, _requests, _reader, fill) = missed();
		let mut fills = RangeFills::default();
		fills.held.push(fill);
		fills.refused.insert(0..2, Refusal::passing(StreamError::Cancel));
		fills.refused.insert(2..5, Refusal::dropped(StreamError::NotFound));
		fills.forget_answered();
		assert!(fills.refused.get(&0).is_some(), "the held fill still owes it");
		assert!(fills.refused.get(&2).is_some(), "the held fill still owes it");
		assert_eq!(fills.refused.get(&3), None, "trimmed to the groups a held fill covers");

		fills.held.clear();
		fills.forget_answered();
		assert!(fills.refused.is_empty(), "no fill owes them any more");
	}

	/// A recovery still out hands each refusal over once: after the fills owing it end, a
	/// later poll does not read it back for a later request.
	#[test]
	fn a_recovery_refusal_is_taken_once() {
		let mut fills = RangeFills::default();
		fills.recovering.insert(1..4);
		let mut fresh = RangeMap::new();
		let mut released = RangeSet::new();
		fresh.insert(1..3, Refusal::passing(StreamError::Cancel));
		fills.take_refused(&mut fresh, &mut released);
		assert!(fills.refused.get(&1).is_some(), "handed over");
		// No longer covered, so a later request asks again rather than waiting on the rest.
		assert!(!fills.recovering.contains(&1), "released");
		assert!(fills.recovering.contains(&3), "still recovering");
		assert!(released.contains(&2));

		// No fill owes it any more, and the recovery is still out.
		fills.forget_answered();
		fills.take_refused(&mut fresh, &mut released);
		assert!(fills.refused.is_empty(), "not read back");
	}

	/// A track whose cache evicted groups 0..3, with a reader of them whose fill is the next
	/// request.
	fn missed() -> (track::Producer, track::Requests, track::Ordered, track::FillGroups) {
		let producer = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"test",
			track::Info::default()
				.with_timescale(crate::Timescale::MILLI)
				.with_max_age(None),
		);
		let requests = producer.requests();
		for key in 0..6 {
			let mut group = producer.create_group(group::Info { start: key }).unwrap();
			group
				.write_frame(crate::Timestamp::from_millis(key * 1000).unwrap(), b"x".as_slice())
				.unwrap();
			group.finish().unwrap();
		}
		// From group 0, which a time start names whatever is held, so the reader asks the
		// missing groups straight away rather than looking up where its start falls.
		for key in 0..3 {
			producer.evict(key);
		}
		let (reader, fill) = ask(&producer, &requests);
		(producer, requests, reader, fill)
	}

	/// A reader of groups 0..3, and the fill it asks for.
	fn ask(producer: &track::Producer, requests: &track::Requests) -> (track::Ordered, track::FillGroups) {
		let waiter = kio::Waiter::noop();
		let mut reader = producer
			.subscribe(Subscription::default().with_max(None).with_range(0..3))
			.ordered();
		assert!(reader.poll_next_group(&waiter).is_pending(), "groups 0..3 are owed");
		let Poll::Ready(Ok(fill)) = requests.poll_requested_fill(&waiter) else {
			panic!("groups 0..3 are a miss");
		};
		(reader, fill)
	}

	/// A range starting past what a fill asked is the subscriber's own choice: the fill's
	/// readers stop waiting, but nothing is recorded, so once a lowered start reaches the
	/// groups a later reader asks again and gets them. A genuine miss stays dropped.
	#[test]
	fn a_skipped_start_is_never_a_loss() {
		let waiter = kio::Waiter::noop();
		let (producer, requests, mut reader, fill) = missed();
		Refusal::passing(StreamError::NotFound).answer(&fill, 0..3);
		assert!(
			reader.poll_next_group(&waiter).is_ready(),
			"the reader stops waiting for the skipped groups"
		);
		drop((reader, fill));

		// The lowered start asks for them again, and they arrive.
		let (mut reader, fill) = ask(&producer, &requests);
		for key in 0..3 {
			let mut group = fill.accept(key, None).unwrap().expect("a fresh group");
			group
				.write_frame(crate::Timestamp::from_millis(key * 1000).unwrap(), b"x".as_slice())
				.unwrap();
			group.finish().unwrap();
		}
		for key in 0..3 {
			let Poll::Ready(Ok(Some(group))) = reader.poll_next_group(&waiter) else {
				panic!("group {key} was delivered");
			};
			assert_eq!(group.start, key);
		}

		// The publisher naming them not found is a loss: nobody asks again.
		let (producer, requests, reader, fill) = missed();
		Refusal::dropped(StreamError::NotFound).answer(&fill, 0..3);
		drop((reader, fill));
		let mut reader = producer
			.subscribe(Subscription::default().with_max(None).with_range(0..3))
			.ordered();
		let _ = reader.poll_next_group(&waiter);
		assert!(
			requests.poll_requested_fill(&waiter).is_pending(),
			"a lost group is not asked again"
		);
	}

	/// Groups a recovery's SUBSCRIBE_SKIP names were never produced: the reader passes over
	/// them, not a gap, even once the pool forgets the skip the copy recorded to pay its debt.
	#[test]
	fn a_skip_answer_is_no_gap_once_the_cache_forgets_it() {
		let waiter = kio::Waiter::noop();
		let (producer, _requests, mut reader, fill) = missed();
		Refusal::skipped().answer(&fill, 0..3);
		producer.forget_drops(0..3);
		assert!(
			matches!(reader.poll_next_item(&waiter), Poll::Ready(Ok(None))),
			"the read ends with no gap"
		);
		drop(fill);
	}

	/// A range starting past what it asked refuses those groups only while that range is live:
	/// a later range asking for them again may start lower.
	#[test]
	fn a_skipped_start_stands_only_while_its_range_does() {
		let mut fills = RangeFills::default();
		fills.ask.groups.insert(2..6);
		fills.ask.groups.insert(8..10);
		fills.skip(2..6, 2..4);
		fills.skip(8..10, 8..9);
		assert_eq!(fills.refusal(3), Some((2..4, Refusal::passing(StreamError::NotFound))));

		// Asking 2..6 whole keeps its range; asking 8..12 retires the range for 8..10.
		let mut asked = RangeSet::new();
		asked.insert(2..6);
		asked.insert(8..12);
		fills.retire_skipped(&asked);
		assert_eq!(fills.refusal(3), Some((2..4, Refusal::passing(StreamError::NotFound))));
		assert_eq!(fills.refusal(8), None, "the new range says where it starts");

		// A drop stands whatever is asked.
		fills.refuse(4..5, StreamError::NotFound);
		fills.retire_skipped(&RangeSet::new());
		assert_eq!(fills.refusal(3), None);
		assert_eq!(fills.refusal(4), Some((4..5, Refusal::dropped(StreamError::NotFound))));
	}
}
