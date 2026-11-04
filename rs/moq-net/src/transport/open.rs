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

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::task::Poll;

use super::boxed::{self, Transport};
use super::poll::SendOptions;
use super::{Dir, StreamCredit};

/// Where an open ranks for credit: the order the transport will send its stream in.
///
/// Mirrors the transport's send scheduling: streams outside every group go before all of
/// them, then a higher group priority, then a higher priority within the group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
	Grouped { group: i64, stream: i64 },
	Ungrouped { stream: i64 },
}

impl Rank {
	fn of(options: &SendOptions<'_, super::boxed::Group>) -> Self {
		match options.group {
			Some(group) => Self::Grouped {
				group: group.priority(),
				stream: options.priority,
			},
			None => Self::Ungrouped {
				stream: options.priority,
			},
		}
	}
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
}

impl Line {
	fn head(&self) -> Option<Key> {
		self.waiting.keys().next().copied()
	}

	/// Say why the line starts waiting: the peer's stream credit, unless the transport reports
	/// otherwise.
	fn start(credit: Option<StreamCredit>) {
		match credit {
			Some(credit) if !credit.blocked => {
				tracing::debug!(available = credit.available, "opens wait in priority order for the transport")
			}
			_ => tracing::debug!("out of the peer's stream credit: opens wait in priority order"),
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

/// The lines of one session's opens, shared by every task opening on it.
pub(crate) struct Lines {
	uni: Mutex<Line>,
	bi: Mutex<Line>,
	/// Whose stream credit the opens wait for.
	pub(super) transport: Transport,
}

impl Lines {
	pub(super) fn new(transport: Transport) -> Self {
		Self {
			uni: Default::default(),
			bi: Default::default(),
			transport,
		}
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
				Line::start(boxed::stream_credit(&lines.transport, dir));
				let key = line.join(rank);
				line.waiting.get_mut(&key).expect("just joined").register(waiter);
				self.place = Some((lines.clone(), dir, key));
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
			return Poll::Pending;
		}
		let res = std::task::ready!(open(waiter));
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
		assert_eq!(boxed::stream_credit(&transport, Dir::Uni), Some(StreamCredit::new(0, true)));

		drop((second, session));
		assert_eq!(boxed::stream_credit(&transport, Dir::Uni), None);
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
