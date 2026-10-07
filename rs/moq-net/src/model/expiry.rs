//! Wakes for group reads parked on their subscription's drift budget.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::Timestamp;

/// A track's parked group reads, indexed by the events that can expire them, so a frame
/// write or a group insert wakes only the reads whose verdict it could change.
///
/// A parked read goes stale once the live edge reaches its deadline: where its immediate
/// successor starts, plus its budget. The edge only ever takes a timestamp some frame
/// write presented, so the write that crosses a deadline wakes the reads behind it and
/// nobody else. The successor itself is watched per group (its first frame, its abort),
/// except for a group landing between the read and it, which reads park on by sequence.
///
/// Waking every parked read on every track change instead costs a backlog of N parked
/// reads N wakes per append: an audio backlog of ~800 one-frame groups pins a runtime.
pub(crate) struct Wakes {
	/// The earliest parked deadline in floored micros, or `u64::MAX`, so a write that
	/// crosses nothing skips the lock.
	///
	/// Relaxed: a read registers before resolving the edge, under the group locks a
	/// write takes before it presents, so a write the read missed sees its deadline.
	earliest: AtomicU64,
	parked: kio::Lock<Parked>,
}

#[derive(Default)]
struct Parked {
	/// Reads by deadline, in floored micros: never later than the exact deadline, so a
	/// write that crosses one is never skipped.
	deadlines: BTreeMap<u64, kio::WaiterList>,
	/// Reads by their group's sequence, woken when a group lands above them.
	landings: BTreeMap<u64, kio::WaiterList>,
}

impl Default for Wakes {
	fn default() -> Self {
		Self {
			earliest: AtomicU64::new(u64::MAX),
			parked: kio::Lock::default(),
		}
	}
}

impl Wakes {
	/// A frame write left its group presenting up to `latest`: wake the reads it expires.
	pub(crate) fn presented(&self, latest: Timestamp) {
		let at = micros(latest.as_micros());
		if at < self.earliest.load(Ordering::Relaxed) {
			return;
		}
		let mut woken = Vec::new();
		{
			let mut parked = self.parked.lock();
			while let Some(entry) = parked.deadlines.first_entry()
				&& *entry.key() <= at
			{
				woken.push(entry.remove());
			}
			let earliest = parked.deadlines.first_key_value().map_or(u64::MAX, |(at, _)| *at);
			self.earliest.store(earliest, Ordering::Relaxed);
		}
		woken.iter_mut().for_each(kio::WaiterList::wake);
	}

	/// Park `waiter` until the edge reaches `reach` plus `budget`.
	pub(crate) fn watch_deadline(&self, reach: Timestamp, budget: Duration, waiter: &kio::Waiter) {
		let deadline = micros(reach.as_micros()).saturating_add(micros(budget.as_micros()));
		// No timestamp reaches it.
		if deadline == u64::MAX {
			return;
		}
		let mut parked = self.parked.lock();
		parked.deadlines.entry(deadline).or_default().register(waiter);
		self.earliest.fetch_min(deadline, Ordering::Relaxed);
	}

	/// Park `waiter` until a group lands above `sequence`.
	pub(crate) fn watch_landing(&self, sequence: u64, waiter: &kio::Waiter) {
		self.parked
			.lock()
			.landings
			.entry(sequence)
			.or_default()
			.register(waiter);
	}

	/// A servable group landed at `sequence`, so it succeeds every read from `below`, the
	/// nearest servable group beneath it, up. Reads below `oldest`, the first group still
	/// cached, can no longer expire and are forgotten.
	pub(crate) fn landed(&self, below: Option<u64>, sequence: u64, oldest: u64) {
		let mut woken = Vec::new();
		{
			let mut parked = self.parked.lock();
			while let Some(entry) = parked.landings.first_entry()
				&& *entry.key() < oldest
			{
				entry.remove();
			}
			while let Some(at) = parked
				.landings
				.range(below.unwrap_or(0)..sequence)
				.next()
				.map(|(at, _)| *at)
			{
				woken.extend(parked.landings.remove(&at));
			}
		}
		woken.iter_mut().for_each(kio::WaiterList::wake);
	}

	/// Wake every parked read, for a change that moves every budget at once.
	pub(crate) fn wake_all(&self) {
		let parked = {
			let mut parked = self.parked.lock();
			self.earliest.store(u64::MAX, Ordering::Relaxed);
			std::mem::take(&mut *parked)
		};
		for mut list in parked.deadlines.into_values().chain(parked.landings.into_values()) {
			list.wake();
		}
	}
}

/// Saturate a microsecond count into a deadline key.
fn micros(value: u128) -> u64 {
	value.try_into().unwrap_or(u64::MAX)
}
