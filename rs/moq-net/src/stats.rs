//! Generic stats publishing for moq-net sessions.
//!
//! [`Stats`] aggregates per-broadcast counter bumps for traffic this relay
//! node is handling and publishes them on a single `<prefix>/node/<node>`
//! broadcast (or `<prefix>/node` when no node is configured). The broadcast
//! carries four tracks, one per `(tier, role)` pair:
//!
//! * `publisher.json`           : external (e.g. customer) egress
//! * `subscriber.json`          : external ingress
//! * `internal/publisher.json`  : internal (e.g. mTLS cluster peer) egress
//! * `internal/subscriber.json` : internal ingress
//!
//! Each frame is a JSON object mapping broadcast path to a cumulative
//! counter snapshot. Tier, role, and node are implied by the track and
//! broadcast paths, so they aren't repeated inside the frame. An entry
//! appears in the frame for a given `(tier, role)` on any tick where the
//! broadcast is live (any open counter still exceeds its `*_closed`
//! counterpart, so a subscription could begin at any moment) or its
//! snapshot changed since the previous tick. Once every counter equals its
//! `*_closed` counterpart no traffic can flow, so the entry is dropped. A
//! downstream aggregator computes rates from successive cumulative
//! snapshots and slices the data however a dashboard wants.
//!
//! Per-snapshot semantics:
//!
//! * `announced` / `announced_closed`: cumulative count of broadcast
//!   announce/unannounce events on this `(tier, role)`. Bumped on every
//!   `publisher()` / `subscriber()` guard creation and drop.
//! * `broadcasts` / `broadcasts_closed`: derived by the snapshot task from
//!   subscription transitions. `broadcasts` bumps each tick the slot
//!   transitions from "no active subs" to "one or more active subs" (or
//!   when subs flickered through 0 within a tick). `broadcasts_closed`
//!   bumps on the reverse transition. Use these for "active broadcasts"
//!   billing/UI metrics; use `announced` if you want all broadcasts ever
//!   seen.
//! * `subscriptions` / `subscriptions_closed`: cumulative count of
//!   track-level subscription guards opened/dropped.
//! * `bytes` / `frames` / `groups`: cumulative payload counters bumped from
//!   the lite session loops.
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

use std::{
	collections::{BTreeMap, HashMap},
	sync::{
		Arc, Weak,
		atomic::{AtomicU64, Ordering},
	},
	time::Duration,
};

use serde::Serialize;
use web_async::{Lock, spawn};

use crate::{AsPath, Broadcast, OriginProducer, Path, PathOwned, Track, TrackProducer};

/// Cumulative atomic counters for a single `(tier, role)` on a broadcast.
///
/// Only `announced` / `announced_closed` and `subscriptions` /
/// `subscriptions_closed` (and the payload counters) are bumped from the
/// hot bump path. `broadcasts` / `broadcasts_closed` are not stored here;
/// they're derived in the snapshot task from observed subscription
/// transitions.
#[derive(Default, Debug)]
#[non_exhaustive]
pub struct Counters {
	pub announced: AtomicU64,
	pub announced_closed: AtomicU64,
	pub subscriptions: AtomicU64,
	pub subscriptions_closed: AtomicU64,
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
		let announced = self.announced.load(Ordering::Relaxed);
		let subscriptions = self.subscriptions.load(Ordering::Relaxed);
		let bytes = self.bytes.load(Ordering::Relaxed);
		let frames = self.frames.load(Ordering::Relaxed);
		let groups = self.groups.load(Ordering::Relaxed);
		RawCounts {
			announced,
			announced_closed,
			subscriptions,
			subscriptions_closed,
			bytes,
			frames,
			groups,
		}
	}
}

/// Raw counter readout, before the snapshot task layers on derived
/// `broadcasts` / `broadcasts_closed`. Intermediate type that doesn't
/// escape this module.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct RawCounts {
	announced: u64,
	announced_closed: u64,
	subscriptions: u64,
	subscriptions_closed: u64,
	bytes: u64,
	frames: u64,
	groups: u64,
}

/// Distinguishes traffic classes so a single [`Stats`] can record
/// customer-facing and cluster-peer traffic separately. Each tracked
/// broadcast keeps per-tier [`Counters`] on both its publisher and
/// subscriber sides.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Tier {
	External,
	Internal,
}

impl Tier {
	fn idx(self) -> usize {
		match self {
			Tier::External => 0,
			Tier::Internal => 1,
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
	pub origin: Option<OriginProducer>,
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
		}
	}

	/// Set the origin to publish the stats broadcast on. Without this the
	/// aggregator is a no-op.
	pub fn with_origin(mut self, origin: impl Into<Option<OriginProducer>>) -> Self {
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
	origin: OriginProducer,
	entries: Lock<HashMap<PathOwned, Arc<BroadcastEntry>>>,
}

/// Per-broadcast counters split by side then tier. The two side fields are
/// named explicitly (rather than indexed by some `Role` enum) because the
/// bump-path call sites always know which side they're on at compile time;
/// only the tier varies dynamically with the session.
struct BroadcastEntry {
	publisher: [Counters; 2],
	subscriber: [Counters; 2],
}

impl BroadcastEntry {
	fn new() -> Self {
		Self {
			publisher: Default::default(),
			subscriber: Default::default(),
		}
	}
}

/// Per-(entry, slot) state owned by the snapshot task. The snapshot task
/// is single-threaded so this needs no atomics; we keep one of these per
/// `(path, side, tier)` in a task-local map, mirroring the structure of
/// [`BroadcastEntry`].
#[derive(Default)]
struct SlotState {
	/// Cumulative count of `inactive -> active` subscription transitions on
	/// this slot since the snapshot task started. Resets to 0 when the
	/// entry is GC'd from the local map (consumers must treat decreases as
	/// a session restart).
	derived_broadcasts: u64,
	/// Cumulative count of `active -> inactive` transitions.
	derived_broadcasts_closed: u64,
	/// Last `Snapshot` we wrote to the frame for this slot, used to detect
	/// changes that warrant re-emission and to derive `broadcasts` transitions.
	prev_emitted: Option<Snapshot>,
}

/// Snapshot-task-local mirror of [`BroadcastEntry`]: per-side, per-tier
/// `SlotState`. Same field layout so iteration in the snapshot loop is
/// trivially parallel between the two.
#[derive(Default)]
struct EntrySnapState {
	publisher: [SlotState; 2],
	subscriber: [SlotState; 2],
}

impl EntrySnapState {
	/// Iterate the four `(track_name, counters, slot_state)` slots in the
	/// fixed order matching `TRACK_ORDER`.
	fn zip_slots<'a>(&'a mut self, entry: &'a BroadcastEntry) -> [(&'static str, &'a Counters, &'a mut SlotState); 4] {
		let [pub_ext_state, pub_int_state] = &mut self.publisher;
		let [sub_ext_state, sub_int_state] = &mut self.subscriber;
		[
			("publisher.json", &entry.publisher[Tier::External.idx()], pub_ext_state),
			(
				"subscriber.json",
				&entry.subscriber[Tier::External.idx()],
				sub_ext_state,
			),
			(
				"internal/publisher.json",
				&entry.publisher[Tier::Internal.idx()],
				pub_int_state,
			),
			(
				"internal/subscriber.json",
				&entry.subscriber[Tier::Internal.idx()],
				sub_int_state,
			),
		]
	}
}

/// Number of `(side, tier)` slots, matching the four tracks per stats
/// broadcast.
const NUM_SLOTS: usize = 4;

/// Track names in the same order [`EntrySnapState::zip_slots`] returns
/// them. Used to construct the per-broadcast track set up front.
const TRACK_ORDER: [&str; NUM_SLOTS] = [
	"publisher.json",
	"subscriber.json",
	"internal/publisher.json",
	"internal/subscriber.json",
];

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
			});
			let advertised = advertised_path(&prefix, node.as_ref().map(|p| p.as_str()));
			spawn(run_publisher(Arc::downgrade(&shared), advertised, interval));
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
	pub fn tier(&self) -> Tier {
		self.tier
	}

	/// Returns a per-broadcast handle scoped to this tier.
	///
	/// Paths under the aggregator's configured `prefix` return an empty handle
	/// whose bumps are no-ops. This keeps stats traffic from feeding back into
	/// the aggregator.
	pub fn broadcast(&self, path: impl AsPath) -> BroadcastStats {
		BroadcastStats {
			entry: self.stats.entry(path),
			tier: self.tier,
		}
	}
}

impl Default for StatsHandle {
	/// A no-op handle backed by a [`Stats::default`] aggregator.
	fn default() -> Self {
		Stats::default().tier(Tier::External)
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
	entry: Option<Arc<BroadcastEntry>>,
	tier: Tier,
}

impl BroadcastStats {
	/// True if this handle has no underlying entry (path was under the
	/// aggregator's own prefix, or stats are disabled). All bumps through an
	/// empty handle are no-ops.
	pub fn is_empty(&self) -> bool {
		self.entry.is_none()
	}

	/// Open a broadcast-lifetime guard for the publisher (egress) role.
	/// Bumps `announced` on construction and `announced_closed` on drop.
	/// (The emitted `broadcasts` counter is derived in the snapshot task
	/// from subscription activity; see the module docs.)
	pub fn publisher(&self) -> PublisherStats {
		if let Some(entry) = &self.entry {
			entry.publisher[self.tier.idx()]
				.announced
				.fetch_add(1, Ordering::Relaxed);
		}
		PublisherStats {
			entry: self.entry.clone(),
			tier: self.tier,
		}
	}

	/// Open a broadcast-lifetime guard for the subscriber (ingress) role.
	/// Bumps `announced` on construction and `announced_closed` on drop.
	/// (The emitted `broadcasts` counter is derived in the snapshot task
	/// from subscription activity; see the module docs.)
	pub fn subscriber(&self) -> SubscriberStats {
		if let Some(entry) = &self.entry {
			entry.subscriber[self.tier.idx()]
				.announced
				.fetch_add(1, Ordering::Relaxed);
		}
		SubscriberStats {
			entry: self.entry.clone(),
			tier: self.tier,
		}
	}

	/// Open a publisher-track guard.
	///
	/// `_name` is unused; counters are per-broadcast only. The track name
	/// parameter is kept for symmetry with the rest of moq-net so callers
	/// don't have to thread an `Option<&str>` through subscribe sites.
	pub fn publisher_track(&self, _name: &str) -> PublisherTrack {
		if let Some(entry) = &self.entry {
			entry.publisher[self.tier.idx()]
				.subscriptions
				.fetch_add(1, Ordering::Relaxed);
		}
		PublisherTrack {
			entry: self.entry.clone(),
			tier: self.tier,
		}
	}

	/// Subscriber-side counterpart to [`Self::publisher_track`].
	pub fn subscriber_track(&self, _name: &str) -> SubscriberTrack {
		if let Some(entry) = &self.entry {
			entry.subscriber[self.tier.idx()]
				.subscriptions
				.fetch_add(1, Ordering::Relaxed);
		}
		SubscriberTrack {
			entry: self.entry.clone(),
			tier: self.tier,
		}
	}
}

/// RAII broadcast guard for the publisher role. See [`BroadcastStats::publisher`].
#[must_use = "drop the guard to record the broadcast as closed"]
pub struct PublisherStats {
	entry: Option<Arc<BroadcastEntry>>,
	tier: Tier,
}

impl PublisherStats {
	/// Open a track-subscription guard. Bumps `subscriptions` on construction
	/// and `subscriptions_closed` on drop.
	pub fn track(&self, name: &str) -> PublisherTrack {
		BroadcastStats {
			entry: self.entry.clone(),
			tier: self.tier,
		}
		.publisher_track(name)
	}
}

impl Drop for PublisherStats {
	fn drop(&mut self) {
		if let Some(entry) = &self.entry {
			// Release pairs with the snapshot reader's Acquire load of
			// `announced_closed`, propagating the open-bump from this
			// guard's construction to whichever thread observes the close.
			entry.publisher[self.tier.idx()]
				.announced_closed
				.fetch_add(1, Ordering::Release);
		}
	}
}

/// RAII broadcast guard for the subscriber role. See [`BroadcastStats::subscriber`].
#[must_use = "drop the guard to record the broadcast as closed"]
pub struct SubscriberStats {
	entry: Option<Arc<BroadcastEntry>>,
	tier: Tier,
}

impl SubscriberStats {
	/// Open a track-subscription guard. Mirrors [`PublisherStats::track`].
	pub fn track(&self, name: &str) -> SubscriberTrack {
		BroadcastStats {
			entry: self.entry.clone(),
			tier: self.tier,
		}
		.subscriber_track(name)
	}
}

impl Drop for SubscriberStats {
	fn drop(&mut self) {
		if let Some(entry) = &self.entry {
			// See `PublisherStats::drop` for why this is Release.
			entry.subscriber[self.tier.idx()]
				.announced_closed
				.fetch_add(1, Ordering::Release);
		}
	}
}

/// RAII subscription guard for the publisher role.
#[must_use = "drop the guard to record the subscription as closed"]
pub struct PublisherTrack {
	entry: Option<Arc<BroadcastEntry>>,
	tier: Tier,
}

impl PublisherTrack {
	/// Bumps `frames` once.
	pub fn frame(&self) {
		if let Some(entry) = &self.entry {
			entry.publisher[self.tier.idx()].frames.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// Bumps `bytes` by `n`.
	pub fn bytes(&self, n: u64) {
		if let Some(entry) = &self.entry {
			entry.publisher[self.tier.idx()].bytes.fetch_add(n, Ordering::Relaxed);
		}
	}

	/// Bumps `groups` once.
	pub fn group(&self) {
		if let Some(entry) = &self.entry {
			entry.publisher[self.tier.idx()].groups.fetch_add(1, Ordering::Relaxed);
		}
	}
}

impl Drop for PublisherTrack {
	fn drop(&mut self) {
		if let Some(entry) = &self.entry {
			// See `PublisherStats::drop` for why this is Release.
			entry.publisher[self.tier.idx()]
				.subscriptions_closed
				.fetch_add(1, Ordering::Release);
		}
	}
}

/// RAII subscription guard for the subscriber role.
#[must_use = "drop the guard to record the subscription as closed"]
pub struct SubscriberTrack {
	entry: Option<Arc<BroadcastEntry>>,
	tier: Tier,
}

impl SubscriberTrack {
	/// Bumps `frames` once.
	pub fn frame(&self) {
		if let Some(entry) = &self.entry {
			entry.subscriber[self.tier.idx()].frames.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// Bumps `bytes` by `n`.
	pub fn bytes(&self, n: u64) {
		if let Some(entry) = &self.entry {
			entry.subscriber[self.tier.idx()].bytes.fetch_add(n, Ordering::Relaxed);
		}
	}

	/// Bumps `groups` once.
	pub fn group(&self) {
		if let Some(entry) = &self.entry {
			entry.subscriber[self.tier.idx()].groups.fetch_add(1, Ordering::Relaxed);
		}
	}
}

impl Drop for SubscriberTrack {
	fn drop(&mut self) {
		if let Some(entry) = &self.entry {
			// See `PublisherStats::drop` for why this is Release.
			entry.subscriber[self.tier.idx()]
				.subscriptions_closed
				.fetch_add(1, Ordering::Release);
		}
	}
}

/// Per-tick work for a single `(side, tier)` slot: derive `broadcasts` /
/// `broadcasts_closed` from subscription transitions, build the emitted
/// `Snapshot`, update the slot's `prev_emitted`, and hand the snap to `emit`
/// iff the slot is live or changed this tick.
fn process_slot(counters: &Counters, slot_state: &mut SlotState, mut emit: impl FnMut(Snapshot)) {
	let raw = counters.snapshot();

	// Derive `broadcasts` / `broadcasts_closed` from subscription-active
	// transitions observed across ticks. `delta_subs > 0` catches the case
	// where a sub opened AND closed within a single tick window so the
	// snapshot shows `subs == subs_closed` (curr_active false) but real
	// activity happened. Such flickers count as a full open/close pair.
	let (prev_subs, prev_subs_closed, prev_broadcasts, prev_broadcasts_closed) = match &slot_state.prev_emitted {
		Some(prev) => (
			prev.subscriptions,
			prev.subscriptions_closed,
			prev.broadcasts,
			prev.broadcasts_closed,
		),
		None => (0, 0, 0, 0),
	};
	let prev_active = prev_subs > prev_subs_closed;
	let curr_active = raw.subscriptions > raw.subscriptions_closed;
	let delta_subs = raw.subscriptions.saturating_sub(prev_subs);
	let active_during = prev_active || curr_active || delta_subs > 0;

	if !prev_active && active_during {
		slot_state.derived_broadcasts = prev_broadcasts.saturating_add(1);
	}
	if active_during && !curr_active {
		slot_state.derived_broadcasts_closed = prev_broadcasts_closed.saturating_add(1);
	}

	let snap = Snapshot {
		announced: raw.announced,
		announced_closed: raw.announced_closed,
		broadcasts: slot_state.derived_broadcasts,
		broadcasts_closed: slot_state.derived_broadcasts_closed,
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

/// Publishes the stats broadcast and writes a frame per tick. Spawned once by
/// [`Stats::new`] when an origin is set; runs until every [`Stats`] clone is
/// dropped (`weak.upgrade()` returns `None`).
async fn run_publisher(weak: Weak<StatsShared>, advertised: PathOwned, interval: Duration) {
	let Some(shared) = weak.upgrade() else {
		return;
	};

	let mut broadcast = Broadcast::new().produce();
	let mut tracks: Vec<TrackProducer> = Vec::with_capacity(NUM_SLOTS);
	for name in TRACK_ORDER {
		match broadcast.create_track(Track::new(name)) {
			Ok(t) => tracks.push(t),
			Err(err) => {
				tracing::warn!(?err, name, "stats: failed to create track");
				return;
			}
		}
	}
	if !shared.origin.publish_broadcast(&advertised, broadcast.consume()) {
		tracing::warn!(advertised = %advertised, "stats: origin rejected stats broadcast");
		return;
	}
	drop(shared);

	// Per-path snapshot state owned by this task. Mirrors the global entries
	// and serves as the diff source for change detection and `broadcasts`
	// derivation across ticks.
	let mut local: HashMap<PathOwned, EntrySnapState> = HashMap::new();
	let mut last_payload: [Vec<u8>; NUM_SLOTS] = Default::default();

	let mut ticker = tokio::time::interval(interval);
	ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

	loop {
		ticker.tick().await;

		let Some(shared) = weak.upgrade() else {
			return;
		};

		// Clone the current entries map into a Vec so we can drop the
		// global lock before the change-detection pass.
		let entries: Vec<(PathOwned, Arc<BroadcastEntry>)> = {
			let map = shared.entries.lock();
			map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
		};

		let mut frames: [BTreeMap<String, Snapshot>; NUM_SLOTS] = Default::default();
		for (path, entry) in &entries {
			let snap_state = local.entry(path.clone()).or_default();
			for (i, (_track_name, counters, slot_state)) in snap_state.zip_slots(entry).into_iter().enumerate() {
				process_slot(counters, slot_state, |snap| {
					frames[i].insert(path.as_str().to_string(), snap);
				});
			}
		}
		drop(entries);

		// GC global entries: keep only those an external guard still holds.
		// `strong_count == 1` (just the map's own `Arc`) means no live
		// publisher/subscriber/track guard remains, so every open counter
		// has caught up to its `*_closed` counterpart and no traffic can
		// flow. We can't key this on the counters directly: a held but idle
		// `BroadcastStats` (all counters equal) must stay so a later bump
		// isn't lost on an orphaned `Arc`. Then drop local state for any
		// path that left the map. We already emitted each removed entry's
		// final snapshot above, so nothing is lost.
		{
			let mut map = shared.entries.lock();
			map.retain(|_, entry| Arc::strong_count(entry) > 1);
			local.retain(|path, _| map.contains_key(path));
		}

		for (((frame, last), track), slot) in frames
			.iter()
			.zip(last_payload.iter_mut())
			.zip(tracks.iter_mut())
			.zip(0usize..)
		{
			let json = match serde_json::to_vec(frame) {
				Ok(b) => b,
				Err(err) => {
					tracing::debug!(?err, slot, "stats: failed to serialize frame");
					continue;
				}
			};
			if &json == last {
				continue;
			}
			if let Err(err) = track.write_frame(json.clone()) {
				tracing::debug!(?err, slot, "stats: failed to write frame");
				// Leave `last_payload` untouched so the next tick retries this
				// snapshot instead of skipping it as "already written".
				continue;
			}
			*last = json;
		}

		drop(shared);
	}
}

/// What we emit for one entry on one tier-role track. `announced` /
/// `announced_closed` and the subscription / payload counters come straight
/// from [`RawCounts`]; `broadcasts` / `broadcasts_closed` are derived in
/// the snapshot task from observed subscription transitions.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(serde::Deserialize))]
struct Snapshot {
	announced: u64,
	announced_closed: u64,
	broadcasts: u64,
	broadcasts_closed: u64,
	subscriptions: u64,
	subscriptions_closed: u64,
	bytes: u64,
	frames: u64,
	groups: u64,
}

fn advertised_path(prefix: &Path, node: Option<&str>) -> PathOwned {
	// The fixed `node` category leaves room for sibling categories (e.g.
	// `<top-prefix>/cluster` for relay-mesh stats) under the same prefix.
	let mut out = format!("{}/node", prefix.as_str());
	if let Some(node) = node {
		out.push('/');
		out.push_str(node);
	}
	PathOwned::from(out)
}

#[cfg(test)]
mod tests {
	use std::{collections::BTreeMap, sync::atomic::Ordering::Relaxed};

	use crate::{Origin, Path, Subscription};

	use super::*;

	fn test_stats(node: Option<&str>) -> (Stats, OriginProducer) {
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
		assert_eq!(advertised_path(&prefix, Some("sjc")).as_str(), ".stats/node/sjc");
		assert_eq!(advertised_path(&prefix, Some("sjc/1")).as_str(), ".stats/node/sjc/1");
		assert_eq!(advertised_path(&prefix, None).as_str(), ".stats/node");

		let prefix = Path::new("metrics");
		assert_eq!(advertised_path(&prefix, Some("lon")).as_str(), "metrics/node/lon");
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
		let bs1 = stats.tier(Tier::External).broadcast("demo/bbb");
		let bs2 = stats.tier(Tier::External).broadcast("demo/ccc");
		let g1 = bs1.publisher().track("video");
		g1.bytes(100);
		let g2 = bs2.publisher().track("video");
		g2.bytes(7);

		let entries = stats.shared().entries.lock();
		let e1 = entries.get(&PathOwned::from("demo/bbb")).expect("entry");
		let e2 = entries.get(&PathOwned::from("demo/ccc")).expect("entry");
		assert_eq!(e1.publisher[Tier::External.idx()].bytes.load(Relaxed), 100);
		assert_eq!(e2.publisher[Tier::External.idx()].bytes.load(Relaxed), 7);
	}

	#[tokio::test(start_paused = true)]
	async fn external_and_internal_tiers_are_independent() {
		let (stats, _origin) = test_stats(Some("sjc"));
		let ext = stats.tier(Tier::External);
		let int = stats.tier(Tier::Internal);

		let ext_track = ext.broadcast("demo/bbb").publisher().track("video");
		ext_track.bytes(100);
		let int_track = int.broadcast("demo/bbb").subscriber().track("audio");
		int_track.bytes(7);

		let entries = stats.shared().entries.lock();
		let entry = entries.get(&PathOwned::from("demo/bbb")).expect("entry");
		assert_eq!(entry.publisher[Tier::External.idx()].bytes.load(Relaxed), 100);
		assert_eq!(entry.subscriber[Tier::External.idx()].bytes.load(Relaxed), 0);
		assert_eq!(entry.publisher[Tier::Internal.idx()].bytes.load(Relaxed), 0);
		assert_eq!(entry.subscriber[Tier::Internal.idx()].bytes.load(Relaxed), 7);
	}

	#[tokio::test(start_paused = true)]
	async fn paths_under_prefix_are_no_op() {
		// Our own stats broadcasts (and any sibling category under the same
		// prefix) must not feed back into the aggregator.
		let (stats, _origin) = test_stats(Some("sjc"));
		let bs = stats.tier(Tier::External).broadcast(".stats/node/sjc");
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
		let bs = stats.tier(Tier::External).broadcast("demo/bbb");
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

		let bs1 = stats.tier(Tier::External).broadcast("foo/bar");
		let _t1 = bs1.publisher().track("video");
		let bs2 = stats.tier(Tier::External).broadcast("baz/qux");
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

		let bs = stats.tier(Tier::External).broadcast("foo/bar");
		let _t = bs.publisher().track("video");

		tokio::time::advance(Duration::from_millis(1)).await;
		let (path, event) = consumer.next().await.expect("expected announce");
		assert!(event.broadcast().is_some());
		assert_eq!(path.as_str(), ".stats/node");
	}

	/// Drives the snapshot task forward by `count` ticks. In paused-time
	/// tests, `tokio::time::advance` doesn't poll spawned tasks itself; we
	/// have to combine it with explicit awaits. This helper interleaves
	/// `advance` with `consumer.announced()` (and later `yield_now` calls)
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
		let bs = stats.tier(Tier::External).broadcast("foo/bar");
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
		let bs = stats.tier(Tier::External).broadcast("foo/bar");
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
		let bs = stats.tier(Tier::External).broadcast("foo/bar");
		let track = bs.publisher().track("video");
		track.bytes(42);
		track.frame();

		tokio::time::advance(Duration::from_millis(1100)).await;

		let (_path, event) = consumer.next().await.expect("expected announce");
		let broadcast = event.broadcast().expect("active");
		let track = broadcast
			.consume_track("publisher.json")
			.subscribe(Subscription::default())
			.await
			.expect("subscribe");
		let frame = read_frame(track).await;
		let snap = frame.get("foo/bar").expect("foo/bar entry");
		assert_eq!(snap.announced, 1, "publisher() guard bumps announced");
		assert_eq!(snap.broadcasts, 1, "subs went 0->1, derived broadcasts++");
		assert_eq!(snap.subscriptions, 1);
		assert_eq!(snap.bytes, 42);
		assert_eq!(snap.frames, 1);
	}

	#[tokio::test(start_paused = true)]
	async fn announced_decouples_from_broadcasts() {
		// publisher() with no track subscription should bump announced but
		// NOT broadcasts (which only counts slots with sub activity).
		let (stats, origin) = test_stats(Some("sjc"));
		let mut consumer = origin.consume().announced();
		let bs = stats.tier(Tier::External).broadcast("foo/bar");
		let _guard = bs.publisher();

		tokio::time::advance(Duration::from_millis(1100)).await;

		let (_path, event) = consumer.next().await.expect("announce");
		let broadcast = event.broadcast().expect("active");
		let track = broadcast
			.consume_track("publisher.json")
			.subscribe(Subscription::default())
			.await
			.expect("subscribe");
		let frame = read_frame(track).await;
		let snap = frame.get("foo/bar").expect("foo/bar entry");
		assert_eq!(snap.announced, 1);
		assert_eq!(snap.broadcasts, 0, "no sub, no derived broadcasts");
		assert_eq!(snap.subscriptions, 0);
	}

	#[tokio::test(start_paused = true)]
	async fn short_lived_sub_is_surfaced() {
		// A subscription that opens AND closes within a single tick window
		// must still surface as a complete broadcasts open/close cycle.
		// Before the change-driven inclusion fix, this entry would never
		// have appeared in any frame and would have been GC'd silently.
		let (stats, origin) = test_stats(Some("sjc"));
		let mut consumer = origin.consume().announced();
		let bs = stats.tier(Tier::External).broadcast("foo/bar");
		{
			let track = bs.publisher().track("video");
			track.bytes(123);
			track.frame();
			// track dropped here, all within tick 1
		}

		tokio::time::advance(Duration::from_millis(1100)).await;

		let (_path, event) = consumer.next().await.expect("announce");
		let broadcast = event.broadcast().expect("active");
		let track = broadcast
			.consume_track("publisher.json")
			.subscribe(Subscription::default())
			.await
			.expect("subscribe");
		let frame = read_frame(track).await;
		let snap = frame.get("foo/bar").expect("foo/bar entry");
		// subs went 0->1->0 within the same tick. delta_subs > 0 triggers
		// both broadcasts++ and broadcasts_closed++.
		assert_eq!(snap.subscriptions, 1);
		assert_eq!(snap.subscriptions_closed, 1);
		assert_eq!(snap.broadcasts, 1, "flicker counts as one broadcast");
		assert_eq!(snap.broadcasts_closed, 1);
		assert_eq!(snap.bytes, 123);
		assert_eq!(snap.frames, 1);
	}

	#[tokio::test(start_paused = true)]
	async fn multiple_subs_count_as_one_broadcast() {
		// Two concurrent subs on the same slot should bump broadcasts by 1,
		// not 2. broadcasts is "broadcasts that had >=1 active sub" not
		// "subscription count". And dropping both subs should bump
		// broadcasts_closed by 1 (the 1->0 transition is one event).
		let (stats, _origin) = test_stats(Some("sjc"));
		let bs = stats.tier(Tier::External).broadcast("foo/bar");
		let pub_guard = bs.publisher();
		let t1 = pub_guard.track("video");
		let t2 = pub_guard.track("audio");

		drive_ticks(2).await;
		{
			let entries = stats.shared().entries.lock();
			let entry = entries.get(&PathOwned::from("foo/bar")).expect("entry");
			let raw = entry.publisher[Tier::External.idx()].snapshot();
			assert_eq!(raw.subscriptions, 2, "two track subs");
			assert_eq!(raw.subscriptions_closed, 0, "neither dropped yet");
		}

		// Drop only the track guards; keep `pub_guard` (and `bs`) so the
		// broadcast stays announced (live) and the entry isn't GC'd before
		// we can read the raw counters.
		drop(t1);
		drop(t2);
		drive_ticks(1).await;

		// All subs dropped: subscriptions_closed catches up to subscriptions.
		// The snapshot task observes the 1->0 transition; derived
		// broadcasts_closed (which lives in task-local state, not on the
		// global entry) gets bumped to 1, but we can only see that through
		// the wire frame. Here we just confirm the raw counters squared up.
		let entries = stats.shared().entries.lock();
		let entry = entries
			.get(&PathOwned::from("foo/bar"))
			.expect("entry still live (publisher guard held)");
		let raw = entry.publisher[Tier::External.idx()].snapshot();
		assert_eq!(raw.subscriptions, 2);
		assert_eq!(raw.subscriptions_closed, 2, "both dropped");
	}

	#[tokio::test(start_paused = true)]
	async fn unused_slots_dont_surface() {
		// A broadcast that only sees External Publisher traffic must NOT
		// appear in the other three tracks with zero counters. Regression
		// for the "None != Some(default)" first-tick change-detection bug:
		// without the unwrap_or_default fix, every entry would surface
		// once in every track even when only one slot had real activity.
		let (stats, origin) = test_stats(Some("sjc"));
		let mut consumer = origin.consume().announced();
		let bs = stats.tier(Tier::External).broadcast("foo/bar");
		let track = bs.publisher().track("video");
		track.frame();

		drive_ticks(2).await;

		let (_path, event) = consumer.next().await.expect("announce");
		let broadcast = event.broadcast().expect("active");

		// External publisher slot SHOULD include foo/bar.
		let pub_track = broadcast
			.consume_track("publisher.json")
			.subscribe(Subscription::default())
			.await
			.expect("subscribe");
		assert!(
			read_frame(pub_track).await.contains_key("foo/bar"),
			"publisher.json must include the active foo/bar entry"
		);

		// The other three slots had zero activity. The first frame on
		// each must be `{}`, not `{"foo/bar": {all zeros}}`.
		for name in ["subscriber.json", "internal/publisher.json", "internal/subscriber.json"] {
			let t = broadcast
				.consume_track(name)
				.subscribe(Subscription::default())
				.await
				.expect("subscribe");
			let frame = read_frame(t).await;
			assert!(
				frame.is_empty(),
				"{name} must be empty for an entry with no activity on that slot, got {frame:?}",
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
	}

	async fn read_frame(mut track: crate::TrackSubscriber) -> BTreeMap<String, Snapshot> {
		let bytes = track.read_frame().await.expect("ok").expect("frame");
		serde_json::from_slice(&bytes).expect("json parse")
	}
}
