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
//! # Where counting happens
//!
//! Counting lives in the model layer, not the wire loops. A session tags its
//! origin pair with a [`Session`] context ([`crate::Client::with_stats`] /
//! [`crate::Server::with_stats`], which call `origin::{Consumer, Producer}`
//! `with_stats`); every derived handle (broadcast, announce, track, group, frame)
//! then attributes its reads (egress = publisher) and writes (ingress =
//! subscriber) through that context. So any protocol that drives the model gets
//! the full counter set for free, and an untagged handle pays nothing.
//!
//! Per-counter semantics:
//!
//! * `announced` / `announced_closed`: cumulative broadcast announce/unannounce
//!   events on this `(tier, role)`. Driven by the tagged announce stream on the
//!   egress side, and by `create_broadcast` route transitions on the ingress
//!   side.
//! * `announced_bytes`: cumulative broadcast-name length summed over each
//!   model-visible announce and unannounce of this broadcast (the name, not the
//!   encoded message size, so hop/framing overhead isn't charged, and the count
//!   is the same across protocol versions). Kept separate from the `bytes`
//!   payload counter.
//! * `broadcasts` / `broadcasts_closed`: per-(broadcast, context) egress
//!   subscription sentinel. The first active subscription a context opens for a
//!   broadcast bumps `broadcasts`; the last it closes bumps `broadcasts_closed`.
//!   Summed across contexts, `broadcasts - broadcasts_closed` is the number of
//!   distinct sessions currently subscribed (viewers on the egress side).
//! * `subscriptions` / `subscriptions_closed`: cumulative track-level
//!   subscriptions opened/dropped (egress `track::Subscriber`, ingress
//!   `track::Producer`).
//! * `fetches`: cumulative one-shot group fetches *requested* by a calling
//!   context, counted once per coalesced fetch at request time. A fetch that
//!   resolves to `NotFound` still counts. Separate from `subscriptions` and the
//!   viewer refcount; fetched payload still flows into `bytes` / `frames` /
//!   `groups`.
//! * `bytes` / `frames` / `groups`: cumulative payload counters bumped as
//!   groups/frames are read (egress) or written (ingress) in the model.
//! * `datagrams`: cumulative single-frame groups carried over unreliable QUIC
//!   datagrams. A datagram is metered as the group it stands in for, so it also
//!   bumps `groups`, `frames`, and `bytes`; this counter breaks out how many of
//!   those took the datagram path.
//! * `sessions` / `sessions_closed` ([`Presence`]): cumulative count of
//!   sessions connected/disconnected under an auth root on this tier.
//!   Driven by [`Handle::session`] (the [`Session`] context).
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
/// Open counters bump when a model handle records activity; their `_closed`
/// counterparts bump from the [`Scope`] / [`Subscription`] / [`Announce`] RAII
/// guards on drop. `broadcasts` / `broadcasts_closed` are the per-(broadcast,
/// context) egress subscription sentinel (the first active subscription a context
/// opens for the broadcast bumps `broadcasts`, the last to close bumps
/// `broadcasts_closed`), so summed across contexts `broadcasts -
/// broadcasts_closed` is the count of distinct sessions currently subscribed.
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
	// Cumulative one-shot group fetches requested by a calling context. Counted
	// once per coalesced fetch, at request time rather than on resolution; does
	// not touch `subscriptions` or the viewer refcount.
	fetches: AtomicU64,
	broadcasts: AtomicU64,
	broadcasts_closed: AtomicU64,
	bytes: AtomicU64,
	frames: AtomicU64,
	groups: AtomicU64,
	// Subset of `groups` carried over an unreliable QUIC datagram.
	datagrams: AtomicU64,
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
		let fetches = self.fetches.load(Ordering::Relaxed);
		let broadcasts = self.broadcasts.load(Ordering::Relaxed);
		let bytes = self.bytes.load(Ordering::Relaxed);
		let frames = self.frames.load(Ordering::Relaxed);
		let groups = self.groups.load(Ordering::Relaxed);
		let datagrams = self.datagrams.load(Ordering::Relaxed);
		Traffic {
			announced,
			announced_closed,
			announced_bytes,
			broadcasts,
			broadcasts_closed,
			subscriptions,
			subscriptions_closed,
			fetches,
			bytes,
			frames,
			groups,
			datagrams,
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
	/// Cumulative one-shot group fetches requested. Counted once per coalesced fetch
	/// when the fetch is issued, so one that resolves to `NotFound` still counts.
	/// Separate from `subscriptions` and the viewer refcount. Fetched payload still
	/// flows into `bytes`/`frames`/`groups`.
	pub fetches: u64,
	/// Cumulative payload bytes.
	pub bytes: u64,
	/// Cumulative frames delivered.
	pub frames: u64,
	/// Cumulative groups delivered.
	pub groups: u64,
	/// Cumulative single-frame groups delivered over an unreliable QUIC datagram.
	/// A subset of `groups`: each one also counts there and its payload in
	/// `frames` / `bytes`.
	pub datagrams: u64,
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
		self.fetches += other.fetches;
		self.bytes += other.bytes;
		self.frames += other.frames;
		self.groups += other.groups;
		self.datagrams += other.datagrams;
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

	/// Record a connected session authenticated under `root` on this tier. Hold
	/// the returned guard for the session's lifetime; dropping it bumps
	/// `sessions_closed`. Counts presence regardless of any data flow, so a
	/// session that merely connects is still billable. Surfaced on the session
	/// track for this tier, keyed by `root`.
	pub fn session(&self, root: impl AsPath) -> Session {
		Session::new(self.clone(), self.stats.session_counters(&self.tier, root))
	}
}

impl Default for Handle {
	/// A no-op handle backed by a disabled [`Registry`].
	fn default() -> Self {
		Registry::disabled().tier(Tier::default())
	}
}

/// Which side of a [`TierCounters`] a bump lands on: publisher (egress) or
/// subscriber (ingress). `Default` is `Publisher`, chosen only so an empty
/// [`Meter`] / [`Scope`] has one; it never records because its counters are `None`.
#[derive(Copy, Clone, Default)]
enum Side {
	#[default]
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

/// Per-connection stats context, created via [`Handle::session`].
///
/// Cheap to clone (an `Arc` inside): one context is shared by both origin handles
/// of a session (its publish and subscribe halves) so presence and viewer counts
/// are never double-attributed. It carries three things:
///
/// * the tier + auth root, so any broadcast reached through a tagged origin handle
///   resolves the right per-`(path, tier)` counters,
/// * the presence gauge: `sessions` bumps when the context is created and
///   `sessions_closed` when the last clone drops (a more honest close than a
///   separately-held guard),
/// * the egress viewer refcount map (first/last active subscription per broadcast),
///   driving `broadcasts` / `broadcasts_closed`.
///
/// [`Session::default`] is the no-op context (disabled registry / untagged caller):
/// every bump reached through it is silently dropped, so a handle can hold one
/// unconditionally instead of threading an `Option`.
#[derive(Clone, Default)]
pub struct Session {
	/// `None` for the no-op context (disabled registry or a `default()` handle).
	inner: Option<Arc<SessionInner>>,
}

/// The shared state behind a [`Session`]. Its `Drop` (on the last clone) records
/// the session as closed.
struct SessionInner {
	/// Registry + tier, so derived model handles resolve `(path, tier)` counters.
	handle: Handle,
	/// The presence gauge for `(tier, root)`, or `None` for a disabled registry.
	presence: Option<Arc<SessionCounters>>,
	/// Egress viewer refcount, keyed by absolute broadcast path: the first active
	/// subscription this context opens for a broadcast bumps `broadcasts`, the last
	/// to close bumps `broadcasts_closed`.
	viewers: Mutex<HashMap<PathOwned, u32>>,
}

impl Session {
	fn new(handle: Handle, presence: Option<Arc<SessionCounters>>) -> Self {
		if let Some(presence) = &presence {
			presence.sessions.fetch_add(1, Ordering::Relaxed);
		}
		Self {
			inner: Some(Arc::new(SessionInner {
				handle,
				presence,
				viewers: Mutex::new(HashMap::new()),
			})),
		}
	}

	/// Egress (publisher / reads) scope for a broadcast path. The path is the
	/// absolute broadcast name; counters are resolved once here.
	pub(crate) fn egress(&self, path: impl AsPath) -> Scope {
		self.scope(path, Side::Publisher)
	}

	/// Ingress (subscriber / writes) scope for a broadcast path.
	pub(crate) fn ingress(&self, path: impl AsPath) -> Scope {
		self.scope(path, Side::Subscriber)
	}

	fn scope(&self, path: impl AsPath, side: Side) -> Scope {
		let Some(inner) = &self.inner else {
			return Scope::default();
		};
		let path = path.as_path().to_owned();
		let counters = inner
			.handle
			.stats
			.entry(&path)
			.map(|entry| entry.tier(&inner.handle.tier));
		Scope {
			session: self.clone(),
			counters,
			side,
			path,
		}
	}

	/// Register one active egress subscription to `path`, returning `true` if it was
	/// the first (so the caller bumps `broadcasts`).
	fn viewer_open(&self, path: &PathOwned) -> bool {
		let Some(inner) = &self.inner else { return false };
		let mut viewers = inner.viewers.lock().expect("stats viewers poisoned");
		let n = viewers.entry(path.clone()).or_insert(0);
		let first = *n == 0;
		*n += 1;
		first
	}

	/// Release one active egress subscription to `path`, returning `true` if it was
	/// the last (so the caller bumps `broadcasts_closed`).
	fn viewer_close(&self, path: &PathOwned) -> bool {
		let Some(inner) = &self.inner else { return false };
		let mut viewers = inner.viewers.lock().expect("stats viewers poisoned");
		match viewers.get_mut(path) {
			Some(n) => {
				*n -= 1;
				if *n == 0 {
					viewers.remove(path);
					true
				} else {
					false
				}
			}
			None => false,
		}
	}
}

impl Drop for SessionInner {
	fn drop(&mut self) {
		if let Some(presence) = &self.presence {
			// Release pairs with the readout's Acquire load of `sessions_closed`
			// (see the module-level "Snapshot atomicity" note).
			presence.sessions_closed.fetch_add(1, Ordering::Release);
		}
	}
}

// ---------------------------------------------------------------------------
// Model-layer carriers
//
// These are what a tagged `origin::{Consumer, Producer}` threads down through the
// derived handles (broadcast -> track -> group -> frame). A tagged origin resolves
// the per-`(path, tier)` counters once into a [`Scope`]; child handles carry a
// cheap [`Meter`] for the payload bumps. All of them are no-ops when empty (a
// disabled registry, an excluded path, or an untagged caller), so an untagged
// handle pays nothing.
// ---------------------------------------------------------------------------

/// Payload bump handle carried by the group and frame model handles. Cheap to
/// clone (an `Option<Arc>` plus a `Side`); empty when the broadcast is untracked.
#[derive(Clone, Default)]
pub(crate) struct Meter {
	counters: Option<Arc<TierCounters>>,
	side: Side,
}

impl Meter {
	fn counters(&self) -> Option<&Counters> {
		self.counters.as_ref().map(|c| self.side.counters(c))
	}

	/// Bump `groups` once (a group delivered/consumed on this side).
	pub(crate) fn group(&self) {
		if let Some(counters) = self.counters() {
			counters.groups.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// Bump `frames` by `n`.
	pub(crate) fn frames(&self, n: u64) {
		if n == 0 {
			return;
		}
		if let Some(counters) = self.counters() {
			counters.frames.fetch_add(n, Ordering::Relaxed);
		}
	}

	/// Record one datagram of `n` payload bytes. A datagram stands in for the
	/// single-frame group it replaces, so this bumps `groups`, `frames`, and
	/// `bytes` alongside `datagrams`.
	pub(crate) fn datagram(&self, n: u64) {
		if let Some(counters) = self.counters() {
			counters.datagrams.fetch_add(1, Ordering::Relaxed);
			counters.groups.fetch_add(1, Ordering::Relaxed);
			counters.frames.fetch_add(1, Ordering::Relaxed);
			counters.bytes.fetch_add(n, Ordering::Relaxed);
		}
	}

	/// Bump `bytes` by `n`.
	pub(crate) fn bytes(&self, n: u64) {
		if n == 0 {
			return;
		}
		if let Some(counters) = self.counters() {
			counters.bytes.fetch_add(n, Ordering::Relaxed);
		}
	}
}

/// A per-`(broadcast, tier, side)` scope, carried by the broadcast and track model
/// handles. Resolved once by a tagged origin at the broadcast handoff; hands out
/// [`Meter`]s for the payload path and RAII guards for the subscription / announce
/// lifecycle. Cheap to clone; empty (no-op) when the broadcast is untracked.
#[derive(Clone, Default)]
pub(crate) struct Scope {
	/// The owning context, kept for the egress viewer refcount map.
	session: Session,
	/// Resolved counters for `(path, tier)`, or `None` when untracked.
	counters: Option<Arc<TierCounters>>,
	side: Side,
	/// Absolute broadcast path, used to key the viewer refcount and as the
	/// `announced_bytes` length.
	path: PathOwned,
}

impl Scope {
	fn counters(&self) -> Option<&Counters> {
		self.counters.as_ref().map(|c| self.side.counters(c))
	}

	/// A payload [`Meter`] for a group/frame derived from this scope.
	pub(crate) fn meter(&self) -> Meter {
		Meter {
			counters: self.counters.clone(),
			side: self.side,
		}
	}

	/// Open a track-subscription guard: bumps `subscriptions` now and
	/// `subscriptions_closed` on drop. On the egress (publisher) side it also drives
	/// the context's viewer refcount (`broadcasts` / `broadcasts_closed`).
	pub(crate) fn subscribe(&self) -> Subscription {
		if let Some(counters) = self.counters() {
			counters.subscriptions.fetch_add(1, Ordering::Relaxed);
		}
		// Viewer refcount is egress-only: `broadcasts` counts distinct sessions
		// watching a broadcast.
		let viewer = if matches!(self.side, Side::Publisher) && self.counters.is_some() {
			if self.session.viewer_open(&self.path)
				&& let Some(counters) = self.counters()
			{
				counters.broadcasts.fetch_add(1, Ordering::Relaxed);
			}
			Some((self.session.clone(), self.path.clone()))
		} else {
			None
		};
		Subscription {
			counters: self.counters.clone(),
			side: self.side,
			viewer,
		}
	}

	/// Bump the `fetches` counter once (a coalesced group fetch served).
	pub(crate) fn fetch(&self) {
		if let Some(counters) = self.counters() {
			counters.fetches.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// Bump `subscriptions` once. The ingress (producer-lifetime) counterpart to
	/// [`Self::subscribe`], which cannot use an RAII guard because the producer is
	/// cloneable and closes only on the last clone drop. No viewer refcount (that is
	/// egress-only). Pair with [`Self::close_subscription`].
	pub(crate) fn open_subscription(&self) {
		if let Some(counters) = self.counters() {
			counters.subscriptions.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// Bump `subscriptions_closed` once. See [`Self::open_subscription`].
	pub(crate) fn close_subscription(&self) {
		if let Some(counters) = self.counters() {
			// Release pairs with the readout's Acquire load of `subscriptions_closed`.
			counters.subscriptions_closed.fetch_add(1, Ordering::Release);
		}
	}

	/// Open an announce guard: bumps `announced` and adds the path length to
	/// `announced_bytes` now; on drop bumps `announced_closed` and adds the path
	/// length again. Used for egress announce-stream events and ingress
	/// route-transition (un)announces.
	pub(crate) fn announce(&self) -> Announce {
		let len = self.path.as_str().len() as u64;
		if let Some(counters) = self.counters() {
			counters.announced.fetch_add(1, Ordering::Relaxed);
			counters.announced_bytes.fetch_add(len, Ordering::Relaxed);
		}
		Announce {
			counters: self.counters.clone(),
			side: self.side,
			len,
		}
	}
}

/// RAII guard for a track subscription (either side). See [`Scope::subscribe`].
/// [`Subscription::default`] is an empty no-op guard.
#[derive(Default)]
#[must_use = "drop the guard to record the subscription as closed"]
pub(crate) struct Subscription {
	counters: Option<Arc<TierCounters>>,
	side: Side,
	/// `Some((session, path))` on the egress side, to release the viewer refcount.
	viewer: Option<(Session, PathOwned)>,
}

impl Drop for Subscription {
	fn drop(&mut self) {
		if let Some((session, path)) = &self.viewer
			&& session.viewer_close(path)
			&& let Some(counters) = &self.counters
		{
			// Release pairs with the readout's Acquire load of `broadcasts_closed`.
			self.side
				.counters(counters)
				.broadcasts_closed
				.fetch_add(1, Ordering::Release);
		}
		if let Some(counters) = &self.counters {
			// Release pairs with the readout's Acquire load of `subscriptions_closed`.
			self.side
				.counters(counters)
				.subscriptions_closed
				.fetch_add(1, Ordering::Release);
		}
	}
}

/// RAII guard for one announce lifetime. See [`Scope::announce`].
#[must_use = "drop the guard to record the unannounce"]
pub(crate) struct Announce {
	counters: Option<Arc<TierCounters>>,
	side: Side,
	len: u64,
}

impl Drop for Announce {
	fn drop(&mut self) {
		if let Some(counters) = &self.counters {
			let counters = self.side.counters(counters);
			counters.announced_bytes.fetch_add(self.len, Ordering::Relaxed);
			// Release pairs with the readout's Acquire load of `announced_closed`.
			counters.announced_closed.fetch_add(1, Ordering::Release);
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
	fn default_and_named_tiers_are_independent() {
		let stats = test_stats();
		let default = stats.tier(Tier::default()).session("root");
		let regional = stats.tier(Tier::new("region/sjc")).session("root");

		default.egress("demo/bbb").meter().bytes(100);
		regional.ingress("demo/bbb").meter().bytes(7);

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

		// Two default-tier sessions under one root, one regional; presence sums them.
		let s1 = default.session("acme");
		let _s2 = default.session("acme");
		let s3 = regional.session("peer");

		// Default-tier egress across two broadcasts; the snapshot sums them.
		{
			let m = s1.egress("demo/aaa").meter();
			m.bytes(100);
			m.frames(1);
			m.group();
		}
		s1.egress("demo/bbb").meter().bytes(50);
		// Regional ingress on a different tier/role stays isolated.
		s3.ingress("demo/aaa").meter().bytes(7);

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
		let ctx = stats.tier(Tier::default()).session("root");
		let scope = ctx.egress("foo/bar");
		let sub = scope.subscribe();
		scope.meter().bytes(42);

		let report = stats.report();
		let row = report
			.traffic
			.iter()
			.find(|row| row.path == key)
			.expect("live entry present");
		assert_eq!(row.publisher.bytes, 42);
		assert_eq!(row.publisher.subscriptions, 1);
		assert!(!row.publisher.is_idle(), "subscription guard still open");
		assert!(
			stats.shared().entries.lock().contains_key(&key),
			"live entry kept across drains"
		);

		drop(sub);
		drop(scope);

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
		let ctx = stats.tier(Tier::default()).session("root");
		let scope = ctx.egress("foo/bar");
		let guard = scope.announce();

		for _ in 0..3 {
			let report = stats.report();
			assert!(
				report.traffic.iter().any(|row| row.path == key),
				"announced-but-idle broadcast stays while the guard is held"
			);
		}

		drop(guard);
		drop(scope);
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
	fn paths_under_exclude_are_no_op() {
		// Our own stats broadcasts (and any sibling category under the same
		// prefix) must not feed back into the registry.
		let stats = test_stats();
		let ctx = stats.tier(Tier::default()).session("root");
		let scope = ctx.egress(".stats/node/sjc");
		scope.meter().bytes(100);
		let _guard = scope.announce();
		let _sub = scope.subscribe();
		assert!(stats.shared().entries.lock().is_empty());
	}

	#[test]
	fn disabled_stats_are_noop() {
		// A disabled registry allocates no shared state; every handle is empty
		// and bumps are dropped.
		let stats = Registry::default();
		assert!(stats.shared.is_none());
		let ctx = stats.tier(Tier::default()).session("root");
		let scope = ctx.egress("demo/bbb");
		scope.meter().bytes(100);
		let _guard = scope.announce();
		let _sub = scope.subscribe();
		assert!(stats.report().traffic.is_empty());
		assert!(stats.snapshot().traffic().is_empty());
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
	fn context_presence_closes_on_last_clone() {
		// The reshaped Session context bumps `sessions` once at creation and
		// `sessions_closed` only when the last clone drops.
		let stats = test_stats();
		let snap =
			|root: &str| session_snapshot(&stats, &Tier::default(), root).map(|p| (p.sessions, p.sessions_closed));

		let ctx = stats.tier(Tier::default()).session("acme");
		assert_eq!(snap("acme"), Some((1, 0)));

		let clone = ctx.clone();
		// A clone shares the Arc: no extra `sessions`, and dropping one does nothing.
		assert_eq!(snap("acme"), Some((1, 0)));
		drop(ctx);
		assert_eq!(snap("acme"), Some((1, 0)));
		drop(clone);
		assert_eq!(snap("acme"), Some((1, 1)));
	}

	#[test]
	fn meter_bumps_the_right_side() {
		// A payload meter records on its own side only.
		let stats = test_stats();
		let ctx = stats.tier(Tier::default()).session("root");

		let egress = ctx.egress("demo/bbb").meter();
		egress.group();
		egress.frames(3);
		egress.bytes(100);

		let ingress = ctx.ingress("demo/bbb").meter();
		ingress.group();
		ingress.frames(1);
		ingress.bytes(7);

		let counters = tier_counters(&stats, "demo/bbb", &Tier::default());
		let pub_ = counters.publisher.snapshot();
		let sub = counters.subscriber.snapshot();
		assert_eq!((pub_.groups, pub_.frames, pub_.bytes), (1, 3, 100));
		assert_eq!((sub.groups, sub.frames, sub.bytes), (1, 1, 7));
	}

	#[test]
	fn egress_subscribe_drives_subscriptions_and_viewers() {
		// An egress subscription bumps `subscriptions` and, being the context's first
		// for the broadcast, `broadcasts`. Dropping closes both.
		let stats = test_stats();
		let ctx = stats.tier(Tier::default()).session("root");
		let raw = || tier_counters(&stats, "demo/bbb", &Tier::default()).publisher.snapshot();

		let scope = ctx.egress("demo/bbb");
		let s1 = scope.subscribe();
		let s2 = scope.subscribe();
		let r = raw();
		assert_eq!(r.subscriptions, 2, "two track subs");
		assert_eq!(r.broadcasts, 1, "one context => one viewer");
		assert_eq!(r.broadcasts_closed, 0);

		drop(s1);
		assert_eq!(raw().broadcasts_closed, 0, "context still has a sub open");
		drop(s2);
		let r = raw();
		assert_eq!(r.subscriptions_closed, 2);
		assert_eq!(r.broadcasts_closed, 1, "last sub closed => one broadcasts_closed");
	}

	#[test]
	fn distinct_contexts_are_distinct_viewers() {
		// Two contexts (sessions) subscribing to the same broadcast are two viewers.
		let stats = test_stats();
		let raw = || tier_counters(&stats, "demo/bbb", &Tier::default()).publisher.snapshot();

		let v1 = stats.tier(Tier::default()).session("a").egress("demo/bbb").subscribe();
		assert_eq!(raw().broadcasts, 1);
		let v2 = stats.tier(Tier::default()).session("b").egress("demo/bbb").subscribe();
		assert_eq!(raw().broadcasts, 2, "two distinct contexts => two viewers");

		drop(v1);
		assert_eq!(raw().active_broadcasts(), 1);
		drop(v2);
		assert_eq!(raw().broadcasts_closed, 2);
	}

	#[test]
	fn ingress_subscription_has_no_viewer() {
		// The ingress (producer-lifetime) subscription pair bumps subscriptions but
		// never the viewer refcount.
		let stats = test_stats();
		let ctx = stats.tier(Tier::default()).session("root");
		let scope = ctx.ingress("demo/bbb");
		scope.open_subscription();
		let sub = tier_counters(&stats, "demo/bbb", &Tier::default())
			.subscriber
			.snapshot();
		assert_eq!(sub.subscriptions, 1);
		assert_eq!(sub.broadcasts, 0, "ingress has no viewer refcount");
		scope.close_subscription();
		assert_eq!(
			tier_counters(&stats, "demo/bbb", &Tier::default())
				.subscriber
				.snapshot()
				.subscriptions_closed,
			1
		);
	}

	#[test]
	fn fetch_counts_separately_from_subscriptions() {
		// A fetch bumps `fetches`, not `subscriptions` or the viewer refcount.
		let stats = test_stats();
		let ctx = stats.tier(Tier::default()).session("root");
		let scope = ctx.egress("demo/bbb");
		scope.fetch();
		scope.fetch();
		let r = tier_counters(&stats, "demo/bbb", &Tier::default()).publisher.snapshot();
		assert_eq!(r.fetches, 2);
		assert_eq!(r.subscriptions, 0);
		assert_eq!(r.broadcasts, 0);
	}

	#[test]
	fn announce_guard_records_bytes_on_open_and_close() {
		// The announce guard bumps `announced` + the path length on open, and
		// `announced_closed` + the path length again on drop.
		let stats = test_stats();
		let ctx = stats.tier(Tier::default()).session("root");
		let path_len = "demo/bbb".len() as u64;

		let guard = ctx.egress("demo/bbb").announce();
		let r = tier_counters(&stats, "demo/bbb", &Tier::default()).publisher.snapshot();
		assert_eq!(r.announced, 1);
		assert_eq!(r.announced_closed, 0);
		assert_eq!(r.announced_bytes, path_len);

		drop(guard);
		let r = tier_counters(&stats, "demo/bbb", &Tier::default()).publisher.snapshot();
		assert_eq!(r.announced_closed, 1);
		assert_eq!(
			r.announced_bytes,
			path_len * 2,
			"path length recorded on open and close"
		);
	}

	#[test]
	fn disabled_context_is_noop() {
		// A default (disabled) context resolves empty scopes: every bump is dropped.
		let ctx = Session::default();
		let scope = ctx.egress("demo/bbb");
		scope.meter().bytes(100);
		let _guard = scope.announce();
		let _sub = scope.subscribe();
		scope.fetch();
		// No registry to inspect; the point is that none of this panics or allocates.
		assert!(ctx.inner.is_none());
	}

	#[test]
	fn fetches_serde_roundtrips() {
		// The new `fetches` field is additive: an older frame omits it (defaults to
		// zero), and it survives a roundtrip.
		let old: Traffic = serde_json::from_str(r#"{"bytes":5}"#).expect("older shape parses");
		assert_eq!(old.fetches, 0);

		let t = Traffic {
			fetches: 9,
			..Default::default()
		};
		let json = serde_json::to_string(&t).unwrap();
		let back: Traffic = serde_json::from_str(&json).unwrap();
		assert_eq!(back.fetches, 9);
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
