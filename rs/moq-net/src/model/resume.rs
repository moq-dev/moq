//! Serve a front's logical track straight from its routes' copies.
//!
//! A front serves each track through one route at a time and switches when that route
//! dies, withdraws, or is beaten by another with the same instance. Routes with one instance
//! serve one broadcast, so every route's copy of a track holds the same groups and frames. A reader of the logical
//! track therefore reads the serving route's copy directly, through its own
//! [`Subscriber`]: nothing is copied, and with one route it is a passthrough.
//!
//! A switch splits the reader's subscription at one boundary: the largest group any copy
//! holds, never below the newest handed out. Each reader subscribes to the new copy from the
//! boundary, past the frames of it a copy holds from its first, which that copy adopts once
//! the rest lands so a later reader finds the group whole. Each copy it replaces is capped at
//! the boundary on the wire at once, stopping the boundary group at that same frame, so it
//! finishes the groups up to it and sends nothing newer. A replaced copy is kept while it
//! may still deliver a group under its cap the
//! reader lacks (unread, or still landing within the grace a passed group is waited for),
//! and while a group still read is read from it or may continue from it (it holds that
//! group, or may yet get it). Nothing under the cap is waited for past the reader's budget,
//! counted once the serving copy holds a group past the boundary, so the boundary group, the
//! latest, never is. The serving copy draining cleanly ends the track only once nothing it
//! replaced may still deliver below that end. Until then, a copy that ran out still hands
//! out a group landing on it late, one still in flight below the end its session settled. A
//! reader that never asked for a group keeps no copy.
//!
//! A new copy is asked from the boundary, so it never has the groups below the reader's
//! local floor that the copies it replaced cached. Lowering that floor asks the newest route
//! for them through a backfill: another copy of that route, bounded to the groups the floor
//! newly covers, and kept like a replaced copy. A switch asks the same way for the groups
//! below the boundary that the reader still lacks, from the lowest one no replaced copy holds
//! under its cap, and again once a replaced copy fails: one late, reordered, or in flight on
//! the replaced route still arrives within the reader's budget, as it would on one cursor.
//!
//! A group carries on across the switch through its [`Recover`]: once its copy stalls while
//! a newer route serves, it continues from the serving route's copy of the same group at the
//! same frame, found in that copy's cache or fetched from it, skipping the bytes of a frame
//! already read. Once its copy fails, any other copy still holding the group continues it
//! first, since a group's name fixes its content.
//!
//! A group is handed out once however many copies deliver it: the reader skips any
//! key its [`track::Subscriber`] already handed out, the same record that keeps a
//! direct reader from taking a re-created group twice.
//!
//! A reader polled for drops reports what the serving copy gave up, other than a route
//! failure, once no copy it replaced may still deliver it. A route change discards what the
//! former serving copy gave up and the reader had yet to report.

use rangemap::{RangeMap, RangeSet};

use std::collections::{BTreeMap, BTreeSet};

use std::ops::Bound;
use std::sync::{Arc, PoisonError, Weak};
use std::sync::{Mutex, MutexGuard};
use std::task::{Poll, ready};
use std::time::Duration;

use crate::time::Instant;
use crate::{Error, Result, StreamError, group, stats, track};

use super::delivered::Delivered;
use super::idle::{Idle, Walk};
use super::subscription::{Accounted, Containing, Cover, Holdings, Position, Resolved, Subscription, Unheld, Window};
use crate::ledger;

/// What the front says about a logical track: the copy to read now, and how it ended.
#[derive(Default)]
struct Route {
	/// Bumped whenever `copy` changes, so a reader can tell a replacement apart.
	generation: u64,
	/// The serving route's copy; `None` while nobody reads the track, between routes, or
	/// once the front let go.
	copy: Option<track::Consumer>,
	/// The copy the front let go of when nobody read the track, until the front serves
	/// another or its source detaches: a whole group it still caches is served without
	/// waking the front. A lazy handle, so it keeps neither the copy nor its route alive.
	parked: Option<track::Consumer>,
	/// Whether the front may replace the copy, or how it finished.
	phase: Phase,
	/// The copy the front asked for while its info is in flight, by stage id: a fetch goes
	/// to it at once, and hands its group out only once that copy is spliced.
	staged: Option<(u64, track::Consumer)>,
	/// The stage the serving copy was spliced from, if any.
	spliced: Option<u64>,
	/// The id the next stage takes.
	next_stage: u64,
}

impl Route {
	/// Ready once the route moved past `generation`, or the track ended. Reads only the
	/// route and `generation`, so a [`kio::Watch`] may skip it while neither changed.
	fn moved_past(&self, generation: u64) -> Poll<()> {
		match self.generation != generation || self.phase.terminal() {
			true => Poll::Ready(()),
			false => Poll::Pending,
		}
	}

	/// The copy a read goes to first. Once the track finished, that is the copy it ended
	/// on until that copy fails: one the front serves after the end caches nothing, so
	/// asking it would send upstream what the last copy still answers.
	fn answering(&self) -> Option<(u64, &track::Consumer)> {
		match &self.phase {
			Phase::Finished {
				last: (generation, Some(copy)),
				..
			} if !copy_failed(copy) => Some((*generation, copy)),
			_ => self.copy.as_ref().map(|copy| (self.generation, copy)),
		}
	}
}

#[derive(Default)]
enum Phase {
	#[default]
	Open,
	/// Readers follow the last copy to its end; no front will replace it.
	Concluded,
	/// The front ended the track cleanly. Its readers stay on the generation and copy it
	/// ended on (`last`), which holds everything below the end. While `routed`, the front
	/// still replaces the serving copy when its source changes, for the group reads and
	/// fetches still holding the track, so they move off a route that dies.
	Finished {
		last: (u64, Option<track::Consumer>),
		routed: bool,
	},
	/// The front failed the track for good.
	Failed(Error),
}

impl Phase {
	fn terminal(&self) -> bool {
		!matches!(self, Self::Open)
	}

	/// Whether a front may still replace the copy.
	fn moves(&self) -> bool {
		matches!(self, Self::Open | Self::Finished { routed: true, .. })
	}

	/// The front's verdict on the track, once it gave one.
	fn end(&self) -> Option<Result<()>> {
		match self {
			Self::Finished { .. } => Some(Ok(())),
			Self::Failed(err) => Some(Err(err.clone())),
			Self::Open | Self::Concluded => None,
		}
	}
}

/// Every reader of a logical track, so a new copy subscribes them all as it is served:
/// one that is not reading right now still gets what that copy delivers.
type Readers = Arc<Mutex<Vec<Weak<Mutex<Reader>>>>>;

/// The front's side of a logical track: which copy serves it, and how it ends.
///
/// Dropping it concludes the track: readers follow the last copy to its end, since no
/// front is left to replace it. [`Producer::release`] also lets go of that copy, so it
/// stops keeping its route subscribed however long the logical track's state stays
/// allocated.
pub(super) struct Producer {
	state: kio::Producer<Route>,
	readers: Readers,
}

impl Producer {
	pub(super) fn new() -> Self {
		Self {
			state: kio::Producer::default(),
			readers: Readers::default(),
		}
	}

	/// The readers, or `None` until every one of `subscribers` is here: a subscription not
	/// yet polled has demand but no reader.
	fn readers(&self, subscribers: usize) -> Option<Vec<Arc<Mutex<Reader>>>> {
		let readers: Vec<_> = self
			.readers
			.lock()
			.unwrap_or_else(PoisonError::into_inner)
			.iter()
			.filter_map(Weak::upgrade)
			.collect();
		(readers.len() == subscribers && !readers.is_empty()).then_some(readers)
	}

	/// The readers' demand on a new copy, with each reader resuming where it got to.
	#[cfg(test)]
	pub(super) fn subscription(&self, demand: Subscription, subscribers: usize) -> Subscription {
		self.asked(demand, subscribers).0
	}

	/// Subscribe `copy` to the readers' demand, with each reader resuming where it got to,
	/// lending it the frames held of the group the demand resumes past.
	pub(super) fn subscribe(&self, copy: &track::Consumer, demand: Subscription, subscribers: usize) -> Held {
		let (demand, head) = self.asked(demand, subscribers);
		let resume = demand.resume;
		let sub = copy.subscribe(demand);
		// Once subscribed, so the copy's state exists to hold the loan.
		if let (Some(head), Some(resume)) = (head, resume) {
			sub.lend_head(resume.group, head);
		}
		Held::new(sub)
	}

	/// [`Self::subscribe`]'s demand, and the head it resumes past.
	fn asked(&self, mut demand: Subscription, subscribers: usize) -> (Subscription, Option<Arc<group::Head>>) {
		// A subscription not yet polled still needs the original ranges, so never narrow the
		// aggregate until every contribution is here.
		let Some(readers) = self.readers(subscribers) else {
			return (demand, None);
		};
		let mut ranges = Vec::new();
		let mut resume = Resume::default();
		for reader in &readers {
			// Ranges and resume under one lock, so a reader moving on between them cannot ask
			// a range from one group and resume past another.
			let mut reader = reader.lock().unwrap_or_else(PoisonError::into_inner);
			let preferences: Vec<_> = reader.subscription.read().ranges().collect();
			ranges.extend(reader.resumed(&preferences).into_iter().map(|(_, range)| range));
			resume.add(&reader);
		}
		demand.ranges = ranges;
		let resume = resume.lowest();
		demand.resume = resume.as_ref().map(|(resume, _)| *resume);
		(demand, resume.map(|(_, head)| head))
	}

	/// Keep `held` asking for the readers' demand of `weak`, as [`Self::subscription`] folds
	/// it. Returns `false` once no reader is left, to let it go. Once every reader is here,
	/// a change costs the ranges it touched: a reader's edit reaches `held` as an edit too.
	pub(super) fn follow(&self, held: &mut Held, weak: &track::TrackWeak) -> bool {
		let Some((terms, subscribers)) = weak.terms() else {
			return false;
		};
		let Some(readers) = self.readers(subscribers) else {
			held.fold = None;
			if let Some((demand, _)) = weak.subscription() {
				let _ = held.sub.update(demand);
			}
			return true;
		};
		let fold = match &mut held.fold {
			Some(fold) if fold.feed == terms.feed => fold,
			_ => {
				// Asked whole, every range of every reader keyed by its place in the fold.
				let mut fold = Fold {
					feed: terms.feed,
					..Default::default()
				};
				let mut ranges = Vec::new();
				let mut resume = Resume::default();
				for reader in &readers {
					// Ranges and resume under one lock, as `asked` takes them.
					let mut locked = reader.lock().unwrap_or_else(PoisonError::into_inner);
					ranges.extend(
						fold.fold_locked(reader, &mut locked, None)
							.into_iter()
							.map(|(_, range)| range),
					);
					resume.add(&locked);
				}
				// Asked whole, it resumes where the readers got to, as a new copy is asked. An edit
				// keeps that resume, as a reader's own copy does.
				let resume = resume.lowest();
				if let Some((resume, head)) = &resume {
					held.sub.lend_head(resume.group, head.clone());
				}
				let resume = resume.map(|(resume, _)| resume);
				let _ = held.sub.update(Subscription {
					ranges,
					resume,
					..terms
				});
				let keyed = held
					.sub
					.preferences()
					.read()
					.ranges()
					.map(|(key, _)| key)
					.eq(0..fold.next);
				if !keyed {
					let _ = held.sub.preferences().rekey(0..fold.next);
				}
				held.fold = Some(fold);
				return true;
			}
		};
		let mut edit = track::Edit {
			priority: terms.priority,
			delay: terms.delay,
			order: terms.order,
			..Default::default()
		};
		let mut seen = std::collections::HashSet::with_capacity(readers.len());
		for reader in &readers {
			seen.insert(Arc::as_ptr(reader) as usize);
			let (retire, add) = fold.refold(reader);
			edit.retire.extend(retire);
			edit.add.extend(add);
		}
		// Readers that left take their ranges with them.
		fold.readers.retain(|id, folded| {
			let kept = seen.contains(id);
			if !kept {
				edit.retire.extend(folded.keys.values().copied());
			}
			kept
		});
		let _ = held.sub.preferences().edit(edit);
		true
	}

	/// Stage `copy`, whose info is still in flight, for fetches until the returned
	/// [`Staged`] is spliced with [`Self::splice`] or dropped.
	pub(super) fn stage(&self, copy: track::Consumer) -> Staged {
		let id = match self.state.write() {
			Ok(mut route) => {
				let id = route.next_stage;
				route.next_stage += 1;
				route.staged = Some((id, copy));
				id
			}
			Err(_) => u64::MAX,
		};
		Staged {
			id,
			state: self.state.clone(),
		}
	}

	/// Serve the track from `copy` from now on, subscribing every reader to it.
	#[cfg(any(test, feature = "fuzz"))]
	pub(super) fn serve(&self, copy: track::Consumer) {
		self.serve_from(copy, None);
	}

	/// Serve the track from `copy`, the one `staged` was made for, subscribing every reader
	/// to it: what was fetched from it is handed out from here on.
	pub(super) fn splice(&self, copy: track::Consumer, staged: Staged) {
		self.serve_from(copy, Some(staged.id));
	}

	fn serve_from(&self, copy: track::Consumer, stage: Option<u64>) {
		if let Ok(mut route) = self.state.write() {
			route.generation += 1;
			route.copy = Some(copy);
			route.parked = None;
			route.spliced = stage;
			if stage.is_some() && route.staged.as_ref().map(|(id, _)| *id) == stage {
				route.staged = None;
			}
		}
		let readers = {
			let mut readers = self.readers.lock().unwrap_or_else(PoisonError::into_inner);
			readers.retain(|reader| reader.strong_count() > 0);
			readers.clone()
		};
		for reader in readers.iter().filter_map(Weak::upgrade) {
			reader
				.lock()
				.unwrap_or_else(PoisonError::into_inner)
				.sync(&kio::Waiter::noop());
		}
	}

	/// Let go of the serving copy: nobody reads the track, so its route goes idle.
	pub(super) fn park(&self) {
		if let Ok(mut route) = self.state.write()
			&& route.copy.is_some()
		{
			route.generation += 1;
			route.parked = route.copy.take();
			route.spliced = None;
		}
	}

	/// Stop serving whole groups from the parked copy: its source detached, so a fetch
	/// resolves the current route again.
	pub(super) fn unpark(&self) {
		if let Ok(mut route) = self.state.write() {
			route.parked = None;
		}
	}

	/// Whether the track already ended, cleanly or not: its copy feeds it nothing more.
	pub(super) fn ended(&self) -> bool {
		self.state.read().phase.end().is_some()
	}

	/// Whether a front may still replace the copy: the track is open, or finished with
	/// reads still to move off a dying route.
	pub(super) fn moves(&self) -> bool {
		self.state.read().phase.moves()
	}

	/// The track ended: cleanly with `Ok`, or failed for good. A clean end still moves to
	/// each copy the front serves from until it concludes.
	pub(super) fn end(&self, result: Result<()>) {
		if let Ok(mut route) = self.state.write()
			&& route.phase.end().is_none()
		{
			route.phase = match result {
				Ok(()) => Phase::Finished {
					last: (route.generation, route.copy.clone()),
					routed: !matches!(route.phase, Phase::Concluded),
				},
				Err(err) => Phase::Failed(err),
			};
		}
	}

	/// No front will replace the serving copy: readers follow it to its end, while this
	/// still holds it for the readers on their way.
	pub(super) fn conclude(&self) {
		if let Ok(mut route) = self.state.write() {
			match &mut route.phase {
				Phase::Open => route.phase = Phase::Concluded,
				Phase::Finished { routed, .. } => *routed = false,
				Phase::Concluded | Phase::Failed(_) => {}
			}
		}
	}

	/// Let go of the serving copy once no front will replace it and nobody reads the
	/// track. Same generation: a reader on the copy keeps it as the serving one.
	pub(super) fn release(self) {
		if let Ok(mut route) = self.state.write() {
			route.copy = None;
			route.parked = None;
			route.spliced = None;
			if let Phase::Finished { last: (_, copy), .. } = &mut route.phase {
				*copy = None;
			}
		}
	}

	pub(super) fn consume(&self) -> Consumer {
		Consumer {
			state: self.state.consume(),
			readers: self.readers.clone(),
		}
	}
}

impl Drop for Producer {
	fn drop(&mut self) {
		self.conclude();
	}
}

/// A copy staged with [`Producer::stage`]. Dropping it unspliced (its info was refused, its
/// source left, or nobody reads the track) withdraws it, so a fetch sent to it asks the
/// next route instead.
pub(super) struct Staged {
	id: u64,
	state: kio::Producer<Route>,
}

impl Drop for Staged {
	fn drop(&mut self) {
		if let Ok(mut route) = self.state.write()
			&& route.staged.as_ref().is_some_and(|(id, _)| *id == self.id)
		{
			route.staged = None;
		}
	}
}

/// A reader's handle on a logical track's [`Producer`].
#[derive(Clone)]
pub(super) struct Consumer {
	state: kio::Consumer<Route>,
	readers: Readers,
}

impl Consumer {
	/// The copy a read goes to first: the serving route's, or the one a finished track
	/// ended on while it has not failed.
	pub(super) fn serving(&self) -> Option<track::Consumer> {
		self.state.read().answering().map(|(_, copy)| copy.clone())
	}

	/// The copy a read goes to first and the generation it serves at (the route's, when
	/// none), and whether the track ended.
	pub(super) fn answering(&self) -> (u64, Option<track::Consumer>, bool) {
		let route = self.state.read();
		let terminal = route.phase.terminal();
		match route.answering() {
			Some((generation, copy)) => (generation, Some(copy.clone()), terminal),
			None => (route.generation, None, terminal),
		}
	}

	/// The copy the front parked, while it serves none and may still serve one again.
	pub(super) fn parked(&self) -> Option<track::Consumer> {
		let route = self.state.read();
		match route.copy.is_none() && !route.phase.terminal() {
			true => route.parked.clone(),
			false => None,
		}
	}

	/// Whether `position` is the serving copy's largest and its group ended there.
	pub(super) fn largest_finished(&self, position: Position) -> bool {
		self.serving().is_some_and(|copy| copy.largest_finished(position))
	}

	/// Poll for a largest location within `bounds` newer than `seen` on the serving copy,
	/// recording it there. A copy ending is its route failing, so the logical track ends
	/// only with the route: at once when the front ends it, or with the last copy once the
	/// front concluded.
	pub(super) fn poll_largest(
		&self,
		seen: &mut Option<Position>,
		bounds: track::Bounds,
		waiter: &kio::Waiter,
	) -> Poll<Option<Position>> {
		loop {
			// One read, so a copy served just before the front concluded is the one polled.
			let (generation, copy, end) = {
				let route = self.state.read();
				let end = route.phase.terminal().then(|| route.phase.end());
				(route.generation, route.answering().map(|(_, copy)| copy.clone()), end)
			};
			// A replacement copy can start below what an earlier one reported.
			match copy.map(|copy| copy.poll_largest_after(seen, bounds, waiter)) {
				Some(Poll::Ready(Some(largest))) => return Poll::Ready(Some(largest)),
				_ if matches!(end, Some(Some(_))) => return Poll::Ready(None),
				Some(Poll::Pending) if end.is_some() => return Poll::Pending,
				Some(Poll::Ready(None)) | None if end.is_some() => return Poll::Ready(None),
				_ => {}
			}
			ready!(self.poll_changed(generation, waiter));
		}
	}

	/// Ready once the serving copy holds a group. Stays pending once the front settled on a
	/// last copy without one.
	pub(super) fn poll_grouped(&self, waiter: &kio::Waiter) -> Poll<()> {
		loop {
			// One read, so a copy served just before the front concluded is the one polled.
			let (generation, copy, terminal) = {
				let route = self.state.read();
				(
					route.generation,
					route.answering().map(|(_, copy)| copy.clone()),
					route.phase.terminal(),
				)
			};
			if copy.is_some_and(|copy| copy.poll_grouped(waiter).is_ready()) {
				return Poll::Ready(());
			}
			if terminal {
				return Poll::Pending;
			}
			ready!(self.poll_changed(generation, waiter));
		}
	}

	/// Subscribe to the logical track with the reader's own preferences.
	pub(super) fn subscribe(&self, subscription: track::Preferences) -> Subscriber {
		let (mirrored, revision, delay) = {
			let prefs = subscription.read();
			// Only the ranges: the first sync mirrors the scalars, before it asks any copy.
			let mut mirrored = Mirrored::default();
			for (key, range) in prefs.ranges() {
				mirrored.insert(key, range, prefs.since(key));
			}
			(mirrored, prefs.revision, prefs.delay)
		};
		let window = keyed(
			mirrored.ranges.iter().map(|(key, range)| (*key, *range)),
			delay,
			&Unheld,
		);
		let reader = Arc::new_cyclic(|this| {
			Mutex::new(Reader {
				this: this.clone(),
				route: self.clone(),
				subscription: subscription.clone(),
				mirrored,
				revision,
				generation: None,
				copies: Vec::new(),
				newest: None,
				skipped: RangeSet::new(),
				resumed: None,
				refused: None,
				window,
				groups: (0, None),
				capped: kio::WaiterList::new(),
				groups_read: false,
				out: Vec::new(),
				drops: RangeMap::new(),
				received: RangeSet::new(),
				received_checked: None,
				declared: None,
				declared_error: None,
			})
		});
		// Subscribed to the serving copy at once, so groups it has now are the reader's
		// even if the route changes before the first read, and to each copy after.
		{
			let mut readers = self.readers.lock().unwrap_or_else(PoisonError::into_inner);
			// Sweep the readers that left once the list fills, so it tracks the live count.
			if readers.len() == readers.capacity() {
				readers.retain(|reader| reader.strong_count() > 0);
				let live = readers.len();
				readers.reserve(live.max(1));
			}
			readers.push(Arc::downgrade(&reader));
		}
		reader
			.lock()
			.unwrap_or_else(PoisonError::into_inner)
			.sync(&kio::Waiter::noop());
		Subscriber {
			reader,
			subscription,
			idle: None,
		}
	}

	/// Fetch one group from whichever route serves the track. `logical` is the logical
	/// track's state, which the fetched group holds while it is read.
	pub(super) fn fetch_group(
		&self,
		key: u64,
		options: track::FetchOptions,
		logical: kio::Consumer<track::TrackState>,
	) -> Fetching {
		Fetching {
			route: self.clone(),
			key,
			options,
			logical,
			pending: None,
			left: None,
		}
	}

	/// Register `waiter` for any change to the route, so a read that parked on a path that
	/// never looked at the route still wakes when it changes.
	fn register(&self, waiter: &kio::Waiter) {
		let _ = self.state.poll(waiter, |_| Poll::<()>::Pending);
	}

	/// Poll for the route state to move past `generation`, or the track to end.
	pub(super) fn poll_changed(&self, generation: u64, waiter: &kio::Waiter) -> Poll<()> {
		self.state
			.poll(waiter, |route| route.moved_past(generation))
			.map(|_| ())
	}

	/// A watch on the route, for a caller that re-polls [`Route::moved_past`] against a
	/// generation it moves only once that is ready.
	fn watch(&self) -> kio::Watch<Route> {
		kio::Watch::new(self.state.clone())
	}

	/// How the logical track ended: `None` while the front may still replace its copy,
	/// `Some(None)` once it concluded (readers follow the last copy), else the front's
	/// decision.
	fn end(&self) -> Option<Option<Result<()>>> {
		let route = self.state.read();
		route.phase.terminal().then(|| route.phase.end())
	}

	/// Whether a front may still replace the copy: the track is open, or finished while
	/// its front still serves it.
	fn moves(&self) -> bool {
		self.state.read().phase.moves()
	}
}

/// Ranges a copy resolved, for a reader that holds no groups of its own: each start names the
/// group the copy resolved it to, so it is that group's start, not a time to look for among
/// groups held.
struct Placed;

impl Holdings for Placed {
	fn resolves(&self) -> bool {
		false
	}

	fn containing(&self, time: u64, _: Duration) -> Containing {
		Containing::Group(time)
	}

	fn next_start(&self, _: u64) -> Option<u64> {
		None
	}

	fn complete(&self) -> bool {
		true
	}
}

/// The time start of `range` that a copy asked from `floor` raised to it, if any: that copy's
/// resolution says nothing of which group holds it, which only a copy asked from below it
/// places. Mirrors the JS `raised`.
fn raised(range: &track::Range, floor: Option<u64>) -> Option<u64> {
	match (floor, range.start) {
		(Some(floor), track::Start::Time(time)) if time < floor => Some(time),
		_ => None,
	}
}

/// Whether `copy` lost its route, so an error read from it is no verdict on what it carried.
/// A terminal track verdict, unlike a delivery failure, still judges its content.
fn copy_failed(copy: &track::Consumer) -> bool {
	matches!(copy.poll_complete(&kio::Waiter::noop()), Poll::Ready(Err(err)) if err.retryable())
}

/// A route's copy, as one reader reads it.
struct Copy {
	generation: u64,
	track: track::Consumer,
	sub: Sub,
	/// Where the subscription was asked to start, so demand updates never ask for less.
	floor: Option<u64>,
	/// A backfill asked from at or below a range's time start under the serving copy's resume
	/// point: its floor is where it was asked from, not a resume point of its own, so the group
	/// holding that time, which may lie under it, is read. A backfill of gaps alone owes
	/// nothing under its floor.
	backfill: bool,
	/// Asked only for groups under the serving copy's resume point. They come as fills under
	/// where the track's live feed starts, which [`track::Consumer::may_land`] never waits
	/// for, so a bounded reader waits for what only they may deliver until its own
	/// subscription accounts for it, it fails, or the reader's budget passes.
	fills: bool,
	/// The frames of the group at `floor` the reader already held when this copy was asked,
	/// so it asks for that group from the next frame; see [`Reader::resume_at`].
	resume: Option<Position>,
	/// The reader's ranges resolved, by the reader's key, so clamping never widens one: as
	/// the serving copy had them when this one was subscribed, until this one answers. A
	/// range left out is not resolved. The copy's own subscription keys each range as the
	/// reader does, so a change to the reader's ranges reaches it as an edit of those alone.
	resolved: BTreeMap<u64, Resolved>,
	/// Replaced by a newer route.
	replaced: bool,
	/// Capped when replaced, at the boundary the copy replacing it starts at, or for a
	/// backfill at the groups it fills: nothing past this group is taken from it or asked of
	/// it. Unset while it serves.
	until: Option<Option<u64>>,
	/// The frame of the group at `until` the copy replacing it resumes at, which its wire
	/// subscription stops before, so the two send none of the group twice; see
	/// [`Subscription::cap`]. `None` asks the group whole.
	cap: Option<Position>,
	/// The group channel ran out: cleanly with how many groups had arrived on the copy by
	/// then, since one arriving later is still in flight below its end, which a read still
	/// open takes.
	done: Option<Result<usize>>,
	/// When a newer group superseded every group under the cap: once the serving copy holds a
	/// group past it or drained, or for a backfill, when it was asked. What the reader still
	/// lacks from it is waited for from here, for no longer than the reader's budget, so the
	/// boundary group, the latest, is never past it.
	since: Option<Instant>,
	/// Held by every group handed out from this copy, so a replaced copy stays subscribed
	/// while one is still read from it.
	lease: Arc<()>,
	/// Whether it may still deliver a group the reader lacks, as [`Reader::let_go`] last
	/// judged: a drop under its bound waits for it.
	feeds: bool,
}

// Boxed: a copy holds one for its whole life, and neither arm should pad every copy.
enum Sub {
	Pending(Box<kio::Pending<track::Subscribing>>),
	Ready(Box<track::Subscriber>),
}

/// Where a copy is asked from: no earlier than `floor`, up to the exclusive `end`, and the
/// group at `floor` from past the frames held of it.
struct Span {
	floor: Option<u64>,
	end: Option<u64>,
	resume: Option<Position>,
}

impl Copy {
	/// The parts of `groups` under this copy's bound that its publisher declared skipped:
	/// through its subscriber once ready, which for another front's track counts the routes
	/// that front replaced. One bound whether the copy is held or let go.
	fn skipped_in(&self, groups: std::ops::Range<u64>, waiter: &kio::Waiter) -> RangeSet<u64> {
		let groups = groups.start..groups.end.min(self.end().unwrap_or(u64::MAX));
		if groups.is_empty() {
			return RangeSet::new();
		}
		match &self.sub {
			Sub::Ready(sub) => sub.skipped_in(groups, waiter),
			Sub::Pending(_) => self.track.skipped_in(groups, waiter),
		}
	}

	/// Whether a group in `groups` may still land on this copy; see
	/// [`track::Consumer::may_land`]. Through its subscriber once ready, which for another
	/// front's track asks every copy that front still keeps.
	fn may_land(
		&self,
		groups: std::ops::Range<u64>,
		floor: bool,
		grace: Duration,
		delivered: Option<&Delivered>,
		waiter: &kio::Waiter,
	) -> bool {
		match &self.sub {
			Sub::Ready(sub) => sub.may_land(groups, floor, grace, delivered, waiter),
			Sub::Pending(_) => self.track.may_land(groups, floor, grace, delivered, waiter),
		}
	}

	/// Subscribe to `track` for `reader`, from `floor` up to the exclusive `end`, each range
	/// clamped by where `resolved` places it, keyed as the reader keys it.
	fn subscribe(track: &track::Consumer, reader: &Mirrored, at: Span, resolved: &BTreeMap<u64, Resolved>) -> Sub {
		// The wire request carries the floor from the first message: some sessions read the
		// demand only once.
		let mut wire = reader.wire(at.floor, at.end, resolved);
		wire.resume = at.resume;
		let pending = track.subscribe(wire);
		// Keyed by position as subscribed: the reader's own keys from then on.
		if !reader.ranges.keys().copied().eq(0..reader.ranges.len() as u64) {
			let _ = pending.preferences().rekey(reader.ranges.keys().copied());
		}
		// Each live join as the reader added it, so an answer the copy recorded since places
		// it, even one recorded before this subscription.
		let _ = pending
			.preferences()
			.stamp(reader.since.iter().map(|(key, since)| (*key, *since)));
		Sub::Pending(Box::new(pending))
	}

	/// The copy's own subscription preferences.
	fn preferences(&self) -> &track::Preferences {
		match &self.sub {
			Sub::Pending(pending) => pending.preferences(),
			Sub::Ready(sub) => sub.preferences(),
		}
	}

	/// The first group past where this copy was replaced, if any: one replaced before
	/// any group ends at 0, and one replaced at `u64::MAX` has no end.
	fn end(&self) -> Option<u64> {
		self.until
			.and_then(|last| last.map_or(Some(0), |last| last.checked_add(1)))
	}

	/// Mirror `change` to the reader's preferences, which now read as `reader`.
	fn update(&mut self, reader: &Mirrored, change: &Change) {
		self.reopen();
		match change {
			Change::Scalars => self.send(reader, []),
			Change::Edit(keys) => {
				let added: Vec<(u64, track::Range)> = keys
					.iter()
					.filter_map(|key| Some((*key, *reader.ranges.get(key)?)))
					.collect();
				for key in keys {
					self.resolved.remove(key);
				}
				// Added ranges resolve against this copy's cache now, as any subscriber's do;
				// a copy still unanswered resolves them once it answers.
				if let Sub::Ready(sub) = &mut self.sub {
					let subscription = reader.with_ranges(added.iter().map(|(_, range)| *range).collect());
					for ((key, _), resolved) in added.iter().zip(sub.resolve_ranges(&subscription)) {
						if let Some(resolved) = resolved {
							self.resolved.insert(*key, resolved);
						}
					}
				}
				self.send(reader, keys.iter().copied());
			}
			Change::All => {
				// Replaced ranges resolve against this copy's cache now, as any subscriber's
				// do, and the copy resolves them all again too.
				self.resolved = match &mut self.sub {
					Sub::Ready(sub) => keyed_resolution(
						reader.ranges.keys().copied(),
						sub.resolve_ranges(&reader.subscription()),
					),
					Sub::Pending(_) => BTreeMap::new(),
				};
				self.ask_whole(reader);
			}
		}
	}

	/// Ask for every range as [`Self::wire`] has it, and the resume, in one replacement.
	fn ask_whole(&mut self, reader: &Mirrored) {
		let mut wire = reader.wire(self.floor, self.end(), &self.resolved);
		wire.resume = self.resume;
		wire.cap = self.cap;
		// Each live join as the reader added it, keyed by position as replaced, noted before the
		// copy's window places it: rekeying below carries it to the reader's own keys.
		let since = (0..)
			.zip(reader.ranges.keys())
			.filter_map(|(position, key)| Some((position, *reader.since.get(key)?)));
		let _ = match &mut self.sub {
			Sub::Pending(pending) => pending.update(wire).and_then(|_| pending.preferences().stamp(since)),
			Sub::Ready(sub) => sub.update_stamped(wire, since),
		};
		// Keyed by position as replaced: the reader's own keys from then on.
		let keyed = self
			.preferences()
			.read()
			.ranges()
			.map(|(key, _)| key)
			.eq(reader.ranges.keys().copied());
		if !keyed {
			let _ = self.preferences().rekey(reader.ranges.keys().copied());
		}
	}

	/// Take nothing past group `until` from this replaced copy, and ask for nothing past it on
	/// the wire, nor past `stop` in it: the copy replacing it starts there. A cap only narrows
	/// what it may deliver, so a copy that ran out stays out: reopened, it would be waited on
	/// with nothing left to wake the reader.
	///
	/// The rest of the group at `until` comes from the copy replacing it: a read of it here
	/// stalls at `stop`, and its [`Recover`] moves to that copy.
	fn cap(&mut self, until: u64, stop: Option<Position>, reader: &Mirrored) {
		let cap = stop.filter(|stop| stop.group == until && stop.frame != 0);
		let narrower = match self.until {
			None => true,
			Some(None) => false,
			Some(Some(capped)) => {
				until < capped || until == capped && cap.is_some_and(|cap| self.cap.is_none_or(|capped| cap < capped))
			}
		};
		if !narrower {
			return;
		}
		self.until = Some(Some(until));
		// Its own resolution, which capping its end must not move.
		if let Sub::Ready(sub) = &mut self.sub {
			let keys = reader.ranges.keys().copied();
			self.resolved = keyed_resolution(reader.ranges.keys().copied(), sub.resolved_keys(keys));
		}
		match cap == self.cap {
			true => self.send(reader, reader.ranges.keys().copied()),
			// An edit carries no cap, so a new one is asked whole.
			false => {
				self.cap = cap;
				self.ask_whole(reader);
			}
		}
	}

	/// Read this copy again: one that ran out under the reader's old ranges, budget, or
	/// bounds may hold a group the new ones admit, and runs out again at once otherwise. A
	/// copy that failed stays failed.
	fn reopen(&mut self) {
		if matches!(self.done, Some(Ok(_))) {
			self.done = None;
		}
	}

	/// The range keyed `key` as this copy is asked for it: the reader's own, from no earlier
	/// than where it was asked to start, and through no later than where it was replaced.
	fn wire(&self, reader: &Mirrored, key: u64) -> Option<track::Range> {
		let range = reader.ranges.get(&key)?;
		Some(range.clamped(self.floor, self.end(), self.resolved.get(&key).copied()))
	}

	/// Ask for the reader's current priority, delay, and order, and for the ranges keyed in
	/// `keys` as [`Self::wire`] has them, answered or not: an edit of the ranges that moved.
	fn send(&mut self, reader: &Mirrored, keys: impl IntoIterator<Item = u64>) {
		let (mut retire, mut add) = (Vec::new(), Vec::new());
		{
			let held = self.preferences().read();
			// A feed an edit can't carry is asked whole, which keys the ranges by position.
			if held.feed != reader.feed {
				drop(held);
				return self.update(reader, &Change::All);
			}
			for key in keys {
				let wire = self.wire(reader, key);
				if wire == held.range(key) {
					continue;
				}
				match wire {
					Some(range) => add.push((key, range)),
					None => retire.push(key),
				}
			}
		}
		let edit = track::Edit {
			retire,
			add,
			priority: reader.priority,
			delay: reader.delay,
			order: reader.order,
		};
		// Each live join as the reader added it, noted before the copy's window places it, so
		// an answer the copy recorded since places it.
		let since = reader.since.iter().map(|(key, since)| (*key, *since));
		let _ = match &mut self.sub {
			Sub::Pending(pending) => pending
				.preferences()
				.edit(edit)
				.and_then(|_| pending.preferences().stamp(since)),
			Sub::Ready(sub) => sub.edit_stamped(edit, since),
		};
	}

	/// The subscription, once the copy answered. `Err` once it refused.
	fn poll_ready(&mut self, groups: (u64, Option<u64>), waiter: &kio::Waiter) -> Poll<Result<&mut track::Subscriber>> {
		if let Sub::Pending(pending) = &self.sub {
			let sub = ready!(pending.poll_ok(waiter))?;
			// An anchor the wire kept may resolve below where the reader got to.
			// The routes judge where the reader ends, not each copy.
			let sub = sub
				.with_range((
					Bound::Included(self.start(groups.0)),
					groups.1.map_or(Bound::Unbounded, Bound::Excluded),
				))
				.parked();
			self.sub = Sub::Ready(Box::new(sub));
		}
		let Sub::Ready(sub) = &mut self.sub else { unreachable!() };
		Poll::Ready(Ok(sub))
	}

	/// The local floor `start` as this copy applies it: never below its own resume point,
	/// since groups under it were handed out by an earlier route, or never owed. A backfill
	/// has none: the reader's window and what it handed out decide what it takes.
	fn start(&self, start: u64) -> u64 {
		match self.floor {
			Some(floor) if !self.backfill => start.max(floor),
			_ => start,
		}
	}

	/// Whether `within` passed since this copy was superseded; see [`Self::since`]. Parks
	/// `waiter` until it has, so a reader waiting on the copy looks again then.
	fn expired(&self, within: Duration, waiter: &kio::Waiter) -> bool {
		self.since.is_some_and(|since| {
			since
				.checked_add(within)
				.is_some_and(|at| self.track.poll_wall(at, waiter).is_ready())
		})
	}

	/// How a reader still needs this copy, once replaced.
	fn retains(&mut self, reader: &Retain<'_>) -> Retains {
		// Only groups keep a copy: a reader that never read one keeps none, whatever its budget.
		if !reader.groups_read {
			return Retains::No;
		}
		// Superseded once the serving copy has a group past the cap.
		if self.since.is_none()
			&& let Some(end) = self.end()
			&& reader.edge.is_some_and(|edge| edge >= end)
		{
			self.since = Some(self.track.now());
		}
		let budget = reader.mirrored.budget();
		let grace = ledger::grace(budget);
		// What is under the bound is past the reader's budget a full budget after a newer
		// group superseded it, landed or not.
		if self.done.is_none()
			&& !self.expired(budget, reader.waiter)
			&& let Some(end) = self.end()
		{
			let start = reader.groups.0;
			// From where it was asked to start, or else from what it first got. A backfill
			// owes what the reader's window hands out below its floor too.
			let floor = self.floor;
			let from = self.start(start);
			// A group still to land is waited for, as a gap is.
			if reader
				.window
				.runs(from)
				.take_while(|run| *run.start() < end)
				.any(|run| {
					let end = run.end().saturating_add(1).min(end);
					self.may_land(
						*run.start()..end,
						floor.is_some(),
						grace,
						Some(reader.delivered),
						reader.waiter,
					)
				}) {
				return Retains::Wait;
			}
			// A fill still owes what the reader owes under its bound and its own feed has not
			// accounted for: unanswered, it may land.
			if self.fills {
				let lacking: Vec<std::ops::Range<u64>> = (reader.owed.overlapping(&(from..end)))
					.map(|run| run.start.max(from)..run.end.min(end))
					.collect();
				let owes = !lacking.is_empty()
					&& match self.poll_ready(reader.groups, &kio::Waiter::noop()) {
						Poll::Pending => true,
						Poll::Ready(Ok(sub)) => !sub.accounts_for(&[reader.delivered], &lacking, false, reader.waiter),
						Poll::Ready(Err(_)) => false,
					};
				if owes {
					return Retains::Wait;
				}
			}
			// Inspect without moving either cursor: a later group poll still chooses its
			// order and applies the current bounds and drift budget. Ignore the local cap
			// here, since unread groups become eligible again when it rises.
			if let Poll::Ready(Ok(sub)) = self.poll_ready(reader.groups, &kio::Waiter::noop())
				&& sub.has_unread_group(&track::Unread {
					start,
					end: Some(end),
					delivered: reader.delivered,
				}) {
				return Retains::Wait;
			}
		}
		// A group still read from it, or that it holds or may yet get within the grace a late
		// group is waited for: a failure of the copy that group is read from continues here.
		let lands = !self.expired(grace, reader.waiter);
		let backs = |key: u64| {
			// A group at or past where it was asked to start may land even before its first
			// arrival; without a floor, only what follows its first arrival can.
			let asked = self.floor.is_some_and(|floor| self.backfill || floor <= key);
			self.track.peek_group(key).is_some()
				|| (lands && self.may_land(key..key.saturating_add(1), asked, grace, None, reader.waiter))
		};
		match Arc::strong_count(&self.lease) > 1 || reader.out.iter().any(|(key, _)| backs(*key)) {
			true => Retains::Keep,
			false => Retains::No,
		}
	}
}

/// What [`Copy::retains`] judges a replaced copy by: its reader, as it lets copies go.
struct Retain<'a> {
	groups: (u64, Option<u64>),
	window: &'a Window,
	groups_read: bool,
	/// The newest group the serving copy has.
	edge: Option<u64>,
	mirrored: &'a Mirrored,
	delivered: &'a Delivered,
	out: &'a [(u64, Weak<()>)],
	/// What a bounded reader lacks under the serving copy's resume point that only a fill may
	/// still deliver: no copy holds it, no other replaced copy still feeds it, and no copy
	/// declared it skipped. Empty for an unbounded reader, which no gap ends early.
	owed: &'a RangeSet<u64>,
	waiter: &'a kio::Waiter,
}

/// How a reader still needs a replaced copy; see [`Copy::retains`].
enum Retains {
	/// It may still deliver a group the reader lacks.
	Wait,
	/// It backs a group the reader handed out.
	Keep,
	/// Not at all.
	No,
}

/// One reader's subscription to a logical track; see the module docs.
pub(super) struct Subscriber {
	/// Shared with the groups it hands out, so one stuck on a dead route can follow the
	/// reader onto the next route without the reader polling for groups.
	reader: Arc<Mutex<Reader>>,
	subscription: track::Preferences,
	/// The group read that last parked, answered again without the walk until something it
	/// parked on fires, once it first parks; see [`super::idle`].
	idle: Option<Box<Idle<Walk>>>,
}

impl Subscriber {
	fn reader(&self) -> MutexGuard<'_, Reader> {
		self.reader.lock().unwrap_or_else(PoisonError::into_inner)
	}

	/// The reader's preferences channel, shared with its [`track::Control`].
	pub(super) fn subscription(&self) -> &track::Preferences {
		&self.subscription
	}

	/// A handle that mirrors the reader's preferences into its copies; see [`Mirror`].
	pub(super) fn mirror(&self) -> Mirror {
		Mirror(Arc::downgrade(&self.reader))
	}
}

/// Mirrors a reader's changed preferences into its copies at once, so the change reaches
/// their sources without waiting for the reader's next poll: a reader that demotes a track
/// it has stopped reading would otherwise keep its old priority upstream.
#[derive(Clone)]
pub(super) struct Mirror(Weak<Mutex<Reader>>);

impl Mirror {
	pub(super) fn sync(&self) {
		if let Some(reader) = self.0.upgrade() {
			reader
				.lock()
				.unwrap_or_else(PoisonError::into_inner)
				.sync(&kio::Waiter::noop());
		}
	}
}

impl Subscriber {
	/// Reserve the lowest next group not in `delivered` across copies, without handing it
	/// out: in arrival order, or in key order for `ordered`.
	/// Unless it `parks`, a bounded reader ends once its window has nothing left on any copy;
	/// see [`Reader::exhausted`].
	///
	/// A read that parked is answered again with `Pending` until something it parked on
	/// fires: the reader's preferences, the route, a copy's answer, its cursor, or a wall
	/// deadline. What the reader changes itself (its range, what it handed out) disarms it.
	/// Another handle's read that follows the route or the preferences first only applies
	/// a change that already woke this one.
	///
	/// An `ordered` read that ends passes every key below its window's end, or its cap below
	/// that, in `delivered` for good, as [`track::Cursor::pass_end`] does: a group created
	/// there later is late. The end is the one the read ended at, taken under the same lock,
	/// so a range widened meanwhile is still read.
	pub(super) fn poll_peek_group(
		&mut self,
		ordered: bool,
		parks: bool,
		delivered: &mut Delivered,
		waiter: &kio::Waiter,
	) -> Poll<Result<Option<Candidate>>> {
		let walk = Walk::new(ordered);
		if self.parked(walk, waiter) {
			return Poll::Pending;
		}
		let mut idle = Idle::start(&mut self.idle, waiter);
		let polled = {
			let waiter = idle.waiter();
			let mut reader = self.reader();
			// Registered before looking, so a route change from here on wakes the read,
			// even on a path that parks without looking at the route.
			reader.route.register(waiter);
			let polled = loop {
				if let Some(result) = reader.poll_lowest(ordered, parks, delivered, waiter) {
					break result;
				}
			};
			// Below the cap: a read that ran out under a lowered cap, its window or the track
			// having ended, still owes what lies above it. A read that ended leaves the gate
			// unarmed, so what this passes needs no disarm.
			let cap = reader.groups.1;
			let window = reader.window();
			let end = window.end().filter(|_| !window.is_pending());
			if let (true, Poll::Ready(Ok(None)), Some(end)) = (ordered, &polled, end) {
				delivered.forget_below(cap.map_or(end, |cap| cap.min(end)));
			}
			polled
		};
		idle.finish(&mut self.idle, walk, polled.is_pending());
		polled
	}

	/// Whether a group read in `walk` order would park again at once; see
	/// [`Self::poll_peek_group`].
	pub(super) fn parked(&self, walk: Walk, waiter: &kio::Waiter) -> bool {
		Idle::parked(&self.idle, walk, waiter)
	}

	/// Forget the parked group read: what the reader handed out changed, which nothing
	/// notifies.
	pub(super) fn disarm(&mut self) {
		Idle::disarm(&mut self.idle);
	}

	/// The parts of `groups` this reader knows were skipped: by any copy it holds, or one it
	/// let go. A skip is final for the whole track, so an outer front reading this one as a
	/// copy keeps what a route this front replaced declared.
	pub(super) fn skipped_in(&self, groups: std::ops::Range<u64>, waiter: &kio::Waiter) -> RangeSet<u64> {
		self.reader().skipped_in(groups, waiter)
	}

	/// Whether this reader accounts for every key of `gaps`, which none of the readers that
	/// handed out `taken` did, the first of which is its own; see [`Reader::accounts`].
	pub(super) fn accounts(
		&mut self,
		taken: &[&Delivered],
		gaps: &[std::ops::Range<u64>],
		ordered: bool,
		waiter: &kio::Waiter,
	) -> bool {
		let mut reader = self.reader();
		!reader.let_go(taken[0], waiter) && reader.accounts(taken, gaps, ordered, waiter)
	}

	/// Hand out the group a [`Self::poll_peek_group`] reserved.
	pub(super) fn take(&mut self, candidate: Candidate) -> Result<group::Consumer> {
		self.disarm();
		self.reader().take(candidate)
	}

	/// The keys that landed on any copy since the last call, below `below`; see
	/// [`track::Subscriber::note_late`].
	pub(super) fn late(&mut self, below: u64) -> Vec<u64> {
		let mut reader = self.reader();
		reader
			.copies
			.iter_mut()
			.filter_map(|copy| match &mut copy.sub {
				Sub::Ready(sub) => Some(sub.late(below)),
				Sub::Pending(_) => None,
			})
			.flatten()
			.collect()
	}

	/// Forget the gaps in `delivered` below each group past the reader's budget, judged on
	/// the newest copy that answered; see [`track::Subscriber::forget_past_budget`].
	pub(super) fn forget_past_budget(&self, delivered: &mut Delivered) {
		let reader = self.reader();
		let mut ready = reader.copies.iter().rev().filter_map(|copy| match &copy.sub {
			Sub::Ready(sub) => Some(sub),
			Sub::Pending(_) => None,
		});
		if let Some(sub) = ready.next() {
			sub.forget_past_budget(delivered);
		}
	}

	/// Poll for the track's declared exclusive end.
	pub(super) fn poll_finished(&mut self, waiter: &kio::Waiter) -> Poll<Result<Position>> {
		self.reader().poll_finished(waiter)
	}

	/// Poll for where the serving copy's live feed starts; see
	/// [`track::Subscriber::poll_start`].
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(super) fn poll_start(&mut self, waiter: &kio::Waiter) -> Poll<Option<u64>> {
		let mut reader = self.reader();
		let sub = ready!(reader.poll_serving(waiter));
		match sub {
			Some(sub) => sub.poll_start(waiter),
			None => Poll::Ready(None),
		}
	}

	/// The lowest group the reader's ranges name; see [`track::Subscriber::floor`].
	pub(super) fn floor(&self) -> Option<u64> {
		self.reader().window().start()
	}

	/// The reader's ranges keyed in `keys` resolved; see [`track::Subscriber::resolved_keys`].
	pub(super) fn resolved_keys(&self, keys: impl IntoIterator<Item = u64>) -> Vec<Option<Resolved>> {
		let mut reader = self.reader();
		let window = reader.window();
		keys.into_iter().map(|key| window.resolved_key(key)).collect()
	}

	/// Whether a group in `groups` may still land on a copy the reader keeps, past where that
	/// copy starts and under its cap; see [`track::Subscriber::may_land`].
	pub(super) fn may_land(
		&self,
		groups: std::ops::Range<u64>,
		floor: bool,
		grace: Duration,
		delivered: Option<&Delivered>,
		waiter: &kio::Waiter,
	) -> bool {
		self.reader().copies.iter().any(|copy| {
			let groups = copy.start(groups.start)..groups.end.min(copy.end().unwrap_or(u64::MAX));
			!groups.is_empty() && copy.may_land(groups, floor, grace, delivered, waiter)
		})
	}

	/// The reader's ranges keyed in `keys` while their time starts wait; see
	/// [`track::Subscriber::waiting_keys`].
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(super) fn waiting_keys(&self, keys: impl IntoIterator<Item = u64>) -> Vec<Option<(u64, Option<u64>)>> {
		let mut reader = self.reader();
		reader.window();
		let reader = &mut *reader;
		// Only the serving copy places one: the reader's own window holds no groups. Its
		// subscription keys each range as the reader does.
		let indices: Vec<Option<u64>> = keys
			.into_iter()
			.map(|key| match reader.window.resolved_key(key) {
				Some(_) => None,
				None => reader.mirrored.ranges.contains_key(&key).then_some(key),
			})
			.collect();
		let mut found = match reader.copies.last_mut().map(|copy| &mut copy.sub) {
			Some(Sub::Ready(sub)) => sub.waiting_keys(indices.iter().flatten().copied()).into_iter(),
			_ => Vec::new().into_iter(),
		};
		indices
			.into_iter()
			.map(|index| index.and_then(|_| found.next().flatten()))
			.collect()
	}

	/// The lowest group at or above `from` the reader's ranges cover; see
	/// [`track::Subscriber::next_in_range`].
	pub(super) fn next_in_range(&self, from: u64) -> Option<u64> {
		let mut reader = self.reader();
		reader.window();
		// The serving route's copy holds what its publisher skipped or gave up.
		let serving = reader.copies.last().map(|copy| copy.track.clone());
		track::first_open(reader.window.runs(from), |key| {
			serving.as_ref().and_then(|track| track.hole_end(key, true))
		})
	}

	/// Whether the serving route's copy holds every group of `run` declared skipped; see
	/// [`track::Subscriber::is_skipped`].
	pub(super) fn is_skipped(&self, run: impl Iterator<Item = std::ops::RangeInclusive<u64>>) -> bool {
		let serving = self.reader().copies.last().map(|copy| copy.track.clone());
		let Some(serving) = serving else {
			return false;
		};
		track::first_open(run, |key| serving.hole_end(key, false)).is_none()
	}

	/// Where a run of starts up to `end` ends with the span the serving copy knows it reaches
	/// into; see [`track::Subscriber::span_end`].
	pub(super) fn span_end(&self, end: u64) -> u64 {
		let serving = self.reader().copies.last().map(|copy| copy.track.clone());
		serving.map_or(end, |serving| serving.span_end(end))
	}

	/// When the latest group any copy caches at `keys` landed: a group a replaced copy
	/// delivered landed all the same.
	pub(super) fn landed(&self, keys: std::ops::Range<u64>) -> Option<crate::time::Instant> {
		let copies: Vec<_> = self.reader().copies.iter().map(|copy| copy.track.clone()).collect();
		copies.iter().filter_map(|track| track.landed(keys.clone())).max()
	}

	/// The groups the reader's ranges name from `from` on; see
	/// [`track::Subscriber::runs_in_range`].
	pub(super) fn runs(&self, from: u64) -> Vec<std::ops::RangeInclusive<u64>> {
		self.reader().window().runs(from).collect()
	}

	/// Whether `accounted` holds every group the reader's bounded ranges name from `from`
	/// up to `cap`; see [`track::Subscriber::covered`].
	pub(super) fn covered(&self, from: u64, cap: Option<u64>, accounted: &impl Accounted) -> bool {
		let mut reader = self.reader();
		reader.window();
		reader.window.covered_by(from, cap, accounted)
	}

	/// Poll for keys the serving copy gave up that no copy may still deliver; see
	/// [`track::Subscriber::poll_drop`].
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(super) fn poll_drop(
		&mut self,
		delivered: &Delivered,
		probe: bool,
		waiter: &kio::Waiter,
	) -> Poll<(std::ops::Range<u64>, track::Hole)> {
		self.reader().poll_drop(delivered, probe, waiter)
	}

	/// Assign the local read floor and exclusive cap; see [`track::Subscriber::with_range`].
	/// `delivered` says which groups a lowered floor newly owes.
	pub(super) fn set_range(&mut self, start: u64, end: Option<u64>, delivered: &Delivered) {
		// A moved range notifies nothing a parked read waits on.
		self.disarm();
		self.reader().set_range(start, end, delivered)
	}

	/// Whether any subscribed copy still has a group matching `unread`.
	pub(super) fn has_unread_group(&self, unread: &track::Unread<'_>) -> bool {
		self.reader().copies.iter().any(|copy| match &copy.sub {
			Sub::Ready(sub) => sub.has_unread_group(unread),
			Sub::Pending(_) => false,
		})
	}

	/// The newest key any copy has.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(super) fn latest(&self) -> Option<u64> {
		self.reader().latest()
	}

	/// The longest run of keys just below `key` any copy knows were never produced.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(super) fn skipped_below(&self, key: u64) -> u64 {
		let reader = self.reader();
		reader
			.copies
			.iter()
			.map(|copy| copy.track.skipped_below(key))
			.max()
			.unwrap_or(0)
	}

	/// Whether `position` is the serving copy's largest and its group ended there.
	pub(super) fn largest_finished(&self, position: Position) -> bool {
		self.reader().route.largest_finished(position)
	}

	/// Poll for a largest location within `bounds` newer than `seen`; see [`Consumer::poll_largest`].
	pub(super) fn poll_largest(
		&mut self,
		seen: &mut Option<Position>,
		bounds: track::Bounds,
		waiter: &kio::Waiter,
	) -> Poll<Option<Position>> {
		let route = self.reader().route.clone();
		route.poll_largest(seen, bounds, waiter)
	}

	/// The final key and the error the logical track ended with, once the serving copy
	/// completed and its groups drained to that end.
	pub(super) fn end_error(&self) -> Option<(u64, Error)> {
		let reader = self.reader();
		let serving = reader
			.copies
			.last()
			.filter(|copy| Some(copy.generation) == reader.generation)?;
		if !matches!(serving.done, Some(Ok(_)))
			|| !matches!(serving.track.poll_complete(&kio::Waiter::noop()), Poll::Ready(Ok(())))
		{
			return None;
		}
		Some((reader.declared?.final_key()?, reader.declared_error.clone()?))
	}

	/// The error the logical track's declared end carries, reached or not.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(super) fn declared_error(&self) -> Option<Error> {
		let mut reader = self.reader();
		reader.learn_end(None);
		reader.declared_error.clone()
	}

	/// The local group filter, as [`track::Subscriber::poll_largest`] reports within it.
	pub(super) fn bounds(&self) -> track::Bounds {
		let mut reader = self.reader();
		reader.sync(&kio::Waiter::noop());
		let (start, end) = reader.groups;
		// Requested ranges move independently of the explicit local cursor bounds.
		let start = start.max(reader.window().floor().unwrap_or(0));
		track::Bounds { start, end }
	}

	/// The groups the copies' drift budget skipped since the last call, counted against
	/// the logical track: the copies themselves are untagged.
	pub(super) fn take_stale(&mut self) -> stats::Content {
		let mut stale = stats::Content::default();
		for copy in &mut self.reader().copies {
			if let Sub::Ready(sub) = &mut copy.sub {
				stale.add(sub.take_stale());
			}
		}
		stale
	}

	/// Poll for the serving copy's cache reflecting its live feed; see
	/// [`track::Subscriber::poll_live`].
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(super) fn poll_live(&mut self, waiter: &kio::Waiter) -> Poll<Option<Position>> {
		let mut reader = self.reader();
		let sub = ready!(reader.poll_serving(waiter));
		match sub {
			Some(sub) => sub.poll_live(waiter),
			None => Poll::Ready(None),
		}
	}
}

/// A group a [`Reader`] reserved on one of its copies.
pub(super) struct Candidate {
	/// Its copy's lease, which names that copy: one route can back several. A route may
	/// change before the caller takes this candidate, so it also keeps that exact copy
	/// alive until then.
	lease: Arc<()>,
	/// The copy's own reservation, which taking hands out whatever the copy's cursor did
	/// since: a route returning to it moves its bounds.
	reserved: track::Reserved,
}

impl Candidate {
	pub(super) fn key(&self) -> u64 {
		self.reserved.key()
	}

	pub(super) fn ordered(&self) -> bool {
		self.reserved.ordered()
	}
}

/// Where every reader resumes the newest group it handed out, past the frames a copy holds
/// of it: the lowest of those and the frames, folded in one reader at a time so only the
/// lowest head is kept. `None` unless every reader resumes, as in a track's own demand.
#[derive(Default)]
enum Resume {
	#[default]
	Empty,
	Lowest(Position, Arc<group::Head>),
	/// A reader resumes nothing, so neither does the aggregate.
	Whole,
}

impl Resume {
	fn add(&mut self, reader: &Reader) {
		if matches!(self, Self::Whole) {
			return;
		}
		*self = match (std::mem::take(self), reader.held_at(reader.boundary())) {
			(_, None) => Self::Whole,
			(Self::Lowest(at, head), Some((next, _))) if at <= next => Self::Lowest(at, head),
			(_, Some((next, head))) => Self::Lowest(next, head),
		};
	}

	fn lowest(self) -> Option<(Position, Arc<group::Head>)> {
		match self {
			Self::Lowest(at, head) => Some((at, head)),
			_ => None,
		}
	}
}

/// A subscription on a copy asking for its readers' demand, as [`Producer::subscription`]
/// folds it, kept current by [`Producer::follow`].
pub(super) struct Held {
	sub: kio::Pending<track::Subscribing>,
	/// How the subscription's ranges map to the readers' once every reader is here, so a
	/// reader's change reaches it as an edit. `None` while it was last asked whole.
	fold: Option<Fold>,
}

impl Held {
	pub(super) fn new(sub: kio::Pending<track::Subscribing>) -> Self {
		Self { sub, fold: None }
	}

	/// Resolve the copy's info with this subscription registered.
	pub(super) fn info(&self) -> track::Resolving {
		self.sub.info()
	}
}

/// Each reader's ranges as [`Held`] asks for them: by reader, the revision of its preferences
/// folded and the key each of its ranges has in the held subscription.
#[derive(Default)]
struct Fold {
	readers: std::collections::HashMap<usize, Folded>,
	/// The next key the held subscription gives a range.
	next: u64,
	/// The feed the held subscription was asked with, which only asking it whole changes.
	feed: bool,
}

struct Folded {
	/// Keeps the reader's allocation, so no other reader takes its address, the entry's key,
	/// until the entry leaves.
	_reader: Weak<Mutex<Reader>>,
	revision: u64,
	/// The held key of each of the reader's ranges, by the reader's key.
	keys: BTreeMap<u64, u64>,
}

impl Fold {
	/// Fold in every range of `reader`, or the ranges keyed `keys` it still holds, returning
	/// each under the held key it now has.
	fn fold(&mut self, reader: &Arc<Mutex<Reader>>, keys: Option<BTreeSet<u64>>) -> Vec<(u64, track::Range)> {
		let mut locked = reader.lock().unwrap_or_else(PoisonError::into_inner);
		self.fold_locked(reader, &mut locked, keys)
	}

	/// [`Self::fold`] with `reader` already locked as `locked`.
	fn fold_locked(
		&mut self,
		reader: &Arc<Mutex<Reader>>,
		locked: &mut Reader,
		keys: Option<BTreeSet<u64>>,
	) -> Vec<(u64, track::Range)> {
		let (preferences, revision) = {
			let current = locked.subscription.read();
			let preferences: Vec<_> = match &keys {
				Some(keys) => keys
					.iter()
					.filter_map(|key| Some((*key, current.range(*key)?)))
					.collect(),
				None => current.ranges().collect(),
			};
			(preferences, current.revision)
		};
		let resumed = locked.resumed(&preferences);
		let folded = self
			.readers
			.entry(Arc::as_ptr(reader) as usize)
			.or_insert_with(|| Folded {
				_reader: Arc::downgrade(reader),
				revision,
				keys: BTreeMap::new(),
			});
		folded.revision = revision;
		resumed
			.into_iter()
			.map(|(key, range)| {
				let held = self.next;
				self.next += 1;
				folded.keys.insert(key, held);
				(held, range)
			})
			.collect()
	}

	/// What `reader` changed since it was last folded in: the held keys of the ranges it
	/// retired or replaced, and the ranges it added, under new held keys.
	fn refold(&mut self, reader: &Arc<Mutex<Reader>>) -> (Vec<u64>, Vec<(u64, track::Range)>) {
		let id = Arc::as_ptr(reader) as usize;
		let Some(folded) = self.readers.get_mut(&id) else {
			return (Vec::new(), self.fold(reader, None));
		};
		let changes = {
			let locked = reader.lock().unwrap_or_else(PoisonError::into_inner);
			let current = locked.subscription.read();
			if current.revision == folded.revision {
				return Default::default();
			}
			current.changes(folded.revision)
		};
		let retire = match &changes {
			Some(keys) => keys.iter().filter_map(|key| folded.keys.remove(key)).collect(),
			None => std::mem::take(&mut folded.keys).into_values().collect(),
		};
		(retire, self.fold(reader, changes))
	}
}

/// A reader's preferences as its copies are asked for them, each range by the key the
/// reader's own preferences give it.
#[derive(Clone, Default)]
struct Mirrored {
	priority: i8,
	delay: track::Delay,
	order: track::Order,
	feed: bool,
	ranges: BTreeMap<u64, track::Range>,
	/// The join tick each live join range was added at in the reader's own preferences, which
	/// each copy asked for it places it from: an answer the copy recorded since then.
	since: BTreeMap<u64, u64>,
}

impl Mirrored {
	fn insert(&mut self, key: u64, range: track::Range, since: u64) {
		self.ranges.insert(key, range);
		match range.start {
			track::Start::Live(_) => _ = self.since.insert(key, since),
			track::Start::Oldest | track::Start::Time(_) => _ = self.since.remove(&key),
		}
	}

	fn remove(&mut self, key: u64) {
		self.ranges.remove(&key);
		self.since.remove(&key);
	}

	fn budget(&self) -> Duration {
		self.delay.budget()
	}

	/// These preferences with `ranges` instead.
	fn with_ranges(&self, ranges: Vec<track::Range>) -> Subscription {
		Subscription {
			priority: self.priority,
			delay: self.delay,
			ranges,
			order: self.order,
			feed: self.feed,
			resume: None,
			cap: None,
			asks: Vec::new(),
			joins: Vec::new(),
		}
	}

	/// These preferences, the ranges in key order. Costs every range.
	fn subscription(&self) -> Subscription {
		self.with_ranges(self.ranges.values().copied().collect())
	}

	/// [`Self::subscription`] from no earlier than `floor` and through no later than the
	/// exclusive `end`, each range clamped where `resolved` places it.
	fn wire(&self, floor: Option<u64>, end: Option<u64>, resolved: &BTreeMap<u64, Resolved>) -> Subscription {
		self.with_ranges(
			self.ranges
				.iter()
				.map(|(key, range)| range.clamped(floor, end, resolved.get(key).copied()))
				.collect(),
		)
	}
}

#[cfg(test)]
impl Mirrored {
	/// `subscription`, its ranges keyed by position.
	fn of(subscription: &Subscription) -> Self {
		let mut mirrored = Self {
			priority: subscription.priority,
			delay: subscription.delay,
			order: subscription.order,
			feed: subscription.feed,
			..Default::default()
		};
		for (key, range) in (0..).zip(&subscription.ranges) {
			mirrored.insert(key, *range, 0);
		}
		mirrored
	}
}

#[cfg(test)]
impl Copy {
	/// What this copy is asked for, every range as [`Self::wire`] has it, a ready copy's
	/// resolution its own.
	fn wire_all(&mut self, reader: &Mirrored) -> Subscription {
		if let Sub::Ready(sub) = &mut self.sub {
			let keys = reader.ranges.keys().copied();
			self.resolved = keyed_resolution(keys.clone(), sub.resolved_keys(keys));
		}
		reader.wire(self.floor, self.end(), &self.resolved)
	}
}

/// What changed in a reader's preferences, for its copies to mirror.
enum Change {
	/// Only the priority, delay, order, or feed.
	Scalars,
	/// Those, and the ranges keyed here: each one retired, added, or both.
	Edit(BTreeSet<u64>),
	/// Every range, keyed afresh.
	All,
}

/// Why the serving copy's resume point may pass over groups the reader lacks.
enum Resumed {
	/// The route changed: the floor the copy serving before was asked from, and where it
	/// placed each range, by key.
	Route(Option<u64>, BTreeMap<u64, Resolved>),
	/// The ranges keyed here were added, and may cover groups below the resume point.
	Ranges(BTreeSet<u64>),
}

/// Each of `keys` with what `resolved` holds in its place, leaving out the unresolved.
fn keyed_resolution(keys: impl IntoIterator<Item = u64>, resolved: Vec<Option<Resolved>>) -> BTreeMap<u64, Resolved> {
	keys.into_iter()
		.zip(resolved)
		.filter_map(|(key, resolved)| Some((key, resolved?)))
		.collect()
}

/// A [`Subscriber`]'s state.
struct Reader {
	/// The first exclusive end any copy declared, refined by a matching mid-group end. A copy
	/// declaring another breaks the track's identity, which hurts only its own broadcast, so
	/// nothing checks that copies agree.
	declared: Option<Position>,
	/// The first error a copy's end carried, from a copy at the end [`Self::declared`] holds, kept
	/// with the end so a route switch keeps it.
	declared_error: Option<Error>,
	this: Weak<Mutex<Reader>>,
	route: Consumer,
	/// The reader's own preferences, mirrored into every copy's subscription.
	subscription: track::Preferences,
	mirrored: Mirrored,
	/// The preferences' revision [`Self::mirrored`] holds.
	revision: u64,
	/// The route generation last read.
	generation: Option<u64>,
	/// Oldest first, except that a backfill goes before the copy of the route it backfills: the
	/// last one is the serving route's own, when it is subscribed.
	copies: Vec<Copy>,
	/// The newest group handed out: a switch's boundary is never below it.
	newest: Option<u64>,
	/// What copies declared skipped as they were let go, so a key the local floor or cap keeps
	/// out under the serving copy's resume point counts as accounted for after the copy that
	/// knew it is gone; see [`Reader::accounts`] and [`Reader::lacking`]. Only the skipped start
	/// and end of each gap the reader lacks there, where the floor and cap keep keys out, so it
	/// holds at most two runs a gap.
	skipped: RangeSet<u64>,
	/// Set once the serving copy's resume point may pass over groups the reader lacks: the
	/// route changed, or the ranges did. Kept until the gaps are backfilled: see
	/// [`Reader::backfill_gaps`].
	resumed: Option<Resumed>,
	/// The groups a backfill of route `generation` failed to get, which that route is not
	/// asked for again until the route or the reader's preferences change: a backfill it
	/// refuses would only fail again, and asking each poll would spin.
	refused: Option<(u64, std::ops::Range<u64>)>,
	/// The reader's ranges resolved; see [`Reader::window`].
	window: Window,
	/// The local floor and exclusive cap; see [`track::Subscriber::with_range`].
	groups: (u64, Option<u64>),
	/// The groups moved onto another copy, which judge their expiry within [`Self::groups`]'
	/// cap ([`Recover::window`]), woken when it moves. The ranges wake them through
	/// [`Self::subscription`].
	capped: kio::WaiterList,
	/// Whether groups were ever asked for; until then the reader keeps no replaced copy.
	groups_read: bool,
	/// The groups handed out, by key, held while each is read.
	out: Vec<(u64, Weak<()>)>,
	/// Runs the serving copy gave up that the reader has yet to report; see
	/// [`Reader::poll_drop`].
	drops: RangeMap<u64, track::Hole>,
	/// The part of [`Self::drops`] that is a received stale run the serving copy held, which
	/// a new upstream subscription can supersede. Kept apart so a poll costs these runs, not every queued one.
	received: RangeSet<u64>,
	/// The serving copy's [`track::Subscriber::stale_revision`] when [`Self::received`] was
	/// last checked against it, so a poll rechecks those runs only after one may be gone.
	/// `None` until the serving copy is first checked.
	received_checked: Option<u64>,
}

impl Reader {
	/// The ranges in `preferences`, from the reader's own, each as a new copy is asked for
	/// it: from the boundary a switch splits at, its anchors where the serving copy placed
	/// them.
	fn resumed(&mut self, preferences: &[(u64, track::Range)]) -> Vec<(u64, track::Range)> {
		let (revision, delay) = {
			let current = self.subscription.read();
			(current.revision, current.delay)
		};
		// Whether the reader mirrored these ranges, and so its copies resolved them.
		let current = revision == self.revision;
		let keys = preferences.iter().map(|(key, _)| *key);
		// A failed copy's resolution is no placement, as in `Reader::window`: the reader's
		// own stands, or the anchors do for ranges it has yet to take up.
		let failed = self.route.end().is_none()
			&& self
				.copies
				.last()
				.is_some_and(|copy| matches!(copy.sub, Sub::Ready(_)) && copy_failed(&copy.track));
		let resolved: Vec<Option<Resolved>> = match self.copies.last_mut() {
			_ if failed && current => {
				let window = self.window();
				keys.map(|key| window.resolved_key(key)).collect()
			}
			_ if failed => Vec::new(),
			Some(Copy {
				sub: Sub::Ready(sub), ..
			}) if current => sub.resolved_keys(keys),
			Some(Copy {
				sub: Sub::Ready(sub), ..
			}) => sub.resolve_ranges(&Subscription {
				ranges: preferences.iter().map(|(_, range)| *range).collect(),
				delay,
				..Default::default()
			}),
			Some(copy) if current => keys.map(|key| copy.resolved.get(&key).copied()).collect(),
			_ => Vec::new(),
		};
		// A reader that handed out nothing asks its own ranges as they are, a live join
		// included: the groups its copy holds are no place it read to.
		let floor = self.newest.and(self.boundary());
		preferences
			.iter()
			.enumerate()
			.map(|(index, (key, range))| {
				let resolved = resolved.get(index).copied().flatten();
				(*key, range.clamped(floor, None, resolved))
			})
			.collect()
	}

	/// The reader's ranges resolved, a live join as the first ready copy to answer for them
	/// placed it, whatever its failover cap: that bounds what the copy delivers, not where the
	/// ranges resolve. Kept once resolved, past that copy being let go. Pending while a live
	/// join waits on a copy to place it. Costs the ranges still unsettled.
	fn window(&mut self) -> &Window {
		if self.window.is_settled() {
			return &self.window;
		}
		let concluded = self.route.end().is_some();
		for index in 0..self.copies.len() {
			if self.window.is_settled() {
				break;
			}
			let copy = &mut self.copies[index];
			// A copy whose delivery failed placed a time start past its last group as if nothing
			// followed, but the front's next copy continues the track: only that copy knows
			// which group holds it, until the track concludes.
			if !concluded && copy_failed(&copy.track) {
				continue;
			}
			let (cap, floor) = (copy.end(), copy.floor);
			let Sub::Ready(sub) = &mut copy.sub else {
				continue;
			};
			let keys = self.window.unsettled();
			let mirrored = &self.mirrored;
			// A failover cap at or below where a range starts empties the copy's range, which
			// then says nothing of where the reader's own range starts. One at or below where
			// it ends cut that end short, so it says nothing of where the reader's range ends.
			let resolved: BTreeMap<u64, Resolved> = keys
				.iter()
				.copied()
				.zip(sub.resolved_keys(keys.iter().copied()))
				.filter_map(|(key, resolved)| {
					let range = mirrored.ranges.get(&key)?;
					// A copy asked from above a range's time start says nothing of where it starts:
					// a backfill asked from below places it; see `backfill_gaps`.
					if raised(range, floor).is_some() {
						return None;
					}
					let resolved = resolved?;
					if cap.is_some() && resolved.end.is_some_and(|end| end <= resolved.start) {
						return None;
					}
					let end = resolved.end.filter(|end| cap.is_none_or(|cap| *end < cap));
					Some((key, Resolved { end, ..resolved }))
				})
				.collect();
			self.resolve(&resolved);
		}
		&self.window
	}

	/// The time starts of the unsettled ranges that the serving copy's resume point raised,
	/// by key: no copy asked from there places them, only a backfill from below.
	fn raised(&self) -> BTreeMap<u64, u64> {
		let floor = self.copies.last().and_then(|copy| copy.floor);
		if floor.is_none() || self.window.is_settled() {
			return BTreeMap::new();
		}
		self.window
			.unsettled()
			.into_iter()
			.filter_map(|key| Some((key, raised(self.mirrored.ranges.get(&key)?, floor)?)))
			.collect()
	}

	/// Place each unsettled [`Self::window`] range where a copy resolved it, on its own: one
	/// whose start still waits leaves the rest placed, so a publisher names their starts.
	fn resolve(&mut self, resolved: &BTreeMap<u64, Resolved>) {
		let mut placed = Vec::new();
		for key in self.window.unsettled() {
			let Some(range) = self.mirrored.ranges.get(&key).copied() else {
				continue;
			};
			// A time past 0, or a live join, which only a copy can place.
			let waits = match range.start {
				track::Start::Time(start) => start > 0,
				track::Start::Live(_) => true,
				track::Start::Oldest => false,
			};
			let Some(resolved) = resolved.get(&key).copied() else {
				if !range.is_empty() && waits {
					continue;
				}
				// Placed as it is: nothing of it waits on a copy.
				placed.push((key, range));
				continue;
			};
			let end = match (range.end, resolved.end) {
				(Some(_), Some(end)) => match end.checked_sub(1) {
					Some(last) => Some(last),
					None => {
						placed.push((key, track::Range::EMPTY));
						continue;
					}
				},
				(end, _) => end,
			};
			// One that reached back to the oldest group still does, as on a plain reader.
			let start = match (waits || range.time().is_some(), resolved.floored) {
				(true, true) => track::Start::Time(resolved.start),
				(true, false) => track::Start::Oldest,
				(false, _) => range.start,
			};
			placed.push((key, track::Range { start, end }));
		}
		// An edit restarts the window's coverage check, so one that places nothing is skipped.
		if placed.is_empty() {
			return;
		}
		self.window.edit([], &placed, None, &Placed);
	}

	/// Follow the route: subscribe to a new copy, mark the ones it replaced, and mirror
	/// the reader's preferences into each. A change to the ranges costs the ranges it
	/// touched, in the reader and in each copy alike.
	fn sync(&mut self, waiter: &kio::Waiter) {
		// Learn before a swap or retention drops the only copy that knows the end.
		self.learn_end(None);
		// Snapshot and re-arm together, including when a change was observed: another
		// update must wake a reader that parks after applying this one.
		let mut changed = None;
		let mirrored = &self.mirrored;
		let revision = self.revision;
		let _ = self.subscription.poll_ref(waiter, |sub| {
			let scalars = (sub.priority, sub.delay, sub.order, sub.feed);
			let moved = scalars != (mirrored.priority, mirrored.delay, mirrored.order, mirrored.feed);
			if sub.revision != revision {
				// The keys the changes since touched, each with its range now, or every range
				// once the log no longer reaches back that far.
				let ranges = match sub.changes(revision) {
					Some(keys) => Ok(keys
						.into_iter()
						.map(|key| (key, sub.range(key).map(|range| (range, sub.since(key)))))
						.collect::<Vec<_>>()),
					None => Err(sub
						.ranges()
						.map(|(key, range)| (key, (range, sub.since(key))))
						.collect::<BTreeMap<_, _>>()),
				};
				changed = Some((scalars, Some(ranges), sub.revision));
			} else if moved {
				changed = Some((scalars, None, revision));
			}
			Poll::<()>::Pending
		});
		if let Some((scalars, ranges, revision)) = changed {
			self.revision = revision;
			let mirrored = &mut self.mirrored;
			(mirrored.priority, mirrored.delay, mirrored.order, mirrored.feed) = scalars;
			self.window.set_delay(mirrored.delay);
			let change = match ranges {
				None => Change::Scalars,
				Some(Ok(edited)) => {
					for (key, range) in &edited {
						match range {
							Some((range, since)) => self.mirrored.insert(*key, *range, *since),
							None => self.mirrored.remove(*key),
						}
					}
					let added: Vec<(u64, track::Range)> = edited
						.iter()
						.filter_map(|(key, range)| Some((*key, (*range)?.0)))
						.collect();
					self.window
						.edit(edited.iter().map(|(key, _)| *key), &added, None, &Unheld);
					// A retired range has nothing left to backfill, so a peer that keeps retiring
					// and adding ranges before the floor resolves holds only its live ones here.
					if let Some(Resumed::Ranges(keys)) = &mut self.resumed {
						for (key, _) in edited.iter().filter(|(_, range)| range.is_none()) {
							keys.remove(key);
						}
					}
					// The serving copy's resume point may cut off groups the added ranges cover,
					// judged by the reader's window. A route change still to judge is judged
					// against every range instead, since the ones it placed moved.
					if !added.is_empty() {
						let added = added.iter().map(|(key, _)| *key);
						self.resumed = Some(Resumed::Ranges(match self.resumed.take() {
							Some(Resumed::Ranges(mut keys)) => {
								keys.extend(added);
								keys
							}
							Some(Resumed::Route(..)) => self.mirrored.ranges.keys().copied().collect(),
							None => added.collect(),
						}));
					}
					Change::Edit(edited.into_iter().map(|(key, _)| key).collect())
				}
				// Unchanged only if each live join also keeps its tick: one added again under the
				// same key asks its copies anew.
				Some(Err(ranges))
					if ranges
						.iter()
						.map(|(key, (range, _))| (key, range))
						.eq(&self.mirrored.ranges)
						&& (ranges.iter())
							.filter(|(_, (range, _))| matches!(range.start, track::Start::Live(_)))
							.map(|(key, (_, since))| (key, since))
							.eq(&self.mirrored.since) =>
				{
					Change::Scalars
				}
				Some(Err(ranges)) => {
					let mut mirrored = Mirrored {
						ranges: BTreeMap::new(),
						since: BTreeMap::new(),
						..std::mem::take(&mut self.mirrored)
					};
					for (key, (range, since)) in ranges {
						mirrored.insert(key, range, since);
					}
					self.mirrored = mirrored;
					self.window = keyed(
						self.mirrored.ranges.iter().map(|(key, range)| (*key, *range)),
						self.mirrored.delay,
						&Unheld,
					);
					// The serving copy's resume point may cut off groups the new ranges cover,
					// judged by the reader's window.
					self.resumed = Some(Resumed::Ranges(self.mirrored.ranges.keys().copied().collect()));
					Change::All
				}
			};
			// New preferences may ask what a backfill failed to get.
			self.refused = None;
			for copy in &mut self.copies {
				copy.update(&self.mirrored, &change);
			}
			// A ready copy just resolved the changed ranges against its cache, before its
			// failover cap raised them, so the reader takes that resolution now.
			if !matches!(change, Change::Scalars) {
				let concluded = self.route.end().is_some();
				for index in (0..self.copies.len()).rev() {
					if self.window.is_settled() {
						break;
					}
					// As in [`Self::window`], a failed copy leaves the placement to the copy continuing the track.
					if !concluded && copy_failed(&self.copies[index].track) {
						continue;
					}
					if matches!(self.copies[index].sub, Sub::Ready(_)) {
						let copy = &self.copies[index].resolved;
						let resolved: BTreeMap<u64, Resolved> = self
							.window
							.unsettled()
							.into_iter()
							.filter_map(|key| Some((key, *copy.get(&key)?)))
							.collect();
						self.resolve(&resolved);
					}
				}
			}
		}

		let (generation, serving) = {
			let route = self.route.state.read();
			match &route.phase {
				Phase::Finished {
					last: (generation, copy),
					..
				} => (*generation, copy.clone()),
				_ => (route.generation, route.copy.clone()),
			}
		};
		if self.generation == Some(generation) {
			return;
		}
		self.generation = Some(generation);
		// What a former route gave up is no verdict on the one that serves now.
		self.drops = RangeMap::new();
		self.received = RangeSet::new();
		self.received_checked = None;
		let Some(track) = serving else { return };
		if self.copies.last().is_some_and(|copy| copy.generation == generation) {
			return;
		}
		// Where the serving copy resolved the reader's ranges: the floor raises each start
		// there, never lowering a later range to the floor. A failed copy's resolution is no
		// placement, as in `window`: the reader's own carries over, pending until a copy
		// continuing the track places it.
		let failed = self.route.end().is_none()
			&& self
				.copies
				.last()
				.is_some_and(|copy| matches!(copy.sub, Sub::Ready(_)) && copy_failed(&copy.track));
		let keys = self.mirrored.ranges.keys().copied();
		let resolved = match self.copies.last_mut() {
			Some(_) if failed => self.window().resolved_by_key(),
			Some(Copy {
				sub: Sub::Ready(sub), ..
			}) => keyed_resolution(keys.clone(), sub.resolved_keys(keys)),
			// One still unanswered passes on what it was handed.
			Some(Copy {
				sub: Sub::Pending(_),
				resolved,
				..
			}) => resolved.clone(),
			None => BTreeMap::new(),
		};
		// The new copy starts at the boundary, so a lower group no replaced copy holds is asked
		// of its route once the reader says which; see `backfill_gaps`.
		self.resumed = Some(Resumed::Route(
			self.copies.last().and_then(|copy| copy.floor),
			resolved.clone(),
		));
		// One subscription split at one boundary: the largest group any copy holds, never below
		// one handed out. The new copy starts there, and each copy it replaces is capped there
		// now, so it finishes the groups up to it and sends nothing newer.
		let boundary = self.boundary();
		// Returning to a route reuses its subscription and leases, including an older
		// group's in-flight tail. A fresh copy would replay it and retain one per flap. The
		// newest is the route's own copy, not a backfill of it.
		let returning = self.copies.iter().rposition(|copy| copy.track.same_channel(&track));
		// The copy's own resolution ends at its old cap; the reader's does not.
		let returning = returning.map(|index| (self.window().resolved_by_key(), self.copies.remove(index)));
		match boundary {
			Some(boundary) => {
				// The copies it replaces stop the boundary group where the new copy resumes it, so
				// none of it is sent twice: only when the new copy resumes at the boundary.
				let floor = (returning.as_ref()).map_or(Some(boundary), |(_, copy)| copy.floor.max(Some(boundary)));
				let stop = match floor == Some(boundary) {
					true => self.held_at(floor).map(|(stop, _)| stop),
					false => None,
				};
				for copy in &mut self.copies {
					copy.replaced = true;
					copy.cap(boundary, stop, &self.mirrored);
				}
			}
			// Nothing held nor handed out: nothing to split, so the new copy serves it all. Kept
			// and asked for nothing, a copy's subscription would end with the track, and a fetch
			// then landing a later group would read as the whole of it. An end one declared
			// outlives it.
			None => {
				self.learn_end(Some(&kio::Waiter::noop()));
				self.copies.clear();
			}
		}
		if let Some((resolved, mut copy)) = returning {
			copy.generation = generation;
			copy.replaced = false;
			copy.until = None;
			let capped = copy.cap.take().is_some();
			copy.since = None;
			copy.done = None;
			// Resume where a fresh copy would: a group it buffered while replaced, below the
			// boundary, is the capped copies' to deliver.
			copy.floor = copy.floor.max(boundary);
			copy.backfill = false;
			let resume = self.resume_at(copy.floor, &copy.track);
			let start = copy.start(self.groups.0);
			if let (Sub::Ready(sub), Some(_)) = (&mut copy.sub, copy.floor) {
				sub.set_range((
					Bound::Included(start),
					self.groups.1.map_or(Bound::Unbounded, Bound::Excluded),
				));
			}
			copy.resolved = resolved;
			match resume == copy.resume && !capped {
				true => copy.send(&self.mirrored, self.mirrored.ranges.keys().copied()),
				// An edit carries no resume nor cap, so a new one is asked whole.
				false => {
					copy.resume = resume;
					copy.ask_whole(&self.mirrored);
				}
			}
			self.copies.push(copy);
			return;
		}
		let floor = boundary;
		let resume = self.resume_at(floor, &track);
		let span = Span {
			floor,
			end: None,
			resume,
		};
		self.copies.push(Copy {
			generation,
			sub: Copy::subscribe(&track, &self.mirrored, span, &resolved),
			track,
			floor,
			backfill: false,
			fills: false,
			resume,
			resolved,
			replaced: false,
			until: None,
			cap: None,
			done: None,
			since: None,
			lease: Arc::new(()),
			feeds: true,
		});
		self.learn_end(None);
	}

	/// Ask `track` to resume group `key` past the frames held of it (see [`Self::held_at`]),
	/// lending it those frames, so once the rest arrives it holds the group whole for a
	/// reader that joins later, after the copies holding them are gone.
	fn resume_at(&self, key: Option<u64>, track: &track::Consumer) -> Option<Position> {
		let (resume, head) = self.held_at(key)?;
		track.lend_head(resume.group, head);
		Some(resume)
	}

	/// Where a new copy asked from group `key` resumes it: past the most frames of it a copy
	/// holds from its first, which the reader already has or reads locally, and those frames.
	/// A route switch behind the same next hop then sends none of them again, only the rest of
	/// a group still arriving, and nothing of a finished one. `None` while no copy holds its
	/// first frame.
	fn held_at(&self, key: Option<u64>) -> Option<(Position, Arc<group::Head>)> {
		let key = key?;
		(self.copies.iter())
			.filter_map(|copy| copy.track.peek_group(key)?.handoff().head())
			.filter_map(|head| Some((head.frames_from_first()?, head)))
			.max_by_key(|(frames, _)| *frames)
			.map(|(frames, head)| (Position::new(key, frames), head))
	}

	/// One logical track has one end, whichever route first reports it.
	fn learn_end(&mut self, waiter: Option<&kio::Waiter>) {
		for copy in &mut self.copies {
			let end = match waiter {
				Some(waiter) => match copy.poll_ready(self.groups, waiter) {
					Poll::Ready(Ok(sub)) => match sub.poll_finished(waiter) {
						Poll::Ready(Ok(end)) => Some(end),
						_ => None,
					},
					_ => None,
				},
				None => match &mut copy.sub {
					Sub::Ready(sub) => sub.declared_end(),
					Sub::Pending(_) => copy.track.declared_end(),
				},
			};
			let Some(end) = end else { continue };
			match self.declared {
				None => self.declared = Some(end),
				// FETCH can name only the group boundary just after a mid-group end.
				Some(declared) if declared.frame == 0 && end.frame != 0 && declared.final_key() == end.final_key() => {
					self.declared = Some(end);
				}
				Some(_) => {}
			}
			// A copy over a version with no end error declares the same end cleanly. Only a copy
			// at the end that stands says how the track ended there.
			let same = self
				.declared
				.is_some_and(|declared| declared == end || end.frame == 0 && end.final_key() == declared.final_key());
			if same && self.declared_error.is_none() {
				self.declared_error = copy.track.declared_error();
			}
		}
	}

	/// Drop the replaced copies nothing needs any more; see [`Copy::retains`]. Whether one
	/// may still deliver a group the reader lacks, which a clean end waits for.
	fn let_go(&mut self, delivered: &Delivered, waiter: &kio::Waiter) -> bool {
		self.learn_end(None);
		self.window();
		self.out.retain(|(_, out)| out.strong_count() > 0);
		// Only a replaced copy is judged against the edge, which costs the serving track's
		// lock: a reader following one route never pays it.
		let edge = match self.copies.iter().any(|copy| copy.replaced) {
			true => self.copies.last().and_then(|copy| copy.track.latest_key()),
			false => None,
		};
		// Only a bounded window ends on what it was handed out, so only there may a fill let
		// go early lose a group for good.
		let owed = match self.window.end().is_some() && self.copies.iter().any(|copy| copy.fills) {
			true => self.owed(delivered, waiter),
			false => RangeSet::new(),
		};
		let reader = Retain {
			groups: self.groups,
			window: &self.window,
			groups_read: self.groups_read,
			edge,
			mirrored: &self.mirrored,
			delivered,
			out: &self.out,
			owed: &owed,
			waiter,
		};
		let mut waits = false;
		let mut gone = Vec::new();
		// The serving copy, or one between routes with nothing replacing it yet, stays.
		for mut copy in std::mem::take(&mut self.copies) {
			let retains = copy.replaced.then(|| copy.retains(&reader));
			copy.feeds = matches!(retains, None | Some(Retains::Wait));
			match retains {
				None | Some(Retains::Keep) => self.copies.push(copy),
				Some(Retains::Wait) => {
					waits = true;
					self.copies.push(copy);
				}
				Some(Retains::No) => gone.push(copy),
			}
		}
		if gone.is_empty() && self.skipped.is_empty() {
			return waits;
		}
		// A skip is final, and only that copy may know it: keep what the copies skipped of the
		// start and end of each gap the reader lacks, merged with what was
		// kept before, since a start or end may be skipped in parts no one copy knows whole. Only
		// the start and end, at most two runs a gap, so what is kept never outgrows the gaps
		// however many routes come and go; a skip inside a gap is left to the copies that hold it.
		let lacking = self.lacking(delivered);
		let mut kept = RangeSet::new();
		for gap in &lacking {
			let mut found: RangeSet<u64> = self
				.skipped
				.overlapping(gap)
				.map(|run| run.start.max(gap.start)..run.end.min(gap.end))
				.collect();
			if !gone.is_empty() {
				for copy in gone.iter().chain(&self.copies) {
					found.extend(copy.skipped_in(gap.clone(), &kio::Waiter::noop()).iter().cloned());
				}
			}
			if let Some(first) = found.iter().next().filter(|run| run.start == gap.start) {
				kept.insert(first.clone());
			}
			if let Some(last) = found.iter().next_back().filter(|run| run.end == gap.end) {
				kept.insert(last.clone());
			}
		}
		self.skipped = kept;
		waits
	}

	/// The parts of `groups` a copy held now or one let go declared skipped. Parks `waiter` on
	/// each copy read while some of `groups` is not skipped there.
	fn skipped_in(&self, groups: std::ops::Range<u64>, waiter: &kio::Waiter) -> RangeSet<u64> {
		let mut skipped: RangeSet<u64> = self
			.skipped
			.overlapping(&groups)
			.map(|run| run.start.max(groups.start)..run.end.min(groups.end))
			.collect();
		for copy in &self.copies {
			skipped.extend(copy.skipped_in(groups.clone(), waiter).iter().cloned());
		}
		skipped
	}

	/// What [`Retain::owed`] holds: the gaps [`Self::lacking`] finds, less what a copy still
	/// read holds and may hand out, what a replaced copy other than a fill still feeds, and
	/// what a copy declared skipped.
	fn owed(&self, delivered: &Delivered, waiter: &kio::Waiter) -> RangeSet<u64> {
		let (start, budget) = (self.groups.0, self.mirrored.budget());
		let mut owed = RangeSet::new();
		for gap in self.lacking(delivered) {
			owed.insert(gap.clone());
			for run in self.skipped_in(gap.clone(), waiter).iter() {
				owed.remove(run.clone());
			}
			for copy in &self.copies {
				// Only a copy `poll_lowest` still reads delivers: not ran out, failed, or past
				// the budget.
				if copy.done.is_some() || copy.expired(budget, waiter) {
					continue;
				}
				for key in copy.track.cached_groups(gap.clone()) {
					if copy.start(start) <= key && Self::deliverable(copy, key, delivered) {
						owed.remove(key..key + 1);
					}
				}
				if copy.replaced && !copy.fills && copy.feeds && !copy_failed(&copy.track) {
					let (from, until) = (copy.start(start), copy.end().unwrap_or(u64::MAX));
					let fed = from.max(gap.start)..until.min(gap.end);
					if !fed.is_empty() {
						owed.remove(fed);
					}
				}
			}
		}
		owed
	}

	/// The gaps of `delivered` in the reader's window under the serving copy's resume point:
	/// where a key the local floor or cap keeps out, a start or an end of one, needs a skip to
	/// count as accounted for. See [`Self::accounts`].
	fn lacking(&self, delivered: &Delivered) -> Vec<std::ops::Range<u64>> {
		let resumed = (self.copies.last())
			.filter(|copy| !copy.replaced)
			.and_then(|copy| copy.floor)
			.unwrap_or(0);
		self.window
			.runs(self.window.floor().unwrap_or(0))
			.take_while(|run| *run.start() < resumed)
			.flat_map(|run| delivered.gaps(*run.start()..run.end().saturating_add(1).min(resumed)))
			.collect()
	}

	/// Whether the serving copy drained cleanly. Nothing newer then supersedes what the copies
	/// it replaced still carry under their caps, so the budget for it runs from now.
	fn drained(&mut self) -> bool {
		let Some(serving) = self
			.copies
			.last()
			.filter(|copy| Some(copy.generation) == self.generation)
		else {
			return false;
		};
		if !matches!(serving.done, Some(Ok(_))) {
			return false;
		}
		for copy in &mut self.copies {
			if copy.replaced && copy.since.is_none() {
				copy.since = Some(copy.track.now());
			}
		}
		true
	}

	/// Where a switch splits the subscription: the largest group any copy holds, never below
	/// the newest handed out. `None` while neither exists, when the new copy is asked for the
	/// ranges as the copies it replaces were, and they are dropped.
	fn boundary(&self) -> Option<u64> {
		(self.copies.iter())
			.filter_map(|copy| copy.track.latest_key())
			.max()
			.max(self.newest)
	}

	/// Whether the window and the local bounds name a group from `floor` up to `key` the reader
	/// lacks and that may yet land on copy `index`: one outside the local bounds never lands on a
	/// copy, nor one that copy declares skipped, nor any on a failed copy, which receives nothing
	/// more.
	fn owes_below(&mut self, index: usize, floor: u64, key: u64, delivered: &Delivered) -> bool {
		let copy = self.copies[index].track.clone();
		if matches!(copy.poll_complete(&kio::Waiter::noop()), Poll::Ready(Err(_))) {
			return false;
		}
		let (start, cap) = self.groups;
		let key = cap.map_or(key, |cap| key.min(cap));
		self.window()
			.runs(floor.max(start))
			.take_while(|run| *run.start() < key)
			.flat_map(|run| delivered.gaps(*run.start()..run.end().saturating_add(1).min(key)))
			.any(|gap| {
				track::first_open(std::iter::once(gap.start..=gap.end - 1), |at| copy.hole_end(at, false)).is_some()
			})
	}

	fn deliverable(copy: &Copy, key: u64, delivered: &Delivered) -> bool {
		let replaced = copy.until.is_some_and(|until| until.is_none_or(|until| key > until));
		!replaced && !delivered.contains(key)
	}

	fn hand_out(&mut self, index: usize, group: group::Consumer) -> group::Consumer {
		let key = group.start;
		self.newest = self.newest.max(Some(key));
		// Another copy holding the group, now or once it lands there, is where a failure
		// continues it, so it stays while the group is read; see [`Copy::retains`]. Recorded
		// even with one copy, since a later route's copy may come to hold it.
		// Sweep the groups no longer read once the list fills, so it tracks the live count.
		if self.out.len() == self.out.capacity() {
			self.out.retain(|(_, out)| out.strong_count() > 0);
			self.out.reserve(self.out.len().max(1));
		}
		let out = Arc::new(());
		self.out.push((key, Arc::downgrade(&out)));
		let copy = &self.copies[index];
		let handoff = group.handoff();
		group.with_recover(Recover {
			key,
			reader: Some(self.this.clone()),
			max_delay: None,
			route: self.route.watch(),
			generation: copy.generation,
			copy: copy.track.clone(),
			handoff,
			lease: Some(copy.lease.clone()),
			out: Some(out),
			logical: None,
			fetch: None,
			recovering: false,
			live: Arc::default(),
		})
	}

	/// Hand out the group `candidate` reserved. Selection reserves the exact group, so
	/// eviction can abort that group but cannot substitute a later one and advance the
	/// logical reader past a gap.
	fn take(&mut self, candidate: Candidate) -> Result<group::Consumer> {
		let index = self
			.copies
			.iter()
			.position(|copy| Arc::ptr_eq(&copy.lease, &candidate.lease))
			.expect("the candidate's lease retains its copy");
		let Sub::Ready(sub) = &mut self.copies[index].sub else {
			unreachable!("a candidate comes from a copy that answered")
		};
		let mut group = sub.take(candidate.reserved)?;
		// A mid-group end caps the group it falls in, as it caps a direct track's.
		if let Some(end) = self.declared.filter(|end| end.frame != 0 && end.group == group.start) {
			group = group.with_end(end.frame);
		}
		Ok(self.hand_out(index, group))
	}

	/// Choose without consuming a candidate, so a newer copy cannot overtake a lower
	/// group still buffered on an older one. A tie favors the newer copy.
	fn poll_lowest(
		&mut self,
		ordered: bool,
		parks: bool,
		delivered: &Delivered,
		waiter: &kio::Waiter,
	) -> Option<Poll<Result<Option<Candidate>>>> {
		self.sync(waiter);
		self.groups_read = true;
		self.backfill_gaps(delivered, waiter);
		let groups = self.groups;
		let budget = self.mirrored.budget();
		let floor = if ordered { delivered.floor() } else { 0 };
		let mut lowest: Option<Candidate> = None;
		let mut held = false;
		for index in 0..self.copies.len() {
			let copy = &mut self.copies[index];
			// What a replaced copy holds is past the reader's budget once a newer group
			// superseded it a full budget ago.
			// A fully handed-out bounded copy still backs groups being read, but cannot
			// supply another one. Keep its recovery lease without polling its empty range.
			let spent = copy.end().is_some_and(|end| delivered.covers(groups.0..end));
			// A copy that ran out cleanly still takes a group in flight below its end while the
			// read stays open, as a cursor still reading it would.
			let ran_out = match &copy.done {
				None => false,
				Some(Ok(drained)) if !spent && copy.track.poll_arrived(*drained, waiter).is_ready() => true,
				Some(_) => continue,
			};
			if spent || copy.expired(budget, waiter) {
				continue;
			}
			if ran_out {
				copy.done = None;
			}
			let mut late = ran_out;
			let mut arrivals = copy.track.arrived();
			loop {
				let copy = &mut self.copies[index];
				let sub = match copy.poll_ready(groups, waiter) {
					Poll::Pending => break,
					Poll::Ready(Err(err)) => {
						copy.done = Some(Err(err));
						break;
					}
					Poll::Ready(Ok(sub)) => sub,
				};
				// Each copy's wire decides what it resolves: a copy it replaced refusing the
				// reader's ranges leaves the rest to the copy now serving.
				if let Some(err) = sub.refused() {
					if !copy.replaced {
						return Some(Poll::Ready(Err(err)));
					}
					copy.done = Some(Err(err));
					break;
				}
				if ordered {
					sub.skip_ordered(floor);
				}
				// The copy's own record bills to its track as a direct reader's does, including
				// what a peek skipped before it parked.
				let mut res = sub.poll_peek_group(ordered, waiter);
				if let Err(err) = sub.settle_record(None) {
					res = Poll::Ready(Err(err));
				}
				match res {
					Poll::Pending => break,
					Poll::Ready(Ok(Some(reserved))) => {
						let key = reserved.key();
						// Nor does a failed copy's group go out before the window is placed, which only
						// a copy continuing the track does, nor a backfill's that no range placed so far
						// holds, since only a placed range bounds the groups under the resume point: it
						// stays there until then.
						let copy = &self.copies[index];
						let (backfill, failed) =
							(copy.backfill, self.route.end().is_none() && copy_failed(&copy.track));
						// Only a copy the hold can apply to computes the window.
						let held_back = (failed || backfill) && {
							let window = self.window();
							window.is_pending() && (failed || !window.holds(key))
						};
						if held_back {
							held = true;
							break;
						}
						// A copy resolves a time start against the groups it holds, which may place
						// it below where the reader's window did: the window decides.
						let deliverable = Self::deliverable(&self.copies[index], key, delivered);
						if !deliverable || !self.window().contains(key) {
							// Skipping an ordered cursor past a group the window still owes below
							// would lose it, though it may yet land on this copy: leave it there.
							// The peek returned the held group, so only the copy's arrivals wake
							// the read when the owed one lands: one already landed is peeked again.
							if ordered && deliverable && self.owes_below(index, floor, key, delivered) {
								let copy = &self.copies[index];
								if copy.track.poll_arrived(arrivals, waiter).is_ready() {
									arrivals = copy.track.arrived();
									continue;
								}
								break;
							}
							let Sub::Ready(sub) = &mut self.copies[index].sub else {
								unreachable!()
							};
							if ordered {
								sub.skip_ordered(key + 1);
							} else {
								let _ = sub.take(reserved);
							}
							continue;
						}
						if lowest.as_ref().is_none_or(|lowest| key <= lowest.key()) {
							lowest = Some(Candidate {
								lease: self.copies[index].lease.clone(),
								reserved,
							});
						}
						break;
					}
					// Drained: the track ended as the copy did, which is not cleanly when
					// its session went away short of the declared end.
					Poll::Ready(Ok(None)) => {
						// Counted before the read, so a group landing during it is read again.
						copy.done = Some(match copy.track.poll_complete(&kio::Waiter::noop()) {
							Poll::Ready(end) => end.map(|()| arrivals),
							Poll::Pending => Ok(arrivals),
						});
						if matches!(copy.done, Some(Ok(_))) && copy.track.poll_arrived(arrivals, waiter).is_ready() {
							copy.done = None;
							late = true;
							arrivals = copy.track.arrived();
							continue;
						}
						break;
					}
					Poll::Ready(Err(err)) => {
						copy.done = Some(Err(err));
						break;
					}
				}
			}
			// A late group under the serving copy's resume point is not this copy's to read, and
			// a backfill that already ran out was let go: ask its route again. So is what a
			// replaced copy that just failed still had to drain.
			let copy = &self.copies[index];
			let serving = !copy.replaced && Some(copy.generation) == self.generation;
			let failed = copy.replaced && matches!(copy.done, Some(Err(_)));
			// A backfill of the serving route that failed is not asked of that route again.
			if failed
				&& Some(copy.generation) == self.generation
				&& let (Some(floor), Some(end)) = (copy.floor, copy.end())
			{
				self.refused = Some((copy.generation, floor..end));
			}
			// A bounded window ends on what it was handed out, so a replaced copy that ran out
			// short of a group the reader lacks under its bound has that group asked again: a
			// fill let go while that copy still fed it would otherwise leave it unasked.
			let ran_short =
				copy.replaced && !copy.fills && matches!(copy.done, Some(Ok(_))) && self.window.end().is_some();
			if (late && serving && copy.floor.is_some()) || failed || ran_short {
				self.resumed
					.get_or_insert_with(|| Resumed::Route(None, BTreeMap::new()));
			}
		}
		// A later copy placed the window this pass: a group held back above may now be the lowest.
		if held && !self.window().is_pending() {
			drop(lowest);
			return self.poll_lowest(ordered, parks, delivered, waiter);
		}
		if let Some(lowest) = lowest {
			return Some(Poll::Ready(Ok(Some(lowest))));
		}
		// A copy answering this pass may have placed the ranges the switch's backfill waits
		// for: ask for it now, since no later group is sure to wake the read.
		if self.resumed.is_some() && self.window().floor().is_some() {
			return None;
		}
		let drained = self.drained();
		let waits = self.let_go(delivered, waiter);
		if !parks && !waits && self.exhausted(&[delivered], ordered, waiter) {
			return Some(Poll::Ready(Ok(None)));
		}
		match self.poll_end(waiter, |copy| copy.done.clone().map(|done| done.map(|_| ()))) {
			// The serving copy drained, but a copy it replaced may still deliver a group below
			// its end: the track ends once that copy has.
			Some(Poll::Ready(Ok(None))) if drained && waits => Some(Poll::Pending),
			end => end,
		}
	}

	/// Whether a bounded window has nothing left to deliver, once no replaced copy may still
	/// deliver a group under its bound: every group of the whole window is handed out (in any
	/// of `taken`, the logical readers' records, the reader's own first) or accounted for, as
	/// [`Self::accounts`] judges.
	fn exhausted(&mut self, taken: &[&Delivered], ordered: bool, waiter: &kio::Waiter) -> bool {
		let window = self.window();
		let Some(end) = window.end().filter(|_| !window.is_pending()) else {
			return false;
		};
		let gaps: Vec<std::ops::Range<u64>> = window
			.runs(window.floor().unwrap_or(0))
			.take_while(|run| *run.start() < end)
			.flat_map(|run| Delivered::gaps_in_all(taken, *run.start()..run.end().saturating_add(1).min(end)))
			.collect();
		self.accounts(taken, &gaps, ordered, waiter)
	}

	/// Whether every key of `gaps`, which none of `taken` handed out, is accounted for.
	///
	/// Under the serving copy's resume point, a key the local floor and cap admit was the copies'
	/// it replaced, each let go once it had nothing more; one they keep out stays owed, as on one
	/// cursor, unless a copy declares it skipped. The serving copy judges the rest by where it
	/// placed the reader's ranges itself: a time a copy held no group for places the reader's
	/// window at the time itself, wider than any group the serving copy reads. A backfill still
	/// out is not judged yet, and a route between copies has nothing to judge.
	fn accounts(
		&mut self,
		taken: &[&Delivered],
		gaps: &[std::ops::Range<u64>],
		ordered: bool,
		waiter: &kio::Waiter,
	) -> bool {
		if self.resumed.is_some() {
			return false;
		}
		let window = self.window();
		if window.end().is_none() || window.is_pending() {
			return false;
		}
		let (lowest, cap) = self.groups;
		let serving = self.copies.last().filter(|copy| {
			Some(copy.generation) == self.generation
				&& !copy.replaced
				&& copy.done.is_none()
				&& matches!(copy.sub, Sub::Ready(_))
		});
		let Some(serving) = serving else {
			return false;
		};
		let resumed = serving.floor.unwrap_or(0);
		// Declared skipped by a copy let go, or by one held now, any of which wakes the read
		// once it declares more.
		let skipped = |open: &std::ops::Range<u64>| {
			let mut held = RangeSet::new();
			for copy in &self.copies {
				held.extend(copy.skipped_in(open.clone(), waiter).iter().cloned());
			}
			held.gaps(open).next().is_none()
		};
		let mut above = Vec::new();
		for gap in gaps {
			let split = resumed.clamp(gap.start, gap.end);
			// Under the resume point, only what the local floor or cap keeps out.
			let kept_out = [
				gap.start..split.min(lowest),
				cap.map_or(split, |cap| cap.max(gap.start))..split,
			];
			let open = kept_out
				.into_iter()
				.filter(|run| !run.is_empty())
				.flat_map(|run| self.skipped.gaps(&run).collect::<Vec<_>>())
				.any(|open| !skipped(&open));
			if open {
				return false;
			}
			if split < gap.end {
				above.push(split..gap.end);
			}
		}
		match self.copies.last_mut() {
			Some(Copy {
				sub: Sub::Ready(sub), ..
			}) => sub.accounts_for(taken, &above, ordered, waiter),
			_ => false,
		}
	}

	/// Whether the track ended, once the serving copy's channel (as `done` reads it) ran
	/// out. `None` when the route moved since it was read: the caller looks again.
	fn poll_end<T>(
		&self,
		waiter: &kio::Waiter,
		done: impl Fn(&Copy) -> Option<Result<()>>,
	) -> Option<Poll<Result<Option<T>>>> {
		let serving = self
			.copies
			.last()
			.filter(|copy| Some(copy.generation) == self.generation);
		// A drained copy's clean end or content verdict ends the track. Only a delivery
		// failure is the front's to replace. Mirrors the JS reader's `#end`.
		match serving.and_then(&done) {
			Some(Ok(())) => return Some(Poll::Ready(Ok(None))),
			Some(Err(err)) if !err.retryable() => return Some(Poll::Ready(Err(err))),
			_ => {}
		}
		let end = self.route.end();
		let ended = end.is_some();
		match end {
			Some(Some(Err(err))) => return Some(Poll::Ready(Err(err))),
			// Ended cleanly: once the serving copy (if any) drained.
			Some(Some(Ok(()))) => match serving.map(&done) {
				None | Some(Some(_)) => return Some(Poll::Ready(Ok(None))),
				Some(None) => {}
			},
			// No front is left to replace the copy: the track ends as it does, or unroutable
			// when no route served it. A connection ending retains its session error.
			Some(None) => match serving.map(&done) {
				None => return Some(Poll::Ready(Err(Error::Unroutable))),
				Some(Some(Ok(()))) => return Some(Poll::Ready(Ok(None))),
				Some(Some(Err(err))) => return Some(Poll::Ready(Err(err))),
				Some(None) => {}
			},
			None => {}
		}
		match self.poll_moved(ended, waiter) {
			true => None,
			false => Some(Poll::Pending),
		}
	}

	/// Park until the route moves past the generation last synced, or `true` when it
	/// already has and the caller should look again. `ended` is the end the caller
	/// already acted on: an end set since then readies the route without parking on it,
	/// so it has to be read rather than waited for.
	fn poll_moved(&self, ended: bool, waiter: &kio::Waiter) -> bool {
		!ended
			&& self
				.route
				.poll_changed(self.generation.unwrap_or_default(), waiter)
				.is_ready()
	}

	fn poll_finished(&mut self, waiter: &kio::Waiter) -> Poll<Result<Position>> {
		loop {
			self.sync(waiter);
			self.learn_end(Some(waiter));
			if let Some(end) = self.declared {
				return Poll::Ready(Ok(end));
			}
			let groups = self.groups;
			let concluded = self.route.end();
			if let Some(Some(Err(err))) = concluded {
				return Poll::Ready(Err(err));
			}
			let serving = self.generation;
			if let Some(copy) = self.copies.last_mut().filter(|copy| Some(copy.generation) == serving) {
				match copy.poll_ready(groups, waiter) {
					Poll::Ready(Ok(sub)) => match sub.poll_finished(waiter) {
						Poll::Ready(Ok(fin)) => return Poll::Ready(Ok(fin)),
						// Only a delivery failure waits for the front to replace its copy.
						Poll::Ready(Err(err)) if !err.retryable() || concluded.is_some() => {
							return Poll::Ready(Err(err));
						}
						_ => {}
					},
					Poll::Ready(Err(err)) if !err.retryable() || concluded.is_some() => {
						return Poll::Ready(Err(err));
					}
					_ => {}
				}
			} else if concluded.is_some() {
				return Poll::Ready(Err(Error::Unroutable));
			}
			if !self.poll_moved(concluded.is_some(), waiter) {
				return Poll::Pending;
			}
		}
	}

	/// The next run the serving copy gave up that `delivered` lacks and no other copy may
	/// still deliver. Only the serving copy's verdicts count, and a route failure is none:
	/// another route may still deliver the group, and a failed group it buffered continues
	/// through its [`Recover`]. A copy it replaced may still deliver what is under its bound,
	/// so a run there waits until that copy no longer feeds.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	fn poll_drop(
		&mut self,
		delivered: &Delivered,
		probe: bool,
		waiter: &kio::Waiter,
	) -> Poll<(std::ops::Range<u64>, track::Hole)> {
		let mut given_up = Vec::new();
		let mut superseded = Vec::new();
		let ready = matches!(self.poll_serving(waiter), Poll::Ready(Some(_)));
		// The serving copy's subscription, borrowed apart from the received runs it checks.
		if let Some(Sub::Ready(sub)) = self.copies.last_mut().filter(|_| ready).map(|copy| &mut copy.sub) {
			// A received stale run queued here that a new upstream subscription superseded since
			// is unknown again: the copy no longer holds it, and that subscription may deliver it. Checked
			// only once the copy let a stale run go, never on every poll: each poll reports one
			// run, so a check per poll would cost the received runs squared to drain them. With
			// none received there is nothing to check, so a live reader never reads the revision
			// through the track lock every subscriber shares.
			if !self.received.is_empty() {
				let current = sub.stale_revision();
				if self.received_checked != Some(current) {
					for run in self.received.iter() {
						superseded.extend(sub.stale_held(run.clone()).gaps(run));
					}
				}
				self.received_checked = Some(current);
			}
			loop {
				let polled = match probe {
					true => sub.poll_drop(waiter),
					false => sub.poll_queued_drop(waiter),
				};
				let Poll::Ready((run, hole)) = polled else { break };
				// A stale run the copy holds came from its upstream, which a new upstream
				// subscription may supersede.
				let received = hole.stale_budget().is_some() && !sub.stale_held(run.clone()).is_empty();
				given_up.push((run, hole, received));
			}
		}
		for run in superseded {
			self.remove_drop(run);
		}
		for (run, hole, received) in given_up {
			if !Error::Stream(hole.error()).retryable() {
				match received {
					true => self.received.insert(run.clone()),
					false => self.received.remove(run.clone()),
				}
				self.drops.insert(run, hole);
			}
		}
		// A delivered run goes once a scan reaches it, never in a pass over the whole queue:
		// each poll reports one run, so such a pass would cost the queue squared to drain it,
		// and a new reader can be handed one run per cached group and per gap between them.
		while let Some(run) = self.drops.first_range_value().map(|(run, _)| run.clone())
			&& delivered.covers(run.clone())
		{
			self.remove_drop(run);
		}
		// The common case: nothing to report on one route, and the serving copy's own poll
		// above wakes this for the next run it gives up. A replaced copy is still judged:
		// dropping the last group handed out from it wakes nothing else that lets it go.
		if self.drops.is_empty() && !self.copies.iter().any(|copy| copy.replaced) {
			return Poll::Pending;
		}
		// Judged as the group path judges it, so a copy kept only for a group still read
		// holds nothing back, and letting one go wakes this too.
		self.let_go(delivered, waiter);
		let serving = self.generation;
		let (start, end) = self.groups;
		let mut feeding = RangeSet::new();
		for copy in &self.copies {
			if Some(copy.generation) == serving || !copy.feeds {
				continue;
			}
			let from = copy.start(start);
			let until = copy.end().unwrap_or(u64::MAX);
			if from < until {
				feeding.insert(from..until);
			}
		}
		let mut passed = Vec::new();
		let next = self.drops.iter().find_map(|(run, err)| {
			if delivered.covers(run.clone()) {
				passed.push(run.clone());
				return None;
			}
			let run = run.start.max(start)..run.end.min(end.unwrap_or(u64::MAX));
			let open = delivered
				.gaps(run)
				.into_iter()
				.find_map(|gap| feeding.gaps(&gap).next())?;
			Some((open, err.clone()))
		});
		for run in passed {
			self.remove_drop(run);
		}
		let Some((open, err)) = next else {
			return Poll::Pending;
		};
		// Reported once. As on a plain cursor, the report is not final: a miss (a fill given
		// up) may still land, and is handed out if it does.
		self.remove_drop(open.clone());
		Poll::Ready((open, err))
	}

	/// Forget `run` as queued, received or not.
	fn remove_drop(&mut self, run: std::ops::Range<u64>) {
		self.received.remove(run.clone());
		self.drops.remove(run);
	}

	/// The serving copy's subscription, once it answered; `None` once the track ended
	/// without one.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	fn poll_serving(&mut self, waiter: &kio::Waiter) -> Poll<Option<&mut track::Subscriber>> {
		loop {
			self.sync(waiter);
			let groups = self.groups;
			let serving = self.generation;
			let concluded = self.route.end().is_some();
			let copy = self.copies.last_mut().filter(|copy| Some(copy.generation) == serving);
			match copy.map(|copy| copy.poll_ready(groups, waiter).map(|res| res.is_ok())) {
				Some(Poll::Ready(true)) => break,
				Some(Poll::Ready(false)) | None if concluded => return Poll::Ready(None),
				_ => {}
			}
			if !self.poll_moved(concluded, waiter) {
				return Poll::Pending;
			}
		}
		match self.copies.last_mut().map(|copy| &mut copy.sub) {
			Some(Sub::Ready(sub)) => Poll::Ready(Some(sub)),
			_ => unreachable!("the serving copy answered above"),
		}
	}

	/// Assign the local floor and cap. A copy still waiting on its info takes them when it
	/// resolves; one already reading has to be moved, or a group it skipped stays skipped.
	fn set_range(&mut self, start: u64, end: Option<u64>, delivered: &Delivered) {
		let passed = std::mem::replace(&mut self.groups, (start, end)).0;
		self.capped.wake();
		for copy in &mut self.copies {
			copy.reopen();
			let start = copy.start(start);
			if let Sub::Ready(sub) = &mut copy.sub {
				sub.set_range((Bound::Included(start), end.map_or(Bound::Unbounded, Bound::Excluded)));
			}
		}
		self.backfill(start..passed, false, delivered);
	}

	/// Ask the newest copy's route for the groups in `lowered` under that copy's resume point,
	/// which a lowered floor or a route change newly covers. The copies that cached them may
	/// be gone, and that copy never asked for them, so a bounded copy of the same route asks
	/// for just those. One asked from a `raised` time start also reads the group holding it,
	/// under where it was asked from.
	fn backfill(&mut self, lowered: std::ops::Range<u64>, raised: bool, delivered: &Delivered) {
		let Some(end) = self.copies.last().and_then(|newest| newest.floor) else {
			return;
		};
		let lowered = lowered.start..lowered.end.min(end);
		// An ordered reader never goes back, nor does any reader past its budget.
		if delivered.covers(lowered.clone()) {
			return;
		}
		let newest = self.copies.last().expect("checked above");
		let (generation, track) = (newest.generation, newest.track.clone());
		// What a backfill of this route already failed to get, it would fail to get again.
		if let Some((refused, range)) = &self.refused
			&& *refused == generation
			&& range.start <= lowered.start
			&& lowered.end <= range.end
		{
			return;
		}
		// A floor lowered again asks once.
		let asks =
			|copy: &Copy| copy.track.same_channel(&track) && copy.floor.is_some_and(|floor| floor <= lowered.start);
		if self
			.copies
			.iter()
			.any(|copy| asks(copy) && copy.end().is_none_or(|end| end >= lowered.end))
		{
			return;
		}
		// Resolved as the reader first had them: the newest copy's resolution starts at its
		// resume point, and a bounded copy's own ends at its earlier cap.
		let resolved = self.window().resolved_by_key();
		// A later resume point widens the route's backfill rather than asking again, so flaps
		// over a gap keep one per route, reading again one that ran out. Waited for as long as a
		// new one would be.
		let last = self.copies.len() - 1;
		if let Some(copy) = self.copies[..last]
			.iter_mut()
			.find(|copy| !matches!(copy.done, Some(Err(_))) && copy.end().is_some() && asks(copy))
		{
			copy.until = Some(lowered.end.checked_sub(1));
			copy.since = Some(copy.track.now());
			copy.done = None;
			copy.backfill |= raised;
			copy.fills = true;
			copy.resolved = resolved;
			// Widened past the group it stopped partway through, which it now fills whole.
			match copy.cap.take() {
				None => copy.send(&self.mirrored, self.mirrored.ranges.keys().copied()),
				Some(_) => copy.ask_whole(&self.mirrored),
			}
			return;
		}
		let floor = Some(lowered.start);
		let span = Span {
			floor,
			end: Some(lowered.end),
			resume: None,
		};
		let copy = Copy {
			generation,
			sub: Copy::subscribe(&track, &self.mirrored, span, &resolved),
			floor,
			backfill: raised,
			fills: true,
			// Below every group handed out, so the reader holds none of the first.
			resume: None,
			resolved,
			// Bounded from the start: everything it may deliver is older than what the reader
			// already read, so it is waited for no longer than the reader's budget.
			replaced: true,
			until: Some(lowered.end.checked_sub(1)),
			cap: None,
			since: Some(track.now()),
			track,
			done: None,
			lease: Arc::new(()),
			feeds: true,
		};
		// The last copy stays the newest route's own.
		self.copies.insert(self.copies.len() - 1, copy);
	}

	/// Ask the serving route for the groups below its copy's resume point that the reader
	/// still lacks: a late or reordered one, one in flight on the route it replaced, or one
	/// widened ranges newly cover, as one cursor on the logical track would still get. From
	/// the lowest gap within the reader's ranges, so groups already handed out are not asked
	/// for again below it. A gap past the reader's budget was already forgotten as handed out.
	/// A time start the resume point raised is asked from its time, so the backfill places it
	/// at the group holding it, as the route would for a plain reader.
	fn backfill_gaps(&mut self, delivered: &Delivered, waiter: &kio::Waiter) {
		if self.resumed.is_none() {
			return;
		}
		let placed = self.window().floor();
		let raised = self.raised();
		let resumed = self.copies.last().and_then(|copy| copy.floor);
		// A raised start whose groups from its time the reader already holds or passed owes
		// nothing under the resume point: placed at its time, it hands out what the group
		// holding it would, as none under it goes out.
		let owed: BTreeMap<u64, u64> = raised
			.iter()
			.filter(|(_, time)| resumed.is_some_and(|resumed| !delivered.covers(**time..resumed)))
			.map(|(key, time)| (*key, *time))
			.collect();
		// Kept until a copy places the ranges, but those whose start the resume point raised,
		// which only the backfill places: only then can the gaps they cover be judged.
		let floor = match placed {
			Some(floor) => floor,
			None if !raised.is_empty() && raised.len() == self.window.unsettled().len() => {
				self.window.runs(0).next().map_or(0, |run| *run.start())
			}
			None => {
				// The gaps wait for the other ranges to place, but an owed raised start is asked
				// now: its backfill places it whatever they wait on, and a later ask finds it.
				if let (Some(resumed), Some(start)) = (resumed, owed.values().min()) {
					self.backfill(*start..resumed, true, delivered);
				}
				return;
			}
		};
		if owed.len() < raised.len() {
			let mut placed = BTreeMap::new();
			for (key, start) in raised.iter().filter(|(key, _)| !owed.contains_key(key)) {
				let end = self.mirrored.ranges[key].end.and_then(|end| end.checked_add(1));
				let at = Resolved {
					start: *start,
					end,
					floored: true,
				};
				placed.insert(*key, at);
			}
			self.resolve(&placed);
		}
		let Some(resumed_by) = self.resumed.take() else {
			return;
		};
		let Some(resumed) = resumed else {
			return;
		};
		// Each range from where the copy it replaced placed it, else where the reader did: a
		// start at that copy's own resume point was only raised there. After an edit, only the
		// ranges it added: the rest were judged when they came.
		let start = |range: &track::Range, replaced: Option<u64>, resolved: Option<Resolved>| match range
			.clamped(Some(0), None, resolved)
			.start
		{
			track::Start::Time(start) if replaced.is_none_or(|replaced| start > replaced) => start,
			_ => floor,
		};
		let from = match &resumed_by {
			Resumed::Route(replaced, resolved) => self
				.mirrored
				.ranges
				.iter()
				.map(|(key, range)| start(range, *replaced, resolved.get(key).copied()))
				.min(),
			Resumed::Ranges(keys) => {
				let starts: Vec<u64> = keys
					.iter()
					.filter_map(|key| self.mirrored.ranges.get(key))
					.map(|range| start(range, None, None))
					.collect();
				// Retired since, so nothing new is covered.
				if starts.is_empty() && owed.is_empty() {
					return;
				}
				Some(starts.into_iter().min().unwrap_or(resumed))
			}
		};
		let from = from.unwrap_or(floor).max(self.groups.0);
		// A group a replaced copy holds under its cap is that copy's to drain: the serving route
		// is asked from the lowest one none holds. A copy that failed or spent its drain budget
		// delivers nothing more, even while a read still leases it, and one holding a group
		// without its head may give that group up, so neither counts.
		let start = self.groups.0;
		let budget = self.mirrored.budget();
		let drains = |key: u64| {
			self.copies.iter().any(|copy| {
				copy.replaced
					&& copy.done.is_none()
					&& !copy.expired(budget, waiter)
					&& copy.start(start) <= key
					&& copy.end().is_some_and(|end| key < end)
					&& !copy_failed(&copy.track)
					&& copy.track.peek_group(key).is_some_and(|group| group.first_frame() == 0)
			})
		};
		let lowest =
			(delivered.gaps(from..resumed).into_iter()).find_map(|gap| gap.into_iter().find(|key| !drains(*key)));
		// A raised start is asked from its time whatever the local floor, so the backfill
		// places it, and the group holding it is read once the floor lets it.
		if let Some(start) = lowest.into_iter().chain(owed.into_values()).min() {
			// The group holding a time start at or past where it is asked from may lie under it.
			let raised = (self.mirrored.ranges.values())
				.any(|range| range.time().is_some_and(|time| start <= time && time < resumed));
			self.backfill(start..resumed, raised, delivered);
		}
	}

	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	fn latest(&self) -> Option<u64> {
		self.copies.iter().filter_map(|copy| copy.track.latest_key()).max()
	}
}

/// Carries a group across route changes; see the module docs.
///
/// Held by a group handed out from a logical track. It is consulted only once the
/// group's copy fails or stalls, so a passthrough read never touches it.
pub(super) struct Recover {
	key: u64,
	/// The reader it was handed out to, followed onto each new route; `None` for a fetch.
	reader: Option<Weak<Mutex<Reader>>>,
	/// A fetch's own age budget, asked of each route and judged while waiting on one.
	/// `None` leaves the publisher's max age alone; unused for a reader.
	max_delay: Option<std::time::Duration>,
	/// The route, watched for [`Self::wants`]: every read of the group that parks asks it
	/// whether the route moved past [`Self::generation`]. Moving that generation any other
	/// way resets the watch, or a read would skip the move.
	route: kio::Watch<Route>,
	/// The route generation of the copy being read, and that copy.
	generation: u64,
	copy: track::Consumer,
	/// The head of the group as that copy holds it, which the copy continuing it adopts,
	/// so the serving route caches the group whole.
	handoff: Arc<group::Handoff>,
	/// Keeps a replaced copy subscribed while this group is read from it.
	lease: Option<Arc<()>>,
	/// Keeps this group on the reader's list of those still read, so the other copies
	/// holding it, or that may yet get it, stay for a failure to continue from.
	out: Option<Arc<()>>,
	/// A fetch's logical track, held while the group is read. A fetch is no subscription,
	/// so without it the last subscriber leaving parks the track, dropping the copy that
	/// still feeds an open group, and the rest of the group never comes.
	logical: Option<kio::Consumer<track::TrackState>>,
	/// A fetch of the rest of the group from the serving route, by its generation.
	fetch: Option<(u64, Fetch)>,
	/// A recovery started and has not ended, so its events are logged once each.
	recovering: bool,
	/// The index of the serving copy's live groups a pending recovery's expiry searches
	/// within the reader's window, kept across polls so each costs the groups that arrived
	/// since; see [`Self::window`].
	live: Arc<super::track::Live>,
}

enum Fetch {
	Pending(Box<track::Fetching>),
	/// The serving route will not fetch it: wait for its subscription instead.
	Refused(Error),
}

impl Clone for Recover {
	fn clone(&self) -> Self {
		Self {
			key: self.key,
			reader: self.reader.clone(),
			max_delay: self.max_delay,
			route: self.route.clone(),
			generation: self.generation,
			copy: self.copy.clone(),
			handoff: self.handoff.clone(),
			lease: self.lease.clone(),
			out: self.out.clone(),
			logical: self.logical.clone(),
			// A clone reads in parallel, so it asks and logs for itself.
			fetch: None,
			recovering: false,
			live: self.live.clone(),
		}
	}
}

impl Recover {
	/// Whether to look past the copy being read: it stalled, closed short, or failed with a
	/// verdict on the group while a newer route serves, or it failed delivering the group, or
	/// it closed short on a route that failed since, which the front is to replace. A route
	/// that returns to serve the copy being read is no change.
	pub(super) fn wants(&mut self, err: Option<&Error>, waiter: &kio::Waiter) -> bool {
		match err {
			Some(err) if self.undelivered(err) => return true,
			None if copy_failed(&self.copy) => return true,
			_ => {}
		}
		while self
			.route
			.poll(waiter, |route| route.moved_past(self.generation))
			.is_ready()
		{
			// A route that returned serves the copy being read again, which the reader reuses.
			// Closed routes stay ready forever, so they end the wait like a conclusion.
			let route = self.route.consumer().read();
			if route.phase.terminal() || !route.copy.as_ref().is_some_and(|copy| copy.same_channel(&self.copy)) {
				return true;
			}
			self.generation = route.generation;
		}
		false
	}

	/// Log, once per recovery, that the read at frame `index` looks past its copy, and why.
	pub(super) fn recovering(&mut self, index: usize, cause: &Cause) {
		if std::mem::replace(&mut self.recovering, true) {
			return;
		}
		tracing::debug!(
			broadcast = %self.copy.broadcast().path,
			track = self.copy.name(),
			group = self.key,
			frame = index,
			cause = cause.to_string(),
			fetch = self.reader.is_none(),
			read_generation = self.generation,
			serving_generation = self.route.consumer().read().generation,
			"group read recovering",
		);
	}

	/// The read went on without a replacement: its copy answered after all.
	pub(super) fn resumed(&mut self) {
		self.recovering = false;
	}

	/// Log the error a recovery ended the read with, which may differ from its cause.
	pub(super) fn failed(&mut self, index: usize, err: &Error) {
		if std::mem::take(&mut self.recovering) {
			tracing::debug!(
				broadcast = %self.copy.broadcast().path,
				track = self.copy.name(),
				group = self.key,
				frame = index,
				%err,
				read_generation = self.generation,
				serving_generation = self.route.consumer().read().generation,
				"group read recovery failed",
			);
		}
	}

	/// Whether the copy being read failed delivering the group rather than judging it, so
	/// its own route may be asked again. Any other failure would repeat on every copy from
	/// that route, so only another route is asked, which bounds a hostile route to one bad
	/// answer per route change.
	///
	/// A copy that never held the frame read next lags only itself: one asked from that frame
	/// holds it. Too old and evicted judge a subscription's delivery, not whether the group
	/// exists, so neither is a verdict on a fetch: one answered from a group still arriving
	/// over a subscription asks the route again for the rest when that delivery gives up on it.
	fn undelivered(&self, err: &Error) -> bool {
		err.retryable()
			|| copy_failed(&self.copy)
			|| matches!(err, Error::Lagged)
			|| (self.reader.is_none() && matches!(StreamError::from(err), StreamError::Old | StreamError::Evicted))
	}

	/// Poll for the serving route's copy of the group, positioned at frame `index`, once the
	/// copy being read looks past itself for `cause`.
	///
	/// Returns the error that ends the group when nothing can continue it; for
	/// [`Cause::Closed`], why the close stands.
	pub(super) fn poll(&mut self, index: u64, cause: &Cause, waiter: &kio::Waiter) -> Poll<Result<Replacement>> {
		let failed = cause.error();
		// The copy being read will deliver nothing more of the group.
		let settled = !matches!(cause, Cause::Stalled);
		// Subscribe the reader to a new route from its newest group, which is this one or
		// later, so that route's subscription delivers the rest of it.
		if let Some(reader) = self.reader.as_ref().and_then(Weak::upgrade) {
			let mut reader = reader.lock().unwrap_or_else(PoisonError::into_inner);
			reader.sync(waiter);
		}
		loop {
			let (generation, serving, end, moves) = {
				let route = self.route.consumer().read();
				(
					route.generation,
					route.copy.clone(),
					route.phase.end(),
					route.phase.moves(),
				)
			};
			// Once no front will replace the copy, nothing comes after it.
			let done = !moves;
			// Registered before looking, so a route change from here on wakes the reader, a
			// finished track's too. Not through the watch: `generation` is the one just read,
			// not `self.generation`.
			let moved = self
				.route
				.consumer()
				.poll(waiter, |route| {
					match route.generation != generation || !route.phase.moves() {
						true => Poll::Ready(()),
						false => Poll::Pending,
					}
				})
				.is_ready();
			if moved && moves {
				continue;
			}
			if let Some(Err(err)) = &end {
				return Poll::Ready(Err(err.clone()));
			}
			// A failed group continues from any copy still holding it, newest first: its name
			// fixes its content. A stall only moves to the serving copy, so two stalled copies
			// never trade the group back and forth.
			if failed.is_some()
				&& let Some(replacement) = self.held(index)
			{
				return Poll::Ready(Ok(replacement));
			}
			// A verdict would repeat from the route that gave it.
			if let Some(err) = failed
				&& generation == self.generation
				&& !self.undelivered(err)
			{
				return Poll::Ready(Err(err.clone()));
			}
			// An unroutable copy waits for another route: asking its own again could only replace
			// the withdrawal's cause with a refusal. None comes once the front is done.
			let withdrawn = failed.filter(|err| generation == self.generation && err.unroutable());
			if let Some(err) = withdrawn
				&& done
			{
				return Poll::Ready(Err(err.clone()));
			}
			let serving = match (serving, cause) {
				(Some(serving), _) => serving,
				// Nothing replaced a copy that closed the group, as when its track parks, so its
				// close stands.
				(None, Cause::Closed) => return Poll::Ready(Err(Error::NotFound)),
				// Nothing serves: a failed copy waits for a route, unless none will come.
				(None, _) => {
					return match (failed, done) {
						(Some(err), true) => Poll::Ready(Err(err.clone())),
						_ => Poll::Pending,
					};
				}
			};
			if generation == self.generation {
				match cause {
					// The copy being read still serves: only a failure asks it again.
					Cause::Stalled => return Poll::Pending,
					// Its route failed, so the next one may hold more, unless none will come.
					Cause::Closed if copy_failed(&self.copy) && !done => return Poll::Pending,
					// It serves on with no front to replace it, so its close stands.
					Cause::Closed => return Poll::Ready(Err(Error::NotFound)),
					Cause::Failed(_) => {}
				}
			}
			let res = self.poll_serving(generation, &serving, index, settled, withdrawn.is_none(), waiter);
			// The serving route failed too, or will not fetch the rest while its subscription
			// will not deliver it either, and no front is left to replace it.
			if res.is_pending() && settled && done && (copy_failed(&serving) || self.refused(generation, &serving)) {
				return Poll::Ready(Err(failed.cloned().unwrap_or(Error::NotFound)));
			}
			// Judged within the reader's window, as a moved group is: the serving copy's
			// subscription starts at the boundary, and newer groups it holds for others outside
			// the window supersede nothing of the reader's.
			if res.is_pending()
				&& let Some(budget) = self.poll_budget(waiter)
				&& serving
					.poll_stale(self.key, budget, self.window(waiter), &self.live, waiter)
					.is_ready()
			{
				return Poll::Ready(Err(Error::Old));
			}
			return res;
		}
	}

	/// Whether `serving`, the copy of route `generation`, failed or refused the fetch of the
	/// rest, and is the copy that failed or will not deliver the group otherwise, as
	/// [`Self::poll_fetch`] judges a refusal.
	fn refused(&self, generation: u64, serving: &track::Consumer) -> bool {
		matches!(&self.fetch, Some((asked, Fetch::Refused(_))) if *asked == generation)
			&& (generation == self.generation || serving.poll_group(self.key, &kio::Waiter::noop()).is_ready())
	}

	/// The group from the newest of the reader's other copies that still holds it unfailed,
	/// positioned at frame `index`.
	fn held(&self, index: u64) -> Option<Replacement> {
		let reader = self.reader.as_ref()?.upgrade()?;
		let reader = reader.lock().unwrap_or_else(PoisonError::into_inner);
		reader.copies.iter().rev().find_map(|copy| {
			if copy.generation == self.generation {
				return None;
			}
			let group = copy.track.peek_group(self.key)?;
			lend_head(&self.handoff, self.key, &copy.track, &group);
			let group = group.with_range(index..);
			(group.index() == index).then(|| Replacement {
				group,
				generation: copy.generation,
				copy: copy.track.clone(),
			})
		})
	}

	/// [`Self::poll`] once a newer route serves, through its copy `serving`, fetching the rest
	/// from it only if it may `ask`. `settled` says the copy being read delivers no more.
	fn poll_serving(
		&mut self,
		generation: u64,
		serving: &track::Consumer,
		index: u64,
		settled: bool,
		ask: bool,
		waiter: &kio::Waiter,
	) -> Poll<Result<Replacement>> {
		// A newer copy's subscription delivered it: continue from there. The copy that
		// failed delivered it already, so its subscription will not again.
		let same = generation == self.generation;
		match serving.poll_group(self.key, waiter) {
			Poll::Ready(Ok(Some(group))) if !same => {
				lend_head(&self.handoff, self.key, serving, &group);
				let group = group.with_range(index..);
				if group.index() == index {
					return Poll::Ready(Ok(Replacement {
						group,
						generation,
						copy: serving.clone(),
					}));
				}
			}
			// The serving route gave the group up, which ends a reader's group with that
			// route's error however a fetch of the rest would answer. A drop judges that
			// route's subscription, not whether the group exists, so it is no verdict on a
			// fetch; nor is one a copy that failed dropped its open groups with, or a failure
			// of the route it came through.
			Poll::Ready(Err(err)) if self.reader.is_some() && !err.retryable() && !copy_failed(serving) => {
				return Poll::Ready(Err(err));
			}
			_ => {}
		}

		// Finished history may still continue the group above, and so may a fetch already
		// answered. A subscribed group otherwise follows its track's verdict, even while its
		// fetch is unanswered; an independent fetch judges its own outcome.
		let mut verdict = None;
		if self.reader.is_some()
			&& serving.poll_closed(waiter).is_ready()
			&& let Poll::Ready(Err(err)) = serving.poll_complete(&kio::Waiter::noop())
			&& !err.retryable()
		{
			verdict = Some(err);
		}
		match self.poll_fetch(generation, serving, index, settled, ask && verdict.is_none(), waiter) {
			Poll::Ready(Ok(replacement)) => Poll::Ready(Ok(replacement)),
			res => verdict.map_or(res, |err| Poll::Ready(Err(err))),
		}
	}

	/// Ask `serving` for the rest of the group from frame `index`, unless told not to `ask`, and
	/// poll the answer: the group, a refusal that ends a copy that delivers no more (`settled`),
	/// or nothing yet.
	fn poll_fetch(
		&mut self,
		generation: u64,
		serving: &track::Consumer,
		index: u64,
		settled: bool,
		ask: bool,
		waiter: &kio::Waiter,
	) -> Poll<Result<Replacement>> {
		// The subscription may still deliver it first.
		if self.fetch.as_ref().is_none_or(|(asked, _)| *asked != generation) {
			if !ask {
				return Poll::Pending;
			}
			let options = group::Fetch {
				max_delay: self.max_delay,
				frame_start: index,
				..Default::default()
			};
			self.fetch = Some((
				generation,
				Fetch::Pending(Box::new(serving.fetch_group(self.key, options).into_inner())),
			));
		}
		let Some((_, fetch)) = &mut self.fetch else {
			unreachable!("asked above")
		};
		let err = match fetch {
			// That route failed: the next one is asked.
			Fetch::Refused(err) if err.retryable() || copy_failed(serving) => return Poll::Pending,
			Fetch::Refused(err) => err.clone(),
			Fetch::Pending(pending) => match ready!(kio::Task::poll(&mut **pending, waiter)) {
				Ok(group) => {
					lend_head(&self.handoff, self.key, serving, &group);
					let group = group.with_range(index..);
					if group.index() == index {
						return Poll::Ready(Ok(Replacement {
							group,
							generation,
							copy: serving.clone(),
						}));
					}
					*fetch = Fetch::Refused(Error::NotFound);
					Error::NotFound
				}
				// That route is failing too: wait for the next one.
				Err(err) if err.retryable() || copy_failed(serving) => {
					*fetch = Fetch::Refused(err);
					return Poll::Pending;
				}
				Err(err) => {
					*fetch = Fetch::Refused(err.clone());
					err
				}
			},
		};
		// A refusal is a verdict only for a copy that delivers no more, and once the serving
		// subscription will not deliver the group either.
		let same = generation == self.generation;
		if settled && (same || serving.poll_group(self.key, waiter).is_ready()) {
			return Poll::Ready(Err(err));
		}

		Poll::Pending
	}

	/// The budget for a late group: the reader's current one, or a fetch's own, which is
	/// unbounded when it named none so that only the publisher's max age applies.
	pub(super) fn poll_budget(&self, waiter: &kio::Waiter) -> Option<std::time::Duration> {
		let Some(reader) = &self.reader else {
			return Some(self.max_delay.unwrap_or(std::time::Duration::MAX));
		};
		let reader = reader.upgrade()?;
		let mut reader = reader.lock().unwrap_or_else(PoisonError::into_inner);
		// A held group or frame may be the only thing being polled. Mirror and watch
		// preferences here too, before judging its budget against the live edge.
		reader.sync(waiter);
		Some(reader.mirrored.budget())
	}

	/// The groups the reader's ranges resolve to, `None` while a live join waits on a first
	/// group, and its local exclusive cap: what a group moved onto another copy is judged
	/// within, since that copy's own subscription starts at the switch's boundary, and the copy
	/// the group moved off is capped there. `None` once the reader is gone. Registers `waiter`
	/// for a move of the cap; [`Self::poll_budget`] already did for the ranges.
	pub(super) fn window(&self, waiter: &kio::Waiter) -> Option<(Option<Cover>, Option<u64>)> {
		let reader = self.reader.as_ref()?.upgrade()?;
		let mut reader = reader.lock().unwrap_or_else(PoisonError::into_inner);
		waiter.register(&mut reader.capped);
		let cap = reader.groups.1;
		Some((reader.window().groups().cloned(), cap))
	}

	/// Commit the replacement only once the caller has acquired its cursor or frame.
	pub(super) fn adopt(&mut self, replacement: &Replacement) {
		if std::mem::take(&mut self.recovering) {
			tracing::debug!(
				broadcast = %self.copy.broadcast().path,
				track = self.copy.name(),
				group = self.key,
				frame = replacement.group.index(),
				read_generation = self.generation,
				adopted_generation = replacement.generation,
				"group read recovered",
			);
		}
		self.generation = replacement.generation;
		// The generation moved without `wants` seeing it ready, so it judges the new one.
		self.route.reset();
		self.copy = replacement.copy.clone();
		self.handoff = replacement.group.handoff();
		// A later switch must keep this copy subscribed while the recovered group
		// still reads it, just like a group handed out from that copy directly. The newest of
		// the generation is the route's own copy, not a backfill of it.
		self.lease = self.reader.as_ref().and_then(Weak::upgrade).and_then(|reader| {
			let reader = reader.lock().unwrap_or_else(PoisonError::into_inner);
			reader
				.copies
				.iter()
				.rfind(|copy| copy.generation == replacement.generation)
				.map(|copy| copy.lease.clone())
		});
		self.fetch = None;
	}
}

/// Why a group read looks past the copy it reads.
pub(super) enum Cause {
	/// The copy stalled while a newer route serves.
	Stalled,
	/// The copy closed the group short of a known end, a FIN without GROUP_END, after the
	/// front replaced it: the route that replaced it may hold more.
	Closed,
	/// The copy failed with this error.
	Failed(Error),
}

impl Cause {
	/// The error the copy failed with, if it did.
	pub(super) fn error(&self) -> Option<&Error> {
		match self {
			Self::Failed(err) => Some(err),
			Self::Stalled | Self::Closed => None,
		}
	}
}

impl std::fmt::Display for Cause {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::Stalled => f.write_str("stalled behind a newer route"),
			Self::Closed => f.write_str("closed short after the route moved"),
			Self::Failed(err) => err.fmt(f),
		}
	}
}

/// Lend the head `handoff` holds to `serving`'s copy of group `key`, `group`, when that
/// starts later: the fetch for the rest asks only from the reader's frame.
fn lend_head(handoff: &group::Handoff, key: u64, serving: &track::Consumer, group: &group::Consumer) {
	if group.first_frame() > 0
		&& let Some(head) = handoff.head()
	{
		serving.adopt_head(key, &head);
	}
}

/// A candidate cursor, committed only when the requested read can use it.
pub(super) struct Replacement {
	pub(super) group: group::Consumer,
	pub(super) copy: track::Consumer,
	generation: u64,
}

/// A fetch of one group through whichever route serves the logical track.
pub(super) struct Fetching {
	route: Consumer,
	key: u64,
	options: track::FetchOptions,
	logical: kio::Consumer<track::TrackState>,
	/// The fetch in flight, and the copy it went to.
	pending: Option<Asked>,
	/// The generation of a finished track's last copy that failed this fetch, which leaves
	/// it to the copy served after the end.
	left: Option<u64>,
}

/// Which copy a fetch went to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
	/// The serving copy of a route generation.
	Serving(u64),
	/// A staged copy, by stage id: its info is still in flight.
	Staged(u64),
}

struct Asked {
	target: Target,
	fetch: kio::Pending<track::Fetching>,
	/// What a staged copy answered, held until that copy is spliced.
	answer: Option<Result<group::Consumer>>,
}

impl Fetching {
	/// How the staged copy last asked failed the fetch, if it did.
	fn staged_failure(&mut self, waiter: &kio::Waiter) -> Option<Error> {
		let asked = self.pending.as_mut()?;
		if !matches!(asked.target, Target::Staged(_)) {
			return None;
		}
		if asked.answer.is_none()
			&& let Poll::Ready(result) = kio::Task::poll(&mut *asked.fetch, waiter)
		{
			asked.answer = Some(result);
		}
		match &asked.answer {
			Some(Err(err)) => Some(err.clone()),
			_ => None,
		}
	}

	/// Whether the serving copy or the staged one changed since the snapshot of `generation`
	/// and `staged`.
	fn moved(&self, generation: u64, staged: Option<u64>) -> bool {
		let route = self.route.state.read();
		route.generation != generation || route.staged.as_ref().map(|(id, _)| *id) != staged
	}

	/// Poll for the serving copy or the staged one to change, or for no front to be left to
	/// change it.
	fn poll_moved(&self, generation: u64, staged: Option<u64>, waiter: &kio::Waiter) -> Poll<()> {
		match self.route.state.poll(waiter, |route| {
			let moved = route.generation != generation || route.staged.as_ref().map(|(id, _)| *id) != staged;
			match moved || !route.phase.moves() {
				true => Poll::Ready(()),
				false => Poll::Pending,
			}
		}) {
			Poll::Pending => Poll::Pending,
			Poll::Ready(_) => Poll::Ready(()),
		}
	}
}

impl kio::Task for Fetching {
	type Output = Result<group::Consumer>;

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		loop {
			let (generation, serving, staged, spliced) = {
				let route = self.route.state.read();
				(
					route.generation,
					route.answering().map(|(generation, copy)| (generation, copy.clone())),
					route.staged.clone(),
					route.spliced,
				)
			};
			// A finished track's last copy that failed this fetch leaves it to the copy served
			// after the end.
			let serving = match serving {
				Some((last, _)) if last != generation && self.left == Some(last) => {
					self.route.state.read().copy.clone().map(|copy| (generation, copy))
				}
				serving => serving,
			};
			let staged_id = staged.as_ref().map(|(id, _)| *id);
			// A fetch sent to a staged copy carries on once that copy is spliced.
			if let Some(asked) = &mut self.pending
				&& serving.is_some()
				&& let Target::Staged(id) = asked.target
				&& spliced == Some(id)
			{
				asked.target = Target::Serving(generation);
			}
			// The serving copy answers; with none, the copy whose info is in flight is asked
			// at once rather than a round trip later, and judged once it is spliced.
			let (target, copy) = match (serving, staged) {
				(Some((generation, serving)), _) => (Target::Serving(generation), serving),
				(None, Some((id, copy))) if self.route.moves() => (Target::Staged(id), copy),
				(None, _) => {
					if self.route.moves() {
						ready!(self.poll_moved(generation, staged_id, waiter));
						continue;
					}
					// The route moved since the snapshot, as the front left it.
					if self.moved(generation, staged_id) {
						continue;
					}
					// A fetch whose staged copy failed reports that failure once no route
					// follows, as one the serving copy failed does.
					if let Some(err) = self.staged_failure(waiter) {
						return Poll::Ready(Err(err));
					}
					return match self.route.end() {
						Some(Some(Err(err))) => Poll::Ready(Err(err)),
						_ => Poll::Ready(Err(Error::NotFound)),
					};
				}
			};
			// Re-read whenever the route moved past the snapshot, even if no front is left to
			// move it again: the copy served last may answer what the snapshot's cannot.
			if self.poll_moved(generation, staged_id, waiter).is_ready() && self.moved(generation, staged_id) {
				continue;
			}
			if self.pending.as_ref().is_none_or(|asked| asked.target != target) {
				self.pending = Some(Asked {
					target,
					fetch: copy.fetch_range(self.key, self.options.clone()),
					answer: None,
				});
			}
			let asked = self.pending.as_mut().expect("asked above");
			let result = match asked.answer.take() {
				Some(result) => result,
				None => match kio::Task::poll(&mut *asked.fetch, waiter) {
					Poll::Ready(result) => result,
					Poll::Pending => {
						return match self.route.end() {
							Some(Some(Err(err))) => Poll::Ready(Err(err)),
							_ => Poll::Pending,
						};
					}
				},
			};
			// Never handed out before its copy's info passes the front's check.
			let Target::Serving(served) = target else {
				asked.answer = Some(result);
				return match self.route.end() {
					Some(Some(Err(err))) => Poll::Ready(Err(err)),
					_ => Poll::Pending,
				};
			};
			let serving = copy;
			return match result {
				Ok(group) => {
					// The copy may have widened its fill; the caller still reads only its range.
					let group = group
						.with_range(self.options.options.frame_start..)
						.with_end(self.options.end);
					let handoff = group.handoff();
					Poll::Ready(Ok(group.with_recover(Recover {
						key: self.key,
						reader: None,
						max_delay: self.options.options.max_delay,
						route: self.route.watch(),
						generation: served,
						copy: serving.clone(),
						handoff,
						lease: None,
						out: None,
						logical: Some(self.logical.clone()),
						fetch: None,
						recovering: false,
						live: Arc::default(),
					})))
				}
				// A finished track's last copy failed: the copy served after the end is already
				// there, whether or not a front may still replace it.
				Err(err) if (err.retryable() || copy_failed(&serving)) && served != generation => {
					self.left = Some(served);
					continue;
				}
				// The route failed: ask the next one, unless none will come and none came since
				// the snapshot, which a front may have spliced just before concluding. Kept as
				// the answer, since the same copy is never asked twice.
				Err(err)
					if (err.retryable() || copy_failed(&serving))
						&& (self.route.moves() || self.moved(generation, staged_id)) =>
				{
					if let Some(asked) = &mut self.pending {
						asked.answer = Some(Err(err));
					}
					ready!(self.poll_moved(generation, staged_id, waiter));
					continue;
				}
				Err(err) => Poll::Ready(Err(err)),
			};
		}
	}
}

/// A window over `ranges`, each under its key, for a subscriber with `delay`.
fn keyed(
	ranges: impl IntoIterator<Item = (u64, track::Range)>,
	delay: track::Delay,
	holdings: &impl Holdings,
) -> Window {
	Window::keyed(ranges, delay, None, holdings)
}

#[cfg(test)]
mod test {
	use std::time::Duration;

	use futures::FutureExt;

	use super::*;
	use crate::{SessionError, Timestamp, broadcast, track::Delay};

	/// A route's copy of the track.
	fn copy() -> track::Producer {
		track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"video",
			crate::track::Info::timed(),
		)
	}

	/// A route's copy received from a remote publisher, whose groups may land out of order.
	fn received_copy() -> track::Producer {
		let copy = copy();
		copy.received();
		copy
	}

	/// The logical track `routes` serves.
	fn logical(routes: &Producer) -> track::Producer {
		track::Request::new(Arc::new(broadcast::Info::default()), "video")
			.routes(routes.consume())
			.accept(crate::track::Info::timed())
	}

	fn subscribe(logical: &track::Producer, max_delay: Duration) -> track::Subscriber {
		let subscription = Subscription::default().with_delay(Delay::default().with_max(max_delay));
		logical
			.consume()
			.subscribe(subscription)
			.now_or_never()
			.unwrap()
			.unwrap()
	}

	fn recv(sub: &mut track::Subscriber) -> group::Consumer {
		let Poll::Ready(group) = sub.poll_recv_group(&kio::Waiter::noop()) else {
			panic!("a group")
		};
		group.unwrap().expect("track ended")
	}

	fn read(group: &mut group::Consumer) -> Option<Vec<u8>> {
		let Poll::Ready(frame) = group.poll_read_frame(&kio::Waiter::noop()) else {
			panic!("a frame")
		};
		frame.unwrap().map(|frame| frame.payload.to_vec())
	}

	fn ts(ms: u64) -> Timestamp {
		Timestamp::from_millis(ms).unwrap()
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_tracks_abort_wakes_its_handed_out_group_and_keeps_finished_history() {
		for routed in [false, true] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let source = copy();
			routes.serve(source.consume());
			let mut sub = subscribe(if routed { &logical } else { &source }, HOUR);
			let mut history = source.create_group(group::Info { start: 0 }).unwrap();
			history.write_frame(ts(0), b"history".as_ref()).unwrap();
			history.finish().unwrap();
			let mut finished = recv(&mut sub);
			let mut live = source.create_group(group::Info { start: 1 }).unwrap();
			live.write_frame(ts(1), b"live".as_ref()).unwrap();
			let mut reading = recv(&mut sub);
			assert_eq!(read(&mut reading), Some(b"live".to_vec()));
			let wake = Arc::new(Wake::default());
			let waiter = kio::Waiter::new(wake.clone().into());
			assert!(reading.poll_read_frame(&waiter).is_pending());
			wake.0.store(false, std::sync::atomic::Ordering::Relaxed);
			source.abort(Error::GroupTooLarge).unwrap();
			assert!(wake.0.load(std::sync::atomic::Ordering::Relaxed), "routed={routed}");
			let result = reading.poll_read_frame(&waiter);
			assert!(
				matches!(result, Poll::Ready(Err(Error::GroupTooLarge))),
				"routed={routed}: {result:?}"
			);
			assert_eq!(read(&mut finished), Some(b"history".to_vec()));
			assert_eq!(read(&mut finished), None);
			assert!(matches!(
				sub.poll_finished(&waiter),
				Poll::Ready(Err(Error::GroupTooLarge))
			));
			let result = sub
				.poll_recv_group(&waiter)
				.map(|result| result.map(|group| group.map(|group| group.start)));
			assert!(
				matches!(result, Poll::Ready(Err(Error::GroupTooLarge))),
				"routed={routed}: {result:?}"
			);
		}
	}

	/// A task that has not polled yet. Nothing drives these copies' pools, so no wall
	/// deadline wakes a read parked on one: once time passes, a test looks again from a
	/// fresh task, since the same task's read answers as it parked until a wake.
	fn fresh() -> kio::Waiter {
		kio::Waiter::new(Arc::new(Wake::default()).into())
	}

	/// A later stream header is not a verdict on the group held from a failed route.
	#[kio::test]
	async fn a_failed_route_waits_for_a_reordered_group_header() {
		let waiter = kio::Waiter::noop();
		for arrives in [true, false] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, Duration::from_secs(1));
			let mut open = a.create_group(group::Info { start: 0 }).unwrap();
			open.write_frame(ts(0), b"first".as_ref()).unwrap();
			let mut held = recv(&mut sub);
			assert_eq!(read(&mut held), Some(b"first".to_vec()));

			let b = copy();
			let dynamic = b.requests();
			let mut newer = b.create_group(group::Info { start: 1 }).unwrap();
			newer.write_frame(ts(1), b"newer".as_ref()).unwrap();
			newer.finish().unwrap();
			routes.serve(b.consume());
			open.abort(Error::Stream(StreamError::Session(SessionError::Cancel)))
				.unwrap();
			a.abort(Error::Stream(StreamError::Session(SessionError::Cancel)))
				.unwrap();
			assert!(held.poll_read_frame(&waiter).is_pending());
			let Poll::Ready(request) = dynamic.poll_requested_fill_group(&waiter) else {
				panic!("recovery fetch")
			};
			request.unwrap().reject(Error::NotFound);
			assert!(
				held.poll_read_frame(&waiter).is_pending(),
				"the gap is still within grace"
			);

			if arrives {
				let mut late = b.create_group(group::Info { start: 0 }).unwrap();
				late.write_frame(ts(0), b"first".as_ref()).unwrap();
				late.write_frame(ts(1), b"continued".as_ref()).unwrap();
				late.finish().unwrap();
				assert_eq!(read(&mut held), Some(b"continued".to_vec()));
				assert_eq!(read(&mut held), None);
			} else {
				kio::clock::sleep(Duration::from_secs(1)).await;
				assert!(matches!(
					held.poll_read_frame(&fresh()),
					Poll::Ready(Err(Error::NotFound))
				));
			}
		}
	}

	/// A short-budget reader sharing the replacement copy never settles a refused group for
	/// an hour-budget one, whichever recovers first: each reader subscribes to the new route
	/// before judging, so the copy's combined budget always covers its own. Nor does the
	/// hour-budget reader keep the short one waiting past its own budget.
	#[kio::test]
	async fn a_refused_group_waits_for_its_own_readers_grace() {
		let waiter = kio::Waiter::noop();
		let short = Duration::from_millis(100);
		let hour = Duration::from_secs(3600);
		for short_first in [true, false] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let mut quick = subscribe(&logical, short);
			let mut patient = subscribe(&logical, hour);
			let mut open = a.create_group(group::Info { start: 0 }).unwrap();
			open.write_frame(ts(0), b"first".as_ref()).unwrap();
			let mut quick_held = recv(&mut quick);
			let mut patient_held = recv(&mut patient);
			assert_eq!(read(&mut quick_held), Some(b"first".to_vec()));
			assert_eq!(read(&mut patient_held), Some(b"first".to_vec()));

			let b = copy();
			let dynamic = b.requests();
			let mut newer = b.create_group(group::Info { start: 1 }).unwrap();
			newer.write_frame(ts(1), b"newer".as_ref()).unwrap();
			newer.finish().unwrap();
			routes.serve(b.consume());
			open.abort(Error::Stream(StreamError::Session(SessionError::Cancel)))
				.unwrap();
			a.abort(Error::Stream(StreamError::Session(SessionError::Cancel)))
				.unwrap();
			let mut held = [&mut quick_held, &mut patient_held];
			if !short_first {
				held.reverse();
			}
			for group in held {
				assert!(group.poll_read_frame(&waiter).is_pending());
			}
			let mut refused = 0;
			while let Poll::Ready(request) = dynamic.poll_requested_fill_group(&waiter) {
				request.unwrap().reject(Error::NotFound);
				refused += 1;
			}
			assert!(refused > 0, "short_first={short_first}: the recovery fetch arrives");

			kio::clock::sleep(short).await;
			let res = quick_held.poll_read_frame(&waiter);
			assert!(
				matches!(res, Poll::Ready(Err(Error::Old | Error::NotFound))),
				"short_first={short_first}: {res:?}"
			);
			assert!(
				patient_held.poll_read_frame(&waiter).is_pending(),
				"short_first={short_first}: still within the hour"
			);
			drop(quick_held);
			drop(quick);
			let mut late = b.create_group(group::Info { start: 0 }).unwrap();
			late.write_frame(ts(0), b"first".as_ref()).unwrap();
			late.write_frame(ts(1), b"continued".as_ref()).unwrap();
			late.finish().unwrap();
			assert_eq!(
				read(&mut patient_held),
				Some(b"continued".to_vec()),
				"short_first={short_first}"
			);
		}
	}

	#[test]
	fn a_route_preserves_a_mid_group_end() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let mut source = copy();
		routes.serve(source.consume());
		let mut sub = subscribe(&logical, Duration::ZERO);
		assert!(sub.poll_finished(&kio::Waiter::noop()).is_pending());
		let end = Position { group: 5, frame: 5 };
		source.finish_at(end).unwrap();
		assert_eq!(
			sub.poll_finished(&kio::Waiter::noop()).map(|result| result.unwrap()),
			Poll::Ready(end)
		);
	}

	/// The serving copy failing to deliver is no verdict on the track's end: the end waits for
	/// the copy the front serves next.
	#[test]
	fn the_end_waits_out_the_serving_copys_delivery_failure() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		let waiter = kio::Waiter::noop();
		assert!(sub.poll_finished(&waiter).is_pending());
		a.abort(Error::Dropped).unwrap();
		assert!(sub.poll_finished(&waiter).is_pending());
		let mut b = copy();
		routes.serve(b.consume());
		b.finish_at(Position::group(3)).unwrap();
		assert_eq!(
			sub.poll_finished(&waiter).map(Result::unwrap),
			Poll::Ready(Position::group(3))
		);
	}

	/// The serving copy refusing the reader ends the track's end with that refusal, unless it
	/// is a delivery failure, which waits for the front's next copy.
	#[test]
	fn a_refusing_copy_ends_the_end_only_with_a_verdict() {
		for err in [Error::Unauthorized, Error::Dropped] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let request = track::Request::new(Arc::new(broadcast::Info::default()), "video");
			routes.serve(request.consume());
			let mut sub = subscribe(&logical, HOUR);
			let waiter = kio::Waiter::noop();
			assert!(sub.poll_finished(&waiter).is_pending());
			let verdict = !err.retryable();
			request.reject(err);
			let res = sub.poll_finished(&waiter);
			match verdict {
				true => assert!(matches!(res, Poll::Ready(Err(Error::Unauthorized))), "{res:?}"),
				false => assert!(res.is_pending(), "{res:?}"),
			}
		}
	}

	/// A mid-group end caps only the group it falls in: an earlier group is read whole.
	#[test]
	fn a_mid_group_end_caps_only_its_own_group() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let mut source = copy();
		routes.serve(source.consume());
		let mut sub = subscribe(&logical, HOUR);
		let mut whole = source.create_group(group::Info { start: 0 }).unwrap();
		whole.write_frame(ts(0), b"a".as_ref()).unwrap();
		whole.write_frame(ts(1), b"b".as_ref()).unwrap();
		whole.finish().unwrap();
		let mut last = source.create_group(group::Info { start: 2 }).unwrap();
		last.write_frame(ts(2), b"c".as_ref()).unwrap();
		source.finish_at(Position { group: 2, frame: 1 }).unwrap();
		let mut first = recv(&mut sub);
		assert_eq!(read(&mut first), Some(b"a".to_vec()));
		assert_eq!(read(&mut first), Some(b"b".to_vec()));
		assert_eq!(read(&mut first), None);
		let mut capped = recv(&mut sub);
		assert_eq!(read(&mut capped), Some(b"c".to_vec()));
		assert_eq!(read(&mut capped), None);
	}

	#[kio::test]
	async fn a_declared_end_survives_a_swap_before_it_is_polled() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let mut a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		let end = Position { group: 4, frame: 3 };
		a.finish_at(end).unwrap();
		let b = copy();
		routes.serve(b.consume());
		assert_eq!(
			sub.poll_finished(&kio::Waiter::noop()).map(Result::unwrap),
			Poll::Ready(end)
		);
	}

	/// A route switch keeps the error the logical end was declared with, even when the copy
	/// serving at the end declares that end cleanly.
	#[kio::test]
	async fn an_errored_end_survives_a_route_switch() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let mut a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		write(&a, 0);
		a.end_at(Position::group(1), Some(Error::App(7))).unwrap();
		let mut b = copy();
		routes.serve(b.consume());
		write(&b, 0);
		b.finish_at(Position::group(1)).unwrap();
		assert_eq!(
			sub.poll_finished(&kio::Waiter::noop()).map(Result::unwrap),
			Poll::Ready(Position::group(1))
		);
		assert!(matches!(sub.end_error(), Some(Error::App(7))));
		assert!(!sub.ended_with_error(), "group 0 is still to read");
		assert_eq!(recv(&mut sub).start, 0);
		assert!(matches!(
			sub.poll_recv_group(&kio::Waiter::noop()),
			Poll::Ready(Err(Error::App(7)))
		));
		assert!(sub.ended_with_error());
	}

	/// The first end any copy declares stands, refined only by a mid-group end just before
	/// it. A copy contradicting it breaks the track's identity, which hurts only this track:
	/// the reader never fails or panics over it.
	#[kio::test]
	async fn the_first_declared_end_stands() {
		let mid = Position { group: 4, frame: 3 };
		for (first, next, end) in [
			(Position::group(5), Position::group(5), Position::group(5)),
			(Position::group(5), mid, mid),
			(Position::group(5), Position::group(6), Position::group(5)),
			(Position::group(5), Position::group(3), Position::group(5)),
			(mid, Position::group(5), mid),
			(mid, Position { group: 4, frame: 5 }, mid),
		] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let mut a = copy();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, HOUR);
			write(&a, first.group - 1);
			assert_eq!(recv(&mut sub).start, first.group - 1);
			a.finish_at(first).unwrap();
			let mut b = copy();
			b.finish_at(next).unwrap();
			routes.serve(b.consume());
			assert_eq!(
				sub.poll_finished(&kio::Waiter::noop()).map(Result::unwrap),
				Poll::Ready(end),
				"{first:?} then {next:?}"
			);
		}
	}

	/// A mid-group end carries the error its copy declared it with, as does a copy at that
	/// end that can name only the group boundary just after it.
	#[kio::test]
	async fn a_mid_group_end_keeps_the_error_it_was_declared_with() {
		let end = Position { group: 4, frame: 3 };
		for boundary in [false, true] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let mut a = copy();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, HOUR);
			match boundary {
				false => a.end_at(end, Some(Error::App(7))).unwrap(),
				true => {
					a.finish_at(end).unwrap();
					let mut b = copy();
					b.end_at(Position::group(5), Some(Error::App(7))).unwrap();
					routes.serve(b.consume());
				}
			}
			assert_eq!(
				sub.poll_finished(&kio::Waiter::noop()).map(Result::unwrap),
				Poll::Ready(end)
			);
			assert!(matches!(sub.end_error(), Some(Error::App(7))), "boundary={boundary}");
		}
	}

	/// A reader waiting for the track's end wakes when a copy it replaced declares it.
	#[test]
	fn a_replaced_copys_end_wakes_a_reader_waiting_for_it() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let mut a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		write(&a, 0);
		assert_eq!(recv(&mut sub).start, 0);
		let b = copy();
		routes.serve(b.consume());
		let wake = Arc::new(Wake::default());
		let waiter = kio::Waiter::new(wake.clone().into());
		assert!(sub.poll_finished(&waiter).is_pending());
		a.finish_at(Position::group(3)).unwrap();
		assert!(wake.0.load(std::sync::atomic::Ordering::Relaxed));
		assert_eq!(
			sub.poll_finished(&waiter).map(Result::unwrap),
			Poll::Ready(Position::group(3))
		);
	}

	/// A nested front's end reaches the reader through the copy of it the reader replaced
	/// before that copy first answered.
	#[test]
	fn a_nested_end_reaches_the_reader_through_an_unanswered_replaced_copy() {
		let inner_routes = Producer::new();
		let inner = logical(&inner_routes);
		let mut x = copy();
		inner_routes.serve(x.consume());
		x.finish_at(Position::group(3)).unwrap();
		let outer_routes = Producer::new();
		let outer = logical(&outer_routes);
		outer_routes.serve(inner.consume());
		let mut sub = subscribe(&outer, HOUR);
		let b = copy();
		outer_routes.serve(b.consume());
		assert_eq!(
			sub.poll_finished(&kio::Waiter::noop()).map(Result::unwrap),
			Poll::Ready(Position::group(3))
		);
	}

	/// Only a copy at the end that stands says how the track ended: another copy's end, with
	/// its error, leaves the first end clean.
	#[kio::test]
	async fn a_contradicting_copys_end_error_leaves_the_end_clean() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let mut a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		write(&a, 4);
		assert_eq!(recv(&mut sub).start, 4);
		a.finish_at(Position::group(5)).unwrap();
		let mut b = copy();
		b.end_at(Position::group(6), Some(Error::App(7))).unwrap();
		routes.serve(b.consume());
		assert_eq!(
			sub.poll_finished(&kio::Waiter::noop()).map(Result::unwrap),
			Poll::Ready(Position::group(5))
		);
		assert!(sub.end_error().is_none());
	}

	/// A copy holding a group past an end another copy declared never fails the reader.
	#[kio::test]
	async fn content_past_the_declared_end_leaves_the_end() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let mut a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		a.finish_at(Position::group(5)).unwrap();
		let b = copy();
		routes.serve(b.consume());
		write(&b, 4);
		assert_eq!(recv(&mut sub).start, 4);
		write(&b, 5);
		write(&b, 6);
		let _ = drain(&mut sub, false);
		assert_eq!(
			sub.poll_finished(&kio::Waiter::noop()).map(Result::unwrap),
			Poll::Ready(Position::group(5))
		);
	}

	/// A mid-group end another copy declared caps the group it falls in, even for frames
	/// that arrive after the group was handed out.
	#[kio::test]
	async fn a_mid_group_end_caps_another_copys_group() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let mut a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		a.finish_at(Position { group: 4, frame: 2 }).unwrap();
		let b = copy();
		routes.serve(b.consume());
		let mut group = b.create_group(group::Info { start: 4 }).unwrap();
		group.write_frame(ts(0), b"x".as_ref()).unwrap();
		let mut reading = recv(&mut sub);
		// Frames past the end arrive only once the group is handed out.
		for frame in 1..4 {
			group.write_frame(ts(frame), b"x".as_ref()).unwrap();
		}
		group.finish().unwrap();
		assert_eq!(read(&mut reading), Some(b"x".to_vec()));
		assert_eq!(read(&mut reading), Some(b"x".to_vec()));
		assert_eq!(read(&mut reading), None);
	}

	/// A gap that stays open across route flaps keeps one backfill per distinct route: a
	/// later resume point widens the route's backfill rather than asking again.
	#[kio::test]
	async fn route_flaps_keep_one_backfill_per_distinct_route() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let tracks = [copy(), copy()];
		routes.serve(tracks[0].consume());
		let mut sub = subscribe(&logical, HOUR);
		for round in 1..24u64 {
			let track = &tracks[round as usize % 2];
			routes.serve(track.consume());
			// Every odd group never lands, so every resume point leaves a gap.
			write(track, round * 2);
			assert_eq!(drain(&mut sub, false), [round * 2]);
			assert!(
				copies(&routes) <= 4,
				"round {round} retained {} copies",
				copies(&routes)
			);
		}
		write(&tracks[1], 1);
		assert_eq!(drain(&mut sub, false), [1]);
	}

	#[kio::test]
	async fn route_flaps_keep_one_copy_per_distinct_route() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let tracks = [copy(), copy()];
		routes.serve(tracks[0].consume());
		let mut sub = subscribe(&logical, HOUR);
		write(&tracks[0], 0);
		let held = recv(&mut sub);
		for round in 0..32 {
			routes.serve(tracks[round % 2].consume());
			assert!(sub.poll_recv_group(&kio::Waiter::noop()).is_pending());
			assert!(
				copies(&routes) <= 2,
				"round {round} retained {} copies",
				copies(&routes)
			);
		}
		// Returning to a route must not leave its old replacement cap in place.
		write(&tracks[1], 1);
		assert_eq!(recv(&mut sub).start, 1);
		routes.serve(tracks[0].consume());
		write(&tracks[0], 2);
		assert_eq!(recv(&mut sub).start, 2);
		drop(held);
	}

	/// A copy that ended holds nothing past its last group, so it reaches a time start past
	/// that group back to its oldest. The serving copy continues the track and places the
	/// start where the track holds it, which is where the reader's range starts: a group of
	/// the ended copy below it is no part of that range, and nothing between is owed.
	#[kio::test]
	async fn an_ended_copy_leaves_a_time_start_to_the_serving_copy() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let ended = received_copy();
		ended.anchored();
		routes.serve(ended.consume());
		write(&ended, 64);
		let mut live = subscribe(&logical, HOUR);
		let held = recv(&mut live);
		assert_eq!(held.start, 64);

		// The source fails, and a reader joins before its replacement's copy is served.
		ended.clone().abort(Error::Cancel).unwrap();
		let joined = Subscription::default()
			.with_delay(Delay::default().with_max(None))
			.with_ranges([track::Range {
				start: crate::track::Start::Time(66),
				end: None,
			}]);
		let mut joined = logical.consume().subscribe(joined).now_or_never().unwrap().unwrap();
		assert_eq!(drain(&mut joined, false), Vec::<u64>::new());
		let serving = received_copy();
		serving.anchored();
		routes.serve(serving.consume());
		// Fetched ahead of the live feed's 66.
		write(&serving, 67);
		write(&serving, 66);
		let mut read = drain(&mut joined, false);
		read.sort_unstable();
		assert_eq!(read, [66, 67]);
		assert_eq!(joined.resolved_range(0).map(|resolved| resolved.start), Some(66));
		drop(held);
	}

	/// A routed reader's time start that its serving copy can't place yet says what it waits
	/// on and where it ends, as a plain reader's does, so a publisher whose floor already keeps
	/// out the group containing it can say where the range starts.
	#[kio::test]
	async fn a_routed_reader_tells_a_waiting_time_start() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let serving = received_copy();
		serving.anchored();
		routes.serve(serving.consume());
		write(&serving, 1000);
		let ranges = Subscription::default()
			.with_delay(Delay::default().with_max(None))
			.with_ranges([track::Range {
				start: crate::track::Start::Time(500),
				end: None,
			}]);
		let mut open = logical
			.consume()
			.subscribe(ranges.clone())
			.now_or_never()
			.unwrap()
			.unwrap();
		assert!(open.poll_start(&kio::Waiter::noop()).is_pending());
		assert_eq!(open.resolved_range(0), None);
		assert_eq!(open.waiting_keys([0]), [Some((500, None))]);

		// A floor at the time keeps out whichever group contains it: the start waits on nothing.
		let mut floored = logical
			.consume()
			.subscribe(ranges)
			.now_or_never()
			.unwrap()
			.unwrap()
			.with_range(500..);
		assert!(floored.poll_start(&kio::Waiter::noop()).is_ready());
		assert_eq!(floored.resolved_range(0), None);
		assert_eq!(floored.waiting_keys([0]), [Some((500, None))]);
	}

	/// A routed reader places each range its copy resolved, even while an earlier one waits on
	/// a time start: a floor at that time lets the start go out, and the later range names its
	/// group rather than none.
	#[kio::test]
	async fn a_routed_reader_places_a_range_past_one_still_waiting() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let serving = received_copy();
		serving.anchored();
		routes.serve(serving.consume());
		write(&serving, 1000);
		let ranges = Subscription::default()
			.with_delay(Delay::default().with_max(None))
			.with_ranges([
				track::Range {
					start: crate::track::Start::Time(500),
					end: None,
				},
				track::Range {
					start: crate::track::Start::Time(1000),
					end: None,
				},
			]);
		let mut floored = logical
			.consume()
			.subscribe(ranges)
			.now_or_never()
			.unwrap()
			.unwrap()
			.with_range(500..);
		assert!(floored.poll_start(&kio::Waiter::noop()).is_ready());
		assert_eq!(floored.waiting_keys([0, 1]), [Some((500, None)), None]);
		assert_eq!(
			floored.resolved_keys([0, 1]),
			[
				None,
				Some(Resolved {
					start: 1000,
					end: None,
					floored: true,
				})
			]
		);
	}

	/// As [`an_ended_copy_leaves_a_time_start_to_the_serving_copy`], for a reader that replaces
	/// its ranges while its copy has failed: the replacement places them.
	#[kio::test]
	async fn an_ended_copy_leaves_replaced_ranges_to_the_serving_copy() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let ended = received_copy();
		routes.serve(ended.consume());
		write(&ended, 64);
		let mut live = subscribe(&logical, HOUR);
		let held = recv(&mut live);
		assert_eq!(held.start, 64);
		drop(held);
		// The failed copy holds a group at the new start, which still places nothing.
		write(&ended, 66);

		ended.clone().abort(Error::Cancel).unwrap();
		let ranges = live.subscription().with_ranges([track::Range {
			start: crate::track::Start::Time(66),
			end: None,
		}]);
		live.update(ranges).unwrap();
		assert_eq!(drain(&mut live, false), Vec::<u64>::new());
		assert_eq!(live.resolved_range(0), None);
		let serving = received_copy();
		routes.serve(serving.consume());
		write(&serving, 67);
		write(&serving, 66);
		let mut read = drain(&mut live, false);
		read.sort_unstable();
		assert_eq!(read, [66, 67]);
	}

	/// A switch off a failed copy asks the next for the reader's ranges as the reader placed
	/// them, not where the failed copy's cache placed them, from the boundary that copy holds.
	#[test]
	fn a_switch_off_a_failed_copy_asks_for_ranges_as_the_reader_placed_them() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let ended = received_copy();
		routes.serve(ended.consume());
		write(&ended, 64);
		let mut live = subscribe(&logical, HOUR);
		assert_eq!(recv(&mut live).start, 64);
		write(&ended, 66);
		ended.clone().abort(Error::Cancel).unwrap();
		// The failed copy would place this inside group 64.
		let ranges = live.subscription().with_ranges([track::Range {
			start: crate::track::Start::Time(65),
			end: None,
		}]);
		live.update(ranges).unwrap();

		let serving = received_copy();
		routes.serve(serving.consume());
		let asked = serving.subscription().unwrap();
		assert_eq!(
			asked.ranges[0].start,
			crate::track::Start::Time(66),
			"not inside group 64"
		);
		assert_eq!(asked.resume, Some(Position::new(66, 1)));
	}

	/// As [`Self::a_switch_off_a_failed_copy_asks_for_ranges_as_the_reader_placed_them`], with
	/// the reader taking the update before the next route comes: the failed copy places
	/// nothing then either.
	#[test]
	fn a_failed_copy_places_no_range_the_reader_updates() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let ended = received_copy();
		routes.serve(ended.consume());
		write(&ended, 64);
		let mut live = subscribe(&logical, HOUR);
		assert_eq!(recv(&mut live).start, 64);
		write(&ended, 66);
		ended.clone().abort(Error::Cancel).unwrap();
		// The failed copy would place this inside group 64.
		let ranges = live.subscription().with_ranges([track::Range {
			start: crate::track::Start::Time(65),
			end: None,
		}]);
		live.update(ranges).unwrap();
		let _ = live.poll_recv_group(&kio::Waiter::noop());

		let serving = received_copy();
		routes.serve(serving.consume());
		// Raised to the boundary, group 66, which the failed copy holds: not inside group 64.
		let asked = serving.subscription().unwrap();
		assert_eq!(asked.start(), Some(66));
		assert_eq!(asked.resume, Some(Position::new(66, 1)));
	}

	/// A route returned to after the reader moved on is asked to resume past what the reader
	/// now holds, which only a whole ask carries.
	#[test]
	#[cfg(feature = "async")]
	fn a_returning_route_resumes_past_the_reader_now() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		write(&a, 0);
		assert_eq!(recv(&mut sub).start, 0);

		let b = copy();
		routes.serve(b.consume());
		assert_eq!(b.subscription().unwrap().resume, Some(Position::new(0, 1)));
		write(&b, 1);
		assert_eq!(recv(&mut sub).start, 1);
		routes.serve(a.consume());
		assert_eq!(a.subscription().unwrap().resume, Some(Position::new(1, 1)));
	}

	/// A failed copy's group held back for a placement goes out in key order once a later copy
	/// in the same pass places the window below it, rather than being passed over.
	#[kio::test]
	async fn a_held_back_group_goes_out_once_a_later_copy_places_the_window() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let ended = received_copy();
		routes.serve(ended.consume());
		write(&ended, 64);
		let mut live = subscribe(&logical, HOUR);
		let held = recv(&mut live);
		assert_eq!(held.start, 64);

		ended.clone().abort(Error::Cancel).unwrap();
		let joined = Subscription::default()
			.with_delay(Delay::default().with_max(None))
			.with_ranges([track::Range {
				start: crate::track::Start::Time(64),
				end: None,
			}]);
		let mut joined = logical.consume().subscribe(joined).now_or_never().unwrap().unwrap();
		assert_eq!(drain(&mut joined, true), Vec::<u64>::new());
		let serving = received_copy();
		write(&serving, 65);
		routes.serve(serving.consume());
		assert_eq!(drain(&mut joined, true), [64, 65]);
		drop(held);
	}

	/// Its old cap bounds a replaced copy's wire, not the reader's resolved end. Reusing
	/// that copy must ask past the cap while keeping the reader's range and local bounds.
	#[kio::test]
	async fn a_returning_route_keeps_its_resolved_time_end() {
		let end = 10;
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		// The end resolves the reader's exclusive end at 11.
		for key in 2..=11 {
			write(&a, key);
		}
		write(&a, 1001);
		routes.serve(a.consume());
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_ranges([track::Range {
				start: crate::track::Start::Time(2),
				end: Some(end),
			}]);
		let mut sub = logical
			.consume()
			.subscribe(subscription)
			.await
			.unwrap()
			.with_range(2..3);
		let held = recv(&mut sub);
		assert_eq!(held.start, 2);
		assert_eq!(sub.next_in_range(11), None, "the reader resolved its end");
		sub.set_range(4..9);

		// A's copy, capped at the largest group it holds, drains the local range.
		let b = copy();
		routes.serve(b.consume());
		let mut drained = drain(&mut sub, false);
		drained.sort_unstable();
		assert_eq!(drained, [4, 5, 6, 7, 8], "end={end:?}");
		assert_eq!(copies(&routes), 2, "the held group keeps A's capped copy");

		routes.serve(a.consume());
		for key in [1, 12] {
			write(&a, key);
		}
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new(), "end={end:?}");
		// Widening the local cursor still respects the reader's resolved end.
		sub.set_range(4..);
		let mut widened = drain(&mut sub, false);
		widened.sort_unstable();
		assert_eq!(widened, [9, 10], "end={end:?}");
		drop(held);
	}

	/// A route capped while replaced and then returned to reads on to the end of the reader's
	/// bounded range: the cap never becomes the range's own end. Mirrors the `@moq/net` "a
	/// returning route keeps the reader's bounded end across its cap".
	#[kio::test]
	async fn a_returning_route_keeps_the_readers_bounded_end_across_its_cap() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		let mut open = a.create_group(group::Info { start: 0 }).unwrap();
		open.write_frame(ts(0), b"x".as_ref()).unwrap();
		routes.serve(a.consume());
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_ranges([track::Range {
				start: crate::track::Start::Time(0),
				end: Some(100),
			}]);
		let mut sub = logical
			.consume()
			.subscribe(subscription.clone())
			.now_or_never()
			.unwrap()
			.unwrap();
		let held = recv(&mut sub);
		assert_eq!(held.start, 0);

		let b = copy();
		routes.serve(b.consume());
		write(&b, 10);
		write(&b, 20);
		assert_eq!(drain(&mut sub, false), [10, 20]);

		routes.serve(a.consume());
		write(&a, 30);
		assert_eq!(drain(&mut sub, false), [30], "past the cap A had while replaced");
		// An update keeping the ranges asks A again with its own resolution of them.
		sub.update(subscription.with_priority(1)).unwrap();
		write(&a, 40);
		assert_eq!(drain(&mut sub, false), [40], "an update keeps the reader's end");
		open.finish().unwrap();
		drop(held);
	}

	/// A returning route resumes where a fresh copy would, and what it buffered while
	/// replaced below what another route already handed out comes through the backfill:
	/// the late group once, and nothing handed out again.
	#[kio::test]
	async fn a_returning_route_backfills_below_the_readers_newest() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		let b = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		write(&a, 0);
		let held = recv(&mut sub);
		routes.serve(b.consume());
		write(&b, 10);
		write(&b, 20);
		assert_eq!(drain(&mut sub, false), [10, 20]);
		write(&a, 15);
		assert_eq!(copies(&routes), 2, "the held group keeps the first route");
		routes.serve(a.consume());
		assert_eq!(drain(&mut sub, false), [15]);
		write(&a, 21);
		assert_eq!(drain(&mut sub, false), [21]);
		drop(held);
	}

	#[kio::test]
	async fn a_replaced_copy_is_not_kept_for_a_gap_outside_the_window() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		// Held before the reader subscribes, so the second range starts at group 2 itself.
		write(&a, 0);
		write(&a, 2);
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_ranges([track::Range::groups(0..1), track::Range::groups(2..4)]);
		let mut sub = logical
			.consume()
			.subscribe(subscription)
			.now_or_never()
			.unwrap()
			.unwrap();
		let mut read = drain(&mut sub, false);
		read.sort_unstable();
		assert_eq!(read, [0, 2]);
		let b = copy();
		routes.serve(b.consume());
		write(&b, 3);
		assert_eq!(drain(&mut sub, false), [3]);
		assert_eq!(
			copies(&routes),
			1,
			"the only missing group, 1, is outside the reader's window"
		);
	}

	/// A parked copy serves whole groups only while the track may still be served: an ended
	/// track serves nothing from it.
	#[test]
	fn an_ended_track_serves_nothing_from_its_parked_copy() {
		let routes = Producer::new();
		let consumer = routes.consume();
		let a = copy();
		routes.serve(a.consume());
		routes.park();
		assert!(consumer.parked().is_some());
		routes.end(Ok(()));
		assert!(consumer.parked().is_none());
	}

	/// Only the front's verdict ends the track: once concluded, readers still follow the last
	/// copy to its end.
	#[test]
	fn a_concluded_track_has_not_ended() {
		let routes = Producer::new();
		assert!(!routes.ended());
		routes.conclude();
		assert!(!routes.ended(), "readers still follow the last copy");
		routes.end(Err(Error::NotFound));
		assert!(routes.ended());
	}

	/// Releasing the front lets go of the copy it served, so that copy's route goes idle.
	#[test]
	fn a_released_front_serves_no_copy() {
		let routes = Producer::new();
		let consumer = routes.consume();
		let a = copy();
		routes.serve(a.consume());
		assert!(consumer.serving().is_some());
		routes.release();
		assert!(consumer.serving().is_none());
	}

	/// A parked copy no longer serves its reader: its clean end is not the track's, and the
	/// reader follows the next route the front serves.
	#[test]
	fn a_parked_copys_end_leaves_the_reader_for_the_next_route() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		write(&a, 0);
		assert_eq!(drain(&mut sub, false), [0]);
		routes.park();
		let _a = a.finish().unwrap();
		assert!(sub.poll_recv_group(&kio::Waiter::noop()).is_pending());

		let b = copy();
		routes.serve(b.consume());
		write(&b, 1);
		assert_eq!(drain(&mut sub, false), [1]);
	}

	/// Retaining an old copy's cache does not keep a parked logical track live.
	#[test]
	fn a_parked_warm_cache_is_not_live() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let resumed = logical.consume();
		let source = copy();
		let mut warm = source.create_group(group::Info { start: 2 }).unwrap();
		warm.write_frame(crate::Timestamp::from_millis(0).unwrap(), b"2-0".as_ref())
			.unwrap();
		warm.finish().unwrap();
		routes.serve(source.consume());
		assert!(resumed.is_live(), "a serving live copy is live");

		routes.park();
		assert!(source.consume().peek_group(2).is_some(), "the old copy stays warm");
		assert!(routes.consume().serving().is_none(), "no copy is spliced after it");
		assert!(!resumed.is_live(), "a parked warm cache is not live");

		let next = copy();
		routes.serve(next.consume());
		assert!(resumed.is_live(), "the next serving copy is live");
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_reader_reads_the_serving_copy() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::ZERO);

		let mut group = a.create_group(group::Info { start: 0 }).unwrap();
		group.write_frame(ts(0), b"a".as_ref()).unwrap();
		group.finish().unwrap();

		let mut reading = recv(&mut sub);
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));
		assert_eq!(read(&mut reading), None);
	}

	/// A front's reader reports where its ranges resolved, the groups they name, what of
	/// them is accounted for, and whether a group is left unread, as a plain reader of the
	/// serving copy does.
	#[test]
	#[cfg(feature = "async")]
	#[expect(
		clippy::single_range_in_vec_init,
		reason = "the runs and accounted groups are lists, here one"
	)]
	fn a_front_reader_reports_its_ranges_as_a_plain_reader_does() {
		// No start: from the oldest group.
		let range = track::Range {
			start: crate::track::Start::Oldest,
			end: Some(5),
		};
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_ranges([range]);
		for routed in [false, true] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let track = if routed { &logical } else { &a };
			let mut sub = track
				.consume()
				.subscribe(subscription.clone())
				.now_or_never()
				.unwrap()
				.unwrap();
			assert!(drain(&mut sub, false).is_empty());
			assert_eq!(sub.floor(), None, "routed={routed}: the start names no group");
			for sequence in 0..4 {
				write(&a, sequence);
			}
			assert_eq!(drain(&mut sub, false), [0, 1, 2, 3], "routed={routed}");
			// It reaches back to the oldest group rather than naming one.
			assert_eq!(sub.floor(), None, "routed={routed}");
			assert_eq!(
				sub.resolved_range(0),
				Some(Resolved {
					start: 0,
					end: Some(6),
					floored: false,
				}),
				"routed={routed}"
			);
			assert_eq!(sub.runs_in_range(0, 10), [0..6], "routed={routed}");
			assert!(!sub.covered(0, &RangeSet::from_iter([0..5])), "routed={routed}");
			assert!(sub.covered(0, &RangeSet::from_iter([0..6])), "routed={routed}");
			let unread = track::Unread {
				start: 0,
				end: None,
				delivered: &Delivered::default(),
			};
			assert!(!sub.has_unread_group(&unread), "routed={routed}: every group was read");
			write(&a, 4);
			assert!(sub.has_unread_group(&unread), "routed={routed}");
		}
	}

	/// An ordered read that ran out under a lowered cap passes only what lies below it, on a
	/// front's reader as on a plain one: raising the cap reads the rest of a finished track.
	#[test]
	#[cfg(feature = "async")]
	fn an_ordered_read_ended_under_a_cap_reads_on_once_it_rises() {
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_ranges([track::Range::groups(0..3)]);
		for routed in [false, true] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			for sequence in 0..3 {
				write(&a, sequence);
			}
			let _finished = a.clone().finish().unwrap();
			let track = if routed { &logical } else { &a };
			let sub = track
				.consume()
				.subscribe(subscription.clone())
				.now_or_never()
				.unwrap()
				.unwrap();
			let mut ordered = sub.ordered().with_range(..1);
			let noop = kio::Waiter::noop();
			let next = |ordered: &mut track::Ordered| match ordered.poll_next_group(&noop) {
				Poll::Ready(Ok(group)) => Some(group.map(|group| group.start)),
				_ => None,
			};
			assert_eq!(next(&mut ordered), Some(Some(0)), "routed={routed}");
			assert_eq!(next(&mut ordered), Some(None), "routed={routed}: nothing below the cap");
			let mut ordered = ordered.with_range(..);
			let mut rest = Vec::new();
			while let Poll::Ready(Ok(Some(group))) = ordered.poll_next_group(&noop) {
				rest.push(group.start);
			}
			assert_eq!(rest, [1, 2], "routed={routed}");
		}
	}

	/// A front's reader counts the groups its budget skips, as a plain reader does.
	#[test]
	fn a_front_reader_counts_the_groups_its_budget_skips() {
		for routed in [false, true] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let mut sub = subscribe(if routed { &logical } else { &a }, Duration::from_secs(1));
			for second in 0..4 {
				let mut group = a.create_group(group::Info { start: second * 1000 }).unwrap();
				group.write_frame(ts(second * 1000), b"x".as_ref()).unwrap();
				group.finish().unwrap();
			}
			assert_eq!(drain(&mut sub, true), [2000, 3000], "routed={routed}");
			assert_eq!(sub.take_stale().groups, 2, "routed={routed}");
			assert_eq!(sub.take_stale().groups, 0, "routed={routed}: counted once");
		}
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_stalled_group_continues_from_the_replacement() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(10));

		let mut open = a.create_group(group::Info { start: 0 }).unwrap();
		open.write_frame(ts(0), b"a".as_ref()).unwrap();
		open.write_frame(ts(1), b"b".as_ref()).unwrap();
		let mut reading = recv(&mut sub);
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));
		assert_eq!(read(&mut reading), Some(b"b".to_vec()));

		// The replacement holds the whole group; the replaced route stays quiet.
		let b = copy();
		let mut whole = b.create_group(group::Info { start: 0 }).unwrap();
		for (ms, frame) in [(0, b"a"), (1, b"b"), (2, b"c")] {
			whole.write_frame(ts(ms), frame.as_ref()).unwrap();
		}
		routes.serve(b.consume());

		assert_eq!(read(&mut reading), Some(b"c".to_vec()));
		whole.finish().unwrap();
		assert_eq!(read(&mut reading), None);
		assert!(
			sub.recv_group().now_or_never().is_none(),
			"the group was handed out twice"
		);
	}

	/// A verdict from the copy serving now ends the group, though the replaced copy it moved
	/// off still holds the rest: only a newer route or a delivery failure looks past it.
	#[test]
	#[cfg(feature = "async")]
	fn the_serving_copys_verdict_stands_over_a_replaced_copy_holding_the_group() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(10));
		let mut open = a.create_group(group::Info { start: 0 }).unwrap();
		open.write_frame(ts(0), b"a".as_ref()).unwrap();
		let mut reading = recv(&mut sub);
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));

		let b = copy();
		let mut repeat = b.create_group(group::Info { start: 0 }).unwrap();
		for (ms, frame) in [(0, b"a"), (1, b"b")] {
			repeat.write_frame(ts(ms), frame.as_ref()).unwrap();
		}
		routes.serve(b.consume());
		assert_eq!(read(&mut reading), Some(b"b".to_vec()), "the stall moved to b");

		for (ms, frame) in [(1, b"b"), (2, b"c")] {
			open.write_frame(ts(ms), frame.as_ref()).unwrap();
		}
		open.finish().unwrap();
		repeat.abort(Error::WrongSize).unwrap();
		let res = reading.read_frame().now_or_never();
		assert!(matches!(res, Some(Err(Error::WrongSize))), "{res:?}");
	}

	/// A group the replaced copy closed short (a FIN without GROUP_END) continues from the
	/// replacement at the next frame, whichever of the close and the switch the reader sees
	/// first, and ends where the replacement ends it.
	#[test]
	#[cfg(feature = "async")]
	fn a_replaced_copys_short_group_continues_from_the_replacement() {
		for parked in [false, true] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, Duration::from_secs(300));

			let mut short = a.create_group(group::Info { start: 0 }).unwrap();
			short.write_frame(ts(0), b"a".as_ref()).unwrap();
			let mut reading = recv(&mut sub);
			assert_eq!(read(&mut reading), Some(b"a".to_vec()));
			// Its end, too, is the replacement's.
			let mut finishing = reading.clone();
			if parked {
				assert!(reading.poll_read_frame(&kio::Waiter::noop()).is_pending());
			}
			short.finish().unwrap();

			let b = copy();
			let mut whole = b.create_group(group::Info { start: 0 }).unwrap();
			for (ms, frame) in [(0, b"a"), (1, b"b"), (2, b"c")] {
				whole.write_frame(ts(ms), frame.as_ref()).unwrap();
			}
			routes.serve(b.consume());

			assert_eq!(read(&mut reading), Some(b"b".to_vec()), "parked={parked}");
			assert_eq!(read(&mut reading), Some(b"c".to_vec()), "parked={parked}");
			assert!(reading.poll_read_frame(&kio::Waiter::noop()).is_pending());
			assert!(finishing.poll_finished(&kio::Waiter::noop()).is_pending());
			whole.finish_at(3).unwrap();
			assert!(matches!(
				finishing.poll_finished(&kio::Waiter::noop()),
				Poll::Ready(Ok(1))
			));
			assert_eq!(read(&mut reading), None, "parked={parked}");
			assert_eq!(reading.end(), Some(3), "parked={parked}: whole from the replacement");
			assert!(sub.recv_group().now_or_never().is_none(), "handed out twice");
		}
	}

	/// A short group whose cache freed it once the reader drained it still reads on from the
	/// replacement: what a copy's cache retains never decides what the reader gets.
	#[test]
	#[cfg(feature = "async")]
	fn a_released_short_group_continues_from_the_replacement() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(300));

		let mut short = a.create_group(group::Info { start: 0 }).unwrap();
		short.write_frame(ts(0), b"a".as_ref()).unwrap();
		let mut reading = recv(&mut sub);
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));
		short.finish().unwrap().abort(Error::Old).unwrap();

		let b = copy();
		let mut whole = b.create_group(group::Info { start: 0 }).unwrap();
		for (ms, frame) in [(0, b"a"), (1, b"b")] {
			whole.write_frame(ts(ms), frame.as_ref()).unwrap();
		}
		whole.finish_at(2).unwrap();
		routes.serve(b.consume());

		assert_eq!(read(&mut reading), Some(b"b".to_vec()));
		assert_eq!(read(&mut reading), None);
		assert_eq!(reading.end(), Some(2), "whole from the replacement");
	}

	/// A group a copy closed short before its route failed waits for the route replacing it,
	/// which reads it on, rather than ending where the failed copy closed it.
	#[test]
	#[cfg(feature = "async")]
	fn a_short_group_on_a_failed_route_waits_for_the_next() {
		let routes = Producer::new();
		let front = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&front, HOUR);
		let mut short = a.create_group(group::Info { start: 0 }).unwrap();
		short.write_frame(ts(0), b"a".as_ref()).unwrap();
		short.finish().unwrap();
		let mut reading = recv(&mut sub);
		a.abort(Error::Stream(StreamError::Session(SessionError::Cancel)))
			.unwrap();
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));
		assert!(
			reading.read_frame().now_or_never().is_none(),
			"waits for the next route"
		);

		let b = copy();
		let mut whole = b.create_group(group::Info { start: 0 }).unwrap();
		for (ms, frame) in [(0, b"a"), (1, b"b")] {
			whole.write_frame(ts(ms), frame.as_ref()).unwrap();
		}
		whole.finish_at(2).unwrap();
		routes.serve(b.consume());
		assert_eq!(read(&mut reading), Some(b"b".to_vec()));
		assert_eq!(read(&mut reading), None);
	}

	/// A replacement that holds none of a short group is asked for the rest, which continues
	/// the group; a refusal leaves it to the replacement's subscription.
	#[test]
	#[cfg(feature = "async")]
	fn a_short_group_the_replacement_lacks_is_fetched() {
		for answered in [true, false] {
			let routes = Producer::new();
			let front = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let mut sub = subscribe(&front, HOUR);
			let mut short = a.create_group(group::Info { start: 0 }).unwrap();
			short.write_frame(ts(0), b"a".as_ref()).unwrap();
			let mut reading = recv(&mut sub);
			assert_eq!(read(&mut reading), Some(b"a".to_vec()));
			short.finish().unwrap();

			let b = copy();
			let dynamic = b.requests();
			routes.serve(b.consume());
			assert!(reading.read_frame().now_or_never().is_none(), "answered={answered}");
			let request = dynamic.requested_fill_group().now_or_never().unwrap().unwrap();
			assert_eq!((request.key(), request.frame_start()), (0, 1), "answered={answered}");
			if answered {
				let mut rest = request.accept(None).unwrap().expect("a fresh group");
				rest.write_frame(ts(1), b"b".as_ref()).unwrap();
				rest.finish_at(2).unwrap();
				assert_eq!(read(&mut reading), Some(b"b".to_vec()));
				assert_eq!(read(&mut reading), None);
				assert_eq!(reading.end(), Some(2));
			} else {
				// A refusal leaves it to the subscription, which ends it where it closed.
				request.reject(Error::NotFound);
				assert!(reading.read_frame().now_or_never().is_none(), "still waiting");
				let track::Received::Group(none) = b.create_group_at(group::Info { start: 0 }, 1, 0..0).unwrap() else {
					panic!("nothing else holds the group");
				};
				none.finish().unwrap();
				assert_eq!(read(&mut reading), None);
			}
		}
	}

	/// As [`a_short_group_the_replacement_lacks_is_fetched`], with the replacement a front of
	/// its own: a refused fetch of the rest is no verdict while the group may still land
	/// there, which reads it on once it does, or ends it where that copy closed it.
	#[test]
	#[cfg(feature = "async")]
	fn a_short_group_a_nested_replacement_lacks_reads_on_once_it_lands() {
		for whole in [true, false] {
			let routes = Producer::new();
			let front = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let mut sub = subscribe(&front, HOUR);
			let mut short = a.create_group(group::Info { start: 0 }).unwrap();
			short.write_frame(ts(0), b"a".as_ref()).unwrap();
			let mut reading = recv(&mut sub);
			assert_eq!(read(&mut reading), Some(b"a".to_vec()));
			short.finish().unwrap();

			let inner_routes = Producer::new();
			let inner = logical(&inner_routes);
			let b = copy();
			let dynamic = b.requests();
			inner_routes.serve(b.consume());
			routes.serve(inner.consume());
			assert!(reading.read_frame().now_or_never().is_none(), "whole={whole}");
			let request = dynamic.requested_fill_group().now_or_never().unwrap().unwrap();
			assert_eq!(request.key(), 0, "whole={whole}");
			request.reject(Error::NotFound);
			assert!(
				reading.read_frame().now_or_never().is_none(),
				"whole={whole}: still waiting"
			);

			let mut landed = b.create_group(group::Info { start: 0 }).unwrap();
			landed.write_frame(ts(0), b"a".as_ref()).unwrap();
			if whole {
				landed.write_frame(ts(1), b"b".as_ref()).unwrap();
				landed.finish_at(2).unwrap();
				assert_eq!(read(&mut reading), Some(b"b".to_vec()));
			} else {
				landed.finish().unwrap();
			}
			assert_eq!(read(&mut reading), None, "whole={whole}");
		}
	}

	/// A short group the replacement neither holds nor fetches ends where it closed once the
	/// reader's budget gives up on it, where a stalled one fails as too old.
	#[test]
	#[cfg(feature = "async")]
	fn a_short_group_ends_where_it_closed_once_stale() {
		let routes = Producer::new();
		let front = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&front, Duration::from_secs(2));
		let mut short = a.create_group(group::Info { start: 0 }).unwrap();
		short.write_frame(ts(0), b"a".as_ref()).unwrap();
		let mut reading = recv(&mut sub);
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));
		short.finish().unwrap();

		let b = copy();
		routes.serve(b.consume());
		for key in 1..=2 {
			let mut group = b.create_group(group::Info { start: key * 1000 }).unwrap();
			group.write_frame(ts(key * 1000), b"b".as_ref()).unwrap();
			group.finish().unwrap();
		}
		assert!(reading.read_frame().now_or_never().is_none(), "not stale yet");

		let mut group = b.create_group(group::Info { start: 3000 }).unwrap();
		group.write_frame(ts(3000), b"b".as_ref()).unwrap();
		assert_eq!(read(&mut reading), None);
	}

	/// A short group ends where it closed once no route can supply more: the replacement holds
	/// no more of it, or none is left to ask.
	#[test]
	#[cfg(feature = "async")]
	fn a_short_group_ends_once_no_route_holds_more() {
		// The replacement closed it at the same frame.
		let routes = Producer::new();
		let front = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&front, HOUR);
		let mut short = a.create_group(group::Info { start: 0 }).unwrap();
		short.write_frame(ts(0), b"a".as_ref()).unwrap();
		short.finish().unwrap();
		let mut reading = recv(&mut sub);
		let b = copy();
		let mut also = b.create_group(group::Info { start: 0 }).unwrap();
		also.write_frame(ts(0), b"a".as_ref()).unwrap();
		also.finish().unwrap();
		routes.serve(b.consume());
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));
		assert_eq!(read(&mut reading), None);

		// No route is left to ask.
		let routes = Producer::new();
		let front = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&front, HOUR);
		let mut short = a.create_group(group::Info { start: 0 }).unwrap();
		short.write_frame(ts(0), b"a".as_ref()).unwrap();
		short.finish().unwrap();
		let mut reading = recv(&mut sub);
		drop(routes);
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));
		assert_eq!(read(&mut reading), None);

		// The copy being read still serves: its close stands.
		let routes = Producer::new();
		let front = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&front, HOUR);
		let mut short = a.create_group(group::Info { start: 0 }).unwrap();
		short.write_frame(ts(0), b"a".as_ref()).unwrap();
		short.finish().unwrap();
		let mut reading = recv(&mut sub);
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));
		assert_eq!(read(&mut reading), None);
	}

	/// A switch asks the new copy for the newest group handed out from past the frames the
	/// copies hold of it, never again from its first: the rest of one still arriving, and
	/// nothing of a finished one. The reader reads the group on from the part the new copy
	/// gets, every frame once.
	#[test]
	#[cfg(feature = "async")]
	fn a_switch_resumes_the_newest_group_past_the_frames_held() {
		let routes = Producer::new();
		let track = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&track, Duration::from_secs(10));

		let mut open = a.create_group(group::Info { start: 0 }).unwrap();
		open.write_frame(ts(0), b"a".as_ref()).unwrap();
		open.write_frame(ts(1), b"b".as_ref()).unwrap();
		let mut reading = recv(&mut sub);
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));

		let b = copy();
		routes.serve(b.consume());
		assert_eq!(read(&mut reading), Some(b"b".to_vec()));
		let asked = b.subscription().expect("subscribed");
		assert_eq!(asked.start(), Some(0));
		assert_eq!(asked.resume, Some(Position::new(0, 2)), "past the two frames held");

		// The new copy gets only the rest, and the reader reads on from it.
		let mut rest = b.create_group(group::Info { start: 0 }).unwrap();
		rest.start_at(2).unwrap();
		rest.write_frame(ts(2), b"c".as_ref()).unwrap();
		rest.finish().unwrap();
		assert_eq!(read(&mut reading), Some(b"c".to_vec()));
		assert_eq!(read(&mut reading), None);
		assert!(sub.recv_group().now_or_never().is_none(), "handed out twice");

		// A finished group is resumed past its every frame, so none of it is sent again.
		write(&b, 1);
		assert_eq!(recv(&mut sub).start, 1);
		let c = copy();
		routes.serve(c.consume());
		assert_eq!(c.subscription().expect("subscribed").resume, Some(Position::new(1, 1)));

		// A reader yet to read the boundary group resumes past the frames held all the same:
		// the copy holding them is capped there, and lends them to the new one.
		let routes = Producer::new();
		let front = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut first = subscribe(&front, HOUR);
		let mut second = subscribe(&front, HOUR);
		write(&a, 5);
		assert_eq!(recv(&mut first).start, 5);
		let demand = front.subscription().unwrap();
		assert_eq!(routes.subscription(demand.clone(), 2).resume, Some(Position::new(5, 1)));
		assert_eq!(recv(&mut second).start, 5);
		assert_eq!(routes.subscription(demand, 2).resume, Some(Position::new(5, 1)));
	}

	/// A switch caps the copy it replaces at the frame the new one resumes the boundary group
	/// at, so the old route stops sending it there, and the reader still reads each frame once:
	/// one already in flight on the old route, then the rest from the new one.
	#[test]
	#[cfg(feature = "async")]
	fn a_switch_caps_the_replaced_copy_at_the_frame_the_new_one_resumes() {
		let routes = Producer::new();
		let track = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&track, Duration::from_secs(10));

		let mut old = a.create_group(group::Info { start: 0 }).unwrap();
		old.write_frame(ts(0), b"a".as_ref()).unwrap();
		old.write_frame(ts(1), b"b".as_ref()).unwrap();
		let mut reading = recv(&mut sub);
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));
		assert_eq!(
			a.subscription().expect("subscribed").cap,
			None,
			"a serving copy is uncapped"
		);

		let b = copy();
		routes.serve(b.consume());
		assert_eq!(read(&mut reading), Some(b"b".to_vec()));
		assert_eq!(b.subscription().expect("subscribed").resume, Some(Position::new(0, 2)));
		let capped = a.subscription().expect("still subscribed");
		assert_eq!(
			capped.cap,
			Some(Position::new(0, 2)),
			"stopped where the new copy resumes"
		);
		assert!(
			capped.ranges.iter().all(|range| range.end == Some(0)),
			"{:?}",
			capped.ranges
		);

		// A frame already in flight on the old route lands there, and the rest on the new one.
		old.write_frame(ts(2), b"c".as_ref()).unwrap();
		let mut rest = b.create_group(group::Info { start: 0 }).unwrap();
		rest.start_at(2).unwrap();
		rest.write_frame(ts(2), b"c".as_ref()).unwrap();
		rest.write_frame(ts(3), b"d".as_ref()).unwrap();
		rest.finish().unwrap();
		assert_eq!(read(&mut reading), Some(b"c".to_vec()));
		// The old route never finishes the group, which it stopped: the read moves on.
		assert_eq!(read(&mut reading), Some(b"d".to_vec()));
		assert_eq!(read(&mut reading), None);
		assert!(sub.recv_group().now_or_never().is_none(), "handed out twice");

		// A copy returning to serve drops its cap, and the one it replaces takes it.
		let routes = Producer::new();
		let track = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&track, HOUR);
		let mut old = a.create_group(group::Info { start: 0 }).unwrap();
		old.write_frame(ts(0), b"a".as_ref()).unwrap();
		let mut reading = recv(&mut sub);
		let b = copy();
		routes.serve(b.consume());
		assert_eq!(a.subscription().unwrap().cap, Some(Position::new(0, 1)));
		routes.serve(a.consume());
		assert_eq!(a.subscription().unwrap().cap, None, "serving again");
		assert_eq!(b.subscription().unwrap().cap, None, "b holds none of the group's head");
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));
	}

	/// A copy asked past the frames a reader held of a group adopts them once the rest lands,
	/// so a reader joining after the copy that held them is gone reads the group whole.
	#[test]
	#[cfg(feature = "async")]
	fn a_resumed_copy_holds_the_group_whole_for_a_later_reader() {
		let routes = Producer::new();
		let track = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut holder = subscribe(&track, HOUR);
		write(&a, 0);
		assert_eq!(recv(&mut holder).start, 0);

		let b = copy();
		routes.serve(b.consume());
		assert_eq!(b.subscription().unwrap().resume, Some(Position::new(0, 1)));
		// As a session receives it, partway through. The holder never polls again, so the
		// copy takes the head on arrival, not from the holder's reads.
		let track::Received::Group(rest) = b.create_group_at(group::Info { start: 0 }, 1, 0..0).unwrap() else {
			panic!("nothing else holds the group");
		};
		rest.finish().unwrap();
		drop(a);

		let mut late = subscribe(&track, HOUR);
		let mut group = recv(&mut late);
		assert_eq!(group.start, 0);
		assert_eq!(read(&mut group), Some(b"x".to_vec()));
		assert_eq!(read(&mut group), None);
	}

	/// The subscription an origin opens on a new copy before serving it, which it then holds
	/// for the readers, lends the head it resumes past, as a reader's own does.
	/// Otherwise a reader moving on before the switch lends the next group's head instead,
	/// and the copy caches the group the query resumed without its head.
	#[test]
	#[cfg(feature = "async")]
	fn a_query_resuming_a_new_copy_lends_it_the_head() {
		let routes = Producer::new();
		let track = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut holder = subscribe(&track, HOUR);
		write(&a, 0);
		assert_eq!(recv(&mut holder).start, 0);

		let b = copy();
		let _held = routes.subscribe(&b.consume(), track.subscription().unwrap(), 1);
		assert_eq!(b.subscription().unwrap().resume, Some(Position::new(0, 1)));
		write(&a, 1);
		assert_eq!(recv(&mut holder).start, 1);
		routes.serve(b.consume());

		for key in [0, 1] {
			let track::Received::Group(rest) = b.create_group_at(group::Info { start: key }, 1, 0..0).unwrap() else {
				panic!("nothing else holds group {key}");
			};
			rest.finish().unwrap();
			let cached = b.consume().peek_group(key).expect("cached");
			assert_eq!(cached.first_frame(), 0, "group {key} is whole once visible");
		}
	}

	/// Routes that close without concluding read like a conclusion: a reader stalled
	/// mid-group parks on the copy it is reading rather than polling the closed routes
	/// forever, and finishes the group from that copy.
	#[kio::test]
	async fn a_stalled_group_parks_when_the_routes_close() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(10));

		let mut open = a.create_group(group::Info { start: 0 }).unwrap();
		open.write_frame(ts(0), b"a".as_ref()).unwrap();
		let mut reading = recv(&mut sub);
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));

		drop(routes);
		assert!(reading.poll_read_frame(&kio::Waiter::noop()).is_pending());

		open.write_frame(ts(1), b"b".as_ref()).unwrap();
		assert_eq!(read(&mut reading), Some(b"b".to_vec()));
		open.finish().unwrap();
		assert_eq!(read(&mut reading), None);
	}

	/// A plain reader of a compressed group inflates from frame 0, so the replacement that
	/// fetches only the rest adopts the failed route's compressed head.
	#[test]
	#[cfg(feature = "async")]
	fn a_failed_compressed_group_continues_from_the_replacement() {
		let info = crate::track::Info::timed().with_compression(crate::track::Compression::Deflate);
		let compressed = || track::Producer::new(Arc::new(broadcast::Info::default()), "video", info.clone());
		let routes = Producer::new();
		let logical = track::Request::new(Arc::new(broadcast::Info::default()), "video")
			.routes(routes.consume())
			.accept(info.clone());
		let a = compressed();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(10));
		let frames: Vec<Vec<u8>> = (0..3)
			.map(|index| format!("{index}{}", "z".repeat(64)).into())
			.collect();

		let mut open = a.create_group(group::Info { start: 0 }).unwrap();
		open.write_frame(ts(0), frames[0].clone()).unwrap();
		open.write_frame(ts(1), frames[1].clone()).unwrap();
		let mut reading = recv(&mut sub);
		assert_eq!(read(&mut reading).as_ref(), Some(&frames[0]));
		assert_eq!(read(&mut reading).as_ref(), Some(&frames[1]));
		open.abort(Error::Dropped).unwrap();

		let b = compressed();
		let dynamic = b.requests();
		routes.serve(b.consume());
		assert!(reading.read_frame().now_or_never().is_none());
		let request = dynamic.requested_fill_group().now_or_never().unwrap().unwrap();
		assert_eq!((request.key(), request.frame_start()), (0, 2));
		let mut rest = request.accept(None).unwrap().expect("a fresh group");
		rest.write_frame(ts(2), frames[2].clone()).unwrap();
		rest.finish().unwrap();
		assert_eq!(read(&mut reading).as_ref(), Some(&frames[2]));
		assert_eq!(read(&mut reading), None);
		assert_eq!(read_cached(&b.consume(), 0), frames);
	}

	/// A slice that inflates to other than its declared size fails a plain read of the
	/// logical track, instead of resuming it from the copy that failed, forever.
	#[test]
	#[cfg(feature = "async")]
	fn a_malformed_compressed_frame_fails_the_read() {
		let info = crate::track::Info::timed().with_compression(crate::track::Compression::Deflate);
		let routes = Producer::new();
		let logical = track::Request::new(Arc::new(broadcast::Info::default()), "video")
			.routes(routes.consume())
			.accept(info.clone());
		let a = track::Producer::new(Arc::new(broadcast::Info::default()), "video", info);
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(10));
		let frame = || bytes::Bytes::from("z".repeat(64));

		let mut group = a.create_group(group::Info { start: 0 }).unwrap();
		group.write_frame(ts(0), frame()).unwrap();
		let slice = super::super::flate::Encoder::new().frame(&frame());
		let header = crate::model::frame::Header {
			info: crate::model::frame::Info {
				size: slice.len() as u64,
				timestamp: ts(1),
			},
			fields: None,
			inflated: Some(frame().len() as u64 + 1),
		};
		let mut corrupt = group.create_object(header).unwrap();
		corrupt.write(slice).unwrap();
		let group = corrupt.finish().unwrap();
		group.finish().unwrap();

		let mut reading = recv(&mut sub);
		assert_eq!(read(&mut reading), Some(frame().to_vec()));
		let res = reading.read_frame().now_or_never().expect("a result");
		assert!(matches!(res, Err(Error::WrongSize)), "{res:?}");
	}

	/// Once the failed route's compressed head is reclaimed, a replacement holding only the
	/// rest cannot be inflated, so the plain read fails with the route's error.
	#[test]
	#[cfg(feature = "async")]
	fn a_compressed_group_without_its_head_fails_the_read() {
		let pool = crate::cache::Pool::new(crate::cache::Config::default().with_capacity(1 << 40));
		let broadcast = Arc::new(broadcast::Info {
			pool: pool.clone(),
			..Default::default()
		});
		let info = crate::track::Info::timed().with_compression(crate::track::Compression::Deflate);
		let routes = Producer::new();
		let logical = track::Request::new(broadcast.clone(), "video")
			.routes(routes.consume())
			.accept(info.clone());
		let a = track::Producer::new(broadcast.clone(), "video", info.clone());
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(10));
		let frame = || bytes::Bytes::from("z".repeat(64));

		let mut open = a.create_group(group::Info { start: 0 }).unwrap();
		open.write_frame(ts(0), frame()).unwrap();
		let mut reading = recv(&mut sub);
		assert_eq!(read(&mut reading), Some(frame().to_vec()));
		let handoff = open.consume().handoff();
		open.abort(Error::Dropped).unwrap();
		drop(a);

		let b = track::Producer::new(broadcast.clone(), "video", info.clone());
		let dynamic = b.requests();
		routes.serve(b.consume());
		assert!(reading.read_frame().now_or_never().is_none());
		let request = dynamic.requested_fill_group().now_or_never().unwrap().unwrap();
		assert_eq!(request.frame_start(), 1);

		// Shrink the pool so the next write reclaims the held head.
		let other = track::Producer::new(broadcast, "other", crate::track::Info::timed());
		pool.resize(pool.used().saturating_sub(1));
		let mut filler = other.append_next().unwrap();
		filler.write_frame(ts(0), frame()).unwrap();
		filler.finish().unwrap();
		assert!(handoff.head().is_none(), "the shrink reclaimed the head");

		let mut rest = request.accept(None).unwrap().expect("a fresh group");
		rest.write_frame(ts(1), frame()).unwrap();
		rest.finish().unwrap();
		let res = reading.read_frame().now_or_never().expect("fails instead of spinning");
		assert!(matches!(res, Err(Error::Dropped)), "{res:?}");
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_half_read_frame_continues_from_the_replacement() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(10));

		let open = a.create_group(group::Info { start: 0 }).unwrap();
		let mut frame = open
			.create_frame(crate::frame::Info {
				size: 4,
				timestamp: ts(0),
			})
			.unwrap();
		frame.write(b"ab".as_ref()).unwrap();
		let mut reading = recv(&mut sub);
		let mut chunks = reading.next_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(
			&chunks.read_chunk().now_or_never().unwrap().unwrap().unwrap()[..],
			b"ab"
		);

		// The route dies mid-frame, and the replacement holds the frame whole.
		drop(frame);
		a.clone().abort(Error::Dropped).unwrap();
		let b = copy();
		let mut whole = b.create_group(group::Info { start: 0 }).unwrap();
		whole.write_frame(ts(0), b"abcd".as_ref()).unwrap();
		routes.serve(b.consume());

		assert_eq!(
			&chunks.read_chunk().now_or_never().unwrap().unwrap().unwrap()[..],
			b"cd"
		);
		assert!(chunks.read_chunk().now_or_never().unwrap().unwrap().is_none());
	}

	/// A resumed group that the serving route gives up or aborts ends with that route's
	/// error, even with no successor or later writes and its fetch still unanswered.
	#[test]
	#[cfg(feature = "async")]
	fn a_resumed_group_ends_with_the_serving_routes_verdict() {
		for (fails, abort) in [(false, false), (true, false), (false, true), (true, true)] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, Duration::from_secs(10));

			let mut open = a.create_group(group::Info { start: 0 }).unwrap();
			open.write_frame(ts(0), b"a".as_ref()).unwrap();
			let mut reading = recv(&mut sub);
			assert_eq!(read(&mut reading), Some(b"a".to_vec()));
			if fails {
				a.clone().abort(Error::Dropped).unwrap();
			}

			let b = copy();
			let dynamic = b.requests();
			routes.serve(b.consume());
			assert!(reading.read_frame().now_or_never().is_none(), "fails={fails}");
			let _pending = dynamic.requested_fill().now_or_never().unwrap().unwrap();

			if abort {
				b.abort(Error::Stream(StreamError::App(5))).unwrap();
			} else {
				b.drop_groups(0..1, StreamError::App(5));
			}
			let res = reading.read_frame().now_or_never();
			assert!(
				matches!(res, Some(Err(Error::Stream(StreamError::App(5))))),
				"fails={fails}, abort={abort}: {res:?}"
			);
		}
	}

	/// A recovery logs its cause and route generations once when it starts, however often the
	/// read parks, then the copy it adopted or the error it ended the read with.
	#[test]
	#[cfg(feature = "async")]
	fn a_recovery_logs_its_start_and_outcome() {
		let events = |message: &str, events: &[String]| {
			events
				.iter()
				.filter(|event| event.contains(&format!("message={message} ")))
				.cloned()
				.collect::<Vec<_>>()
		};

		// A stall while a newer route serves, which the replacement continues.
		let logs = crate::test_tracing::capture(tracing::Level::DEBUG, || {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, Duration::from_secs(10));
			let mut open = a.create_group(group::Info { start: 0 }).unwrap();
			open.write_frame(ts(0), b"a".as_ref()).unwrap();
			let mut reading = recv(&mut sub);
			assert_eq!(read(&mut reading), Some(b"a".to_vec()));

			let b = copy();
			routes.serve(b.consume());
			for _ in 0..3 {
				assert!(reading.read_frame().now_or_never().is_none());
			}
			let mut whole = b.create_group(group::Info { start: 0 }).unwrap();
			for (ms, frame) in [(0, b"a"), (1, b"b")] {
				whole.write_frame(ts(ms), frame.as_ref()).unwrap();
			}
			assert_eq!(read(&mut reading), Some(b"b".to_vec()));
		});
		let [start] = events("group read recovering", &logs).try_into().unwrap();
		assert!(start.contains("group=0 frame=1 "), "{start}");
		assert!(start.contains("cause=\"stalled behind a newer route\""), "{start}");
		assert!(start.contains("read_generation=1 serving_generation=2 "), "{start}");
		let [end] = events("group read recovered", &logs).try_into().unwrap();
		assert!(end.contains("read_generation=1 adopted_generation=2 "), "{end}");
		assert!(events("group read recovery failed", &logs).is_empty());

		// A failed copy whose replacement route then gives the group up.
		let logs = crate::test_tracing::capture(tracing::Level::DEBUG, || {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, Duration::from_secs(10));
			let mut open = a.create_group(group::Info { start: 0 }).unwrap();
			open.write_frame(ts(0), b"a".as_ref()).unwrap();
			let mut reading = recv(&mut sub);
			assert_eq!(read(&mut reading), Some(b"a".to_vec()));
			a.clone().abort(Error::Dropped).unwrap();

			let b = copy();
			routes.serve(b.consume());
			assert!(reading.read_frame().now_or_never().is_none());
			b.abort(Error::Stream(StreamError::App(5))).unwrap();
			assert!(reading.read_frame().now_or_never().unwrap().is_err());
		});
		let [start] = events("group read recovering", &logs).try_into().unwrap();
		assert!(start.contains(&format!("cause=\"{}\"", Error::Dropped)), "{start}");
		let [end] = events("group read recovery failed", &logs).try_into().unwrap();
		assert!(
			end.contains(&format!("err={} ", Error::Stream(StreamError::App(5)))),
			"{end}"
		);
		assert!(events("group read recovered", &logs).is_empty());

		// A stall its own copy answers ends that recovery, so the next stall starts another.
		let logs = crate::test_tracing::capture(tracing::Level::DEBUG, || {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, Duration::from_secs(10));
			let mut open = a.create_group(group::Info { start: 0 }).unwrap();
			open.write_frame(ts(0), b"a".as_ref()).unwrap();
			let mut reading = recv(&mut sub);
			assert_eq!(read(&mut reading), Some(b"a".to_vec()));

			let b = copy();
			routes.serve(b.consume());
			assert!(reading.read_frame().now_or_never().is_none());
			open.write_frame(ts(1), b"b".as_ref()).unwrap();
			assert_eq!(read(&mut reading), Some(b"b".to_vec()));
			assert!(reading.read_frame().now_or_never().is_none());
		});
		assert_eq!(events("group read recovering", &logs).len(), 2);
		assert!(events("group read recovered", &logs).is_empty());
	}

	/// A fetch the serving route already answered continues the group, even when that route
	/// aborts before the reader polls again. Asked of the route whose copy failed, the answer
	/// arrives only through the fetch, not that copy's subscription.
	#[test]
	#[cfg(feature = "async")]
	fn an_answered_fetch_outlives_the_serving_routes_abort() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		let dynamic = a.requests();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(10));

		let mut open = a.create_group(group::Info { start: 0 }).unwrap();
		open.write_frame(ts(0), b"a".as_ref()).unwrap();
		let mut reading = recv(&mut sub);
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));
		open.abort(Error::Dropped).unwrap();
		assert!(reading.read_frame().now_or_never().is_none());

		let request = dynamic.requested_fill_group().now_or_never().unwrap().unwrap();
		assert_eq!((request.key(), request.frame_start()), (0, 1));
		let mut rest = request.accept(None).unwrap().expect("a fresh group");
		rest.write_frame(ts(1), b"b".as_ref()).unwrap();
		rest.finish().unwrap();
		a.abort(Error::Stream(StreamError::App(5))).unwrap();

		assert_eq!(read(&mut reading), Some(b"b".to_vec()));
		assert_eq!(read(&mut reading), None);
	}

	/// A group's own failure, read from a copy that then lost its route, is no verdict on the
	/// group: it continues from the next route.
	#[test]
	#[cfg(feature = "async")]
	fn a_group_failure_from_a_failed_copy_continues_from_the_next_route() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(10));
		let mut open = a.create_group(group::Info { start: 0 }).unwrap();
		open.write_frame(ts(0), b"a".as_ref()).unwrap();
		let mut reading = recv(&mut sub);
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));
		open.abort(Error::App(5)).unwrap();
		a.abort(Error::Dropped).unwrap();
		assert!(reading.read_frame().now_or_never().is_none());

		let b = copy();
		let mut whole = b.create_group(group::Info { start: 0 }).unwrap();
		whole.write_frame(ts(0), b"a".as_ref()).unwrap();
		whole.write_frame(ts(1), b"b".as_ref()).unwrap();
		whole.finish().unwrap();
		routes.serve(b.consume());
		assert_eq!(read(&mut reading), Some(b"b".to_vec()));
		assert_eq!(read(&mut reading), None);
	}

	/// A group whose copy fails after the front let go of it, with no route left to come, ends
	/// with that failure.
	#[test]
	#[cfg(feature = "async")]
	fn a_failed_group_ends_once_no_route_will_serve_it() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(10));
		let mut open = a.create_group(group::Info { start: 0 }).unwrap();
		open.write_frame(ts(0), b"a".as_ref()).unwrap();
		let mut reading = recv(&mut sub);
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));
		routes.release();
		open.abort(Error::Dropped).unwrap();
		let res = reading.read_frame().now_or_never().map(|res| res.err());
		assert!(matches!(res, Some(Some(Error::Dropped))), "{res:?}");
	}

	/// A failed group's own route refusing the rest ends it with that refusal, unless the
	/// refusal is itself a delivery failure: then it waits for the next route.
	#[test]
	#[cfg(feature = "async")]
	fn a_refused_fetch_of_the_rest_ends_a_failed_group_only_with_a_verdict() {
		for verdict in [true, false] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			let dynamic = a.requests();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, Duration::from_secs(10));
			let mut open = a.create_group(group::Info { start: 0 }).unwrap();
			open.write_frame(ts(0), b"a".as_ref()).unwrap();
			let mut reading = recv(&mut sub);
			assert_eq!(read(&mut reading), Some(b"a".to_vec()));
			open.abort(Error::Dropped).unwrap();
			assert!(reading.read_frame().now_or_never().is_none());
			let request = dynamic.requested_fill_group().now_or_never().unwrap().unwrap();
			request.reject(if verdict { Error::NotFound } else { Error::Dropped });
			let res = reading.read_frame().now_or_never().map(|res| res.err());
			if verdict {
				assert!(matches!(res, Some(Some(Error::NotFound))), "{res:?}");
				continue;
			}
			assert!(res.is_none(), "{res:?}");
			assert!(reading.read_frame().now_or_never().is_none(), "still waiting");

			let b = copy();
			let mut whole = b.create_group(group::Info { start: 0 }).unwrap();
			whole.write_frame(ts(0), b"a".as_ref()).unwrap();
			whole.write_frame(ts(1), b"b".as_ref()).unwrap();
			whole.finish().unwrap();
			routes.serve(b.consume());
			assert_eq!(read(&mut reading), Some(b"b".to_vec()));
		}
	}

	/// A copy whose route became unroutable waits for another route rather than asking its own
	/// again, which could only replace the withdrawal's cause with a refusal. It keeps that
	/// cause once no route will come.
	#[test]
	#[cfg(feature = "async")]
	fn an_unroutable_group_waits_for_another_route() {
		for (fetched, concluded) in [(false, false), (true, false), (false, true), (true, true)] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			let dynamic = a.requests();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, Duration::from_secs(10));

			let mut open = a.create_group(group::Info { start: 0 }).unwrap();
			open.write_frame(ts(0), b"a".as_ref()).unwrap();
			let mut reading = if fetched {
				logical.consume().fetch_group(0, None).now_or_never().unwrap().unwrap()
			} else {
				recv(&mut sub)
			};
			assert_eq!(read(&mut reading), Some(b"a".to_vec()));
			open.abort(Error::Unroutable).unwrap();
			assert!(reading.read_frame().now_or_never().is_none(), "fetched={fetched}");

			// The route still serves the track but no longer holds the group: it would refuse.
			if let Some(request) = dynamic.requested_fill_group().now_or_never() {
				request.unwrap().reject(Error::NotFound);
			}
			let res = reading.read_frame().now_or_never();
			assert!(res.is_none(), "fetched={fetched}: {res:?}");

			if concluded {
				drop(routes);
				let res = reading.read_frame().now_or_never();
				assert!(
					matches!(
						res,
						Some(Err(Error::Unroutable | Error::Stream(StreamError::Unroutable)))
					),
					"fetched={fetched}: {res:?}"
				);
				continue;
			}

			// Another route holds it: the group continues there.
			let b = copy();
			let mut whole = b.create_group(group::Info { start: 0 }).unwrap();
			whole.write_frame(ts(0), b"a".as_ref()).unwrap();
			whole.write_frame(ts(1), b"b".as_ref()).unwrap();
			whole.finish().unwrap();
			routes.serve(b.consume());
			assert_eq!(read(&mut reading), Some(b"b".to_vec()), "fetched={fetched}");
			assert_eq!(read(&mut reading), None, "fetched={fetched}");
		}
	}

	/// A drop judges the serving route's subscription, not whether the group exists, so a
	/// fetched group it drops still waits on that route's fetch of the rest.
	#[test]
	#[cfg(feature = "async")]
	fn a_fetched_group_outlives_the_serving_routes_drop() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut open = a.create_group(group::Info { start: 0 }).unwrap();
		open.write_frame(ts(0), b"a".as_ref()).unwrap();
		let mut fetched = logical.consume().fetch_group(0, None).now_or_never().unwrap().unwrap();
		assert_eq!(
			fetched.read_frame().now_or_never().unwrap().unwrap().unwrap().payload,
			b"a".as_ref()
		);

		let b = copy();
		let dynamic = b.requests();
		routes.serve(b.consume());
		b.drop_groups(0..1, StreamError::Old);
		assert!(fetched.read_frame().now_or_never().is_none());
		let pending = dynamic.requested_fill_group().now_or_never().unwrap().unwrap();
		assert!(fetched.read_frame().now_or_never().is_none());
		let mut rest = pending.accept(None).unwrap().expect("a fresh group");
		rest.write_frame(ts(1), b"b".as_ref()).unwrap();
		rest.finish().unwrap();
		assert_eq!(read(&mut fetched), Some(b"b".to_vec()));
		assert_eq!(read(&mut fetched), None);
	}

	/// A failed group no front is left to move ends once the serving route refuses the rest
	/// and its subscription will not deliver the group either, even when that refusal is a
	/// delivery failure: no next route will come. It waits while the fetch is unanswered, or
	/// while the subscription may still deliver the group.
	#[test]
	#[cfg(feature = "async")]
	fn a_concluded_tracks_group_ends_once_the_serving_route_refuses_the_rest() {
		for dropped in [true, false] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let mut open = a.create_group(group::Info { start: 0 }).unwrap();
			open.write_frame(ts(0), b"a".as_ref()).unwrap();
			let mut fetched = logical.consume().fetch_group(0, None).now_or_never().unwrap().unwrap();
			assert_eq!(read(&mut fetched), Some(b"a".to_vec()));

			let b = copy();
			let dynamic = b.requests();
			routes.serve(b.consume());
			if dropped {
				b.drop_groups(0..1, StreamError::Old);
			}
			routes.conclude();
			open.abort(Error::Cancel).unwrap();
			assert!(
				fetched.read_frame().now_or_never().is_none(),
				"dropped={dropped}: the fetch of the rest is unanswered"
			);

			let request = dynamic.requested_fill_group().now_or_never().unwrap().unwrap();
			request.reject(Error::Cancel);
			let res = fetched.read_frame().now_or_never();
			match dropped {
				true => assert!(matches!(res, Some(Err(Error::Cancel))), "{res:?}"),
				false => assert!(res.is_none(), "b's subscription may still deliver it: {res:?}"),
			}
		}
	}

	/// A fetched group whose route dies after the front finished the track continues on the
	/// copy the front serves next, and fails, rather than waiting, once the front concluded.
	#[test]
	#[cfg(feature = "async")]
	fn a_finished_tracks_fetch_moves_off_a_dying_route() {
		for rerouted in [true, false] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			let dynamic = a.requests();
			routes.serve(a.consume());
			let mut open = a.create_group(group::Info { start: 0 }).unwrap();
			open.write_frame(ts(0), b"a".as_ref()).unwrap();
			let _a = a.finish().unwrap();
			let mut fetched = logical.consume().fetch_group(0, None).now_or_never().unwrap().unwrap();
			assert_eq!(read(&mut fetched), Some(b"a".to_vec()));
			routes.end(Ok(()));

			// The route dies under the open group, and fails the fetch of the rest too.
			open.abort(Error::Cancel).unwrap();
			assert!(fetched.read_frame().now_or_never().is_none(), "rerouted={rerouted}");
			let request = dynamic.requested_fill_group().now_or_never().unwrap().unwrap();
			request.reject(Error::Cancel);
			assert!(fetched.read_frame().now_or_never().is_none(), "rerouted={rerouted}");

			if !rerouted {
				drop(routes);
				let res = fetched.read_frame().now_or_never();
				assert!(matches!(res, Some(Err(Error::Cancel))), "{res:?}");
				continue;
			}
			let b = copy();
			let mut whole = b.create_group(group::Info { start: 0 }).unwrap();
			whole.write_frame(ts(0), b"a".as_ref()).unwrap();
			whole.write_frame(ts(1), b"b".as_ref()).unwrap();
			whole.finish().unwrap();
			routes.serve(b.consume());
			assert_eq!(read(&mut fetched), Some(b"b".to_vec()));
			assert_eq!(read(&mut fetched), None);
		}
	}

	/// A fetch of a finished track goes to the copy it ended on, which caches what the
	/// copy rerouted after the end does not, and to the rerouted copy only once that one
	/// failed.
	#[test]
	#[cfg(feature = "async")]
	fn a_finished_tracks_fetch_goes_to_its_last_copy_first() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		let dynamic = a.requests();
		routes.serve(a.consume());
		let mut group = a.create_group(group::Info { start: 0 }).unwrap();
		group.write_frame(ts(0), b"a".as_ref()).unwrap();
		group.finish().unwrap();
		// Groups 1 and 2 are missing below the end: a fetch asks for them.
		a.create_group(group::Info { start: 3 }).unwrap().finish().unwrap();
		let last = a.consume();
		let a = a.finish().unwrap();
		routes.end(Ok(()));

		let b = copy();
		let rerouted = b.requests();
		routes.serve(b.consume());
		assert!(routes.consume().serving().is_some_and(|copy| copy.same_channel(&last)));
		let mut fetched = logical.consume().fetch_group(0, None).now_or_never().unwrap().unwrap();
		assert_eq!(read(&mut fetched), Some(b"a".to_vec()));
		assert!(
			rerouted.requested_fill_group().now_or_never().is_none(),
			"the rerouted copy is not asked"
		);

		// The last copy failing a fetch, or failing outright, leaves it to the rerouted one,
		// even once no front is left to replace that.
		routes.conclude();
		let mut fetch = logical.consume().fetch_group(1, None);
		assert!((&mut fetch).now_or_never().is_none());
		let request = dynamic.requested_fill_group().now_or_never().unwrap().unwrap();
		request.reject(Error::Cancel);
		assert!((&mut fetch).now_or_never().is_none());
		assert!(
			rerouted
				.requested_fill_group()
				.now_or_never()
				.is_some_and(|fill| fill.is_ok())
		);
		a.abort(Error::Cancel).unwrap();
		let mut fetch = logical.consume().fetch_group(2, None);
		assert!((&mut fetch).now_or_never().is_none());
		assert!(
			rerouted
				.requested_fill_group()
				.now_or_never()
				.is_some_and(|fill| fill.is_ok())
		);
	}

	/// A finished track answers from the copy it ended on until that copy loses its route,
	/// and from the copy the front serves after the end from then on.
	#[test]
	fn a_finished_track_answers_from_its_last_copy_until_it_fails() {
		let routes = Producer::new();
		let a = copy();
		routes.serve(a.consume());
		routes.end(Ok(()));
		let b = copy();
		routes.serve(b.consume());
		let consumer = routes.consume();
		assert!(consumer.serving().is_some_and(|copy| copy.same_channel(&a.consume())));
		a.abort(Error::Cancel).unwrap();
		assert!(consumer.serving().is_some_and(|copy| copy.same_channel(&b.consume())));
	}

	/// A reader asked about the track's cache answers for the serving copy, which a replaced
	/// copy's groups and skips do not change; the run of skips just below a group counts
	/// from any copy.
	#[test]
	fn a_reader_answers_cache_queries_for_the_serving_copy() {
		let routes = Producer::new();
		let a = copy();
		write(&a, 1);
		write(&a, 2);
		a.skip_groups(3..5);
		routes.serve(a.consume());
		let sub = routes.consume().subscribe(Subscription::default().into());
		let b = copy();
		write(&b, 4);
		write(&b, 5);
		b.skip_groups(6..8);
		b.report_largest(Position::group(5), true);
		routes.serve(b.consume());
		assert_eq!(sub.reader().copies.len(), 2, "the read follows the route to b");

		assert_eq!(sub.skipped_below(5), 2, "the replaced copy knows 3 and 4 were skipped");
		assert!(sub.is_skipped(std::iter::once(6..=7)));
		assert!(
			!sub.is_skipped(std::iter::once(3..=4)),
			"only the replaced copy skipped 3 and 4"
		);
		assert!(sub.largest_finished(Position::group(5)));
		assert!(!sub.largest_finished(Position::group(4)));
		let consumer = routes.consume();
		assert!(consumer.largest_finished(Position::group(5)));
		assert!(!consumer.largest_finished(Position::group(4)));
	}

	/// A copy the front serves after the clean end only continues group reads and fetches: a
	/// reader still draining the copy the track ended on stays there, rather than losing what
	/// only that copy holds (`ResumeReader.tla`'s `CleanEndIsComplete`).
	#[test]
	fn a_reader_stays_on_the_copy_its_track_finished_on() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		write(&a, 2);
		let _a = a.finish().unwrap();
		routes.end(Ok(()));

		let b = copy();
		write(&b, 0);
		routes.serve(b.consume());
		assert_eq!(drain(&mut sub, false), [2]);
		let end = sub.poll_recv_group(&kio::Waiter::noop());
		assert!(matches!(end, Poll::Ready(Ok(None))), "the track ends cleanly");
	}

	/// A session's copy whose info is still in flight, and its fetch handler.
	fn pending_copy() -> (track::Request, track::Requests) {
		let request = track::Request::new(Arc::new(broadcast::Info::default()), "video");
		let dynamic = request.requests();
		(request, dynamic)
	}

	/// The logical track `routes` serves, before any copy is spliced.
	fn unaccepted(routes: &Producer) -> track::Request {
		track::Request::new(Arc::new(broadcast::Info::default()), "video").routes(routes.consume())
	}

	/// A fetch reaches the copy whose info is in flight at once, but its group is handed out
	/// only once the front splices that copy.
	#[test]
	#[cfg(feature = "async")]
	fn a_fetch_goes_to_a_staged_copy_before_its_info() {
		let routes = Producer::new();
		let logical = unaccepted(&routes);
		let (request, dynamic) = pending_copy();
		let staged = routes.stage(request.consume());
		let mut fetching = logical.consume().fetch_group(0, None);
		assert!((&mut fetching).now_or_never().is_none());
		let fill = dynamic
			.requested_fill()
			.now_or_never()
			.expect("the fetch reached the staged copy")
			.unwrap();

		let copy = request.accept(crate::track::Info::timed());
		let mut group = fill
			.accept(0, crate::track::Info::timed())
			.unwrap()
			.expect("a fresh group");
		group.write_frame(ts(0), b"early".as_ref()).unwrap();
		group.finish().unwrap();
		assert!(
			(&mut fetching).now_or_never().is_none(),
			"a group is never handed out before its copy's info passes"
		);

		let _logical = logical.accept(crate::track::Info::timed());
		routes.splice(copy.consume(), staged);
		let mut fetched = fetching.now_or_never().expect("spliced").unwrap();
		assert_eq!(read(&mut fetched), Some(b"early".to_vec()));
	}

	/// A staged copy withdrawn before it is spliced (its info refused) answers nothing: the
	/// fetch asks the next route.
	#[test]
	#[cfg(feature = "async")]
	fn a_withdrawn_stage_refetches_from_the_next_route() {
		let routes = Producer::new();
		let logical = unaccepted(&routes);
		let (refused, refused_dynamic) = pending_copy();
		let staged = routes.stage(refused.consume());
		let mut fetching = logical.consume().fetch_group(0, None);
		assert!((&mut fetching).now_or_never().is_none());
		let fill = refused_dynamic.requested_fill().now_or_never().unwrap().unwrap();
		let wrong = refused.accept(crate::track::Info::timed());
		let mut group = fill
			.accept(0, crate::track::Info::timed())
			.unwrap()
			.expect("a fresh group");
		group.write_frame(ts(0), b"wrong".as_ref()).unwrap();
		group.finish().unwrap();
		drop(staged);
		drop(wrong);

		let (request, dynamic) = pending_copy();
		let staged = routes.stage(request.consume());
		assert!((&mut fetching).now_or_never().is_none());
		let fill = dynamic
			.requested_fill()
			.now_or_never()
			.expect("asked again of the next route")
			.unwrap();
		let copy = request.accept(crate::track::Info::timed());
		let mut group = fill
			.accept(0, crate::track::Info::timed())
			.unwrap()
			.expect("a fresh group");
		group.write_frame(ts(0), b"right".as_ref()).unwrap();
		group.finish().unwrap();
		let _logical = logical.accept(crate::track::Info::timed());
		routes.splice(copy.consume(), staged);
		let mut fetched = fetching.now_or_never().expect("spliced").unwrap();
		assert_eq!(read(&mut fetched), Some(b"right".to_vec()));
	}

	/// A stage withdrawn with none after it takes no fetch, and withdrawing or splicing an
	/// older stage leaves a newer one in place for the next fetch.
	#[test]
	#[cfg(feature = "async")]
	fn a_fetch_goes_only_to_a_stage_still_in_place() {
		let routes = Producer::new();
		let logical = unaccepted(&routes);
		let (withdrawn, dynamic) = pending_copy();
		drop(routes.stage(withdrawn.consume()));
		let mut fetching = logical.consume().fetch_group(0, None);
		assert!((&mut fetching).now_or_never().is_none());
		assert!(
			dynamic.requested_fill().now_or_never().is_none(),
			"the withdrawn copy is not asked"
		);

		for splice in [false, true] {
			let routes = Producer::new();
			let logical = unaccepted(&routes);
			let (older, _older) = pending_copy();
			let staged = routes.stage(older.consume());
			let (newer, dynamic) = pending_copy();
			let _newer = routes.stage(newer.consume());
			match splice {
				false => drop(staged),
				true => {
					let older = older.accept(crate::track::Info::timed());
					routes.splice(older.consume(), staged);
					routes.park();
				}
			}
			let mut fetching = logical.consume().fetch_group(0, None);
			assert!((&mut fetching).now_or_never().is_none());
			assert!(
				dynamic.requested_fill().now_or_never().is_some(),
				"splice={splice}: the newer stage is asked"
			);
		}
	}

	/// A logical track that ends before any copy is spliced fails a fetch waiting on it with
	/// the front's error.
	#[test]
	fn an_unserved_track_fails_its_fetch_with_the_fronts_error() {
		let routes = Producer::new();
		let logical = unaccepted(&routes);
		let (request, _dynamic) = pending_copy();
		let staged = routes.stage(request.consume());
		let mut fetching = logical.consume().fetch_group(0, None);
		assert!((&mut fetching).now_or_never().is_none());
		drop(staged);
		routes.end(Err(Error::Unauthorized));
		assert!(matches!(fetching.now_or_never(), Some(Err(Error::Unauthorized))));
	}

	/// Once the front concludes with no copy served, a fetch still at a staged copy ends: with
	/// that copy's refusal if it refused, else as not found.
	#[test]
	#[cfg(feature = "async")]
	fn a_concluded_track_ends_a_fetch_at_its_stage() {
		for refused in [true, false] {
			let routes = Producer::new();
			let logical = unaccepted(&routes);
			let (request, dynamic) = pending_copy();
			let _staged = routes.stage(request.consume());
			let mut fetching = logical.consume().fetch_group(0, None);
			let waiter = kio::Waiter::noop();
			assert!(kio::Task::poll(&mut *fetching, &waiter).is_pending());
			let fill = dynamic.requested_fill().now_or_never().unwrap().unwrap();
			if refused {
				fill.refuse_groups(0..1, Error::Unauthorized);
				assert!(
					kio::Task::poll(&mut *fetching, &waiter).is_pending(),
					"judged only once no copy may be spliced"
				);
			}
			drop(routes);
			let res = kio::Task::poll(&mut *fetching, &waiter).map(|res| res.err());
			let expected = if refused { Error::Unauthorized } else { Error::NotFound };
			assert!(
				matches!(&res, Poll::Ready(Some(err)) if err.to_string() == expected.to_string()),
				"refused={refused}: {res:?}"
			);
		}
	}

	/// What a staged copy answered is handed out only if that copy is spliced: once another
	/// copy serves instead, the fetch asks that one.
	#[test]
	#[cfg(feature = "async")]
	fn a_fetch_asks_the_serving_copy_over_a_stage_never_spliced() {
		let routes = Producer::new();
		let logical = unaccepted(&routes);
		let (request, dynamic) = pending_copy();
		let _staged = routes.stage(request.consume());
		let mut fetching = logical.consume().fetch_group(0, None);
		assert!((&mut fetching).now_or_never().is_none());
		let fill = dynamic.requested_fill().now_or_never().unwrap().unwrap();
		let _staged_copy = request.accept(crate::track::Info::timed());
		let mut group = fill
			.accept(0, crate::track::Info::timed())
			.unwrap()
			.expect("a fresh group");
		group.write_frame(ts(0), b"x".as_ref()).unwrap();
		group.finish().unwrap();
		assert!((&mut fetching).now_or_never().is_none());

		let _logical = logical.accept(crate::track::Info::timed());
		let b = copy();
		let b_dynamic = b.requests();
		routes.serve(b.consume());
		assert!(
			(&mut fetching).now_or_never().is_none(),
			"the stage's answer is not handed out"
		);
		let fill = b_dynamic
			.requested_fill()
			.now_or_never()
			.expect("asked of the serving copy")
			.unwrap();
		let mut group = fill
			.accept(0, crate::track::Info::timed())
			.unwrap()
			.expect("a fresh group");
		group.write_frame(ts(0), b"x".as_ref()).unwrap();
		group.finish().unwrap();
		let mut fetched = fetching.now_or_never().expect("answered").unwrap();
		assert_eq!(read(&mut fetched), Some(b"x".to_vec()));
	}

	/// A fetch the serving copy fails to deliver, while its route still serves, waits for the
	/// next route rather than ending with that failure.
	#[test]
	#[cfg(feature = "async")]
	fn a_fetch_the_serving_copy_fails_to_deliver_asks_the_next_route() {
		let routes = Producer::new();
		let a = copy();
		let dynamic = a.requests();
		routes.serve(a.consume());
		let logical = logical(&routes);
		let mut fetching = logical.consume().fetch_group(0, None);
		assert!((&mut fetching).now_or_never().is_none());
		let fill = dynamic.requested_fill().now_or_never().unwrap().unwrap();
		fill.refuse_groups(0..1, Error::Dropped);
		assert!((&mut fetching).now_or_never().is_none(), "the next route is asked");

		let b = copy();
		write(&b, 0);
		routes.serve(b.consume());
		let mut fetched = fetching.now_or_never().expect("answered").unwrap();
		assert_eq!(read(&mut fetched), Some(b"x".to_vec()));
	}

	/// Every run the reader gives up right now.
	fn dropped(sub: &mut track::Subscriber) -> Vec<(std::ops::Range<u64>, StreamError)> {
		let mut runs = Vec::new();
		while let Poll::Ready((run, hole)) = sub.poll_drop(&kio::Waiter::noop()) {
			runs.push((run, hole.error()));
		}
		runs
	}

	/// The serving copy's drop is reported once, while a route failure is no verdict:
	/// another route may still deliver the group.
	#[test]
	fn a_resumed_reader_reports_what_its_serving_copy_gives_up() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		write(&a, 0);
		assert_eq!(drain(&mut sub, false), [0]);
		assert_eq!(dropped(&mut sub), []);

		a.drop_groups(1..2, StreamError::Internal);
		assert_eq!(dropped(&mut sub), []);
		a.drop_groups(2..4, StreamError::NotFound);
		assert_eq!(dropped(&mut sub), [(2..4, StreamError::NotFound)]);
		assert_eq!(dropped(&mut sub), []);
		write(&a, 3);
		write(&a, 4);
		assert_eq!(drain(&mut sub, false), [3, 4]);
		assert_eq!(dropped(&mut sub), []);
	}

	/// A skip of a run the serving copy already dropped replaces the drop, so the resumed
	/// reader reports it again, as a skip, and only once.
	#[kio::test]
	async fn a_resumed_reader_reports_a_skip_over_a_reported_drop() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		write(&a, 0);
		assert_eq!(drain(&mut sub, false), [0]);
		let holes = |sub: &mut track::Subscriber| {
			let mut runs = Vec::new();
			while let Poll::Ready(run) = sub.poll_drop(&kio::Waiter::noop()) {
				runs.push(run);
			}
			runs
		};

		a.drop_groups(1..3, StreamError::NotFound);
		assert_eq!(holes(&mut sub), [(1..3, track::Hole::drop(StreamError::NotFound))]);
		a.skip_groups(1..3);
		assert_eq!(holes(&mut sub), [(1..3, track::Hole::Skip)]);
		assert_eq!(holes(&mut sub), []);
		write(&a, 3);
		assert_eq!(drain(&mut sub, false), [3]);
	}

	/// The boundary group the serving copy gave up waits while a copy it replaced may still
	/// deliver it, and goes out once that copy is past the reader's budget, a budget after
	/// the serving copy holds a newer group.
	#[kio::test]
	async fn a_drop_waits_while_a_replaced_copy_may_still_deliver_it() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(1));
		write(&a, 0);
		assert_eq!(drain(&mut sub, false), [0]);
		assert_eq!(dropped(&mut sub), []);
		// Group 1 is the boundary, still arriving on the old route.
		let mut open = a.create_group(group::Info { start: 1 }).unwrap();
		open.write_frame(ts(1), b"x".as_ref()).unwrap();

		let b = copy();
		routes.serve(b.consume());
		// Only the new route gave group 1 up; the old one still holds it.
		b.drop_groups(1..2, StreamError::NotFound);
		assert_eq!(dropped(&mut sub), []);
		// The boundary group is the latest, so nothing is past the budget until a newer one.
		kio::clock::sleep(Duration::from_secs(1)).await;
		assert_eq!(dropped(&mut sub), []);
		write(&b, 2);
		assert_eq!(dropped(&mut sub), []);

		kio::clock::sleep(Duration::from_secs(1)).await;
		assert_eq!(dropped(&mut sub), [(1..2, StreamError::NotFound)]);
		assert_eq!(drain(&mut sub, false), [2]);
	}

	/// A reader takes the boundary group from the copy holding it, whatever the serving copy
	/// says of it.
	#[test]
	fn a_reader_takes_the_boundary_group_the_serving_copy_dropped() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		write(&a, 0);
		assert_eq!(drain(&mut sub, false), [0]);
		let mut open = a.create_group(group::Info { start: 1 }).unwrap();
		open.write_frame(ts(1), b"x".as_ref()).unwrap();

		let b = copy();
		routes.serve(b.consume());
		b.drop_groups(1..2, StreamError::NotFound);
		assert_eq!(drain(&mut sub, false), [1]);
		assert_eq!(dropped(&mut sub), []);
	}

	/// A received stale run the reader held back is unknown again once a new upstream
	/// subscription supersedes it on the serving copy, which may deliver it, so it is not
	/// reported meanwhile.
	#[kio::test]
	async fn a_held_back_stale_run_new_terms_supersede_is_not_reported() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let budget = Duration::from_secs(1);
		let mut sub = subscribe(&logical, budget);
		write(&a, 0);
		assert_eq!(drain(&mut sub, false), [0]);
		let mut open = a.create_group(group::Info { start: 1 }).unwrap();
		open.write_frame(ts(1), b"x".as_ref()).unwrap();

		let b = copy();
		routes.serve(b.consume());
		// The new route's upstream judged the boundary group stale; the old route still holds it.
		b.give_up(1..2, track::Hole::stale(budget));
		assert_eq!(dropped(&mut sub), []);
		write(&b, 2);
		assert_eq!(dropped(&mut sub), []);

		// The route resubscribes before the old copy is past the budget.
		b.supersede_stale();
		kio::clock::sleep(budget).await;
		assert_eq!(dropped(&mut sub), [], "no verdict under the new subscription");
	}

	/// A run the serving copy gave up as stale itself waits like any drop while a replaced copy
	/// may still deliver it, then goes out: no new upstream subscription supersedes it.
	#[kio::test]
	async fn a_held_back_stale_run_judged_here_is_reported() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let budget = Duration::from_secs(1);
		let mut sub = subscribe(&logical, budget);
		write(&a, 0);
		assert_eq!(drain(&mut sub, false), [0]);
		let mut open = a.create_group(group::Info { start: 1 }).unwrap();
		open.write_frame(ts(1), b"x".as_ref()).unwrap();

		let b = copy();
		routes.serve(b.consume());
		b.give_up(1..2, track::Hole::stale(budget));
		write(&b, 2);
		assert_eq!(dropped(&mut sub), []);
		kio::clock::sleep(budget / 2).await;
		assert_eq!(dropped(&mut sub), [], "the old route may still deliver it");
		kio::clock::sleep(budget / 2).await;
		assert_eq!(dropped(&mut sub), [(1..2, StreamError::Old)]);
	}

	/// A run a replaced route held back is dropped with that route: the route serving next
	/// may still deliver it.
	#[test]
	fn a_drop_a_replaced_route_held_back_is_no_verdict_on_the_next() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		write(&a, 0);
		assert_eq!(drain(&mut sub, false), [0]);
		let mut open = a.create_group(group::Info { start: 1 }).unwrap();
		open.write_frame(ts(1), b"x".as_ref()).unwrap();

		let b = copy();
		routes.serve(b.consume());
		// Held back: the copy b replaced may still deliver group 1.
		b.drop_groups(1..2, StreamError::NotFound);
		assert_eq!(dropped(&mut sub), []);

		let c = copy();
		routes.serve(c.consume());
		b.abort(Error::Stream(StreamError::Session(SessionError::Cancel)))
			.unwrap();
		assert_eq!(dropped(&mut sub), []);
		assert_eq!(drain(&mut sub, false), [1]);
		write(&c, 2);
		assert_eq!(drain(&mut sub, false), [2]);
		assert_eq!(dropped(&mut sub), []);
	}

	/// A reader floored at the last sequence holds back nothing for a replaced copy that may
	/// still deliver, since nothing it may deliver is in range.
	#[test]
	fn a_reader_floored_at_the_last_sequence_reports_no_drop() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());
		let b = copy();
		routes.serve(b.consume());
		let mut sub = sub.with_range(u64::MAX..);
		assert_eq!(dropped(&mut sub), []);
	}

	/// Read a fetched group from frame 0, which must already be cached.
	#[cfg(feature = "async")]
	fn read_cached(track: &track::Consumer, key: u64) -> Vec<Vec<u8>> {
		let mut group = track
			.fetch_group(key, None)
			.now_or_never()
			.expect("the whole group is cached")
			.unwrap();
		std::iter::from_fn(|| read(&mut group)).collect()
	}

	/// The replacement adopts the head the failed route received, so its cache holds the
	/// whole group, as on a relay that never saw the route change.
	#[test]
	#[cfg(feature = "async")]
	fn a_failed_route_hands_its_head_to_the_replacement() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(10));

		let mut open = a.create_group(group::Info { start: 0 }).unwrap();
		open.write_frame(ts(0), b"a".as_ref()).unwrap();
		open.write_frame(ts(1), b"b".as_ref()).unwrap();
		let mut reading = recv(&mut sub);
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));
		let mut plain = open.consume();
		open.abort(Error::Dropped).unwrap();
		assert!(
			matches!(plain.read_frame().now_or_never(), Some(Err(Error::Dropped))),
			"a plain consumer reads the abort, not the head"
		);

		let b = copy();
		let dynamic = b.requests();
		routes.serve(b.consume());
		assert_eq!(read(&mut reading), Some(b"b".to_vec()));
		assert!(reading.read_frame().now_or_never().is_none());
		let request = dynamic.requested_fill_group().now_or_never().unwrap().unwrap();
		assert_eq!((request.key(), request.frame_start()), (0, 2));
		let mut rest = request.accept(None).unwrap().expect("a fresh group");
		rest.write_frame(ts(2), b"c".as_ref()).unwrap();
		rest.finish().unwrap();
		assert_eq!(read(&mut reading), Some(b"c".to_vec()));
		assert_eq!(read(&mut reading), None);

		let whole = [b"a".to_vec(), b"b".to_vec(), b"c".to_vec()];
		assert_eq!(read_cached(&b.consume(), 0), whole);
		assert_eq!(read_cached(&logical.consume(), 0), whole);
		assert!(
			dynamic.requested_fill_group().now_or_never().is_none(),
			"no second fetch"
		);
	}

	/// A replaced route that only stalled lends the replacement its head the same way.
	#[test]
	#[cfg(feature = "async")]
	fn a_stalled_route_lends_its_head_to_the_replacement() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(10));

		let mut open = a.create_group(group::Info { start: 0 }).unwrap();
		open.write_frame(ts(0), b"a".as_ref()).unwrap();
		let mut reading = recv(&mut sub);
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));

		let b = copy();
		let dynamic = b.requests();
		routes.serve(b.consume());
		assert!(reading.read_frame().now_or_never().is_none());
		let request = dynamic.requested_fill_group().now_or_never().unwrap().unwrap();
		assert_eq!(request.frame_start(), 1);
		let mut rest = request.accept(None).unwrap().expect("a fresh group");
		rest.write_frame(ts(1), b"b".as_ref()).unwrap();
		rest.finish().unwrap();
		assert_eq!(read(&mut reading), Some(b"b".to_vec()));
		assert_eq!(read(&mut reading), None);

		assert_eq!(read_cached(&b.consume(), 0), [b"a".to_vec(), b"b".to_vec()]);
	}

	/// Cache policy dropping a group frees its frames: nothing is kept for a successor.
	#[test]
	#[cfg(feature = "async")]
	fn an_evicted_group_hands_nothing_to_the_replacement() {
		let routes = Producer::new();
		let a = copy();
		routes.serve(a.consume());
		let mut open = a.create_group(group::Info { start: 0 }).unwrap();
		open.write_frame(ts(0), b"a".as_ref()).unwrap();
		let mut fetched = logical(&routes)
			.consume()
			.fetch_group(0, None)
			.now_or_never()
			.unwrap()
			.unwrap();
		assert_eq!(read(&mut fetched), Some(b"a".to_vec()));
		open.abort(Error::Evicted).unwrap();

		let b = copy();
		let dynamic = b.requests();
		routes.serve(b.consume());
		assert!(fetched.read_frame().now_or_never().is_none());
		let request = dynamic.requested_fill_group().now_or_never().unwrap().unwrap();
		assert_eq!(request.frame_start(), 1);
		let mut rest = request.accept(None).unwrap().expect("a fresh group");
		rest.write_frame(ts(1), b"b".as_ref()).unwrap();
		rest.finish().unwrap();
		assert_eq!(read(&mut fetched), Some(b"b".to_vec()));
		assert_eq!(read(&mut fetched), None);
		let _whole = b.consume().fetch_group(0, None);
		assert!(
			dynamic.requested_fill_group().now_or_never().is_some(),
			"the head was not kept"
		);
	}

	/// A fetched reader whose copy shed the frame it reads next gets `Evicted`, rather than
	/// recovering onto the same copy again.
	#[test]
	#[cfg(feature = "async")]
	fn a_fetched_reader_past_a_shed_front_gets_evicted() {
		let routes = Producer::new();
		let a = copy();
		routes.serve(a.consume());
		let mut open = a.create_group(group::Info { start: 0 }).unwrap();
		open.write_frame(ts(0), b"a".as_ref()).unwrap();
		let mut fetched = logical(&routes)
			.consume()
			.fetch_group(0, None)
			.now_or_never()
			.unwrap()
			.unwrap();
		assert_eq!(read(&mut fetched), Some(b"a".to_vec()));
		for _ in 0..40 {
			open.write_frame(ts(0), vec![0u8; 1 << 20]).unwrap();
		}
		let res = fetched.read_frame().now_or_never().expect("not parked");
		assert!(matches!(res, Err(Error::Evicted)), "{res:?}");
	}

	/// The same holds for a plain reader of a compressed group whose inflated copy shed the
	/// frame while the stored slice stays.
	#[test]
	#[cfg(feature = "async")]
	fn a_fetched_plain_reader_past_a_shed_inflated_front_gets_evicted() {
		let info = crate::track::Info::timed().with_compression(crate::track::Compression::Deflate);
		let routes = Producer::new();
		let a = track::Producer::new(Arc::new(broadcast::Info::default()), "video", info.clone());
		routes.serve(a.consume());
		let mut open = a.create_group(group::Info { start: 0 }).unwrap();
		let mut drain = open.consume();
		// Two frames that compress to almost nothing, but inflate past the tail together.
		for _ in 0..2 {
			open.write_frame(ts(0), vec![0u8; 31 << 20]).unwrap();
			assert!(read(&mut drain).is_some());
		}
		let logical = track::Request::new(Arc::new(broadcast::Info::default()), "video")
			.routes(routes.consume())
			.accept(info);
		let mut fetched = logical.consume().fetch_group(0, None).now_or_never().unwrap().unwrap();
		let res = fetched.read_frame().now_or_never().expect("not parked");
		assert!(matches!(res, Err(Error::Evicted)), "{res:?}");
	}

	/// A group first read while only one copy existed is still backed by a later copy that
	/// holds it, so a failure continues from that copy instead of waiting out the budget.
	#[test]
	#[cfg(feature = "async")]
	fn a_group_read_from_a_single_route_keeps_its_backup() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(10));

		let mut open = a.create_group(group::Info { start: 0 }).unwrap();
		open.write_frame(ts(0), b"a".as_ref()).unwrap();
		let mut reading = recv(&mut sub);
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));

		// B holds the whole group, then its route fails, keeping it.
		let b = copy();
		let mut whole = b.create_group(group::Info { start: 0 }).unwrap();
		whole.write_frame(ts(0), b"a".as_ref()).unwrap();
		whole.write_frame(ts(1), b"b".as_ref()).unwrap();
		whole.finish_at(2).unwrap();
		routes.serve(b.consume());
		assert!(
			sub.poll_recv_group(&kio::Waiter::noop()).is_pending(),
			"B replays only group 0"
		);
		b.abort(Error::Stream(StreamError::Session(SessionError::Cancel)))
			.unwrap();

		// C lacks the group; reading later ones from it lets go of what nothing needs, however
		// many it hands out meanwhile.
		let c = copy();
		for key in 1..9 {
			write(&c, key);
		}
		routes.serve(c.consume());
		assert_eq!(drain(&mut sub, false), (1..9).collect::<Vec<_>>());

		open.abort(Error::Stream(StreamError::Session(SessionError::Cancel)))
			.unwrap();
		a.abort(Error::Stream(StreamError::Session(SessionError::Cancel)))
			.unwrap();
		assert_eq!(read(&mut reading), Some(b"b".to_vec()));
		assert_eq!(read(&mut reading), None);
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_group_both_routes_deliver_is_handed_out_once() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(10));
		let b = copy();
		for (track, keys) in [(&a, 0..2), (&b, 0..3)] {
			for key in keys {
				let mut group = track.create_group(group::Info { start: key }).unwrap();
				group.write_frame(ts(key), b"x".as_ref()).unwrap();
				group.finish().unwrap();
			}
		}

		assert_eq!(recv(&mut sub).start, 0);
		assert_eq!(recv(&mut sub).start, 1);
		routes.serve(b.consume());
		assert_eq!(recv(&mut sub).start, 2);
		assert!(sub.recv_group().now_or_never().is_none());
	}

	/// The largest follows the serving copy across a failover without moving back,
	/// and ends only with the route.
	#[test]
	#[cfg(feature = "async")]
	fn largest_follows_the_serving_copy() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		write(&a, 3);
		assert_eq!(sub.largest().now_or_never(), Some(Some(Position::group(3))));

		// The route fails: its copy's stream ends, the logical track's does not.
		a.clone().abort(Error::Dropped).unwrap();
		assert!(sub.largest().now_or_never().is_none());

		// The replacement starts behind what is already known.
		let b = copy();
		write(&b, 2);
		routes.serve(b.consume());
		assert!(sub.largest().now_or_never().is_none());
		write(&b, 4);
		assert_eq!(sub.largest().now_or_never(), Some(Some(Position::group(4))));

		routes.end(Ok(()));
		drop(b);
		assert_eq!(sub.largest().now_or_never(), Some(None));
	}

	/// A front's reader reports the serving copy's largest only within its cap.
	#[test]
	#[cfg(feature = "async")]
	fn largest_honours_the_cap_on_the_serving_copy() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		for key in 0..5 {
			write(&a, key);
		}
		sub.set_range(..3);
		assert!(sub.largest().now_or_never().is_none(), "the largest is past the cap");
		sub.set_range(..);
		assert_eq!(sub.largest().now_or_never(), Some(Some(Position::group(4))));
	}

	/// A front's reader whose start time is past the serving copy's edge starts at the group
	/// containing it, so it reports that copy's largest, as a plain reader of the copy does.
	#[test]
	#[cfg(feature = "async")]
	fn largest_honours_the_subscription_start_on_the_serving_copy() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_range(8..);
		let mut sub = logical
			.consume()
			.subscribe(subscription.clone())
			.now_or_never()
			.unwrap()
			.unwrap();
		for key in 0..5 {
			write(&a, key);
		}
		let mut plain = a.consume().subscribe(subscription).now_or_never().unwrap().unwrap();
		assert_eq!(plain.largest().now_or_never(), Some(Some(Position::group(4))));
		assert_eq!(sub.largest().now_or_never(), Some(Some(Position::group(4))));
		write(&a, 8);
		assert_eq!(sub.largest().now_or_never(), Some(Some(Position::group(8))));
	}

	/// A track reader waiting before any route serves is woken by the first one's frame.
	/// The front ending the track ends the stream, though the copy's producer stays open.
	#[test]
	fn largest_ends_with_the_route() {
		let routes = Producer::new();
		let mut consumer = logical(&routes).consume();
		let a = copy();
		routes.serve(a.consume());
		write(&a, 5);
		let noop = kio::Waiter::noop();
		assert_eq!(consumer.poll_largest(&noop), Poll::Ready(Some(Position::group(5))));
		assert!(consumer.poll_largest(&noop).is_pending(), "the copy is still open");

		routes.end(Ok(()));
		assert_eq!(consumer.poll_largest(&noop), Poll::Ready(None));
		drop(a);
	}

	/// The track holds a group once its serving copy does, whichever copy that is.
	#[test]
	fn grouped_waits_for_the_serving_copy_to_hold_a_group() {
		let routes = Producer::new();
		let consumer = routes.consume();
		let noop = kio::Waiter::noop();
		assert!(consumer.poll_grouped(&noop).is_pending(), "no copy serves");
		let a = copy();
		routes.serve(a.consume());
		assert!(consumer.poll_grouped(&noop).is_pending(), "the copy holds no group");
		write(&a, 0);
		assert!(consumer.poll_grouped(&noop).is_ready());
	}

	/// Once the front concluded, the largest follows the last copy and ends with it, or at
	/// once when no copy is left.
	#[test]
	fn largest_ends_with_the_last_copy_once_the_front_concluded() {
		let routes = Producer::new();
		let mut consumer = logical(&routes).consume();
		let a = copy();
		routes.serve(a.consume());
		write(&a, 5);
		let noop = kio::Waiter::noop();
		assert_eq!(consumer.poll_largest(&noop), Poll::Ready(Some(Position::group(5))));
		routes.conclude();
		assert!(consumer.poll_largest(&noop).is_pending(), "the last copy is still open");
		drop(a);
		assert_eq!(consumer.poll_largest(&noop), Poll::Ready(None));

		let routes = Producer::new();
		let mut consumer = logical(&routes).consume();
		drop(routes);
		assert_eq!(consumer.poll_largest(&noop), Poll::Ready(None));
	}

	#[test]
	fn largest_waits_for_a_serving_copy() {
		use std::sync::atomic::{AtomicBool, Ordering};

		struct Flag(AtomicBool);
		impl futures::task::ArcWake for Flag {
			fn wake_by_ref(flag: &Arc<Self>) {
				flag.0.store(true, Ordering::SeqCst);
			}
		}

		let routes = Producer::new();
		let mut consumer = logical(&routes).consume();
		let flag = Arc::new(Flag(AtomicBool::new(false)));
		let waiter = kio::Waiter::new(futures::task::waker(flag.clone()));
		assert!(consumer.poll_largest(&waiter).is_pending());

		let a = copy();
		routes.serve(a.consume());
		assert!(flag.0.load(Ordering::SeqCst), "the serving copy woke the reader");
		assert!(consumer.poll_largest(&waiter).is_pending(), "it has no group yet");
		write(&a, 5);
		assert_eq!(
			consumer.poll_largest(&kio::Waiter::noop()),
			Poll::Ready(Some(Position::group(5)))
		);

		// Waiting on the serving copy, the reader still wakes for a replacement.
		assert!(consumer.poll_largest(&waiter).is_pending());
		flag.0.store(false, Ordering::SeqCst);
		let b = copy();
		write(&b, 7);
		routes.serve(b.consume());
		assert!(flag.0.load(Ordering::SeqCst), "the replacement woke the reader");
		assert_eq!(
			consumer.poll_largest(&kio::Waiter::noop()),
			Poll::Ready(Some(Position::group(7)))
		);
	}

	/// Where the live feed starts waits for a serving copy, and wakes when one is served. With
	/// none left to come, it is unknown.
	#[test]
	#[cfg(feature = "async")]
	fn the_live_start_waits_for_a_serving_copy() {
		for served in [true, false] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let mut sub = subscribe(&logical, HOUR);
			let wake = Arc::new(Wake::default());
			let waiter = kio::Waiter::new(wake.clone().into());
			assert!(sub.poll_start(&waiter).is_pending(), "served={served}");
			if !served {
				drop(routes);
				assert_eq!(sub.poll_start(&fresh()), Poll::Ready(None));
				continue;
			}
			let a = copy();
			routes.serve(a.consume());
			assert!(
				wake.0.load(std::sync::atomic::Ordering::Relaxed),
				"the new copy wakes the read"
			);
			assert!(sub.poll_start(&fresh()).is_ready());
		}
	}

	/// A budget that never writes off a group in these tests.
	const HOUR: Duration = Duration::from_secs(3600);

	/// Write a finished one-frame group, presented at its key in milliseconds.
	fn write(track: &track::Producer, key: u64) {
		let mut group = track.create_group(group::Info { start: key }).unwrap();
		group.write_frame(ts(key), b"x".as_ref()).unwrap();
		group.finish().unwrap();
	}

	/// The group a peek reserves, to take later.
	fn peek(sub: &mut track::Subscriber, ordered: bool) -> track::Reserved {
		let Poll::Ready(Ok(Some(reserved))) = sub.poll_peek_group(ordered, &kio::Waiter::noop()) else {
			panic!("a group to reserve")
		};
		reserved
	}

	/// Every group the reader hands out right now.
	fn drain(sub: &mut track::Subscriber, ordered: bool) -> Vec<u64> {
		let mut keys = Vec::new();
		loop {
			let res = match ordered {
				true => sub.poll_next_group(&kio::Waiter::noop()),
				false => sub.poll_recv_group(&kio::Waiter::noop()),
			};
			let Poll::Ready(Ok(Some(group))) = res else {
				return keys;
			};
			keys.push(group.start);
		}
	}

	/// A front's key-order reader holds back a gap above a time start its serving copy still
	/// waits to place, as a plain reader does: the group containing it may land below the gap.
	#[test]
	fn a_front_holds_a_gap_above_a_waiting_time_start() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let serving = received_copy();
		serving.anchored();
		routes.serve(serving.consume());
		write(&serving, 2000);
		serving.drop_groups(1500..1600, StreamError::NotFound);
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(None))
			.with_ranges([track::Range {
				start: crate::track::Start::Time(1500),
				end: None,
			}]);
		let mut sub = logical
			.consume()
			.subscribe(subscription)
			.now_or_never()
			.unwrap()
			.unwrap()
			.ordered();
		let waiter = kio::Waiter::noop();
		let mut read = || -> Vec<String> {
			let mut items = Vec::new();
			while let Poll::Ready(Ok(Some(item))) = sub.poll_next_item(&waiter) {
				items.push(match item {
					track::Item::Group(group) => format!("group {}", group.start),
					track::Item::Gap { range, .. } => format!("gap {range:?}"),
				});
			}
			items
		};
		assert_eq!(read(), Vec::<String>::new(), "the gap waits on the start");
		serving.time_starts([(1500, Some(1000), Duration::MAX)]);
		write(&serving, 1000);
		serving.skip_groups(1001..1500);
		serving.skip_groups(1600..2000);
		assert_eq!(read(), ["group 1000", "gap 1500..1600", "group 2000"]);
	}

	/// A front's key-order reader reports a group that lands on its copy after the read passed
	/// its key, as old, with when it landed, and never one it handed out.
	#[test]
	fn a_front_reports_a_late_arrival_as_a_gap() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR).ordered();
		let waiter = kio::Waiter::noop();
		let read = |sub: &mut track::Ordered| -> Vec<String> {
			let mut items = Vec::new();
			while let Poll::Ready(Ok(Some(item))) = sub.poll_next_item(&waiter) {
				items.push(match item {
					track::Item::Group(group) => format!("group {}", group.start),
					track::Item::Gap { range, error, landed } => {
						format!("gap {range:?} ({error}, landed {})", landed.is_some())
					}
				});
			}
			items
		};
		write(&a, 0);
		write(&a, 5);
		assert_eq!(read(&mut sub), ["group 0", "group 5"]);
		write(&a, 3);
		assert_eq!(read(&mut sub), ["gap 3..4 (old, landed true)"]);
		write(&a, 6);
		assert_eq!(read(&mut sub), ["group 6"]);
	}

	/// A front's ordered read that ends past a group its copy gave up, as one missing its head
	/// with no fill to ask, returns that gap before the end.
	#[test]
	#[cfg(feature = "async")]
	fn a_front_returns_the_gap_its_copy_gave_up_before_ending() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		let track::Received::Group(mut rest) = a.create_group_at(group::Info { start: 0 }, 1, 0..0).unwrap() else {
			panic!("nothing else holds the group");
		};
		rest.write_frame(ts(0), b"x".as_ref()).unwrap();
		rest.finish().unwrap();
		write(&a, 2);
		routes.serve(a.consume());
		let mut sub = logical
			.consume()
			.subscribe(
				Subscription::default()
					.with_delay(Delay::default().with_max(HOUR))
					.with_ranges([track::Range::groups(0..2)]),
			)
			.now_or_never()
			.unwrap()
			.unwrap()
			.ordered();
		let waiter = kio::Waiter::noop();
		let mut items = Vec::new();
		let end = loop {
			match sub.poll_next_item(&waiter) {
				Poll::Ready(Ok(Some(track::Item::Group(group)))) => items.push(format!("group {}", group.start)),
				Poll::Ready(Ok(Some(track::Item::Gap { range, .. }))) => items.push(format!("gap {range:?}")),
				end => break end,
			}
		};
		assert_eq!(items, ["gap 0..1"]);
		assert!(matches!(end, Poll::Ready(Ok(None))), "the read ends");
	}

	/// A front served by another front reports a late arrival on the inner copy too.
	#[test]
	fn a_nested_front_reports_a_late_arrival_as_a_gap() {
		let inner_routes = Producer::new();
		let inner = logical(&inner_routes);
		let b = copy();
		inner_routes.serve(b.consume());
		let routes = Producer::new();
		let outer = logical(&routes);
		routes.serve(inner.consume());
		let mut sub = subscribe(&outer, HOUR).ordered();
		let waiter = kio::Waiter::noop();
		let read = |sub: &mut track::Ordered| -> Vec<String> {
			let mut items = Vec::new();
			while let Poll::Ready(Ok(Some(item))) = sub.poll_next_item(&waiter) {
				items.push(match item {
					track::Item::Group(group) => format!("group {}", group.start),
					track::Item::Gap { range, error, .. } => format!("gap {range:?} ({error})"),
				});
			}
			items
		};
		write(&b, 0);
		write(&b, 5);
		assert_eq!(read(&mut sub), ["group 0", "group 5"]);
		write(&b, 3);
		assert_eq!(read(&mut sub), ["gap 3..4 (old)"]);
	}

	/// A group the window names under the local floor never lands on a copy, so it is no reason
	/// to hold an ordered copy's cursor on a group the window leaves out.
	#[test]
	fn an_ordered_reader_owes_no_window_group_under_its_local_floor() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = received_copy();
		a.anchored();
		routes.serve(a.consume());
		write(&a, 10);
		let mut open = a.create_group(group::Info { start: 100 }).unwrap();
		open.write_frame(ts(100), b"x".as_ref()).unwrap();
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(None))
			.with_ranges([
				track::Range {
					start: crate::track::Start::Time(10),
					end: Some(10),
				},
				track::Range {
					start: crate::track::Start::Time(100),
					end: Some(200),
				},
			]);
		let mut sub = logical
			.consume()
			.subscribe(subscription)
			.now_or_never()
			.unwrap()
			.unwrap()
			.with_range(60..);
		// The window resolves before any group is read.
		assert_eq!(peek(&mut sub, true).key(), 100);
		let starts: Vec<_> = sub
			.resolved_keys([0, 1])
			.into_iter()
			.map(|at| at.map(|at| at.start))
			.collect();
		assert_eq!(starts, [Some(10), Some(100)]);

		// The replacement places time 100 at group 80, the newest it holds at or before it, which
		// the window leaves out. Group 10 is still owed, but under the local floor.
		a.clone().abort(Error::Cancel).unwrap();
		// Published in order, so its ordered cursor waits on no group between.
		let b = copy();
		write(&b, 80);
		write(&b, 120);
		routes.serve(b.consume());
		assert_eq!(drain(&mut sub, true), [120]);
	}

	/// A window group the replacement declares skipped never lands on it, so it is no reason to
	/// hold its ordered cursor on a group the window leaves out.
	#[test]
	fn an_ordered_reader_owes_no_window_group_its_copy_declares_skipped() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = received_copy();
		a.anchored();
		routes.serve(a.consume());
		write(&a, 10);
		let mut open = a.create_group(group::Info { start: 100 }).unwrap();
		open.write_frame(ts(100), b"x".as_ref()).unwrap();
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(None))
			.with_ranges([
				track::Range {
					start: crate::track::Start::Time(10),
					end: Some(30),
				},
				track::Range {
					start: crate::track::Start::Time(100),
					end: Some(200),
				},
			]);
		let mut sub = logical
			.consume()
			.subscribe(subscription)
			.now_or_never()
			.unwrap()
			.unwrap();
		assert_eq!(peek(&mut sub, true).key(), 10);
		assert_eq!(drain(&mut sub, true), [10]);

		// The replacement places time 100 at group 80, which the window leaves out, and skips the
		// rest of the first range. Published in order, so its ordered cursor waits on no group
		// between.
		a.clone().abort(Error::Cancel).unwrap();
		let b = copy();
		b.skip_groups(11..31);
		write(&b, 80);
		write(&b, 120);
		routes.serve(b.consume());
		assert_eq!(drain(&mut sub, true), [120]);
	}

	/// A failed copy receives nothing more, so a window group under its cursor is no reason to
	/// hold it: it hands out what it holds and its failure is judged.
	#[test]
	fn an_ordered_reader_does_not_hold_a_failed_copy_for_a_group_it_can_no_longer_receive() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = received_copy();
		a.anchored();
		routes.serve(a.consume());
		write(&a, 10);
		let mut open = a.create_group(group::Info { start: 100 }).unwrap();
		open.write_frame(ts(100), b"x".as_ref()).unwrap();
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(None))
			.with_ranges([
				track::Range {
					start: crate::track::Start::Time(10),
					end: Some(30),
				},
				track::Range {
					start: crate::track::Start::Time(100),
					end: Some(200),
				},
			]);
		let mut sub = logical
			.consume()
			.subscribe(subscription)
			.now_or_never()
			.unwrap()
			.unwrap();
		assert_eq!(peek(&mut sub, true).key(), 10);
		assert_eq!(drain(&mut sub, true), [10]);

		// The replacement holds group 80, which the window leaves out, then fails without 11 to
		// 30 ever landing on it.
		a.clone().abort(Error::Cancel).unwrap();
		let b = received_copy();
		write(&b, 80);
		write(&b, 120);
		routes.serve(b.consume());
		// Open, it may still receive 11 to 30, which the failed copy no longer can: the switch
		// asks it for them, and its cursor stays on 80.
		assert_eq!(drain(&mut sub, true), [0u64; 0]);
		b.clone().abort(Error::Cancel).unwrap();
		assert_eq!(drain(&mut sub, true), [120]);
	}

	/// A late arrival the reader's ranges leave out is no gap of its own: it is neither reported
	/// nor held, and widening the ranges does not report it later, as the read never goes back.
	#[test]
	fn a_front_ignores_a_late_arrival_its_ranges_exclude() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR).ordered().with_range(50..);
		let waiter = kio::Waiter::noop();
		let read = |sub: &mut track::Ordered| -> Vec<String> {
			let mut items = Vec::new();
			while let Poll::Ready(Ok(Some(item))) = sub.poll_next_item(&waiter) {
				items.push(match item {
					track::Item::Group(group) => format!("group {}", group.start),
					track::Item::Gap { range, error, .. } => format!("gap {range:?} ({error})"),
				});
			}
			items
		};
		write(&a, 50);
		write(&a, 60);
		assert_eq!(read(&mut sub), ["group 50", "group 60"]);
		for key in (0..40).step_by(2) {
			write(&a, key);
			assert!(read(&mut sub).is_empty(), "group {key} is outside the ranges");
		}
		let mut sub = sub.with_range(..);
		let widened = read(&mut sub);
		assert!(
			widened.is_empty(),
			"an ordered read never goes back, so widening reports no gap and returns no group: {widened:?}"
		);
	}

	/// Lowering a front reader's local floor offers the cached groups below the old one it
	/// passed over, as a plain reader does.
	#[test]
	fn a_lowered_range_reoffers_the_unread_cached_groups() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR).with_range(4..);
		for key in 0..6 {
			write(&a, key);
		}
		assert_eq!(drain(&mut sub, false), vec![4, 5]);

		let mut sub = sub.with_range(1..);
		assert_eq!(drain(&mut sub, false), vec![3, 2, 1], "passed below the floor");
		let mut sub = sub.with_range(..);
		assert_eq!(drain(&mut sub, false), vec![0]);
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new(), "nothing goes out twice");
	}

	/// A reader that ran out of a finished track under a cap and ranges short of its last
	/// group reads that group once both widen past it, whichever widens last, as a
	/// publisher's reader does when an update reaches for the rest of an ended track.
	#[test]
	fn a_widened_reader_reads_a_finished_tracks_group_it_ran_out_before() {
		for ranges_first in [false, true] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let mut a = copy();
			routes.serve(a.consume());
			write(&a, 407);
			a.finish_at(Position::group(408)).unwrap();
			let history = Subscription::default()
				.with_delay(Delay::default().with_max(HOUR))
				.with_ranges([track::Range {
					start: crate::track::Start::Time(0),
					end: Some(406),
				}]);
			let sub = logical.consume().subscribe(history).now_or_never().unwrap().unwrap();
			let mut sub = sub.with_range(0..407);
			let Poll::Ready(ended) = sub.poll_recv_group(&kio::Waiter::noop()) else {
				panic!("the history read ends")
			};
			assert!(ended.unwrap().is_none(), "ranges_first={ranges_first}");

			let everything = sub.subscription().with_ranges([track::Range {
				start: crate::track::Start::Oldest,
				end: None,
			}]);
			let mut sub = match ranges_first {
				true => {
					sub.update(everything).unwrap();
					assert_eq!(drain(&mut sub, false), Vec::<u64>::new(), "the cap still ends at 407");
					sub.with_range(0..)
				}
				false => {
					let mut sub = sub.with_range(0..);
					assert_eq!(drain(&mut sub, false), Vec::<u64>::new(), "the ranges still end at 407");
					sub.update(everything).unwrap();
					sub
				}
			};
			assert_eq!(drain(&mut sub, false), [407], "ranges_first={ranges_first}");
		}
	}

	/// Lowering a resumed reader's floor offers the groups below the old floor that only a
	/// copy it already let go cached: the serving route is asked for just those, whether it
	/// caches them or gets them once asked. An ordered reader never goes back, so it asks
	/// for nothing.
	#[test]
	fn a_lowered_floor_asks_the_serving_route_for_a_released_copys_groups() {
		for (cached, ordered) in [(true, false), (false, false), (true, true)] {
			let case = format!("cached={cached}, ordered={ordered}");
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, HOUR).with_range(4..);
			for key in 0..6 {
				write(&a, key);
			}
			assert_eq!(drain(&mut sub, ordered), [4, 5]);

			let b = copy();
			if cached {
				for key in 0..6 {
					write(&b, key);
				}
			}
			routes.serve(b.consume());
			write(&b, 6);
			assert_eq!(drain(&mut sub, ordered), [6]);
			assert_eq!(copies(&routes), 1, "the replaced copy is let go: {case}");
			assert_eq!(b.subscription().unwrap().start(), Some(5));

			let mut sub = sub.with_range(1..).with_range(4..).with_range(1..);
			let asked = if ordered { 5 } else { 1 };
			assert_eq!(b.subscription().unwrap().start(), Some(asked), "{case}");
			assert_eq!(copies(&routes), if ordered { 1 } else { 2 }, "asked once: {case}");
			if !cached {
				assert_eq!(drain(&mut sub, ordered), Vec::<u64>::new());
				for key in 1..4 {
					write(&b, key);
				}
			}
			let lowered: &[u64] = if ordered { &[] } else { &[1, 2, 3] };
			assert_eq!(drain(&mut sub, ordered), lowered, "{case}");
			write(&b, 7);
			assert_eq!(drain(&mut sub, ordered), [7], "{case}");
		}
	}

	/// Replacing ranges below the serving copy's resume point places them where its cache
	/// holds them, not at that resume point, and the groups between are asked of the route.
	#[test]
	fn replaced_ranges_below_the_resume_point_place_where_the_cache_holds_them() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		for key in 0..6 {
			write(&a, key);
		}
		assert_eq!(drain(&mut sub, false), [0, 1, 2, 3, 4, 5]);
		let b = copy();
		for key in 0..6 {
			write(&b, key);
		}
		routes.serve(b.consume());
		write(&b, 6);
		assert_eq!(drain(&mut sub, false), [6]);
		assert_eq!(copies(&routes), 1, "the replaced copy is let go");

		let ranges = sub.subscription().with_ranges([track::Range {
			start: crate::track::Start::Time(2),
			end: None,
		}]);
		sub.update(ranges).unwrap();
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new(), "every group went out");
		assert_eq!(sub.resolved_range(0).map(|resolved| resolved.start), Some(2));
	}

	/// A range an edit adds below the serving copy's resume point is asked of the route for
	/// the groups that resume point cut off, as a whole replacement's would be.
	#[test]
	#[cfg(feature = "async")]
	fn an_added_range_below_the_resume_point_backfills_from_the_route() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let late = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_ranges([track::Range {
				start: crate::track::Start::Time(4),
				end: None,
			}]);
		let mut sub = logical.consume().subscribe(late).now_or_never().unwrap().unwrap();
		for key in 0..6 {
			write(&a, key);
		}
		assert_eq!(drain(&mut sub, false), [4, 5]);
		let b = copy();
		for key in 0..6 {
			write(&b, key);
		}
		routes.serve(b.consume());
		write(&b, 6);
		assert_eq!(drain(&mut sub, false), [6]);
		assert_eq!(copies(&routes), 1, "the replaced copy is let go");

		sub.edit(track::Edit {
			retire: Vec::new(),
			add: vec![(
				1,
				track::Range {
					start: crate::track::Start::Time(1),
					end: None,
				},
			)],
			priority: 0,
			delay: Delay::default().with_max(HOUR),
			order: track::Order::default(),
		})
		.unwrap();
		let mut read = drain(&mut sub, false);
		read.sort_unstable();
		assert_eq!(read, [1, 2, 3]);
	}

	/// A range an edit adds with a time start inside a group under the serving copy's resume
	/// point starts at that group, which the copy's cache holds.
	#[test]
	#[cfg(feature = "async")]
	fn an_added_time_start_below_the_resume_point_reads_the_group_holding_it() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let late = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_ranges([track::Range {
				start: crate::track::Start::Time(4),
				end: None,
			}]);
		let mut sub = logical.consume().subscribe(late).now_or_never().unwrap().unwrap();
		for key in [0, 2, 4, 6] {
			write(&a, key);
		}
		assert_eq!(drain(&mut sub, false), [4, 6]);
		let b = copy();
		for key in [0, 2, 4, 6] {
			write(&b, key);
		}
		routes.serve(b.consume());
		write(&b, 8);
		assert_eq!(drain(&mut sub, false), [8]);

		sub.edit(track::Edit {
			retire: Vec::new(),
			add: vec![(
				1,
				track::Range {
					start: crate::track::Start::Time(3),
					end: None,
				},
			)],
			priority: 0,
			delay: Delay::default().with_max(HOUR),
			order: track::Order::default(),
		})
		.unwrap();
		assert_eq!(drain(&mut sub, false), [2], "group 2 holds time 3");
	}

	/// A live join an edit adds with a Min Age reaching under the serving copy's resume point
	/// starts at the group holding its live edge minus the Min Age, which the copy's cache
	/// holds, as the copy resolved it before its resume point raised it.
	#[test]
	#[cfg(feature = "async")]
	fn an_added_live_join_below_the_resume_point_reads_the_group_its_min_age_reaches() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let late = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_ranges([track::Range {
				start: crate::track::Start::Time(4),
				end: None,
			}]);
		let mut sub = logical.consume().subscribe(late).now_or_never().unwrap().unwrap();
		for key in [0, 2, 4, 6] {
			write(&a, key);
		}
		assert_eq!(drain(&mut sub, false), [4, 6]);
		let b = copy();
		for key in [0, 2, 4, 6] {
			write(&b, key);
		}
		routes.serve(b.consume());
		write(&b, 8);
		assert_eq!(drain(&mut sub, false), [8]);

		sub.edit(track::Edit {
			retire: Vec::new(),
			add: vec![(1, track::Range::from(track::Start::Live(Duration::from_millis(6))))],
			priority: 0,
			delay: Delay::default().with_max(HOUR),
			order: track::Order::default(),
		})
		.unwrap();
		assert_eq!(drain(&mut sub, false), [2], "group 2 holds time 2, 6 ms behind group 8");
	}

	/// Ranges only an unanswered replacement can place, around its resume point: one wholly
	/// below it ends where it asks, and one above it starting at a time inside a group starts
	/// at that group.
	#[test]
	#[cfg(feature = "async")]
	fn a_replacement_places_time_anchors_around_its_resume_point() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let range = |start: u64, end: Option<u64>| track::Range {
			start: crate::track::Start::Time(start),
			end,
		};
		let late = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_ranges([range(4, None)]);
		let mut sub = logical.consume().subscribe(late).now_or_never().unwrap().unwrap();
		for key in 0..6 {
			write(&a, key);
		}
		assert_eq!(drain(&mut sub, false), [4, 5]);
		a.clone().abort(Error::Cancel).unwrap();

		let b = track::Request::new(Arc::new(broadcast::Info::default()), "video");
		routes.serve(b.consume());
		assert!(sub.recv_group().now_or_never().is_none());
		let ranges = sub.subscription().with_ranges([range(1, Some(2)), range(9, None)]);
		sub.update(ranges).unwrap();
		assert!(sub.recv_group().now_or_never().is_none());

		let b = b.accept(crate::track::Info::timed());
		for key in [0, 1, 2, 3, 4, 5, 6, 8, 10] {
			write(&b, key);
		}
		let mut read = drain(&mut sub, false);
		read.sort_unstable();
		assert_eq!(read, [1, 2, 8, 10], "group 8 holds time 9");
	}

	/// Edits past what the reader's log holds reach its copy as one whole ask, keyed as the
	/// reader keys its ranges, so the reader's next edit names the range it replaces there.
	#[test]
	fn a_copy_asked_whole_keys_its_ranges_as_the_reader_does() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let range = |start: u64| track::Range {
			start: crate::track::Start::Time(start),
			end: None,
		};
		let edit = |retire: u64, add: (u64, track::Range)| track::Edit {
			retire: vec![retire],
			add: vec![add],
			priority: 0,
			delay: Delay::default().with_max(HOUR),
			order: track::Order::default(),
		};
		let start = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_ranges([range(0)]);
		let mut sub = logical.consume().subscribe(start).now_or_never().unwrap().unwrap();
		assert!(sub.poll_recv_group(&kio::Waiter::noop()).is_pending());
		// Edited behind the reader's back, which a subscriber's own edit never is: each edit
		// logs two keys, past the 64 the log keeps beyond twice the ranges.
		let reader = routes.readers.lock().unwrap().iter().find_map(Weak::upgrade).unwrap();
		let channel = reader.lock().unwrap().subscription.clone();
		for key in 1..=40 {
			channel.edit(edit(key - 1, (key, range(key)))).unwrap();
		}
		assert!(sub.poll_recv_group(&kio::Waiter::noop()).is_pending());
		assert_eq!(a.subscription().unwrap().ranges, [range(40)]);

		sub.edit(edit(40, (41, range(41)))).unwrap();
		assert!(sub.poll_recv_group(&kio::Waiter::noop()).is_pending());
		assert_eq!(
			a.subscription().unwrap().ranges,
			[range(41)],
			"the edit replaced the range"
		);
	}

	#[test]
	fn a_fresh_nested_reader_ends_after_its_outer_reader_already_passed_its_groups() {
		let routes = Producer::new();
		let outer = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&outer, HOUR);
		for key in 0..4 {
			write(&a, key);
		}
		assert_eq!(drain(&mut sub, true), [0, 1, 2, 3]);

		let nested_routes = Producer::new();
		let nested = logical(&nested_routes);
		let b = copy();
		for key in 0..4 {
			write(&b, key);
		}
		nested_routes.serve(b.consume());
		routes.serve(nested.consume());
		let waiter = kio::Waiter::noop();
		assert!(sub.poll_next_group(&waiter).is_pending());

		// The nested reader has handed nothing out itself. Its containing reader's
		// past prefix must also release the replaced copy when its serving copy ends.
		let c = copy();
		for key in 0..4 {
			write(&c, key);
		}
		let c = c.finish().unwrap();
		nested_routes.serve(c.consume());
		assert!(matches!(sub.poll_next_group(&waiter), Poll::Ready(Ok(None))));
	}

	/// A bounded read through a nested front ends once its window is handed out, and not
	/// before.
	#[test]
	fn a_bounded_nested_read_ends_once_its_window_is_handed_out() {
		for ordered in [false, true] {
			let inner_routes = Producer::new();
			let inner = logical(&inner_routes);
			let outer_routes = Producer::new();
			let outer = logical(&outer_routes);
			outer_routes.serve(inner.consume());
			let a = copy();
			inner_routes.serve(a.consume());
			let subscription = Subscription::default()
				.with_delay(Delay::default().with_max(HOUR))
				.with_range(0..3);
			let mut sub = outer.consume().subscribe(subscription).now_or_never().unwrap().unwrap();
			let poll = |sub: &mut track::Subscriber| match ordered {
				true => sub.poll_next_group(&kio::Waiter::noop()),
				false => sub.poll_recv_group(&kio::Waiter::noop()),
			};
			write(&a, 0);
			write(&a, 1);
			assert_eq!(drain(&mut sub, ordered), [0, 1], "ordered={ordered}");
			assert!(
				poll(&mut sub).is_pending(),
				"ordered={ordered}: group 2 is still to come"
			);
			write(&a, 2);
			assert_eq!(drain(&mut sub, ordered), [2], "ordered={ordered}");
			assert!(
				matches!(poll(&mut sub), Poll::Ready(Ok(None))),
				"ordered={ordered}: the window was handed out"
			);
		}
	}

	/// A switch backfills the reader's gaps from where its ranges start, not from where the
	/// copy it replaced was raised to: a group still missing below that copy's resume point
	/// is asked of the next route too.
	#[test]
	fn a_second_switch_backfills_below_the_replaced_copys_resume_point() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = received_copy();
		routes.serve(a.consume());
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_ranges([track::Range {
				start: crate::track::Start::Time(3),
				end: None,
			}]);
		let mut sub = logical
			.consume()
			.subscribe(subscription)
			.now_or_never()
			.unwrap()
			.unwrap();
		write(&a, 3);
		write(&a, 7);
		assert_eq!(drain(&mut sub, false), [3, 7], "5 is missing");

		// b resumes at 7, placing the range's start there, and lacks 5 too.
		let b = received_copy();
		write(&b, 7);
		write(&b, 8);
		routes.serve(b.consume());
		assert_eq!(drain(&mut sub, false), [8]);

		let c = received_copy();
		routes.serve(c.consume());
		assert!(drain(&mut sub, false).is_empty());
		write(&c, 5);
		write(&c, 9);
		let mut rest = drain(&mut sub, false);
		rest.sort();
		assert_eq!(rest, [5, 9]);
	}

	/// A copy that resolves a time start below where the reader's window placed it offers
	/// a key-order read a group outside that window. The read leaves the copy's cursor
	/// below it while the window still owes a group under it, which may yet land on that
	/// copy, and moves past it once nothing under it is owed.
	#[test]
	fn a_key_order_read_waits_below_a_group_outside_its_window_only_while_one_is_owed() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = received_copy();
		write(&a, 10);
		routes.serve(a.consume());
		let from_ten = track::Range {
			start: crate::track::Start::Time(10),
			end: None,
		};
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_ranges([track::Range::groups(0..1), from_ten]);
		let mut sub = logical
			.consume()
			.subscribe(subscription)
			.now_or_never()
			.unwrap()
			.unwrap();
		// a places the window at 0 and from 10; the cap holds 10 back.
		sub.set_range(..10);
		assert!(drain(&mut sub, true).is_empty());

		// b holds 5 but not 10, so it places the second range at 5.
		let b = received_copy();
		write(&b, 5);
		routes.serve(b.consume());
		assert!(drain(&mut sub, true).is_empty(), "0 is owed below 5");
		for key in [0, 10, 11] {
			write(&b, key);
		}
		// Its own range from 5 waits for the keys up to 10 unless they were never produced.
		b.skip_groups(6..10);
		sub.set_range(..);
		assert_eq!(drain(&mut sub, true), [0, 10, 11]);
	}

	/// A key-order read holding a copy's cursor on a group outside its window, for a group the
	/// window still owes below it, wakes when that group lands on the copy.
	#[test]
	#[cfg(feature = "async")]
	fn a_key_order_read_holding_a_copy_wakes_when_the_owed_group_lands() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = received_copy();
		write(&a, 10);
		routes.serve(a.consume());
		let from_ten = track::Range {
			start: crate::track::Start::Time(10),
			end: None,
		};
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_ranges([track::Range::groups(0..1), from_ten]);
		let mut sub = logical
			.consume()
			.subscribe(subscription)
			.now_or_never()
			.unwrap()
			.unwrap();
		// a places the window at 0 and from 10; the cap holds 10 back.
		sub.set_range(..10);
		assert!(drain(&mut sub, true).is_empty());

		// b holds 5 but not 10, so it places the second range at 5, and 0 is owed below it.
		let b = received_copy();
		write(&b, 5);
		routes.serve(b.consume());
		let wake = Arc::new(Wake::default());
		let waiter = kio::Waiter::new(wake.clone().into());
		assert!(sub.poll_next_group(&waiter).is_pending());
		wake.0.store(false, std::sync::atomic::Ordering::Relaxed);
		write(&b, 0);
		assert!(wake.0.load(std::sync::atomic::Ordering::Relaxed));
		// The same task's read hands it out, not the idle answer it parked with.
		let Poll::Ready(Ok(Some(group))) = sub.poll_next_group(&waiter) else {
			panic!("the owed group is handed out")
		};
		assert_eq!(group.start, 0);
	}

	/// An ordered read replays the capped copies' groups once while the serving route is still
	/// pending, and the serving copy, once ready, reads on from the boundary.
	#[test]
	fn ordered_replay_reads_capped_copies_while_a_nested_route_waits() {
		for nested in [false, true] {
			let routes = Producer::new();
			let inner = logical(&routes);
			let outer_routes = Producer::new();
			let outer = logical(&outer_routes);
			outer_routes.serve(inner.consume());
			let a = copy();
			routes.serve(a.consume());
			let mut sub = subscribe(if nested { &outer } else { &inner }, HOUR);
			sub.set_range(..0);
			for key in 0..4 {
				write(&a, key);
			}
			assert!(
				drain(&mut sub, true).is_empty(),
				"subscribe the nested reader before switching"
			);
			let b = copy();
			for key in 0..4 {
				write(&b, key);
			}
			routes.serve(b.consume());
			assert!(drain(&mut sub, true).is_empty());
			let c = track::Request::new(Arc::new(broadcast::Info::default()), "video");
			routes.serve(c.consume());
			assert!(drain(&mut sub, true).is_empty());
			sub.set_range(..);
			assert_eq!(drain(&mut sub, true), [0, 1, 2, 3], "nested={nested}");
			let c = c.accept(crate::track::Info::timed());
			for key in 0..5 {
				write(&c, key);
			}
			assert_eq!(drain(&mut sub, true), [4], "a newly ready copy inherits the floor");
			let _c = c.finish().unwrap();
			assert!(
				matches!(sub.poll_next_group(&kio::Waiter::noop()), Poll::Ready(Ok(None))),
				"nested end propagates past covered copies"
			);
		}
	}

	/// A bounded read whose whole window was handed out ends after a route switch, to a copy
	/// with nothing or only a group already read, though the replaced copy is still live.
	#[test]
	fn a_read_range_ends_after_a_switch_to_a_repeating_route() {
		for ordered in [false, true] {
			for repeats in [false, true] {
				let case = format!("ordered={ordered}, repeats={repeats}");
				let routes = Producer::new();
				let logical = logical(&routes);
				let a = copy();
				routes.serve(a.consume());
				let subscription = Subscription::default()
					.with_delay(Delay::default().with_max(HOUR))
					.with_range(0..1);
				let mut sub = logical
					.consume()
					.subscribe(subscription)
					.now_or_never()
					.unwrap()
					.unwrap();
				write(&a, 0);
				assert_eq!(drain(&mut sub, ordered), [0], "{case}");
				let b = copy();
				if repeats {
					write(&b, 0);
				}
				routes.serve(b.consume());
				let res = match ordered {
					true => sub.poll_next_group(&kio::Waiter::noop()),
					false => sub.poll_recv_group(&kio::Waiter::noop()),
				};
				let res = res.map(|res| res.map(|group| group.map(|group| group.start)));
				assert!(
					matches!(res, Poll::Ready(Ok(None))),
					"{case}: the read ends, got {res:?}"
				);
				// A widening reads past the boundary from the serving route, not the capped copy.
				write(&a, 1);
				write(&b, 1);
				sub.update(
					Subscription::default()
						.with_delay(Delay::default().with_max(HOUR))
						.with_range(0..2),
				)
				.unwrap();
				assert_eq!(drain(&mut sub, ordered), [1], "{case}");
			}
		}
	}

	/// A bounded read whose whole window was handed out has nothing to judge it by while no
	/// route serves: it ends once one serves again.
	#[test]
	#[cfg(feature = "async")]
	fn a_read_range_waits_for_a_route_to_end() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_range(0..2);
		let mut sub = logical
			.consume()
			.subscribe(subscription)
			.now_or_never()
			.unwrap()
			.unwrap();
		write(&a, 0);
		write(&a, 1);
		assert_eq!(recv(&mut sub).start, 0);
		assert_eq!(recv(&mut sub).start, 1);
		routes.park();
		assert!(sub.poll_recv_group(&kio::Waiter::noop()).is_pending());
		routes.serve(a.consume());
		assert!(matches!(
			sub.poll_recv_group(&kio::Waiter::noop()),
			Poll::Ready(Ok(None))
		));
	}

	/// A bounded read takes the boundary group the serving copy gave up from the copy it
	/// replaced, which still holds it, and ends once it has.
	#[test]
	fn a_bounded_read_takes_the_boundary_group_from_the_replaced_copy() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_range(0..2);
		let mut sub = logical
			.consume()
			.subscribe(subscription)
			.now_or_never()
			.unwrap()
			.unwrap();
		write(&a, 0);
		assert_eq!(drain(&mut sub, false), [0]);
		let mut open = a.create_group(group::Info { start: 1 }).unwrap();
		open.write_frame(ts(1), b"x".as_ref()).unwrap();

		let b = copy();
		routes.serve(b.consume());
		b.drop_groups(1..2, StreamError::NotFound);
		assert_eq!(drain(&mut sub, false), [1]);
		open.finish().unwrap();
		// The serving copy places the window's end once a group past it lands.
		write(&b, 2);
		assert!(matches!(
			sub.poll_recv_group(&kio::Waiter::noop()),
			Poll::Ready(Ok(None))
		));
	}

	/// A bounded read whose serving copy resumed past a gap no replaced copy filled ends once
	/// those copies are let go, if its local range owes nothing there: a gap inside the local
	/// floor and cap was given up with them, while one the local range keeps out stays owed.
	#[kio::test]
	async fn a_read_range_past_a_given_up_gap_ends_only_inside_its_local_range() {
		for (start, end, ends) in [(2, 3, true), (2, 4, true), (0, 2, false)] {
			let case = format!("{start}..{end}");
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let budget = Duration::from_secs(1);
			let subscription = Subscription::default()
				.with_delay(Delay::default().with_max(budget))
				.with_range(0..5);
			let mut sub = logical
				.consume()
				.subscribe(subscription)
				.now_or_never()
				.unwrap()
				.unwrap();
			for sequence in [0, 1, 3] {
				write(&a, sequence);
			}
			assert_eq!(drain(&mut sub, false), [0, 1, 3], "{case}");
			let b = copy();
			routes.serve(b.consume());
			write(&b, 4);
			assert_eq!(drain(&mut sub, false), [4], "{case}");
			assert!(
				sub.poll_recv_group(&kio::Waiter::noop()).is_pending(),
				"{case}: the replaced copies may still deliver group 2"
			);
			sub.set_range(start..end);
			kio::clock::sleep(budget).await;
			let res = sub
				.poll_recv_group(&fresh())
				.map(|res| res.map(|group| group.map(|group| group.start)));
			match ends {
				true => assert!(matches!(res, Poll::Ready(Ok(None))), "{case}: {res:?}"),
				false => assert!(res.is_pending(), "{case}: {res:?}"),
			}
		}
	}

	/// A replaced copy is bounded only once the new copy delivers its first group: a group
	/// buffered on it must be read before a later group on the serving copy.
	#[test]
	fn key_reads_take_a_replaced_copys_lower_buffered_group() {
		replaced_copy_reads_lowest(true);
	}

	#[test]
	fn arrival_reads_take_a_replaced_copys_lower_buffered_group() {
		replaced_copy_reads_lowest(false);
	}

	fn replaced_copy_reads_lowest(ordered: bool) {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		write(&a, 0);
		assert_eq!(drain(&mut sub, ordered), [0]);
		write(&a, 1);
		let b = copy();
		routes.serve(b.consume());
		write(&b, 2);
		assert_eq!(drain(&mut sub, ordered), [1, 2]);
	}

	/// An ordered read after arrival reads passes a group the new copy replays that an arrival
	/// read already handed out, and takes the next.
	#[test]
	fn an_ordered_read_passes_a_replayed_group_an_arrival_read_handed_out() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		write(&a, 0);
		write(&a, 2);
		assert_eq!(drain(&mut sub, false), [0, 2]);
		let b = copy();
		write(&b, 2);
		write(&b, 3);
		routes.serve(b.consume());
		assert_eq!(drain(&mut sub, true), [3]);
	}

	#[test]
	fn a_reserved_group_survives_eviction_and_nested_route_replacement() {
		for ordered in [false, true] {
			for nested in [false, true] {
				let routes = Producer::new();
				let inner = logical(&routes);
				let outer_routes = Producer::new();
				let outer = logical(&outer_routes);
				outer_routes.serve(inner.consume());
				let a = copy();
				routes.serve(a.consume());
				let mut sub = subscribe(if nested { &outer } else { &inner }, HOUR);
				let mut one = a.create_group(group::Info { start: 1 }).unwrap();
				one.write_frame(ts(1), b"x".as_ref()).unwrap();
				let one = one.finish().unwrap();
				write(&a, 3);
				let reserved = peek(&mut sub, ordered);
				assert_eq!(reserved.key(), 1);
				one.abort(Error::Evicted).unwrap();
				let b = copy();
				routes.serve(b.consume());
				write(&b, 2);
				let group = sub.take(reserved).unwrap();
				assert_eq!(group.start, 1, "ordered={ordered}, nested={nested}");
				// Group 3, the boundary, is the replaced copy's; group 2 is backfilled.
				assert_eq!(drain(&mut sub, ordered), [2, 3], "ordered={ordered}, nested={nested}");
			}
		}
	}

	/// A route that returns to the reserving copy after another served moves that copy's
	/// bounds, even past the reserved group when it arrived late, but taking still hands it
	/// out: whichever reader's route returns, the containing one's or a nested copy's.
	#[test]
	fn a_reserved_group_survives_its_route_returning() {
		for ordered in [false, true] {
			for late in [false, true].into_iter().filter(|late| !(ordered && *late)) {
				for (nested, outer_flips) in [(false, false), (true, false), (true, true)] {
					let case = format!("ordered={ordered}, late={late}, nested={nested}, outer_flips={outer_flips}");
					let routes = Producer::new();
					let inner = logical(&routes);
					let outer_routes = Producer::new();
					let outer = logical(&outer_routes);
					outer_routes.serve(inner.consume());
					let a = copy();
					routes.serve(a.consume());
					let mut sub = subscribe(if nested { &outer } else { &inner }, HOUR);
					write(&a, 0);
					if late {
						write(&a, 3);
					}
					drain(&mut sub, ordered);
					let mut two = a.create_group(group::Info { start: 2 }).unwrap();
					two.write_frame(ts(2), b"x".as_ref()).unwrap();
					let two = two.finish().unwrap();
					let reserved = peek(&mut sub, ordered);
					assert_eq!(reserved.key(), 2, "{case}");
					two.abort(Error::Evicted).unwrap();
					match outer_flips {
						true => {
							outer_routes.serve(copy().consume());
							outer_routes.serve(inner.consume());
						}
						false => {
							routes.serve(copy().consume());
							routes.serve(a.consume());
						}
					}
					assert_eq!(sub.take(reserved).unwrap().start, 2, "{case}");
					write(&a, 4);
					assert_eq!(drain(&mut sub, ordered), [4], "{case}");
				}
			}
		}
	}

	#[test]
	fn arrival_reads_still_deliver_a_lower_group_that_arrives_later() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		write(&a, 0);
		assert_eq!(drain(&mut sub, false), [0]);
		let b = copy();
		routes.serve(b.consume());
		write(&b, 2);
		assert_eq!(drain(&mut sub, false), [2]);
		write(&b, 1);
		assert_eq!(drain(&mut sub, false), [1]);
	}

	/// A switch caps the replaced copy at the largest group any copy holds, at once: a group
	/// the old route gets after the switch is the new route's, which is asked from there.
	#[test]
	fn a_switch_caps_the_replaced_copy_at_the_largest_group_held() {
		for ordered in [false, true] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, HOUR);
			write(&a, 0);
			assert_eq!(drain(&mut sub, ordered), [0]);
			// Held, though not yet handed out.
			write(&a, 1);

			let b = copy();
			routes.serve(b.consume());
			let asked = b.subscription().expect("subscribed");
			assert_eq!(asked.start(), Some(1));
			assert_eq!(asked.resume, Some(Position::new(1, 1)), "past the frames held");
			// The old route finishes the boundary group and sends nothing newer.
			let old = a.subscription().expect("still subscribed");
			assert_eq!(old.ranges[0].end, Some(1), "capped on the wire");
			assert_eq!(drain(&mut sub, ordered), [1]);
			write(&a, 2);
			assert_eq!(drain(&mut sub, ordered), Vec::<u64>::new());
			write(&b, 2);
			assert_eq!(drain(&mut sub, ordered), [2]);
		}
	}

	/// A switch before any copy holds a group has nothing to split: the replaced copy goes at
	/// once, and the new route serves everything from the reader's own start. Mirrors the
	/// `@moq/net` "a switch before any group drops the replaced copy once the new route delivers",
	/// where the front switches before the new copy delivers.
	#[test]
	fn a_switch_before_any_group_drops_the_replaced_copy() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		assert_eq!(copies(&routes), 1);

		let b = copy();
		routes.serve(b.consume());
		assert_eq!(copies(&routes), 1, "only the new route's copy");
		assert!(a.subscription().is_none(), "the old route is unsubscribed");
		assert_eq!(
			b.subscription().expect("subscribed").start(),
			None,
			"asked as the reader was"
		);
		write(&a, 0);
		write(&b, 1);
		assert_eq!(drain(&mut sub, false), [1]);
	}

	/// How many copies the logical track's only reader holds.
	fn copies(routes: &Producer) -> usize {
		let readers = routes.readers.lock().unwrap();
		let reader = readers.iter().find_map(Weak::upgrade).unwrap();
		reader.lock().unwrap().copies.len()
	}

	/// A group below the boundary the old route was still sending when the new route took
	/// over arrives once polled: the old copy stays while it may still land, and goes once its
	/// publisher drops it or the reader's budget passes, a budget after the serving copy holds
	/// a group past the boundary.
	#[kio::test]
	async fn an_arrival_reader_gets_a_group_still_in_flight_on_the_replaced_route() {
		for outcome in ["lands", "dropped", "expired"] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = received_copy();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, Duration::from_secs(1));
			write(&a, 0);
			assert_eq!(drain(&mut sub, false), [0]);
			write(&a, 2);

			let b = copy();
			routes.serve(b.consume());
			write(&b, 3);
			assert_eq!(drain(&mut sub, false), [2, 3]);
			let held = |routes: &Producer| {
				let readers = routes.readers.lock().unwrap();
				let reader = readers.iter().find_map(Weak::upgrade).unwrap();
				let reader = reader.lock().unwrap();
				let a = a.consume();
				reader.copies.iter().any(|copy| copy.track.same_channel(&a))
			};
			assert!(held(&routes), "group 1 may still land on {outcome}");

			match outcome {
				"lands" => {}
				"dropped" => a.drop_groups(1..2, StreamError::NotFound),
				_ => {
					kio::clock::sleep(Duration::from_secs(1)).await;
					assert!(sub.poll_recv_group(&fresh()).is_pending());
				}
			}
			if outcome != "lands" {
				assert_eq!(drain(&mut sub, false), Vec::<u64>::new());
				assert!(!held(&routes), "nothing lacked is left on {outcome}");
			}
			write(&a, 1);
			let expected: &[u64] = if outcome == "lands" { &[1] } else { &[] };
			assert_eq!(drain(&mut sub, false), expected, "{outcome}");
		}
	}

	/// A replaced copy kept only for a group still read from it delivers nothing more once the
	/// reader's budget passed since the serving copy superseded it.
	#[kio::test]
	async fn an_expired_copy_kept_for_a_read_delivers_no_more_groups() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(1));
		write(&a, 0);
		let _reading = recv(&mut sub);
		let b = copy();
		routes.serve(b.consume());
		write(&b, 2);
		assert_eq!(drain(&mut sub, false), [2]);
		kio::clock::sleep(Duration::from_secs(1)).await;
		assert!(sub.poll_recv_group(&fresh()).is_pending());
		assert_eq!(copies(&routes), 2, "kept for the group still read");
		write(&a, 1);
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());
	}

	/// An ordered reader past the new copy's first group skips what the old route still
	/// sends below it, so it keeps nothing for that.
	#[test]
	fn an_ordered_reader_keeps_no_copy_for_a_group_it_passed() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(1));
		write(&a, 0);
		assert_eq!(drain(&mut sub, true), [0]);
		let b = copy();
		routes.serve(b.consume());
		write(&b, 2);
		assert_eq!(drain(&mut sub, true), [2]);
		assert_eq!(drain(&mut sub, true), Vec::<u64>::new());
		assert_eq!(copies(&routes), 1);
	}

	/// A replaced copy stays bounded just below the serving copy's first group: a group it gets
	/// past that bound is the serving route's to deliver, whatever the serving copy hands out
	/// later and however it ends.
	#[test]
	fn a_replaced_copy_keeps_its_bound_as_the_serving_copy_goes_on() {
		for ended in [false, true] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, HOUR);
			write(&a, 0);
			write(&a, 1);
			// Still read, so the replaced copy stays.
			let _reading = recv(&mut sub);
			assert_eq!(drain(&mut sub, false), [1]);
			let b = copy();
			routes.serve(b.consume());
			write(&b, 2);
			assert_eq!(drain(&mut sub, false), [2]);
			write(&b, 4);
			assert_eq!(drain(&mut sub, false), [4]);
			write(&a, 3);
			if !ended {
				assert_eq!(drain(&mut sub, false), Vec::<u64>::new());
				continue;
			}
			let _b = b.finish().unwrap();
			assert!(matches!(
				sub.poll_recv_group(&kio::Waiter::noop()),
				Poll::Ready(Ok(None))
			));
		}
	}

	/// A switch asks the serving route for the groups below the boundary from the lowest one
	/// no replaced copy holds, and the replaced copy, capped there, delivers nothing past it.
	#[test]
	fn a_switch_backfills_from_the_lowest_group_no_replaced_copy_holds() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		write(&a, 0);
		assert_eq!(drain(&mut sub, false), [0]);
		// Held, though not yet handed out.
		write(&a, 2);
		write(&a, 3);
		let b = copy();
		routes.serve(b.consume());
		// Group 2 is the replaced copy's to drain, group 1 the serving route's to fill.
		assert_eq!(drain(&mut sub, false), [2, 3]);
		{
			let readers = routes.readers.lock().unwrap();
			let reader = readers.iter().find_map(Weak::upgrade).unwrap();
			let reader = reader.lock().unwrap();
			let spans: Vec<_> = reader.copies.iter().map(|copy| (copy.floor, copy.end())).collect();
			// The replaced copy stays while group 1 may still land on it.
			assert_eq!(spans, [(None, Some(4)), (Some(1), Some(3)), (Some(3), None)]);
		}
		write(&b, 1);
		assert_eq!(drain(&mut sub, false), [1]);
		write(&a, 4);
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());
		write(&b, 4);
		assert_eq!(drain(&mut sub, false), [4]);
	}

	/// A replaced copy holding a group without its head gives it up under a bounded window, so
	/// the switch asks the serving route for that group rather than leave it to the copy.
	#[test]
	#[cfg(feature = "async")]
	fn a_switch_backfills_a_group_the_replaced_copy_holds_without_its_head() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = logical
			.consume()
			.subscribe(
				Subscription::default()
					.with_delay(Delay::default().with_max(HOUR))
					.with_ranges([track::Range::groups(0..4)]),
			)
			.now_or_never()
			.unwrap()
			.unwrap()
			.ordered();
		let waiter = kio::Waiter::noop();
		assert!(sub.poll_next_item(&waiter).is_pending());
		let track::Received::Group(mut rest) = a.create_group_at(group::Info { start: 0 }, 1, 0..0).unwrap() else {
			panic!("nothing else holds the group");
		};
		rest.write_frame(ts(0), b"x".as_ref()).unwrap();
		rest.finish().unwrap();
		for key in 1..4 {
			write(&a, key);
		}

		let b = copy();
		for key in 0..4 {
			write(&b, key);
		}
		routes.serve(b.consume());
		let mut items = Vec::new();
		let end = loop {
			match sub.poll_next_item(&waiter) {
				Poll::Ready(Ok(Some(track::Item::Group(group)))) => items.push(format!("group {}", group.start)),
				Poll::Ready(Ok(Some(track::Item::Gap { range, .. }))) => items.push(format!("gap {range:?}")),
				end => break end,
			}
		};
		assert_eq!(items, ["group 0", "group 1", "group 2", "group 3"]);
		assert!(matches!(end, Poll::Ready(Ok(None))), "the read ends");
	}

	/// A replaced copy that gets a handed-out group only after the reader polled on backs it:
	/// the serving copy's group failing continues from the old copy's.
	#[test]
	fn a_group_the_replaced_copy_gets_late_backs_the_serving_copys() {
		for ordered in [false, true] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, HOUR);
			write(&a, 0);
			assert_eq!(drain(&mut sub, ordered), [0]);
			let b = copy();
			routes.serve(b.consume());

			let mut failing = b.create_group(group::Info { start: 1 }).unwrap();
			failing.write_frame(ts(1), b"a".as_ref()).unwrap();
			let noop = kio::Waiter::noop();
			let res = match ordered {
				true => sub.poll_next_group(&noop),
				false => sub.poll_recv_group(&noop),
			};
			let Poll::Ready(Ok(Some(mut reading))) = res else {
				panic!("expected group 1");
			};
			assert_eq!(read(&mut reading), Some(b"a".to_vec()));
			assert_eq!(drain(&mut sub, ordered), Vec::<u64>::new());

			// The old route finishes group 1 only now, and the new route's copy of it fails.
			let mut whole = a.create_group(group::Info { start: 1 }).unwrap();
			whole.write_frame(ts(1), b"a".as_ref()).unwrap();
			whole.write_frame(ts(2), b"b".as_ref()).unwrap();
			whole.finish().unwrap();
			failing
				.abort(Error::Stream(StreamError::Session(SessionError::Cancel)))
				.unwrap();
			assert_eq!(read(&mut reading), Some(b"b".to_vec()), "ordered={ordered}");
			assert_eq!(read(&mut reading), None);
		}
	}

	/// A replaced copy that got no group yet still backs one at or past where it was asked to
	/// start: its first group may still land within the grace.
	#[test]
	fn a_replaced_copy_backs_a_group_before_its_first_arrival() {
		for ordered in [false, true] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, HOUR);
			write(&a, 0);
			assert_eq!(drain(&mut sub, ordered), [0]);
			a.clone().abort(Error::Dropped).unwrap();
			let b = copy();
			routes.serve(b.consume());
			let c = copy();
			routes.serve(c.consume());

			let mut failing = c.create_group(group::Info { start: 1 }).unwrap();
			failing.write_frame(ts(1), b"a".as_ref()).unwrap();
			let noop = kio::Waiter::noop();
			let res = match ordered {
				true => sub.poll_next_group(&noop),
				false => sub.poll_recv_group(&noop),
			};
			let Poll::Ready(Ok(Some(mut reading))) = res else {
				panic!("expected group 1");
			};
			assert_eq!(read(&mut reading), Some(b"a".to_vec()));
			assert_eq!(drain(&mut sub, ordered), Vec::<u64>::new());

			// b's first group lands only now, and c's copy of it fails.
			let mut whole = b.create_group(group::Info { start: 1 }).unwrap();
			whole.write_frame(ts(1), b"a".as_ref()).unwrap();
			whole.write_frame(ts(2), b"b".as_ref()).unwrap();
			whole.finish().unwrap();
			failing
				.abort(Error::Stream(StreamError::Session(SessionError::Cancel)))
				.unwrap();
			assert_eq!(read(&mut reading), Some(b"b".to_vec()), "ordered={ordered}");
			assert_eq!(read(&mut reading), None);
		}
	}

	/// A new copy that ends cleanly before delivering a group does not end the track while
	/// the copy it replaced may still deliver a group below that end.
	#[test]
	fn a_clean_end_waits_for_a_group_the_replaced_copy_still_carries() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = received_copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(1));
		write(&a, 0);
		write(&a, 2);
		assert_eq!(drain(&mut sub, false), [0, 2]);

		// The new route has only the group already handed out, and ends there.
		let b = copy();
		write(&b, 2);
		let b = b.finish().unwrap();
		routes.serve(b.consume());
		let noop = kio::Waiter::noop();
		assert!(sub.poll_recv_group(&noop).is_pending(), "group 1 may still land");

		write(&a, 1);
		assert_eq!(drain(&mut sub, false), [1]);
		assert!(matches!(sub.poll_recv_group(&noop), Poll::Ready(Ok(None))));
	}

	/// A capped cursor never probes the gap outside its cap, but the resume reader still
	/// waits for it. Its grace must wake the clean end before the replacement's budget.
	#[test]
	fn a_replaced_copys_gap_wakes_the_clean_end_at_its_grace() {
		for first_group in [false, true] {
			for ordered in [false, true] {
				let routes = Producer::new();
				let logical = logical(&routes);
				let a = received_copy();
				let pool = a.broadcast().pool.clone();
				let opened = a.consume().now();
				let noop = kio::Waiter::noop();
				pool.poll(opened, &noop);
				routes.serve(a.consume());
				let budget = Duration::from_secs(1);
				let mut sub = subscribe(&logical, budget);
				write(&a, 0);
				write(&a, 2);
				assert_eq!(drain(&mut sub, false), [0, 2]);

				pool.poll(opened.saturating_add(budget / 2), &noop);
				let b = copy();
				write(&b, if first_group { 3 } else { 2 });
				routes.serve(b.consume());
				if first_group {
					assert_eq!(recv(&mut sub).start, 3);
				}
				let _b = b.finish().unwrap();
				sub.set_range(..1);
				let wake = Arc::new(Wake::default());
				let waiter = kio::Waiter::new(wake.clone().into());
				let mut poll = || match ordered {
					true => sub.poll_next_group(&waiter),
					false => sub.poll_recv_group(&waiter),
				};
				assert!(poll().is_pending());
				wake.0.store(false, std::sync::atomic::Ordering::Relaxed);
				assert!(poll().is_pending());
				assert!(!wake.0.load(std::sync::atomic::Ordering::Relaxed));

				let deadline = opened.saturating_add(budget);
				assert_eq!(
					pool.poll(deadline.saturating_sub(Duration::from_micros(1)), &noop),
					Some(deadline)
				);
				assert!(
					!wake.0.load(std::sync::atomic::Ordering::Relaxed),
					"not before the grace"
				);
				pool.poll(deadline, &noop);
				assert!(
					wake.0.load(std::sync::atomic::Ordering::Relaxed),
					"first_group={first_group}, ordered={ordered}: the gap's grace wakes the reader"
				);
				assert!(matches!(poll(), Poll::Ready(Ok(None))));
			}
		}
	}

	/// A read parked on that wait, which never looks at the route, still wakes once the
	/// front serves another route.
	#[test]
	fn a_read_waiting_past_a_drained_copy_wakes_when_the_route_changes() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = received_copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(1));
		write(&a, 0);
		write(&a, 2);
		assert_eq!(drain(&mut sub, false), [0, 2]);

		let b = copy();
		write(&b, 2);
		let b = b.finish().unwrap();
		routes.serve(b.consume());
		let wake = Arc::new(Wake::default());
		let waiter = kio::Waiter::new(wake.clone().into());
		assert!(sub.poll_recv_group(&waiter).is_pending(), "group 1 may still land");
		// Bounding the replaced copy updates its subscription, which wakes the read once.
		wake.0.store(false, std::sync::atomic::Ordering::Relaxed);
		assert!(sub.poll_recv_group(&waiter).is_pending());
		assert!(
			sub.poll_recv_group(&waiter).is_pending(),
			"nothing fired, so it parks again"
		);
		assert!(!wake.0.load(std::sync::atomic::Ordering::Relaxed));

		let c = copy();
		routes.serve(c.consume());
		assert!(wake.0.load(std::sync::atomic::Ordering::Relaxed));
	}

	/// A resumed read that found nothing parks again at once until something it parked on
	/// fires, but parks anew for another task, which the next group then wakes instead.
	#[test]
	fn an_idle_resumed_read_parks_for_the_task_polling_it() {
		for ordered in [false, true] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, Duration::from_secs(1));
			write(&a, 0);
			let mut read = |waiter: &kio::Waiter| match ordered {
				true => sub.poll_next_group(waiter),
				false => sub.poll_recv_group(waiter),
			};
			let first = Arc::new(Wake::default());
			let waiter = kio::Waiter::new(first.clone().into());
			assert!(matches!(read(&waiter), Poll::Ready(Ok(Some(_)))));
			assert!(read(&waiter).is_pending());
			assert!(read(&waiter).is_pending(), "nothing fired, so it parks again");
			assert!(!first.0.load(std::sync::atomic::Ordering::Relaxed));

			let other = Arc::new(Wake::default());
			let moved = kio::Waiter::new(other.clone().into());
			assert!(read(&moved).is_pending());
			write(&a, 1);
			assert!(
				other.0.load(std::sync::atomic::Ordering::Relaxed),
				"the next group wakes the task that polled last"
			);
			assert!(matches!(read(&moved), Poll::Ready(Ok(Some(_)))));
		}
	}

	/// An error a group read from a copy whose route then failed is no verdict on the group:
	/// the read continues from the next route.
	#[test]
	#[cfg(feature = "async")]
	fn a_failed_routes_group_error_continues_from_the_next_route() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		let mut open = a.create_group(group::Info { start: 0 }).unwrap();
		open.write_frame(ts(0), b"a".as_ref()).unwrap();
		let mut reading = recv(&mut sub);
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));
		open.abort(Error::NotFound).unwrap();
		a.clone().abort(Error::Dropped).unwrap();
		assert!(reading.read_frame().now_or_never().is_none(), "no verdict yet");

		let b = copy();
		let mut whole = b.create_group(group::Info { start: 0 }).unwrap();
		whole.write_frame(ts(0), b"a".as_ref()).unwrap();
		whole.write_frame(ts(1), b"b".as_ref()).unwrap();
		whole.finish().unwrap();
		routes.serve(b.consume());
		assert_eq!(read(&mut reading), Some(b"b".to_vec()));
		assert_eq!(read(&mut reading), None);
	}

	/// A copy whose route failed still continues a failed group it finished: a failed track
	/// keeps its finished groups and drops its open ones, so only a whole group is taken.
	#[test]
	#[cfg(feature = "async")]
	fn a_failed_group_continues_from_a_failed_route_that_finished_it() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		write(&a, 0);
		assert_eq!(drain(&mut sub, false), [0]);
		let b = copy();
		routes.serve(b.consume());
		let c = copy();
		routes.serve(c.consume());

		// Every route holds group 1: a whole, b cut off by its route failing, c failing after a frame.
		let mut whole = a.create_group(group::Info { start: 1 }).unwrap();
		whole.write_frame(ts(1), b"a".as_ref()).unwrap();
		whole.write_frame(ts(2), b"b".as_ref()).unwrap();
		whole.finish().unwrap();
		let mut partial = b.create_group(group::Info { start: 1 }).unwrap();
		partial.write_frame(ts(1), b"a".as_ref()).unwrap();
		b.clone().abort(Error::Dropped).unwrap();
		let mut failing = c.create_group(group::Info { start: 1 }).unwrap();
		failing.write_frame(ts(1), b"a".as_ref()).unwrap();
		let mut reading = recv(&mut sub);
		assert_eq!(reading.start, 1);
		// a's route fails too, after it finished the group.
		a.clone().abort(Error::Dropped).unwrap();
		failing
			.abort(Error::Stream(StreamError::Session(SessionError::Cancel)))
			.unwrap();
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));
		assert_eq!(read(&mut reading), Some(b"b".to_vec()));
		assert_eq!(read(&mut reading), None);
		drop(partial);
	}

	/// A group the serving copy failed continues from a replaced copy that holds it whole,
	/// in either read order, whether it failed before or after it was handed out, and even
	/// once the reader read on past it.
	#[test]
	#[cfg(feature = "async")]
	fn a_group_the_serving_copy_failed_continues_from_a_replaced_copy() {
		for (ordered, early) in [(false, true), (true, true), (false, false), (true, false)] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, HOUR);
			write(&a, 0);
			assert_eq!(drain(&mut sub, ordered), [0]);
			let b = copy();
			routes.serve(b.consume());

			// Both routes hold group 1: the old one whole, the serving one failing after a frame.
			let mut whole = a.create_group(group::Info { start: 1 }).unwrap();
			whole.write_frame(ts(1), b"a".as_ref()).unwrap();
			whole.write_frame(ts(2), b"b".as_ref()).unwrap();
			whole.finish().unwrap();
			let mut failing = b.create_group(group::Info { start: 1 }).unwrap();
			failing.write_frame(ts(1), b"a".as_ref()).unwrap();
			let mut failing = Some(failing);
			if early {
				failing
					.take()
					.unwrap()
					.abort(Error::Stream(StreamError::Session(SessionError::Cancel)))
					.unwrap();
			}
			let noop = kio::Waiter::noop();
			let res = match ordered {
				true => sub.poll_next_group(&noop),
				false => sub.poll_recv_group(&noop),
			};
			let Poll::Ready(Ok(Some(mut reading))) = res else {
				panic!("expected group 1");
			};
			assert_eq!(reading.start, 1);
			// Reading on must not let go of the replaced copy while the group still needs it.
			assert_eq!(drain(&mut sub, ordered), Vec::<u64>::new());
			if let Some(failing) = failing {
				failing
					.abort(Error::Stream(StreamError::Session(SessionError::Cancel)))
					.unwrap();
			}
			assert_eq!(read(&mut reading), Some(b"a".to_vec()));
			assert_eq!(read(&mut reading), Some(b"b".to_vec()));
			assert_eq!(read(&mut reading), None);
		}
	}

	/// A replaced copy whose route fails still backs a group handed out while it held it
	/// whole, even once the reader polled on and saw that copy end.
	#[test]
	#[cfg(feature = "async")]
	fn a_failed_group_continues_from_a_backup_whose_route_ended() {
		for ordered in [false, true] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, HOUR);
			write(&a, 0);
			assert_eq!(drain(&mut sub, ordered), [0]);
			let b = copy();
			routes.serve(b.consume());

			let mut whole = a.create_group(group::Info { start: 1 }).unwrap();
			whole.write_frame(ts(1), b"a".as_ref()).unwrap();
			whole.write_frame(ts(2), b"b".as_ref()).unwrap();
			whole.finish().unwrap();
			let mut failing = b.create_group(group::Info { start: 1 }).unwrap();
			failing.write_frame(ts(1), b"a".as_ref()).unwrap();
			let res = match ordered {
				true => sub.poll_next_group(&kio::Waiter::noop()),
				false => sub.poll_recv_group(&kio::Waiter::noop()),
			};
			let Poll::Ready(Ok(Some(mut reading))) = res else {
				panic!("expected group 1");
			};
			assert_eq!(read(&mut reading), Some(b"a".to_vec()));

			// The old route goes away, and the reader polls on and sees its copy end.
			a.clone().abort(Error::Dropped).unwrap();
			assert_eq!(drain(&mut sub, ordered), Vec::<u64>::new());
			failing
				.abort(Error::Stream(StreamError::Session(SessionError::Cancel)))
				.unwrap();
			assert_eq!(read(&mut reading), Some(b"b".to_vec()));
			assert_eq!(read(&mut reading), None);
		}
	}

	/// A group handed out from a replaced copy, then moved to the serving copy once the
	/// replaced route gave it up, reads on to its last frame after that copy is let go,
	/// in either read order.
	#[test]
	#[cfg(feature = "async")]
	fn a_moved_group_outlives_the_replaced_copy_it_was_handed_out_from() {
		for ordered in [false, true] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, HOUR);
			// The boundary group, which the serving route has nothing of yet, so it comes from
			// the replaced one.
			let mut open = a.create_group(group::Info { start: 8 }).unwrap();
			open.write_frame(ts(8), b"a".as_ref()).unwrap();
			let b = copy();
			let dynamic = b.requests();
			routes.serve(b.consume());

			let res = match ordered {
				true => sub.poll_next_group(&kio::Waiter::noop()),
				false => sub.poll_recv_group(&kio::Waiter::noop()),
			};
			let Poll::Ready(Ok(Some(mut reading))) = res else {
				panic!("expected group 8");
			};
			assert_eq!(reading.start, 8);
			assert_eq!(read(&mut reading), Some(b"a".to_vec()));

			// The replaced route gives it up, so the rest is fetched from the serving route.
			open.abort(Error::Stream(StreamError::DeliveryTimeout)).unwrap();
			assert!(reading.read_frame().now_or_never().is_none());
			let request = dynamic.requested_fill_group().now_or_never().unwrap().unwrap();
			assert_eq!((request.key(), request.frame_start()), (8, 1));
			let mut rest = request.accept(None).unwrap().expect("a fresh group");
			assert!(reading.read_frame().now_or_never().is_none());

			// Past the boundary, the replaced copy is let go.
			write(&b, 9);
			assert_eq!(drain(&mut sub, ordered), [9]);
			{
				let readers = routes.readers.lock().unwrap();
				let reader = readers.iter().find_map(Weak::upgrade).unwrap();
				let reader = reader.lock().unwrap();
				let b = b.consume();
				assert!(
					reader.copies.iter().all(|copy| copy.track.same_channel(&b)),
					"ordered={ordered}"
				);
			}

			rest.write_frame(ts(9), b"b".as_ref()).unwrap();
			rest.write_frame(ts(10), b"c".as_ref()).unwrap();
			rest.finish().unwrap();
			assert_eq!(read(&mut reading), Some(b"b".to_vec()), "ordered={ordered}");
			assert_eq!(read(&mut reading), Some(b"c".to_vec()));
			assert_eq!(read(&mut reading), None);
		}
	}

	/// A drop poll with nothing to report still lets go of a replaced copy once the last
	/// group handed out from it is dropped: nothing wakes the parked group read for that.
	#[test]
	#[cfg(feature = "async")]
	fn a_drop_poll_lets_go_a_replaced_copy_once_its_group_is_dropped() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		write(&a, 0);
		let Poll::Ready(Ok(Some(held))) = sub.poll_recv_group(&kio::Waiter::noop()) else {
			panic!("expected group 0");
		};
		let b = copy();
		routes.serve(b.consume());
		write(&b, 1);
		assert_eq!(drain(&mut sub, false), [1]);
		assert_eq!(copies(&routes), 2, "the held group keeps its copy");

		drop(held);
		assert!(sub.poll_drop(&kio::Waiter::noop()).is_pending());
		assert_eq!(copies(&routes), 1, "the replaced copy was let go");
	}

	/// A group still read from a replaced copy keeps that copy, even once the copy neither
	/// caches it nor may land it again within its grace, until the read lets it go.
	#[test]
	#[cfg(feature = "async")]
	fn a_held_group_keeps_its_replaced_copy_until_it_is_dropped() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = received_copy();
		let pool = a.broadcast().pool.clone();
		let opened = a.consume().now();
		let noop = kio::Waiter::noop();
		pool.poll(opened, &noop);
		routes.serve(a.consume());
		let budget = Duration::from_secs(1);
		let mut sub = subscribe(&logical, budget);
		let mut open = a.create_group(group::Info { start: 0 }).unwrap();
		open.write_frame(ts(0), b"a".as_ref()).unwrap();
		let mut reading = recv(&mut sub);
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));

		let b = copy();
		routes.serve(b.consume());
		write(&b, 1);
		assert_eq!(drain(&mut sub, false), [1]);
		open.abort(Error::Stream(StreamError::DeliveryTimeout)).unwrap();
		pool.poll(opened.saturating_add(budget), &noop);
		assert!(sub.poll_recv_group(&fresh()).is_pending());
		assert_eq!(copies(&routes), 2, "group 0 is still read from the replaced copy");

		drop(reading);
		assert!(sub.poll_recv_group(&fresh()).is_pending());
		assert_eq!(copies(&routes), 1);
	}

	/// A replaced copy that may still land a group the reader holds backs that group only
	/// within its grace.
	#[test]
	fn a_replaced_copy_backs_a_held_group_only_within_its_grace() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = received_copy();
		let pool = a.broadcast().pool.clone();
		let opened = a.consume().now();
		let noop = kio::Waiter::noop();
		pool.poll(opened, &noop);
		routes.serve(a.consume());
		let budget = Duration::from_secs(1);
		let mut sub = subscribe(&logical, budget);
		write(&a, 0);
		assert_eq!(drain(&mut sub, false), [0]);

		let b = copy();
		routes.serve(b.consume());
		write(&b, 1);
		let held = recv(&mut sub);
		assert_eq!(held.start, 1);
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());
		assert_eq!(copies(&routes), 2, "group 1 may still land on the replaced copy");

		pool.poll(opened.saturating_add(budget), &noop);
		assert!(sub.poll_recv_group(&fresh()).is_pending());
		assert_eq!(copies(&routes), 1, "past its grace");
	}

	/// A group the new copy replays, already handed out, does not bound the replaced copy:
	/// the old route's next group still arrives.
	#[test]
	fn a_group_the_new_route_replays_bounds_nothing() {
		for ordered in [false, true] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, Duration::ZERO);
			write(&a, 0);
			assert_eq!(drain(&mut sub, ordered), [0]);
			write(&a, 1);

			let b = copy();
			routes.serve(b.consume());
			write(&b, 0);
			assert_eq!(drain(&mut sub, ordered), [1]);
		}
	}

	/// A copy delivering a group long after the reader moved past it does not hand it out
	/// again, however many groups came between; one never handed out is still delivered.
	#[test]
	fn a_late_group_is_handed_out_once_however_late() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		write(&a, 0);
		assert_eq!(drain(&mut sub, false), [0]);

		let b = copy();
		routes.serve(b.consume());
		for key in 2..=2000 {
			write(&b, key);
		}
		assert_eq!(drain(&mut sub, false), (2..=2000).collect::<Vec<_>>());
		assert_eq!(sub.delivered_runs(), 2, "the gap at 1 is within the budget");

		// The replacement's copies of groups 1 and 0 land last: 1 is new, 0 is not.
		write(&b, 1);
		write(&b, 0);
		assert_eq!(drain(&mut sub, false), [1]);
		assert_eq!(sub.delivered_runs(), 1);
	}

	/// An ordered reader never goes back, late duplicate or not.
	#[test]
	fn an_ordered_reader_skips_a_late_group() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		write(&a, 0);
		assert_eq!(drain(&mut sub, true), [0]);

		let b = copy();
		routes.serve(b.consume());
		for key in 1..=2000 {
			write(&b, key);
		}
		assert_eq!(drain(&mut sub, true), (1..=2000).collect::<Vec<_>>());
		write(&b, 0);
		assert_eq!(drain(&mut sub, true), Vec::<u64>::new());
	}

	/// A group the serving copy aborts and re-creates is handed out once, as it is to a
	/// direct reader of that copy.
	#[test]
	fn a_recreated_group_is_handed_out_once() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		write(&a, 0);
		let open = a.create_group(group::Info { start: 1 }).unwrap();
		assert_eq!(drain(&mut sub, false), [0, 1]);

		open.abort(Error::Cancel).unwrap();
		write(&a, 1);
		write(&a, 2);
		assert_eq!(drain(&mut sub, false), [2]);
	}

	/// Two routes taking turns, each also delivering the other's groups late and out of
	/// order, hand out every group once. What the reader remembers does not grow with the
	/// groups or the flaps.
	#[test]
	fn route_flaps_hand_out_every_group_once() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let tracks = [copy(), copy()];
		routes.serve(tracks[0].consume());
		let mut sub = subscribe(&logical, HOUR);
		let mut seen = Vec::new();
		for round in 0..16u64 {
			let (serving, other) = (&tracks[round as usize % 2], &tracks[1 - round as usize % 2]);
			let groups = round * 10..round * 10 + 10;
			// The serving route brings half the round, the reader takes it, then the rest.
			for key in groups.clone().take(5) {
				write(serving, key);
			}
			seen.extend(drain(&mut sub, false));
			for key in groups.clone().skip(5) {
				write(serving, key);
			}
			// The other route takes over and the replaced copy hands out the rest; the
			// other's own copy of the round lands only afterwards, newest first.
			routes.serve(other.consume());
			seen.extend(drain(&mut sub, false));
			for key in groups.clone().rev() {
				write(other, key);
			}
			seen.extend(drain(&mut sub, false));
			assert_eq!(sub.delivered_runs(), 1, "round {round}");
		}
		let mut sorted = seen.clone();
		sorted.sort_unstable();
		assert_eq!(sorted, (0..160).collect::<Vec<_>>(), "handed out: {seen:?}");
	}

	/// A reader holding an old group open keeps its replaced copy, and moving its group
	/// bounds neither repeats a group nor loses one.
	#[test]
	fn held_groups_and_moved_bounds_hand_out_each_group_once() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		sub.set_range(..=0);
		let mut held = a.create_group(group::Info { start: 0 }).unwrap();
		held.write_frame(ts(0), b"a".as_ref()).unwrap();
		let Poll::Ready(Ok(Some(reading))) = sub.poll_recv_group(&kio::Waiter::noop()) else {
			panic!("group 0 is handed out")
		};
		// Past the cap: parked on the copy, not handed out.
		write(&a, 1);
		write(&a, 2);
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());

		let b = copy();
		routes.serve(b.consume());
		for key in [2, 0, 3, 1] {
			write(&b, key);
		}
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());

		sub.set_range(..);
		let mut seen = drain(&mut sub, false);
		seen.sort_unstable();
		assert_eq!(seen, [1, 2, 3]);
		sub.set_range(..);
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());

		// The replaced copy ends with the held group, declaring no end: another route continues the track.
		held.finish().unwrap();
		a.clone().abort(Error::Dropped).unwrap();
		drop(reading);
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());
		write(&b, 4);
		assert_eq!(drain(&mut sub, false), [4]);
		assert_eq!(sub.delivered_runs(), 1);
	}

	/// Across route changes, an endless track with groups missing remembers only the gaps
	/// within the reader's budget: one past it is forgotten, one within it still delivered.
	#[test]
	fn a_resumed_reader_forgets_gaps_past_its_budget() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let tracks = [copy(), copy()];
		routes.serve(tracks[0].consume());
		// Groups present 1ms apart, so the budget spans ten of them.
		let mut sub = subscribe(&logical, Duration::from_millis(10));
		let mut most = 0;
		for key in 0..3000u64 {
			let track = &tracks[(key / 500) as usize % 2];
			// Every 500 groups the other route takes over.
			if key % 500 == 0 {
				routes.serve(track.consume());
			}
			// Every third group never arrives.
			if key % 3 == 1 {
				continue;
			}
			write(track, key);
			assert_eq!(drain(&mut sub, false), [key]);
			most = most.max(sub.delivered_runs());
		}
		// The floor and the four gaps a budget spans, twice over across a route change: the
		// new copy starts at its first group, so the gaps just before it wait for that one
		// to fall past the budget.
		assert_eq!(most, 9);

		// 2998 is within the budget, 2500 far past it.
		write(&tracks[1], 2998);
		write(&tracks[1], 2500);
		assert_eq!(drain(&mut sub, false), [2998]);
	}

	/// A group below the replacement's resume point that the reader never got is asked of
	/// the new route, as one cursor on the logical track would still get it late, and a
	/// widened floor hands nothing out twice.
	#[test]
	#[cfg(feature = "async")]
	fn a_group_below_the_replacement_resume_point_is_backfilled() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(10));
		let mut group = a.create_group(group::Info { start: 5 }).unwrap();
		group.write_frame(ts(5), b"x".as_ref()).unwrap();
		group.finish().unwrap();
		assert_eq!(recv(&mut sub).start, 5);

		let b = copy();
		routes.serve(b.consume());
		// Subscribe the replacement before it has anything, so the update moves a ready copy.
		assert!(sub.recv_group().now_or_never().is_none());
		for key in [2, 6] {
			let mut group = b.create_group(group::Info { start: key }).unwrap();
			group.write_frame(ts(key), b"x".as_ref()).unwrap();
			group.finish().unwrap();
		}

		sub.set_range(..);
		assert_eq!(drain(&mut sub, false), [2, 6]);
	}

	/// A serving copy that ran out at its session's settled end still hands out a group in
	/// flight below that end while a copy it replaced holds the read open, and wakes the read.
	#[test]
	fn a_late_group_on_a_drained_serving_copy_is_read() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = received_copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		write(&a, 0);
		assert_eq!(drain(&mut sub, false), [0]);
		write(&a, 2);

		// The replaced copy, capped at 2, may still deliver 1, which the new route is asked for.
		let b = received_copy();
		routes.serve(b.consume());
		assert_eq!(drain(&mut sub, false), [2]);
		write(&b, 3);
		assert_eq!(drain(&mut sub, false), [3]);
		b.finish_shared().unwrap();
		let wake = Arc::new(Wake::default());
		let waiter = kio::Waiter::new(wake.clone().into());
		assert!(
			sub.poll_recv_group(&waiter).is_pending(),
			"the replaced copy may still deliver 1"
		);

		write(&b, 1);
		assert!(
			wake.0.load(std::sync::atomic::Ordering::Relaxed),
			"the late group wakes the read"
		);
		assert_eq!(drain(&mut sub, false), [1]);
	}

	/// A late group under the serving copy's resume point is asked of its route again, once
	/// the backfill that asked for it ran out and was let go.
	#[test]
	fn a_late_group_under_the_resume_point_is_backfilled_again() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = received_copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		write(&a, 2);
		a.finish_shared().unwrap();
		assert_eq!(recv(&mut sub).start, 2);

		// A switch away and back resumes `a` past 2; the backfill below it finds its track
		// complete and runs out, while `b`'s copies hold the read open.
		let b = received_copy();
		routes.serve(b.consume());
		assert!(sub.poll_recv_group(&kio::Waiter::noop()).is_pending());
		routes.serve(a.consume());
		assert!(sub.poll_recv_group(&kio::Waiter::noop()).is_pending());
		// `b` fails, and 1 lands late on `a` before the read looks again.
		b.abort(Error::Stream(StreamError::Session(SessionError::Cancel)))
			.unwrap();
		write(&a, 1);
		assert_eq!(drain(&mut sub, false), [1]);
	}

	/// A replacement copy keeps each range where the serving copy resolved it: the floor
	/// raises a later time range's start rather than lowering it to the floor, and capping
	/// the replaced copy never widens a time end.
	#[test]
	#[cfg(feature = "async")]
	fn a_replacement_keeps_a_later_time_range_disjoint() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let ranges = [
			track::Range::groups(0..1),
			track::Range {
				start: crate::track::Start::Time(10_000),
				end: Some(11_000),
			},
		];
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(Duration::from_secs(30)))
			.with_ranges(ranges);
		let mut sub = logical
			.consume()
			.subscribe(subscription)
			.now_or_never()
			.unwrap()
			.unwrap();
		for key in 0..13 {
			let mut group = a.create_group(group::Info { start: key * 1000 }).unwrap();
			group.write_frame(ts(key * 1000), b"x".as_ref()).unwrap();
			group.finish().unwrap();
		}
		assert_eq!(recv(&mut sub).start, 0);

		let b = copy();
		routes.serve(b.consume());
		// The read subscribes the replacement while the replaced copy still serves.
		let mut rest = Vec::new();
		while let Some(Ok(Some(group))) = sub.recv_group().now_or_never() {
			rest.push(group.start);
		}
		for key in 0..13 {
			let mut group = b.create_group(group::Info { start: key * 1000 }).unwrap();
			group.write_frame(ts(key * 1000), b"x".as_ref()).unwrap();
			group.finish().unwrap();
		}
		while let Some(Ok(Some(group))) = sub.recv_group().now_or_never() {
			rest.push(group.start);
		}
		rest.sort();
		assert_eq!(rest, vec![10_000, 11_000]);
	}

	/// Ranges a front's reader edits one key at a time reach each of its copies as an edit,
	/// keyed as the reader keys them, and place the reader's window where the same ranges
	/// given whole do, across a route switch and copies answered or not.
	#[test]
	fn edited_ranges_reach_every_copy_as_given_whole() {
		use rand::{RngExt, SeedableRng};
		let mut rng = rand::rngs::StdRng::seed_from_u64(0x66726f6e74);
		let routes = Producer::new();
		let groups = || {
			let copy = copy();
			for key in (0..20_000).step_by(1000) {
				write(&copy, key);
			}
			copy
		};
		let (a, b) = (groups(), groups());
		routes.serve(a.consume());
		let range = |rng: &mut rand::rngs::StdRng| {
			let start = match rng.random_range(0..4) {
				0 => track::Start::Oldest,
				_ => track::Start::Time(rng.random_range(0..24_000)),
			};
			let end = rng.random_bool(0.6).then(|| rng.random_range(0..24_000));
			track::Range { start, end }
		};
		let answer = |sub: &Subscriber| {
			let mut reader = sub.reader();
			let groups = reader.groups;
			for copy in &mut reader.copies {
				let _ = copy.poll_ready(groups, &kio::Waiter::noop());
			}
		};
		// Key 0 is the default subscription's one range.
		let mut live = BTreeMap::from([(0, track::Range::default())]);
		let (mut next, mut placed) = (0, 0);
		let edited = routes.consume().subscribe(Subscription::default().into());
		for step in 0..300 {
			match step {
				50 => answer(&edited),
				150 => routes.serve(b.consume()),
				200 => answer(&edited),
				_ => {}
			}
			let retire: Vec<u64> = live.keys().copied().filter(|_| rng.random_bool(0.2)).collect();
			let add: Vec<(u64, track::Range)> = (0..rng.random_range(0..3))
				.map(|_| {
					next += 1;
					(next, range(&mut rng))
				})
				.collect();
			for key in &retire {
				live.remove(key);
			}
			live.extend(add.iter().copied());
			edited
				.subscription()
				.edit(track::Edit {
					retire,
					add,
					priority: 0,
					delay: Delay::default(),
					order: track::Order::default(),
				})
				.unwrap();
			edited.mirror().sync();
			{
				let reader = edited.reader();
				assert!(reader.mirrored.ranges.iter().eq(live.iter()), "step {step}");
				if let Some(Resumed::Ranges(keys)) = &reader.resumed {
					assert!(
						keys.iter().all(|key| live.contains_key(key)),
						"step {step}: only live ranges wait to be backfilled"
					);
				}
				for copy in &reader.copies {
					let held = copy.preferences().read();
					let keys: Vec<u64> = held.ranges().map(|(key, _)| key).collect();
					assert!(
						keys.iter().eq(live.keys()),
						"step {step}: the copy keys as the reader does"
					);
					for key in keys {
						assert_eq!(
							held.range(key),
							copy.wire(&reader.mirrored, key),
							"step {step}, key {key}"
						);
					}
				}
			}
			if step < 50 {
				continue;
			}
			let whole = routes
				.consume()
				.subscribe(Subscription::default().with_ranges(live.values().copied()).into());
			answer(&whole);
			let resolved = edited.resolved_keys(live.keys().copied());
			assert_eq!(resolved, whole.resolved_keys(0..live.len() as u64), "step {step}");
			assert_eq!(edited.runs(0), whole.runs(0), "step {step}");
			placed += resolved.iter().flatten().count();
		}
		assert!(placed > 1000, "the copies placed the ranges: {placed}");
	}

	/// A new copy asked for a reader whose update has yet to reach its copies, as when
	/// another thread asks between the update and its mirroring, places the updated live
	/// join behind the live edge by the updated delay, as the reader would, then raises it
	/// to the boundary: the copy holding the groups behind the edge drains them.
	#[test]
	fn a_new_copy_places_an_unmirrored_live_join_by_its_delay() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		write(&a, 0);
		let mut sub = subscribe(&logical, HOUR).with_range(0..1);
		assert_eq!(recv(&mut sub).start, 0);
		for key in [1000, 2000, 3000] {
			write(&a, key);
		}
		// A new range, so it resolves afresh rather than where the reader's old one did.
		let behind = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_ranges([track::Range {
				start: track::Start::Live(Duration::from_millis(1500)),
				end: Some(1_000_000),
			}]);
		let reader = routes.readers.lock().unwrap().iter().find_map(Weak::upgrade).unwrap();
		let channel = reader.lock().unwrap().subscription.clone();
		channel.update(behind).unwrap();

		let b = copy();
		let _held = routes.subscribe(&b.consume(), logical.subscription().unwrap(), 1);
		assert_eq!(
			b.subscription().unwrap().start(),
			Some(3000),
			"the join 1.5s behind the edge at 3000, raised to it"
		);
	}

	/// As [`a_new_copy_places_an_unmirrored_live_join_by_its_delay`], from a failed copy: its
	/// cache places nothing, so the new copy is asked for the updated start as given, raised
	/// to the boundary at the largest group the failed copy holds.
	#[test]
	fn a_new_copy_takes_no_placement_of_an_unmirrored_range_from_a_failed_copy() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		write(&a, 0);
		let mut sub = subscribe(&logical, HOUR).with_range(0..1);
		assert_eq!(recv(&mut sub).start, 0);
		for key in [1000, 2000] {
			write(&a, key);
		}
		a.clone().abort(Error::Cancel).unwrap();
		let later = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_ranges([track::Range {
				start: crate::track::Start::Time(1500),
				end: None,
			}]);
		let reader = routes.readers.lock().unwrap().iter().find_map(Weak::upgrade).unwrap();
		let channel = reader.lock().unwrap().subscription.clone();
		channel.update(later).unwrap();

		let b = copy();
		let _held = routes.subscribe(&b.consume(), logical.subscription().unwrap(), 1);
		assert_eq!(
			b.subscription().unwrap().start(),
			Some(2000),
			"not the failed copy's 1000, nor the start as given"
		);
	}

	/// A held subscription asked whole resumes where its readers got to, as a new copy is
	/// asked, and a reader's edit then reaches it as an edit, in the readers' order, even
	/// while they need the live feed. A change of feed, which only a whole ask carries, keys
	/// its ranges by position again.
	#[test]
	#[cfg(feature = "async")]
	fn a_held_subscription_follows_its_readers_terms_edit_by_edit() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut terms = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_order(track::Order::Ascending);
		terms.feed = true;
		let mut holder = logical.consume().subscribe(terms).now_or_never().unwrap().unwrap();
		write(&a, 0);
		assert_eq!(recv(&mut holder).start, 0);

		let b = copy();
		let weak = logical.weak();
		let mut held = super::Held::new(b.consume().subscribe(Subscription::default()));
		assert!(routes.follow(&mut held, &weak));
		assert_eq!(
			b.subscription().unwrap().resume,
			Some(Position::new(0, 1)),
			"past the group the reader holds"
		);

		let revision = held.sub.preferences().read().revision;
		holder
			.edit(track::Edit {
				retire: Vec::new(),
				add: vec![(
					1,
					track::Range {
						start: crate::track::Start::Time(5),
						end: None,
					},
				)],
				priority: 0,
				delay: Delay::default().with_max(HOUR),
				order: track::Order::Ascending,
			})
			.unwrap();
		assert!(routes.follow(&mut held, &weak));
		{
			let asked = held.sub.preferences().read();
			assert!(asked.changes(revision).is_some(), "asked as an edit, not whole");
			assert_eq!(asked.subscription().order, track::Order::Ascending);
		}

		// Retiring the first range leaves the held subscription keyed past it. A feed only
		// a whole ask carries then keys the same ranges by position again.
		holder
			.edit(track::Edit {
				retire: vec![0],
				add: Vec::new(),
				priority: 0,
				delay: Delay::default().with_max(HOUR),
				order: track::Order::Ascending,
			})
			.unwrap();
		assert!(routes.follow(&mut held, &weak));
		let mut unfed = holder.subscription();
		unfed.feed = false;
		holder.update(unfed).unwrap();
		assert!(routes.follow(&mut held, &weak));
		let asked = held.sub.preferences().read();
		assert!(!asked.subscription().feed);
		assert_eq!(asked.ranges().map(|(key, _)| key).collect::<Vec<_>>(), [0]);
	}

	/// A held subscription following its readers' demand asks, edit by edit, for the ranges
	/// the whole fold does, as readers edit their ranges, arrive, and leave, and keeps an
	/// entry only for each reader still here.
	#[test]
	fn a_held_subscription_follows_each_edit_as_the_whole_fold() {
		use rand::{RngExt, SeedableRng};
		let mut rng = rand::rngs::StdRng::seed_from_u64(0x68656c64);
		let routes = Producer::new();
		let a = copy();
		for key in (0..20_000).step_by(1000) {
			write(&a, key);
		}
		let logical = logical(&routes);
		routes.serve(a.consume());
		let weak = logical.weak();
		let range = |rng: &mut rand::rngs::StdRng| {
			let start = match rng.random_range(0..4) {
				0 => track::Start::Oldest,
				_ => track::Start::Time(rng.random_range(0..24_000)),
			};
			let end = rng.random_bool(0.6).then(|| rng.random_range(0..24_000));
			track::Range { start, end }
		};
		let reader = || {
			logical
				.consume()
				.subscribe(Subscription::default())
				.now_or_never()
				.unwrap()
				.unwrap()
		};
		// Each reader with its live keys; key 0 is the default subscription's one range.
		let mut readers: Vec<(track::Subscriber, Vec<u64>, u64)> = (0..3).map(|_| (reader(), vec![0], 0)).collect();
		let mut held = super::Held::new(a.consume().subscribe(Subscription::default()));
		let sorted = |mut ranges: Vec<track::Range>| {
			ranges.sort_unstable();
			ranges
		};
		for step in 0..400 {
			match rng.random_range(0..20) {
				0 => readers.push((reader(), vec![0], 0)),
				1 if readers.len() > 1 => {
					readers.swap_remove(rng.random_range(0..readers.len()));
				}
				_ => {
					let index = rng.random_range(0..readers.len());
					let (sub, live, next) = &mut readers[index];
					let retire: Vec<u64> = live.iter().copied().filter(|_| rng.random_bool(0.2)).collect();
					live.retain(|key| !retire.contains(key));
					let add: Vec<(u64, track::Range)> = (0..rng.random_range(0..3))
						.map(|_| {
							*next += 1;
							live.push(*next);
							(*next, range(&mut rng))
						})
						.collect();
					sub.edit(track::Edit {
						retire,
						add,
						priority: rng.random_range(0..3),
						delay: Delay::default(),
						order: track::Order::default(),
					})
					.unwrap();
				}
			}
			assert!(routes.follow(&mut held, &weak));
			let (demand, subscribers) = weak.subscription().unwrap();
			let whole = routes.subscription(demand.clone(), subscribers);
			let asked = held.sub.preferences().read().subscription();
			assert_eq!(sorted(asked.ranges), sorted(whole.ranges), "step {step}");
			assert_eq!(asked.priority, demand.priority, "step {step}");
			// A reader that left takes its entry, and the allocation that entry kept, with it.
			let folded = held.fold.as_ref().map_or(0, |fold| fold.readers.len());
			assert_eq!(folded, readers.len(), "step {step}");
		}
		readers.clear();
		assert!(!routes.follow(&mut held, &weak), "the last reader lets it go");
	}

	/// A replacement updated to a time range it cannot resolve yet asks for that range,
	/// not everything from the floor: before it answers, and once it answers with nothing
	/// to resolve it against.
	#[test]
	fn a_replacement_keeps_an_updated_time_start() {
		let routes = Producer::new();
		let a = copy();
		routes.serve(a.consume());
		let sub = routes.consume().subscribe(Subscription::default().into());
		let mut reader = sub.reader.lock().unwrap();
		let copy = &mut reader.copies[0];
		copy.floor = Some(0);
		let ranges = vec![track::Range {
			start: crate::track::Start::Time(10_000),
			end: None,
		}];
		let updated = Subscription::default().with_ranges(ranges.clone());
		assert!(matches!(copy.sub, Sub::Pending(_)));
		let updated = Mirrored::of(&updated);
		copy.update(&updated, &Change::All);
		assert_eq!(copy.wire_all(&updated).ranges, ranges);
		assert!(copy.poll_ready((0, None), &kio::Waiter::noop()).is_ready());
		assert_eq!(copy.wire_all(&updated).ranges, ranges);
	}

	/// A copy that already answered resolves ranges the reader replaced against its cache,
	/// rather than asking with its resolution of the ranges it was first asked.
	#[test]
	fn an_answered_copy_resolves_replaced_ranges() {
		let routes = Producer::new();
		let a = copy();
		for sequence in 0..5 {
			write(&a, sequence);
		}
		routes.serve(a.consume());
		let sub = routes.consume().subscribe(Subscription::default().into());
		let mut reader = sub.reader.lock().unwrap();
		let copy = &mut reader.copies[0];
		copy.floor = Some(0);
		assert!(copy.poll_ready((0, None), &kio::Waiter::noop()).is_ready());
		// Two behind the newest frame, at 4.
		let updated =
			Subscription::default().with_ranges([track::Range::from(track::Start::Live(Duration::from_millis(2)))]);
		let updated = Mirrored::of(&updated);
		copy.update(&updated, &Change::All);
		assert_eq!(copy.wire_all(&updated).ranges[0].start, crate::track::Start::Time(2));
	}

	/// A replacement asked for a range reaching back past where the reader got to, while
	/// still unanswered, hands out each group the reader lacks below that point once: the
	/// backfill under its resume point carries them, and nothing comes twice.
	#[test]
	#[cfg(feature = "async")]
	fn a_pending_replacement_hands_out_the_groups_below_the_floor_once() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(30));
		let mut five = a.create_group(group::Info { start: 5000 }).unwrap();
		five.write_frame(ts(5000), b"x".as_ref()).unwrap();
		assert_eq!(recv(&mut sub).start, 5000);

		let b = track::Request::new(Arc::new(broadcast::Info::default()), "video");
		routes.serve(b.consume());
		// The read subscribes the replacement, which has not answered yet.
		assert!(sub.recv_group().now_or_never().is_none());
		// No start: from the oldest group.
		let back = sub.subscription().with_ranges([track::Range::default()]);
		sub.update(back).unwrap();
		assert!(sub.recv_group().now_or_never().is_none());

		let b = b.accept(crate::track::Info::timed());
		for key in 0..7 {
			let mut group = b.create_group(group::Info { start: key * 1000 }).unwrap();
			group.write_frame(ts(key * 1000), b"x".as_ref()).unwrap();
			group.finish().unwrap();
		}
		let mut rest = Vec::new();
		while let Some(Ok(Some(group))) = sub.recv_group().now_or_never() {
			rest.push(group.start);
		}
		assert_eq!(rest, vec![0, 1000, 2000, 3000, 4000, 6000]);
	}

	/// Ranges widened while only an unanswered replacement can place them keep the switch's
	/// backfill pending: the read it answers in asks for the groups below its resume point,
	/// which come once without waiting on a later group.
	#[test]
	#[cfg(feature = "async")]
	fn a_range_widened_before_the_replacement_answers_backfills_once() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_ranges([track::Range {
				start: crate::track::Start::Time(5),
				end: None,
			}]);
		let mut sub = logical
			.consume()
			.subscribe(subscription)
			.now_or_never()
			.unwrap()
			.unwrap();
		write(&a, 5);
		drop(recv(&mut sub));
		a.abort(Error::Dropped).unwrap();

		let b = track::Request::new(Arc::new(broadcast::Info::default()), "video");
		routes.serve(b.consume());
		// The read subscribes the replacement, which has not answered yet.
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());
		let back = sub.subscription().with_ranges([track::Range {
			start: crate::track::Start::Time(1),
			end: None,
		}]);
		sub.update(back).unwrap();
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());

		let b = b.accept(crate::track::Info::timed());
		for key in 0..5 {
			write(&b, key);
		}
		let mut rest = drain(&mut sub, false);
		rest.sort_unstable();
		assert_eq!(rest, [1, 2, 3, 4]);
		write(&b, 6);
		assert_eq!(drain(&mut sub, false), [6]);
	}

	/// A route's backfill widened by a later resume point asks for the wider span even once
	/// it answered for the narrower one: a bounded range keeps the reader's end, not the
	/// backfill's earlier cap.
	#[kio::test]
	async fn a_widened_backfill_reaches_past_its_earlier_cap() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		let b = copy();
		// The latest group, past the range's end at 11, which it resolves without handing out.
		write(&a, 1001);
		routes.serve(a.consume());
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_ranges([track::Range {
				start: crate::track::Start::Time(0),
				end: Some(10),
			}]);
		let mut sub = logical
			.consume()
			.subscribe(subscription)
			.now_or_never()
			.unwrap()
			.unwrap();
		write(&a, 2);
		assert_eq!(drain(&mut sub, false), [2]);
		// B resumes at 2, so its backfill asks for the groups below it.
		routes.serve(b.consume());
		write(&b, 4);
		assert_eq!(drain(&mut sub, false), [4]);

		// C never answers, so B's copy keeps delivering uncapped.
		let c = track::Request::new(Arc::new(broadcast::Info::default()), "video");
		routes.serve(c.consume());
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());
		write(&b, 6);
		assert_eq!(drain(&mut sub, false), [6]);

		// B returns at 6, widening its backfill past 2.
		routes.serve(b.consume());
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());
		for key in [1, 3, 5] {
			write(&b, key);
		}
		let mut late = drain(&mut sub, false);
		late.sort_unstable();
		assert_eq!(late, [1, 3, 5]);
		drop(c);
	}

	/// A time start widened under the serving copy's resume point starts at the group holding
	/// that time, as a plain reader of the route would, not at a group key equal to it.
	#[kio::test]
	async fn a_time_start_under_the_resume_point_reads_the_group_holding_it() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = received_copy();
		let b = received_copy();
		for copy in [&a, &b] {
			write(copy, 6);
			write(copy, 8);
		}
		routes.serve(a.consume());
		let from = |time| {
			Subscription::default()
				.with_delay(Delay::default().with_max(HOUR))
				.with_ranges([track::Range {
					start: crate::track::Start::Time(time),
					end: None,
				}])
		};
		let mut sub = logical.consume().subscribe(from(8)).now_or_never().unwrap().unwrap();
		assert_eq!(drain(&mut sub, false), [8]);
		// B resumes past 8, holding 6 and 8, and its next group lets A go.
		routes.serve(b.consume());
		write(&b, 9);
		assert_eq!(drain(&mut sub, false), [9]);
		a.abort(Error::Cancel).unwrap();
		sub.update(from(7)).unwrap();
		assert_eq!(drain(&mut sub, false), [6]);
		assert_eq!(sub.resolved_range(0).map(|resolved| resolved.start), Some(6));
		write(&b, 10);
		assert_eq!(drain(&mut sub, false), [10]);
	}

	/// As [`a_time_start_under_the_resume_point_reads_the_group_holding_it`], with the group
	/// holding the time still to land when the backfill learns no group sits between it and
	/// the resume point: the backfill is kept for it, below where it was asked from.
	#[kio::test]
	async fn a_backfill_waits_for_the_group_holding_a_time_under_its_floor() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = received_copy();
		write(&a, 6);
		write(&a, 8);
		let b = received_copy();
		write(&b, 8);
		routes.serve(a.consume());
		let from = |time| {
			Subscription::default()
				.with_delay(Delay::default().with_max(HOUR))
				.with_ranges([track::Range {
					start: crate::track::Start::Time(time),
					end: None,
				}])
		};
		let mut sub = logical.consume().subscribe(from(8)).now_or_never().unwrap().unwrap();
		assert_eq!(drain(&mut sub, false), [8]);
		routes.serve(b.consume());
		write(&b, 9);
		assert_eq!(drain(&mut sub, false), [9]);
		a.abort(Error::Cancel).unwrap();
		sub.update(from(7)).unwrap();
		b.skip_groups(7..8);
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());
		write(&b, 10);
		assert_eq!(drain(&mut sub, false), [10]);
		write(&b, 6);
		assert_eq!(drain(&mut sub, false), [6]);
	}

	/// As [`a_time_start_under_the_resume_point_reads_the_group_holding_it`], for ranges
	/// widened while the serving copy has yet to answer, so its cache places nothing: the
	/// backfill asked from below its resume point places the start.
	#[test]
	#[cfg(feature = "async")]
	fn a_backfill_places_a_time_start_its_serving_copy_raised() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = received_copy();
		write(&a, 6);
		write(&a, 8);
		routes.serve(a.consume());
		let from = |time| {
			Subscription::default()
				.with_delay(Delay::default().with_max(HOUR))
				.with_ranges([track::Range {
					start: crate::track::Start::Time(time),
					end: None,
				}])
		};
		let mut sub = logical.consume().subscribe(from(8)).now_or_never().unwrap().unwrap();
		assert_eq!(drain(&mut sub, false), [8]);
		let b = track::Request::new(Arc::new(broadcast::Info::default()), "video");
		routes.serve(b.consume());
		// The read subscribes the replacement, from 8, which has not answered yet.
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());
		a.abort(Error::Cancel).unwrap();
		sub.update(from(7)).unwrap();
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());

		let b = b.accept(crate::track::Info::timed());
		b.received();
		for key in [6, 8, 9] {
			write(&b, key);
		}
		let mut read = drain(&mut sub, false);
		read.sort_unstable();
		assert_eq!(read, [6, 9]);
		assert_eq!(sub.resolved_range(0).map(|resolved| resolved.start), Some(6));
	}

	/// As [`a_backfill_places_a_time_start_its_serving_copy_raised`], beside a range still
	/// waiting on a group past the live edge: the raised start's backfill is asked without
	/// waiting for that range to place.
	#[test]
	#[cfg(feature = "async")]
	fn a_backfill_places_a_raised_start_beside_a_waiting_range() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = received_copy();
		write(&a, 6);
		write(&a, 8);
		routes.serve(a.consume());
		let range = |start, end: Option<u64>| track::Range {
			start: crate::track::Start::Time(start),
			end,
		};
		let subscription = |ranges: Vec<track::Range>| {
			Subscription::default()
				.with_delay(Delay::default().with_max(HOUR))
				.with_ranges(ranges)
		};
		let mut sub = logical
			.consume()
			.subscribe(subscription(vec![range(8, None)]))
			.now_or_never()
			.unwrap()
			.unwrap();
		assert_eq!(drain(&mut sub, false), [8]);
		let b = track::Request::new(Arc::new(broadcast::Info::default()), "video");
		routes.serve(b.consume());
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());
		a.abort(Error::Cancel).unwrap();
		sub.update(subscription(vec![range(7, Some(8)), range(20, None)]))
			.unwrap();
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());

		let b = b.accept(crate::track::Info::timed());
		b.received();
		for key in [6, 8] {
			write(&b, key);
		}
		assert_eq!(drain(&mut sub, false), [6]);
		assert_eq!(sub.resolved_range(0).map(|resolved| resolved.start), Some(6));
	}

	/// A reader of time 5 on, which read groups 5 to 8 from copy `a` before the switch to `b`,
	/// an empty received copy that resumes at 8: the routes, the logical track, `a`, `b`, and
	/// the reader.
	fn read_to_8() -> (
		Producer,
		track::Producer,
		track::Producer,
		track::Producer,
		track::Subscriber,
	) {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = received_copy();
		for key in 5..=8 {
			write(&a, key);
		}
		routes.serve(a.consume());
		let mut sub = logical
			.consume()
			.subscribe(from_times(&[5]))
			.now_or_never()
			.unwrap()
			.unwrap();
		let mut read = drain(&mut sub, false);
		read.sort_unstable();
		assert_eq!(read, [5, 6, 7, 8]);
		let b = received_copy();
		routes.serve(b.consume());
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());
		(routes, logical, a, b, sub)
	}

	/// A subscription of a range from each time, within an hour.
	fn from_times(times: &[u64]) -> Subscription {
		Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_ranges(times.iter().map(|time| track::Range {
				start: track::Start::Time(*time),
				end: None,
			}))
	}

	/// A raised time start whose groups from its time the reader already holds is placed at
	/// its time, with no backfill.
	#[test]
	fn a_raised_start_the_reader_holds_places_at_its_time() {
		let (routes, _logical, a, _b, mut sub) = read_to_8();
		a.abort(Error::Cancel).unwrap();
		sub.update(from_times(&[6])).unwrap();
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());
		assert_eq!(sub.resolved_range(0).map(|resolved| resolved.start), Some(6));
		assert_eq!(copies(&routes), 1, "nothing is backfilled");
	}

	/// As [`a_raised_start_the_reader_holds_places_at_its_time`], beside a range still waiting
	/// on its anchors: the raised start is placed only once every other range is.
	#[test]
	fn a_held_raised_start_waits_for_a_range_beside_it() {
		let (_routes, _logical, a, b, mut sub) = read_to_8();
		a.abort(Error::Cancel).unwrap();
		sub.update(from_times(&[6, 20])).unwrap();
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());
		assert_eq!(sub.resolved_range(0), None, "waits for the range beside it");
		write(&b, 20);
		assert_eq!(drain(&mut sub, false), [20]);
		assert_eq!(sub.resolved_range(0).map(|resolved| resolved.start), Some(6));
	}

	/// As [`a_backfill_places_a_time_start_its_serving_copy_raised`], for a range that ends
	/// past the resume point: the backfill, capped under it, places only the start, and the
	/// read ends at the range's own end.
	#[test]
	#[cfg(feature = "async")]
	fn a_backfill_places_a_raised_start_under_an_end_past_the_resume_point() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = received_copy();
		write(&a, 6);
		write(&a, 8);
		routes.serve(a.consume());
		let from = |time, end| {
			Subscription::default()
				.with_delay(Delay::default().with_max(HOUR))
				.with_ranges([track::Range {
					start: crate::track::Start::Time(time),
					end,
				}])
		};
		let mut sub = logical
			.consume()
			.subscribe(from(8, None))
			.now_or_never()
			.unwrap()
			.unwrap();
		assert_eq!(drain(&mut sub, false), [8]);
		let b = track::Request::new(Arc::new(broadcast::Info::default()), "video");
		routes.serve(b.consume());
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());
		a.abort(Error::Cancel).unwrap();
		sub.update(from(7, Some(9))).unwrap();
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());

		let b = b.accept(crate::track::Info::timed());
		b.received();
		b.anchored();
		for key in [6, 8, 9] {
			write(&b, key);
		}
		b.skip_groups(7..8);
		let mut read = drain(&mut sub, false);
		read.sort_unstable();
		assert_eq!(read, [6, 9]);
		let resolved = sub.resolved_range(0).expect("placed");
		assert_eq!((resolved.start, resolved.end), (6, Some(10)));
		assert!(matches!(
			sub.poll_recv_group(&kio::Waiter::noop()),
			Poll::Ready(Ok(None))
		));
	}

	/// As [`a_backfill_places_a_raised_start_under_an_end_past_the_resume_point`], with the
	/// replacement behind an inner front: the backfill through it places the start as a plain
	/// copy's would, so the read ends once the range is read.
	#[test]
	#[cfg(feature = "async")]
	fn a_raised_start_behind_an_inner_front_places_through_it() {
		let routes = Producer::new();
		let front = logical(&routes);
		let a = received_copy();
		write(&a, 6);
		write(&a, 8);
		routes.serve(a.consume());
		let from = |time, end| {
			Subscription::default()
				.with_delay(Delay::default().with_max(HOUR))
				.with_ranges([track::Range {
					start: crate::track::Start::Time(time),
					end,
				}])
		};
		let mut sub = front
			.consume()
			.subscribe(from(8, None))
			.now_or_never()
			.unwrap()
			.unwrap();
		assert_eq!(drain(&mut sub, false), [8]);
		let inner_routes = Producer::new();
		let inner = logical(&inner_routes);
		let b = track::Request::new(Arc::new(broadcast::Info::default()), "video");
		inner_routes.serve(b.consume());
		routes.serve(inner.consume());
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());
		a.abort(Error::Cancel).unwrap();
		sub.update(from(7, Some(7))).unwrap();
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());

		let b = b.accept(crate::track::Info::timed());
		b.received();
		b.anchored();
		let mut read = Vec::new();
		for key in [6, 7, 8, 9] {
			write(&b, key);
			read.extend(drain(&mut sub, false));
		}
		assert_eq!(read, [7]);
		let resolved = sub.resolved_range(0).expect("placed");
		assert_eq!((resolved.start, resolved.end), (7, Some(8)));
		assert!(matches!(
			sub.poll_recv_group(&kio::Waiter::noop()),
			Poll::Ready(Ok(None))
		));
	}

	/// A backfill through an inner front waits on a copy that front replaced, not only on its
	/// serving copy: the replaced copy still lands a late group under its cap that the serving
	/// copy, whose feed starts past it, never will.
	#[test]
	#[cfg(feature = "async")]
	fn a_backfill_through_an_inner_front_waits_on_a_copy_it_replaced() {
		let routes = Producer::new();
		let front = logical(&routes);
		let a = received_copy();
		write(&a, 6);
		write(&a, 8);
		routes.serve(a.consume());
		let mut sub = subscribe(&front, HOUR);
		assert_eq!(drain(&mut sub, false), [6, 8]);
		let inner_routes = Producer::new();
		let inner = logical(&inner_routes);
		let b = received_copy();
		write(&b, 6);
		write(&b, 8);
		inner_routes.serve(b.consume());
		routes.serve(inner.consume());
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());
		// The late group 7 is asked of the inner front.
		a.abort(Error::Cancel).unwrap();
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());

		// The inner front moves on to a copy whose feed starts past the gap.
		let mut c = received_copy();
		c.start_at(9).unwrap();
		write(&c, 9);
		inner_routes.serve(c.consume());
		assert_eq!(drain(&mut sub, false), [9]);

		write(&b, 7);
		assert_eq!(drain(&mut sub, false), [7]);
	}

	/// A backfill's group that a range already placed holds goes out though another range
	/// still waits on its anchors.
	#[test]
	#[cfg(feature = "async")]
	fn a_backfill_group_a_placed_range_holds_goes_out_beside_a_waiting_range() {
		let routes = Producer::new();
		let front = logical(&routes);
		let a = received_copy();
		write(&a, 6);
		write(&a, 8);
		routes.serve(a.consume());
		let subscription = |ranges: &[track::Range]| {
			Subscription::default()
				.with_delay(Delay::default().with_max(HOUR))
				.with_ranges(ranges.iter().copied())
		};
		let from = |time| track::Range {
			start: crate::track::Start::Time(time),
			end: None,
		};
		let mut sub = front
			.consume()
			.subscribe(subscription(&[from(8)]))
			.now_or_never()
			.unwrap()
			.unwrap();
		assert_eq!(drain(&mut sub, false), [8]);
		let b = track::Request::new(Arc::new(broadcast::Info::default()), "video");
		routes.serve(b.consume());
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());
		a.abort(Error::Cancel).unwrap();
		sub.update(subscription(&[from(7)])).unwrap();
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());
		let b = b.accept(crate::track::Info::timed());
		b.received();
		b.anchored();
		write(&b, 7);
		// A range whose start waits on a group still to land leaves the window pending.
		let waiting = track::Range {
			start: crate::track::Start::Time(9),
			end: None,
		};
		sub.update(subscription(&[from(7), waiting])).unwrap();
		assert_eq!(drain(&mut sub, false), [7]);
	}

	/// A copy updated while pending, then answering, hands its own resolution of the
	/// updated ranges to the copy replacing it, rather than leaving the replacement to
	/// resolve them from whatever it caches.
	#[test]
	#[cfg(feature = "async")]
	fn a_copy_updated_while_pending_passes_on_its_resolution() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(30));
		let mut zero = a.create_group(group::Info { start: 0 }).unwrap();
		zero.write_frame(ts(0), b"x".as_ref()).unwrap();
		assert_eq!(recv(&mut sub).start, 0);

		let b = track::Request::new(Arc::new(broadcast::Info::default()), "video");
		routes.serve(b.consume());
		// The read subscribes the replacement, which has not answered yet.
		assert!(sub.recv_group().now_or_never().is_none());
		let ranges = [track::Range {
			start: crate::track::Start::Time(10_000),
			end: None,
		}];
		sub.update(sub.subscription().with_ranges(ranges)).unwrap();
		assert!(sub.recv_group().now_or_never().is_none());

		let b = b.accept(crate::track::Info::timed());
		for key in 0..13 {
			let mut group = b.create_group(group::Info { start: key * 1000 }).unwrap();
			group.write_frame(ts(key * 1000), b"x".as_ref()).unwrap();
			group.finish().unwrap();
		}
		while let Some(Ok(Some(_))) = sub.recv_group().now_or_never() {}

		let c = copy();
		routes.serve(c.consume());
		assert!(sub.recv_group().now_or_never().is_none());
		let start = c.subscription().expect("subscribed").ranges[0].start;
		assert!(
			matches!(start, crate::track::Start::Time(group) if group >= 10_000),
			"the replacement asks from where b resolved the time: {start:?}"
		);
	}

	/// A copy still unanswered passes on where the reader's ranges resolved to the copy that
	/// replaces it, through a changed preference that keeps the ranges.
	#[test]
	#[cfg(feature = "async")]
	fn an_unanswered_copy_passes_on_its_resolution_through_an_update() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		for sequence in 0..=10 {
			write(&a, sequence);
		}
		routes.serve(a.consume());
		// Two behind the newest frame, at 10.
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_ranges([track::Range::from(track::Start::Live(Duration::from_millis(2)))]);
		let mut sub = logical
			.consume()
			.subscribe(subscription)
			.now_or_never()
			.unwrap()
			.unwrap();
		assert_eq!(recv(&mut sub).start, 8);

		let (b, _requests) = pending_copy();
		routes.serve(b.consume());
		sub.update(sub.subscription().with_priority(3)).unwrap();
		let c = copy();
		routes.serve(c.consume());
		// Resolved, then raised to the boundary.
		assert_eq!(
			c.subscription().expect("subscribed").ranges[0].start,
			crate::track::Start::Time(10)
		);
	}

	/// A live join stays where it was placed while a time end settles, though a later copy,
	/// asked before the reader read anything, resolves it at its own latest group.
	#[test]
	fn a_placed_start_stays_while_a_time_end_settles() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		for sequence in 0..=5 {
			write(&a, sequence);
		}
		routes.serve(a.consume());
		let range = track::Range {
			start: track::Start::Live(Duration::ZERO),
			end: Some(1_000_000),
		};
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_ranges([range]);
		let mut sub = logical
			.consume()
			.subscribe(subscription)
			.now_or_never()
			.unwrap()
			.unwrap();
		let noop = kio::Waiter::noop();
		assert!(sub.poll_finished(&noop).is_pending());
		assert_eq!(sub.floor(), Some(5));

		let b = copy();
		for sequence in 0..=9 {
			write(&b, sequence);
		}
		routes.serve(b.consume());
		assert!(sub.poll_finished(&noop).is_pending());
		assert_eq!(sub.floor(), Some(5));
	}

	/// A live join a front's reader asks for resolves on the copy serving now, where that
	/// copy's upstream answered it.
	#[test]
	#[cfg(feature = "async")]
	fn the_serving_copy_places_a_live_join() {
		let routes = Producer::new();
		let front = logical(&routes);
		let a = copy();
		a.received();
		routes.serve(a.consume());
		let mut sub = subscribe(&front, Duration::from_secs(30));
		let mut zero = a.create_group(group::Info { start: 0 }).unwrap();
		zero.write_frame(ts(0), b"x".as_ref()).unwrap();
		assert_eq!(recv(&mut sub).start, 0);

		let b = copy();
		b.received();
		b.anchored();
		b.answers_joins();
		routes.serve(b.consume());
		let joined = sub
			.subscription()
			.with_ranges([track::Range::from(track::Start::Live(Duration::ZERO))])
			.with_delay(Delay::default().with_max(Duration::from_secs(30)));
		sub.update(joined).unwrap();
		assert!(sub.recv_group().now_or_never().is_none());
		let mut group = b.create_group(group::Info { start: 1000 }).unwrap();
		group.write_frame(ts(1000), b"x".as_ref()).unwrap();
		group.finish().unwrap();
		assert!(
			sub.recv_group().now_or_never().is_none(),
			"only its upstream's answer places it"
		);
		b.answer_join(Duration::ZERO, None, Some(1000), Duration::MAX);
		let group = sub.recv_group().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(group.start, 1000);
	}

	/// A changed preference reaches the serving copy without the reader polling again,
	/// through the subscriber or its control: a reader demoting a track it stopped reading
	/// would otherwise keep its old priority upstream.
	#[test]
	#[cfg(feature = "async")]
	fn an_update_reaches_the_copy_without_a_read() {
		let routes = Producer::new();
		let front = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&front, Duration::from_secs(30));
		assert_eq!(a.subscription().unwrap().priority, 0);

		sub.update(sub.subscription().with_priority(-2)).unwrap();
		assert_eq!(a.subscription().unwrap().priority, -2);
		sub.control().update(sub.subscription().with_priority(3)).unwrap();
		assert_eq!(a.subscription().unwrap().priority, 3);
	}

	/// Capping a replaced copy past a time end keeps the end: a group below the cap that
	/// starts past the end stays out.
	#[test]
	#[cfg(feature = "async")]
	fn capping_a_replaced_copy_keeps_its_time_end() {
		let routes = Producer::new();
		let a = copy();
		routes.serve(a.consume());
		let mut first = a.create_group(group::Info { start: 3000 }).unwrap();
		first.write_frame(ts(3000), b"x".as_ref()).unwrap();
		first.finish().unwrap();
		let range = track::Range {
			start: crate::track::Start::Time(3000),
			end: Some(3500),
		};
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(Duration::from_secs(30)))
			.with_ranges([range]);
		let sub = routes.consume().subscribe(subscription.into());
		let mut reader = sub.reader.lock().unwrap();
		let (groups, mirrored) = (reader.groups, reader.mirrored.clone());
		let old = &mut reader.copies[0];
		old.replaced = true;
		old.until = Some(Some(3800));
		old.update(&mirrored, &Change::Scalars);
		assert_eq!(
			old.wire_all(&mirrored).ranges,
			vec![range],
			"the time end is inside the cap"
		);

		for key in [3600, 3700] {
			write(&a, key);
		}
		let Poll::Ready(Ok(copy)) = old.poll_ready(groups, &kio::Waiter::noop()) else {
			panic!("the copy answered");
		};
		let mut read = Vec::new();
		while let Some(Ok(Some(group))) = copy.recv_group().now_or_never() {
			read.push(group.start);
		}
		assert_eq!(read, vec![3000]);
	}

	/// A replaced copy capped at or below where a range starts says nothing of that range, and
	/// one capped at or below where it ends says nothing of its end; a cap past either lets
	/// the copy place it. A window only a capped copy can place waits instead of taking a
	/// range the cap cut short.
	#[test]
	fn a_capped_copy_places_only_what_its_cap_leaves_whole() {
		let end = Some(3500);
		// Groups 3000 and 3600: the range ends at 3501.
		for (cap, placed) in [(2999, None), (3000, None), (3001, Some(3501))] {
			let routes = Producer::new();
			let a = copy();
			write(&a, 3000);
			write(&a, 3600);
			routes.serve(a.consume());
			let range = track::Range {
				start: crate::track::Start::Time(3000),
				end,
			};
			let subscription = Subscription::default()
				.with_delay(Delay::default().with_max(HOUR))
				.with_ranges([range]);
			let sub = routes.consume().subscribe(subscription.into());
			let mut reader = sub.reader.lock().unwrap();
			let (groups, mirrored) = (reader.groups, reader.mirrored.clone());
			let old = &mut reader.copies[0];
			old.replaced = true;
			assert!(old.poll_ready(groups, &kio::Waiter::noop()).is_ready());
			old.cap(cap - 1, None, &mirrored);
			let window = reader.window();
			assert_eq!(window.is_settled(), placed.is_some(), "{end:?} capped at {cap}");
			assert_eq!(window.end(), placed, "{end:?} capped at {cap}");
		}
	}

	/// Bounding a replaced copy keeps its live join where the copy resolved it, though its
	/// cache moved on since.
	#[test]
	fn capping_a_replaced_copy_keeps_where_its_live_join_resolved() {
		let routes = Producer::new();
		let a = copy();
		for sequence in 0..=10 {
			write(&a, sequence);
		}
		routes.serve(a.consume());
		// Two behind the newest frame, at 10: group 8.
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_ranges([track::Range::from(track::Start::Live(Duration::from_millis(2)))]);
		let sub = routes.consume().subscribe(subscription.into());
		let mut reader = sub.reader.lock().unwrap();
		let (groups, mirrored) = (reader.groups, reader.mirrored.clone());
		let old = &mut reader.copies[0];
		assert!(old.poll_ready(groups, &kio::Waiter::noop()).is_ready());
		for sequence in 11..=20 {
			write(&a, sequence);
		}
		old.replaced = true;
		old.until = Some(Some(12));
		assert_eq!(
			old.wire_all(&mirrored).ranges,
			[track::Range {
				start: crate::track::Start::Time(8),
				end: Some(12),
			}]
		);
	}

	#[test]
	fn a_replaced_copy_at_the_last_key_has_no_overflow_cap() {
		for (last, end) in [(Some(u64::MAX), None), (None, Some(0))] {
			let routes = Producer::new();
			let a = copy();
			routes.serve(a.consume());
			let sub = routes.consume().subscribe(Subscription::default().into());
			let mut reader = sub.reader.lock().unwrap();
			let old = &mut reader.copies[0];
			old.until = Some(last);
			assert_eq!(old.wire_all(&Mirrored::of(&Subscription::default())).end(), end);
			// A reader's narrower bound survives an unbounded replacement cap.
			let requested = Mirrored::of(&Subscription::default().with_end(7));
			assert_eq!(old.wire_all(&requested).end(), Some(end.unwrap_or(7)));
		}
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_replaced_route_yields_to_the_serving_copy() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(10));
		let mut first = a.create_group(group::Info { start: 0 }).unwrap();
		first.write_frame(ts(0), b"a0".as_ref()).unwrap();
		assert_eq!(recv(&mut sub).start, 0);

		let b = copy();
		routes.serve(b.consume());
		// Both routes go on to group 1; only the serving one's copy counts.
		for (track, payload) in [(&a, b"a1"), (&b, b"b1")] {
			let mut group = track.create_group(group::Info { start: 1 }).unwrap();
			group.write_frame(ts(1), payload.as_ref()).unwrap();
			group.finish().unwrap();
		}
		let mut next = recv(&mut sub);
		assert_eq!(next.start, 1);
		assert_eq!(read(&mut next), Some(b"b1".to_vec()));
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_group_no_route_continues_is_given_up_once_stale() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(2));
		let mut open = a.create_group(group::Info { start: 0 }).unwrap();
		open.write_frame(ts(0), b"a".as_ref()).unwrap();
		let mut reading = recv(&mut sub);
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));

		// The replacement starts past the group and cannot fetch it; the replaced route
		// stays up but quiet.
		let b = copy();
		routes.serve(b.consume());
		for key in 1..=2 {
			let mut group = b.create_group(group::Info { start: key * 1000 }).unwrap();
			group.write_frame(ts(key * 1000), b"b".as_ref()).unwrap();
			group.finish().unwrap();
		}
		assert!(reading.read_frame().now_or_never().is_none(), "not stale yet");

		let mut group = b.create_group(group::Info { start: 3000 }).unwrap();
		group.write_frame(ts(3000), b"b".as_ref()).unwrap();
		assert!(matches!(reading.read_frame().now_or_never(), Some(Err(Error::Old))));
	}

	/// An untimed track has no media time to convict a group no route continues, so only
	/// the wall clock gives it up. Nothing is written after the replacement's successor
	/// arrives, so only the deadline wake can release the reader: against a cold route, and
	/// against one whose copy already caches groups past the held one, so a cursor that
	/// skips ahead cannot hide the stall.
	#[kio::test]
	#[cfg(feature = "async")]
	async fn an_untimed_group_no_route_continues_is_given_up_by_wall_clock() {
		// No publisher max age, so only the subscription's budget gives the group up.
		let info = || crate::track::Info::default().with_max_age(None);
		let untimed = || track::Producer::new(Arc::new(broadcast::Info::default()), "video", info());
		for warm in [false, true] {
			let routes = Producer::new();
			let logical = track::Request::new(Arc::new(broadcast::Info::default()), "video")
				.routes(routes.consume())
				.accept(info());
			let a = untimed();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, Duration::from_secs(1));
			let mut open = a.create_group(group::Info { start: 0 }).unwrap();
			open.write_frame(crate::Timestamp::unscaled(open.start), b"a".as_ref())
				.unwrap();
			let mut reading = recv(&mut sub);
			assert_eq!(read(&mut reading), Some(b"a".to_vec()));

			let b = untimed();
			let pool = b.broadcast().pool.clone();
			// A driver collects the replacement's pool, so its wall deadlines fire.
			pool.step(Duration::ZERO);
			let write = |key| {
				let mut group = b.create_group(group::Info { start: key }).unwrap();
				group
					.write_frame(crate::Timestamp::unscaled(group.start), b"b".as_ref())
					.unwrap();
				group.finish().unwrap();
			};
			if warm {
				(1..=3).for_each(write);
			}
			routes.serve(b.consume());
			if !warm {
				write(1);
			}

			let wake = Arc::new(Wake::default());
			let waiter = kio::Waiter::new(wake.clone().into());
			assert!(
				reading.poll_read_frame(&waiter).is_pending(),
				"warm {warm}: not stale yet"
			);
			wake.0.store(false, std::sync::atomic::Ordering::Relaxed);
			assert!(
				reading.poll_read_frame(&waiter).is_pending(),
				"warm {warm}: not stale yet"
			);

			pool.step(Duration::from_secs(1));
			assert!(
				wake.0.load(std::sync::atomic::Ordering::Relaxed),
				"warm {warm}: the deadline never woke the reader"
			);
			let result = reading.poll_read_frame(&waiter);
			assert!(
				matches!(result, Poll::Ready(Err(Error::Old))),
				"warm {warm}: {result:?}"
			);
		}
	}

	/// The demand on a new copy keeps each reader's live join where its copy resolved it,
	/// raised to the boundary a switch splits at. Ranges the reader changed since its
	/// copies last heard resolve against the newest
	/// answered copy's cache now, and an unanswered copy's resolution is for the ranges it
	/// was asked for alone.
	#[test]
	fn the_fronts_subscription_keeps_where_a_live_join_resolved() {
		let routes = Producer::new();
		let a = copy();
		for sequence in 0..=10 {
			write(&a, sequence);
		}
		routes.serve(a.consume());
		// A live join `back` behind the newest frame, a range of its own for each.
		let ranges = |back| {
			Subscription::default()
				.with_delay(Delay::default().with_max(HOUR))
				.with_ranges([track::Range {
					start: track::Start::Live(Duration::from_millis(back)),
					end: Some(1_000_000 + back),
				}])
		};
		let preferences = track::Preferences::from(ranges(5));
		let mut sub = routes.consume().subscribe(preferences.clone());
		let mut delivered = Delivered::default();
		let Poll::Ready(Ok(Some(candidate))) = sub.poll_peek_group(false, false, &mut delivered, &kio::Waiter::noop())
		else {
			panic!("a group to reserve")
		};
		assert_eq!(sub.take(candidate).unwrap().start, 5);
		for sequence in 11..=20 {
			write(&a, sequence);
		}
		let start = || routes.subscription(Subscription::default(), 1).ranges[0].start;
		// Resolved at 5, raised to 20, the largest group held, which the copy drains to.
		assert_eq!(start(), crate::track::Start::Time(20));

		// Changed, but not yet mirrored into the copies.
		preferences.update(ranges(0)).unwrap();
		assert_eq!(start(), crate::track::Start::Time(20));

		sub.mirror().sync();
		let (b, _requests) = pending_copy();
		routes.serve(b.consume());
		assert_eq!(start(), crate::track::Start::Time(20));
		preferences.update(ranges(3)).unwrap();
		assert_eq!(
			start(),
			track::Start::Live(Duration::from_millis(3)),
			"left for the copy to place"
		);
	}

	/// Every live reader counts toward the demand on a new copy however many readers came
	/// and went, and the readers list tracks the live count rather than every reader there
	/// ever was.
	#[test]
	fn the_readers_list_tracks_the_live_readers() {
		let routes = Producer::new();
		let front = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut live: Vec<_> = (0..5).map(|_| subscribe(&front, HOUR)).collect();
		write(&a, 5);
		for sub in &mut live {
			assert_eq!(recv(sub).start, 5);
		}
		let demand = front.subscription().unwrap();
		assert_eq!(routes.subscription(demand.clone(), 5).start(), Some(5));
		for _ in 0..64 {
			drop(subscribe(&front, HOUR));
		}
		assert_eq!(routes.subscription(demand, 5).start(), Some(5));
		// Swept each time it fills, so it grows by doubling over the most readers alive at
		// once: here 6.
		let len = routes.readers.lock().unwrap().len();
		assert!(len <= 4 * 6, "{len} entries for 5 live readers");
	}

	#[test]
	fn the_fronts_subscription_resumes_from_each_readers_floor() {
		let routes = Producer::new();
		let front = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut first = subscribe(&front, HOUR);
		let mut second = subscribe(&front, HOUR);
		write(&a, 5);
		assert_eq!(recv(&mut first).start, 5);
		let demand = front.subscription().unwrap();
		// One reader has not read anything yet, so it still needs the oldest range.
		assert_eq!(routes.subscription(demand.clone(), 2).start(), None);
		assert_eq!(recv(&mut second).start, 5);
		assert_eq!(routes.subscription(demand.clone(), 2).start(), Some(5));
		write(&a, 10);
		assert_eq!(recv(&mut first).start, 10);
		// From the boundary, the largest group held: what the second reader lacks below it is
		// the capped copy's to drain, or asked of the new route once a switch finds it missing.
		assert_eq!(routes.subscription(demand.clone(), 2).start(), Some(10));
		let pending = front
			.consume()
			.subscribe(Subscription::default().with_delay(Delay::default().with_max(HOUR)));
		assert_eq!(routes.subscription(front.subscription().unwrap(), 3).start(), None);
		drop(pending);
		drop(second);
		assert_eq!(routes.subscription(demand, 1).start(), Some(10));
	}

	/// A front resumes a new copy at the boundary, past the largest group held, whichever
	/// reader is behind: what that reader lacks below it is the capped copy's to drain.
	#[test]
	fn the_fronts_subscription_resumes_at_the_boundary_whichever_reader_is_behind() {
		let routes = Producer::new();
		let front = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut first = subscribe(&front, HOUR);
		let mut second = subscribe(&front, HOUR);
		let resume = || routes.subscription(front.subscription().unwrap(), 2).resume;
		write(&a, 5);
		assert_eq!(recv(&mut first).start, 5);
		assert_eq!(recv(&mut second).start, 5);
		write(&a, 10);
		assert_eq!(recv(&mut second).start, 10);
		assert_eq!(resume(), Some(Position::new(10, 1)), "the first reader is behind");
		assert_eq!(recv(&mut first).start, 10);
		write(&a, 15);
		assert_eq!(recv(&mut first).start, 15);
		assert_eq!(resume(), Some(Position::new(15, 1)), "the second reader is behind");
	}

	#[derive(Default)]
	struct Wake(std::sync::atomic::AtomicBool);

	impl std::task::Wake for Wake {
		fn wake(self: Arc<Self>) {
			self.0.store(true, std::sync::atomic::Ordering::Relaxed);
		}
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_parked_group_wakes_when_the_route_changes() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(10));
		let _open = a.create_group(group::Info { start: 0 }).unwrap();
		let mut reading = recv(&mut sub);
		let wake = Arc::new(Wake::default());
		let waiter = kio::Waiter::new(wake.clone().into());
		assert!(reading.poll_read_frame(&waiter).is_pending());
		let b = copy();
		routes.serve(b.consume());
		assert!(wake.0.load(std::sync::atomic::Ordering::Relaxed));
	}

	/// A reader holding group 2, which its quiet route never finishes, after the front
	/// switched to `copy`, a replacement that starts past it.
	#[cfg(feature = "async")]
	struct Held {
		group: group::Consumer,
		copy: track::Producer,
		/// The replacement's groups, by key.
		groups: std::collections::BTreeMap<u64, group::Producer>,
		wake: Arc<Wake>,
		waiter: kio::Waiter,
		_route: (
			Producer,
			track::Producer,
			track::Producer,
			group::Producer,
			track::Subscriber,
		),
	}

	#[cfg(feature = "async")]
	impl Held {
		/// `groups` are the replacement's `(key, first timestamp)`, written before the
		/// switch is noticed, so its subscription has passed the held group.
		#[cfg(feature = "async")]
		fn new(groups: &[(u64, Option<u64>)]) -> Self {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, Duration::from_millis(100));
			for (key, ms) in [(0, 0), (1, 1000)] {
				let mut group = a.create_group(group::Info { start: key }).unwrap();
				group.write_frame(ts(ms), b"x".as_ref()).unwrap();
				group.finish().unwrap();
			}
			assert_eq!(recv(&mut sub).start, 0);
			assert_eq!(recv(&mut sub).start, 1);
			let mut open = a.create_group(group::Info { start: 2 }).unwrap();
			open.write_frame(ts(2000), b"held".as_ref()).unwrap();
			let mut group = recv(&mut sub);
			assert_eq!(read(&mut group), Some(b"held".to_vec()));

			// A lite-05/06 route's copy, whose timestamps are never checked for a rewind.
			let b = track::Producer::new(
				Arc::new(broadcast::Info::default()),
				"video",
				crate::track::Info {
					unchecked: true,
					..crate::track::Info::timed()
				},
			);
			routes.serve(b.consume());
			let groups = groups
				.iter()
				.map(|&(key, ms)| {
					let mut group = b.create_group(group::Info { start: key }).unwrap();
					if let Some(ms) = ms {
						group.write_frame(ts(ms), b"b".as_ref()).unwrap();
					}
					(key, group)
				})
				.collect();

			let wake = Arc::new(Wake::default());
			let waiter = kio::Waiter::new(wake.clone().into());
			let mut held = Self {
				group,
				copy: b,
				groups,
				wake,
				waiter,
				_route: (routes, logical, a, open, sub),
			};
			assert!(!held.is_old(), "not stale yet");
			held
		}

		/// Poll the held group, forgetting earlier wakes so only a later one shows.
		fn is_old(&mut self) -> bool {
			self.wake.0.store(false, std::sync::atomic::Ordering::Relaxed);
			match self.group.poll_read_frame(&self.waiter) {
				Poll::Ready(Err(Error::Old)) => true,
				Poll::Pending => false,
				other => panic!("unexpected read: {other:?}"),
			}
		}

		/// Re-poll after the first change to the replacement. The refused recovery fetch
		/// parked on the replacement's track state once, so that change wakes the group
		/// whatever it is; from here on only the expiry index can.
		fn settle(&mut self) {
			assert!(self.woken());
			assert!(!self.is_old());
		}

		fn woken(&self) -> bool {
			self.wake.0.load(std::sync::atomic::Ordering::Relaxed)
		}

		fn group(&self, key: u64) -> group::Producer {
			self.copy.create_group(group::Info { start: key }).unwrap()
		}
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_group_no_route_continues_wakes_when_its_successor_is_stamped() {
		let mut held = Held::new(&[(3, None), (4, Some(4000))]);
		// Group 2 ends where group 3 begins, so this is when it is known to be 1s behind.
		held.groups
			.get_mut(&3)
			.unwrap()
			.write_frame(ts(3000), b"successor".as_ref())
			.unwrap();
		assert!(held.woken(), "the successor's first timestamp wakes the held group");
		assert!(held.is_old());
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_group_no_route_continues_wakes_when_its_successor_aborts() {
		// A frameless successor bounds nothing, so it takes one that started near the edge.
		let mut held = Held::new(&[(3, Some(4950)), (4, Some(4000)), (5, Some(5000))]);
		// Group 4 succeeds it instead, a full second behind the edge.
		held.groups.remove(&3).unwrap().abort(Error::Dropped).unwrap();
		assert!(held.woken(), "the successor's abort wakes the held group");
		assert!(held.is_old());
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_group_no_route_continues_wakes_when_the_edge_passes_it() {
		let mut held = Held::new(&[(3, Some(3000))]);
		let mut newer = held.group(4);
		held.settle();
		newer.write_frame(ts(3050), b"b".as_ref()).unwrap();
		assert!(!held.woken(), "the edge is still within the budget");
		newer.write_frame(ts(3100), b"b".as_ref()).unwrap();
		assert!(held.woken(), "the edge crossing the budget wakes the held group");
		assert!(held.is_old());
	}

	/// The held group sits below everything the replacement holds, so a landing at the
	/// top must not forget it before an older group lands as its new successor.
	#[test]
	#[cfg(feature = "async")]
	fn a_group_no_route_continues_wakes_when_an_older_successor_lands() {
		let mut held = Held::new(&[(4, Some(4000))]);
		let mut edge = held.group(5);
		edge.write_frame(ts(4050), b"b".as_ref()).unwrap();
		held.settle();
		let _newer = held.group(6);
		assert!(!held.woken(), "a landing above the successor changes nothing");

		let mut successor = held.group(3);
		assert!(held.woken(), "a group landing below the successor wakes the held group");
		assert!(!held.is_old(), "its successor has no timestamp yet");
		successor.write_frame(ts(3000), b"b".as_ref()).unwrap();
		assert!(held.woken());
		assert!(held.is_old());
	}

	/// The replacement passed the held group, so it looked lost, but its subscription
	/// can still deliver it out of order: that landing is the continuation.
	#[test]
	#[cfg(feature = "async")]
	fn a_group_no_route_continues_wakes_when_it_lands_late() {
		let mut held = Held::new(&[(3, Some(3000))]);
		let _newer = held.group(4);
		held.settle();
		let _late = held.group(2);
		assert!(held.woken(), "the held group's own landing wakes it");
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_half_read_frame_waits_for_the_replacement_header() {
		for arrives in [true, false] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, Duration::from_secs(10));
			let open = a.create_group(group::Info { start: 0 }).unwrap();
			let mut frame = open
				.create_frame(crate::frame::Info {
					size: 4,
					timestamp: ts(0),
				})
				.unwrap();
			frame.write(b"ab".as_ref()).unwrap();
			let mut reading = recv(&mut sub);
			let mut chunks = reading.next_frame().now_or_never().unwrap().unwrap().unwrap();
			assert_eq!(
				&chunks.read_chunk().now_or_never().unwrap().unwrap().unwrap()[..],
				b"ab"
			);

			let b = copy();
			let mut replacement = b.create_group(group::Info { start: 0 }).unwrap();
			routes.serve(b.consume());
			assert!(chunks.read_chunk().now_or_never().is_none());
			if !arrives {
				for key in 1..=4 {
					let mut group = b.create_group(group::Info { start: key }).unwrap();
					group.write_frame(ts(key * 10_000), b"new".as_ref()).unwrap();
					group.finish().unwrap();
				}
				assert!(matches!(chunks.read_chunk().now_or_never(), Some(Err(Error::Old))));
				continue;
			}
			replacement.write_frame(ts(0), b"abcd".as_ref()).unwrap();
			assert_eq!(
				&chunks
					.read_chunk()
					.now_or_never()
					.expect("replacement header arrived")
					.unwrap()
					.unwrap()[..],
				b"cd"
			);
		}
	}

	#[test]
	#[cfg(feature = "async")]
	fn recovered_groups_and_frames_expire_against_the_new_route() {
		for partial in [false, true] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, Duration::from_secs(2));
			let open = a.create_group(group::Info { start: 0 }).unwrap();
			let mut frame = open
				.create_frame(crate::frame::Info {
					size: 4,
					timestamp: ts(0),
				})
				.unwrap();
			frame.write(b"a".as_ref()).unwrap();
			let mut reading = recv(&mut sub);
			let mut chunks = reading.next_frame().now_or_never().unwrap().unwrap().unwrap();
			assert!(chunks.read_chunk().now_or_never().unwrap().is_ok());

			let b = copy();
			let replacement = b.create_group(group::Info { start: 0 }).unwrap();
			let mut replacement_frame = replacement
				.create_frame(crate::frame::Info {
					size: 4,
					timestamp: ts(0),
				})
				.unwrap();
			replacement_frame.write(b"ab".as_ref()).unwrap();
			routes.serve(b.consume());
			assert_eq!(&chunks.read_chunk().now_or_never().unwrap().unwrap().unwrap()[..], b"b");
			// Transfer the group cursor too, while its next frame is still pending.
			assert!(reading.read_frame().now_or_never().is_none());
			for key in 1..=4 {
				let mut next = b.create_group(group::Info { start: key }).unwrap();
				next.write_frame(ts(key * 1000), b"x".as_ref()).unwrap();
				next.finish().unwrap();
			}
			if partial {
				assert!(matches!(chunks.read_chunk().now_or_never(), Some(Err(Error::Old))));
			} else {
				assert!(matches!(reading.read_frame().now_or_never(), Some(Err(Error::Old))));
			}
		}
	}

	/// A read that moves a group onto a new route judges it within the reader's own window:
	/// newer groups that route holds for others never expire it, and groups in the window do,
	/// even below where the new route's subscription starts.
	#[test]
	#[cfg(feature = "async")]
	fn a_recovered_group_expires_only_within_the_readers_window() {
		// The reader's groups, the group past them the old route holds, the groups the new route
		// holds past the one read, and whether they expire it.
		let cases = [
			(None, None, 1..5, true),
			(Some(0..1), None, 1..5, false),
			// The old route's group 4 puts the boundary past the window, so the new route's own
			// subscription asks for nothing: its groups 1 to 3 are the reader's all the same.
			(Some(0..4), Some(4), 1..4, true),
		];
		for (ranges, held, newer, expires) in cases {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let mut subscription =
				Subscription::default().with_delay(Delay::default().with_max(Duration::from_secs(2)));
			if let Some(ranges) = ranges.clone() {
				subscription = subscription.with_ranges([track::Range::groups(ranges)]);
			}
			let mut sub = logical
				.consume()
				.subscribe(subscription)
				.now_or_never()
				.unwrap()
				.unwrap();
			let mut open = a.create_group(group::Info { start: 0 }).unwrap();
			open.write_frame(ts(0), b"a".as_ref()).unwrap();
			let mut reading = recv(&mut sub);
			assert_eq!(read(&mut reading), Some(b"a".to_vec()));
			assert!(reading.read_frame().now_or_never().is_none());
			if let Some(key) = held {
				write(&a, key);
			}

			let b = copy();
			let mut replacement = b.create_group(group::Info { start: 0 }).unwrap();
			replacement.write_frame(ts(0), b"a".as_ref()).unwrap();
			routes.serve(b.consume());
			// The stalled read moves onto the new route's copy.
			assert!(reading.read_frame().now_or_never().is_none());
			for key in newer {
				let mut next = b.create_group(group::Info { start: key }).unwrap();
				next.write_frame(ts(key * 1000), b"x".as_ref()).unwrap();
				next.finish().unwrap();
			}
			let read = reading.read_frame().now_or_never();
			match expires {
				true => assert!(matches!(read, Some(Err(Error::Old))), "ranges={ranges:?}"),
				false => assert!(read.is_none(), "ranges={ranges:?}"),
			}
		}
	}

	/// A replaced copy that fails while it still owes a group hands it to the serving route in
	/// the same poll, and the poll returns, whether or not that route failed too: a backfill
	/// reads the serving copy's own track, which still holds the group.
	#[test]
	fn a_failed_replaced_copy_backfills_once() {
		for serving_fails in [false, true] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, HOUR);
			write(&a, 0);
			assert_eq!(drain(&mut sub, false), [0]);
			// Held under the cap, so the switch leaves both to the replaced copy.
			let mut open = a.create_group(group::Info { start: 1 }).unwrap();
			open.write_frame(ts(1), b"x".as_ref()).unwrap();
			write(&a, 2);
			let b = copy();
			routes.serve(b.consume());
			write(&b, 1);
			if serving_fails {
				b.clone()
					.abort(Error::Stream(StreamError::Session(SessionError::Cancel)))
					.unwrap();
			}
			// The old route fails before group 1 went out: the serving route owes it.
			open.abort(Error::Stream(StreamError::Session(SessionError::Cancel)))
				.unwrap();
			a.abort(Error::Stream(StreamError::Session(SessionError::Cancel)))
				.unwrap();
			let mut got = drain(&mut sub, false);
			got.sort_unstable();
			assert_eq!(got, [1, 2], "serving_fails={serving_fails}");
		}
	}

	/// A group a replaced copy got past its cap is no group it drains: a later switch asks the
	/// next route for it, though that copy stays for a group still read from it.
	#[test]
	fn a_group_past_a_replaced_copys_cap_is_backfilled() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = received_copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		for key in 0..=4 {
			write(&a, key);
		}
		assert_eq!(drain(&mut sub, false), [0, 1, 2, 3, 4]);
		let mut open = a.create_group(group::Info { start: 5 }).unwrap();
		open.write_frame(ts(5), b"x".as_ref()).unwrap();
		let reading = recv(&mut sub);
		assert_eq!(reading.start, 5);
		let b = received_copy();
		routes.serve(b.consume());
		write(&b, 7);
		assert_eq!(drain(&mut sub, false), [7]);
		// Past the cap at 5, so a never hands it out.
		write(&a, 6);
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());
		let c = received_copy();
		routes.serve(c.consume());
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());
		write(&c, 6);
		assert_eq!(drain(&mut sub, false), [6]);
		drop(reading);
	}

	/// A bounded reader of keys up to `end`, with an hour's budget.
	fn subscribe_until(logical: &track::Producer, end: u64) -> track::Subscriber {
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_ranges([track::Range {
				start: crate::track::Start::Time(0),
				end: Some(end),
			}]);
		logical
			.consume()
			.subscribe(subscription)
			.now_or_never()
			.unwrap()
			.unwrap()
	}

	/// A bounded read waits for a backfill of a group a replaced copy holds only past its
	/// cap, which that copy never hands out.
	#[test]
	fn a_bounded_read_waits_for_a_group_held_past_a_replaced_copys_cap() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = received_copy();
		routes.serve(a.consume());
		let mut sub = subscribe_until(&logical, 7);
		for key in 0..=4 {
			write(&a, key);
		}
		assert_eq!(drain(&mut sub, false), [0, 1, 2, 3, 4]);
		let mut open = a.create_group(group::Info { start: 5 }).unwrap();
		open.write_frame(ts(5), b"x".as_ref()).unwrap();
		let reading = recv(&mut sub);
		let b = received_copy();
		routes.serve(b.consume());
		write(&b, 7);
		assert_eq!(drain(&mut sub, false), [7]);
		// Past the cap at 5, so a never hands it out, and b's route is gone.
		write(&a, 6);
		b.abort(Error::Stream(StreamError::Session(SessionError::Cancel)))
			.unwrap();
		let mut c = received_copy();
		routes.serve(c.consume());
		// Its live feed starts at the resume point, so 6 comes only as a fill.
		c.start_at(7).unwrap();
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());
		write(&c, 6);
		assert_eq!(drain(&mut sub, false), [6]);
		drop(reading);
	}

	/// A bounded read waits for a backfill of a group a replaced copy gave up and then ran out
	/// past: that copy delivers nothing more, so only the backfill may.
	#[test]
	fn a_bounded_read_waits_for_a_group_a_drained_replaced_copy_lacks() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = received_copy();
		routes.serve(a.consume());
		let mut sub = subscribe_until(&logical, 5);
		for key in [0, 1, 2, 4] {
			write(&a, key);
		}
		let mut got = drain(&mut sub, false);
		got.sort_unstable();
		assert_eq!(got, [0, 1, 2, 4]);
		let mut b = received_copy();
		routes.serve(b.consume());
		// Its live feed starts at the resume point, so 3 comes only as a fill.
		b.start_at(4).unwrap();
		write(&b, 5);
		assert_eq!(drain(&mut sub, false), [5]);
		// The old route gives 3 up and finishes, so it ran out under its cap without it.
		a.drop_groups(3..4, StreamError::NotFound);
		let _finished = a.finish().unwrap();
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());
		write(&b, 3);
		assert_eq!(drain(&mut sub, false), [3]);
	}

	/// A group one route refused to backfill is still asked of the next route, though the
	/// reader's preferences never changed.
	#[test]
	fn a_group_one_route_refused_is_backfilled_by_the_next() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = received_copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, HOUR);
		for key in 0..=4 {
			write(&a, key);
		}
		assert_eq!(drain(&mut sub, false), [0, 1, 2, 3, 4]);
		let mut open = a.create_group(group::Info { start: 5 }).unwrap();
		open.write_frame(ts(5), b"x".as_ref()).unwrap();
		let reading = recv(&mut sub);
		let b = received_copy();
		routes.serve(b.consume());
		write(&b, 7);
		assert_eq!(drain(&mut sub, false), [7]);
		write(&a, 6);
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());
		// c's backfill of group 6 fails with its route.
		let c = received_copy();
		routes.serve(c.consume());
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());
		c.abort(Error::Cancel).unwrap();
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());
		let d = received_copy();
		routes.serve(d.consume());
		write(&d, 6);
		assert_eq!(drain(&mut sub, false), [6]);
		drop(reading);
	}

	/// A front asks its next route from the lowest boundary among its readers, though only one
	/// still keeps a replaced copy holding a group past that copy's cap.
	#[test]
	fn the_fronts_subscription_resumes_at_the_lowest_boundary_when_one_reader_keeps_a_replaced_copy() {
		for keeper_first in [true, false] {
			let routes = Producer::new();
			let front = logical(&routes);
			let a = received_copy();
			routes.serve(a.consume());
			let mut first = subscribe(&front, HOUR);
			let mut second = subscribe(&front, HOUR);
			for key in 0..=4 {
				write(&a, key);
			}
			assert_eq!(drain(&mut first, false), [0, 1, 2, 3, 4]);
			assert_eq!(drain(&mut second, false), [0, 1, 2, 3, 4]);
			let mut open = a.create_group(group::Info { start: 5 }).unwrap();
			open.write_frame(ts(5), b"x".as_ref()).unwrap();
			let (keeper, other) = match keeper_first {
				true => (&mut first, &mut second),
				false => (&mut second, &mut first),
			};
			// Only the keeper still reads group 5, so only it keeps `a` after the switch.
			let reading = recv(keeper);
			drop(recv(other));
			let b = received_copy();
			routes.serve(b.consume());
			write(&b, 7);
			assert_eq!(drain(keeper, false), [7]);
			assert_eq!(drain(other, false), [7]);
			// Past a's cap: only the keeper's boundary sees it.
			write(&a, 8);
			assert_eq!(drain(keeper, false), Vec::<u64>::new());
			assert_eq!(drain(other, false), Vec::<u64>::new());
			let resume = routes.subscription(front.subscription().unwrap(), 2).resume;
			assert_eq!(resume, Some(Position::new(7, 1)), "keeper_first={keeper_first}");
			drop(reading);
		}
	}

	/// A stalled read still waiting for the new route to continue it is judged within the
	/// reader's window too: the new route's groups in it expire the read, though its
	/// subscription starts past them.
	#[test]
	#[cfg(feature = "async")]
	fn a_pending_recovery_expires_within_the_readers_window() {
		// The reader's groups, the group past them the old route holds, the groups the new route
		// holds, and whether they expire it.
		for (ranges, held, newer, expires) in [(0..4, Some(4), 1..4, true), (0..1, None, 1..5, false)] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let subscription = Subscription::default()
				.with_delay(Delay::default().with_max(Duration::from_secs(2)))
				.with_ranges([track::Range::groups(ranges.clone())]);
			let mut sub = logical
				.consume()
				.subscribe(subscription)
				.now_or_never()
				.unwrap()
				.unwrap();
			let mut open = a.create_group(group::Info { start: 0 }).unwrap();
			open.write_frame(ts(0), b"a".as_ref()).unwrap();
			let mut reading = recv(&mut sub);
			assert_eq!(read(&mut reading), Some(b"a".to_vec()));
			assert!(reading.read_frame().now_or_never().is_none());
			if let Some(key) = held {
				write(&a, key);
			}

			// The new route never has group 0, so the read waits on it.
			let b = copy();
			routes.serve(b.consume());
			assert!(reading.read_frame().now_or_never().is_none());
			for key in newer {
				let mut next = b.create_group(group::Info { start: key }).unwrap();
				next.write_frame(ts(key * 1000), b"x".as_ref()).unwrap();
				next.finish().unwrap();
			}
			let read = reading.read_frame().now_or_never();
			match expires {
				true => assert!(matches!(read, Some(Err(Error::Old))), "ranges={ranges:?}: {read:?}"),
				false => assert!(read.is_none(), "ranges={ranges:?}: {read:?}"),
			}
		}
	}

	/// A group moved onto another copy wakes when the reader's local cap rises over groups that
	/// expire it.
	#[test]
	#[cfg(feature = "async")]
	fn a_recovered_group_wakes_when_the_readers_cap_rises() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(2));
		sub.set_range(..1);
		let mut open = a.create_group(group::Info { start: 0 }).unwrap();
		open.write_frame(ts(0), b"a".as_ref()).unwrap();
		let mut reading = recv(&mut sub);
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));

		let b = copy();
		let mut replacement = b.create_group(group::Info { start: 0 }).unwrap();
		replacement.write_frame(ts(0), b"a".as_ref()).unwrap();
		routes.serve(b.consume());
		for key in 1..5 {
			let mut next = b.create_group(group::Info { start: key }).unwrap();
			next.write_frame(ts(key * 1000), b"x".as_ref()).unwrap();
			next.finish().unwrap();
		}
		let wake = Arc::new(Wake::default());
		let waiter = kio::Waiter::new(wake.clone().into());
		assert!(reading.poll_read_frame(&waiter).is_pending(), "past the reader's cap");
		wake.0.store(false, std::sync::atomic::Ordering::Relaxed);

		sub.set_range(..5);
		assert!(wake.0.load(std::sync::atomic::Ordering::Relaxed));
		assert!(matches!(reading.poll_read_frame(&waiter), Poll::Ready(Err(Error::Old))));
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_pending_fetch_wakes_and_moves_to_the_new_route() {
		let routes = Producer::new();
		let a = copy();
		let dynamic = a.requests();
		routes.serve(a.consume());
		let logical = logical(&routes);
		let mut fetching = logical.consume().fetch_group(0, None);
		let wake = Arc::new(Wake::default());
		let waiter = kio::Waiter::new(wake.clone().into());
		assert!(kio::Task::poll(&mut *fetching, &waiter).is_pending());
		let _request = dynamic.requested_fill_group().now_or_never().unwrap().unwrap();
		wake.0.store(false, std::sync::atomic::Ordering::Relaxed);
		let b = copy();
		let mut group = b.create_group(group::Info { start: 0 }).unwrap();
		group.write_frame(ts(0), b"b".as_ref()).unwrap();
		group.finish().unwrap();
		routes.serve(b.consume());
		assert!(wake.0.load(std::sync::atomic::Ordering::Relaxed));
		assert!(matches!(kio::Task::poll(&mut *fetching, &waiter), Poll::Ready(Ok(_))));
	}

	/// A cold route widens its fill to the group's head, but the caller still reads from
	/// the frame it asked for.
	#[test]
	#[cfg(feature = "async")]
	fn a_widened_route_fetch_keeps_the_callers_start() {
		let routes = Producer::new();
		let a = copy();
		let dynamic = a.requests();
		routes.serve(a.consume());
		let logical = logical(&routes);
		let options = group::Fetch {
			frame_start: 2,
			..Default::default()
		};
		let mut fetching = logical.consume().fetch_group(0, options);
		let waiter = kio::Waiter::noop();
		assert!(kio::Task::poll(&mut *fetching, &waiter).is_pending());
		let request = dynamic.requested_fill_group().now_or_never().unwrap().unwrap();
		assert_eq!(request.frame_start(), 0, "the cold copy fills from the head");
		let mut group = request.accept(None).unwrap().expect("a fresh group");
		for frame in 0..4u8 {
			group.write_frame(ts(0), vec![frame]).unwrap();
		}
		group.finish().unwrap();
		let Poll::Ready(Ok(mut group)) = kio::Task::poll(&mut *fetching, &waiter) else {
			panic!("the fill did not resolve the fetch");
		};
		assert_eq!(group.index(), 2);
		let frame = group.read_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(&frame.payload[..], &[2]);
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_pending_recovery_fetch_does_not_disable_expiry() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(2));
		let mut open = a.create_group(group::Info { start: 0 }).unwrap();
		open.write_frame(ts(0), b"a".as_ref()).unwrap();
		let mut reading = recv(&mut sub);
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));
		let b = copy();
		let _dynamic = b.requests();
		routes.serve(b.consume());
		assert!(reading.read_frame().now_or_never().is_none());
		for key in 1..=4 {
			let mut group = b.create_group(group::Info { start: key * 1000 }).unwrap();
			group.write_frame(ts(key * 1000), b"b".as_ref()).unwrap();
			group.finish().unwrap();
		}
		assert!(matches!(reading.read_frame().now_or_never(), Some(Err(Error::Old))));
	}

	#[test]
	#[cfg(feature = "async")]
	fn sparse_object_ids_refuse_ordinal_recovery() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		let group = a.create_group(group::Info { start: 0 }).unwrap();
		group
			.set_wire(group::Wire {
				sparse: false,
				first_object: true,
				first_id: 6,
				extensions: false,
				has_end: true,
				fetchable: false,
				last: None,
			})
			.unwrap();
		let mut frame = group
			.create_object(crate::frame::Header {
				info: crate::frame::Info {
					size: 1,
					timestamp: ts(0),
				},
				fields: Some(Arc::new(super::super::object::Fields {
					id: 6,
					properties: Default::default(),
				})),
				inflated: None,
			})
			.unwrap();
		frame.write(bytes::Bytes::from_static(b"a")).unwrap();
		let group = frame.finish().unwrap();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::MAX);
		let mut reading = recv(&mut sub);
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));
		assert_eq!(a.consume().resume_position(), Some(Position::group(1)));
		let mut partial = group
			.create_object(crate::frame::Header {
				info: crate::frame::Info {
					size: 2,
					timestamp: ts(1),
				},
				fields: Some(Arc::new(super::super::object::Fields {
					id: 8,
					properties: Default::default(),
				})),
				inflated: None,
			})
			.unwrap();
		partial.write(bytes::Bytes::from_static(b"b")).unwrap();
		let mut chunks = reading.next_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(chunks.read_chunk().now_or_never().unwrap().unwrap().unwrap(), b"b"[..]);
		partial.abort(Error::Dropped).unwrap();
		routes.serve(copy().consume());
		assert!(matches!(
			chunks.read_chunk().now_or_never(),
			Some(Err(Error::Unsupported))
		));
		assert!(matches!(
			reading.read_frame().now_or_never(),
			Some(Err(Error::Unsupported))
		));
	}

	/// A flap back to the route being read is no route change for an in-flight group, so
	/// a sparse group the reader never polled across it keeps reading its own copy.
	#[test]
	#[cfg(feature = "async")]
	fn an_unpolled_flap_back_keeps_reading_a_sparse_group() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		let mut group = a.create_group(group::Info { start: 0 }).unwrap();
		group
			.set_wire(group::Wire {
				sparse: false,
				first_object: true,
				first_id: 6,
				extensions: false,
				has_end: true,
				fetchable: false,
				last: None,
			})
			.unwrap();
		let object = |group: group::Producer, id: u64, payload: &'static [u8]| {
			let header = crate::frame::Header {
				info: crate::frame::Info {
					size: payload.len() as u64,
					timestamp: ts(id),
				},
				fields: Some(Arc::new(super::super::object::Fields {
					id,
					properties: Default::default(),
				})),
				inflated: None,
			};
			let mut frame = group.create_object(header).unwrap();
			frame.write(bytes::Bytes::from_static(payload)).unwrap();
			frame.finish().unwrap()
		};
		group = object(group, 6, b"a");
		group = object(group, 8, b"b");
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::MAX);
		let mut reading = recv(&mut sub);
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));
		assert_eq!(read(&mut reading), Some(b"b".to_vec()));
		routes.serve(copy().consume());
		routes.serve(a.consume());
		assert!(reading.read_frame().now_or_never().is_none());
		let _group = object(group, 9, b"c");
		assert_eq!(read(&mut reading), Some(b"c".to_vec()));
	}

	/// A replacement copy that joined past a gap, holding the group from Object 6.
	#[cfg(feature = "async")]
	fn joined_past_a_gap(objects: &[(u64, &'static [u8])]) -> track::Producer {
		let b = copy();
		let mut group = b.create_group(group::Info { start: 0 }).unwrap();
		group
			.set_wire(group::Wire {
				sparse: false,
				first_object: false,
				first_id: 6,
				extensions: false,
				has_end: true,
				fetchable: false,
				last: None,
			})
			.unwrap();
		for &(id, payload) in objects {
			let mut frame = group
				.create_object(crate::frame::Header {
					info: crate::frame::Info {
						size: payload.len() as u64,
						timestamp: ts(id),
					},
					fields: Some(Arc::new(super::super::object::Fields {
						id,
						properties: Default::default(),
					})),
					inflated: None,
				})
				.unwrap();
			frame.write(bytes::Bytes::from_static(payload)).unwrap();
			group = frame.finish().unwrap();
		}
		group.finish().unwrap();
		b
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_replacement_past_a_gap_refuses_ordinal_recovery() {
		// A half-read Object 0 must not continue with Object 6's bytes.
		let routes = Producer::new();
		let front = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&front, Duration::from_secs(10));
		let open = a.create_group(group::Info { start: 0 }).unwrap();
		let mut frame = open
			.create_frame(crate::frame::Info {
				size: 4,
				timestamp: ts(0),
			})
			.unwrap();
		frame.write(b"ab".as_ref()).unwrap();
		let mut reading = recv(&mut sub);
		let mut chunks = reading.next_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(
			&chunks.read_chunk().now_or_never().unwrap().unwrap().unwrap()[..],
			b"ab"
		);
		drop(frame);
		a.clone().abort(Error::Dropped).unwrap();
		let b = joined_past_a_gap(&[(6, b"WXYZ")]);
		routes.serve(b.consume());
		assert!(matches!(
			chunks.read_chunk().now_or_never(),
			Some(Err(Error::Unsupported))
		));

		// Ordinal 2 of that copy is Object 8, not Object 2.
		let routes = Producer::new();
		let front = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&front, Duration::from_secs(10));
		let mut open = a.create_group(group::Info { start: 0 }).unwrap();
		open.write_frame(ts(0), b"a".as_ref()).unwrap();
		open.write_frame(ts(1), b"b".as_ref()).unwrap();
		let mut reading = recv(&mut sub);
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));
		assert_eq!(read(&mut reading), Some(b"b".to_vec()));
		let b = joined_past_a_gap(&[(6, b"x"), (7, b"y"), (8, b"z")]);
		routes.serve(b.consume());
		assert!(matches!(
			reading.read_frame().now_or_never(),
			Some(Err(Error::Unsupported))
		));

		// A batch holding the old copy's frame hides the refusal, but the next read sees it,
		// whether a batch or a single frame served from what that batch prefetched.
		for single in [false, true] {
			let routes = Producer::new();
			let front = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let mut sub = subscribe(&front, Duration::from_secs(10));
			let mut open = a.create_group(group::Info { start: 0 }).unwrap();
			open.write_frame(ts(0), b"a".as_ref()).unwrap();
			let mut reading = recv(&mut sub);
			let b = joined_past_a_gap(&[(6, b"x"), (7, b"y"), (8, b"z")]);
			routes.serve(b.consume());
			let mut batch = crate::frame::Buffer::<4>::new();
			let waiter = kio::Waiter::noop();
			assert!(matches!(
				reading.poll_read_frames(&waiter, &mut batch),
				Poll::Ready(Ok(1))
			));
			assert_eq!(&batch.filled_mut()[0].payload[..], b"a");
			let next = match single {
				true => reading.read_frame().now_or_never().unwrap().map(|_| ()),
				false => match reading.poll_read_frames(&waiter, &mut batch) {
					Poll::Ready(res) => res.map(|_| ()),
					Poll::Pending => panic!("the batch parked"),
				},
			};
			assert!(matches!(next, Err(Error::Unsupported)), "single={single}");
		}
	}

	#[test]
	fn a_fetched_group_wakes_when_the_route_changes() {
		let routes = Producer::new();
		let a = copy();
		let _open = a.create_group(group::Info { start: 0 }).unwrap();
		routes.serve(a.consume());
		let mut reading = logical(&routes)
			.consume()
			.fetch_group(0, None)
			.now_or_never()
			.unwrap()
			.unwrap();
		let wake = Arc::new(Wake::default());
		let waiter = kio::Waiter::new(wake.clone().into());
		assert!(reading.poll_read_frame(&waiter).is_pending());
		routes.serve(copy().consume());
		assert!(wake.0.load(std::sync::atomic::Ordering::Relaxed));
	}

	/// A fetch answered from a group still arriving over a subscription outlives that
	/// delivery giving up on it as too old: the route is asked for the rest instead.
	#[test]
	#[cfg(feature = "async")]
	fn a_fetched_group_outlives_its_delivery_giving_up() {
		let routes = Producer::new();
		let a = copy();
		let dynamic = a.requests();
		routes.serve(a.consume());

		let mut arriving = a.create_group(group::Info { start: 0 }).unwrap();
		arriving.write_frame(ts(0), b"a".as_ref()).unwrap();
		let mut fetched = logical(&routes)
			.consume()
			.fetch_group(0, None)
			.now_or_never()
			.unwrap()
			.unwrap();
		assert_eq!(read(&mut fetched), Some(b"a".to_vec()));

		// The publisher resets the subscription's stream for the group as too old.
		arriving.abort(Error::Stream(StreamError::Old)).unwrap();
		assert!(fetched.read_frame().now_or_never().is_none());

		let request = dynamic.requested_fill_group().now_or_never().unwrap().unwrap();
		assert_eq!((request.key(), request.frame_start()), (0, 1));
		let mut rest = request.accept(None).unwrap().expect("a fresh group");
		rest.write_frame(ts(1), b"b".as_ref()).unwrap();
		rest.finish().unwrap();
		assert_eq!(read(&mut fetched), Some(b"b".to_vec()));
		assert_eq!(read(&mut fetched), None);
	}

	/// Mirrors `routeFailed` in `js/net/src/resume.test.ts`: only these stream codes are a
	/// route failing to deliver; every other code judges the group.
	#[test]
	fn only_delivery_codes_are_a_route_failing() {
		const DELIVERY: [u32; 6] = [0x0, 0x1, 0x3, 0x4, 0x31, 0x36];
		for code in (0..0x40).chain([64, 64 + 5, 0xffff_ffff]) {
			let err = Error::Stream(StreamError::from_code(code));
			assert_eq!(err.retryable(), DELIVERY.contains(&code), "{code:#x}: {err:?}");
		}
		assert!(Error::Unroutable.retryable());
	}

	/// A failure judging the group repeats on every copy from its route, so it ends a read or
	/// fetched group once without asking that route again.
	#[test]
	#[cfg(feature = "async")]
	fn a_verdict_is_never_asked_of_its_own_route() {
		let verdicts = [
			Error::WrongSize,
			Error::GroupTooLarge,
			Error::Stream(StreamError::MalformedTrack),
			Error::Stream(StreamError::TooFarBehind),
			Error::Stream(StreamError::Unauthorized),
		];
		for err in verdicts {
			for fetched in [false, true] {
				let routes = Producer::new();
				let logical = logical(&routes);
				let a = copy();
				let dynamic = a.requests();
				routes.serve(a.consume());
				let mut sub = subscribe(&logical, HOUR);
				let mut open = a.create_group(group::Info { start: 0 }).unwrap();
				open.write_frame(ts(0), b"a".as_ref()).unwrap();
				let mut reading = match fetched {
					true => logical.consume().fetch_group(0, None).now_or_never().unwrap().unwrap(),
					false => recv(&mut sub),
				};
				assert_eq!(read(&mut reading), Some(b"a".to_vec()));

				open.abort(err.clone()).unwrap();
				for _ in 0..2 {
					let res = reading.read_frame().now_or_never();
					assert!(
						matches!(&res, Some(Err(got)) if StreamError::from(got) == StreamError::from(&err)),
						"{err:?} fetched={fetched}: {res:?}"
					);
				}
				assert!(
					dynamic.requested_fill_group().now_or_never().is_none(),
					"{err:?} fetched={fetched}: asked its own route again"
				);
			}
		}
	}

	/// A verdict moves once to the newer route serving, and ends the group when that route's
	/// copy repeats it.
	#[test]
	#[cfg(feature = "async")]
	fn a_verdict_every_route_repeats_ends_the_group_once() {
		for fetched in [false, true] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			let a_requests = a.requests();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, HOUR);
			let mut open = a.create_group(group::Info { start: 0 }).unwrap();
			open.write_frame(ts(0), b"a".as_ref()).unwrap();
			let mut reading = match fetched {
				true => logical.consume().fetch_group(0, None).now_or_never().unwrap().unwrap(),
				false => recv(&mut sub),
			};
			assert_eq!(read(&mut reading), Some(b"a".to_vec()));

			let b = copy();
			let b_requests = b.requests();
			routes.serve(b.consume());
			let mut repeat = b.create_group(group::Info { start: 0 }).unwrap();
			repeat.write_frame(ts(0), b"a".as_ref()).unwrap();
			open.abort(Error::WrongSize).unwrap();
			assert!(reading.read_frame().now_or_never().is_none(), "fetched={fetched}");
			repeat.abort(Error::WrongSize).unwrap();

			for _ in 0..2 {
				let res = reading.read_frame().now_or_never();
				assert!(matches!(res, Some(Err(Error::WrongSize))), "fetched={fetched}: {res:?}");
			}
			assert!(
				a_requests.requested_fill_group().now_or_never().is_none(),
				"fetched={fetched}"
			);
			assert!(
				b_requests.requested_fill_group().now_or_never().is_none(),
				"fetched={fetched}"
			);
		}
	}

	/// A peer resetting the group with a delivery code continues it from the route's next copy.
	#[test]
	#[cfg(feature = "async")]
	fn a_delivery_failure_continues_from_the_next_route() {
		for code in [StreamError::Internal, StreamError::Cancel, StreamError::ControlTimeout] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, HOUR);
			let mut open = a.create_group(group::Info { start: 0 }).unwrap();
			open.write_frame(ts(0), b"a".as_ref()).unwrap();
			let mut reading = recv(&mut sub);
			assert_eq!(read(&mut reading), Some(b"a".to_vec()));

			let b = copy();
			let dynamic = b.requests();
			routes.serve(b.consume());
			open.abort(Error::Stream(code.clone())).unwrap();
			assert!(reading.read_frame().now_or_never().is_none(), "{code:?}");
			let request = dynamic.requested_fill_group().now_or_never().unwrap().unwrap();
			let mut rest = request.accept(None).unwrap().expect("a fresh group");
			rest.write_frame(ts(1), b"b".as_ref()).unwrap();
			rest.finish().unwrap();
			assert_eq!(read(&mut reading), Some(b"b".to_vec()), "{code:?}");
			assert_eq!(read(&mut reading), None, "{code:?}");
		}
	}

	/// A fetched frame read to its end no longer holds the logical track, so a caller
	/// keeping the handle does not stop the track from parking.
	#[test]
	#[cfg(feature = "async")]
	fn a_read_frame_releases_the_track() {
		let routes = Producer::new();
		let a = copy();
		routes.serve(a.consume());
		let mut open = a.create_group(group::Info { start: 0 }).unwrap();
		open.write_frame(ts(0), b"a".as_ref()).unwrap();

		let logical = logical(&routes);
		let demand = logical.demand();
		let mut fetched = logical.consume().fetch_group(0, None).now_or_never().unwrap().unwrap();
		assert!(demand.is_wanted(), "an open fetched group holds the track");
		let mut frame = fetched.next_frame().now_or_never().unwrap().unwrap().unwrap();
		drop(fetched);
		assert!(demand.is_wanted(), "an unread frame holds the track");
		assert_eq!(frame.read_all().now_or_never().unwrap().unwrap(), b"a".as_ref());
		assert!(!demand.is_wanted(), "a read frame does not");
	}

	/// A rerouted fetch keeps its own age budget: the new route is asked with it, and a
	/// read waiting on that route gives up once the group falls past it there.
	#[test]
	#[cfg(feature = "async")]
	fn a_rerouted_fetch_keeps_its_budget() {
		let routes = Producer::new();
		let a = copy();
		routes.serve(a.consume());
		let mut open = a.create_group(group::Info { start: 0 }).unwrap();
		open.write_frame(ts(0), b"a".as_ref()).unwrap();

		let budget = Duration::from_millis(500);
		let options = group::Fetch::default().with_max_delay(budget);
		let mut fetched = logical(&routes)
			.consume()
			.fetch_group(0, options)
			.now_or_never()
			.unwrap()
			.unwrap();
		assert_eq!(read(&mut fetched), Some(b"a".to_vec()));

		let b = copy();
		let dynamic = b.requests();
		routes.serve(b.consume());
		assert!(fetched.read_frame().now_or_never().is_none());
		let request = dynamic.requested_fill_group().now_or_never().unwrap().unwrap();
		assert_eq!((request.frame_start(), request.max_delay()), (1, Some(budget)));

		// The new route runs a full second past the group's successor while its fetch is unanswered.
		for (key, ms) in [(1, 1000), (2, 2000)] {
			let mut group = b.create_group(group::Info { start: key }).unwrap();
			group.write_frame(ts(ms), b"x".as_ref()).unwrap();
			group.finish().unwrap();
		}
		assert!(matches!(fetched.read_frame().now_or_never(), Some(Err(Error::Old))));
	}

	#[test]
	#[cfg(feature = "async")]
	fn ending_the_front_releases_a_stalled_group() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(10));
		let _open = a.create_group(group::Info { start: 0 }).unwrap();
		let mut reading = recv(&mut sub);
		assert!(reading.read_frame().now_or_never().is_none());
		routes.end(Err(Error::Unroutable));
		assert!(matches!(
			reading.read_frame().now_or_never(),
			Some(Err(Error::Unroutable))
		));
	}

	/// A live join, as the first copy resolved it, stays once a route switch lets that copy
	/// go.
	#[test]
	#[cfg(feature = "async")]
	fn the_floor_outlives_the_first_copy() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		write(&a, 4);
		write(&a, 5);
		// One behind the newest frame, at 5.
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(Duration::from_secs(10)))
			.with_ranges([track::Range::from(track::Start::Live(Duration::from_millis(1)))]);
		let mut sub = logical
			.consume()
			.subscribe(subscription)
			.now_or_never()
			.unwrap()
			.unwrap();
		let mut read = drain(&mut sub, false);
		read.sort();
		assert_eq!(read, [4, 5]);
		assert_eq!(sub.floor(), Some(4));
		assert_eq!(sub.next_in_range(0), Some(4));

		let b = copy();
		routes.serve(b.consume());
		write(&b, 6);
		assert_eq!(drain(&mut sub, false), [6]);
		let readers = routes.readers.lock().unwrap();
		let reader = readers[0].upgrade().unwrap();
		assert_eq!(reader.lock().unwrap().copies.len(), 1, "the first copy was let go");
		drop(readers);
		assert_eq!(sub.floor(), Some(4));
		assert_eq!(sub.next_in_range(0), Some(4));
	}

	/// Ranges replaced once a route switch let the first copy go still resolve, from the
	/// serving copy, whose failover cap bounds what it delivers and not where they resolve.
	#[test]
	#[cfg(feature = "async")]
	fn replaced_ranges_resolve_after_the_first_copy_is_let_go() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(10));
		write(&a, 0);
		assert_eq!(drain(&mut sub, false), [0]);

		let b = copy();
		routes.serve(b.consume());
		for key in 1..=5 {
			write(&b, key);
		}
		assert!(!drain(&mut sub, false).is_empty());
		{
			let readers = routes.readers.lock().unwrap();
			let reader = readers[0].upgrade().unwrap();
			let reader = reader.lock().unwrap();
			assert_eq!(reader.copies.len(), 1, "the first copy was let go");
			assert!(reader.copies[0].floor.is_some(), "the serving copy has a failover cap");
		}

		// A live join one behind the newest frame, at 5.
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(Duration::from_secs(10)))
			.with_ranges([
				track::Range::groups(0..1),
				track::Range::from(track::Start::Live(Duration::from_millis(1))),
			]);
		sub.update(subscription).unwrap();
		drain(&mut sub, false);
		assert_eq!(
			sub.next_in_range(1),
			Some(4),
			"the live join resolved on the serving copy"
		);
		assert_eq!(sub.floor(), Some(0));
	}

	/// A time end resolves on the serving copy even when the start is already absolute.
	#[test]
	#[cfg(feature = "async")]
	fn a_resumed_reader_reports_starts_within_its_time_end() {
		for start in [track::Start::Time(0), track::Start::Live(Duration::ZERO)] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			write(&a, 0);
			routes.serve(a.consume());
			let subscription = Subscription::default()
				.with_delay(Delay::default().with_max(HOUR))
				.with_ranges([track::Range { start, end: Some(1) }]);
			let mut sub = logical
				.consume()
				.subscribe(subscription)
				.now_or_never()
				.unwrap()
				.unwrap();
			let _ = sub.poll_start(&kio::Waiter::noop());
			// Where the range starts once delivery starts at `from`, none past its end.
			let start = |sub: &mut track::Subscriber, from: u64| {
				let resolved = sub.resolved_range(0)?;
				let from = resolved.start.max(from);
				resolved.end.is_none_or(|end| end > from).then_some(from)
			};
			assert_eq!(start(&mut sub, 0), Some(0));
			for key in 1..3 {
				write(&a, key);
			}
			assert_eq!(start(&mut sub, 10), None);
			assert_eq!(
				start(&mut sub, 0),
				Some(0),
				"the start stays pinned while the end settles"
			);
		}
	}

	/// A resumed reader names the next group its ranges cover, so a reader waiting on each
	/// group in key walks over the gap between two ranges instead of waiting for it.
	#[test]
	#[cfg(feature = "async")]
	fn a_resumed_reader_names_the_next_group_in_range() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(Duration::from_secs(10)))
			.with_ranges([track::Range::groups(0..1), track::Range::groups(10..11)]);
		let mut sub = logical
			.consume()
			.subscribe(subscription)
			.now_or_never()
			.unwrap()
			.unwrap();
		for key in [0, 5, 10] {
			write(&a, key);
		}
		let mut read = drain(&mut sub, false);
		read.sort();
		assert_eq!(read, [0, 10]);
		assert_eq!(
			sub.next_in_range(1),
			Some(10),
			"the gap between the ranges is never read"
		);
		assert_eq!(sub.next_in_range(11), None);
	}

	#[test]
	fn ending_the_front_releases_a_pending_fetch() {
		let routes = Producer::new();
		let a = copy();
		let _dynamic = a.requests();
		routes.serve(a.consume());
		let logical = logical(&routes);
		let mut fetching = logical.consume().fetch_group(0, None);
		let waiter = kio::Waiter::noop();
		assert!(kio::Task::poll(&mut *fetching, &waiter).is_pending());
		routes.end(Err(Error::Unroutable));
		assert!(matches!(
			kio::Task::poll(&mut *fetching, &waiter),
			Poll::Ready(Err(Error::Unroutable))
		));
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_recovered_group_keeps_its_new_copy_subscribed_during_another_switch() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(10));
		let _open = a.create_group(group::Info { start: 0 }).unwrap();
		let mut reading = recv(&mut sub);
		let b = copy();
		let mut replacement = b.create_group(group::Info { start: 0 }).unwrap();
		replacement.write_frame(ts(0), b"b".as_ref()).unwrap();
		routes.serve(b.consume());
		assert_eq!(read(&mut reading), Some(b"b".to_vec()));
		let c = copy();
		routes.serve(c.consume());
		assert!(sub.recv_group().now_or_never().is_none());
		let readers = routes.readers.lock().unwrap();
		let reader = readers[0].upgrade().unwrap();
		assert!(reader.lock().unwrap().copies.iter().any(|copy| copy.generation == 2));
	}

	#[test]
	#[cfg(feature = "async")]
	fn held_read_preferences_follow_the_reader() {
		enum Read {
			Group(Box<group::Consumer>),
			Frame(Box<crate::frame::Consumer>),
		}
		impl Read {
			fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<()>> {
				match self {
					Self::Group(group) => group
						.poll_read_frame(waiter)
						.map(|result| result.map(|frame| assert!(frame.is_none()))),
					Self::Frame(frame) => frame
						.poll_read_chunk(waiter)
						.map(|result| result.map(|frame| assert!(frame.is_none()))),
				}
			}
		}
		for recovered in [false, true] {
			for widen in [false, true] {
				for chunked in [false, true] {
					let routes = Producer::new();
					let logical = logical(&routes);
					let a = copy();
					routes.serve(a.consume());
					let mut sub = subscribe(&logical, Duration::from_secs(if widen { 2 } else { 10 }));
					let b = copy();
					// Both routes carry the same content, including the unfinished payload.
					let writers = [&a, &b].map(|source| source.create_group(group::Info { start: 0 }).unwrap());
					// Each group's writer, held open by its unfinished frame when chunked.
					let _writers: Vec<_> = writers
						.into_iter()
						.map(|mut group| {
							group.write_frame(ts(0), b"first".as_ref()).unwrap();
							match chunked {
								false => (Some(group), None),
								true => {
									let mut frame = group
										.create_frame(crate::frame::Info {
											size: 4,
											timestamp: ts(1),
										})
										.unwrap();
									frame.write(b"ab".as_ref()).unwrap();
									(None, Some(frame))
								}
							}
						})
						.collect();
					let mut group = recv(&mut sub);
					assert_eq!(read(&mut group), Some(b"first".to_vec()));
					// Give both read APIs the same wake-driven schedule. A detached frame
					// must observe preferences without its parent group being polled.
					let mut reading = if chunked {
						let mut frame = group.next_frame().now_or_never().unwrap().unwrap().unwrap();
						assert_eq!(&frame.read_chunk().now_or_never().unwrap().unwrap().unwrap()[..], b"ab");
						drop(group);
						Read::Frame(Box::new(frame))
					} else {
						Read::Group(Box::new(group))
					};
					let source = if recovered {
						routes.serve(b.consume());
						&b
					} else {
						&a
					};
					let wake = Arc::new(Wake::default());
					let waiter = kio::Waiter::new(wake.clone().into());
					assert!(reading.poll(&waiter).is_pending());
					if recovered {
						// Retire the original copy so its preferences stop being mirrored: its
						// route goes away, since a replayed group alone never bounds it.
						a.clone().abort(Error::Dropped).unwrap();
						assert!(sub.recv_group().now_or_never().is_none());
						assert!(a.subscription().is_none());
					}
					sub.control()
						.update(
							Subscription::default()
								.with_priority(7)
								.with_delay(Delay::default().with_max(Duration::from_secs(if widen { 10 } else { 2 }))),
						)
						.unwrap();
					// The update reaches the copy being read at once. Nothing landed above the
					// held group, so no budget can convict it yet: the read need not wake until
					// a group lands, and judges the new budget then.
					assert_eq!(
						source.subscription().unwrap().priority,
						7,
						"recovered={recovered}, widen={widen}, chunked={chunked}: preference update did not reach the copy"
					);
					for key in 1..=4 {
						let mut next = source.create_group(group::Info { start: key }).unwrap();
						next.write_frame(ts(key * 1000), b"next".as_ref()).unwrap();
						next.finish().unwrap();
					}
					let result = reading.poll(&waiter);
					if widen {
						assert!(
							result.is_pending(),
							"recovered={recovered}, chunked={chunked}: widened budget was ignored"
						);
					} else {
						assert!(
							matches!(result, Poll::Ready(Err(Error::Old))),
							"recovered={recovered}, chunked={chunked}: narrowed budget did not expire the read"
						);
					}
					assert_eq!(source.subscription().unwrap().priority, 7);
					if widen {
						// Observing one change must re-arm the waiter for the next change.
						wake.0.store(false, std::sync::atomic::Ordering::Relaxed);
						sub.control()
							.update(
								Subscription::default()
									.with_priority(8)
									.with_delay(Delay::default().with_max(Duration::from_secs(10))),
							)
							.unwrap();
						assert!(
							wake.0.load(std::sync::atomic::Ordering::Relaxed),
							"recovered={recovered}, chunked={chunked}: second update did not wake the held read"
						);
						assert!(reading.poll(&waiter).is_pending());
						assert_eq!(source.subscription().unwrap().priority, 8);
					}
				}
			}
		}
	}

	/// Replay every ordering of independent actions, with the return to A following
	/// the switch to B. The payload is the oracle, independent of route bookkeeping.
	#[test]
	#[cfg(feature = "async")]
	fn route_flap_interleavings_preserve_readers() {
		#[derive(Clone, Copy, Debug, PartialEq, Eq)]
		enum Event {
			SwitchToB,
			ReturnToA,
			FinishA,
			FinishB,
			Update,
			Cancel,
		}

		#[cfg(feature = "async")]
		fn run(events: &[Event], wake_only: bool) {
			let routes = Producer::new();
			let logical = logical(&routes);
			let copies = [copy(), copy()];
			routes.serve(copies[0].consume());
			let mut writers: Vec<_> = copies
				.iter()
				.map(|copy| Some(copy.create_group(group::Info { start: 0 }).unwrap()))
				.collect();
			writers[0]
				.as_mut()
				.unwrap()
				.write_frame(ts(0), b"first".as_ref())
				.unwrap();
			let mut subscriptions = [
				Some(subscribe(&logical, Duration::from_secs(10))),
				Some(subscribe(&logical, Duration::from_secs(10))),
			];
			let control = subscriptions[1].as_ref().unwrap().control();
			let mut groups = subscriptions.each_mut().map(|sub| Some(recv(sub.as_mut().unwrap())));
			let wakes = [Arc::new(Wake::default()), Arc::new(Wake::default())];
			let waiters = wakes.each_ref().map(|wake| kio::Waiter::new(wake.clone().into()));
			let mut received: [Vec<Vec<u8>>; 2] = Default::default();
			let mut ended = [false; 2];

			let mut poll = |groups: &mut [Option<group::Consumer>; 2], initial: bool| {
				for i in 0..2 {
					let woken = wakes[i].0.swap(false, std::sync::atomic::Ordering::Relaxed);
					if ended[i] || (!initial && wake_only && !woken) {
						continue;
					}
					let Some(group) = &mut groups[i] else { continue };
					loop {
						match group.poll_read_frame(&waiters[i]) {
							Poll::Ready(Ok(Some(frame))) => received[i].push(frame.payload.to_vec()),
							Poll::Ready(Ok(None)) => {
								ended[i] = true;
								break;
							}
							Poll::Ready(Err(err)) => panic!("{events:?}, wake_only={wake_only}, reader={i}: {err}"),
							Poll::Pending => break,
						}
					}
				}
			};
			poll(&mut groups, true);
			for event in events {
				match event {
					Event::SwitchToB => routes.serve(copies[1].consume()),
					Event::ReturnToA => routes.serve(copies[0].consume()),
					Event::FinishA | Event::FinishB => {
						let index = usize::from(*event == Event::FinishB);
						let mut writer = writers[index].take().unwrap();
						if index == 1 {
							writer.write_frame(ts(0), b"first".as_ref()).unwrap();
						}
						writer.write_frame(ts(1), b"second".as_ref()).unwrap();
						writer.finish().unwrap();
					}
					Event::Update => control
						.update(
							Subscription::default()
								.with_priority(7)
								.with_delay(Delay::default().with_max(Duration::from_secs(2))),
						)
						.unwrap(),
					Event::Cancel => {
						groups[0] = None;
						subscriptions[0] = None;
					}
				}
				poll(&mut groups, false);
			}
			assert_eq!(
				received[1],
				[b"first".to_vec(), b"second".to_vec()],
				"{events:?}, wake_only={wake_only}"
			);
			assert!(ended[1], "{events:?}, wake_only={wake_only}: missing group end");
			assert!(
				[b"first".to_vec(), b"second".to_vec()].starts_with(&received[0]),
				"{events:?}: cancelled reader replayed content"
			);
			assert!(
				subscriptions[1].as_mut().unwrap().recv_group().now_or_never().is_none(),
				"{events:?}: duplicate group"
			);
			let demand = copies[0].subscription().expect("surviving reader");
			assert_eq!(demand.priority, 7, "{events:?}");
			assert_eq!(demand.delay.max, Some(Duration::from_secs(2)), "{events:?}");
			drop(groups);
			drop(subscriptions);
			drop(control);
			for copy in &copies {
				assert!(copy.subscription().is_none(), "{events:?}: cancelled demand survived");
			}
		}

		#[cfg(feature = "async")]
		fn walk(events: &mut Vec<Event>, remaining: &[Event]) {
			if remaining.is_empty() {
				for wake_only in [false, true] {
					run(events, wake_only);
				}
				return;
			}
			for (i, event) in remaining.iter().enumerate() {
				if *event == Event::ReturnToA && !events.contains(&Event::SwitchToB) {
					continue;
				}
				events.push(*event);
				let mut next = remaining.to_vec();
				next.remove(i);
				walk(events, &next);
				events.pop();
			}
		}
		walk(
			&mut Vec::new(),
			&[
				Event::SwitchToB,
				Event::ReturnToA,
				Event::FinishA,
				Event::FinishB,
				Event::Update,
				Event::Cancel,
			],
		);
	}

	/// A group of the window under the serving copy's resume point that the local floor keeps
	/// out and the publisher skipped is accounted for: the read ends once the rest is read.
	#[test]
	fn a_skip_under_the_resume_point_the_local_floor_keeps_out_ends_the_read() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		write(&a, 0);
		a.skip_groups(1..2);
		write(&a, 2);
		routes.serve(a.consume());
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_ranges([track::Range {
				start: track::Start::Time(0),
				end: Some(3),
			}]);
		let sub = logical
			.consume()
			.subscribe(subscription)
			.now_or_never()
			.unwrap()
			.unwrap();
		let mut sub = sub;
		assert_eq!(drain(&mut sub, false), [0, 2]);
		let b = copy();
		write(&b, 3);
		routes.serve(b.consume());
		let mut sub = sub.with_range(2..4);
		assert_eq!(drain(&mut sub, false), [3]);
		assert!(matches!(
			sub.poll_recv_group(&kio::Waiter::noop()),
			Poll::Ready(Ok(None))
		));
	}

	/// As above, with the skip only the start of a gap the reader lacks: the rest of the gap,
	/// a group the replaced copy dropped, is inside the local floor, so only the skip is needed.
	#[kio::test]
	async fn a_skip_starting_a_gap_the_local_floor_keeps_out_ends_the_read() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		write(&a, 0);
		a.skip_groups(1..2);
		a.drop_groups(2..3, StreamError::NotFound);
		write(&a, 3);
		routes.serve(a.consume());
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_ranges([track::Range {
				start: track::Start::Time(0),
				end: Some(4),
			}]);
		let mut sub = logical
			.consume()
			.subscribe(subscription)
			.now_or_never()
			.unwrap()
			.unwrap();
		assert_eq!(drain(&mut sub, false), [0, 3]);
		let b = copy();
		write(&b, 4);
		routes.serve(b.consume());
		let mut sub = sub.with_range(2..5);
		assert_eq!(drain(&mut sub, false), [4]);
		assert!(
			sub.poll_recv_group(&kio::Waiter::noop()).is_pending(),
			"the replaced copy may still deliver 2"
		);
		// Past the grace a late group is waited for, the replaced copy has nothing more and goes.
		kio::clock::sleep(HOUR).await;
		assert!(matches!(sub.poll_recv_group(&fresh()), Poll::Ready(Ok(None))));
	}

	/// As above, with the kept-out start of the gap skipped in parts: the replaced copy knows
	/// one, the serving copy the other, and neither knows it whole.
	#[kio::test]
	async fn a_skip_split_between_a_replaced_and_the_serving_copy_ends_the_read() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		write(&a, 0);
		a.drop_groups(1..2, StreamError::NotFound);
		a.skip_groups(2..3);
		a.drop_groups(3..4, StreamError::NotFound);
		write(&a, 4);
		routes.serve(a.consume());
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_ranges([track::Range {
				start: track::Start::Time(0),
				end: Some(5),
			}]);
		let mut sub = logical
			.consume()
			.subscribe(subscription)
			.now_or_never()
			.unwrap()
			.unwrap();
		assert_eq!(drain(&mut sub, false), [0, 4]);
		let b = copy();
		b.skip_groups(1..2);
		write(&b, 5);
		routes.serve(b.consume());
		let mut sub = sub.with_range(3..6);
		assert_eq!(drain(&mut sub, false), [5]);
		assert!(
			sub.poll_recv_group(&kio::Waiter::noop()).is_pending(),
			"the replaced copy may still deliver 3"
		);
		kio::clock::sleep(HOUR).await;
		assert!(matches!(sub.poll_recv_group(&fresh()), Poll::Ready(Ok(None))));
	}

	/// A skip declared late on a replaced copy kept only for a group still read from it wakes
	/// the read, though that copy is past its budget and no longer polled for groups.
	#[kio::test]
	async fn a_late_skip_on_a_kept_copy_wakes_the_read() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		write(&a, 0);
		a.drop_groups(1..2, StreamError::NotFound);
		a.drop_groups(3..5, StreamError::NotFound);
		let mut open = a.create_group(group::Info { start: 5 }).unwrap();
		open.write_frame(ts(5), b"x".as_ref()).unwrap();
		routes.serve(a.consume());
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_ranges([track::Range {
				start: track::Start::Time(0),
				end: Some(6),
			}]);
		let mut sub = logical
			.consume()
			.subscribe(subscription)
			.now_or_never()
			.unwrap()
			.unwrap();
		assert_eq!(recv(&mut sub).start, 0);
		let reading = recv(&mut sub);
		assert_eq!(reading.start, 5);
		let b = copy();
		b.skip_groups(1..2);
		write(&b, 6);
		routes.serve(b.consume());
		let mut sub = sub.with_range(3..7);
		assert_eq!(drain(&mut sub, false), [6]);
		kio::clock::sleep(HOUR).await;
		let wake = Arc::new(Wake::default());
		let waiter = kio::Waiter::new(wake.clone().into());
		assert!(sub.poll_recv_group(&waiter).is_pending(), "2 is still owed");
		wake.0.store(false, std::sync::atomic::Ordering::Relaxed);
		a.skip_groups(2..3);
		assert!(
			wake.0.load(std::sync::atomic::Ordering::Relaxed),
			"the skip wakes the read"
		);
		assert!(matches!(sub.poll_recv_group(&waiter), Poll::Ready(Ok(None))));
		drop(reading);
	}

	/// A skip known only to a route an inner front replaced counts for an outer front reading
	/// that inner front as a copy, after the outer front switches away from it too.
	#[kio::test]
	async fn a_skip_behind_an_inner_switch_ends_the_read() {
		let routes = Producer::new();
		let front = logical(&routes);
		let inner_routes = Producer::new();
		let inner = logical(&inner_routes);
		let a = copy();
		write(&a, 0);
		a.skip_groups(1..2);
		write(&a, 2);
		inner_routes.serve(a.consume());
		routes.serve(inner.consume());
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_ranges([track::Range {
				start: track::Start::Time(0),
				end: Some(4),
			}]);
		let mut sub = front.consume().subscribe(subscription).now_or_never().unwrap().unwrap();
		assert_eq!(drain(&mut sub, false), [0, 2]);
		let b = copy();
		write(&b, 3);
		inner_routes.serve(b.consume());
		assert_eq!(drain(&mut sub, false), [3]);
		let c = copy();
		write(&c, 4);
		routes.serve(c.consume());
		let mut sub = sub.with_range(2..5);
		assert_eq!(drain(&mut sub, false), [4]);
		kio::clock::sleep(HOUR).await;
		assert!(matches!(sub.poll_recv_group(&fresh()), Poll::Ready(Ok(None))));
	}

	/// As above, with the replaced route behind an inner front: its skip is read through that
	/// front's serving copy.
	#[test]
	fn a_skip_behind_an_inner_front_the_local_floor_keeps_out_ends_the_read() {
		let routes = Producer::new();
		let front = logical(&routes);
		let a = copy();
		write(&a, 0);
		a.skip_groups(1..2);
		write(&a, 2);
		let inner_routes = Producer::new();
		let inner = logical(&inner_routes);
		inner_routes.serve(a.consume());
		routes.serve(inner.consume());
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_ranges([track::Range {
				start: track::Start::Time(0),
				end: Some(3),
			}]);
		let mut sub = front.consume().subscribe(subscription).now_or_never().unwrap().unwrap();
		assert_eq!(drain(&mut sub, false), [0, 2]);
		let b = copy();
		write(&b, 3);
		routes.serve(b.consume());
		let mut sub = sub.with_range(2..4);
		assert_eq!(drain(&mut sub, false), [3]);
		assert!(matches!(
			sub.poll_recv_group(&kio::Waiter::noop()),
			Poll::Ready(Ok(None))
		));
	}

	/// On one route, a group of the window that the local floor keeps out and the publisher
	/// skipped is the serving copy's to account for: the read ends once the rest is read.
	#[test]
	fn a_skip_under_the_local_floor_ends_a_single_route_read() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		a.skip_groups(0..1);
		write(&a, 1);
		write(&a, 2);
		routes.serve(a.consume());
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_ranges([track::Range {
				start: track::Start::Time(0),
				end: Some(2),
			}]);
		let mut sub = logical
			.consume()
			.subscribe(subscription)
			.now_or_never()
			.unwrap()
			.unwrap()
			.with_range(1..3);
		assert_eq!(drain(&mut sub, false), [1, 2]);
		assert!(matches!(
			sub.poll_recv_group(&kio::Waiter::noop()),
			Poll::Ready(Ok(None))
		));
	}

	/// With overlapping ranges, a group the publisher skipped above the serving copy's resume
	/// point is accounted for through either range, wherever an earlier copy placed the other.
	#[test]
	fn a_skip_between_overlapping_placements_ends_the_read() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		a.skip_groups(0..6);
		write(&a, 6);
		routes.serve(a.consume());
		let subscription = Subscription::default()
			.with_delay(Delay::default().with_max(HOUR))
			.with_ranges([
				track::Range {
					start: track::Start::Time(8),
					end: Some(8),
				},
				track::Range {
					start: track::Start::Time(0),
					end: Some(8),
				},
			]);
		let mut sub = logical
			.consume()
			.subscribe(subscription)
			.now_or_never()
			.unwrap()
			.unwrap();
		assert_eq!(drain(&mut sub, false), [6]);
		let b = copy();
		b.skip_groups(0..6);
		routes.serve(b.consume());
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());
		b.skip_groups(7..8);
		write(&b, 8);
		write(&b, 9);
		assert_eq!(drain(&mut sub, false), [8]);
		assert!(matches!(
			sub.poll_recv_group(&kio::Waiter::noop()),
			Poll::Ready(Ok(None))
		));
	}
}
