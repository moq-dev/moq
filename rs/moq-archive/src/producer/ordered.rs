//! A track's groups and datagrams in sequence order, waiting a bounded time for a late one.
//!
//! `moq_net::track::Ordered` seeks past a missing group at once and orders groups alone, so it
//! fits a player, not an archive that must keep every group and datagram it can in key order.

use std::collections::{BTreeMap, VecDeque};
use std::pin::Pin;
use std::task::{Poll, ready};
use std::time::Duration;

use moq_clock::Instant;
use moq_net::track::{Position, Subscriber};
use moq_net::{Datagram, group};

/// One sequence, in the form it was forwarded in.
pub(super) enum Item {
	// Boxed, since a group consumer dwarfs a datagram and waits in `Ordered::pending`.
	Group(Box<group::Consumer>),
	Datagram(Datagram),
}

/// Hands out each sequence once every lower one was handed out or given up: at once when it is
/// the next, otherwise once `max_delay` passes from the oldest arrival still waiting, which is
/// when the gap below it was first seen. While no start is known, anything but 0 waits the same
/// way, since arrival is not sequence order. Once both channels end, everything received is
/// handed out without waiting.
pub(super) struct Ordered {
	subscriber: Subscriber,
	max_delay: Duration,
	/// The next sequence to hand out; `None` until a start is known or the first wait ends.
	next: Option<u64>,
	/// Received sequences waiting on a lower one.
	pending: BTreeMap<u64, Item>,
	/// Each pending sequence with when it arrived, oldest first. The front is always pending,
	/// and the ones handed out behind it go once it does, so this holds at most what arrived
	/// within a max delay. The oldest dates the gap, so a later, lower arrival never extends it.
	arrivals: VecDeque<(u64, Instant)>,
	timer: Option<Pin<Box<moq_clock::Sleep>>>,
	groups_done: bool,
	datagrams_done: bool,
	/// The first error a channel ended with: an end declared with an error, or an abort.
	error: Option<moq_net::Error>,
}

impl Ordered {
	/// Order `subscriber` from `next`, where a resume continues; otherwise from the subscription's
	/// floor when it names one.
	pub(super) fn new(subscriber: Subscriber, max_delay: Duration, next: Option<u64>) -> Self {
		let next = next.or_else(|| subscriber.floor());
		Self {
			subscriber,
			max_delay,
			next,
			pending: BTreeMap::new(),
			arrivals: VecDeque::new(),
			timer: None,
			groups_done: false,
			datagrams_done: false,
			error: None,
		}
	}

	/// Take every arrival so far, registering `waiter` for the next.
	pub(super) fn poll_arrivals(&mut self, waiter: &kio::Waiter) {
		let now = moq_clock::now();
		while !self.groups_done {
			match self.subscriber.poll_recv_group(waiter) {
				Poll::Ready(Ok(Some(group))) => self.arrive(group.sequence, Item::Group(Box::new(group)), now),
				// How the track ended is read from `poll_finished`.
				Poll::Ready(Ok(None)) => self.groups_done = true,
				Poll::Ready(Err(err)) => {
					self.error.get_or_insert(err);
					self.groups_done = true;
				}
				Poll::Pending => break,
			}
		}
		while !self.datagrams_done {
			match self.subscriber.poll_recv_datagram(waiter) {
				Poll::Ready(Ok(Some(datagram))) => self.arrive(datagram.sequence, Item::Datagram(datagram), now),
				Poll::Ready(Ok(None)) => self.datagrams_done = true,
				Poll::Ready(Err(err)) => {
					self.error.get_or_insert(err);
					self.datagrams_done = true;
				}
				Poll::Pending => break,
			}
		}
	}

	fn arrive(&mut self, sequence: u64, item: Item, now: Instant) {
		// A sequence names one group, so a copy of one already here or handed out is dropped.
		if self.next.is_none_or(|next| sequence >= next)
			&& let std::collections::btree_map::Entry::Vacant(entry) = self.pending.entry(sequence)
		{
			entry.insert(item);
			self.arrivals.push_back((sequence, now));
		}
	}

	/// The next sequence in order, or `None` once both channels ended and nothing is left.
	pub(super) fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Option<(u64, Item)>> {
		self.poll_arrivals(waiter);
		let draining = self.groups_done && self.datagrams_done;
		loop {
			let Some(entry) = self.pending.first_entry() else {
				return match draining {
					true => Poll::Ready(None),
					false => Poll::Pending,
				};
			};
			let sequence = *entry.key();
			let gap = match self.next {
				Some(next) if sequence < next => {
					entry.remove();
					self.trim();
					continue;
				}
				Some(next) => sequence > next,
				// Nothing is below 0.
				None => sequence > 0,
			};
			if gap && !draining {
				// The oldest arrival still pending is when this gap was first seen.
				let (_, since) = self.arrivals.front().expect("a pending sequence arrived");
				let deadline = *since + self.max_delay;
				if moq_clock::now() < deadline {
					let timer = self
						.timer
						.get_or_insert_with(|| Box::pin(moq_clock::sleep_until(deadline)));
					timer.as_mut().reset(deadline);
					if waiter.poll_future(timer.as_mut()).is_pending() {
						return Poll::Pending;
					}
				}
			}
			let item = self.pending.remove(&sequence).expect("the lowest pending sequence");
			self.trim();
			self.next = Some(sequence.saturating_add(1));
			return Poll::Ready(Some((sequence, item)));
		}
	}

	/// Drop arrivals handed out, up to the oldest one still pending.
	fn trim(&mut self) {
		while self
			.arrivals
			.front()
			.is_some_and(|(sequence, _)| !self.pending.contains_key(sequence))
		{
			self.arrivals.pop_front();
		}
	}

	/// How the track ended, once it has: the error a channel ended with, which an end declared
	/// with an error reads as while keeping its history, or else its declared end.
	pub(super) fn poll_finished(&mut self, waiter: &kio::Waiter) -> Poll<moq_net::Result<Position>> {
		let end = ready!(self.subscriber.poll_finished(waiter))?;
		Poll::Ready(match self.error.clone() {
			Some(err) => Err(err),
			None => Ok(end),
		})
	}
}

#[cfg(test)]
mod tests {
	use moq_net::track::{Order, Range, Subscription};
	use moq_net::{broadcast, track};

	use super::*;

	const MAX_DELAY: Duration = Duration::from_secs(10);

	fn group(track: &track::Producer, sequence: u64) {
		let mut group = track.create_group(group::Info { sequence }).unwrap();
		group.write_frame(None, "x").unwrap();
		group.finish().unwrap();
	}

	fn next(ordered: &mut Ordered) -> Option<u64> {
		match ordered.poll_next(&kio::Waiter::noop()) {
			Poll::Ready(Some((sequence, _))) => Some(sequence),
			_ => None,
		}
	}

	async fn ordered(broadcast: &broadcast::Producer, next: Option<u64>) -> Ordered {
		let subscription = Subscription::default()
			.with_order(Order::Ascending)
			.with_max_delay(MAX_DELAY)
			.with_ranges([Range::oldest()]);
		let subscriber = broadcast
			.consume()
			.track("t")
			.unwrap()
			.subscribe(subscription)
			.await
			.unwrap();
		Ordered::new(subscriber, MAX_DELAY, next)
	}

	/// The next sequence is handed out at once, a datagram in turn with the groups, and one past a
	/// missing sequence only once the max delay from its arrival passes.
	#[tokio::test(start_paused = true)]
	async fn only_a_missing_sequence_waits_the_max_delay() {
		let broadcast = broadcast::Info::new().produce();
		let mut track = broadcast.create_track("t", track::Info::default()).unwrap();
		let mut ordered = ordered(&broadcast, None).await;

		group(&track, 0);
		assert_eq!(next(&mut ordered), Some(0));

		group(&track, 2);
		assert_eq!(next(&mut ordered), None);
		track.insert_datagram(1, None, "d").unwrap();
		assert_eq!(next(&mut ordered), Some(1));
		assert_eq!(next(&mut ordered), Some(2));

		group(&track, 4);
		assert_eq!(next(&mut ordered), None);
		moq_clock::sleep(MAX_DELAY - Duration::from_millis(1)).await;
		assert_eq!(next(&mut ordered), None);
		moq_clock::sleep(Duration::from_millis(1)).await;
		assert_eq!(next(&mut ordered), Some(4));

		// Too late for its turn.
		group(&track, 3);
		track.finish().unwrap();
		assert!(matches!(ordered.poll_next(&kio::Waiter::noop()), Poll::Ready(None)));
	}

	/// A gap's wait runs from when it was first seen, so ever lower arrivals above it can't extend it.
	#[tokio::test(start_paused = true)]
	async fn lower_arrivals_never_extend_a_gap() {
		let broadcast = broadcast::Info::new().produce();
		let track = broadcast.create_track("t", track::Info::default()).unwrap();
		let mut ordered = ordered(&broadcast, Some(0)).await;

		group(&track, 10);
		assert_eq!(next(&mut ordered), None);
		moq_clock::sleep(MAX_DELAY - Duration::from_millis(1)).await;
		group(&track, 9);
		assert_eq!(next(&mut ordered), None);
		moq_clock::sleep(Duration::from_millis(1)).await;
		assert_eq!(next(&mut ordered), Some(9));
		assert_eq!(next(&mut ordered), Some(10));
	}

	/// A gap-free track keeps no arrival it handed out.
	#[tokio::test(start_paused = true)]
	async fn handed_out_arrivals_are_dropped() {
		let broadcast = broadcast::Info::new().produce();
		let track = broadcast.create_track("t", track::Info::default()).unwrap();
		let mut ordered = ordered(&broadcast, None).await;
		for sequence in 0..100 {
			group(&track, sequence);
			assert_eq!(next(&mut ordered), Some(sequence));
		}
		assert!(ordered.arrivals.is_empty());
	}
}
