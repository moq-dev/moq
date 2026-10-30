//! Indexed wakes for group expiry, cache arrivals, and groups held across a route switch.
//!
//! The budget is judged on two clocks (see `TrackState::drifted`): media time, which only a
//! frame write advances, and the wall clock, which the cache pool's driver advances.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::time::Duration;

#[cfg(loom)]
use ::loom::sync::atomic::{AtomicBool, AtomicU64, fence};
#[cfg(not(loom))]
use std::sync::atomic::{AtomicBool, AtomicU64, fence};

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
	/// The lowest sequence a parked time end watches for a start, or `u64::MAX`, so a first
	/// frame below every one skips the lock. Ordered by the starts index's lock, which a
	/// read takes after registering and a first frame before checking this.
	starts_from: AtomicU64,
	/// Whether either landing index has entries. Insertion checks this after raising
	/// `newest`, and a landing read sets it before reading `newest`, each across a SeqCst
	/// fence: whichever goes second sees the other, so a read registering without the
	/// track lock is either woken or sees the group. Held reads register under the track
	/// read lock, which orders them against insertion too. First-frame reveal holds only
	/// a track read lock and must always take the parked lock instead.
	landing_waiters: AtomicBool,
	/// The highest sequence a servable group ever landed at, or 0, so a read with nothing
	/// above it skips the track lock. Nothing landing and group 0 landing put no group
	/// above any read, so 0 serves both. Never lowered: a group that later fails or leaves
	/// only sends the read the slow way.
	newest: AtomicU64,
	parked: kio::Lock<Parked>,
}

#[derive(Default)]
struct Parked {
	/// Reads by deadline, in floored micros: never later than the exact deadline, so a
	/// write that crosses one is never skipped.
	deadlines: BTreeMap<u64, kio::WaiterList>,
	/// The `deadlines` size that sweeps out the lists every reader has left. A read that
	/// returns or is dropped leaves its deadline behind, and distinct budgets never share one.
	deadlines_sweep: usize,
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
	/// Reads parked on a time end, by the lowest sequence whose start could move it.
	starts: BTreeMap<u64, kio::WaiterList>,
	starts_sweep: usize,
	/// Reads by wall-clock deadline, released by the pool's driver.
	wall: BTreeMap<crate::time::Instant, kio::WaiterList>,
	wall_sweep: usize,
	/// The earliest `wall` deadline the pool was asked to release, so arming a later one
	/// skips the pool's lock.
	wall_armed: Option<crate::time::Instant>,
}

/// The smallest index size worth sweeping.
const HELD_SWEEP_MIN: usize = 16;

/// The size at which an index of `len` lists next sweeps out the ones every reader has left:
/// twice what it holds, so each sweep is paid for by the keys added before it.
fn sweep_at(len: usize) -> usize {
	(len * 2).max(HELD_SWEEP_MIN)
}

/// An index drained to `len` lists: bring its next sweep down to match, so what it can hold of
/// departed readers follows what it holds now rather than its peak. Only ever down: a drain
/// that raised it could keep pace with the departed keys and never sweep them.
fn lower(sweep: usize, len: usize) -> usize {
	sweep.min(sweep_at(len))
}

impl Default for Wakes {
	fn default() -> Self {
		Self {
			earliest: AtomicU64::new(u64::MAX),
			starts_from: AtomicU64::new(u64::MAX),
			landing_waiters: AtomicBool::new(false),
			newest: AtomicU64::new(0),
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
			parked.deadlines_sweep = lower(parked.deadlines_sweep, parked.deadlines.len());
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
		if parked.deadlines.len() >= parked.deadlines_sweep {
			parked.deadlines.retain(|_, list| !list.is_empty());
			// `earliest` may now be below every deadline left, which only costs the next write
			// the lock: `presented` recomputes it.
			parked.deadlines_sweep = sweep_at(parked.deadlines.len());
		}
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
		// A swept deadline may stay armed with the pool, which then releases nothing.
		if parked.wall.len() >= parked.wall_sweep {
			parked.wall.retain(|_, list| !list.is_empty());
			parked.wall_sweep = sweep_at(parked.wall.len());
		}
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
			parked.wall_sweep = lower(parked.wall_sweep, parked.wall.len());
			parked.wall_armed
		};
		woken.iter_mut().for_each(kio::WaiterList::wake);
		next
	}

	/// A servable group was inserted at `sequence`: returns whether a landing read may be
	/// parked for it to wake.
	pub(super) fn land(&self, sequence: u64) -> bool {
		self.newest.fetch_max(sequence, Ordering::Relaxed);
		// Pairs with the fence in `watch_landing`.
		fence(Ordering::SeqCst);
		self.landing_waiters.load(Ordering::Relaxed)
	}

	/// Park `waiter` until a group lands above `sequence`, returning whether one already
	/// has. Needs no track lock: a group landing after it either wakes `waiter` or is seen.
	///
	/// `false` means no servable group ever landed above `sequence`, so it has no successor
	/// and neither of the budget's clocks can convict it yet.
	pub(super) fn watch_landing(&self, sequence: u64, waiter: &kio::Waiter) -> bool {
		{
			let mut parked = self.parked.lock();
			parked.landings.entry(sequence).or_default().register(waiter);
			self.landing_waiters.store(true, Ordering::Relaxed);
		}
		// Pairs with the fence in `land`.
		fence(Ordering::SeqCst);
		self.newest.load(Ordering::Relaxed) > sequence
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
			parked.held_sweep = sweep_at(parked.held.len());
		}
		parked.held.entry(sequence).or_default().register(waiter);
		self.landing_waiters.store(true, Ordering::Relaxed);
	}

	/// Park until this exact group is cached, or a declared boundary excludes it.
	pub(super) fn watch_arrival(&self, sequence: u64, waiter: &kio::Waiter) {
		let mut parked = self.parked.lock();
		if parked.arrivals.len() >= parked.arrival_sweep {
			parked.arrivals.retain(|_, list| !list.is_empty());
			parked.arrival_sweep = sweep_at(parked.arrivals.len());
		}
		parked.arrivals.entry(sequence).or_default().register(waiter);
	}

	/// Park `waiter` until a group at or above `from` starts.
	pub(super) fn watch_start(&self, from: u64, waiter: &kio::Waiter) {
		let mut parked = self.parked.lock();
		if parked.starts.len() >= parked.starts_sweep {
			parked.starts.retain(|_, list| !list.is_empty());
			parked.starts_sweep = sweep_at(parked.starts.len());
		}
		parked.starts.entry(from).or_default().register(waiter);
		self.starts_from.fetch_min(from, Ordering::Relaxed);
	}

	/// Group `sequence` started: wake the time ends it could move.
	pub(super) fn started(&self, sequence: u64) {
		if sequence < self.starts_from.load(Ordering::Relaxed) {
			return;
		}
		let mut woken = Vec::new();
		{
			let mut parked = self.parked.lock();
			while let Some(entry) = parked.starts.first_entry()
				&& *entry.key() <= sequence
			{
				woken.push(entry.remove());
			}
			let from = parked.starts.first_key_value().map_or(u64::MAX, |(from, _)| *from);
			self.starts_from.store(from, Ordering::Relaxed);
			parked.starts_sweep = lower(parked.starts_sweep, parked.starts.len());
		}
		woken.iter_mut().for_each(kio::WaiterList::wake);
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
			parked.arrival_sweep = lower(parked.arrival_sweep, parked.arrivals.len());
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
			parked.held_sweep = lower(parked.held_sweep, parked.held.len());
			self.landing_waiters.store(
				!parked.landings.is_empty() || !parked.held.is_empty(),
				Ordering::Relaxed,
			);
		}
		woken.iter_mut().for_each(kio::WaiterList::wake);
	}

	/// Wake every parked read, for a change that moves every budget at once.
	pub(super) fn wake_all(&self) {
		let parked = {
			let mut parked = self.parked.lock();
			self.earliest.store(u64::MAX, Ordering::Relaxed);
			self.starts_from.store(u64::MAX, Ordering::Relaxed);
			self.landing_waiters.store(false, Ordering::Relaxed);
			std::mem::take(&mut *parked)
		};
		let lists = parked.deadlines.into_values().chain(parked.landings.into_values());
		let lists = lists
			.chain(parked.held.into_values())
			.chain(parked.wall.into_values())
			.chain(parked.arrivals.into_values())
			.chain(parked.starts.into_values());
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
impl Wakes {
	/// How many media and wall-clock deadlines are indexed, live or not.
	pub(super) fn deadlines_indexed(&self) -> (usize, usize) {
		let parked = self.parked.lock();
		(parked.deadlines.len(), parked.wall.len())
	}
}

#[cfg(all(test, not(loom)))]
mod test {
	use super::*;

	#[test]
	fn landing_hint_keeps_both_indices_armed() {
		let wakes = Wakes::default();
		let waiter = kio::Waiter::noop();
		assert!(!wakes.landing_waiters.load(Ordering::Relaxed));
		wakes.watch_landing(0, &waiter);
		wakes.watch_held(10, &waiter);
		wakes.landed(None, 1, 0);
		assert!(
			wakes.landing_waiters.load(Ordering::Relaxed),
			"the held group still waits"
		);
		wakes.landed(Some(1), 10, 1);
		assert!(!wakes.landing_waiters.load(Ordering::Relaxed));
		wakes.watch_held(11, &waiter);
		assert!(wakes.land(13));
		assert!(wakes.watch_landing(12, &waiter), "a group landed above it");
		assert!(!wakes.watch_landing(13, &waiter), "nothing landed above it");
		assert!(wakes.land(u64::MAX));
		assert!(
			wakes.watch_landing(u64::MAX - 1, &waiter),
			"the last sequence is above it"
		);
		wakes.wake_all();
		assert!(!wakes.landing_waiters.load(Ordering::Relaxed));
	}

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

	/// Writes that cross one deadline at a time between bursts of departed reads must not
	/// keep the next sweep out of reach. Each burst stops just short of the threshold a drain
	/// that raised it would set, which then doubles the index every round.
	#[test]
	fn partial_drains_still_sweep() {
		let wakes = Wakes::default();
		let mut next = 0u64;
		let mut park = |count: usize| {
			for _ in 0..count {
				let waiter = kio::Waiter::noop();
				wakes.watch_deadline(Timestamp::from_micros(next).unwrap(), Duration::ZERO, &waiter);
				next += 1;
			}
		};
		park(HELD_SWEEP_MIN - 1);
		for round in 0..12u64 {
			// Crosses only the earliest deadline still indexed.
			wakes.presented(Timestamp::from_micros(round).unwrap());
			let (len, _) = wakes.deadlines_indexed();
			park(len.saturating_sub(2).max(1));
		}
		let (media, _) = wakes.deadlines_indexed();
		assert!(media <= 4 * HELD_SWEEP_MIN, "{media} media deadlines indexed");
	}

	/// Live reads parked on distinct deadlines sweep only as often as the index doubles, so
	/// parking N of them costs O(N), not a sweep over every one per park.
	#[test]
	fn live_deadlines_sweep_amortized() {
		let wakes = Wakes::default();
		let now = crate::time::Instant::ZERO;
		let mut live = Vec::new();
		let (mut media_sweeps, mut wall_sweeps) = (0, 0);
		for i in 0..1_024u64 {
			{
				let parked = wakes.parked.lock();
				media_sweeps += usize::from(parked.deadlines.len() >= parked.deadlines_sweep);
				wall_sweeps += usize::from(parked.wall.len() >= parked.wall_sweep);
			}
			let waiter = kio::Waiter::noop();
			let budget = Duration::from_secs(1) + Duration::from_micros(i);
			wakes.watch_deadline(Timestamp::from_secs(10).unwrap(), budget, &waiter);
			wakes.watch_wall(now + budget, &waiter);
			live.push(waiter);
		}
		assert!(
			media_sweeps <= 8 && wall_sweeps <= 8,
			"{media_sweeps} media and {wall_sweeps} wall sweeps"
		);
	}

	/// Reads parked on distinct deadlines that leave before either clock reaches them do
	/// not stay indexed until it does.
	#[test]
	fn departed_deadlines_are_swept() {
		let wakes = Wakes::default();
		let now = crate::time::Instant::ZERO;
		for i in 0..1_000u64 {
			let waiter = kio::Waiter::noop();
			let budget = Duration::from_secs(1) + Duration::from_micros(i);
			wakes.watch_deadline(Timestamp::from_secs(10).unwrap(), budget, &waiter);
			wakes.watch_wall(now + budget, &waiter);
		}
		let (media, wall) = wakes.deadlines_indexed();
		assert!(
			media <= HELD_SWEEP_MIN && wall <= HELD_SWEEP_MIN,
			"{media} media and {wall} wall"
		);
	}
}
