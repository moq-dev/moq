//! A shared byte budget for cached groups, reclaimed with LRU eviction.
//!
//! Every group registers its cached bytes in a [`Pool`]. When the pool exceeds its
//! capacity, the least-recently-read groups are aborted with
//! [`Error::Evicted`](crate::Error::Evicted), freeing their frames immediately. The
//! latest group of each track is pinned and never evicted, so the live edge always
//! survives memory pressure.
//!
//! A pool is inert by default ([`Pool::unbounded`]): publishers and subscribers that
//! never set a capacity pay only a couple of atomic counters. A relay creates one
//! bounded pool and shares it across every origin so the whole process caches into a
//! single budget.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use web_async::Lock;

/// Fixed bookkeeping charged per cached group on top of its frame payload bytes.
///
/// Covers the group/track slot allocations so a track producing many tiny groups
/// (e.g. one frame per group) is billed roughly for its real footprint instead of
/// just its payload bytes.
const ENTRY_OVERHEAD: u64 = 256;

/// A shared byte budget that caches charge into; cloning shares the same budget.
///
/// The pool tracks how many payload bytes are cached across every registered group
/// and evicts the least-recently-read groups once `used` exceeds `capacity`.
/// Eviction aborts the victim group with [`Error::Evicted`](crate::Error::Evicted),
/// which frees its frames immediately and wakes any parked readers. Pinned (latest)
/// groups are never evicted but still count against the budget.
///
/// Reads and writes only touch atomics; the internal lock is taken when a group
/// registers/unregisters and when the pool is actually over budget.
#[derive(Clone, Default)]
pub struct Pool {
	inner: Arc<Inner>,
}

struct Inner {
	// Total bytes currently charged, including pinned entries and per-entry overhead.
	used: AtomicU64,
	// u64::MAX means unbounded.
	capacity: AtomicU64,
	// Reference point for the coarse `last_access` clock.
	epoch: web_async::time::Instant,
	lru: Lock<Lru>,
}

impl Default for Inner {
	fn default() -> Self {
		Self {
			used: AtomicU64::new(0),
			capacity: AtomicU64::new(u64::MAX),
			epoch: web_async::time::Instant::now(),
			lru: Lock::default(),
		}
	}
}

#[derive(Default)]
struct Lru {
	// Min-heap of (last_access snapshot, id). Snapshots go stale when an entry is
	// touched; a popped entry whose current last_access differs is re-pushed with
	// the fresh key instead of being evicted (lazy re-keying).
	heap: BinaryHeap<Reverse<(u64, u64)>>,
	// Live entries by id. An id popped from the heap that's no longer here was
	// already evicted or dropped.
	entries: HashMap<u64, Arc<Entry>>,
	next_id: u64,
}

/// A single cached group's registration in the pool.
///
/// Holds only atomics plus the eviction hook, so touching recency or flipping the
/// pin never takes a lock. The hook holds a weak handle to the group, so an entry
/// never keeps its group alive.
pub(crate) struct Entry {
	id: u64,
	// Payload bytes plus ENTRY_OVERHEAD currently charged by this entry.
	bytes: AtomicU64,
	// Coarse milliseconds since the pool epoch of the last read (or write).
	last_access: AtomicU64,
	// Pinned entries (the track's latest group) are skipped by eviction.
	pinned: AtomicBool,
	// Aborts the group with Error::Evicted. Called without any pool lock held.
	evict: Box<dyn Fn() + Send + Sync>,
	epoch: web_async::time::Instant,
}

impl Entry {
	fn now(&self) -> u64 {
		self.epoch.elapsed().as_millis() as u64
	}

	/// Record a read so eviction considers this group recently used.
	pub(crate) fn touch(&self) {
		self.last_access.store(self.now(), Ordering::Relaxed);
	}

	/// Pin or unpin this entry. Pinned entries are immune to eviction.
	pub(crate) fn set_pinned(&self, pinned: bool) {
		self.pinned.store(pinned, Ordering::Relaxed);
	}
}

impl Pool {
	/// Create a pool that evicts once `capacity` bytes are cached.
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

	/// Change the capacity, evicting immediately if the new capacity is exceeded.
	/// `None` makes the pool unbounded.
	pub fn resize(&self, capacity: impl Into<Option<u64>>) {
		let capacity = capacity.into().unwrap_or(u64::MAX);
		self.inner.capacity.store(capacity, Ordering::Relaxed);
		self.evict();
	}

	/// Returns true if both handles share the same underlying pool.
	pub fn same_pool(&self, other: &Self) -> bool {
		Arc::ptr_eq(&self.inner, &other.inner)
	}

	/// Test-only snapshot of the live entries: (id, last_access ms, bytes, pinned),
	/// plus the pool's current clock reading. For diagnosing eviction order.
	#[cfg(test)]
	pub(crate) fn debug_entries(&self) -> (u64, Vec<(u64, u64, u64, bool)>) {
		let now = self.inner.epoch.elapsed().as_millis() as u64;
		let lru = self.inner.lru.lock();
		let mut entries: Vec<_> = lru
			.entries
			.values()
			.map(|e| {
				(
					e.id,
					e.last_access.load(Ordering::Relaxed),
					e.bytes.load(Ordering::Relaxed),
					e.pinned.load(Ordering::Relaxed),
				)
			})
			.collect();
		entries.sort_unstable();
		(now, entries)
	}

	/// Register a group, returning the [`Charge`] that tracks its bytes.
	///
	/// `evict` must abort the group (releasing its charge); it is invoked without
	/// any pool lock held, and never after the returned charge is dropped.
	pub(crate) fn register(&self, evict: Box<dyn Fn() + Send + Sync>) -> Charge {
		let inner = self.inner.clone();
		let entry = {
			let mut lru = inner.lru.lock();
			let id = lru.next_id;
			lru.next_id += 1;

			let entry = Arc::new(Entry {
				id,
				bytes: AtomicU64::new(ENTRY_OVERHEAD),
				last_access: AtomicU64::new(0),
				pinned: AtomicBool::new(false),
				evict,
				epoch: inner.epoch,
			});
			entry.touch();

			lru.entries.insert(id, entry.clone());
			lru.heap.push(Reverse((entry.last_access.load(Ordering::Relaxed), id)));
			entry
		};

		inner.used.fetch_add(ENTRY_OVERHEAD, Ordering::Relaxed);
		Charge {
			inner: Some((inner, entry)),
		}
	}

	/// Evict least-recently-read groups until the pool is back under capacity.
	///
	/// Victims are collected under the lock but aborted after it's released, so an
	/// eviction hook can freely take its group's lock.
	pub(crate) fn evict(&self) {
		let inner = &self.inner;
		if inner.used.load(Ordering::Relaxed) <= inner.capacity.load(Ordering::Relaxed) {
			return;
		}

		let mut victims = Vec::new();
		{
			let mut lru = inner.lru.lock();
			// Bytes the collected victims will release once aborted below.
			let mut freed = 0u64;
			// Pinned entries popped this pass; re-pushing them immediately would just
			// pop them again (same key), so they're held aside until the pass ends.
			let mut pinned = Vec::new();
			// Bounding the pops guarantees termination: an entry needs at most two
			// (one re-key of its stale snapshot, one settle). A concurrent touch can
			// steal a slot, ending the pass early; the next write retries.
			let mut budget = 2 * lru.heap.len();

			while budget > 0
				&& inner.used.load(Ordering::Relaxed).saturating_sub(freed) > inner.capacity.load(Ordering::Relaxed)
			{
				budget -= 1;
				let Some(Reverse((snapshot, id))) = lru.heap.pop() else {
					break;
				};
				let Some(entry) = lru.entries.get(&id) else {
					// Already evicted or dropped; discard the stale heap slot.
					continue;
				};
				let access = entry.last_access.load(Ordering::Relaxed);
				if access != snapshot {
					// Touched since this snapshot; re-key instead of evicting.
					lru.heap.push(Reverse((access, id)));
					continue;
				}
				if entry.pinned.load(Ordering::Relaxed) {
					pinned.push(Reverse((access, id)));
					continue;
				}

				let entry = lru.entries.remove(&id).unwrap();
				freed += entry.bytes.load(Ordering::Relaxed);
				victims.push(entry);
			}

			for slot in pinned {
				lru.heap.push(slot);
			}
		}

		for victim in victims {
			(victim.evict)();
		}
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

/// The RAII side of a group's pool registration, owned by the group's state.
///
/// `add`/`sub` mirror the group's cached payload bytes into the pool with plain
/// atomics; no lock is taken until the group unregisters. Dropping (or
/// [`clear`](Self::clear)ing) the charge releases everything it holds. The default
/// charge is detached: it belongs to no pool and every operation is a no-op.
#[derive(Default)]
pub(crate) struct Charge {
	inner: Option<(Arc<Inner>, Arc<Entry>)>,
}

impl Charge {
	/// Charge `n` more payload bytes and mark the group recently used.
	///
	/// Doesn't evict; callers trigger [`Pool::evict`] after releasing their own locks.
	pub(crate) fn add(&self, n: u64) {
		if let Some((inner, entry)) = &self.inner {
			entry.bytes.fetch_add(n, Ordering::Relaxed);
			inner.used.fetch_add(n, Ordering::Relaxed);
			entry.touch();
		}
	}

	/// Release `n` payload bytes (a frame evicted by the group's own cap).
	pub(crate) fn sub(&self, n: u64) {
		if let Some((inner, entry)) = &self.inner {
			entry.bytes.fetch_sub(n, Ordering::Relaxed);
			inner.used.fetch_sub(n, Ordering::Relaxed);
		}
	}

	/// Release everything this charge holds (bytes and overhead). Idempotent;
	/// used when the group aborts and clears its frames.
	pub(crate) fn clear(&self) {
		if let Some((inner, entry)) = &self.inner {
			let bytes = entry.bytes.swap(0, Ordering::Relaxed);
			inner.used.fetch_sub(bytes, Ordering::Relaxed);
		}
	}

	/// Mark the group recently used (a consumer read a frame).
	pub(crate) fn touch(&self) {
		if let Some((_, entry)) = &self.inner {
			entry.touch();
		}
	}

	/// The registration entry, for pinning. `None` when detached.
	pub(crate) fn entry(&self) -> Option<Arc<Entry>> {
		self.inner.as_ref().map(|(_, entry)| entry.clone())
	}
}

impl Drop for Charge {
	fn drop(&mut self) {
		let Some((inner, entry)) = self.inner.take() else {
			return;
		};
		let bytes = entry.bytes.swap(0, Ordering::Relaxed);
		inner.used.fetch_sub(bytes, Ordering::Relaxed);
		// The heap slot (if any) goes stale and is discarded on its next pop.
		inner.lru.lock().entries.remove(&entry.id);
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use std::time::Duration;

	fn flag() -> (Arc<AtomicBool>, Box<dyn Fn() + Send + Sync>) {
		let evicted = Arc::new(AtomicBool::new(false));
		let hook = evicted.clone();
		(
			evicted,
			Box::new(move || {
				hook.store(true, Ordering::Relaxed);
			}),
		)
	}

	#[test]
	fn unbounded_never_evicts() {
		let pool = Pool::unbounded();
		let (evicted, hook) = flag();
		let charge = pool.register(hook);
		charge.add(1 << 40);
		pool.evict();
		assert!(!evicted.load(Ordering::Relaxed));
		assert_eq!(pool.used(), (1 << 40) + ENTRY_OVERHEAD);
		drop(charge);
		assert_eq!(pool.used(), 0);
	}

	#[test]
	fn detached_charge_is_noop() {
		let charge = Charge::default();
		charge.add(123);
		charge.sub(23);
		charge.clear();
		charge.touch();
		assert!(charge.entry().is_none());
	}

	#[tokio::test]
	async fn evicts_least_recently_used() {
		tokio::time::pause();

		let pool = Pool::new(3 * ENTRY_OVERHEAD + 2500);
		let (evicted_a, hook_a) = flag();
		let (evicted_b, hook_b) = flag();
		let (evicted_c, hook_c) = flag();

		let a = pool.register(hook_a);
		a.add(1000);
		tokio::time::advance(Duration::from_millis(10)).await;
		let b = pool.register(hook_b);
		b.add(1000);
		tokio::time::advance(Duration::from_millis(10)).await;

		// Read A so B becomes the least recently used.
		a.touch();
		tokio::time::advance(Duration::from_millis(10)).await;

		let c = pool.register(hook_c);
		c.add(1000);
		// Over budget by 500: evicting B (stalest) is enough once its charge clears.
		pool.evict();

		assert!(!evicted_a.load(Ordering::Relaxed));
		assert!(evicted_b.load(Ordering::Relaxed));
		assert!(!evicted_c.load(Ordering::Relaxed));

		// The hook is responsible for releasing the charge (a real group aborts).
		b.clear();
		assert_eq!(pool.used(), 2 * (1000 + ENTRY_OVERHEAD));
	}

	#[tokio::test]
	async fn pinned_entries_survive() {
		tokio::time::pause();

		let pool = Pool::new(ENTRY_OVERHEAD);
		let (evicted_a, hook_a) = flag();
		let (evicted_b, hook_b) = flag();

		let a = pool.register(hook_a);
		a.entry().unwrap().set_pinned(true);
		a.add(1000);
		tokio::time::advance(Duration::from_millis(10)).await;

		let b = pool.register(hook_b);
		b.add(1000);

		pool.evict();
		// A is older but pinned; B is the only eligible victim.
		assert!(!evicted_a.load(Ordering::Relaxed));
		assert!(evicted_b.load(Ordering::Relaxed));

		// With B gone, everything left is pinned: eviction terminates without
		// touching A even though the pool stays over budget.
		b.clear();
		drop(b);
		pool.evict();
		assert!(!evicted_a.load(Ordering::Relaxed));
	}

	#[tokio::test]
	async fn resize_evicts() {
		tokio::time::pause();

		let pool = Pool::unbounded();
		let (evicted_a, hook_a) = flag();
		let a = pool.register(hook_a);
		a.add(1000);

		pool.evict();
		assert!(!evicted_a.load(Ordering::Relaxed));

		pool.resize(100);
		assert!(evicted_a.load(Ordering::Relaxed));

		// Back to unbounded: nothing more to do, and capacity() reports None.
		pool.resize(None);
		assert_eq!(pool.capacity(), None);
	}

	#[tokio::test]
	async fn touched_entry_is_rekeyed_not_evicted() {
		tokio::time::pause();

		let pool = Pool::new(2 * ENTRY_OVERHEAD + 1500);
		let (evicted_a, hook_a) = flag();
		let (evicted_b, hook_b) = flag();

		let a = pool.register(hook_a);
		a.add(1000);
		tokio::time::advance(Duration::from_millis(10)).await;
		let b = pool.register(hook_b);
		b.add(1000);
		tokio::time::advance(Duration::from_millis(10)).await;

		// A's heap snapshot is stale after this touch, so eviction re-keys A and
		// picks B instead.
		a.touch();
		tokio::time::advance(Duration::from_millis(10)).await;
		pool.evict();

		assert!(!evicted_a.load(Ordering::Relaxed));
		assert!(evicted_b.load(Ordering::Relaxed));
	}

	#[test]
	fn dropped_charge_leaves_stale_heap_slot() {
		let pool = Pool::new(0);
		let (evicted_a, hook_a) = flag();
		let a = pool.register(hook_a);
		drop(a);

		// The heap still has A's slot, but the entry is gone: eviction discards it
		// without calling the hook.
		let (evicted_b, hook_b) = flag();
		let b = pool.register(hook_b);
		b.add(1);
		pool.evict();
		assert!(!evicted_a.load(Ordering::Relaxed));
		assert!(evicted_b.load(Ordering::Relaxed));
	}
}
