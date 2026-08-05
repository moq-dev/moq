//! The publishing half: drain a [`Registry`] on an interval into stats tracks.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};
use std::time::Duration;

use std::task::Poll;

use moq_net::stats::{Presence, Registry, Role, Tier, Traffic};
use moq_net::{Path, PathOwned, broadcast, kio, origin, track};
use serde::Serialize;
use web_async::spawn;

use crate::{COMPRESSED_SUFFIX, SessionsFrame, TrafficFrame, sessions_track, traffic_track};

/// Settings for a [`Producer`]. Construct with [`ProducerConfig::new`] and chain
/// the `with_*` setters (e.g.
/// `ProducerConfig::new().with_origin(origin).with_prefix(".foo")`), then hand it
/// to [`Producer::new`].
///
/// With no origin set the resulting producer is a no-op: its registry is
/// disabled (bumps are dropped) and no task spawns. Call
/// [`ProducerConfig::with_origin`] to publish.
#[derive(Clone)]
#[non_exhaustive]
pub struct ProducerConfig {
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

impl ProducerConfig {
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

impl Default for ProducerConfig {
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
	pub fn new(config: ProducerConfig) -> Self {
		let ProducerConfig {
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

		let registry = Registry::new(moq_net::stats::Config::new().with_exclude(prefix.clone()));
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
		let node = self.node.as_ref().map(moq_net::Path::as_str);
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
				for (_, publisher) in groups.drain() {
					publisher.finish();
				}
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

				// A requested pair whose tier just recorded becomes an ordinary
				// tier pair: kept for the broadcast's life, no longer counting
				// against the requested quota.
				for name in frames.keys().chain(session_frames.keys()) {
					publisher.requested.remove(name);
				}

				publisher.traffic.flush(&mut publisher.broadcast, &frames);
				publisher.sessions.flush(&mut publisher.broadcast, &session_frames);
			}

			// Serve consumer requests for tracks no drain has created yet: a
			// tier's tracks appear lazily on its first traffic, so a subscriber
			// arriving first would otherwise be rejected and forced into a
			// retry loop (fleet-wide, that rejection churn is a log and CPU
			// storm). Held open with zeros instead; see `serve_requests`.
			for publisher in groups.values_mut() {
				publisher.serve_requests();
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

			// Deliberate unpublish: finish evicted publishers (tracks included)
			// rather than dropping them, so there is no dropped-without-finish
			// warning.
			let evicted: Vec<PathOwned> = groups
				.keys()
				.filter(|group| !active.contains(*group))
				.cloned()
				.collect();
			for group in evicted {
				if let Some(publisher) = groups.remove(&group) {
					publisher.finish();
				}
			}
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
		Ok(Self::from_tracks(plain_track, compressed_track))
	}

	/// Build a pair from consumer requests, creating whichever flavor was not
	/// requested. A popped request is no longer queued, so `create_track`'s
	/// queued-request fulfillment cannot reach it; the caller collects both
	/// flavors' popped requests and this serves each through its actual
	/// request where one exists.
	fn adopt(broadcast: &mut broadcast::Producer, name: &str, pending: PendingPair) -> Result<Self, moq_net::Error> {
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
		let plain_config = moq_json::snapshot::ProducerConfig::default().with_delta_ratio(0);
		let compressed_config = moq_json::snapshot::ProducerConfig::default().with_compression(true);

		Self {
			plain: moq_json::snapshot::Producer::new(plain_track, plain_config),
			compressed: moq_json::snapshot::Producer::new(compressed_track, compressed_config),
		}
	}

	/// Whether any consumer exists on either flavor.
	fn is_used(&self) -> bool {
		self.plain.is_used() || self.compressed.is_used()
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
struct TrackFamily<T> {
	tracks: HashMap<String, TrackPair<T>>,
	/// Valid-shaped requests awaiting quota, keyed by plain name and bounded by
	/// [`MAX_PARKED_REQUESTS`] across both families. Adopted as the quota
	/// frees, or dropped once every requester leaves.
	parked: HashMap<String, PendingPair>,
}

impl<T: Serialize + Default> TrackFamily<T> {
	fn new() -> Self {
		Self {
			tracks: HashMap::new(),
			parked: HashMap::new(),
		}
	}

	/// Ensure a track pair exists for every frame this drain produced, then push
	/// each pair its frame (an empty one when the drain had nothing for it, so a
	/// track whose last entry closed transitions to `{}` exactly once).
	///
	/// A pair created here serves any parked requests for its name: a parked
	/// request was already popped off the broadcast queue, so `create_track`'s
	/// queued-request fulfillment cannot reach it, and creating the pair blind
	/// would strand its requesters on a name that now exists.
	fn flush(&mut self, broadcast: &mut broadcast::Producer, frames: &HashMap<String, T>) {
		for name in frames.keys() {
			if !self.tracks.contains_key(name) {
				let result = match self.parked.remove(name) {
					Some(pending) => TrackPair::adopt(broadcast, name, pending),
					None => TrackPair::create(broadcast, name),
				};
				match result {
					Ok(pair) => {
						self.tracks.insert(name.clone(), pair);
					}
					Err(err) => tracing::warn!(?err, name, "stats: failed to create track"),
				}
			}
		}

		let empty = T::default();
		for (name, pair) in self.tracks.iter_mut() {
			pair.update(name, frames.get(name).unwrap_or(&empty));
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
	fn adopt_parked(&mut self, broadcast: &mut broadcast::Producer, requested: &mut HashSet<String>) {
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
		broadcast: &mut broadcast::Producer,
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
				pair.update(&plain, &T::default());
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
	traffic: TrackFamily<TrafficFrame>,
	sessions: TrackFamily<SessionsFrame>,
	local: HashMap<PathOwned, HashMap<Tier, SideSlots>>,
	session_local: HashMap<Tier, HashMap<PathOwned, SessionSlotState>>,
}

impl GroupPublisher {
	fn create(origin: &origin::Producer, prefix: &Path, group: &Path, node: Option<&str>) -> Option<Self> {
		let advertised = advertised_path(prefix, group, node);
		let mut broadcast = match origin.create_broadcast(&advertised, broadcast::Route::new().with_announce(true)) {
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
			match TrackPair::create(&mut broadcast, &name) {
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
		match TrackPair::create(&mut broadcast, &name) {
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
		})
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

		self.traffic.adopt_parked(&mut self.broadcast, &mut self.requested);
		self.sessions.adopt_parked(&mut self.broadcast, &mut self.requested);
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

	use moq_net::stats::{Registry, Tier};
	use moq_net::{Origin, Timestamp, announce, broadcast, track};

	use super::*;

	fn test_producer(node: Option<&str>) -> (Producer, origin::Producer) {
		let origin = Origin::random().produce();
		let producer = Producer::new(
			ProducerConfig::new()
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
		let origin = Origin::random().produce();
		// Egress (publisher side) is tagged; the local publisher stays untagged.
		let egress = origin.consume().with_stats(ctx);

		let mut announced = egress.announced();
		let mut source = origin
			.create_broadcast(path, broadcast::Route::announced())
			.expect("create_broadcast");
		let mut producer = source.create_track("video", None).expect("create_track");

		// Let the origin's source watcher attach and announce (paused time advances
		// instantly and yields to the spawned tasks).
		tokio::time::sleep(Duration::from_millis(1)).await;
		tokio::time::sleep(Duration::from_millis(1)).await;

		let announce::Update { broadcast, .. } = announced.next().await.expect("announce");
		let consumer = broadcast.expect("active");

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

	/// Read the latest buffered traffic frame off a track. The producer emits an
	/// immediate first (often empty) frame at time zero, so a test that records
	/// traffic asynchronously reads the accumulated state rather than that stale one.
	async fn read_last_frame(broadcast: &moq_net::broadcast::Consumer, name: &str) -> BTreeMap<String, Traffic> {
		use futures::FutureExt;
		let mut track = subscribe(broadcast, name).await;
		let mut last = track.read_frame().await.expect("ok").expect("frame");
		while let Some(Ok(Some(frame))) = track.read_frame().now_or_never() {
			last = frame;
		}
		serde_json::from_slice(&last.payload).expect("json parse")
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
		assert_eq!(snap.announced, 1, "egress announce stream bumps announced");
		assert_eq!(snap.broadcasts, 1, "one session subscribed");
		assert_eq!(snap.subscriptions, 1);
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
		assert_eq!(snap.announced, 1);
		assert_eq!(
			snap.announced_bytes,
			"foo/bar".len() as u64,
			"name length recorded on announce"
		);
	}

	#[tokio::test(start_paused = true)]
	async fn announced_decouples_from_broadcasts() {
		// An announce with no subscription should bump announced but NOT broadcasts
		// (which only counts sessions with an active sub).
		let (producer, origin) = test_producer(Some("sjc"));
		let _f = feed(producer.registry(), Tier::default(), "foo/bar", false, 0, 0).await;

		drive_tick().await;

		let (_, broadcast) = announced(&origin).await;
		let frame = read_last_frame(&broadcast, "publisher.json").await;
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
		let _c = producer.registry().tier(Tier::new("region/sjc")).session("peer");

		drive_tick().await;

		let (_, broadcast) = announced(&origin).await;
		let frame = read_session_frame(&broadcast, "sessions.json").await;
		let snap = frame.get("acme").expect("root entry");
		assert_eq!(snap.sessions, 2);
		assert_eq!(snap.sessions_closed, 0);
		assert!(
			!frame.contains_key("peer"),
			"regional session must not appear on the default track"
		);

		let snap = *read_session_frame(&broadcast, "region/sjc/sessions.json")
			.await
			.get("peer")
			.expect("regional entry");
		assert_eq!(snap.sessions, 1);
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
		let mut sub = subscribing.await.expect("an idle tier's track is held open");
		let frame = sub.read_frame().await.expect("ok").expect("frame");
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
		assert_eq!(group_key("acme/room/cam", 0), Path::empty().to_owned());
		assert_eq!(group_key("acme/room/cam", 1), Path::new("acme").to_owned());
		assert_eq!(group_key("acme/room/cam", 2), Path::new("acme/room").to_owned());
		assert_eq!(group_key("acme/room", 3), Path::new("acme/room").to_owned());
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
		let mut sub = subscribing.await.expect("held open, not rejected");
		let frame = sub.read_frame().await.expect("ok").expect("frame");
		let parsed: BTreeMap<String, Traffic> = serde_json::from_slice(&frame.payload).expect("json");
		assert!(parsed.is_empty(), "an idle tier serves zeros");

		// The tier records: the same subscription carries the data.
		let _rtmp = feed(producer.registry(), Tier::new("rtmp"), "foo/live", true, 1, 7).await;
		drive_tick().await;
		let frame = sub.read_frame().await.expect("ok").expect("frame");
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
		let mut sub = subscribing.await.expect("held open, not rejected");
		let frame = sub.read_frame().await.expect("ok").expect("frame");
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
		let mut sub = subscribing.await.expect("fulfilled by the tick's own creation");
		let frame = sub.read_frame().await.expect("ok").expect("frame");
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
		let mut sub = subscribing.await.expect("adopted by the flush");
		let frame = sub.read_frame().await.expect("ok").expect("frame");
		let parsed: BTreeMap<String, Traffic> = serde_json::from_slice(&frame.payload).expect("json");
		assert_eq!(parsed.get("foo/live").expect("entry").bytes, 9);
	}
}
