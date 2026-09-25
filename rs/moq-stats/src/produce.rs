//! The publishing half: drain a [`Registry`] on an interval into stats tracks.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use std::task::Poll;

use moq_net::stats::{Presence, Registry, Report, Role, Tier, Traffic};
use moq_net::{AsPath, Path, PathOwned, broadcast, kio, origin, track};
use serde::Serialize;
use web_async::spawn;

use crate::{COMPRESSED_SUFFIX, Ext, Stats, sessions_track, traffic_track};

/// Settings for a [`Producer`]. Construct with [`Config::new`] and chain
/// the `with_*` setters (e.g.
/// `Config::new().with_origin(origin).with_prefix(".foo")`), then hand it
/// to [`Producer::new`].
///
/// With no origin set the resulting producer is a no-op: its registry is
/// disabled (bumps are dropped) and no task spawns. Call
/// [`Config::with_origin`] to publish.
#[derive(Clone)]
#[non_exhaustive]
pub struct Config {
	/// Origin the stats broadcasts are created on.
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
	/// The exact broadcast path set by [`Self::at`], replacing the
	/// `prefix`/`node`/`depth` layout.
	path: Option<PathOwned>,
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
			path: None,
		}
	}

	/// A config publishing one broadcast at exactly `path`, the way a client
	/// reports its own stats (`room/alice.stats`), instead of the relay's
	/// `<prefix>/node/<node>` layout. `prefix`, `node`, and `depth` are ignored.
	///
	/// Refuses a path whose last segment does not end in `.stats`, so the
	/// broadcast is always recognizable as telemetry ([`crate::is_stats`]).
	pub fn at(path: impl AsPath) -> crate::Result<Self> {
		let path = path.as_path().to_owned();
		let last = path.as_str().rsplit('/').next().unwrap_or_default();
		if !last.ends_with(crate::STATS_SUFFIX) {
			return Err(crate::Error::NotStats(path));
		}
		Ok(Self {
			path: Some(path),
			..Self::new()
		})
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

/// Cap on concurrently-held consumer-requested (vs traffic-created) track
/// pairs per group broadcast. Requests mint real tracks, so a connected
/// subscriber probing arbitrary tier names must hit a bound - but only while
/// its subscriptions are actually held: a requested pair that loses its last
/// consumer before its tier ever records is reclaimed on the next drain,
/// refunding the cap, so a disconnected prober cannot deny a later collector.
/// A valid-shaped request over the cap parks rather than being rejected (see
/// [`MAX_PARKED_REQUESTS`]). Sized far above any real tier set (a deployment
/// has on the order of ten tiers, three track kinds each).
const MAX_REQUESTED_TRACKS: usize = 64;

/// Cap on parked (valid-shaped, awaiting quota) consumer requests per group
/// broadcast, beyond which new names are rejected outright. Parking instead of
/// rejecting is what keeps a quota-full window from terminally stranding a
/// collector - a consumer that treats one rejection as final would otherwise
/// lose the tier until the broadcast unannounces - so this bound exists only
/// to stop the parked buffer itself growing without limit.
const MAX_PARKED_REQUESTS: usize = 256;

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
///
/// Each entry carries an extension `E` beside its [`Traffic`], attached with
/// [`Producer::entry`]; a relay uses `()`.
pub struct Producer<E: Ext = ()> {
	registry: Registry,
	/// The attached extensions the task drains, `None` for a no-op producer.
	exts: Option<Exts<E>>,
	/// `None` for a no-op producer (config had no origin): no task was spawned
	/// and the registry is disabled.
	_keepalive: Option<Arc<Keepalive>>,
}

impl<E: Ext> Clone for Producer<E> {
	fn clone(&self) -> Self {
		Self {
			registry: self.registry.clone(),
			exts: self.exts.clone(),
			_keepalive: self._keepalive.clone(),
		}
	}
}

/// Extensions attached through [`Entry`] handles, shared with the publish task.
type Exts<E> = Arc<Mutex<ExtTable<E>>>;

struct ExtTable<E> {
	slots: HashMap<ExtKey, ExtSlot<E>>,
	/// Stamped on a slot by every [`Entry::set`], so a drain detects a change
	/// without comparing values, even across a slot's removal and re-creation.
	version: u64,
}

impl<E> Default for ExtTable<E> {
	fn default() -> Self {
		Self {
			slots: HashMap::new(),
			version: 0,
		}
	}
}

/// Which entry an extension is attached to.
#[derive(Clone, PartialEq, Eq, Hash)]
struct ExtKey {
	path: PathOwned,
	tier: Tier,
	role: Role,
}

/// One attached extension.
struct ExtSlot<E> {
	value: E,
	/// The table's version at the last [`Entry::set`].
	version: u64,
	/// Live [`Entry`] handles; the drain removes the slot once this is zero.
	handles: usize,
}

/// Attaches an extension to one entry of a [`Producer`]'s frames, until the
/// last clone drops.
///
/// While held, the entry is reported every drain, even with no traffic. Once
/// dropped, its last value is reported once more and then kept beside the
/// entry's [`Traffic`] for as long as the registry still reports it.
pub struct Entry<E: Ext> {
	exts: Option<Exts<E>>,
	key: ExtKey,
}

impl<E: Ext> Entry<E> {
	/// Replace the extension, reported on the next drain.
	pub fn set(&self, value: E) {
		let Some(exts) = &self.exts else {
			return;
		};
		let mut table = exts.lock().expect("stats extensions poisoned");
		table.version += 1;
		let version = table.version;
		let slot = table.slots.get_mut(&self.key).expect("held entries stay in the table");
		slot.value = value;
		slot.version = version;
	}
}

impl<E: Ext> Clone for Entry<E> {
	fn clone(&self) -> Self {
		if let Some(exts) = &self.exts {
			let mut table = exts.lock().expect("stats extensions poisoned");
			table
				.slots
				.get_mut(&self.key)
				.expect("held entries stay in the table")
				.handles += 1;
		}
		Self {
			exts: self.exts.clone(),
			key: self.key.clone(),
		}
	}
}

impl<E: Ext> Drop for Entry<E> {
	fn drop(&mut self) {
		if let Some(exts) = &self.exts
			&& let Ok(mut table) = exts.lock()
			&& let Some(slot) = table.slots.get_mut(&self.key)
		{
			slot.handles -= 1;
		}
	}
}

impl<E: Ext> Producer<E> {
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
			path,
		} = config;
		// An empty path after normalization is indistinguishable from "no node
		// set"; collapse it so downstream code only sees a single representation.
		// We do this here (not in `with_node`) so a directly-assigned
		// `config.node` is normalized too.
		let node = node.filter(|p| !p.is_empty());

		let Some(origin) = origin else {
			return Self {
				registry: Registry::disabled(),
				exts: None,
				_keepalive: None,
			};
		};

		let layout = match path {
			Some(path) => Layout::Exact(path),
			None => Layout::Prefix { prefix, node, depth },
		};

		// The excluded path is literal, so its subtree claim cannot fail.
		let exclude = moq_net::Pattern::subtree(layout.exclude().as_str()).expect("the stats path is a literal path");
		let registry = Registry::new(moq_net::stats::Config::new().with_exclude(exclude));
		let exts = Exts::default();
		let keepalive = Arc::new(Keepalive);
		let task = Task {
			registry: registry.clone(),
			exts: exts.clone(),
			origin,
			layout,
			interval,
		};
		spawn(task.run(Arc::downgrade(&keepalive)));

		Self {
			registry,
			exts: Some(exts),
			_keepalive: Some(keepalive),
		}
	}

	/// Attach an extension to `path`'s `role` entry on `tier`, starting from
	/// `E::default()`. See [`Entry`].
	pub fn entry(&self, tier: Tier, role: Role, path: impl AsPath) -> Entry<E> {
		let key = ExtKey {
			path: path.as_path().to_owned(),
			tier,
			role,
		};
		if let Some(exts) = &self.exts {
			let mut table = exts.lock().expect("stats extensions poisoned");
			table
				.slots
				.entry(key.clone())
				.or_insert_with(|| ExtSlot {
					value: E::default(),
					version: 0,
					handles: 0,
				})
				.handles += 1;
		}
		Entry {
			exts: self.exts.clone(),
			key,
		}
	}

	/// The registry this producer drains. Hand sessions tier-scoped handles via
	/// [`Registry::tier`]; read node totals back with [`Registry::snapshot`].
	/// Disabled (all bumps no-op) for a no-op producer.
	pub fn registry(&self) -> &Registry {
		&self.registry
	}
}

/// Where a producer's broadcasts are advertised.
enum Layout {
	/// A relay's `<prefix>[/<group>]/node[/<node>]`, one broadcast per group.
	Prefix {
		prefix: PathOwned,
		node: Option<PathOwned>,
		depth: usize,
	},
	/// One broadcast at exactly this path, from [`Config::at`].
	Exact(PathOwned),
}

impl Layout {
	/// The path whose subtree the registry leaves uncounted, so serving stats
	/// doesn't generate more stats.
	fn exclude(&self) -> &PathOwned {
		match self {
			Self::Prefix { prefix, .. } => prefix,
			Self::Exact(path) => path,
		}
	}

	fn depth(&self) -> usize {
		match self {
			Self::Prefix { depth, .. } => *depth,
			Self::Exact(_) => 0,
		}
	}

	/// The advertised path of `group`'s broadcast.
	fn advertised(&self, group: &Path) -> PathOwned {
		match self {
			Self::Prefix { prefix, node, .. } => advertised_path(prefix, group, node.as_ref().map(Path::as_str)),
			Self::Exact(path) => path.clone(),
		}
	}
}

/// Everything the publish task owns.
struct Task<E: Ext> {
	registry: Registry,
	exts: Exts<E>,
	origin: origin::Producer,
	layout: Layout,
	interval: Duration,
}

impl<E: Ext> Task<E> {
	/// Publishes stats broadcasts and writes a frame per drain. Runs until
	/// every [`Producer`] clone is dropped (`weak.upgrade()` returns `None`).
	async fn run(self, weak: Weak<Keepalive>) {
		let interval = self.interval;
		let Some(mut drain) = Drain::new(self) else {
			return;
		};

		let mut ticker = web_async::time::interval(interval);
		ticker.set_missed_tick_behavior(web_async::time::MissedTickBehavior::Delay);

		loop {
			ticker.tick().await;

			if weak.upgrade().is_none() {
				drain.finish();
				return;
			}

			drain.collect();
			drain.publish();
		}
	}
}

/// One attached extension as a drain saw it.
struct ExtRow<E> {
	key: ExtKey,
	value: E,
	version: u64,
	/// Whether any [`Entry`] still holds it.
	held: bool,
}

/// The publish task's state, kept across drains so a steady-state drain
/// reuses its buffers instead of allocating per entry.
struct Drain<E: Ext> {
	task: Task<E>,
	/// Keyed by the group's path; `""` at depth 0.
	groups: HashMap<String, GroupPublisher<E>>,
	/// Refilled by every drain.
	report: Report,
	/// The attached extensions, refilled by every drain.
	exts: Vec<ExtRow<E>>,
	/// Groups whose broadcast the origin refused this drain, so a refusal is
	/// logged once per drain rather than once per entry.
	refused: Vec<String>,
	/// Drain counter, stamped on the change-detection state an entry touches
	/// so state the report no longer carries can be dropped.
	tick: u64,
}

impl<E: Ext> Drain<E> {
	/// Build the drain state. At depth 0 the single broadcast is announced
	/// up front and lives for the producer's life; `None` if the origin
	/// refused it.
	fn new(task: Task<E>) -> Option<Self> {
		let mut groups = HashMap::new();
		if task.layout.depth() == 0 {
			let group = GroupPublisher::create(&task.origin, &task.layout, &Path::empty())?;
			groups.insert(String::new(), group);
		}
		Some(Self {
			task,
			groups,
			report: Report::default(),
			exts: Vec::new(),
			refused: Vec::new(),
			tick: 0,
		})
	}

	/// Drain the registry and sort each entry into its group's pending frames,
	/// creating group broadcasts and tracks on first sight.
	fn collect(&mut self) {
		self.task.registry.report(&mut self.report);
		self.tick += 1;
		self.refused.clear();

		// Copy the attached extensions out, dropping released ones: this drain
		// reports their final value and the slot state keeps it from here.
		self.exts.clear();
		{
			let mut table = self.task.exts.lock().expect("stats extensions poisoned");
			table.slots.retain(|key, slot| {
				self.exts.push(ExtRow {
					key: key.clone(),
					value: slot.value.clone(),
					version: slot.version,
					held: slot.handles > 0,
				});
				slot.handles > 0
			});
		}

		for group in self.groups.values_mut() {
			group.traffic_rows.clear();
			group.session_rows.clear();
			group.ext_rows.clear();
		}

		let depth = self.task.layout.depth();
		for (i, entry) in self.report.traffic.iter().enumerate() {
			let key = group_key(entry.path.as_str(), depth);
			if let Some(group) = Self::group(&self.task, &mut self.groups, &mut self.refused, key) {
				group.traffic_rows.push(i);
			}
		}
		for (i, entry) in self.report.sessions.iter().enumerate() {
			let key = group_key(entry.root.as_str(), depth);
			if let Some(group) = Self::group(&self.task, &mut self.groups, &mut self.refused, key) {
				group.session_rows.push(i);
			}
		}
		for (i, row) in self.exts.iter().enumerate() {
			let key = group_key(row.key.path.as_str(), depth);
			if let Some(group) = Self::group(&self.task, &mut self.groups, &mut self.refused, key) {
				group.ext_rows.push(i);
			}
		}

		for group in self.groups.values_mut() {
			group.collect(&self.report, &self.exts, self.tick);
		}
	}

	/// Get or create the group publisher for `key`, `None` if the origin
	/// refused its broadcast.
	fn group<'a>(
		task: &Task<E>,
		groups: &'a mut HashMap<String, GroupPublisher<E>>,
		refused: &mut Vec<String>,
		key: &str,
	) -> Option<&'a mut GroupPublisher<E>> {
		if !groups.contains_key(key) {
			if refused.iter().any(|name| name == key) {
				return None;
			}
			match GroupPublisher::create(&task.origin, &task.layout, &Path::new(key)) {
				Some(group) => {
					groups.insert(key.to_string(), group);
				}
				None => {
					refused.push(key.to_string());
					return None;
				}
			}
		}
		groups.get_mut(key)
	}

	/// Write every group's pending frames, serve consumer track requests, and
	/// unpublish groups with nothing left to report.
	fn publish(&mut self) {
		// At depth 0 the single broadcast stays for the producer's life; a
		// group broadcast lives while its group has entries.
		let depth = self.task.layout.depth();
		for (_, group) in self.groups.extract_if(|_, group| {
			depth > 0 && group.traffic_rows.is_empty() && group.session_rows.is_empty() && group.ext_rows.is_empty()
		}) {
			// Deliberate unpublish: finish (tracks included) rather than drop,
			// so there is no dropped-without-finish warning.
			group.finish();
		}

		for group in self.groups.values_mut() {
			group.flush(self.tick);

			// Serve consumer requests for tracks no drain has created yet: a
			// tier's tracks appear lazily on its first traffic, so a subscriber
			// arriving first would otherwise be rejected and forced into a
			// retry loop (fleet-wide, that rejection churn is a log and CPU
			// storm). Held open with zeros instead; see `serve_requests`.
			group.serve_requests();
		}
	}

	fn finish(self) {
		for (_, group) in self.groups {
			group.finish();
		}
	}
}

/// One track's frame, rebuilt every drain in a buffer kept across drains.
/// Serializes as a JSON object keyed by path, byte-identical to
/// [`TrafficFrame`](crate::TrafficFrame) / [`SessionsFrame`](crate::SessionsFrame) once sorted.
struct Frame<V> {
	entries: Vec<(PathOwned, V)>,
}

impl<V> Default for Frame<V> {
	fn default() -> Self {
		Self { entries: Vec::new() }
	}
}

impl<V: Serialize> Serialize for Frame<V> {
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.collect_map(self.entries.iter().map(|(path, value)| (path.as_str(), value)))
	}
}

/// A plain track and its `.z` sibling, kept in lockstep. The plain side runs
/// moq-json with deltas and compression off, which is wire-identical to
/// writing each frame as its own single-frame group; the compressed side uses
/// merge-patch deltas inside a shared DEFLATE window.
struct TrackPair<V> {
	plain: moq_json::snapshot::Producer<Frame<V>>,
	compressed: moq_json::snapshot::Producer<Frame<V>>,
	/// This drain's entries, published and cleared by [`Self::publish`].
	frame: Frame<V>,
}

impl<V: Serialize> TrackPair<V> {
	fn create(broadcast: &broadcast::Producer, name: &str) -> Result<Self, moq_net::Error> {
		let plain_track = broadcast.create_track(name, None)?;
		let compressed_track = broadcast.create_track(format!("{name}{COMPRESSED_SUFFIX}").as_str(), None)?;
		Ok(Self::from_tracks(plain_track, compressed_track))
	}

	/// Build a pair from consumer requests, creating whichever flavor was not
	/// requested. A popped request is no longer queued, so `create_track`'s
	/// queued-request fulfillment cannot reach it; the caller collects both
	/// flavors' popped requests and this serves each through its actual
	/// request where one exists.
	fn adopt(broadcast: &broadcast::Producer, name: &str, pending: PendingPair) -> Result<Self, moq_net::Error> {
		let PendingPair { plain, compressed } = pending;
		let plain_track = match plain {
			Some(request) => request.accept(None),
			None => broadcast.create_track(name, None)?,
		};
		let compressed_track = match compressed {
			Some(request) => request.accept(None),
			None => broadcast.create_track(format!("{name}{COMPRESSED_SUFFIX}").as_str(), None)?,
		};
		Ok(Self::from_tracks(plain_track, compressed_track))
	}

	fn from_tracks(plain_track: track::Producer, compressed_track: track::Producer) -> Self {
		let plain_config = moq_json::snapshot::Config::default().with_delta_ratio(0);
		let mut compressed_config = moq_json::snapshot::Config::default();
		compressed_config.compression = moq_json::Compression::Deflate;

		Self {
			plain: moq_json::snapshot::Producer::new(plain_track, plain_config),
			compressed: moq_json::snapshot::Producer::new(compressed_track, compressed_config),
			frame: Frame::default(),
		}
	}

	/// Whether any consumer exists on either flavor.
	fn is_used(&self) -> bool {
		self.plain.is_used() || self.compressed.is_used()
	}

	/// Publish this drain's entries on both flavors (`{}` when there are none)
	/// and clear them for the next drain; moq-json skips unchanged values.
	fn publish(&mut self, name: &str) {
		self.frame.entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
		if let Err(err) = self.plain.update(&self.frame) {
			tracing::debug!(?err, name, "stats: failed to write frame");
		}
		if let Err(err) = self.compressed.update(&self.frame) {
			tracing::debug!(?err, name, "stats: failed to write compressed frame");
		}
		self.frame.entries.clear();
	}

	/// Finish both flavors, so dropping the pair is a deliberate end instead of
	/// a dropped-without-finish warning. An error means the track already
	/// ended; there is nothing left to close.
	fn finish(&mut self) {
		let _ = self.plain.finish();
		let _ = self.compressed.finish();
	}
}

/// Both flavors' pending requests for one plain track name, collected before
/// serving so each is answered through its own request.
#[derive(Default)]
struct PendingPair {
	plain: Option<track::Request>,
	compressed: Option<track::Request>,
}

impl PendingPair {
	fn reject(self, err: moq_net::Error) {
		if let Some(request) = self.plain {
			request.reject(err.clone());
		}
		if let Some(request) = self.compressed {
			request.reject(err);
		}
	}

	/// Whether any present flavor still has a live requester. Through a relay
	/// origin the serving task holds the request while its info is pending, so
	/// this can read used for a while after the end subscriber left; that only
	/// delays reclamation, it never strands anyone.
	fn is_used(&self, waiter: &kio::Waiter) -> bool {
		self.plain
			.iter()
			.chain(self.compressed.iter())
			.any(|request| request.poll_unused(waiter).is_pending())
	}
}

/// One frame type's live pairs and the requests parked for them; the traffic
/// tracks and the sessions tracks each form one family.
struct TrackFamily<V> {
	tracks: HashMap<String, TrackPair<V>>,
	/// Valid-shaped requests awaiting quota, keyed by plain name and bounded by
	/// [`MAX_PARKED_REQUESTS`] across both families. Adopted as the quota
	/// frees, or dropped once every requester leaves.
	parked: HashMap<String, PendingPair>,
}

impl<V: Serialize> TrackFamily<V> {
	fn new() -> Self {
		Self {
			tracks: HashMap::new(),
			parked: HashMap::new(),
		}
	}

	/// Add one entry to track `name`'s pending frame, creating the pair on the
	/// track's first entry.
	///
	/// A pair created here serves any parked requests for its name: a parked
	/// request was already popped off the broadcast queue, so `create_track`'s
	/// queued-request fulfillment cannot reach it, and creating the pair blind
	/// would strand its requesters on a name that now exists. A requested pair
	/// whose tier records becomes an ordinary tier pair: kept for the
	/// broadcast's life, no longer counting against the requested quota.
	fn push(
		&mut self,
		broadcast: &broadcast::Producer,
		requested: &mut HashSet<String>,
		name: &str,
		path: PathOwned,
		value: V,
	) {
		if !self.tracks.contains_key(name) {
			let result = match self.parked.remove(name) {
				Some(pending) => TrackPair::adopt(broadcast, name, pending),
				None => TrackPair::create(broadcast, name),
			};
			match result {
				Ok(pair) => {
					self.tracks.insert(name.to_string(), pair);
				}
				Err(err) => {
					tracing::warn!(?err, name, "stats: failed to create track");
					return;
				}
			}
		}
		if !requested.is_empty() {
			requested.remove(name);
		}
		let pair = self.tracks.get_mut(name).expect("just ensured");
		pair.frame.entries.push((path, value));
	}

	/// Publish every pair's pending frame, an empty one when the drain had
	/// nothing for it, so a track whose last entry closed transitions to `{}`
	/// exactly once.
	fn flush(&mut self) {
		for (name, pair) in self.tracks.iter_mut() {
			pair.publish(name);
		}
	}

	/// Reclaim requested pairs whose last consumer left before their tier ever
	/// recorded: cached state nobody is watching. The pair is finished (a
	/// deliberate end, not a warning) and dropped, so a returning subscriber
	/// re-requests and is re-adopted; the quota refund means a disconnected
	/// prober can never deny a later drain's legitimate requests.
	fn reclaim(&mut self, requested: &mut HashSet<String>) {
		self.tracks.retain(|name, pair| {
			if !requested.contains(name) || pair.is_used() {
				return true;
			}
			requested.remove(name);
			pair.finish();
			false
		});
	}

	/// Park one popped request, merging the two flavors of a plain name. Only a
	/// NEW name while the parked buffer is `full` is rejected.
	fn park(&mut self, plain: String, compressed: bool, request: track::Request, full: bool) {
		match self.parked.get_mut(&plain) {
			Some(pending) => {
				let slot = match compressed {
					true => &mut pending.compressed,
					false => &mut pending.plain,
				};
				// Keep the first requester for a flavor. A duplicate means
				// the original was already popped off the broadcast queue;
				// dropping the newcomer aborts it into a retry, which joins
				// the live track once the parked pair is adopted.
				if slot.is_none() {
					*slot = Some(request);
				}
			}
			None if full => request.reject(moq_net::Error::NotFound),
			None => {
				let mut pending = PendingPair::default();
				match compressed {
					true => pending.compressed = Some(request),
					false => pending.plain = Some(request),
				}
				self.parked.insert(plain, pending);
			}
		}
	}

	/// Adopt parked requests as the quota allows; the rest stay parked for a
	/// later drain, so a valid-shaped request is never terminally rejected
	/// merely for arriving while the quota was full. Entries whose every
	/// requester left are dropped instead of adopted.
	fn adopt_parked(&mut self, broadcast: &broadcast::Producer, requested: &mut HashSet<String>) {
		let noop = kio::Waiter::noop();
		let mut parked = std::mem::take(&mut self.parked);
		parked.retain(|plain, pending| {
			if !pending.is_used(&noop) {
				return false;
			}
			if requested.len() >= MAX_REQUESTED_TRACKS {
				return true;
			}
			self.adopt_pair(broadcast, requested, plain.clone(), std::mem::take(pending));
			false
		});
		self.parked = parked;
	}

	/// Adopt one plain name's pending requests into a live [`TrackPair`],
	/// publishing a zero frame so the subscription resolves immediately. The
	/// caller owns the quota decision; this only mints the pair.
	fn adopt_pair(
		&mut self,
		broadcast: &broadcast::Producer,
		requested: &mut HashSet<String>,
		plain: String,
		pending: PendingPair,
	) {
		// Defensive only: a request racing the pair's creation is fulfilled by
		// `create_track` (queued) or adopted by [`Self::flush`] (parked), so it
		// never reaches this with the pair already live. Rejecting is still
		// safe there - the requester's retry resolves against the live track.
		if self.tracks.contains_key(&plain) {
			pending.reject(moq_net::Error::NotFound);
			return;
		}
		match TrackPair::adopt(broadcast, &plain, pending) {
			Ok(mut pair) => {
				// Hold the subscription open with zeros until the tier records.
				pair.publish(&plain);
				self.tracks.insert(plain.clone(), pair);
				requested.insert(plain);
			}
			Err(err) => tracing::warn!(?err, name = %plain, "stats: failed to adopt requested track"),
		}
	}

	/// Finish every pair, making teardown a deliberate end.
	fn finish(&mut self) {
		for pair in self.tracks.values_mut() {
			pair.finish();
		}
	}
}

/// One group stats broadcast and its change-detection state.
struct GroupPublisher<E: Ext> {
	broadcast: broadcast::Producer,
	/// Holds the broadcast's request queue open, so a subscriber asking for a
	/// tier track no drain has created yet parks (served next tick) instead of
	/// being rejected `NotFound` on the spot.
	dynamic: broadcast::Dynamic,
	/// Names of consumer-requested pairs whose tier has not recorded yet. Its
	/// size is the [`MAX_REQUESTED_TRACKS`] quota; a name leaves the set by
	/// recording real traffic (now an ordinary tier pair, kept forever) or by
	/// losing its last consumer (reclaimed, quota refunded). A traffic name
	/// still here is served as a per-broadcast track (see [`Self::filter`]).
	requested: HashSet<String>,
	traffic: TrackFamily<Stats<E>>,
	sessions: TrackFamily<Presence>,
	local: HashMap<PathOwned, HashMap<Tier, SideSlots<E>>>,
	session_local: HashMap<Tier, HashMap<PathOwned, SessionSlotState>>,
	/// Track names per tier, built once so a drain never formats a name. Its
	/// keys are also the tier labels a per-broadcast track name is matched
	/// against.
	names: HashMap<Tier, TierNames>,
	/// This drain's entries for the group, as indices into the report.
	traffic_rows: Vec<usize>,
	session_rows: Vec<usize>,
	/// This drain's attached extensions for the group, as indices into the
	/// drain's extension rows.
	ext_rows: Vec<usize>,
}

/// The plain track names one tier's entries land on.
struct TierNames {
	publisher: String,
	subscriber: String,
	sessions: String,
}

impl TierNames {
	fn new(tier: &Tier) -> Self {
		Self {
			publisher: traffic_track(tier, Role::Publisher, false),
			subscriber: traffic_track(tier, Role::Subscriber, false),
			sessions: sessions_track(tier, false),
		}
	}
}

impl<E: Ext> GroupPublisher<E> {
	fn create(origin: &origin::Producer, layout: &Layout, group: &Path) -> Option<Self> {
		let advertised = layout.advertised(group);
		let broadcast = match origin.publish(&advertised, origin::Route::default()) {
			Ok(broadcast) => broadcast,
			Err(err) => {
				tracing::warn!(advertised = %advertised, ?err, "stats: origin rejected stats broadcast");
				return None;
			}
		};
		tracing::debug!(advertised = %advertised, "stats: publishing broadcast");

		let mut traffic = TrackFamily::new();
		let mut sessions = TrackFamily::new();

		// The default tier's tracks always exist, even while idle. A client
		// publishes its sessions track only once it records a session.
		let tier = Tier::default();
		for role in [Role::Publisher, Role::Subscriber] {
			let name = traffic_track(&tier, role, false);
			match TrackPair::create(&broadcast, &name) {
				Ok(pair) => {
					traffic.tracks.insert(name, pair);
				}
				Err(err) => {
					tracing::warn!(?err, name, "stats: failed to create track");
					return None;
				}
			}
		}
		if let Layout::Prefix { .. } = layout {
			let name = sessions_track(&tier, false);
			match TrackPair::create(&broadcast, &name) {
				Ok(pair) => {
					sessions.tracks.insert(name, pair);
				}
				Err(err) => {
					tracing::warn!(?err, name, "stats: failed to create track");
					return None;
				}
			}
		}

		let dynamic = broadcast.dynamic();

		Some(Self {
			broadcast,
			dynamic,
			requested: HashSet::new(),
			traffic,
			sessions,
			local: HashMap::new(),
			session_local: HashMap::new(),
			names: HashMap::new(),
			traffic_rows: Vec::new(),
			session_rows: Vec::new(),
			ext_rows: Vec::new(),
		})
	}

	/// Run this drain's rows through change detection into the pending frames.
	fn collect(&mut self, report: &Report, exts: &[ExtRow<E>], tick: u64) {
		let Self {
			broadcast,
			requested,
			traffic,
			sessions,
			local,
			session_local,
			names,
			traffic_rows,
			session_rows,
			ext_rows,
			..
		} = self;

		// Extensions first, so the traffic rows below carry this drain's value.
		for &i in ext_rows.iter() {
			let row = &exts[i];
			let slots = local
				.entry(row.key.path.clone())
				.or_default()
				.entry(row.key.tier.clone())
				.or_default();
			slots.seen = tick;
			let slot = slots.side(row.key.role);
			slot.ext = row.value.clone();
			slot.version = row.version;
			if row.held {
				slot.held = tick;
			}
		}

		for &i in traffic_rows.iter() {
			let entry = &report.traffic[i];
			let names = names
				.entry(entry.tier.clone())
				.or_insert_with(|| TierNames::new(&entry.tier));
			let slots = local
				.entry(entry.path.clone())
				.or_default()
				.entry(entry.tier.clone())
				.or_default();
			slots.seen = tick;
			slots.reported = tick;
			process_slot(entry.publisher, &mut slots.publisher, tick, |snap| {
				traffic.push(broadcast, requested, &names.publisher, entry.path.clone(), snap);
			});
			process_slot(entry.subscriber, &mut slots.subscriber, tick, |snap| {
				traffic.push(broadcast, requested, &names.subscriber, entry.path.clone(), snap);
			});
		}

		// Extensions on entries the registry did not report this drain: the
		// traffic stays at the last value emitted, which is its final one.
		for &i in ext_rows.iter() {
			let row = &exts[i];
			let slots = local
				.get_mut(&row.key.path)
				.and_then(|tiers| tiers.get_mut(&row.key.tier))
				.expect("inserted above");
			if slots.reported == tick {
				continue;
			}
			let names = names
				.entry(row.key.tier.clone())
				.or_insert_with(|| TierNames::new(&row.key.tier));
			let name = match row.key.role {
				Role::Publisher => &names.publisher,
				Role::Subscriber => &names.subscriber,
			};
			let slot = slots.side(row.key.role);
			let snap = slot.prev_emitted.unwrap_or_default();
			process_slot(snap, slot, tick, |snap| {
				traffic.push(broadcast, requested, name, row.key.path.clone(), snap);
			});
		}

		for &i in session_rows.iter() {
			let entry = &report.sessions[i];
			let names = names
				.entry(entry.tier.clone())
				.or_insert_with(|| TierNames::new(&entry.tier));
			let state = session_local
				.entry(entry.tier.clone())
				.or_default()
				.entry(entry.root.clone())
				.or_default();
			state.seen = tick;
			process_session_slot(entry.presence, state, |snap| {
				sessions.push(broadcast, requested, &names.sessions, entry.root.clone(), snap);
			});
		}
	}

	/// Publish the pending frames, then drop change-detection state for
	/// entries this drain did not carry (the registry pruned them).
	fn flush(&mut self, tick: u64) {
		self.filter();
		self.traffic.flush();
		self.sessions.flush();

		self.local.retain(|_, tiers| {
			tiers.retain(|_, slots| slots.seen == tick);
			!tiers.is_empty()
		});
		self.session_local.retain(|_, roots| {
			roots.retain(|_, state| state.seen == tick);
			!roots.is_empty()
		});
	}

	/// Fill each requested per-broadcast track with its one entry from the
	/// matching tier track's pending frame: the same track, filtered to a path.
	fn filter(&mut self) {
		if self.requested.is_empty() {
			return;
		}

		let mut rows = Vec::new();
		for name in &self.requested {
			let Some((Some(prefix), role)) = split_role(name) else {
				continue;
			};
			let Some((tier, path)) = resolve_tier(&self.names, prefix) else {
				continue;
			};
			let Some(source) = self.traffic.tracks.get(&traffic_track(&tier, role, false)) else {
				continue;
			};
			if let Some((path, value)) = source.frame.entries.iter().find(|(p, _)| p.as_str() == path) {
				rows.push((name.clone(), path.clone(), value.clone()));
			}
		}

		for (name, path, value) in rows {
			if let Some(pair) = self.traffic.tracks.get_mut(&name) {
				pair.frame.entries.push((path, value));
			}
		}
	}

	/// Whether a per-broadcast request for `prefix` is ambiguous: it resolves
	/// to a named tier, but a default-tier broadcast also lives at `prefix`.
	fn ambiguous(&self, prefix: &str) -> bool {
		let Some((tier, _)) = resolve_tier(&self.names, prefix) else {
			return false;
		};
		!tier.is_default()
			&& self
				.local
				.get(&Path::new(prefix).to_owned())
				.is_some_and(|tiers| tiers.contains_key(&Tier::default()))
	}

	/// Serve consumer requests for tracks no drain has created yet.
	///
	/// A tier's tracks are created lazily, on the tier's first recorded byte, so
	/// a subscriber can legitimately ask before they exist (an idle protocol a
	/// collector watches on every node). Rejecting such a request forces every
	/// one of those subscribers into a resubscribe loop; instead any
	/// stats-shaped name is accepted immediately and held open with a zero
	/// frame, and the tier's real data rides the same tracks once it records
	/// ([`TrackFamily::push`] finds the pair already created). Names that do not
	/// match the stats track shape are rejected as before, and valid names over
	/// the quota park (bounded) until it frees rather than being rejected.
	fn serve_requests(&mut self) {
		// Reclaim before parking and adopting, so a freed quota slot is usable
		// by this very drain.
		self.traffic.reclaim(&mut self.requested);
		self.sessions.reclaim(&mut self.requested);

		// Pop everything queued into the parked maps, grouping the two flavors
		// of one plain name so the pair is built from the actual requests where
		// present. Only names past the parked bound are rejected.
		let noop = kio::Waiter::noop();
		while let Poll::Ready(Ok(request)) = self.dynamic.poll_requested_track(&noop) {
			let Some(shape) = requested_track_shape(request.name()) else {
				request.reject(moq_net::Error::NotFound);
				continue;
			};
			if let Some((Some(prefix), _)) = split_role(&shape.plain)
				&& self.ambiguous(prefix)
			{
				request.reject(moq_net::Error::NotFound);
				continue;
			}
			let full = self.traffic.parked.len() + self.sessions.parked.len() >= MAX_PARKED_REQUESTS;
			match shape.sessions {
				true => self.sessions.park(shape.plain, shape.compressed, request, full),
				false => self.traffic.park(shape.plain, shape.compressed, request, full),
			}
		}

		self.traffic.adopt_parked(&self.broadcast, &mut self.requested);
		self.sessions.adopt_parked(&self.broadcast, &mut self.requested);
	}

	/// Deliberately end the broadcast: finish every pair, then the broadcast
	/// itself, so teardown emits no dropped-without-finish warnings.
	fn finish(mut self) {
		self.traffic.finish();
		self.sessions.finish();
		self.broadcast.finish();
	}
}

/// The parsed shape of a consumer-requested stats track name.
struct RequestedShape {
	/// The plain (uncompressed) track name, the pair maps' key.
	plain: String,
	/// Whether the requested flavor was the [`COMPRESSED_SUFFIX`] one.
	compressed: bool,
	/// Sessions track vs traffic track, picking the frame type.
	sessions: bool,
}

/// Classify a consumer-requested track name against the stats track shape
/// `[<prefix>/]{publisher|subscriber|sessions}.json[.z]`, or `None` for a name
/// no drain could ever serve. The prefix is a tier label, or on a traffic track
/// a tier label and broadcast path ([`resolve_tier`]).
fn requested_track_shape(name: &str) -> Option<RequestedShape> {
	let (base, compressed) = match name.strip_suffix(COMPRESSED_SUFFIX) {
		Some(base) => (base, true),
		None => (name, false),
	};
	let (prefix, kind) = match base.rsplit_once('/') {
		Some((prefix, kind)) => (Some(prefix), kind),
		None => (None, base),
	};
	let sessions = match kind {
		"publisher.json" | "subscriber.json" => false,
		"sessions.json" => true,
		_ => return None,
	};
	// The prefix is an arbitrary path; require a clean one so a malformed
	// name can't mint a track a real entry could never fill.
	if let Some(prefix) = prefix
		&& (prefix.is_empty() || prefix.starts_with('/') || prefix.ends_with('/') || prefix.contains("//"))
	{
		return None;
	}
	Some(RequestedShape {
		plain: base.to_string(),
		compressed,
		sessions,
	})
}

/// Split a plain traffic track name into its prefix and role, `None` for a
/// sessions track or a name of another shape.
fn split_role(plain: &str) -> Option<(Option<&str>, Role)> {
	let (prefix, kind) = match plain.rsplit_once('/') {
		Some((prefix, kind)) => (Some(prefix), kind),
		None => (None, plain),
	};
	let role = match kind {
		"publisher.json" => Role::Publisher,
		"subscriber.json" => Role::Subscriber,
		_ => return None,
	};
	Some((prefix, role))
}

/// Resolve a traffic track name's prefix into the tier and broadcast path of a
/// per-broadcast track, matching the known tier labels longest first; a prefix
/// matching none is a default-tier path. `None` when the prefix is itself a
/// known tier label: that name is the tier's whole track.
fn resolve_tier<'a, V>(tiers: &HashMap<Tier, V>, prefix: &'a str) -> Option<(Tier, &'a str)> {
	let mut best: Option<&Tier> = None;
	for tier in tiers.keys().filter(|tier| !tier.is_default()) {
		let label = tier.as_str();
		if prefix == label {
			return None;
		}
		let nested = prefix.strip_prefix(label).is_some_and(|rest| rest.starts_with('/'));
		if nested && best.is_none_or(|best| label.len() > best.as_str().len()) {
			best = Some(tier);
		}
	}
	match best {
		Some(tier) => Some((tier.clone(), &prefix[tier.as_str().len() + 1..])),
		None => Some((Tier::default(), prefix)),
	}
}

/// Change-detection state for one `(path, tier, side)` slot, owned by the
/// publish task. The task is single-threaded so this needs no atomics.
struct SlotState<E> {
	/// Last [`Traffic`] we emitted for this slot, used to detect changes that
	/// warrant re-emission.
	prev_emitted: Option<Traffic>,
	/// The latest attached extension, kept after its [`Entry`] drops.
	ext: E,
	/// The extension's version, and the one last emitted.
	version: u64,
	emitted: u64,
	/// The last drain that saw the extension held.
	held: u64,
}

impl<E: Default> Default for SlotState<E> {
	fn default() -> Self {
		Self {
			prev_emitted: None,
			ext: E::default(),
			version: 0,
			emitted: 0,
			held: 0,
		}
	}
}

/// Change-detection state for one `(path, tier)`: a [`SlotState`] per side.
struct SideSlots<E> {
	publisher: SlotState<E>,
	subscriber: SlotState<E>,
	/// The last drain that carried this `(path, tier)`, from the registry or
	/// an attached extension.
	seen: u64,
	/// The last drain whose registry report carried this `(path, tier)`.
	reported: u64,
}

impl<E: Default> Default for SideSlots<E> {
	fn default() -> Self {
		Self {
			publisher: SlotState::default(),
			subscriber: SlotState::default(),
			seen: 0,
			reported: 0,
		}
	}
}

impl<E> SideSlots<E> {
	fn side(&mut self, role: Role) -> &mut SlotState<E> {
		match role {
			Role::Publisher => &mut self.publisher,
			Role::Subscriber => &mut self.subscriber,
		}
	}
}

/// Change-detection state for one session-track root, mirroring [`SlotState`].
#[derive(Default)]
struct SessionSlotState {
	prev_emitted: Option<Presence>,
	/// The last drain that reported this root.
	seen: u64,
}

/// Per-drain work for a single `(side, tier)` slot: update the slot's
/// emitted state and hand the entry to `emit` iff the slot is live or changed
/// this drain.
fn process_slot<E: Clone>(snap: Traffic, slot: &mut SlotState<E>, tick: u64, emit: impl FnOnce(Stats<E>)) {
	// A slot is live while any started counter still exceeds its `*_ended`
	// counterpart (a guard is held, so a subscription could begin at any
	// moment) or while an extension is attached. Live slots are emitted every
	// drain so a downstream "currently active" view always sees the full set.
	// Once every pair is equal and the extension is released no traffic can
	// flow and the entry is on its way out.
	let live = !snap.is_idle() || slot.held == tick;

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
	let prev_snap = slot.prev_emitted.unwrap_or_default();
	let changed = snap != prev_snap || slot.version != slot.emitted;
	if changed {
		slot.prev_emitted = Some(snap);
		slot.emitted = slot.version;
	}
	if live || changed {
		emit(Stats {
			traffic: snap,
			ext: slot.ext.clone(),
		});
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

/// The leading `depth` segments of `path`, the group it publishes under.
fn group_key(path: &str, depth: usize) -> &str {
	if depth == 0 {
		return "";
	}
	match path.match_indices('/').nth(depth - 1) {
		Some((end, _)) => &path[..end],
		None => path,
	}
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
	/// Build an origin producer, spawning its driver on the ambient runtime.
	fn produce_origin() -> moq_net::origin::Producer {
		let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::default());
		if tokio::runtime::Handle::try_current().is_ok() {
			tokio::spawn(moq_net::time::run(driver));
		} else {
			// A sync test: nothing polls the driver, and dropping it would tear
			// the origin down, so leak it and rely on the synchronous half.
			std::mem::forget(driver);
		}
		producer
	}

	use std::collections::BTreeMap;

	use moq_net::stats::{Registry, Tier};
	use moq_net::{Timestamp, announce, broadcast, track};

	use super::*;

	fn test_producer(node: Option<&str>) -> (Producer, origin::Producer) {
		let origin = produce_origin();
		let producer = Producer::<()>::new(
			Config::new()
				.with_origin(origin.clone())
				.with_node(node.map(|s| PathOwned::from(s.to_string()))),
		);
		(producer, origin)
	}

	/// Kept-alive handles from [`feed`]: dropping them closes the subscription and the
	/// announce (bumping the `_closed` counters).
	#[allow(dead_code)]
	struct Feed {
		announced: announce::Consumer,
		source: broadcast::Producer,
		consumer: broadcast::Consumer,
		sub: Option<track::Subscriber>,
	}

	/// Drive a tagged egress broadcast so `registry` records publisher-side traffic on
	/// `path` under `tier`. The local publisher (ingress) is left untagged, so only the
	/// egress (publisher) counters move, matching how a relay bills read-out traffic.
	///
	/// Announces the broadcast; if `subscribe`, opens one subscription and reads a group
	/// of `frames` frames of `frame_size` bytes each (so `bytes`/`frames`/`groups` move).
	async fn feed(
		registry: &Registry,
		tier: Tier,
		path: &str,
		subscribe: bool,
		frames: usize,
		frame_size: usize,
	) -> Feed {
		let ctx = registry.tier(tier).session("feed");
		let origin = produce_origin();
		// Egress (publisher side) is tagged; the local publisher stays untagged.
		let egress = origin.consume().with_stats(ctx);

		let mut announced = egress.announced();
		let source = origin.create_broadcast(path).expect("create_broadcast");
		source.announce(origin::Route::default()).expect("announce");
		let producer = source.create_track("video", None).expect("create_track");

		let update = announced.next().await.expect("announce");
		assert!(update.kind.is_active());
		let consumer = egress.request_broadcast(path).await.expect("resolve");

		let sub = if subscribe {
			let mut sub = consumer
				.track("video")
				.expect("track")
				.subscribe(None)
				.await
				.expect("subscribe");

			if frames > 0 {
				let mut group = producer.append_group().expect("group");
				for _ in 0..frames {
					group
						.write_frame(Timestamp::ZERO, vec![0u8; frame_size])
						.expect("write");
				}
				group.finish().expect("finish");

				let mut group = sub.recv_group().await.expect("recv").expect("group");
				while group.read_frame().await.expect("read").is_some() {}
			}
			Some(sub)
		} else {
			None
		};

		Feed {
			announced,
			source,
			consumer,
			sub,
		}
	}

	/// Awaits the stats announce and returns its broadcast.
	async fn announced(origin: &origin::Producer) -> (String, moq_net::broadcast::Consumer) {
		let mut consumer = origin.consume().announced();
		tokio::time::advance(Duration::from_millis(1)).await;
		let update = consumer.next().await.expect("expected announce");
		assert!(update.kind.is_active());
		let broadcast = origin
			.consume()
			.request_broadcast(moq_net::Path::new(update.prefix.as_str()))
			.await
			.expect("resolve");
		(update.prefix.as_str().to_string(), broadcast)
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
		let frame = next_frame(&mut track).await;
		serde_json::from_slice(&frame.payload).expect("json parse")
	}

	/// Read the latest buffered traffic frame off a track. The producer emits an
	/// immediate first (often empty) frame at time zero, so a test that records
	/// traffic asynchronously reads the accumulated state rather than that stale one.
	async fn read_last_frame(broadcast: &moq_net::broadcast::Consumer, name: &str) -> BTreeMap<String, Traffic> {
		let mut track = subscribe(broadcast, name).await;
		let mut last = next_frame(&mut track).await;
		while let Some(frame) = try_next_frame(&mut track) {
			last = frame;
		}
		serde_json::from_slice(&last.payload).expect("json parse")
	}

	async fn read_session_frame(broadcast: &moq_net::broadcast::Consumer, name: &str) -> BTreeMap<String, Presence> {
		let mut track = subscribe(broadcast, name).await;
		let frame = next_frame(&mut track).await;
		serde_json::from_slice(&frame.payload).expect("json parse")
	}

	async fn subscribe(broadcast: &moq_net::broadcast::Consumer, name: &str) -> track::Ordered {
		broadcast
			.track(name)
			.expect("track")
			.subscribe(None)
			.await
			.expect("subscribe")
			.ordered()
	}

	/// The next group's first frame. Stats tracks are one frame per group, so this is
	/// one published sample.
	async fn next_frame(track: &mut track::Ordered) -> moq_net::frame::Frame {
		let mut group = track.next_group().await.expect("ok").expect("group");
		group.read_frame().await.expect("ok").expect("frame")
	}

	/// The same, without blocking: `None` once nothing more is buffered.
	fn try_next_frame(track: &mut track::Ordered) -> Option<moq_net::frame::Frame> {
		use futures::FutureExt;
		let mut group = track.next_group().now_or_never()?.expect("ok")?;
		group.read_frame().now_or_never()?.expect("ok")
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

		let _f1 = feed(producer.registry(), Tier::default(), "foo/bar", true, 1, 8).await;
		let _f2 = feed(producer.registry(), Tier::default(), "baz/qux", true, 1, 8).await;

		assert_eq!(announced(&origin).await.0, ".stats/node/sjc/1");
	}

	#[tokio::test(start_paused = true)]
	async fn task_announces_without_node_suffix() {
		let (producer, origin) = test_producer(None);
		let _f = feed(producer.registry(), Tier::default(), "foo/bar", true, 1, 8).await;
		assert_eq!(announced(&origin).await.0, ".stats/node");
	}

	#[tokio::test(start_paused = true)]
	async fn frame_emits_expected_counters() {
		let (producer, origin) = test_producer(Some("sjc"));
		// One announced broadcast, one subscription, one 42-byte frame read out.
		let _f = feed(producer.registry(), Tier::default(), "foo/bar", true, 1, 42).await;

		drive_tick().await;

		let (_, broadcast) = announced(&origin).await;
		let frame = read_last_frame(&broadcast, "publisher.json").await;
		let snap = frame.get("foo/bar").expect("foo/bar entry");
		assert_eq!(
			snap.announces_started, 1,
			"egress announce stream bumps announces_started"
		);
		assert_eq!(snap.broadcasts_started, 1, "one session subscribed");
		assert_eq!(snap.subscriptions_started, 1);
		assert_eq!(snap.bytes, 42);
		assert_eq!(snap.frames, 1);
	}

	#[tokio::test(start_paused = true)]
	async fn announced_bytes_surfaces_in_frame() {
		let (producer, origin) = test_producer(Some("sjc"));
		// Announce only: the guard records the broadcast-name length once on open.
		let _f = feed(producer.registry(), Tier::default(), "foo/bar", false, 0, 0).await;

		drive_tick().await;

		let (_, broadcast) = announced(&origin).await;
		let frame = read_last_frame(&broadcast, "publisher.json").await;
		let snap = frame.get("foo/bar").expect("foo/bar entry");
		assert_eq!(snap.announces_started, 1);
		assert_eq!(
			snap.announced_bytes,
			"foo/bar".len() as u64,
			"name length recorded on announce"
		);
	}

	#[tokio::test(start_paused = true)]
	async fn announced_decouples_from_broadcasts() {
		// An announce with no subscription should bump announces_started but NOT broadcasts_started
		// (which only counts sessions with an active sub).
		let (producer, origin) = test_producer(Some("sjc"));
		let _f = feed(producer.registry(), Tier::default(), "foo/bar", false, 0, 0).await;

		drive_tick().await;

		let (_, broadcast) = announced(&origin).await;
		let frame = read_last_frame(&broadcast, "publisher.json").await;
		let snap = frame.get("foo/bar").expect("foo/bar entry");
		assert_eq!(snap.announces_started, 1);
		assert_eq!(snap.broadcasts_started, 0, "no subscription, no broadcasts sentinel");
		assert_eq!(snap.subscriptions_started, 0);
	}

	#[tokio::test(start_paused = true)]
	async fn short_lived_sub_is_surfaced() {
		// A subscription that opens AND closes within a single drain window
		// must still surface as a complete broadcasts start/end cycle. The
		// cumulative counters retain broadcasts_started=1/broadcasts_ended=1, and the
		// change-driven inclusion surfaces the entry even though it's net-idle
		// by drain time.
		let (producer, origin) = test_producer(Some("sjc"));
		{
			// Subscribe, read one 123-byte frame, then drop everything within the
			// first interval so the open and close both land before the drain.
			let _f = feed(producer.registry(), Tier::default(), "foo/bar", true, 1, 123).await;
		}

		drive_tick().await;

		let (_, broadcast) = announced(&origin).await;
		let frame = read_last_frame(&broadcast, "publisher.json").await;
		let snap = frame.get("foo/bar").expect("foo/bar entry");
		// One session opened then closed a subscription within the drain.
		assert_eq!(snap.subscriptions_started, 1);
		assert_eq!(snap.subscriptions_ended, 1);
		assert_eq!(snap.broadcasts_started, 1, "one session subscribed");
		assert_eq!(snap.broadcasts_ended, 1);
		assert_eq!(snap.bytes, 123);
		assert_eq!(snap.frames, 1);
	}

	#[tokio::test(start_paused = true)]
	async fn session_track_surfaces_by_root() {
		let (producer, origin) = test_producer(Some("sjc"));
		let _a = producer.registry().tier(Tier::default()).session("acme");
		let _b = producer.registry().tier(Tier::default()).session("acme");
		let _c = producer.registry().tier(Tier::new("region/sjc")).session("peer");

		drive_tick().await;

		let (_, broadcast) = announced(&origin).await;
		let frame = read_session_frame(&broadcast, "sessions.json").await;
		let snap = frame.get("acme").expect("root entry");
		assert_eq!(snap.sessions_started, 2);
		assert_eq!(snap.sessions_ended, 0);
		assert!(
			!frame.contains_key("peer"),
			"regional session must not appear on the default track"
		);

		let snap = *read_session_frame(&broadcast, "region/sjc/sessions.json")
			.await
			.get("peer")
			.expect("regional entry");
		assert_eq!(snap.sessions_started, 1);
	}

	#[tokio::test(start_paused = true)]
	async fn unused_slots_dont_surface() {
		// A broadcast that only sees default-tier publisher traffic must NOT
		// surface on its sibling default-tier subscriber track, and a tier
		// with no traffic gets no tracks at all.
		let (producer, origin) = test_producer(Some("sjc"));
		// Only the egress (publisher) side is tagged, so `foo/bar` gets publisher
		// traffic and no subscriber traffic.
		let _f = feed(producer.registry(), Tier::default(), "foo/bar", true, 1, 8).await;

		drive_tick().await;
		drive_tick().await;

		let (_, broadcast) = announced(&origin).await;

		// Default-tier publisher slot SHOULD include foo/bar.
		assert!(
			read_last_frame(&broadcast, "publisher.json")
				.await
				.contains_key("foo/bar"),
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

		// The regional tier never saw traffic, so no drain created its tracks;
		// a subscribe is held open and served zeros instead of being rejected
		// (see `serve_requests`), and its slot still never surfaces in the
		// frames above.
		let subscribing = broadcast
			.track("region/sjc/publisher.json")
			.expect("logical track")
			.subscribe(None);
		drive_tick().await;
		let mut sub = subscribing.await.expect("an idle tier's track is held open").ordered();
		let frame = next_frame(&mut sub).await;
		let parsed: BTreeMap<String, Traffic> = serde_json::from_slice(&frame.payload).expect("json");
		assert!(parsed.is_empty(), "an idle tier serves zeros, got {parsed:?}");
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
		assert_eq!(group_key("acme/room/cam", 0), "");
		assert_eq!(group_key("acme/room/cam", 1), "acme");
		assert_eq!(group_key("acme/room/cam", 2), "acme/room");
		assert_eq!(group_key("acme/room", 3), "acme/room");
	}

	#[test]
	fn requested_track_shape_classifies() {
		let shape = requested_track_shape("rtmp/publisher.json").expect("valid");
		assert_eq!(shape.plain, "rtmp/publisher.json");
		assert!(!shape.compressed);
		assert!(!shape.sessions);

		let shape = requested_track_shape("region/sjc/subscriber.json.z").expect("valid");
		assert_eq!(shape.plain, "region/sjc/subscriber.json");
		assert!(shape.compressed);
		assert!(!shape.sessions);

		let shape = requested_track_shape("sessions.json").expect("default tier");
		assert_eq!(shape.plain, "sessions.json");
		assert!(shape.sessions);

		assert!(requested_track_shape("bogus.json").is_none());
		assert!(requested_track_shape("xpublisher.json").is_none());
		assert!(requested_track_shape("/publisher.json").is_none());
		assert!(requested_track_shape("rtmp//publisher.json").is_none());
		assert!(requested_track_shape("rtmp/publisher.json.z.z").is_none());
	}

	/// A subscribe for a tier that has never recorded resolves with a zero
	/// frame instead of being rejected, and the tier's real data later rides
	/// the SAME subscription (the retry storm this held open replaces).
	#[tokio::test(start_paused = true)]
	async fn idle_tier_track_resolves_with_zeros() {
		let (producer, origin) = test_producer(Some("sjc"));
		// Some default-tier traffic so the group broadcast exists at all.
		let _f = feed(producer.registry(), Tier::default(), "foo/bar", true, 1, 42).await;
		drive_tick().await;
		let (_, broadcast) = announced(&origin).await;

		// Nothing has recorded on the rtmp tier: the track does not exist yet.
		let subscribing = broadcast.track("rtmp/publisher.json").expect("track").subscribe(None);
		drive_tick().await;
		let mut sub = subscribing.await.expect("held open, not rejected").ordered();
		let frame = next_frame(&mut sub).await;
		let parsed: BTreeMap<String, Traffic> = serde_json::from_slice(&frame.payload).expect("json");
		assert!(parsed.is_empty(), "an idle tier serves zeros");

		// The tier records: the same subscription carries the data.
		let _rtmp = feed(producer.registry(), Tier::new("rtmp"), "foo/live", true, 1, 7).await;
		drive_tick().await;
		let frame = next_frame(&mut sub).await;
		let parsed: BTreeMap<String, Traffic> = serde_json::from_slice(&frame.payload).expect("json");
		assert_eq!(parsed.get("foo/live").expect("entry").bytes, 7);
	}

	/// The compressed flavor is adoptable too, and adopting either flavor
	/// creates its sibling, so the pair stays in lockstep.
	#[tokio::test(start_paused = true)]
	async fn compressed_tier_request_creates_the_pair() {
		let (producer, origin) = test_producer(Some("sjc"));
		let _f = feed(producer.registry(), Tier::default(), "foo/bar", true, 1, 42).await;
		drive_tick().await;
		let (_, broadcast) = announced(&origin).await;

		let subscribing = broadcast.track("srt/subscriber.json.z").expect("track").subscribe(None);
		drive_tick().await;
		subscribing.await.expect("compressed flavor held open");

		// The plain sibling was created alongside, so it resolves immediately.
		subscribe(&broadcast, "srt/subscriber.json").await;
	}

	/// A sessions-shaped request is held open with zeros like the traffic ones.
	#[tokio::test(start_paused = true)]
	async fn idle_tier_sessions_track_resolves_with_zeros() {
		let (producer, origin) = test_producer(Some("sjc"));
		let _f = feed(producer.registry(), Tier::default(), "foo/bar", true, 1, 42).await;
		drive_tick().await;
		let (_, broadcast) = announced(&origin).await;

		let subscribing = broadcast.track("webrtc/sessions.json").expect("track").subscribe(None);
		drive_tick().await;
		let mut sub = subscribing.await.expect("held open, not rejected").ordered();
		let frame = next_frame(&mut sub).await;
		let parsed: BTreeMap<String, Presence> = serde_json::from_slice(&frame.payload).expect("json");
		assert!(parsed.is_empty());
	}

	/// A name no tier could produce is still rejected.
	#[tokio::test(start_paused = true)]
	async fn malformed_track_name_rejected() {
		let (producer, origin) = test_producer(Some("sjc"));
		let _f = feed(producer.registry(), Tier::default(), "foo/bar", true, 1, 42).await;
		drive_tick().await;
		let (_, broadcast) = announced(&origin).await;

		let subscribing = broadcast.track("bogus.json").expect("track").subscribe(None);
		drive_tick().await;
		assert!(subscribing.await.is_err(), "a non-stats name is rejected");
	}

	/// A request queued just before its tier's first traffic must not be
	/// stranded: the tick's own `create_track` fulfills the queued request, so
	/// the subscriber and the traffic-created pair are one track and the first
	/// real frame reaches the waiting subscription.
	#[tokio::test(start_paused = true)]
	async fn request_racing_first_traffic_is_fulfilled() {
		let (producer, origin) = test_producer(Some("sjc"));
		let _f = feed(producer.registry(), Tier::default(), "foo/bar", true, 1, 42).await;
		drive_tick().await;
		let (_, broadcast) = announced(&origin).await;

		// Queue the request and drive it far enough to reach the stats
		// broadcast's request queue (the serve chain runs on yields)...
		let subscribing = broadcast.track("rtmp/publisher.json").expect("track").subscribe(None);
		assert!(subscribing.poll_ok(&moq_net::kio::Waiter::noop()).is_pending());
		for _ in 0..8 {
			tokio::task::yield_now().await;
		}

		// ...then the tier records its first traffic before the next tick.
		let _rtmp = feed(producer.registry(), Tier::new("rtmp"), "foo/live", true, 1, 7).await;
		drive_tick().await;

		// The queued subscription resolves and carries the tier's first data.
		let mut sub = subscribing
			.await
			.expect("fulfilled by the tick's own creation")
			.ordered();
		let frame = next_frame(&mut sub).await;
		let parsed: BTreeMap<String, Traffic> = serde_json::from_slice(&frame.payload).expect("json");
		assert_eq!(parsed.get("foo/live").expect("entry").bytes, 7);
	}

	/// The requested-pair quota binds only while its subscriptions are held,
	/// and never terminally rejects a valid collector: an over-quota request
	/// parks until the quota frees (here, a prober disconnecting), then the
	/// SAME subscription resolves.
	#[tokio::test(start_paused = true)]
	async fn requested_quota_recovers_after_disconnect() {
		let (producer, origin) = test_producer(Some("sjc"));
		let _f = feed(producer.registry(), Tier::default(), "foo/bar", true, 1, 42).await;
		drive_tick().await;
		let (_, broadcast) = announced(&origin).await;

		// Fill the whole quota and HOLD it.
		let mut held = Vec::new();
		for i in 0..MAX_REQUESTED_TRACKS {
			let name = format!("junk{i}/publisher.json");
			let subscribing = broadcast.track(&name).expect("track").subscribe(None);
			drive_tick().await;
			held.push(subscribing.await.expect("within the cap"));
		}

		// While held, the next request parks: pending, not rejected.
		let subscribing = broadcast.track("real/publisher.json").expect("track").subscribe(None);
		assert!(subscribing.poll_ok(&moq_net::kio::Waiter::noop()).is_pending());
		drive_tick().await;
		assert!(
			subscribing.poll_ok(&moq_net::kio::Waiter::noop()).is_pending(),
			"an over-quota request parks instead of being rejected"
		);

		// Disconnecting frees the quota: once the origin releases its idle
		// copies (the track linger) the next drains reclaim the junk pairs and
		// adopt the parked request, resolving the SAME subscription. Yield
		// first so the serve tasks observe the demand edge and ARM the linger,
		// then advance past it, then let a few drains observe the releases.
		drop(held);
		for _ in 0..4 {
			tokio::task::yield_now().await;
		}
		tokio::time::advance(Duration::from_secs(31)).await;
		for _ in 0..3 {
			drive_tick().await;
		}
		subscribing
			.await
			.expect("the parked request is adopted once the quota frees");
	}

	/// A parked request whose tier records while parked is adopted by the
	/// flush itself (quota-exempt, it is traffic-backed now), so the waiting
	/// subscription resolves with the tier's first data instead of being
	/// stranded on a name that meanwhile exists.
	#[tokio::test(start_paused = true)]
	async fn parked_request_is_adopted_by_first_traffic() {
		let (producer, origin) = test_producer(Some("sjc"));
		let _f = feed(producer.registry(), Tier::default(), "foo/bar", true, 1, 42).await;
		drive_tick().await;
		let (_, broadcast) = announced(&origin).await;

		// Fill the whole quota and HOLD it, so the next request parks.
		let mut held = Vec::new();
		for i in 0..MAX_REQUESTED_TRACKS {
			let name = format!("junk{i}/publisher.json");
			let subscribing = broadcast.track(&name).expect("track").subscribe(None);
			drive_tick().await;
			held.push(subscribing.await.expect("within the cap"));
		}
		let subscribing = broadcast.track("rt/publisher.json").expect("track").subscribe(None);
		assert!(subscribing.poll_ok(&moq_net::kio::Waiter::noop()).is_pending());
		drive_tick().await;
		assert!(
			subscribing.poll_ok(&moq_net::kio::Waiter::noop()).is_pending(),
			"parked"
		);

		// The tier records while the request is parked: the flush adopts it.
		let _rt = feed(producer.registry(), Tier::new("rt"), "foo/live", true, 1, 9).await;
		drive_tick().await;
		let mut sub = subscribing.await.expect("adopted by the flush").ordered();
		let frame = next_frame(&mut sub).await;
		let parsed: BTreeMap<String, Traffic> = serde_json::from_slice(&frame.payload).expect("json");
		assert_eq!(parsed.get("foo/live").expect("entry").bytes, 9);
	}

	#[test]
	fn frame_serializes_like_a_btreemap() {
		// The producer's reused frame must stay byte-identical to a plain map
		// of `Traffic`, the relay's wire shape before the extension existed,
		// including key order.
		let traffic = |bytes| {
			let mut traffic = Traffic::default();
			traffic.bytes = bytes;
			traffic
		};
		let mut frame = Frame::default();
		let mut map = BTreeMap::new();
		for (path, bytes) in [("room/b", 2), ("room/a", 1), ("other", 3), ("room/a/cam", 4)] {
			frame.entries.push((
				PathOwned::from(path),
				Stats {
					traffic: traffic(bytes),
					ext: (),
				},
			));
			map.insert(path.to_string(), traffic(bytes));
		}
		frame.entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
		assert_eq!(serde_json::to_vec(&frame).unwrap(), serde_json::to_vec(&map).unwrap());
		assert_eq!(serde_json::to_vec(&Frame::<Stats>::default()).unwrap(), b"{}");
	}

	/// A test extension: one counter and one gauge.
	#[derive(Debug, Default, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
	#[serde(default)]
	struct Media {
		decoded: u64,
		#[serde(skip_serializing_if = "Option::is_none")]
		latency: Option<u64>,
	}

	impl crate::Merge for Media {
		fn merge(&mut self, other: &Self) {
			self.decoded += other.decoded;
		}
	}

	impl Ext for Media {}

	fn media_producer(origin: &origin::Producer, path: &str) -> Producer<Media> {
		Producer::new(Config::at(path).expect("stats path").with_origin(origin.clone()))
	}

	#[test]
	fn at_refuses_a_path_without_the_stats_suffix() {
		assert!(Config::at("room/alice.stats").is_ok());
		assert!(Config::at("alice.stats").is_ok());
		assert!(matches!(
			Config::at("room/alice"),
			Err(crate::Error::NotStats(path)) if path.as_str() == "room/alice"
		));
		assert!(
			Config::at("room.stats/alice").is_err(),
			"the last segment must carry it"
		);
	}

	/// The exact-path mode advertises the path itself, with no `node` segment,
	/// so the broadcast name still ends in `.stats`.
	#[tokio::test(start_paused = true)]
	async fn at_advertises_the_exact_path() {
		let origin = produce_origin();
		let _producer = media_producer(&origin, "room/alice.stats");
		let (path, _) = announced(&origin).await;
		assert_eq!(path, "room/alice.stats");
		assert!(crate::is_stats(&path));
	}

	/// An attached extension rides beside the entry's traffic, is reported
	/// while held even with no traffic, and its last value is reported once
	/// more after the handle drops.
	#[tokio::test(start_paused = true)]
	async fn entry_carries_the_extension() {
		let origin = produce_origin();
		let producer = media_producer(&origin, "alice.stats");
		let entry = producer.entry(Tier::default(), Role::Subscriber, "room/bob");
		let (_, broadcast) = announced(&origin).await;
		let mut track = subscribe(&broadcast, "subscriber.json").await;

		entry.set(Media {
			decoded: 30,
			latency: Some(250),
		});
		drive_tick().await;
		let frame: crate::TrafficFrame<Media> = serde_json::from_slice(&last_frame(&mut track).await.payload).unwrap();
		let stats = frame.get("room/bob").expect("held entry is reported");
		assert_eq!(stats.ext.decoded, 30);
		assert_eq!(stats.ext.latency, Some(250));

		// Held and unchanged: still reported every drain.
		drive_tick().await;
		drive_tick().await;
		assert!(
			try_next_frame(&mut track).is_none(),
			"an unchanged frame is not re-sent"
		);

		entry.set(Media {
			decoded: 45,
			latency: None,
		});
		drop(entry);
		drive_tick().await;
		let frame: crate::TrafficFrame<Media> = serde_json::from_slice(&last_frame(&mut track).await.payload).unwrap();
		assert_eq!(frame.get("room/bob").expect("final value").ext.decoded, 45);

		drive_tick().await;
		let frame: crate::TrafficFrame<Media> = serde_json::from_slice(&last_frame(&mut track).await.payload).unwrap();
		assert!(frame.is_empty(), "a released entry with no traffic leaves: {frame:?}");
	}

	/// The per-broadcast track is the tier track filtered to one path.
	#[tokio::test(start_paused = true)]
	async fn per_broadcast_track_filters_one_entry() {
		let (producer, origin) = test_producer(Some("sjc"));
		let _a = feed(producer.registry(), Tier::default(), "room/a", true, 1, 11).await;
		let _b = feed(producer.registry(), Tier::default(), "room/b", true, 1, 22).await;
		drive_tick().await;
		let (_, broadcast) = announced(&origin).await;

		let subscribing = broadcast.track("room/a/publisher.json").expect("track").subscribe(None);
		drive_tick().await;
		let mut sub = subscribing.await.expect("served on request").ordered();
		// Adoption publishes zeros; the next drain fills the entry.
		drive_tick().await;
		let frame = last_frame(&mut sub).await;
		let parsed: BTreeMap<String, Traffic> = serde_json::from_slice(&frame.payload).expect("json");
		assert_eq!(parsed.len(), 1, "only the requested path: {parsed:?}");
		assert_eq!(parsed.get("room/a").expect("entry").bytes, 11);
	}

	/// On a tiered producer the per-broadcast name is `<tier>/<path>/...`,
	/// matched against the known tier labels longest first.
	#[tokio::test(start_paused = true)]
	async fn per_broadcast_track_on_a_named_tier() {
		let (producer, origin) = test_producer(Some("sjc"));
		let _r = feed(producer.registry(), Tier::new("region"), "room/a", true, 1, 5).await;
		let _s = feed(producer.registry(), Tier::new("region/sjc"), "room/a", true, 1, 7).await;
		drive_tick().await;
		let (_, broadcast) = announced(&origin).await;

		for (name, bytes) in [
			("region/room/a/publisher.json", 5),
			("region/sjc/room/a/publisher.json", 7),
		] {
			let subscribing = broadcast.track(name).expect("track").subscribe(None);
			drive_tick().await;
			let mut sub = subscribing.await.expect("served on request").ordered();
			// Adoption publishes zeros; the next drain fills the entry.
			drive_tick().await;
			let frame = last_frame(&mut sub).await;
			let parsed: BTreeMap<String, Traffic> = serde_json::from_slice(&frame.payload).expect("json");
			assert_eq!(parsed.get("room/a").expect(name).bytes, bytes, "{name}");
		}
	}

	/// A default-tier broadcast whose path starts with a tier label cannot be
	/// named unambiguously, so its per-broadcast request is refused.
	#[tokio::test(start_paused = true)]
	async fn ambiguous_per_broadcast_track_is_refused() {
		let (producer, origin) = test_producer(Some("sjc"));
		let _t = feed(producer.registry(), Tier::new("rtmp"), "cam", true, 1, 5).await;
		let _d = feed(producer.registry(), Tier::default(), "rtmp/cam", true, 1, 7).await;
		drive_tick().await;
		let (_, broadcast) = announced(&origin).await;

		let subscribing = broadcast
			.track("rtmp/cam/publisher.json")
			.expect("track")
			.subscribe(None);
		drive_tick().await;
		assert!(
			subscribing.await.is_err(),
			"ambiguous between tier rtmp and path rtmp/cam"
		);
	}

	/// The newest buffered frame on a track, waiting for the first.
	async fn last_frame(track: &mut track::Ordered) -> moq_net::frame::Frame {
		let mut last = next_frame(track).await;
		while let Some(frame) = try_next_frame(track) {
			last = frame;
		}
		last
	}

	/// Counts this thread's allocations, so the test below measures only its
	/// own drain while other tests run in parallel.
	mod counting {
		use std::alloc::{GlobalAlloc, Layout, System};
		use std::cell::Cell;

		thread_local! {
			static ALLOCS: Cell<usize> = const { Cell::new(0) };
		}

		struct Counting;

		unsafe impl GlobalAlloc for Counting {
			unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
				let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
				unsafe { System.alloc(layout) }
			}

			unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
				unsafe { System.dealloc(ptr, layout) }
			}

			unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
				let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
				unsafe { System.realloc(ptr, layout, new_size) }
			}
		}

		#[global_allocator]
		static GLOBAL: Counting = Counting;

		pub fn allocs() -> usize {
			ALLOCS.with(Cell::get)
		}
	}

	#[tokio::test(start_paused = true)]
	async fn steady_drain_collects_without_allocating() {
		// Once every group, track, and buffer exists, draining the registry
		// into the pending frames allocates nothing, however many broadcasts
		// and tiers there are. Encoding the frames (moq-json) is out of scope.
		for depth in [0, 1] {
			for (broadcasts, tiers) in [(1, 1), (16, 1), (1, 4), (16, 4)] {
				let registry = Registry::new(moq_net::stats::Config::new());
				let mut feeds = Vec::new();
				for t in 0..tiers {
					let tier = match t {
						0 => Tier::default(),
						t => Tier::new(format!("tier{t}")),
					};
					for b in 0..broadcasts {
						feeds.push(feed(&registry, tier.clone(), &format!("room{b}/cam"), true, 1, 8).await);
					}
				}

				let mut drain = Drain::new(Task::<()> {
					registry,
					exts: Exts::default(),
					origin: produce_origin(),
					layout: Layout::Prefix {
						prefix: PathOwned::from(".stats"),
						node: None,
						depth,
					},
					interval: Duration::from_secs(1),
				})
				.expect("drain");

				// Warm up: create the groups and tracks and grow every buffer.
				for _ in 0..3 {
					drain.collect();
					drain.publish();
				}

				let before = counting::allocs();
				drain.collect();
				let allocs = counting::allocs() - before;
				drain.publish();

				let pending: usize = drain
					.groups
					.values()
					.map(|group| group.traffic_rows.len() + group.session_rows.len())
					.sum();
				assert!(pending > 0, "the drain carried entries");
				assert_eq!(allocs, 0, "depth {depth}, {broadcasts} broadcasts x {tiers} tiers");
			}
		}
	}
}
