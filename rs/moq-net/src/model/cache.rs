//! A shared byte budget for cached groups, repaid by write-time eviction.
//!
//! Every group charges its cached bytes into a [`Pool`] through a crate-internal
//! `Charge`. The pool itself never evicts: it is a handful of atomic counters. While
//! the pool is over capacity, each track accrues eviction debt as it writes (`accrue`),
//! sized proportionally to what it wrote, and pays that debt by aborting its own oldest
//! groups with [`Error::Evicted`](crate::Error::Evicted). Reclamation is therefore
//! distributed across every writing track and converges on the capacity without any
//! global lock, registry, or background task.
//!
//! Cross-track ordering comes from one statistic: the mean last-access time of the
//! evictable population (every cached group except each track's protected latest).
//! A group accessed more recently than that mean is never evicted, so a fresh fetch
//! in one track can't die while another track holds staler content, and a track
//! whose oldest group is staler than the mean accrues debt at double rate. Evicting
//! old entries and inserting new ones both advance the mean, so the eviction
//! frontier moves with cache turnover on its own.
//!
//! A pool is inert by default ([`Pool::unbounded`]): publishers and subscribers that
//! never set a capacity pay only a couple of atomic counters. A relay creates one
//! bounded pool and shares it across every origin so the whole process caches into a
//! single budget.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Fixed bookkeeping charged per cached group on top of its frame payload bytes.
///
/// Covers the group/track slot allocations so a track producing many tiny groups
/// (e.g. one frame per group) is billed roughly for its real footprint instead of
/// just its payload bytes. Also bounds the live group count (`used / 256`), which
/// keeps the access-time sum below u64 (see [`TICK_MS`]).
const ENTRY_OVERHEAD: u64 = 256;

/// Milliseconds per tick of the coarse clock behind access timestamps.
///
/// Coarse ticks keep the count-weighted timestamp sum far from u64 overflow: the
/// sum is bounded by `elapsed_ticks * live_groups`, live groups are bounded by
/// `used / ENTRY_OVERHEAD`, and twenty years of ticks (6.3e9) times a 64 GiB
/// target's worst-case ~270M groups is ~1.7e18, a tenth of `u64::MAX`. A
/// byte-weighted mean would overflow u64 even at whole-second ticks, which is why
/// the mean is count-weighted.
const TICK_MS: u64 = 100;

/// A shared byte budget that caches charge into; cloning shares the same budget.
///
/// The pool tracks how many payload bytes are cached across every registered group,
/// plus the mean last-access time of the evictable ones. It never evicts on its own:
/// tracks accrue eviction debt as they write and evict their own oldest groups to
/// pay it, so every operation here is a few atomics with no lock. The capacity is
/// therefore a target usage converges toward, not a hard limit: carried debt, capped
/// payments, and the always-protected live edge all let usage transiently exceed it.
#[derive(Clone, Default)]
pub struct Pool {
	inner: Arc<Inner>,
}

struct Inner {
	// Total bytes currently charged, including per-entry overhead.
	used: AtomicU64,
	// u64::MAX means unbounded.
	capacity: AtomicU64,
	// Reference point for the coarse tick clock.
	epoch: web_async::time::Instant,
	// Sum and count of last-access ticks across the evictable population, giving a
	// count-weighted mean. Tracks add a group when it becomes evictable (demoted
	// from the live edge, or inserted behind it) and remove it when it leaves.
	access_sum: AtomicU64,
	access_count: AtomicU64,
}

impl Default for Inner {
	fn default() -> Self {
		Self {
			used: AtomicU64::new(0),
			capacity: AtomicU64::new(u64::MAX),
			epoch: web_async::time::Instant::now(),
			access_sum: AtomicU64::new(0),
			access_count: AtomicU64::new(0),
		}
	}
}

impl Pool {
	/// Create a pool with a byte target that tracks evict toward as they write.
	///
	/// The budget counts frame payload bytes (plus a small fixed overhead per
	/// group), not process RSS, and is a convergence target rather than a hard
	/// limit; leave headroom when sizing it from real memory.
	pub fn new(capacity: u64) -> Self {
		let pool = Self::default();
		pool.inner.capacity.store(capacity, Ordering::Relaxed);
		pool
	}

	/// Create a pool that never evicts. This is the [`Default`].
	pub fn unbounded() -> Self {
		Self::default()
	}

	/// The configured byte target, or `None` when unbounded.
	pub fn capacity(&self) -> Option<u64> {
		match self.inner.capacity.load(Ordering::Relaxed) {
			u64::MAX => None,
			capacity => Some(capacity),
		}
	}

	/// Bytes currently cached across every registered group.
	pub fn used(&self) -> u64 {
		self.inner.used.load(Ordering::Relaxed)
	}

	/// Change the capacity. `None` makes the pool unbounded.
	///
	/// Takes effect as tracks write: a shrink leaves the pool over budget, which every
	/// subsequent write pays down proportionally. Nothing is reclaimed synchronously.
	pub fn resize(&self, capacity: impl Into<Option<u64>>) {
		let capacity = capacity.into().unwrap_or(u64::MAX);
		self.inner.capacity.store(capacity, Ordering::Relaxed);
	}

	/// Returns true if both handles share the same underlying pool.
	pub fn same_pool(&self, other: &Self) -> bool {
		Arc::ptr_eq(&self.inner, &other.inner)
	}

	/// Charge `n` more cached bytes.
	pub(crate) fn add(&self, n: u64) {
		self.inner.used.fetch_add(n, Ordering::Relaxed);
	}

	/// Release `n` cached bytes.
	pub(crate) fn sub(&self, n: u64) {
		self.inner.used.fetch_sub(n, Ordering::Relaxed);
	}

	/// Coarse ticks since the pool was created: the clock access timestamps use.
	pub(crate) fn now(&self) -> u64 {
		self.inner.epoch.elapsed().as_millis() as u64 / TICK_MS
	}

	/// Convert a duration into coarse ticks, saturating.
	pub(crate) fn ticks(duration: Duration) -> u64 {
		u64::try_from(duration.as_millis() / TICK_MS as u128).unwrap_or(u64::MAX)
	}

	/// Mean last-access tick across the evictable population, or `None` when it is
	/// empty. The sum and count are read separately, so the mean is approximate
	/// under concurrent updates; eviction only needs a rough frontier.
	pub(crate) fn average(&self) -> Option<u64> {
		let count = self.inner.access_count.load(Ordering::Relaxed);
		if count == 0 {
			return None;
		}
		Some(self.inner.access_sum.load(Ordering::Relaxed) / count)
	}

	/// A group with last-access tick `ts` joined the evictable population.
	pub(crate) fn access_insert(&self, ts: u64) {
		self.inner.access_sum.fetch_add(ts, Ordering::Relaxed);
		self.inner.access_count.fetch_add(1, Ordering::Relaxed);
	}

	/// A group with last-access tick `ts` left the evictable population.
	pub(crate) fn access_remove(&self, ts: u64) {
		self.inner.access_sum.fetch_sub(ts, Ordering::Relaxed);
		self.inner.access_count.fetch_sub(1, Ordering::Relaxed);
	}

	/// An evictable group's last-access tick moved from `old` to `new` (a FETCH hit).
	pub(crate) fn access_refresh(&self, old: u64, new: u64) {
		// A single wrapping add keeps the sum exact even under racing refreshes.
		self.inner
			.access_sum
			.fetch_add(new.wrapping_sub(old), Ordering::Relaxed);
	}

	/// The eviction debt a track takes on by writing `written` bytes, or `None` while
	/// the pool is under capacity (the caller should forget any outstanding debt).
	///
	/// The debt is `written * used / capacity`, so paying it evicts slightly more
	/// than was written and the overshoot decays toward the capacity. Tracks double
	/// it when their oldest content is staler than [`Self::average`]. Saturates: a
	/// tiny capacity must not wrap a huge debt into a small one.
	pub(crate) fn accrue(&self, written: u64) -> Option<u64> {
		let used = self.inner.used.load(Ordering::Relaxed);
		let capacity = self.inner.capacity.load(Ordering::Relaxed);
		if used <= capacity {
			return None;
		}
		let debt = written as u128 * used as u128 / capacity.max(1) as u128;
		Some(u64::try_from(debt).unwrap_or(u64::MAX))
	}
}

impl std::fmt::Debug for Pool {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Pool")
			.field("used", &self.used())
			.field("capacity", &self.capacity())
			.finish()
	}
}

/// The RAII byte accounting for one cached group, owned by the group's state.
///
/// `add`/`sub` mirror the group's cached payload bytes into the pool with plain
/// atomics, and every charged byte (overhead included) is also accumulated into the
/// track's gross-write counter, which the track drains into eviction debt on its
/// next write. The charge also owns the group's sample in the pool's access mean,
/// so the sample lives exactly as long as the cached bytes do: aborting or dropping
/// the group removes both, no matter who does it or when. The default charge is
/// detached: it belongs to no pool and every operation is a no-op.
#[derive(Default)]
pub(crate) struct Charge {
	pool: Option<Pool>,
	// The owning track's gross-write counter (see `track::Info`); never decremented
	// here, the track swaps it out as it accrues debt.
	written: Option<Arc<AtomicU64>>,
	// Bytes currently charged, including ENTRY_OVERHEAD, released on drop.
	bytes: u64,
	// Tick of the last cache access: creation, a FETCH hit, or a fetched backfill's
	// birth. Eviction protection and age expiry key off this.
	last: u64,
	// Whether `last` is currently a sample in the pool's access mean, i.e. the
	// group is in the evictable population.
	counted: bool,
}

impl Charge {
	/// Charge a new group's fixed overhead into `pool`, counting it as written.
	pub(crate) fn new(pool: Pool, written: Arc<AtomicU64>) -> Self {
		pool.add(ENTRY_OVERHEAD);
		written.fetch_add(ENTRY_OVERHEAD, Ordering::Relaxed);
		let last = pool.now();
		Self {
			pool: Some(pool),
			written: Some(written),
			bytes: ENTRY_OVERHEAD,
			last,
			counted: false,
		}
	}

	/// Charge `n` more payload bytes, counting them as written.
	///
	/// A write is also an access: it restarts the retention clock and keeps an
	/// actively-growing group (a straggler or backfill still being filled) from
	/// being evicted or expired mid-write.
	pub(crate) fn add(&mut self, n: u64) {
		if let Some(pool) = &self.pool {
			pool.add(n);
			self.bytes += n;
			let now = pool.now();
			if now > self.last {
				if self.counted {
					pool.access_refresh(self.last, now);
				}
				self.last = now;
			}
		}
		if let Some(written) = &self.written {
			written.fetch_add(n, Ordering::Relaxed);
		}
	}

	/// Release `n` payload bytes (a frame evicted by the group's own cap).
	pub(crate) fn sub(&mut self, n: u64) {
		if let Some(pool) = &self.pool {
			pool.sub(n);
			self.bytes = self.bytes.saturating_sub(n);
		}
	}

	/// The group's full cached footprint: payload bytes plus overhead.
	pub(crate) fn size(&self) -> u64 {
		self.bytes
	}

	/// Tick of the group's last cache access.
	pub(crate) fn accessed(&self) -> u64 {
		self.last
	}

	/// Enter the group into the evictable population (demoted from the live edge,
	/// or inserted behind it), sampling its access time into the pool's mean.
	/// Idempotent.
	pub(crate) fn demote(&mut self) {
		if let Some(pool) = &self.pool
			&& !self.counted
		{
			pool.access_insert(self.last);
			self.counted = true;
		}
	}

	/// Record a cache access (a FETCH hit, or a fetched backfill's birth).
	///
	/// Stamps one tick past the current one, so an access within the same coarse
	/// tick as the population mean still reads as strictly newer and protects the
	/// group. Idempotent within a tick, so repeated hits can't run ahead of the
	/// clock.
	pub(crate) fn refresh(&mut self) {
		let Some(pool) = &self.pool else { return };
		let now = pool.now().saturating_add(1);
		if now <= self.last {
			return;
		}
		if self.counted {
			pool.access_refresh(self.last, now);
		}
		self.last = now;
	}

	/// Release everything this charge holds: bytes, overhead, and the access
	/// sample. Idempotent; used when the group aborts and clears its frames.
	pub(crate) fn clear(&mut self) {
		if let Some(pool) = &self.pool {
			pool.sub(self.bytes);
			self.bytes = 0;
			if self.counted {
				pool.access_remove(self.last);
				self.counted = false;
			}
		}
	}
}

impl Drop for Charge {
	fn drop(&mut self) {
		self.clear();
	}
}

#[cfg(test)]
mod test {
	use super::*;

	fn charge(pool: &Pool) -> Charge {
		Charge::new(pool.clone(), Arc::new(AtomicU64::new(0)))
	}

	#[test]
	fn unbounded_never_accrues() {
		let pool = Pool::unbounded();
		let mut charge = charge(&pool);
		charge.add(1 << 40);
		assert_eq!(pool.accrue(1 << 30), None);
		assert_eq!(pool.used(), (1 << 40) + ENTRY_OVERHEAD);
		drop(charge);
		assert_eq!(pool.used(), 0);
	}

	#[test]
	fn accrue_none_under_capacity() {
		let pool = Pool::new(1000);
		let mut charge = charge(&pool);
		charge.add(500);
		assert_eq!(pool.accrue(100), None);
	}

	#[test]
	fn accrue_proportional_over_capacity() {
		let pool = Pool::new(1000);
		let mut charge = charge(&pool);
		charge.add(2000 - ENTRY_OVERHEAD); // used = 2000, twice the capacity

		// Debt exceeds what was written by the overshoot ratio, so the pool drains.
		assert_eq!(pool.accrue(100), Some(200));
		// Zero written accrues zero: an idle track takes on no debt.
		assert_eq!(pool.accrue(0), Some(0));
	}

	#[test]
	fn average_tracks_evictable_population() {
		let pool = Pool::new(1000);
		assert_eq!(pool.average(), None);

		pool.access_insert(10);
		pool.access_insert(20);
		assert_eq!(pool.average(), Some(15));

		// A refresh moves one member's contribution, exactly.
		pool.access_refresh(10, 40);
		assert_eq!(pool.average(), Some(30));

		pool.access_remove(40);
		assert_eq!(pool.average(), Some(20));
		pool.access_remove(20);
		assert_eq!(pool.average(), None);
	}

	#[test]
	fn charge_raii() {
		let pool = Pool::new(1000);
		let mut charge = charge(&pool);
		assert_eq!(pool.used(), ENTRY_OVERHEAD);

		charge.add(100);
		assert_eq!(pool.used(), ENTRY_OVERHEAD + 100);
		charge.sub(40);
		assert_eq!(pool.used(), ENTRY_OVERHEAD + 60);

		charge.clear();
		assert_eq!(pool.used(), 0);
		// Idempotent: a second clear (and the eventual drop) releases nothing more.
		charge.clear();
		drop(charge);
		assert_eq!(pool.used(), 0);
	}

	#[test]
	fn detached_charge_is_noop() {
		let mut charge = Charge::default();
		charge.add(123);
		charge.sub(23);
		charge.clear();
	}

	#[test]
	fn accrue_saturates() {
		// A huge overshoot against a tiny capacity must saturate, not wrap.
		let pool = Pool::new(1);
		let mut c = charge(&pool);
		c.add(1 << 40);
		assert_eq!(pool.accrue(1 << 40), Some(u64::MAX));
	}

	#[test]
	fn charge_counts_gross_writes() {
		let pool = Pool::new(1000);
		let written = Arc::new(AtomicU64::new(0));
		let mut c = Charge::new(pool.clone(), written.clone());
		c.add(100);
		c.sub(40); // releases don't refund the gross counter
		assert_eq!(written.load(Ordering::Relaxed), ENTRY_OVERHEAD + 100);
	}

	#[test]
	fn charge_owns_access_sample() {
		let pool = Pool::new(1000);
		let mut c = charge(&pool);
		assert_eq!(pool.average(), None, "not evictable until demoted");

		c.demote();
		c.demote(); // idempotent
		assert!(pool.average().is_some());

		// Clearing (an abort, from anyone) removes the sample with the bytes.
		c.clear();
		assert_eq!(pool.average(), None, "aborted groups leave no ghost sample");
		drop(c);
		assert_eq!(pool.average(), None);
	}

	#[test]
	fn refresh_protects_within_a_tick() {
		let pool = Pool::new(1000);
		let mut c = charge(&pool);
		c.demote();
		let average = pool.average().unwrap();
		// A refresh in the same coarse tick still lifts the group above the mean.
		c.refresh();
		assert!(c.accessed() > average);
		// Repeated same-tick refreshes are idempotent, not runaway.
		let stamped = c.accessed();
		c.refresh();
		assert_eq!(c.accessed(), stamped);
	}

	#[test]
	fn resize() {
		let pool = Pool::unbounded();
		assert_eq!(pool.capacity(), None);

		let mut charge = charge(&pool);
		charge.add(1000);

		// Shrinking doesn't reclaim anything synchronously; writers accrue debt instead.
		pool.resize(100);
		assert_eq!(pool.capacity(), Some(100));
		assert!(pool.used() > 100);
		assert!(pool.accrue(50).unwrap() > 50);

		pool.resize(None);
		assert_eq!(pool.capacity(), None);
		assert_eq!(pool.accrue(50), None);
	}
}
