//! Generic stats publishing for moq-net sessions.
//!
//! [`Stats`] aggregates per-broadcast counter bumps into per-prefix levels and
//! publishes a `<top-prefix>/prefix/<level-path>/<node>` broadcast on a caller-provided
//! [`OriginProducer`]. The `<node>` suffix is omitted when no node is configured.
//! Each stats broadcast carries four tracks, one per `(tier, role)` pair:
//!
//! * `publisher.json`           : external (e.g. customer) egress
//! * `subscriber.json`          : external ingress
//! * `internal/publisher.json`  : internal (e.g. mTLS cluster peer) egress
//! * `internal/subscriber.json` : internal ingress
//!
//! A caller hands each session a tier-scoped [`StatsHandle`] (built from the
//! single shared [`Stats`] via [`Stats::tier`]) which determines which counter
//! set its bumps land in. Multiple relays in the same cluster origin can
//! coexist by giving each one a distinct `<node>` suffix on advertised paths.
//! The suffix itself may be multi-segment (e.g. `sjc/1`, `sjc/2`) so a region
//! with multiple hosts can nest under a shared region key without colliding.
//!
//! Each broadcast contributes to every prefix of its path (within the
//! configured depth), so publishing `anon/bbb` with `levels = 2` produces:
//!
//! * `<top-prefix>/prefix/<node>`           (root aggregate)
//! * `<top-prefix>/prefix/anon/<node>`      (per-first-segment aggregate)
//! * `<top-prefix>/prefix/anon/bbb/<node>`  (per-broadcast)
//!
//! The fixed `prefix` segment between the top-level prefix and the
//! aggregation level leaves room for sibling categories under the same prefix
//! (e.g. `<top-prefix>/nodes/<node>` for host-level stats).
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
//! No background work runs while no role × tier has an active subscription.
//! The first `track()` call on a level spawns a per-level snapshot task that
//! ticks every second. The task exits as soon as all four counter sets report
//! zero active subscriptions, dropping its [`BroadcastProducer`] and
//! unannouncing.
//!
//! # Idle frame skipping
//!
//! On each tick the task compares the current `Snapshot` against the last one
//! it emitted for the same `(tier, role)` and writes a frame only when
//! something changed. New subscribers still pick up a baseline immediately
//! because track-latest semantics retain the most recent emitted frame.
//!
//! # Cycles
//!
//! Calling [`Stats::broadcast`] for a path under the configured top-level
//! prefix returns an empty handle whose bumps no-op. This breaks the feedback
//! loop where serving a `<top-prefix>/...` broadcast would itself generate
//! more stats traffic.

use std::{
	collections::HashMap,
	sync::{
		Arc, Weak,
		atomic::{AtomicU64, Ordering},
	},
	time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use web_async::{Lock, spawn};

use crate::{AsPath, Broadcast, Origin, OriginProducer, Path, PathOwned, Track};

/// Cumulative atomic counters for a single (tier, role) on a level.
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
/// the four tracks published on each level's broadcast.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Tier {
	External,
	Internal,
}

impl Tier {
	fn as_str(&self) -> &'static str {
		match self {
			Tier::External => "external",
			Tier::Internal => "internal",
		}
	}
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Role {
	Publisher,
	Subscriber,
}

impl Role {
	fn as_str(&self) -> &'static str {
		match self {
			Role::Publisher => "publisher",
			Role::Subscriber => "subscriber",
		}
	}
}

/// Top-level stats aggregator. Cheap to clone (`Arc` inside). One instance per
/// relay; sessions get tier-scoped handles via [`Stats::tier`].
#[derive(Clone)]
pub struct Stats {
	inner: Arc<StatsInner>,
}

struct StatsInner {
	prefix: PathOwned,
	levels: u32,
	node: Option<PathOwned>,
	origin: OriginProducer,
	entries: Lock<HashMap<PathOwned, Arc<Level>>>,
}

struct Level {
	advertised: PathOwned,
	external_publisher: Counters,
	external_subscriber: Counters,
	internal_publisher: Counters,
	internal_subscriber: Counters,
	task: Lock<Option<()>>,
	origin: OriginProducer,
	node: Option<PathOwned>,
	level_key: PathOwned,
}

impl Level {
	fn counters(&self, tier: Tier, role: Role) -> &Counters {
		match (tier, role) {
			(Tier::External, Role::Publisher) => &self.external_publisher,
			(Tier::External, Role::Subscriber) => &self.external_subscriber,
			(Tier::Internal, Role::Publisher) => &self.internal_publisher,
			(Tier::Internal, Role::Subscriber) => &self.internal_subscriber,
		}
	}

	fn any_active(&self) -> bool {
		self.external_publisher.active()
			|| self.external_subscriber.active()
			|| self.internal_publisher.active()
			|| self.internal_subscriber.active()
	}
}

impl Stats {
	/// Build a new stats aggregator.
	///
	/// * `prefix` is the top-level path under which stats are published, e.g. `.stats`.
	///   The full advertised path is `<prefix>/prefix/<level-path>/<node>` (or
	///   `<prefix>/prefix/<level-path>` when `node` is `None`).
	/// * `levels` is the maximum segment depth stats are bucketed by, which caps the
	///   number of aggregation buckets per broadcast. `0` disables stats entirely (no
	///   buckets, including no root bucket). `1` produces the root bucket plus a
	///   per-first-segment bucket. `2` adds a per-second-segment bucket, and so on.
	///   A broadcast within the configured depth also gets its own dedicated bucket;
	///   broadcasts deeper than `levels` are truncated.
	/// * `node` disambiguates broadcasts published by different relays into a shared
	///   cluster origin. Set this on every node in multi-relay deployments. The
	///   value may be multi-segment (e.g. `sjc/1`, `sjc/2`) so a region with
	///   multiple hosts can nest under a shared region key. `None` (or an empty
	///   path after normalization) omits the suffix, which is fine for
	///   single-relay deployments.
	/// * `origin` is the [`OriginProducer`] that receives `publish_broadcast` calls
	///   for each stats broadcast.
	pub fn new(
		prefix: impl Into<PathOwned>,
		levels: u32,
		node: impl Into<Option<PathOwned>>,
		origin: OriginProducer,
	) -> Self {
		// An empty path after normalization is indistinguishable from "no node
		// set"; collapse it so downstream code only sees a single representation.
		let node = node.into().filter(|p| !p.is_empty());
		Self {
			inner: Arc::new(StatsInner {
				prefix: prefix.into(),
				levels,
				node,
				origin,
				entries: Lock::default(),
			}),
		}
	}

	/// A no-op aggregator. Counter bumps are silently dropped and no snapshot
	/// task is ever spawned. Use this when stats are disabled so call sites
	/// can hold a [`Stats`] (or [`StatsHandle`]) unconditionally.
	pub fn disabled() -> Self {
		// Levels = 0 short-circuits broadcast_levels to an empty Arc, so every
		// downstream operation is a no-op iteration. The origin is never
		// touched because the snapshot task only spawns on the first track.
		Self {
			inner: Arc::new(StatsInner {
				prefix: PathOwned::default(),
				levels: 0,
				node: None,
				origin: Origin::random().produce(),
				entries: Lock::default(),
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

	fn broadcast_levels(&self, path: impl AsPath) -> Arc<[Arc<Level>]> {
		let path = path.as_path();
		// Skip our own stats broadcasts (and any sibling category under the same
		// prefix) so serving a stats broadcast doesn't generate more stats.
		if path.has_prefix(&self.inner.prefix) {
			return Arc::from([]);
		}

		let keys = level_keys(&path, self.inner.levels);
		let mut entries = self.inner.entries.lock();
		let arcs: Vec<Arc<Level>> = keys
			.into_iter()
			.map(|key| {
				entries
					.entry(key.clone())
					.or_insert_with(|| {
						let advertised = advertised_path(&self.inner.prefix, &key, self.inner.node.as_ref());
						Arc::new(Level {
							advertised,
							external_publisher: Counters::default(),
							external_subscriber: Counters::default(),
							internal_publisher: Counters::default(),
							internal_subscriber: Counters::default(),
							task: Lock::new(None),
							origin: self.inner.origin.clone(),
							node: self.inner.node.clone(),
							level_key: key,
						})
					})
					.clone()
			})
			.collect();

		arcs.into()
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

	/// Returns a per-broadcast handle scoped to this tier. Cheap; level state is
	/// created lazily and cached.
	///
	/// Paths under the aggregator's configured `prefix` return an empty handle
	/// whose bumps are no-ops. This keeps stats traffic from feeding back into
	/// the aggregator.
	pub fn broadcast(&self, path: impl AsPath) -> BroadcastStats {
		BroadcastStats {
			levels: self.stats.broadcast_levels(path),
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
/// [`Self::subscriber_track`] when the broadcast's lifetime is tracked elsewhere.
#[derive(Clone)]
pub struct BroadcastStats {
	levels: Arc<[Arc<Level>]>,
	tier: Tier,
}

impl BroadcastStats {
	/// True if this handle covers no levels (its path was under the aggregator's
	/// own prefix, or stats are disabled). All bumps through an empty handle
	/// are no-ops.
	pub fn is_empty(&self) -> bool {
		self.levels.is_empty()
	}

	/// Open a broadcast-lifetime guard for the publisher (egress) role.
	/// Bumps `broadcasts` on construction and `broadcasts_closed` on drop.
	pub fn publisher(&self) -> PublisherStats {
		for level in self.levels.iter() {
			level
				.counters(self.tier, Role::Publisher)
				.broadcasts
				.fetch_add(1, Ordering::Relaxed);
		}
		PublisherStats {
			levels: self.levels.clone(),
			tier: self.tier,
		}
	}

	/// Open a broadcast-lifetime guard for the subscriber (ingress) role.
	/// Bumps `broadcasts` on construction and `broadcasts_closed` on drop.
	pub fn subscriber(&self) -> SubscriberStats {
		for level in self.levels.iter() {
			level
				.counters(self.tier, Role::Subscriber)
				.broadcasts
				.fetch_add(1, Ordering::Relaxed);
		}
		SubscriberStats {
			levels: self.levels.clone(),
			tier: self.tier,
		}
	}

	/// Open a publisher-track guard without bumping the broadcast counters.
	///
	/// Use this when the broadcast is already counted by a [`PublisherStats`]
	/// guard held elsewhere (e.g. by the announce loop), so the track guard
	/// only contributes to subscription counters.
	///
	/// `_name` is currently unused; counters are per-level only. Reserved for
	/// future per-track granularity.
	pub fn publisher_track(&self, _name: &str) -> PublisherTrack {
		for level in self.levels.iter() {
			level
				.counters(self.tier, Role::Publisher)
				.subscriptions
				.fetch_add(1, Ordering::Relaxed);
			ensure_task(level);
		}
		PublisherTrack {
			levels: self.levels.clone(),
			tier: self.tier,
		}
	}

	/// Subscriber-side counterpart to [`Self::publisher_track`].
	pub fn subscriber_track(&self, _name: &str) -> SubscriberTrack {
		for level in self.levels.iter() {
			level
				.counters(self.tier, Role::Subscriber)
				.subscriptions
				.fetch_add(1, Ordering::Relaxed);
			ensure_task(level);
		}
		SubscriberTrack {
			levels: self.levels.clone(),
			tier: self.tier,
		}
	}
}

/// RAII broadcast guard for the publisher role. See [`BroadcastStats::publisher`].
#[must_use = "drop the guard to record the broadcast as closed"]
pub struct PublisherStats {
	levels: Arc<[Arc<Level>]>,
	tier: Tier,
}

impl PublisherStats {
	/// Open a track-subscription guard. Bumps `subscriptions` on every level
	/// and (on the 0->N transition in any role) spawns the level's snapshot
	/// task. Drop bumps `subscriptions_closed`.
	pub fn track(&self, name: &str) -> PublisherTrack {
		BroadcastStats {
			levels: self.levels.clone(),
			tier: self.tier,
		}
		.publisher_track(name)
	}
}

impl Drop for PublisherStats {
	fn drop(&mut self) {
		for level in self.levels.iter() {
			level
				.counters(self.tier, Role::Publisher)
				.broadcasts_closed
				.fetch_add(1, Ordering::Relaxed);
		}
	}
}

/// RAII broadcast guard for the subscriber role. See [`BroadcastStats::subscriber`].
#[must_use = "drop the guard to record the broadcast as closed"]
pub struct SubscriberStats {
	levels: Arc<[Arc<Level>]>,
	tier: Tier,
}

impl SubscriberStats {
	/// Open a track-subscription guard. Mirrors [`PublisherStats::track`].
	pub fn track(&self, name: &str) -> SubscriberTrack {
		BroadcastStats {
			levels: self.levels.clone(),
			tier: self.tier,
		}
		.subscriber_track(name)
	}
}

impl Drop for SubscriberStats {
	fn drop(&mut self) {
		for level in self.levels.iter() {
			level
				.counters(self.tier, Role::Subscriber)
				.broadcasts_closed
				.fetch_add(1, Ordering::Relaxed);
		}
	}
}

/// RAII subscription guard for the publisher role.
#[must_use = "drop the guard to record the subscription as closed"]
pub struct PublisherTrack {
	levels: Arc<[Arc<Level>]>,
	tier: Tier,
}

impl PublisherTrack {
	/// Bumps `frames` once.
	pub fn frame(&self) {
		for level in self.levels.iter() {
			level
				.counters(self.tier, Role::Publisher)
				.frames
				.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// Bumps `bytes` by `n`.
	pub fn bytes(&self, n: u64) {
		for level in self.levels.iter() {
			level
				.counters(self.tier, Role::Publisher)
				.bytes
				.fetch_add(n, Ordering::Relaxed);
		}
	}

	/// Bumps `groups` once.
	pub fn group(&self) {
		for level in self.levels.iter() {
			level
				.counters(self.tier, Role::Publisher)
				.groups
				.fetch_add(1, Ordering::Relaxed);
		}
	}
}

impl Drop for PublisherTrack {
	fn drop(&mut self) {
		for level in self.levels.iter() {
			level
				.counters(self.tier, Role::Publisher)
				.subscriptions_closed
				.fetch_add(1, Ordering::Relaxed);
		}
	}
}

/// RAII subscription guard for the subscriber role.
#[must_use = "drop the guard to record the subscription as closed"]
pub struct SubscriberTrack {
	levels: Arc<[Arc<Level>]>,
	tier: Tier,
}

impl SubscriberTrack {
	/// Bumps `frames` once.
	pub fn frame(&self) {
		for level in self.levels.iter() {
			level
				.counters(self.tier, Role::Subscriber)
				.frames
				.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// Bumps `bytes` by `n`.
	pub fn bytes(&self, n: u64) {
		for level in self.levels.iter() {
			level
				.counters(self.tier, Role::Subscriber)
				.bytes
				.fetch_add(n, Ordering::Relaxed);
		}
	}

	/// Bumps `groups` once.
	pub fn group(&self) {
		for level in self.levels.iter() {
			level
				.counters(self.tier, Role::Subscriber)
				.groups
				.fetch_add(1, Ordering::Relaxed);
		}
	}
}

impl Drop for SubscriberTrack {
	fn drop(&mut self) {
		for level in self.levels.iter() {
			level
				.counters(self.tier, Role::Subscriber)
				.subscriptions_closed
				.fetch_add(1, Ordering::Relaxed);
		}
	}
}

fn ensure_task(level: &Arc<Level>) {
	let mut slot = level.task.lock();
	if slot.is_none() {
		*slot = Some(());
		let weak = Arc::downgrade(level);
		spawn(run_publisher(weak));
	}
}

async fn run_publisher(weak: Weak<Level>) {
	let setup = {
		let Some(level) = weak.upgrade() else {
			return;
		};
		let mut broadcast = Broadcast::new().produce();
		let mut make = |name: &str| {
			broadcast.create_track(Track {
				name: name.into(),
				priority: 0,
			})
		};
		let ext_pub = match make("publisher.json") {
			Ok(t) => t,
			Err(err) => {
				tracing::warn!(?err, "stats: failed to create publisher.json");
				clear_task(&level);
				return;
			}
		};
		let ext_sub = match make("subscriber.json") {
			Ok(t) => t,
			Err(err) => {
				tracing::warn!(?err, "stats: failed to create subscriber.json");
				clear_task(&level);
				return;
			}
		};
		let int_pub = match make("internal/publisher.json") {
			Ok(t) => t,
			Err(err) => {
				tracing::warn!(?err, "stats: failed to create internal/publisher.json");
				clear_task(&level);
				return;
			}
		};
		let int_sub = match make("internal/subscriber.json") {
			Ok(t) => t,
			Err(err) => {
				tracing::warn!(?err, "stats: failed to create internal/subscriber.json");
				clear_task(&level);
				return;
			}
		};
		if !level.origin.publish_broadcast(&level.advertised, broadcast.consume()) {
			tracing::warn!(level = %level.advertised, "stats: origin rejected stats broadcast");
			clear_task(&level);
			return;
		}
		(broadcast, ext_pub, ext_sub, int_pub, int_sub)
	};
	let (broadcast, mut ext_pub, mut ext_sub, mut int_pub, mut int_sub) = setup;

	let mut last_ext_pub: Option<Snapshot> = None;
	let mut last_ext_sub: Option<Snapshot> = None;
	let mut last_int_pub: Option<Snapshot> = None;
	let mut last_int_sub: Option<Snapshot> = None;

	let mut tick = tokio::time::interval(Duration::from_secs(1));
	tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

	loop {
		tick.tick().await;

		let Some(level) = weak.upgrade() else {
			return;
		};

		if !level.any_active() {
			// Take the task slot under the lock and re-check. Any subscribe that
			// raced with us either landed before we set None (so it sees Some
			// and won't respawn) or after, in which case it spawns a fresh task.
			let mut slot = level.task.lock();
			if !level.any_active() {
				*slot = None;
				drop(slot);
				drop(level);
				// Drop `broadcast` to unannounce. Leftover producers/consumers
				// follow the existing `closed()` watcher in OriginProducer.
				drop(broadcast);
				return;
			}
		}

		maybe_write(&mut ext_pub, Tier::External, Role::Publisher, &level, &mut last_ext_pub);
		maybe_write(
			&mut ext_sub,
			Tier::External,
			Role::Subscriber,
			&level,
			&mut last_ext_sub,
		);
		maybe_write(&mut int_pub, Tier::Internal, Role::Publisher, &level, &mut last_int_pub);
		maybe_write(
			&mut int_sub,
			Tier::Internal,
			Role::Subscriber,
			&level,
			&mut last_int_sub,
		);
	}
}

fn maybe_write(track: &mut crate::TrackProducer, tier: Tier, role: Role, level: &Level, last: &mut Option<Snapshot>) {
	let snapshot = level.counters(tier, role).snapshot();
	if last.as_ref() == Some(&snapshot) {
		return;
	}
	write_snapshot(track, tier, role, level, snapshot);
	*last = Some(snapshot);
}

fn clear_task(level: &Level) {
	*level.task.lock() = None;
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
struct Snapshot {
	broadcasts: u64,
	broadcasts_closed: u64,
	subscriptions: u64,
	subscriptions_closed: u64,
	bytes: u64,
	frames: u64,
	groups: u64,
}

#[derive(Debug, Serialize)]
struct SnapshotFrame<'a> {
	v: u32,
	level: &'a str,
	tier: &'a str,
	role: &'a str,
	#[serde(skip_serializing_if = "Option::is_none")]
	node: Option<&'a str>,
	ts_ms: u64,
	#[serde(flatten)]
	snapshot: Snapshot,
}

fn write_snapshot(track: &mut crate::TrackProducer, tier: Tier, role: Role, level: &Level, snapshot: Snapshot) {
	let frame = SnapshotFrame {
		v: 1,
		level: level.level_key.as_str(),
		tier: tier.as_str(),
		role: role.as_str(),
		node: level.node.as_ref().map(|p| p.as_str()),
		ts_ms: now_ms(),
		snapshot,
	};

	let buf = match serde_json::to_vec(&frame) {
		Ok(buf) => buf,
		Err(err) => {
			tracing::debug!(?err, ?tier, ?role, level = %level.advertised, "stats: failed to serialize snapshot");
			return;
		}
	};

	if let Err(err) = track.write_frame(buf) {
		tracing::debug!(?err, ?tier, ?role, level = %level.advertised, "stats: failed to write snapshot frame");
	}
}

fn now_ms() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map(|d| d.as_millis() as u64)
		.unwrap_or(0)
}

/// Compute the level prefix keys this broadcast contributes to.
///
/// Produces every prefix of the broadcast path from 0 segments (root) up to
/// `min(levels, segments)` segments, inclusive, so a broadcast within the
/// configured depth budget gets a dedicated per-broadcast bucket in addition
/// to the aggregating prefixes. Broadcasts deeper than `levels` are truncated
/// (no per-broadcast bucket). `levels == 0` produces no buckets at all.
fn level_keys(broadcast: &Path, levels: u32) -> Vec<PathOwned> {
	if levels == 0 {
		return Vec::new();
	}
	if broadcast.is_empty() {
		return vec![PathOwned::default()];
	}

	let segs: Vec<&str> = broadcast.as_str().split('/').collect();
	let max = (levels as usize).min(segs.len());
	(0..=max).map(|i| PathOwned::from(segs[..i].join("/"))).collect()
}

fn advertised_path(prefix: &Path, level_key: &Path, node: Option<&Path>) -> PathOwned {
	// The fixed `prefix` category leaves room for sibling categories (e.g.
	// `<top-prefix>/nodes/<node>` for host-level stats) under the same prefix.
	let top = prefix.as_str();
	let mut out = format!("{top}/prefix");
	if !level_key.is_empty() {
		out.push('/');
		out.push_str(level_key.as_str());
	}
	if let Some(node) = node {
		// `node` is already a normalized path, so multi-segment values like
		// `sjc/1` nest under the region without any extra handling.
		out.push('/');
		out.push_str(node.as_str());
	}
	PathOwned::from(out)
}

#[cfg(test)]
mod tests {
	use std::sync::atomic::Ordering::Relaxed;

	use crate::{Origin, Path};

	use super::*;

	#[test]
	fn level_keys_basic() {
		let key = |s: &str, n: u32| {
			level_keys(&Path::new(s), n)
				.into_iter()
				.map(|p| p.as_str().to_string())
				.collect::<Vec<_>>()
		};

		// levels=1 covers root + the first segment; for "demo/bbb" that's the
		// "demo" aggregating prefix (no per-broadcast bucket since we'd need
		// levels=2 to reach the broadcast itself).
		assert_eq!(key("demo/bbb", 1), vec!["", "demo"]);
		// levels=2 reaches the broadcast itself, so we get root + prefix + own.
		assert_eq!(key("demo/bbb", 2), vec!["", "demo", "demo/bbb"]);
		// Capped: broadcast is 2 segments, levels=3 still tops out at the
		// broadcast's own path.
		assert_eq!(key("demo/bbb", 3), vec!["", "demo", "demo/bbb"]);
		// Deeper broadcast, levels=3 stops one short of the broadcast itself.
		assert_eq!(key("a/b/c/d", 3), vec!["", "a", "a/b", "a/b/c"]);
		// 1-segment broadcast, levels=2 reaches the broadcast.
		assert_eq!(key("demo", 2), vec!["", "demo"]);
		// levels=0 yields no buckets at all.
		assert!(key("demo/bbb", 0).is_empty());
	}

	#[test]
	fn advertised_path_root_and_nested() {
		let prefix = Path::new(".stats");
		let node = PathOwned::from("sjc");
		assert_eq!(
			advertised_path(&prefix, &Path::new(""), Some(&node)).as_str(),
			".stats/prefix/sjc"
		);
		assert_eq!(
			advertised_path(&prefix, &Path::new("demo"), Some(&node)).as_str(),
			".stats/prefix/demo/sjc"
		);
		assert_eq!(
			advertised_path(&prefix, &Path::new("demo/foo"), Some(&node)).as_str(),
			".stats/prefix/demo/foo/sjc"
		);
	}

	#[test]
	fn advertised_path_without_node() {
		let prefix = Path::new(".stats");
		assert_eq!(advertised_path(&prefix, &Path::new(""), None).as_str(), ".stats/prefix");
		assert_eq!(
			advertised_path(&prefix, &Path::new("demo"), None).as_str(),
			".stats/prefix/demo"
		);
	}

	#[test]
	fn advertised_path_honors_custom_prefix() {
		let prefix = Path::new("metrics");
		let node = PathOwned::from("lon");
		assert_eq!(
			advertised_path(&prefix, &Path::new(""), Some(&node)).as_str(),
			"metrics/prefix/lon"
		);
		assert_eq!(
			advertised_path(&prefix, &Path::new("demo/room"), Some(&node)).as_str(),
			"metrics/prefix/demo/room/lon"
		);
	}

	#[test]
	fn advertised_path_multi_segment_node() {
		let prefix = Path::new(".stats");
		let node = PathOwned::from("sjc/1");
		assert_eq!(
			advertised_path(&prefix, &Path::new(""), Some(&node)).as_str(),
			".stats/prefix/sjc/1"
		);
		assert_eq!(
			advertised_path(&prefix, &Path::new("demo"), Some(&node)).as_str(),
			".stats/prefix/demo/sjc/1"
		);
		assert_eq!(
			advertised_path(&prefix, &Path::new("demo/foo"), Some(&node)).as_str(),
			".stats/prefix/demo/foo/sjc/1"
		);
		// A second host in the same region nests under the shared region key
		// without colliding with the first.
		let node2 = PathOwned::from("sjc/2");
		assert_eq!(
			advertised_path(&prefix, &Path::new("demo"), Some(&node2)).as_str(),
			".stats/prefix/demo/sjc/2"
		);
	}

	#[test]
	fn new_normalizes_and_drops_empty_node() {
		let origin = Origin::random().produce();

		// Trailing slash and a doubled separator both get normalized away.
		let stats = Stats::new(".stats", 1, Some(PathOwned::from("/sjc//1/")), origin.clone());
		assert_eq!(stats.inner.node.as_ref().unwrap().as_str(), "sjc/1");

		// Empty / whitespace-only inputs collapse to None so downstream code
		// only has to handle a single "no node" representation.
		let stats = Stats::new(".stats", 1, Some(PathOwned::from("")), origin.clone());
		assert!(stats.inner.node.is_none());
		let stats = Stats::new(".stats", 1, Some(PathOwned::from("///")), origin);
		assert!(stats.inner.node.is_none());
	}

	#[tokio::test(start_paused = true)]
	async fn external_publisher_bumps_external_publisher_counters() {
		let origin = Origin::random().produce();
		let stats = Stats::new(".stats", 2, Some(PathOwned::from("sjc")), origin);
		let bs = stats.tier(Tier::External).broadcast("demo/bbb");
		let pub_role = bs.publisher();
		let track = pub_role.track("video");
		track.frame();
		track.bytes(100);
		track.group();
		drop(track);
		drop(pub_role);

		let entries = stats.inner.entries.lock();
		let root = entries.get(&PathOwned::from("")).expect("root level");
		assert_eq!(root.external_publisher.frames.load(Relaxed), 1);
		assert_eq!(root.external_publisher.bytes.load(Relaxed), 100);
		assert_eq!(root.external_publisher.groups.load(Relaxed), 1);
		assert_eq!(root.external_publisher.subscriptions.load(Relaxed), 1);
		assert_eq!(root.external_publisher.subscriptions_closed.load(Relaxed), 1);
		assert_eq!(root.external_publisher.broadcasts.load(Relaxed), 1);
		assert_eq!(root.external_publisher.broadcasts_closed.load(Relaxed), 1);
		// Other tier/role combos must remain untouched.
		assert_eq!(root.external_subscriber.bytes.load(Relaxed), 0);
		assert_eq!(root.internal_publisher.bytes.load(Relaxed), 0);
		assert_eq!(root.internal_subscriber.bytes.load(Relaxed), 0);
	}

	#[tokio::test(start_paused = true)]
	async fn external_subscriber_bumps_external_subscriber_counters() {
		let origin = Origin::random().produce();
		let stats = Stats::new(".stats", 1, Some(PathOwned::from("sjc")), origin);
		let bs = stats.tier(Tier::External).broadcast("demo/bbb");
		let sub_role = bs.subscriber();
		let track = sub_role.track("video");
		track.frame();
		track.bytes(50);

		let entries = stats.inner.entries.lock();
		let root = entries.get(&PathOwned::from("")).expect("root level");
		assert_eq!(root.external_subscriber.frames.load(Relaxed), 1);
		assert_eq!(root.external_subscriber.bytes.load(Relaxed), 50);
		assert_eq!(root.external_subscriber.broadcasts.load(Relaxed), 1);
		assert_eq!(root.external_subscriber.subscriptions.load(Relaxed), 1);
		assert_eq!(root.external_publisher.bytes.load(Relaxed), 0);
		assert_eq!(root.internal_subscriber.bytes.load(Relaxed), 0);
	}

	#[tokio::test(start_paused = true)]
	async fn external_and_internal_tiers_are_independent() {
		let origin = Origin::random().produce();
		let stats = Stats::new(".stats", 1, Some(PathOwned::from("sjc")), origin);
		let ext = stats.tier(Tier::External);
		let int = stats.tier(Tier::Internal);

		let ext_track = ext.broadcast("demo/bbb").publisher().track("video");
		ext_track.bytes(100);
		let int_track = int.broadcast("demo/bbb").subscriber().track("audio");
		int_track.bytes(7);

		let entries = stats.inner.entries.lock();
		let root = entries.get(&PathOwned::from("")).expect("root level");
		assert_eq!(root.external_publisher.bytes.load(Relaxed), 100);
		assert_eq!(root.external_subscriber.bytes.load(Relaxed), 0);
		assert_eq!(root.internal_publisher.bytes.load(Relaxed), 0);
		assert_eq!(root.internal_subscriber.bytes.load(Relaxed), 7);
	}

	#[tokio::test(start_paused = true)]
	async fn bumps_fanout_to_all_levels() {
		let origin = Origin::random().produce();
		let stats = Stats::new(".stats", 2, Some(PathOwned::from("sjc")), origin);
		let bs = stats.tier(Tier::External).broadcast("demo/bbb");
		let p = bs.publisher();
		let track = p.track("video");
		track.bytes(100);

		let entries = stats.inner.entries.lock();
		let root = entries.get(&PathOwned::from("")).expect("root level");
		let demo = entries.get(&PathOwned::from("demo")).expect("demo level");
		assert_eq!(root.external_publisher.bytes.load(Relaxed), 100);
		assert_eq!(demo.external_publisher.bytes.load(Relaxed), 100);
	}

	#[tokio::test(start_paused = true)]
	async fn paths_under_prefix_are_no_op() {
		// Our own stats broadcasts (and any sibling category under the same
		// prefix) must not feed back into the aggregator.
		let origin = Origin::random().produce();
		let stats = Stats::new(".stats", 2, Some(PathOwned::from("sjc")), origin);
		let bs = stats.tier(Tier::External).broadcast(".stats/prefix/sjc");
		assert!(bs.is_empty());

		let p = bs.publisher();
		let track = p.track("video");
		track.bytes(100);
		track.frame();
		track.group();
		drop(track);
		drop(p);

		assert!(stats.inner.entries.lock().is_empty());
	}

	#[tokio::test(start_paused = true)]
	async fn publisher_track_does_not_bump_broadcasts() {
		// Subscription-side track creation should not record a broadcast: the
		// broadcast lifetime is tracked separately by the announce loop.
		let origin = Origin::random().produce();
		let stats = Stats::new(".stats", 1, Some(PathOwned::from("sjc")), origin);
		let bs = stats.tier(Tier::External).broadcast("demo/bbb");
		let track = bs.publisher_track("video");
		track.bytes(10);
		drop(track);

		let entries = stats.inner.entries.lock();
		let root = entries.get(&PathOwned::from("")).expect("root level");
		assert_eq!(root.external_publisher.broadcasts.load(Relaxed), 0);
		assert_eq!(root.external_publisher.broadcasts_closed.load(Relaxed), 0);
		assert_eq!(root.external_publisher.subscriptions.load(Relaxed), 1);
		assert_eq!(root.external_publisher.subscriptions_closed.load(Relaxed), 1);
		assert_eq!(root.external_publisher.bytes.load(Relaxed), 10);
	}

	#[tokio::test(start_paused = true)]
	async fn disabled_stats_are_noop() {
		// A disabled aggregator must not allocate level state or spawn tasks.
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
	async fn task_spawns_on_first_subscribe_and_announces() {
		let origin = Origin::random().produce();
		let stats = Stats::new(".stats", 1, Some(PathOwned::from("sjc")), origin.clone());
		let mut consumer = origin.consume().announced();

		let bs = stats.tier(Tier::External).broadcast("foo/bar");
		let p = bs.publisher();
		let _track = p.track("video");

		tokio::time::advance(Duration::from_millis(1)).await;
		// levels=1 + broadcast "foo/bar" → buckets ["", "foo"]: root + per-first-segment.
		let mut seen = std::collections::HashSet::new();
		for _ in 0..2 {
			let (path, broadcast) = consumer.next().await.expect("expected announce");
			assert!(broadcast.is_some());
			seen.insert(path.as_str().to_string());
		}
		assert!(seen.contains(".stats/prefix/sjc"));
		assert!(seen.contains(".stats/prefix/foo/sjc"));
	}

	#[tokio::test(start_paused = true)]
	async fn task_spawns_with_node_suffix() {
		let origin = Origin::random().produce();
		let stats = Stats::new(".stats", 2, Some(PathOwned::from("sjc")), origin.clone());
		let mut consumer = origin.consume().announced();

		let bs = stats.tier(Tier::External).broadcast("foo/bar");
		let p = bs.publisher();
		let _track = p.track("video");

		tokio::time::advance(Duration::from_millis(1)).await;
		// levels=2 + broadcast "foo/bar" → buckets ["", "foo", "foo/bar"], each
		// suffixed with `/sjc`.
		let mut seen = std::collections::HashSet::new();
		for _ in 0..3 {
			let (path, broadcast) = consumer.next().await.expect("expected announce");
			assert!(broadcast.is_some());
			seen.insert(path.as_str().to_string());
		}
		assert!(seen.contains(".stats/prefix/sjc"));
		assert!(seen.contains(".stats/prefix/foo/sjc"));
		assert!(seen.contains(".stats/prefix/foo/bar/sjc"));
	}

	#[tokio::test(start_paused = true)]
	async fn task_spawns_without_node_suffix() {
		// node=None: paths should omit the trailing /<node> segment.
		let origin = Origin::random().produce();
		let stats = Stats::new(".stats", 1, None, origin.clone());
		let mut consumer = origin.consume().announced();

		let bs = stats.tier(Tier::External).broadcast("foo/bar");
		let p = bs.publisher();
		let _track = p.track("video");

		tokio::time::advance(Duration::from_millis(1)).await;
		let mut seen = std::collections::HashSet::new();
		for _ in 0..2 {
			let (path, broadcast) = consumer.next().await.expect("expected announce");
			assert!(broadcast.is_some());
			seen.insert(path.as_str().to_string());
		}
		assert!(seen.contains(".stats/prefix"));
		assert!(seen.contains(".stats/prefix/foo"));
	}

	#[tokio::test(start_paused = true)]
	async fn task_exits_when_all_roles_idle() {
		let origin = Origin::random().produce();
		// levels=1 + broadcast "foo/bar" → buckets ["", "foo"] (root prefix plus
		// the first-segment prefix; the broadcast's own path isn't reachable at
		// this depth, so we get exactly two stats announces).
		let stats = Stats::new(".stats", 1, Some(PathOwned::from("sjc")), origin.clone());
		let mut consumer = origin.consume().announced();

		let bs = stats.tier(Tier::External).broadcast("foo/bar");
		let p = bs.publisher();
		let track = p.track("video");

		tokio::time::advance(Duration::from_millis(1)).await;
		let mut announced: Vec<String> = Vec::new();
		for _ in 0..2 {
			let (path, broadcast) = consumer.next().await.expect("expected announce");
			assert!(broadcast.is_some(), "expected an active announce");
			announced.push(path.as_str().to_string());
		}
		announced.sort();
		assert_eq!(announced, vec![".stats/prefix/foo/sjc", ".stats/prefix/sjc"]);

		drop(track);
		drop(p);
		drop(bs);

		tokio::time::advance(Duration::from_secs(2)).await;
		let mut unannounced: Vec<String> = Vec::new();
		for _ in 0..2 {
			let (path, broadcast) = consumer.next().await.expect("expected unannounce");
			assert!(broadcast.is_none(), "expected an unannounce");
			unannounced.push(path.as_str().to_string());
		}
		unannounced.sort();
		assert_eq!(unannounced, vec![".stats/prefix/foo/sjc", ".stats/prefix/sjc"]);
	}

	// Idle-skip behavior (the snapshot task suppresses a write when the
	// current Snapshot equals the last emitted one) is covered end-to-end in
	// local relay verification; driving the broadcast/track plumbing in a unit
	// test is awkward enough that the skip logic itself (`if last == new
	// { return; }` in `maybe_write`) is more clearly exercised by inspection.
}
