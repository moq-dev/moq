//! A keyed cache of weak handles with amortized garbage collection.

use std::{borrow::Borrow, collections::hash_map::RandomState, hash::Hash};

/// A weak handle whose liveness and identity the model can probe.
///
/// Implemented by the weak consumer handles cached for dynamic dedup
/// ([`super::broadcast::WeakConsumer`], [`super::track::TrackWeak`]).
pub(super) trait WeakEntry {
	/// True once the underlying channel has closed (every producer dropped).
	fn is_closed(&self) -> bool;

	/// True if both handles reference the same underlying channel.
	fn same_channel(&self, other: &Self) -> bool;
}

/// Number of entries probed per insert or owner removal.
const GC_PROBE: usize = 2;

/// A keyed cache of weak handles, ordered for bounded, rotating garbage collection.
///
/// Each handle has one slot: keyed removal unlinks it immediately, including when
/// no later operation arrives. Inserts and owner removals also probe a bounded
/// window, so closed one-shot keys do not accumulate or require a whole-table scan.
pub(super) struct WeakCache<K, V> {
	// Preserve the standard map's hasher for peer-chosen names.
	map: hashlink::LinkedHashMap<K, V, RandomState>,
}

impl<K, V> Default for WeakCache<K, V> {
	fn default() -> Self {
		Self {
			map: Default::default(),
		}
	}
}

impl<K, V> WeakCache<K, V>
where
	K: Eq + Hash,
	V: WeakEntry + Clone,
{
	/// Return a live handle for `key`, ignoring a closed one.
	///
	/// Takes `&self` so a hit read through a shared guard writes nothing; a caller
	/// reclaims a closed entry it finds with [`Self::remove`].
	pub fn get<Q>(&self, key: &Q) -> Option<V>
	where
		K: Borrow<Q>,
		Q: Hash + Eq + ?Sized,
	{
		self.map.get(key).filter(|v| !v.is_closed()).cloned()
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

		self.map.insert(key, value);
		self.gc();
		None
	}

	/// Iterate every entry, closed ones included, for a teardown walk. A closed
	/// entry may still carry side state the walk must settle (a cancelled front's
	/// parked requesters), so nothing is filtered here.
	pub fn values(&self) -> impl Iterator<Item = &V> {
		self.map.values()
	}

	/// Remove and return the entry for `key`, if any.
	pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
	where
		K: Borrow<Q>,
		Q: Hash + Eq + ?Sized,
	{
		self.map.remove(key)
	}

	/// Remove the entry for `key`, live or closed, only when it satisfies `f`, so an
	/// owner taking out its own entry leaves a successor's alone.
	///
	/// Also probes for closed entries, so owners leaving reclaim one-shot keys
	/// without making another insert necessary.
	pub fn remove_if<Q>(&mut self, key: &Q, f: impl FnOnce(&V) -> bool) -> Option<V>
	where
		K: Borrow<Q>,
		Q: Hash + Eq + ?Sized,
	{
		let removed = match self.map.get(key) {
			Some(entry) if f(entry) => self.map.remove(key),
			_ => None,
		};
		if removed.is_some() {
			self.gc();
		}
		removed
	}

	/// True if `key` has an entry, whether live or closed-but-not-yet-reclaimed.
	pub fn contains_key<Q>(&self, key: &Q) -> bool
	where
		K: Borrow<Q>,
		Q: Hash + Eq + ?Sized,
	{
		self.map.contains_key(key)
	}

	/// Iterate the cached handles, including closed entries GC hasn't reclaimed
	/// yet (callers filter on liveness themselves).
	pub fn iter(&self) -> impl Iterator<Item = &V> {
		self.map.values()
	}

	/// Probe a bounded, rotating window, unlinking closed entries in place.
	fn gc(&mut self) {
		for _ in 0..self.map.len().min(GC_PROBE) {
			let Some(mut entry) = self.map.front_entry() else { break };
			if entry.get().is_closed() {
				entry.remove();
			} else {
				entry.to_back();
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
	fn get_ignores_closed() {
		let mut cache = WeakCache::default();
		let entry = Fake::open();
		cache.insert("a", entry.clone());
		assert!(cache.get("a").is_some());

		entry.close();
		assert!(cache.get("a").is_none(), "a closed entry must not resolve");
	}

	#[test]
	fn removals_release_handles_without_followup() {
		for conditional in [false, true] {
			let mut cache = WeakCache::default();
			let entry = Fake::open();
			let weak = Arc::downgrade(&entry.0);
			for key in 0..1000 {
				cache.insert(key, Fake::open());
			}
			cache.insert(1000, entry);
			let removed = if conditional {
				cache.remove_if(&1000, |_| true)
			} else {
				cache.remove(&1000)
			};
			assert!(removed.is_some());
			drop(removed);
			assert!(weak.upgrade().is_none(), "the idle cache still pins a removed handle");
		}
	}

	/// Owners leaving one by one, live or closed, with nothing inserted after them: each
	/// removal leaves a successor's entry alone without scanning the cache.
	#[test]
	fn owners_leaving_preserve_the_successor() {
		let mut cache = WeakCache::default();
		let entries: Vec<Fake> = (0..1000).map(|_| Fake::open()).collect();
		for (i, entry) in entries.iter().enumerate() {
			cache.insert(i, entry.clone());
		}

		// A successor holds key 0: its predecessor's removal finds someone else's entry.
		entries[0].close();
		let successor = Fake::open();
		cache.insert(0, successor.clone());
		assert!(cache.remove_if(&0, |entry| entry.same_channel(&entries[0])).is_none());
		assert!(cache.get(&0).expect("the successor stays").same_channel(&successor));

		for (i, entry) in entries.iter().enumerate().skip(1) {
			assert!(cache.remove_if(&i, |current| current.same_channel(entry)).is_some());
		}
		assert_eq!(cache.len(), 1, "only the successor is left");
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
	}

	#[test]
	fn same_key_churn_does_not_leak() {
		// Re-serving the same key with a fresh handle after the old one closed must replace its
		// old slot, without accumulating superseded handles.
		let mut cache = WeakCache::default();
		for _ in 0..1000 {
			let entry = Fake::open();
			cache.insert("hot", entry.clone());
			entry.close();
		}
		assert_eq!(cache.len(), 1, "one key must map to at most one entry");
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
