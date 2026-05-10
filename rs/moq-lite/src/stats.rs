//! Generic stats publishing for moq-lite sessions.
//!
//! [`Stats`] aggregates per-broadcast counter bumps into per-prefix levels and
//! publishes a `.stats/<level>/<name>` broadcast on a caller-provided
//! [`OriginProducer`]. Each stats broadcast carries up to four tracks:
//!
//! * `publisher`          - external egress (downstream non-mTLS clients)
//! * `publisher_internal` - internal egress (cluster peers / mTLS sessions)
//! * `subscriber`         - external ingress
//! * `subscriber_internal`- internal ingress
//!
//! Internal vs external is a property of the session (typically determined by
//! mTLS); a relay tags the [`Stats`] handle it hands to a session via
//! [`Stats::external`] or [`Stats::internal`]. Counters from internal sessions
//! land on the `_internal` tracks so a billing service can rate-differentiate
//! between intra-cluster and customer traffic.
//!
//! # Lifecycle
//!
//! No background work runs while no role has an active subscription. The first
//! `track()` call on a level (in any of the four roles) spawns a per-level
//! snapshot task that ticks every second. The task exits the moment all four
//! roles report zero active subscriptions, dropping its [`BroadcastProducer`]
//! and unannouncing.
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

use web_async::{Lock, spawn};

use crate::{AsPath, Broadcast, OriginProducer, Path, PathOwned, Track};

/// Cumulative atomic counters for the publisher role (egress).
#[derive(Default, Debug)]
#[non_exhaustive]
pub struct PublisherCounters {
	/// Cumulative count of broadcasts this role has published.
	pub broadcasts: AtomicU64,
	pub broadcasts_closed: AtomicU64,
	/// Cumulative count of subscriptions this role has accepted.
	pub subscriptions: AtomicU64,
	pub subscriptions_closed: AtomicU64,
	pub bytes: AtomicU64,
	pub frames: AtomicU64,
	pub groups: AtomicU64,
}

/// Cumulative atomic counters for the subscriber role (ingress).
#[derive(Default, Debug)]
#[non_exhaustive]
pub struct SubscriberCounters {
	/// Cumulative count of broadcast announcements this role has observed.
	pub broadcasts: AtomicU64,
	pub broadcasts_closed: AtomicU64,
	/// Cumulative count of subscriptions this role has issued.
	pub subscriptions: AtomicU64,
	pub subscriptions_closed: AtomicU64,
	pub bytes: AtomicU64,
	pub frames: AtomicU64,
	pub groups: AtomicU64,
}

trait RoleCounters {
	fn subscriptions(&self) -> u64;
	fn subscriptions_closed(&self) -> u64;
	fn snapshot(&self) -> RoleSnapshot;
}

impl RoleCounters for PublisherCounters {
	fn subscriptions(&self) -> u64 {
		self.subscriptions.load(Ordering::Relaxed)
	}
	fn subscriptions_closed(&self) -> u64 {
		self.subscriptions_closed.load(Ordering::Relaxed)
	}
	fn snapshot(&self) -> RoleSnapshot {
		RoleSnapshot {
			broadcasts: self.broadcasts.load(Ordering::Relaxed),
			broadcasts_closed: self.broadcasts_closed.load(Ordering::Relaxed),
			subscriptions: self.subscriptions.load(Ordering::Relaxed),
			subscriptions_closed: self.subscriptions_closed.load(Ordering::Relaxed),
			bytes: self.bytes.load(Ordering::Relaxed),
			frames: self.frames.load(Ordering::Relaxed),
			groups: self.groups.load(Ordering::Relaxed),
		}
	}
}

impl RoleCounters for SubscriberCounters {
	fn subscriptions(&self) -> u64 {
		self.subscriptions.load(Ordering::Relaxed)
	}
	fn subscriptions_closed(&self) -> u64 {
		self.subscriptions_closed.load(Ordering::Relaxed)
	}
	fn snapshot(&self) -> RoleSnapshot {
		RoleSnapshot {
			broadcasts: self.broadcasts.load(Ordering::Relaxed),
			broadcasts_closed: self.broadcasts_closed.load(Ordering::Relaxed),
			subscriptions: self.subscriptions.load(Ordering::Relaxed),
			subscriptions_closed: self.subscriptions_closed.load(Ordering::Relaxed),
			bytes: self.bytes.load(Ordering::Relaxed),
			frames: self.frames.load(Ordering::Relaxed),
			groups: self.groups.load(Ordering::Relaxed),
		}
	}
}

/// Top-level stats handle. Cheap to clone (`Arc` inside).
///
/// A handle carries an internal-vs-external tier flag. Use [`Self::external`] or
/// [`Self::internal`] to derive a clone for the appropriate tier; counter bumps
/// go to the matching `_internal` track or the default external track.
#[derive(Clone)]
pub struct Stats {
	inner: Arc<StatsInner>,
	internal: bool,
}

struct StatsInner {
	name: String,
	levels: u32,
	origin: OriginProducer,
	entries: Lock<HashMap<PathOwned, Arc<Level>>>,
}

struct Level {
	advertised: PathOwned,
	publisher_external: PublisherCounters,
	publisher_internal: PublisherCounters,
	subscriber_external: SubscriberCounters,
	subscriber_internal: SubscriberCounters,
	task: Lock<Option<()>>, // unit: presence means a snapshot task is running
	origin: OriginProducer,
	name: String,
	level_key: PathOwned,
}

impl Level {
	fn publisher(&self, internal: bool) -> &PublisherCounters {
		if internal {
			&self.publisher_internal
		} else {
			&self.publisher_external
		}
	}
	fn subscriber(&self, internal: bool) -> &SubscriberCounters {
		if internal {
			&self.subscriber_internal
		} else {
			&self.subscriber_external
		}
	}
}

impl Stats {
	/// Build a new stats aggregator. The returned handle is `external` (default tier).
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
			internal: false,
		}
	}

	/// Returns the configured `name`.
	pub fn name(&self) -> &str {
		&self.inner.name
	}

	/// Returns true if this handle records bumps on the `_internal` counter set.
	pub fn is_internal(&self) -> bool {
		self.internal
	}

	/// Returns a clone of this handle tagged as internal traffic. Bumps land on
	/// the `_internal` counter sets and surface on the `publisher_internal` /
	/// `subscriber_internal` tracks of each level's stats broadcast.
	pub fn internal(&self) -> Self {
		Self {
			inner: self.inner.clone(),
			internal: true,
		}
	}

	/// Returns a clone tagged as external traffic (the default).
	pub fn external(&self) -> Self {
		Self {
			inner: self.inner.clone(),
			internal: false,
		}
	}

	/// Returns a clone tagged as `internal` if true, otherwise external.
	pub fn tier(&self, internal: bool) -> Self {
		if internal { self.internal() } else { self.external() }
	}

	/// Returns a per-broadcast handle. Cheap; level state is created lazily and cached.
	///
	/// Hidden paths (any segment starting with `.`) return an empty handle whose bumps
	/// are no-ops. This keeps stats traffic from feeding back into the aggregator.
	pub fn broadcast(&self, path: impl AsPath) -> BroadcastStats {
		let path = path.as_path();
		if path.is_hidden() {
			return BroadcastStats {
				levels: Arc::from([]),
				internal: self.internal,
			};
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
							publisher_external: PublisherCounters::default(),
							publisher_internal: PublisherCounters::default(),
							subscriber_external: SubscriberCounters::default(),
							subscriber_internal: SubscriberCounters::default(),
							task: Lock::new(None),
							origin: self.inner.origin.clone(),
							name: self.inner.name.clone(),
							level_key: key,
						})
					})
					.clone()
			})
			.collect();

		BroadcastStats {
			levels: arcs.into(),
			internal: self.internal,
		}
	}
}

/// A per-broadcast handle. Cheap to clone.
///
/// Open a role-scoped guard via [`Self::publisher`] or [`Self::subscriber`]; each
/// returns a RAII handle whose creation bumps the matching `broadcasts` counter
/// and whose drop bumps `broadcasts_closed`. The tier (internal vs external) is
/// inherited from the [`Stats`] this handle was derived from.
#[derive(Clone)]
pub struct BroadcastStats {
	levels: Arc<[Arc<Level>]>,
	internal: bool,
}

impl BroadcastStats {
	/// True if this handle is for a hidden path (no levels, all bumps are no-ops).
	pub fn is_empty(&self) -> bool {
		self.levels.is_empty()
	}

	/// Open the publisher (egress) role for this broadcast. Bumps `broadcasts`
	/// on each level (on the appropriate tier); drop bumps `broadcasts_closed`.
	pub fn publisher(&self) -> PublisherStats {
		for level in self.levels.iter() {
			level
				.publisher(self.internal)
				.broadcasts
				.fetch_add(1, Ordering::Relaxed);
		}
		PublisherStats {
			levels: self.levels.clone(),
			internal: self.internal,
		}
	}

	/// Open the subscriber (ingress) role for this broadcast.
	pub fn subscriber(&self) -> SubscriberStats {
		for level in self.levels.iter() {
			level
				.subscriber(self.internal)
				.broadcasts
				.fetch_add(1, Ordering::Relaxed);
		}
		SubscriberStats {
			levels: self.levels.clone(),
			internal: self.internal,
		}
	}
}

/// RAII broadcast guard for the publisher role. See [`BroadcastStats::publisher`].
#[must_use = "drop the guard to record the broadcast as closed"]
pub struct PublisherStats {
	levels: Arc<[Arc<Level>]>,
	internal: bool,
}

impl PublisherStats {
	/// Open a track-subscription guard.
	///
	/// Bumps `subscriptions` on every level for the tier and (on the 0->N
	/// transition in any role) spawns the level's snapshot task. Drop bumps
	/// `subscriptions_closed`.
	///
	/// `_name` is currently unused; counters are per-level only. Reserved for
	/// future per-track granularity.
	pub fn track(&self, _name: &str) -> PublisherTrack {
		for level in self.levels.iter() {
			level
				.publisher(self.internal)
				.subscriptions
				.fetch_add(1, Ordering::Relaxed);
			ensure_task(level);
		}
		PublisherTrack {
			levels: self.levels.clone(),
			internal: self.internal,
		}
	}
}

impl Drop for PublisherStats {
	fn drop(&mut self) {
		for level in self.levels.iter() {
			level
				.publisher(self.internal)
				.broadcasts_closed
				.fetch_add(1, Ordering::Relaxed);
		}
	}
}

/// RAII broadcast guard for the subscriber role. See [`BroadcastStats::subscriber`].
#[must_use = "drop the guard to record the broadcast as closed"]
pub struct SubscriberStats {
	levels: Arc<[Arc<Level>]>,
	internal: bool,
}

impl SubscriberStats {
	/// Open a track-subscription guard. Mirrors [`PublisherStats::track`].
	pub fn track(&self, _name: &str) -> SubscriberTrack {
		for level in self.levels.iter() {
			level
				.subscriber(self.internal)
				.subscriptions
				.fetch_add(1, Ordering::Relaxed);
			ensure_task(level);
		}
		SubscriberTrack {
			levels: self.levels.clone(),
			internal: self.internal,
		}
	}
}

impl Drop for SubscriberStats {
	fn drop(&mut self) {
		for level in self.levels.iter() {
			level
				.subscriber(self.internal)
				.broadcasts_closed
				.fetch_add(1, Ordering::Relaxed);
		}
	}
}

/// RAII subscription guard for the publisher role.
#[must_use = "drop the guard to record the subscription as closed"]
pub struct PublisherTrack {
	levels: Arc<[Arc<Level>]>,
	internal: bool,
}

impl PublisherTrack {
	/// Bumps `frames` once.
	pub fn frame(&self) {
		for level in self.levels.iter() {
			level.publisher(self.internal).frames.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// Bumps `bytes` by `n`.
	pub fn bytes(&self, n: u64) {
		for level in self.levels.iter() {
			level.publisher(self.internal).bytes.fetch_add(n, Ordering::Relaxed);
		}
	}

	/// Bumps `groups` once.
	pub fn group(&self) {
		for level in self.levels.iter() {
			level.publisher(self.internal).groups.fetch_add(1, Ordering::Relaxed);
		}
	}
}

impl Drop for PublisherTrack {
	fn drop(&mut self) {
		for level in self.levels.iter() {
			level
				.publisher(self.internal)
				.subscriptions_closed
				.fetch_add(1, Ordering::Relaxed);
		}
	}
}

/// RAII subscription guard for the subscriber role.
#[must_use = "drop the guard to record the subscription as closed"]
pub struct SubscriberTrack {
	levels: Arc<[Arc<Level>]>,
	internal: bool,
}

impl SubscriberTrack {
	/// Bumps `frames` once.
	pub fn frame(&self) {
		for level in self.levels.iter() {
			level.subscriber(self.internal).frames.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// Bumps `bytes` by `n`.
	pub fn bytes(&self, n: u64) {
		for level in self.levels.iter() {
			level.subscriber(self.internal).bytes.fetch_add(n, Ordering::Relaxed);
		}
	}

	/// Bumps `groups` once.
	pub fn group(&self) {
		for level in self.levels.iter() {
			level.subscriber(self.internal).groups.fetch_add(1, Ordering::Relaxed);
		}
	}
}

impl Drop for SubscriberTrack {
	fn drop(&mut self) {
		for level in self.levels.iter() {
			level
				.subscriber(self.internal)
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
		let pub_ext = match broadcast.create_track(Track {
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
		let pub_int = match broadcast.create_track(Track {
			name: "publisher_internal".into(),
			priority: 0,
		}) {
			Ok(t) => t,
			Err(err) => {
				tracing::warn!(?err, "stats: failed to create publisher_internal track");
				clear_task(&level);
				return;
			}
		};
		let sub_ext = match broadcast.create_track(Track {
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
		let sub_int = match broadcast.create_track(Track {
			name: "subscriber_internal".into(),
			priority: 0,
		}) {
			Ok(t) => t,
			Err(err) => {
				tracing::warn!(?err, "stats: failed to create subscriber_internal track");
				clear_task(&level);
				return;
			}
		};
		level.origin.publish_broadcast(&level.advertised, broadcast.consume());
		(broadcast, pub_ext, pub_int, sub_ext, sub_int)
	};
	let (broadcast, mut pub_ext, mut pub_int, mut sub_ext, mut sub_int) = setup;

	let mut tick = tokio::time::interval(Duration::from_secs(1));
	tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

	loop {
		tick.tick().await;

		let Some(level) = weak.upgrade() else {
			return;
		};

		let any_active = role_active(&level.publisher_external)
			|| role_active(&level.publisher_internal)
			|| role_active(&level.subscriber_external)
			|| role_active(&level.subscriber_internal);

		if !any_active {
			// Take the task slot under the lock and re-check. Any subscribe that
			// raced with us either landed before we set None (so it sees Some
			// and won't respawn) or after, in which case it spawns a fresh task.
			let mut slot = level.task.lock();
			let still_idle = !role_active(&level.publisher_external)
				&& !role_active(&level.publisher_internal)
				&& !role_active(&level.subscriber_external)
				&& !role_active(&level.subscriber_internal);
			if still_idle {
				*slot = None;
				drop(slot);
				drop(level);
				// Drop `broadcast` to unannounce. Leftover producers/consumers
				// follow the existing `closed()` watcher in OriginProducer.
				drop(broadcast);
				return;
			}
		}

		// Always emit a snapshot for every track. Idle roles see their counters
		// held steady; that itself is informative for a billing service.
		write_snapshot(&mut pub_ext, "publisher", &level, level.publisher_external.snapshot());
		write_snapshot(
			&mut pub_int,
			"publisher_internal",
			&level,
			level.publisher_internal.snapshot(),
		);
		write_snapshot(&mut sub_ext, "subscriber", &level, level.subscriber_external.snapshot());
		write_snapshot(
			&mut sub_int,
			"subscriber_internal",
			&level,
			level.subscriber_internal.snapshot(),
		);
	}
}

fn role_active<R: RoleCounters>(role: &R) -> bool {
	role.subscriptions() > role.subscriptions_closed()
}

fn clear_task(level: &Level) {
	*level.task.lock() = None;
}

fn write_snapshot(track: &mut crate::TrackProducer, role: &str, level: &Level, snap: RoleSnapshot) {
	use std::fmt::Write as _;
	// Hand-rolled JSON keeps serde optional in moq-lite while still producing valid output.
	let mut buf = String::with_capacity(256);
	buf.push('{');
	buf.push_str("\"v\":1,\"name\":");
	write_json_str(&mut buf, &level.name);
	buf.push_str(",\"level\":");
	write_json_str(&mut buf, level.level_key.as_str());
	buf.push_str(",\"role\":");
	write_json_str(&mut buf, role);
	let _ = write!(
		&mut buf,
		",\"ts_ms\":{},\"broadcasts\":{},\"broadcasts_closed\":{},\"subscriptions\":{},\"subscriptions_closed\":{},\"bytes\":{},\"frames\":{},\"groups\":{}",
		now_ms(),
		snap.broadcasts,
		snap.broadcasts_closed,
		snap.subscriptions,
		snap.subscriptions_closed,
		snap.bytes,
		snap.frames,
		snap.groups,
	);
	buf.push('}');

	if let Err(err) = track.write_frame(buf.into_bytes()) {
		tracing::debug!(?err, role, level = %level.advertised, "stats: failed to write snapshot frame");
	}
}

fn write_json_str(buf: &mut String, s: &str) {
	use std::fmt::Write as _;
	buf.push('"');
	for ch in s.chars() {
		match ch {
			'"' => buf.push_str("\\\""),
			'\\' => buf.push_str("\\\\"),
			'\n' => buf.push_str("\\n"),
			'\r' => buf.push_str("\\r"),
			'\t' => buf.push_str("\\t"),
			c if (c as u32) < 0x20 => {
				let _ = write!(buf, "\\u{:04x}", c as u32);
			}
			c => buf.push(c),
		}
	}
	buf.push('"');
}

fn now_ms() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map(|d| d.as_millis() as u64)
		.unwrap_or(0)
}

#[derive(Debug, Default, Clone, Copy)]
struct RoleSnapshot {
	broadcasts: u64,
	broadcasts_closed: u64,
	subscriptions: u64,
	subscriptions_closed: u64,
	bytes: u64,
	frames: u64,
	groups: u64,
}

/// Compute the level prefix keys this broadcast contributes to.
///
/// The keys are the prefixes of the broadcast path with 0..N segments, where N is
/// `min(levels, segments)`. The key with `segments` segments is intentionally
/// omitted: it would be equal to the broadcast path itself, which carries no
/// aggregation value.
fn level_keys(broadcast: &Path, levels: u32) -> Vec<PathOwned> {
	if levels == 0 || broadcast.is_empty() {
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
	async fn external_publisher_bumps_only_external_publisher_counters() {
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
		assert_eq!(root.publisher_external.frames.load(Relaxed), 1);
		assert_eq!(root.publisher_external.bytes.load(Relaxed), 100);
		assert_eq!(root.publisher_external.groups.load(Relaxed), 1);
		assert_eq!(root.publisher_external.subscriptions.load(Relaxed), 1);
		assert_eq!(root.publisher_external.subscriptions_closed.load(Relaxed), 1);
		assert_eq!(root.publisher_external.broadcasts.load(Relaxed), 1);
		assert_eq!(root.publisher_external.broadcasts_closed.load(Relaxed), 1);
		// Internal must remain untouched.
		assert_eq!(root.publisher_internal.bytes.load(Relaxed), 0);
		assert_eq!(root.publisher_internal.broadcasts.load(Relaxed), 0);
		assert_eq!(root.subscriber_external.bytes.load(Relaxed), 0);
		assert_eq!(root.subscriber_internal.bytes.load(Relaxed), 0);
	}

	#[tokio::test]
	async fn internal_publisher_bumps_only_internal_publisher_counters() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let stats = Stats::new("use", 1, origin).internal();
		assert!(stats.is_internal());

		let bs = stats.broadcast("demo/bbb");
		let p = bs.publisher();
		let track = p.track("video");
		track.frame();
		track.bytes(100);
		track.group();
		drop(track);
		drop(p);

		let entries = stats.inner.entries.lock();
		let root = entries.get(&PathOwned::from("")).expect("root level");
		assert_eq!(root.publisher_internal.frames.load(Relaxed), 1);
		assert_eq!(root.publisher_internal.bytes.load(Relaxed), 100);
		assert_eq!(root.publisher_internal.groups.load(Relaxed), 1);
		assert_eq!(root.publisher_internal.subscriptions.load(Relaxed), 1);
		assert_eq!(root.publisher_internal.subscriptions_closed.load(Relaxed), 1);
		assert_eq!(root.publisher_internal.broadcasts.load(Relaxed), 1);
		assert_eq!(root.publisher_internal.broadcasts_closed.load(Relaxed), 1);
		// External must remain untouched.
		assert_eq!(root.publisher_external.bytes.load(Relaxed), 0);
		assert_eq!(root.publisher_external.broadcasts.load(Relaxed), 0);
	}

	#[tokio::test]
	async fn external_subscriber_bumps_only_external_subscriber_counters() {
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
		assert_eq!(root.subscriber_external.frames.load(Relaxed), 1);
		assert_eq!(root.subscriber_external.bytes.load(Relaxed), 50);
		assert_eq!(root.subscriber_external.broadcasts.load(Relaxed), 1);
		assert_eq!(root.subscriber_external.subscriptions.load(Relaxed), 1);
		assert_eq!(root.subscriber_internal.bytes.load(Relaxed), 0);
		assert_eq!(root.publisher_external.bytes.load(Relaxed), 0);
	}

	#[tokio::test]
	async fn internal_and_external_share_level_state() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let stats = Stats::new("use", 1, origin);
		let internal = stats.internal();

		// Open a broadcast on each tier.
		let _g1 = stats.broadcast("foo/bar").publisher();
		let _g2 = internal.broadcast("foo/bar").publisher();

		// Both should resolve to the same Level (only one entry).
		let entries = stats.inner.entries.lock();
		assert_eq!(entries.len(), 1);
		let root = entries.get(&PathOwned::from("")).expect("root level");
		assert_eq!(root.publisher_external.broadcasts.load(Relaxed), 1);
		assert_eq!(root.publisher_internal.broadcasts.load(Relaxed), 1);
	}

	#[tokio::test]
	async fn tier_bool_picks_the_right_clone() {
		let origin = Origin::random().produce();
		let stats = Stats::new("use", 1, origin);
		assert!(!stats.tier(false).is_internal());
		assert!(stats.tier(true).is_internal());
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
		assert_eq!(root.publisher_external.bytes.load(Relaxed), 100);
		assert_eq!(demo.publisher_external.bytes.load(Relaxed), 100);
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
		let (path, broadcast) = consumer.announced_hidden().await.expect("expected announce");
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
		let (_, broadcast) = consumer.announced_hidden().await.expect("expected announce");
		assert!(broadcast.is_some());

		drop(track);
		drop(p);
		drop(bs);

		tokio::time::advance(Duration::from_secs(2)).await;
		let (path, broadcast) = consumer.announced_hidden().await.expect("expected unannounce");
		assert_eq!(path, Path::new(".stats/use"));
		assert!(broadcast.is_none());
	}

	#[tokio::test]
	async fn task_stays_alive_while_internal_role_active() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let stats = Stats::new("use", 1, origin.clone());
		let internal = stats.internal();
		let mut consumer = origin.consume();

		let ext_bs = stats.broadcast("foo/bar");
		let int_bs = internal.broadcast("foo/bar");
		let ext_p = ext_bs.publisher();
		let int_p = int_bs.publisher();
		let ext_track = ext_p.track("video");
		let int_track = int_p.track("video");

		tokio::time::advance(Duration::from_millis(1)).await;
		let (_, broadcast) = consumer.announced_hidden().await.expect("expected announce");
		assert!(broadcast.is_some());

		// Drop the external side. Internal keeps the task alive.
		drop(ext_track);
		drop(ext_p);
		drop(ext_bs);

		tokio::time::advance(Duration::from_secs(3)).await;
		assert!(consumer.try_announced_hidden().is_none());

		// Drop the internal side too -> task exits.
		drop(int_track);
		drop(int_p);
		drop(int_bs);

		tokio::time::advance(Duration::from_secs(2)).await;
		let (_, broadcast) = consumer.announced_hidden().await.expect("expected unannounce");
		assert!(broadcast.is_none());
	}
}
