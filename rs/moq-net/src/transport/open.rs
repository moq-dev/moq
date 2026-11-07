//! The one entry point every stream a session opens goes through.
//!
//! A peer grants a bounded number of concurrent streams in each direction. Once that credit
//! runs out, every open waits for the peer to grant more, and the transport hands whatever it
//! grants to whichever waiting open polls first. Here the opens of one session queue instead,
//! ranked exactly as the transport will send their streams, and only the first in line asks
//! the transport for credit, so returning credit goes to the most important open.
//!
//! An open can also wait on something other than credit, such as a WebTransport stream's
//! header, so the line asks the transport's [`StreamCredit`] whether the peer is what it waits
//! for.
//!
//! The session also lists the group streams it has open, each with its rank and whether the
//! peer has acknowledged everything written to it. A unidirectional open waiting for the peer's
//! credit resets the lowest-ranked one below it to make room, at most one reset in flight per
//! waiting open, since each frees credit only once the peer's MAX_STREAMS arrives.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, Weak};
use std::task::Poll;

use super::boxed::{self, Transport};
use super::poll::SendOptions;
use super::{Dir, StreamCredit};

/// Where an open or an open stream ranks: the order the transport sends its stream in.
///
/// Mirrors the transport's send scheduling: streams outside every group go before all of
/// them, then a higher group priority, then a higher priority within the group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Rank {
	Grouped { group: i64, stream: i64 },
	Ungrouped { stream: i64 },
}

impl Rank {
	/// The rank of a stream at `stream` priority, in a group at `group` priority if any.
	pub(super) fn new(group: Option<i64>, stream: i64) -> Self {
		match group {
			Some(group) => Self::Grouped { group, stream },
			None => Self::Ungrouped { stream },
		}
	}

	fn of(options: &SendOptions<'_, super::boxed::Group>) -> Self {
		Self::new(options.group.map(super::boxed::Group::priority), options.priority)
	}
}

/// One of a session's open group streams, as its list read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OpenStream {
	/// Where the stream ranks now.
	pub rank: Rank,
	/// Whether the peer has acknowledged every byte written to it, or `None` when the
	/// transport cannot tell.
	pub acknowledged: Option<bool>,
}

/// The group streams a session has open, keyed so each can leave.
#[derive(Default)]
struct Listing {
	streams: HashMap<u64, Weak<boxed::Listed>>,
	next: u64,
}

/// A place in line: the highest rank first, then the earliest arrival.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
	rank: Reverse<Rank>,
	arrival: u64,
}

/// The opens waiting for credit in one direction.
#[derive(Default)]
struct Line {
	waiting: BTreeMap<Key, kio::WaiterList>,
	arrivals: u64,
	/// Streams reset to make room whose credit has yet to reach an open, never more than the
	/// opens waiting: the first this many in line are owed one.
	resets: usize,
}

impl Line {
	fn head(&self) -> Option<Key> {
		self.waiting.keys().next().copied()
	}

	/// Say why the line starts waiting: the peer's stream credit, unless the transport reports
	/// otherwise, and what the session's open group streams could make room with.
	fn start(credit: Option<StreamCredit>, lines: &Lines) {
		if !tracing::enabled!(tracing::Level::DEBUG) {
			return;
		}
		let open = lines.open_streams();
		let open_streams = open.len();
		let acknowledged = open.iter().filter(|stream| stream.acknowledged == Some(true)).count();
		let lowest = open.first().map(|stream| stream.rank);
		match credit {
			Some(credit) if !credit.blocked => {
				tracing::debug!(
					available = credit.available,
					open_streams,
					acknowledged,
					?lowest,
					"opens wait in priority order for the transport"
				)
			}
			_ => tracing::debug!(
				open_streams,
				acknowledged,
				?lowest,
				"out of the peer's stream credit: opens wait in priority order"
			),
		}
	}

	fn join(&mut self, rank: Rank) -> Key {
		let key = Key {
			rank: Reverse(rank),
			arrival: self.arrivals,
		};
		self.arrivals += 1;
		self.waiting.insert(key, kio::WaiterList::default());
		key
	}

	/// Leave the line, waking whoever is now first so it asks the transport for credit.
	fn leave(&mut self, key: Key) {
		let was_head = self.head() == Some(key);
		self.waiting.remove(&key);
		// Whichever open the credit reaches, one fewer open is owed a reset's credit.
		self.resets = self.resets.min(self.waiting.len());
		if was_head {
			self.wake_head();
		}
		if self.waiting.is_empty() {
			tracing::debug!("no opens waiting in line");
		}
	}

	fn wake_head(&mut self) {
		if let Some(waiters) = self.waiting.values_mut().next() {
			waiters.wake();
		}
	}
}

/// The lines of one session's opens, and the group streams it has open, shared by every task
/// opening on it.
pub(crate) struct Lines {
	uni: Mutex<Line>,
	bi: Mutex<Line>,
	listing: Mutex<Listing>,
	/// Whose stream credit the opens wait for.
	pub(super) transport: Transport,
}

impl Lines {
	pub(super) fn new(transport: Transport) -> Self {
		Self {
			uni: Default::default(),
			bi: Default::default(),
			listing: Default::default(),
			transport,
		}
	}

	/// List an open group stream, returning the key it leaves with.
	pub(super) fn list(&self, stream: &Arc<boxed::Listed>) -> u64 {
		let mut listing = self.listing();
		let key = listing.next;
		listing.next += 1;
		listing.streams.insert(key, Arc::downgrade(stream));
		key
	}

	/// Remove the group stream listed under `key`.
	pub(super) fn unlist(&self, key: u64) {
		self.listing().streams.remove(&key);
	}

	/// Reset the lowest-ranked open group stream below each waiting open not yet owed a reset,
	/// unless the transport reports credit left, so the open waits on something else.
	fn make_room(&self, line: &mut Line) {
		if boxed::stream_credit(&self.transport, Dir::Uni).is_some_and(|credit| credit.available > 0) {
			return;
		}
		let owed: Vec<Rank> = line.waiting.keys().skip(line.resets).map(|key| key.rank.0).collect();
		if owed.is_empty() {
			return;
		}
		// Read each stream outside the listing's lock, which its owner takes to leave.
		let listed: Vec<_> = self
			.listing()
			.streams
			.iter()
			.filter_map(|(key, stream)| Some((*key, stream.upgrade()?)))
			.collect();
		let mut open: Vec<_> = listed.into_iter().map(|(key, stream)| (stream.read().rank, key, stream)).collect();
		// Sorted highest first, so the lowest pops off the end.
		open.sort_by_key(|(rank, key, _)| Reverse((*rank, *key)));
		for rank in owed {
			let Some((lowest, key, stream)) = open.pop_if(|(lowest, _, _)| *lowest < rank) else {
				// The opens behind rank lower still.
				break;
			};
			tracing::debug!(?lowest, waiting = ?rank, "reset a lower-ranked stream to make room");
			stream.preempt();
			self.unlist(key);
			line.resets += 1;
		}
	}

	/// The session's open group streams, the lowest ranked first.
	///
	/// A stream is open from when it opens until it is finished, reset, or dropped.
	pub(crate) fn open_streams(&self) -> Vec<OpenStream> {
		// Read each stream outside the listing's lock, which its owner takes to leave.
		let streams: Vec<_> = self.listing().streams.values().filter_map(Weak::upgrade).collect();
		let mut open: Vec<_> = streams.iter().map(|stream| stream.read()).collect();
		open.sort_by_key(|stream| stream.rank);
		open
	}

	fn listing(&self) -> std::sync::MutexGuard<'_, Listing> {
		self.listing.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
	}

	fn line(&self, dir: Dir) -> std::sync::MutexGuard<'_, Line> {
		let line = match dir {
			Dir::Uni => &self.uni,
			Dir::Bi => &self.bi,
		};
		line.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
	}
}

/// One open's place in its session's line for stream credit.
///
/// Keep it across the polls of one open: dropping it gives up the place, and the next open in
/// line asks for credit instead.
#[derive(Default)]
pub(crate) struct Open {
	place: Option<(Arc<Lines>, Dir, Key)>,
}

impl Open {
	/// Poll `open` once this is the most important open waiting for credit in `dir`, ranked by
	/// `options` as of this poll.
	pub(super) fn poll<T, E>(
		&mut self,
		lines: &Arc<Lines>,
		dir: Dir,
		waiter: &kio::Waiter,
		options: &SendOptions<'_, super::boxed::Group>,
		open: impl FnOnce(&kio::Waiter) -> Poll<Result<T, E>>,
	) -> Poll<Result<T, E>> {
		let rank = Rank::of(options);
		let mut line = lines.line(dir);
		let key = match self.place.take() {
			// Nobody is waiting: take credit if there is any, and join the line only without it.
			None if line.waiting.is_empty() => {
				if let Poll::Ready(res) = open(waiter) {
					return Poll::Ready(res);
				}
				Line::start(boxed::stream_credit(&lines.transport, dir), lines);
				let key = line.join(rank);
				line.waiting.get_mut(&key).expect("just joined").register(waiter);
				self.place = Some((lines.clone(), dir, key));
				if dir == Dir::Uni {
					lines.make_room(&mut line);
				}
				return Poll::Pending;
			}
			None => line.join(rank),
			// A rank that moved while waiting moves the open's place, keeping its arrival.
			Some((_, _, key)) if key.rank.0 != rank => {
				let was_head = line.head() == Some(key);
				let waiters = line.waiting.remove(&key).expect("an open's place stays in line");
				let moved = Key {
					rank: Reverse(rank),
					arrival: key.arrival,
				};
				line.waiting.insert(moved, waiters);
				if was_head && line.head() != Some(moved) {
					line.wake_head();
				}
				moved
			}
			Some((_, _, key)) => key,
		};
		self.place = Some((lines.clone(), dir, key));

		line.waiting
			.get_mut(&key)
			.expect("an open's place stays in line")
			.register(waiter);
		if line.head() != Some(key) {
			if dir == Dir::Uni {
				lines.make_room(&mut line);
			}
			return Poll::Pending;
		}
		let Poll::Ready(res) = open(waiter) else {
			if dir == Dir::Uni {
				lines.make_room(&mut line);
			}
			return Poll::Pending;
		};
		self.place = None;
		// Whoever is next may find credit left over, or the same failure.
		line.leave(key);
		Poll::Ready(res)
	}
}

impl Drop for Open {
	fn drop(&mut self) {
		if let Some((lines, dir, key)) = self.place.take() {
			lines.line(dir).leave(key);
		}
	}
}

#[cfg(test)]
impl Default for Lines {
	/// Lines without a transport, which therefore never says why they wait.
	fn default() -> Self {
		Self::new(std::sync::Weak::<boxed::Shared<super::memory::Session>>::new())
	}
}

#[cfg(test)]
mod tests {
	use std::sync::atomic::{AtomicUsize, Ordering};

	use super::*;

	/// A peer's stream credit, granted a stream at a time.
	#[derive(Default)]
	struct Credit {
		available: usize,
		waiters: kio::WaiterList,
	}

	impl Credit {
		fn grant(&mut self, streams: usize) {
			self.available += streams;
			self.waiters.wake();
		}
	}

	/// One open, ranked by an ungrouped send priority.
	struct Opener {
		open: Open,
		priority: i64,
		wakes: Arc<Wakes>,
		waiter: kio::Waiter,
	}

	#[derive(Default)]
	struct Wakes(AtomicUsize);

	impl std::task::Wake for Wakes {
		fn wake(self: Arc<Self>) {
			self.0.fetch_add(1, Ordering::SeqCst);
		}
	}

	impl Opener {
		fn new(priority: i64) -> Self {
			let wakes = Arc::new(Wakes::default());
			Self {
				open: Open::default(),
				priority,
				waiter: kio::Waiter::new(wakes.clone().into()),
				wakes,
			}
		}

		/// Whether it opened, taking a stream from `credit`.
		fn poll(&mut self, lines: &Arc<Lines>, credit: &Mutex<Credit>) -> bool {
			let options = SendOptions {
				group: None,
				priority: self.priority,
			};
			let poll = self.open.poll(lines, Dir::Uni, &self.waiter, &options, |waiter| {
				let mut credit = credit.lock().unwrap();
				if credit.available == 0 {
					waiter.register(&mut credit.waiters);
					return Poll::Pending;
				}
				credit.available -= 1;
				Poll::Ready(Ok::<(), ()>(()))
			});
			poll.is_ready()
		}

		fn woken(&self) -> bool {
			self.wakes.0.swap(0, Ordering::SeqCst) > 0
		}
	}

	/// More opens than credit each get a stream as it returns, the highest priority first,
	/// whichever order they are polled in.
	#[test]
	fn credit_goes_to_the_highest_priority_first() {
		let lines = Arc::new(Lines::default());
		let credit = Mutex::new(Credit::default());
		let mut opens: Vec<Opener> = [1, 3, 2, 3].into_iter().map(Opener::new).collect();
		for open in &mut opens {
			assert!(!open.poll(&lines, &credit));
		}

		let mut order = Vec::new();
		while order.len() < opens.len() {
			credit.lock().unwrap().grant(1);
			for (index, open) in opens.iter_mut().enumerate() {
				if !order.contains(&index) && open.poll(&lines, &credit) {
					order.push(index);
				}
			}
		}
		// Equal priorities keep their arrival order.
		assert_eq!(order, vec![1, 3, 2, 0]);
	}

	/// An open that arrives while others wait queues behind any that outrank it, even with
	/// credit free, rather than taking credit the first in line is owed.
	#[test]
	fn a_late_open_waits_its_turn() {
		let lines = Arc::new(Lines::default());
		let credit = Mutex::new(Credit::default());
		let mut high = Opener::new(5);
		assert!(!high.poll(&lines, &credit));
		credit.lock().unwrap().grant(1);

		let mut low = Opener::new(1);
		assert!(!low.poll(&lines, &credit), "the waiting open is owed the credit");
		assert!(high.poll(&lines, &credit));
		assert!(low.woken(), "the next in line asks for what is left");
		assert!(!low.poll(&lines, &credit));
		credit.lock().unwrap().grant(1);
		assert!(low.poll(&lines, &credit));
	}

	/// A rank that moves while waiting moves the open's place in line.
	#[test]
	fn a_moved_rank_moves_the_place() {
		let lines = Arc::new(Lines::default());
		let credit = Mutex::new(Credit::default());
		let mut first = Opener::new(2);
		let mut second = Opener::new(1);
		assert!(!first.poll(&lines, &credit));
		assert!(!second.poll(&lines, &credit));

		first.priority = 0;
		assert!(!first.poll(&lines, &credit));
		assert!(second.woken(), "the new head asks for credit");
		credit.lock().unwrap().grant(1);
		assert!(!first.poll(&lines, &credit));
		assert!(second.poll(&lines, &credit));
	}

	/// An open given up leaves the line, and the next one asks for credit in its place.
	#[test]
	fn dropping_the_head_wakes_the_next() {
		let lines = Arc::new(Lines::default());
		let credit = Mutex::new(Credit::default());
		let mut head = Opener::new(2);
		let mut next = Opener::new(1);
		assert!(!head.poll(&lines, &credit));
		assert!(!next.poll(&lines, &credit));
		assert!(!next.woken());

		drop(head);
		assert!(next.woken());
		credit.lock().unwrap().grant(1);
		assert!(next.poll(&lines, &credit));
		assert!(lines.line(Dir::Uni).waiting.is_empty());
	}

	/// A session reports the peer's stream credit through its line's transport, blocked once
	/// an open in line was refused, and nothing once the transport is gone.
	#[test]
	fn the_line_reads_the_transports_credit() {
		use super::super::{memory, poll::Session as _};

		let (a, _b) = memory::pair(memory::Config {
			max_concurrent_uni_streams: Some(1),
			..Default::default()
		});
		let session = boxed::Session::new(a);
		let waiter = kio::Waiter::noop();
		let options = || SendOptions {
			group: None,
			priority: 0,
		};
		let mut first = Open::default();
		let opened = session.poll_open_uni(&mut first, &waiter, options());
		assert!(matches!(opened, Poll::Ready(Ok(_))));
		assert_eq!(session.stream_credit(Dir::Uni), Some(StreamCredit::new(0, false)));

		let mut second = Open::default();
		assert!(session.poll_open_uni(&mut second, &waiter, options()).is_pending());
		assert_eq!(session.stream_credit(Dir::Uni), Some(StreamCredit::new(0, true)));
		let transport = session.opens.transport.clone();
		assert_eq!(
			boxed::stream_credit(&transport, Dir::Uni),
			Some(StreamCredit::new(0, true))
		);

		drop((second, session));
		assert_eq!(boxed::stream_credit(&transport, Dir::Uni), None);
	}

	/// Out of credit, each waiting open resets the lowest stream below it, one reset in flight
	/// per waiting open, and opens once the peer returns the credit.
	#[test]
	fn a_waiting_open_resets_the_lowest_stream() {
		use super::super::{Error as _, memory, poll::RecvStream as _, poll::SendStream as _, poll::Session as _};

		let (a, b) = memory::pair(memory::Config {
			max_concurrent_uni_streams: Some(2),
			..Default::default()
		});
		let session = boxed::Session::new(a);
		let waiter = kio::Waiter::noop();
		let options = |priority| SendOptions { group: None, priority };
		let open = |open: &mut Open, priority| session.poll_open_group(open, &waiter, options(priority), 7);
		let ranks = |session: &boxed::Session| -> Vec<Rank> {
			session.open_streams().into_iter().map(|stream| stream.rank).collect()
		};

		let mut streams: Vec<_> = [2, 1]
			.into_iter()
			.map(|priority| {
				let Poll::Ready(Ok(mut stream)) = open(&mut Open::default(), priority) else {
					panic!("credit is free");
				};
				assert!(stream.poll_write(&waiter, b"group").is_ready());
				stream
			})
			.collect();
		let mut lowest = streams.pop().unwrap();

		let mut below = Open::default();
		assert!(open(&mut below, 0).is_pending());
		assert_eq!(ranks(&session).len(), 2, "nothing ranks below the waiting open");
		drop(below);

		let mut high = Open::default();
		assert!(open(&mut high, 5).is_pending());
		assert_eq!(ranks(&session), [Rank::Ungrouped { stream: 2 }]);
		let Poll::Ready(Err(err)) = lowest.poll_write(&waiter, b"more") else {
			panic!("the reset stream fails its writer");
		};
		assert_eq!(err.stream_error(), Some(7));
		assert!(open(&mut high, 5).is_pending());
		assert_eq!(ranks(&session).len(), 1, "one reset in flight per waiting open");

		let mut next = Open::default();
		assert!(open(&mut next, 4).is_pending());
		assert!(ranks(&session).is_empty(), "the next waiting open resets another");

		// The peer is done with both reset streams, returning their credit.
		for _ in 0..2 {
			let Poll::Ready(Ok(mut recv)) = b.poll_accept_uni(&waiter) else {
				panic!("the peer accepts the stream");
			};
			while let Poll::Ready(Ok(Some(_))) = recv.poll_read_chunk(&waiter, 16) {}
		}
		drop((lowest, streams));
		assert!(matches!(open(&mut high, 5), Poll::Ready(Ok(_))));
		assert!(matches!(open(&mut next, 4), Poll::Ready(Ok(_))));
	}

	/// Ranked as the transport sends: outside every group before any group, then by priority.
	#[test]
	fn ungrouped_streams_outrank_every_group() {
		let ungrouped = Rank::Ungrouped { stream: i64::MIN };
		let grouped = Rank::Grouped {
			group: i64::MAX,
			stream: i64::MAX,
		};
		assert!(ungrouped > grouped);
		assert!(
			Rank::Grouped { group: 2, stream: 0 }
				> Rank::Grouped {
					group: 1,
					stream: i64::MAX
				}
		);
	}
}
