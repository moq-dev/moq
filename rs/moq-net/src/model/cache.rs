//! A shared RAM LRU cache for groups, attachable at the origin, broadcast, or track level.
//!
//! By default a track keeps only its latest group: a live subscriber can always grab the
//! current group, but nothing older is retained. Attach a [`Cache`] to keep more history in
//! RAM, bounded by a byte budget and a wall-clock age. The budget is shared: clone the same
//! [`Cache`] handle across many tracks (or whole broadcasts / origins) and they all draw from
//! one `max_bytes` total and one `max_age`. Two distinct [`Cache`] instances have independent
//! budgets.
//!
//! Eviction is LRU by wall-clock last-access time (when a group was last read), not by media
//! timestamp or arrival order. A group is evicted once it is older than `max_age` since its
//! last access, or once the shared total exceeds `max_bytes` (least-recently-accessed first).
//! A track's current latest group is never handed to the cache, so it is never evicted out
//! from under a live subscriber.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use web_async::time::Instant;

use super::GroupProducer;
use crate::Error;

/// Configuration for a [`Cache`]: the shared byte budget and the wall-clock age bound.
///
/// Construct via [`Config::default`] (an empty, do-nothing budget) and the `with_*` setters,
/// then build a handle with [`Cache::new`]. New fields stay additive, so build via `default()`
/// plus setters rather than a struct literal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Config {
	/// Maximum total bytes retained across every track sharing this cache. Summed over all
	/// cached (non-latest) groups; the least-recently-accessed groups are evicted once the
	/// total would exceed this. Defaults to `0` (retain nothing beyond the latest group).
	pub max_bytes: u64,

	/// Maximum wall-clock age since a group was last accessed before it is evicted. Measured
	/// with [`web_async::time::Instant`], so `tokio::time::pause` controls it in tests. Defaults
	/// to [`Duration::MAX`] (no age bound; eviction is by `max_bytes` alone).
	pub max_age: Duration,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			max_bytes: 0,
			max_age: Duration::MAX,
		}
	}
}

impl Config {
	/// Set the shared byte budget, returning `self` for chaining.
	pub fn with_max_bytes(mut self, max_bytes: u64) -> Self {
		self.max_bytes = max_bytes;
		self
	}

	/// Set the wall-clock age bound, returning `self` for chaining.
	pub fn with_max_age(mut self, max_age: Duration) -> Self {
		self.max_age = max_age;
		self
	}
}

/// A shared, cheaply cloneable handle to a RAM LRU group cache.
///
/// Everything attached with the *same* handle (clones share it) draws from one budget. Attach
/// it via [`crate::OriginProducer::with_cache`], [`crate::BroadcastProducer::with_cache`], or
/// [`crate::TrackProducer::with_cache`]; the most specific attach point wins (track over
/// broadcast over origin). Clone the handle to share a single budget across many of them.
///
/// See the [module docs](self) for the eviction policy.
#[derive(Clone)]
#[non_exhaustive]
pub struct Cache {
	state: Arc<Mutex<State>>,
}

impl Cache {
	/// Create a cache with the given [`Config`].
	pub fn new(config: Config) -> Self {
		Self {
			state: Arc::new(Mutex::new(State {
				max_bytes: config.max_bytes,
				max_age: config.max_age,
				total_bytes: 0,
				next_id: 0,
				entries: HashMap::new(),
				lru: BTreeMap::new(),
			})),
		}
	}

	/// Whether two handles share the same underlying budget (one is a clone of the other).
	pub fn is_clone(&self, other: &Self) -> bool {
		Arc::ptr_eq(&self.state, &other.state)
	}

	/// Register a group with the cache, recording its byte size and an initial access time of
	/// `now`, then evict anything now over budget. Returns a [`Token`] identifying the entry.
	///
	/// Called by a track when a group stops being the latest (a newer group arrived), handing
	/// the now-evictable group to the shared budget.
	pub(crate) fn insert(&self, group: GroupProducer, bytes: u64, now: Instant) -> Token {
		let mut state = self.state.lock().unwrap();
		let id = state.next_id;
		state.next_id += 1;
		state.total_bytes += bytes;
		state.lru.insert(Key { last_access: now, id }, ());
		state.entries.insert(
			id,
			Entry {
				group,
				bytes,
				last_access: now,
			},
		);
		state.evict(now);
		Token(id)
	}

	/// Run eviction first (age then byte budget), then, if the entry is still present and not
	/// aborted, bump its last-access time to `now` and return `true`.
	///
	/// Eviction runs before the recency refresh so a group already past `max_age` is dropped on
	/// read rather than revived by the access. Returns `false` if the entry was evicted (now or
	/// earlier), so the caller treats the read as a miss.
	pub(crate) fn touch(&self, token: Token, now: Instant) -> bool {
		let mut state = self.state.lock().unwrap();
		state.evict(now);

		let Some(entry) = state.entries.get_mut(&token.0) else {
			return false;
		};
		if entry.group.is_aborted() {
			return false;
		}
		let old = entry.last_access;
		entry.last_access = now;
		state.lru.remove(&Key {
			last_access: old,
			id: token.0,
		});
		state.lru.insert(
			Key {
				last_access: now,
				id: token.0,
			},
			(),
		);
		true
	}

	/// Remove a registered group from the cache without aborting it (the track is dropping it
	/// for its own reasons, e.g. the track is closing).
	pub(crate) fn remove(&self, token: Token) {
		let mut state = self.state.lock().unwrap();
		state.drop_id(token.0, false);
	}

	/// Evict groups over budget, aborting each so any reader unblocks with [`Error::Old`].
	/// `now` is the current wall clock. Called by a track on insert and on each retain pass.
	pub(crate) fn evict(&self, now: Instant) {
		let mut state = self.state.lock().unwrap();
		state.evict(now);
	}
}

/// Identifies a track's group within the shared cache. The track holds one per cached group.
#[derive(Clone, Copy)]
pub(crate) struct Token(u64);

/// Ordering key for the LRU index: least-recently-accessed sorts first. `id` is a monotonic
/// tie-breaker so two groups accessed at the same instant stay distinct and ordered.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
	last_access: Instant,
	id: u64,
}

struct Entry {
	/// A clone of the group's producer, held so its frame buffers stay in memory while cached.
	/// Aborting it on eviction frees the buffers and unblocks any waiting reader.
	group: GroupProducer,
	bytes: u64,
	last_access: Instant,
}

struct State {
	max_bytes: u64,
	max_age: Duration,
	total_bytes: u64,
	/// Monotonic id counter feeding each entry's tie-breaker.
	next_id: u64,
	/// Entries by id, the source of truth for bytes and last-access.
	entries: HashMap<u64, Entry>,
	/// LRU index into `entries`, ordered least-recently-accessed first.
	lru: BTreeMap<Key, ()>,
}

impl State {
	fn evict(&mut self, now: Instant) {
		// Age first: drop anything not accessed within max_age.
		if self.max_age != Duration::MAX {
			while let Some((key, _)) = self.lru.iter().next() {
				let key = *key;
				if now.saturating_duration_since(key.last_access) <= self.max_age {
					break;
				}
				self.drop_id(key.id, true);
			}
		}

		// A cached group can keep growing after it was superseded (late frames on an
		// out-of-order group), so the size captured at insert undercounts its real RAM. Refresh
		// each entry's tracked size from the live group before deciding the byte budget, so the
		// budget reflects current usage rather than the snapshot at supersession.
		self.refresh_sizes();

		// Then byte budget: drop least-recently-accessed until within max_bytes.
		while self.total_bytes > self.max_bytes {
			let Some((key, _)) = self.lru.iter().next() else {
				break;
			};
			let id = key.id;
			self.drop_id(id, true);
		}
	}

	/// Re-query each entry's current buffered size, keeping `total_bytes` in sync with the live
	/// groups. Cheap (a read lock per group); only called on the eviction path.
	fn refresh_sizes(&mut self) {
		for entry in self.entries.values_mut() {
			let current = entry.group.cached_size();
			if current != entry.bytes {
				self.total_bytes = self.total_bytes - entry.bytes + current;
				entry.bytes = current;
			}
		}
	}

	/// Remove an entry by id, optionally aborting its group. Keeps `lru` and `total_bytes`
	/// in sync with `entries`.
	fn drop_id(&mut self, id: u64, abort: bool) {
		let Some(mut entry) = self.entries.remove(&id) else {
			return;
		};
		self.total_bytes -= entry.bytes;
		self.lru.remove(&Key {
			last_access: entry.last_access,
			id,
		});
		if abort {
			// Surface `Error::Old` to a parked reader rather than letting it hang, matching the
			// track-local eviction path.
			let _ = entry.group.abort(Error::Old);
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use crate::Group;

	/// Build a group carrying `bytes` of real frame data, so `cached_size()` matches the byte
	/// count handed to `insert` (the cache refreshes from the live group during eviction).
	fn group(seq: u64, bytes: usize) -> GroupProducer {
		let mut g = GroupProducer::new(Group { sequence: seq }, None);
		if bytes > 0 {
			g.write_frame(bytes::Bytes::from(vec![0u8; bytes])).unwrap();
		}
		g
	}

	#[tokio::test]
	async fn bytes_evicts_least_recently_accessed() {
		tokio::time::pause();
		let cache = Cache::new(Config::default().with_max_bytes(100));

		let now = Instant::now();
		let g0 = group(0, 60);
		let g1 = group(1, 60);
		let _t0 = cache.insert(g0.clone(), g0.cached_size(), now);
		// The second insert pushes the total to 120 > 100, evicting the LRU (g0).
		let _t1 = cache.insert(g1.clone(), g1.cached_size(), now);

		assert!(g0.is_aborted(), "least recently inserted group evicted");
		assert!(!g1.is_aborted());
	}

	#[tokio::test]
	async fn touch_updates_recency() {
		tokio::time::pause();
		let cache = Cache::new(Config::default().with_max_bytes(120));

		let now = Instant::now();
		let g0 = group(0, 60);
		let g1 = group(1, 60);
		let t0 = cache.insert(g0.clone(), g0.cached_size(), now);
		let _t1 = cache.insert(g1.clone(), g1.cached_size(), now);

		// Access g0 so it becomes most-recently-used; g1 is now the LRU victim.
		tokio::time::advance(Duration::from_secs(1)).await;
		let later = Instant::now();
		assert!(cache.touch(t0, later), "g0 still cached, touch succeeds");

		// A third group pushes over budget; the least-recently-accessed (g1) is evicted.
		let g2 = group(2, 60);
		let _t2 = cache.insert(g2.clone(), g2.cached_size(), later);

		assert!(!g0.is_aborted(), "recently accessed group survives");
		assert!(g1.is_aborted(), "least recently accessed group evicted");
		assert!(!g2.is_aborted());
	}

	#[tokio::test]
	async fn age_evicts_by_wall_clock() {
		tokio::time::pause();
		let cache = Cache::new(
			Config::default()
				.with_max_bytes(u64::MAX)
				.with_max_age(Duration::from_secs(5)),
		);

		let now = Instant::now();
		let g0 = group(0, 10);
		let _t0 = cache.insert(g0.clone(), g0.cached_size(), now);

		tokio::time::advance(Duration::from_secs(6)).await;
		cache.evict(Instant::now());
		assert!(g0.is_aborted(), "group older than max_age is evicted");
	}

	#[tokio::test]
	async fn touch_evicts_aged_group_instead_of_reviving() {
		tokio::time::pause();
		let cache = Cache::new(
			Config::default()
				.with_max_bytes(u64::MAX)
				.with_max_age(Duration::from_secs(5)),
		);

		let now = Instant::now();
		let g0 = group(0, 10);
		let t0 = cache.insert(g0.clone(), g0.cached_size(), now);

		// Past max_age: a read must evict the stale group, not refresh its recency.
		tokio::time::advance(Duration::from_secs(6)).await;
		assert!(
			!cache.touch(t0, Instant::now()),
			"aged-out group is a miss, not revived"
		);
		assert!(g0.is_aborted(), "the stale group is evicted on read");
	}

	#[tokio::test]
	async fn remove_frees_budget_without_abort() {
		tokio::time::pause();
		let cache = Cache::new(Config::default().with_max_bytes(50));

		let now = Instant::now();
		let g0 = group(0, 40);
		let t0 = cache.insert(g0.clone(), g0.cached_size(), now);
		cache.remove(t0);

		// g0 is no longer counted, so a fresh 40-byte group fits without eviction.
		let g1 = group(1, 40);
		let _t1 = cache.insert(g1.clone(), g1.cached_size(), now);
		assert!(!g0.is_aborted());
		assert!(!g1.is_aborted());
	}

	#[tokio::test]
	async fn grown_group_counted_at_current_size() {
		tokio::time::pause();
		// Budget holds a ~10-byte group. Insert a small group, then let it grow via late
		// frames; a second insert must evict the grown group based on its current size.
		let cache = Cache::new(Config::default().with_max_bytes(25));

		let now = Instant::now();
		let mut g0 = group(0, 10);
		let _t0 = cache.insert(g0.clone(), g0.cached_size(), now);

		// A late frame grows g0 from 10 to 30 bytes after it was cached.
		g0.write_frame(bytes::Bytes::from(vec![0u8; 20])).unwrap();

		// Inserting a tiny second group triggers eviction; with the refreshed size (30 > 25) g0
		// is over budget and evicted.
		let g1 = group(1, 1);
		let _t1 = cache.insert(g1.clone(), g1.cached_size(), now);
		assert!(
			g0.is_aborted(),
			"grown group is evicted at its current size, not its insert size"
		);
	}
}
