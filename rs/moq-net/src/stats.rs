//! Generic stats publishing for moq-net sessions.
//!
//! [`Stats`] aggregates per-broadcast counter bumps for traffic this relay
//! node is handling and publishes them on a single `<prefix>/node/<node>`
//! broadcast (or `<prefix>/node` when no node is configured). Traffic is
//! bucketed by an arbitrary [`Tier`] label chosen by business logic (billing
//! class, region, ...). The default tier is unprefixed; a named tier prefixes
//! its track names with its label. So the broadcast carries, per tier, a
//! publisher and a subscriber track:
//!
//! * `publisher.json`           : default-tier egress
//! * `subscriber.json`          : default-tier ingress
//! * `<tier>/publisher.json`   : named-tier egress (e.g. `internal/publisher.json`)
//! * `<tier>/subscriber.json`  : named-tier ingress
//!
//! plus one session track per tier that counts connected sessions keyed by
//! auth root rather than broadcast:
//!
//! * `sessions.json`            : default-tier sessions by root
//! * `<tier>/sessions.json`    : named-tier sessions by root
//!
//! The default-tier tracks always exist (emitting `{}` while idle); a named
//! tier's tracks are created the first time traffic routes to that label.
//!
//! Each per-broadcast frame is a JSON object mapping broadcast path to a
//! cumulative counter snapshot. Tier, role, and node are implied by the track
//! and broadcast paths, so they aren't repeated inside the frame. An entry
//! appears in the frame for a given `(tier, role)` on any tick where the
//! broadcast is live (any open counter still exceeds its `*_closed`
//! counterpart, so a subscription could begin at any moment) or its
//! snapshot changed since the previous tick. Once every counter equals its
//! `*_closed` counterpart no traffic can flow, so the entry is dropped. A
//! downstream aggregator computes rates from successive cumulative
//! snapshots and slices the data however a dashboard wants.
//!
//! Each session frame maps auth root to a `{ sessions, sessions_closed }`
//! snapshot: `sessions` bumps when a session authenticated under that root
//! connects, `sessions_closed` when it disconnects, so `sessions -
//! sessions_closed` is the live session count for the root. This counts
//! connected sessions regardless of whether any data flows, which is what
//! presence-based billing wants. A root entry is emitted while live or on the
//! tick it changed, then dropped once no session under it remains.
//!
//! Per-snapshot semantics:
//!
//! * `announced` / `announced_closed`: cumulative count of broadcast
//!   announce/unannounce events on this `(tier, role)`. Bumped on every
//!   `publisher()` / `subscriber()` guard creation and drop.
//! * `announced_bytes`: cumulative broadcast-name length summed over each
//!   announce and unannounce of this broadcast (the name, not the encoded
//!   message size, so hop/framing overhead isn't charged, and the count is
//!   the same across protocol versions). Recorded keyed by path via
//!   [`BroadcastStats::publisher_announced_bytes`] /
//!   [`BroadcastStats::subscriber_announced_bytes`], independent of the
//!   announce lifetime guard, so filtered/reflected/unmatched control flows
//!   still count. Kept separate from the `bytes` payload counter.
//! * `broadcasts` / `broadcasts_closed`: per-(broadcast, session)
//!   subscription sentinel. The first active subscription a peer session
//!   opens for a broadcast bumps `broadcasts`; the last one it closes bumps
//!   `broadcasts_closed`. Summed across sessions, `broadcasts -
//!   broadcasts_closed` is the number of distinct sessions currently
//!   subscribed to the broadcast (i.e. viewers on the egress side). Driven
//!   by [`SessionBroadcasts`]; use `announced` if you want all broadcasts
//!   ever seen.
//! * `subscriptions` / `subscriptions_closed`: cumulative count of
//!   track-level subscription guards opened/dropped.
//! * `bytes` / `frames` / `groups`: cumulative payload counters bumped from
//!   the session loops (both lite and IETF).
//! * `sessions` / `sessions_closed` (session tracks only): cumulative count
//!   of sessions connected/disconnected under an auth root on this tier.
//!   Driven by [`StatsHandle::session`].
//!
//! Counters are strictly monotonic (only `fetch_add`); a counter going
//! backwards across snapshots means the underlying entry was garbage
//! collected and re-created. Downstream consumers should treat decreases
//! as a fresh session segment, summing across resets when computing
//! lifetime totals.
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
//! A [`StatsConfig`] with no origin (the default) builds a no-op aggregator:
//! all counter bumps are silently dropped, no snapshot task spawns, and no
//! broadcast is published. [`Stats::default`] / [`StatsHandle::default`]
//! return one, so call sites can hold a [`StatsHandle`] unconditionally
//! instead of threading an `Option`.
//!
//! # Lifecycle
//!
//! When the config has an origin, [`Stats::new`] spawns the snapshot task
//! immediately, publishes the stats broadcast, and ticks at the configured
//! interval, writing a frame per (tier, role) track. The broadcast stays
//! announced for the lifetime of the [`Stats`] aggregator, even while idle
//! (frames just go to `{}`). The task exits when the last [`Stats`] clone is
//! dropped (the task holds only a `Weak` to the shared state).
//!
//! # Idle frame skipping
//!
//! On each tick the task compares the just-built per-(tier, role) JSON payload
//! against the last one it emitted and writes a frame only when something
//! changed. New subscribers still pick up a baseline immediately because
//! track-latest semantics retain the most recent emitted frame.
//!
//! # Snapshot atomicity
//!
//! Each [`Counters`] snapshot reads `*_closed` atomics (with `Acquire`)
//! before their open counterparts (with `Relaxed`). The matching close
//! bumps in the RAII guards' `Drop` impls use `Release`. With this
//! pairing the snapshot always satisfies `open >= closed` even on
//! weakly-ordered architectures (ARM, POWER): the `Acquire` load of
//! close synchronizes-with the `Release` bump that produced the
//! observed value, making every write that happened-before that close
//! (including the matching open bump on whichever thread opened the
//! guard) visible to the snapshot thread. Open / payload counters can
//! then stay `Relaxed` because the visibility comes for free through
//! the close pairing. The cost is a slight upward bias on the open
//! counts when a bump lands between the two loads, which never produces
//! a logically impossible (`closed > open`) snapshot for downstream.
//!
//! # Cycles
//!
//! Calling [`StatsHandle::broadcast`] for a path under the configured
//! top-level prefix returns an empty handle whose bumps no-op. This breaks
//! the feedback loop where serving a `<top-prefix>/...` broadcast would
//! itself generate more stats traffic.

use crate::{broadcast, origin, track};
use std::{
	collections::{BTreeMap, HashMap, HashSet},
	fmt,
	sync::{
		Arc, Mutex, Weak,
		atomic::{AtomicU64, Ordering},
	},
	time::Duration,
};

use serde::Serialize;
use web_async::{Lock, spawn};

use crate::{AsPath, Path, PathOwned};

/// Cumulative atomic counters for a single `(tier, role)` on a broadcast.
///
/// Every field is bumped from a RAII guard: the open counters on construction
/// and their `_closed` counterparts on drop. `broadcasts` / `broadcasts_closed`
/// are the per-(broadcast, session) subscription sentinel driven by
/// [`SessionBroadcasts`] (the first active subscription a session opens for the
/// broadcast bumps `broadcasts`, the last to close bumps `broadcasts_closed`),
/// so summed across sessions `broadcasts - broadcasts_closed` is the count of
/// distinct sessions currently subscribed.
#[derive(Default, Debug)]
#[non_exhaustive]
pub struct Counters {
	pub announced: AtomicU64,
	pub announced_closed: AtomicU64,
	/// Cumulative broadcast-name length summed over each announce and unannounce
	/// of this broadcast. Counts the name, not the encoded message size, so it
	/// doesn't penalize the broadcast for hop/framing overhead. Kept separate
	/// from `bytes`, which is media payload.
	pub announced_bytes: AtomicU64,
	pub subscriptions: AtomicU64,
	pub subscriptions_closed: AtomicU64,
	pub broadcasts: AtomicU64,
	pub broadcasts_closed: AtomicU64,
	pub bytes: AtomicU64,
	pub frames: AtomicU64,
	pub groups: AtomicU64,
}

impl Counters {
	/// Read all atomics into a `RawCounts`. Closed counters are read with
	/// `Acquire` ordering before their open counterparts so the snapshot
	/// always satisfies `open >= closed`; see the module-level "Snapshot
	/// atomicity" note. Open / payload counters stay `Relaxed`: the
	/// Acquire on close synchronizes-with the matching Release on the
	/// close bump, which transitively makes all earlier writes (including
	/// the prior open bump) visible to this thread.
	fn snapshot(&self) -> RawCounts {
		let announced_closed = self.announced_closed.load(Ordering::Acquire);
		let subscriptions_closed = self.subscriptions_closed.load(Ordering::Acquire);
		let broadcasts_closed = self.broadcasts_closed.load(Ordering::Acquire);
		let announced = self.announced.load(Ordering::Relaxed);
		let announced_bytes = self.announced_bytes.load(Ordering::Relaxed);
		let subscriptions = self.subscriptions.load(Ordering::Relaxed);
		let broadcasts = self.broadcasts.load(Ordering::Relaxed);
		let bytes = self.bytes.load(Ordering::Relaxed);
		let frames = self.frames.load(Ordering::Relaxed);
		let groups = self.groups.load(Ordering::Relaxed);
		RawCounts {
			announced,
			announced_closed,
			announced_bytes,
			broadcasts,
			broadcasts_closed,
			subscriptions,
			subscriptions_closed,
			bytes,
			frames,
			groups,
		}
	}
}

/// Per-(tier, root) session gauge. One of these is shared (via `Arc`) by every
/// [`SessionStats`] guard for the same auth root on the same tier: `sessions`
/// bumps on connect, `sessions_closed` on disconnect.
#[derive(Default, Debug)]
struct SessionCounters {
	sessions: AtomicU64,
	sessions_closed: AtomicU64,
}

impl SessionCounters {
	/// Read `(sessions, sessions_closed)`. Closed is loaded with `Acquire`
	/// before open with `Relaxed`, the same pairing as [`Counters::snapshot`],
	/// so the readout never shows `closed > open`.
	fn snapshot(&self) -> (u64, u64) {
		let closed = self.sessions_closed.load(Ordering::Acquire);
		let open = self.sessions.load(Ordering::Relaxed);
		(open, closed)
	}
}

/// Raw counter readout. Intermediate type that doesn't escape this module.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct RawCounts {
	announced: u64,
	announced_closed: u64,
	announced_bytes: u64,
	broadcasts: u64,
	broadcasts_closed: u64,
	subscriptions: u64,
	subscriptions_closed: u64,
	bytes: u64,
	frames: u64,
	groups: u64,
}

/// Traffic-class label that selects which counter set a session's bumps record
/// in, so a single [`Stats`] can split customer-facing, cluster-peer, regional,
/// etc. traffic. Each tracked broadcast keeps per-tier [`Counters`] on both its
/// publisher and subscriber sides.
///
/// The default tier ([`Tier::default`]) is unprefixed: its tracks are
/// `publisher.json`, `subscriber.json`, and `sessions.json`. A named tier
/// prefixes every track with its label, so `Tier::new("internal")` records on
/// `internal/publisher.json`. The label is an arbitrary path, so business logic
/// can bucket by anything (`"internal"`, `"region/sjc"`, ...); an empty label is
/// the default tier.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Tier(PathOwned);

impl Tier {
	/// A tier with the given label. An empty label is the default tier.
	pub fn new(label: impl Into<PathOwned>) -> Self {
		Self(label.into())
	}

	/// The tier label, empty for the default tier.
	pub fn label(&self) -> &PathOwned {
		&self.0
	}

	/// True for the default (unprefixed) tier.
	pub fn is_default(&self) -> bool {
		self.0.is_empty()
	}

	/// Track name for this tier: `name` on the default tier, else `<tier>/<name>`.
	fn track_name(&self, name: &str) -> String {
		if self.0.is_empty() {
			name.to_string()
		} else {
			format!("{}/{}", self.0.as_str(), name)
		}
	}
}

impl fmt::Display for Tier {
	/// The label, or `external` for the default (unprefixed) tier. For logs only;
	/// the wire uses the empty-prefix track names.
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		if self.0.is_empty() {
			f.write_str("external")
		} else {
			fmt::Display::fmt(&self.0, f)
		}
	}
}

/// Settings for a [`Stats`] aggregator. Construct with [`StatsConfig::new`]
/// and chain the `with_*` setters (e.g.
/// `StatsConfig::new().with_origin(origin).with_prefix(".foo")`), then hand it
/// to [`Stats::new`].
///
/// With no origin set the resulting aggregator is a no-op: bumps are dropped
/// and no task spawns. Call [`StatsConfig::with_origin`] to publish.
///
/// Distinct from the relay's clap-derived `StatsConfig`, which holds the raw
/// CLI/TOML knobs and resolves into one of these.
///
/// `#[non_exhaustive]` so new knobs can land without breaking call sites; build
/// via [`StatsConfig::new`] rather than a struct literal.
#[derive(Clone)]
#[non_exhaustive]
pub struct StatsConfig {
	/// Origin that receives the stats broadcast's `publish_broadcast` calls.
	/// When `None`, [`Stats::new`] spawns no task and publishes nothing.
	pub origin: Option<origin::Producer>,
	/// Top-level path stats are published under (default `.stats`). The full
	/// advertised path is `<prefix>/node/<node>` (or `<prefix>/node` when
	/// `node` is unset).
	pub prefix: PathOwned,
	/// Node suffix that disambiguates broadcasts from different relays sharing a
	/// cluster origin. Set this on every node in multi-relay deployments. May be
	/// multi-segment (e.g. `sjc/1`, `sjc/2`) so a region with multiple hosts can
	/// nest under a shared region key. An empty path is treated as unset.
	/// Default none.
	pub node: Option<PathOwned>,
	/// How long the snapshot task waits between publishes. Default 1s.
	pub interval: Duration,
	/// How many leading broadcast-path segments to use as a grouping key.
	///
	/// Default `0` publishes one `<prefix>/node/<node>` broadcast carrying every
	/// path. `1` publishes one broadcast per first segment at
	/// `<prefix>/<group>/node/<node>`, and larger values include more leading
	/// segments. Group broadcasts are announced while their group has live traffic;
	/// at depth `0`, the single broadcast stays announced for the aggregator's life.
	pub depth: usize,
}

impl StatsConfig {
	/// A config with default settings: no origin (no-op), `.stats` prefix, 1s
	/// snapshot interval, and no node suffix. Call [`Self::with_origin`] to
	/// actually publish.
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
	/// aggregator is a no-op.
	pub fn with_origin(mut self, origin: impl Into<Option<origin::Producer>>) -> Self {
		self.origin = origin.into();
		self
	}

	/// Override the top-level prefix (default `.stats`).
	pub fn with_prefix(mut self, prefix: impl Into<PathOwned>) -> Self {
		self.prefix = prefix.into();
		self
	}

	/// Override the snapshot interval (default 1s).
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

impl Default for StatsConfig {
	fn default() -> Self {
		Self::new()
	}
}

/// Top-level stats aggregator. Cheap to clone (`Arc` inside for the shared
/// runtime state). One instance per relay; sessions get tier-scoped handles via
/// [`Stats::tier`]. Build it from a [`StatsConfig`] via [`Stats::new`].
#[derive(Clone)]
pub struct Stats {
	prefix: PathOwned,
	/// `None` for a no-op aggregator (config had no origin): bumps are
	/// dropped and no task was spawned.
	shared: Option<Arc<StatsShared>>,
}

/// Runtime state shared by every clone of a [`Stats`] and held by the
/// snapshot task through a `Weak`. Only allocated when an origin is set.
struct StatsShared {
	origin: origin::Producer,
	entries: Lock<HashMap<PathOwned, Arc<BroadcastEntry>>>,
	/// Connected-session gauges keyed by `(tier, auth root)`. Independent of any
	/// broadcast; surfaced on the per-tier session tracks. A tier's inner map is
	/// created the first time a session records under it.
	sessions: Lock<HashMap<Tier, HashMap<PathOwned, Arc<SessionCounters>>>>,
}

/// Per-broadcast counters, lazily split by tier. A tier's [`TierCounters`] is
/// created the first time a guard records under that label, so the set of tiers
/// is fully dynamic. Bump-path call sites resolve the `Arc<TierCounters>` once
/// (at guard creation) and hold it, so the per-byte path never touches this map.
struct BroadcastEntry {
	tiers: Mutex<HashMap<Tier, Arc<TierCounters>>>,
}

impl BroadcastEntry {
	fn new() -> Self {
		Self {
			tiers: Mutex::new(HashMap::new()),
		}
	}

	/// Get-or-create the counters for `tier` on this broadcast.
	fn tier(&self, tier: &Tier) -> Arc<TierCounters> {
		self.tiers
			.lock()
			.expect("stats tiers poisoned")
			.entry(tier.clone())
			.or_default()
			.clone()
	}
}

/// Publisher and subscriber [`Counters`] for one `(broadcast, tier)`. The two
/// sides are named explicitly (rather than indexed by a `Role` enum) because
/// the bump-path call sites always know which side they're on at compile time.
#[derive(Default)]
struct TierCounters {
	publisher: Counters,
	subscriber: Counters,
}

/// Per-slot state owned by the snapshot task. The snapshot task is
/// single-threaded so this needs no atomics; one is kept per `(path, tier,
/// side)` in a task-local map.
#[derive(Default)]
struct SlotState {
	/// Last `Snapshot` we wrote to the frame for this slot, used to detect
	/// changes that warrant re-emission.
	prev_emitted: Option<Snapshot>,
}

/// Snapshot-task-local change-detection state for one `(path, tier)`: a
/// `SlotState` per side.
#[derive(Default)]
struct SideSlots {
	publisher: SlotState,
	subscriber: SlotState,
}

/// Base track names. A named tier prefixes these with its label; see
/// [`Tier::track_name`].
const PUBLISHER_TRACK: &str = "publisher.json";
const SUBSCRIBER_TRACK: &str = "subscriber.json";
const SESSIONS_TRACK: &str = "sessions.json";

impl Stats {
	/// Build a stats aggregator from `config`.
	///
	/// When `config` has an origin, this spawns the snapshot task immediately
	/// and publishes the stats broadcast; the task runs until the last [`Stats`]
	/// clone is dropped. With no origin the aggregator is a no-op (bumps are
	/// dropped, nothing is published) and no task spawns, so it's safe to build
	/// outside an async runtime.
	pub fn new(config: StatsConfig) -> Self {
		let StatsConfig {
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

		let shared = origin.map(|origin| {
			let shared = Arc::new(StatsShared {
				origin,
				entries: Lock::default(),
				sessions: Default::default(),
			});
			spawn(run_publisher(
				Arc::downgrade(&shared),
				prefix.clone(),
				node.clone(),
				depth,
				interval,
			));
			shared
		});

		Self { prefix, shared }
	}

	/// Returns the configured top-level prefix.
	pub fn prefix(&self) -> &Path<'static> {
		&self.prefix
	}

	/// The shared state, panicking for a no-op aggregator. Tests build with an
	/// origin so this is always present.
	#[cfg(test)]
	fn shared(&self) -> &Arc<StatsShared> {
		self.shared.as_ref().expect("enabled stats aggregator")
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
		// No-op aggregator (no origin) never allocates state.
		let shared = self.shared.as_ref()?;
		let path = path.as_path();
		// Skip our own stats broadcasts (and any sibling category under the
		// same prefix) so serving a stats broadcast doesn't generate more
		// stats.
		if path.has_prefix(&self.prefix) {
			return None;
		}
		let owned = path.to_owned();
		let mut entries = shared.entries.lock();
		Some(
			entries
				.entry(owned)
				.or_insert_with(|| Arc::new(BroadcastEntry::new()))
				.clone(),
		)
	}

	/// Get-or-create the session gauge for `root` on `tier`. `None` for a no-op
	/// aggregator. Unlike [`Self::entry`], roots are auth scopes (never under
	/// the stats prefix), so no cycle-breaking filter is needed.
	fn session_counters(&self, tier: &Tier, root: impl AsPath) -> Option<Arc<SessionCounters>> {
		let shared = self.shared.as_ref()?;
		let owned = root.as_path().to_owned();
		let mut sessions = shared.sessions.lock();
		Some(
			sessions
				.entry(tier.clone())
				.or_default()
				.entry(owned)
				.or_default()
				.clone(),
		)
	}
}

impl Default for Stats {
	fn default() -> Self {
		Self::new(StatsConfig::new())
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
	pub fn tier(&self) -> &Tier {
		&self.tier
	}

	/// Returns a per-broadcast handle scoped to this tier.
	///
	/// Paths under the aggregator's configured `prefix` return an empty handle
	/// whose bumps are no-ops. This keeps stats traffic from feeding back into
	/// the aggregator.
	pub fn broadcast(&self, path: impl AsPath) -> BroadcastStats {
		BroadcastStats {
			counters: self.stats.entry(path).map(|entry| entry.tier(&self.tier)),
		}
	}

	/// Per-session egress (publisher) broadcast-subscription tracker. Construct
	/// one per session and call [`SessionBroadcasts::subscribe`] for each
	/// downstream subscription so `broadcasts - broadcasts_closed` counts the
	/// distinct sessions watching each broadcast.
	pub fn publisher_broadcasts(&self) -> SessionBroadcasts {
		SessionBroadcasts::new(self.stats.clone(), self.tier.clone(), Side::Publisher)
	}

	/// Per-session ingress (subscriber) counterpart to
	/// [`Self::publisher_broadcasts`].
	pub fn subscriber_broadcasts(&self) -> SessionBroadcasts {
		SessionBroadcasts::new(self.stats.clone(), self.tier.clone(), Side::Subscriber)
	}

	/// Record a connected session authenticated under `root` on this tier. Hold
	/// the returned guard for the session's lifetime; dropping it bumps
	/// `sessions_closed`. Counts presence regardless of any data flow, so a
	/// session that merely connects is still billable. Surfaced on the session
	/// track for this tier, keyed by `root`.
	pub fn session(&self, root: impl AsPath) -> SessionStats {
		SessionStats::new(self.stats.session_counters(&self.tier, root))
	}
}

impl Default for StatsHandle {
	/// A no-op handle backed by a [`Stats::default`] aggregator.
	fn default() -> Self {
		Stats::default().tier(Tier::default())
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
	/// Resolved counters for this `(broadcast, tier)`, or `None` when the path
	/// was under the aggregator's own prefix or stats are disabled.
	counters: Option<Arc<TierCounters>>,
}

impl BroadcastStats {
	/// True if this handle has no underlying entry (path was under the
	/// aggregator's own prefix, or stats are disabled). All bumps through an
	/// empty handle are no-ops.
	pub fn is_empty(&self) -> bool {
		self.counters.is_none()
	}

	/// Open a broadcast-lifetime guard for the publisher (egress) role.
	/// Bumps `announced` on construction and `announced_closed` on drop.
	/// (The `broadcasts` sentinel is driven separately by
	/// [`SessionBroadcasts`]; see the module docs.)
	pub fn publisher(&self) -> PublisherStats {
		if let Some(counters) = &self.counters {
			counters.publisher.announced.fetch_add(1, Ordering::Relaxed);
		}
		PublisherStats {
			counters: self.counters.clone(),
		}
	}

	/// Open a broadcast-lifetime guard for the subscriber (ingress) role.
	/// Bumps `announced` on construction and `announced_closed` on drop.
	/// (The `broadcasts` sentinel is driven separately by
	/// [`SessionBroadcasts`]; see the module docs.)
	pub fn subscriber(&self) -> SubscriberStats {
		if let Some(counters) = &self.counters {
			counters.subscriber.announced.fetch_add(1, Ordering::Relaxed);
		}
		SubscriberStats {
			counters: self.counters.clone(),
		}
	}

	/// Open a publisher-track guard.
	///
	/// `_name` is unused; counters are per-broadcast only. The track name
	/// parameter is kept for symmetry with the rest of moq-net so callers
	/// don't have to thread an `Option<&str>` through subscribe sites.
	pub fn publisher_track(&self, _name: &str) -> PublisherTrack {
		if let Some(counters) = &self.counters {
			counters.publisher.subscriptions.fetch_add(1, Ordering::Relaxed);
		}
		PublisherTrack {
			counters: self.counters.clone(),
		}
	}

	/// Record `n` announce-control bytes (the broadcast name length) for one
	/// publisher-side announce/unannounce, independent of any lifetime guard.
	/// Recording is keyed by broadcast path, so it still captures messages
	/// whose matching guard was skipped, reflected, or already dropped (e.g.
	/// an unannounce whose announce was filtered out). Bumps `announced_bytes`;
	/// distinct from [`PublisherTrack::bytes`], which counts media payload.
	pub fn publisher_announced_bytes(&self, n: u64) {
		if let Some(counters) = &self.counters {
			counters.publisher.announced_bytes.fetch_add(n, Ordering::Relaxed);
		}
	}

	/// Subscriber-side counterpart to [`Self::publisher_announced_bytes`].
	pub fn subscriber_announced_bytes(&self, n: u64) {
		if let Some(counters) = &self.counters {
			counters.subscriber.announced_bytes.fetch_add(n, Ordering::Relaxed);
		}
	}

	/// Subscriber-side counterpart to [`Self::publisher_track`].
	pub fn subscriber_track(&self, _name: &str) -> SubscriberTrack {
		if let Some(counters) = &self.counters {
			counters.subscriber.subscriptions.fetch_add(1, Ordering::Relaxed);
		}
		SubscriberTrack {
			counters: self.counters.clone(),
		}
	}
}

/// Which side of a [`BroadcastEntry`] a [`SessionBroadcasts`] bumps.
#[derive(Copy, Clone)]
enum Side {
	Publisher,
	Subscriber,
}

impl Side {
	fn counters(self, tier: &TierCounters) -> &Counters {
		match self {
			Side::Publisher => &tier.publisher,
			Side::Subscriber => &tier.subscriber,
		}
	}
}

/// Per-session tracker that turns a peer session's per-broadcast subscription
/// lifecycle into `broadcasts` / `broadcasts_closed` bumps.
///
/// Hold one per session (and side). Call [`Self::subscribe`] for every
/// subscription the session opens and keep the returned [`BroadcastSubscription`]
/// alive for that subscription's lifetime. The guard refcounts subscriptions per
/// broadcast for this session, so the session's *first* subscription to a
/// broadcast bumps `broadcasts` and its *last* to drop bumps `broadcasts_closed`.
/// Summed across sessions, `broadcasts - broadcasts_closed` is the number of
/// distinct sessions currently subscribed to the broadcast (viewers on the
/// egress side).
///
/// Cheap to clone; clones share the same per-broadcast refcounts (so a single
/// logical session that clones its handle still counts as one).
#[derive(Clone)]
pub struct SessionBroadcasts {
	stats: Stats,
	tier: Tier,
	side: Side,
	counts: Arc<Mutex<HashMap<PathOwned, u32>>>,
}

impl SessionBroadcasts {
	fn new(stats: Stats, tier: Tier, side: Side) -> Self {
		Self {
			stats,
			tier,
			side,
			counts: Arc::new(Mutex::new(HashMap::new())),
		}
	}

	/// Register one active subscription to `path` for this session. Hold the
	/// returned guard for the subscription's lifetime; dropping it releases the
	/// subscription (bumping `broadcasts_closed` when it was the session's last
	/// for that broadcast).
	pub fn subscribe(&self, path: impl AsPath) -> BroadcastSubscription {
		let path = path.as_path().to_owned();
		let counters = self.stats.entry(&path).map(|entry| entry.tier(&self.tier));
		let first = {
			let mut counts = self.counts.lock().expect("stats refcount poisoned");
			let n = counts.entry(path.clone()).or_insert(0);
			let first = *n == 0;
			*n += 1;
			first
		};
		if first {
			if let Some(counters) = &counters {
				self.side.counters(counters).broadcasts.fetch_add(1, Ordering::Relaxed);
			}
		}
		BroadcastSubscription {
			counters,
			side: self.side,
			counts: self.counts.clone(),
			path,
		}
	}
}

/// RAII guard for one of a session's per-broadcast subscriptions.
/// See [`SessionBroadcasts::subscribe`].
#[must_use = "drop the guard to release the subscription"]
pub struct BroadcastSubscription {
	counters: Option<Arc<TierCounters>>,
	side: Side,
	counts: Arc<Mutex<HashMap<PathOwned, u32>>>,
	path: PathOwned,
}

impl Drop for BroadcastSubscription {
	fn drop(&mut self) {
		let last = {
			let mut counts = self.counts.lock().expect("stats refcount poisoned");
			match counts.get_mut(&self.path) {
				Some(n) => {
					*n -= 1;
					if *n == 0 {
						counts.remove(&self.path);
						true
					} else {
						false
					}
				}
				None => false,
			}
		};
		if last {
			if let Some(counters) = &self.counters {
				// Release pairs with the snapshot reader's Acquire load of
				// `broadcasts_closed`; see `PublisherStats::drop`.
				self.side
					.counters(counters)
					.broadcasts_closed
					.fetch_add(1, Ordering::Release);
			}
		}
	}
}

/// RAII guard for a connected session, keyed by auth root and tier. Bumps
/// `sessions` on construction and `sessions_closed` on drop. See
/// [`StatsHandle::session`].
#[must_use = "drop the guard to record the session as closed"]
pub struct SessionStats {
	/// `None` for a no-op aggregator; bumps are then dropped.
	counters: Option<Arc<SessionCounters>>,
}

impl SessionStats {
	fn new(counters: Option<Arc<SessionCounters>>) -> Self {
		if let Some(counters) = &counters {
			counters.sessions.fetch_add(1, Ordering::Relaxed);
		}
		Self { counters }
	}
}

impl Drop for SessionStats {
	fn drop(&mut self) {
		if let Some(counters) = &self.counters {
			// Release pairs with the snapshot reader's Acquire load of
			// `sessions_closed`; see `PublisherStats::drop`.
			counters.sessions_closed.fetch_add(1, Ordering::Release);
		}
	}
}

/// RAII broadcast guard for the publisher role. See [`BroadcastStats::publisher`].
#[must_use = "drop the guard to record the broadcast as closed"]
pub struct PublisherStats {
	counters: Option<Arc<TierCounters>>,
}

impl PublisherStats {
	/// Open a track-subscription guard. Bumps `subscriptions` on construction
	/// and `subscriptions_closed` on drop.
	pub fn track(&self, name: &str) -> PublisherTrack {
		BroadcastStats {
			counters: self.counters.clone(),
		}
		.publisher_track(name)
	}
}

impl Drop for PublisherStats {
	fn drop(&mut self) {
		if let Some(counters) = &self.counters {
			// Release pairs with the snapshot reader's Acquire load of
			// `announced_closed`, propagating the open-bump from this
			// guard's construction to whichever thread observes the close.
			counters.publisher.announced_closed.fetch_add(1, Ordering::Release);
		}
	}
}

/// RAII broadcast guard for the subscriber role. See [`BroadcastStats::subscriber`].
#[must_use = "drop the guard to record the broadcast as closed"]
pub struct SubscriberStats {
	counters: Option<Arc<TierCounters>>,
}

impl SubscriberStats {
	/// Open a track-subscription guard. Mirrors [`PublisherStats::track`].
	pub fn track(&self, name: &str) -> SubscriberTrack {
		BroadcastStats {
			counters: self.counters.clone(),
		}
		.subscriber_track(name)
	}
}

impl Drop for SubscriberStats {
	fn drop(&mut self) {
		if let Some(counters) = &self.counters {
			// See `PublisherStats::drop` for why this is Release.
			counters.subscriber.announced_closed.fetch_add(1, Ordering::Release);
		}
	}
}

/// RAII subscription guard for the publisher role.
#[must_use = "drop the guard to record the subscription as closed"]
pub struct PublisherTrack {
	counters: Option<Arc<TierCounters>>,
}

impl PublisherTrack {
	/// Bumps `frames` once.
	pub fn frame(&self) {
		if let Some(counters) = &self.counters {
			counters.publisher.frames.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// Bumps `bytes` by `n`.
	pub fn bytes(&self, n: u64) {
		if let Some(counters) = &self.counters {
			counters.publisher.bytes.fetch_add(n, Ordering::Relaxed);
		}
	}

	/// Bumps `groups` once.
	pub fn group(&self) {
		if let Some(counters) = &self.counters {
			counters.publisher.groups.fetch_add(1, Ordering::Relaxed);
		}
	}
}

impl Drop for PublisherTrack {
	fn drop(&mut self) {
		if let Some(counters) = &self.counters {
			// See `PublisherStats::drop` for why this is Release.
			counters.publisher.subscriptions_closed.fetch_add(1, Ordering::Release);
		}
	}
}

/// RAII subscription guard for the subscriber role.
#[must_use = "drop the guard to record the subscription as closed"]
pub struct SubscriberTrack {
	counters: Option<Arc<TierCounters>>,
}

impl SubscriberTrack {
	/// Bumps `frames` once.
	pub fn frame(&self) {
		if let Some(counters) = &self.counters {
			counters.subscriber.frames.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// Bumps `bytes` by `n`.
	pub fn bytes(&self, n: u64) {
		if let Some(counters) = &self.counters {
			counters.subscriber.bytes.fetch_add(n, Ordering::Relaxed);
		}
	}

	/// Bumps `groups` once.
	pub fn group(&self) {
		if let Some(counters) = &self.counters {
			counters.subscriber.groups.fetch_add(1, Ordering::Relaxed);
		}
	}
}

impl Drop for SubscriberTrack {
	fn drop(&mut self) {
		if let Some(counters) = &self.counters {
			// See `PublisherStats::drop` for why this is Release.
			counters.subscriber.subscriptions_closed.fetch_add(1, Ordering::Release);
		}
	}
}

/// Per-tick work for a single `(side, tier)` slot: build the emitted
/// `Snapshot` from the raw counters, update the slot's `prev_emitted`, and
/// hand the snap to `emit` iff the slot is live or changed this tick.
fn process_slot(counters: &Counters, slot_state: &mut SlotState, mut emit: impl FnMut(Snapshot)) {
	let raw = counters.snapshot();

	let snap = Snapshot {
		announced: raw.announced,
		announced_closed: raw.announced_closed,
		announced_bytes: raw.announced_bytes,
		broadcasts: raw.broadcasts,
		broadcasts_closed: raw.broadcasts_closed,
		subscriptions: raw.subscriptions,
		subscriptions_closed: raw.subscriptions_closed,
		bytes: raw.bytes,
		frames: raw.frames,
		groups: raw.groups,
	};

	// A slot is live while any open counter still exceeds its `*_closed`
	// counterpart: a guard is held, so a subscription could begin at any
	// moment. Live slots are emitted every tick so a downstream "currently
	// active" view always sees the full set. Once every pair is equal no
	// traffic can flow and the entry is on its way out (the global GC drops
	// it as soon as the last guard releases its `Arc`).
	let live = snap.announced != snap.announced_closed
		|| snap.subscriptions != snap.subscriptions_closed
		|| snap.broadcasts != snap.broadcasts_closed;

	// Include the entry whenever it's live OR its snapshot changed this
	// tick. Change-driven inclusion catches bumps since the previous tick
	// (incl. sub-tick flickers) and emits the final close snapshot on the
	// tick a slot transitions to fully closed.
	//
	// `None` (slot never emitted) is treated as the default Snapshot so a
	// first-tick all-zeros snap on an unused tier-side slot doesn't count
	// as a "change". Without this, every entry would surface in all four
	// tracks with zeros on the tick after creation even if only one slot
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

/// Snapshot-task-local change-detection state for one session-track root,
/// mirroring [`SlotState`].
#[derive(Default)]
struct SessionSlotState {
	prev_emitted: Option<SessionSnapshot>,
}

/// Per-tick work for one session-track root (a `(tier, root)` gauge): build the
/// snapshot, update `prev_emitted`, and emit iff a session is connected
/// (`sessions != sessions_closed`) or the snapshot changed this tick. Same
/// live-or-changed rule as [`process_slot`].
fn process_session_slot(
	counters: &SessionCounters,
	slot_state: &mut SessionSlotState,
	mut emit: impl FnMut(SessionSnapshot),
) {
	let (sessions, sessions_closed) = counters.snapshot();
	let snap = SessionSnapshot {
		sessions,
		sessions_closed,
	};

	let live = sessions != sessions_closed;
	let prev_snap = slot_state.prev_emitted.unwrap_or_default();
	let changed = snap != prev_snap;
	if changed {
		slot_state.prev_emitted = Some(snap);
	}
	if live || changed {
		emit(snap);
	}
}

/// Serialize `frame` and write it to `track` unless it's byte-identical to
/// `last` (idle-frame skipping). On success `last` is updated; on a serialize
/// or write error it's left untouched so the next tick retries.
fn flush_track<T: Serialize>(track: &mut track::Producer, frame: &T, last: &mut Vec<u8>, name: &str) {
	let json = match serde_json::to_vec(frame) {
		Ok(b) => b,
		Err(err) => {
			tracing::debug!(?err, name, "stats: failed to serialize frame");
			return;
		}
	};
	if &json == last {
		return;
	}
	if let Err(err) = track.write_frame_now(json.clone()) {
		tracing::debug!(?err, name, "stats: failed to write frame");
		return;
	}
	*last = json;
}

/// One group stats publisher and its change-detection state.
struct GroupPublisher {
	_publish: origin::Publish,
	broadcast: broadcast::Producer,
	broadcast_tracks: HashMap<String, track::Producer>,
	session_tracks: HashMap<String, track::Producer>,
	local: HashMap<PathOwned, HashMap<Tier, SideSlots>>,
	broadcast_last: HashMap<String, Vec<u8>>,
	session_local: HashMap<Tier, HashMap<PathOwned, SessionSlotState>>,
	session_last: HashMap<String, Vec<u8>>,
}

type EntryTiers = Vec<(Tier, Arc<TierCounters>)>;
type EntrySnapshots = Vec<(PathOwned, EntryTiers)>;
type SessionRoots = Vec<(PathOwned, Arc<SessionCounters>)>;
type SessionSnapshots = Vec<(Tier, SessionRoots)>;
type GroupedEntries<'a> = HashMap<PathOwned, Vec<(&'a PathOwned, &'a EntryTiers)>>;
type GroupedSessions<'a> = HashMap<PathOwned, Vec<(&'a PathOwned, &'a Arc<SessionCounters>)>>;

impl GroupPublisher {
	fn create(origin: &origin::Producer, prefix: &Path, group: &Path, node: Option<&str>) -> Option<Self> {
		let mut broadcast = broadcast::Info::new().produce();
		let mut broadcast_tracks = HashMap::new();
		let mut session_tracks = HashMap::new();

		for name in [PUBLISHER_TRACK, SUBSCRIBER_TRACK] {
			match broadcast.create_track(name, None) {
				Ok(track) => {
					broadcast_tracks.insert(name.to_string(), track);
				}
				Err(err) => {
					tracing::warn!(?err, name, "stats: failed to create track");
					return None;
				}
			}
		}
		match broadcast.create_track(SESSIONS_TRACK, None) {
			Ok(track) => {
				session_tracks.insert(SESSIONS_TRACK.to_string(), track);
			}
			Err(err) => {
				tracing::warn!(?err, name = SESSIONS_TRACK, "stats: failed to create track");
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
			broadcast_tracks,
			session_tracks,
			local: HashMap::new(),
			broadcast_last: HashMap::new(),
			session_local: HashMap::new(),
			session_last: HashMap::new(),
		})
	}

	fn flush_dynamic<T: Serialize>(
		broadcast: &mut broadcast::Producer,
		tracks: &mut HashMap<String, track::Producer>,
		last: &mut HashMap<String, Vec<u8>>,
		frames: &HashMap<String, BTreeMap<String, T>>,
	) {
		for name in frames.keys() {
			if !tracks.contains_key(name) {
				match broadcast.create_track(name.as_str(), None) {
					Ok(track) => {
						tracks.insert(name.clone(), track);
					}
					Err(err) => tracing::warn!(?err, name, "stats: failed to create track"),
				}
			}
		}

		let empty = BTreeMap::new();
		for (name, track) in tracks.iter_mut() {
			let frame = frames.get(name).unwrap_or(&empty);
			let last = last.entry(name.clone()).or_default();
			flush_track(track, frame, last, name);
		}
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

/// Publishes stats broadcasts and writes a frame per tick. Spawned once by
/// [`Stats::new`] when an origin is set; runs until every [`Stats`] clone is
/// dropped (`weak.upgrade()` returns `None`).
async fn run_publisher(
	weak: Weak<StatsShared>,
	prefix: PathOwned,
	node: Option<PathOwned>,
	depth: usize,
	interval: Duration,
) {
	let node = node.as_ref().map(|p| p.as_str());
	let mut groups: HashMap<PathOwned, GroupPublisher> = HashMap::new();

	if depth == 0 {
		let Some(shared) = weak.upgrade() else {
			return;
		};
		let Some(group) = GroupPublisher::create(&shared.origin, &prefix, &Path::empty(), node) else {
			return;
		};
		groups.insert(Path::empty().to_owned(), group);
		drop(shared);
	}

	let mut ticker = web_async::time::interval(interval);
	ticker.set_missed_tick_behavior(web_async::time::MissedTickBehavior::Delay);

	loop {
		ticker.tick().await;

		let Some(shared) = weak.upgrade() else {
			return;
		};

		let entries: EntrySnapshots = {
			let map = shared.entries.lock();
			map.iter()
				.map(|(path, entry)| {
					let tiers = entry
						.tiers
						.lock()
						.expect("stats tiers poisoned")
						.iter()
						.map(|(tier, counters)| (tier.clone(), counters.clone()))
						.collect();
					(path.clone(), tiers)
				})
				.collect()
		};

		let sessions: SessionSnapshots = {
			let map = shared.sessions.lock();
			map.iter()
				.map(|(tier, roots)| {
					let roots = roots.iter().map(|(root, c)| (root.clone(), c.clone())).collect();
					(tier.clone(), roots)
				})
				.collect()
		};

		let mut entries_by_group: GroupedEntries<'_> = HashMap::new();
		for (path, tiers) in &entries {
			entries_by_group
				.entry(group_key(path.as_str(), depth))
				.or_default()
				.push((path, tiers));
		}

		let mut sessions_by_group: GroupedSessions<'_> = HashMap::new();
		for (_tier, roots) in &sessions {
			for (root, counters) in roots {
				sessions_by_group
					.entry(group_key(root.as_str(), depth))
					.or_default()
					.push((root, counters));
			}
		}

		let mut active: HashSet<PathOwned> = HashSet::new();
		active.extend(entries_by_group.keys().cloned());
		active.extend(sessions_by_group.keys().cloned());
		if depth == 0 {
			active.insert(Path::empty().to_owned());
		}

		for group in &active {
			if !groups.contains_key(group) {
				let Some(publisher) = GroupPublisher::create(&shared.origin, &prefix, group, node) else {
					continue;
				};
				groups.insert(group.clone(), publisher);
			}
			let publisher = groups.get_mut(group).expect("just inserted");

			let mut frames: HashMap<String, BTreeMap<String, Snapshot>> = HashMap::new();
			if let Some(group_entries) = entries_by_group.get(group) {
				for &(path, tiers) in group_entries {
					let path_local = publisher.local.entry(path.clone()).or_default();
					for (tier, counters) in tiers {
						let slots = path_local.entry(tier.clone()).or_default();
						process_slot(&counters.publisher, &mut slots.publisher, |snap| {
							frames
								.entry(tier.track_name(PUBLISHER_TRACK))
								.or_default()
								.insert(path.as_str().to_string(), snap);
						});
						process_slot(&counters.subscriber, &mut slots.subscriber, |snap| {
							frames
								.entry(tier.track_name(SUBSCRIBER_TRACK))
								.or_default()
								.insert(path.as_str().to_string(), snap);
						});
					}
				}
			}

			let mut session_frames: HashMap<String, BTreeMap<String, SessionSnapshot>> = HashMap::new();
			for (tier, roots) in &sessions {
				for (root, counters) in roots {
					if group_key(root.as_str(), depth) != *group {
						continue;
					}
					let tier_local = publisher.session_local.entry(tier.clone()).or_default();
					let state = tier_local.entry(root.clone()).or_default();
					process_session_slot(counters, state, |snap| {
						session_frames
							.entry(tier.track_name(SESSIONS_TRACK))
							.or_default()
							.insert(root.as_str().to_string(), snap);
					});
				}
			}

			GroupPublisher::flush_dynamic(
				&mut publisher.broadcast,
				&mut publisher.broadcast_tracks,
				&mut publisher.broadcast_last,
				&frames,
			);
			GroupPublisher::flush_dynamic(
				&mut publisher.broadcast,
				&mut publisher.session_tracks,
				&mut publisher.session_last,
				&session_frames,
			);
		}

		drop(entries_by_group);
		drop(sessions_by_group);
		drop(entries);
		drop(sessions);

		{
			let mut map = shared.entries.lock();
			map.retain(|_, entry| {
				if Arc::strong_count(entry) > 1 {
					return true;
				}
				let mut tiers = entry.tiers.lock().expect("stats tiers poisoned");
				tiers.retain(|_, counters| Arc::strong_count(counters) > 1);
				!tiers.is_empty()
			});
			for publisher in groups.values_mut() {
				publisher.local.retain(|path, tier_states| match map.get(path) {
					Some(entry) => {
						let tiers = entry.tiers.lock().expect("stats tiers poisoned");
						tier_states.retain(|tier, _| tiers.contains_key(tier));
						true
					}
					None => false,
				});
			}
		}

		{
			let mut map = shared.sessions.lock();
			for roots in map.values_mut() {
				roots.retain(|_, counters| Arc::strong_count(counters) > 1);
			}
			map.retain(|_, roots| !roots.is_empty());
			for publisher in groups.values_mut() {
				publisher.session_local.retain(|tier, states| match map.get(tier) {
					Some(roots) => {
						states.retain(|root, _| roots.contains_key(root));
						true
					}
					None => false,
				});
			}
		}

		groups.retain(|group, _| active.contains(group));

		drop(shared);
	}
}

/// What we emit for one entry on one tier-role track. Every field comes
/// straight from [`RawCounts`]; `broadcasts` / `broadcasts_closed` are the
/// per-(broadcast, session) subscription sentinel maintained by
/// [`SessionBroadcasts`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(serde::Deserialize))]
struct Snapshot {
	announced: u64,
	announced_closed: u64,
	announced_bytes: u64,
	broadcasts: u64,
	broadcasts_closed: u64,
	subscriptions: u64,
	subscriptions_closed: u64,
	bytes: u64,
	frames: u64,
	groups: u64,
}

/// What we emit for one root on a session track. `sessions - sessions_closed`
/// is the live session count for the root.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(serde::Deserialize))]
struct SessionSnapshot {
	sessions: u64,
	sessions_closed: u64,
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
	use std::{
		collections::BTreeMap,
		sync::{Arc, atomic::Ordering::Relaxed},
	};

	use crate::{Origin, Path};

	use super::*;

	/// Counters for `(path, tier)`, creating the tier slot if absent.
	fn tier_counters(stats: &Stats, path: &str, tier: &Tier) -> Arc<TierCounters> {
		stats
			.shared()
			.entries
			.lock()
			.get(&PathOwned::from(path.to_string()))
			.expect("entry")
			.tier(tier)
	}

	/// `(sessions, sessions_closed)` for `(tier, root)`, or `None` if absent.
	fn session_snapshot(stats: &Stats, tier: &Tier, root: &str) -> Option<(u64, u64)> {
		stats
			.shared()
			.sessions
			.lock()
			.get(tier)
			.and_then(|roots| roots.get(&PathOwned::from(root.to_string())).map(|c| c.snapshot()))
	}

	/// True if a session gauge exists for `(tier, root)`.
	fn session_contains(stats: &Stats, tier: &Tier, root: &str) -> bool {
		stats
			.shared()
			.sessions
			.lock()
			.get(tier)
			.is_some_and(|roots| roots.contains_key(&PathOwned::from(root.to_string())))
	}

	fn test_stats(node: Option<&str>) -> (Stats, origin::Producer) {
		let origin = Origin::random().produce();
		let stats = Stats::new(
			StatsConfig::new()
				.with_origin(origin.clone())
				.with_node(node.map(|s| PathOwned::from(s.to_string()))),
		);
		(stats, origin)
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

	/// The advertised path normalizes a messy node suffix and drops an
	/// all-empty one. Observed through the announced path, since the task
	/// announces at construction.
	async fn announced_path_for_node(node: &str) -> String {
		let origin = Origin::random().produce();
		let _stats = Stats::new(
			StatsConfig::new()
				.with_origin(origin.clone())
				.with_node(PathOwned::from(node.to_string())),
		);
		let mut consumer = origin.consume().announced();
		tokio::time::advance(Duration::from_millis(1)).await;
		let (path, _broadcast) = consumer.next().await.expect("expected announce");
		path.as_str().to_string()
	}

	#[tokio::test(start_paused = true)]
	async fn new_normalizes_and_drops_empty_node() {
		assert_eq!(announced_path_for_node("/sjc//1/").await, ".stats/node/sjc/1");
		assert_eq!(announced_path_for_node("///").await, ".stats/node");
	}

	#[tokio::test(start_paused = true)]
	async fn per_broadcast_counters_isolated() {
		// Bumps on one broadcast must not leak into another.
		let (stats, _origin) = test_stats(Some("sjc"));
		let bs1 = stats.tier(Tier::default()).broadcast("demo/bbb");
		let bs2 = stats.tier(Tier::default()).broadcast("demo/ccc");
		let g1 = bs1.publisher().track("video");
		g1.bytes(100);
		let g2 = bs2.publisher().track("video");
		g2.bytes(7);

		assert_eq!(
			tier_counters(&stats, "demo/bbb", &Tier::default())
				.publisher
				.bytes
				.load(Relaxed),
			100
		);
		assert_eq!(
			tier_counters(&stats, "demo/ccc", &Tier::default())
				.publisher
				.bytes
				.load(Relaxed),
			7
		);
	}

	#[tokio::test(start_paused = true)]
	async fn external_and_internal_tiers_are_independent() {
		let (stats, _origin) = test_stats(Some("sjc"));
		let ext = stats.tier(Tier::default());
		let int = stats.tier(Tier::new("internal"));

		let ext_track = ext.broadcast("demo/bbb").publisher().track("video");
		ext_track.bytes(100);
		let int_track = int.broadcast("demo/bbb").subscriber().track("audio");
		int_track.bytes(7);

		let ext_c = tier_counters(&stats, "demo/bbb", &Tier::default());
		let int_c = tier_counters(&stats, "demo/bbb", &Tier::new("internal"));
		assert_eq!(ext_c.publisher.bytes.load(Relaxed), 100);
		assert_eq!(ext_c.subscriber.bytes.load(Relaxed), 0);
		assert_eq!(int_c.publisher.bytes.load(Relaxed), 0);
		assert_eq!(int_c.subscriber.bytes.load(Relaxed), 7);
	}

	#[tokio::test(start_paused = true)]
	async fn paths_under_prefix_are_no_op() {
		// Our own stats broadcasts (and any sibling category under the same
		// prefix) must not feed back into the aggregator.
		let (stats, _origin) = test_stats(Some("sjc"));
		let bs = stats.tier(Tier::default()).broadcast(".stats/node/sjc");
		assert!(bs.is_empty());
		let p = bs.publisher();
		let track = p.track("video");
		track.bytes(100);
		drop(track);
		drop(p);
		assert!(stats.shared().entries.lock().is_empty());
	}

	#[tokio::test(start_paused = true)]
	async fn disabled_stats_are_noop() {
		// A no-op aggregator (no origin) allocates no shared state and never
		// announces; every handle is empty and bumps are dropped.
		let stats = Stats::default();
		assert!(stats.shared.is_none());
		let bs = stats.tier(Tier::default()).broadcast("demo/bbb");
		assert!(bs.is_empty());
		let p = bs.publisher();
		let track = p.track("video");
		track.bytes(100);
		drop(track);
		drop(p);
	}

	#[tokio::test(start_paused = true)]
	async fn single_broadcast_path_announced() {
		// No matter how many broadcasts get bumped, exactly one stats
		// broadcast is announced (the per-node aggregate).
		let (stats, origin) = test_stats(Some("sjc/1"));
		let mut consumer = origin.consume().announced();

		let bs1 = stats.tier(Tier::default()).broadcast("foo/bar");
		let _t1 = bs1.publisher().track("video");
		let bs2 = stats.tier(Tier::default()).broadcast("baz/qux");
		let _t2 = bs2.publisher().track("video");

		tokio::time::advance(Duration::from_millis(1)).await;
		let (path, event) = consumer.next().await.expect("expected announce");
		assert!(event.broadcast().is_some());
		assert_eq!(path.as_str(), ".stats/node/sjc/1");
	}

	#[tokio::test(start_paused = true)]
	async fn task_announces_without_node_suffix() {
		let origin = Origin::random().produce();
		let stats = Stats::new(StatsConfig::new().with_origin(origin.clone()));
		let mut consumer = origin.consume().announced();

		let bs = stats.tier(Tier::default()).broadcast("foo/bar");
		let _t = bs.publisher().track("video");

		tokio::time::advance(Duration::from_millis(1)).await;
		let (path, event) = consumer.next().await.expect("expected announce");
		assert!(event.broadcast().is_some());
		assert_eq!(path.as_str(), ".stats/node");
	}

	/// Drives the snapshot task forward by `count` ticks. In paused-time
	/// tests, `tokio::time::advance` doesn't poll spawned tasks itself; we
	/// have to combine it with explicit awaits. This helper interleaves
	/// `advance` with `consumer.next()` (and later `yield_now` calls)
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
	async fn live_entry_kept_while_idle() {
		// A broadcast with a live announce guard but no traffic must stay in
		// the map indefinitely: announced != announced_closed means a
		// subscription could still begin at any moment.
		let (stats, _origin) = test_stats(Some("sjc"));
		let key = PathOwned::from("foo/bar".to_string());
		let bs = stats.tier(Tier::default()).broadcast("foo/bar");
		let guard = bs.publisher();

		drive_ticks(5).await;
		assert!(
			stats.shared().entries.lock().contains_key(&key),
			"announced-but-idle broadcast must stay while the guard is held"
		);

		drop(guard);
		drop(bs);
		// announced == announced_closed now, and no guard holds the Arc, so
		// the entry is dropped on the next tick.
		drive_ticks(1).await;
		assert!(
			!stats.shared().entries.lock().contains_key(&key),
			"entry dropped once the announce guard closes"
		);
	}

	#[tokio::test(start_paused = true)]
	async fn entry_dropped_once_fully_closed() {
		// Once every open counter equals its `*_closed` counterpart and no
		// guard holds the Arc, the entry is removed the very next tick.
		let (stats, _origin) = test_stats(Some("sjc"));
		let key = PathOwned::from("foo/bar".to_string());
		let bs = stats.tier(Tier::default()).broadcast("foo/bar");
		let track = bs.publisher().track("video");

		drive_ticks(1).await;
		assert!(
			stats.shared().entries.lock().contains_key(&key),
			"live entry present while the track guard is held"
		);

		drop(track);
		drop(bs);
		drive_ticks(1).await;
		assert!(
			!stats.shared().entries.lock().contains_key(&key),
			"fully-closed entry dropped on the next tick"
		);
	}

	#[tokio::test(start_paused = true)]
	async fn frame_emits_expected_counters() {
		let (stats, origin) = test_stats(Some("sjc"));
		let mut consumer = origin.consume().announced();
		let bs = stats.tier(Tier::default()).broadcast("foo/bar");
		let track = bs.publisher().track("video");
		track.bytes(42);
		track.frame();
		let sessions = stats.tier(Tier::default()).publisher_broadcasts();
		let _sub = sessions.subscribe("foo/bar");

		tokio::time::advance(Duration::from_millis(1100)).await;

		let (_path, event) = consumer.next().await.expect("expected announce");
		let broadcast = event.broadcast().expect("active");
		let track = broadcast
			.track("publisher.json")
			.unwrap()
			.subscribe(None)
			.await
			.expect("subscribe");
		let frame = read_frame(track).await;
		let snap = frame.get("foo/bar").expect("foo/bar entry");
		assert_eq!(snap.announced, 1, "publisher() guard bumps announced");
		assert_eq!(snap.broadcasts, 1, "one session subscribed");
		assert_eq!(snap.subscriptions, 1);
		assert_eq!(snap.bytes, 42);
		assert_eq!(snap.frames, 1);
	}

	#[tokio::test(start_paused = true)]
	async fn announced_bytes_recorded_per_side() {
		// Path-keyed announce-byte recording is isolated per side, accumulates,
		// works without holding a lifetime guard, and doesn't touch the payload
		// `bytes` counter.
		let (stats, _origin) = test_stats(Some("sjc"));
		let bs = stats.tier(Tier::default()).broadcast("foo/bar");
		bs.publisher_announced_bytes(40);
		bs.publisher_announced_bytes(2);
		bs.subscriber_announced_bytes(7);

		let counters = tier_counters(&stats, "foo/bar", &Tier::default());
		let pub_ext = counters.publisher.snapshot();
		let sub_ext = counters.subscriber.snapshot();
		assert_eq!(pub_ext.announced_bytes, 42, "publisher announce bytes accumulate");
		assert_eq!(pub_ext.bytes, 0, "announce bytes are not payload bytes");
		assert_eq!(sub_ext.announced_bytes, 7, "subscriber side tracked independently");
	}

	#[tokio::test(start_paused = true)]
	async fn announced_bytes_surfaces_in_frame() {
		let (stats, origin) = test_stats(Some("sjc"));
		let mut consumer = origin.consume().announced();
		let bs = stats.tier(Tier::default()).broadcast("foo/bar");
		let _guard = bs.publisher();
		bs.publisher_announced_bytes(123);

		tokio::time::advance(Duration::from_millis(1100)).await;

		let (_path, event) = consumer.next().await.expect("announce");
		let broadcast = event.broadcast().expect("active");
		let track = broadcast
			.track("publisher.json")
			.unwrap()
			.subscribe(None)
			.await
			.expect("subscribe");
		let frame = read_frame(track).await;
		let snap = frame.get("foo/bar").expect("foo/bar entry");
		assert_eq!(snap.announced, 1);
		assert_eq!(snap.announced_bytes, 123);
	}

	#[tokio::test(start_paused = true)]
	async fn announced_decouples_from_broadcasts() {
		// publisher() (announce) with no subscription should bump announced but
		// NOT broadcasts (which only counts sessions with an active sub).
		let (stats, origin) = test_stats(Some("sjc"));
		let mut consumer = origin.consume().announced();
		let bs = stats.tier(Tier::default()).broadcast("foo/bar");
		let _guard = bs.publisher();

		tokio::time::advance(Duration::from_millis(1100)).await;

		let (_path, event) = consumer.next().await.expect("announce");
		let broadcast = event.broadcast().expect("active");
		let track = broadcast
			.track("publisher.json")
			.unwrap()
			.subscribe(None)
			.await
			.expect("subscribe");
		let frame = read_frame(track).await;
		let snap = frame.get("foo/bar").expect("foo/bar entry");
		assert_eq!(snap.announced, 1);
		assert_eq!(snap.broadcasts, 0, "no subscription, no broadcasts sentinel");
		assert_eq!(snap.subscriptions, 0);
	}

	#[tokio::test(start_paused = true)]
	async fn short_lived_sub_is_surfaced() {
		// A subscription that opens AND closes within a single tick window
		// must still surface as a complete broadcasts open/close cycle. The
		// cumulative counters retain broadcasts=1/broadcasts_closed=1, and the
		// change-driven inclusion surfaces the entry even though it's net-idle
		// by snapshot time.
		let (stats, origin) = test_stats(Some("sjc"));
		let mut consumer = origin.consume().announced();
		let bs = stats.tier(Tier::default()).broadcast("foo/bar");
		let sessions = stats.tier(Tier::default()).publisher_broadcasts();
		{
			let track = bs.publisher().track("video");
			track.bytes(123);
			track.frame();
			let _sub = sessions.subscribe("foo/bar");
			// track + sub dropped here, all within tick 1
		}

		tokio::time::advance(Duration::from_millis(1100)).await;

		let (_path, event) = consumer.next().await.expect("announce");
		let broadcast = event.broadcast().expect("active");
		let track = broadcast
			.track("publisher.json")
			.unwrap()
			.subscribe(None)
			.await
			.expect("subscribe");
		let frame = read_frame(track).await;
		let snap = frame.get("foo/bar").expect("foo/bar entry");
		// One session opened then closed a subscription within the tick.
		assert_eq!(snap.subscriptions, 1);
		assert_eq!(snap.subscriptions_closed, 1);
		assert_eq!(snap.broadcasts, 1, "one session subscribed");
		assert_eq!(snap.broadcasts_closed, 1);
		assert_eq!(snap.bytes, 123);
		assert_eq!(snap.frames, 1);
	}

	#[tokio::test(start_paused = true)]
	async fn multiple_subs_count_as_one_broadcast() {
		// Two concurrent subs from the SAME session count as one broadcast, not
		// two: broadcasts is "distinct sessions with >=1 active sub", not
		// "subscription count". broadcasts_closed only bumps once the session's
		// last sub for the broadcast closes.
		let (stats, _origin) = test_stats(Some("sjc"));
		let bs = stats.tier(Tier::default()).broadcast("foo/bar");
		let sessions = stats.tier(Tier::default()).publisher_broadcasts();
		let pub_guard = bs.publisher();
		let t1 = pub_guard.track("video");
		let t2 = pub_guard.track("audio");
		let s1 = sessions.subscribe("foo/bar");
		let s2 = sessions.subscribe("foo/bar");

		let raw = || tier_counters(&stats, "foo/bar", &Tier::default()).publisher.snapshot();

		let r = raw();
		assert_eq!(r.subscriptions, 2, "two track subs");
		assert_eq!(r.subscriptions_closed, 0, "neither dropped yet");
		assert_eq!(r.broadcasts, 1, "one session => one broadcast");
		assert_eq!(r.broadcasts_closed, 0);

		drop(s1);
		assert_eq!(raw().broadcasts_closed, 0, "session still has a sub open");

		drop(s2);
		drop(t1);
		drop(t2);
		let r = raw();
		assert_eq!(r.subscriptions_closed, 2, "both track subs dropped");
		assert_eq!(r.broadcasts, 1);
		assert_eq!(r.broadcasts_closed, 1, "last sub closed => one broadcasts_closed");

		drop(pub_guard);
		drop(bs);
	}

	#[tokio::test(start_paused = true)]
	async fn distinct_sessions_count_as_separate_broadcasts() {
		// The viewer-count invariant: two different sessions subscribing to the
		// same broadcast bump broadcasts to 2 (each is a distinct viewer).
		let (stats, _origin) = test_stats(Some("sjc"));
		let viewer1 = stats.tier(Tier::default()).publisher_broadcasts();
		let viewer2 = stats.tier(Tier::default()).publisher_broadcasts();

		let raw = || tier_counters(&stats, "foo/bar", &Tier::default()).publisher.snapshot();

		let s1 = viewer1.subscribe("foo/bar");
		assert_eq!(raw().broadcasts, 1, "one viewer");
		let s2 = viewer2.subscribe("foo/bar");
		assert_eq!(raw().broadcasts, 2, "two distinct viewers");
		assert_eq!(raw().broadcasts_closed, 0);

		drop(s1);
		let r = raw();
		assert_eq!(r.broadcasts, 2, "broadcasts is cumulative");
		assert_eq!(r.broadcasts_closed, 1, "one viewer left");
		// broadcasts - broadcasts_closed = 1 remaining viewer.

		drop(s2);
		assert_eq!(raw().broadcasts_closed, 2, "both viewers gone");
	}

	#[tokio::test(start_paused = true)]
	async fn session_counts_by_root() {
		// session() counts connected sessions per auth root, independent of any
		// broadcast: open bumps `sessions`, drop bumps `sessions_closed`.
		let (stats, _origin) = test_stats(Some("sjc"));
		let ext = stats.tier(Tier::default());

		let snap = |root: &str| session_snapshot(&stats, &Tier::default(), root);

		let a1 = ext.session("acme");
		let a2 = ext.session("acme");
		let b1 = ext.session("globex");
		assert_eq!(snap("acme"), Some((2, 0)), "two sessions under one root");
		assert_eq!(snap("globex"), Some((1, 0)), "a distinct root is counted separately");

		drop(a1);
		assert_eq!(snap("acme"), Some((2, 1)));
		drop(a2);
		drop(b1);
		assert_eq!(snap("acme"), Some((2, 2)));
		assert_eq!(snap("globex"), Some((1, 1)));
	}

	#[tokio::test(start_paused = true)]
	async fn session_track_surfaces_by_root() {
		let (stats, origin) = test_stats(Some("sjc"));
		let mut consumer = origin.consume().announced();
		let _a = stats.tier(Tier::default()).session("acme");
		let _b = stats.tier(Tier::default()).session("acme");
		let _c = stats.tier(Tier::new("internal")).session("peer");

		tokio::time::advance(Duration::from_millis(1100)).await;

		let (_path, event) = consumer.next().await.expect("announce");
		let broadcast = event.broadcast().expect("active");

		let track = broadcast
			.track("sessions.json")
			.unwrap()
			.subscribe(None)
			.await
			.expect("subscribe");
		let frame = read_session_frame(track).await;
		let snap = frame.get("acme").expect("root entry");
		assert_eq!(snap.sessions, 2);
		assert_eq!(snap.sessions_closed, 0);
		assert!(
			!frame.contains_key("peer"),
			"internal session must not appear on the external track"
		);

		let int_track = broadcast
			.track("internal/sessions.json")
			.unwrap()
			.subscribe(None)
			.await
			.expect("subscribe");
		let snap = *read_session_frame(int_track).await.get("peer").expect("internal entry");
		assert_eq!(snap.sessions, 1);
	}

	#[tokio::test(start_paused = true)]
	async fn session_root_dropped_when_empty() {
		// Once the last session under a root disconnects, the root leaves the
		// map on the next tick (its final snapshot already emitted).
		let (stats, _origin) = test_stats(Some("sjc"));
		let session = stats.tier(Tier::default()).session("acme");

		drive_ticks(1).await;
		assert!(
			session_contains(&stats, &Tier::default(), "acme"),
			"root present while a session is connected"
		);

		drop(session);
		drive_ticks(1).await;
		assert!(
			!session_contains(&stats, &Tier::default(), "acme"),
			"root GC'd after the last session leaves"
		);
	}

	#[tokio::test(start_paused = true)]
	async fn unused_slots_dont_surface() {
		// A broadcast that only sees default-tier publisher traffic must NOT
		// surface on its sibling default-tier subscriber track. Regression for
		// the "None != Some(default)" first-tick change-detection bug: without
		// the unwrap_or_default fix, the entry would surface once in every track
		// even when only one slot had real activity.
		let (stats, origin) = test_stats(Some("sjc"));
		let mut consumer = origin.consume().announced();
		let bs = stats.tier(Tier::default()).broadcast("foo/bar");
		let track = bs.publisher().track("video");
		track.frame();

		drive_ticks(2).await;

		let (_path, event) = consumer.next().await.expect("announce");
		let broadcast = event.broadcast().expect("active");

		// Default-tier publisher slot SHOULD include foo/bar.
		let pub_track = broadcast
			.track("publisher.json")
			.unwrap()
			.subscribe(None)
			.await
			.expect("subscribe");
		assert!(
			read_frame(pub_track).await.contains_key("foo/bar"),
			"publisher.json must include the active foo/bar entry"
		);

		// The default-tier subscriber slot had zero activity; its first frame
		// must be `{}`, not `{"foo/bar": {all zeros}}`.
		let sub_track = broadcast
			.track("subscriber.json")
			.unwrap()
			.subscribe(None)
			.await
			.expect("subscribe");
		let frame = read_frame(sub_track).await;
		assert!(frame.is_empty(), "subscriber.json must be empty, got {frame:?}");

		// The internal tier never saw traffic, so its tracks were never created.
		for name in ["internal/publisher.json", "internal/subscriber.json"] {
			assert!(
				broadcast.track(name).is_err(),
				"{name} must not exist for a tier with no traffic",
			);
		}
	}

	#[test]
	fn snapshot_reads_closed_before_open() {
		// Reading closed counters before their open counterparts is the
		// guarantee that the emitted Snapshot never shows close > open
		// under concurrent bumps. This unit-test pins the ordering at the
		// source level so a future refactor that re-orders the loads
		// trips the test.
		let src = include_str!("stats.rs");
		// Find the body of `impl Counters { fn snapshot(...) ... }` and
		// check the line order.
		let body_start = src
			.find("fn snapshot(&self) -> RawCounts")
			.expect("snapshot fn present");
		let body = &src[body_start..];
		let closed_pos = body.find("self.announced_closed.load").expect("announced_closed load");
		let open_pos = body.find("self.announced.load(").expect("announced load");
		assert!(
			closed_pos < open_pos,
			"announced_closed must be loaded before announced; reversing breaks the open>=closed invariant",
		);
		let subs_closed_pos = body
			.find("self.subscriptions_closed.load")
			.expect("subscriptions_closed load");
		let subs_pos = body.find("self.subscriptions.load").expect("subscriptions load");
		assert!(
			subs_closed_pos < subs_pos,
			"subscriptions_closed must be loaded before subscriptions",
		);
		let bcast_closed_pos = body
			.find("self.broadcasts_closed.load")
			.expect("broadcasts_closed load");
		let bcast_pos = body.find("self.broadcasts.load").expect("broadcasts load");
		assert!(
			bcast_closed_pos < bcast_pos,
			"broadcasts_closed must be loaded before broadcasts",
		);
	}

	#[test]
	fn session_snapshot_reads_closed_before_open() {
		// Same `closed`-before-`open` invariant as `Counters::snapshot`, pinned
		// at the source level so a reordering refactor can't let
		// `sessions_closed > sessions` leak into an emitted session frame.
		let src = include_str!("stats.rs");
		let body_start = src
			.find("fn snapshot(&self) -> (u64, u64)")
			.expect("SessionCounters::snapshot fn present");
		let body = &src[body_start..];
		let closed_pos = body.find("self.sessions_closed.load").expect("sessions_closed load");
		let open_pos = body.find("self.sessions.load").expect("sessions load");
		assert!(closed_pos < open_pos, "sessions_closed must be loaded before sessions",);
	}

	async fn read_frame(mut track: track::Subscriber) -> BTreeMap<String, Snapshot> {
		let bytes = track.read_frame().await.expect("ok").expect("frame");
		serde_json::from_slice(&bytes).expect("json parse")
	}

	async fn read_session_frame(mut track: track::Subscriber) -> BTreeMap<String, SessionSnapshot> {
		let bytes = track.read_frame().await.expect("ok").expect("frame");
		serde_json::from_slice(&bytes).expect("json parse")
	}
}
