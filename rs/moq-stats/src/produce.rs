//! The publishing half: drain a [`Registry`] on an interval into stats tracks.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};
use std::time::Duration;

use std::task::Poll;

use moq_net::stats::{Presence, Registry, Report, Role, Tier, Traffic};
use moq_net::{Path, PathOwned, broadcast, kio, origin, track};
use serde::Serialize;
use web_async::spawn;

use crate::{COMPRESSED_SUFFIX, sessions_track, traffic_track};

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

		// The prefix is a literal path, so its subtree claim cannot fail.
		let exclude = moq_net::Pattern::subtree(prefix.as_str()).expect("the stats prefix is a literal path");
		let registry = Registry::new(moq_net::stats::Config::new().with_exclude(exclude));
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
	fn node(&self) -> Option<&str> {
		self.node.as_ref().map(moq_net::Path::as_str)
	}
}

/// The publish task's state, kept across drains so a steady-state drain
/// reuses its buffers instead of allocating per entry.
struct Drain {
	task: Task,
	/// Keyed by the group's path; `""` at depth 0.
	groups: HashMap<String, GroupPublisher>,
	/// Refilled by every drain.
	report: Report,
	/// Groups whose broadcast the origin refused this drain, so a refusal is
	/// logged once per drain rather than once per entry.
	refused: Vec<String>,
	/// Drain counter, stamped on the change-detection state an entry touches
	/// so state the report no longer carries can be dropped.
	tick: u64,
}

impl Drain {
	/// Build the drain state. At depth 0 the single broadcast is announced
	/// up front and lives for the producer's life; `None` if the origin
	/// refused it.
	fn new(task: Task) -> Option<Self> {
		let mut groups = HashMap::new();
		if task.depth == 0 {
			let group = GroupPublisher::create(&task.origin, &task.prefix, &Path::empty(), task.node())?;
			groups.insert(String::new(), group);
		}
		Some(Self {
			task,
			groups,
			report: Report::default(),
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

		for group in self.groups.values_mut() {
			group.traffic_rows.clear();
			group.session_rows.clear();
		}

		for (i, entry) in self.report.traffic.iter().enumerate() {
			let key = group_key(entry.path.as_str(), self.task.depth);
			if let Some(group) = Self::group(&self.task, &mut self.groups, &mut self.refused, key) {
				group.traffic_rows.push(i);
			}
		}
		for (i, entry) in self.report.sessions.iter().enumerate() {
			let key = group_key(entry.root.as_str(), self.task.depth);
			if let Some(group) = Self::group(&self.task, &mut self.groups, &mut self.refused, key) {
				group.session_rows.push(i);
			}
		}

		for group in self.groups.values_mut() {
			group.collect(&self.report, self.tick);
		}
	}

	/// Get or create the group publisher for `key`, `None` if the origin
	/// refused its broadcast.
	fn group<'a>(
		task: &Task,
		groups: &'a mut HashMap<String, GroupPublisher>,
		refused: &mut Vec<String>,
		key: &str,
	) -> Option<&'a mut GroupPublisher> {
		if !groups.contains_key(key) {
			if refused.iter().any(|name| name == key) {
				return None;
			}
			match GroupPublisher::create(&task.origin, &task.prefix, &Path::new(key), task.node()) {
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
		let depth = self.task.depth;
		for (_, group) in self
			.groups
			.extract_if(|_, group| depth > 0 && group.traffic_rows.is_empty() && group.session_rows.is_empty())
		{
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
struct GroupPublisher {
	broadcast: broadcast::Producer,
	/// Holds the broadcast's request queue open, so a subscriber asking for a
	/// tier track no drain has created yet parks (served next tick) instead of
	/// being rejected `NotFound` on the spot.
	dynamic: broadcast::Dynamic,
	/// Names of consumer-requested pairs whose tier has not recorded yet. Its
	/// size is the [`MAX_REQUESTED_TRACKS`] quota; a name leaves the set by
	/// recording real traffic (now an ordinary tier pair, kept forever) or by
	/// losing its last consumer (reclaimed, quota refunded).
	requested: HashSet<String>,
	traffic: TrackFamily<Traffic>,
	sessions: TrackFamily<Presence>,
	local: HashMap<PathOwned, HashMap<Tier, SideSlots>>,
	session_local: HashMap<Tier, HashMap<PathOwned, SessionSlotState>>,
	/// Track names per tier, built once so a drain never formats a name.
	names: HashMap<Tier, TierNames>,
	/// This drain's entries for the group, as indices into the report.
	traffic_rows: Vec<usize>,
	session_rows: Vec<usize>,
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

impl GroupPublisher {
	fn create(origin: &origin::Producer, prefix: &Path, group: &Path, node: Option<&str>) -> Option<Self> {
		let advertised = advertised_path(prefix, group, node);
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

		// The default tier's tracks always exist, even while idle.
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
		})
	}

	/// Run this drain's rows through change detection into the pending frames.
	fn collect(&mut self, report: &Report, tick: u64) {
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
			..
		} = self;

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
			process_slot(entry.publisher, &mut slots.publisher, |snap| {
				traffic.push(broadcast, requested, &names.publisher, entry.path.clone(), snap);
			});
			process_slot(entry.subscriber, &mut slots.subscriber, |snap| {
				traffic.push(broadcast, requested, &names.subscriber, entry.path.clone(), snap);
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

	/// Serve consumer requests for tracks no drain has created yet.
	///
	/// A tier's tracks are created lazily, on the tier's first recorded byte, so
	/// a subscriber can legitimately ask before they exist (an idle protocol a
	/// collector watches on every node). Rejecting such a request forces every
	/// one of those subscribers into a resubscribe loop; instead any
	/// stats-shaped name is accepted immediately and held open with a zero
	/// frame, and the tier's real data rides the same tracks once it records
	/// ([`flush_dynamic`] finds the pair already created). Names that do not
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
/// `[<tier>/]{publisher|subscriber|sessions}.json[.z]`, or `None` for a name no
/// tier could ever produce.
fn requested_track_shape(name: &str) -> Option<RequestedShape> {
	let (base, compressed) = match name.strip_suffix(COMPRESSED_SUFFIX) {
		Some(base) => (base, true),
		None => (name, false),
	};
	let (tier, kind) = match base.rsplit_once('/') {
		Some((tier, kind)) => (Some(tier), kind),
		None => (None, base),
	};
	let sessions = match kind {
		"publisher.json" | "subscriber.json" => false,
		"sessions.json" => true,
		_ => return None,
	};
	// The tier label is an arbitrary path; require a clean one so a malformed
	// name can't mint a track a real tier could never produce.
	if let Some(tier) = tier
		&& (tier.is_empty() || tier.starts_with('/') || tier.ends_with('/') || tier.contains("//"))
	{
		return None;
	}
	Some(RequestedShape {
		plain: base.to_string(),
		compressed,
		sessions,
	})
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
	/// The last drain that reported this `(path, tier)`.
	seen: u64,
}

/// Change-detection state for one session-track root, mirroring [`SlotState`].
#[derive(Default)]
struct SessionSlotState {
	prev_emitted: Option<Presence>,
	/// The last drain that reported this root.
	seen: u64,
}

/// Per-drain work for a single `(side, tier)` slot: update the slot's
/// `prev_emitted` and hand `snap` to `emit` iff the slot is live or changed
/// this drain.
fn process_slot(snap: Traffic, slot_state: &mut SlotState, emit: impl FnOnce(Traffic)) {
	// A slot is live while any started counter still exceeds its `*_ended`
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

	/// The next route and whether it is active, skipping the caught-up marker.
	async fn next_update(announced: &mut moq_net::announce::Consumer) -> Option<(moq_net::announce::Announce, bool)> {
		loop {
			return match announced.next().await? {
				moq_net::announce::Event::Announced(route) | moq_net::announce::Event::Updated(route) => {
					Some((route, true))
				}
				moq_net::announce::Event::Retracted(route) => Some((route, false)),
				moq_net::announce::Event::Live => continue,
			};
		}
	}

	use std::collections::BTreeMap;

	use moq_net::stats::{Registry, Tier};
	use moq_net::{Timestamp, announce, broadcast, track};

	use super::*;

	fn test_producer(node: Option<&str>) -> (Producer, origin::Producer) {
		let origin = produce_origin();
		let producer = Producer::new(
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

		let (_, active) = next_update(&mut announced).await.expect("announce");
		assert!(active);
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
		let mut consumer = origin.consume().with_hidden(true).announced();
		tokio::time::advance(Duration::from_millis(1)).await;
		let (update, active) = next_update(&mut consumer).await.expect("expected announce");
		assert!(active);
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
		// The producer's reused frame must stay byte-identical to the
		// `TrafficFrame` consumers parse, including key order.
		let traffic = |bytes| {
			let mut traffic = Traffic::default();
			traffic.bytes = bytes;
			traffic
		};
		let mut frame = Frame::default();
		let mut map = BTreeMap::new();
		for (path, bytes) in [("room/b", 2), ("room/a", 1), ("other", 3), ("room/a/cam", 4)] {
			frame.entries.push((PathOwned::from(path), traffic(bytes)));
			map.insert(path.to_string(), traffic(bytes));
		}
		frame.entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
		assert_eq!(serde_json::to_vec(&frame).unwrap(), serde_json::to_vec(&map).unwrap());
		assert_eq!(serde_json::to_vec(&Frame::<Traffic>::default()).unwrap(), b"{}");
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

				let mut drain = Drain::new(Task {
					registry,
					origin: produce_origin(),
					prefix: PathOwned::from(".stats"),
					node: None,
					depth,
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
