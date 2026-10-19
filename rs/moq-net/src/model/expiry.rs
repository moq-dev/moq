//! Indexed wakes for group expiry, cache arrivals, and groups held across a route switch.
//!
//! The budget is judged on two clocks (see `TrackState::drifted`): media time, which only a
//! frame write advances, and the wall clock, which the cache pool's driver advances.

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
/// A parked read also goes stale once the wall clock reaches its successor's arrival plus
/// its budget. Nothing written moves that clock, so those reads park by instant and the
/// pool's driver releases them; see `cache::Track::watch_wall`.
///
/// A group held across a route switch parks in the serving copy's index the same way,
/// although that copy need not hold it; see `track::Consumer::poll_stale`.
///
/// Waking every parked read on every track change instead costs a backlog of N parked
/// reads N wakes per append: an audio backlog of ~800 one-frame groups pins a runtime.
pub(super) struct Wakes {
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
	/// `landings` for groups held from another copy, also woken by their own group
	/// landing here. Kept apart because this track may never have held them, so falling
	/// below its oldest group does not end them; their readers leaving does.
	held: BTreeMap<u64, kio::WaiterList>,
	/// The `held` size that sweeps out the lists every reader has left.
	held_sweep: usize,
	/// Exact cache arrivals, independent of the live edge and neighbouring groups.
	arrivals: BTreeMap<u64, kio::WaiterList>,
	arrival_sweep: usize,
	/// Reads by wall-clock deadline, released by the pool's driver.
	wall: BTreeMap<crate::time::Instant, kio::WaiterList>,
	/// The earliest `wall` deadline the pool was asked to release, so arming a later one
	/// skips the pool's lock.
	wall_armed: Option<crate::time::Instant>,
}

/// The smallest `held` size worth sweeping.
const HELD_SWEEP_MIN: usize = 16;

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
	pub(super) fn presented(&self, latest: Timestamp) {
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
	pub(super) fn watch_deadline(&self, reach: Timestamp, budget: Duration, waiter: &kio::Waiter) {
		let deadline = micros(reach.as_micros()).saturating_add(micros(budget.as_micros()));
		// No timestamp reaches it.
		if deadline == u64::MAX {
			return;
		}
		let mut parked = self.parked.lock();
		parked.deadlines.entry(deadline).or_default().register(waiter);
		self.earliest.fetch_min(deadline, Ordering::Relaxed);
	}

	/// Park `waiter` until the wall clock reaches `at`. When `at` is earlier than any
	/// deadline the pool already releases for this track, so the caller asks it to, returns
	/// `Some` of the one it replaces.
	pub(super) fn watch_wall(
		&self,
		at: crate::time::Instant,
		waiter: &kio::Waiter,
	) -> Option<Option<crate::time::Instant>> {
		let mut parked = self.parked.lock();
		parked.wall.entry(at).or_default().register(waiter);
		if parked.wall_armed.is_some_and(|armed| armed <= at) {
			return None;
		}
		Some(parked.wall_armed.replace(at))
	}

	/// The wall-clock deadline the pool releases for this track next.
	pub(super) fn wall_armed(&self) -> Option<crate::time::Instant> {
		self.parked.lock().wall_armed
	}

	/// The wall clock reached `now`: wake the reads due by then, returning the earliest
	/// deadline left for the pool to release next.
	pub(super) fn release_wall(&self, now: crate::time::Instant) -> Option<crate::time::Instant> {
		let mut woken = Vec::new();
		let next = {
			let mut parked = self.parked.lock();
			while let Some(entry) = parked.wall.first_entry()
				&& *entry.key() <= now
			{
				woken.push(entry.remove());
			}
			parked.wall_armed = parked.wall.first_key_value().map(|(at, _)| *at);
			parked.wall_armed
		};
		woken.iter_mut().for_each(kio::WaiterList::wake);
		next
	}

	/// Park `waiter` until a group lands above `sequence`.
	pub(super) fn watch_landing(&self, sequence: u64, waiter: &kio::Waiter) {
		self.parked
			.lock()
			.landings
			.entry(sequence)
			.or_default()
			.register(waiter);
	}

	/// Park `waiter`, holding group `sequence` from another copy, until it or a group
	/// above it lands.
	pub(super) fn watch_held(&self, sequence: u64, waiter: &kio::Waiter) {
		let mut parked = self.parked.lock();
		let parked = &mut *parked;
		// Sweep only once the map has doubled, so each sweep is paid for by the keys
		// added before it: a switch holding N groups costs O(N), not O(N²). Here rather
		// than in `landed`, which every insert takes.
		if parked.held.len() >= parked.held_sweep {
			parked.held.retain(|_, list| !list.is_empty());
			parked.held_sweep = (parked.held.len() * 2).max(HELD_SWEEP_MIN);
		}
		parked.held.entry(sequence).or_default().register(waiter);
	}

	/// Park until this exact group is cached, or a declared boundary excludes it.
	pub(super) fn watch_arrival(&self, sequence: u64, waiter: &kio::Waiter) {
		let mut parked = self.parked.lock();
		if parked.arrivals.len() >= parked.arrival_sweep {
			parked.arrivals.retain(|_, list| !list.is_empty());
			parked.arrival_sweep = (parked.arrivals.len() * 2).max(HELD_SWEEP_MIN);
		}
		parked.arrivals.entry(sequence).or_default().register(waiter);
	}

	/// Wake only cache arrivals whose outcome this insertion or boundary changes.
	pub(super) fn arrived(&self, groups: impl std::ops::RangeBounds<u64>) {
		let groups = (groups.start_bound().cloned(), groups.end_bound().cloned());
		let mut woken = Vec::new();
		{
			let mut parked = self.parked.lock();
			while let Some(at) = parked.arrivals.range(groups).next().map(|(at, _)| *at) {
				woken.extend(parked.arrivals.remove(&at));
			}
		}
		woken.iter_mut().for_each(kio::WaiterList::wake);
	}

	/// A servable group landed at `sequence`, so it succeeds every read from `below`, the
	/// nearest servable group beneath it, up, and continues a held group of its own
	/// sequence. Reads below `oldest`, the first group still cached, can no longer expire
	/// and are forgotten; held groups are not.
	pub(super) fn landed(&self, below: Option<u64>, sequence: u64, oldest: u64) {
		let mut woken = Vec::new();
		{
			let mut parked = self.parked.lock();
			let parked = &mut *parked;
			while let Some(entry) = parked.landings.first_entry()
				&& *entry.key() < oldest
			{
				entry.remove();
			}
			let from = below.unwrap_or(0);
			while let Some(at) = parked.landings.range(from..sequence).next().map(|(at, _)| *at) {
				woken.extend(parked.landings.remove(&at));
			}
			while let Some(at) = parked.held.range(from..=sequence).next().map(|(at, _)| *at) {
				woken.extend(parked.held.remove(&at));
			}
		}
		woken.iter_mut().for_each(kio::WaiterList::wake);
	}

	/// Wake every parked read, for a change that moves every budget at once.
	pub(super) fn wake_all(&self) {
		let parked = {
			let mut parked = self.parked.lock();
			self.earliest.store(u64::MAX, Ordering::Relaxed);
			std::mem::take(&mut *parked)
		};
		let lists = parked.deadlines.into_values().chain(parked.landings.into_values());
		let lists = lists
			.chain(parked.held.into_values())
			.chain(parked.wall.into_values())
			.chain(parked.arrivals.into_values());
		for mut list in lists {
			list.wake();
		}
	}
}

/// Saturate a microsecond count into a deadline key.
fn micros(value: u128) -> u64 {
	value.try_into().unwrap_or(u64::MAX)
}

#[cfg(test)]
mod test {
	use super::*;

	/// Each route switch parks the groups then in flight; once their readers drop, the
	/// next switch's park must not find them still indexed.
	#[test]
	fn held_groups_do_not_accumulate_across_route_switches() {
		let wakes = Wakes::default();
		let mut reader = None;
		for sequence in 0..100 {
			// The last switch's reader gave its group up.
			drop(reader.take());
			let waiter = kio::Waiter::noop();
			wakes.watch_held(sequence, &waiter);
			reader = Some(waiter);
		}
		assert!(
			wakes.parked.lock().held.len() <= HELD_SWEEP_MIN,
			"the readers that left are swept"
		);
	}
}
