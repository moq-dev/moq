//! Generic stats publishing for moq-lite sessions.
//!
//! [`Stats`] aggregates per-broadcast counter bumps into per-prefix levels and
//! publishes a `<level>/.stats/<name>[/<pop>]` broadcast on a caller-provided
//! [`OriginProducer`]. Each stats broadcast carries four tracks, one per
//! `(tier, role)` pair:
//!
//! * `publisher.json`           : external (e.g. customer) egress
//! * `subscriber.json`          : external ingress
//! * `internal/publisher.json`  : internal (e.g. mTLS cluster peer) egress
//! * `internal/subscriber.json` : internal ingress
//!
//! A caller hands each session a tier-scoped [`StatsHandle`] (built from the
//! single shared [`Stats`] via [`Stats::tier`]) which determines which counter
//! set its bumps land in. The `pop` suffix on the advertised path lets multiple
//! relays in the same cluster origin coexist without overwriting each other's
//! broadcast.
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
//! Calling [`Stats::broadcast`] for a hidden path (any segment starting with
//! `.`) returns an empty handle whose bumps no-op. This breaks the obvious
//! feedback loop where serving a `.../.stats/...` broadcast would itself
//! generate more stats traffic.

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

use crate::{AsPath, Broadcast, OriginProducer, Path, PathOwned, Track};

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
	name: String,
	levels: u32,
	pop: Option<String>,
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
	name: String,
	pop: Option<String>,
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
	/// * `name` is baked into the advertised path of every published stats broadcast,
	///   following the convention `<level>/.stats/<name>[/<pop>]` (or `.stats/<name>[/<pop>]`
	///   for the root).
	/// * `levels` controls how many path-prefix levels stats are bucketed into. A value
	///   of `1` produces only the root bucket. `2` adds a per-first-segment bucket, and
	///   so on. Levels deeper than the number of segments in a given broadcast path are
	///   skipped (we never publish a level whose key equals the broadcast path itself).
	/// * `pop` disambiguates broadcasts published by different relays into a shared
	///   cluster origin. Required whenever more than one relay shares an `origin`.
	///   Without it, peer relays would publish to the same path and the origin's
	///   single-source delivery would drop all but one. `None` is fine for single-node
	///   dev.
	/// * `origin` is the [`OriginProducer`] that receives `publish_broadcast` calls
	///   for each stats broadcast.
	pub fn new(name: impl Into<String>, levels: u32, pop: Option<String>, origin: OriginProducer) -> Self {
		Self {
			inner: Arc::new(StatsInner {
				name: name.into(),
				levels,
				pop,
				origin,
				entries: Lock::default(),
			}),
		}
	}

	/// Returns the configured `name`.
	pub fn name(&self) -> &str {
		&self.inner.name
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
		if path.is_hidden() {
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
						let advertised = advertised_path(&key, &self.inner.name, self.inner.pop.as_deref());
						Arc::new(Level {
							advertised,
							external_publisher: Counters::default(),
							external_subscriber: Counters::default(),
							internal_publisher: Counters::default(),
							internal_subscriber: Counters::default(),
							task: Lock::new(None),
							origin: self.inner.origin.clone(),
							name: self.inner.name.clone(),
							pop: self.inner.pop.clone(),
							level_key: key,
						})
					})
					.clone()
			})
			.collect();

		arcs.into()
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
	/// Hidden paths (any segment starting with `.`) return an empty handle whose
	/// bumps are no-ops. This keeps stats traffic from feeding back into the
	/// aggregator.
	pub fn broadcast(&self, path: impl AsPath) -> BroadcastStats {
		BroadcastStats {
			levels: self.stats.broadcast_levels(path),
			tier: self.tier,
		}
	}
}

/// A per-broadcast, tier-scoped handle. Cheap to clone.
///
/// Open a role-scoped guard via [`Self::publisher`] or [`Self::subscriber`]; each
/// returns a RAII handle whose creation bumps the matching `broadcasts` counter
/// and whose drop bumps `broadcasts_closed`.
#[derive(Clone)]
pub struct BroadcastStats {
	levels: Arc<[Arc<Level>]>,
	tier: Tier,
}

impl BroadcastStats {
	/// True if this handle is for a hidden path (no levels, all bumps are no-ops).
	pub fn is_empty(&self) -> bool {
		self.levels.is_empty()
	}

	/// Open the publisher (egress) role for this broadcast.
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

	/// Open the subscriber (ingress) role for this broadcast.
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
}

/// RAII broadcast guard for the publisher role. See [`BroadcastStats::publisher`].
#[must_use = "drop the guard to record the broadcast as closed"]
pub struct PublisherStats {
	levels: Arc<[Arc<Level>]>,
	tier: Tier,
}

impl PublisherStats {
	/// Open a track-subscription guard.
	///
	/// Bumps `subscriptions` on every level and (on the 0->N transition in any
	/// role) spawns the level's snapshot task. Drop bumps `subscriptions_closed`.
	///
	/// `_name` is currently unused; counters are per-level only. Reserved for
	/// future per-track granularity.
	pub fn track(&self, _name: &str) -> PublisherTrack {
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
	pub fn track(&self, _name: &str) -> SubscriberTrack {
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
		level.origin.publish_broadcast(&level.advertised, broadcast.consume());
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
	name: &'a str,
	level: &'a str,
	tier: &'a str,
	role: &'a str,
	#[serde(skip_serializing_if = "Option::is_none")]
	pop: Option<&'a str>,
	ts_ms: u64,
	#[serde(flatten)]
	snapshot: Snapshot,
}

fn write_snapshot(track: &mut crate::TrackProducer, tier: Tier, role: Role, level: &Level, snapshot: Snapshot) {
	let frame = SnapshotFrame {
		v: 1,
		name: &level.name,
		level: level.level_key.as_str(),
		tier: tier.as_str(),
		role: role.as_str(),
		pop: level.pop.as_deref(),
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

fn advertised_path(level_key: &Path, name: &str, pop: Option<&str>) -> PathOwned {
	let tail = match pop {
		Some(p) => format!("{name}/{p}"),
		None => name.to_string(),
	};
	if level_key.is_empty() {
		PathOwned::from(format!(".stats/{tail}"))
	} else {
		PathOwned::from(format!("{}/.stats/{tail}", level_key.as_str()))
	}
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
		assert_eq!(advertised_path(&Path::new(""), "use", None).as_str(), ".stats/use");
		assert_eq!(
			advertised_path(&Path::new("demo"), "use", None).as_str(),
			"demo/.stats/use"
		);
		assert_eq!(
			advertised_path(&Path::new("demo/foo"), "use", None).as_str(),
			"demo/foo/.stats/use"
		);
	}

	#[test]
	fn advertised_path_with_pop() {
		assert_eq!(
			advertised_path(&Path::new(""), "use", Some("sjc")).as_str(),
			".stats/use/sjc"
		);
		assert_eq!(
			advertised_path(&Path::new("demo"), "use", Some("sjc")).as_str(),
			"demo/.stats/use/sjc"
		);
		assert_eq!(
			advertised_path(&Path::new("demo/room"), "use", Some("lon")).as_str(),
			"demo/room/.stats/use/lon"
		);
	}

	#[tokio::test]
	async fn external_publisher_bumps_external_publisher_counters() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let stats = Stats::new("use", 2, None, origin);
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

	#[tokio::test]
	async fn external_subscriber_bumps_external_subscriber_counters() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let stats = Stats::new("use", 1, None, origin);
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

	#[tokio::test]
	async fn external_and_internal_tiers_are_independent() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let stats = Stats::new("use", 1, None, origin);
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

	#[tokio::test]
	async fn bumps_fanout_to_all_levels() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let stats = Stats::new("use", 2, None, origin);
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

	#[tokio::test]
	async fn hidden_paths_are_no_op() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let stats = Stats::new("use", 2, None, origin);
		let bs = stats.tier(Tier::External).broadcast(".stats/use");
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

	#[tokio::test]
	async fn hidden_paths_are_no_op_with_inner_dot_segment() {
		// e.g. `mycdn/.stats/use/sjc` (our own stats output) must not feed back.
		tokio::time::pause();
		let origin = Origin::random().produce();
		let stats = Stats::new("use", 5, Some("sjc".into()), origin);
		let bs = stats.tier(Tier::External).broadcast("mycdn/.stats/use/sjc");
		assert!(bs.is_empty());
	}

	#[tokio::test]
	async fn task_spawns_on_first_subscribe_and_announces() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let stats = Stats::new("use", 1, None, origin.clone());
		let mut consumer = origin.consume();

		let bs = stats.tier(Tier::External).broadcast("foo/bar");
		let p = bs.publisher();
		let _track = p.track("video");

		tokio::time::advance(Duration::from_millis(1)).await;
		let (path, broadcast) = consumer.announced_all().await.expect("expected announce");
		assert_eq!(path, Path::new(".stats/use"));
		assert!(broadcast.is_some());
	}

	#[tokio::test]
	async fn task_spawns_with_pop_suffix() {
		tokio::time::pause();
		let origin = Origin::random().produce();
		let stats = Stats::new("use", 2, Some("sjc".into()), origin.clone());
		let mut consumer = origin.consume();

		let bs = stats.tier(Tier::External).broadcast("foo/bar");
		let p = bs.publisher();
		let _track = p.track("video");

		tokio::time::advance(Duration::from_millis(1)).await;
		// We should see two announces: the root and the per-first-segment level,
		// each suffixed with `/sjc`.
		let mut seen = std::collections::HashSet::new();
		for _ in 0..2 {
			let (path, broadcast) = consumer.announced_all().await.expect("expected announce");
			assert!(broadcast.is_some());
			seen.insert(path.as_str().to_string());
		}
		assert!(seen.contains(".stats/use/sjc"));
		assert!(seen.contains("foo/.stats/use/sjc"));
	}

	#[tokio::test]
	async fn task_exits_when_all_roles_idle() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		// levels=1 + broadcast "foo/bar" → buckets ["", "foo"] (root prefix plus
		// the first-segment prefix; the broadcast's own path isn't reachable at
		// this depth, so we get exactly two stats announces).
		let stats = Stats::new("use", 1, None, origin.clone());
		let mut consumer = origin.consume();

		let bs = stats.tier(Tier::External).broadcast("foo/bar");
		let p = bs.publisher();
		let track = p.track("video");

		tokio::time::advance(Duration::from_millis(1)).await;
		let mut announced: Vec<String> = Vec::new();
		for _ in 0..2 {
			let (path, broadcast) = consumer.announced_all().await.expect("expected announce");
			assert!(broadcast.is_some(), "expected an active announce");
			announced.push(path.as_str().to_string());
		}
		announced.sort();
		assert_eq!(announced, vec![".stats/use", "foo/.stats/use"]);

		drop(track);
		drop(p);
		drop(bs);

		tokio::time::advance(Duration::from_secs(2)).await;
		let mut unannounced: Vec<String> = Vec::new();
		for _ in 0..2 {
			let (path, broadcast) = consumer.announced_all().await.expect("expected unannounce");
			assert!(broadcast.is_none(), "expected an unannounce");
			unannounced.push(path.as_str().to_string());
		}
		unannounced.sort();
		assert_eq!(unannounced, vec![".stats/use", "foo/.stats/use"]);
	}

	// Idle-skip behavior (the snapshot task suppresses a write when the
	// current Snapshot equals the last emitted one) is covered end-to-end in
	// local relay verification; driving the broadcast/track plumbing in a unit
	// test is awkward enough that the skip logic itself (`if last == new
	// { return; }` in `maybe_write`) is more clearly exercised by inspection.
}
