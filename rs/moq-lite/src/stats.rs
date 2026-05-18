//! Generic stats publishing for moq-lite sessions.
//!
//! [`Stats`] aggregates per-broadcast counter bumps into per-prefix levels and
//! publishes a `.stats/<level>/<name>` broadcast on a caller-provided
//! [`OriginProducer`]. Each stats broadcast carries two tracks:
//!
//! * `publisher`  - egress (counters bumped when serving subscriptions)
//! * `subscriber` - ingress (counters bumped when receiving data)
//!
//! A caller that wants to differentiate two classes of traffic (e.g. internal
//! cluster peers vs external customers) constructs two [`Stats`] instances with
//! different `name`s and hands each session the appropriate one via
//! [`crate::Client::with_stats`] / [`crate::Server::with_stats`].
//!
//! # Lifecycle
//!
//! No background work runs while no role has an active subscription. The first
//! `track()` call on a level (in either role) spawns a per-level snapshot task
//! that ticks every second. The task exits the moment both roles report zero
//! active subscriptions, dropping its [`BroadcastProducer`] and unannouncing.
//!
//! # Cycles
//!
//! Calling [`Stats::broadcast`] for a hidden path (any segment starting with
//! `.`) returns an empty handle whose bumps no-op. This breaks the obvious
//! feedback loop where serving a `.stats/...` broadcast would generate more
//! stats traffic.

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

/// Cumulative atomic counters for a single role on a level.
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

/// Top-level stats handle. Cheap to clone (`Arc` inside).
#[derive(Clone)]
pub struct Stats {
	inner: Arc<StatsInner>,
}

struct StatsInner {
	name: String,
	levels: u32,
	origin: OriginProducer,
	entries: Lock<HashMap<PathOwned, Arc<Level>>>,
}

struct Level {
	advertised: PathOwned,
	publisher: Counters,
	subscriber: Counters,
	task: Lock<Option<()>>, // unit: presence means a snapshot task is running
	origin: OriginProducer,
	name: String,
	level_key: PathOwned,
}

impl Stats {
	/// Build a new stats aggregator.
	///
	/// * `name` is baked into the advertised path of every published stats broadcast,
	///   following the convention `.stats/<level>/<name>` (or `.stats/<name>` for the root).
	/// * `levels` controls how many path-prefix levels stats are bucketed into. A value
	///   of `1` produces only the root bucket. `2` adds a per-first-segment bucket, and
	///   so on. Levels deeper than the number of segments in a given broadcast path are
	///   skipped (we never publish a level whose key equals the broadcast path itself).
	/// * `origin` is the [`OriginProducer`] that receives `publish_broadcast` calls
	///   for each `.stats/...` broadcast.
	pub fn new(name: impl Into<String>, levels: u32, origin: OriginProducer) -> Self {
		Self {
			inner: Arc::new(StatsInner {
				name: name.into(),
				levels,
				origin,
				entries: Lock::default(),
			}),
		}
	}

	/// Returns the configured `name`.
	pub fn name(&self) -> &str {
		&self.inner.name
	}

	/// Returns a per-broadcast handle. Cheap; level state is created lazily and cached.
	///
	/// Hidden paths (any segment starting with `.`) return an empty handle whose bumps
	/// are no-ops. This keeps stats traffic from feeding back into the aggregator.
	pub fn broadcast(&self, path: impl AsPath) -> BroadcastStats {
		let path = path.as_path();
		if path.is_hidden() {
			return BroadcastStats { levels: Arc::from([]) };
		}

		let keys = level_keys(&path, self.inner.levels);
		let mut entries = self.inner.entries.lock();
		let arcs: Vec<Arc<Level>> = keys
			.into_iter()
			.map(|key| {
				entries
					.entry(key.clone())
					.or_insert_with(|| {
						let advertised = advertised_path(&key, &self.inner.name);
						Arc::new(Level {
							advertised,
							publisher: Counters::default(),
							subscriber: Counters::default(),
							task: Lock::new(None),
							origin: self.inner.origin.clone(),
							name: self.inner.name.clone(),
							level_key: key,
						})
					})
					.clone()
			})
			.collect();

		BroadcastStats { levels: arcs.into() }
	}
}

/// A per-broadcast handle. Cheap to clone.
///
/// Open a role-scoped guard via [`Self::publisher`] or [`Self::subscriber`]; each
/// returns a RAII handle whose creation bumps the matching `broadcasts` counter
/// and whose drop bumps `broadcasts_closed`.
#[derive(Clone)]
pub struct BroadcastStats {
	levels: Arc<[Arc<Level>]>,
}

impl BroadcastStats {
	/// True if this handle is for a hidden path (no levels, all bumps are no-ops).
	pub fn is_empty(&self) -> bool {
		self.levels.is_empty()
	}

	/// Open the publisher (egress) role for this broadcast.
	pub fn publisher(&self) -> PublisherStats {
		for level in self.levels.iter() {
			level.publisher.broadcasts.fetch_add(1, Ordering::Relaxed);
		}
		PublisherStats {
			levels: self.levels.clone(),
		}
	}

	/// Open the subscriber (ingress) role for this broadcast.
	pub fn subscriber(&self) -> SubscriberStats {
		for level in self.levels.iter() {
			level.subscriber.broadcasts.fetch_add(1, Ordering::Relaxed);
		}
		SubscriberStats {
			levels: self.levels.clone(),
		}
	}
}

/// RAII broadcast guard for the publisher role. See [`BroadcastStats::publisher`].
#[must_use = "drop the guard to record the broadcast as closed"]
pub struct PublisherStats {
	levels: Arc<[Arc<Level>]>,
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
			level.publisher.subscriptions.fetch_add(1, Ordering::Relaxed);
			ensure_task(level);
		}
		PublisherTrack {
			levels: self.levels.clone(),
		}
	}
}

impl Drop for PublisherStats {
	fn drop(&mut self) {
		for level in self.levels.iter() {
			level.publisher.broadcasts_closed.fetch_add(1, Ordering::Relaxed);
		}
	}
}

/// RAII broadcast guard for the subscriber role. See [`BroadcastStats::subscriber`].
#[must_use = "drop the guard to record the broadcast as closed"]
pub struct SubscriberStats {
	levels: Arc<[Arc<Level>]>,
}

impl SubscriberStats {
	/// Open a track-subscription guard. Mirrors [`PublisherStats::track`].
	pub fn track(&self, _name: &str) -> SubscriberTrack {
		for level in self.levels.iter() {
			level.subscriber.subscriptions.fetch_add(1, Ordering::Relaxed);
			ensure_task(level);
		}
		SubscriberTrack {
			levels: self.levels.clone(),
		}
	}
}

impl Drop for SubscriberStats {
	fn drop(&mut self) {
		for level in self.levels.iter() {
			level.subscriber.broadcasts_closed.fetch_add(1, Ordering::Relaxed);
		}
	}
}

/// RAII subscription guard for the publisher role.
#[must_use = "drop the guard to record the subscription as closed"]
pub struct PublisherTrack {
	levels: Arc<[Arc<Level>]>,
}

impl PublisherTrack {
	/// Bumps `frames` once.
	pub fn frame(&self) {
		for level in self.levels.iter() {
			level.publisher.frames.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// Bumps `bytes` by `n`.
	pub fn bytes(&self, n: u64) {
		for level in self.levels.iter() {
			level.publisher.bytes.fetch_add(n, Ordering::Relaxed);
		}
	}

	/// Bumps `groups` once.
	pub fn group(&self) {
		for level in self.levels.iter() {
			level.publisher.groups.fetch_add(1, Ordering::Relaxed);
		}
	}
}

impl Drop for PublisherTrack {
	fn drop(&mut self) {
		for level in self.levels.iter() {
			level.publisher.subscriptions_closed.fetch_add(1, Ordering::Relaxed);
		}
	}
}

/// RAII subscription guard for the subscriber role.
#[must_use = "drop the guard to record the subscription as closed"]
pub struct SubscriberTrack {
	levels: Arc<[Arc<Level>]>,
}

impl SubscriberTrack {
	/// Bumps `frames` once.
	pub fn frame(&self) {
		for level in self.levels.iter() {
			level.subscriber.frames.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// Bumps `bytes` by `n`.
	pub fn bytes(&self, n: u64) {
		for level in self.levels.iter() {
			level.subscriber.bytes.fetch_add(n, Ordering::Relaxed);
		}
	}

	/// Bumps `groups` once.
	pub fn group(&self) {
		for level in self.levels.iter() {
			level.subscriber.groups.fetch_add(1, Ordering::Relaxed);
		}
	}
}

impl Drop for SubscriberTrack {
	fn drop(&mut self) {
		for level in self.levels.iter() {
			level.subscriber.subscriptions_closed.fetch_add(1, Ordering::Relaxed);
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
		let publisher = match broadcast.create_track(Track {
			name: "publisher".into(),
			priority: 0,
		}) {
			Ok(t) => t,
			Err(err) => {
				tracing::warn!(?err, "stats: failed to create publisher track");
				clear_task(&level);
				return;
			}
		};
		let subscriber = match broadcast.create_track(Track {
			name: "subscriber".into(),
			priority: 0,
		}) {
			Ok(t) => t,
			Err(err) => {
				tracing::warn!(?err, "stats: failed to create subscriber track");
				clear_task(&level);
				return;
			}
		};
		level.origin.publish_broadcast(&level.advertised, broadcast.consume());
		(broadcast, publisher, subscriber)
	};
	let (broadcast, mut publisher, mut subscriber) = setup;

	let mut tick = tokio::time::interval(Duration::from_secs(1));
	tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

	loop {
		tick.tick().await;

		let Some(level) = weak.upgrade() else {
			return;
		};

		if !level.publisher.active() && !level.subscriber.active() {
			// Take the task slot under the lock and re-check. Any subscribe that
			// raced with us either landed before we set None (so it sees Some
			// and won't respawn) or after, in which case it spawns a fresh task.
			let mut slot = level.task.lock();
			if !level.publisher.active() && !level.subscriber.active() {
				*slot = None;
				drop(slot);
				drop(level);
				// Drop `broadcast` to unannounce. Leftover producers/consumers
				// follow the existing `closed()` watcher in OriginProducer.
				drop(broadcast);
				return;
			}
		}

		// Always emit a snapshot for both tracks. Idle roles see their counters
		// held steady; that itself is informative for a billing service.
		write_snapshot(&mut publisher, "publisher", &level, level.publisher.snapshot());
		write_snapshot(&mut subscriber, "subscriber", &level, level.subscriber.snapshot());
	}
}

fn clear_task(level: &Level) {
	*level.task.lock() = None;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
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
	role: &'a str,
	ts_ms: u64,
	#[serde(flatten)]
	snapshot: Snapshot,
}

fn write_snapshot(track: &mut crate::TrackProducer, role: &str, level: &Level, snapshot: Snapshot) {
	let frame = SnapshotFrame {
		v: 1,
		name: &level.name,
		level: level.level_key.as_str(),
		role,
		ts_ms: now_ms(),
		snapshot,
	};

	let buf = match serde_json::to_vec(&frame) {
		Ok(buf) => buf,
		Err(err) => {
			tracing::debug!(?err, role, level = %level.advertised, "stats: failed to serialize snapshot");
			return;
		}
	};

	if let Err(err) = track.write_frame(buf) {
		tracing::debug!(?err, role, level = %level.advertised, "stats: failed to write snapshot frame");
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
/// The keys are the prefixes of the broadcast path with 0..N segments, where N is
/// `min(levels, segments)`. The key with `segments` segments is intentionally
/// omitted: it would be equal to the broadcast path itself, which carries no
/// aggregation value. `levels == 0` produces no buckets (stats are effectively
/// disabled).
fn level_keys(broadcast: &Path, levels: u32) -> Vec<PathOwned> {
	if levels == 0 {
		return Vec::new();
	}
	if broadcast.is_empty() {
		return vec![PathOwned::default()];
	}

	let segs: Vec<&str> = broadcast.as_str().split('/').collect();
	let max = (levels as usize).min(segs.len());
	(0..max).map(|i| PathOwned::from(segs[..i].join("/"))).collect()
}

fn advertised_path(level_key: &Path, name: &str) -> PathOwned {
	if level_key.is_empty() {
		PathOwned::from(format!(".stats/{name}"))
	} else {
		PathOwned::from(format!(".stats/{}/{name}", level_key.as_str()))
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

		assert_eq!(key("demo/bbb", 1), vec![""]);
		assert_eq!(key("demo/bbb", 2), vec!["", "demo"]);
		// Capped: broadcast is 2 segments, levels=3 still yields 2 keys.
		assert_eq!(key("demo/bbb", 3), vec!["", "demo"]);
		assert_eq!(key("a/b/c/d", 3), vec!["", "a", "a/b"]);
		// 1-segment broadcast, levels=2 still yields just root.
		assert_eq!(key("demo", 2), vec![""]);
		// levels=0 yields no buckets at all.
		assert!(key("demo/bbb", 0).is_empty());
	}

	#[test]
	fn advertised_path_root_and_nested() {
		assert_eq!(advertised_path(&Path::new(""), "use").as_str(), ".stats/use");
		assert_eq!(advertised_path(&Path::new("demo"), "use").as_str(), ".stats/demo/use");
		assert_eq!(
			advertised_path(&Path::new("demo/foo"), "use").as_str(),
			".stats/demo/foo/use"
		);
	}

	#[tokio::test]
	async fn publisher_bumps_publisher_counters() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let stats = Stats::new("use", 2, origin);
		let bs = stats.broadcast("demo/bbb");
		let pub_role = bs.publisher();
		let track = pub_role.track("video");
		track.frame();
		track.bytes(100);
		track.group();
		drop(track);
		drop(pub_role);

		let entries = stats.inner.entries.lock();
		let root = entries.get(&PathOwned::from("")).expect("root level");
		assert_eq!(root.publisher.frames.load(Relaxed), 1);
		assert_eq!(root.publisher.bytes.load(Relaxed), 100);
		assert_eq!(root.publisher.groups.load(Relaxed), 1);
		assert_eq!(root.publisher.subscriptions.load(Relaxed), 1);
		assert_eq!(root.publisher.subscriptions_closed.load(Relaxed), 1);
		assert_eq!(root.publisher.broadcasts.load(Relaxed), 1);
		assert_eq!(root.publisher.broadcasts_closed.load(Relaxed), 1);
		// Subscriber must remain untouched.
		assert_eq!(root.subscriber.bytes.load(Relaxed), 0);
		assert_eq!(root.subscriber.broadcasts.load(Relaxed), 0);
	}

	#[tokio::test]
	async fn subscriber_bumps_subscriber_counters() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let stats = Stats::new("use", 1, origin);
		let bs = stats.broadcast("demo/bbb");
		let sub_role = bs.subscriber();
		let track = sub_role.track("video");
		track.frame();
		track.bytes(50);

		let entries = stats.inner.entries.lock();
		let root = entries.get(&PathOwned::from("")).expect("root level");
		assert_eq!(root.subscriber.frames.load(Relaxed), 1);
		assert_eq!(root.subscriber.bytes.load(Relaxed), 50);
		assert_eq!(root.subscriber.broadcasts.load(Relaxed), 1);
		assert_eq!(root.subscriber.subscriptions.load(Relaxed), 1);
		assert_eq!(root.publisher.bytes.load(Relaxed), 0);
	}

	#[tokio::test]
	async fn bumps_fanout_to_all_levels() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let stats = Stats::new("use", 2, origin);
		let bs = stats.broadcast("demo/bbb");
		let p = bs.publisher();
		let track = p.track("video");
		track.bytes(100);

		let entries = stats.inner.entries.lock();
		let root = entries.get(&PathOwned::from("")).expect("root level");
		let demo = entries.get(&PathOwned::from("demo")).expect("demo level");
		assert_eq!(root.publisher.bytes.load(Relaxed), 100);
		assert_eq!(demo.publisher.bytes.load(Relaxed), 100);
	}

	#[tokio::test]
	async fn hidden_paths_are_no_op() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let stats = Stats::new("use", 2, origin);
		let bs = stats.broadcast(".stats/use");
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
	async fn task_spawns_on_first_subscribe_and_announces() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let stats = Stats::new("use", 1, origin.clone());
		let mut consumer = origin.consume();

		let bs = stats.broadcast("foo/bar");
		let p = bs.publisher();
		let _track = p.track("video");

		tokio::time::advance(Duration::from_millis(1)).await;
		let (path, broadcast) = consumer.announced_all().await.expect("expected announce");
		assert_eq!(path, Path::new(".stats/use"));
		assert!(broadcast.is_some());
	}

	#[tokio::test]
	async fn task_exits_when_all_roles_idle() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let stats = Stats::new("use", 1, origin.clone());
		let mut consumer = origin.consume();

		let bs = stats.broadcast("foo/bar");
		let p = bs.publisher();
		let track = p.track("video");

		tokio::time::advance(Duration::from_millis(1)).await;
		let (_, broadcast) = consumer.announced_all().await.expect("expected announce");
		assert!(broadcast.is_some());

		drop(track);
		drop(p);
		drop(bs);

		tokio::time::advance(Duration::from_secs(2)).await;
		let (path, broadcast) = consumer.announced_all().await.expect("expected unannounce");
		assert_eq!(path, Path::new(".stats/use"));
		assert!(broadcast.is_none());
	}

	#[tokio::test]
	async fn two_stats_handles_are_independent() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let external = Stats::new("external", 1, origin.clone());
		let internal = Stats::new("internal", 1, origin.clone());
		let mut consumer = origin.consume();

		let ext_bs = external.broadcast("foo/bar");
		let int_bs = internal.broadcast("foo/bar");
		let ext_p = ext_bs.publisher();
		let int_p = int_bs.publisher();
		let _ext_track = ext_p.track("video");
		let _int_track = int_p.track("video");

		// Both stats handles should announce their own broadcast.
		let mut seen = std::collections::HashSet::new();
		tokio::time::advance(Duration::from_millis(1)).await;
		for _ in 0..2 {
			let (path, broadcast) = consumer.announced_all().await.expect("expected announce");
			assert!(broadcast.is_some());
			seen.insert(path.as_str().to_string());
		}
		assert!(seen.contains(".stats/external"));
		assert!(seen.contains(".stats/internal"));
	}
}
