//! Serve a front's logical track straight from its routes' copies.
//!
//! A front serves each track through one route at a time and switches when that route
//! dies, withdraws, or is beaten by another with the same epoch. Routes with one epoch
//! serve one broadcast, so every route's copy of a track holds the same groups and frames. A reader of the logical
//! track therefore reads the serving route's copy directly, through its own
//! [`Subscriber`]: nothing is copied, and with one route it is a passthrough.
//!
//! When the front switches, each reader subscribes to the new copy from the newest group
//! it handed out. A replaced copy keeps delivering until the new copy delivers its first
//! group, then is bounded just below it (or at the newest group handed out, if later), so a
//! group only the old route got after the switch is not lost. It is kept for unread groups it
//! had and groups still being read (from it, or from another copy while it holds them too),
//! and dropped once neither needs it. A group carries on across the switch through its
//! [`Recover`]: once its copy stalls while a newer route serves, it continues from the
//! serving route's copy of the same group at the same frame (and byte), found in that copy's
//! cache or fetched from it. Once its copy fails, any other copy still holding the group
//! continues it first, since a group's name fixes its content.
//!
//! A group is handed out once however many copies deliver it: the reader skips any
//! sequence its [`track::Subscriber`] already handed out, the same record that keeps a
//! direct reader from taking a re-created group twice.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::task::{Poll, ready};

use crate::{Error, Result, StreamError, group, stats, track};

use super::delivered::Delivered;
use super::subscription::{Anchor, Cap, Position, Resolved, Subscription, Window};

/// What the front says about a logical track: the copy to read now, and how it ended.
#[derive(Default)]
struct Route {
	/// Bumped whenever `copy` changes, so a reader can tell a replacement apart.
	generation: u64,
	/// The serving route's copy; `None` while nobody reads the track, between routes, or
	/// once the front let go.
	copy: Option<track::Consumer>,
	/// How the track ended, once the front decided.
	end: Option<Result<()>>,
	/// No front is left to replace the copy, though one may still hold it for readers
	/// on their way: readers follow it to its end.
	concluded: bool,
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

	/// Serve the track from `copy` from now on, subscribing every reader to it.
	pub(super) fn serve(&self, copy: track::Consumer) {
		if let Ok(mut route) = self.state.write() {
			route.generation += 1;
			route.copy = Some(copy);
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
			route.copy = None;
		}
	}

	/// The track ended: cleanly with `Ok`, or failed for good.
	pub(super) fn end(&self, result: Result<()>) {
		if let Ok(mut route) = self.state.write()
			&& route.end.is_none()
		{
			route.end = Some(result);
		}
	}

	/// No front will replace the serving copy: readers follow it to its end, while this
	/// still holds it for the readers on their way.
	pub(super) fn conclude(&self) {
		if let Ok(mut route) = self.state.write() {
			route.concluded = true;
		}
	}

	/// Let go of the serving copy once no front will replace it and nobody reads the
	/// track. Same generation: a reader on the copy keeps it as the serving one.
	pub(super) fn release(self) {
		if let Ok(mut route) = self.state.write() {
			route.copy = None;
		}
	}

	pub(super) fn consume(&self) -> Consumer {
		Consumer {
			state: self.state.consume(),
			readers: self.readers.clone(),
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
	/// The serving route's copy, if any.
	pub(super) fn serving(&self) -> Option<track::Consumer> {
		self.state.read().copy.clone()
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
			let (generation, copy) = {
				let route = self.state.read();
				(route.generation, route.copy.clone())
			};
			let end = self.end();
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

	/// Subscribe to the logical track with the reader's own preferences.
	pub(super) fn subscribe(&self, subscription: track::Preferences) -> Subscriber {
		let mirrored = subscription.read().clone();
		let window = Window::new(&mirrored.ranges, None);
		// An absolute floor holds from the start, as on a plain cursor; a relative one
		// counts back from an edge, so the reported largest is never below it.
		let floor = window.floor().unwrap_or(0);
		let reader = Arc::new_cyclic(|this| {
			Mutex::new(Reader {
				this: this.clone(),
				route: self.clone(),
				subscription: subscription.clone(),
				mirrored,
				generation: None,
				copies: Vec::new(),
				newest: None,
				window,
				datagram: None,
				groups: (floor, Cap::from(..)),
				groups_read: false,
				peeked: None,
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
		Subscriber { reader, subscription }
	}

	/// Fetch one group from whichever route serves the track. `logical` is the logical
	/// track's state, which the fetched group holds while it is read.
	pub(super) fn fetch_group(
		&self,
		sequence: u64,
		options: group::Fetch,
		logical: kio::Consumer<track::TrackState>,
	) -> Fetching {
		Fetching {
			route: self.clone(),
			sequence,
			options,
			logical,
			pending: None,
		}
	}

	/// Poll for the route state to move past `generation`, or the track to end.
	fn poll_changed(&self, generation: u64, waiter: &kio::Waiter) -> Poll<()> {
		match self.state.poll(waiter, |route| {
			match route.generation != generation || route.end.is_some() || route.concluded {
				true => Poll::Ready(()),
				false => Poll::Pending,
			}
		}) {
			Poll::Pending => Poll::Pending,
			Poll::Ready(_) => Poll::Ready(()),
		}
	}

	/// How the logical track ended: `None` while the front may still replace its copy,
	/// `Some(None)` once it concluded (readers follow the last copy), else the front's
	/// decision.
	fn end(&self) -> Option<Option<Result<()>>> {
		let route = self.state.read();
		match (&route.end, route.concluded || route.is_closed()) {
			(Some(end), _) => Some(Some(end.clone())),
			(None, true) => Some(None),
			(None, false) => None,
		}
	}
}

/// Whether `err` is the route failing rather than refusing: another route may still
/// deliver what it failed.
fn route_failed(err: &Error) -> bool {
	matches!(
		err,
		Error::Transport(_)
			| Error::IdleTimeout
			| Error::Session(_)
			| Error::SessionClosed
			| Error::GoingAway
			| Error::GoawayTimeout
			| Error::Dropped
			| Error::Closed
			| Error::Stream(StreamError::Session(_) | StreamError::GoingAway)
	)
}

/// Whether `copy` failed rather than ended: its route went away, so an error read from it
/// is no verdict on what it carried.
fn copy_failed(copy: &track::Consumer) -> bool {
	matches!(copy.poll_complete(&kio::Waiter::noop()), Poll::Ready(Err(_)))
}

/// A route's copy, as one reader reads it.
struct Copy {
	generation: u64,
	track: track::Consumer,
	sub: Sub,
	/// Where the subscription was asked to start, so demand updates never ask for less.
	floor: Option<Position>,
	/// The reader's ranges [`Self::resolved`] are for.
	from: Vec<track::Range>,
	/// Each of those ranges resolved, by index, so clamping never widens one: as the
	/// serving copy had them when this one was subscribed, until this one answers.
	resolved: Vec<Option<Resolved>>,
	/// Replaced by a newer route: its datagrams are the newer copy's to deliver.
	replaced: bool,
	/// Bounded once the newer copy delivered its first group: nothing past this group is
	/// taken from it. Unset while it may still feed a group the newer copy never has.
	until: Option<Option<u64>>,
	/// The group channel ran out.
	done: Option<Result<()>>,
	/// The datagram channel ran out.
	datagrams_done: Option<Result<()>>,
	/// The newest datagram handed out before this copy was subscribed: a copy that
	/// buffers datagrams replays them, and those were already handed out.
	datagrams_seen: Option<(u64, u64)>,
	/// Held by every group handed out from this copy, or that it held too when handed
	/// out, so a replaced copy stays subscribed while one is still read.
	lease: Arc<()>,
}

// Boxed: a subscriber is twice a pending answer, and a copy holds one for its whole life.
enum Sub {
	Pending(kio::Pending<track::Subscribing>),
	Ready(Box<track::Subscriber>),
}

impl Copy {
	/// What this copy is asked for: the reader's own preferences, from no earlier than
	/// where it was asked to start, and through no later than where it was replaced.
	fn wire(&mut self, reader: &Subscription) -> Subscription {
		match &mut self.sub {
			// Its own resolution, which capping its end must not move.
			Sub::Ready(sub) if reader.ranges == self.from => self.resolved = sub.resolved_ranges(),
			// Replaced ranges resolve against this copy's cache now, as any subscriber's do.
			Sub::Ready(sub) => {
				self.resolved = sub.resolve_ranges(&reader.ranges);
				self.from = reader.ranges.clone();
			}
			Sub::Pending(_) if reader.ranges == self.from => {}
			// Its own resolution, once it answers, is for these.
			Sub::Pending(_) => {
				self.resolved.clear();
				self.from = reader.ranges.clone();
			}
		}
		reader.clamped(self.floor, self.end().map(Position::group), &self.resolved)
	}

	/// The first group past where this copy was replaced, if any: one replaced before
	/// any group ends at 0, and one replaced at `u64::MAX` has no end.
	fn end(&self) -> Option<u64> {
		self.until
			.and_then(|last| last.map_or(Some(0), |last| last.checked_add(1)))
	}

	fn update(&mut self, reader: &Subscription) {
		let wire = self.wire(reader);
		let _ = match &mut self.sub {
			Sub::Pending(pending) => pending.update(wire),
			Sub::Ready(sub) => sub.update(wire),
		};
	}

	/// The subscription, once the copy answered. `Err` once it refused.
	fn poll_ready(&mut self, groups: (u64, Cap), waiter: &kio::Waiter) -> Poll<Result<&mut track::Subscriber>> {
		if let Sub::Pending(pending) = &self.sub {
			let mut sub = ready!(pending.poll_ok(waiter))?;
			// An anchor the wire kept may resolve below where the reader got to.
			sub.raise_start_to(self.floor.map_or(groups.0, |floor| groups.0.max(floor.group)));
			sub.end_at(groups.1);
			self.sub = Sub::Ready(Box::new(sub));
		}
		let Sub::Ready(sub) = &mut self.sub else { unreachable!() };
		Poll::Ready(Ok(sub))
	}

	/// Whether this copy is still needed: it serves, or a group read from or backed by it is
	/// still out. Even once its channel ran out, since a failure continues from what it holds.
	fn needed(&self, serving: bool) -> bool {
		serving || Arc::strong_count(&self.lease) > 1
	}
}

/// One reader's subscription to a logical track; see the module docs.
pub(super) struct Subscriber {
	/// Shared with the groups it hands out, so one stuck on a dead route can follow the
	/// reader onto the next route without the reader polling for groups.
	reader: Arc<Mutex<Reader>>,
	subscription: track::Preferences,
}

impl Subscriber {
	fn reader(&self) -> MutexGuard<'_, Reader> {
		self.reader.lock().unwrap_or_else(PoisonError::into_inner)
	}

	/// The reader's preferences channel, shared with its [`track::Control`].
	pub(super) fn subscription(&self) -> &track::Preferences {
		&self.subscription
	}

	/// Poll for the next group not in `delivered`: in arrival order, or in sequence order
	/// for `ordered`.
	pub(super) fn poll_group(
		&mut self,
		ordered: bool,
		delivered: &Delivered,
		waiter: &kio::Waiter,
	) -> Poll<Result<Option<group::Consumer>>> {
		self.reader().poll_group(ordered, delivered, waiter)
	}

	/// The lowest next group across copies, without handing it out.
	pub(super) fn poll_peek_group(
		&mut self,
		ordered: bool,
		delivered: &Delivered,
		waiter: &kio::Waiter,
	) -> Poll<Result<Option<u64>>> {
		let mut reader = self.reader();
		reader.peeked = None;
		loop {
			if let Some(result) = reader.poll_lowest(ordered, delivered, waiter) {
				return result.map(|result| {
					result.map(|next| {
						let sequence = next.as_ref().map(|next| next.sequence);
						reader.peeked = next;
						sequence
					})
				});
			}
		}
	}

	/// Poll for the next datagram in arrival order. `delivered` decides which replaced
	/// copies still have a group to hand out.
	pub(super) fn poll_recv_datagram(
		&mut self,
		delivered: &Delivered,
		waiter: &kio::Waiter,
	) -> Poll<Result<Option<super::datagram::Stored>>> {
		self.reader().poll_recv_datagram(delivered, waiter)
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
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
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

	/// The lowest group at or above `from` the reader's ranges cover; see
	/// [`track::Subscriber::next_in_range`].
	pub(super) fn next_in_range(&self, from: u64) -> Option<u64> {
		self.reader().window().runs(from).next().map(|run| *run.start())
	}

	/// Poll for sequences the serving copy gave up; see [`track::Subscriber::poll_drop`].
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(super) fn poll_drop(&mut self, waiter: &kio::Waiter) -> Poll<std::ops::Range<u64>> {
		let mut reader = self.reader();
		let sub = ready!(reader.poll_serving(waiter));
		match sub {
			Some(sub) => sub.poll_drop(waiter),
			None => Poll::Pending,
		}
	}

	/// Raise the local read floor; see [`track::Subscriber::set_groups`].
	pub(super) fn raise_start_to(&mut self, start: u64) {
		self.reader().raise_start_to(start)
	}

	/// Move the local read floor to `start`, including downward.
	/// See [`track::Subscriber::start_at`].
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(super) fn start_at(&mut self, start: u64) {
		self.reader().start_at(start)
	}

	/// Cap local reads at `end`; see [`track::Subscriber::set_groups`].
	pub(super) fn end_at(&mut self, end: Cap) {
		self.reader().end_at(end)
	}

	/// Whether any subscribed copy still has a group matching `unread`.
	pub(super) fn has_unread_group(&self, unread: &track::Unread<'_>) -> bool {
		self.reader().copies.iter().any(|copy| match &copy.sub {
			Sub::Ready(sub) => sub.has_unread_group(unread),
			Sub::Pending(_) => false,
		})
	}

	/// The newest sequence any copy has.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(super) fn latest(&self) -> Option<u64> {
		self.reader().latest()
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

	/// The local group filter, as [`track::Subscriber::poll_largest`] reports within it.
	pub(super) fn bounds(&self) -> track::Bounds {
		let (start, end) = self.reader().groups;
		track::Bounds {
			start,
			end: end.exclusive(),
		}
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
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(super) fn poll_live(&mut self, waiter: &kio::Waiter) -> Poll<Option<Position>> {
		let mut reader = self.reader();
		let sub = ready!(reader.poll_serving(waiter));
		match sub {
			Some(sub) => sub.poll_live(waiter),
			None => Poll::Ready(None),
		}
	}
}

struct Candidate {
	generation: u64,
	sequence: u64,
	/// A nested reader may sync a route swap before its caller consumes this
	/// candidate. Keep its exact copy alive until that commit.
	_lease: Arc<()>,
}

/// A [`Subscriber`]'s state.
struct Reader {
	peeked: Option<Candidate>,
	this: Weak<Mutex<Reader>>,
	route: Consumer,
	/// The reader's own preferences, mirrored into every copy's subscription.
	subscription: track::Preferences,
	mirrored: Subscription,
	/// The route generation last read.
	generation: Option<u64>,
	/// Oldest first: the last one is the serving route's, when it is subscribed.
	copies: Vec<Copy>,
	/// The newest group handed out: where a new copy is asked to start.
	newest: Option<u64>,
	/// The reader's ranges resolved; see [`Reader::window`].
	window: Window,
	/// The newest datagram handed out, by Group and Object ID.
	datagram: Option<(u64, u64)>,
	/// The local group filter; see [`track::Subscriber::set_groups`].
	groups: (u64, Cap),
	/// Whether groups were read, so a datagram read keeps the copies they still need.
	groups_read: bool,
}

impl Reader {
	/// The reader's ranges resolved, a relative anchor as the first ready copy to answer for
	/// them resolved it, whatever its failover cap: that bounds what the copy delivers, not
	/// where the ranges resolve. Kept once resolved, past that copy being let go. Pending
	/// while a relative anchor waits on a copy to resolve it.
	fn window(&mut self) -> &Window {
		for index in 0..self.copies.len() {
			if !self.window.is_pending() {
				break;
			}
			let copy = &mut self.copies[index];
			let resolved = match &mut copy.sub {
				Sub::Ready(sub) if copy.from == self.mirrored.ranges => sub.resolved_ranges(),
				_ => continue,
			};
			self.resolve(&resolved);
		}
		&self.window
	}

	/// Resolve a pending [`Self::window`] with a copy's resolution of the reader's ranges,
	/// unless a relative anchor still waits there too.
	fn resolve(&mut self, resolved: &[Option<Resolved>]) {
		let ranges: Vec<_> = self
			.mirrored
			.ranges
			.iter()
			.enumerate()
			.map(|(index, range)| {
				let Some(resolved) = resolved.get(index).copied().flatten() else {
					return *range;
				};
				track::Range {
					start: match range.start {
						// The farthest offset reaches back to the oldest group, which names no floor.
						Anchor::Relative(back) if back != u64::MAX => Anchor::Absolute(Position::group(resolved.start)),
						start => start,
					},
					end: match (range.end, resolved.end) {
						(Some(Anchor::Relative(_)), Some(end)) => Some(Anchor::Absolute(Position::group(end))),
						(end, _) => end,
					},
				}
			})
			.collect();
		let window = Window::new(&ranges, None);
		if !window.is_pending() {
			self.window = window;
		}
	}

	/// Follow the route: subscribe to a new copy, mark the ones it replaced, and mirror
	/// the reader's preferences into each.
	fn sync(&mut self, waiter: &kio::Waiter) {
		// Snapshot and re-arm together, including when a change was observed: another
		// update must wake a reader that parks after applying this one.
		let mut changed = None;
		let _ = self.subscription.poll_ref(waiter, |sub| {
			if **sub != self.mirrored {
				changed = Some((**sub).clone());
			}
			Poll::<()>::Pending
		});
		if let Some(sub) = changed {
			let replaced = sub.ranges != self.mirrored.ranges;
			if replaced {
				self.window = Window::new(&sub.ranges, None);
			}
			self.mirrored = sub;
			for copy in &mut self.copies {
				copy.update(&self.mirrored);
			}
			// A ready copy just resolved the replaced ranges against its cache, before its
			// failover cap raised them, so the reader takes that resolution now.
			if replaced {
				for index in (0..self.copies.len()).rev() {
					if !self.window.is_pending() {
						break;
					}
					if matches!(self.copies[index].sub, Sub::Ready(_)) {
						let resolved = self.copies[index].resolved.clone();
						self.resolve(&resolved);
					}
				}
			}
		}

		let (generation, serving) = {
			let route = self.route.state.read();
			(route.generation, route.copy.clone())
		};
		if self.generation == Some(generation) {
			return;
		}
		self.generation = Some(generation);
		let Some(track) = serving else { return };
		if self.copies.last().is_some_and(|copy| copy.generation == generation) {
			return;
		}
		// Where the serving copy resolved the reader's ranges: the floor raises each start
		// there, never lowering a later range to the floor.
		let resolved = match self.copies.last_mut() {
			Some(Copy {
				sub: Sub::Ready(sub),
				from,
				..
			}) if *from == self.mirrored.ranges => sub.resolved_ranges(),
			// One still unanswered passes on what it was handed.
			Some(Copy {
				sub: Sub::Pending(_),
				from,
				resolved,
				..
			}) if *from == self.mirrored.ranges => resolved.clone(),
			_ => Vec::new(),
		};
		// The replaced copies keep delivering until the new one delivers its first group: a
		// group they get meanwhile may be one the new route never has. See `observe`.
		for copy in &mut self.copies {
			copy.replaced = true;
		}
		let floor = self.newest.map(Position::group);
		// The wire request carries the floor from the first message: some sessions read
		// the demand only once.
		let wire = self.mirrored.clamped(floor, None, &resolved);
		self.copies.push(Copy {
			generation,
			sub: Sub::Pending(track.subscribe(wire)),
			track,
			floor,
			from: self.mirrored.ranges.clone(),
			resolved,
			replaced: false,
			until: None,
			done: None,
			datagrams_done: None,
			datagrams_seen: self.datagram,
			lease: Arc::new(()),
		});
	}

	/// Drop replaced copies only once neither a handed-out nor an unread group needs them.
	fn let_go(&mut self, delivered: &Delivered) {
		let serving = self.generation;
		let groups = self.groups;
		let groups_read = self.groups_read;
		let newest = self.newest;
		let mirrored = &self.mirrored;
		self.copies.retain_mut(|copy| {
			// The serving copy, or one between routes with nothing replacing it yet.
			if !copy.replaced || copy.needed(Some(copy.generation) == serving) {
				return true;
			}
			// Its group channel ran out: it feeds nothing more.
			if copy.done.is_some() {
				return false;
			}
			if copy.until.is_none() {
				// Not yet bounded, it may still feed a group the new copy never has.
				if groups_read {
					return true;
				}
				// Nobody read groups yet, so no new copy's group will bound it: it finishes
				// the groups it has, and nothing newer.
				copy.until = Some(newest.max(copy.track.latest_sequence()));
				copy.update(mirrored);
			}
			let end = copy.end();
			let Poll::Ready(Ok(sub)) = copy.poll_ready(groups, &kio::Waiter::noop()) else {
				return false;
			};
			// Inspect without moving either cursor: a later group poll still chooses its
			// order and applies the current bounds and drift budget. Ignore the local
			// cap here, since unread groups become eligible again when it rises.
			sub.has_unread_group(&track::Unread {
				start: groups.0,
				end,
				delivered,
			})
		});
	}

	/// The copy at `index` hands out `sequence`. The serving copy's first group bounds the
	/// copies it replaced just below it: what they hold under it may be groups the new route
	/// never has, and what is at or above it the new route delivers. Never below a group
	/// already handed out. A group it replays, already handed out, bounds nothing: the
	/// replaced copies may still hold the next.
	fn observe(&mut self, index: usize, sequence: u64) {
		if Some(self.copies[index].generation) != self.generation {
			return;
		}
		let bound = self.newest.max(sequence.checked_sub(1));
		for copy in &mut self.copies[..index] {
			if copy.replaced && copy.until.is_none() {
				copy.until = Some(bound);
				copy.update(&self.mirrored);
			}
		}
	}

	fn deliverable(copy: &Copy, sequence: u64, delivered: &Delivered) -> bool {
		let replaced = copy
			.until
			.is_some_and(|until| until.is_none_or(|until| sequence > until));
		!replaced && !delivered.contains(sequence)
	}

	fn hand_out(&mut self, index: usize, group: group::Consumer) -> group::Consumer {
		let sequence = group.sequence;
		self.newest = self.newest.max(Some(sequence));
		// Another copy holding the group is where a failure continues it, so it stays while
		// the group is read. Only while copies overlap: a passthrough read has none.
		let backups = match self.copies.len() {
			1 => Vec::new(),
			_ => (self.copies.iter().enumerate())
				.filter(|(other, copy)| *other != index && copy.track.peek_group(sequence).is_some())
				.map(|(_, copy)| copy.lease.clone())
				.collect(),
		};
		let copy = &self.copies[index];
		let handoff = group.handoff();
		group.with_recover(Recover {
			sequence,
			reader: Some(self.this.clone()),
			max_delay: None,
			route: self.route.clone(),
			generation: copy.generation,
			copy: copy.track.clone(),
			handoff,
			lease: Some(copy.lease.clone()),
			backups,
			logical: None,
			fetch: None,
		})
	}

	fn poll_group(
		&mut self,
		ordered: bool,
		delivered: &Delivered,
		waiter: &kio::Waiter,
	) -> Poll<Result<Option<group::Consumer>>> {
		loop {
			if let Some(res) = self.poll_group_once(ordered, delivered, waiter) {
				return res;
			}
		}
	}

	/// One pass over the copies; `None` when the route moved meanwhile and it is worth
	/// another.
	fn poll_group_once(
		&mut self,
		ordered: bool,
		delivered: &Delivered,
		waiter: &kio::Waiter,
	) -> Option<Poll<Result<Option<group::Consumer>>>> {
		let candidate = match self.peeked.take() {
			Some(candidate) => candidate,
			None => match self.poll_lowest(ordered, delivered, waiter)? {
				Poll::Ready(Ok(Some(next))) => next,
				Poll::Ready(Ok(None)) => return Some(Poll::Ready(Ok(None))),
				Poll::Ready(Err(err)) => return Some(Poll::Ready(Err(err))),
				Poll::Pending => return Some(Poll::Pending),
			},
		};
		let index = self
			.copies
			.iter()
			.position(|copy| copy.generation == candidate.generation)
			.expect("the candidate's lease retains its copy");
		let Sub::Ready(sub) = &mut self.copies[index].sub else {
			unreachable!()
		};
		let result = match ordered {
			true => sub.poll_next_group(waiter),
			false => sub.poll_recv_group(waiter),
		};
		// Selection reserves the exact group, so eviction can abort that group but
		// cannot substitute a later one and advance the logical reader past a gap.
		let Poll::Ready(Ok(Some(group))) = result else {
			unreachable!("the peek reserved a group")
		};
		assert_eq!(group.sequence, candidate.sequence, "the peek reserved this exact group");
		self.observe(index, group.sequence);
		let group = self.hand_out(index, group);
		Some(Poll::Ready(Ok(Some(group))))
	}

	/// Choose without consuming a candidate, so a newer copy cannot overtake a lower
	/// group still buffered on an older one. A tie favors the newer copy, which bounds
	/// the copies it replaced when it hands that group out.
	fn poll_lowest(
		&mut self,
		ordered: bool,
		delivered: &Delivered,
		waiter: &kio::Waiter,
	) -> Option<Poll<Result<Option<Candidate>>>> {
		self.sync(waiter);
		self.groups_read = true;
		let groups = self.groups;
		let mut lowest: Option<Candidate> = None;
		for index in 0..self.copies.len() {
			let copy = &mut self.copies[index];
			if copy.done.is_some() {
				continue;
			}
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
				let res = sub.poll_peek_group(ordered, waiter);
				match res {
					Poll::Pending => break,
					Poll::Ready(Ok(Some(sequence))) => {
						if !Self::deliverable(&self.copies[index], sequence, delivered) {
							let Sub::Ready(sub) = &mut self.copies[index].sub else {
								unreachable!()
							};
							let _ = match ordered {
								true => sub.poll_next_group(waiter),
								false => sub.poll_recv_group(waiter),
							};
							continue;
						}
						if lowest.as_ref().is_none_or(|lowest| sequence <= lowest.sequence) {
							lowest = Some(Candidate {
								generation: self.copies[index].generation,
								sequence,
								_lease: self.copies[index].lease.clone(),
							});
						}
						break;
					}
					// Drained: the track ended as the copy did, which is not cleanly when
					// its session went away short of the declared end.
					Poll::Ready(Ok(None)) => {
						copy.done = Some(match copy.track.poll_complete(&kio::Waiter::noop()) {
							Poll::Ready(end) => end,
							Poll::Pending => Ok(()),
						});
						break;
					}
					Poll::Ready(Err(err)) => {
						copy.done = Some(Err(err));
						break;
					}
				}
			}
		}
		if let Some(lowest) = lowest {
			return Some(Poll::Ready(Ok(Some(lowest))));
		}
		self.let_go(delivered);
		self.poll_end(waiter, |copy| copy.done.clone())
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
		// The serving copy drained a complete track: that is its end, whichever route
		// would serve it next. A copy that failed is the front's to replace.
		if let Some(Some(Ok(()))) = serving.map(&done) {
			return Some(Poll::Ready(Ok(None)));
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
			// when no route served it. A locally closed copy ends it cleanly; a connection
			// ending retains its session error.
			Some(None) => match serving.map(&done) {
				None => return Some(Poll::Ready(Err(Error::Unroutable))),
				Some(Some(Ok(()) | Err(Error::Closed))) => return Some(Poll::Ready(Ok(None))),
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

	fn poll_recv_datagram(
		&mut self,
		delivered: &Delivered,
		waiter: &kio::Waiter,
	) -> Poll<Result<Option<super::datagram::Stored>>> {
		loop {
			if let Some(res) = self.poll_datagram_once(delivered, waiter) {
				return res;
			}
		}
	}

	fn poll_datagram_once(
		&mut self,
		delivered: &Delivered,
		waiter: &kio::Waiter,
	) -> Option<Poll<Result<Option<super::datagram::Stored>>>> {
		self.sync(waiter);
		self.let_go(delivered);
		let groups = self.groups;
		for index in (0..self.copies.len()).rev() {
			loop {
				let copy = &mut self.copies[index];
				if copy.datagrams_done.is_some() || copy.replaced {
					break;
				}
				let sub = match copy.poll_ready(groups, waiter) {
					Poll::Pending => break,
					Poll::Ready(Err(err)) => {
						copy.datagrams_done = Some(Err(err));
						break;
					}
					Poll::Ready(Ok(sub)) => sub,
				};
				match sub.poll_recv_wire_datagram(waiter) {
					Poll::Pending => break,
					Poll::Ready(Ok(Some(datagram))) => {
						// A relayed group can carry several datagrams, told apart by Object ID.
						let id = (
							datagram.sequence,
							datagram.wire.as_ref().and_then(|wire| wire.object_id).unwrap_or(0),
						);
						if copy.datagrams_seen.is_some_and(|seen| id <= seen) {
							continue;
						}
						self.datagram = self.datagram.max(Some(id));
						return Some(Poll::Ready(Ok(Some(datagram))));
					}
					Poll::Ready(Ok(None)) => {
						copy.datagrams_done = Some(match copy.track.poll_complete(&kio::Waiter::noop()) {
							Poll::Ready(end) => end,
							Poll::Pending => Ok(()),
						});
						break;
					}
					Poll::Ready(Err(err)) => {
						copy.datagrams_done = Some(Err(err));
						break;
					}
				}
			}
		}
		self.poll_end(waiter, |copy| copy.datagrams_done.clone())
	}

	fn poll_finished(&mut self, waiter: &kio::Waiter) -> Poll<Result<Position>> {
		loop {
			self.sync(waiter);
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
						// The copy failed: only the end of the track if no front replaces it.
						Poll::Ready(Err(err)) if concluded.is_some() => return Poll::Ready(Err(err)),
						_ => {}
					},
					Poll::Ready(Err(err)) if concluded.is_some() => return Poll::Ready(Err(err)),
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

	/// The serving copy's subscription, once it answered; `None` once the track ended
	/// without one.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
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

	fn raise_start_to(&mut self, start: u64) {
		self.peeked = None;
		self.groups.0 = self.groups.0.max(start);
		for copy in &mut self.copies {
			if let Sub::Ready(sub) = &mut copy.sub {
				sub.raise_start_to(start);
			}
		}
	}

	/// Assign the local floor. A copy still waiting on its info takes the floor
	/// from the mirrored subscription when it resolves; one already reading has
	/// to be moved, or a group it skipped stays skipped. Never below a copy's own
	/// resume point: groups under it were handed out by an earlier route, or never owed.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	fn start_at(&mut self, start: u64) {
		self.peeked = None;
		self.groups.0 = start;
		for copy in &mut self.copies {
			if let Sub::Ready(sub) = &mut copy.sub {
				sub.start_at(copy.floor.map_or(start, |floor| start.max(floor.group)));
			}
		}
	}

	fn end_at(&mut self, end: Cap) {
		self.peeked = None;
		self.groups.1 = end;
		for copy in &mut self.copies {
			if let Sub::Ready(sub) = &mut copy.sub {
				sub.end_at(end);
			}
		}
	}

	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	fn latest(&self) -> Option<u64> {
		self.copies.iter().filter_map(|copy| copy.track.latest_sequence()).max()
	}
}

/// Carries a group across route changes; see the module docs.
///
/// Held by a group handed out from a logical track. It is consulted only once the
/// group's copy fails or stalls, so a passthrough read never touches it.
pub(super) struct Recover {
	sequence: u64,
	/// The reader it was handed out to, followed onto each new route; `None` for a fetch.
	reader: Option<Weak<Mutex<Reader>>>,
	/// A fetch's own age budget, asked of each route and judged while waiting on one.
	/// `None` leaves the publisher's retention alone; unused for a reader.
	max_delay: Option<std::time::Duration>,
	route: Consumer,
	/// The route generation of the copy being read, and that copy.
	generation: u64,
	copy: track::Consumer,
	/// The head of the group as that copy holds it, which the copy continuing it adopts,
	/// so the serving route caches the group whole.
	handoff: Arc<group::Handoff>,
	/// Keeps a replaced copy subscribed while this group is read from it.
	lease: Option<Arc<()>>,
	/// Keeps the other copies that held this group when handed out subscribed, for a
	/// failure to continue from.
	backups: Vec<Arc<()>>,
	/// A fetch's logical track, held while the group is read. A fetch is no subscription,
	/// so without it the last subscriber leaving parks the track, dropping the copy that
	/// still feeds an open group, and the rest of the group never comes.
	logical: Option<kio::Consumer<track::TrackState>>,
	/// A fetch of the rest of the group from the serving route, by its generation.
	fetch: Option<(u64, Fetch)>,
}

enum Fetch {
	Pending(kio::Pending<track::Fetching>),
	/// The serving route will not fetch it: wait for its subscription instead.
	Refused(Error),
}

impl Clone for Recover {
	fn clone(&self) -> Self {
		Self {
			sequence: self.sequence,
			reader: self.reader.clone(),
			max_delay: self.max_delay,
			route: self.route.clone(),
			generation: self.generation,
			copy: self.copy.clone(),
			handoff: self.handoff.clone(),
			lease: self.lease.clone(),
			backups: self.backups.clone(),
			logical: self.logical.clone(),
			// A clone reads in parallel, so it asks for itself.
			fetch: None,
		}
	}
}

impl Recover {
	/// Whether to look past the copy being read: it stalled while a newer route serves, or
	/// it failed without a verdict on the group. Only a route that serves can refuse a
	/// group on purpose (it is too old, evicted, or the application said so).
	///
	/// Too old and evicted judge a subscription's delivery, not whether the group exists,
	/// so neither is a verdict on a fetch: one answered from a group still arriving over a
	/// subscription asks the route again for the rest when that delivery gives up on it.
	pub(super) fn wants(&self, err: Option<&Error>, waiter: &kio::Waiter) -> bool {
		let verdict = err.is_some_and(|err| match StreamError::from(err) {
			StreamError::Old | StreamError::Evicted => self.reader.is_some(),
			StreamError::App(_) => true,
			_ => false,
		});
		match err {
			Some(_) if !verdict || copy_failed(&self.copy) => true,
			_ => self.route.poll_changed(self.generation, waiter).is_ready(),
		}
	}

	/// Poll for the serving route's copy of the group, positioned at frame `index`.
	///
	/// `failed` is the error the current copy ended with, if it did; a stall is `None`.
	/// Returns the copy's error when nothing can continue the group.
	pub(super) fn poll(
		&mut self,
		index: u64,
		failed: Option<&Error>,
		waiter: &kio::Waiter,
	) -> Poll<Result<Replacement>> {
		// Subscribe the reader to a new route from its newest group, which is this one or
		// later, so that route's subscription delivers the rest of it.
		if let Some(reader) = self.reader.as_ref().and_then(Weak::upgrade) {
			reader.lock().unwrap_or_else(PoisonError::into_inner).sync(waiter);
		}
		loop {
			let (generation, serving, end, closed) = {
				let route = self.route.state.read();
				(
					route.generation,
					route.copy.clone(),
					route.end.clone(),
					route.concluded || route.is_closed(),
				)
			};
			// Registered before looking, so a route change from here on wakes the reader.
			if self.route.poll_changed(generation, waiter).is_ready() && end.is_none() && !closed {
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
			let Some(serving) = serving else {
				// Nothing serves: a failed copy waits for a route, unless none will come.
				return match (failed, closed || end.is_some()) {
					(Some(err), true) => Poll::Ready(Err(err.clone())),
					_ => Poll::Pending,
				};
			};
			// The copy being read still serves: only a failure asks it again.
			if generation == self.generation && failed.is_none() {
				return Poll::Pending;
			}
			let res = self.poll_serving(generation, &serving, index, failed, waiter);
			// The serving route failed too, and no front is left to replace it.
			if let (Poll::Pending, Some(err)) = (&res, failed)
				&& (closed || end.is_some())
				&& copy_failed(&serving)
			{
				return Poll::Ready(Err(match end {
					Some(Err(end)) => end,
					_ => err.clone(),
				}));
			}
			if res.is_pending()
				&& let Some(budget) = self.poll_budget(waiter)
				&& serving.poll_stale(self.sequence, budget, waiter).is_ready()
			{
				return Poll::Ready(Err(Error::Old));
			}
			return res;
		}
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
			let mut group = copy.track.peek_group(self.sequence)?;
			lend_head(&self.handoff, self.sequence, &copy.track, &group);
			group.start_at(index);
			(group.index() == index).then(|| Replacement {
				group,
				generation: copy.generation,
				copy: copy.track.clone(),
			})
		})
	}

	/// [`Self::poll`] once a newer route serves, through its copy `serving`.
	fn poll_serving(
		&mut self,
		generation: u64,
		serving: &track::Consumer,
		index: u64,
		failed: Option<&Error>,
		waiter: &kio::Waiter,
	) -> Poll<Result<Replacement>> {
		// A newer copy's subscription delivered it: continue from there. The copy that
		// failed delivered it already, so its subscription will not again.
		let same = generation == self.generation;
		if !same && let Poll::Ready(Some(mut group)) = serving.poll_group(self.sequence, waiter) {
			lend_head(&self.handoff, self.sequence, serving, &group);
			group.start_at(index);
			if group.index() == index {
				return Poll::Ready(Ok(Replacement {
					group,
					generation,
					copy: serving.clone(),
				}));
			}
		}

		// Otherwise ask for the rest. The subscription may still deliver it first.
		if self.fetch.as_ref().is_none_or(|(asked, _)| *asked != generation) {
			let options = group::Fetch {
				max_delay: self.max_delay,
				frame_start: index,
				..Default::default()
			};
			self.fetch = Some((generation, Fetch::Pending(serving.fetch_group(self.sequence, options))));
		}
		let Some((_, fetch)) = &mut self.fetch else {
			unreachable!("asked above")
		};
		let err = match fetch {
			// That route failed: the next one is asked.
			Fetch::Refused(err) if route_failed(err) || copy_failed(serving) => return Poll::Pending,
			Fetch::Refused(err) => err.clone(),
			Fetch::Pending(pending) => match ready!(kio::Task::poll(&mut **pending, waiter)) {
				Ok(mut group) => {
					lend_head(&self.handoff, self.sequence, serving, &group);
					group.start_at(index);
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
				Err(err) if route_failed(&err) || copy_failed(serving) => {
					*fetch = Fetch::Refused(err);
					return Poll::Pending;
				}
				Err(err) => {
					*fetch = Fetch::Refused(err.clone());
					err
				}
			},
		};
		// A refusal is a verdict only for a copy that failed, and once the serving
		// subscription will not deliver the group either.
		if failed.is_some() && (same || serving.poll_group(self.sequence, waiter).is_ready()) {
			return Poll::Ready(Err(err));
		}

		Poll::Pending
	}

	/// The budget for a late group: the reader's current one, or a fetch's own, which is
	/// unbounded when it named none so that only the publisher's retention applies.
	pub(super) fn poll_budget(&self, waiter: &kio::Waiter) -> Option<std::time::Duration> {
		let Some(reader) = &self.reader else {
			return Some(self.max_delay.unwrap_or(std::time::Duration::MAX));
		};
		let reader = reader.upgrade()?;
		let mut reader = reader.lock().unwrap_or_else(PoisonError::into_inner);
		// A held group or frame may be the only thing being polled. Mirror and watch
		// preferences here too, before judging its budget against the live edge.
		reader.sync(waiter);
		Some(reader.mirrored.max_delay)
	}

	/// Commit the replacement only once the caller has acquired its cursor or frame.
	pub(super) fn adopt(&mut self, replacement: &Replacement) {
		self.generation = replacement.generation;
		self.copy = replacement.copy.clone();
		self.handoff = replacement.group.handoff();
		// A later switch must keep this copy subscribed while the recovered group
		// still reads it, just like a group handed out from that copy directly.
		self.lease = self.reader.as_ref().and_then(Weak::upgrade).and_then(|reader| {
			let reader = reader.lock().unwrap_or_else(PoisonError::into_inner);
			reader
				.copies
				.iter()
				.find(|copy| copy.generation == replacement.generation)
				.map(|copy| copy.lease.clone())
		});
		self.fetch = None;
	}
}

/// Lend the head `handoff` holds to `serving`'s copy of group `sequence`, `group`, when that
/// starts later: the fetch for the rest asks only from the reader's frame.
fn lend_head(handoff: &group::Handoff, sequence: u64, serving: &track::Consumer, group: &group::Consumer) {
	if group.first_frame() > 0
		&& let Some(head) = handoff.head()
	{
		serving.adopt_head(sequence, &head);
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
	sequence: u64,
	options: group::Fetch,
	logical: kio::Consumer<track::TrackState>,
	/// The fetch in flight, and the route generation it went to.
	pending: Option<(u64, kio::Pending<track::Fetching>)>,
}

impl kio::Task for Fetching {
	type Output = Result<group::Consumer>;

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		loop {
			let (generation, serving) = {
				let route = self.route.state.read();
				(route.generation, route.copy.clone())
			};
			let Some(serving) = serving else {
				return match self.route.end() {
					Some(Some(Err(err))) => Poll::Ready(Err(err)),
					Some(_) => Poll::Ready(Err(Error::NotFound)),
					None => {
						ready!(self.route.poll_changed(generation, waiter));
						continue;
					}
				};
			};
			if self.route.poll_changed(generation, waiter).is_ready() && self.route.end().is_none() {
				continue;
			}
			if self.pending.as_ref().is_none_or(|(asked, _)| *asked != generation) {
				self.pending = Some((generation, serving.fetch_group(self.sequence, self.options.clone())));
			}
			let (_, pending) = self.pending.as_mut().expect("asked above");
			let result = match kio::Task::poll(&mut **pending, waiter) {
				Poll::Ready(result) => result,
				Poll::Pending => {
					return match self.route.end() {
						Some(Some(Err(err))) => Poll::Ready(Err(err)),
						_ => Poll::Pending,
					};
				}
			};
			return match result {
				Ok(group) => {
					let handoff = group.handoff();
					Poll::Ready(Ok(group.with_recover(Recover {
						sequence: self.sequence,
						reader: None,
						max_delay: self.options.max_delay,
						route: self.route.clone(),
						generation,
						copy: serving.clone(),
						handoff,
						lease: None,
						backups: Vec::new(),
						logical: Some(self.logical.clone()),
						fetch: None,
					})))
				}
				// The route failed: ask the next one, unless none will come.
				Err(err) if (route_failed(&err) || copy_failed(&serving)) && self.route.end().is_none() => {
					ready!(self.route.poll_changed(generation, waiter));
					continue;
				}
				Err(err) => Poll::Ready(Err(err)),
			};
		}
	}
}

#[cfg(test)]
mod test {
	use std::time::Duration;

	use futures::FutureExt;

	use super::*;
	use crate::Timestamp;
	use crate::broadcast;

	/// A route's copy of the track.
	fn copy() -> track::Producer {
		track::Producer::new(
			Arc::new(broadcast::Info::default()),
			"video",
			crate::track::Info::timed(),
		)
	}

	/// The logical track `routes` serves.
	fn logical(routes: &Producer) -> track::Producer {
		track::Request::new(Arc::new(broadcast::Info::default()), "video")
			.routes(routes.consume())
			.accept(crate::track::Info::timed())
	}

	fn subscribe(logical: &track::Producer, max_delay: Duration) -> track::Subscriber {
		let subscription = Subscription::default().with_max_delay(max_delay);
		logical
			.consume()
			.subscribe(subscription)
			.now_or_never()
			.unwrap()
			.unwrap()
	}

	#[cfg(feature = "async")]
	fn recv(sub: &mut track::Subscriber) -> group::Consumer {
		sub.recv_group()
			.now_or_never()
			.expect("a group")
			.unwrap()
			.expect("track ended")
	}

	#[cfg(feature = "async")]
	fn read(group: &mut group::Consumer) -> Option<Vec<u8>> {
		let frame = group.read_frame().now_or_never().expect("a frame").unwrap();
		frame.map(|frame| frame.payload.to_vec())
	}

	fn ts(ms: u64) -> Timestamp {
		Timestamp::from_millis(ms).unwrap()
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

	/// Retaining an old copy's cache does not keep a parked logical track live.
	#[test]
	fn a_parked_warm_cache_is_not_live() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let resumed = logical.consume();
		let source = copy();
		let mut warm = source.create_group(group::Info { sequence: 2 }).unwrap();
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

		let mut group = a.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(ts(0), b"a".as_ref()).unwrap();
		group.finish().unwrap();

		let mut reading = recv(&mut sub);
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));
		assert_eq!(read(&mut reading), None);
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_stalled_group_continues_from_the_replacement() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(10));

		let mut open = a.create_group(group::Info { sequence: 0 }).unwrap();
		open.write_frame(ts(0), b"a".as_ref()).unwrap();
		open.write_frame(ts(1), b"b".as_ref()).unwrap();
		let mut reading = recv(&mut sub);
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));
		assert_eq!(read(&mut reading), Some(b"b".to_vec()));

		// The replacement holds the whole group; the replaced route stays quiet.
		let b = copy();
		let mut whole = b.create_group(group::Info { sequence: 0 }).unwrap();
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

	#[test]
	#[cfg(feature = "async")]
	fn a_half_read_frame_continues_from_the_replacement() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(10));

		let mut open = a.create_group(group::Info { sequence: 0 }).unwrap();
		let mut frame = open
			.create_frame(crate::frame::Info {
				size: 4,
				timestamp: Some(ts(0)),
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
		let mut whole = b.create_group(group::Info { sequence: 0 }).unwrap();
		whole.write_frame(ts(0), b"abcd".as_ref()).unwrap();
		routes.serve(b.consume());

		assert_eq!(
			&chunks.read_chunk().now_or_never().unwrap().unwrap().unwrap()[..],
			b"cd"
		);
		assert!(chunks.read_chunk().now_or_never().unwrap().unwrap().is_none());
	}

	/// Read a fetched group from frame 0, which must already be cached.
	#[cfg(feature = "async")]
	fn read_cached(track: &track::Consumer, sequence: u64) -> Vec<Vec<u8>> {
		let mut group = track
			.fetch_group(sequence, None)
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

		let mut open = a.create_group(group::Info { sequence: 0 }).unwrap();
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
		let dynamic = b.dynamic();
		routes.serve(b.consume());
		assert_eq!(read(&mut reading), Some(b"b".to_vec()));
		assert!(reading.read_frame().now_or_never().is_none());
		let request = dynamic.requested_group().now_or_never().unwrap().unwrap();
		assert_eq!((request.sequence(), request.frame_start()), (0, 2));
		let mut rest = request.accept(None).unwrap();
		rest.write_frame(ts(2), b"c".as_ref()).unwrap();
		rest.finish().unwrap();
		assert_eq!(read(&mut reading), Some(b"c".to_vec()));
		assert_eq!(read(&mut reading), None);

		let whole = [b"a".to_vec(), b"b".to_vec(), b"c".to_vec()];
		assert_eq!(read_cached(&b.consume(), 0), whole);
		assert_eq!(read_cached(&logical.consume(), 0), whole);
		assert!(dynamic.requested_group().now_or_never().is_none(), "no second fetch");
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

		let mut open = a.create_group(group::Info { sequence: 0 }).unwrap();
		open.write_frame(ts(0), b"a".as_ref()).unwrap();
		let mut reading = recv(&mut sub);
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));

		let b = copy();
		let dynamic = b.dynamic();
		routes.serve(b.consume());
		assert!(reading.read_frame().now_or_never().is_none());
		let request = dynamic.requested_group().now_or_never().unwrap().unwrap();
		assert_eq!(request.frame_start(), 1);
		let mut rest = request.accept(None).unwrap();
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
		let mut open = a.create_group(group::Info { sequence: 0 }).unwrap();
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
		let dynamic = b.dynamic();
		routes.serve(b.consume());
		assert!(fetched.read_frame().now_or_never().is_none());
		let request = dynamic.requested_group().now_or_never().unwrap().unwrap();
		assert_eq!(request.frame_start(), 1);
		let mut rest = request.accept(None).unwrap();
		rest.write_frame(ts(1), b"b".as_ref()).unwrap();
		rest.finish().unwrap();
		assert_eq!(read(&mut fetched), Some(b"b".to_vec()));
		assert_eq!(read(&mut fetched), None);
		let _whole = b.consume().fetch_group(0, None);
		assert!(
			dynamic.requested_group().now_or_never().is_some(),
			"the head was not kept"
		);
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
		for (track, sequences) in [(&a, 0..2), (&b, 0..3)] {
			for sequence in sequences {
				let mut group = track.create_group(group::Info { sequence }).unwrap();
				group.write_frame(ts(sequence), b"x".as_ref()).unwrap();
				group.finish().unwrap();
			}
		}

		assert_eq!(recv(&mut sub).sequence, 0);
		assert_eq!(recv(&mut sub).sequence, 1);
		routes.serve(b.consume());
		assert_eq!(recv(&mut sub).sequence, 2);
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
		for sequence in 0..5 {
			write(&a, sequence);
		}
		sub.set_groups(..3);
		assert!(sub.largest().now_or_never().is_none(), "the largest is past the cap");
		sub.set_groups(..);
		assert_eq!(sub.largest().now_or_never(), Some(Some(Position::group(4))));
	}

	/// A front's reader created with a floor above the serving copy's edge reports nothing
	/// below it, as a plain reader does.
	#[test]
	#[cfg(feature = "async")]
	fn largest_honours_the_subscription_floor_on_the_serving_copy() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let subscription = Subscription::default().with_max_delay(HOUR).with_groups(8..);
		let mut sub = logical
			.consume()
			.subscribe(subscription)
			.now_or_never()
			.unwrap()
			.unwrap();
		for sequence in 0..5 {
			write(&a, sequence);
		}
		assert!(sub.largest().now_or_never().is_none(), "the largest is below the floor");
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
	}

	/// A budget that never writes off a group in these tests.
	const HOUR: Duration = Duration::from_secs(3600);

	/// Write a finished one-frame group, presented at its sequence in milliseconds.
	fn write(track: &track::Producer, sequence: u64) {
		let mut group = track.create_group(group::Info { sequence }).unwrap();
		group.write_frame(ts(sequence), b"x".as_ref()).unwrap();
		group.finish().unwrap();
	}

	/// Every group the reader hands out right now.
	fn drain(sub: &mut track::Subscriber, ordered: bool) -> Vec<u64> {
		let mut sequences = Vec::new();
		loop {
			let res = match ordered {
				true => sub.poll_next_group(&kio::Waiter::noop()),
				false => sub.poll_recv_group(&kio::Waiter::noop()),
			};
			let Poll::Ready(Ok(Some(group))) = res else {
				return sequences;
			};
			sequences.push(group.sequence);
		}
	}

	/// A replaced copy is bounded only once the new copy delivers its first group: a group
	/// buffered on it must be read before a later group on the serving copy.
	#[test]
	fn sequence_reads_take_a_replaced_copys_lower_buffered_group() {
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
				let mut one = a.create_group(group::Info { sequence: 1 }).unwrap();
				one.write_frame(ts(1), b"x".as_ref()).unwrap();
				one.finish().unwrap();
				write(&a, 3);
				let waiter = kio::Waiter::noop();
				assert_eq!(
					sub.poll_peek_group(ordered, &waiter).map(Result::unwrap),
					Poll::Ready(Some(1))
				);
				one.abort(Error::Evicted).unwrap();
				let b = copy();
				routes.serve(b.consume());
				write(&b, 2);
				let next = match ordered {
					true => sub.poll_next_group(&waiter),
					false => sub.poll_recv_group(&waiter),
				};
				let Poll::Ready(Ok(Some(group))) = next else {
					panic!("the reserved group")
				};
				assert_eq!(group.sequence, 1, "ordered={ordered}, nested={nested}");
				assert_eq!(drain(&mut sub, ordered), [2], "ordered={ordered}, nested={nested}");
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

	/// A replaced copy is bounded only once the new copy delivers its first group: a group
	/// the old route gets after the swap, which the new route never has, still arrives.
	#[test]
	fn a_group_the_old_route_gets_after_the_swap_is_delivered() {
		for ordered in [false, true] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, Duration::ZERO);
			write(&a, 0);
			assert_eq!(drain(&mut sub, ordered), [0]);

			let b = copy();
			routes.serve(b.consume());
			// Group 1 reaches the old route after the swap; the new route joins at group 2.
			write(&a, 1);
			assert_eq!(drain(&mut sub, ordered), [1]);
			write(&b, 2);
			assert_eq!(drain(&mut sub, ordered), [2]);
			// Bounded below the new copy's first group: later groups come from the new route.
			write(&a, 3);
			assert_eq!(drain(&mut sub, ordered), Vec::<u64>::new());
			write(&b, 3);
			assert_eq!(drain(&mut sub, ordered), [3]);
		}
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
		let mut whole = a.create_group(group::Info { sequence: 1 }).unwrap();
		whole.write_frame(ts(1), b"a".as_ref()).unwrap();
		whole.write_frame(ts(2), b"b".as_ref()).unwrap();
		whole.finish().unwrap();
		let mut partial = b.create_group(group::Info { sequence: 1 }).unwrap();
		partial.write_frame(ts(1), b"a".as_ref()).unwrap();
		b.clone().abort(Error::Dropped).unwrap();
		let mut failing = c.create_group(group::Info { sequence: 1 }).unwrap();
		failing.write_frame(ts(1), b"a".as_ref()).unwrap();
		let mut reading = recv(&mut sub);
		assert_eq!(reading.sequence, 1);
		// a's route fails too, after it finished the group.
		a.clone().abort(Error::Dropped).unwrap();
		failing.abort(Error::SessionClosed).unwrap();
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
			let mut whole = a.create_group(group::Info { sequence: 1 }).unwrap();
			whole.write_frame(ts(1), b"a".as_ref()).unwrap();
			whole.write_frame(ts(2), b"b".as_ref()).unwrap();
			whole.finish().unwrap();
			let mut failing = b.create_group(group::Info { sequence: 1 }).unwrap();
			failing.write_frame(ts(1), b"a".as_ref()).unwrap();
			let mut failing = Some(failing);
			if early {
				failing.take().unwrap().abort(Error::SessionClosed).unwrap();
			}
			let noop = kio::Waiter::noop();
			let res = match ordered {
				true => sub.poll_next_group(&noop),
				false => sub.poll_recv_group(&noop),
			};
			let Poll::Ready(Ok(Some(mut reading))) = res else {
				panic!("expected group 1");
			};
			assert_eq!(reading.sequence, 1);
			// Reading on must not let go of the replaced copy while the group still needs it.
			assert_eq!(drain(&mut sub, ordered), Vec::<u64>::new());
			if let Some(failing) = failing {
				failing.abort(Error::SessionClosed).unwrap();
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

			let mut whole = a.create_group(group::Info { sequence: 1 }).unwrap();
			whole.write_frame(ts(1), b"a".as_ref()).unwrap();
			whole.write_frame(ts(2), b"b".as_ref()).unwrap();
			whole.finish().unwrap();
			let mut failing = b.create_group(group::Info { sequence: 1 }).unwrap();
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
			failing.abort(Error::SessionClosed).unwrap();
			assert_eq!(read(&mut reading), Some(b"b".to_vec()));
			assert_eq!(read(&mut reading), None);
		}
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
		for sequence in 2..=2000 {
			write(&b, sequence);
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
		for sequence in 1..=2000 {
			write(&b, sequence);
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
		let open = a.create_group(group::Info { sequence: 1 }).unwrap();
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
			for sequence in groups.clone().take(5) {
				write(serving, sequence);
			}
			seen.extend(drain(&mut sub, false));
			for sequence in groups.clone().skip(5) {
				write(serving, sequence);
			}
			// The other route takes over and the replaced copy hands out the rest; the
			// other's own copy of the round lands only afterwards, newest first.
			routes.serve(other.consume());
			seen.extend(drain(&mut sub, false));
			for sequence in groups.clone().rev() {
				write(other, sequence);
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
		sub.end_at(Cap::from(..=0));
		let mut held = a.create_group(group::Info { sequence: 0 }).unwrap();
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
		for sequence in [2, 0, 3, 1] {
			write(&b, sequence);
		}
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());

		sub.end_at(Cap::from(..));
		let mut seen = drain(&mut sub, false);
		seen.sort_unstable();
		assert_eq!(seen, [1, 2, 3]);
		sub.start_at(0);
		assert_eq!(drain(&mut sub, false), Vec::<u64>::new());

		// The replaced copy finishes with the held group.
		held.finish().unwrap();
		a.finish().unwrap();
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
		for sequence in 0..3000u64 {
			let track = &tracks[(sequence / 500) as usize % 2];
			// Every 500 groups the other route takes over.
			if sequence % 500 == 0 {
				routes.serve(track.consume());
			}
			// Every third group never arrives.
			if sequence % 3 == 1 {
				continue;
			}
			write(track, sequence);
			assert_eq!(drain(&mut sub, false), [sequence]);
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

	/// A replacement copy asked to start at the newest group handed out keeps that
	/// floor when the reader's floor widens: an older group its route cached was
	/// never owed, and handing it out now would be out of order.
	#[test]
	#[cfg(feature = "async")]
	fn a_widened_floor_stays_above_the_replacement_resume_point() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(10));
		let mut group = a.create_group(group::Info { sequence: 5 }).unwrap();
		group.write_frame(ts(5), b"x".as_ref()).unwrap();
		group.finish().unwrap();
		assert_eq!(recv(&mut sub).sequence, 5);

		let b = copy();
		routes.serve(b.consume());
		// Subscribe the replacement before it has anything, so the update moves a ready copy.
		assert!(sub.recv_group().now_or_never().is_none());
		for sequence in [2, 6] {
			let mut group = b.create_group(group::Info { sequence }).unwrap();
			group.write_frame(ts(sequence), b"x".as_ref()).unwrap();
			group.finish().unwrap();
		}

		sub.start_at(0);
		assert_eq!(recv(&mut sub).sequence, 6);
	}

	/// A replacement copy keeps a later range disjoint: the floor raises nothing above it,
	/// and capping the replaced copy never widens its end.
	#[test]
	#[cfg(feature = "async")]
	fn a_replacement_keeps_a_later_range_disjoint() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let ranges = [track::Range::groups(0..1), track::Range::groups(10..12)];
		let subscription = Subscription::default()
			.with_max_delay(Duration::from_secs(30))
			.with_ranges(ranges);
		let mut sub = logical
			.consume()
			.subscribe(subscription)
			.now_or_never()
			.unwrap()
			.unwrap();
		for sequence in 0..13 {
			let mut group = a.create_group(group::Info { sequence }).unwrap();
			group.write_frame(ts(sequence * 1000), b"x".as_ref()).unwrap();
			group.finish().unwrap();
		}
		assert_eq!(recv(&mut sub).sequence, 0);

		let b = copy();
		routes.serve(b.consume());
		// The read subscribes the replacement while the replaced copy still serves.
		let mut rest = Vec::new();
		while let Some(Ok(Some(group))) = sub.recv_group().now_or_never() {
			rest.push(group.sequence);
		}
		for sequence in 0..13 {
			let mut group = b.create_group(group::Info { sequence }).unwrap();
			group.write_frame(ts(sequence * 1000), b"x".as_ref()).unwrap();
			group.finish().unwrap();
		}
		while let Some(Ok(Some(group))) = sub.recv_group().now_or_never() {
			rest.push(group.sequence);
		}
		rest.sort();
		assert_eq!(rest, vec![10, 11]);
	}

	/// A replacement updated to a relative range it cannot resolve yet asks for that range,
	/// not everything from the floor: before it answers, and once it answers with nothing
	/// to resolve it against.
	#[test]
	fn a_replacement_keeps_an_updated_relative_start() {
		let routes = Producer::new();
		let a = copy();
		routes.serve(a.consume());
		let sub = routes
			.consume()
			.subscribe(kio::Producer::new(Subscription::default()).into());
		let mut reader = sub.reader.lock().unwrap();
		let copy = &mut reader.copies[0];
		copy.floor = Some(Position::group(0));
		let ranges = vec![track::Range {
			start: track::Anchor::Relative(3),
			end: None,
		}];
		let updated = Subscription::default().with_ranges(ranges.clone());
		assert!(matches!(copy.sub, Sub::Pending(_)));
		copy.update(&updated);
		assert_eq!(copy.wire(&updated).ranges, ranges);
		assert!(copy.poll_ready((0, Cap::from(..)), &kio::Waiter::noop()).is_ready());
		assert_eq!(copy.wire(&updated).ranges, ranges);
	}

	/// A replacement asked for a range reaching back past where the reader got to, while
	/// still unanswered, never hands out a group the reader skipped below that point.
	#[test]
	#[cfg(feature = "async")]
	fn a_pending_replacement_never_reads_below_the_floor() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(30));
		let mut five = a.create_group(group::Info { sequence: 5 }).unwrap();
		five.write_frame(ts(5000), b"x".as_ref()).unwrap();
		assert_eq!(recv(&mut sub).sequence, 5);

		let b = track::Request::new(Arc::new(broadcast::Info::default()), "video");
		routes.serve(b.consume());
		// The read subscribes the replacement, which has not answered yet.
		assert!(sub.recv_group().now_or_never().is_none());
		let back = sub.subscription().with_ranges([track::Range {
			start: track::Anchor::Relative(100),
			end: None,
		}]);
		sub.update(back).unwrap();
		assert!(sub.recv_group().now_or_never().is_none());

		let b = b.accept(crate::track::Info::timed());
		for sequence in 0..7 {
			let mut group = b.create_group(group::Info { sequence }).unwrap();
			group.write_frame(ts(sequence * 1000), b"x".as_ref()).unwrap();
			group.finish().unwrap();
		}
		let mut rest = Vec::new();
		while let Some(Ok(Some(group))) = sub.recv_group().now_or_never() {
			rest.push(group.sequence);
		}
		assert_eq!(rest, vec![6]);
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
		let mut zero = a.create_group(group::Info { sequence: 0 }).unwrap();
		zero.write_frame(ts(0), b"x".as_ref()).unwrap();
		assert_eq!(recv(&mut sub).sequence, 0);

		let b = track::Request::new(Arc::new(broadcast::Info::default()), "video");
		routes.serve(b.consume());
		// The read subscribes the replacement, which has not answered yet.
		assert!(sub.recv_group().now_or_never().is_none());
		let ranges = [track::Range {
			start: track::Anchor::Relative(2),
			end: None,
		}];
		sub.update(sub.subscription().with_ranges(ranges)).unwrap();
		assert!(sub.recv_group().now_or_never().is_none());

		let b = b.accept(crate::track::Info::timed());
		for sequence in 0..13 {
			let mut group = b.create_group(group::Info { sequence }).unwrap();
			group.write_frame(ts(sequence * 1000), b"x".as_ref()).unwrap();
			group.finish().unwrap();
		}
		while let Some(Ok(Some(_))) = sub.recv_group().now_or_never() {}

		let c = copy();
		routes.serve(c.consume());
		assert!(sub.recv_group().now_or_never().is_none());
		let start = c.subscription().expect("subscribed").ranges[0].start;
		assert!(
			matches!(start, track::Anchor::Absolute(_)),
			"the replacement asks from where b resolved the relative start: {start:?}"
		);
	}

	#[test]
	fn a_replaced_copy_at_the_last_sequence_has_no_overflow_cap() {
		for (last, end) in [(Some(u64::MAX), None), (None, Some(Position::default()))] {
			let routes = Producer::new();
			let a = copy();
			routes.serve(a.consume());
			let subscription = kio::Producer::new(Subscription::default());
			let sub = routes.consume().subscribe(subscription.into());
			let mut reader = sub.reader.lock().unwrap();
			let old = &mut reader.copies[0];
			old.until = Some(last);
			assert_eq!(old.wire(&Subscription::default()).end(), end);
			// A reader's narrower bound survives an unbounded replacement cap.
			let requested = Subscription::default().with_end(Position::group(7));
			assert_eq!(old.wire(&requested).end(), Some(end.unwrap_or(Position::group(7))));
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
		let mut first = a.create_group(group::Info { sequence: 0 }).unwrap();
		first.write_frame(ts(0), b"a0".as_ref()).unwrap();
		assert_eq!(recv(&mut sub).sequence, 0);

		let b = copy();
		routes.serve(b.consume());
		// Both routes go on to group 1; only the serving one's copy counts.
		for (track, payload) in [(&a, b"a1"), (&b, b"b1")] {
			let mut group = track.create_group(group::Info { sequence: 1 }).unwrap();
			group.write_frame(ts(1), payload.as_ref()).unwrap();
			group.finish().unwrap();
		}
		let mut next = recv(&mut sub);
		assert_eq!(next.sequence, 1);
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
		let mut open = a.create_group(group::Info { sequence: 0 }).unwrap();
		open.write_frame(ts(0), b"a".as_ref()).unwrap();
		let mut reading = recv(&mut sub);
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));

		// The replacement starts past the group and cannot fetch it; the replaced route
		// stays up but quiet.
		let b = copy();
		routes.serve(b.consume());
		for sequence in 1..=2 {
			let mut group = b.create_group(group::Info { sequence }).unwrap();
			group.write_frame(ts(sequence * 1000), b"b".as_ref()).unwrap();
			group.finish().unwrap();
		}
		assert!(reading.read_frame().now_or_never().is_none(), "not stale yet");

		let mut group = b.create_group(group::Info { sequence: 3 }).unwrap();
		group.write_frame(ts(3000), b"b".as_ref()).unwrap();
		assert!(matches!(reading.read_frame().now_or_never(), Some(Err(Error::Old))));
	}

	/// An untimed track has no media time to convict a group no route continues, so only
	/// the wall clock gives it up. Nothing is written after the replacement's successor
	/// arrives, so only the deadline wake can release the reader: against a cold route, and
	/// against one whose copy already caches groups past the held one, so a cursor that
	/// skips ahead cannot hide the stall.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn an_untimed_group_no_route_continues_is_given_up_by_wall_clock() {
		let untimed = || {
			track::Producer::new(
				Arc::new(broadcast::Info::default()),
				"video",
				crate::track::Info::default(),
			)
		};
		for warm in [false, true] {
			let routes = Producer::new();
			let logical = track::Request::new(Arc::new(broadcast::Info::default()), "video")
				.routes(routes.consume())
				.accept(crate::track::Info::default());
			let a = untimed();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, Duration::from_secs(1));
			let mut open = a.create_group(group::Info { sequence: 0 }).unwrap();
			open.write_frame(None, b"a".as_ref()).unwrap();
			let mut reading = recv(&mut sub);
			assert_eq!(read(&mut reading), Some(b"a".to_vec()));

			let b = untimed();
			let pool = b.broadcast().pool.clone();
			// A driver collects the replacement's pool, so its wall deadlines fire.
			pool.step(Duration::ZERO);
			let write = |sequence| {
				let mut group = b.create_group(group::Info { sequence }).unwrap();
				group.write_frame(None, b"b".as_ref()).unwrap();
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

	#[test]
	#[cfg(feature = "async")]
	fn datagrams_a_new_copy_replays_are_handed_out_once() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let mut a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(10));
		let mut b = copy();
		for track in [&mut a, &mut b] {
			for sequence in [1, 2] {
				track.insert_datagram(sequence, ts(sequence), b"d".as_ref()).unwrap();
			}
		}
		b.insert_datagram(3, ts(3), b"d".as_ref()).unwrap();

		let recv =
			|sub: &mut track::Subscriber| sub.recv_datagram().now_or_never().map(|d| d.unwrap().unwrap().sequence);
		assert_eq!(recv(&mut sub), Some(1));
		assert_eq!(recv(&mut sub), Some(2));
		routes.serve(b.consume());
		assert_eq!(recv(&mut sub), Some(3));
		assert_eq!(recv(&mut sub), None);
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_new_copy_hands_out_later_objects_of_a_seen_group() {
		let object = |id: u64| super::super::datagram::Stored {
			datagram: super::super::datagram::Datagram {
				sequence: 1,
				timestamp: Some(ts(id)),
				payload: bytes::Bytes::from_static(b"d"),
			},
			wire: Some(Arc::new(super::super::datagram::Wire {
				object_id: Some(id),
				priority: None,
				end_of_group: false,
				status: None,
				properties: Default::default(),
			})),
		};
		let routes = Producer::new();
		let front = logical(&routes);
		let mut a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&front, Duration::from_secs(10));
		let mut b = copy();
		a.insert_wire_datagram(object(6)).unwrap();
		b.insert_wire_datagram(object(6)).unwrap();
		b.insert_wire_datagram(object(8)).unwrap();

		let mut recv = || {
			sub.poll_recv_wire_datagram(&kio::Waiter::noop()).map(|datagram| {
				let datagram = datagram.unwrap().unwrap();
				datagram.wire.as_ref().unwrap().object_id.unwrap()
			})
		};
		assert_eq!(recv(), Poll::Ready(6));
		routes.serve(b.consume());
		assert_eq!(recv(), Poll::Ready(8));
		assert!(recv().is_pending());
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
		let _open = a.create_group(group::Info { sequence: 0 }).unwrap();
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
		/// The replacement's groups, by sequence.
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
		/// `groups` are the replacement's `(sequence, first timestamp)`, written before the
		/// switch is noticed, so its subscription has passed the held group.
		#[cfg(feature = "async")]
		fn new(groups: &[(u64, Option<u64>)]) -> Self {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, Duration::from_millis(100));
			for (sequence, ms) in [(0, 0), (1, 1000)] {
				let mut group = a.create_group(group::Info { sequence }).unwrap();
				group.write_frame(ts(ms), b"x".as_ref()).unwrap();
				group.finish().unwrap();
			}
			assert_eq!(recv(&mut sub).sequence, 0);
			assert_eq!(recv(&mut sub).sequence, 1);
			let mut open = a.create_group(group::Info { sequence: 2 }).unwrap();
			open.write_frame(ts(2000), b"held".as_ref()).unwrap();
			let mut group = recv(&mut sub);
			assert_eq!(read(&mut group), Some(b"held".to_vec()));

			let b = copy();
			routes.serve(b.consume());
			let groups = groups
				.iter()
				.map(|&(sequence, ms)| {
					let mut group = b.create_group(group::Info { sequence }).unwrap();
					if let Some(ms) = ms {
						group.write_frame(ts(ms), b"b".as_ref()).unwrap();
					}
					(sequence, group)
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

		fn group(&self, sequence: u64) -> group::Producer {
			self.copy.create_group(group::Info { sequence }).unwrap()
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
		let mut held = Held::new(&[(3, None), (4, Some(4000)), (5, Some(5000))]);
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
			let mut open = a.create_group(group::Info { sequence: 0 }).unwrap();
			let mut frame = open
				.create_frame(crate::frame::Info {
					size: 4,
					timestamp: Some(ts(0)),
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
			let mut replacement = b.create_group(group::Info { sequence: 0 }).unwrap();
			routes.serve(b.consume());
			assert!(chunks.read_chunk().now_or_never().is_none());
			if !arrives {
				for sequence in 1..=4 {
					let mut group = b.create_group(group::Info { sequence }).unwrap();
					group.write_frame(ts(sequence * 10_000), b"new".as_ref()).unwrap();
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
			let mut open = a.create_group(group::Info { sequence: 0 }).unwrap();
			let mut frame = open
				.create_frame(crate::frame::Info {
					size: 4,
					timestamp: Some(ts(0)),
				})
				.unwrap();
			frame.write(b"a".as_ref()).unwrap();
			let mut reading = recv(&mut sub);
			let mut chunks = reading.next_frame().now_or_never().unwrap().unwrap().unwrap();
			assert!(chunks.read_chunk().now_or_never().unwrap().is_ok());

			let b = copy();
			let mut replacement = b.create_group(group::Info { sequence: 0 }).unwrap();
			let mut replacement_frame = replacement
				.create_frame(crate::frame::Info {
					size: 4,
					timestamp: Some(ts(0)),
				})
				.unwrap();
			replacement_frame.write(b"ab".as_ref()).unwrap();
			routes.serve(b.consume());
			assert_eq!(&chunks.read_chunk().now_or_never().unwrap().unwrap().unwrap()[..], b"b");
			// Transfer the group cursor too, while its next frame is still pending.
			assert!(reading.read_frame().now_or_never().is_none());
			for sequence in 1..=4 {
				let mut next = b.create_group(group::Info { sequence }).unwrap();
				next.write_frame(ts(sequence * 1000), b"x".as_ref()).unwrap();
				next.finish().unwrap();
			}
			if partial {
				assert!(matches!(chunks.read_chunk().now_or_never(), Some(Err(Error::Old))));
			} else {
				assert!(matches!(reading.read_frame().now_or_never(), Some(Err(Error::Old))));
			}
		}
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_pending_fetch_wakes_and_moves_to_the_new_route() {
		let routes = Producer::new();
		let a = copy();
		let dynamic = a.dynamic();
		routes.serve(a.consume());
		let logical = logical(&routes);
		let mut fetching = logical.consume().fetch_group(0, None);
		let wake = Arc::new(Wake::default());
		let waiter = kio::Waiter::new(wake.clone().into());
		assert!(kio::Task::poll(&mut *fetching, &waiter).is_pending());
		let _request = dynamic.requested_group().now_or_never().unwrap().unwrap();
		wake.0.store(false, std::sync::atomic::Ordering::Relaxed);
		let b = copy();
		let mut group = b.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(ts(0), b"b".as_ref()).unwrap();
		group.finish().unwrap();
		routes.serve(b.consume());
		assert!(wake.0.load(std::sync::atomic::Ordering::Relaxed));
		assert!(matches!(kio::Task::poll(&mut *fetching, &waiter), Poll::Ready(Ok(_))));
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_pending_recovery_fetch_does_not_disable_expiry() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(2));
		let mut open = a.create_group(group::Info { sequence: 0 }).unwrap();
		open.write_frame(ts(0), b"a".as_ref()).unwrap();
		let mut reading = recv(&mut sub);
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));
		let b = copy();
		let _dynamic = b.dynamic();
		routes.serve(b.consume());
		assert!(reading.read_frame().now_or_never().is_none());
		for sequence in 1..=4 {
			let mut group = b.create_group(group::Info { sequence }).unwrap();
			group.write_frame(ts(sequence * 1000), b"b".as_ref()).unwrap();
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
		let group = a.create_group(group::Info { sequence: 0 }).unwrap();
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
			.create_frame_owned(
				crate::frame::Header {
					info: crate::frame::Info {
						size: 1,
						timestamp: Some(ts(0)),
					},
					fields: Some(Arc::new(super::super::object::Fields {
						id: 6,
						properties: Default::default(),
					})),
				},
				&crate::frame::Budget::default(),
			)
			.unwrap();
		frame.write(bytes::Bytes::from_static(b"a")).unwrap();
		let group = frame.finish().unwrap();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::MAX);
		let mut reading = recv(&mut sub);
		assert_eq!(read(&mut reading), Some(b"a".to_vec()));
		assert_eq!(a.consume().resume_position(), Some(Position::group(1)));
		let mut partial = group
			.create_frame_owned(
				crate::frame::Header {
					info: crate::frame::Info {
						size: 2,
						timestamp: Some(ts(1)),
					},
					fields: Some(Arc::new(super::super::object::Fields {
						id: 8,
						properties: Default::default(),
					})),
				},
				&crate::frame::Budget::default(),
			)
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

	/// A replacement copy that joined past a gap, holding the group from Object 6.
	#[cfg(feature = "async")]
	fn joined_past_a_gap(objects: &[(u64, &'static [u8])]) -> track::Producer {
		let b = copy();
		let mut group = b.create_group(group::Info { sequence: 0 }).unwrap();
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
				.create_frame_owned(
					crate::frame::Header {
						info: crate::frame::Info {
							size: payload.len() as u64,
							timestamp: Some(ts(id)),
						},
						fields: Some(Arc::new(super::super::object::Fields {
							id,
							properties: Default::default(),
						})),
					},
					&crate::frame::Budget::default(),
				)
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
		let mut open = a.create_group(group::Info { sequence: 0 }).unwrap();
		let mut frame = open
			.create_frame(crate::frame::Info {
				size: 4,
				timestamp: Some(ts(0)),
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
		let mut open = a.create_group(group::Info { sequence: 0 }).unwrap();
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
			let mut open = a.create_group(group::Info { sequence: 0 }).unwrap();
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
		let _open = a.create_group(group::Info { sequence: 0 }).unwrap();
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
		let dynamic = a.dynamic();
		routes.serve(a.consume());

		let mut arriving = a.create_group(group::Info { sequence: 0 }).unwrap();
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

		let request = dynamic.requested_group().now_or_never().unwrap().unwrap();
		assert_eq!((request.sequence(), request.frame_start()), (0, 1));
		let mut rest = request.accept(None).unwrap();
		rest.write_frame(ts(1), b"b".as_ref()).unwrap();
		rest.finish().unwrap();
		assert_eq!(read(&mut fetched), Some(b"b".to_vec()));
		assert_eq!(read(&mut fetched), None);
	}

	/// A fetched frame read to its end no longer holds the logical track, so a caller
	/// keeping the handle does not stop the track from parking.
	#[test]
	#[cfg(feature = "async")]
	fn a_read_frame_releases_the_track() {
		let routes = Producer::new();
		let a = copy();
		routes.serve(a.consume());
		let mut open = a.create_group(group::Info { sequence: 0 }).unwrap();
		open.write_frame(ts(0), b"a".as_ref()).unwrap();

		let logical = logical(&routes);
		let demand = logical.demand();
		let mut fetched = logical.consume().fetch_group(0, None).now_or_never().unwrap().unwrap();
		assert!(demand.is_used(), "an open fetched group holds the track");
		let mut frame = fetched.next_frame().now_or_never().unwrap().unwrap().unwrap();
		drop(fetched);
		assert!(demand.is_used(), "an unread frame holds the track");
		assert_eq!(frame.read_all().now_or_never().unwrap().unwrap(), b"a".as_ref());
		assert!(!demand.is_used(), "a read frame does not");
	}

	/// A rerouted fetch keeps its own age budget: the new route is asked with it, and a
	/// read waiting on that route gives up once the group falls past it there.
	#[test]
	#[cfg(feature = "async")]
	fn a_rerouted_fetch_keeps_its_budget() {
		let routes = Producer::new();
		let a = copy();
		routes.serve(a.consume());
		let mut open = a.create_group(group::Info { sequence: 0 }).unwrap();
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
		let dynamic = b.dynamic();
		routes.serve(b.consume());
		assert!(fetched.read_frame().now_or_never().is_none());
		let request = dynamic.requested_group().now_or_never().unwrap().unwrap();
		assert_eq!((request.frame_start(), request.max_delay()), (1, Some(budget)));

		// The new route runs a full second past the group's successor while its fetch is unanswered.
		for (sequence, ms) in [(1, 1000), (2, 2000)] {
			let mut group = b.create_group(group::Info { sequence }).unwrap();
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
		let _open = a.create_group(group::Info { sequence: 0 }).unwrap();
		let mut reading = recv(&mut sub);
		assert!(reading.read_frame().now_or_never().is_none());
		routes.end(Err(Error::Unroutable));
		assert!(matches!(
			reading.read_frame().now_or_never(),
			Some(Err(Error::Unroutable))
		));
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_datagram_route_failure_waits_for_replacement() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(10));
		a.abort(Error::Dropped).unwrap();
		assert!(sub.recv_datagram().now_or_never().is_none());
		let mut b = copy();
		routes.serve(b.consume());
		b.insert_datagram(1, ts(0), b"b".as_ref()).unwrap();
		assert_eq!(
			sub.recv_datagram().now_or_never().unwrap().unwrap().unwrap().sequence,
			1
		);
	}

	/// Datagrams and groups are independent channels: reading datagrams first must not
	/// release a replaced copy whose buffered groups the reader has yet to read.
	#[test]
	#[cfg(feature = "async")]
	fn datagram_reads_keep_unread_groups_on_a_replaced_copy() {
		for datagrams_first in [false, true] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, Duration::from_secs(10));
			assert!(sub.recv_datagram().now_or_never().is_none());
			let mut group = a.create_group(group::Info { sequence: 0 }).unwrap();
			group.write_frame(ts(0), b"a0".as_ref()).unwrap();
			let b = copy();
			routes.serve(b.consume());
			if datagrams_first {
				assert!(sub.recv_datagram().now_or_never().is_none());
			}
			let mut next = recv(&mut sub);
			assert_eq!(next.sequence, 0, "datagrams_first={datagrams_first}");
			assert_eq!(read(&mut next), Some(b"a0".to_vec()));
		}
	}

	#[test]
	#[cfg(feature = "async")]
	fn datagram_retention_preserves_ordered_backlog() {
		for ordered_first in [false, true] {
			let routes = Producer::new();
			let logical = logical(&routes);
			let a = copy();
			routes.serve(a.consume());
			let mut sub = subscribe(&logical, Duration::from_secs(10));
			assert!(sub.recv_datagram().now_or_never().is_none());
			let _zero = ordered_first.then(|| a.create_group(group::Info { sequence: 0 }).unwrap());
			if ordered_first {
				assert_eq!(
					kio::wait(|waiter| sub.poll_next_group(waiter))
						.now_or_never()
						.unwrap()
						.unwrap()
						.unwrap()
						.sequence,
					0
				);
			}
			let _two = a.create_group(group::Info { sequence: 2 }).unwrap();
			let _one = a.create_group(group::Info { sequence: 1 }).unwrap();
			let b = copy();
			routes.serve(b.consume());
			assert!(sub.recv_datagram().now_or_never().is_none());
			for sequence in [1, 2] {
				assert_eq!(
					kio::wait(|waiter| sub.poll_next_group(waiter))
						.now_or_never()
						.unwrap()
						.unwrap()
						.unwrap()
						.sequence,
					sequence
				);
			}
		}
	}

	#[test]
	#[cfg(feature = "async")]
	fn datagram_retention_obeys_changed_group_bounds() {
		for ordered in [false, true] {
			for raise_floor in [false, true] {
				let routes = Producer::new();
				let logical = logical(&routes);
				let a = copy();
				routes.serve(a.consume());
				let mut sub = subscribe(&logical, Duration::from_secs(10));
				assert!(sub.recv_datagram().now_or_never().is_none());
				let _zero = a.create_group(group::Info { sequence: 0 }).unwrap();
				let b = copy();
				routes.serve(b.consume());
				assert!(sub.recv_datagram().now_or_never().is_none());
				if raise_floor {
					sub.set_groups(1..);
				} else {
					sub.set_groups(..0);
				}
				assert!(
					if ordered {
						sub.poll_next_group(&kio::Waiter::noop())
					} else {
						sub.poll_recv_group(&kio::Waiter::noop())
					}
					.is_pending()
				);
				if !raise_floor {
					sub.set_groups(..);
					let next = if ordered {
						sub.poll_next_group(&kio::Waiter::noop())
					} else {
						sub.poll_recv_group(&kio::Waiter::noop())
					};
					assert_eq!(
						match next {
							Poll::Ready(Ok(Some(group))) => group.sequence,
							_ => panic!("a group"),
						},
						0
					);
				}
			}
		}
	}

	/// A relative start, as the first copy resolved it, stays once a route switch lets that
	/// copy go.
	#[test]
	#[cfg(feature = "async")]
	fn the_floor_outlives_the_first_copy() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		write(&a, 4);
		write(&a, 5);
		let subscription = Subscription::default()
			.with_max_delay(Duration::from_secs(10))
			.with_ranges([track::Range {
				start: Anchor::Relative(1),
				end: None,
			}]);
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
		for sequence in 1..=5 {
			write(&b, sequence);
		}
		assert!(!drain(&mut sub, false).is_empty());
		{
			let readers = routes.readers.lock().unwrap();
			let reader = readers[0].upgrade().unwrap();
			let reader = reader.lock().unwrap();
			assert_eq!(reader.copies.len(), 1, "the first copy was let go");
			assert!(reader.copies[0].floor.is_some(), "the serving copy has a failover cap");
		}

		let subscription = Subscription::default()
			.with_max_delay(Duration::from_secs(10))
			.with_ranges([
				track::Range::groups(0..1),
				track::Range {
					start: Anchor::Relative(1),
					end: None,
				},
			]);
		sub.update(subscription).unwrap();
		drain(&mut sub, false);
		assert_eq!(
			sub.next_in_range(1),
			Some(4),
			"the relative range resolved on the serving copy"
		);
		assert_eq!(sub.floor(), Some(0));
	}

	/// A resumed reader names the next group its ranges cover, so a reader waiting on each
	/// group in sequence walks over the gap between two ranges instead of waiting for it.
	#[test]
	#[cfg(feature = "async")]
	fn a_resumed_reader_names_the_next_group_in_range() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let subscription = Subscription::default()
			.with_max_delay(Duration::from_secs(10))
			.with_ranges([track::Range::groups(0..1), track::Range::groups(10..11)]);
		let mut sub = logical
			.consume()
			.subscribe(subscription)
			.now_or_never()
			.unwrap()
			.unwrap();
		for sequence in [0, 5, 10] {
			write(&a, sequence);
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
	#[cfg(feature = "async")]
	fn datagram_readers_release_replaced_subscriptions() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(10));
		let b = copy();
		routes.serve(b.consume());
		assert!(sub.recv_datagram().now_or_never().is_none());
		let readers = routes.readers.lock().unwrap();
		let reader = readers[0].upgrade().unwrap();
		assert_eq!(reader.lock().unwrap().copies.len(), 1);
	}

	/// A reader that read no groups bounds a replaced copy at what it has, so the copy stops
	/// taking new groups: no new copy's group handed out will ever bound it.
	#[test]
	#[cfg(feature = "async")]
	fn datagram_readers_bound_replaced_copies() {
		let routes = Producer::new();
		let logical = logical(&routes);
		let a = copy();
		routes.serve(a.consume());
		let mut sub = subscribe(&logical, Duration::from_secs(10));
		write(&a, 0);
		let b = copy();
		routes.serve(b.consume());
		assert!(sub.recv_datagram().now_or_never().is_none());
		write(&a, 1);
		{
			let readers = routes.readers.lock().unwrap();
			let reader = readers[0].upgrade().unwrap();
			let reader = reader.lock().unwrap();
			assert_eq!(reader.copies.len(), 2, "its buffered group may still be read");
			assert_eq!(reader.copies[0].until, Some(Some(0)));
		}
		// A group read later still gets what it had, and nothing it took after.
		assert_eq!(drain(&mut sub, false), [0]);
	}

	#[test]
	fn ending_the_front_releases_a_pending_fetch() {
		let routes = Producer::new();
		let a = copy();
		let _dynamic = a.dynamic();
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
		let _open = a.create_group(group::Info { sequence: 0 }).unwrap();
		let mut reading = recv(&mut sub);
		let b = copy();
		let mut replacement = b.create_group(group::Info { sequence: 0 }).unwrap();
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
			Frame(crate::frame::Consumer),
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
					let mut writers = [&a, &b].map(|source| source.create_group(group::Info { sequence: 0 }).unwrap());
					let _frames: Vec<_> = writers
						.iter_mut()
						.map(|group| {
							group.write_frame(ts(0), b"first".as_ref()).unwrap();
							chunked.then(|| {
								let mut frame = group
									.create_frame(crate::frame::Info {
										size: 4,
										timestamp: Some(ts(1)),
									})
									.unwrap();
								frame.write(b"ab".as_ref()).unwrap();
								frame
							})
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
						Read::Frame(frame)
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
					wake.0.store(false, std::sync::atomic::Ordering::Relaxed);
					sub.control()
						.update(
							Subscription::default()
								.with_priority(7)
								.with_max_delay(Duration::from_secs(if widen { 10 } else { 2 })),
						)
						.unwrap();
					assert!(
						wake.0.load(std::sync::atomic::Ordering::Relaxed),
						"recovered={recovered}, widen={widen}, chunked={chunked}: preference update did not wake the held read"
					);
					for sequence in 1..=4 {
						let mut next = source.create_group(group::Info { sequence }).unwrap();
						next.write_frame(ts(sequence * 1000), b"next".as_ref()).unwrap();
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
									.with_max_delay(Duration::from_secs(10)),
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
				.map(|copy| Some(copy.create_group(group::Info { sequence: 0 }).unwrap()))
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
								.with_max_delay(Duration::from_secs(2)),
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
			assert_eq!(demand.max_delay, Duration::from_secs(2), "{events:?}");
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
}
