//! Traffic counter collection for moq-net sessions.
//!
//! This module only *collects*: build a [`Registry`], hand each session a
//! tier-scoped [`Handle`] via [`Registry::tier`], and read the counters back
//! with [`Registry::snapshot`] (host-level rollup, e.g. a `/metrics` scrape) or
//! [`Registry::report`] (per-broadcast detail). Publishing the counters as MoQ
//! broadcasts lives in the `moq-stats` crate, which drains a [`Registry`] on an
//! interval and writes the JSON stats tracks.
//!
//! Traffic is bucketed by an arbitrary [`Tier`] label chosen by business logic
//! (billing class, region, ...) and, within a tier, by broadcast path and
//! [`Role`] (publisher = egress, subscriber = ingress). Connected sessions are
//! tracked separately per (tier, auth root), counting presence regardless of
//! whether any data flows.
//!
//! Per-counter semantics:
//!
//! * `announced` / `announced_closed`: cumulative count of broadcast
//!   announce/unannounce events on this `(tier, role)`. Bumped on every
//!   `publisher()` / `subscriber()` guard creation and drop.
//! * `announced_bytes`: cumulative broadcast-name length summed over each
//!   announce and unannounce of this broadcast (the name, not the encoded
//!   message size, so hop/framing overhead isn't charged, and the count is
//!   the same across protocol versions). Recorded keyed by path via
//!   [`Broadcast::publisher_announced_bytes`] /
//!   [`Broadcast::subscriber_announced_bytes`], independent of the
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
//! * `sessions` / `sessions_closed` ([`Presence`]): cumulative count of
//!   sessions connected/disconnected under an auth root on this tier.
//!   Driven by [`Handle::session`].
//!
//! Counters are strictly monotonic (only `fetch_add`); a counter going
//! backwards across reads means the underlying entry was garbage collected
//! (see [`Registry::report`]) and re-created. Downstream consumers should
//! treat decreases as a fresh segment, summing across resets when computing
//! lifetime totals.
//!
//! # Disabled stats
//!
//! [`Registry::disabled`] builds a no-op registry: all counter bumps are
//! silently dropped and nothing is ever tracked. [`Registry::default`] /
//! [`Handle::default`] return one, so call sites can hold a [`Handle`]
//! unconditionally instead of threading an `Option`.
//!
//! # Garbage collection
//!
//! [`Registry::report`] returns the current per-broadcast detail and prunes
//! entries no longer referenced by any guard, so a publisher draining the
//! registry on an interval keeps it bounded. A registry that is never
//! drained accumulates one entry per broadcast path ever seen; call
//! [`Registry::report`] periodically if you enable a registry without
//! attaching a publisher. [`Registry::snapshot`] never prunes.
//!
//! # Snapshot atomicity
//!
//! Each counter readout loads `*_closed` atomics (with `Acquire`)
//! before their open counterparts (with `Relaxed`). The matching close
//! bumps in the RAII guards' `Drop` impls use `Release`. With this
//! pairing the readout always satisfies `open >= closed` even on
//! weakly-ordered architectures (ARM, POWER): the `Acquire` load of
//! close synchronizes-with the `Release` bump that produced the
//! observed value, making every write that happened-before that close
//! (including the matching open bump on whichever thread opened the
//! guard) visible to the reading thread. Open / payload counters can
//! then stay `Relaxed` because the visibility comes for free through
//! the close pairing. The cost is a slight upward bias on the open
//! counts when a bump lands between the two loads, which never produces
//! a logically impossible (`closed > open`) readout for downstream.
//!
//! # Cycles
//!
//! A [`Registry`] built with excluded prefixes ([`Config::exclude`]) returns
//! empty handles (whose bumps no-op) for any path under one of them. The
//! `moq-stats` publisher excludes its own top-level prefix this way, breaking
//! the feedback loop where serving a stats broadcast would itself generate
//! more stats traffic.

use std::{
	collections::HashMap,
	fmt,
	sync::{
		Arc, Mutex,
		atomic::{AtomicU64, Ordering},
	},
};

use serde::{Deserialize, Serialize};
use web_async::Lock;

use crate::{AsPath, PathOwned};

/// Cumulative atomic counters for a single `(tier, role)` on a broadcast.
///
/// Every field is bumped from a RAII guard: the open counters on construction
/// and their `_closed` counterparts on drop. `broadcasts` / `broadcasts_closed`
/// are the per-(broadcast, session) subscription sentinel driven by
/// [`SessionBroadcasts`] (the first active subscription a session opens for the
/// broadcast bumps `broadcasts`, the last to close bumps `broadcasts_closed`),
/// so summed across sessions `broadcasts - broadcasts_closed` is the count of
/// distinct sessions currently subscribed.
// Kept crate-private: the load/store orderings are load-bearing (see the
// module-level "Snapshot atomicity" note), so external code only ever sees
// the derived [`Traffic`] readout.
#[derive(Default, Debug)]
pub(crate) struct Counters {
	announced: AtomicU64,
	announced_closed: AtomicU64,
	// Cumulative broadcast-name length summed over each announce and unannounce
	// of this broadcast. Counts the name, not the encoded message size, so it
	// doesn't penalize the broadcast for hop/framing overhead. Kept separate
	// from `bytes`, which is media payload.
	announced_bytes: AtomicU64,
	subscriptions: AtomicU64,
	subscriptions_closed: AtomicU64,
	broadcasts: AtomicU64,
	broadcasts_closed: AtomicU64,
	bytes: AtomicU64,
	frames: AtomicU64,
	groups: AtomicU64,
}

impl Counters {
	/// Read all atomics into a [`Traffic`]. Closed counters are read with
	/// `Acquire` ordering before their open counterparts so the readout
	/// always satisfies `open >= closed`; see the module-level "Snapshot
	/// atomicity" note. Open / payload counters stay `Relaxed`: the
	/// Acquire on close synchronizes-with the matching Release on the
	/// close bump, which transitively makes all earlier writes (including
	/// the prior open bump) visible to this thread.
	fn snapshot(&self) -> Traffic {
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
		Traffic {
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
/// [`Session`] guard for the same auth root on the same tier: `sessions`
/// bumps on connect, `sessions_closed` on disconnect.
#[derive(Default, Debug)]
struct SessionCounters {
	sessions: AtomicU64,
	sessions_closed: AtomicU64,
}

impl SessionCounters {
	/// Read the gauge into a [`Presence`]. Closed is loaded with `Acquire`
	/// before open with `Relaxed`, the same pairing as [`Counters::snapshot`],
	/// so the readout never shows `closed > open`.
	fn snapshot(&self) -> Presence {
		let sessions_closed = self.sessions_closed.load(Ordering::Acquire);
		let sessions = self.sessions.load(Ordering::Relaxed);
		Presence {
			sessions,
			sessions_closed,
		}
	}
}

/// A cumulative traffic counter readout for one slice (a broadcast on a
/// `(tier, role)`, or any sum of such slices).
///
/// Every counter is cumulative, so a rate is `delta / delta_t` and a live
/// count is `open - closed`. This is also the wire shape of one entry on a
/// published stats track (the `moq-stats` crate serializes maps of these), so
/// it derives both serde directions; unknown fields from a newer publisher are
/// ignored and missing fields from an older one default to zero.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct Traffic {
	/// Cumulative broadcast announce events on this slice.
	pub announced: u64,
	/// Cumulative broadcast unannounce events on this slice.
	pub announced_closed: u64,
	/// Cumulative announce-control bytes: the broadcast name length summed
	/// over each announce and unannounce. Distinct from `bytes` (payload).
	pub announced_bytes: u64,
	/// Per-(broadcast, session) subscription sentinel opens: the first active
	/// subscription a session holds on a broadcast.
	pub broadcasts: u64,
	/// Sentinel closes: the session's last subscription to the broadcast ended.
	pub broadcasts_closed: u64,
	/// Cumulative track-level subscriptions opened.
	pub subscriptions: u64,
	/// Cumulative track-level subscriptions closed.
	pub subscriptions_closed: u64,
	/// Cumulative payload bytes.
	pub bytes: u64,
	/// Cumulative frames delivered.
	pub frames: u64,
	/// Cumulative groups delivered.
	pub groups: u64,
}

impl Traffic {
	/// Fold another readout into this one, counter by counter.
	pub fn add(&mut self, other: Traffic) {
		self.announced += other.announced;
		self.announced_closed += other.announced_closed;
		self.announced_bytes += other.announced_bytes;
		self.broadcasts += other.broadcasts;
		self.broadcasts_closed += other.broadcasts_closed;
		self.subscriptions += other.subscriptions;
		self.subscriptions_closed += other.subscriptions_closed;
		self.bytes += other.bytes;
		self.frames += other.frames;
		self.groups += other.groups;
	}

	/// True while the broadcast is announced (an announce guard is open).
	pub fn is_announced(&self) -> bool {
		self.announced > self.announced_closed
	}

	/// Distinct sessions currently subscribed (viewers on the egress side).
	pub fn active_broadcasts(&self) -> u64 {
		self.broadcasts.saturating_sub(self.broadcasts_closed)
	}

	/// Track subscriptions currently open.
	pub fn active_subscriptions(&self) -> u64 {
		self.subscriptions.saturating_sub(self.subscriptions_closed)
	}

	/// All bytes attributable to this slice: payload plus announce overhead.
	/// Both inputs are monotonic, so the sum regresses only when the entry was
	/// garbage collected and re-created.
	pub fn total_bytes(&self) -> u64 {
		self.bytes.saturating_add(self.announced_bytes)
	}

	/// True once every open counter equals its closed counterpart: no guard is
	/// held, so no more traffic can flow until a new open.
	pub fn is_idle(&self) -> bool {
		self.announced == self.announced_closed
			&& self.subscriptions == self.subscriptions_closed
			&& self.broadcasts == self.broadcasts_closed
	}
}

/// Connected-session presence for one slice (an auth root on a tier, or any
/// sum of such slices): cumulative connects and disconnects. `sessions -
/// sessions_closed` is the current live session count.
///
/// Like [`Traffic`], this is also the wire shape of one entry on a published
/// sessions track, so it derives both serde directions.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct Presence {
	/// Cumulative sessions connected.
	pub sessions: u64,
	/// Cumulative sessions disconnected.
	pub sessions_closed: u64,
}

impl Presence {
	/// Fold another readout into this one.
	pub fn add(&mut self, other: Presence) {
		self.sessions += other.sessions;
		self.sessions_closed += other.sessions_closed;
	}

	/// Sessions currently connected.
	pub fn active(&self) -> u64 {
		self.sessions.saturating_sub(self.sessions_closed)
	}
}

/// Traffic-class label that selects which counter set a session's bumps record
/// in, so a single [`Registry`] can split customer-facing, cluster-peer, regional,
/// etc. traffic. Each tracked broadcast keeps a per-tier counter set on both its
/// publisher and subscriber sides.
///
/// The default tier ([`Tier::default`]) is unprefixed: its published tracks are
/// `publisher.json`, `subscriber.json`, and `sessions.json`. A named tier
/// prefixes every track with its label, so `Tier::new("region/sjc")` records on
/// `region/sjc/publisher.json`. The label is an arbitrary path chosen by business
/// logic; an empty label is the default tier.
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
	/// This is the naming rule the published stats tracks follow.
	pub fn track_name(&self, name: &str) -> String {
		if self.0.is_empty() {
			name.to_string()
		} else {
			format!("{}/{}", self.0.as_str(), name)
		}
	}

	/// The tier label as used in metrics: empty (`""`) for the default tier,
	/// otherwise the label (e.g. `"region/sjc"`). Mirrors the
	/// wire convention, where the default tier is unprefixed and named
	/// tiers are `<label>/`-prefixed.
	pub fn as_str(&self) -> &str {
		self.0.as_str()
	}
}

impl fmt::Display for Tier {
	/// The label, empty for the default unprefixed tier.
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		fmt::Display::fmt(&self.0, f)
	}
}

/// Publisher (egress) vs subscriber (ingress) side of a broadcast, used as a
/// label on a [`Snapshot`] traffic row. The internal bump paths track the
/// side statically, so this only surfaces on the aggregate read side.
///
/// This is the direction traffic flowed, not the session role a client advertises
/// in its SETUP ([`crate::Role`]): one session records on both sides.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Role {
	/// Egress: bytes this node published to a peer.
	Publisher,
	/// Ingress: bytes this node consumed from a peer.
	Subscriber,
}

impl Role {
	fn idx(self) -> usize {
		match self {
			Role::Publisher => 0,
			Role::Subscriber => 1,
		}
	}

	/// Lowercase label for this role (`"publisher"` / `"subscriber"`).
	pub fn as_str(self) -> &'static str {
		match self {
			Role::Publisher => "publisher",
			Role::Subscriber => "subscriber",
		}
	}
}

/// A point-in-time, host-level rollup of a registry's counters, returned
/// by [`Registry::snapshot`].
///
/// Every counter is summed across all broadcasts the registry is tracking and
/// split by tier and role, plus per-tier connected-session presence. One entry
/// per tier that recorded any traffic or session, keyed by the tier's label (so
/// an idle tier is simply absent). Intended for a scrape / `/metrics`-style
/// endpoint where per-broadcast cardinality is unwanted; use
/// [`Registry::report`] for the per-broadcast breakdown. A disabled registry
/// yields no rows.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Snapshot {
	/// Traffic totals per tier, indexed by [`Role`] within each tier; read via
	/// [`Self::traffic`].
	traffic: HashMap<Tier, [Traffic; 2]>,
	/// Session presence per tier; read via [`Self::sessions`].
	sessions: HashMap<Tier, Presence>,
}

impl Snapshot {
	/// The `(tier, role, totals)` traffic rows, one publisher and one subscriber
	/// row per tier present. Sorted by tier label then role for stable output.
	pub fn traffic(&self) -> Vec<(Tier, Role, Traffic)> {
		let mut rows = Vec::with_capacity(self.traffic.len() * 2);
		for (tier, roles) in &self.traffic {
			rows.push((tier.clone(), Role::Publisher, roles[Role::Publisher.idx()]));
			rows.push((tier.clone(), Role::Subscriber, roles[Role::Subscriber.idx()]));
		}
		rows.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()).then(a.1.idx().cmp(&b.1.idx())));
		rows
	}

	/// The `(tier, sessions)` presence rows, one per tier present, sorted by tier
	/// label.
	pub fn sessions(&self) -> Vec<(Tier, Presence)> {
		let mut rows: Vec<_> = self.sessions.iter().map(|(tier, s)| (tier.clone(), *s)).collect();
		rows.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
		rows
	}
}

/// The per-broadcast detail returned by [`Registry::report`]: one traffic
/// entry per `(broadcast, tier)` and one session entry per `(tier, root)`.
/// Entries are unordered.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct Report {
	/// Per-`(broadcast, tier)` traffic, both roles per entry.
	pub traffic: Vec<TrafficEntry>,
	/// Per-`(tier, root)` connected-session presence.
	pub sessions: Vec<SessionEntry>,
}

/// One `(broadcast, tier)` row of a [`Report`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TrafficEntry {
	/// The broadcast path the counters are keyed by.
	pub path: PathOwned,
	/// The tier the counters recorded under.
	pub tier: Tier,
	/// Egress counters (this node publishing to peers).
	pub publisher: Traffic,
	/// Ingress counters (this node consuming from peers).
	pub subscriber: Traffic,
}

/// One `(tier, root)` row of a [`Report`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SessionEntry {
	/// The tier the sessions recorded under.
	pub tier: Tier,
	/// The auth root the sessions connected under.
	pub root: PathOwned,
	/// The cumulative connect/disconnect gauge.
	pub presence: Presence,
}

/// Settings for a [`Registry`]. Construct with [`Config::new`] and chain the
/// `with_*` setters, then hand it to [`Registry::new`].
///
/// Every field here is about *collection*; the publishing knobs (origin,
/// interval, node, ...) live on the `moq-stats` producer config.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {
	/// Path prefixes whose broadcasts are not tracked: a matching path gets an
	/// empty handle whose bumps no-op. A publisher excludes its own stats
	/// prefix this way, breaking the stats-of-stats feedback loop. Empty (the
	/// default) tracks everything.
	pub exclude: Vec<PathOwned>,
}

impl Config {
	/// A config with default settings: no excluded prefixes.
	pub fn new() -> Self {
		Self::default()
	}

	/// Add a path prefix to exclude from tracking. May be chained to exclude
	/// several prefixes.
	pub fn with_exclude(mut self, prefix: impl Into<PathOwned>) -> Self {
		self.exclude.push(prefix.into());
		self
	}
}

/// Counter collection registry. Cheap to clone (`Arc` inside for the shared
/// state). One instance per relay; sessions get tier-scoped handles via
/// [`Registry::tier`]. The `moq-stats` crate drains it with
/// [`Registry::report`] to publish the counters as MoQ broadcasts.
#[derive(Clone)]
pub struct Registry {
	/// Paths under these prefixes get empty handles (bumps no-op); see
	/// [`Config::exclude`].
	exclude: Vec<PathOwned>,
	/// `None` for a disabled registry: bumps are dropped and nothing is tracked.
	shared: Option<Arc<Shared>>,
}

/// State shared by every clone of a [`Registry`].
struct Shared {
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

impl Registry {
	/// Build an enabled registry from `config`.
	pub fn new(config: Config) -> Self {
		let Config { exclude } = config;
		Self {
			exclude,
			shared: Some(Arc::new(Shared {
				entries: Lock::default(),
				sessions: Default::default(),
			})),
		}
	}

	/// Build a no-op registry: every handle is empty and all bumps are dropped.
	pub fn disabled() -> Self {
		Self {
			exclude: Vec::new(),
			shared: None,
		}
	}

	/// The excluded path prefixes. See [`Config::exclude`].
	pub fn exclude(&self) -> &[PathOwned] {
		&self.exclude
	}

	/// The shared state, panicking for a disabled registry. Tests build enabled
	/// registries so this is always present.
	#[cfg(test)]
	fn shared(&self) -> &Arc<Shared> {
		self.shared.as_ref().expect("enabled stats registry")
	}

	/// Returns a tier-scoped handle. Bumps through this handle land in the
	/// tier's counters.
	pub fn tier(&self, tier: Tier) -> Handle {
		Handle {
			stats: self.clone(),
			tier,
		}
	}

	fn entry(&self, path: impl AsPath) -> Option<Arc<BroadcastEntry>> {
		// A disabled registry never allocates state.
		let shared = self.shared.as_ref()?;
		let path = path.as_path();
		// Skip excluded prefixes (our own stats broadcasts and any sibling
		// category under the same prefix) so serving a stats broadcast doesn't
		// generate more stats.
		if self.exclude.iter().any(|prefix| path.has_prefix(prefix)) {
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

	/// Get-or-create the session gauge for `root` on `tier`. `None` for a
	/// disabled registry. Unlike [`Self::entry`], roots are auth scopes (never
	/// under a stats prefix), so no cycle-breaking filter is needed.
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

	/// Take a host-level [`Snapshot`]: every counter summed across all
	/// tracked broadcasts, split by tier and role, plus per-tier session
	/// presence. Briefly takes the entry then the session locks. Returns an
	/// all-zero snapshot for a disabled registry.
	///
	/// Unlike [`Registry::report`], this collapses per-broadcast detail into
	/// node totals (what a `/metrics`-style scrape wants) and never prunes.
	pub fn snapshot(&self) -> Snapshot {
		let mut snap = Snapshot::default();
		let Some(shared) = self.shared.as_ref() else {
			return snap;
		};
		{
			let entries = shared.entries.lock();
			for entry in entries.values() {
				let tiers = entry.tiers.lock().expect("stats tiers poisoned");
				for (tier, counters) in tiers.iter() {
					let totals = snap.traffic.entry(tier.clone()).or_default();
					totals[Role::Publisher.idx()].add(counters.publisher.snapshot());
					totals[Role::Subscriber.idx()].add(counters.subscriber.snapshot());
				}
			}
		}
		{
			let sessions = shared.sessions.lock();
			for (tier, roots) in sessions.iter() {
				let totals = snap.sessions.entry(tier.clone()).or_default();
				for counters in roots.values() {
					totals.add(counters.snapshot());
				}
			}
		}
		snap
	}

	/// Take a per-broadcast [`Report`] and prune dead entries.
	///
	/// Returns every `(broadcast, tier)` traffic readout and every `(tier,
	/// root)` session gauge, then drops the entries no guard references
	/// anymore (their final values are still in the returned report, so a
	/// publisher draining on an interval emits the closing readout exactly
	/// once). A pruned path that sees traffic again restarts from zero; see
	/// the module docs on counter resets. Returns an empty report for a
	/// disabled registry.
	pub fn report(&self) -> Report {
		let mut report = Report::default();
		let Some(shared) = self.shared.as_ref() else {
			return report;
		};
		{
			let mut entries = shared.entries.lock();
			for (path, entry) in entries.iter() {
				let tiers = entry.tiers.lock().expect("stats tiers poisoned");
				for (tier, counters) in tiers.iter() {
					report.traffic.push(TrafficEntry {
						path: path.clone(),
						tier: tier.clone(),
						publisher: counters.publisher.snapshot(),
						subscriber: counters.subscriber.snapshot(),
					});
				}
			}
			// Prune entries no guard holds anymore: with only the map's Arc
			// left, no future bump can land, so the entry is done. (A guard
			// created after the readout above still holds the Arc and keeps
			// its entry alive.)
			entries.retain(|_, entry| {
				if Arc::strong_count(entry) > 1 {
					return true;
				}
				let mut tiers = entry.tiers.lock().expect("stats tiers poisoned");
				tiers.retain(|_, counters| Arc::strong_count(counters) > 1);
				!tiers.is_empty()
			});
		}
		{
			let mut sessions = shared.sessions.lock();
			for (tier, roots) in sessions.iter() {
				for (root, counters) in roots.iter() {
					report.sessions.push(SessionEntry {
						tier: tier.clone(),
						root: root.clone(),
						presence: counters.snapshot(),
					});
				}
			}
			for roots in sessions.values_mut() {
				roots.retain(|_, counters| Arc::strong_count(counters) > 1);
			}
			sessions.retain(|_, roots| !roots.is_empty());
		}
		report
	}
}

impl Default for Registry {
	/// A disabled (no-op) registry; see [`Registry::disabled`].
	fn default() -> Self {
		Self::disabled()
	}
}

/// Tier-scoped wrapper around [`Registry`]. What [`crate::Client::with_stats`] and
/// [`crate::Server::with_stats`] accept. Cheap to clone.
#[derive(Clone)]
pub struct Handle {
	stats: Registry,
	tier: Tier,
}

impl Handle {
	/// The registry this handle is tied to.
	pub fn parent(&self) -> &Registry {
		&self.stats
	}

	/// The tier this handle bumps into.
	pub fn tier(&self) -> &Tier {
		&self.tier
	}

	/// Returns a per-broadcast handle scoped to this tier.
	///
	/// Paths under the registry's exclude prefix return an empty handle
	/// whose bumps are no-ops. This keeps stats traffic from feeding back into
	/// the registry.
	pub fn broadcast(&self, path: impl AsPath) -> Broadcast {
		Broadcast {
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
	pub fn session(&self, root: impl AsPath) -> Session {
		Session::new(self.stats.session_counters(&self.tier, root))
	}
}

impl Default for Handle {
	/// A no-op handle backed by a disabled [`Registry`].
	fn default() -> Self {
		Registry::disabled().tier(Tier::default())
	}
}

/// A per-broadcast, tier-scoped handle. Cheap to clone.
///
/// Open a broadcast-lifetime guard with [`Self::publisher`] / [`Self::subscriber`],
/// or skip straight to a track guard with [`Self::publisher_track`] /
/// [`Self::subscriber_track`] when the broadcast's lifetime is tracked
/// elsewhere.
#[derive(Clone)]
pub struct Broadcast {
	/// Resolved counters for this `(broadcast, tier)`, or `None` when the path
	/// was under the registry's exclude prefix or stats are disabled.
	counters: Option<Arc<TierCounters>>,
}

impl Broadcast {
	/// True if this handle has no underlying entry (path was under the
	/// registry's exclude prefix, or stats are disabled). All bumps through an
	/// empty handle are no-ops.
	pub fn is_empty(&self) -> bool {
		self.counters.is_none()
	}

	/// Open a broadcast-lifetime guard for the publisher (egress) role.
	/// Bumps `announced` on construction and `announced_closed` on drop.
	/// (The `broadcasts` sentinel is driven separately by
	/// [`SessionBroadcasts`]; see the module docs.)
	pub fn publisher(&self) -> Publisher {
		if let Some(counters) = &self.counters {
			counters.publisher.announced.fetch_add(1, Ordering::Relaxed);
		}
		Publisher {
			counters: self.counters.clone(),
		}
	}

	/// Open a broadcast-lifetime guard for the subscriber (ingress) role.
	/// Bumps `announced` on construction and `announced_closed` on drop.
	/// (The `broadcasts` sentinel is driven separately by
	/// [`SessionBroadcasts`]; see the module docs.)
	pub fn subscriber(&self) -> Subscriber {
		if let Some(counters) = &self.counters {
			counters.subscriber.announced.fetch_add(1, Ordering::Relaxed);
		}
		Subscriber {
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
	stats: Registry,
	tier: Tier,
	side: Side,
	counts: Arc<Mutex<HashMap<PathOwned, u32>>>,
}

impl SessionBroadcasts {
	fn new(stats: Registry, tier: Tier, side: Side) -> Self {
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
				// Release pairs with the readout's Acquire load of
				// `broadcasts_closed`; see `Publisher::drop`.
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
/// [`Handle::session`].
#[must_use = "drop the guard to record the session as closed"]
pub struct Session {
	/// `None` for a disabled registry; bumps are then dropped.
	counters: Option<Arc<SessionCounters>>,
}

impl Session {
	fn new(counters: Option<Arc<SessionCounters>>) -> Self {
		if let Some(counters) = &counters {
			counters.sessions.fetch_add(1, Ordering::Relaxed);
		}
		Self { counters }
	}
}

impl Drop for Session {
	fn drop(&mut self) {
		if let Some(counters) = &self.counters {
			// Release pairs with the readout's Acquire load of
			// `sessions_closed`; see `Publisher::drop`.
			counters.sessions_closed.fetch_add(1, Ordering::Release);
		}
	}
}

/// RAII broadcast guard for the publisher role. See [`Broadcast::publisher`].
#[must_use = "drop the guard to record the broadcast as closed"]
pub struct Publisher {
	counters: Option<Arc<TierCounters>>,
}

impl Publisher {
	/// Open a track-subscription guard. Bumps `subscriptions` on construction
	/// and `subscriptions_closed` on drop.
	pub fn track(&self, name: &str) -> PublisherTrack {
		Broadcast {
			counters: self.counters.clone(),
		}
		.publisher_track(name)
	}
}

impl Drop for Publisher {
	fn drop(&mut self) {
		if let Some(counters) = &self.counters {
			// Release pairs with the readout's Acquire load of
			// `announced_closed`, propagating the open-bump from this
			// guard's construction to whichever thread observes the close.
			counters.publisher.announced_closed.fetch_add(1, Ordering::Release);
		}
	}
}

/// RAII broadcast guard for the subscriber role. See [`Broadcast::subscriber`].
#[must_use = "drop the guard to record the broadcast as closed"]
pub struct Subscriber {
	counters: Option<Arc<TierCounters>>,
}

impl Subscriber {
	/// Open a track-subscription guard. Mirrors [`Publisher::track`].
	pub fn track(&self, name: &str) -> SubscriberTrack {
		Broadcast {
			counters: self.counters.clone(),
		}
		.subscriber_track(name)
	}
}

impl Drop for Subscriber {
	fn drop(&mut self) {
		if let Some(counters) = &self.counters {
			// See `Publisher::drop` for why this is Release.
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
			// See `Publisher::drop` for why this is Release.
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
			// See `Publisher::drop` for why this is Release.
			counters.subscriber.subscriptions_closed.fetch_add(1, Ordering::Release);
		}
	}
}

#[cfg(test)]
mod tests {
	use std::sync::{Arc, atomic::Ordering::Relaxed};

	use super::*;

	#[test]
	fn default_tier_has_empty_label() {
		let tier = Tier::default();
		assert_eq!(tier.as_str(), "");
		assert_eq!(tier.to_string(), "");
		assert_eq!(tier.track_name("publisher.json"), "publisher.json");
	}

	/// Counters for `(path, tier)`, creating the tier slot if absent.
	fn tier_counters(stats: &Registry, path: &str, tier: &Tier) -> Arc<TierCounters> {
		stats
			.shared()
			.entries
			.lock()
			.get(&PathOwned::from(path.to_string()))
			.expect("entry")
			.tier(tier)
	}

	/// The [`Presence`] for `(tier, root)`, or `None` if absent.
	fn session_snapshot(stats: &Registry, tier: &Tier, root: &str) -> Option<Presence> {
		stats
			.shared()
			.sessions
			.lock()
			.get(tier)
			.and_then(|roots| roots.get(&PathOwned::from(root.to_string())).map(|c| c.snapshot()))
	}

	fn test_stats() -> Registry {
		Registry::new(Config::new().with_exclude(".stats"))
	}

	#[test]
	fn per_broadcast_counters_isolated() {
		// Bumps on one broadcast must not leak into another.
		let stats = test_stats();
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

	#[test]
	fn default_and_named_tiers_are_independent() {
		let stats = test_stats();
		let default = stats.tier(Tier::default());
		let regional = stats.tier(Tier::new("region/sjc"));

		let default_track = default.broadcast("demo/bbb").publisher().track("video");
		default_track.bytes(100);
		let regional_track = regional.broadcast("demo/bbb").subscriber().track("audio");
		regional_track.bytes(7);

		let default_counters = tier_counters(&stats, "demo/bbb", &Tier::default());
		let regional_counters = tier_counters(&stats, "demo/bbb", &Tier::new("region/sjc"));
		assert_eq!(default_counters.publisher.bytes.load(Relaxed), 100);
		assert_eq!(default_counters.subscriber.bytes.load(Relaxed), 0);
		assert_eq!(regional_counters.publisher.bytes.load(Relaxed), 0);
		assert_eq!(regional_counters.subscriber.bytes.load(Relaxed), 7);
	}

	#[test]
	fn snapshot_rolls_up_by_tier_role_and_sessions() {
		let stats = test_stats();
		let default = stats.tier(Tier::default());
		let regional = stats.tier(Tier::new("region/sjc"));

		// Default-tier egress across two broadcasts; the snapshot sums them.
		let pub_a = default.broadcast("demo/aaa").publisher().track("video");
		pub_a.bytes(100);
		pub_a.frame();
		pub_a.group();
		let pub_b = default.broadcast("demo/bbb").publisher().track("video");
		pub_b.bytes(50);
		// Regional ingress on a different tier/role stays isolated.
		let sub_a = regional.broadcast("demo/aaa").subscriber().track("audio");
		sub_a.bytes(7);

		// Hold session guards so `sessions_closed` stays zero.
		let _s1 = default.session("acme");
		let _s2 = default.session("acme");
		let _s3 = regional.session("peer");

		let snap = stats.snapshot();

		let slot = |tier, role| {
			snap.traffic()
				.into_iter()
				.find(|(t, r, _)| *t == tier && *r == role)
				.map(|(_, _, c)| c)
				.expect("row present")
		};

		let default_publisher = slot(Tier::default(), Role::Publisher);
		assert_eq!(
			default_publisher.bytes, 150,
			"default egress bytes sum across broadcasts"
		);
		assert_eq!(default_publisher.frames, 1);
		assert_eq!(default_publisher.groups, 1);

		let regional_subscriber = slot(Tier::new("region/sjc"), Role::Subscriber);
		assert_eq!(regional_subscriber.bytes, 7, "regional ingress isolated by tier/role");
		assert_eq!(slot(Tier::default(), Role::Subscriber).bytes, 0);
		assert_eq!(slot(Tier::new("region/sjc"), Role::Publisher).bytes, 0);

		let sessions = |tier| {
			snap.sessions()
				.into_iter()
				.find(|(t, _)| *t == tier)
				.map(|(_, s)| s)
				.expect("tier present")
		};
		let default_sessions = sessions(Tier::default());
		assert_eq!(default_sessions.sessions, 2, "two default-tier sessions under one root");
		assert_eq!(default_sessions.sessions_closed, 0, "guards still held");
		assert_eq!(sessions(Tier::new("region/sjc")).sessions, 1);
	}

	#[test]
	fn report_returns_detail_and_prunes() {
		// report() surfaces per-broadcast rows while a guard is held, keeps the
		// entry across drains while live, and prunes it on the first drain
		// after the last guard drops (returning the final values that once).
		let stats = test_stats();
		let key = PathOwned::from("foo/bar");
		let bs = stats.tier(Tier::default()).broadcast("foo/bar");
		let track = bs.publisher().track("video");
		track.bytes(42);

		let report = stats.report();
		let row = report
			.traffic
			.iter()
			.find(|row| row.path == key)
			.expect("live entry present");
		assert_eq!(row.publisher.bytes, 42);
		assert_eq!(row.publisher.subscriptions, 1);
		assert!(!row.publisher.is_idle(), "track guard still open");
		assert!(
			stats.shared().entries.lock().contains_key(&key),
			"live entry kept across drains"
		);

		drop(track);
		drop(bs);

		// The drain after the last guard drops still returns the final values,
		// then prunes the entry.
		let report = stats.report();
		let row = report
			.traffic
			.iter()
			.find(|row| row.path == key)
			.expect("closing values still reported once");
		assert_eq!(row.publisher.subscriptions_closed, 1);
		assert!(row.publisher.is_idle());
		assert!(
			!stats.shared().entries.lock().contains_key(&key),
			"fully-closed entry pruned"
		);
		assert!(stats.report().traffic.is_empty(), "nothing left after the prune");
	}

	#[test]
	fn report_keeps_idle_but_announced_entry() {
		// A broadcast with a live announce guard but no traffic must stay in
		// the registry indefinitely: announced != announced_closed means a
		// subscription could still begin at any moment.
		let stats = test_stats();
		let key = PathOwned::from("foo/bar");
		let bs = stats.tier(Tier::default()).broadcast("foo/bar");
		let guard = bs.publisher();

		for _ in 0..3 {
			let report = stats.report();
			assert!(
				report.traffic.iter().any(|row| row.path == key),
				"announced-but-idle broadcast stays while the guard is held"
			);
		}

		drop(guard);
		drop(bs);
		let report = stats.report();
		let row = report.traffic.iter().find(|row| row.path == key).expect("final report");
		assert!(row.publisher.is_idle());
		assert!(!stats.shared().entries.lock().contains_key(&key));
	}

	#[test]
	fn report_prunes_empty_session_roots() {
		// Once the last session under a root disconnects, the root leaves the
		// registry on the drain that reports its final gauge.
		let stats = test_stats();
		let session = stats.tier(Tier::default()).session("acme");

		let report = stats.report();
		let row = report
			.sessions
			.iter()
			.find(|row| row.root.as_str() == "acme")
			.expect("root present");
		assert_eq!(row.presence.active(), 1);

		drop(session);
		let report = stats.report();
		let row = report
			.sessions
			.iter()
			.find(|row| row.root.as_str() == "acme")
			.expect("final gauge reported once");
		assert_eq!(row.presence.active(), 0);
		assert!(stats.report().sessions.is_empty(), "root pruned after the last drain");
		assert!(session_snapshot(&stats, &Tier::default(), "acme").is_none());
	}

	#[test]
	fn announced_bytes_recorded_per_side() {
		// Path-keyed announce-byte recording is isolated per side, accumulates,
		// works without holding a lifetime guard, and doesn't touch the payload
		// `bytes` counter.
		let stats = test_stats();
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

	#[test]
	fn paths_under_exclude_are_no_op() {
		// Our own stats broadcasts (and any sibling category under the same
		// prefix) must not feed back into the registry.
		let stats = test_stats();
		let bs = stats.tier(Tier::default()).broadcast(".stats/node/sjc");
		assert!(bs.is_empty());
		let p = bs.publisher();
		let track = p.track("video");
		track.bytes(100);
		drop(track);
		drop(p);
		assert!(stats.shared().entries.lock().is_empty());
	}

	#[test]
	fn disabled_stats_are_noop() {
		// A disabled registry allocates no shared state; every handle is empty
		// and bumps are dropped.
		let stats = Registry::default();
		assert!(stats.shared.is_none());
		let bs = stats.tier(Tier::default()).broadcast("demo/bbb");
		assert!(bs.is_empty());
		let p = bs.publisher();
		let track = p.track("video");
		track.bytes(100);
		drop(track);
		drop(p);
		assert!(stats.report().traffic.is_empty());
		assert!(stats.snapshot().traffic().is_empty());
	}

	#[test]
	fn multiple_subs_count_as_one_broadcast() {
		// Two concurrent subs from the SAME session count as one broadcast, not
		// two: broadcasts is "distinct sessions with >=1 active sub", not
		// "subscription count". broadcasts_closed only bumps once the session's
		// last sub for the broadcast closes.
		let stats = test_stats();
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

	#[test]
	fn distinct_sessions_count_as_separate_broadcasts() {
		// The viewer-count invariant: two different sessions subscribing to the
		// same broadcast bump broadcasts to 2 (each is a distinct viewer).
		let stats = test_stats();
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
		assert_eq!(r.active_broadcasts(), 1, "one remaining viewer");

		drop(s2);
		assert_eq!(raw().broadcasts_closed, 2, "both viewers gone");
	}

	#[test]
	fn session_counts_by_root() {
		// session() counts connected sessions per auth root, independent of any
		// broadcast: open bumps `sessions`, drop bumps `sessions_closed`.
		let stats = test_stats();
		let ext = stats.tier(Tier::default());

		let snap =
			|root: &str| session_snapshot(&stats, &Tier::default(), root).map(|p| (p.sessions, p.sessions_closed));

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

	#[test]
	fn traffic_parses_with_missing_and_unknown_fields() {
		// Wire forward/backward compat: a frame entry from an older publisher
		// (missing fields) or a newer one (extra fields) must still parse.
		let old: Traffic = serde_json::from_str(r#"{"announced":1,"bytes":5}"#).expect("older shape parses");
		assert_eq!(old.announced, 1);
		assert_eq!(old.bytes, 5);
		assert_eq!(old.announced_bytes, 0, "missing fields default to zero");

		let new: Traffic = serde_json::from_str(r#"{"announced":1,"announced_closed":1,"future_counter":9}"#)
			.expect("newer shape parses");
		assert!(new.is_idle());
	}

	#[test]
	fn snapshot_reads_closed_before_open() {
		// Reading closed counters before their open counterparts is the
		// guarantee that a readout never shows close > open under concurrent
		// bumps. This unit-test pins the ordering at the source level so a
		// future refactor that re-orders the loads trips the test.
		let src = include_str!("stats.rs");
		// Find the body of `impl Counters { fn snapshot(...) ... }` and
		// check the line order.
		let body_start = src.find("fn snapshot(&self) -> Traffic").expect("snapshot fn present");
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
		// `sessions_closed > sessions` leak into a readout.
		let src = include_str!("stats.rs");
		let body_start = src
			.find("fn snapshot(&self) -> Presence")
			.expect("SessionCounters::snapshot fn present");
		let body = &src[body_start..];
		let closed_pos = body.find("self.sessions_closed.load").expect("sessions_closed load");
		let open_pos = body.find("self.sessions.load").expect("sessions load");
		assert!(closed_pos < open_pos, "sessions_closed must be loaded before sessions",);
	}
}
