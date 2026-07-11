//! A keyed cache of weak handles with amortized garbage collection.

use std::{
	borrow::Borrow,
	collections::{HashMap, VecDeque},
	hash::Hash,
};

/// A weak handle whose liveness and identity the cache can probe.
///
/// Implemented by the weak consumer handles cached for dynamic dedup
/// ([`super::broadcast::WeakConsumer`], [`super::track::TrackWeak`]).
pub(crate) trait WeakEntry {
	/// True once the underlying channel has closed (every producer dropped).
	fn is_closed(&self) -> bool;

	/// True if `self` and `other` reference the same underlying channel.
	///
	/// Used to tell a live entry apart from a superseded twin: the same key can
	/// hold a fresh handle after an older one closed, leaving a stale copy in the
	/// probe ring.
	fn same_channel(&self, other: &Self) -> bool;
}

/// Number of ring slots probed per insert. A small constant keeps each insert
/// O(1) while the rotating window covers the whole cache over successive inserts.
/// Mirrors the bound in kio's `WaiterList`.
const GC_PROBE: usize = 2;

/// A map of weak handles that reclaims closed entries incrementally.
///
/// Deduplicates dynamically-served handles by key: [`get`](Self::get) returns a
/// live handle and drops a closed one, so a stale entry never resolves. The catch
/// is that a key requested once and never again would otherwise linger forever
/// (its handle closes but nothing re-looks-it-up), so a long-lived cache accreting
/// distinct one-shot keys leaks one dead entry per key.
///
/// Rather than an O(n) sweep on every insert, each [`insert`](Self::insert) probes
/// a bounded, rotating slice of the keys (like Redis sampling expired keys, or the
/// rotating cursor in kio's `WaiterList`) and drops any that have closed or been
/// superseded. The cache stays bounded by the number of currently-live handles
/// without ever scanning all of it.
pub(crate) struct WeakCache<K, V> {
	map: HashMap<K, V>,

	// Rotating probe queue, one entry per key plus transient junk: keys already
	// removed from `map`, and superseded duplicates (a key re-inserted with a fresh
	// handle after the old one closed). GC discards both when it reaches them,
	// telling a live entry from its superseded twin via `same_channel`, so the ring
	// stays bounded by the live-handle count. It can reach ~2x the map when one key
	// churns behind many live keys (its twins wait their turn under the probe cursor),
	// but that is still O(live), not a per-key leak.
	ring: VecDeque<(K, V)>,
}

impl<K, V> Default for WeakCache<K, V> {
	fn default() -> Self {
		Self {
			map: HashMap::new(),
			ring: VecDeque::new(),
		}
	}
}

impl<K, V> WeakCache<K, V>
where
	K: Clone + Eq + Hash,
	V: WeakEntry + Clone,
{
	/// Return a live handle for `key`, dropping and ignoring a closed one.
	pub fn get<Q>(&mut self, key: &Q) -> Option<V>
	where
		K: Borrow<Q>,
		Q: Hash + Eq + ?Sized,
	{
		match self.map.get(key) {
			Some(v) if !v.is_closed() => Some(v.clone()),
			Some(_) => {
				self.map.remove(key);
				None
			}
			None => None,
		}
	}

	/// Insert `value` under `key`, unless a *live* entry already holds it.
	///
	/// Returns the existing live handle (leaving it in place) when the key is
	/// already live, so a caller racing to serve the same key can dedup onto it
	/// rather than replace a good entry. Otherwise inserts `value` (replacing a
	/// closed entry), runs a bounded GC pass, and returns `None`.
	pub fn insert(&mut self, key: K, value: V) -> Option<V> {
		if let Some(existing) = self.map.get(&key)
			&& !existing.is_closed()
		{
			return Some(existing.clone());
		}

		self.map.insert(key.clone(), value.clone());
		self.gc();
		self.ring.push_back((key, value));
		None
	}

	/// Remove and return the entry for `key`, if any.
	pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
	where
		K: Borrow<Q>,
		Q: Hash + Eq + ?Sized,
	{
		// Prune the ring too. Unlike a lazily-dropped closed entry (which a later insert's GC
		// sweeps), an explicit remove has no follow-up insert to rely on, so scan it out now
		// rather than pin the handle (and its kio state) until one happens to arrive.
		self.ring.retain(|(k, _)| k.borrow() != key);
		self.map.remove(key)
	}

	/// True if `key` has an entry, whether live or closed-but-not-yet-reclaimed.
	pub fn contains_key<Q>(&self, key: &Q) -> bool
	where
		K: Borrow<Q>,
		Q: Hash + Eq + ?Sized,
	{
		self.map.contains_key(key)
	}

	/// Probe a bounded, rotating window of the ring, reclaiming dead entries.
	fn gc(&mut self) {
		for _ in 0..self.ring.len().min(GC_PROBE) {
			let Some((key, entry)) = self.ring.pop_front() else {
				break;
			};

			enum Probe {
				// Live and still the current handle for its key: rotate to the back.
				Keep,
				// Closed: reclaim the map entry.
				Reclaim,
				// Already removed, or superseded by a newer handle: drop the ring entry.
				Drop,
			}

			let probe = match self.map.get(&key) {
				Some(cur) if !cur.same_channel(&entry) => Probe::Drop,
				Some(cur) if cur.is_closed() => Probe::Reclaim,
				Some(_) => Probe::Keep,
				None => Probe::Drop,
			};

			match probe {
				Probe::Keep => self.ring.push_back((key, entry)),
				Probe::Reclaim => {
					self.map.remove(&key);
				}
				Probe::Drop => {}
			}
		}
	}
}

#[cfg(test)]
impl<K, V> WeakCache<K, V> {
	/// Number of entries currently in the map (live or not-yet-reclaimed).
	pub fn len(&self) -> usize {
		self.map.len()
	}

	/// Number of entries retained in the probe ring.
	pub fn ring_len(&self) -> usize {
		self.ring.len()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::{
		Arc,
		atomic::{AtomicBool, Ordering},
	};

	// A fake weak handle: liveness is a shared flag, identity is the `Arc` pointer.
	#[derive(Clone)]
	struct Fake(Arc<AtomicBool>);

	impl Fake {
		fn open() -> Self {
			Self(Arc::new(AtomicBool::new(false)))
		}

		fn close(&self) {
			self.0.store(true, Ordering::SeqCst);
		}
	}

	impl WeakEntry for Fake {
		fn is_closed(&self) -> bool {
			self.0.load(Ordering::SeqCst)
		}

		fn same_channel(&self, other: &Self) -> bool {
			Arc::ptr_eq(&self.0, &other.0)
		}
	}

	#[test]
	fn get_drops_closed() {
		let mut cache = WeakCache::default();
		let entry = Fake::open();
		cache.insert("a", entry.clone());
		assert!(cache.get("a").is_some());

		entry.close();
		assert!(cache.get("a").is_none(), "a closed entry must not resolve");
		assert_eq!(cache.len(), 0, "the closed entry is dropped on lookup");
	}

	#[test]
	fn remove_prunes_ring() {
		// An explicit remove must free the handle from the ring too, not just the map, so a
		// batch of removes with no follow-up insert doesn't pin the handles until GC runs.
		let mut cache = WeakCache::default();
		cache.insert("a", Fake::open());
		cache.insert("b", Fake::open());
		assert_eq!(cache.ring_len(), 2);

		assert!(cache.remove("a").is_some());
		assert_eq!(cache.len(), 1);
		assert_eq!(cache.ring_len(), 1, "remove must prune the ring, not just the map");
		assert!(cache.remove("a").is_none(), "already removed");
	}

	#[test]
	fn distinct_one_shot_paths_stay_bounded() {
		// The reported leak: many distinct keys, each served once then closed and never
		// requested again. Amortized GC must keep the map bounded rather than growing per key.
		let mut cache = WeakCache::default();
		for i in 0..1000 {
			let entry = Fake::open();
			cache.insert(i, entry.clone());
			entry.close();
		}
		assert!(cache.len() <= GC_PROBE + 1, "map grew unbounded: {}", cache.len());
		assert!(
			cache.ring_len() <= GC_PROBE + 1,
			"ring grew unbounded: {}",
			cache.ring_len()
		);
	}

	#[test]
	fn same_key_churn_does_not_leak() {
		// Re-serving the same key with a fresh handle after the old one closed leaves a stale
		// twin in the ring. `same_channel` must let GC discard it; a key-only ring would keep
		// rotating the twin forever (it points at a now-live key) and grow without bound.
		let mut cache = WeakCache::default();
		for _ in 0..1000 {
			let entry = Fake::open();
			cache.insert("hot", entry.clone());
			entry.close();
		}
		assert_eq!(cache.len(), 1, "one key must map to at most one entry");
		assert!(
			cache.ring_len() <= 2,
			"superseded twins must not accumulate: {}",
			cache.ring_len()
		);
	}

	#[test]
	fn insert_dedups_live_replaces_closed() {
		let mut cache = WeakCache::default();
		let first = Fake::open();
		assert!(
			cache.insert("a", first.clone()).is_none(),
			"first insert takes the slot"
		);

		// A live entry is kept: the racing insert gets it back and is not applied.
		let second = Fake::open();
		let existing = cache.insert("a", second.clone()).expect("live entry returned");
		assert!(existing.same_channel(&first), "the existing live entry wins");
		assert!(cache.get("a").expect("still present").same_channel(&first));

		// Once the live entry closes, the name is free and a fresh insert replaces it.
		first.close();
		let third = Fake::open();
		assert!(cache.insert("a", third.clone()).is_none(), "closed entry is replaced");
		assert!(cache.get("a").expect("present").same_channel(&third));
	}
}
