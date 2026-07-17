//! The publishing half: drain a [`Registry`] on an interval into stats tracks.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};
use std::time::Duration;

use moq_net::stats::{Presence, Registry, Role, Tier, Traffic};
use moq_net::{Path, PathOwned, broadcast, origin};
use serde::Serialize;
use web_async::spawn;

use crate::{COMPRESSED_SUFFIX, SessionsFrame, TrafficFrame, sessions_track, traffic_track};

/// Settings for a [`Producer`]. Construct with [`Config::new`] and chain the
/// `with_*` setters (e.g. `Config::new().with_origin(origin).with_prefix(".foo")`),
/// then hand it to [`Producer::new`].
///
/// With no origin set the resulting producer is a no-op: its registry is
/// disabled (bumps are dropped) and no task spawns. Call [`Config::with_origin`]
/// to publish.
#[derive(Clone)]
#[non_exhaustive]
pub struct Config {
	/// Origin that receives the stats broadcast's `publish_broadcast` calls.
	/// When `None`, [`Producer::new`] spawns no task and publishes nothing.
	pub origin: Option<origin::Producer>,
	/// Top-level path stats are published under (default `.stats`). The full
	/// advertised path is `<prefix>/node/<node>` (or `<prefix>/node` when
	/// `node` is unset). Also the registry's exclude prefix, so serving a
	/// stats broadcast doesn't generate more stats.
	pub prefix: PathOwned,
	/// Node suffix that disambiguates broadcasts from different relays sharing a
	/// cluster origin. Set this on every node in multi-relay deployments. May be
	/// multi-segment (e.g. `sjc/1`, `sjc/2`) so a region with multiple hosts can
	/// nest under a shared region key. An empty path is treated as unset.
	/// Default none.
	pub node: Option<PathOwned>,
	/// How long the publish task waits between drains. Default 1s.
	pub interval: Duration,
	/// How many leading broadcast-path segments to use as a grouping key.
	///
	/// Default `0` publishes one `<prefix>/node/<node>` broadcast carrying every
	/// path. `1` publishes one broadcast per first segment at
	/// `<prefix>/<group>/node/<node>`, and larger values include more leading
	/// segments. Group broadcasts are announced while their group has live traffic;
	/// at depth `0`, the single broadcast stays announced for the producer's life.
	pub depth: usize,
}

impl Config {
	/// A config with default settings: no origin (no-op), `.stats` prefix, 1s
	/// interval, and no node suffix. Call [`Self::with_origin`] to actually
	/// publish.
	pub fn new() -> Self {
		Self {
			origin: None,
			prefix: PathOwned::from(".stats"),
			node: None,
			interval: Duration::from_secs(1),
			depth: 0,
		}
	}

	/// Set the origin to publish the stats broadcast on. Without this the
	/// producer is a no-op.
	pub fn with_origin(mut self, origin: impl Into<Option<origin::Producer>>) -> Self {
		self.origin = origin.into();
		self
	}

	/// Override the top-level prefix (default `.stats`).
	pub fn with_prefix(mut self, prefix: impl Into<PathOwned>) -> Self {
		self.prefix = prefix.into();
		self
	}

	/// Override the publish interval (default 1s).
	pub fn with_interval(mut self, interval: Duration) -> Self {
		self.interval = interval;
		self
	}

	/// Set the node suffix (default none). An empty path is treated as unset.
	pub fn with_node(mut self, node: impl Into<Option<PathOwned>>) -> Self {
		self.node = node.into();
		self
	}

	/// Set the grouping depth (default 0, a single broadcast). See [`Self::depth`].
	pub fn with_depth(mut self, depth: usize) -> Self {
		self.depth = depth;
		self
	}
}

impl Default for Config {
	fn default() -> Self {
		Self::new()
	}
}

/// Keeps the publish task alive: the task holds only a `Weak` to this, so it
/// exits once the last [`Producer`] clone drops.
struct Keepalive;

/// Publishes a [`Registry`]'s counters as stats broadcasts. Cheap to clone.
///
/// [`Producer::new`] builds the registry itself (wiring the config's prefix as
/// its exclude prefix) and spawns the publish task; hand sessions tier-scoped
/// handles via [`Registry::tier`] on [`Producer::registry`]. The task drains
/// the registry every interval and writes a frame per changed track, running
/// until the last [`Producer`] clone is dropped.
#[derive(Clone)]
pub struct Producer {
	registry: Registry,
	/// `None` for a no-op producer (config had no origin): no task was spawned
	/// and the registry is disabled.
	_keepalive: Option<Arc<Keepalive>>,
}

impl Producer {
	/// Build a producer from `config`.
	///
	/// When `config` has an origin, this spawns the publish task immediately
	/// and announces the stats broadcast; the task runs until the last
	/// [`Producer`] clone is dropped. With no origin the producer is a no-op
	/// (its registry is disabled, nothing is published) and no task spawns, so
	/// it's safe to build outside an async runtime.
	pub fn new(config: Config) -> Self {
		let Config {
			origin,
			prefix,
			node,
			interval,
			depth,
		} = config;
		// An empty path after normalization is indistinguishable from "no node
		// set"; collapse it so downstream code only sees a single representation.
		// We do this here (not in `with_node`) so a directly-assigned
		// `config.node` is normalized too.
		let node = node.filter(|p| !p.is_empty());

		let Some(origin) = origin else {
			return Self {
				registry: Registry::disabled(),
				_keepalive: None,
			};
		};

		let registry = Registry::new(prefix.clone());
		let keepalive = Arc::new(Keepalive);
		let task = Task {
			registry: registry.clone(),
			origin,
			prefix,
			node,
			depth,
			interval,
		};
		spawn(task.run(Arc::downgrade(&keepalive)));

		Self {
			registry,
			_keepalive: Some(keepalive),
		}
	}

	/// The registry this producer drains. Hand sessions tier-scoped handles via
	/// [`Registry::tier`]; read node totals back with [`Registry::snapshot`].
	/// Disabled (all bumps no-op) for a no-op producer.
	pub fn registry(&self) -> &Registry {
		&self.registry
	}
}

/// Everything the publish task owns.
struct Task {
	registry: Registry,
	origin: origin::Producer,
	prefix: PathOwned,
	node: Option<PathOwned>,
	depth: usize,
	interval: Duration,
}

impl Task {
	/// Publishes stats broadcasts and writes a frame per drain. Runs until
	/// every [`Producer`] clone is dropped (`weak.upgrade()` returns `None`).
	async fn run(self, weak: Weak<Keepalive>) {
		let node = self.node.as_ref().map(|p| p.as_str());
		let mut groups: HashMap<PathOwned, GroupPublisher> = HashMap::new();

		if self.depth == 0 {
			let Some(group) = GroupPublisher::create(&self.origin, &self.prefix, &Path::empty(), node) else {
				return;
			};
			groups.insert(Path::empty().to_owned(), group);
		}

		let mut ticker = web_async::time::interval(self.interval);
		ticker.set_missed_tick_behavior(web_async::time::MissedTickBehavior::Delay);

		loop {
			ticker.tick().await;

			if weak.upgrade().is_none() {
				return;
			}

			// Drain the registry: current per-broadcast values, with dead
			// entries pruned (their final values are still in this report).
			let report = self.registry.report();

			let mut entries_by_group: HashMap<PathOwned, Vec<&moq_net::stats::TrafficEntry>> = HashMap::new();
			for entry in &report.traffic {
				entries_by_group
					.entry(group_key(entry.path.as_str(), self.depth))
					.or_default()
					.push(entry);
			}

			let mut sessions_by_group: HashMap<PathOwned, Vec<&moq_net::stats::SessionEntry>> = HashMap::new();
			for entry in &report.sessions {
				sessions_by_group
					.entry(group_key(entry.root.as_str(), self.depth))
					.or_default()
					.push(entry);
			}

			let mut active: HashSet<PathOwned> = HashSet::new();
			active.extend(entries_by_group.keys().cloned());
			active.extend(sessions_by_group.keys().cloned());
			if self.depth == 0 {
				active.insert(Path::empty().to_owned());
			}

			for group in &active {
				if !groups.contains_key(group) {
					let Some(publisher) = GroupPublisher::create(&self.origin, &self.prefix, group, node) else {
						continue;
					};
					groups.insert(group.clone(), publisher);
				}
				let publisher = groups.get_mut(group).expect("just inserted");

				let mut frames: HashMap<String, TrafficFrame> = HashMap::new();
				if let Some(group_entries) = entries_by_group.get(group) {
					for entry in group_entries {
						let slots = publisher
							.local
							.entry(entry.path.clone())
							.or_default()
							.entry(entry.tier.clone())
							.or_default();
						process_slot(entry.publisher, &mut slots.publisher, |snap| {
							frames
								.entry(traffic_track(&entry.tier, Role::Publisher, false))
								.or_default()
								.insert(entry.path.as_str().to_string(), snap);
						});
						process_slot(entry.subscriber, &mut slots.subscriber, |snap| {
							frames
								.entry(traffic_track(&entry.tier, Role::Subscriber, false))
								.or_default()
								.insert(entry.path.as_str().to_string(), snap);
						});
					}
				}

				let mut session_frames: HashMap<String, SessionsFrame> = HashMap::new();
				if let Some(group_sessions) = sessions_by_group.get(group) {
					for entry in group_sessions {
						let state = publisher
							.session_local
							.entry(entry.tier.clone())
							.or_default()
							.entry(entry.root.clone())
							.or_default();
						process_session_slot(entry.presence, state, |snap| {
							session_frames
								.entry(sessions_track(&entry.tier, false))
								.or_default()
								.insert(entry.root.as_str().to_string(), snap);
						});
					}
				}

				flush_dynamic(&mut publisher.broadcast, &mut publisher.traffic_tracks, &frames);
				flush_dynamic(&mut publisher.broadcast, &mut publisher.session_tracks, &session_frames);
			}

			// Drop change-detection state for entries the report no longer
			// carries (they were pruned on a previous drain).
			let reported: HashSet<(&PathOwned, &Tier)> =
				report.traffic.iter().map(|entry| (&entry.path, &entry.tier)).collect();
			let reported_sessions: HashSet<(&Tier, &PathOwned)> =
				report.sessions.iter().map(|entry| (&entry.tier, &entry.root)).collect();
			for publisher in groups.values_mut() {
				publisher.local.retain(|path, tiers| {
					tiers.retain(|tier, _| reported.contains(&(path, tier)));
					!tiers.is_empty()
				});
				publisher.session_local.retain(|tier, roots| {
					roots.retain(|root, _| reported_sessions.contains(&(tier, root)));
					!roots.is_empty()
				});
			}

			groups.retain(|group, _| active.contains(group));
		}
	}
}

/// A plain track and its `.z` sibling, kept in lockstep. The plain side runs
/// moq-json with deltas and compression off, which is wire-identical to
/// writing each frame as its own single-frame group; the compressed side uses
/// merge-patch deltas inside a shared DEFLATE window.
struct TrackPair<T> {
	plain: moq_json::snapshot::Producer<T>,
	compressed: moq_json::snapshot::Producer<T>,
}

impl<T: Serialize> TrackPair<T> {
	fn create(broadcast: &mut broadcast::Producer, name: &str) -> Result<Self, moq_net::Error> {
		let plain_track = broadcast.create_track(name, None)?;
		let compressed_track = broadcast.create_track(format!("{name}{COMPRESSED_SUFFIX}").as_str(), None)?;

		let mut plain_config = moq_json::snapshot::ProducerConfig::default();
		plain_config.delta_ratio = 0;
		let mut compressed_config = moq_json::snapshot::ProducerConfig::default();
		compressed_config.compression = true;

		Ok(Self {
			plain: moq_json::snapshot::Producer::new(plain_track, plain_config),
			compressed: moq_json::snapshot::Producer::new(compressed_track, compressed_config),
		})
	}

	/// Publish `frame` on both flavors; moq-json skips unchanged values.
	fn update(&mut self, name: &str, frame: &T) {
		if let Err(err) = self.plain.update(frame) {
			tracing::debug!(?err, name, "stats: failed to write frame");
		}
		if let Err(err) = self.compressed.update(frame) {
			tracing::debug!(?err, name, "stats: failed to write compressed frame");
		}
	}
}

/// Ensure a track pair exists for every frame this drain produced, then push
/// each pair its frame (an empty one when the drain had nothing for it, so a
/// track whose last entry closed transitions to `{}` exactly once).
fn flush_dynamic<T: Serialize + Default>(
	broadcast: &mut broadcast::Producer,
	tracks: &mut HashMap<String, TrackPair<T>>,
	frames: &HashMap<String, T>,
) {
	for name in frames.keys() {
		if !tracks.contains_key(name) {
			match TrackPair::create(broadcast, name) {
				Ok(pair) => {
					tracks.insert(name.clone(), pair);
				}
				Err(err) => tracing::warn!(?err, name, "stats: failed to create track"),
			}
		}
	}

	let empty = T::default();
	for (name, pair) in tracks.iter_mut() {
		pair.update(name, frames.get(name).unwrap_or(&empty));
	}
}

/// One group stats broadcast and its change-detection state.
struct GroupPublisher {
	_publish: origin::Publish,
	broadcast: broadcast::Producer,
	traffic_tracks: HashMap<String, TrackPair<TrafficFrame>>,
	session_tracks: HashMap<String, TrackPair<SessionsFrame>>,
	local: HashMap<PathOwned, HashMap<Tier, SideSlots>>,
	session_local: HashMap<Tier, HashMap<PathOwned, SessionSlotState>>,
}

impl GroupPublisher {
	fn create(origin: &origin::Producer, prefix: &Path, group: &Path, node: Option<&str>) -> Option<Self> {
		let mut broadcast = broadcast::Info::new().produce();
		let mut traffic_tracks = HashMap::new();
		let mut session_tracks = HashMap::new();

		// The default tier's tracks always exist, even while idle.
		let tier = Tier::default();
		for role in [Role::Publisher, Role::Subscriber] {
			let name = traffic_track(&tier, role, false);
			match TrackPair::create(&mut broadcast, &name) {
				Ok(pair) => {
					traffic_tracks.insert(name, pair);
				}
				Err(err) => {
					tracing::warn!(?err, name, "stats: failed to create track");
					return None;
				}
			}
		}
		let name = sessions_track(&tier, false);
		match TrackPair::create(&mut broadcast, &name) {
			Ok(pair) => {
				session_tracks.insert(name, pair);
			}
			Err(err) => {
				tracing::warn!(?err, name, "stats: failed to create track");
				return None;
			}
		}

		let advertised = advertised_path(prefix, group, node);
		let publish = match origin.publish_broadcast(&advertised, &broadcast) {
			Ok(publish) => publish,
			Err(err) => {
				tracing::warn!(advertised = %advertised, ?err, "stats: origin rejected stats broadcast");
				return None;
			}
		};
		tracing::debug!(advertised = %advertised, "stats: publishing broadcast");

		Some(Self {
			_publish: publish,
			broadcast,
			traffic_tracks,
			session_tracks,
			local: HashMap::new(),
			session_local: HashMap::new(),
		})
	}
}

/// Change-detection state for one `(path, tier, side)` slot, owned by the
/// publish task. The task is single-threaded so this needs no atomics.
#[derive(Default)]
struct SlotState {
	/// Last [`Traffic`] we emitted for this slot, used to detect changes that
	/// warrant re-emission.
	prev_emitted: Option<Traffic>,
}

/// Change-detection state for one `(path, tier)`: a [`SlotState`] per side.
#[derive(Default)]
struct SideSlots {
	publisher: SlotState,
	subscriber: SlotState,
}

/// Change-detection state for one session-track root, mirroring [`SlotState`].
#[derive(Default)]
struct SessionSlotState {
	prev_emitted: Option<Presence>,
}

/// Per-drain work for a single `(side, tier)` slot: update the slot's
/// `prev_emitted` and hand `snap` to `emit` iff the slot is live or changed
/// this drain.
fn process_slot(snap: Traffic, slot_state: &mut SlotState, emit: impl FnOnce(Traffic)) {
	// A slot is live while any open counter still exceeds its `*_closed`
	// counterpart: a guard is held, so a subscription could begin at any
	// moment. Live slots are emitted every drain so a downstream "currently
	// active" view always sees the full set. Once every pair is equal no
	// traffic can flow and the entry is on its way out (the registry pruned
	// it as soon as the last guard released its handle).
	let live = !snap.is_idle();

	// Include the entry whenever it's live OR its snapshot changed this
	// drain. Change-driven inclusion catches bumps since the previous drain
	// (incl. sub-interval flickers) and emits the final close snapshot on the
	// drain a slot transitions to fully closed.
	//
	// `None` (slot never emitted) is treated as the default Traffic so a
	// first-drain all-zeros snap on an unused tier-side slot doesn't count
	// as a "change". Without this, every entry would surface in all four
	// tracks with zeros on the drain after creation even if only one slot
	// is actually in use.
	let prev_snap = slot_state.prev_emitted.unwrap_or_default();
	let changed = snap != prev_snap;
	if changed {
		slot_state.prev_emitted = Some(snap);
	}
	if live || changed {
		emit(snap);
	}
}

/// Per-drain work for one session-track root: same live-or-changed rule as
/// [`process_slot`].
fn process_session_slot(snap: Presence, slot_state: &mut SessionSlotState, emit: impl FnOnce(Presence)) {
	let live = snap.active() > 0;
	let prev_snap = slot_state.prev_emitted.unwrap_or_default();
	let changed = snap != prev_snap;
	if changed {
		slot_state.prev_emitted = Some(snap);
	}
	if live || changed {
		emit(snap);
	}
}

fn group_key(path: &str, depth: usize) -> PathOwned {
	if depth == 0 {
		return Path::empty().to_owned();
	}

	let mut seen = 0;
	let mut end = path.len();
	for (i, b) in path.bytes().enumerate() {
		if b == b'/' {
			seen += 1;
			if seen == depth {
				end = i;
				break;
			}
		}
	}
	Path::new(&path[..end]).to_owned()
}

fn advertised_path(prefix: &Path, group: &Path, node: Option<&str>) -> PathOwned {
	// `<prefix>/<group>/node/<node>`. The group segment is empty at depth 0.
	// The fixed `node` category leaves room for sibling categories (e.g.
	// `<top-prefix>/<group>/cluster` for relay-mesh stats) under the same prefix.
	let mut out = prefix.as_str().to_string();
	if !group.is_empty() {
		out.push('/');
		out.push_str(group.as_str());
	}
	out.push_str("/node");
	if let Some(node) = node {
		out.push('/');
		out.push_str(node);
	}
	PathOwned::from(out)
}

#[cfg(test)]
mod tests {
	use std::collections::BTreeMap;

	use moq_net::{Origin, announce, track};

	use super::*;

	fn test_producer(node: Option<&str>) -> (Producer, origin::Producer) {
		let origin = Origin::random().produce();
		let producer = Producer::new(
			Config::new()
				.with_origin(origin.clone())
				.with_node(node.map(|s| PathOwned::from(s.to_string()))),
		);
		(producer, origin)
	}

	/// Awaits the stats announce and returns its broadcast.
	async fn announced(origin: &origin::Producer) -> (String, moq_net::broadcast::Consumer) {
		let mut consumer = origin.consume().announced();
		tokio::time::advance(Duration::from_millis(1)).await;
		let announce::Update { path, broadcast } = consumer.next().await.expect("expected announce");
		(path.as_str().to_string(), broadcast.expect("active"))
	}

	/// Advance past one publish interval so the task drains and writes frames.
	async fn drive_tick() {
		tokio::time::advance(Duration::from_millis(1100)).await;
		// Yield several times to let the task wake, drain the registry, write
		// the frames, and re-await the next tick.
		for _ in 0..4 {
			tokio::task::yield_now().await;
		}
	}

	/// Reads the first frame off a plain track as raw JSON, pinning the plain
	/// wire format (a full JSON object per frame, no compression).
	async fn read_frame(broadcast: &moq_net::broadcast::Consumer, name: &str) -> BTreeMap<String, Traffic> {
		let mut track = subscribe(broadcast, name).await;
		let frame = track.read_frame().await.expect("ok").expect("frame");
		serde_json::from_slice(&frame.payload).expect("json parse")
	}

	async fn read_session_frame(broadcast: &moq_net::broadcast::Consumer, name: &str) -> BTreeMap<String, Presence> {
		let mut track = subscribe(broadcast, name).await;
		let frame = track.read_frame().await.expect("ok").expect("frame");
		serde_json::from_slice(&frame.payload).expect("json parse")
	}

	async fn subscribe(broadcast: &moq_net::broadcast::Consumer, name: &str) -> track::Subscriber {
		broadcast
			.track(name)
			.expect("track")
			.subscribe(None)
			.await
			.expect("subscribe")
	}

	/// The advertised path normalizes a messy node suffix and drops an
	/// all-empty one. Observed through the announced path, since the task
	/// announces at construction.
	#[tokio::test(start_paused = true)]
	async fn new_normalizes_and_drops_empty_node() {
		let (_producer, origin) = test_producer(Some("/sjc//1/"));
		assert_eq!(announced(&origin).await.0, ".stats/node/sjc/1");

		let (_producer, origin) = test_producer(Some("///"));
		assert_eq!(announced(&origin).await.0, ".stats/node");
	}

	#[tokio::test(start_paused = true)]
	async fn single_broadcast_path_announced() {
		// No matter how many broadcasts get bumped, exactly one stats
		// broadcast is announced (the per-node aggregate).
		let (producer, origin) = test_producer(Some("sjc/1"));

		let bs1 = producer.registry().tier(Tier::default()).broadcast("foo/bar");
		let _t1 = bs1.publisher().track("video");
		let bs2 = producer.registry().tier(Tier::default()).broadcast("baz/qux");
		let _t2 = bs2.publisher().track("video");

		assert_eq!(announced(&origin).await.0, ".stats/node/sjc/1");
	}

	#[tokio::test(start_paused = true)]
	async fn task_announces_without_node_suffix() {
		let (producer, origin) = test_producer(None);
		let bs = producer.registry().tier(Tier::default()).broadcast("foo/bar");
		let _t = bs.publisher().track("video");
		assert_eq!(announced(&origin).await.0, ".stats/node");
	}

	#[tokio::test(start_paused = true)]
	async fn frame_emits_expected_counters() {
		let (producer, origin) = test_producer(Some("sjc"));
		let stats = producer.registry().tier(Tier::default());
		let bs = stats.broadcast("foo/bar");
		let track = bs.publisher().track("video");
		track.bytes(42);
		track.frame();
		let sessions = stats.publisher_broadcasts();
		let _sub = sessions.subscribe("foo/bar");

		drive_tick().await;

		let (_, broadcast) = announced(&origin).await;
		let frame = read_frame(&broadcast, "publisher.json").await;
		let snap = frame.get("foo/bar").expect("foo/bar entry");
		assert_eq!(snap.announced, 1, "publisher() guard bumps announced");
		assert_eq!(snap.broadcasts, 1, "one session subscribed");
		assert_eq!(snap.subscriptions, 1);
		assert_eq!(snap.bytes, 42);
		assert_eq!(snap.frames, 1);
	}

	#[tokio::test(start_paused = true)]
	async fn announced_bytes_surfaces_in_frame() {
		let (producer, origin) = test_producer(Some("sjc"));
		let bs = producer.registry().tier(Tier::default()).broadcast("foo/bar");
		let _guard = bs.publisher();
		bs.publisher_announced_bytes(123);

		drive_tick().await;

		let (_, broadcast) = announced(&origin).await;
		let frame = read_frame(&broadcast, "publisher.json").await;
		let snap = frame.get("foo/bar").expect("foo/bar entry");
		assert_eq!(snap.announced, 1);
		assert_eq!(snap.announced_bytes, 123);
	}

	#[tokio::test(start_paused = true)]
	async fn announced_decouples_from_broadcasts() {
		// publisher() (announce) with no subscription should bump announced but
		// NOT broadcasts (which only counts sessions with an active sub).
		let (producer, origin) = test_producer(Some("sjc"));
		let bs = producer.registry().tier(Tier::default()).broadcast("foo/bar");
		let _guard = bs.publisher();

		drive_tick().await;

		let (_, broadcast) = announced(&origin).await;
		let frame = read_frame(&broadcast, "publisher.json").await;
		let snap = frame.get("foo/bar").expect("foo/bar entry");
		assert_eq!(snap.announced, 1);
		assert_eq!(snap.broadcasts, 0, "no subscription, no broadcasts sentinel");
		assert_eq!(snap.subscriptions, 0);
	}

	#[tokio::test(start_paused = true)]
	async fn short_lived_sub_is_surfaced() {
		// A subscription that opens AND closes within a single drain window
		// must still surface as a complete broadcasts open/close cycle. The
		// cumulative counters retain broadcasts=1/broadcasts_closed=1, and the
		// change-driven inclusion surfaces the entry even though it's net-idle
		// by drain time.
		let (producer, origin) = test_producer(Some("sjc"));
		let stats = producer.registry().tier(Tier::default());
		let bs = stats.broadcast("foo/bar");
		let sessions = stats.publisher_broadcasts();
		{
			let track = bs.publisher().track("video");
			track.bytes(123);
			track.frame();
			let _sub = sessions.subscribe("foo/bar");
			// track + sub dropped here, all within the first interval
		}

		drive_tick().await;

		let (_, broadcast) = announced(&origin).await;
		let frame = read_frame(&broadcast, "publisher.json").await;
		let snap = frame.get("foo/bar").expect("foo/bar entry");
		// One session opened then closed a subscription within the drain.
		assert_eq!(snap.subscriptions, 1);
		assert_eq!(snap.subscriptions_closed, 1);
		assert_eq!(snap.broadcasts, 1, "one session subscribed");
		assert_eq!(snap.broadcasts_closed, 1);
		assert_eq!(snap.bytes, 123);
		assert_eq!(snap.frames, 1);
	}

	#[tokio::test(start_paused = true)]
	async fn session_track_surfaces_by_root() {
		let (producer, origin) = test_producer(Some("sjc"));
		let _a = producer.registry().tier(Tier::default()).session("acme");
		let _b = producer.registry().tier(Tier::default()).session("acme");
		let _c = producer.registry().tier(Tier::new("internal")).session("peer");

		drive_tick().await;

		let (_, broadcast) = announced(&origin).await;
		let frame = read_session_frame(&broadcast, "sessions.json").await;
		let snap = frame.get("acme").expect("root entry");
		assert_eq!(snap.sessions, 2);
		assert_eq!(snap.sessions_closed, 0);
		assert!(
			!frame.contains_key("peer"),
			"internal session must not appear on the external track"
		);

		let snap = *read_session_frame(&broadcast, "internal/sessions.json")
			.await
			.get("peer")
			.expect("internal entry");
		assert_eq!(snap.sessions, 1);
	}

	#[tokio::test(start_paused = true)]
	async fn unused_slots_dont_surface() {
		// A broadcast that only sees default-tier publisher traffic must NOT
		// surface on its sibling default-tier subscriber track, and a tier
		// with no traffic gets no tracks at all.
		let (producer, origin) = test_producer(Some("sjc"));
		let bs = producer.registry().tier(Tier::default()).broadcast("foo/bar");
		let track = bs.publisher().track("video");
		track.frame();

		drive_tick().await;
		drive_tick().await;

		let (_, broadcast) = announced(&origin).await;

		// Default-tier publisher slot SHOULD include foo/bar.
		assert!(
			read_frame(&broadcast, "publisher.json").await.contains_key("foo/bar"),
			"publisher.json must include the active foo/bar entry"
		);

		// The default-tier subscriber slot had zero activity; its first frame
		// must be `{}`, not `{"foo/bar": {all zeros}}`.
		let frame = read_frame(&broadcast, "subscriber.json").await;
		assert!(frame.is_empty(), "subscriber.json must be empty, got {frame:?}");

		// The compressed siblings of the default tracks always exist.
		for name in ["publisher.json.z", "subscriber.json.z", "sessions.json.z"] {
			assert!(broadcast.track(name).is_ok(), "{name} must exist");
		}

		// The internal tier never saw traffic, so its tracks were never created.
		for name in ["internal/publisher.json", "internal/publisher.json.z"] {
			assert!(
				broadcast.track(name).is_err(),
				"{name} must not exist for a tier with no traffic",
			);
		}
	}

	#[test]
	fn advertised_path_with_and_without_node() {
		let prefix = Path::new(".stats");
		let empty = Path::empty();
		assert_eq!(
			advertised_path(&prefix, &empty, Some("sjc")).as_str(),
			".stats/node/sjc"
		);
		assert_eq!(
			advertised_path(&prefix, &empty, Some("sjc/1")).as_str(),
			".stats/node/sjc/1"
		);
		assert_eq!(advertised_path(&prefix, &empty, None).as_str(), ".stats/node");
		assert_eq!(
			advertised_path(&prefix, &Path::new("acme"), Some("sjc")).as_str(),
			".stats/acme/node/sjc"
		);

		let prefix = Path::new("metrics");
		assert_eq!(
			advertised_path(&prefix, &Path::new("demo/room"), Some("lon")).as_str(),
			"metrics/demo/room/node/lon"
		);
	}

	#[test]
	fn group_key_uses_leading_segments() {
		assert_eq!(group_key("acme/room/cam", 0), Path::empty().to_owned());
		assert_eq!(group_key("acme/room/cam", 1), Path::new("acme").to_owned());
		assert_eq!(group_key("acme/room/cam", 2), Path::new("acme/room").to_owned());
		assert_eq!(group_key("acme/room", 3), Path::new("acme/room").to_owned());
	}
}
