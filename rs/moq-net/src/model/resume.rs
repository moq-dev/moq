//! Serve a front's logical track straight from its routes' copies.
//!
//! A front serves each track through one route at a time and switches when that route
//! dies, withdraws, or is beaten. A path names one broadcast whoever serves it, so every
//! route's copy of a track holds the same groups and frames. A reader of the logical
//! track therefore reads the serving route's copy directly, through its own
//! [`Subscriber`]: nothing is copied, and with one route it is a passthrough.
//!
//! When the front switches, each reader subscribes to the new copy from the newest group
//! it handed out, and keeps the replaced copy only for what it already handed out: the
//! replaced subscription is bounded after that group, and dropped once no group read from
//! it is left. A group carries on across the switch through its [`Recover`]: once its copy
//! fails with its route, or stalls while a newer route serves, it continues from the
//! serving route's copy of the same group at the same frame (and byte), found in that
//! copy's cache or fetched from it.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::task::{Poll, ready};

use crate::{Datagram, Error, Result, StreamError, group, stats, track};

use super::subscription::{Cap, Position, Subscription, max_some};

/// How many delivered group sequences a reader remembers, so a group two routes both
/// deliver is handed out once.
const MAX_DELIVERED: usize = 1024;

/// What the front says about a logical track: the copy to read now, and how it ended.
#[derive(Default)]
struct Route {
	/// Bumped whenever `copy` changes, so a reader can tell a replacement apart.
	generation: u64,
	/// The serving route's copy; `None` while nobody reads the track, or between routes.
	copy: Option<track::Consumer>,
	/// How the track ended, once the front decided.
	end: Option<Result<()>>,
}

/// Every reader of a logical track, so a new copy subscribes them all as it is served:
/// one that is not reading right now still gets what that copy delivers.
type Readers = Arc<Mutex<Vec<Weak<Mutex<Reader>>>>>;

/// The front's side of a logical track: which copy serves it, and how it ends.
///
/// Dropping it concludes the track: readers follow the last copy to its end, since no
/// front is left to replace it.
pub(crate) struct Producer {
	state: kio::Producer<Route>,
	readers: Readers,
}

impl Producer {
	pub(crate) fn new() -> Self {
		Self {
			state: kio::Producer::default(),
			readers: Readers::default(),
		}
	}

	/// Serve the track from `copy` from now on, subscribing every reader to it.
	pub(crate) fn serve(&self, copy: track::Consumer) {
		if let Ok(mut route) = self.state.write() {
			route.generation += 1;
			route.copy = Some(copy);
		}
		let readers = {
			let mut readers = self.readers.lock().expect("readers poisoned");
			readers.retain(|reader| reader.strong_count() > 0);
			readers.clone()
		};
		for reader in readers.iter().filter_map(Weak::upgrade) {
			reader.lock().expect("reader poisoned").sync(&kio::Waiter::noop());
		}
	}

	/// Let go of the serving copy: nobody reads the track, so its route goes idle.
	pub(crate) fn park(&self) {
		if let Ok(mut route) = self.state.write()
			&& route.copy.is_some()
		{
			route.generation += 1;
			route.copy = None;
		}
	}

	/// The track ended: cleanly with `Ok`, or failed for good.
	pub(crate) fn end(&self, result: Result<()>) {
		if let Ok(mut route) = self.state.write()
			&& route.end.is_none()
		{
			route.end = Some(result);
		}
	}

	pub(crate) fn consume(&self) -> Consumer {
		Consumer {
			state: self.state.consume(),
			readers: self.readers.clone(),
		}
	}
}

/// A reader's handle on a logical track's [`Producer`].
#[derive(Clone)]
pub(crate) struct Consumer {
	state: kio::Consumer<Route>,
	readers: Readers,
}

impl Consumer {
	/// The serving route's copy, if any.
	pub(crate) fn serving(&self) -> Option<track::Consumer> {
		self.state.read().copy.clone()
	}

	/// Subscribe to the logical track with the reader's own preferences.
	pub(crate) fn subscribe(&self, subscription: kio::Producer<Subscription>) -> Subscriber {
		let mirrored = subscription.read().clone();
		let reader = Arc::new_cyclic(|this| {
			Mutex::new(Reader {
				this: this.clone(),
				route: self.clone(),
				subscription: subscription.clone(),
				mirrored,
				generation: None,
				copies: Vec::new(),
				delivered: BTreeSet::new(),
				newest: None,
				ordered: None,
				datagram: None,
				groups: (0, Cap::from(..)),
			})
		});
		// Subscribed to the serving copy at once, so groups it has now are the reader's
		// even if the route changes before the first read, and to each copy after.
		{
			let mut readers = self.readers.lock().expect("readers poisoned");
			// Sweep the readers that left once the list fills, so it tracks the live count.
			if readers.len() == readers.capacity() {
				readers.retain(|reader| reader.strong_count() > 0);
				let live = readers.len();
				readers.reserve(live.max(1));
			}
			readers.push(Arc::downgrade(&reader));
		}
		reader.lock().expect("reader poisoned").sync(&kio::Waiter::noop());
		Subscriber { reader, subscription }
	}

	/// Fetch one group from whichever route serves the track.
	pub(crate) fn fetch_group(&self, sequence: u64, options: group::Fetch) -> Fetching {
		Fetching {
			route: self.clone(),
			sequence,
			options,
			pending: None,
		}
	}

	/// Poll for the route state to move past `generation`, or the track to end.
	fn poll_changed(&self, generation: u64, waiter: &kio::Waiter) -> Poll<()> {
		match self.state.poll(waiter, |route| {
			match route.generation != generation || route.end.is_some() {
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
		match (&route.end, route.is_closed()) {
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
	/// Replaced by a newer route: nothing past this group, the newest it had then, is
	/// taken from it.
	until: Option<Option<u64>>,
	/// The group channel ran out.
	done: Option<Result<()>>,
	/// The datagram channel ran out.
	datagrams_done: Option<Result<()>>,
	/// The newest datagram handed out before this copy was subscribed: a copy that
	/// buffers datagrams replays them, and those were already handed out.
	datagrams_seen: Option<u64>,
	/// Held by every group handed out from this copy, so a replaced copy stays
	/// subscribed while one is still read.
	lease: Arc<()>,
	/// A group taken from a replaced copy as it was let go, for the next group read.
	held: Option<group::Consumer>,
}

enum Sub {
	Pending(kio::Pending<track::Subscribing>),
	Ready(track::Subscriber),
}

impl Copy {
	/// What this copy is asked for: the reader's own preferences, from no earlier than
	/// where it was asked to start, and through no later than where it was replaced.
	fn wire(&self, reader: &Subscription) -> Subscription {
		let mut wire = Subscription {
			start: max_some(reader.start, self.floor),
			..reader.clone()
		};
		if let Some(until) = self.until {
			let end = until.and_then(Position::after_group).unwrap_or_default();
			wire.end = Some(wire.end.map_or(end, |requested| requested.min(end)));
		}
		wire
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
			sub.raise_start_to(groups.0);
			sub.end_at(groups.1);
			self.sub = Sub::Ready(sub);
		}
		let Sub::Ready(sub) = &mut self.sub else { unreachable!() };
		Poll::Ready(Ok(sub))
	}

	/// Whether this copy is still needed: it serves, or a group read from it is still out
	/// or held.
	fn needed(&self, serving: bool) -> bool {
		serving || (self.done.is_none() && (Arc::strong_count(&self.lease) > 1 || self.held.is_some()))
	}
}

/// One reader's subscription to a logical track; see the module docs.
pub(crate) struct Subscriber {
	/// Shared with the groups it hands out, so one stuck on a dead route can follow the
	/// reader onto the next route without the reader polling for groups.
	reader: Arc<Mutex<Reader>>,
	subscription: kio::Producer<Subscription>,
}

impl Subscriber {
	fn reader(&self) -> MutexGuard<'_, Reader> {
		self.reader.lock().expect("reader poisoned")
	}

	/// The reader's preferences channel, shared with its [`track::Control`].
	pub(crate) fn subscription(&self) -> &kio::Producer<Subscription> {
		&self.subscription
	}

	/// Poll for the next group: in arrival order, or in sequence order for `ordered`.
	pub(crate) fn poll_group(&mut self, ordered: bool, waiter: &kio::Waiter) -> Poll<Result<Option<group::Consumer>>> {
		self.reader().poll_group(ordered, waiter)
	}

	/// Poll for the next datagram in arrival order.
	pub(crate) fn poll_recv_datagram(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Datagram>>> {
		self.reader().poll_recv_datagram(waiter)
	}

	/// Poll for the track's declared final sequence.
	pub(crate) fn poll_finished(&mut self, waiter: &kio::Waiter) -> Poll<Result<u64>> {
		self.reader().poll_finished(waiter)
	}

	/// Poll for where the serving copy's live feed starts; see
	/// [`track::Subscriber::poll_start`].
	pub(crate) fn poll_start(&mut self, waiter: &kio::Waiter) -> Poll<Option<u64>> {
		let mut reader = self.reader();
		let sub = ready!(reader.poll_serving(waiter));
		match sub {
			Some(sub) => sub.poll_start(waiter),
			None => Poll::Ready(None),
		}
	}

	/// Raise the local read floor; see [`track::Subscriber::set_groups`].
	pub(crate) fn raise_start_to(&mut self, start: u64) {
		self.reader().raise_start_to(start)
	}

	/// Cap local reads at `end`; see [`track::Subscriber::set_groups`].
	pub(crate) fn end_at(&mut self, end: Cap) {
		self.reader().end_at(end)
	}

	/// The newest sequence any copy has.
	pub(crate) fn latest(&self) -> Option<u64> {
		self.reader().latest()
	}

	/// The groups the copies' drift budget skipped since the last call, counted against
	/// the logical track: the copies themselves are untagged.
	pub(crate) fn take_stale(&mut self) -> stats::Content {
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
	pub(crate) fn poll_live(&mut self, waiter: &kio::Waiter) -> Poll<Option<Position>> {
		let mut reader = self.reader();
		let sub = ready!(reader.poll_serving(waiter));
		match sub {
			Some(sub) => sub.poll_live(waiter),
			None => Poll::Ready(None),
		}
	}
}

/// A [`Subscriber`]'s state.
struct Reader {
	this: Weak<Mutex<Reader>>,
	route: Consumer,
	/// The reader's own preferences, mirrored into every copy's subscription.
	subscription: kio::Producer<Subscription>,
	mirrored: Subscription,
	/// The route generation last read.
	generation: Option<u64>,
	/// Oldest first: the last one is the serving route's, when it is subscribed.
	copies: Vec<Copy>,
	/// The groups handed out, so a group two copies deliver is handed out once.
	delivered: BTreeSet<u64>,
	/// The newest group handed out: where a new copy is asked to start.
	newest: Option<u64>,
	/// Reading in sequence order: the last group handed out.
	ordered: Option<u64>,
	/// The newest datagram handed out.
	datagram: Option<u64>,
	/// The local group filter; see [`track::Subscriber::set_groups`].
	groups: (u64, Cap),
}

impl Reader {
	/// Follow the route: subscribe to a new copy, bound the ones it replaced, and mirror
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
			self.mirrored = sub;
			for copy in &mut self.copies {
				copy.update(&self.mirrored);
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
		// The replaced copies finish the groups they had when replaced, and nothing newer.
		for copy in &mut self.copies {
			if copy.until.is_none() {
				copy.until = Some(self.newest.max(copy.track.latest()));
				copy.update(&self.mirrored);
			}
		}
		let floor = self.newest.map(Position::group);
		// The wire request carries the floor from the first message: some sessions read
		// the demand only once.
		let wire = Subscription {
			start: max_some(self.mirrored.start, floor),
			..self.mirrored.clone()
		};
		self.copies.push(Copy {
			generation,
			sub: Sub::Pending(track.subscribe(wire)),
			track,
			floor,
			until: None,
			done: None,
			datagrams_done: None,
			datagrams_seen: self.datagram,
			lease: Arc::new(()),
			held: None,
		});
	}

	/// Drop the replaced copies nothing is read from any more.
	fn let_go(&mut self) {
		let serving = self.generation;
		for index in 0..self.copies.len() {
			let copy = &self.copies[index];
			if copy.done.is_none() && !copy.needed(Some(copy.generation) == serving) {
				self.hold(index);
			}
		}
		self.copies.retain(|copy| copy.needed(Some(copy.generation) == serving));
	}

	/// Take a group a replaced copy still has for the reader before letting it go: the
	/// datagram channel lets go too, and must not drop what the group channel has yet to read.
	fn hold(&mut self, index: usize) {
		let groups = self.groups;
		let waiter = kio::Waiter::noop();
		loop {
			let Poll::Ready(Ok(sub)) = self.copies[index].poll_ready(groups, &waiter) else {
				return;
			};
			let Poll::Ready(Ok(Some(group))) = sub.poll_recv_group(&waiter) else {
				return;
			};
			if self.deliverable(&self.copies[index], group.sequence) {
				self.copies[index].held = Some(group);
				return;
			}
		}
	}

	fn deliverable(&self, copy: &Copy, sequence: u64) -> bool {
		if copy
			.until
			.is_some_and(|until| until.is_none_or(|until| sequence > until))
		{
			return false;
		}
		match self.ordered {
			Some(last) => sequence > last,
			None => !self.delivered.contains(&sequence),
		}
	}

	fn hand_out(&mut self, index: usize, group: group::Consumer, ordered: bool) -> group::Consumer {
		let sequence = group.sequence;
		if ordered {
			self.ordered = Some(sequence);
		}
		self.delivered.insert(sequence);
		if self.delivered.len() > MAX_DELIVERED {
			self.delivered.pop_first();
		}
		self.newest = self.newest.max(Some(sequence));
		let copy = &self.copies[index];
		group.with_recover(Recover {
			sequence,
			reader: self.this.clone(),
			route: self.route.clone(),
			generation: copy.generation,
			copy: copy.track.clone(),
			lease: Some(copy.lease.clone()),
			fetch: None,
		})
	}

	fn poll_group(&mut self, ordered: bool, waiter: &kio::Waiter) -> Poll<Result<Option<group::Consumer>>> {
		loop {
			if let Some(res) = self.poll_group_once(ordered, waiter) {
				return res;
			}
		}
	}

	/// One pass over the copies; `None` when the route moved meanwhile and it is worth
	/// another.
	fn poll_group_once(
		&mut self,
		ordered: bool,
		waiter: &kio::Waiter,
	) -> Option<Poll<Result<Option<group::Consumer>>>> {
		self.sync(waiter);
		let groups = self.groups;
		// The serving copy first: it is the one that keeps delivering.
		for index in (0..self.copies.len()).rev() {
			let copy = &mut self.copies[index];
			if copy.done.is_some() {
				continue;
			}
			if let Some(group) = copy.held.take()
				&& self.deliverable(&self.copies[index], group.sequence)
			{
				let group = self.hand_out(index, group, ordered);
				return Some(Poll::Ready(Ok(Some(group))));
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
				let res = match ordered {
					true => sub.poll_next_group(waiter),
					false => sub.poll_recv_group(waiter),
				};
				match res {
					Poll::Pending => break,
					Poll::Ready(Ok(Some(group))) => {
						let copy = &self.copies[index];
						if !self.deliverable(copy, group.sequence) {
							continue;
						}
						let group = self.hand_out(index, group, ordered);
						return Some(Poll::Ready(Ok(Some(group))));
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
		self.let_go();
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
		match self.route.end() {
			Some(Some(Err(err))) => return Some(Poll::Ready(Err(err))),
			// Ended cleanly: once the serving copy (if any) drained.
			Some(Some(Ok(()))) => match serving.map(&done) {
				None | Some(Some(_)) => return Some(Poll::Ready(Ok(None))),
				Some(None) => {}
			},
			// No front is left to replace the copy: the track ends as it does. A copy whose
			// session closed locally ends it cleanly.
			Some(None) => match serving.map(&done) {
				None => return Some(Poll::Ready(Err(Error::Dropped))),
				Some(Some(Ok(()) | Err(Error::Closed))) => return Some(Poll::Ready(Ok(None))),
				Some(Some(Err(err))) => return Some(Poll::Ready(Err(err))),
				Some(None) => {}
			},
			None => {}
		}
		match self.route.poll_changed(self.generation.unwrap_or_default(), waiter) {
			Poll::Ready(()) if self.route.end().is_none() => None,
			_ => Some(Poll::Pending),
		}
	}

	fn poll_recv_datagram(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Datagram>>> {
		loop {
			if let Some(res) = self.poll_datagram_once(waiter) {
				return res;
			}
		}
	}

	fn poll_datagram_once(&mut self, waiter: &kio::Waiter) -> Option<Poll<Result<Option<Datagram>>>> {
		self.sync(waiter);
		self.let_go();
		let groups = self.groups;
		for index in (0..self.copies.len()).rev() {
			loop {
				let copy = &mut self.copies[index];
				if copy.datagrams_done.is_some() || copy.until.is_some() {
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
				match sub.poll_recv_datagram(waiter) {
					Poll::Pending => break,
					Poll::Ready(Ok(Some(datagram))) => {
						if copy.datagrams_seen.is_some_and(|seen| datagram.sequence <= seen) {
							continue;
						}
						self.datagram = self.datagram.max(Some(datagram.sequence));
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

	fn poll_finished(&mut self, waiter: &kio::Waiter) -> Poll<Result<u64>> {
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
			return Poll::Ready(Err(Error::Dropped));
		}
		let _ = self.route.poll_changed(serving.unwrap_or_default(), waiter);
		Poll::Pending
	}

	/// The serving copy's subscription, once it answered; `None` once the track ended
	/// without one.
	fn poll_serving(&mut self, waiter: &kio::Waiter) -> Poll<Option<&mut track::Subscriber>> {
		self.sync(waiter);
		let groups = self.groups;
		let serving = self.generation;
		let concluded = self.route.end().is_some();
		let copy = self.copies.last_mut().filter(|copy| Some(copy.generation) == serving);
		match copy {
			Some(copy) => match copy.poll_ready(groups, waiter) {
				Poll::Ready(Ok(sub)) => Poll::Ready(Some(sub)),
				Poll::Ready(Err(_)) if concluded => Poll::Ready(None),
				_ => {
					let _ = self.route.poll_changed(serving.unwrap_or_default(), waiter);
					Poll::Pending
				}
			},
			None if concluded => Poll::Ready(None),
			None => {
				let _ = self.route.poll_changed(serving.unwrap_or_default(), waiter);
				Poll::Pending
			}
		}
	}

	fn raise_start_to(&mut self, start: u64) {
		self.groups.0 = self.groups.0.max(start);
		for copy in &mut self.copies {
			if let Sub::Ready(sub) = &mut copy.sub {
				sub.raise_start_to(start);
			}
		}
	}

	fn end_at(&mut self, end: Cap) {
		self.groups.1 = end;
		for copy in &mut self.copies {
			if let Sub::Ready(sub) = &mut copy.sub {
				sub.end_at(end);
			}
		}
	}

	fn latest(&self) -> Option<u64> {
		self.copies.iter().filter_map(|copy| copy.track.latest()).max()
	}
}

/// Carries a group across route changes; see the module docs.
///
/// Held by a group handed out from a logical track. It is consulted only once the
/// group's copy fails or stalls, so a passthrough read never touches it.
pub(crate) struct Recover {
	sequence: u64,
	/// The reader it was handed out to, followed onto each new route.
	reader: Weak<Mutex<Reader>>,
	route: Consumer,
	/// The route generation of the copy being read, and that copy.
	generation: u64,
	copy: track::Consumer,
	/// Keeps a replaced copy subscribed while this group is read from it.
	lease: Option<Arc<()>>,
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
			route: self.route.clone(),
			generation: self.generation,
			copy: self.copy.clone(),
			lease: self.lease.clone(),
			// A clone reads in parallel, so it asks for itself.
			fetch: None,
		}
	}
}

impl Recover {
	/// Whether to look past the copy being read: it stalled while a newer route serves, or
	/// it failed without a verdict on the group. Only a route that serves can refuse a
	/// group on purpose (it is too old, evicted, or the application said so).
	pub(crate) fn wants(&self, err: Option<&Error>, waiter: &kio::Waiter) -> bool {
		let verdict = err.is_some_and(|err| {
			matches!(
				StreamError::from(err),
				StreamError::Old | StreamError::Evicted | StreamError::App(_)
			)
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
	pub(crate) fn poll(
		&mut self,
		index: u64,
		failed: Option<&Error>,
		waiter: &kio::Waiter,
	) -> Poll<Result<Replacement>> {
		// Subscribe the reader to a new route from its newest group, which is this one or
		// later, so that route's subscription delivers the rest of it.
		if let Some(reader) = self.reader.upgrade() {
			reader.lock().expect("reader poisoned").sync(waiter);
		}
		loop {
			let (generation, serving, end, closed) = {
				let route = self.route.state.read();
				(
					route.generation,
					route.copy.clone(),
					route.end.clone(),
					route.is_closed(),
				)
			};
			// Registered before looking, so a route change from here on wakes the reader.
			if self.route.poll_changed(generation, waiter).is_ready() && end.is_none() && !closed {
				continue;
			}
			if let Some(Err(err)) = &end {
				return Poll::Ready(Err(err.clone()));
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
			let options = group::Fetch::default().with_frame_start(index);
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

	/// The reader's budget for a late group; `None` for a fetch, which has no live edge
	/// to be late against.
	pub(crate) fn poll_budget(&self, waiter: &kio::Waiter) -> Option<std::time::Duration> {
		let reader = self.reader.upgrade()?;
		let mut reader = reader.lock().expect("reader poisoned");
		// A held group or frame may be the only thing being polled. Mirror and watch
		// preferences here too, before judging its budget against the live edge.
		reader.sync(waiter);
		Some(reader.mirrored.max_age)
	}

	/// Commit the replacement only once the caller has acquired its cursor or frame.
	pub(crate) fn adopt(&mut self, replacement: &Replacement) {
		self.generation = replacement.generation;
		self.copy = replacement.copy.clone();
		// A later switch must keep this copy subscribed while the recovered group
		// still reads it, just like a group handed out from that copy directly.
		self.lease = self.reader.upgrade().and_then(|reader| {
			let reader = reader.lock().expect("reader poisoned");
			reader
				.copies
				.iter()
				.find(|copy| copy.generation == replacement.generation)
				.map(|copy| copy.lease.clone())
		});
		self.fetch = None;
	}
}

/// A candidate cursor, committed only when the requested read can use it.
pub(crate) struct Replacement {
	pub(crate) group: group::Consumer,
	pub(crate) copy: track::Consumer,
	generation: u64,
}

/// A fetch of one group through whichever route serves the logical track.
pub(crate) struct Fetching {
	route: Consumer,
	sequence: u64,
	options: group::Fetch,
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
				Ok(group) => Poll::Ready(Ok(group.with_recover(Recover {
					sequence: self.sequence,
					reader: Weak::new(),
					route: self.route.clone(),
					generation,
					copy: serving.clone(),
					lease: None,
					fetch: None,
				}))),
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
	use crate::{Timestamp, broadcast};

	/// A route's copy of the track.
	fn copy() -> track::Producer {
		track::Producer::new(Arc::new(broadcast::Info::default()), "video", None)
	}

	/// The logical track `routes` serves.
	fn logical(routes: &Producer) -> track::Producer {
		track::Request::new(Arc::new(broadcast::Info::default()), "video")
			.routes(routes.consume())
			.accept(None)
	}

	fn subscribe(logical: &track::Producer, max_age: Duration) -> track::Subscriber {
		let subscription = Subscription::default().with_max_age(max_age);
		logical
			.consume()
			.subscribe(subscription)
			.now_or_never()
			.unwrap()
			.unwrap()
	}

	fn recv(sub: &mut track::Subscriber) -> group::Consumer {
		sub.recv_group()
			.now_or_never()
			.expect("a group")
			.unwrap()
			.expect("track ended")
	}

	fn read(group: &mut group::Consumer) -> Option<Vec<u8>> {
		let frame = group.read_frame().now_or_never().expect("a frame").unwrap();
		frame.map(|frame| frame.payload.to_vec())
	}

	fn ts(ms: u64) -> Timestamp {
		Timestamp::from_millis(ms).unwrap()
	}

	#[test]
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
		let mut whole = b.create_group(group::Info { sequence: 0 }).unwrap();
		whole.write_frame(ts(0), b"abcd".as_ref()).unwrap();
		routes.serve(b.consume());

		assert_eq!(
			&chunks.read_chunk().now_or_never().unwrap().unwrap().unwrap()[..],
			b"cd"
		);
		assert!(chunks.read_chunk().now_or_never().unwrap().unwrap().is_none());
	}

	#[test]
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

	#[test]
	fn a_replaced_route_finishes_only_the_groups_it_had() {
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

	#[test]
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

	#[derive(Default)]
	struct Wake(std::sync::atomic::AtomicBool);

	impl std::task::Wake for Wake {
		fn wake(self: Arc<Self>) {
			self.0.store(true, std::sync::atomic::Ordering::Relaxed);
		}
	}

	#[test]
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

	#[test]
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
					timestamp: ts(0),
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
					timestamp: ts(0),
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
				assert!(matches!(reading.read_frame().now_or_never(), Some(Ok(None))));
			}
		}
	}

	#[test]
	fn a_pending_fetch_wakes_and_moves_to_the_new_route() {
		let routes = Producer::new();
		let a = copy();
		let dynamic = a.dynamic();
		routes.serve(a.consume());
		let mut fetching = routes.consume().fetch_group(0, group::Fetch::default());
		let wake = Arc::new(Wake::default());
		let waiter = kio::Waiter::new(wake.clone().into());
		assert!(kio::Task::poll(&mut fetching, &waiter).is_pending());
		let _request = dynamic.requested_group().now_or_never().unwrap().unwrap();
		wake.0.store(false, std::sync::atomic::Ordering::Relaxed);
		let b = copy();
		let mut group = b.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(ts(0), b"b".as_ref()).unwrap();
		group.finish().unwrap();
		routes.serve(b.consume());
		assert!(wake.0.load(std::sync::atomic::Ordering::Relaxed));
		assert!(matches!(kio::Task::poll(&mut fetching, &waiter), Poll::Ready(Ok(_))));
	}

	#[test]
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
	fn a_fetched_group_wakes_when_the_route_changes() {
		let routes = Producer::new();
		let a = copy();
		let _open = a.create_group(group::Info { sequence: 0 }).unwrap();
		routes.serve(a.consume());
		let mut reading = kio::Pending::new(routes.consume().fetch_group(0, group::Fetch::default()))
			.now_or_never()
			.unwrap()
			.unwrap();
		let wake = Arc::new(Wake::default());
		let waiter = kio::Waiter::new(wake.clone().into());
		assert!(reading.poll_read_frame(&waiter).is_pending());
		routes.serve(copy().consume());
		assert!(wake.0.load(std::sync::atomic::Ordering::Relaxed));
	}

	#[test]
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

	#[test]
	fn ending_the_front_releases_a_pending_fetch() {
		let routes = Producer::new();
		let a = copy();
		let _dynamic = a.dynamic();
		routes.serve(a.consume());
		let mut fetching = routes.consume().fetch_group(0, group::Fetch::default());
		let waiter = kio::Waiter::noop();
		assert!(kio::Task::poll(&mut fetching, &waiter).is_pending());
		routes.end(Err(Error::Unroutable));
		assert!(matches!(
			kio::Task::poll(&mut fetching, &waiter),
			Poll::Ready(Err(Error::Unroutable))
		));
	}

	#[test]
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
										timestamp: ts(1),
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
						// Retire the original copy so its preferences stop being mirrored.
						assert!(sub.recv_group().now_or_never().is_none());
						assert!(a.subscription().is_none());
					}
					wake.0.store(false, std::sync::atomic::Ordering::Relaxed);
					sub.control()
						.update(
							Subscription::default()
								.with_priority(7)
								.with_max_age(Duration::from_secs(if widen { 10 } else { 2 })),
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
					} else if chunked {
						assert!(
							matches!(result, Poll::Ready(Err(Error::Old))),
							"recovered={recovered}: narrowed budget did not truncate the frame"
						);
					} else {
						assert!(
							matches!(result, Poll::Ready(Ok(()))),
							"recovered={recovered}: narrowed budget did not end the drained group"
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
									.with_max_age(Duration::from_secs(10)),
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
								.with_max_age(Duration::from_secs(2)),
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
			assert_eq!(demand.max_age, Duration::from_secs(2), "{events:?}");
			drop(groups);
			drop(subscriptions);
			drop(control);
			for copy in &copies {
				assert!(copy.subscription().is_none(), "{events:?}: cancelled demand survived");
			}
		}

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
