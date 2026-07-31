//! Splice multiple per-session tracks into one logical track, switching at frame
//! boundaries, so a subscription survives route and connection changes.
//!
//! A [`Producer`] holds an ordered list of segments, each a [`track::Consumer`]
//! bounded to a half-open range of [`Position`]s. [`Producer::switch`] appends a
//! segment starting at position `P` and caps the previous one there, so the segments
//! always partition the track. A [`Subscriber`] reads across the segments as if they
//! were one track: bounds are enforced on the read side (a route delivering outside
//! its range is silently filtered), a segment dying stalls the subscriber instead of
//! erroring (the next [`Producer::switch`] resumes it), and demand is forwarded to
//! each underlying track intersected with its segment bounds, so a session serving a
//! segment just sees an ordinary subscription that happens to start or end at a
//! boundary.
//!
//! Boundaries are frame-precise, so a takeover does not have to wait for the next
//! group. When one lands inside a group the subscriber has already handed out, the
//! group itself is spliced: [`Group`] reads each route's copy in turn and the reader
//! sees one continuous frame stream. This is what lets a track whose current group
//! stays open indefinitely (a JSON append log, a catalog with deltas) survive a route
//! change at all.

use std::task::{Poll, ready};

use crate::{Datagram, Error, Result, frame, group, track};

use super::subscription::{Position, Subscription, min_some};

/// One spliced source: a track bounded to a half-open range of positions.
#[derive(Clone)]
struct Segment {
	/// Monotonic id, used by subscribers to reconcile their cursor set.
	id: u64,
	/// First position this segment serves, or `None` for no lower bound (the
	/// initial segment, which may start at the live edge).
	start: Option<Position>,
	/// First position this segment no longer serves, or `None` while it is the
	/// newest segment. Exclusive, so it is exactly the next segment's `start`.
	end: Option<Position>,
	/// The underlying per-session track.
	track: track::Consumer,
}

impl Segment {
	/// Whether this segment serves `position`.
	fn covers(&self, position: Position) -> bool {
		self.start.is_none_or(|start| position >= start) && self.end.is_none_or(|end| position < end)
	}

	/// One past the last position this segment produced within its own range, or
	/// `None` if nothing in range exists yet (out-of-range groups, e.g. a fetch into
	/// the track below the segment's start, don't count).
	fn produced(&self) -> Option<Position> {
		let produced = self.track.resume_position()?;
		// Nothing in range yet. An unbounded segment starts at the very first frame, so
		// a track holding only an empty group still counts as having produced nothing.
		if produced <= self.start.unwrap_or_default() {
			return None;
		}
		Some(match self.end {
			Some(end) => produced.min(end),
			None => produced,
		})
	}
}

/// The demand to register on an underlying track: the subscriber's own
/// preferences intersected with a segment's bounds.
fn slice(prefs: &Subscription, start: Option<Position>, end: Option<Position>) -> Subscription {
	let mut sub = prefs.clone();
	sub.set_start(max_unbounded_start(prefs.start(), start));
	// The segment bound is exclusive and a subscription's is inclusive.
	sub.set_end(min_some(prefs.end(), end.map(Position::before)));
	sub
}

/// The later of two optional start bounds, treating `None` as "the live edge" for the
/// preference and "no lower bound" for the segment. Either way the other one wins.
fn max_unbounded_start(prefs: Option<Position>, segment: Option<Position>) -> Option<Position> {
	match (prefs, segment) {
		(Some(a), Some(b)) => Some(a.max(b)),
		(Some(a), None) | (None, Some(a)) => Some(a),
		(None, None) => None,
	}
}

/// How many segments a logical track keeps before pruning terminal ones from the
/// front: the live segment plus a couple of predecessors still draining to slow
/// readers. Without a bound, every failover leaves one dead segment (pinning a
/// dead session's [`track::Consumer`] and cache) behind for the life of the track.
const MAX_SEGMENTS: usize = 3;

struct ResumeState {
	/// Segments in switch order; ranges are disjoint and ascending.
	segments: Vec<Segment>,
	/// Everything below this position was covered by segments that have since been
	/// pruned: no future segment can serve it (boundaries only move forward), so
	/// readers below it give up rather than parking for a replacement. `None` until
	/// the first prune; reset by [`Producer::release`] along with the segments.
	pruned: Option<Position>,
	/// Bumped on every mutation so subscribers know to reconcile.
	epoch: u64,
	/// No more switches will happen; the logical track ends with its last segment.
	finished: bool,
	/// The logical track was aborted; surfaced to every subscriber.
	abort: Option<Error>,
}

impl Default for ResumeState {
	fn default() -> Self {
		Self {
			segments: Vec::new(),
			pruned: None,
			epoch: 1,
			finished: false,
			abort: None,
		}
	}
}

impl ResumeState {
	/// One past the newest position across the segments, clamped to their bounds: where
	/// a replacement segment should pick the track up.
	///
	/// The pruned floor participates: a pruned segment produced exactly through its
	/// cap, so dropping it must not let the boundary collapse below what it served
	/// (a takeover would re-splice under the delivered edge).
	fn resume_position(&self) -> Option<Position> {
		self.segments
			.iter()
			.filter_map(Segment::produced)
			.chain(self.pruned)
			.max()
	}

	/// The latest group sequence across the segments, clamped to their bounds.
	fn latest(&self) -> Option<u64> {
		let position = self.resume_position()?;
		match position.frame {
			// The resume point sits at the head of a group, so the newest group actually
			// produced is the one below it.
			0 => position.group.checked_sub(1),
			_ => Some(position.group),
		}
	}

	/// Append a segment serving the track from `start` onward, capping (or replacing)
	/// the previous segments so the ranges stay disjoint and ascending.
	fn switch(&mut self, track: track::Consumer, start: Option<Position>) -> Result<()> {
		if !self.segments.is_empty() {
			// A boundary is required once a segment exists.
			let Some(start) = start else {
				return Err(crate::coding::BoundsExceeded.into());
			};

			// Segments the new range fully covers are replaced outright, provided
			// they never produced anything in range (nothing to splice around).
			while let Some(prev) = self.segments.last() {
				let prev_start = prev.start.unwrap_or_default();
				if start > prev_start {
					break;
				}
				if prev.produced().is_some() {
					return Err(crate::coding::BoundsExceeded.into());
				}
				self.segments.pop();
			}

			// Cap whatever remains at the boundary. The bound is exclusive, so the new
			// segment's start is exactly the previous segment's end. The cap may land
			// below the produced edge (a manual boundary re-serving delivered
			// positions from the replacement); readers reconcile on every frame, so a
			// moved cap re-routes them, and the subscriber's floor dedups re-delivery.
			if let Some(prev) = self.segments.last_mut() {
				prev.end = Some(start);
			}
		}

		let id = self.epoch;
		self.segments.push(Segment {
			id,
			start,
			end: None,
			track,
		});
		self.epoch += 1;
		self.prune();
		Ok(())
	}

	/// Drop retired segments from the front once the list outgrows
	/// [`MAX_SEGMENTS`], recording the range they covered in [`Self::pruned`].
	///
	/// A front segment is retired when it owes nothing more: it produced through
	/// its cap (a takeover boundary is the resume position, so this is every
	/// takeover-capped segment, alive or not) or its track is terminal. What it
	/// holds is a cache for slow readers, and a reader mid-drain keeps its own
	/// positioned cursor (see [`Group::poll_current`]'s missing-segment
	/// fallback). Only a manually spliced boundary can sit above the produced
	/// edge; that segment is still expected to backfill, so it blocks the sweep
	/// (and the segments behind it) until it does or dies.
	fn prune(&mut self) {
		while self.segments.len() > MAX_SEGMENTS {
			let front = &self.segments[0];
			let Some(end) = front.end else { break };
			let owes_more =
				front.produced() < Some(end) && front.track.poll_complete(&kio::Waiter::noop()).is_pending();
			if owes_more {
				break;
			}
			self.pruned = self.pruned.max(Some(end));
			self.segments.remove(0);
		}
	}
}

/// Splices tracks into one logical track by switching at group boundaries.
///
/// Created with [`Self::new`]; hand out read access via [`Self::consume`]. Call
/// [`Self::switch`] (or [`Self::takeover`]) whenever the serving route changes;
/// subscribers migrate transparently. The producer only manages boundaries: the
/// actual groups are written by whoever owns each underlying [`track::Producer`].
#[derive(Clone, Default)]
pub struct Producer {
	state: kio::Producer<ResumeState>,
}

impl Producer {
	/// Create a logical track with no segments; subscribers stall until the first
	/// [`Self::switch`].
	pub fn new() -> Self {
		Self::default()
	}

	/// Splice in a track serving the track from `start` onward, capping the previous
	/// segment there.
	///
	/// The first switch may pass `None` to leave the segment unbounded (it serves
	/// whatever the subscriber asks for, typically the live edge). Every later
	/// switch must pass `Some(start)`. A previous segment whose range the new one
	/// fully covers is replaced outright, provided it never produced anything in
	/// range (there is nothing to splice around); otherwise the boundary must
	/// advance past it, or this fails with [`Error::BoundsExceeded`] and the
	/// segment list is unchanged.
	///
	/// Bounds are enforced when reading: a previous segment's session may keep
	/// delivering past its new cap (the switch races the network) and those groups
	/// and frames are simply never surfaced.
	// Production callers go through `takeover`; this is the entry point an explicit
	// wire-driven boundary (a future manual-splice surface) would use, and the
	// boundary tests drive it directly.
	#[cfg_attr(not(test), expect(dead_code))]
	pub fn switch(
		&mut self,
		track: impl super::origin_impl::Consume<track::Consumer>,
		start: impl Into<Option<Position>>,
	) -> Result<()> {
		let track = track.consume();
		let start = start.into();
		let mut state = self.state.write().map_err(|_| Error::Dropped)?;
		if state.finished || state.abort.is_some() {
			return Err(Error::Closed);
		}
		state.switch(track, start)
	}

	/// Splice in a track that resumes wherever the current segments stop: one past
	/// the newest spliced frame.
	///
	/// This is [`Self::switch`] with the boundary computed from the current state,
	/// for callers reacting to a route change rather than choosing a boundary. The
	/// boundary is frame-precise, so a group that was mid-transfer when its route
	/// died is continued rather than abandoned: the replacement is asked for the
	/// frames from the break onward, and subscribers read across the two copies as
	/// one group. It rolls to the next group only once the current one is complete,
	/// since nothing more can be appended to it.
	pub fn takeover(&mut self, track: impl super::origin_impl::Consume<track::Consumer>) -> Result<()> {
		let track = track.consume();
		// Compute the boundary and apply it under one write guard: a boundary
		// computed under a separate read lock could race the old route delivering
		// more frames, splicing the new segment below the delivered edge.
		let mut state = self.state.write().map_err(|_| Error::Dropped)?;
		if state.finished || state.abort.is_some() {
			return Err(Error::Closed);
		}
		let start = if state.segments.is_empty() {
			None
		} else {
			// Segments exist but never produced anything: replace them, which the very
			// first position does.
			Some(state.resume_position().unwrap_or_default())
		};
		state.switch(track, start)
	}

	/// Drop every segment, releasing the underlying tracks while keeping the
	/// logical track alive for a later [`Self::takeover`].
	///
	/// For a track nobody is reading: releasing the last consumer of a segment lets
	/// the serving session tear its copy down, so an idle track stops costing an
	/// upstream subscription and a cached [`track::Info`]. The next takeover starts
	/// unbounded again, since with no segments there is no boundary to splice
	/// around.
	pub(crate) fn release(&mut self) -> Result<()> {
		let mut state = self.state.write().map_err(|_| Error::Dropped)?;
		if state.finished || state.abort.is_some() {
			return Err(Error::Closed);
		}
		if state.segments.is_empty() {
			return Ok(());
		}
		state.segments.clear();
		// The next takeover starts unbounded and may restart the numbering, so a
		// floor from the old numbering must not cut into it.
		state.pruned = None;
		state.epoch += 1;
		Ok(())
	}

	/// Whether any segment is spliced in, and so whether there is anything for
	/// [`Self::release`] to drop.
	///
	/// A segment outlives the route that produced it: the source can leave, or its
	/// copy can die, while the segment stays spliced so readers keep what it already
	/// delivered. That makes this, not the caller's own handle on the serving route,
	/// the condition for arming an idle release.
	pub(crate) fn is_spliced(&self) -> bool {
		!self.state.read().segments.is_empty()
	}

	/// Mark the logical track as complete: no further switches. Subscribers see a
	/// clean end once the final segment's track finishes.
	pub fn finish(&mut self) -> Result<()> {
		let mut state = self.state.write().map_err(|_| Error::Dropped)?;
		if state.finished || state.abort.is_some() {
			return Err(Error::Closed);
		}
		state.finished = true;
		state.epoch += 1;
		Ok(())
	}

	/// Abort the logical track, releasing every subscriber with `err`.
	///
	/// Fails once the track [`finish`](Self::finish)ed: a clean end is terminal,
	/// so a late abort (e.g. route churn re-queueing an already-completed track)
	/// cannot turn it into an error for subscribers still draining.
	pub fn abort(&mut self, err: Error) -> Result<()> {
		let mut state = self.state.write().map_err(|_| Error::Dropped)?;
		if state.finished || state.abort.is_some() {
			return Err(Error::Closed);
		}
		state.abort = Some(err);
		state.epoch += 1;
		Ok(())
	}

	/// Whether the logical track ended in an error, as opposed to still being
	/// servable or cleanly [`finish`](Self::finish)ed.
	pub(crate) fn is_aborted(&self) -> bool {
		self.state.read().abort.is_some()
	}

	/// Create a read handle for the logical track.
	pub fn consume(&self) -> Consumer {
		Consumer {
			state: self.state.consume(),
		}
	}

	/// Whether any read handle for the logical track currently exists.
	///
	/// This is the demand signal: a spliced track with no consumers is cached
	/// state nobody is watching.
	pub fn is_used(&self) -> bool {
		self.state.is_used()
	}

	/// Poll for a consumer appearing, parking `waiter` until one does. Ready
	/// immediately once one exists (or the track closed). Feeds
	/// [`crate::broadcast::Demand`], which recomputes on wake.
	pub(crate) fn poll_used(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.state.poll_used(waiter).map(|_| ())
	}

	/// Poll for the last consumer going away, parking `waiter` until it does.
	/// Ready immediately once none remain (or the track closed).
	pub(crate) fn poll_unused(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.state.poll_unused(waiter).map(|_| ())
	}
}

/// A cheap, cloneable read handle for a spliced logical track.
#[derive(Clone)]
pub struct Consumer {
	state: kio::Consumer<ResumeState>,
}

impl Consumer {
	/// Open a live subscription across every segment.
	///
	/// The subscription's preferences are forwarded to each underlying track
	/// intersected with its segment bounds, so each serving session sees plain
	/// demand for its own range. Demand registers as the subscriber is polled.
	/// Pass `None` for [`Subscription::default`].
	#[cfg(test)]
	pub fn subscribe(&self, subscription: impl Into<Option<Subscription>>) -> Subscriber {
		let prefs = kio::Producer::new(subscription.into().unwrap_or_default());
		self.subscribe_shared(prefs)
	}

	/// Subscribe with an externally-owned preferences channel, so a
	/// [`track::SubscriberControl`]-style handle can update it.
	pub(crate) fn subscribe_shared(&self, prefs: kio::Producer<Subscription>) -> Subscriber {
		let last_prefs = prefs.read().clone();
		Subscriber {
			state: self.state.clone(),
			prefs,
			last_prefs,
			epoch: 0,
			finished: false,
			abort: None,
			segments: Vec::new(),
			next_sequence: 0,
			min_sequence: 0,
			end_sequence: None,
			reading: None,
		}
	}

	/// Poll for the track's [`track::Info`], resolved from the first segment.
	///
	/// Stays pending until a segment exists and its track's info is known (the
	/// serving session may not have accepted it yet).
	pub fn poll_info(&self, waiter: &kio::Waiter) -> Poll<Result<track::Info>> {
		// Wait for the first segment (or a terminal state), then poll its info.
		let track = match self.state.poll(waiter, |state| {
			if state.abort.is_some() || !state.segments.is_empty() {
				Poll::Ready(
					state
						.abort
						.clone()
						.map_or_else(|| Ok(state.segments[0].track.clone()), Err),
				)
			} else {
				Poll::Pending
			}
		}) {
			Poll::Ready(Ok(res)) => res?,
			Poll::Ready(Err(state)) => match (&state.abort, state.segments.first()) {
				(Some(err), _) => return Poll::Ready(Err(err.clone())),
				(None, Some(segment)) => segment.track.clone(),
				// Closed without ever getting a segment: nothing will resolve this.
				(None, None) => return Poll::Ready(Err(Error::Dropped)),
			},
			Poll::Pending => return Poll::Pending,
		};

		track.info().poll_ok(waiter)
	}

	/// Return the track's [`track::Info`], resolved from the first segment.
	#[cfg(test)]
	pub async fn info(&self) -> Result<track::Info> {
		kio::wait(|waiter| self.poll_info(waiter)).await
	}

	/// Fetch a single past group without a live subscription.
	///
	/// Routed to the most recent segment's track: old segments' sessions are
	/// usually gone by the time history is fetched, and a live route can serve
	/// groups outside its subscription bounds (bounds slice demand, not access).
	/// In-flight fetches on older segments are unaffected. With no segment yet
	/// (no route has served the track), the fetch waits for the first one.
	pub fn fetch_group(&self, sequence: u64, options: impl Into<Option<group::Fetch>>) -> kio::Pending<Fetching> {
		kio::Pending::new(Fetching {
			state: self.state.clone(),
			sequence,
			options: options.into().unwrap_or_default(),
			inner: web_async::Lock::new(None),
		})
	}

	/// The latest group sequence across the segments, clamped to their bounds.
	pub fn latest(&self) -> Option<u64> {
		self.state.read().latest()
	}

	/// One past the newest position across the segments: where a route taking this
	/// logical track over would resume.
	pub(crate) fn resume_position(&self) -> Option<Position> {
		self.state.read().resume_position()
	}
}

/// The pollable state of a [`Consumer::fetch_group`]; awaited via the
/// [`kio::Pending`] wrapper.
///
/// Waits for a segment to exist (no route may have served the track yet), then
/// issues the fetch against the newest segment's track and resolves with it.
pub struct Fetching {
	state: kio::Consumer<ResumeState>,
	sequence: u64,
	options: group::Fetch,
	// The underlying fetch, latched once a segment exists. Behind a shared lock
	// both to allow `&self` polling and to break the type recursion with
	// `track::Fetching` (which can wrap a resume [`Fetching`]).
	inner: web_async::Lock<Option<kio::Pending<track::Fetching>>>,
}

impl kio::Pollable for Fetching {
	type Output = Result<group::Consumer>;

	fn poll(&self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		let mut inner = self.inner.lock();

		if inner.is_none() {
			// Wait for the first segment; the newest wins if several arrived.
			let track = match self.state.poll(waiter, |s| {
				if s.abort.is_some() || !s.segments.is_empty() {
					Poll::Ready(match &s.abort {
						Some(err) => Err(err.clone()),
						None => Ok(s.segments.last().expect("nonempty").track.clone()),
					})
				} else {
					Poll::Pending
				}
			}) {
				Poll::Ready(Ok(res)) => res?,
				// The producer is gone; use whatever segment it froze with.
				Poll::Ready(Err(state)) => match (&state.abort, state.segments.last()) {
					(Some(err), _) => return Poll::Ready(Err(err.clone())),
					(None, Some(segment)) => segment.track.clone(),
					(None, None) => return Poll::Ready(Err(Error::NotFound)),
				},
				Poll::Pending => return Poll::Pending,
			};
			*inner = Some(track.fetch_group(self.sequence, self.options.clone()));
		}

		kio::Pollable::poll(&**inner.as_ref().expect("latched above"), waiter)
	}
}

/// The frames of `sequence` a `[start, end)` segment range serves, as a half-open index
/// range. `None` when the group falls outside the range entirely.
fn frames(start: Option<Position>, end: Option<Position>, sequence: u64) -> Option<(u64, Option<u64>)> {
	let start = match start {
		Some(start) if start.group > sequence => return None,
		Some(start) if start.group == sequence => start.frame,
		_ => 0,
	};
	let end = match end {
		// A boundary at the head of a later group leaves this one whole.
		Some(end) if end.group > sequence => None,
		Some(end) if end.group == sequence => Some(end.frame),
		Some(_) => return None,
		None => None,
	};
	Some((start, end))
}

/// A group assembled from several routes' copies, joined at frame boundaries.
///
/// Rather than being fed, it pulls: on every frame it asks the segment list which route
/// owns the next frame of this group and reads that route's copy. A boundary landing
/// mid-group therefore reaches an already-handed-out group with no bookkeeping, and a
/// reader that never touches the subscription again still picks the continuation up.
///
/// A copy that dies (or never arrives) stalls the group rather than erroring it, the
/// same way a dead segment stalls the track; the loss is only surfaced once no
/// replacement can arrive.
pub(crate) struct Group {
	state: kio::Consumer<ResumeState>,

	/// The logical group being assembled.
	sequence: u64,

	/// The index of the next frame to return.
	index: u64,

	/// Inclusive cap from [`Self::end_at`].
	end: Option<u64>,

	/// The route being read: its segment, the exclusive frame bound the segment puts on
	/// this group, and this reader's positioned copy.
	current: Option<Current>,

	/// The segment whose copy died under us, and why.
	dead: Option<(u64, Error)>,
}

struct Current {
	segment: u64,
	cap: Option<u64>,
	group: group::Consumer,
}

impl Clone for Group {
	fn clone(&self) -> Self {
		// Cursors are per-reader, so a clone re-resolves its own positioned copy and
		// then runs in parallel.
		Self {
			state: self.state.clone(),
			sequence: self.sequence,
			index: self.index,
			end: self.end,
			current: None,
			dead: self.dead.clone(),
		}
	}
}

impl Group {
	fn new(state: kio::Consumer<ResumeState>, sequence: u64, index: u64) -> Self {
		Self {
			state,
			sequence,
			index,
			end: None,
			current: None,
			dead: None,
		}
	}

	/// Pre-latch the delivering route's own copy, so the payload it already
	/// delivered survives even if its segment is pruned before the reader drains
	/// it. Costs nothing otherwise: the per-frame reuse check validates the latch
	/// against the live segment list, so a moved cap still re-routes the reader.
	fn latched(mut self, segment: u64, cap: Option<u64>, mut group: group::Consumer) -> Self {
		// The segment bound is exclusive; a group consumer's cap is inclusive.
		group.end_at(cap.map(|cap| cap.saturating_sub(1)));
		self.current = Some(Current { segment, cap, group });
		self
	}

	pub fn index(&self) -> u64 {
		self.index
	}

	pub fn start_at(&mut self, index: u64) {
		if index <= self.index {
			return;
		}
		self.index = index;
		// A different route may own the new cursor.
		self.current = None;
	}

	pub fn end_at(&mut self, index: Option<u64>) {
		self.end = index;
	}

	/// Point `current` at the route owning frame `index` of this group.
	///
	/// `Ready(Ok(false))` once the group can produce nothing more, and `Ready(Err(_))`
	/// only when the route that died is the last word on those frames.
	fn poll_current(&mut self, waiter: &kio::Waiter) -> Poll<Result<bool>> {
		loop {
			let position = Position {
				group: self.sequence,
				frame: self.index,
			};
			let dead = self.dead.as_ref().map(|(segment, _)| *segment);
			let sequence = self.sequence;

			// Scoped so the poll's state borrow ends before `self` is touched again.
			let found = {
				let located = self.state.poll(waiter, |state| {
					// Waiting for a replacement route only makes sense while one can still
					// arrive. A finished logical track has no more switches coming, and an
					// aborted one is over outright, so a dead copy is the end of the group
					// rather than a gap to park on.
					//
					// Nor can one arrive below the resume point. [`Producer::takeover`]
					// derives every boundary from [`ResumeState::resume_position`], which
					// only moves forward, so once it is past this position no future segment
					// will ever cover it and nobody will be asked for these frames again.
					//
					// Only once a route has been given up on, though: a live route that has
					// not delivered this group yet may still do so out of order, and its own
					// progress is what moved the resume point past us.
					let stranded = dead.is_some() && state.resume_position().is_some_and(|resume| resume > position);
					// Below the pruned floor no segment exists and none can arrive:
					// the coverage was dropped along with the segments that held it.
					let lost = state.pruned.is_some_and(|floor| position < floor);
					let terminal = state.finished || state.abort.is_some() || stranded || lost;
					match state.segments.iter().find(|segment| segment.covers(position)) {
						// The route that owns these frames died: wait for a replacement.
						Some(segment) if dead == Some(segment.id) => match terminal {
							true => Poll::Ready(None),
							false => Poll::Pending,
						},
						Some(segment) => Poll::Ready(Some((
							segment.id,
							segment.track.clone(),
							frames(segment.start, segment.end, sequence).map(|(_, end)| end),
						))),
						// No route owns them yet; park unless none is coming.
						None if terminal => Poll::Ready(None),
						None => Poll::Pending,
					}
				});

				match located {
					Poll::Ready(Ok(found)) => found,
					// The producer is gone, so the segment list is frozen.
					Poll::Ready(Err(_)) => None,
					Poll::Pending => return Poll::Pending,
				}
			};

			let Some((segment, track, Some(cap))) = found else {
				// No segment covers the position, but a latched copy still drains:
				// a pruned segment's cursor holds exactly the frames it owned (its
				// cap was its produced edge), so read it dry before giving up. Once
				// it ends or dies, `current` clears and the next resolve settles it.
				if self.current.is_some() {
					return Poll::Ready(Ok(true));
				}
				return Poll::Ready(self.give_up());
			};

			// Reuse the positioned handle while the route and its bound hold: it carries
			// a frame prefetch that re-resolving would throw away.
			if self
				.current
				.as_ref()
				.is_some_and(|current| current.segment == segment && current.cap == cap)
			{
				return Poll::Ready(Ok(true));
			}

			// The route may not have delivered this group yet, so wait on its cache.
			let Some(mut group) = ready!(track.poll_peek_group(sequence, waiter)) else {
				// This route will never have it; fall back to whichever segment replaces it.
				self.dead = Some((segment, Error::NotFound));
				continue;
			};

			// `start_at` clamps up to the first frame the copy still holds, so landing
			// higher than asked means this route can't cover the seam after all. Treat it
			// like a dead copy and wait for one that can.
			group.start_at(self.index);
			if group.index() != self.index {
				self.dead = Some((segment, Error::Lagged));
				continue;
			}

			// The segment bound is exclusive; a group consumer's cap is inclusive.
			group.end_at(cap.map(|cap| cap.saturating_sub(1)));
			self.current = Some(Current { segment, cap, group });
			self.dead = None;
			return Poll::Ready(Ok(true));
		}
	}

	/// No replacement can arrive: report the loss that stalled us, or a clean end.
	fn give_up(&mut self) -> Result<bool> {
		match self.dead.take() {
			Some((_, err)) => {
				// The only place a spliced group's loss becomes visible, so say which
				// frames went missing rather than leaving a stuck group to explain itself.
				tracing::warn!(
					group = self.sequence,
					frame = self.index,
					%err,
					"no route can serve the rest of this group"
				);
				Err(err)
			}
			None => Ok(false),
		}
	}

	/// Advance past a copy that ran out at its boundary, or report the group's end.
	/// `true` to keep reading from the next route.
	fn roll(&mut self) -> bool {
		let Some(current) = &self.current else {
			return false;
		};
		match current.cap {
			Some(cap) => {
				// Reaching the bound is the only clean end a bounded copy has: the
				// boundary was derived from what that route produced.
				debug_assert_eq!(self.index, cap, "bounded copy ended below its boundary");
				self.index = self.index.max(cap);
				self.current = None;
				true
			}
			// Unbounded: this route owns the rest of the group, so its end is the group's.
			None => false,
		}
	}

	/// Mark the current route dead so the reader waits for a replacement instead of
	/// failing the group outright.
	fn bury(&mut self, err: Error) {
		self.dead = self.current.take().map(|current| (current.segment, err));
	}

	pub fn poll_read_frame(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<frame::Frame>>> {
		loop {
			if self.end.is_some_and(|end| self.index > end) {
				return Poll::Ready(Ok(None));
			}
			if !ready!(self.poll_current(waiter))? {
				return Poll::Ready(Ok(None));
			}
			let current = self.current.as_mut().expect("resolved above");
			match ready!(current.group.poll_read_frame(waiter)) {
				Ok(Some(frame)) => {
					self.index += 1;
					return Poll::Ready(Ok(Some(frame)));
				}
				Ok(None) if self.roll() => continue,
				Ok(None) => return Poll::Ready(Ok(None)),
				Err(err) => self.bury(err),
			}
		}
	}

	pub fn poll_next_frame(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<frame::Consumer>>> {
		loop {
			if self.end.is_some_and(|end| self.index > end) {
				return Poll::Ready(Ok(None));
			}
			if !ready!(self.poll_current(waiter))? {
				return Poll::Ready(Ok(None));
			}
			let current = self.current.as_mut().expect("resolved above");
			match ready!(current.group.poll_next_frame(waiter)) {
				Ok(Some(frame)) => {
					self.index += 1;
					return Poll::Ready(Ok(Some(frame)));
				}
				Ok(None) if self.roll() => continue,
				Ok(None) => return Poll::Ready(Ok(None)),
				Err(err) => self.bury(err),
			}
		}
	}

	/// The logical group's total frame count, which only the unbounded tail copy knows:
	/// its own count already includes the frames it skipped.
	pub fn poll_finished(&mut self, waiter: &kio::Waiter) -> Poll<Result<u64>> {
		if !ready!(self.poll_current(waiter))? {
			return Poll::Ready(Ok(self.index));
		}
		let current = self.current.as_mut().expect("resolved above");
		if let Some(cap) = current.cap {
			// A bounded copy can't declare the end; the continuation does, unless
			// it can never arrive: once the seam at the cap is below the pruned
			// floor (or the track ended with nothing covering it), the cap is the
			// group's end. Checked here, not left to `poll_current`: a latched
			// bounded copy resolves without consulting the segment list, so this
			// is the poll that must park on it (or a caller that never drains to
			// the seam would hang with no waiter registered).
			let seam = Position {
				group: self.sequence,
				frame: cap,
			};
			let lost = self.state.poll(waiter, |state| {
				let lost = state.pruned.is_some_and(|floor| seam < floor);
				let terminal = state.finished || state.abort.is_some();
				match lost || (terminal && !state.segments.iter().any(|segment| segment.covers(seam))) {
					true => Poll::Ready(()),
					false => Poll::Pending,
				}
			});
			return match lost {
				// The producer is gone: the frozen list can never cover the seam.
				Poll::Ready(_) => Poll::Ready(Ok(cap)),
				Poll::Pending => Poll::Pending,
			};
		}
		current.group.poll_finished(waiter)
	}
}

/// A subscriber's cursor over one segment.
struct SegmentSub {
	id: u64,
	start: Option<Position>,
	end: Option<Position>,
	sub: SubState,
	/// The producer dropped this segment (pruned, or replaced before producing).
	/// The cursor drains what it already holds, then retires; see
	/// [`Self::retired`].
	pruned: bool,
	/// A received group held back by the subscriber's [`Subscriber::end_at`] cap,
	/// re-offered once the cap rises (arrival-order reads consume the underlying
	/// cursor, so the group is parked here instead of dropped).
	parked: Option<group::Consumer>,
}

impl SegmentSub {
	/// The first group this segment can serve, for the underlying read cursor.
	fn first_group(&self) -> u64 {
		self.start.map_or(0, |start| start.group)
	}

	/// The last group this segment can serve (inclusive), for the underlying read
	/// cursor. `None` while it is the newest segment.
	fn last_group(&self) -> Option<u64> {
		self.end.map(|end| end.before().group)
	}

	/// Whether a producer-dropped segment is spent and can be removed. A capped
	/// cursor is kept until it drains (it may hold delivered-but-unread groups);
	/// an uncapped one was replaced before producing, so it holds nothing. A
	/// parked group never blocks retirement: it is cleared when the segment is
	/// pruned, since its frames resolve through the pruned-away segment list and
	/// could no longer deliver anyway (see [`Subscriber::apply`]).
	fn retired(&self) -> bool {
		self.pruned && (self.end.is_none() || matches!(self.sub, SubState::Done(_)))
	}
}

enum SubState {
	/// Waiting for the underlying track's info (it may not be accepted yet).
	Pending(kio::Pending<track::Subscribing>),
	/// Live cursor over the underlying track.
	Active(track::Subscriber),
	/// The underlying track ended: `Some` with the group count when it finished
	/// cleanly, `None` when it aborted or was dropped. An abort is deliberately
	/// not surfaced: a dead route stalls the logical track until the next switch
	/// replaces it.
	Done(Option<u64>),
}

/// A live subscription spliced across every segment of a logical track.
///
/// Reads switch between the underlying [`track::Subscriber`]s at the segment
/// boundaries. A segment's session failing does not error the subscription; it
/// stalls until [`Producer::switch`] provides a replacement, or ends cleanly once
/// the producer [`finish`](Producer::finish)es and the final segment completes.
pub struct Subscriber {
	state: kio::Consumer<ResumeState>,

	/// This subscriber's preferences; shared with control handles, so changes are
	/// picked up in [`Self::poll_sync`] and re-sliced onto every segment.
	prefs: kio::Producer<Subscription>,
	last_prefs: Subscription,

	/// Last observed producer epoch; a mismatch triggers a reconcile.
	epoch: u64,
	finished: bool,
	abort: Option<Error>,

	/// Cursors over the segments, in segment order.
	segments: Vec<SegmentSub>,

	/// One past the highest sequence returned by [`Self::next_group`].
	next_sequence: u64,
	/// Minimum sequence to surface, set by [`Self::start_at`].
	min_sequence: u64,
	/// Inclusive cap for [`Self::next_group`], set by [`Self::end_at`].
	end_sequence: Option<u64>,

	/// The group currently being drained by [`Self::read_frame`].
	reading: Option<group::Consumer>,
}

impl Subscriber {
	/// Sync with the producer and preferences: pick up new segments, apply moved
	/// boundaries, re-slice demand, and register the waiter for the next change.
	fn poll_sync(&mut self, waiter: &kio::Waiter) {
		// Preference changes re-derive every segment's demand. Loop: a poll that
		// consumes a change leaves no waiter registered, so re-poll until Pending
		// (mirroring the state loop below), or the next update is silently lost.
		loop {
			let prefs = {
				let last = &self.last_prefs;
				match self
					.prefs
					.poll(waiter, |p| if **p != *last { Poll::Ready(()) } else { Poll::Pending })
				{
					Poll::Ready(Ok(guard)) => (*guard).clone(),
					Poll::Ready(Err(_)) | Poll::Pending => break,
				}
			};
			self.last_prefs = prefs;
			for seg in &mut self.segments {
				if let SubState::Active(sub) = &mut seg.sub {
					let _ = sub.update(slice(&self.last_prefs, seg.start, seg.end));
				}
			}
		}

		loop {
			let epoch = self.epoch;
			// Snapshot inside the predicate: `kio::Consumer::poll` yields the
			// predicate's value on change, or the final state once closed. Inline
			// the poll so its state borrow ends with this statement.
			let (snapshot, closed) = match self.state.poll(waiter, |state| {
				if state.epoch != epoch {
					Poll::Ready((state.epoch, state.finished, state.abort.clone(), state.segments.clone()))
				} else {
					Poll::Pending
				}
			}) {
				Poll::Ready(Ok(snapshot)) => (Some(snapshot), false),
				// The producer is gone; the state is frozen, so reconcile one last
				// time and stop watching (existing segments can still drain).
				Poll::Ready(Err(state)) => {
					let snapshot = (state.epoch != epoch)
						.then(|| (state.epoch, state.finished, state.abort.clone(), state.segments.clone()));
					(snapshot, true)
				}
				// Unchanged, and the waiter is now registered for the next switch.
				Poll::Pending => return,
			};

			if let Some(snapshot) = snapshot {
				self.apply(snapshot);
			}
			if closed {
				return;
			}
			// Loop: re-poll so the waiter is registered for the next change.
		}
	}

	/// Apply a producer snapshot: move boundaries on known segments and subscribe
	/// to new ones.
	fn apply(&mut self, snapshot: (u64, bool, Option<Error>, Vec<Segment>)) {
		let (epoch, finished, abort, segments) = snapshot;
		self.epoch = epoch;
		self.finished = finished;
		self.abort = abort;

		// Mark segments the producer dropped: replaced (never produced anything,
		// so their cursor holds nothing and retires at once) or pruned (capped;
		// the cursor may still hold delivered-but-unread groups, so it drains
		// before retiring; see `poll_segment`). A parked group is dropped with
		// the prune: re-offering it would hand out a group whose frames resolve
		// through the segment list (see `hand_out`), where the pruned range is
		// lost, so it could only ever deliver an empty husk while pinning the
		// cursor (and its track subscription) forever.
		for s in &mut self.segments {
			s.pruned = !segments.iter().any(|n| n.id == s.id);
			if s.pruned {
				s.parked = None;
			}
		}
		self.segments.retain(|s| !s.retired());

		for segment in segments {
			match self.segments.iter_mut().find(|s| s.id == segment.id) {
				Some(existing) => {
					if existing.end != segment.end {
						existing.end = segment.end;
						let cap = min_some(existing.last_group(), self.end_sequence);
						if let SubState::Active(sub) = &mut existing.sub {
							sub.end_at(cap);
							// Also shrink the demand so the session can cap upstream.
							let _ = sub.update(slice(&self.last_prefs, segment.start, segment.end));
						}
						// A still-pending subscription picks the moved boundary up
						// when it activates (see `poll_activate`). Groups already handed
						// out need nothing: a [`Group`] re-reads the segment list on
						// every frame, so it sees the new boundary by itself.
					}
				}
				None => {
					let sub = segment
						.track
						.subscribe(slice(&self.last_prefs, segment.start, segment.end));
					self.segments.push(SegmentSub {
						id: segment.id,
						start: segment.start,
						end: segment.end,
						sub: SubState::Pending(sub),
						pruned: false,
						parked: None,
					});
				}
			}
		}
	}

	/// Wrap a group just received from a segment so the reader can follow it across
	/// later route changes.
	///
	/// Returns `None` when the copy continues a group already handed out: its segment
	/// starts partway into the group, which only happens after an earlier segment served
	/// the head. Those frames reach the reader through the group it already holds, so
	/// surfacing the copy again would duplicate them.
	fn hand_out(&self, segment: usize, group: group::Consumer) -> Option<group::Consumer> {
		let seg = &self.segments[segment];
		let sequence = group.sequence;
		let (start, end) = frames(seg.start, seg.end, sequence)?;
		if start != 0 {
			return None;
		}
		// Latch the delivering copy: the spliced reader otherwise re-resolves it
		// through the segment list, which forgets this route the moment its
		// segment is pruned, turning a group the cursor already delivered into an
		// empty husk.
		let spliced = Group::new(self.state.clone(), sequence, 0).latched(seg.id, end, group.clone());
		Some(group.into_spliced(spliced))
	}

	/// Resolve a segment's pending subscription, if any. Ready once the segment is
	/// `Active` or `Done`; a rejected or closed track becomes `Done` (stall, not
	/// error). Never consumes groups, so terminal-state pollers can share it.
	fn poll_activate(
		seg: &mut SegmentSub,
		prefs: &Subscription,
		min_sequence: u64,
		end_sequence: Option<u64>,
		waiter: &kio::Waiter,
	) -> Poll<()> {
		if let SubState::Pending(pending) = &mut seg.sub {
			match pending.poll_ok(waiter) {
				Poll::Ready(Ok(mut sub)) => {
					// Enforce the bounds on the read cursor, and re-slice demand in
					// case a boundary moved while the subscription was pending.
					sub.start_at(seg.first_group().max(min_sequence));
					sub.end_at(min_some(seg.last_group(), end_sequence));
					let _ = sub.update(slice(prefs, seg.start, seg.end));
					seg.sub = SubState::Active(sub);
				}
				// The underlying track was rejected or closed: stall, not error.
				Poll::Ready(Err(_)) => seg.sub = SubState::Done(None),
				Poll::Pending => return Poll::Pending,
			}
		}
		Poll::Ready(())
	}

	/// Drive one segment cursor: resolve a pending subscription, then poll for an
	/// in-bounds group. Out-of-bounds groups (a route racing its cap) are skipped.
	fn poll_segment(
		seg: &mut SegmentSub,
		prefs: &Subscription,
		min_sequence: u64,
		end_sequence: Option<u64>,
		waiter: &kio::Waiter,
	) -> Poll<Option<group::Consumer>> {
		loop {
			match &mut seg.sub {
				SubState::Pending(_) => {
					ready!(Self::poll_activate(seg, prefs, min_sequence, end_sequence, waiter));
				}
				SubState::Active(sub) => match sub.poll_recv_group(waiter) {
					Poll::Ready(Ok(Some(group))) => {
						// `start_at` already floors the cursor; enforce the cap here since
						// arrival-order reads don't honor `end_at`.
						if let Some(end) = seg.last_group()
							&& group.sequence > end
						{
							continue;
						}
						return Poll::Ready(Some(group));
					}
					Poll::Ready(Ok(None)) => {
						let count = sub.poll_finished(waiter).map(|res| res.ok());
						let count = match count {
							Poll::Ready(count) => count,
							Poll::Pending => None,
						};
						seg.sub = SubState::Done(count);
						return Poll::Ready(None);
					}
					// A dead segment stalls the logical track rather than erroring;
					// the next switch resumes it.
					Poll::Ready(Err(_)) => {
						seg.sub = SubState::Done(None);
						return Poll::Ready(None);
					}
					// An empty cursor on a pruned segment is NOT proof it drained:
					// groups below the cap may still arrive out of order, and this
					// cursor's demand is what keeps the upstream serving them. The
					// reap in `poll_recv_group` bounds how many such stragglers may
					// linger instead.
					Poll::Pending => return Poll::Pending,
				},
				SubState::Done(_) => return Poll::Ready(None),
			}
		}
	}

	/// Poll for the next group in arrival order across the segments.
	///
	/// A group that continues one already returned (a boundary landed inside it) is
	/// spliced onto that group rather than surfaced again, so each sequence is handed
	/// out exactly once no matter how many routes contributed to it.
	///
	/// Returns `Poll::Ready(Ok(None))` once the producer finished and every
	/// segment completed, and `Poll::Ready(Err(_))` only if the producer aborted.
	pub fn poll_recv_group(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<group::Consumer>>> {
		self.poll_sync(waiter);

		// Reap retired cursors, then bound the live stragglers: a pruned
		// segment's cursor keeps draining (groups below its cap may still arrive
		// out of order, and its demand keeps the upstream serving them), but only
		// the newest few. Beyond the bound the oldest are cut, mirroring the
		// producer-side policy: a reader that far behind loses the range.
		self.segments.retain(|s| !s.retired());
		let mut cut = self
			.segments
			.iter()
			.filter(|s| s.pruned)
			.count()
			.saturating_sub(MAX_SEGMENTS);
		if cut > 0 {
			for seg in &mut self.segments {
				if cut == 0 {
					break;
				}
				if seg.pruned {
					seg.sub = SubState::Done(None);
					seg.parked = None;
					cut -= 1;
				}
			}
			self.segments.retain(|s| !s.retired());
		}

		let end_sequence = self.end_sequence;
		let beyond_cap = |sequence: u64| end_sequence.is_some_and(|end| sequence > end);

		let mut all_done = true;
		for index in 0..self.segments.len() {
			// Re-offer a group parked at the cap once the cap rises.
			let parked = self.segments[index].parked.take_if(|group| !beyond_cap(group.sequence));
			if let Some(group) = parked
				&& group.sequence >= self.min_sequence
			{
				let sequence = group.sequence;
				if let Some(group) = self.hand_out(index, group) {
					self.next_sequence = self.next_sequence.max(sequence.saturating_add(1));
					return Poll::Ready(Ok(Some(group)));
				}
			}
			// A `start_at` overtook the parked group; drop it and read on.
			if self.segments[index].parked.is_some() {
				// Still capped: the segment isn't done, it's parked.
				all_done = false;
				continue;
			}

			loop {
				let polled = Self::poll_segment(
					&mut self.segments[index],
					&self.last_prefs,
					self.min_sequence,
					end_sequence,
					waiter,
				);
				match polled {
					Poll::Ready(Some(group)) => {
						if beyond_cap(group.sequence) {
							// `end_at` parks the subscriber; hold the group until
							// the cap rises rather than dropping it.
							self.segments[index].parked = Some(group);
							all_done = false;
							break;
						}
						if group.sequence < self.min_sequence {
							// A `start_at` raced an already-delivered group; skip it
							// and re-poll the same segment for what's behind it.
							continue;
						}
						let sequence = group.sequence;
						// Folded into a group already handed out: keep reading this
						// segment rather than surfacing the same sequence twice.
						let Some(group) = self.hand_out(index, group) else {
							continue;
						};
						self.next_sequence = self.next_sequence.max(sequence.saturating_add(1));
						return Poll::Ready(Ok(Some(group)));
					}
					Poll::Ready(None) => break,
					Poll::Pending => {
						all_done = false;
						break;
					}
				}
			}
		}

		if let Some(err) = &self.abort {
			return Poll::Ready(Err(err.clone()));
		}
		if self.finished && all_done {
			return Poll::Ready(Ok(None));
		}
		Poll::Pending
	}

	/// Receive the next group in arrival order across the segments.
	#[cfg(test)]
	pub async fn recv_group(&mut self) -> Result<Option<group::Consumer>> {
		kio::wait(|waiter| self.poll_recv_group(waiter)).await
	}

	/// Poll for the next group with a higher sequence than any previously
	/// returned, skipping late arrivals, across the segments.
	///
	/// Unlike [`track::Subscriber`], the arrival-order and sequence-order cursors
	/// are shared: groups consumed here are also consumed for
	/// [`Self::poll_recv_group`].
	pub fn poll_next_group(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<group::Consumer>>> {
		loop {
			// Snapshot the floor before receiving: `poll_recv_group` advances
			// `next_sequence` for every group it returns, and a duplicate of the
			// last returned sequence (a boundary splicing at the delivered edge)
			// must compare against the floor as it was, or it slips through.
			let floor = self.next_sequence;
			match ready!(self.poll_recv_group(waiter))? {
				Some(group) if group.sequence < floor => continue,
				res => return Poll::Ready(Ok(res)),
			}
		}
	}

	/// Poll for a single full frame from the next group in sequence order,
	/// skipping the rest of the group. Intended for single-frame groups.
	pub fn poll_read_frame(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<frame::Frame>>> {
		loop {
			if let Some(group) = &mut self.reading {
				match group.poll_read_frame(waiter) {
					Poll::Ready(Ok(Some(frame))) => {
						self.reading = None;
						return Poll::Ready(Ok(Some(frame)));
					}
					// An empty or broken group is skipped like a gap.
					Poll::Ready(_) => self.reading = None,
					Poll::Pending => return Poll::Pending,
				}
				continue;
			}

			match ready!(self.poll_next_group(waiter))? {
				Some(group) => self.reading = Some(group),
				None => return Poll::Ready(Ok(None)),
			}
		}
	}

	/// Read a single full frame from the next group in sequence order.
	#[cfg(test)]
	pub async fn read_frame(&mut self) -> Result<Option<frame::Frame>> {
		kio::wait(|waiter| self.poll_read_frame(waiter)).await
	}

	/// Poll for the next datagram, from the newest segment only (datagrams are a
	/// live best-effort channel; there is nothing to resume from older segments).
	pub fn poll_recv_datagram(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Datagram>>> {
		self.poll_sync(waiter);

		// Drive the newest segment's activation too: a subscriber polling only
		// datagrams must still resolve the subscription (registering demand) and
		// be woken when it activates.
		if let Some(seg) = self.segments.last_mut()
			&& Self::poll_activate(seg, &self.last_prefs, self.min_sequence, self.end_sequence, waiter).is_ready()
			&& let SubState::Active(sub) = &mut seg.sub
		{
			match sub.poll_recv_datagram(waiter) {
				Poll::Ready(Ok(Some(datagram))) => return Poll::Ready(Ok(Some(datagram))),
				// Terminal states fall through to the logical checks below.
				Poll::Ready(_) => {}
				Poll::Pending => return Poll::Pending,
			}
		}

		if let Some(err) = &self.abort {
			return Poll::Ready(Err(err.clone()));
		}
		if self.finished {
			return Poll::Ready(Ok(None));
		}
		Poll::Pending
	}

	/// Block until the logical track ends: `Ok` after a clean finish, `Err` after
	/// an abort. Readers use `finished()`; this just discards the group count.
	#[cfg(test)]
	pub async fn closed(&mut self) -> Result<()> {
		kio::wait(|waiter| self.poll_finished(waiter)).await.map(|_| ())
	}

	/// Poll for the logical track finishing, returning the final segment's group
	/// count (one past its last sequence).
	pub fn poll_finished(&mut self, waiter: &kio::Waiter) -> Poll<Result<u64>> {
		self.poll_sync(waiter);

		if let Some(err) = &self.abort {
			return Poll::Ready(Err(err.clone()));
		}
		if !self.finished {
			return Poll::Pending;
		}

		// Drive the final segment to completion; earlier segments don't decide the
		// count. Only the subscription is resolved here: consuming groups would
		// steal them from a `recv_group` caller on the same subscriber.
		let Some(seg) = self.segments.last_mut() else {
			return Poll::Ready(Ok(0));
		};
		ready!(Self::poll_activate(
			seg,
			&self.last_prefs,
			self.min_sequence,
			self.end_sequence,
			waiter
		));
		match &mut seg.sub {
			SubState::Done(count) => Poll::Ready(Ok(count.unwrap_or(0))),
			SubState::Active(sub) => match ready!(sub.poll_finished(waiter)) {
				Ok(count) => {
					seg.sub = SubState::Done(Some(count));
					Poll::Ready(Ok(count))
				}
				Err(_) => {
					seg.sub = SubState::Done(None);
					Poll::Ready(Ok(0))
				}
			},
			SubState::Pending(_) => unreachable!("poll_activate resolved above"),
		}
	}

	/// Block until the logical track is finished, returning the final group count.
	#[cfg(test)]
	pub async fn finished(&mut self) -> Result<u64> {
		kio::wait(|waiter| self.poll_finished(waiter)).await
	}

	/// Start the subscriber at the specified sequence.
	pub fn start_at(&mut self, sequence: u64) {
		self.min_sequence = sequence;
		for seg in &mut self.segments {
			let floor = seg.first_group().max(sequence);
			if let SubState::Active(sub) = &mut seg.sub {
				sub.start_at(floor);
			}
		}
	}

	/// Cap the subscriber at the specified sequence (inclusive), or remove the cap.
	pub fn end_at(&mut self, sequence: impl Into<Option<u64>>) {
		self.end_sequence = sequence.into();
		for seg in &mut self.segments {
			let cap = min_some(seg.last_group(), self.end_sequence);
			if let SubState::Active(sub) = &mut seg.sub {
				sub.end_at(cap);
			}
		}
	}

	/// The shared preferences channel, so `track::SubscriberControl` can wrap it.
	pub(crate) fn prefs(&self) -> kio::Producer<Subscription> {
		self.prefs.clone()
	}

	/// Replace this subscriber's preferences; each segment's demand is re-derived
	/// on the next poll.
	pub fn update(&mut self, subscription: Subscription) {
		if let Ok(mut prefs) = self.prefs.write() {
			*prefs = subscription;
		}
	}

	/// The latest group sequence across the segments, clamped to their bounds.
	pub fn latest(&self) -> Option<u64> {
		self.state.read().latest()
	}

	/// Whether `other` reads the same logical track.
	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use crate::{Timestamp, broadcast};
	use futures::FutureExt;
	use std::sync::Arc;

	fn track_pair(name: &str) -> (track::Producer, track::Consumer) {
		let producer = track::Producer::new(Arc::new(broadcast::Info::default()), name, None);
		let consumer = producer.consume();
		(producer, consumer)
	}

	fn write_group(producer: &mut track::Producer, sequence: u64, payload: &str) {
		let mut group = producer.create_group(group::Info { sequence }).unwrap();
		group.write_frame(Timestamp::ZERO, payload.as_bytes().to_vec()).unwrap();
		group.finish().unwrap();
	}

	fn recv(sub: &mut Subscriber) -> u64 {
		sub.recv_group()
			.now_or_never()
			.expect("should not block")
			.expect("should not error")
			.expect("should not be finished")
			.sequence
	}

	fn read(group: &mut group::Consumer) -> Vec<u8> {
		group
			.read_frame()
			.now_or_never()
			.expect("should not block")
			.expect("should not error")
			.expect("should not be finished")
			.payload
			.to_vec()
	}

	fn recv_pending(sub: &mut Subscriber) {
		assert!(sub.recv_group().now_or_never().is_none(), "should have blocked");
	}

	#[tokio::test]
	async fn switch_splices_groups() {
		let (mut track_a, consumer_a) = track_pair("a");
		let (mut track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();

		let mut sub = producer.consume().subscribe(None);

		write_group(&mut track_a, 0, "a0");
		write_group(&mut track_a, 1, "a1");
		assert_eq!(recv(&mut sub), 0);
		assert_eq!(recv(&mut sub), 1);

		// Switch to B at group 2. A racing past its cap is filtered.
		producer.switch(&consumer_b, Position::group(2)).unwrap();
		write_group(&mut track_a, 2, "a2-over-cap");
		write_group(&mut track_b, 2, "b2");
		write_group(&mut track_b, 3, "b3");

		assert_eq!(recv(&mut sub), 2);
		assert_eq!(recv(&mut sub), 3);
		recv_pending(&mut sub);
	}

	#[tokio::test]
	async fn demand_reflects_boundaries() {
		let (track_a, consumer_a) = track_pair("a");
		let (track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();

		let mut sub = producer
			.consume()
			.subscribe(Subscription::default().with_group_start(0));
		// Poll once so the subscriber registers on segment A.
		recv_pending(&mut sub);
		assert_eq!(track_a.subscription().unwrap().group_end, None);

		producer.switch(&consumer_b, Position::group(5)).unwrap();
		recv_pending(&mut sub);

		// The old session sees its demand capped; the new one starts at the boundary.
		assert_eq!(track_a.subscription().unwrap().group_end, Some(4));
		assert_eq!(track_b.subscription().unwrap().group_start, Some(5));
	}

	#[tokio::test]
	async fn update_reslices_demand() {
		let (track_a, consumer_a) = track_pair("a");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();

		let mut sub = producer.consume().subscribe(None);
		recv_pending(&mut sub);
		assert_eq!(track_a.subscription().unwrap().priority, 0);

		sub.update(Subscription::default().with_priority(7));
		recv_pending(&mut sub);
		assert_eq!(track_a.subscription().unwrap().priority, 7);
	}

	#[tokio::test]
	async fn dead_segment_stalls_until_switch() {
		let (mut track_a, consumer_a) = track_pair("a");
		let (mut track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();
		let mut sub = producer.consume().subscribe(None);

		write_group(&mut track_a, 0, "a0");
		assert_eq!(recv(&mut sub), 0);

		// The route dies: the subscriber stalls, it does not error.
		track_a.abort(Error::Dropped).unwrap();
		recv_pending(&mut sub);

		// A replacement resumes exactly where the old route left off.
		producer.switch(&consumer_b, Position::group(1)).unwrap();
		write_group(&mut track_b, 1, "b1");
		assert_eq!(recv(&mut sub), 1);
	}

	#[tokio::test]
	async fn takeover_computes_boundary() {
		let (mut track_a, consumer_a) = track_pair("a");
		let (mut track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();

		// No segments yet: the takeover is unbounded.
		producer.takeover(&consumer_a).unwrap();
		let mut sub = producer.consume().subscribe(None);
		write_group(&mut track_a, 0, "a0");
		write_group(&mut track_a, 1, "a1");
		assert_eq!(recv(&mut sub), 0);
		assert_eq!(recv(&mut sub), 1);

		// Groups exist: the takeover resumes one past the newest, even when the old
		// route's cache died with it (a group mid-transfer is lost like any loss,
		// never re-delivered live to subscribers that may already have it).
		track_a.abort(Error::Dropped).unwrap();
		producer.takeover(&consumer_b).unwrap();
		write_group(&mut track_b, 2, "b2");
		assert_eq!(recv(&mut sub), 2);
	}

	#[tokio::test]
	async fn takeover_replaces_empty_segment() {
		let (track_a, consumer_a) = track_pair("a");
		let (mut track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.takeover(&consumer_a).unwrap();
		let mut sub = producer.consume().subscribe(None);
		recv_pending(&mut sub);

		// A never produced anything, so B replaces it outright and group 0 is
		// still reachable.
		drop(track_a);
		producer.takeover(&consumer_b).unwrap();
		write_group(&mut track_b, 0, "b0");
		assert_eq!(recv(&mut sub), 0);
	}

	#[tokio::test]
	async fn finish_ends_after_final_segment() {
		let (mut track_a, consumer_a) = track_pair("a");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();
		let mut sub = producer.consume().subscribe(None);

		write_group(&mut track_a, 0, "a0");
		assert_eq!(recv(&mut sub), 0);

		// Finishing the logical track alone isn't the end; the segment must drain.
		producer.finish().unwrap();
		recv_pending(&mut sub);

		track_a.finish().unwrap();
		assert!(
			sub.recv_group()
				.now_or_never()
				.expect("should not block")
				.expect("should not error")
				.is_none(),
			"should be finished"
		);
		assert_eq!(sub.finished().now_or_never().unwrap().unwrap(), 1);
		assert!(sub.closed().now_or_never().unwrap().is_ok());
	}

	#[tokio::test]
	async fn read_frame_across_segments() {
		let (mut track_a, consumer_a) = track_pair("a");
		let (mut track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();
		let mut sub = producer.consume().subscribe(None);

		write_group(&mut track_a, 0, "a0");
		producer.switch(&consumer_b, Position::group(1)).unwrap();
		write_group(&mut track_b, 1, "b1");

		let frame = sub.read_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(&frame.payload[..], b"a0");
		let frame = sub.read_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(&frame.payload[..], b"b1");
	}

	#[tokio::test]
	async fn info_from_first_segment() {
		let (_track_a, consumer_a) = track_pair("a");

		let mut producer = Producer::new();
		let consumer = producer.consume();

		// No segments: info is parked.
		assert!(consumer.info().now_or_never().is_none());

		producer.switch(&consumer_a, None).unwrap();
		let info = consumer.info().now_or_never().unwrap().unwrap();
		assert_eq!(info.timescale, crate::Timescale::default());
	}

	#[tokio::test]
	async fn fetch_routes_to_newest_segment() {
		let (track_a, consumer_a) = track_pair("a");
		let (mut track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();
		producer.switch(&consumer_b, Position::group(10)).unwrap();

		// A cached group on the newest segment resolves immediately, even below
		// its subscribe boundary: bounds slice demand, not access.
		write_group(&mut track_b, 3, "b3");
		let consumer = producer.consume();
		let group = consumer
			.fetch_group(3, None)
			.now_or_never()
			.expect("cached fetch should resolve")
			.unwrap();
		assert_eq!(group.sequence, 3);

		// Fetches never touch the old segment.
		drop(track_a);
	}

	#[tokio::test]
	async fn fetch_waits_for_first_segment() {
		let (mut track_a, consumer_a) = track_pair("a");

		let mut producer = Producer::new();
		let consumer = producer.consume();

		// No segment yet: the fetch parks instead of failing (a route may serve the
		// track any moment).
		let fetch = consumer.fetch_group(0, None);
		let mut fetch = std::pin::pin!(fetch);
		assert!(futures::poll!(fetch.as_mut()).is_pending(), "fetch should wait");

		// The first segment arrives with the group cached: the fetch resolves.
		write_group(&mut track_a, 0, "a0");
		producer.switch(&consumer_a, None).unwrap();
		let group = fetch.await.expect("fetch should resolve");
		assert_eq!(group.sequence, 0);
	}

	#[tokio::test]
	async fn takeover_survives_dead_empty_segment() {
		let (mut track_a, consumer_a) = track_pair("a");
		let (track_b, consumer_b) = track_pair("b");
		let (mut track_c, consumer_c) = track_pair("c");

		let mut producer = Producer::new();
		producer.takeover(&consumer_a).unwrap();
		let mut sub = producer.consume().subscribe(None);
		write_group(&mut track_a, 0, "a0");
		assert_eq!(recv(&mut sub), 0);

		// A dies; B takes over at the boundary but dies before producing.
		track_a.abort(Error::Dropped).unwrap();
		producer.takeover(&consumer_b).unwrap();
		drop(track_b);

		// C replaces B's empty segment instead of failing forever on the
		// unadvanceable boundary.
		producer.takeover(&consumer_c).unwrap();
		write_group(&mut track_c, 1, "c1");
		assert_eq!(recv(&mut sub), 1);
	}

	#[tokio::test]
	async fn finished_does_not_consume_groups() {
		let (mut track_a, consumer_a) = track_pair("a");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();
		let mut sub = producer.consume().subscribe(None);

		write_group(&mut track_a, 0, "a0");
		producer.finish().unwrap();

		// Waiting for the end must not steal the buffered group from recv.
		assert!(sub.finished().now_or_never().is_none(), "final segment still open");
		assert_eq!(recv(&mut sub), 0);

		track_a.finish().unwrap();
		assert_eq!(sub.finished().now_or_never().unwrap().unwrap(), 1);
	}

	#[tokio::test]
	async fn datagram_only_subscriber_activates() {
		let (mut track_a, consumer_a) = track_pair("a");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();
		let mut sub = producer.consume().subscribe(None);

		// Polling only datagrams must still resolve the subscription.
		assert!(
			kio::wait(|waiter| sub.poll_recv_datagram(waiter))
				.now_or_never()
				.is_none(),
			"no datagram yet"
		);
		track_a.append_datagram(Timestamp::ZERO, b"d0".as_ref()).unwrap();
		let datagram = kio::wait(|waiter| sub.poll_recv_datagram(waiter))
			.now_or_never()
			.expect("datagram should be ready")
			.expect("should not error")
			.expect("track should not be finished");
		assert_eq!(&datagram.payload[..], b"d0");
	}

	#[tokio::test]
	async fn end_at_parks_at_cap() {
		let (mut track_a, consumer_a) = track_pair("a");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();
		let mut sub = producer.consume().subscribe(None);

		write_group(&mut track_a, 0, "a0");
		write_group(&mut track_a, 1, "a1");

		// The cap parks the subscriber; the group beyond it is held, not dropped.
		sub.end_at(0);
		assert_eq!(recv(&mut sub), 0);
		recv_pending(&mut sub);

		// Raising the cap re-offers the parked group.
		sub.end_at(1);
		assert_eq!(recv(&mut sub), 1);
	}

	#[tokio::test]
	async fn next_group_skips_boundary_duplicate() {
		let (mut track_a, consumer_a) = track_pair("a");
		let (mut track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();
		let mut sub = producer.consume().subscribe(None);

		let next = |sub: &mut Subscriber| {
			kio::wait(|waiter| sub.poll_next_group(waiter))
				.now_or_never()
				.expect("should not block")
				.expect("should not error")
				.expect("should not be finished")
				.sequence
		};

		write_group(&mut track_a, 0, "a0");
		write_group(&mut track_a, 1, "a1");
		assert_eq!(next(&mut sub), 0);
		assert_eq!(next(&mut sub), 1);

		// A boundary at the delivered edge: B re-serves group 1, which was already
		// returned and must not be delivered twice.
		producer.switch(&consumer_b, Position::group(1)).unwrap();
		write_group(&mut track_b, 1, "b1");
		write_group(&mut track_b, 2, "b2");
		assert_eq!(next(&mut sub), 2);
	}

	#[tokio::test]
	async fn consecutive_updates_wake() {
		use std::sync::atomic::{AtomicUsize, Ordering};
		use std::task::{Context, Wake, Waker};

		struct CountWaker(AtomicUsize);
		impl Wake for CountWaker {
			fn wake(self: Arc<Self>) {
				self.0.fetch_add(1, Ordering::SeqCst);
			}
		}

		let (track_a, consumer_a) = track_pair("a");
		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();
		let mut sub = producer.consume().subscribe(None);
		let prefs = sub.prefs();

		let counter = Arc::new(CountWaker(AtomicUsize::new(0)));
		let waker = Waker::from(counter.clone());
		let mut cx = Context::from_waker(&waker);

		let mut fut = std::pin::pin!(sub.recv_group());
		assert!(fut.as_mut().poll(&mut cx).is_pending());

		// First update wakes and is applied on the next poll.
		*prefs.write().ok().unwrap() = Subscription::default().with_priority(1);
		assert_eq!(counter.0.load(Ordering::SeqCst), 1);
		assert!(fut.as_mut().poll(&mut cx).is_pending());
		assert_eq!(track_a.subscription().unwrap().priority, 1);

		// The poll that consumed the change must have re-registered: a second
		// update, with no other activity in between, still wakes.
		*prefs.write().ok().unwrap() = Subscription::default().with_priority(2);
		assert_eq!(counter.0.load(Ordering::SeqCst), 2, "second update lost its wakeup");
		assert!(fut.as_mut().poll(&mut cx).is_pending());
		assert_eq!(track_a.subscription().unwrap().priority, 2);
	}

	/// A route dying partway through a group is resumed at the frame it stopped on:
	/// the reader keeps the same group handle, sees no duplicate frames, and never
	/// learns a route changed. This is the whole point of frame-precise boundaries
	/// (a JSON append log's group may never roll).
	#[tokio::test]
	async fn takeover_splices_mid_group() {
		let (mut track_a, consumer_a) = track_pair("a");
		let (mut track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.takeover(&consumer_a).unwrap();
		let mut sub = producer.consume().subscribe(None);

		// A opens group 0 and writes two frames, leaving it open.
		let mut group = track_a.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(Timestamp::ZERO, b"a0".to_vec()).unwrap();
		group.write_frame(Timestamp::ZERO, b"a1".to_vec()).unwrap();

		let mut reading = sub.recv_group().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(reading.sequence, 0);
		assert_eq!(read(&mut reading), b"a0", "first");
		assert_eq!(read(&mut reading), b"a1", "second");
		assert!(reading.read_frame().now_or_never().is_none(), "group is still open");

		// The route dies mid-group and B takes over inside it.
		producer.takeover(&consumer_b).unwrap();
		track_a.abort(Error::Dropped).unwrap();
		recv_pending(&mut sub);

		let demand = track_b.subscription().unwrap();
		assert_eq!(demand.group_start, Some(0), "resumes in the same group");
		assert_eq!(demand.frame_start, 2, "resumes at the frame the old route stopped on");

		let mut group = track_b.create_group(group::Info { sequence: 0 }).unwrap();
		group.start_at(2).unwrap();
		group.write_frame(Timestamp::ZERO, b"b2".to_vec()).unwrap();
		group.finish().unwrap();

		// Same handle, no seam: the continuation is not surfaced as a second group.
		assert_eq!(read(&mut reading), b"b2");
		assert!(reading.read_frame().now_or_never().unwrap().unwrap().is_none());
		recv_pending(&mut sub);
	}

	/// A replacement that serves the whole group instead of the requested tail still
	/// splices cleanly: the reader picks up at the seam and never sees the frames it
	/// already has.
	///
	/// This is what a peer too old for frame bounds delivers. The lite subscriber widens
	/// the request to the whole group for such a peer rather than failing to encode it
	/// (see `TrackServe::widen_frame_bounds`), so the extra frames are filtered here.
	#[tokio::test]
	async fn takeover_splices_a_replacement_that_resends_the_head() {
		let (mut track_a, consumer_a) = track_pair("a");
		let (mut track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.takeover(&consumer_a).unwrap();
		let mut sub = producer.consume().subscribe(None);

		let mut group = track_a.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(Timestamp::ZERO, b"a0".to_vec()).unwrap();
		group.write_frame(Timestamp::ZERO, b"a1".to_vec()).unwrap();

		let mut reading = sub.recv_group().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(read(&mut reading), b"a0");
		assert_eq!(read(&mut reading), b"a1");

		producer.takeover(&consumer_b).unwrap();
		track_a.abort(Error::Dropped).unwrap();
		recv_pending(&mut sub);

		// B numbers the group from 0, as an older peer that cannot carry the offset does.
		let mut group = track_b.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(Timestamp::ZERO, b"dup0".to_vec()).unwrap();
		group.write_frame(Timestamp::ZERO, b"dup1".to_vec()).unwrap();
		group.write_frame(Timestamp::ZERO, b"b2".to_vec()).unwrap();
		group.finish().unwrap();

		// The reader resumes at frame 2; the re-sent head is filtered, not replayed.
		assert_eq!(read(&mut reading), b"b2");
		assert!(reading.read_frame().now_or_never().unwrap().unwrap().is_none());

		// And the group is not surfaced a second time just because B served it whole.
		recv_pending(&mut sub);
	}

	/// A route dying midway through a chunked frame resumes *at* that frame, not after
	/// it. Only the dead route ever saw its payload, and only part of it, so nothing
	/// downstream can use it.
	#[tokio::test]
	async fn takeover_redelivers_an_incomplete_frame() {
		let (mut track_a, consumer_a) = track_pair("a");
		let (mut track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.takeover(&consumer_a).unwrap();
		let mut sub = producer.consume().subscribe(None);

		let mut group = track_a.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(Timestamp::ZERO, b"a0".to_vec()).unwrap();

		let mut reading = sub.recv_group().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(read(&mut reading), b"a0");

		// Frame 1 is opened and half written when the route dies. Dropping the frame
		// producer without finishing aborts the group, as a broken stream would.
		{
			let mut frame = group
				.create_frame(frame::Info {
					size: 6,
					timestamp: Timestamp::ZERO,
				})
				.unwrap();
			frame.write(b"foo".to_vec()).unwrap();
		}
		track_a.abort(Error::Dropped).unwrap();

		producer.takeover(&consumer_b).unwrap();
		recv_pending(&mut sub);

		let demand = track_b.subscription().unwrap();
		assert_eq!(
			(demand.group_start, demand.frame_start),
			(Some(0), 1),
			"the half-written frame must be redelivered, not skipped"
		);

		let mut group = track_b.create_group(group::Info { sequence: 0 }).unwrap();
		group.start_at(1).unwrap();
		group.write_frame(Timestamp::ZERO, b"b1".to_vec()).unwrap();

		// The reader was parked on frame 1 and picks it up from the replacement.
		assert_eq!(read(&mut reading), b"b1");
	}

	/// The old route racing past its frame boundary is filtered, exactly as an
	/// out-of-range group is.
	#[tokio::test]
	async fn mid_group_boundary_caps_the_old_route() {
		let (mut track_a, consumer_a) = track_pair("a");
		let (mut track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.takeover(&consumer_a).unwrap();
		let mut sub = producer.consume().subscribe(None);

		let mut group_a = track_a.create_group(group::Info { sequence: 0 }).unwrap();
		group_a.write_frame(Timestamp::ZERO, b"a0".to_vec()).unwrap();

		let mut reading = sub.recv_group().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(read(&mut reading), b"a0");

		producer.takeover(&consumer_b).unwrap();
		recv_pending(&mut sub);
		// The old copy's demand is capped just below the boundary.
		let demand = track_a.subscription().unwrap();
		assert_eq!((demand.group_end, demand.frame_end), (Some(0), Some(0)));

		// A keeps writing anyway; those frames belong to B's range now.
		group_a.write_frame(Timestamp::ZERO, b"a1-over-cap".to_vec()).unwrap();

		let mut group_b = track_b.create_group(group::Info { sequence: 0 }).unwrap();
		group_b.start_at(1).unwrap();
		group_b.write_frame(Timestamp::ZERO, b"b1".to_vec()).unwrap();

		assert_eq!(read(&mut reading), b"b1");
	}

	/// A takeover only rolls to the next group once the current one is complete;
	/// there is nothing left to append to it.
	#[tokio::test]
	async fn takeover_rolls_past_a_finished_group() {
		let (mut track_a, consumer_a) = track_pair("a");
		let (track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.takeover(&consumer_a).unwrap();
		let mut sub = producer.consume().subscribe(None);

		write_group(&mut track_a, 0, "a0");
		assert_eq!(recv(&mut sub), 0);

		producer.takeover(&consumer_b).unwrap();
		recv_pending(&mut sub);
		let demand = track_b.subscription().unwrap();
		assert_eq!(demand.group_start, Some(1));
		assert_eq!(demand.frame_start, 0);
	}

	/// A copy dying mid-group stalls its readers instead of erroring them, the same
	/// way a dead segment stalls the track. The next takeover resumes them.
	#[tokio::test]
	async fn dead_copy_stalls_until_the_continuation() {
		let (mut track_a, consumer_a) = track_pair("a");
		let (mut track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.takeover(&consumer_a).unwrap();
		let mut sub = producer.consume().subscribe(None);

		let mut group = track_a.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(Timestamp::ZERO, b"a0".to_vec()).unwrap();

		let mut reading = sub.recv_group().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(read(&mut reading), b"a0");

		// The copy fails outright; the reader must not see the error.
		group.abort(Error::Dropped).unwrap();
		assert!(reading.read_frame().now_or_never().is_none(), "should stall, not error");

		producer.takeover(&consumer_b).unwrap();
		let mut group = track_b.create_group(group::Info { sequence: 0 }).unwrap();
		group.start_at(1).unwrap();
		group.write_frame(Timestamp::ZERO, b"b1".to_vec()).unwrap();
		assert_eq!(read(&mut reading), b"b1");
	}

	/// A dead copy stalls only while a replacement can still arrive. Once the logical
	/// track aborts, no switch is coming, so the reader has to surface the loss rather
	/// than park on it forever.
	#[tokio::test]
	async fn dead_copy_ends_once_the_track_aborts() {
		let (mut track_a, consumer_a) = track_pair("a");

		let mut producer = Producer::new();
		producer.takeover(&consumer_a).unwrap();
		let mut sub = producer.consume().subscribe(None);

		let mut group = track_a.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(Timestamp::ZERO, b"a0".to_vec()).unwrap();

		let mut reading = sub.recv_group().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(read(&mut reading), b"a0");

		// The route dies. A switch could still replace it, so the reader parks.
		group.abort(Error::Dropped).unwrap();
		assert!(
			reading.read_frame().now_or_never().is_none(),
			"a switch could still come"
		);

		// The logical track aborts: nothing can replace it now.
		producer.abort(Error::Cancel).unwrap();
		assert!(
			matches!(reading.read_frame().now_or_never(), Some(Err(_))),
			"an aborted track must not leave the reader parked"
		);
	}

	/// A route whose copy of the group is missing the frames the reader needs is treated
	/// like a dead one. Reading the tail as if it were the head would silently renumber
	/// every frame in the group.
	///
	/// The loss is reported rather than parked on, because the route already produced
	/// past the missing frames. Boundaries come from `resume_position`, which only moves
	/// forward, so no future takeover will ever ask anyone for frame 0 again.
	#[tokio::test]
	async fn copy_missing_the_head_is_lost() {
		let (mut track_a, consumer_a) = track_pair("a");

		let mut producer = Producer::new();
		producer.takeover(&consumer_a).unwrap();
		let mut sub = producer.consume().subscribe(None);

		// This route only ever held the tail: its copy starts at frame 2.
		let mut group = track_a.create_group(group::Info { sequence: 0 }).unwrap();
		group.start_at(2).unwrap();
		group.write_frame(Timestamp::ZERO, b"a2".to_vec()).unwrap();

		let mut reading = sub.recv_group().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(reading.sequence, 0);
		assert_eq!(reading.index(), 0, "the reader still wants frame 0");
		assert!(
			matches!(reading.read_frame().now_or_never(), Some(Err(Error::Lagged))),
			"must report the loss rather than serve the tail as the head"
		);
	}

	/// A route that has run ahead of the seam but has not been given up on still parks.
	///
	/// It may yet deliver the missing frames out of order, and its own progress is what
	/// moved the resume point past them, so its being ahead is not evidence the frames
	/// are gone. Only a route already declared dead strands the reader.
	#[tokio::test]
	async fn live_route_ahead_of_the_seam_still_parks() {
		let (mut track_a, consumer_a) = track_pair("a");
		let (mut track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.takeover(&consumer_a).unwrap();
		let mut sub = producer.consume().subscribe(None);

		let mut group = track_a.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(Timestamp::ZERO, b"a0".to_vec()).unwrap();
		group.write_frame(Timestamp::ZERO, b"a1".to_vec()).unwrap();

		let mut reading = sub.recv_group().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(read(&mut reading), b"a0");
		assert_eq!(read(&mut reading), b"a1");

		// B takes over inside group 0, then runs ahead to group 1 without ever serving
		// the rest of group 0. That pushes the resume point past the seam.
		producer.takeover(&consumer_b).unwrap();
		write_group(&mut track_b, 1, "b1");
		assert_eq!(recv(&mut sub), 1);

		assert!(
			reading.read_frame().now_or_never().is_none(),
			"a live route may still fill the seam out of order"
		);

		// Once it does, the reader picks up where it left off.
		let mut group = track_b.create_group(group::Info { sequence: 0 }).unwrap();
		group.start_at(2).unwrap();
		group.write_frame(Timestamp::ZERO, b"b2".to_vec()).unwrap();
		assert_eq!(read(&mut reading), b"b2");
	}

	#[tokio::test]
	async fn switch_validates_boundaries() {
		let (mut track_a, consumer_a) = track_pair("a");
		let (_track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();

		// A later switch requires an explicit, advancing boundary; 0 is only legal
		// when the previous segment never produced a group.
		assert!(producer.switch(&consumer_b, None).is_err());
		write_group(&mut track_a, 0, "a0");
		assert!(producer.switch(&consumer_b, Position::group(0)).is_err());
		producer.switch(&consumer_b, Position::group(1)).unwrap();
	}

	/// Repeated failovers must not accumulate a segment per takeover: dead
	/// predecessors are pruned once the list outgrows [`MAX_SEGMENTS`], and the
	/// boundary stays where the pruned segments left it rather than collapsing.
	#[tokio::test]
	async fn prune_bounds_segments_and_keeps_the_boundary() {
		let mut producer = Producer::new();
		let mut sub = producer.consume().subscribe(None);

		// Each route serves one group and dies; the next takeover resumes past it.
		let rounds = 2 * MAX_SEGMENTS as u64;
		for sequence in 0..rounds {
			let (mut track, consumer) = track_pair("t");
			producer.takeover(&consumer).unwrap();
			write_group(&mut track, sequence, "payload");
			assert_eq!(recv(&mut sub), sequence);
			track.abort(Error::Dropped).unwrap();
		}
		assert_eq!(
			producer.state.read().segments.len(),
			MAX_SEGMENTS,
			"dead predecessors should have been pruned"
		);

		// The floor carries what the pruned segments served: a replacement splices
		// one past the newest group, never back at the start.
		let (mut track, consumer) = track_pair("final");
		producer.takeover(&consumer).unwrap();
		write_group(&mut track, 0, "below-the-floor");
		recv_pending(&mut sub);
		write_group(&mut track, rounds, "resumed");
		assert_eq!(recv(&mut sub), rounds);
	}

	/// Pruning must not depend on predecessors dying: a takeover boundary is the
	/// resume position, so a capped segment already produced everything it owns
	/// and is retired even while its track is alive. Route churn with long-lived
	/// sessions would otherwise grow the list without bound, since nothing
	/// re-prunes between switches.
	#[tokio::test]
	async fn prune_retires_live_predecessors() {
		let mut producer = Producer::new();
		let mut sub = producer.consume().subscribe(None);

		// Every route stays alive: only the takeover cap retires them.
		let mut tracks = Vec::new();
		let rounds = 2 * MAX_SEGMENTS as u64;
		for sequence in 0..rounds {
			let (mut track, consumer) = track_pair("t");
			producer.takeover(&consumer).unwrap();
			write_group(&mut track, sequence, "payload");
			assert_eq!(recv(&mut sub), sequence);
			tracks.push(track);
		}
		assert_eq!(
			producer.state.read().segments.len(),
			MAX_SEGMENTS,
			"live predecessors should still be pruned"
		);
	}

	/// A route that skips a group for good (SUBSCRIBE_START names a later first
	/// group) must fail the readers waiting on it over, not stall them: the route
	/// is alive, so nothing else would ever mark the gap as permanent.
	#[tokio::test]
	async fn declared_start_fails_over_a_skipped_group() {
		let (mut track_a, consumer_a) = track_pair("a");
		let (mut track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.takeover(&consumer_a).unwrap();
		let mut sub = producer.consume().subscribe(None);

		// A serves one frame of an open group, then dies mid-group.
		let mut group = track_a.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(Timestamp::ZERO, b"a0".to_vec()).unwrap();
		let mut reading = sub.recv_group().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(read(&mut reading), b"a0");
		assert!(
			reading.read_frame().now_or_never().is_none(),
			"the seam parks for a continuation"
		);
		// The session dies mid-group: its group producers go with it.
		drop(group);
		track_a.abort(Error::Dropped).unwrap();

		// B takes over but declares group 1 as its first: the seam is skipped for
		// good, not merely late.
		producer.takeover(&consumer_b).unwrap();
		track_b.start_at(1).unwrap();
		write_group(&mut track_b, 1, "b1");
		assert_eq!(recv(&mut sub), 1);

		// The reader waiting on group 0's continuation surfaces the loss instead
		// of parking forever on a live route.
		assert!(
			reading
				.read_frame()
				.now_or_never()
				.expect("the skipped seam must resolve")
				.is_err(),
			"the skipped frames are a loss, not a clean end"
		);
	}

	/// A reader latched on a capped copy must follow a moved boundary: popping an
	/// empty successor can re-cap its predecessor lower, and the frames past the
	/// revised cap belong to the replacement route. Serving them from the stale
	/// copy would substitute (or duplicate) the replacement's frames.
	#[tokio::test]
	async fn latched_reader_follows_a_moved_boundary() {
		let (mut track_a, consumer_a) = track_pair("a");
		let (track_b, consumer_b) = track_pair("b");
		let (mut track_c, consumer_c) = track_pair("c");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();
		let mut sub = producer.consume().subscribe(None);

		// A serves two frames of an open group; B splices at the produced edge, so
		// the reader latches A's copy with a cap of (0, 2).
		let mut group = track_a.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(Timestamp::ZERO, b"a0".to_vec()).unwrap();
		group.write_frame(Timestamp::ZERO, b"a1".to_vec()).unwrap();
		producer.switch(&consumer_b, Position { group: 0, frame: 2 }).unwrap();
		let mut reading = sub.recv_group().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(read(&mut reading), b"a0");

		// B dies empty; C replaces it with an earlier boundary, moving A's cap down
		// to (0, 1). Frame 1 now belongs to C, and the latched reader must fetch
		// C's copy rather than serving A's past the revised cap.
		drop(track_b);
		producer.switch(&consumer_c, Position { group: 0, frame: 1 }).unwrap();
		let mut group = track_c.create_group(group::Info { sequence: 0 }).unwrap();
		group.start_at(1).unwrap();
		group.write_frame(Timestamp::ZERO, b"c1".to_vec()).unwrap();
		assert_eq!(read(&mut reading), b"c1");
	}

	/// A long-lived subscriber must not accumulate a cursor per takeover when the
	/// old routes stay alive: their tracks never end, so nothing else would ever
	/// cut them. A bounded number of pruned cursors linger to drain out-of-order
	/// stragglers; beyond the bound the oldest are cut, subscription, demand,
	/// and all.
	#[tokio::test]
	async fn pruned_cursors_stay_bounded() {
		let mut producer = Producer::new();
		let mut sub = producer.consume().subscribe(None);

		let mut tracks = Vec::new();
		let rounds = 3 * MAX_SEGMENTS as u64;
		for sequence in 0..rounds {
			let (mut track, consumer) = track_pair("t");
			producer.takeover(&consumer).unwrap();
			write_group(&mut track, sequence, "payload");
			assert_eq!(recv(&mut sub), sequence);
			tracks.push(track);
		}

		// The cut happens while polling and the next poll reaps the entries.
		recv_pending(&mut sub);
		recv_pending(&mut sub);
		assert_eq!(
			sub.segments.len(),
			2 * MAX_SEGMENTS,
			"pruned cursors beyond the bound must be cut"
		);
		assert!(tracks[0].subscription().is_none(), "a cut cursor releases its demand");
		assert!(
			tracks[rounds as usize - MAX_SEGMENTS - 1].subscription().is_some(),
			"a pruned cursor within the bound keeps draining"
		);
	}

	/// A capped subscriber riding out route churn must not accumulate parked
	/// cursors: a pruned segment's parked group can no longer deliver (its frames
	/// resolve through the pruned-away segment list), so it drops with the prune,
	/// and the straggler bound cuts the oldest entries whole.
	#[tokio::test]
	async fn capped_subscriber_retires_pruned_parked_segments() {
		let mut producer = Producer::new();
		let mut sub = producer.consume().subscribe(None);
		sub.end_at(0);

		// Every round parks one group beyond the cap, then fails over to a live
		// replacement route.
		let mut tracks = Vec::new();
		let rounds = 3 * MAX_SEGMENTS as u64;
		for round in 0..rounds {
			let (mut track, consumer) = track_pair("t");
			producer.takeover(&consumer).unwrap();
			write_group(&mut track, round + 1, "beyond-the-cap");
			recv_pending(&mut sub);
			tracks.push(track);
		}

		recv_pending(&mut sub);
		assert_eq!(
			sub.segments.len(),
			2 * MAX_SEGMENTS,
			"pruned parked entries must stay bounded, not accumulate"
		);
		assert!(tracks[0].subscription().is_none(), "a cut entry releases its demand");

		// Raising the cap delivers what the surviving segments still cover; the
		// pruned ranges are lost with their routes.
		sub.end_at(None);
		for sequence in (rounds - MAX_SEGMENTS as u64 + 1)..=rounds {
			assert_eq!(recv(&mut sub), sequence);
		}
		recv_pending(&mut sub);
	}

	/// An out-of-order group that lands after its segment was pruned still drains:
	/// the cursor lingers (within the straggler bound) exactly because an empty
	/// poll is not proof of completeness, and its demand is what keeps the
	/// upstream serving the stragglers.
	#[tokio::test]
	async fn late_group_drains_from_a_pruned_cursor() {
		let mut producer = Producer::new();
		let mut sub = producer.consume().subscribe(None);

		// A delivers group 1 first; group 0 is still in flight when A is outranked
		// and enough failovers prune its segment.
		let (mut track_a, consumer_a) = track_pair("a");
		producer.takeover(&consumer_a).unwrap();
		write_group(&mut track_a, 1, "a1");
		assert_eq!(recv(&mut sub), 1);

		for sequence in 2..=(1 + MAX_SEGMENTS as u64) {
			let (mut track, consumer) = track_pair("t");
			producer.takeover(&consumer).unwrap();
			write_group(&mut track, sequence, "payload");
			assert_eq!(recv(&mut sub), sequence);
		}
		assert!(
			producer.state.read().pruned.is_some(),
			"the first segment should be pruned"
		);

		// The straggler finally lands and surfaces through the lingering cursor.
		write_group(&mut track_a, 0, "a0");
		assert_eq!(recv(&mut sub), 0);
	}

	/// `finished()` on a group bounded by a mid-group takeover resolves once no
	/// segment can serve the seam: the continuation owns the count, and when the
	/// covering segments are pruned away the cap is the group's end. Polled
	/// without draining first, which is exactly the caller the seam check must
	/// park (and wake) rather than hang.
	#[tokio::test]
	async fn finished_resolves_for_a_pruned_bounded_group() {
		let (mut track_a, consumer_a) = track_pair("a");
		let mut producer = Producer::new();
		producer.takeover(&consumer_a).unwrap();
		let mut sub = producer.consume().subscribe(None);

		// A serves two frames of an open group; B splices at the seam, so the
		// reader's copy is handed out bounded at (0, 2).
		let mut group = track_a.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(Timestamp::ZERO, b"f0".to_vec()).unwrap();
		group.write_frame(Timestamp::ZERO, b"f1".to_vec()).unwrap();
		let (mut track_b, consumer_b) = track_pair("b");
		producer.takeover(&consumer_b).unwrap();
		let mut reading = sub.recv_group().now_or_never().unwrap().unwrap().unwrap();
		drop(group);
		track_a.abort(Error::Dropped).unwrap();

		// While B covers the seam the count is still open: B may serve frame 2.
		write_group(&mut track_b, 1, "b1");
		assert_eq!(recv(&mut sub), 1);
		assert!(
			reading.finished().now_or_never().is_none(),
			"the seam is still coverable"
		);

		// Enough failovers prune A and B: nothing can serve the seam anymore, so
		// the cap is the end.
		for sequence in 2..=(1 + MAX_SEGMENTS as u64) {
			let (mut track, consumer) = track_pair("t");
			producer.takeover(&consumer).unwrap();
			write_group(&mut track, sequence, "payload");
			assert_eq!(recv(&mut sub), sequence);
		}
		assert_eq!(
			reading
				.finished()
				.now_or_never()
				.expect("the lost seam must resolve the count")
				.unwrap(),
			2
		);
	}

	/// A handed-out group survives its segment's prune: the delivering copy is
	/// latched into the spliced reader at hand-out, so the payload the cursor
	/// already carried is read out even though the segment list has forgotten
	/// the route. A re-resolving reader (a clone re-latches its own copy) finds
	/// the range below the floor and gives up instead of parking forever.
	#[tokio::test]
	async fn group_reader_gives_up_below_the_pruned_floor() {
		let mut producer = Producer::new();
		let mut sub = producer.consume().subscribe(None);

		// Group 0 is handed out but never read; its route dies and enough
		// failovers follow to prune the segment that held it.
		let (mut track, consumer) = track_pair("t0");
		producer.takeover(&consumer).unwrap();
		write_group(&mut track, 0, "kept");
		let mut reading = sub.recv_group().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(reading.sequence, 0);
		let mut resolving = reading.clone();
		track.abort(Error::Dropped).unwrap();

		for sequence in 1..=MAX_SEGMENTS as u64 {
			let (mut track, consumer) = track_pair("t");
			producer.takeover(&consumer).unwrap();
			write_group(&mut track, sequence, "payload");
			assert_eq!(recv(&mut sub), sequence);
			track.abort(Error::Dropped).unwrap();
		}
		assert!(
			producer.state.read().pruned.is_some(),
			"the first segment should be pruned"
		);

		// The latched copy still delivers, then the group ends cleanly.
		assert_eq!(read(&mut reading), b"kept");
		assert!(
			reading.read_frame().now_or_never().unwrap().unwrap().is_none(),
			"the group ends at what the route produced"
		);

		// The clone re-resolves through the segment list, where the range is
		// gone for good: it resolves (empty) rather than stalling.
		assert!(
			resolving
				.read_frame()
				.now_or_never()
				.expect("should not park below the pruned floor")
				.expect("a pruned range ends the group, it does not error")
				.is_none(),
			"an unlatched reader below the floor is lost"
		);
	}

	/// A reader that already latched a pruned segment's copy keeps draining it: the
	/// cursor holds the buffered frames, and a pruned segment produced everything it
	/// owned, so the copy runs out exactly at the boundary.
	#[tokio::test]
	async fn reader_drains_a_pruned_segments_copy() {
		let mut producer = Producer::new();
		let mut sub = producer.consume().subscribe(None);

		// Group 0 has two frames; the reader consumes one, latching the copy.
		let (mut track, consumer) = track_pair("t0");
		producer.takeover(&consumer).unwrap();
		let mut group = track.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(Timestamp::ZERO, b"f0".to_vec()).unwrap();
		group.write_frame(Timestamp::ZERO, b"f1".to_vec()).unwrap();
		group.finish().unwrap();
		let mut reading = sub.recv_group().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(read(&mut reading), b"f0");
		track.abort(Error::Dropped).unwrap();

		// Failovers prune the segment out from under the latched cursor.
		for sequence in 1..=MAX_SEGMENTS as u64 {
			let (mut track, consumer) = track_pair("t");
			producer.takeover(&consumer).unwrap();
			write_group(&mut track, sequence, "payload");
			assert_eq!(recv(&mut sub), sequence);
			track.abort(Error::Dropped).unwrap();
		}
		assert!(
			producer.state.read().pruned.is_some(),
			"the first segment should be pruned"
		);

		// The second frame still arrives from the latched copy.
		assert_eq!(read(&mut reading), b"f1");
	}
}
