//! A broadcast's track listing: the tracks it lists, reported to each reader in order.
//!
//! A track is listed while it is in the broadcast's inventory: created by its
//! [`Producer`](super::Producer) (until it leaves the track table), or answered by a
//! handler through a [`Lister`], as a relay's copy lists its upstream's tracks. A track
//! served only on request is never listed, and listing never requests one.
//!
//! Each [`Tracks`] reader keeps its own queue, keyed by name in insertion order, so a
//! stalled reader holds at most one pending change per name: an add and a removal it
//! has not reached cancel, and a removal and re-add of a name it saw collapse in place.
//! Its state is bounded by the names it saw plus the live set, however long it stalls.

use std::{
	sync::Arc,
	task::{Poll, ready},
};

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use hashlink::LinkedHashMap;

use super::{BroadcastState, Consumer};
use crate::{Error, track};

/// What a [`Tracks`] listing reports, in order.
#[derive(Clone, Debug)]
pub enum TracksEvent {
	/// A track the broadcast lists, with its fixed properties.
	Added {
		/// The track's name, unique among the broadcast's listed tracks.
		name: Arc<str>,
		/// The track's properties, the same its TRACK_INFO carries.
		info: track::Info,
	},
	/// Every track this hop listed when the listing began, and still lists, has been
	/// reported. Sent once; later events are live.
	Synced,
	/// A track is no longer listed, unfinished: its producer dropped or aborted before
	/// declaring its end, or the route or handler that listed it withdrew. A handler may still
	/// serve its name on request.
	Removed {
		/// The name an earlier [`Self::Added`] reported.
		name: Arc<str>,
	},
	/// A track is no longer listed because it finished: its producer declared its end. Its
	/// groups stay readable while the cache retains them.
	Finished {
		/// The name an earlier [`Self::Added`] reported.
		name: Arc<str>,
	},
}

/// One listed track and who lists it.
struct Entry {
	info: track::Info,
	/// The producer's listing, numbered so a stale unlist never removes a later track
	/// of the name.
	own: Option<u64>,
	/// Whether a handler's answer lists it.
	answered: bool,
}

/// Who holds a listing, such as a session serving a peer: at most one listing of each
/// broadcast at a time. A newer listing by the same owner ends the older with
/// [`Error::Cancel`], so an owner's listings cost one queue per broadcast however many
/// paths or requests reach it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
// Sessions dedup a peer's listings per broadcast; a custom session has no public owner yet.
pub(crate) struct Owner(u64);

impl Owner {
	// Sessions dedup a peer's listings per broadcast; a custom session has no public owner yet.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn new() -> Self {
		static NEXT: AtomicU64 = AtomicU64::new(0);
		Self(NEXT.fetch_add(1, Ordering::Relaxed))
	}
}

/// The listing half of a broadcast's state, guarded by the broadcast's lock.
#[derive(Default)]
pub(super) struct ListingState {
	/// Every listed track, in the order it was listed.
	live: LinkedHashMap<Arc<str>, Entry>,
	/// The queue of every reader of the current answer.
	readers: slab::Slab<kio::Shared<Queue>>,
	/// Bumped whenever every reader is detached at once, so a reader of an earlier
	/// answer releases nothing on drop.
	generation: u64,
	/// Each owner's reader, by its key in `readers`.
	owners: HashMap<Owner, usize>,
	/// Whether a [`Lister`] exists. While one does, readers wait for its answer to sync.
	lister: bool,
	/// Whether the handler's answer has synced.
	answered: bool,
	next_token: u64,
}

impl ListingState {
	fn synced(&self) -> bool {
		!self.lister || self.answered
	}

	fn each(&self, mut f: impl FnMut(&mut Queue)) {
		for (_, queue) in &self.readers {
			f(&mut queue.lock());
		}
	}

	/// Start a reader with every listed track queued, ending `owner`'s older one.
	fn read(&mut self, owner: Option<Owner>) -> (kio::Shared<Queue>, usize, u64) {
		let mut queue = Queue::default();
		for (name, entry) in &self.live {
			queue.pending.insert(name.clone(), Pending::Add(entry.info.clone()));
		}
		if self.synced() {
			queue.bound = Some(queue.pending.len());
		}
		let queue = kio::Shared::new(queue);
		let key = self.readers.insert(queue.clone());
		if let Some(older) = owner.and_then(|owner| self.owners.insert(owner, key)) {
			let older = self.readers.remove(older);
			let mut older = older.lock();
			older.end = Some(Err(Error::Cancel));
			older.detached = true;
		}
		(queue, key, self.generation)
	}

	fn release(&mut self, key: usize, generation: u64, owner: Option<Owner>, queue: &kio::Shared<Queue>) {
		// A replaced reader already left, and its key may be another's now.
		if generation != self.generation || queue.read().detached {
			return;
		}
		self.readers.try_remove(key);
		if let Some(owner) = owner {
			self.owners.remove(&owner);
		}
	}

	/// Re-evaluate whether the listing has synced, telling readers when it changed.
	fn resync(&mut self, was: bool) {
		match (was, self.synced()) {
			(false, true) => self.each(Queue::sync),
			(true, false) => self.each(Queue::unsync),
			_ => {}
		}
	}

	fn insert(&mut self, name: Arc<str>, entry: Entry) {
		let info = entry.info.clone();
		self.live.insert(name.clone(), entry);
		self.each(|queue| queue.add(name.clone(), info.clone()));
	}

	/// Unlist `name`, telling readers whether it `finished`.
	fn remove(&mut self, name: &str, finished: bool) {
		if let Some((name, _)) = self.live.remove_entry(name) {
			self.each(|queue| queue.remove(&name, finished));
		}
	}

	/// List a track the producer created, returning the token that unlists it.
	pub(super) fn list_own(&mut self, name: Arc<str>, info: track::Info) -> u64 {
		let token = self.next_token;
		self.next_token += 1;
		match self.live.get_mut(&name) {
			// The earlier track of the name has not unlisted yet, but it left the table
			// for this one to be created: readers see it go and the new one arrive.
			Some(entry) if entry.own.is_some() && !entry.answered => {
				self.remove(&name, false);
				let own = Some(token);
				self.insert(
					name,
					Entry {
						info,
						own,
						answered: false,
					},
				);
			}
			// Listed already, by the handler if not by the producer, and it never stopped
			// being listed: only the token moves.
			Some(entry) => entry.own = Some(token),
			None => {
				let own = Some(token);
				self.insert(
					name,
					Entry {
						info,
						own,
						answered: false,
					},
				);
			}
		}
		token
	}

	/// Unlist a track the producer created, unless a later one took its name.
	pub(super) fn unlist_own(&mut self, name: &str, token: u64, finished: bool) {
		let Some(entry) = self.live.get_mut(name) else {
			return;
		};
		if entry.own != Some(token) {
			return;
		}
		entry.own = None;
		if !entry.answered {
			self.remove(name, finished);
		}
	}

	fn answer_add(&mut self, name: Arc<str>, info: track::Info) -> Result<(), Error> {
		match self.live.get_mut(&name) {
			Some(entry) if entry.answered => Err(Error::Duplicate),
			Some(entry) => {
				entry.answered = true;
				Ok(())
			}
			None => {
				self.insert(
					name,
					Entry {
						info,
						own: None,
						answered: true,
					},
				);
				Ok(())
			}
		}
	}

	fn answer_remove(&mut self, name: &str, finished: bool) -> Result<(), Error> {
		let Some(entry) = self.live.get_mut(name).filter(|entry| entry.answered) else {
			return Err(Error::NotFound);
		};
		entry.answered = false;
		if entry.own.is_none() {
			self.remove(name, finished);
		}
		Ok(())
	}

	/// Drop the handler's answer: every track it listed, and its sync.
	fn answer_clear(&mut self) {
		let was = self.synced();
		self.unanswer();
		self.resync(was);
	}

	/// Unlist every track the handler's answer listed.
	fn unanswer(&mut self) {
		let answered: Vec<Arc<str>> = self
			.live
			.iter()
			.filter(|(_, entry)| entry.answered)
			.map(|(name, _)| name.clone())
			.collect();
		for name in answered {
			let _ = self.answer_remove(&name, false);
		}
		self.answered = false;
	}

	/// End every reader with `end` and detach them.
	fn detach(&mut self, end: Result<(), Error>) {
		self.each(|queue| queue.end = Some(end.clone()));
		self.readers.clear();
		self.owners.clear();
		self.generation += 1;
	}

	/// The broadcast ended: so does every listing of it, first unlisting each track its
	/// producer created that `finished`, as its holder dropping later would have. A handler's
	/// answer of the same name does not keep it live, since the producer's track is what served it.
	pub(super) fn close(&mut self, finished: impl Fn(&str) -> bool) {
		let done: Vec<Arc<str>> = self
			.live
			.iter()
			.filter(|(name, entry)| entry.own.is_some() && finished(name))
			.map(|(name, _)| name.clone())
			.collect();
		for name in done {
			self.remove(&name, true);
		}
		self.detach(Ok(()));
	}
}

/// Unlists a track the producer created once it leaves the track table. Held by the
/// track's teardown guard, so it drops with the last producer, or when the track aborts.
pub(in crate::model) struct Unlist {
	state: kio::Shared<BroadcastState>,
	name: Arc<str>,
	token: u64,
	/// Whether the track declared its end, so readers see it finish rather than vanish.
	finished: bool,
}

impl Unlist {
	/// List `name` now, from inside the broadcast's lock.
	pub(super) fn list(
		state: &kio::Shared<BroadcastState>,
		listing: &mut ListingState,
		name: Arc<str>,
		info: &track::Info,
	) -> Self {
		let token = listing.list_own(name.clone(), info.clone());
		Self {
			state: state.clone(),
			name,
			token,
			finished: false,
		}
	}

	/// Unlist now, as a track that `finished` or left unfinished.
	pub(in crate::model) fn unlist(mut self, finished: bool) {
		self.finished = finished;
	}
}

impl Drop for Unlist {
	fn drop(&mut self) {
		self.state
			.lock()
			.listing
			.unlist_own(&self.name, self.token, self.finished);
	}
}

/// Lists a reserved track once it is accepted with its info.
pub(in crate::model) struct ListOnAccept {
	pub(super) state: kio::Shared<BroadcastState>,
	pub(super) name: Arc<str>,
}

impl ListOnAccept {
	/// List the track with the info it was accepted with. Called outside the broadcast's lock.
	pub(in crate::model) fn list(self, info: &track::Info) -> Unlist {
		let mut state = self.state.lock();
		Unlist::list(&self.state, &mut state.listing, self.name.clone(), info)
	}
}

/// A change a reader has yet to read.
enum Pending {
	Add(track::Info),
	/// The track left, having finished or not.
	Remove {
		finished: bool,
	},
	/// A removal of a track the reader saw, then a track of the same name listed again.
	Readd {
		finished: bool,
		info: track::Info,
	},
}

/// One reader's pending changes, keyed by name in the order they happened.
#[derive(Default)]
pub(super) struct Queue {
	pending: LinkedHashMap<Arc<str>, Pending>,
	/// Events left to send before SYNCED, once the listing has synced.
	bound: Option<usize>,
	/// SYNCED was sent.
	synced: bool,
	end: Option<Result<(), Error>>,
	/// A newer reader of the same owner replaced this one.
	detached: bool,
}

impl Queue {
	fn add(&mut self, name: Arc<str>, info: track::Info) {
		match self.pending.get_mut(&name) {
			Some(pending) => match *pending {
				Pending::Remove { finished } => *pending = Pending::Readd { finished, info },
				// A name is listed once at a time, so only its removal can be pending.
				_ => debug_assert!(false, "a listed track was listed again"),
			},
			None => {
				self.pending.insert(name, Pending::Add(info));
			}
		}
	}

	fn remove(&mut self, name: &Arc<str>, finished: bool) {
		match self.pending.get_mut(name) {
			// Never read: the two cancel.
			Some(Pending::Add(_)) => {
				self.pending.remove(name);
			}
			// Read, then re-added unread: the reader sees only the last removal.
			Some(pending) => *pending = Pending::Remove { finished },
			None => {
				self.pending.insert(name.clone(), Pending::Remove { finished });
			}
		}
	}

	/// The events the pending changes take to read.
	fn events(&self) -> usize {
		self.pending
			.values()
			.map(|pending| match pending {
				Pending::Readd { .. } => 2,
				_ => 1,
			})
			.sum()
	}

	/// The listing synced: SYNCED goes out once what is queued now has been read, or
	/// earlier if the queue drains first.
	fn sync(&mut self) {
		if !self.synced && self.bound.is_none() {
			self.bound = Some(self.events());
		}
	}

	/// The listing's answer restarted before this reader was told it synced.
	fn unsync(&mut self) {
		if !self.synced {
			self.bound = None;
		}
	}

	fn is_ready(&self) -> bool {
		self.end.is_some() || !self.pending.is_empty() || (!self.synced && self.bound.is_some())
	}

	fn next(&mut self) -> Result<Option<TracksEvent>, Error> {
		match &self.end {
			Some(Err(err)) => return Err(err.clone()),
			// A clean end still reports each track that left before it, and how; an add the
			// reader never read is dropped.
			Some(Ok(())) => {
				while let Some((name, pending)) = self.pending.pop_front() {
					match pending {
						Pending::Add(_) => {}
						Pending::Remove { finished } | Pending::Readd { finished, .. } => {
							return Ok(Some(removal(name, finished)));
						}
					}
				}
				return Ok(None);
			}
			None => {}
		}
		if !self.synced && (self.bound == Some(0) || (self.bound.is_some() && self.pending.is_empty())) {
			self.synced = true;
			return Ok(Some(TracksEvent::Synced));
		}
		let (name, pending) = self.pending.pop_front().expect("a ready queue has a pending change");
		if let Some(bound) = self.bound.as_mut().filter(|_| !self.synced) {
			*bound -= 1;
		}
		Ok(Some(match pending {
			Pending::Add(info) => TracksEvent::Added { name, info },
			Pending::Remove { finished } => removal(name, finished),
			Pending::Readd { finished, info } => {
				self.pending.insert_front(name.clone(), Pending::Add(info));
				removal(name, finished)
			}
		}))
	}
}

/// The event for `name` leaving the listing.
fn removal(name: Arc<str>, finished: bool) -> TracksEvent {
	match finished {
		true => TracksEvent::Finished { name },
		false => TracksEvent::Removed { name },
	}
}

/// The tracks a broadcast lists, from [`Consumer::tracks`].
///
/// Reports every track listed when it was created, then [`TracksEvent::Synced`], then
/// each change as it happens, until the broadcast ends: a clean end comes after each
/// track that left before it, and a track still listed then ends unfinished. Listing a broadcast never
/// requests a track: subscribe to each listed one through [`Consumer::track`].
pub struct Tracks {
	broadcast: Consumer,
	queue: kio::Shared<Queue>,
	key: usize,
	generation: u64,
	owner: Option<Owner>,
}

impl Tracks {
	/// Poll for the next event: `None` once the broadcast ended, or the error that ended
	/// the listing, such as an upstream refusing it.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<TracksEvent>, Error>> {
		let mut queue = ready!(self.queue.poll(waiter, |queue| match queue.is_ready() {
			true => Poll::Ready(()),
			false => Poll::Pending,
		}));
		Poll::Ready(queue.next())
	}

	/// Wait for the next event; see [`Self::poll_next`].
	#[cfg(feature = "async")]
	pub async fn next(&mut self) -> Result<Option<TracksEvent>, Error> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// The broadcast being listed.
	pub fn broadcast(&self) -> &Consumer {
		&self.broadcast
	}
}

impl Drop for Tracks {
	fn drop(&mut self) {
		let mut state = self.broadcast.state.lock();
		state
			.listing
			.release(self.key, self.generation, self.owner, &self.queue);
	}
}

impl Consumer {
	/// List the broadcast's tracks: each one with its info, then
	/// [`TracksEvent::Synced`], then changes as they happen.
	///
	/// Lists the tracks the producer created and any a session answers with, such as a
	/// relay's copy listing its upstream's. A track served only on
	/// request is never listed, and listing never requests one. Fails with
	/// [`Error::Unroutable`] once the broadcast has ended.
	pub fn tracks(&self) -> Result<Tracks, Error> {
		self.list(None)
	}

	/// [`Self::tracks`] for `owner`, ending its older listing of this broadcast with
	/// [`Error::Cancel`].
	// Sessions dedup a peer's listings per broadcast; a custom session has no public owner yet.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn tracks_as(&self, owner: Owner) -> Result<Tracks, Error> {
		self.list(Some(owner))
	}

	fn list(&self, owner: Option<Owner>) -> Result<Tracks, Error> {
		let mut state = self.state.lock();
		if state.closing {
			return Err(Error::Unroutable);
		}
		let (queue, key, generation) = state.listing.read(owner);
		drop(state);
		Ok(Tracks {
			broadcast: self.clone(),
			queue,
			key,
			generation,
			owner,
		})
	}
}

/// A handler's answer to the broadcast's listings, from [`Requests::lister`](super::Requests::lister).
///
/// A broadcast has at most one. While it exists, readers wait for it to
/// [`sync`](Self::synced) before reporting [`TracksEvent::Synced`]. Answer while [`poll_wanted`](Self::poll_wanted): list what the
/// publisher behind the handler holds and keep it current, then [`clear`](Self::clear)
/// once [`poll_unwanted`](Self::poll_unwanted). Dropping it clears what it listed.
pub(crate) struct Lister {
	state: kio::Shared<BroadcastState>,
}

impl Lister {
	/// Fails with [`Error::Duplicate`] while the broadcast has another.
	pub(super) fn new(state: kio::Shared<BroadcastState>) -> Result<Self, Error> {
		{
			let mut state = state.lock();
			if state.listing.lister {
				return Err(Error::Duplicate);
			}
			let was = state.listing.synced();
			state.listing.lister = true;
			state.listing.resync(was);
		}
		Ok(Self { state })
	}

	fn poll_readers(&self, waiter: &kio::Waiter, wanted: bool) -> Poll<Result<(), Error>> {
		let state = ready!(self.state.poll(waiter, |state| {
			match state.closing || state.listing.readers.is_empty() != wanted {
				true => Poll::Ready(()),
				false => Poll::Pending,
			}
		}));
		Poll::Ready(match state.closing {
			true => Err(Error::Closed),
			false => Ok(()),
		})
	}

	/// Ready once anyone lists the broadcast; [`Error::Closed`] once it ended.
	pub fn poll_wanted(&self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		self.poll_readers(waiter, true)
	}

	/// Ready once nobody lists the broadcast; [`Error::Closed`] once it ended.
	pub fn poll_unwanted(&self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		self.poll_readers(waiter, false)
	}

	/// List a track. Fails with [`Error::Duplicate`] if this answer already lists it.
	pub fn add(&self, name: impl Into<Arc<str>>, info: track::Info) -> Result<(), Error> {
		let mut state = self.state.lock();
		if state.closing {
			return Err(Error::Closed);
		}
		state.listing.answer_add(name.into(), info)
	}

	/// Stop listing a track that left unfinished, reported as [`TracksEvent::Removed`].
	/// Fails with [`Error::NotFound`] if this answer does not list it.
	pub fn remove(&self, name: &str) -> Result<(), Error> {
		self.unlist(name, false)
	}

	/// Stop listing a track that finished, reported as [`TracksEvent::Finished`]. Fails with
	/// [`Error::NotFound`] if this answer does not list it.
	pub fn finish(&self, name: &str) -> Result<(), Error> {
		self.unlist(name, true)
	}

	fn unlist(&self, name: &str, finished: bool) -> Result<(), Error> {
		let mut state = self.state.lock();
		if state.closing {
			return Err(Error::Closed);
		}
		state.listing.answer_remove(name, finished)
	}

	/// The answer holds everything this hop knows now; later changes are live.
	pub fn synced(&self) {
		let mut state = self.state.lock();
		let was = state.listing.synced();
		state.listing.answered = true;
		state.listing.resync(was);
	}

	/// Withdraw the answer: every track it listed, and its sync.
	pub fn clear(&self) {
		self.state.lock().listing.answer_clear();
	}

	/// End every current reader as if the broadcast ended, and withdraw the answer, as
	/// when the publisher behind the handler ends its broadcast first. A later reader
	/// asks again.
	pub fn end(&self) {
		self.detach(Ok(()));
	}

	/// Fail every current reader with `err` and withdraw the answer. A later reader
	/// asks again.
	pub fn abort(&self, err: Error) {
		self.detach(Err(err));
	}

	fn detach(&self, end: Result<(), Error>) {
		let mut state = self.state.lock();
		state.listing.detach(end);
		state.listing.answer_clear();
	}
}

impl Drop for Lister {
	fn drop(&mut self) {
		let mut state = self.state.lock();
		let was = state.listing.synced();
		state.listing.lister = false;
		// Synced from here on, with only the producer's tracks.
		state.listing.unanswer();
		state.listing.resync(was);
	}
}

#[cfg(test)]
mod tests {
	use super::super::Info;
	use super::*;

	/// The next event as `+name`, `-name`, `synced`, `end`, or `error: ..`; `None` while pending.
	fn next(tracks: &mut Tracks) -> Option<String> {
		match tracks.poll_next(&kio::Waiter::noop()) {
			Poll::Pending => None,
			Poll::Ready(Ok(Some(TracksEvent::Added { name, .. }))) => Some(format!("+{name}")),
			Poll::Ready(Ok(Some(TracksEvent::Removed { name }))) => Some(format!("-{name}")),
			Poll::Ready(Ok(Some(TracksEvent::Finished { name }))) => Some(format!("~{name}")),
			Poll::Ready(Ok(Some(TracksEvent::Synced))) => Some("synced".into()),
			Poll::Ready(Ok(None)) => Some("end".into()),
			Poll::Ready(Err(err)) => Some(format!("error: {err}")),
		}
	}

	/// Every event ready now, through the end of the listing.
	fn drain(tracks: &mut Tracks) -> Vec<String> {
		let mut events = Vec::new();
		while let Some(event) = next(tracks) {
			let end = event == "end" || event.starts_with("error");
			events.push(event);
			if end {
				break;
			}
		}
		events
	}

	fn pending(tracks: &Tracks) -> usize {
		tracks.queue.read().pending.len()
	}

	const NONE: [&str; 0] = [];

	#[test]
	fn tracks_present_arrive_before_synced() {
		let broadcast = Info::new().produce();
		let _a = broadcast.create_track("a", None).unwrap();
		let _b = broadcast.create_track("b", None).unwrap();
		let mut tracks = broadcast.consume().tracks().unwrap();
		assert_eq!(drain(&mut tracks), ["+a", "+b", "synced"]);
	}

	#[test]
	fn an_empty_listing_syncs_at_once() {
		let broadcast = Info::new().produce();
		let mut tracks = broadcast.consume().tracks().unwrap();
		assert_eq!(drain(&mut tracks), ["synced"]);
		let _a = broadcast.create_track("a", None).unwrap();
		assert_eq!(drain(&mut tracks), ["+a"]);
	}

	#[test]
	fn a_listed_track_reports_its_info() {
		let broadcast = Info::new().produce();
		let _a = broadcast
			.create_track("a", track::Info::default().with_priority(9))
			.unwrap();
		let mut tracks = broadcast.consume().tracks().unwrap();
		let Poll::Ready(Ok(Some(TracksEvent::Added { info, .. }))) = tracks.poll_next(&kio::Waiter::noop()) else {
			panic!("expected the track");
		};
		assert_eq!(info.priority, 9);
	}

	#[test]
	fn a_track_leaving_the_table_is_removed() {
		let broadcast = Info::new().produce();
		let a = broadcast.create_track("a", None).unwrap();
		let b = broadcast.create_track("b", None).unwrap();
		let mut tracks = broadcast.consume().tracks().unwrap();
		assert_eq!(drain(&mut tracks), ["+a", "+b", "synced"]);
		drop(a);
		b.abort(Error::Cancel).unwrap();
		assert_eq!(drain(&mut tracks), ["-a", "-b"]);
	}

	/// A track whose producer declared its end leaves as finished; one dropped or aborted
	/// without an end leaves unfinished.
	#[test]
	fn a_removal_says_whether_the_track_finished() {
		let broadcast = Info::new().produce();
		let a = broadcast.create_track("a", None).unwrap();
		let b = broadcast.create_track("b", None).unwrap();
		let c = broadcast.create_track("c", None).unwrap();
		let mut tracks = broadcast.consume().tracks().unwrap();
		assert_eq!(drain(&mut tracks), ["+a", "+b", "+c", "synced"]);
		a.finish().unwrap();
		drop(a);
		drop(b);
		c.finish().unwrap();
		c.abort(Error::Cancel).unwrap();
		assert_eq!(drain(&mut tracks), ["~a", "-b", "-c"]);
	}

	/// A finished track listed again under its name is still reported as having finished
	/// first, and the re-add follows.
	#[test]
	fn a_finished_track_listed_again_collapses_in_place() {
		let broadcast = Info::new().produce();
		let a = broadcast.create_track("a", None).unwrap();
		let mut tracks = broadcast.consume().tracks().unwrap();
		assert_eq!(drain(&mut tracks), ["+a", "synced"]);
		a.finish().unwrap();
		drop(a);
		let _a = broadcast.create_track("a", None).unwrap();
		assert_eq!(pending(&tracks), 1, "the removal and re-add collapse");
		assert_eq!(drain(&mut tracks), ["~a", "+a"]);
	}

	/// A handler's answer reports each track as its upstream ended it.
	#[test]
	fn a_lister_finishes_or_removes_a_track() {
		let broadcast = Info::new().produce();
		let lister = broadcast.requests().lister().unwrap();
		lister.add("up", track::Info::default()).unwrap();
		lister.add("gone", track::Info::default()).unwrap();
		lister.synced();
		let mut tracks = broadcast.consume().tracks().unwrap();
		assert_eq!(drain(&mut tracks), ["+up", "+gone", "synced"]);
		lister.finish("up").unwrap();
		lister.remove("gone").unwrap();
		assert_eq!(drain(&mut tracks), ["~up", "-gone"]);
		assert!(matches!(lister.finish("up"), Err(Error::NotFound)));
	}

	/// A clean end reports each track that left before it, and how, but not one the reader
	/// never saw; an error ends at once.
	#[test]
	fn a_clean_end_follows_the_removals_before_it() {
		let broadcast = Info::new().produce();
		let a = broadcast.create_track("a", None).unwrap();
		let b = broadcast.create_track("b", None).unwrap();
		let live = broadcast.create_track("live", None).unwrap();
		let mut tracks = broadcast.consume().tracks().unwrap();
		assert_eq!(drain(&mut tracks), ["+a", "+b", "+live", "synced"]);
		a.finish().unwrap();
		drop(a);
		drop(b);
		let _unseen = broadcast.create_track("unseen", None).unwrap();
		live.finish().unwrap();
		broadcast.close();
		assert_eq!(drain(&mut tracks), ["~a", "-b", "~live", "end"]);

		// The producer's finished track is what a handler's answer of its name served.
		let broadcast = Info::new().produce();
		let lister = broadcast.requests().lister().unwrap();
		lister.add("a", track::Info::default()).unwrap();
		lister.synced();
		let a = broadcast.create_track("a", None).unwrap();
		let mut tracks = broadcast.consume().tracks().unwrap();
		assert_eq!(drain(&mut tracks), ["+a", "synced"]);
		a.finish().unwrap();
		broadcast.close();
		assert_eq!(drain(&mut tracks), ["~a", "end"]);

		// A handler's end is clean too, and its abort is not.
		for abort in [false, true] {
			let broadcast = Info::new().produce();
			let lister = broadcast.requests().lister().unwrap();
			lister.add("up", track::Info::default()).unwrap();
			lister.synced();
			let mut tracks = broadcast.consume().tracks().unwrap();
			assert_eq!(drain(&mut tracks), ["+up", "synced"]);
			lister.finish("up").unwrap();
			match abort {
				true => {
					lister.abort(Error::Cancel);
					assert_eq!(drain(&mut tracks), [format!("error: {}", Error::Cancel)]);
				}
				false => {
					lister.end();
					assert_eq!(drain(&mut tracks), ["~up", "end"]);
				}
			}
		}
	}

	#[test]
	fn a_recreated_track_is_removed_before_it_is_added_again() {
		let broadcast = Info::new().produce();
		let a = broadcast.create_track("a", None).unwrap();
		let mut tracks = broadcast.consume().tracks().unwrap();
		assert_eq!(drain(&mut tracks), ["+a", "synced"]);
		drop(a);
		let _a = broadcast.create_track("a", None).unwrap();
		assert_eq!(pending(&tracks), 1, "the removal and re-add collapse");
		assert_eq!(drain(&mut tracks), ["-a", "+a"]);
	}

	#[test]
	fn a_reserved_track_is_listed_once_accepted() {
		let broadcast = Info::new().produce();
		let request = broadcast.reserve_track("a").unwrap();
		let mut tracks = broadcast.consume().tracks().unwrap();
		assert_eq!(drain(&mut tracks), ["synced"]);
		let a = request.accept(None);
		assert_eq!(drain(&mut tracks), ["+a"]);
		drop(a);
		assert_eq!(drain(&mut tracks), ["-a"]);
	}

	#[test]
	fn a_staged_track_is_listed_until_dropped_uncommitted() {
		let broadcast = Info::new().produce();
		let request = broadcast.reserve_track("a").unwrap();
		let mut tracks = broadcast.consume().tracks().unwrap();
		assert_eq!(drain(&mut tracks), ["synced"]);
		let staged = request.stage(None);
		assert_eq!(drain(&mut tracks), ["+a"]);
		drop(staged);
		assert_eq!(drain(&mut tracks), ["-a"]);
	}

	#[test]
	fn a_track_served_on_request_is_never_listed() {
		let broadcast = Info::new().produce();
		let mut dynamic = broadcast.requests();
		let consumer = broadcast.consume();
		let mut tracks = consumer.tracks().unwrap();
		assert_eq!(drain(&mut tracks), ["synced"]);
		// Listing asks the handler for nothing.
		dynamic.assert_no_request();

		let _resolved = consumer.resolve_track("x", None).unwrap();
		let _x = dynamic.assert_request().accept(None);
		assert_eq!(drain(&mut tracks), NONE);
	}

	#[test]
	fn an_initial_track_removed_before_it_is_read_is_omitted() {
		let broadcast = Info::new().produce();
		let a = broadcast.create_track("a", None).unwrap();
		let _b = broadcast.create_track("b", None).unwrap();
		let mut tracks = broadcast.consume().tracks().unwrap();
		drop(a);
		assert_eq!(drain(&mut tracks), ["+b", "synced"]);
	}

	#[test]
	fn churn_never_postpones_synced() {
		let broadcast = Info::new().produce();
		let _a = broadcast.create_track("a", None).unwrap();
		let _b = broadcast.create_track("b", None).unwrap();
		let mut tracks = broadcast.consume().tracks().unwrap();
		// New names keep arriving faster than the reader drains them.
		let _c = broadcast.create_track("c", None).unwrap();
		assert_eq!(next(&mut tracks).as_deref(), Some("+a"));
		let _d = broadcast.create_track("d", None).unwrap();
		assert_eq!(next(&mut tracks).as_deref(), Some("+b"));
		let _e = broadcast.create_track("e", None).unwrap();
		assert_eq!(drain(&mut tracks), ["synced", "+c", "+d", "+e"]);
	}

	#[test]
	fn a_removal_of_a_seen_track_may_precede_synced() {
		let broadcast = Info::new().produce();
		let a = broadcast.create_track("a", None).unwrap();
		let _b = broadcast.create_track("b", None).unwrap();
		let c = broadcast.create_track("c", None).unwrap();
		let mut tracks = broadcast.consume().tracks().unwrap();
		assert_eq!(next(&mut tracks).as_deref(), Some("+a"));
		drop(a);
		drop(c);
		assert_eq!(drain(&mut tracks), ["+b", "-a", "synced"]);
	}

	#[test]
	fn a_stalled_reader_holds_one_change_per_name() {
		let broadcast = Info::new().produce();
		let seen = broadcast.create_track("seen", None).unwrap();
		let _stay = broadcast.create_track("stay", None).unwrap();
		let mut tracks = broadcast.consume().tracks().unwrap();
		assert_eq!(drain(&mut tracks), ["+seen", "+stay", "synced"]);
		drop(seen);
		for round in 0..1000 {
			let churn = broadcast.create_track(format!("churn{round}"), None).unwrap();
			let seen = broadcast.create_track("seen", None).unwrap();
			drop(churn);
			drop(seen);
			assert!(pending(&tracks) <= 2, "round {round}: {}", pending(&tracks));
		}
		assert_eq!(drain(&mut tracks), ["-seen"]);
	}

	#[test]
	fn the_broadcast_ending_ends_the_listing() {
		let broadcast = Info::new().produce();
		let _a = broadcast.create_track("a", None).unwrap();
		let consumer = broadcast.consume();
		let mut tracks = consumer.tracks().unwrap();
		broadcast.close();
		assert_eq!(drain(&mut tracks), ["end"]);
		assert!(matches!(consumer.tracks(), Err(Error::Unroutable)));
	}

	#[test]
	fn readers_wait_for_the_lister_to_sync() {
		let broadcast = Info::new().produce();
		let _own = broadcast.create_track("own", None).unwrap();
		let lister = broadcast.requests().lister().unwrap();
		let waiter = kio::Waiter::noop();
		assert!(lister.poll_wanted(&waiter).is_pending());

		let mut tracks = broadcast.consume().tracks().unwrap();
		assert!(matches!(lister.poll_wanted(&waiter), Poll::Ready(Ok(()))));
		assert_eq!(drain(&mut tracks), ["+own"], "not synced until the lister says so");
		lister.add("up", track::Info::default()).unwrap();
		assert!(matches!(
			lister.add("up", track::Info::default()),
			Err(Error::Duplicate)
		));
		lister.synced();
		assert_eq!(drain(&mut tracks), ["+up", "synced"]);

		lister.remove("up").unwrap();
		assert!(matches!(lister.remove("up"), Err(Error::NotFound)));
		assert_eq!(drain(&mut tracks), ["-up"]);

		assert!(lister.poll_unwanted(&waiter).is_pending());
		drop(tracks);
		assert!(matches!(lister.poll_unwanted(&waiter), Poll::Ready(Ok(()))));
	}

	#[test]
	fn a_name_listed_by_both_is_reported_once() {
		let broadcast = Info::new().produce();
		let own = broadcast.create_track("a", None).unwrap();
		let lister = broadcast.requests().lister().unwrap();
		lister.add("a", track::Info::default()).unwrap();
		lister.synced();
		let mut tracks = broadcast.consume().tracks().unwrap();
		assert_eq!(drain(&mut tracks), ["+a", "synced"]);
		drop(own);
		assert_eq!(drain(&mut tracks), NONE, "still answered");
		lister.remove("a").unwrap();
		assert_eq!(drain(&mut tracks), ["-a"]);
	}

	#[test]
	fn an_aborted_answer_fails_its_readers_and_a_later_reader_asks_again() {
		let broadcast = Info::new().produce();
		let lister = broadcast.requests().lister().unwrap();
		let waiter = kio::Waiter::noop();
		let mut tracks = broadcast.consume().tracks().unwrap();
		lister.add("up", track::Info::default()).unwrap();
		lister.abort(Error::NotFound);
		assert_eq!(drain(&mut tracks), [format!("error: {}", Error::NotFound)]);
		assert!(
			matches!(lister.poll_unwanted(&waiter), Poll::Ready(Ok(()))),
			"a failed reader is not demand"
		);

		let mut again = broadcast.consume().tracks().unwrap();
		assert!(matches!(lister.poll_wanted(&waiter), Poll::Ready(Ok(()))));
		assert_eq!(drain(&mut again), NONE, "a fresh answer");
		lister.synced();
		assert_eq!(drain(&mut again), ["synced"]);
		drop(tracks);
		assert!(
			lister.poll_unwanted(&waiter).is_pending(),
			"a failed reader releases nothing"
		);
	}

	#[test]
	fn a_dropped_lister_withdraws_its_answer_and_syncs() {
		let broadcast = Info::new().produce();
		let lister = broadcast.requests().lister().unwrap();
		lister.add("up", track::Info::default()).unwrap();
		let mut tracks = broadcast.consume().tracks().unwrap();
		assert_eq!(drain(&mut tracks), ["+up"]);
		drop(lister);
		assert_eq!(drain(&mut tracks), ["-up", "synced"]);
	}

	/// An owner holds one listing per broadcast: its newer listing ends the older, while
	/// other owners and other broadcasts keep theirs.
	#[test]
	fn an_owner_lists_each_broadcast_once() {
		let broadcast = Info::new().produce();
		let other = Info::new().produce();
		let _a = broadcast.create_track("a", None).unwrap();
		let owner = Owner::new();

		let mut older = broadcast.consume().tracks_as(owner).unwrap();
		let mut elsewhere = other.consume().tracks_as(owner).unwrap();
		let mut stranger = broadcast.consume().tracks_as(Owner::new()).unwrap();
		let mut newer = broadcast.consume().tracks_as(owner).unwrap();
		assert_eq!(drain(&mut older), [format!("error: {}", Error::Cancel)]);
		assert_eq!(drain(&mut newer), ["+a", "synced"]);
		assert_eq!(
			drain(&mut elsewhere),
			["synced"],
			"another broadcast is its own listing"
		);
		assert_eq!(
			drain(&mut stranger),
			["+a", "synced"],
			"another owner keeps its listing"
		);

		// The replaced reader leaving releases nothing of the newer one's.
		drop(older);
		let _b = broadcast.create_track("b", None).unwrap();
		assert_eq!(drain(&mut newer), ["+b"]);
		assert_eq!(broadcast.state.lock().listing.readers.len(), 2);
		drop(newer);
		assert_eq!(broadcast.state.lock().listing.readers.len(), 1);
		assert!(
			broadcast.state.lock().listing.owners.len() == 1,
			"the stranger's entry stays"
		);
	}

	/// A producer's track replaced under a name the handler also lists never stopped
	/// being listed, so readers see nothing.
	#[test]
	fn an_answered_name_survives_its_own_track_being_replaced() {
		let mut listing = ListingState::default();
		let first = listing.list_own("a".into(), track::Info::default());
		listing.answer_add("a".into(), track::Info::default()).unwrap();
		let (queue, _, _) = listing.read(None);
		assert!(matches!(queue.lock().next(), Ok(Some(TracksEvent::Added { .. }))));
		// The new track of the name lists before the earlier one unlists.
		listing.list_own("a".into(), track::Info::default());
		listing.unlist_own("a", first, false);
		assert!(queue.lock().pending.is_empty(), "the name never stopped being listed");
	}

	/// One answer per broadcast: a second handler's answer would share the first's sync
	/// and outlive the handler that gave it.
	#[test]
	fn a_broadcast_has_one_lister() {
		let broadcast = Info::new().produce();
		let lister = broadcast.requests().lister().unwrap();
		assert!(matches!(broadcast.requests().lister(), Err(Error::Duplicate)));
		drop(lister);
		broadcast.requests().lister().expect("the first lister left");
	}

	#[test]
	fn an_ended_answer_ends_its_readers_and_a_later_reader_asks_again() {
		let broadcast = Info::new().produce();
		let lister = broadcast.requests().lister().unwrap();
		let waiter = kio::Waiter::noop();
		let mut tracks = broadcast.consume().tracks().unwrap();
		lister.add("up", track::Info::default()).unwrap();
		lister.synced();
		assert_eq!(drain(&mut tracks), ["+up", "synced"]);
		lister.end();
		assert_eq!(drain(&mut tracks), ["end"]);
		assert!(matches!(lister.poll_unwanted(&waiter), Poll::Ready(Ok(()))));

		let mut again = broadcast.consume().tracks().unwrap();
		assert!(matches!(lister.poll_wanted(&waiter), Poll::Ready(Ok(()))));
		assert_eq!(drain(&mut again), NONE, "the finished answer was withdrawn");
	}

	#[test]
	fn a_cleared_answer_unsyncs_a_reader_still_waiting() {
		let broadcast = Info::new().produce();
		let lister = broadcast.requests().lister().unwrap();
		let _a = broadcast.create_track("a", None).unwrap();
		let _b = broadcast.create_track("b", None).unwrap();
		lister.synced();
		let mut tracks = broadcast.consume().tracks().unwrap();
		assert_eq!(next(&mut tracks).as_deref(), Some("+a"));
		lister.clear();
		assert_eq!(drain(&mut tracks), ["+b"], "the answer restarted");
		lister.synced();
		assert_eq!(drain(&mut tracks), ["synced"]);
	}

	#[test]
	fn a_lister_sees_the_broadcast_end() {
		let broadcast = Info::new().produce();
		let lister = broadcast.requests().lister().unwrap();
		broadcast.close();
		assert!(matches!(
			lister.poll_wanted(&kio::Waiter::noop()),
			Poll::Ready(Err(Error::Closed))
		));
		assert!(matches!(lister.add("a", track::Info::default()), Err(Error::Closed)));
	}
}
