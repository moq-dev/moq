//! A track's groups and datagrams in sequence order, waiting a bounded time for a late one.
//!
//! `moq_net::track::Ordered` seeks past a missing group at once and orders groups alone, so it
//! fits a player, not an archive that must keep every group and datagram it can in key order.

use std::collections::BTreeMap;
use std::pin::Pin;
use std::task::Poll;
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
/// the next, otherwise after `max_delay` from its arrival, or from the first arrival while no
/// start is known. Once both channels end, everything received is handed out without waiting.
pub(super) struct Ordered {
	subscriber: Subscriber,
	max_delay: Duration,
	/// The next sequence to hand out; `None` until a start is known or the first wait ends.
	next: Option<u64>,
	/// Received sequences waiting on a lower one, with when they arrived.
	pending: BTreeMap<u64, (Item, Instant)>,
	/// When the first sequence arrived while `next` was unknown, since arrival is not sequence
	/// order and a lower one may still be on its way. Fixed, so ever lower arrivals can't extend
	/// the wait.
	first: Option<Instant>,
	timer: Option<Pin<Box<moq_clock::Sleep>>>,
	groups_done: bool,
	datagrams_done: bool,
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
			first: None,
			timer: None,
			groups_done: false,
			datagrams_done: false,
		}
	}

	/// Take every arrival so far, registering `waiter` for the next.
	pub(super) fn poll_arrivals(&mut self, waiter: &kio::Waiter) {
		let now = moq_clock::now();
		while !self.groups_done {
			match self.subscriber.poll_recv_group(waiter) {
				Poll::Ready(Ok(Some(group))) => self.arrive(group.sequence, Item::Group(Box::new(group)), now),
				// How the track ended is read from `poll_finished`.
				Poll::Ready(_) => self.groups_done = true,
				Poll::Pending => break,
			}
		}
		while !self.datagrams_done {
			match self.subscriber.poll_recv_datagram(waiter) {
				Poll::Ready(Ok(Some(datagram))) => self.arrive(datagram.sequence, Item::Datagram(datagram), now),
				Poll::Ready(_) => self.datagrams_done = true,
				Poll::Pending => break,
			}
		}
	}

	fn arrive(&mut self, sequence: u64, item: Item, now: Instant) {
		// A sequence names one group, so a copy of one already here or handed out is dropped.
		if self.next.is_none_or(|next| sequence >= next) {
			self.pending.entry(sequence).or_insert((item, now));
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
			let arrived = entry.get().1;
			let waiting = match self.next {
				Some(next) if sequence < next => {
					entry.remove();
					continue;
				}
				Some(next) => (sequence > next).then_some(arrived),
				// Nothing is below 0.
				None => (sequence > 0).then(|| *self.first.get_or_insert(arrived)),
			};
			if let Some(waiting) = waiting
				&& !draining
			{
				let deadline = waiting + self.max_delay;
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
			let (item, _) = entry.remove();
			self.next = Some(sequence.saturating_add(1));
			return Poll::Ready(Some((sequence, item)));
		}
	}

	/// How the track ended, once it has.
	pub(super) fn poll_finished(&mut self, waiter: &kio::Waiter) -> Poll<moq_net::Result<Position>> {
		self.subscriber.poll_finished(waiter)
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

	/// The next sequence is handed out at once, a datagram in turn with the groups, and one past a
	/// missing sequence only once the max delay from its arrival passes.
	#[tokio::test(start_paused = true)]
	async fn only_a_missing_sequence_waits_the_max_delay() {
		let broadcast = broadcast::Info::new().produce();
		let mut track = broadcast.create_track("t", track::Info::default()).unwrap();
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
		let mut ordered = Ordered::new(subscriber, MAX_DELAY, None);

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
}
