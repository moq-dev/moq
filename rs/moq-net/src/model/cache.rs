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
//! A group that a FETCH refreshed is spared by its track and its bytes are put back
//! into the pool as `credit`; the credit is re-billed to all writers on top of their
//! base debt, so protecting hot groups never lowers the total eviction rate, it only
//! shifts it toward tracks whose content goes unread.
//!
//! A pool is inert by default ([`Pool::unbounded`]): publishers and subscribers that
//! never set a capacity pay only a couple of atomic counters. A relay creates one
//! bounded pool and shares it across every origin so the whole process caches into a
//! single budget.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Fixed bookkeeping charged per cached group on top of its frame payload bytes.
///
/// Covers the group/track slot allocations so a track producing many tiny groups
/// (e.g. one frame per group) is billed roughly for its real footprint instead of
/// just its payload bytes.
const ENTRY_OVERHEAD: u64 = 256;

/// A shared byte budget that caches charge into; cloning shares the same budget.
///
/// The pool tracks how many payload bytes are cached across every registered group.
/// It never evicts on its own: tracks accrue eviction debt as they write and evict
/// their own oldest groups to pay it, so every operation here is a few atomics with
/// no lock.
#[derive(Clone, Default)]
pub struct Pool {
	inner: Arc<Inner>,
}

struct Inner {
	// Total bytes currently charged, including per-entry overhead.
	used: AtomicU64,
	// u64::MAX means unbounded.
	capacity: AtomicU64,
	// Bytes spared from eviction because a FETCH refreshed them, awaiting re-billing
	// through `accrue` so the total eviction rate is conserved.
	credit: AtomicU64,
}

impl Default for Inner {
	fn default() -> Self {
		Self {
			used: AtomicU64::new(0),
			capacity: AtomicU64::new(u64::MAX),
			credit: AtomicU64::new(0),
		}
	}
}

impl Pool {
	/// Create a pool with a byte budget that tracks evict toward as they write.
	///
	/// The budget counts frame payload bytes (plus a small fixed overhead per
	/// group), not process RSS; leave headroom when sizing it from real memory.
	pub fn new(capacity: u64) -> Self {
		let pool = Self::default();
		pool.inner.capacity.store(capacity, Ordering::Relaxed);
		pool
	}

	/// Create a pool that never evicts. This is the [`Default`].
	pub fn unbounded() -> Self {
		Self::default()
	}

	/// The configured capacity in bytes, or `None` when unbounded.
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

	/// The eviction debt a track takes on by writing `written` bytes, or `None` while
	/// the pool is under capacity (the caller should forget any outstanding debt).
	///
	/// The base debt is `written * used / capacity`, so paying it evicts slightly
	/// more than was written and the overshoot decays toward the capacity. On top of
	/// that, a proportional share of the outstanding [`credit`](Self::credit) is
	/// drained and re-billed, conserving the total eviction rate when hot groups are
	/// being spared.
	pub(crate) fn accrue(&self, written: u64) -> Option<u64> {
		let used = self.inner.used.load(Ordering::Relaxed);
		let capacity = self.inner.capacity.load(Ordering::Relaxed);
		if used <= capacity {
			return None;
		}

		let capacity = capacity.max(1) as u128;
		let base = (written as u128 * used as u128 / capacity) as u64;

		// Drain credit over roughly one capacity turnover of writes, claiming only
		// what's actually left so racing writers never re-bill the same bytes twice.
		let credit = self.inner.credit.load(Ordering::Relaxed);
		let mut drain = (written as u128 * credit as u128 / capacity) as u64;
		if drain > 0 {
			let claimed = self
				.inner
				.credit
				.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
					Some(cur.saturating_sub(drain))
				})
				.unwrap_or(0);
			drain = drain.min(claimed);
		}

		Some(base.saturating_add(drain))
	}

	/// Report `n` bytes spared from eviction because a FETCH refreshed them.
	///
	/// The spared bytes count as debt paid for the sparing track and are re-billed to
	/// all writers through [`Self::accrue`].
	pub(crate) fn credit(&self, n: u64) {
		self.inner.credit.fetch_add(n, Ordering::Relaxed);
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
/// atomics. Dropping (or [`clear`](Self::clear)ing) the charge releases everything it
/// holds. The default charge is detached: it belongs to no pool and every operation
/// is a no-op.
#[derive(Default)]
pub(crate) struct Charge {
	pool: Option<Pool>,
	// Bytes currently charged, including ENTRY_OVERHEAD, released on drop.
	bytes: u64,
}

impl Charge {
	/// Charge a new group's fixed overhead into `pool`.
	pub(crate) fn new(pool: Pool) -> Self {
		pool.add(ENTRY_OVERHEAD);
		Self {
			pool: Some(pool),
			bytes: ENTRY_OVERHEAD,
		}
	}

	/// Charge `n` more payload bytes.
	pub(crate) fn add(&mut self, n: u64) {
		if let Some(pool) = &self.pool {
			pool.add(n);
			self.bytes += n;
		}
	}

	/// Release `n` payload bytes (a frame evicted by the group's own cap).
	pub(crate) fn sub(&mut self, n: u64) {
		if let Some(pool) = &self.pool {
			pool.sub(n);
			self.bytes = self.bytes.saturating_sub(n);
		}
	}

	/// Release everything this charge holds (bytes and overhead). Idempotent; used
	/// when the group aborts and clears its frames.
	pub(crate) fn clear(&mut self) {
		if let Some(pool) = &self.pool {
			pool.sub(self.bytes);
			self.bytes = 0;
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

	#[test]
	fn unbounded_never_accrues() {
		let pool = Pool::unbounded();
		let mut charge = Charge::new(pool.clone());
		charge.add(1 << 40);
		assert_eq!(pool.accrue(1 << 30), None);
		assert_eq!(pool.used(), (1 << 40) + ENTRY_OVERHEAD);
		drop(charge);
		assert_eq!(pool.used(), 0);
	}

	#[test]
	fn accrue_none_under_capacity() {
		let pool = Pool::new(1000);
		let mut charge = Charge::new(pool.clone());
		charge.add(500);
		assert_eq!(pool.accrue(100), None);
	}

	#[test]
	fn accrue_proportional_over_capacity() {
		let pool = Pool::new(1000);
		let mut charge = Charge::new(pool.clone());
		charge.add(2000 - ENTRY_OVERHEAD); // used = 2000, twice the capacity

		// Debt exceeds what was written by the overshoot ratio, so the pool drains.
		assert_eq!(pool.accrue(100), Some(200));
		// Zero written accrues zero: an idle track takes on no debt.
		assert_eq!(pool.accrue(0), Some(0));
	}

	#[test]
	fn accrue_drains_credit() {
		let pool = Pool::new(1000);
		let mut charge = Charge::new(pool.clone());
		charge.add(2000 - ENTRY_OVERHEAD); // used = 2000

		// A spared group re-bills its bytes on top of the base debt: base 200 plus
		// 100 * 500/1000 = 50 of the credit.
		pool.credit(500);
		assert_eq!(pool.accrue(100), Some(250));

		// The drained share is claimed, not copied: repeated accruals exhaust it.
		let mut total = 250u64;
		for _ in 0..100 {
			total += pool.accrue(100).unwrap() - 200;
		}
		assert!(total <= 250 + 500, "drained more credit than was granted");
	}

	#[test]
	fn charge_raii() {
		let pool = Pool::new(1000);
		let mut charge = Charge::new(pool.clone());
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
	fn resize() {
		let pool = Pool::unbounded();
		assert_eq!(pool.capacity(), None);

		let mut charge = Charge::new(pool.clone());
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
