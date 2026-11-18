//! A broadcast's track listing: the tracks it lists, reported to each reader in order.
//!
//! A track is listed while it is in the broadcast's inventory: created by its
//! [`Producer`](super::Producer) (a track of groups until it leaves the track table, a
//! datagram track until it ends), or answered by a handler through a [`Lister`], as a
//! relay's copy lists its upstream's tracks. A track served only on request is never
//! listed, and listing never requests one.
//!
//! The two [`Kind`]s are namespaces apart, so a name is listed at most once per kind.
//!
//! Each [`Tracks`] reader keeps its own queue, keyed by kind and name in insertion order, so
//! a stalled reader holds at most one pending change per listed track: an add and a removal
//! it has not reached cancel, and a removal and re-add of a track it saw collapse in place.
//! Its state is bounded by the tracks it saw plus the live set, however long it stalls.

use std::{
	sync::Arc,
	task::{Poll, ready},
};

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use hashlink::LinkedHashMap;

use super::{BroadcastState, Consumer};
use crate::{Error, datagram, track};

/// Which of a broadcast's two namespaces a listed track is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
	/// A track of groups, read through [`Consumer::track`].
	Track,
	/// A datagram track, read through [`Consumer::datagrams`].
	Datagrams,
}

/// A listed track's fixed properties, whose variant is its [`Kind`].
#[derive(Clone, Debug)]
pub enum Listed {
	/// A track of groups, with the properties its TRACK_INFO carries.
	Track(track::Info),
	/// A datagram track, with the properties its DATAGRAMS_INFO carries.
	Datagrams(datagram::Info),
}

impl Listed {
	/// The namespace the track is in.
	pub fn kind(&self) -> Kind {
		match self {
			Self::Track(_) => Kind::Track,
			Self::Datagrams(_) => Kind::Datagrams,
		}
	}
}

impl From<track::Info> for Listed {
	fn from(info: track::Info) -> Self {
		Self::Track(info)
	}
}

impl From<datagram::Info> for Listed {
	fn from(info: datagram::Info) -> Self {
		Self::Datagrams(info)
	}
}

/// A listed track's identity: a name is unique within its kind.
type Key = (Kind, Arc<str>);

/// What a [`Tracks`] listing reports, in order.
#[derive(Clone, Debug)]
pub enum TracksEvent {
	/// A track the broadcast lists, with its kind and fixed properties.
	Added {
		/// The track's name, unique among the broadcast's listed tracks of its kind.
		name: Arc<str>,
		/// The track's kind and properties.
		info: Listed,
	},
	/// Every track this hop listed when the listing began, and still lists, has been
	/// reported. Sent once; later events are live.
	Synced,
	/// A track is no longer listed, unfinished: its producer dropped or aborted before
	/// declaring its end, or the route or handler that listed it withdrew. A handler may still
	/// serve its name on request.
	Removed {
		/// The kind an earlier [`Self::Added`] reported.
		kind: Kind,
		/// The name an earlier [`Self::Added`] reported.
		name: Arc<str>,
	},
	/// A track is no longer listed because it finished: its producer declared its end. A
	/// track of groups stays readable while the cache retains its groups.
	Finished {
		/// The kind an earlier [`Self::Added`] reported.
		kind: Kind,
		/// The name an earlier [`Self::Added`] reported.
		name: Arc<str>,
	},
}

/// One listed track and who lists it.
struct Entry {
	info: Listed,
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
	live: LinkedHashMap<Key, Entry>,
	/// The queue of every reader of the current answer.
	readers: slab::Slab<kio::Shared<Queue>>,
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
	fn read(&mut self, owner: Option<Owner>) -> (kio::Shared<Queue>, usize) {
		let mut queue = Queue::default();
		for (key, entry) in &self.live {
			queue.pending.insert(key.clone(), Pending::Add(entry.info.clone()));
		}
		if self.synced() {
			queue.sync = SyncPhase::Counting(queue.pending.len());
		}
		let queue = kio::Shared::new(queue);
		let key = self.readers.insert(queue.clone());
		if let Some(older) = owner.and_then(|owner| self.owners.insert(owner, key)) {
			self.readers.remove(older).lock().end = Some(Err(Error::Cancel));
		}
		(queue, key)
	}

	fn release(&mut self, key: usize, owner: Option<Owner>, queue: &kio::Shared<Queue>) {
		// A replaced or ended reader already left, and its key may be another's now.
		if !self.readers.get(key).is_some_and(|current| current.same_channel(queue)) {
			return;
		}
		self.readers.remove(key);
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

	fn insert(&mut self, key: Key, entry: Entry) {
		let info = entry.info.clone();
		self.live.insert(key.clone(), entry);
		self.each(|queue| queue.add(key.clone(), info.clone()));
	}

	/// Unlist `key`, telling readers whether it `finished`.
	fn remove(&mut self, key: &Key, finished: bool) {
		if let Some((key, _)) = self.live.remove_entry(key) {
			self.each(|queue| queue.remove(&key, finished));
		}
	}

	/// List a track the producer created, returning the token that unlists it.
	pub(super) fn list_own(&mut self, name: Arc<str>, info: Listed) -> u64 {
		let token = self.next_token;
		self.next_token += 1;
		let key = (info.kind(), name);
		match self.live.get_mut(&key) {
			// The earlier track of the name has not unlisted yet, but it left the table
			// for this one to be created: readers see it go and the new one arrive.
			Some(entry) if entry.own.is_some() && !entry.answered => {
				self.remove(&key, false);
				let own = Some(token);
				self.insert(
					key,
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
					key,
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
	pub(super) fn unlist_own(&mut self, key: &Key, token: u64, finished: bool) {
		let Some(entry) = self.live.get_mut(key) else {
			return;
		};
		if entry.own != Some(token) {
			return;
		}
		entry.own = None;
		if !entry.answered {
			self.remove(key, finished);
		}
	}

	fn answer_add(&mut self, name: Arc<str>, info: Listed) -> Result<(), Error> {
		let key = (info.kind(), name);
		match self.live.get_mut(&key) {
			Some(entry) if entry.answered => Err(Error::Duplicate),
			Some(entry) => {
				entry.answered = true;
				Ok(())
			}
			None => {
				self.insert(
					key,
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

	fn answer_remove(&mut self, key: &Key, finished: bool) -> Result<(), Error> {
		let Some(entry) = self.live.get_mut(key).filter(|entry| entry.answered) else {
			return Err(Error::NotFound);
		};
		entry.answered = false;
		if entry.own.is_none() {
			self.remove(key, finished);
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
		let answered: Vec<Key> = self
			.live
			.iter()
			.filter(|(_, entry)| entry.answered)
			.map(|(key, _)| key.clone())
			.collect();
		for key in answered {
			let _ = self.answer_remove(&key, false);
		}
		self.answered = false;
	}

	/// End every reader with `end` and detach them.
	fn detach(&mut self, end: Result<(), Error>) {
		self.each(|queue| queue.end = Some(end.clone()));
		self.readers.clear();
		self.owners.clear();
	}

	/// The broadcast ended: so does every listing of it, first unlisting each track of groups
	/// its producer created that `finished`, as its holder dropping later would have. A
	/// handler's answer of the same name does not keep it live, since the producer's track is
	/// what served it. A datagram track that finished already left the listing as it did.
	pub(super) fn close(&mut self, finished: impl Fn(&str) -> bool) {
		let done: Vec<Key> = self
			.live
			.iter()
			.filter(|((kind, name), entry)| *kind == Kind::Track && entry.own.is_some() && finished(name))
			.map(|(key, _)| key.clone())
			.collect();
		for key in done {
			self.remove(&key, true);
		}
		self.detach(Ok(()));
	}
}

/// Unlists a track the producer created once it leaves the track table. Held by the
/// track's teardown guard, so it drops with the last producer, or when the track aborts;
/// by a datagram track until it ends.
pub(in crate::model) struct Unlist {
	state: kio::Shared<BroadcastState>,
	key: Key,
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
		info: Listed,
	) -> Self {
		let kind = info.kind();
		let token = listing.list_own(name.clone(), info);
		Self {
			state: state.clone(),
			key: (kind, name),
			token,
			finished: false,
		}
	}

	/// Unlist now, as a track that `finished` or left unfinished.
	pub(in crate::model) fn unlist(self, finished: bool) {
		drop(self.finished(finished));
	}

	/// Unlist once dropped, as a track that `finished` or left unfinished: for a caller
	/// holding a lock taken under the broadcast's, which must release it first.
	pub(in crate::model) fn finished(mut self, finished: bool) -> Self {
		self.finished = finished;
		self
	}
}

impl Drop for Unlist {
	fn drop(&mut self) {
		self.state
			.lock()
			.listing
			.unlist_own(&self.key, self.token, self.finished);
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
		Unlist::list(
			&self.state,
			&mut state.listing,
			self.name.clone(),
			Listed::Track(info.clone()),
		)
	}
}

/// A change a reader has yet to read.
enum Pending {
	Add(Listed),
	/// The track left, having finished or not.
	Remove {
		finished: bool,
	},
	/// A removal of a track the reader saw, then a track of the same kind and name listed again.
	Readd {
		finished: bool,
		info: Listed,
	},
}

/// One reader's pending changes, keyed by kind and name in the order they happened.
#[derive(Default)]
pub(super) struct Queue {
	pending: LinkedHashMap<Key, Pending>,
	sync: SyncPhase,
	/// How the listing ended for this reader, once it did: its broadcast ended, or a newer
	/// reader of the same owner replaced it.
	end: Option<Result<(), Error>>,
}

/// Where one reader's SYNCED stands.
#[derive(Default)]
enum SyncPhase {
	/// The listing has not synced.
	#[default]
	Waiting,
	/// The listing synced: SYNCED goes out after this many more events, or once the queue
	/// drains if that comes first.
	Counting(usize),
	/// SYNCED went out.
	Sent,
}

impl Queue {
	fn add(&mut self, key: Key, info: Listed) {
		match self.pending.get_mut(&key) {
			Some(pending) => match *pending {
				Pending::Remove { finished } => {
					*pending = Pending::Readd { finished, info };
					// The readd's extra event must not use up the count owed to changes queued behind it.
					if let SyncPhase::Counting(left) = &mut self.sync {
						*left += 1;
					}
				}
				// A name is listed once at a time, so only its removal can be pending.
				_ => debug_assert!(false, "a listed track was listed again"),
			},
			None => {
				self.pending.insert(key, Pending::Add(info));
			}
		}
	}

	fn remove(&mut self, key: &Key, finished: bool) {
		match self.pending.get_mut(key) {
			// Never read: the two cancel.
			Some(Pending::Add(_)) => {
				self.pending.remove(key);
			}
			// Read, then re-added unread: the reader sees only the last removal, one event fewer.
			Some(pending @ Pending::Readd { .. }) => {
				*pending = Pending::Remove { finished };
				if let SyncPhase::Counting(left) = &mut self.sync {
					*left = left.saturating_sub(1);
				}
			}
			Some(pending) => *pending = Pending::Remove { finished },
			None => {
				self.pending.insert(key.clone(), Pending::Remove { finished });
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
		if let SyncPhase::Waiting = self.sync {
			self.sync = SyncPhase::Counting(self.events());
		}
	}

	/// The listing's answer restarted before this reader was told it synced.
	fn unsync(&mut self) {
		if let SyncPhase::Counting(_) = self.sync {
			self.sync = SyncPhase::Waiting;
		}
	}

	fn is_ready(&self) -> bool {
		self.end.is_some() || !self.pending.is_empty() || matches!(self.sync, SyncPhase::Counting(_))
	}

	fn next(&mut self) -> Result<Option<TracksEvent>, Error> {
		match &self.end {
			Some(Err(err)) => return Err(err.clone()),
			// A clean end still reports each track that left before it, and how; an add the
			// reader never read is dropped.
			Some(Ok(())) => {
				while let Some((key, pending)) = self.pending.pop_front() {
					match pending {
						Pending::Add(_) => {}
						Pending::Remove { finished } | Pending::Readd { finished, .. } => {
							return Ok(Some(removal(key, finished)));
						}
					}
				}
				return Ok(None);
			}
			None => {}
		}
		if let SyncPhase::Counting(left) = self.sync
			&& (left == 0 || self.pending.is_empty())
		{
			self.sync = SyncPhase::Sent;
			return Ok(Some(TracksEvent::Synced));
		}
		let (key, pending) = self.pending.pop_front().expect("a ready queue has a pending change");
		if let SyncPhase::Counting(left) = &mut self.sync {
			*left -= 1;
		}
		Ok(Some(match pending {
			Pending::Add(info) => TracksEvent::Added { name: key.1, info },
			Pending::Remove { finished } => removal(key, finished),
			Pending::Readd { finished, info } => {
				self.pending.insert_front(key.clone(), Pending::Add(info));
				removal(key, finished)
			}
		}))
	}
}

/// The event for `key` leaving the listing.
fn removal((kind, name): Key, finished: bool) -> TracksEvent {
	match finished {
		true => TracksEvent::Finished { kind, name },
		false => TracksEvent::Removed { kind, name },
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
	owner: Option<Owner>,
}

impl Tracks {
	/// The next event: `None` once the broadcast ended, or the error that ended
	/// the listing, such as an upstream refusing it.
	#[cfg_attr(feature = "async", kio::wrapper)]
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<TracksEvent>, Error>> {
		let mut queue = ready!(self.queue.poll(waiter, |queue| match queue.is_ready() {
			true => Poll::Ready(()),
			false => Poll::Pending,
		}));
		Poll::Ready(queue.next())
	}

	/// The broadcast being listed.
	pub fn broadcast(&self) -> &Consumer {
		&self.broadcast
	}
}

impl Drop for Tracks {
	fn drop(&mut self) {
		let mut state = self.broadcast.state.lock();
		state.listing.release(self.key, self.owner, &self.queue);
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
		let (queue, key) = state.listing.read(owner);
		drop(state);
		Ok(Tracks {
			broadcast: self.clone(),
			queue,
			key,
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
pub struct Lister {
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

	/// List a track of the kind `info` is: a [`track::Info`] or a [`datagram::Info`]. Fails
	/// with [`Error::Duplicate`] if this answer already lists that kind and name.
	pub fn add(&self, name: impl Into<Arc<str>>, info: impl Into<Listed>) -> Result<(), Error> {
		let mut state = self.state.lock();
		if state.closing {
			return Err(Error::Closed);
		}
		state.listing.answer_add(name.into(), info.into())
	}

	/// Stop listing a track that left unfinished, reported as [`TracksEvent::Removed`].
	/// Fails with [`Error::NotFound`] if this answer does not list that kind and name.
	pub fn remove(&self, kind: Kind, name: impl Into<Arc<str>>) -> Result<(), Error> {
		self.unlist((kind, name.into()), false)
	}

	/// Stop listing a track that finished, reported as [`TracksEvent::Finished`]. Fails with
	/// [`Error::NotFound`] if this answer does not list that kind and name.
	pub fn finish(&self, kind: Kind, name: impl Into<Arc<str>>) -> Result<(), Error> {
		self.unlist((kind, name.into()), true)
	}

	fn unlist(&self, key: Key, finished: bool) -> Result<(), Error> {
		let mut state = self.state.lock();
		if state.closing {
			return Err(Error::Closed);
		}
		state.listing.answer_remove(&key, finished)
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

	/// An event as `+name`, `-name`, `~name` (finished), or `synced`, a datagram track's name
	/// prefixed `d:`.
	fn show(event: TracksEvent) -> String {
		let named = |kind: Kind, name: Arc<str>| match kind {
			Kind::Track => name.to_string(),
			Kind::Datagrams => format!("d:{name}"),
		};
		match event {
			TracksEvent::Added { name, info } => format!("+{}", named(info.kind(), name)),
			TracksEvent::Removed { kind, name } => format!("-{}", named(kind, name)),
			TracksEvent::Finished { kind, name } => format!("~{}", named(kind, name)),
			TracksEvent::Synced => "synced".into(),
		}
	}

	/// The next event as [`show`] has it, `end`, or `error: ..`; `None` while pending.
	fn next(tracks: &mut Tracks) -> Option<String> {
		match tracks.poll_next(&kio::Waiter::noop()) {
			Poll::Pending => None,
			Poll::Ready(Ok(Some(event))) => Some(show(event)),
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

	/// A removal re-added after SYNCED was owed still lets the changes queued behind it out first.
	#[test]
	fn a_readd_after_sync_keeps_synced_behind_queued_changes() {
		let key = |name: &str| (Kind::Track, Arc::from(name));
		let info = || Listed::Track(track::Info::default());
		let mut queue = Queue::default();
		queue.remove(&key("a"), false);
		queue.add(key("b"), info());
		queue.sync();
		// Churn coalesces in place, so it owes SYNCED nothing extra.
		for _ in 0..3 {
			queue.add(key("a"), info());
			queue.remove(&key("a"), false);
		}
		queue.add(key("a"), info());
		let mut events = Vec::new();
		while queue.is_ready() {
			events.push(show(queue.next().unwrap().unwrap()));
		}
		assert_eq!(events, ["-a", "+a", "+b", "synced"]);
	}

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

	/// A datagram track the producer creates is listed with its info until it ends, finished
	/// or not.
	#[test]
	fn a_datagram_track_is_listed_until_it_ends() {
		let broadcast = Info::new().produce();
		let early = broadcast
			.create_datagrams("early", datagram::Info::default().with_priority(4))
			.unwrap();
		let mut tracks = broadcast.consume().tracks().unwrap();
		let Poll::Ready(Ok(Some(TracksEvent::Added {
			info: Listed::Datagrams(info),
			..
		}))) = tracks.poll_next(&kio::Waiter::noop())
		else {
			panic!("expected the datagram track");
		};
		assert_eq!(info.priority, 4);
		assert_eq!(drain(&mut tracks), ["synced"]);

		let late = broadcast.create_datagrams("late", Default::default()).unwrap();
		let gone = broadcast.create_datagrams("gone", Default::default()).unwrap();
		assert_eq!(drain(&mut tracks), ["+d:late", "+d:gone"]);
		let clone = early.clone();
		early.finish();
		drop(clone);
		late.abort(Error::Cancel);
		drop(gone);
		assert_eq!(drain(&mut tracks), ["~d:early", "-d:late", "-d:gone"]);

		// A name ended is free for a new track, listed afresh.
		let _early = broadcast.create_datagrams("early", Default::default()).unwrap();
		assert_eq!(drain(&mut tracks), ["+d:early"]);
	}

	/// A track of groups and a datagram track of one name are listed apart, and each leaves
	/// on its own.
	#[test]
	fn the_two_kinds_of_a_name_are_listed_apart() {
		let broadcast = Info::new().produce();
		let track = broadcast.create_track("a", None).unwrap();
		let datagrams = broadcast.create_datagrams("a", Default::default()).unwrap();
		let mut tracks = broadcast.consume().tracks().unwrap();
		assert_eq!(drain(&mut tracks), ["+a", "+d:a", "synced"]);
		drop(track);
		assert_eq!(drain(&mut tracks), ["-a"]);
		datagrams.finish();
		assert_eq!(drain(&mut tracks), ["~d:a"]);

		let lister = broadcast.requests().lister().unwrap();
		lister.add("b", track::Info::default()).unwrap();
		lister.add("b", datagram::Info::default()).unwrap();
		assert!(matches!(
			lister.add("b", datagram::Info::default()),
			Err(Error::Duplicate)
		));
		lister.synced();
		assert_eq!(drain(&mut tracks), ["+b", "+d:b"]);
		lister.finish(Kind::Datagrams, "b").unwrap();
		assert!(matches!(lister.remove(Kind::Datagrams, "b"), Err(Error::NotFound)));
		lister.remove(Kind::Track, "b").unwrap();
		assert_eq!(drain(&mut tracks), ["~d:b", "-b"]);
	}

	/// A datagram track a consumer asked for and a handler served is not in the inventory,
	/// as a track served on request is not.
	#[test]
	fn a_datagram_track_served_on_request_is_never_listed() {
		let broadcast = Info::new().produce();
		let mut requests = broadcast.requests().datagrams();
		let consumer = broadcast.consume();
		let mut tracks = consumer.tracks().unwrap();
		assert_eq!(drain(&mut tracks), ["synced"]);
		let _reader = consumer.datagrams("x").unwrap();
		let Poll::Ready(Ok(request)) = requests.poll_requested(&kio::Waiter::noop()) else {
			panic!("expected the request");
		};
		let _x = request.accept(Default::default());
		assert_eq!(drain(&mut tracks), NONE);
	}

	/// Creating a datagram track a consumer already asked for serves that consumer, and lists it.
	#[test]
	fn a_created_datagram_track_lists_though_it_was_asked_for() {
		let broadcast = Info::new().produce();
		let _requests = broadcast.requests().datagrams();
		let consumer = broadcast.consume();
		let mut tracks = consumer.tracks().unwrap();
		assert_eq!(drain(&mut tracks), ["synced"]);
		let _reader = consumer.datagrams("x").unwrap();
		let x = broadcast.create_datagrams("x", Default::default()).unwrap();
		assert_eq!(drain(&mut tracks), ["+d:x"]);
		drop(x);
		assert_eq!(drain(&mut tracks), ["-d:x"]);
	}

	#[test]
	fn a_listed_track_reports_its_info() {
		let broadcast = Info::new().produce();
		let _a = broadcast
			.create_track("a", track::Info::default().with_priority(9))
			.unwrap();
		let mut tracks = broadcast.consume().tracks().unwrap();
		let Poll::Ready(Ok(Some(TracksEvent::Added {
			info: Listed::Track(info),
			..
		}))) = tracks.poll_next(&kio::Waiter::noop())
		else {
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
		let a = a.finish().unwrap();
		drop(a);
		drop(b);
		let c = c.finish().unwrap();
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
		let a = a.finish().unwrap();
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
		lister.finish(Kind::Track, "up").unwrap();
		lister.remove(Kind::Track, "gone").unwrap();
		assert_eq!(drain(&mut tracks), ["~up", "-gone"]);
		assert!(matches!(lister.finish(Kind::Track, "up"), Err(Error::NotFound)));
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
		let a = a.finish().unwrap();
		drop(a);
		drop(b);
		let _unseen = broadcast.create_track("unseen", None).unwrap();
		let _live = live.finish().unwrap();
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
		let _a = a.finish().unwrap();
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
			lister.finish(Kind::Track, "up").unwrap();
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

	/// A reservation that is never listed is still read by anyone who names it.
	#[test]
	fn an_unlisted_track_is_read_by_name_but_never_listed() {
		let broadcast = Info::new().produce();
		let consumer = broadcast.consume();
		let mut tracks = consumer.tracks().unwrap();
		assert_eq!(drain(&mut tracks), ["synced"]);
		let request = broadcast.reserve_unlisted_track("a").unwrap();
		let a = request.accept(None);
		let mut group = a.append_group(crate::Timestamp::unscaled(0)).unwrap();
		group.write_frame(crate::Timestamp::unscaled(0), vec![7]).unwrap();
		group.finish().unwrap();

		let waiter = kio::Waiter::noop();
		let Poll::Ready(Ok(mut reader)) = consumer.track("a").unwrap().subscribe(None).poll_ok(&waiter) else {
			panic!("the track is read by name");
		};
		let Poll::Ready(Ok(Some(mut group))) = reader.poll_recv_group(&waiter) else {
			panic!("expected the group");
		};
		let Poll::Ready(Ok(Some(frame))) = group.poll_read_frame(&waiter) else {
			panic!("expected the frame");
		};
		assert_eq!(&frame.payload[..], [7]);
		drop(a);
		assert_eq!(drain(&mut tracks), NONE);
	}

	/// An abort handle ends a listed track the way its producer's abort does: unlisted, with
	/// its name free for a new track.
	#[test]
	fn an_abort_handle_unlists_the_track_it_ends() {
		let broadcast = Info::new().produce();
		let a = broadcast.create_track("a", None).unwrap();
		let handle = a.abort_handle();
		let mut tracks = broadcast.consume().tracks().unwrap();
		assert_eq!(drain(&mut tracks), ["+a", "synced"]);
		assert!(!handle.is_closed());
		handle.abort(Error::Cancel);
		assert!(handle.is_closed());
		assert_eq!(drain(&mut tracks), ["-a"]);
		assert!(a.append_group(crate::Timestamp::unscaled(0)).is_err());
		let _again = broadcast.create_track("a", None).unwrap();
		assert_eq!(drain(&mut tracks), ["+a"]);
	}

	/// An abort handle keeps no track open: once every producer dropped, the track closed and
	/// the handle does nothing.
	#[test]
	fn an_abort_handle_keeps_no_dropped_track_alive() {
		let broadcast = Info::new().produce();
		let a = broadcast.create_track("a", None).unwrap();
		let handle = a.abort_handle();
		let mut tracks = broadcast.consume().tracks().unwrap();
		assert_eq!(drain(&mut tracks), ["+a", "synced"]);
		drop(a);
		assert!(handle.is_closed());
		assert_eq!(drain(&mut tracks), ["-a"]);
		handle.abort(Error::Cancel);
		assert_eq!(drain(&mut tracks), NONE);
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

		lister.remove(Kind::Track, "up").unwrap();
		assert!(matches!(lister.remove(Kind::Track, "up"), Err(Error::NotFound)));
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
		lister.remove(Kind::Track, "a").unwrap();
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
		let first = listing.list_own("a".into(), track::Info::default().into());
		listing.answer_add("a".into(), track::Info::default().into()).unwrap();
		let (queue, _) = listing.read(None);
		assert!(matches!(queue.lock().next(), Ok(Some(TracksEvent::Added { .. }))));
		// The new track of the name lists before the earlier one unlists.
		listing.list_own("a".into(), track::Info::default().into());
		listing.unlist_own(&(Kind::Track, "a".into()), first, false);
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
