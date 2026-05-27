//! Generic stats publishing for moq-net sessions.
//!
//! [`Stats`] aggregates per-broadcast counter bumps for traffic this relay
//! node is handling and publishes them on a single `<prefix>/node/<node>`
//! broadcast (or `<prefix>/node` when no node is configured). The broadcast
//! carries four tracks, one per `(tier, role)` pair:
//!
//! * `publisher.json.gz`           : external (e.g. customer) egress
//! * `subscriber.json.gz`          : external ingress
//! * `internal/publisher.json.gz`  : internal (e.g. mTLS cluster peer) egress
//! * `internal/subscriber.json.gz` : internal ingress
//!
//! Each frame is a gzipped JSON object mapping broadcast path to a cumulative
//! counter snapshot. Tier, role, and node are implied by the track and
//! broadcast paths, so they aren't repeated inside the frame. A broadcast
//! appears in the frame for a given (tier, role) while it has at least one
//! active subscription, and lingers for `retention_ticks` ticks after the last
//! one drops so short disconnects don't immediately erase the entry. A
//! downstream aggregator computes rates from successive cumulative snapshots
//! and slices the data however a dashboard wants.
//!
//! A caller hands each session a tier-scoped [`StatsHandle`] (built from the
//! single shared [`Stats`] via [`Stats::tier`]) which determines which counter
//! set its bumps land in. Multiple relays in the same cluster origin can
//! coexist by giving each one a distinct `<node>` suffix on the advertised
//! path. The suffix itself may be multi-segment (e.g. `sjc/1`, `sjc/2`) so a
//! region with multiple hosts can nest under a shared region key without
//! colliding.
//!
//! # Disabled stats
//!
//! [`Stats::disabled`] (and the matching [`Default`] impl) returns a no-op
//! aggregator. All counter bumps through it are silently dropped and no
//! snapshot task is ever spawned, so call sites can hold a [`StatsHandle`]
//! unconditionally instead of threading an `Option`.
//!
//! # Lifecycle
//!
//! No background work runs until something happens worth reporting. The first
//! `broadcast()` call on any path spawns the snapshot task, which constructs
//! the stats broadcast, ticks at the configured interval, and writes a frame
//! per (tier, role) track. The task exits once the entry map has been empty
//! for `2 * retention_ticks`, dropping the broadcast and unannouncing. The
//! next `broadcast()` call respawns it.
//!
//! # Idle frame skipping
//!
//! On each tick the task compares the just-built per-(tier, role) JSON payload
//! against the last one it emitted and writes a frame only when something
//! changed. New subscribers still pick up a baseline immediately because
//! track-latest semantics retain the most recent emitted frame.
//!
//! # Cycles
//!
//! Calling [`StatsHandle::broadcast`] for a path under the configured
//! top-level prefix returns an empty handle whose bumps no-op. This breaks
//! the feedback loop where serving a `<top-prefix>/...` broadcast would
//! itself generate more stats traffic.

use std::{
	collections::{BTreeMap, HashMap},
	io::Write,
	sync::{
		Arc, Weak,
		atomic::{AtomicU64, Ordering},
	},
	time::Duration,
};

use flate2::{Compression, write::GzEncoder};
use serde::Serialize;
use web_async::{Lock, spawn};

use crate::{AsPath, Broadcast, Origin, OriginProducer, Path, PathOwned, Track, TrackProducer};

/// Cumulative atomic counters for a single (tier, role) on a broadcast.
#[derive(Default, Debug)]
#[non_exhaustive]
pub struct Counters {
	pub broadcasts: AtomicU64,
	pub broadcasts_closed: AtomicU64,
	pub subscriptions: AtomicU64,
	pub subscriptions_closed: AtomicU64,
	pub bytes: AtomicU64,
	pub frames: AtomicU64,
	pub groups: AtomicU64,
}

impl Counters {
	fn snapshot(&self) -> Snapshot {
		Snapshot {
			broadcasts: self.broadcasts.load(Ordering::Relaxed),
			broadcasts_closed: self.broadcasts_closed.load(Ordering::Relaxed),
			subscriptions: self.subscriptions.load(Ordering::Relaxed),
			subscriptions_closed: self.subscriptions_closed.load(Ordering::Relaxed),
			bytes: self.bytes.load(Ordering::Relaxed),
			frames: self.frames.load(Ordering::Relaxed),
			groups: self.groups.load(Ordering::Relaxed),
		}
	}

	fn active(&self) -> bool {
		self.subscriptions.load(Ordering::Relaxed) > self.subscriptions_closed.load(Ordering::Relaxed)
	}
}

/// Distinguishes traffic classes so a single [`Stats`] can record customer-facing
/// and cluster-peer traffic separately. The four `(Tier, Role)` combinations are
/// the four tracks published on the per-node stats broadcast.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Tier {
	External,
	Internal,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Role {
	Publisher,
	Subscriber,
}

const NUM_SLOTS: usize = 4;

fn slot_index(tier: Tier, role: Role) -> usize {
	match (tier, role) {
		(Tier::External, Role::Publisher) => 0,
		(Tier::External, Role::Subscriber) => 1,
		(Tier::Internal, Role::Publisher) => 2,
		(Tier::Internal, Role::Subscriber) => 3,
	}
}

const TRACK_NAMES: [&str; NUM_SLOTS] = [
	"publisher.json.gz",
	"subscriber.json.gz",
	"internal/publisher.json.gz",
	"internal/subscriber.json.gz",
];

/// Top-level stats aggregator. Cheap to clone (`Arc` inside). One instance per
/// relay; sessions get tier-scoped handles via [`Stats::tier`].
#[derive(Clone)]
pub struct Stats {
	inner: Arc<StatsInner>,
}

struct StatsInner {
	prefix: PathOwned,
	/// `None` when stats are disabled; otherwise the path the stats broadcast
	/// is published on. Computed once at construction so the snapshot task
	/// doesn't have to recompute it per spawn.
	advertised: Option<PathOwned>,
	origin: OriginProducer,
	tick: Duration,
	retention_ticks: u32,
	entries: Lock<HashMap<PathOwned, Arc<BroadcastEntry>>>,
	task: Lock<Option<()>>,
	/// Monotonic tick counter; `0` is a sentinel meaning "no tick has run yet"
	/// so a slot's `last_active_tick == 0` reliably means "never observed
	/// active". Counts up from `1`.
	tick_counter: AtomicU64,
}

struct BroadcastEntry {
	path: PathOwned,
	slots: [Counters; NUM_SLOTS],
	/// Tick index of the most recent sampling tick that observed an active
	/// subscription in this slot. `0` means never observed.
	last_active_tick: [AtomicU64; NUM_SLOTS],
}

impl BroadcastEntry {
	fn new(path: PathOwned) -> Self {
		Self {
			path,
			slots: Default::default(),
			last_active_tick: Default::default(),
		}
	}
}

impl Stats {
	/// Build a new stats aggregator.
	///
	/// * `prefix` is the top-level path under which stats are published, e.g.
	///   `.stats`. The full advertised path is `<prefix>/node/<node>` (or
	///   `<prefix>/node` when `node` is `None`).
	/// * `tick` is the interval between snapshot publishes.
	/// * `retention_ticks` is how many ticks an idle broadcast lingers in the
	///   emitted frame after its last observed active subscription, so a
	///   short reconnect window doesn't erase the entry.
	/// * `node` disambiguates broadcasts published by different relays into a
	///   shared cluster origin. Set this on every node in multi-relay
	///   deployments. The value may be multi-segment (e.g. `sjc/1`, `sjc/2`)
	///   so a region with multiple hosts can nest under a shared region key.
	///   `None` (or an empty path after normalization) omits the suffix.
	/// * `origin` is the [`OriginProducer`] that receives `publish_broadcast`
	///   for the stats broadcast.
	pub fn new(
		prefix: impl Into<PathOwned>,
		tick: Duration,
		retention_ticks: u32,
		node: impl Into<Option<PathOwned>>,
		origin: OriginProducer,
	) -> Self {
		let prefix = prefix.into();
		// An empty path after normalization is indistinguishable from "no node
		// set"; collapse it so downstream code only sees a single representation.
		let node = node.into().filter(|p| !p.is_empty());
		let advertised = Some(advertised_path(&prefix, node.as_ref().map(|p| p.as_str())));
		Self {
			inner: Arc::new(StatsInner {
				prefix,
				advertised,
				origin,
				tick,
				retention_ticks,
				entries: Lock::default(),
				task: Lock::new(None),
				tick_counter: AtomicU64::new(0),
			}),
		}
	}

	/// A no-op aggregator. Counter bumps are silently dropped and no snapshot
	/// task is ever spawned. Use this when stats are disabled so call sites
	/// can hold a [`Stats`] (or [`StatsHandle`]) unconditionally.
	pub fn disabled() -> Self {
		Self {
			inner: Arc::new(StatsInner {
				prefix: PathOwned::default(),
				advertised: None,
				origin: Origin::random().produce(),
				tick: Duration::from_secs(1),
				retention_ticks: 0,
				entries: Lock::default(),
				task: Lock::new(None),
				tick_counter: AtomicU64::new(0),
			}),
		}
	}

	/// Returns the configured top-level prefix.
	pub fn prefix(&self) -> &Path<'static> {
		&self.inner.prefix
	}

	/// Returns a tier-scoped handle. Bumps through this handle land in the
	/// tier's counters.
	pub fn tier(&self, tier: Tier) -> StatsHandle {
		StatsHandle {
			stats: self.clone(),
			tier,
		}
	}

	fn entry(&self, path: impl AsPath) -> Option<Arc<BroadcastEntry>> {
		// Disabled aggregator has no advertised broadcast; never allocate state.
		self.inner.advertised.as_ref()?;
		let path = path.as_path();
		// Skip our own stats broadcasts (and any sibling category under the
		// same prefix) so serving a stats broadcast doesn't generate more
		// stats.
		if path.has_prefix(&self.inner.prefix) {
			return None;
		}
		let owned = path.to_owned();
		let arc = {
			let mut entries = self.inner.entries.lock();
			entries
				.entry(owned.clone())
				.or_insert_with(|| Arc::new(BroadcastEntry::new(owned)))
				.clone()
		};
		ensure_task(self);
		Some(arc)
	}
}

impl Default for Stats {
	fn default() -> Self {
		Self::disabled()
	}
}

/// Tier-scoped wrapper around [`Stats`]. What [`crate::Client::with_stats`] and
/// [`crate::Server::with_stats`] accept. Cheap to clone.
#[derive(Clone)]
pub struct StatsHandle {
	stats: Stats,
	tier: Tier,
}

impl StatsHandle {
	/// A no-op handle. See [`Stats::disabled`].
	pub fn disabled() -> Self {
		Stats::disabled().tier(Tier::External)
	}

	/// The aggregator this handle is tied to.
	pub fn parent(&self) -> &Stats {
		&self.stats
	}

	/// The tier this handle bumps into.
	pub fn tier(&self) -> Tier {
		self.tier
	}

	/// Returns a per-broadcast handle scoped to this tier.
	///
	/// Paths under the aggregator's configured `prefix` return an empty handle
	/// whose bumps are no-ops. This keeps stats traffic from feeding back into
	/// the aggregator.
	pub fn broadcast(&self, path: impl AsPath) -> BroadcastStats {
		BroadcastStats {
			entry: self.stats.entry(path),
			tier: self.tier,
		}
	}
}

impl Default for StatsHandle {
	fn default() -> Self {
		Self::disabled()
	}
}

/// A per-broadcast, tier-scoped handle. Cheap to clone.
///
/// Open a broadcast-lifetime guard with [`Self::publisher`] / [`Self::subscriber`],
/// or skip straight to a track guard with [`Self::publisher_track`] /
/// [`Self::subscriber_track`] when the broadcast's lifetime is tracked
/// elsewhere.
#[derive(Clone)]
pub struct BroadcastStats {
	entry: Option<Arc<BroadcastEntry>>,
	tier: Tier,
}

impl BroadcastStats {
	/// True if this handle has no underlying entry (path was under the
	/// aggregator's own prefix, or stats are disabled). All bumps through an
	/// empty handle are no-ops.
	pub fn is_empty(&self) -> bool {
		self.entry.is_none()
	}

	/// Open a broadcast-lifetime guard for the publisher (egress) role.
	/// Bumps `broadcasts` on construction and `broadcasts_closed` on drop.
	pub fn publisher(&self) -> PublisherStats {
		if let Some(entry) = &self.entry {
			entry.slots[slot_index(self.tier, Role::Publisher)]
				.broadcasts
				.fetch_add(1, Ordering::Relaxed);
		}
		PublisherStats {
			entry: self.entry.clone(),
			tier: self.tier,
		}
	}

	/// Open a broadcast-lifetime guard for the subscriber (ingress) role.
	/// Bumps `broadcasts` on construction and `broadcasts_closed` on drop.
	pub fn subscriber(&self) -> SubscriberStats {
		if let Some(entry) = &self.entry {
			entry.slots[slot_index(self.tier, Role::Subscriber)]
				.broadcasts
				.fetch_add(1, Ordering::Relaxed);
		}
		SubscriberStats {
			entry: self.entry.clone(),
			tier: self.tier,
		}
	}

	/// Open a publisher-track guard without bumping the broadcast counters.
	///
	/// `_name` is currently unused; counters are per-broadcast only. Reserved
	/// for future per-track granularity.
	pub fn publisher_track(&self, _name: &str) -> PublisherTrack {
		if let Some(entry) = &self.entry {
			entry.slots[slot_index(self.tier, Role::Publisher)]
				.subscriptions
				.fetch_add(1, Ordering::Relaxed);
		}
		PublisherTrack {
			entry: self.entry.clone(),
			tier: self.tier,
		}
	}

	/// Subscriber-side counterpart to [`Self::publisher_track`].
	pub fn subscriber_track(&self, _name: &str) -> SubscriberTrack {
		if let Some(entry) = &self.entry {
			entry.slots[slot_index(self.tier, Role::Subscriber)]
				.subscriptions
				.fetch_add(1, Ordering::Relaxed);
		}
		SubscriberTrack {
			entry: self.entry.clone(),
			tier: self.tier,
		}
	}
}

/// RAII broadcast guard for the publisher role. See [`BroadcastStats::publisher`].
#[must_use = "drop the guard to record the broadcast as closed"]
pub struct PublisherStats {
	entry: Option<Arc<BroadcastEntry>>,
	tier: Tier,
}

impl PublisherStats {
	/// Open a track-subscription guard. Bumps `subscriptions` on construction
	/// and `subscriptions_closed` on drop.
	pub fn track(&self, name: &str) -> PublisherTrack {
		BroadcastStats {
			entry: self.entry.clone(),
			tier: self.tier,
		}
		.publisher_track(name)
	}
}

impl Drop for PublisherStats {
	fn drop(&mut self) {
		if let Some(entry) = &self.entry {
			entry.slots[slot_index(self.tier, Role::Publisher)]
				.broadcasts_closed
				.fetch_add(1, Ordering::Relaxed);
		}
	}
}

/// RAII broadcast guard for the subscriber role. See [`BroadcastStats::subscriber`].
#[must_use = "drop the guard to record the broadcast as closed"]
pub struct SubscriberStats {
	entry: Option<Arc<BroadcastEntry>>,
	tier: Tier,
}

impl SubscriberStats {
	/// Open a track-subscription guard. Mirrors [`PublisherStats::track`].
	pub fn track(&self, name: &str) -> SubscriberTrack {
		BroadcastStats {
			entry: self.entry.clone(),
			tier: self.tier,
		}
		.subscriber_track(name)
	}
}

impl Drop for SubscriberStats {
	fn drop(&mut self) {
		if let Some(entry) = &self.entry {
			entry.slots[slot_index(self.tier, Role::Subscriber)]
				.broadcasts_closed
				.fetch_add(1, Ordering::Relaxed);
		}
	}
}

/// RAII subscription guard for the publisher role.
#[must_use = "drop the guard to record the subscription as closed"]
pub struct PublisherTrack {
	entry: Option<Arc<BroadcastEntry>>,
	tier: Tier,
}

impl PublisherTrack {
	/// Bumps `frames` once.
	pub fn frame(&self) {
		if let Some(entry) = &self.entry {
			entry.slots[slot_index(self.tier, Role::Publisher)]
				.frames
				.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// Bumps `bytes` by `n`.
	pub fn bytes(&self, n: u64) {
		if let Some(entry) = &self.entry {
			entry.slots[slot_index(self.tier, Role::Publisher)]
				.bytes
				.fetch_add(n, Ordering::Relaxed);
		}
	}

	/// Bumps `groups` once.
	pub fn group(&self) {
		if let Some(entry) = &self.entry {
			entry.slots[slot_index(self.tier, Role::Publisher)]
				.groups
				.fetch_add(1, Ordering::Relaxed);
		}
	}
}

impl Drop for PublisherTrack {
	fn drop(&mut self) {
		if let Some(entry) = &self.entry {
			entry.slots[slot_index(self.tier, Role::Publisher)]
				.subscriptions_closed
				.fetch_add(1, Ordering::Relaxed);
		}
	}
}

/// RAII subscription guard for the subscriber role.
#[must_use = "drop the guard to record the subscription as closed"]
pub struct SubscriberTrack {
	entry: Option<Arc<BroadcastEntry>>,
	tier: Tier,
}

impl SubscriberTrack {
	/// Bumps `frames` once.
	pub fn frame(&self) {
		if let Some(entry) = &self.entry {
			entry.slots[slot_index(self.tier, Role::Subscriber)]
				.frames
				.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// Bumps `bytes` by `n`.
	pub fn bytes(&self, n: u64) {
		if let Some(entry) = &self.entry {
			entry.slots[slot_index(self.tier, Role::Subscriber)]
				.bytes
				.fetch_add(n, Ordering::Relaxed);
		}
	}

	/// Bumps `groups` once.
	pub fn group(&self) {
		if let Some(entry) = &self.entry {
			entry.slots[slot_index(self.tier, Role::Subscriber)]
				.groups
				.fetch_add(1, Ordering::Relaxed);
		}
	}
}

impl Drop for SubscriberTrack {
	fn drop(&mut self) {
		if let Some(entry) = &self.entry {
			entry.slots[slot_index(self.tier, Role::Subscriber)]
				.subscriptions_closed
				.fetch_add(1, Ordering::Relaxed);
		}
	}
}

fn ensure_task(stats: &Stats) {
	let inner = &stats.inner;
	if inner.advertised.is_none() {
		return;
	}
	let mut slot = inner.task.lock();
	if slot.is_none() {
		*slot = Some(());
		let weak = Arc::downgrade(inner);
		spawn(run_publisher(weak));
	}
}

fn clear_task(inner: &StatsInner) {
	*inner.task.lock() = None;
}

async fn run_publisher(weak: Weak<StatsInner>) {
	let Some(inner) = weak.upgrade() else {
		return;
	};
	let Some(advertised) = inner.advertised.clone() else {
		clear_task(&inner);
		return;
	};
	let tick = inner.tick;
	let retention_ticks = inner.retention_ticks;

	let mut broadcast = Broadcast::new().produce();
	let mut tracks: Vec<TrackProducer> = Vec::with_capacity(NUM_SLOTS);
	for name in TRACK_NAMES {
		match broadcast.create_track(Track {
			name: name.into(),
			priority: 0,
		}) {
			Ok(t) => tracks.push(t),
			Err(err) => {
				tracing::warn!(?err, name, "stats: failed to create track");
				clear_task(&inner);
				return;
			}
		}
	}
	if !inner.origin.publish_broadcast(&advertised, broadcast.consume()) {
		tracing::warn!(advertised = %advertised, "stats: origin rejected stats broadcast");
		clear_task(&inner);
		return;
	}
	drop(inner);

	let mut last_payload: [Vec<u8>; NUM_SLOTS] = Default::default();
	let mut empty_ticks: u32 = 0;

	let mut ticker = tokio::time::interval(tick);
	ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

	loop {
		ticker.tick().await;

		let Some(inner) = weak.upgrade() else {
			return;
		};

		let current_tick = inner.tick_counter.fetch_add(1, Ordering::Relaxed) + 1;

		let entries: Vec<Arc<BroadcastEntry>> = {
			let map = inner.entries.lock();
			map.values().cloned().collect()
		};

		let mut frames: [BTreeMap<String, Snapshot>; NUM_SLOTS] = Default::default();
		for entry in &entries {
			for ((counters, last_tick), frame) in entry
				.slots
				.iter()
				.zip(entry.last_active_tick.iter())
				.zip(frames.iter_mut())
			{
				let snap = counters.snapshot();
				if counters.active() {
					last_tick.store(current_tick, Ordering::Relaxed);
					frame.insert(entry.path.as_str().to_string(), snap);
					continue;
				}
				let last = last_tick.load(Ordering::Relaxed);
				// `<=` so retention_ticks counts the number of *idle* ticks the
				// entry lingers after its last observed active tick. retention=0
				// means "drop as soon as the sub goes idle"; the active branch
				// above still emits while subs are live.
				if last != 0 && current_tick.saturating_sub(last) <= retention_ticks as u64 {
					frame.insert(entry.path.as_str().to_string(), snap);
				}
			}
		}
		// Release our snapshot refs before GC so Arc::strong_count below only
		// reflects map + outstanding guard references.
		drop(entries);

		{
			let mut map = inner.entries.lock();
			map.retain(|_, entry| {
				// Keep entries with outstanding guards alive even if they
				// haven't surfaced in a frame yet, so a bump that races with
				// GC can't be silently lost on an orphaned Arc.
				if Arc::strong_count(entry) > 1 {
					return true;
				}
				entry.last_active_tick.iter().any(|t| {
					let last = t.load(Ordering::Relaxed);
					last != 0 && current_tick.saturating_sub(last) <= retention_ticks as u64
				})
			});
		}

		for (((frame, last), track), slot) in frames
			.iter()
			.zip(last_payload.iter_mut())
			.zip(tracks.iter_mut())
			.zip(0usize..)
		{
			let json = match serde_json::to_vec(frame) {
				Ok(b) => b,
				Err(err) => {
					tracing::debug!(?err, slot, "stats: failed to serialize frame");
					continue;
				}
			};
			if &json == last {
				continue;
			}
			let compressed = match gzip(&json) {
				Ok(b) => b,
				Err(err) => {
					tracing::debug!(?err, slot, "stats: failed to gzip frame");
					continue;
				}
			};
			if let Err(err) = track.write_frame(compressed) {
				tracing::debug!(?err, slot, "stats: failed to write frame");
				// Leave `last_payload` untouched so the next tick retries this
				// snapshot instead of skipping it as "already written".
				continue;
			}
			*last = json;
		}

		let map_empty = inner.entries.lock().is_empty();
		if map_empty {
			empty_ticks = empty_ticks.saturating_add(1);
			// Once the map has been empty long enough that no consumer could
			// learn anything new, drop the broadcast and let the next bump
			// respawn us. Take the task slot under lock and re-check the map
			// to avoid racing with a fresh insert.
			let exit_threshold = retention_ticks.saturating_mul(2).max(1);
			if empty_ticks >= exit_threshold {
				let mut slot = inner.task.lock();
				if inner.entries.lock().is_empty() {
					*slot = None;
					drop(slot);
					drop(inner);
					drop(tracks);
					drop(broadcast);
					return;
				}
				empty_ticks = 0;
			}
		} else {
			empty_ticks = 0;
		}
	}
}

fn gzip(data: &[u8]) -> std::io::Result<Vec<u8>> {
	let mut enc = GzEncoder::new(Vec::new(), Compression::default());
	enc.write_all(data)?;
	enc.finish()
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(serde::Deserialize))]
struct Snapshot {
	broadcasts: u64,
	broadcasts_closed: u64,
	subscriptions: u64,
	subscriptions_closed: u64,
	bytes: u64,
	frames: u64,
	groups: u64,
}

fn advertised_path(prefix: &Path, node: Option<&str>) -> PathOwned {
	// The fixed `node` category leaves room for sibling categories (e.g.
	// `<top-prefix>/cluster` for relay-mesh stats) under the same prefix.
	let mut out = format!("{}/node", prefix.as_str());
	if let Some(node) = node {
		out.push('/');
		out.push_str(node);
	}
	PathOwned::from(out)
}

#[cfg(test)]
mod tests {
	use std::{collections::BTreeMap, io::Read, sync::atomic::Ordering::Relaxed};

	use flate2::read::GzDecoder;

	use crate::{Origin, Path};

	use super::*;

	fn test_stats(node: Option<&str>) -> (Stats, OriginProducer) {
		let origin = Origin::random().produce();
		let stats = Stats::new(
			".stats",
			Duration::from_secs(1),
			10,
			node.map(|s| PathOwned::from(s.to_string())),
			origin.clone(),
		);
		(stats, origin)
	}

	#[test]
	fn advertised_path_with_and_without_node() {
		let prefix = Path::new(".stats");
		assert_eq!(advertised_path(&prefix, Some("sjc")).as_str(), ".stats/node/sjc");
		assert_eq!(advertised_path(&prefix, Some("sjc/1")).as_str(), ".stats/node/sjc/1");
		assert_eq!(advertised_path(&prefix, None).as_str(), ".stats/node");

		let prefix = Path::new("metrics");
		assert_eq!(advertised_path(&prefix, Some("lon")).as_str(), "metrics/node/lon");
	}

	#[test]
	fn new_normalizes_and_drops_empty_node() {
		let origin = Origin::random().produce();
		let stats = Stats::new(
			".stats",
			Duration::from_secs(1),
			10,
			Some(PathOwned::from("/sjc//1/".to_string())),
			origin.clone(),
		);
		assert_eq!(stats.inner.advertised.as_ref().unwrap().as_str(), ".stats/node/sjc/1");

		let stats = Stats::new(
			".stats",
			Duration::from_secs(1),
			10,
			Some(PathOwned::from("///".to_string())),
			origin,
		);
		assert_eq!(stats.inner.advertised.as_ref().unwrap().as_str(), ".stats/node");
	}

	#[tokio::test(start_paused = true)]
	async fn per_broadcast_counters_isolated() {
		// Bumps on one broadcast must not leak into another.
		let (stats, _origin) = test_stats(Some("sjc"));
		let bs1 = stats.tier(Tier::External).broadcast("demo/bbb");
		let bs2 = stats.tier(Tier::External).broadcast("demo/ccc");
		let g1 = bs1.publisher().track("video");
		g1.bytes(100);
		let g2 = bs2.publisher().track("video");
		g2.bytes(7);

		let entries = stats.inner.entries.lock();
		let e1 = entries.get(&PathOwned::from("demo/bbb")).expect("entry");
		let e2 = entries.get(&PathOwned::from("demo/ccc")).expect("entry");
		let i = slot_index(Tier::External, Role::Publisher);
		assert_eq!(e1.slots[i].bytes.load(Relaxed), 100);
		assert_eq!(e2.slots[i].bytes.load(Relaxed), 7);
	}

	#[tokio::test(start_paused = true)]
	async fn external_and_internal_tiers_are_independent() {
		let (stats, _origin) = test_stats(Some("sjc"));
		let ext = stats.tier(Tier::External);
		let int = stats.tier(Tier::Internal);

		let ext_track = ext.broadcast("demo/bbb").publisher().track("video");
		ext_track.bytes(100);
		let int_track = int.broadcast("demo/bbb").subscriber().track("audio");
		int_track.bytes(7);

		let entries = stats.inner.entries.lock();
		let entry = entries.get(&PathOwned::from("demo/bbb")).expect("entry");
		assert_eq!(
			entry.slots[slot_index(Tier::External, Role::Publisher)]
				.bytes
				.load(Relaxed),
			100
		);
		assert_eq!(
			entry.slots[slot_index(Tier::External, Role::Subscriber)]
				.bytes
				.load(Relaxed),
			0
		);
		assert_eq!(
			entry.slots[slot_index(Tier::Internal, Role::Publisher)]
				.bytes
				.load(Relaxed),
			0
		);
		assert_eq!(
			entry.slots[slot_index(Tier::Internal, Role::Subscriber)]
				.bytes
				.load(Relaxed),
			7
		);
	}

	#[tokio::test(start_paused = true)]
	async fn paths_under_prefix_are_no_op() {
		// Our own stats broadcasts (and any sibling category under the same
		// prefix) must not feed back into the aggregator.
		let (stats, _origin) = test_stats(Some("sjc"));
		let bs = stats.tier(Tier::External).broadcast(".stats/node/sjc");
		assert!(bs.is_empty());
		let p = bs.publisher();
		let track = p.track("video");
		track.bytes(100);
		drop(track);
		drop(p);
		assert!(stats.inner.entries.lock().is_empty());
	}

	#[tokio::test(start_paused = true)]
	async fn disabled_stats_are_noop() {
		let stats = Stats::disabled();
		let bs = stats.tier(Tier::External).broadcast("demo/bbb");
		assert!(bs.is_empty());
		let p = bs.publisher();
		let track = p.track("video");
		track.bytes(100);
		drop(track);
		drop(p);
		assert!(stats.inner.entries.lock().is_empty());
	}

	#[tokio::test(start_paused = true)]
	async fn single_broadcast_path_announced() {
		// No matter how many broadcasts get bumped, exactly one stats
		// broadcast is announced (the per-node aggregate).
		let (stats, origin) = test_stats(Some("sjc/1"));
		let mut consumer = origin.consume();

		let bs1 = stats.tier(Tier::External).broadcast("foo/bar");
		let _t1 = bs1.publisher().track("video");
		let bs2 = stats.tier(Tier::External).broadcast("baz/qux");
		let _t2 = bs2.publisher().track("video");

		tokio::time::advance(Duration::from_millis(1)).await;
		let (path, broadcast) = consumer.announced().await.expect("expected announce");
		assert!(broadcast.is_some());
		assert_eq!(path.as_str(), ".stats/node/sjc/1");
	}

	#[tokio::test(start_paused = true)]
	async fn task_announces_without_node_suffix() {
		let origin = Origin::random().produce();
		let stats = Stats::new(".stats", Duration::from_secs(1), 10, None, origin.clone());
		let mut consumer = origin.consume();

		let bs = stats.tier(Tier::External).broadcast("foo/bar");
		let _t = bs.publisher().track("video");

		tokio::time::advance(Duration::from_millis(1)).await;
		let (path, broadcast) = consumer.announced().await.expect("expected announce");
		assert!(broadcast.is_some());
		assert_eq!(path.as_str(), ".stats/node");
	}

	/// Drives the snapshot task forward by `count` ticks. In paused-time
	/// tests, `tokio::time::advance` doesn't poll spawned tasks itself; we
	/// have to combine it with explicit awaits. This helper interleaves
	/// `advance` with `consumer.announced()` (and later `yield_now` calls)
	/// so the task wakes, processes the tick, and re-parks each iteration.
	async fn drive_ticks(count: u32) {
		for _ in 0..count {
			tokio::time::advance(Duration::from_secs(1)).await;
			// Yield several times to let the task wake, snapshot, write the
			// frame, and re-await the next tick.
			for _ in 0..4 {
				tokio::task::yield_now().await;
			}
		}
	}

	#[tokio::test(start_paused = true)]
	async fn retention_boundary_keeps_last_idle_tick() {
		// retention_ticks=2 means the entry lingers for exactly 2 idle ticks
		// after its last observed active tick. Drive the boundary precisely.
		let origin = Origin::random().produce();
		let stats = Stats::new(
			".stats",
			Duration::from_secs(1),
			2,
			Some(PathOwned::from("sjc".to_string())),
			origin,
		);
		let key = PathOwned::from("foo/bar".to_string());
		let bs = stats.tier(Tier::External).broadcast("foo/bar");
		let track = bs.publisher().track("video");

		// One active tick so last_active_tick is set.
		drive_ticks(1).await;
		drop(track);
		drop(bs);

		// Idle tick 1: diff == 1, still <= retention_ticks. Kept.
		drive_ticks(1).await;
		assert!(
			stats.inner.entries.lock().contains_key(&key),
			"entry must remain after 1 idle tick (retention=2)"
		);
		// Idle tick 2: diff == 2, still <= retention_ticks. Kept.
		drive_ticks(1).await;
		assert!(
			stats.inner.entries.lock().contains_key(&key),
			"entry must remain after 2 idle ticks (retention=2)"
		);
		// Idle tick 3: diff == 3, exceeds retention_ticks. GC'd.
		drive_ticks(1).await;
		assert!(
			!stats.inner.entries.lock().contains_key(&key),
			"entry must be GC'd after 3 idle ticks (retention=2)"
		);
	}

	#[tokio::test(start_paused = true)]
	async fn retention_keeps_recently_dropped_entry() {
		let (stats, _origin) = test_stats(Some("sjc"));
		let bs = stats.tier(Tier::External).broadcast("foo/bar");
		let track = bs.publisher().track("video");

		// Tick a few times while subs are active so last_active_tick is set.
		drive_ticks(2).await;
		drop(track);
		drop(bs);
		// Tick a few more times within retention_ticks=10. Entry must remain
		// in the map (because last_active_tick is still recent enough).
		drive_ticks(3).await;

		assert!(
			stats
				.inner
				.entries
				.lock()
				.contains_key(&PathOwned::from("foo/bar".to_string())),
			"entry must remain in the map within the retention window"
		);
	}

	#[tokio::test(start_paused = true)]
	async fn retention_evicts_after_window() {
		// retention_ticks=2, drop and tick well past the window. Entry should
		// be GC'd from the map and absent from subsequent frames.
		let origin = Origin::random().produce();
		let stats = Stats::new(
			".stats",
			Duration::from_secs(1),
			2,
			Some(PathOwned::from("sjc".to_string())),
			origin,
		);
		let bs = stats.tier(Tier::External).broadcast("foo/bar");
		let track = bs.publisher().track("video");

		drive_ticks(2).await;
		drop(track);
		drop(bs);
		// Tick well past 2 * retention_ticks so the entry ages out and the
		// GC sweep removes it.
		drive_ticks(8).await;

		assert!(stats.inner.entries.lock().is_empty(), "entries should be GC'd");
	}

	#[tokio::test(start_paused = true)]
	async fn gzip_frame_decompresses_to_expected_json() {
		let (stats, origin) = test_stats(Some("sjc"));
		let mut consumer = origin.consume();
		let bs = stats.tier(Tier::External).broadcast("foo/bar");
		let track = bs.publisher().track("video");
		track.bytes(42);
		track.frame();

		tokio::time::advance(Duration::from_millis(1100)).await;

		let (_path, broadcast) = consumer.announced().await.expect("expected announce");
		let broadcast = broadcast.expect("active");
		let track = broadcast
			.subscribe_track(&Track {
				name: "publisher.json.gz".into(),
				priority: 0,
			})
			.expect("subscribe");
		let frame = read_frame(track).await;
		let snap = frame.get("foo/bar").expect("foo/bar entry");
		assert_eq!(snap.bytes, 42);
		assert_eq!(snap.frames, 1);
		assert_eq!(snap.subscriptions, 1);
	}

	async fn read_frame(mut track: crate::TrackConsumer) -> BTreeMap<String, Snapshot> {
		let bytes = track.read_frame().await.expect("ok").expect("frame");
		let mut dec = GzDecoder::new(bytes.as_ref());
		let mut json = String::new();
		dec.read_to_string(&mut json).expect("decompress");
		serde_json::from_str(&json).expect("json parse")
	}
}
