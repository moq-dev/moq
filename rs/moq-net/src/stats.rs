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
//! appears in the frame for a given `(tier, role)` whenever its snapshot
//! differs from what we last emitted; it then lingers for `retention`
//! intervals past the most recent change so short disconnects don't
//! immediately erase it. A downstream aggregator computes rates from successive
//! cumulative snapshots and slices the data however a dashboard wants.
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
//! [`Stats::disabled`] (and the matching [`Default`] impl) returns a no-op
//! aggregator. All counter bumps through it are silently dropped and no
//! snapshot task is ever spawned, so call sites can hold a [`StatsHandle`]
//! unconditionally instead of threading an `Option`.
//!
//! # Lifecycle
//!
//! No background work runs until something happens worth reporting. The first
//! `broadcast()` call on any path spawns the snapshot task, which constructs
//! the stats broadcast, ticks at the configured interval, and writes a frame
//! per (tier, role) track. The task exits once the entry map has been empty
//! for `2 * retention` intervals, dropping the broadcast and unannouncing. The
//! next `broadcast()` call respawns it.
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

use crate::{AsPath, Broadcast, Origin, OriginProducer, Path, PathOwned, Track, TrackProducer};

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

/// Top-level stats aggregator. Cheap to clone (`Arc` inside). One instance per
/// relay; sessions get tier-scoped handles via [`Stats::tier`].
#[derive(Clone)]
pub struct Stats {
	inner: Arc<StatsInner>,
}

struct StatsInner {
	prefix: PathOwned,
	/// `None` when stats are disabled; otherwise the path the stats broadcast
	/// is published on. Computed once at construction so the snapshot task
	/// doesn't have to recompute it per spawn.
	advertised: Option<PathOwned>,
	origin: OriginProducer,
	interval: Duration,
	retention: u32,
	entries: Lock<HashMap<PathOwned, Arc<BroadcastEntry>>>,
	task: Lock<Option<()>>,
	/// Monotonic tick counter; `0` is a sentinel meaning "no tick has run yet"
	/// so a [`SlotState::last_change_tick == 0`] reliably means "never
	/// observed". Counts up from `1`.
	tick_counter: AtomicU64,
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
#[derive(Default, Clone)]
struct SlotState {
	/// Cumulative count of `inactive -> active` subscription transitions on
	/// this slot since the snapshot task started. Resets to 0 when the
	/// entry is GC'd from the local map (consumers must treat decreases as
	/// a session restart).
	derived_broadcasts: u64,
	/// Cumulative count of `active -> inactive` transitions.
	derived_broadcasts_closed: u64,
	/// Last `Snapshot` we wrote to the frame for this slot, used to detect
	/// changes that warrant re-emission.
	prev_emitted: Option<Snapshot>,
	/// Tick index of the most recent change in `prev_emitted`. `0` means
	/// no change has been observed yet. Drives both the per-slot frame
	/// inclusion (within `retention` of this) and the global entry
	/// GC.
	last_change_tick: u64,
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

	/// Walk all four slot states (read-only). Used by GC.
	fn all_slots(&self) -> impl Iterator<Item = &SlotState> {
		self.publisher.iter().chain(self.subscriber.iter())
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
	/// Build a new stats aggregator.
	///
	/// * `prefix` is the top-level path under which stats are published, e.g.
	///   `.stats`. The full advertised path is `<prefix>/node/<node>` (or
	///   `<prefix>/node` when `node` is `None`).
	/// * `interval` is how long the snapshot task waits between snapshot
	///   publishes.
	/// * `retention` is how many intervals an entry lingers in the emitted
	///   frame after its last observed change, so a short reconnect window
	///   doesn't erase the entry.
	/// * `node` disambiguates broadcasts published by different relays into a
	///   shared cluster origin. Set this on every node in multi-relay
	///   deployments. The value may be multi-segment (e.g. `sjc/1`, `sjc/2`)
	///   so a region with multiple hosts can nest under a shared region key.
	///   `None` (or an empty path after normalization) omits the suffix.
	/// * `origin` is the [`OriginProducer`] that receives `publish_broadcast`
	///   for the stats broadcast.
	pub fn new(
		prefix: impl Into<PathOwned>,
		interval: Duration,
		retention: u32,
		node: impl Into<Option<PathOwned>>,
		origin: OriginProducer,
	) -> Self {
		let prefix = prefix.into();
		// An empty path after normalization is indistinguishable from "no node
		// set"; collapse it so downstream code only sees a single representation.
		let node = node.into().filter(|p| !p.is_empty());
		let advertised = Some(advertised_path(&prefix, node.as_ref().map(|p| p.as_str())));
		Self {
			inner: Arc::new(StatsInner {
				prefix,
				advertised,
				origin,
				interval,
				retention,
				entries: Lock::default(),
				task: Lock::new(None),
				tick_counter: AtomicU64::new(0),
			}),
		}
	}

	/// A no-op aggregator. Counter bumps are silently dropped and no snapshot
	/// task is ever spawned. Use this when stats are disabled so call sites
	/// can hold a [`Stats`] (or [`StatsHandle`]) unconditionally.
	pub fn disabled() -> Self {
		Self {
			inner: Arc::new(StatsInner {
				prefix: PathOwned::default(),
				advertised: None,
				origin: Origin::random().produce(),
				interval: Duration::from_secs(1),
				retention: 0,
				entries: Lock::default(),
				task: Lock::new(None),
				tick_counter: AtomicU64::new(0),
			}),
		}
	}

	/// Returns the configured top-level prefix.
	pub fn prefix(&self) -> &Path<'static> {
		&self.inner.prefix
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
		// Disabled aggregator has no advertised broadcast; never allocate state.
		self.inner.advertised.as_ref()?;
		let path = path.as_path();
		// Skip our own stats broadcasts (and any sibling category under the
		// same prefix) so serving a stats broadcast doesn't generate more
		// stats.
		if path.has_prefix(&self.inner.prefix) {
			return None;
		}
		let owned = path.to_owned();
		let arc = {
			let mut entries = self.inner.entries.lock();
			entries
				.entry(owned)
				.or_insert_with(|| Arc::new(BroadcastEntry::new()))
				.clone()
		};
		ensure_task(self);
		Some(arc)
	}
}

impl Default for Stats {
	fn default() -> Self {
		Self::disabled()
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
	/// A no-op handle. See [`Stats::disabled`].
	pub fn disabled() -> Self {
		Stats::disabled().tier(Tier::External)
	}

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
	fn default() -> Self {
		Self::disabled()
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

fn ensure_task(stats: &Stats) {
	let inner = &stats.inner;
	if inner.advertised.is_none() {
		return;
	}
	let mut slot = inner.task.lock();
	if slot.is_none() {
		*slot = Some(());
		let weak = Arc::downgrade(inner);
		spawn(run_publisher(weak));
	}
}

fn clear_task(inner: &StatsInner) {
	*inner.task.lock() = None;
}

/// True iff any of the supplied slots had a snapshot change within the
/// retention window. Used by both the global-entry GC and the local-state
/// GC so they agree on lifetime.
fn within_retention<'a>(slots: impl IntoIterator<Item = &'a SlotState>, current_tick: u64, retention: u32) -> bool {
	slots
		.into_iter()
		.any(|s| s.last_change_tick != 0 && current_tick.saturating_sub(s.last_change_tick) <= retention as u64)
}

/// Per-tick work for a single `(side, tier)` slot: derive `broadcasts` /
/// `broadcasts_closed` from subscription transitions, build the emitted
/// `Snapshot`, update the slot's `prev_emitted` / `last_change_tick`, and
/// hand the snap to `emit` iff the slot is still within the retention
/// window of its most recent change.
fn process_slot(
	counters: &Counters,
	slot_state: &mut SlotState,
	current_tick: u64,
	retention: u32,
	mut emit: impl FnMut(Snapshot),
) {
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

	// Include the entry whenever the snapshot differs from the last
	// emitted one OR we're still within the retention window of the most
	// recent change. Change-driven inclusion catches bumps since the
	// previous tick (incl. sub-tick flickers); the retention path lets
	// idle entries linger so a downstream "currently active" view doesn't
	// flicker on a single idle tick.
	//
	// `None` (slot never emitted) is treated as the default Snapshot so a
	// first-tick all-zeros snap on an unused tier-side slot doesn't count
	// as a "change". Without this, every entry would surface in all four
	// tracks with zeros on the tick after creation even if only one slot
	// is actually in use.
	let prev_snap = slot_state.prev_emitted.unwrap_or_default();
	let changed = snap != prev_snap;
	if changed {
		slot_state.last_change_tick = current_tick;
		slot_state.prev_emitted = Some(snap);
	}
	if slot_state.last_change_tick != 0 && current_tick.saturating_sub(slot_state.last_change_tick) <= retention as u64
	{
		emit(snap);
	}
}

async fn run_publisher(weak: Weak<StatsInner>) {
	let Some(inner) = weak.upgrade() else {
		return;
	};
	let Some(advertised) = inner.advertised.clone() else {
		clear_task(&inner);
		return;
	};
	let interval = inner.interval;
	let retention = inner.retention;

	let mut broadcast = Broadcast::new().produce();
	let mut tracks: Vec<TrackProducer> = Vec::with_capacity(NUM_SLOTS);
	for name in TRACK_ORDER {
		match broadcast.create_track(Track {
			name: name.into(),
			priority: 0,
			..Default::default()
		}) {
			Ok(t) => tracks.push(t),
			Err(err) => {
				tracing::warn!(?err, name, "stats: failed to create track");
				clear_task(&inner);
				return;
			}
		}
	}
	if !inner.origin.publish_broadcast(&advertised, broadcast.consume()) {
		tracing::warn!(advertised = %advertised, "stats: origin rejected stats broadcast");
		clear_task(&inner);
		return;
	}
	drop(inner);

	// Per-path snapshot state owned by this task. Mirrors entries we've
	// seen recently; serves both as the diff source for change detection
	// and as the authority on which global entries to GC.
	let mut local: HashMap<PathOwned, EntrySnapState> = HashMap::new();
	let mut last_payload: [Vec<u8>; NUM_SLOTS] = Default::default();
	let mut empty_ticks: u32 = 0;

	let mut ticker = tokio::time::interval(interval);
	ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

	loop {
		ticker.tick().await;

		let Some(inner) = weak.upgrade() else {
			return;
		};

		let current_tick = inner.tick_counter.fetch_add(1, Ordering::Relaxed) + 1;

		// Clone the current entries map into a Vec so we can drop the
		// global lock before the change-detection pass.
		let entries: Vec<(PathOwned, Arc<BroadcastEntry>)> = {
			let map = inner.entries.lock();
			map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
		};

		let mut frames: [BTreeMap<String, Snapshot>; NUM_SLOTS] = Default::default();
		for (path, entry) in &entries {
			let snap_state = local.entry(path.clone()).or_default();
			for (i, (_track_name, counters, slot_state)) in snap_state.zip_slots(entry).into_iter().enumerate() {
				process_slot(counters, slot_state, current_tick, retention, |snap| {
					frames[i].insert(path.as_str().to_string(), snap);
				});
			}
		}
		// Build a snapshot of the live path set before dropping `entries`
		// (the Arc clones we hold inflate strong_count for the GC check).
		let live: std::collections::HashSet<PathOwned> = entries.iter().map(|(p, _)| p.clone()).collect();
		drop(entries);

		// GC global entries: keep if any external guard still holds the
		// Arc, OR our local state shows a recent change. Without the
		// `strong_count > 1` check a bump racing with GC could land on an
		// orphaned Arc and be silently lost.
		{
			let mut map = inner.entries.lock();
			map.retain(|path, entry| {
				if Arc::strong_count(entry) > 1 {
					return true;
				}
				local
					.get(path)
					.is_some_and(|snap_state| within_retention(snap_state.all_slots(), current_tick, retention))
			});
		}

		// GC local state: drop entries whose global counterpart is gone
		// AND retention has expired. Bounded growth even if a relay
		// churns through many transient broadcast paths.
		local.retain(|path, snap_state| {
			live.contains(path) || within_retention(snap_state.all_slots(), current_tick, retention)
		});

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

		let map_empty = inner.entries.lock().is_empty();
		if map_empty {
			empty_ticks = empty_ticks.saturating_add(1);
			// Once the map has been empty long enough that no consumer could
			// learn anything new, drop the broadcast and let the next bump
			// respawn us. Take the task slot under lock and re-check the map
			// to avoid racing with a fresh insert.
			let exit_threshold = retention.saturating_mul(2).max(1);
			if empty_ticks >= exit_threshold {
				let mut slot = inner.task.lock();
				if inner.entries.lock().is_empty() {
					*slot = None;
					drop(slot);
					drop(inner);
					drop(tracks);
					drop(broadcast);
					return;
				}
				empty_ticks = 0;
			}
		} else {
			empty_ticks = 0;
		}
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

	use crate::{Origin, Path};

	use super::*;

	fn test_stats(node: Option<&str>) -> (Stats, OriginProducer) {
		let origin = Origin::random().produce();
		let stats = Stats::new(
			".stats",
			Duration::from_secs(1),
			10,
			node.map(|s| PathOwned::from(s.to_string())),
			origin.clone(),
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

	#[test]
	fn new_normalizes_and_drops_empty_node() {
		let origin = Origin::random().produce();
		let stats = Stats::new(
			".stats",
			Duration::from_secs(1),
			10,
			Some(PathOwned::from("/sjc//1/".to_string())),
			origin.clone(),
		);
		assert_eq!(stats.inner.advertised.as_ref().unwrap().as_str(), ".stats/node/sjc/1");

		let stats = Stats::new(
			".stats",
			Duration::from_secs(1),
			10,
			Some(PathOwned::from("///".to_string())),
			origin,
		);
		assert_eq!(stats.inner.advertised.as_ref().unwrap().as_str(), ".stats/node");
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

		let entries = stats.inner.entries.lock();
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

		let entries = stats.inner.entries.lock();
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
		assert!(stats.inner.entries.lock().is_empty());
	}

	#[tokio::test(start_paused = true)]
	async fn disabled_stats_are_noop() {
		let stats = Stats::disabled();
		let bs = stats.tier(Tier::External).broadcast("demo/bbb");
		assert!(bs.is_empty());
		let p = bs.publisher();
		let track = p.track("video");
		track.bytes(100);
		drop(track);
		drop(p);
		assert!(stats.inner.entries.lock().is_empty());
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
		let (path, broadcast) = consumer.next().await.expect("expected announce");
		assert!(broadcast.is_some());
		assert_eq!(path.as_str(), ".stats/node/sjc/1");
	}

	#[tokio::test(start_paused = true)]
	async fn task_announces_without_node_suffix() {
		let origin = Origin::random().produce();
		let stats = Stats::new(".stats", Duration::from_secs(1), 10, None, origin.clone());
		let mut consumer = origin.consume().announced();

		let bs = stats.tier(Tier::External).broadcast("foo/bar");
		let _t = bs.publisher().track("video");

		tokio::time::advance(Duration::from_millis(1)).await;
		let (path, broadcast) = consumer.next().await.expect("expected announce");
		assert!(broadcast.is_some());
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
	async fn retention_boundary_after_drop() {
		// retention=2 lingers exactly 2 idle ticks after the LAST
		// snapshot change. The drop itself is a change (it bumps
		// announced_closed/subs_closed/derived broadcasts_closed), so the
		// retention countdown starts from the tick that observes the drop,
		// not the tick that observed the open guard.
		let origin = Origin::random().produce();
		let stats = Stats::new(
			".stats",
			Duration::from_secs(1),
			2,
			Some(PathOwned::from("sjc".to_string())),
			origin,
		);
		let key = PathOwned::from("foo/bar".to_string());
		let bs = stats.tier(Tier::External).broadcast("foo/bar");
		let track = bs.publisher().track("video");

		// Tick 1: observe open. snapshot changes.
		drive_ticks(1).await;
		drop(track);
		drop(bs);

		// Tick 2: observe drop. snapshot changes (broadcasts_closed bumps).
		drive_ticks(1).await;
		assert!(
			stats.inner.entries.lock().contains_key(&key),
			"kept on the tick the drop is observed"
		);
		// Tick 3: idle tick 1 after change. diff=1<=2, kept.
		drive_ticks(1).await;
		assert!(
			stats.inner.entries.lock().contains_key(&key),
			"kept after 1 idle tick (retention=2)"
		);
		// Tick 4: idle tick 2. diff=2<=2, kept.
		drive_ticks(1).await;
		assert!(
			stats.inner.entries.lock().contains_key(&key),
			"kept after 2 idle ticks (retention=2)"
		);
		// Tick 5: idle tick 3. diff=3>2, GC'd.
		drive_ticks(1).await;
		assert!(
			!stats.inner.entries.lock().contains_key(&key),
			"GC'd after 3 idle ticks (retention=2)"
		);
	}

	#[tokio::test(start_paused = true)]
	async fn retention_keeps_recently_dropped_entry() {
		let (stats, _origin) = test_stats(Some("sjc"));
		let bs = stats.tier(Tier::External).broadcast("foo/bar");
		let track = bs.publisher().track("video");

		drive_ticks(2).await;
		drop(track);
		drop(bs);
		// Within retention=10. Drop is observed as a change at tick 3,
		// so we have plenty of ticks left before GC.
		drive_ticks(3).await;

		assert!(
			stats
				.inner
				.entries
				.lock()
				.contains_key(&PathOwned::from("foo/bar".to_string())),
			"entry must remain in the map within the retention window"
		);
	}

	#[tokio::test(start_paused = true)]
	async fn retention_evicts_after_window() {
		let origin = Origin::random().produce();
		let stats = Stats::new(
			".stats",
			Duration::from_secs(1),
			2,
			Some(PathOwned::from("sjc".to_string())),
			origin,
		);
		let bs = stats.tier(Tier::External).broadcast("foo/bar");
		let track = bs.publisher().track("video");

		drive_ticks(2).await;
		drop(track);
		drop(bs);
		// Far past 2 * retention so the entry is fully aged out.
		drive_ticks(10).await;

		assert!(stats.inner.entries.lock().is_empty(), "entries should be GC'd");
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

		let (_path, broadcast) = consumer.next().await.expect("expected announce");
		let broadcast = broadcast.expect("active");
		let track = broadcast
			.subscribe_track(&Track {
				name: "publisher.json".into(),
				priority: 0,
				..Default::default()
			})
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

		let (_path, broadcast) = consumer.next().await.expect("announce");
		let broadcast = broadcast.expect("active");
		let track = broadcast
			.subscribe_track(&Track {
				name: "publisher.json".into(),
				priority: 0,
				..Default::default()
			})
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

		let (_path, broadcast) = consumer.next().await.expect("announce");
		let broadcast = broadcast.expect("active");
		let track = broadcast
			.subscribe_track(&Track {
				name: "publisher.json".into(),
				priority: 0,
				..Default::default()
			})
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
			let entries = stats.inner.entries.lock();
			let entry = entries.get(&PathOwned::from("foo/bar")).expect("entry");
			let raw = entry.publisher[Tier::External.idx()].snapshot();
			assert_eq!(raw.subscriptions, 2, "two track subs");
			assert_eq!(raw.subscriptions_closed, 0, "neither dropped yet");
		}

		drop(t1);
		drop(t2);
		drop(pub_guard);
		drop(bs);
		drive_ticks(1).await;

		// All subs dropped: subscriptions_closed catches up to subscriptions.
		// The snapshot task observes the 1->0 transition; derived
		// broadcasts_closed (which lives in task-local state, not on the
		// global entry) gets bumped to 1, but we can only see that through
		// the wire frame. Here we just confirm the raw counters squared up.
		let entries = stats.inner.entries.lock();
		let entry = entries
			.get(&PathOwned::from("foo/bar"))
			.expect("entry still in retention");
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

		let (_path, broadcast) = consumer.next().await.expect("announce");
		let broadcast = broadcast.expect("active");

		// External publisher slot SHOULD include foo/bar.
		let pub_track = broadcast
			.subscribe_track(&Track {
				name: "publisher.json".into(),
				priority: 0,
				..Default::default()
			})
			.expect("subscribe");
		assert!(
			read_frame(pub_track).await.contains_key("foo/bar"),
			"publisher.json must include the active foo/bar entry"
		);

		// The other three slots had zero activity. The first frame on
		// each must be `{}`, not `{"foo/bar": {all zeros}}`.
		for name in ["subscriber.json", "internal/publisher.json", "internal/subscriber.json"] {
			let t = broadcast
				.subscribe_track(&Track {
					name: name.into(),
					priority: 0,
					..Default::default()
				})
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

	async fn read_frame(mut track: crate::TrackConsumer) -> BTreeMap<String, Snapshot> {
		let bytes = track.read_frame().await.expect("ok").expect("frame");
		serde_json::from_slice(&bytes).expect("json parse")
	}
}
