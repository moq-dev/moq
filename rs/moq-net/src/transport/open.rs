//! The one entry point every stream a session opens goes through.
//!
//! A peer grants a bounded number of concurrent streams in each direction. Once that credit
//! runs out, every open waits for the peer to grant more, and the transport hands whatever it
//! grants to whichever waiting open polls first. Here the opens of one session queue instead,
//! ranked exactly as the transport will send their streams, and only the first in line asks
//! the transport for credit, so returning credit goes to the most important open.
//!
//! A transport's open waits only for the peer's credit, writing anything it puts in front of a
//! stream, such as a WebTransport header, as the stream's first data. So a pending head of the
//! line means the peer's credit has run out.
//!
//! The session also lists the group streams it has open, each with its rank and whether the
//! peer has acknowledged everything written to it. A unidirectional open waiting for the peer's
//! credit resets the lowest-ranked one it preempts to make room: one in a lower-priority group,
//! or an older one of its own group, never a tied group's. At most one reset is in flight per
//! waiting open, since each frees credit only once the peer's MAX_STREAMS arrives.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::task::Poll;

use super::boxed;
use super::poll::SendOptions;

/// Which streams a session opens: one-way, or both ways.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Dir {
	/// Unidirectional streams.
	Uni,
	/// Bidirectional streams.
	Bi,
}

/// Where an open or an open stream ranks: the order the transport sends its stream in.
///
/// Mirrors the transport's send scheduling: streams outside every group go before all of
/// them, then a higher group priority, then a higher priority within the group. Groups of
/// equal priority take turns, as the transport's round robin shares bytes between them: the
/// one that took credit longest ago goes first. A group's stream priority orders only its own
/// streams, so a subscription whose groups are numbered higher, say from a clock, never
/// starves another of the same priority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Rank {
	Grouped {
		group: i64,
		turn: Reverse<u64>,
		stream: i64,
	},
	Ungrouped {
		stream: i64,
	},
}

impl Rank {
	/// The rank of a stream at `stream` priority, in a group at this priority and turn if any.
	pub(super) fn new(group: Option<(i64, u64)>, stream: i64) -> Self {
		match group {
			Some((group, turn)) => Self::Grouped {
				group,
				turn: Reverse(turn),
				stream,
			},
			None => Self::Ungrouped { stream },
		}
	}

	/// Whether an open at this rank may reset a stream at `other` to make room: one outside
	/// every group or in a higher-priority group, or an older stream of its own group. A tied
	/// group's stream is only behind in turn, so resetting it would just hand the turn back.
	fn preempts(self, other: Self) -> bool {
		match (self, other) {
			(Self::Ungrouped { stream }, Self::Ungrouped { stream: other }) => stream > other,
			(Self::Ungrouped { .. }, Self::Grouped { .. }) => true,
			(Self::Grouped { .. }, Self::Ungrouped { .. }) => false,
			(
				Self::Grouped { group, turn, stream },
				Self::Grouped {
					group: other_group,
					turn: other_turn,
					stream: other,
				},
			) => group > other_group || (group == other_group && turn == other_turn && stream > other),
		}
	}

	fn of(options: &SendOptions<'_, super::boxed::Group>) -> Self {
		Self::new(options.group.map(super::boxed::Group::rank), options.priority)
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

	/// Say that the line starts waiting for the peer's stream credit, and what the session's open
	/// group streams could make room with.
	fn start(lines: &Lines) {
		if !tracing::enabled!(tracing::Level::DEBUG) {
			return;
		}
		let open = lines.open_streams();
		let open_streams = open.len();
		let acknowledged = open.iter().filter(|stream| stream.acknowledged == Some(true)).count();
		let lowest = open.first().map(|stream| stream.rank);
		tracing::debug!(
			open_streams,
			acknowledged,
			?lowest,
			"out of the peer's stream credit: opens wait in priority order"
		);
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
		// Never more resets owed than opens left to owe them to.
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
#[derive(Default)]
pub(crate) struct Lines {
	uni: Mutex<Line>,
	bi: Mutex<Line>,
	listing: Mutex<Listing>,
	/// The last turn handed to a send group, so every group's turn is its own.
	turns: AtomicU64,
}

impl Lines {
	/// A turn after every one handed out before.
	pub(super) fn next_turn(&self) -> u64 {
		self.turns.fetch_add(1, Ordering::Relaxed) + 1
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

	/// Reset the lowest-ranked open group stream each waiting open not yet owed a reset preempts.
	fn make_room(&self, line: &mut Line) {
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
		let mut open: BTreeMap<(Rank, u64), _> = listed
			.into_iter()
			.map(|(key, stream)| ((stream.read().rank, key), stream))
			.collect();
		for rank in owed {
			// An open that cannot preempt the lowest stream finds no lower-priority group open,
			// which would rank lower, so only its own group's oldest stream is left to check.
			let candidate = match (open.first_key_value(), rank) {
				(Some((&(lowest, key), _)), _) if rank.preempts(lowest) => (lowest, key),
				(Some(_), Rank::Grouped { group, turn, .. }) => {
					let first = Rank::Grouped {
						group,
						turn,
						stream: i64::MIN,
					};
					match open.range((first, 0)..).next() {
						Some((&(oldest, key), _)) if rank.preempts(oldest) => (oldest, key),
						_ => continue,
					}
				}
				_ => continue,
			};
			let ((lowest, key), stream) = open.remove_entry(&candidate).expect("just found");
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
					return Poll::Ready(took_turn(res, lines, options));
				}
				Line::start(lines);
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
		// Whichever open the credit reaches, a reset's credit may be the one it took.
		if res.is_ok() {
			line.resets = line.resets.saturating_sub(1);
		}
		// Whoever is next may find credit left over, or the same failure.
		line.leave(key);
		Poll::Ready(took_turn(res, lines, options))
	}
}

/// Move an opened stream's group to the back of its priority's turns.
fn took_turn<T, E>(res: Result<T, E>, lines: &Lines, options: &SendOptions<'_, super::boxed::Group>) -> Result<T, E> {
	if let (Ok(_), Some(group)) = (&res, options.group) {
		group.take_turn(lines.next_turn());
	}
	res
}

impl Drop for Open {
	fn drop(&mut self) {
		if let Some((lines, dir, key)) = self.place.take() {
			lines.line(dir).leave(key);
		}
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

	/// A reset's credit taken by a later, higher open leaves the open that reset still owed
	/// one, so the next open to arrive can make room again.
	#[test]
	fn credit_taken_by_another_open_is_owed_again() {
		use super::super::{memory, poll::RecvStream as _, poll::Session as _};

		let (a, b) = memory::pair(memory::Config {
			max_concurrent_uni_streams: Some(1),
			..Default::default()
		});
		let session = boxed::Session::new(a);
		let waiter = kio::Waiter::noop();
		let open = |open: &mut Open, priority| {
			session.poll_open_group(open, &waiter, SendOptions { group: None, priority }, 7)
		};
		let drain = || {
			let Poll::Ready(Ok(mut recv)) = b.poll_accept_uni(&waiter) else {
				panic!("the peer accepts the stream");
			};
			while let Poll::Ready(Ok(Some(_))) = recv.poll_read_chunk(&waiter, 16) {}
		};

		let Poll::Ready(Ok(low)) = open(&mut Open::default(), 1) else {
			panic!("credit is free");
		};
		let mut first = Open::default();
		assert!(open(&mut first, 5).is_pending());
		assert!(session.open_streams().is_empty());
		drop(low);
		drain();

		let Poll::Ready(Ok(_taken)) = open(&mut Open::default(), 6) else {
			panic!("the higher open takes the returned credit");
		};
		let mut last = Open::default();
		assert!(open(&mut last, 7).is_pending());
		assert!(
			session.open_streams().is_empty(),
			"the open that reset is owed credit again"
		);
		drop(first);
	}

	/// Ranked as the transport sends: outside every group before any group, then by priority.
	#[test]
	fn ungrouped_streams_outrank_every_group() {
		let ungrouped = Rank::Ungrouped { stream: i64::MIN };
		let grouped = Rank::new(Some((i64::MAX, 0)), i64::MAX);
		assert!(ungrouped > grouped);
		assert!(Rank::new(Some((2, 9)), 0) > Rank::new(Some((1, 0)), i64::MAX));
		assert!(Rank::new(Some((1, 1)), 0) > Rank::new(Some((1, 2)), i64::MAX));
	}

	/// Groups of one priority take turns at credit, whatever their streams' priorities, so a
	/// subscription whose groups are numbered higher, say from a clock, never starves another.
	#[test]
	fn tied_groups_take_turns() {
		use super::super::poll::Session as _;

		let session = boxed::Session::new(crate::lite::test_transport::SinkSession::default().with_groups());
		let lines = &session.opens;
		let credit = Mutex::new(Credit::default());
		// Live subscriptions in their own groups, numbering their groups far apart.
		let groups = [("early", 10), ("late", 1_000_000)]
			.map(|(name, key)| (name, key, session.send_group(0).expect("the sink opens groups")));
		let waiter = kio::Waiter::noop();
		let mut order = Vec::new();
		for round in 0..4 {
			// Each round's group replaces the one still waiting, as a zero delay budget gives
			// up a group once its successor exists.
			let mut opens: Vec<_> = groups
				.iter()
				.map(|(name, key, group)| {
					let options = SendOptions {
						group: Some(group),
						priority: key + round,
					};
					(*name, options, Open::default())
				})
				.collect();
			let poll = |open: &mut Open, options: &SendOptions<'_, boxed::Group>| {
				open.poll(lines, Dir::Uni, &waiter, options, |waiter| {
					let mut credit = credit.lock().unwrap();
					if credit.available == 0 {
						waiter.register(&mut credit.waiters);
						return Poll::Pending;
					}
					credit.available -= 1;
					Poll::Ready(Ok::<(), ()>(()))
				})
				.is_ready()
			};
			for (_, options, open) in &mut opens {
				assert!(!poll(open, options));
			}
			credit.lock().unwrap().grant(1);
			// Polled highest stream priority first, so only the line's order can pick `early`.
			for (name, options, open) in opens.iter_mut().rev() {
				if poll(open, options) {
					order.push(*name);
				}
			}
		}
		assert_eq!(order, ["early", "late", "early", "late"]);
	}

	/// Out of credit, a waiting open resets a stream only of a lower group priority or of its
	/// own group, never a tied group's, so tied subscriptions take turns instead of resetting
	/// each other's streams.
	#[test]
	fn tied_groups_never_reset_each_other() {
		use super::super::poll::Session as _;

		let credit = kio::Producer::new(true);
		let session = boxed::Session::new(
			crate::lite::test_transport::SinkSession::gated_open_uni(credit.consume()).with_groups(),
		);
		let waiter = kio::Waiter::noop();
		let [served, tied, high] =
			[0, 0, 1].map(|priority| session.send_group(priority).expect("the sink opens groups"));
		let grant = |open: bool| {
			let Ok(mut credit) = credit.write() else {
				panic!("the gate stays open");
			};
			*credit = open;
		};
		let open = |open: &mut Open, group: &boxed::Group, stream| {
			let options = SendOptions {
				group: Some(group),
				priority: stream,
			};
			session.poll_open_group(open, &waiter, options, 7)
		};
		let Poll::Ready(Ok(_older)) = open(&mut Open::default(), &served, 1) else {
			panic!("credit is free");
		};
		grant(false);

		let mut waiting = Open::default();
		assert!(open(&mut waiting, &tied, 1).is_pending());
		assert_eq!(session.open_streams().len(), 1, "a tied group waits its turn");

		let mut newer = Open::default();
		assert!(open(&mut newer, &served, 2).is_pending());
		assert!(
			session.open_streams().is_empty(),
			"a group's newer stream resets its older one"
		);
		drop((waiting, newer));

		grant(true);
		let Poll::Ready(Ok(_other)) = open(&mut Open::default(), &tied, 0) else {
			panic!("credit is free");
		};
		grant(false);
		let mut higher = Open::default();
		assert!(open(&mut higher, &high, 0).is_pending());
		assert!(
			session.open_streams().is_empty(),
			"a higher group priority resets a lower"
		);
		drop(higher);
	}
}
