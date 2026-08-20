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
//! each underlying track capped at its segment end, so a session serving a segment
//! just sees an ordinary subscription that happens to end at a boundary. A
//! group-aligned boundary never becomes a *start*: it says which groups this segment
//! owns, not which ones anyone wants, so a subscriber asking for the live edge still
//! asks for it after a switch.
//!
//! Boundaries are frame-precise, so a takeover does not have to wait for the next
//! group. When one lands inside a group the subscriber has already handed out, the
//! group itself is spliced: [`Group`] reads each route's copy in turn and the reader
//! sees one continuous frame stream. This is what lets a track whose current group
//! stays open indefinitely (a JSON append log, a catalog with deltas) survive a route
//! change at all. That is the one boundary a live-edge subscriber does demand, since
//! only demand carries the frame offset the continuation has to start from.

use std::collections::BTreeMap;
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
	///
	/// Clamped to `end`: once the track's edge races past the cap the range
	/// reads as settled through it, even if a position inside it never arrived. A
	/// late arrival is still served (bounds filter reads, not access); the next
	/// takeover splices above the cap either way.
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
	// A segment's bounds and a subscription's are both half-open, so they intersect
	// directly with no inclusive/exclusive conversion in between.
	Subscription {
		start: max_unbounded_start(prefs.start, start),
		end: min_some(prefs.end, end),
		..prefs.clone()
	}
}

/// The later of two optional start bounds, treating `None` as "the live edge" for the
/// preference and "no lower bound" for the segment. Either way the other one wins.
///
/// A takeover boundary therefore becomes demand even for a live-edge subscriber, so the
/// replacement resumes exactly where the dead route stopped and the splice loses nothing.
/// The catch-up that buys is bounded by how far the boundary trails the replacement's live
/// edge, which is failover max_age: a broadcast closes with its last source (there is no
/// linger), so a takeover only happens between overlapping routes. A consumer that would
/// rather skip even that much drops it downstream on its own max age budget, which is what
/// [`std::time::Duration::ZERO`](std::time::Duration::ZERO) means.
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

/// A point-in-time copy of the producer state, reconciled into a
/// [`Subscriber`]'s cursor set by [`Subscriber::apply`].
struct Snapshot {
	epoch: u64,
	finished: bool,
	abort: Option<Error>,
	segments: Vec<Segment>,
}

impl ResumeState {
	fn snapshot(&self) -> Snapshot {
		Snapshot {
			epoch: self.epoch,
			finished: self.finished,
			abort: self.abort.clone(),
			segments: self.segments.clone(),
		}
	}

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
		// One past the newest delivered position. With nothing delivered (or no
		// segments at all) there is nothing to splice around, so the replacement
		// replaces them outright and starts unbounded, exactly like a first splice.
		// `switch` rejects a `None` start once a segment exists, hence the clear.
		let start = state.resume_position();
		if start.is_none() {
			state.segments.clear();
		}
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

	/// One past the newest delivered position across the segments. The origin's
	/// serve task reads this as its delivered-progress signal.
	pub(crate) fn resume_position(&self) -> Option<Position> {
		self.state.read().resume_position()
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
			closed: false,
			segments: Vec::new(),
			next_sequence: 0,
			min_sequence: 0,
			end_sequence: None,
			drift_cap: kio::Producer::new(None),
			stale_stats: Default::default(),
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
/// issues the fetch against the newest segment's track and resolves with it. A
/// fetch whose copy dies fails over: it re-latches onto a newer segment if one
/// already spliced in, or parks for the next takeover like a live subscription
/// (the front aborting the track ends the wait). An error from a copy that is
/// still live (e.g. the group is gone upstream) is authoritative and surfaces.
pub struct Fetching {
	state: kio::Consumer<ResumeState>,
	sequence: u64,
	options: group::Fetch,
	// The latched segment (id, its track, the in-flight fetch), set once a
	// segment exists. Behind a shared lock both to allow `&self` polling and to
	// break the type recursion with `track::Fetching` (which can wrap a resume
	// [`Fetching`]).
	#[allow(clippy::type_complexity)]
	inner: web_async::Lock<Option<(u64, track::Consumer, kio::Pending<track::Fetching>)>>,
}

impl Fetching {
	/// Poll for a segment to latch the fetch onto: the newest one, strictly newer
	/// than `latched` when given. `Err` when the logical track aborted;
	/// `Ok(None)` when the producer is gone with no qualifying segment, so
	/// nothing will ever answer.
	fn poll_latch(&self, waiter: &kio::Waiter, latched: Option<u64>) -> Poll<Result<Option<(u64, track::Consumer)>>> {
		let newest = move |segments: &[Segment]| {
			segments
				.last()
				.filter(|segment| latched.is_none_or(|latched| segment.id > latched))
				.map(|segment| (segment.id, segment.track.clone()))
		};
		match self.state.poll(waiter, |s| match (&s.abort, newest(&s.segments)) {
			(Some(err), _) => Poll::Ready(Err(err.clone())),
			(None, Some(next)) => Poll::Ready(Ok(next)),
			(None, None) => Poll::Pending,
		}) {
			Poll::Ready(Ok(res)) => Poll::Ready(res.map(Some)),
			// The producer is gone; whatever segment it froze with is all
			// there will ever be.
			Poll::Ready(Err(state)) => Poll::Ready(match &state.abort {
				Some(err) => Err(err.clone()),
				None => Ok(newest(&state.segments)),
			}),
			Poll::Pending => Poll::Pending,
		}
	}
}

impl kio::Pollable for Fetching {
	type Output = Result<group::Consumer>;

	fn poll(&self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		let mut inner = self.inner.lock();

		loop {
			if inner.is_none() {
				// Wait for the first segment; the newest wins if several arrived.
				let (id, track) = match ready!(self.poll_latch(waiter, None))? {
					Some(next) => next,
					// The producer died without a route ever serving the track.
					None => return Poll::Ready(Err(Error::NotFound)),
				};
				let fetch = track.fetch_group(self.sequence, self.options.clone());
				*inner = Some((id, track, fetch));
			}

			let (latched, track, fetch) = inner.as_ref().expect("latched above");
			let err = match kio::Pollable::poll(&**fetch, waiter) {
				Poll::Ready(Err(err)) => err,
				Poll::Ready(Ok(group)) => return Poll::Ready(Ok(group)),
				// Park on the resume state too: the front aborting must end an
				// in-flight fetch even when the latched copy never answers it.
				Poll::Pending => {
					return match self.state.poll(waiter, |s| match &s.abort {
						Some(err) => Poll::Ready(err.clone()),
						None => Poll::Pending,
					}) {
						Poll::Ready(Ok(err)) => Poll::Ready(Err(err)),
						// The producer froze without aborting; only the copy can
						// answer now.
						_ => Poll::Pending,
					};
				}
			};

			// The latched copy failed the fetch. Fail over to a segment spliced in
			// above it, if any; ids are monotonic, so "newer" is a plain compare.
			let next = match self.poll_latch(waiter, Some(*latched)) {
				Poll::Ready(res) => res?,
				// No replacement yet, and the waiter is registered for the next
				// switch. A dead copy's failure is the route's, not the group's:
				// park for the takeover, exactly like a live subscription stalls.
				// A live copy's answer stands.
				Poll::Pending => match track.poll_complete(&kio::Waiter::noop()) {
					Poll::Ready(Err(_)) => return Poll::Pending,
					_ => return Poll::Ready(Err(err)),
				},
			};
			// The producer froze with nothing newer spliced in: the latched
			// copy's answer is final.
			let Some((id, track)) = next else {
				return Poll::Ready(Err(err));
			};
			let fetch = track.fetch_group(self.sequence, self.options.clone());
			*inner = Some((id, track, fetch));
			// Loop: poll the replacement fetch in this same pass.
		}
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

/// The last group a half-open segment can serve, or no bound for the newest segment.
fn last_group(end: Option<Position>) -> Option<u64> {
	end?.before().map(|position| position.group)
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
	/// The logical subscription's live max age budget.
	subscription: kio::Consumer<Subscription>,
	/// The logical reader's group cap, used only to bound each route's drift anchor.
	cap: kio::Consumer<Option<u64>>,

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

	/// The tagged logical subscriber's meter for route-copy expiry only.
	stale_stats: crate::stats::Meter,
}

struct Current {
	segment: u64,
	cap: Option<u64>,
	bound: Option<u64>,
	group: group::Consumer,
}

/// A route covering one position: the segment id, its track, and the segment's
/// exclusive frame bound on the group (outer `None` when the group escapes the
/// segment's range entirely, from [`frames`]).
type Covering = (u64, track::Consumer, Option<Option<u64>>, Option<u64>);

impl Clone for Group {
	fn clone(&self) -> Self {
		// Cursors are per-reader, so the clone re-latches its own cursor over the
		// same copy at this reader's position. Re-resolving from the segment list
		// instead would break fanout for a pruned segment's group: the list has
		// forgotten the route, but the latch still holds its frames.
		let current = self.current.as_ref().and_then(|current| {
			let mut group = current.group.clone();
			group.start_at(self.index);
			// The copy no longer holds this position (evicted); the clone
			// re-resolves like any unlatched reader rather than misaligning.
			(group.index() == self.index).then_some(Current {
				segment: current.segment,
				cap: current.cap,
				bound: current.bound,
				group,
			})
		});
		Self {
			state: self.state.clone(),
			subscription: self.subscription.clone(),
			cap: self.cap.clone(),
			sequence: self.sequence,
			index: self.index,
			end: self.end,
			current,
			dead: self.dead.clone(),
			stale_stats: self.stale_stats.clone(),
		}
	}
}

impl Group {
	fn new(
		state: kio::Consumer<ResumeState>,
		subscription: kio::Consumer<Subscription>,
		cap: kio::Consumer<Option<u64>>,
		sequence: u64,
		index: u64,
	) -> Self {
		Self {
			state,
			subscription,
			cap,
			sequence,
			index,
			end: None,
			current: None,
			dead: None,
			stale_stats: Default::default(),
		}
	}

	/// Attribute expiry for route-specific copies behind this logical group.
	pub(crate) fn set_stale_meter(&mut self, meter: crate::stats::Meter) {
		if let Some(current) = &mut self.current {
			current.group.set_stale_meter(meter.clone());
		}
		self.stale_stats = meter;
	}

	/// Pre-latch the delivering route's own copy, so the payload it already
	/// delivered survives even if its segment is pruned before the reader drains
	/// it. Costs nothing otherwise: the per-frame reuse check validates the latch
	/// against the live segment list, so a moved cap still re-routes the reader.
	///
	/// The copy must actually hold this reader's position: a partial copy that
	/// starts higher (a peer delivering a partial group nobody asked for) is not
	/// latched, since the reuse path trusts the latch's alignment and would
	/// misnumber its frames. The reader re-resolves instead, and the peek path's
	/// own check buries the copy as lagged.
	fn latched(mut self, segment: u64, cap: Option<u64>, bound: Option<u64>, mut group: group::Consumer) -> Self {
		// The segment bound is exclusive; a group consumer's cap is inclusive.
		group.end_at(cap.map(|cap| cap.saturating_sub(1)));
		group.start_at(self.index);
		group.set_stale_meter(self.stale_stats.clone());
		if group.index() == self.index {
			self.current = Some(Current {
				segment,
				cap,
				bound,
				group,
			});
		}
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
		// Keep the latch when its copy still covers the new cursor: for a pruned
		// segment it is the only copy left, and the per-read reuse check still
		// re-routes if a different route owns the position. A copy that cannot
		// land exactly there (or whose bound the cursor passed) is dropped, and
		// the next read re-resolves.
		if let Some(current) = &mut self.current {
			current.group.start_at(index);
			if current.group.index() != index || current.cap.is_some_and(|cap| index >= cap) {
				self.current = None;
			}
		}
	}

	pub fn end_at(&mut self, index: Option<u64>) {
		self.end = index;
	}

	/// Locate the route owning `position`: the shared resolve behind the read
	/// cursor and the finish probe. `dead` is the segment already given up on
	/// for these frames. `Ready(None)` once nothing can ever serve them.
	fn poll_covering(&self, position: Position, dead: Option<u64>, waiter: &kio::Waiter) -> Poll<Option<Covering>> {
		let sequence = self.sequence;
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
				// The route that owns these frames was given up on. The verdict is
				// reversible: a peek miss can come from the route's declared start,
				// which demand moving backward lowers, so reconsider before the
				// terminal checks. It has to come first because the arrival itself
				// is what strands us (it moves the resume point past this
				// position), which would otherwise condemn the very copy that
				// resolves the wait. Watching the cache is also what wakes this
				// poll when the copy lands.
				Some(segment) if dead == Some(segment.id) => {
					match segment.track.poll_serving_group(sequence, position.frame, waiter) {
						Poll::Ready(()) => Poll::Ready(Some((
							segment.id,
							segment.track.clone(),
							frames(segment.start, segment.end, sequence).map(|(_, end)| end),
							last_group(segment.end),
						))),
						Poll::Pending => match terminal {
							true => Poll::Ready(None),
							false => Poll::Pending,
						},
					}
				}
				Some(segment) => Poll::Ready(Some((
					segment.id,
					segment.track.clone(),
					frames(segment.start, segment.end, sequence).map(|(_, end)| end),
					last_group(segment.end),
				))),
				// No route owns them yet; park unless none is coming.
				None if terminal => Poll::Ready(None),
				None => Poll::Pending,
			}
		});

		match located {
			Poll::Ready(Ok(found)) => Poll::Ready(found),
			// The producer is gone, so the segment list is frozen.
			Poll::Ready(Err(_)) => Poll::Ready(None),
			Poll::Pending => Poll::Pending,
		}
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
			let found = ready!(self.poll_covering(position, dead, waiter));

			let Some((segment, track, Some(cap), bound)) = found else {
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
				.is_some_and(|current| current.segment == segment && current.cap == cap && current.bound == bound)
			{
				return Poll::Ready(Ok(true));
			}

			// The route may not have delivered this group yet, so wait on its cache.
			let Some(group) = ready!(track.poll_peek_group(sequence, waiter)) else {
				// This route will never have it; fall back to whichever segment replaces it.
				self.dead = Some((segment, Error::NotFound));
				continue;
			};

			// `start_at` clamps up to the first frame the copy still holds, so landing
			// higher than asked means this route can't cover the seam after all. Treat it
			// like a dead copy and wait for one that can.
			let mut group = track.guard_group(group, self.subscription.clone(), self.cap.clone(), bound);
			group.set_stale_meter(self.stale_stats.clone());
			group.start_at(self.index);
			if group.index() != self.index {
				self.dead = Some((segment, Error::Lagged));
				continue;
			}

			// The segment bound is exclusive; a group consumer's cap is inclusive.
			group.end_at(cap.map(|cap| cap.saturating_sub(1)));
			self.current = Some(Current {
				segment,
				cap,
				bound,
				group,
			});
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
			let result = {
				let current = self.current.as_mut().expect("resolved above");
				ready!(current.group.poll_read_frame(waiter))
			};
			let latency_expired = self
				.current
				.as_ref()
				.is_some_and(|current| current.group.latency_expired());
			match result {
				Ok(Some(frame)) => {
					self.index += 1;
					return Poll::Ready(Ok(Some(frame)));
				}
				Ok(None) if self.roll() => continue,
				Ok(None) => return Poll::Ready(Ok(None)),
				Err(err) if latency_expired => return Poll::Ready(Err(err)),
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
			let result = {
				let current = self.current.as_mut().expect("resolved above");
				ready!(current.group.poll_next_frame(waiter))
			};
			let latency_expired = self
				.current
				.as_ref()
				.is_some_and(|current| current.group.latency_expired());
			match result {
				Ok(Some(frame)) => {
					self.index += 1;
					return Poll::Ready(Ok(Some(frame)));
				}
				Ok(None) if self.roll() => continue,
				Ok(None) => return Poll::Ready(Ok(None)),
				Err(err) if latency_expired => return Poll::Ready(Err(err)),
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
		let Some(cap) = current.cap else {
			return current.group.poll_finished(waiter);
		};

		// A bounded copy can't declare the end; the continuation does, unless it
		// can never arrive: then the cap is the group's end. Probed here, not
		// left to `poll_current`: a latched bounded copy resolves without
		// consulting the segment list, so this is the poll that must park on the
		// seam (or a caller that never drains to it would hang with no waiter
		// registered). The covering route's own copy is consulted too, since a
		// route that skip-declared this group (SUBSCRIBE_START above it) never
		// delivers the seam even though its segment covers it.
		let seam = Position {
			group: self.sequence,
			frame: cap,
		};
		loop {
			let dead = self.dead.as_ref().map(|(segment, _)| *segment);
			let Some((segment, track, _, bound)) = ready!(self.poll_covering(seam, dead, waiter)) else {
				return Poll::Ready(Ok(cap));
			};
			match ready!(track.poll_peek_group(self.sequence, waiter)) {
				// The continuation's copy declares the count: its own count
				// already includes the frames it skipped.
				Some(continuation) => {
					let mut continuation =
						track.guard_group(continuation, self.subscription.clone(), self.cap.clone(), bound);
					continuation.set_stale_meter(self.stale_stats.clone());
					return continuation.poll_finished(waiter);
				}
				// This route will never have it; wait for whatever replaces it.
				None => self.dead = Some((segment, Error::NotFound)),
			}
		}
	}
}

/// A subscriber's cursor over one segment.
struct SegmentSub {
	id: u64,
	start: Option<Position>,
	end: Option<Position>,
	sub: SubState,
	/// A completed segment's cursor, retained while parked groups may need their
	/// max age budget re-evaluated after the outer cap rises.
	terminal: Option<track::Subscriber>,
	/// The producer dropped this segment (pruned, or replaced before producing).
	/// The cursor drains what it already holds, then retires; see
	/// [`Self::retired`].
	pruned: bool,
	/// Received groups held back by the subscriber's [`Subscriber::end_at`] cap,
	/// re-offered once the cap rises (arrival-order reads consume the underlying
	/// cursor, so they are parked here instead of dropped). Keyed by sequence so
	/// the lowest is re-offered first; holding them here (rather than blocking on
	/// the first) keeps in-range groups that arrive behind a capped one flowing.
	parked: BTreeMap<u64, group::Consumer>,
}

impl SegmentSub {
	/// The cursor that can evaluate staleness, whether the segment is live or has
	/// already completed.
	fn stale_sub_mut(&mut self) -> Option<&mut track::Subscriber> {
		match &mut self.sub {
			SubState::Active(sub) => Some(sub),
			_ => self.terminal.as_mut(),
		}
	}

	/// Move an active cursor into terminal retention and mark the segment done.
	fn complete(&mut self, count: Option<u64>) {
		let previous = std::mem::replace(&mut self.sub, SubState::Done(count));
		if let SubState::Active(sub) = previous {
			self.terminal = Some(sub);
		}
	}

	/// The first group this segment can serve, for the underlying read cursor.
	fn first_group(&self) -> u64 {
		self.start.map_or(0, |start| start.group)
	}

	/// The last group this segment can serve (inclusive), for the underlying read
	/// cursor. `None` while it is the newest segment.
	fn last_group(&self) -> Option<u64> {
		// An empty segment would serve nothing, and no switch produces one: every
		// boundary comes from `resume_position`, which sits at or above the first frame.
		// `None` here reads as "no cap", which is why it is asserted rather than relied
		// on; the authoritative filter is `Segment::covers`, which uses the exclusive
		// bound directly.
		debug_assert!(
			self.end != Some(Position::default()),
			"a segment cannot end at the start of the track"
		);
		last_group(self.end)
	}

	/// Whether a producer-dropped segment is spent and can be removed. A capped
	/// cursor is kept until it drains (it may hold delivered-but-unread groups,
	/// including one parked at the subscriber's cap, whose re-offer latches the
	/// delivering copy and so still reads out post-prune); an uncapped one was
	/// replaced before producing, so it holds nothing. The straggler bound in
	/// `reap` cuts what lingers too long, parked group and all.
	fn retired(&self) -> bool {
		self.pruned && (self.end.is_none() || (matches!(self.sub, SubState::Done(_)) && self.parked.is_empty()))
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
/// The producer itself going away without a terminal state is an error
/// ([`Error::Dropped`]) once the remaining segments drain: with nobody left to
/// splice a replacement, a stall would never end.
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
	/// The producer is gone without a terminal state: the segment list is frozen,
	/// so once it drains the read paths surface [`Error::Dropped`] (no takeover
	/// can ever resume the track), mirroring a plain track's dropped producer.
	closed: bool,

	/// Cursors over the segments, in segment order.
	segments: Vec<SegmentSub>,

	/// One past the highest sequence returned by [`Self::next_group`].
	next_sequence: u64,
	/// Minimum sequence to surface, set by [`Self::start_at`].
	min_sequence: u64,
	/// Inclusive cap for [`Self::next_group`], set by [`Self::end_at`].
	end_sequence: Option<u64>,
	/// Shared copy of [`Self::end_sequence`] for groups that outlive this cursor poll.
	drift_cap: kio::Producer<Option<u64>>,
	/// Logical subscriber meter for groups drained internally by [`Self::poll_read_frame`].
	stale_stats: crate::stats::Meter,

	/// The group currently being drained by [`Self::read_frame`].
	reading: Option<group::Consumer>,
}

impl Subscriber {
	/// Sync with the producer and preferences: pick up new segments, apply moved
	/// boundaries, re-slice demand, and register the waiter for the next change.
	fn poll_sync(&mut self, waiter: &kio::Waiter) {
		self.sync(waiter);
		self.reap();
	}

	/// Attribute expiry for the group this frame helper drains internally.
	pub(crate) fn set_stale_meter(&mut self, meter: crate::stats::Meter) {
		if let Some(reading) = &mut self.reading {
			reading.set_stale_meter(meter.clone());
		}
		self.stale_stats = meter;
	}

	/// Reap retired cursors, then bound the live stragglers: a pruned segment's
	/// cursor keeps draining (groups below its cap may still arrive out of
	/// order, and its demand keeps the upstream serving them), but only the
	/// newest few. Beyond the bound the oldest are cut, mirroring the
	/// producer-side policy: a reader that far behind loses the range.
	///
	/// Runs from [`Self::poll_sync`] so every polling entry point enforces the
	/// bound; a subscriber driven only through datagrams or `poll_finished`
	/// accumulates cursors all the same.
	fn reap(&mut self) {
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
					seg.terminal = None;
					seg.parked.clear();
					cut -= 1;
				}
			}
			self.segments.retain(|s| !s.retired());
		}
	}

	fn sync(&mut self, waiter: &kio::Waiter) {
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
				let prefs = slice(&self.last_prefs, seg.start, seg.end);
				if let Some(sub) = seg.stale_sub_mut() {
					let _ = sub.update(prefs);
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
					Poll::Ready(state.snapshot())
				} else {
					Poll::Pending
				}
			}) {
				Poll::Ready(Ok(snapshot)) => (Some(snapshot), false),
				// The producer is gone; the state is frozen, so reconcile one last
				// time and stop watching (existing segments can still drain).
				Poll::Ready(Err(state)) => {
					let snapshot = (state.epoch != epoch).then(|| state.snapshot());
					(snapshot, true)
				}
				// Unchanged, and the waiter is now registered for the next switch.
				Poll::Pending => return,
			};

			if let Some(snapshot) = snapshot {
				self.apply(snapshot);
			}
			if closed {
				self.closed = true;
				return;
			}
			// Loop: re-poll so the waiter is registered for the next change.
		}
	}

	/// Apply a producer snapshot: move boundaries on known segments and subscribe
	/// to new ones.
	fn apply(&mut self, snapshot: Snapshot) {
		let Snapshot {
			epoch,
			finished,
			abort,
			segments,
		} = snapshot;
		self.epoch = epoch;
		self.finished = finished;
		self.abort = abort;

		// Mark segments the producer dropped: replaced (never produced anything,
		// so their cursor holds nothing and retires at once) or pruned (capped;
		// the cursor may still hold delivered-but-unread groups, so it drains
		// before retiring; see `poll_segment`). A parked group survives the
		// prune: its re-offer latches the delivering copy (see `hand_out`), so it
		// still reads out, and the straggler bound in `reap` is what keeps such
		// entries from pinning their cursors forever.
		for s in &mut self.segments {
			s.pruned = !segments.iter().any(|n| n.id == s.id);
		}
		self.segments.retain(|s| !s.retired());

		for segment in segments {
			match self.segments.iter_mut().find(|s| s.id == segment.id) {
				Some(existing) => {
					if existing.end != segment.end {
						existing.end = segment.end;
						let cap = Self::stale_cap(existing, self.end_sequence);
						if let Some(sub) = existing.stale_sub_mut() {
							// The boundary bounds the drift anchor as well as the demand.
							sub.set_stale_cap(cap);
							// Shrink the demand so the session can cap upstream. The
							// read bounds stay on this subscriber (see `poll_recv_group`):
							// an inner `end_at` would park boundary-crossing groups in the
							// inner cursor, hiding the segment's completion.
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
						terminal: None,
						pruned: false,
						parked: BTreeMap::new(),
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
		let spliced = Group::new(
			self.state.clone(),
			self.prefs.consume(),
			self.drift_cap.consume(),
			sequence,
			0,
		)
		.latched(seg.id, end, seg.last_group(), group.clone());
		Some(group.into_spliced(spliced))
	}

	/// The highest sequence a segment could hand its reader: the reader's own cap and
	/// the segment's boundary, whichever is lower.
	///
	/// A segment's inner cursor is deliberately left uncapped (an inner `end_at` would
	/// park boundary-crossing groups where its completion can't be seen), so this is how
	/// both bounds reach the drift anchor. Without them a segment measures staleness
	/// against groups it will never surface: the route running past the boundary, or the
	/// reader's own cap holding content back.
	fn stale_cap(seg: &SegmentSub, end_sequence: Option<u64>) -> Option<u64> {
		min_some(end_sequence, seg.last_group())
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
					// Enforce the floor on the read cursor, and re-slice demand in
					// case a boundary moved while the subscription was pending. The
					// upper bounds (segment boundary and `end_at` cap) are enforced by
					// this subscriber, never on the inner cursor: an inner cap would
					// park groups there and hide the segment's completion.
					sub.start_at(seg.first_group().max(min_sequence));
					sub.set_stale_cap(Self::stale_cap(seg, end_sequence));
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
						seg.complete(count);
						return Poll::Ready(None);
					}
					// A dead segment stalls the logical track rather than erroring;
					// the next switch resumes it.
					Poll::Ready(Err(_)) => {
						seg.complete(None);
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
	/// segment completed, and `Poll::Ready(Err(_))` if the producer aborted, or
	/// was dropped without finishing and every segment has drained
	/// ([`Error::Dropped`], like a plain track's dropped producer).
	pub fn poll_recv_group(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<group::Consumer>>> {
		self.poll_sync(waiter);

		let end_sequence = self.end_sequence;
		let min_sequence = self.min_sequence;
		let beyond_cap = |sequence: u64| end_sequence.is_some_and(|end| sequence > end);

		let mut all_done = true;

		// An eviction aborts a parked group without touching any cursor this
		// subscriber polls, so each entry needs a waiter or this poll would never
		// rerun. `poll_closed` observes-or-registers under one lock: `Pending` parks
		// the waiter while the group is open (an open group cannot be aborted), and
		// `Ready` means closed, where only an abort invalidates the entry. A cleanly
		// closed group can never gain an abort, so it needs no waiter. Checking
		// `is_aborted` separately from the registration would leave a window where an
		// abort lands between the two and wakes nobody.
		let watch = |group: &group::Consumer| match group.poll_closed(waiter) {
			Poll::Pending => true,
			Poll::Ready(()) => !group.is_aborted(),
		};

		for index in 0..self.segments.len() {
			// A `start_at` overtook these parked groups; drop them and read on.
			// Eviction/expiry (which aborts a cached group) drops its entry too,
			// bounding parking by the track's cache policy rather than retaining
			// every group a long-capped subscription ever observed.
			self.segments[index]
				.parked
				.retain(|sequence, group| *sequence >= min_sequence && watch(group));

			// Re-offer the lowest parked group back inside the cap once it rises.
			while let Some(&sequence) = self.segments[index].parked.keys().next() {
				if beyond_cap(sequence) {
					break;
				}
				let group = self.segments[index]
					.parked
					.remove(&sequence)
					.expect("parked key just observed");
				// The cap rising widens the live edge too, so a group parked while it
				// was current can be a backlog by the time it is owed again. Re-check it
				// rather than hand back content the budget has since given up on.
				if let Some(sub) = self.segments[index].stale_sub_mut()
					&& matches!(sub.poll_stale(&group, waiter), Poll::Ready(Ok(true)))
				{
					continue;
				}
				// Folded into a group already handed out: try the next parked one.
				if let Some(group) = self.hand_out(index, group) {
					self.next_sequence = self.next_sequence.max(sequence.saturating_add(1));
					return Poll::Ready(Ok(Some(group)));
				}
			}

			loop {
				let polled = Self::poll_segment(
					&mut self.segments[index],
					&self.last_prefs,
					min_sequence,
					end_sequence,
					waiter,
				);
				match polled {
					Poll::Ready(Some(group)) => {
						if beyond_cap(group.sequence) {
							// `end_at` holds the group until the cap rises rather than
							// dropping it; keep draining so an in-range group that
							// arrived behind it still flows. Watch it from the moment it
							// parks: the retain pass above already ran, so an entry
							// admitted here would otherwise sit unwatched for the rest
							// of this poll, and an abort could wake nobody.
							if watch(&group) {
								self.segments[index].parked.insert(group.sequence, group);
							}
							continue;
						}
						if group.sequence < min_sequence {
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
					Poll::Pending => break,
				}
			}

			// Parked groups become deliverable if the cap rises, and a segment that
			// hasn't completed can still produce; either way the track isn't over.
			let seg = &self.segments[index];
			if !seg.parked.is_empty() || !matches!(seg.sub, SubState::Done(_)) {
				all_done = false;
			}
		}

		if let Some(err) = &self.abort {
			return Poll::Ready(Err(err.clone()));
		}
		if all_done {
			if self.finished {
				return Poll::Ready(Ok(None));
			}
			// The producer is gone without finishing: no takeover can ever
			// resume the drained segments, so report the drop like a plain
			// track would rather than stalling forever.
			if self.closed {
				return Poll::Ready(Err(Error::Dropped));
			}
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
				Some(mut group) => {
					group.set_stale_meter(self.stale_stats.clone());
					self.reading = Some(group);
				}
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
		let mut pending_activation = false;
		if let Some(seg) = self.segments.last_mut() {
			if Self::poll_activate(seg, &self.last_prefs, self.min_sequence, self.end_sequence, waiter).is_pending() {
				pending_activation = true;
			} else if let SubState::Active(sub) = &mut seg.sub {
				match sub.poll_recv_datagram(waiter) {
					Poll::Ready(Ok(Some(datagram))) => return Poll::Ready(Ok(Some(datagram))),
					// Terminal states fall through to the logical checks below.
					Poll::Ready(_) => {}
					Poll::Pending => return Poll::Pending,
				}
			}
		}

		if let Some(err) = &self.abort {
			return Poll::Ready(Err(err.clone()));
		}
		if self.finished {
			return Poll::Ready(Ok(None));
		}
		// The newest segment can't progress and the producer is gone: no
		// takeover is coming, so surface the drop rather than stalling forever.
		if self.closed && !pending_activation {
			return Poll::Ready(Err(Error::Dropped));
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
			// A dropped producer can never finish the logical track.
			if self.closed {
				return Poll::Ready(Err(Error::Dropped));
			}
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
					seg.complete(Some(count));
					Poll::Ready(Ok(count))
				}
				Err(_) => {
					seg.complete(None);
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
	///
	/// Enforced on this subscriber's reads (see [`Self::poll_recv_group`]), never
	/// on the inner segment cursors, so a capped group parks here and a rising cap
	/// re-offers it.
	pub fn end_at(&mut self, sequence: impl Into<Option<u64>>) {
		self.end_sequence = sequence.into();
		if let Ok(mut cap) = self.drift_cap.write() {
			*cap = self.end_sequence;
		}
		// The cap bounds each segment's drift anchor as well as this reader's own
		// delivery: a segment must not measure against groups this cap hides.
		let end_sequence = self.end_sequence;
		for seg in &mut self.segments {
			let cap = Self::stale_cap(seg, end_sequence);
			if let Some(sub) = seg.stale_sub_mut() {
				sub.set_stale_cap(cap);
			}
		}
	}

	/// The shared preferences channel, so `track::SubscriberControl` can wrap it.
	pub(crate) fn prefs(&self) -> kio::Producer<Subscription> {
		self.prefs.clone()
	}

	/// Take the groups every segment's drift budget skipped since the last call.
	///
	/// The segments are subscribed untagged, so their skips have nowhere to be counted
	/// until they reach the [`track::Subscriber`] that owns the stats scope. A retired
	/// segment's tail is lost with it, which is the same bound its groups already had.
	pub(crate) fn take_stale(&mut self) -> crate::stats::Content {
		let mut stale = crate::stats::Content::default();
		for seg in &mut self.segments {
			if let Some(sub) = seg.stale_sub_mut() {
				stale.add(sub.take_stale());
			}
		}
		stale
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
	use std::time::Duration;

	fn track_pair(name: &str) -> (track::Producer, track::Consumer) {
		let producer = track::Producer::new(Arc::new(broadcast::Info::default()), name, None);
		let consumer = producer.consume();
		(producer, consumer)
	}

	/// [`track_pair`] with explicit publisher properties, for tests that care about the
	/// retention window a subscriber's drift budget is clamped to.
	fn track_pair_with(name: &str, info: track::Info) -> (track::Producer, track::Consumer) {
		let producer = track::Producer::new(Arc::new(broadcast::Info::default()), name, info);
		let consumer = producer.consume();
		(producer, consumer)
	}

	/// A bounded replay window for tests whose subject requires every buffered group.
	fn replay() -> Subscription {
		Subscription::default().with_max_age(std::time::Duration::from_secs(30))
	}

	fn write_group(producer: &mut track::Producer, sequence: u64, payload: &str) {
		let mut group = producer.create_group(group::Info { sequence }).unwrap();
		group.write_frame(Timestamp::ZERO, payload.as_bytes().to_vec()).unwrap();
		group.finish().unwrap();
	}

	/// Like [`write_group`], but placing the group on a real media timeline so the
	/// drift budget has something to measure. Most tests here are about splicing
	/// mechanics and use [`write_group`], paired with [`replay`] when they need every
	/// buffered group rather than the real-time live edge.
	fn write_group_at(producer: &mut track::Producer, sequence: u64, payload: &str, at: Duration) {
		let mut group = producer.create_group(group::Info { sequence }).unwrap();
		group
			.write_frame(at.try_into().unwrap(), payload.as_bytes().to_vec())
			.unwrap();
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

	/// A waker that counts its wakes, for asserting a pending poll left a live
	/// registration behind.
	struct CountWaker(std::sync::atomic::AtomicUsize);

	impl std::task::Wake for CountWaker {
		fn wake(self: Arc<Self>) {
			self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
		}
	}

	impl CountWaker {
		fn new() -> (Arc<Self>, std::task::Waker) {
			let counter = Arc::new(Self(std::sync::atomic::AtomicUsize::new(0)));
			(counter.clone(), std::task::Waker::from(counter))
		}

		fn count(&self) -> usize {
			self.0.load(std::sync::atomic::Ordering::SeqCst)
		}
	}

	#[tokio::test]
	async fn switch_splices_groups() {
		let (mut track_a, consumer_a) = track_pair("a");
		let (mut track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();

		let mut sub = producer.consume().subscribe(replay());

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
			.subscribe(Subscription::default().with_start(Position::group(0)));
		// Poll once so the subscriber registers on segment A.
		recv_pending(&mut sub);
		assert_eq!(track_a.subscription().unwrap().end, None);

		producer.switch(&consumer_b, Position::group(5)).unwrap();
		recv_pending(&mut sub);

		// The old session sees its demand capped; the new one starts at the boundary.
		assert_eq!(track_a.subscription().unwrap().end, Some(Position::group(5)));
		assert_eq!(track_b.subscription().unwrap().start, Some(Position::group(5)));
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
		let mut sub = producer.consume().subscribe(replay());
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

	/// A route that stalls while a replacement runs on leaves the boundary trailing,
	/// so the takeover asks the new route to replay a backlog. That backlog is bounded
	/// by the drift budget and nothing else: a REAL_TIME subscriber jumps to the live
	/// edge, while one that declared a budget covering the gap reads it whole.
	#[tokio::test]
	async fn a_takeover_backlog_is_bounded_by_the_budget() {
		// Both routes retain a minute, so the budget is the only thing bounding the
		// backlog (a budget past the publisher's window is clamped to it).
		let retain = track::Info::default().with_max_age(std::time::Duration::from_secs(60));
		let (mut track_a, consumer_a) = track_pair_with("a", retain.clone());
		let (mut track_b, consumer_b) = track_pair_with("b", retain);

		let mut producer = Producer::new();
		producer.takeover(&consumer_a).unwrap();
		let mut live = producer.consume().subscribe(None);
		let mut patient = producer
			.consume()
			.subscribe(Subscription::default().with_max_age(std::time::Duration::from_secs(60)));

		write_group_at(&mut track_a, 0, "a0", std::time::Duration::ZERO);
		assert_eq!(recv(&mut live), 0);
		assert_eq!(recv(&mut patient), 0);

		// The old route stalls; the replacement resumes at the boundary and replays
		// half a minute of media as fast as the wire allows.
		track_a.abort(Error::Dropped).unwrap();
		producer.takeover(&consumer_b).unwrap();
		recv_pending(&mut live);
		for second in 1..=30 {
			write_group_at(&mut track_b, second, "b", std::time::Duration::from_secs(second));
		}

		// The live subscriber takes the edge and writes the backlog off; the patient
		// one asked to tolerate it and gets every group.
		assert_eq!(recv(&mut live), 30);
		recv_pending(&mut live);

		let mut backfill = Vec::new();
		while let Some(Ok(Some(group))) = patient.recv_group().now_or_never() {
			backfill.push(group.sequence);
		}
		assert_eq!(backfill, (1..=30).collect::<Vec<_>>());
	}

	/// A capped spliced subscriber measures drift against its cap, exactly like a plain
	/// one. The segment is deliberately left uncapped so its completion stays visible, so
	/// the cap has to reach its drift anchor by another route or the groups the reader
	/// still wants read as ancient.
	#[tokio::test]
	async fn a_capped_spliced_subscriber_measures_drift_against_its_cap() {
		let retain = track::Info::default().with_max_age(std::time::Duration::from_secs(60));
		let (mut track_a, consumer_a) = track_pair_with("a", retain);

		let mut producer = Producer::new();
		producer.takeover(&consumer_a).unwrap();
		let mut sub = producer.consume().subscribe(None);
		sub.end_at(0);

		write_group_at(&mut track_a, 0, "a0", std::time::Duration::ZERO);
		write_group_at(&mut track_a, 1, "a1", std::time::Duration::from_secs(30));

		// Group 1 is past the cap, so it is not a live edge this reader can jump to.
		assert_eq!(recv(&mut sub), 0);
	}

	/// A group parked above the cap is re-checked when the cap rises: the live edge moved
	/// while it waited, so handing it back unconditionally would deliver a backlog the
	/// budget has already given up on.
	#[tokio::test]
	async fn a_raised_cap_rechecks_parked_groups() {
		let retain = track::Info::default().with_max_age(std::time::Duration::from_secs(60));
		let (mut track_a, consumer_a) = track_pair_with("a", retain);

		let mut producer = Producer::new();
		producer.takeover(&consumer_a).unwrap();
		let mut sub = producer.consume().subscribe(None);
		sub.end_at(0);

		write_group_at(&mut track_a, 0, "a0", std::time::Duration::ZERO);
		assert_eq!(recv(&mut sub), 0);

		// Group 1 parks above the cap; group 2 lands half a minute further on.
		write_group_at(&mut track_a, 1, "a1", std::time::Duration::from_secs(1));
		recv_pending(&mut sub);
		write_group_at(&mut track_a, 2, "a2", std::time::Duration::from_secs(30));

		// Raising the cap owes the reader group 1 again, but by now it is a backlog.
		sub.end_at(None);
		assert_eq!(recv(&mut sub), 2);
		recv_pending(&mut sub);
	}

	/// Completing the segment must not discard the cursor that evaluates parked
	/// groups. A later cap increase still uses the terminal track's live edge.
	#[tokio::test]
	async fn a_raised_cap_rechecks_parked_groups_after_segment_finish() {
		let retain = track::Info::default().with_max_age(std::time::Duration::from_secs(60));
		let (mut track_a, consumer_a) = track_pair_with("a", retain);

		let mut producer = Producer::new();
		producer.takeover(&consumer_a).unwrap();
		let mut sub = producer.consume().subscribe(None);
		sub.end_at(0);

		write_group_at(&mut track_a, 0, "a0", std::time::Duration::ZERO);
		assert_eq!(recv(&mut sub), 0);

		write_group_at(&mut track_a, 1, "a1", std::time::Duration::from_secs(1));
		write_group_at(&mut track_a, 2, "a2", std::time::Duration::from_secs(30));
		track_a.finish().unwrap();
		// Drive the inner cursor through its clean end while both newer groups are
		// parked above the cap.
		recv_pending(&mut sub);

		sub.end_at(None);
		assert_eq!(recv(&mut sub), 2, "the stale parked group is skipped after finish");
		recv_pending(&mut sub);
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

	/// A parked beyond-cap group must not block in-range groups that arrive
	/// behind it: a relay can ingest a burst micro-reordered (newest first).
	#[tokio::test]
	async fn end_at_reoffers_reordered_arrivals() {
		let (mut track_a, consumer_a) = track_pair("a");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();
		let mut sub = producer.consume().subscribe(replay());

		sub.end_at(1);

		// Reordered burst: the beyond-cap group arrives first.
		write_group(&mut track_a, 2, "a2");
		write_group(&mut track_a, 0, "a0");
		write_group(&mut track_a, 1, "a1");

		// The capped group parks without blocking the in-range late arrivals.
		assert_eq!(recv(&mut sub), 0);
		assert_eq!(recv(&mut sub), 1);
		recv_pending(&mut sub);

		// Raising the cap re-offers the parked group.
		sub.end_at(2);
		assert_eq!(recv(&mut sub), 2);
	}

	/// A parked group the producer aborts (eviction/expiry) is dropped rather
	/// than re-offered when the cap rises.
	#[tokio::test]
	async fn evicted_parked_groups_are_dropped() {
		let (mut track_a, consumer_a) = track_pair("a");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();
		let mut sub = producer.consume().subscribe(None);

		sub.end_at(0);
		write_group(&mut track_a, 0, "a0");
		assert_eq!(recv(&mut sub), 0);

		let straggler = track_a.create_group(group::Info { sequence: 1 }).unwrap();
		recv_pending(&mut sub);
		straggler.abort(Error::Old).unwrap();

		sub.end_at(None);
		write_group(&mut track_a, 2, "a2");
		assert_eq!(recv(&mut sub), 2, "the evicted parked group is dropped, not re-offered");
	}

	#[tokio::test]
	async fn next_group_skips_boundary_duplicate() {
		let (mut track_a, consumer_a) = track_pair("a");
		let (mut track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();
		let mut sub = producer.consume().subscribe(replay());

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
		use std::task::Context;

		let (track_a, consumer_a) = track_pair("a");
		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();
		let mut sub = producer.consume().subscribe(None);
		let prefs = sub.prefs();

		let (counter, waker) = CountWaker::new();
		let mut cx = Context::from_waker(&waker);

		let mut fut = std::pin::pin!(sub.recv_group());
		assert!(fut.as_mut().poll(&mut cx).is_pending());

		// First update wakes and is applied on the next poll.
		*prefs.write().ok().unwrap() = Subscription::default().with_priority(1);
		assert_eq!(counter.count(), 1);
		assert!(fut.as_mut().poll(&mut cx).is_pending());
		assert_eq!(track_a.subscription().unwrap().priority, 1);

		// The poll that consumed the change must have re-registered: a second
		// update, with no other activity in between, still wakes. Count the delta
		// rather than the total: `kio::Park` reuses a waiter that still holds
		// registrations, so applying the change above notifies a list this poll is
		// itself parked on and self-wakes once. That extra wake costs a redundant
		// poll and nothing else, while a lost wakeup parks the task forever.
		let before = counter.count();
		*prefs.write().ok().unwrap() = Subscription::default().with_priority(2);
		assert!(counter.count() > before, "second update lost its wakeup");
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
		assert_eq!(
			demand.start,
			Some(Position { group: 0, frame: 2 }),
			"resumes in the same group, at the frame the old route stopped on"
		);

		let mut group = track_b.create_group(group::Info { sequence: 0 }).unwrap();
		group.start_at(2).unwrap();
		group.write_frame(Timestamp::ZERO, b"b2".to_vec()).unwrap();
		group.finish().unwrap();

		// Same handle, no seam: the continuation is not surfaced as a second group.
		assert_eq!(read(&mut reading), b"b2");
		assert!(reading.read_frame().now_or_never().unwrap().unwrap().is_none());
		recv_pending(&mut sub);
	}

	#[tokio::test]
	async fn a_replacement_copy_keeps_the_handed_out_group_latency_budget() {
		tokio::time::pause();

		let (mut track_a, consumer_a) = track_pair("a");
		let (mut track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.takeover(&consumer_a).unwrap();
		let mut sub = producer.consume().subscribe(None);

		let mut head = track_a.create_group(group::Info { sequence: 0 }).unwrap();
		head.write_frame(Timestamp::ZERO, b"a0".to_vec()).unwrap();
		let mut reading = sub.recv_group().now_or_never().unwrap().unwrap().expect("head group");
		assert_eq!(read(&mut reading), b"a0");

		producer.takeover(&consumer_b).unwrap();
		track_a.abort(Error::Dropped).unwrap();
		recv_pending(&mut sub);

		let mut continuation = track_b.create_group(group::Info { sequence: 0 }).unwrap();
		continuation.start_at(1).unwrap();
		continuation.write_frame(Timestamp::ZERO, b"b1".to_vec()).unwrap();
		assert_eq!(read(&mut reading), b"b1");

		assert!(
			reading.read_frame().now_or_never().is_none(),
			"the replacement group is still the live edge"
		);

		tokio::time::advance(std::time::Duration::from_secs(1)).await;
		write_group_at(&mut track_b, 1, "edge", std::time::Duration::from_secs(1));

		let result = reading
			.read_frame()
			.now_or_never()
			.expect("the newer group changes the verdict");
		// Drained, so the budget ends the replacement rather than truncating it.
		assert!(matches!(result, Ok(None)), "the replacement ends: {result:?}");
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
			demand.start,
			Some(Position { group: 0, frame: 1 }),
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
		// Exclusive: serve group 0 up to and including frame 0.
		assert_eq!(demand.end, Some(Position { group: 0, frame: 1 }));

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

		// The boundary rolls to the next group once the current one is complete, and
		// the replacement is asked to resume there.
		assert_eq!(producer.resume_position(), Some(Position::group(1)));
		assert_eq!(track_b.subscription().unwrap().start, Some(Position::group(1)));
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
		let mut sub = producer.consume().subscribe(replay());

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
	async fn takeover_after_empty_segment_keeps_live_edge() {
		let (track_a, consumer_a) = track_pair("a");
		let (track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.takeover(&consumer_a).unwrap();
		let mut sub = producer.consume().subscribe(None);
		recv_pending(&mut sub);
		assert_eq!(track_a.subscription().unwrap().start, None);

		// A dies before producing anything; B takes over.
		drop(track_a);
		producer.takeover(&consumer_b).unwrap();
		recv_pending(&mut sub);

		// The replacement must inherit live-edge demand, not a full backfill.
		assert_eq!(track_b.subscription().unwrap().start, None);
	}

	#[tokio::test]
	async fn fetch_fails_over_to_a_newer_segment() {
		let (track_a, consumer_a) = track_pair("a");
		let (mut track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.takeover(&consumer_a).unwrap();
		let consumer = producer.consume();

		// The latched copy is dead: the fetch parks for a takeover instead of
		// surfacing the route's failure as the group's.
		track_a.abort(Error::Dropped).unwrap();
		let fetch = consumer.fetch_group(0, None);
		let mut fetch = std::pin::pin!(fetch);
		assert!(futures::poll!(fetch.as_mut()).is_pending(), "a dead copy should park");

		// The replacement has the group cached; the fetch fails over to it.
		write_group(&mut track_b, 0, "b0");
		producer.takeover(&consumer_b).unwrap();
		let group = fetch.await.expect("fetch should fail over");
		assert_eq!(group.sequence, 0);
	}

	#[tokio::test]
	async fn fetch_aborts_with_the_track() {
		let (track_a, consumer_a) = track_pair("a");

		let mut producer = Producer::new();
		producer.takeover(&consumer_a).unwrap();
		let consumer = producer.consume();

		track_a.abort(Error::Dropped).unwrap();
		let fetch = consumer.fetch_group(0, None);
		let mut fetch = std::pin::pin!(fetch);
		assert!(futures::poll!(fetch.as_mut()).is_pending(), "a dead copy should park");

		// The logical track aborting ends the wait with its error.
		producer.abort(Error::Cancel).unwrap();
		assert!(matches!(fetch.await, Err(Error::Cancel)));
	}

	#[tokio::test]
	async fn fetch_pending_ends_when_the_track_aborts() {
		let (track_a, consumer_a) = track_pair("a");

		let mut producer = Producer::new();
		producer.takeover(&consumer_a).unwrap();
		let consumer = producer.consume();

		// A fetch handler exists but never answers: the latched fetch parks.
		let handler = track_a.dynamic();
		let fetch = consumer.fetch_group(0, None);
		let mut fetch = std::pin::pin!(fetch);
		assert!(
			futures::poll!(fetch.as_mut()).is_pending(),
			"unanswered fetch should park"
		);

		// The front aborting must end the in-flight fetch, not strand it on the
		// copy that will never answer.
		producer.abort(Error::Cancel).unwrap();
		assert!(matches!(fetch.await, Err(Error::Cancel)));
		drop(handler);
	}

	#[tokio::test]
	async fn fetch_error_from_a_live_copy_is_authoritative() {
		let (_track_a, consumer_a) = track_pair("a");

		let mut producer = Producer::new();
		producer.takeover(&consumer_a).unwrap();
		let consumer = producer.consume();

		// The copy is alive and will never carry group 0 (no fetch handler):
		// its answer surfaces instead of parking for a takeover.
		let result = consumer
			.fetch_group(0, None)
			.now_or_never()
			.expect("a live copy's answer must resolve immediately");
		assert!(matches!(result, Err(Error::NotFound)));
	}

	#[tokio::test]
	async fn takeover_after_produced_segment_resumes_at_the_boundary() {
		let (mut track_a, consumer_a) = track_pair("a");
		let (mut track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.takeover(&consumer_a).unwrap();
		let mut sub = producer.consume().subscribe(replay());
		recv_pending(&mut sub);
		assert_eq!(track_a.subscription().unwrap().start, None);

		// A produced groups before its route died; B takes over.
		write_group(&mut track_a, 0, "a0");
		write_group(&mut track_a, 1, "a1");
		assert_eq!(recv(&mut sub), 0);
		assert_eq!(recv(&mut sub), 1);
		drop(track_a);
		producer.takeover(&consumer_b).unwrap();
		recv_pending(&mut sub);

		// A live-edge subscriber resumes at the boundary rather than at B's own live
		// edge, so the splice loses nothing. What that can replay is bounded by how
		// far the boundary trails B, which is failover latency; a consumer that wants
		// none of it skips on its own max age budget downstream.
		assert_eq!(track_b.subscription().unwrap().start, Some(Position::group(2)));

		// The segment range still filters a group below the boundary, so a replacement
		// that serves one anyway never delivers it twice.
		write_group(&mut track_b, 1, "b1");
		recv_pending(&mut sub);

		// Groups at the live edge still arrive past the boundary.
		write_group(&mut track_b, 5, "b5");
		assert_eq!(recv(&mut sub), 5);
	}

	#[tokio::test]
	async fn takeover_keeps_an_explicit_start() {
		let (mut track_a, consumer_a) = track_pair("a");
		let (track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.takeover(&consumer_a).unwrap();
		// The budget is what makes the backfill readable: an explicit start says which
		// groups the publisher should send, not that the subscriber will wait for them.
		let mut sub = producer.consume().subscribe(
			Subscription::default()
				.with_start(Position::group(0))
				.with_max_age(std::time::Duration::from_secs(60)),
		);
		recv_pending(&mut sub);
		assert_eq!(track_a.subscription().unwrap().start, Some(Position::group(0)));

		write_group(&mut track_a, 0, "a0");
		write_group(&mut track_a, 1, "a1");
		assert_eq!(recv(&mut sub), 0);
		assert_eq!(recv(&mut sub), 1);
		drop(track_a);
		producer.takeover(&consumer_b).unwrap();
		recv_pending(&mut sub);

		// A subscriber that asked for history keeps its gapless backfill: the
		// boundary raises the explicit start to the first missing group.
		assert_eq!(track_b.subscription().unwrap().start, Some(Position::group(2)));
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

	/// A capped subscriber riding out route churn keeps its parked groups across
	/// prunes (their re-offer latches the delivering copy, so they still read
	/// out) while the straggler bound cuts the oldest entries whole, parked group
	/// and all, so nothing accumulates without bound.
	#[tokio::test]
	async fn capped_subscriber_bounds_parked_segments() {
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
			"parked entries must stay bounded, not accumulate"
		);
		assert!(tracks[0].subscription().is_none(), "a cut entry releases its demand");

		// Raising the cap re-offers every retained parked group: the pruned ones
		// within the bound deliver through their latched copies, and only the cut
		// ranges are lost.
		sub.end_at(None);
		for sequence in (rounds - 2 * MAX_SEGMENTS as u64 + 1)..=rounds {
			assert_eq!(recv(&mut sub), sequence);
		}
		recv_pending(&mut sub);
	}

	/// A route given up on is not gone for good: the peek miss may have come from
	/// its declared start (SUBSCRIBE_START), which demand moving backward lowers.
	/// When the copy lands after all, the reader revives the route instead of
	/// reporting the frames lost.
	///
	/// The seam is reached with no latched copy in hand (A's is capped at the
	/// boundary and rolls clean), so the revival is the only way through: a stale
	/// latch would otherwise error, re-point `dead` at its own segment, and
	/// un-guard this route by accident.
	#[tokio::test]
	async fn buried_route_revives_when_the_copy_lands() {
		let (mut track_a, consumer_a) = track_pair("a");
		let (mut track_b, consumer_b) = track_pair("b");
		let mut producer = Producer::new();
		producer.takeover(&consumer_a).unwrap();
		let mut sub = producer.consume().subscribe(None);

		// A serves one frame of a group and stops there.
		let mut group = track_a.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(Timestamp::ZERO, b"a0".to_vec()).unwrap();

		// B takes over at the seam and declares it starts at group 1, so the peek
		// for the continuation is a permanent miss and the route is buried.
		producer.takeover(&consumer_b).unwrap();
		track_b.start_at(1).unwrap();

		// Handed out after the boundary exists, so the copy is capped at the seam.
		let mut reading = sub.recv_group().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(read(&mut reading), b"a0");
		assert!(reading.read_frame().now_or_never().is_none(), "the seam parks");

		// Demand widens backward: the floor drops and B serves the continuation
		// after all. Reviving has to beat the strand check, since B producing the
		// frames is itself what moves the resume point past the seam.
		track_b.start_at(None).unwrap();
		let mut cont = track_b.create_group(group::Info { sequence: 0 }).unwrap();
		cont.start_at(1).unwrap();
		cont.write_frame(Timestamp::ZERO, b"b1".to_vec()).unwrap();
		assert_eq!(read(&mut reading), b"b1");
	}

	/// A partial copy nobody asked for (its first frame above the reader's
	/// position, from a protocol-violating peer) is never latched: the latch is
	/// trusted for alignment, so a misaligned one would surface its frames under
	/// the wrong indices. The reader buries the copy as lagged instead.
	///
	/// This is also the guard on the revival in [`Group::poll_covering`], which
	/// reconsiders a buried route ahead of the terminal checks: revival keys on
	/// [`track::Consumer::poll_serving_group`], and if that stopped requiring the
	/// copy to serve the requested frame, the copy here would revive, re-bury on
	/// the very next peek, and loop forever inside one poll. That regression
	/// surfaces as a hang rather than a failed assertion, which nextest reports
	/// as a TIMEOUT.
	#[tokio::test]
	async fn misaligned_copy_is_lost_without_spinning() {
		let (mut track, consumer) = track_pair("t");
		let mut producer = Producer::new();
		producer.takeover(&consumer).unwrap();
		let mut sub = producer.consume().subscribe(None);

		// The copy of group 0 starts at frame 5 while the segment owes the whole
		// group.
		let mut group = track.create_group(group::Info { sequence: 0 }).unwrap();
		group.start_at(5).unwrap();
		group.write_frame(Timestamp::ZERO, b"f5".to_vec()).unwrap();

		let mut reading = sub.recv_group().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(reading.index(), 0);

		// The head is gone for good, so the group ends as a loss naming the copy
		// that could not cover it, rather than surfacing misnumbered frames.
		let err = reading
			.read_frame()
			.now_or_never()
			.expect("the loss must resolve rather than park")
			.expect_err("a misaligned copy is a loss, never misnumbered frames");
		assert!(matches!(err, Error::Lagged), "expected a lagged copy, got {err:?}");
	}

	/// Seeking forward keeps a latched copy that still covers the new position:
	/// for a pruned segment it is the only copy left, so clearing it would lose
	/// frames the reader still holds.
	#[tokio::test]
	async fn seek_keeps_a_pruned_latch() {
		let (mut track, consumer) = track_pair("t");
		let mut producer = Producer::new();
		producer.takeover(&consumer).unwrap();
		let mut sub = producer.consume().subscribe(None);

		let mut group = track.create_group(group::Info { sequence: 0 }).unwrap();
		for payload in [b"f0", b"f1", b"f2"] {
			group.write_frame(Timestamp::ZERO, payload.to_vec()).unwrap();
		}
		group.finish().unwrap();
		let mut reading = sub.recv_group().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(read(&mut reading), b"f0");
		track.abort(Error::Dropped).unwrap();

		// Failovers prune the segment; the latch is all that remains of group 0.
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

		// The seek lands inside the latch: the frame survives the prune.
		reading.start_at(2);
		assert_eq!(read(&mut reading), b"f2");
	}

	/// The straggler bound holds on every polling entry point: a subscriber
	/// driven only through datagrams accumulates a cursor per takeover all the
	/// same, so the reap must run from the shared sync, not just the group path.
	#[tokio::test]
	async fn datagram_poller_bounds_pruned_cursors() {
		let mut producer = Producer::new();
		let mut sub = producer.consume().subscribe(None);

		for sequence in 0..(3 * MAX_SEGMENTS as u64) {
			let (mut track, consumer) = track_pair("t");
			producer.takeover(&consumer).unwrap();
			write_group(&mut track, sequence, "payload");
			assert!(
				kio::wait(|waiter| sub.poll_recv_datagram(waiter))
					.now_or_never()
					.is_none(),
				"no datagram expected"
			);
		}

		assert_eq!(
			sub.segments.len(),
			2 * MAX_SEGMENTS,
			"the reap must run on the datagram path too"
		);
	}

	/// An out-of-order group that lands after its segment was pruned still drains:
	/// the cursor lingers (within the straggler bound) exactly because an empty
	/// poll is not proof of completeness, and its demand is what keeps the
	/// upstream serving the stragglers.
	#[tokio::test]
	async fn late_group_drains_from_a_pruned_cursor() {
		let mut producer = Producer::new();
		let mut sub = producer.consume().subscribe(replay());

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

	/// A handed-out group survives its segment's prune, for every reader: the
	/// delivering copy is latched at hand-out and a clone re-latches it at its
	/// own position (fanout must not depend on the segment list remembering the
	/// route), so both read the payload out and end cleanly instead of stalling.
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
		let mut cloned = reading.clone();
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

		// Both readers still deliver, then the group ends cleanly at what the
		// pruned route produced.
		for reader in [&mut reading, &mut cloned] {
			assert_eq!(read(reader), b"kept");
			assert!(
				reader.read_frame().now_or_never().unwrap().unwrap().is_none(),
				"the group ends at what the route produced"
			);
		}
	}

	/// `finished()` resolves when the seam's covering route skip-declared the
	/// group: its segment geometrically covers the continuation, but its
	/// SUBSCRIBE_START floor proves the group will never arrive, so the cap is
	/// the end. Polled without draining, and woken by the successor's track (the
	/// seam probe parks on the peek), not just the segment list.
	#[tokio::test]
	async fn finished_resolves_when_the_successor_skips_the_seam() {
		let (mut track_a, consumer_a) = track_pair("a");
		let (mut track_b, consumer_b) = track_pair("b");
		let mut producer = Producer::new();
		producer.takeover(&consumer_a).unwrap();
		let mut sub = producer.consume().subscribe(None);

		// A serves two frames of an open group; B splices at the seam.
		let mut group = track_a.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(Timestamp::ZERO, b"f0".to_vec()).unwrap();
		group.write_frame(Timestamp::ZERO, b"f1".to_vec()).unwrap();
		producer.takeover(&consumer_b).unwrap();
		let mut reading = sub.recv_group().now_or_never().unwrap().unwrap().unwrap();
		drop(group);
		track_a.abort(Error::Dropped).unwrap();

		// B covers the seam, so the count is still open.
		assert!(reading.finished().now_or_never().is_none(), "the seam is coverable");

		// B declares it starts at group 1 and produces it: group 0's continuation
		// is skipped for good, so the cap is the end.
		track_b.start_at(1).unwrap();
		write_group(&mut track_b, 1, "b1");
		assert_eq!(recv(&mut sub), 1);
		assert_eq!(
			reading
				.finished()
				.now_or_never()
				.expect("a skip-declared seam must resolve the count")
				.unwrap(),
			2
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

	#[tokio::test]
	async fn switch_replaces_a_run_of_empty_segments() {
		let (mut track_a, consumer_a) = track_pair("a");
		let (_track_b, consumer_b) = track_pair("b");
		let (_track_c, consumer_c) = track_pair("c");
		let (mut track_d, consumer_d) = track_pair("d");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();
		let mut sub = producer.consume().subscribe(None);
		write_group(&mut track_a, 0, "a0");
		assert_eq!(recv(&mut sub), 0);

		// B and C splice in but die before producing anything.
		producer.switch(&consumer_b, Position::group(1)).unwrap();
		producer.switch(&consumer_c, Position::group(2)).unwrap();

		// D's boundary covers both empty segments: one switch replaces the run,
		// and the group they never produced is D's to serve.
		producer.switch(&consumer_d, Position::group(1)).unwrap();
		write_group(&mut track_d, 1, "d1");
		assert_eq!(recv(&mut sub), 1);
		assert_eq!(producer.state.read().segments.len(), 2);
	}

	#[tokio::test]
	async fn abort_drains_before_erroring() {
		let (mut track_a, consumer_a) = track_pair("a");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();
		let mut sub = producer.consume().subscribe(None);

		write_group(&mut track_a, 0, "a0");
		producer.abort(Error::Cancel).unwrap();

		// Buffered groups drain first; then the abort surfaces. Waiting on the
		// end sees the abort immediately (it consumes nothing).
		assert!(matches!(sub.finished().now_or_never().unwrap(), Err(Error::Cancel)));
		assert_eq!(recv(&mut sub), 0);
		assert!(matches!(sub.recv_group().now_or_never().unwrap(), Err(Error::Cancel)));
	}

	#[tokio::test]
	async fn terminal_states_are_exclusive() {
		let (_track_a, consumer_a) = track_pair("a");
		let (_track_b, consumer_b) = track_pair("b");

		// A finished track accepts no further transitions: a late abort (route
		// churn re-queueing a completed track) must not error draining readers.
		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();
		producer.finish().unwrap();
		assert!(matches!(producer.abort(Error::Cancel), Err(Error::Closed)));
		assert!(matches!(producer.finish(), Err(Error::Closed)));
		assert!(matches!(
			producer.switch(&consumer_b, Position::group(1)),
			Err(Error::Closed)
		));
		assert!(matches!(producer.takeover(&consumer_b), Err(Error::Closed)));
		assert!(matches!(producer.release(), Err(Error::Closed)));

		// An aborted track is just as terminal.
		let mut producer = Producer::new();
		producer.abort(Error::Cancel).unwrap();
		assert!(matches!(producer.finish(), Err(Error::Closed)));
		assert!(matches!(producer.abort(Error::Cancel), Err(Error::Closed)));
		assert!(matches!(producer.takeover(&consumer_b), Err(Error::Closed)));
	}

	#[tokio::test]
	async fn dropped_producer_errors_once_drained() {
		let (mut track_a, consumer_a) = track_pair("a");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();
		let mut sub = producer.consume().subscribe(None);

		write_group(&mut track_a, 0, "a0");
		assert_eq!(recv(&mut sub), 0);

		// The segment dies, then the producer goes away without finish/abort:
		// no takeover can ever come, so stalling would hang forever.
		track_a.abort(Error::Cancel).unwrap();
		drop(producer);

		let result = sub.recv_group().now_or_never().expect("must not stall forever");
		assert!(matches!(result, Err(Error::Dropped)));
	}

	#[tokio::test]
	async fn dropped_producer_keeps_a_live_segment_serving() {
		let (mut track_a, consumer_a) = track_pair("a");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();
		let mut sub = producer.consume().subscribe(None);
		drop(producer);

		// The frozen state still has a live segment: groups keep flowing.
		write_group(&mut track_a, 0, "a0");
		assert_eq!(recv(&mut sub), 0);
		recv_pending(&mut sub);

		// Only once that segment ends, without the logical track having
		// finished, does the missing producer surface.
		track_a.finish().unwrap();
		let result = sub.recv_group().now_or_never().expect("must not stall forever");
		assert!(matches!(result, Err(Error::Dropped)));
	}

	#[tokio::test]
	async fn dropped_producer_fails_finished_waiters() {
		let (_track_a, consumer_a) = track_pair("a");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();
		let mut sub = producer.consume().subscribe(None);
		recv_pending(&mut sub);

		// The producer can never finish the logical track once dropped, so an
		// end-waiter must not park forever, even while a segment is still live.
		drop(producer);
		let result = sub.finished().now_or_never().expect("must not stall forever");
		assert!(matches!(result, Err(Error::Dropped)));
	}

	#[tokio::test]
	async fn dropped_producer_ends_datagrams() {
		let (track_a, consumer_a) = track_pair("a");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();
		let mut sub = producer.consume().subscribe(None);

		// The segment is terminal and the producer is gone: a datagram-only
		// poller must observe the drop, not park forever.
		drop(track_a);
		drop(producer);
		let result = kio::wait(|waiter| sub.poll_recv_datagram(waiter))
			.now_or_never()
			.expect("must not stall forever");
		assert!(matches!(result, Err(Error::Dropped)));
	}

	#[tokio::test]
	async fn evicted_parked_group_wakes_the_clean_end() {
		use std::task::Context;

		let (mut track_a, consumer_a) = track_pair("a");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();
		let mut sub = producer.consume().subscribe(None);

		sub.end_at(0);
		write_group(&mut track_a, 0, "a0");
		let straggler = track_a.create_group(group::Info { sequence: 1 }).unwrap();
		assert_eq!(recv(&mut sub), 0);
		recv_pending(&mut sub); // the straggler parks beyond the cap

		track_a.finish().unwrap();
		producer.finish().unwrap();

		let (counter, waker) = CountWaker::new();
		let mut cx = Context::from_waker(&waker);
		let mut fut = std::pin::pin!(sub.recv_group());
		assert!(
			fut.as_mut().poll(&mut cx).is_pending(),
			"the parked group holds the end open"
		);

		// Eviction aborts the parked group behind the subscriber's back: it must
		// wake and observe the clean end rather than sleeping forever.
		straggler.abort(Error::Old).unwrap();
		assert!(counter.count() > 0, "the eviction wakeup was lost");
		let result = fut.as_mut().poll(&mut cx);
		assert!(matches!(result, Poll::Ready(Ok(None))));
	}

	/// The counterpart of [`evicted_parked_group_wakes_the_clean_end`] with the
	/// terminal states already in place when the straggler is first observed:
	/// the poll that parks it is the same poll that sees the segment finish, so
	/// the group must be watched from the moment it parks, not from the next
	/// poll (which nothing would trigger).
	#[tokio::test]
	async fn straggler_parked_after_finish_still_wakes() {
		use std::task::Context;

		let (mut track_a, consumer_a) = track_pair("a");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();
		let mut sub = producer.consume().subscribe(None);

		sub.end_at(0);
		write_group(&mut track_a, 0, "a0");
		assert_eq!(recv(&mut sub), 0);

		let straggler = track_a.create_group(group::Info { sequence: 1 }).unwrap();
		track_a.finish().unwrap();
		producer.finish().unwrap();

		let (counter, waker) = CountWaker::new();
		let mut cx = Context::from_waker(&waker);
		let mut fut = std::pin::pin!(sub.recv_group());
		assert!(
			fut.as_mut().poll(&mut cx).is_pending(),
			"the parked group holds the end open"
		);

		straggler.abort(Error::Old).unwrap();
		assert!(counter.count() > 0, "the abort wakeup was lost");
		assert!(matches!(fut.as_mut().poll(&mut cx), Poll::Ready(Ok(None))));
	}

	#[tokio::test]
	async fn release_restarts_unbounded() {
		let (mut track_a, consumer_a) = track_pair("a");
		let (mut track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.takeover(&consumer_a).unwrap();
		write_group(&mut track_a, 7, "a7");
		assert!(producer.is_spliced());

		// Nobody is reading: release the segments, keeping the track alive.
		producer.release().unwrap();
		assert!(!producer.is_spliced());
		assert_eq!(producer.resume_position(), None);

		// The next takeover starts unbounded: a fresh reader gets the live edge
		// even below the old numbering (the source may have restarted).
		producer.takeover(&consumer_b).unwrap();
		let mut sub = producer.consume().subscribe(None);
		recv_pending(&mut sub);
		assert_eq!(track_b.subscription().unwrap().start, None);
		write_group(&mut track_b, 2, "b2");
		assert_eq!(recv(&mut sub), 2);
	}

	#[tokio::test]
	async fn release_resets_the_pruned_floor() {
		let mut producer = Producer::new();

		// Enough takeovers that the front segments prune, leaving a floor.
		let count = 2 * MAX_SEGMENTS as u64;
		let mut tracks = Vec::new();
		for sequence in 0..count {
			let (mut track, consumer) = track_pair("t");
			producer.takeover(&consumer).unwrap();
			write_group(&mut track, sequence, "g");
			tracks.push(track);
		}
		producer.release().unwrap();

		// The old numbering went with the segments: a restarted source's low
		// sequences must not be filtered by the stale floor.
		let (mut track, consumer) = track_pair("fresh");
		producer.takeover(&consumer).unwrap();
		let mut sub = producer.consume().subscribe(None);
		write_group(&mut track, 0, "g0");
		assert_eq!(recv(&mut sub), 0);
	}

	#[tokio::test]
	async fn fetch_fails_when_the_producer_dies_segmentless() {
		let producer = Producer::new();
		let consumer = producer.consume();

		// No segment yet: the fetch waits for a route to serve the track.
		let fetch = consumer.fetch_group(0, None);
		let mut fetch = std::pin::pin!(fetch);
		assert!(futures::poll!(fetch.as_mut()).is_pending(), "fetch should wait");

		// The producer dies without one ever arriving: nothing can serve it.
		drop(producer);
		assert!(matches!(fetch.await, Err(Error::NotFound)));
	}

	#[tokio::test]
	async fn info_fails_when_the_producer_dies_segmentless() {
		let producer = Producer::new();
		let consumer = producer.consume();

		let info = consumer.info();
		let mut info = std::pin::pin!(info);
		assert!(futures::poll!(info.as_mut()).is_pending(), "info should wait");

		drop(producer);
		assert!(matches!(info.await, Err(Error::Dropped)));
	}

	#[tokio::test]
	async fn start_at_drops_parked_groups_below_the_floor() {
		let (mut track_a, consumer_a) = track_pair("a");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();
		let mut sub = producer.consume().subscribe(None);

		sub.end_at(0);
		write_group(&mut track_a, 0, "a0");
		write_group(&mut track_a, 1, "a1");
		write_group(&mut track_a, 2, "a2");
		assert_eq!(recv(&mut sub), 0);
		recv_pending(&mut sub); // groups 1 and 2 park beyond the cap

		// The reader skips ahead: the parked range below the floor is dropped,
		// while the parked group at the floor is still re-offered.
		sub.start_at(2);
		sub.end_at(None);
		assert_eq!(recv(&mut sub), 2, "group 1 was overtaken by start_at");
		recv_pending(&mut sub);
	}

	#[tokio::test]
	async fn demand_intersects_subscriber_end_with_boundary() {
		let (track_a, consumer_a) = track_pair("a");
		let (track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();
		let mut sub = producer.consume().subscribe(
			Subscription::default()
				.with_start(Position::group(0))
				.with_end(Position::after_group(3)),
		);
		recv_pending(&mut sub);
		assert_eq!(track_a.subscription().unwrap().end, Position::after_group(3));

		producer.switch(&consumer_b, Position::group(2)).unwrap();
		recv_pending(&mut sub);

		// The old segment's demand caps at whichever end is tighter (here the
		// boundary); the new one keeps the subscriber's own end.
		assert_eq!(track_a.subscription().unwrap().end, Some(Position::group(2)));
		assert_eq!(track_b.subscription().unwrap().end, Position::after_group(3));
	}

	#[tokio::test]
	async fn subscribers_read_independently() {
		let (mut track_a, consumer_a) = track_pair("a");
		let (mut track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();
		let consumer = producer.consume();
		let mut sub1 = consumer.subscribe(None);
		let mut sub2 = consumer.subscribe(None);
		recv_pending(&mut sub1);
		recv_pending(&mut sub2);

		write_group(&mut track_a, 0, "a0");
		producer.switch(&consumer_b, Position::group(1)).unwrap();
		write_group(&mut track_b, 1, "b1");

		// Each subscriber holds its own cursors over the shared segments.
		assert_eq!(recv(&mut sub1), 0);
		assert_eq!(recv(&mut sub1), 1);
		assert_eq!(recv(&mut sub2), 0);
		assert_eq!(recv(&mut sub2), 1);
		assert!(sub1.is_clone(&sub2));
	}

	#[tokio::test]
	async fn latest_clamps_to_segment_bounds() {
		let (mut track_a, consumer_a) = track_pair("a");
		let (mut track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();
		let consumer = producer.consume();
		assert_eq!(consumer.latest(), None);

		write_group(&mut track_a, 0, "a0");
		assert_eq!(consumer.latest(), Some(0));

		producer.switch(&consumer_b, Position::group(2)).unwrap();

		// The old route races past its cap: the logical edge stays clamped to
		// the segment's range (its owed range is settled once the track's edge
		// passes the cap, even if some groups in it never arrived).
		write_group(&mut track_a, 5, "a5");
		assert_eq!(consumer.latest(), Some(1));

		// A below-boundary group on the new route (unbounded demand racing the
		// splice) doesn't drag the edge backwards either.
		write_group(&mut track_b, 0, "b0");
		assert_eq!(consumer.latest(), Some(1));

		write_group(&mut track_b, 3, "b3");
		assert_eq!(consumer.latest(), Some(3));
	}

	#[tokio::test]
	async fn datagrams_come_from_the_newest_segment() {
		let (mut track_a, consumer_a) = track_pair("a");
		let (mut track_b, consumer_b) = track_pair("b");

		let mut producer = Producer::new();
		producer.switch(&consumer_a, None).unwrap();
		let mut sub = producer.consume().subscribe(None);
		assert!(
			kio::wait(|waiter| sub.poll_recv_datagram(waiter))
				.now_or_never()
				.is_none(),
			"no datagram yet"
		);

		producer.switch(&consumer_b, Position::group(1)).unwrap();

		// Datagrams are a live best-effort channel: one from a replaced segment
		// is stale and never surfaces, only the live route's flow does.
		track_a.append_datagram(Timestamp::ZERO, b"old".as_ref()).unwrap();
		assert!(
			kio::wait(|waiter| sub.poll_recv_datagram(waiter))
				.now_or_never()
				.is_none(),
			"stale datagram must not surface"
		);
		track_b.append_datagram(Timestamp::ZERO, b"new".as_ref()).unwrap();
		let datagram = kio::wait(|waiter| sub.poll_recv_datagram(waiter))
			.now_or_never()
			.expect("datagram should be ready")
			.expect("should not error")
			.expect("track should not be finished");
		assert_eq!(&datagram.payload[..], b"new");
	}
}
