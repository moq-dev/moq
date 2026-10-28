//! The aggregating half: fold every node's stats broadcast into one view.
//!
//! A single-broadcast [`Consumer`](crate::Consumer) reads one
//! `<prefix>/node/<node>` broadcast. This reader watches an origin's announce
//! stream for *every* node broadcast and folds their cumulative counters into
//! one merged frame per track, so a downstream sees a project's whole live
//! traffic as if it came from a single node.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::task::Poll;
use std::time::Duration;

use moq_clock::Instant;
use moq_net::kio::{self, Pending, Waiter};
use moq_net::stats::{Presence, Traffic};
use moq_net::track::Subscribing;
use moq_net::{Epoch, Path, broadcast, origin};

use crate::{
	Encoding, Group, GroupFrame, Prefix, PrefixFrame, PresenceFrame, Result, Totals, TotalsFrame, Usage, group_track,
	parse_node_path, prefix_track, presence_track, totals_track,
};

/// Configuration for an [`Consumer`]. Construct with [`Config::new`] and chain
/// the `with_*` setters.
///
/// The `prefix` must match the producing side's
/// [`produce::Config`](crate::produce::Config): it is how announced paths are
/// recognized as node broadcasts and filtered from sibling categories under the
/// same prefix.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Config {
	/// Top-level path stats are published under (default `.stats`). Must match
	/// the producer's prefix.
	pub prefix: Path,
	/// Which encoding of the totals, presence, and prefix tracks to read.
	/// [`Encoding::Compressed`] carries the same data for a fraction of the
	/// bytes. Defaults to [`Encoding::Plain`]. A group's track compresses
	/// itself, so this does not apply to it.
	pub encoding: Encoding,
	/// How long a departed node's traffic waits for its path to return before
	/// folding into the retired total (default 10 minutes). A return within it
	/// under the same epoch (or again without one) resumes the node's own
	/// counters; a return after it with counters intact counts its earlier
	/// traffic twice, so size this above reconnect times.
	pub grace: Duration,
}

impl Config {
	/// A config with default settings: the `.stats` prefix and the plain
	/// `.json` tracks.
	pub fn new() -> Self {
		Self::default()
	}

	/// Override the top-level prefix (default `.stats`). Must match the producer.
	pub fn with_prefix(mut self, prefix: impl Into<Path>) -> Self {
		self.prefix = prefix.into();
		self
	}

	/// Read the totals, presence, and prefix tracks in `encoding`.
	pub fn with_encoding(mut self, encoding: Encoding) -> Self {
		self.encoding = encoding;
		self
	}

	/// Override how long a departed node waits to return (default 10 minutes).
	pub fn with_grace(mut self, grace: Duration) -> Self {
		self.grace = grace;
		self
	}
}

impl Default for Config {
	fn default() -> Self {
		Self {
			prefix: Path::from_static(".stats"),
			encoding: Encoding::Plain,
			// Well above reconnect times; a longer grace only costs memory.
			grace: Duration::from_secs(600),
		}
	}
}

/// Folds every node's stats broadcast into one merged view.
///
/// Hand it an [`origin::Consumer`] that sees the node broadcasts (e.g. scoped
/// to `.stats`); each [`Self::totals`] / [`Self::group`] / [`Self::presence`]
/// / [`Self::prefix`] call opens its own announce cursor and subscribes to that
/// track on every node broadcast, summing the cumulative counters per key, so
/// the merged track has the same format as one node's. A group's track is
/// read only while the node's track listing lists it, so it needs a hop that
/// carries listings (moq-lite 07, or moq-transport draft 18 and later).
///
/// Every contribution is sticky: a node dropping out (its broadcast
/// unannounces or its reader ends) keeps its last contribution with its open
/// pairs (announces, subscriptions, sessions) closed, so a relay that returns
/// within [`Config::grace`] with its counters intact never looks like new
/// traffic. After the grace, a departed node's contribution folds into one
/// retired total and the node is forgotten, so memory follows live nodes and
/// the keys ever reported, not every node ever seen. Each route epoch is its
/// own node: a restarted producer announces a new epoch whose counters add to
/// the old epoch's kept contribution instead of regressing it, and a group
/// track's new instance, under a new [`GroupFrame::epoch`], adds to the old
/// instance's the same way. A reader stays pinned to its epoch, so a
/// replacement never feeds it. When epoch metadata disappears or returns at a
/// path (one publisher reached through mixed-version links), only the
/// immediately outgoing contribution carries across; a different known epoch
/// keeps its own entry, even when an unversioned reader resolves it first.
///
/// A group's per-label sessions are the exception: they are a gauge, so a
/// departed node stops counting them immediately.
///
/// The totals, presence, and prefix counters never regress: a node's counter
/// going down is that node resetting (a restart on a route without an epoch,
/// or a presence or prefix track held again from zero), so its previous
/// readout is kept as a baseline the new one adds to. A prefix's children are
/// the exception: they sum the live nodes' current children, so a child leaves
/// the merged map with the last node carrying it while its tail stays in the
/// rollup. A node that resumes after its departure closed its open pairs
/// reopens them as new starts rather than lowering their ends. A group's
/// per-path traffic also reopens pairs when a node resumes without regressing,
/// but keeps the reset contract of a single node: a route without an epoch
/// that returns within the grace with a lower counter regresses the merged
/// counter.
pub struct Consumer {
	origin: origin::Consumer,
	config: Config,
}

impl Consumer {
	/// Wrap an origin consumer. `config`'s `prefix` must match the producing
	/// side.
	pub fn new(origin: origin::Consumer, config: Config) -> Self {
		Self { origin, config }
	}

	/// A merged reader over the node-wide totals track, folding every node
	/// broadcast. Nodes are subscribed lazily as they announce, so this returns
	/// without a handshake.
	pub fn totals(&self) -> TotalsConsumer {
		TotalsConsumer {
			inner: self.merged(totals_track(self.config.encoding), self.config.encoding),
		}
	}

	/// A merged reader over group `group`'s track on every node broadcast that
	/// lists it; see [`Self::totals`].
	pub fn group(&self, group: impl Into<Path>) -> GroupConsumer {
		GroupConsumer {
			inner: self.merged(group_track(group), Encoding::Plain),
		}
	}

	/// A merged reader over `label`'s presence track, holding it on every
	/// node broadcast; see [`Self::totals`].
	pub fn presence(&self, label: impl Into<Path>) -> PresenceConsumer {
		PresenceConsumer {
			inner: self.merged(presence_track(label, self.config.encoding), self.config.encoding),
		}
	}

	/// A merged reader over broadcast prefix `path`'s track, holding it on
	/// every node broadcast; see [`Self::totals`].
	pub fn prefix(&self, path: impl Into<Path>) -> PrefixConsumer {
		PrefixConsumer {
			inner: self.merged(prefix_track(path, self.config.encoding), self.config.encoding),
		}
	}

	fn merged<S: Shape>(&self, name: String, encoding: Encoding) -> Merged<S> {
		Merged::new(self.origin.clone(), &self.config, name, encoding)
	}
}

/// A merged reader over the totals track across every node. Yields the latest
/// merged [`TotalsFrame`]; a slow reader collapses intermediate frames, which
/// is safe because the counters are cumulative.
pub struct TotalsConsumer {
	inner: Merged<Keyed<Totals>>,
}

impl TotalsConsumer {
	/// The next merged frame, or `None` once the announce stream ends (the
	/// source origin went away).
	pub async fn next(&mut self) -> Result<Option<TotalsFrame>> {
		kio::wait(|waiter| self.inner.poll_next(waiter)).await
	}
}

/// A merged reader over one group's track across every node; see
/// [`TotalsConsumer`].
pub struct GroupConsumer {
	inner: Merged<Grouped>,
}

impl GroupConsumer {
	/// The next merged frame, each tier's [`Group`] keyed by tier label as in
	/// [`GroupFrame::tiers`], or `None` once the announce stream ends.
	pub async fn next(&mut self) -> Result<Option<BTreeMap<String, Group>>> {
		kio::wait(|waiter| self.inner.poll_next(waiter)).await
	}
}

/// A merged reader over one label's presence track across every node; see
/// [`TotalsConsumer`].
pub struct PresenceConsumer {
	inner: Merged<Keyed<Presence>>,
}

impl PresenceConsumer {
	/// The next merged frame, or `None` once the announce stream ends.
	pub async fn next(&mut self) -> Result<Option<PresenceFrame>> {
		kio::wait(|waiter| self.inner.poll_next(waiter)).await
	}
}

/// A merged reader over one broadcast prefix's track across every node; see
/// [`TotalsConsumer`].
pub struct PrefixConsumer {
	inner: Merged<Keyed<Prefix>>,
}

impl PrefixConsumer {
	/// The next merged frame, or `None` once the announce stream ends.
	pub async fn next(&mut self) -> Result<Option<PrefixFrame>> {
		kio::wait(|waiter| self.inner.poll_next(waiter)).await
	}
}

/// A per-key cumulative counter that folds across nodes.
trait Mergeable: serde::de::DeserializeOwned + Default + Clone + PartialEq + 'static {
	/// Whether a node's last contribution survives its departure, with its
	/// open pairs closed. Only the sessions gauge drops it instead.
	const STICKY: bool = true;

	/// Whether a node's counter going down, or a key leaving its frame, means
	/// that node reset, so its previous readout becomes a baseline. A per-path
	/// map drops keys and restarts counters as entries end, so it does not.
	const RESETS: bool;

	/// Fold `other` into `acc`.
	fn merge(acc: &mut Self, other: Self);

	/// Close every open counter pair, so a departed node stops reporting live
	/// state while its cumulative counters stay.
	fn retire(self) -> Self;

	/// The pairs [`Self::retire`] closes that `next` has not since ended, as
	/// that many starts and ends and nothing else. Added to a node that resumes
	/// with `next` after a departure, so those pairs reopen as new starts
	/// instead of their ends going back down.
	fn reopen(self, next: &Self) -> Self;

	/// Whether any counter in `self` is below `prev`.
	fn regressed(&self, prev: &Self) -> bool;
}

/// Close an open started/ended pair.
fn close(started: u64, ended: &mut u64) {
	*ended = (*ended).max(started);
}

/// Set a pair to as many starts and ends as `next_ended` falls short of the
/// closed pair's `started`.
fn reopen(started: u64, next_ended: u64, into_started: &mut u64, into_ended: &mut u64) {
	let open = started.saturating_sub(next_ended);
	*into_started = open;
	*into_ended = open;
}

impl Mergeable for Traffic {
	const RESETS: bool = false;

	fn merge(acc: &mut Self, other: Self) {
		acc.add(other);
	}

	/// A departed relay can no longer carry its broadcasts or subscriptions,
	/// so the merged view must not keep counting them as live.
	fn retire(mut self) -> Self {
		close(self.announces_started, &mut self.announces_ended);
		close(self.broadcasts_started, &mut self.broadcasts_ended);
		close(self.subscriptions_started, &mut self.subscriptions_ended);
		self
	}

	fn reopen(self, next: &Self) -> Self {
		let mut open = Self::default();
		reopen(
			self.announces_started,
			next.announces_ended,
			&mut open.announces_started,
			&mut open.announces_ended,
		);
		reopen(
			self.broadcasts_started,
			next.broadcasts_ended,
			&mut open.broadcasts_started,
			&mut open.broadcasts_ended,
		);
		reopen(
			self.subscriptions_started,
			next.subscriptions_ended,
			&mut open.subscriptions_started,
			&mut open.subscriptions_ended,
		);
		open
	}

	fn regressed(&self, prev: &Self) -> bool {
		self.announces_started < prev.announces_started
			|| self.announces_ended < prev.announces_ended
			|| self.announced_bytes < prev.announced_bytes
			|| self.broadcasts_started < prev.broadcasts_started
			|| self.broadcasts_ended < prev.broadcasts_ended
			|| self.subscriptions_started < prev.subscriptions_started
			|| self.subscriptions_ended < prev.subscriptions_ended
			|| self.fetches < prev.fetches
			|| self.bytes < prev.bytes
			|| self.frames < prev.frames
			|| self.groups < prev.groups
			|| self.datagrams < prev.datagrams
			|| self.inflated < prev.inflated
			|| self.stale.bytes < prev.stale.bytes
			|| self.stale.frames < prev.stale.frames
			|| self.stale.groups < prev.stale.groups
			|| self.stale.datagrams < prev.stale.datagrams
	}
}

impl Mergeable for Presence {
	const RESETS: bool = true;

	fn merge(acc: &mut Self, other: Self) {
		acc.add(other);
	}

	/// A departed relay's sessions are gone.
	fn retire(mut self) -> Self {
		close(self.sessions_started, &mut self.sessions_ended);
		self
	}

	fn reopen(self, next: &Self) -> Self {
		let mut open = Self::default();
		reopen(
			self.sessions_started,
			next.sessions_ended,
			&mut open.sessions_started,
			&mut open.sessions_ended,
		);
		open
	}

	fn regressed(&self, prev: &Self) -> bool {
		self.wire_tx < prev.wire_tx
			|| self.wire_rx < prev.wire_rx
			|| self.sessions_refused.regressed(&prev.sessions_refused)
			|| self.sessions_failed.regressed(&prev.sessions_failed)
			|| self.requests < prev.requests
			|| self.sessions_started < prev.sessions_started
			|| self.sessions_ended < prev.sessions_ended
			|| self.sent_bytes < prev.sent_bytes
			|| self.received_bytes < prev.received_bytes
			|| self.announces_peak < prev.announces_peak
			|| self.subscriptions_peak < prev.subscriptions_peak
	}
}

/// One label's sessions on a group track, merged as a gauge: a departed
/// node's contribution leaves at once rather than staying with its pairs
/// closed, and a label leaving a node's frame is not a reset.
#[derive(Debug, Default, Clone, Copy, PartialEq, serde::Deserialize)]
#[serde(transparent)]
struct Gauge(Presence);

impl Mergeable for Gauge {
	const STICKY: bool = false;
	const RESETS: bool = false;

	fn merge(acc: &mut Self, other: Self) {
		acc.0.add(other.0);
	}

	fn retire(self) -> Self {
		Self(self.0.retire())
	}

	fn reopen(self, next: &Self) -> Self {
		Self(self.0.reopen(&next.0))
	}

	fn regressed(&self, prev: &Self) -> bool {
		self.0.regressed(&prev.0)
	}
}

impl Mergeable for Totals {
	const RESETS: bool = true;

	fn merge(acc: &mut Self, other: Self) {
		acc.add(other);
	}

	fn retire(mut self) -> Self {
		self.publisher = self.publisher.retire();
		self.subscriber = self.subscriber.retire();
		self.sessions = self.sessions.retire();
		self
	}

	fn reopen(self, next: &Self) -> Self {
		Self {
			publisher: self.publisher.reopen(&next.publisher),
			subscriber: self.subscriber.reopen(&next.subscriber),
			sessions: self.sessions.reopen(&next.sessions),
		}
	}

	fn regressed(&self, prev: &Self) -> bool {
		self.publisher.regressed(&prev.publisher)
			|| self.subscriber.regressed(&prev.subscriber)
			|| self.sessions.regressed(&prev.sessions)
	}
}

/// One broadcast path's traffic on a group track. Like any per-path entry, a
/// path leaves the frame and restarts its counters when its entry ends, which
/// is not a reset.
impl Mergeable for Usage {
	const RESETS: bool = false;

	fn merge(acc: &mut Self, other: Self) {
		acc.add(other);
	}

	fn retire(self) -> Self {
		Self {
			publisher: self.publisher.retire(),
			subscriber: self.subscriber.retire(),
		}
	}

	fn reopen(self, next: &Self) -> Self {
		Self {
			publisher: self.publisher.reopen(&next.publisher),
			subscriber: self.subscriber.reopen(&next.subscriber),
		}
	}

	fn regressed(&self, prev: &Self) -> bool {
		self.publisher.regressed(&prev.publisher) || self.subscriber.regressed(&prev.subscriber)
	}
}

impl Mergeable for Prefix {
	const RESETS: bool = true;

	fn merge(acc: &mut Self, other: Self) {
		acc.publisher.add(other.publisher);
		acc.subscriber.add(other.subscriber);
		acc.own.add(other.own);
		for (segment, child) in other.children {
			acc.children.entry(segment).or_default().add(child);
		}
	}

	/// A departed node's children leave the map; their tails stay in the rollup.
	fn retire(self) -> Self {
		Self {
			publisher: self.publisher.retire(),
			subscriber: self.subscriber.retire(),
			own: self.own.retire(),
			children: BTreeMap::new(),
		}
	}

	fn reopen(self, next: &Self) -> Self {
		Self {
			publisher: self.publisher.reopen(&next.publisher),
			subscriber: self.subscriber.reopen(&next.subscriber),
			own: self.own.reopen(&next.own),
			children: BTreeMap::new(),
		}
	}

	/// Children are left out: one leaving or returning lower is not a reset.
	fn regressed(&self, prev: &Self) -> bool {
		self.publisher.regressed(&prev.publisher)
			|| self.subscriber.regressed(&prev.subscriber)
			|| self.own.regressed(&prev.own)
	}
}

/// What one node contributes to a merged track, rebuilt from each frame it
/// reads.
trait Shape: Default {
	/// One frame off the track.
	type Frame: serde::de::DeserializeOwned + 'static;
	/// The merged frame.
	type Merged: Default + Clone;
	/// Whether the track is read only while the node's broadcast lists it, so
	/// a reader waits for its next listing instead of re-resolving.
	const LISTED: bool;

	/// Take a fresh frame.
	fn observe(&mut self, frame: Self::Frame);

	/// The node stopped reading: keep what is sticky with its open pairs
	/// closed. Returns whether the contribution changed.
	fn depart(&mut self) -> bool;

	/// Add this contribution to `acc`.
	fn contribute(&self, acc: &mut Self::Merged);

	/// Forget the contribution, once it has folded into the retired total.
	fn clear(&mut self);
}

/// One node's fold of a keyed frame of [`Mergeable`] values.
struct Keyed<V, K = String> {
	/// Previous reset readouts with closed pairs, plus starts and ends added
	/// when stale pairs reopen. Traffic discards its additions on a reset.
	base: BTreeMap<K, V>,
	/// The last frame the node produced, as read.
	last: Option<BTreeMap<K, V>>,
	/// Whether `last` is from a reader that has since ended, so its open pairs
	/// count as closed until a fresh frame arrives.
	stale: bool,
}

impl<V, K> Default for Keyed<V, K> {
	fn default() -> Self {
		Self {
			base: BTreeMap::new(),
			last: None,
			stale: false,
		}
	}
}

impl<V: Mergeable, K: Ord + Clone> Keyed<V, K> {
	/// Take a fresh frame. A resume reopens the closed pairs still open as new
	/// starts. On a reset (`reset` for every key, or a counter going down),
	/// totals and presence keep the closed readout as a baseline, while
	/// per-path traffic discards its additions to preserve per-path resets.
	fn update(&mut self, frame: BTreeMap<K, V>, reset: bool) {
		if (reset || V::RESETS || self.stale || !self.base.is_empty())
			&& let Some(last) = self.last.take()
		{
			for (key, prev) in last {
				let base = match frame.get(&key) {
					Some(next) if !reset && !next.regressed(&prev) => match self.stale {
						true => prev.reopen(next),
						false => continue,
					},
					_ if V::RESETS => prev.retire(),
					_ => {
						self.base.remove(&key);
						continue;
					}
				};
				if V::RESETS || base != V::default() {
					V::merge(self.base.entry(key).or_default(), base);
				}
			}
		}
		self.last = Some(frame);
		self.stale = false;
	}

	/// Keep the last frame with its open pairs closed, or drop it for a
	/// gauge. Returns whether the contribution changed.
	fn leave(&mut self) -> bool {
		if !V::STICKY {
			return self.last.take().is_some_and(|last| !last.is_empty());
		}
		if self.stale {
			return false;
		}
		self.stale = true;
		self.last
			.iter()
			.flatten()
			.any(|(_, value)| value.clone().retire() != *value)
	}

	/// Hand each key's contribution to `f`.
	fn each(&self, mut f: impl FnMut(&K, V)) {
		for (key, value) in &self.base {
			f(key, value.clone());
		}
		for (key, value) in self.last.iter().flatten() {
			f(
				key,
				match self.stale {
					true => value.clone().retire(),
					false => value.clone(),
				},
			);
		}
	}

	fn forget(&mut self) {
		self.base.clear();
		self.last = None;
	}
}

impl<V: Mergeable> Shape for Keyed<V> {
	type Frame = BTreeMap<String, V>;
	type Merged = BTreeMap<String, V>;
	const LISTED: bool = false;

	fn observe(&mut self, frame: Self::Frame) {
		self.update(frame, false);
	}

	fn depart(&mut self) -> bool {
		self.leave()
	}

	fn contribute(&self, acc: &mut Self::Merged) {
		self.each(|key, value| V::merge(acc.entry(key.clone()).or_default(), value));
	}

	fn clear(&mut self) {
		self.forget();
	}
}

/// One node's fold of a group track: the tiers' totals, each `(tier, path)`'s
/// traffic, and each `(tier, label)`'s sessions.
#[derive(Default)]
struct Grouped {
	/// The track instance the last frame came from.
	epoch: Option<Epoch>,
	totals: Keyed<Totals>,
	paths: Keyed<Usage, (String, String)>,
	labels: Keyed<Gauge, (String, String)>,
}

impl Shape for Grouped {
	type Frame = GroupFrame;
	type Merged = BTreeMap<String, Group>;
	const LISTED: bool = true;

	fn observe(&mut self, frame: GroupFrame) {
		// A new instance counts from zero, whatever its counters read by the time
		// this reader sees them.
		let reset = self.epoch.as_ref().is_some_and(|epoch| *epoch != frame.epoch);
		self.epoch = Some(frame.epoch);
		let mut totals = BTreeMap::new();
		let mut paths = BTreeMap::new();
		let mut labels = BTreeMap::new();
		for (tier, group) in frame.tiers {
			for (path, usage) in group.paths {
				paths.insert((tier.clone(), path), usage);
			}
			for (label, presence) in group.labels {
				labels.insert((tier.clone(), label), Gauge(presence));
			}
			totals.insert(
				tier,
				Totals {
					publisher: group.publisher,
					subscriber: group.subscriber,
					sessions: group.sessions,
				},
			);
		}
		self.totals.update(totals, reset);
		self.paths.update(paths, reset);
		self.labels.update(labels, reset);
	}

	fn depart(&mut self) -> bool {
		self.totals.leave() | self.paths.leave() | self.labels.leave()
	}

	fn contribute(&self, acc: &mut Self::Merged) {
		self.totals.each(|tier, totals| {
			let group = acc.entry(tier.clone()).or_default();
			group.publisher.add(totals.publisher);
			group.subscriber.add(totals.subscriber);
			group.sessions.add(totals.sessions);
		});
		self.paths.each(|(tier, path), usage| {
			let group = acc.entry(tier.clone()).or_default();
			group.paths.entry(path.clone()).or_default().add(usage);
		});
		self.labels.each(|(tier, label), gauge| {
			let group = acc.entry(tier.clone()).or_default();
			group.labels.entry(label.clone()).or_default().add(gauge.0);
		});
	}

	fn clear(&mut self) {
		self.epoch = None;
		self.totals.forget();
		self.paths.forget();
		self.labels.forget();
	}
}

/// One node's subscription to the merged track.
enum Reader<F> {
	/// Resolving the instance while owning discovery and retrying route churn.
	Resolving(Box<Pending<origin::Requesting>>),
	/// Waiting for the node's listing to offer the track.
	Waiting,
	/// Awaiting the subscription handshake.
	Subscribing(Pending<Subscribing>),
	/// Reading frames. Boxed: the snapshot consumer dwarfs the other variants,
	/// and one lives per node in a map.
	Active(Box<moq_json::snapshot::Consumer<F>>),
	/// The subscription failed or the track ended; the node no longer reads. It
	/// lingers until it reannounces or, once unannounced, its grace elapses,
	/// still contributing its last frame.
	Ended,
}

/// One node's reader plus what it contributes to the merged view.
struct Node<S: Shape> {
	reader: Reader<S::Frame>,
	/// The resolved broadcast's track listing, for a [`Shape::LISTED`] track.
	listing: Option<broadcast::Tracks>,
	/// Whether the listing offered the track since the reader last subscribed.
	offered: bool,
	/// The announced path, relative to the announce cursor: what a re-resolve
	/// after the reader ends requests again.
	path: Path,
	/// The publisher instance every initial and retry resolution must serve.
	epoch: Option<Epoch>,
	/// The last known publisher identity, retained while route metadata vanishes.
	identity: Option<Epoch>,
	shape: S,
	/// When the path unannounced, starting its grace; cleared by a reannounce.
	/// Only an unannounced node folds, so a still-announced node whose reader
	/// failed keeps its own counters.
	departed: Option<Instant>,
}

impl<S: Shape> Node<S> {
	/// The node's broadcast went away (unannounced, replaced by one without
	/// this track, or its subscription ended): stop reading and keep the last
	/// frame with its open pairs closed, so a departed node stops reporting
	/// live state while its cumulative counters stay. Returns whether the
	/// merged view changed.
	fn depart(&mut self) -> bool {
		self.reader = Reader::Ended;
		self.listing = None;
		self.offered = false;
		self.shape.depart()
	}

	/// Take a fresh frame.
	fn observe(&mut self, frame: S::Frame) {
		self.shape.observe(frame);
	}

	/// Fold the contribution into `retired` if the node departed at least
	/// `grace` before `now`, leaving the merged view unchanged. Returns whether
	/// it folded. A grace too long to represent never folds.
	fn fold_expired(&mut self, retired: &mut S::Merged, grace: Duration, now: Instant) -> bool {
		let Some(departed) = self.departed else { return false };
		if departed.checked_add(grace).is_none_or(|at| at > now) {
			return false;
		}
		self.shape.contribute(retired);
		self.shape.clear();
		true
	}
}

/// Watches node announces and folds one track across all of them.
struct Merged<S: Shape> {
	/// Resolves announced node paths into broadcasts.
	origin: origin::Consumer,
	announce: moq_net::announce::Consumer,
	prefix: Path,
	/// Track name subscribed on each node broadcast.
	name: String,
	config: moq_json::snapshot::consumer::Config,
	/// One entry per live or recently departed node broadcast, keyed by
	/// absolute announced path and route epoch, so each epoch is its own entry.
	nodes: HashMap<(Path, Option<Epoch>), Node<S>>,
	/// The immediately outgoing identity, including while it remains in grace.
	last_announced: HashMap<Path, Option<Epoch>>,
	/// How long a departed node waits before folding into `retired`.
	grace: Duration,
	/// The summed last frames of every node whose grace elapsed.
	retired: S::Merged,
}

impl<S: Shape> Merged<S> {
	fn new(origin: origin::Consumer, config: &Config, name: String, encoding: Encoding) -> Self {
		Self {
			// Stats live under a `.`-named prefix, which discovery hides by default.
			announce: origin.clone().with_hidden(true).announced(),
			origin,
			prefix: config.prefix.clone(),
			name,
			config: {
				let mut json = moq_json::snapshot::consumer::Config::default();
				json.compression = encoding.compression();
				json
			},
			nodes: HashMap::new(),
			last_announced: HashMap::new(),
			grace: config.grace,
			retired: S::Merged::default(),
		}
	}

	/// Poll for the next merged frame. Returns `Ready(Some(_))` whenever the
	/// merged view changed (a node produced a frame, a departed node's open
	/// pairs closed, or a gauge's contribution left), `Ready(None)` once the
	/// announce stream closes, else `Pending`.
	fn poll_next(&mut self, waiter: &Waiter) -> Poll<Result<Option<S::Merged>>> {
		let mut changed = false;

		// Drain announce membership updates: add/replace on announce, drop on
		// unannounce. A closed stream ends the merged view.
		loop {
			match self.announce.poll_next(waiter) {
				Poll::Ready(Some(
					moq_net::announce::Event::Start(update) | moq_net::announce::Event::Update(update),
				)) => changed |= self.apply_announce(update, true),
				Poll::Ready(Some(moq_net::announce::Event::End(update))) => {
					changed |= self.apply_announce(update, false)
				}
				// Another publisher instance: the one tracked at the path ends, then the
				// new one starts, so its reader resolves afresh.
				Poll::Ready(Some(moq_net::announce::Event::Restart(update))) => {
					let mut ended = update.clone();
					// An announced prefix is a path beneath the root, so rooting it fits.
					let absolute = self.announce.absolute(&update.prefix).ok();
					ended.route.epoch =
						absolute.and_then(|absolute| self.last_announced.get(&absolute).cloned().flatten());
					changed |= self.apply_announce(ended, false);
					changed |= self.apply_announce(update, true);
				}
				Poll::Ready(None) => return Poll::Ready(Ok(None)),
				Poll::Pending => break,
			}
		}

		// Advance each node's reader, collapsing any backlog to its latest frame,
		// then fold every departed node whose grace elapsed. A fold leaves the
		// merged view unchanged, and only an announce adds a node, which polls
		// here, so no timer is needed to keep the map bounded.
		let now = moq_clock::now();
		let config = &self.config;
		let name = self.name.as_str();
		let origin = &self.origin;
		let grace = self.grace;
		let retired = &mut self.retired;
		let last_announced = &mut self.last_announced;
		self.nodes.retain(|(path, epoch), node| {
			changed |= advance(node, origin, config, name, waiter);
			if node.fold_expired(retired, grace, now) {
				if last_announced.get(path) == Some(epoch) {
					last_announced.remove(path);
				}
				false
			} else {
				true
			}
		});
		// Release a churn peak's buckets: walking a map costs its capacity, so a
		// map that once held thousands of nodes would slow every frame.
		if self.nodes.capacity() > 4 * self.nodes.len().max(16) {
			self.nodes.shrink_to_fit();
		}

		if changed {
			Poll::Ready(Ok(Some(self.merged())))
		} else {
			Poll::Pending
		}
	}

	/// Apply one announce update to the node set. Returns whether the merged view
	/// changed (only a departure closing open pairs, or a gauge's contribution
	/// leaving, does).
	fn apply_announce(&mut self, update: moq_net::announce::Announce, active: bool) -> bool {
		let path = update.prefix;
		// An announced prefix is a path beneath the root, so rooting it fits.
		let Ok(absolute) = self.announce.absolute(&path) else {
			return false;
		};

		// Only fold node-category routes; skip sibling categories a producer
		// may publish under the same prefix. A route names a prefix, and node
		// broadcasts are announced at their exact path by convention.
		if parse_node_path(&self.prefix, &absolute).is_none() {
			return false;
		}
		// Publisher replacement can race reader polling on another worker, so every
		// reader pins the identity it contributes to, independent of announce order.
		let key = (absolute, update.route.epoch);

		if !active {
			// Unannounce: keep the cumulative counters, close the open pairs,
			// and stop reading. The entry stays so a reannounce within the
			// grace re-arms it.
			return match self.nodes.get_mut(&key) {
				Some(node) => {
					node.departed = Some(moq_clock::now());
					node.depart()
				}
				None => false,
			};
		}

		if let Some(previous) = self.last_announced.insert(key.0.clone(), key.1.clone()) {
			// An unversioned route cannot identify a restart. Carry only the
			// immediately outgoing contribution across loss or recovery of metadata;
			// distinct known epochs remain separate, even while several are in grace.
			if previous != key.1
				&& (previous.is_none() || key.1.is_none())
				&& let Some(mut node) = self.nodes.remove(&(key.0.clone(), previous))
			{
				node.depart();
				if key.1.is_some() && node.identity.is_some() && node.identity != key.1 {
					// Recovery identifies a distinct known publisher. Keep the outgoing
					// lineage in grace and re-arm the incoming identity's own entry.
					node.epoch = node.identity.clone();
					self.nodes.insert((key.0.clone(), node.identity.clone()), node);
				} else {
					node.epoch = key.1.clone();
					if key.1.is_some() {
						node.identity = key.1.clone();
					}
					self.nodes.insert(key.clone(), node);
				}
			}
		}
		// A route update on a node already tracked (a reprice, or a takeover
		// with different metadata) keeps the live reader: existing
		// subscriptions survive a takeover, and the reader re-resolves through
		// the current best route the moment it actually ends. Only an ended
		// node re-arms here, since a fresh route is new evidence that a
		// re-resolve could succeed. A sticky contribution carries across the
		// re-arm, holding the total until a fresh frame replaces it.
		match self.nodes.entry(key) {
			Entry::Occupied(mut entry) => {
				let node = entry.get_mut();
				// A return after the grace starts over, even when this announce
				// is the first poll since the grace elapsed.
				node.fold_expired(&mut self.retired, self.grace, moq_clock::now());
				node.departed = None;
				if matches!(node.reader, Reader::Ended) {
					node.reader = resolve(&self.origin, &node.path, node.epoch.clone());
				}
			}
			Entry::Vacant(entry) => {
				let epoch = entry.key().1.clone();
				entry.insert(Node {
					reader: resolve(&self.origin, &path, epoch.clone()),
					listing: None,
					offered: false,
					identity: epoch.clone(),
					epoch,
					path,
					shape: S::default(),
					departed: None,
				});
			}
		}
		false
	}

	/// Sum the retired total and every node's contribution, per key.
	fn merged(&self) -> S::Merged {
		let mut acc = self.retired.clone();
		for node in self.nodes.values() {
			node.shape.contribute(&mut acc);
		}
		acc
	}
}

/// Drive one node's reader as far as it goes, updating its contribution.
/// Returns whether that contribution to the merged view changed.
fn advance<S: Shape>(
	node: &mut Node<S>,
	origin: &origin::Consumer,
	config: &moq_json::snapshot::consumer::Config,
	name: &str,
	waiter: &Waiter,
) -> bool {
	let mut changed = false;
	// At most one re-resolve per call: a subscription that terminates
	// synchronously twice in a row is done, not failing over.
	let mut rearmed = false;
	loop {
		// Follow the listing's word on the track, so a reader whose subscription
		// ended subscribes again once the track's next instance is listed. A
		// listing that ends took its broadcast with it.
		while let Some(listing) = &mut node.listing {
			match listing.poll_next(waiter) {
				Poll::Ready(Ok(Some(broadcast::TracksEvent::Added { name: listed, .. }))) => {
					node.offered |= &*listed == name;
				}
				Poll::Ready(Ok(Some(
					broadcast::TracksEvent::Removed { name: listed }
					| broadcast::TracksEvent::Finished { name: listed },
				))) => {
					node.offered &= &*listed != name;
				}
				Poll::Ready(Ok(Some(broadcast::TracksEvent::Synced))) => {}
				Poll::Ready(result) => {
					if let Err(err) = result {
						tracing::debug!(?err, name, "stats: node listing ended");
					}
					node.listing = None;
				}
				Poll::Pending => break,
			}
		}

		match &mut node.reader {
			Reader::Resolving(pending) => match pending.poll_ok(waiter) {
				// An unpinned retry on a known lineage can reach a replacement publisher
				// before its announcement drains here. Its counters belong to its own entry,
				// which that announcement creates, so this lineage keeps its last frame.
				Poll::Ready(Ok(broadcast))
					if node.epoch.is_none()
						&& node.identity.is_some()
						&& broadcast
							.info()
							.epoch
							.as_ref()
							.is_some_and(|epoch| node.identity.as_ref() != Some(epoch)) =>
				{
					tracing::debug!(name, "stats: node resolved another publisher");
					return changed | node.depart();
				}
				Poll::Ready(Ok(broadcast)) if S::LISTED => match broadcast.tracks() {
					Ok(listing) => {
						node.listing = Some(listing);
						node.offered = false;
						node.reader = Reader::Waiting;
					}
					Err(err) => {
						tracing::debug!(?err, name, "stats: node listing refused");
						return changed | node.depart();
					}
				},
				Poll::Ready(Ok(broadcast)) => match broadcast.track(name) {
					Ok(track) => node.reader = Reader::Subscribing(track.subscribe(None)),
					Err(err) => {
						tracing::debug!(?err, name, "stats: node missing track");
						return changed | node.depart();
					}
				},
				Poll::Ready(Err(err)) => {
					tracing::debug!(?err, name, "stats: node broadcast unresolvable");
					return changed | node.depart();
				}
				Poll::Pending => return changed,
			},
			Reader::Waiting => {
				let Some(listing) = &node.listing else {
					// The listing ended with the broadcast: re-resolve, as for a
					// subscription that ended.
					changed |= node.depart();
					if rearmed {
						return changed;
					}
					rearmed = true;
					node.reader = resolve(origin, &node.path, node.epoch.clone());
					continue;
				};
				if !node.offered {
					return changed;
				}
				node.offered = false;
				match listing.broadcast().track(name) {
					Ok(track) => node.reader = Reader::Subscribing(track.subscribe(None)),
					Err(err) => {
						tracing::debug!(?err, name, "stats: node missing track");
						return changed | node.depart();
					}
				}
			}
			Reader::Subscribing(pending) => match pending.poll_ok(waiter) {
				Poll::Ready(Ok(subscriber)) => {
					node.reader =
						Reader::Active(Box::new(moq_json::snapshot::Consumer::new(subscriber, config.clone())));
				}
				// The listed instance ended before the subscription landed: wait for
				// the next one.
				Poll::Ready(Err(err)) if node.listing.is_some() => {
					tracing::debug!(?err, name, "stats: node subscribe failed");
					node.reader = Reader::Waiting;
				}
				Poll::Ready(Err(err)) => {
					tracing::debug!(?err, name, "stats: node subscribe failed");
					return changed | node.depart();
				}
				Poll::Pending => return changed,
			},
			Reader::Active(reader) => match reader.poll_latest(waiter) {
				Poll::Ready(Ok(Some(frame))) => {
					node.observe(frame.value);
					changed = true;
				}
				// The subscription ended: the serving session died (a failover to
				// an identical route delivers no announce update) or the track
				// finished. Keep the last frame with its open pairs closed, then
				// wait for the listing's next instance or, without a listing,
				// re-resolve through the current best route; an authoritative
				// refusal on the way ends the node instead.
				Poll::Ready(result @ (Ok(None) | Err(_))) => {
					if let Err(err) = result {
						// One bad node must not tear down the whole merged view;
						// re-resolve just this node and keep folding the rest.
						tracing::debug!(?err, name, "stats: node read error");
					}
					if node.listing.is_some() {
						changed |= node.shape.depart();
						node.reader = Reader::Waiting;
						continue;
					}
					// `depart` drops the dead reader before the re-request: our
					// own handle is what keeps a dying served broadcast cached,
					// and releasing it first lets the request materialize a
					// fresh one.
					changed |= node.depart();
					if rearmed {
						return changed;
					}
					rearmed = true;
					node.reader = resolve(origin, &node.path, node.epoch.clone());
				}
				Poll::Pending => return changed,
			},
			Reader::Ended => return changed,
		}
	}
}

/// Start resolving `path` (relative to the announce cursor) into a broadcast.
fn resolve<F>(origin: &origin::Consumer, path: &Path, epoch: Option<Epoch>) -> Reader<F> {
	let pending = origin.request_broadcast(path, epoch);
	Reader::Resolving(Box::new(pending))
}

#[cfg(test)]
mod tests {
	/// A millisecond track: the frames written here carry timestamps.
	fn timed() -> moq_net::track::Info {
		moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
	}

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

	/// The next route and whether it is active.
	async fn next_update(announced: &mut moq_net::announce::Consumer) -> Option<(moq_net::announce::Announce, bool)> {
		match announced.next().await? {
			moq_net::announce::Event::Start(route)
			| moq_net::announce::Event::Update(route)
			| moq_net::announce::Event::Restart(route) => Some((route, true)),
			moq_net::announce::Event::End(route) => Some((route, false)),
		}
	}

	use std::time::Duration;

	use moq_net::{Path, Timestamp, announce, broadcast, origin, track};

	use moq_net::stats::Tier;

	use crate::{Producer, produce};

	use super::*;

	type TrafficFrame = BTreeMap<String, Traffic>;
	type SessionsFrame = BTreeMap<String, Presence>;

	/// The default tier's egress per path, off group `acme`'s merged track.
	struct TrafficConsumer {
		inner: Merged<Grouped>,
	}

	impl TrafficConsumer {
		async fn next(&mut self) -> Result<Option<TrafficFrame>> {
			let frame = kio::wait(|waiter| self.inner.poll_next(waiter)).await?;
			Ok(frame.map(|tiers| publisher(&tiers)))
		}
	}

	fn publisher(tiers: &BTreeMap<String, Group>) -> TrafficFrame {
		let paths = tiers.get("").map(|group| &group.paths).into_iter().flatten();
		paths.map(|(path, usage)| (path.clone(), usage.publisher)).collect()
	}

	fn traffic(agg: &Consumer) -> TrafficConsumer {
		TrafficConsumer {
			inner: agg.group(moq_net::Path::from_static("acme")).inner,
		}
	}

	/// The retired egress bytes on `path`.
	fn retired_bytes(traffic: &TrafficConsumer, path: &str) -> Option<u64> {
		publisher(&traffic.inner.retired).get(path).map(|t| t.bytes)
	}

	/// The default tier's sessions per label, off group `acme`'s merged track.
	struct SessionsConsumer {
		inner: GroupConsumer,
	}

	impl SessionsConsumer {
		async fn next(&mut self) -> Result<Option<SessionsFrame>> {
			let frame = self.inner.next().await?;
			Ok(frame.map(|mut tiers| tiers.remove("").map(|group| group.labels).unwrap_or_default()))
		}
	}

	fn sessions(agg: &Consumer) -> SessionsConsumer {
		SessionsConsumer {
			inner: agg.group(moq_net::Path::from_static("acme")),
		}
	}

	/// A source of merged frames for [`read_until`].
	trait Frames<V> {
		async fn frame(&mut self) -> BTreeMap<String, V>;
	}

	impl<V: Mergeable> Frames<V> for Merged<Keyed<V>> {
		async fn frame(&mut self) -> BTreeMap<String, V> {
			kio::wait(|waiter| self.poll_next(waiter))
				.await
				.expect("read")
				.expect("frame")
		}
	}

	impl Frames<Traffic> for TrafficConsumer {
		async fn frame(&mut self) -> TrafficFrame {
			self.next().await.expect("read").expect("frame")
		}
	}

	/// The group epoch every hand-published group frame carries, so a new
	/// [`NodeBroadcast`] at the same path is the same track instance returning.
	fn group_epoch() -> Epoch {
		"01900000-0000-7000-8000-0000000000ff".parse().unwrap()
	}

	/// A stats producer publishing one node's broadcast on `origin`, grouped at
	/// depth 1 (so feeding a broadcast under `<group>/...` publishes a
	/// `<group>/group.json` track on `.stats/node/<node>`).
	fn node_producer(origin: &origin::Producer, node: &str) -> Producer {
		Producer::new(
			produce::Config::new()
				.with_origin(origin.clone())
				.with_node(Path::try_from(node.to_string()).unwrap())
				.with_depth(1),
		)
	}

	/// Kept-alive handles from [`feed`]: dropping them closes the subscription and
	/// announce, bumping the `_closed` counters.
	#[allow(dead_code, reason = "held for their drop, never read")]
	struct Feed {
		announced: announce::Consumer,
		source: origin::Publication,
		consumer: broadcast::Consumer,
		sub: track::Subscriber,
		ctx: moq_net::stats::Session,
	}

	/// Record `bytes` of egress traffic on `path` in `producer`'s registry under
	/// `tier`/`label`, by driving a throwaway tagged broadcast. The broadcast lives
	/// on its own origin so it never lands on the stats origin.
	async fn feed(producer: &Producer, tier: Tier, label: &str, path: &str, bytes: usize) -> Feed {
		let ctx = producer
			.registry()
			.tier(tier)
			.session(moq_net::Path::new(label).unwrap());
		let feed_origin = produce_origin();
		let egress = feed_origin.consume().with_stats(ctx.clone());

		let mut announced = egress.announced();
		let source = feed_origin.create_broadcast(path).expect("create_broadcast");
		source.announce(origin::Route::default()).expect("announce");
		let track = source.create_track("video", timed()).expect("create_track");

		let (_, active) = next_update(&mut announced).await.expect("announce");
		assert!(active);
		let consumer = egress.request_broadcast(path, None).await.expect("resolve");
		let mut sub = consumer.track("video").unwrap().subscribe(None).await.unwrap();

		let mut group = track.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, vec![0u8; bytes]).unwrap();
		group.finish().unwrap();
		let mut group = sub.recv_group().await.unwrap().unwrap();
		while group.read_frame().await.unwrap().is_some() {}

		Feed {
			announced,
			source,
			consumer,
			sub,
			ctx,
		}
	}

	/// Advance past one publish interval so every producer task drains and writes.
	async fn drive_tick() {
		tokio::time::advance(Duration::from_millis(1100)).await;
		for _ in 0..8 {
			tokio::task::yield_now().await;
		}
	}

	/// Read merged traffic frames until `path`'s byte count reaches `want` (each
	/// node folds in independently, so a partial frame can arrive first).
	async fn read_until_bytes(consumer: &mut TrafficConsumer, path: &str, want: u64) -> TrafficFrame {
		loop {
			let frame = consumer.next().await.expect("read").expect("frame");
			if frame.get(path).map(|t| t.bytes).unwrap_or(0) >= want {
				return frame;
			}
		}
	}

	/// Read merged traffic frames until `path`'s byte count reaches `want`,
	/// asserting it never regresses below `min` along the way: a departed node's
	/// kept contribution must hold the total through every intermediate frame.
	async fn read_monotonic_until(consumer: &mut TrafficConsumer, path: &str, min: u64, want: u64) -> TrafficFrame {
		loop {
			let frame = consumer.next().await.expect("read").expect("frame");
			let bytes = frame.get(path).map(|t| t.bytes).unwrap_or(0);
			assert!(bytes >= min, "traffic regressed below {min}: {bytes}");
			if bytes >= want {
				return frame;
			}
		}
	}

	/// A hand-published node broadcast at `.stats/node/<node>` with one plain
	/// group track carrying default-tier egress per path, so a test controls
	/// the exact frames and can fail one node's reader alone (the
	/// registry-driven producer only publishes whole broadcasts). Its route
	/// carries no epoch, so a new one at the same path is that node returning.
	/// Dropping it unannounces the node.
	#[allow(dead_code, reason = "held for their drop, never read")]
	struct NodeBroadcast {
		source: origin::Publication,
		group: moq_json::snapshot::Producer<GroupFrame>,
		track: track::Producer,
		frame: TrafficFrame,
	}

	impl NodeBroadcast {
		fn new(origin: &origin::Producer, group: &str, node: &str) -> Self {
			let path = format!(".stats/node/{node}");
			let source = origin.create_broadcast(path.as_str()).expect("create broadcast");
			source.announce(origin::Route::default()).expect("announce");
			let track = source
				.create_track(group_track(moq_net::Path::new(group).unwrap()), timed())
				.expect("create track");
			let config = moq_json::snapshot::Config::default().with_deltas(false);
			Self {
				group: moq_json::snapshot::Producer::new(track.clone(), config),
				track,
				source,
				frame: TrafficFrame::new(),
			}
		}

		/// Add `bytes` to `path`'s cumulative counter and publish the node's
		/// whole snapshot, like a real node's registry drain would.
		fn publish(&mut self, path: &str, bytes: u64) {
			let entry = self.frame.entry(path.to_string()).or_default();
			entry.bytes += bytes;
			let mut group = Group::default();
			for (path, traffic) in &self.frame {
				group.paths.entry(path.clone()).or_default().publisher = *traffic;
			}
			let frame = GroupFrame {
				epoch: group_epoch(),
				tiers: BTreeMap::from([(String::new(), group)]),
			};
			self.group
				.update(moq_net::Timed::from(&frame).at(moq_net::Timestamp::now()))
				.expect("publish");
		}

		/// Fail the node's reader: append a frame the snapshot decoder can't
		/// parse, so the subscription errors while the broadcast stays announced.
		fn fail_traffic(&mut self) {
			let mut group = self.track.append_group().expect("append group");
			group
				.write_frame(Timestamp::now(), b"not json".to_vec())
				.expect("write frame");
			group.finish().expect("finish group");
		}
	}

	#[tokio::test(start_paused = true)]
	async fn merges_traffic_across_nodes() {
		// Two nodes each serve the same broadcast; the merged view sums their
		// cumulative counters per path.
		let origin = produce_origin();
		let node_a = node_producer(&origin, "a");
		let node_b = node_producer(&origin, "b");

		let _fa = feed(&node_a, Tier::default(), "acme", "acme/room", 100).await;
		let _fb = feed(&node_b, Tier::default(), "acme", "acme/room", 40).await;
		drive_tick().await;

		let agg = Consumer::new(origin.consume(), Config::new());
		let mut traffic = traffic(&agg);

		let frame = read_until_bytes(&mut traffic, "acme/room", 140).await;
		let snap = frame.get("acme/room").expect("entry");
		assert_eq!(snap.bytes, 140, "bytes sum across both nodes");
		assert_eq!(snap.subscriptions_started, 2, "one subscription per node");
		assert_eq!(snap.broadcasts_started, 2, "one viewer per node");
	}

	#[tokio::test(start_paused = true)]
	async fn node_drop_keeps_the_traffic_total() {
		// Dropping a node unannounces its broadcast, but traffic is sticky: its
		// last contribution stays in the total, so a relay that returns with its
		// boot-lifetime counters intact never looks like new traffic.
		let origin = produce_origin();
		let node_a = node_producer(&origin, "a");
		let node_b = node_producer(&origin, "b");

		let _fa = feed(&node_a, Tier::default(), "acme", "acme/room", 100).await;
		let fb = feed(&node_b, Tier::default(), "acme", "acme/room", 40).await;
		drive_tick().await;

		let agg = Consumer::new(origin.consume(), Config::new());
		let mut traffic = traffic(&agg);
		read_until_bytes(&mut traffic, "acme/room", 140).await;

		// Drop node B entirely: its publish task ends and finishes the broadcast.
		drop(fb);
		drop(node_b);
		drive_tick().await;

		// Node A serves more traffic on another path; the merged frame still
		// carries node B's kept contribution for acme/room.
		let _fa2 = feed(&node_a, Tier::default(), "acme", "acme/other", 10).await;
		drive_tick().await;

		let frame = read_until_bytes(&mut traffic, "acme/other", 10).await;
		assert_eq!(
			frame.get("acme/room").map(|t| t.bytes),
			Some(140),
			"the departed node's contribution stays in the total",
		);
	}

	#[tokio::test(start_paused = true)]
	async fn restarted_producer_adds_a_new_epoch() {
		// A restarted node announces a new epoch with fresh counters. The new
		// epoch adds to the old one's kept contribution instead of regressing
		// the total, and the old epoch folds into the retired total once the
		// grace elapses.
		let origin = produce_origin();
		let node_a = node_producer(&origin, "a");
		let node_b = node_producer(&origin, "b");

		let fa = feed(&node_a, Tier::default(), "acme", "acme/room", 100).await;
		let _fb = feed(&node_b, Tier::default(), "acme", "acme/room", 40).await;
		drive_tick().await;

		let agg = Consumer::new(origin.consume(), Config::new().with_grace(GRACE));
		let mut traffic = traffic(&agg);
		read_until_bytes(&mut traffic, "acme/room", 140).await;

		drop(fa);
		drop(node_a);
		drive_tick().await;

		let node_a = node_producer(&origin, "a");
		let _fa = feed(&node_a, Tier::default(), "acme", "acme/room", 30).await;
		drive_tick().await;

		let frame = read_monotonic_until(&mut traffic, "acme/room", 140, 170).await;
		assert_eq!(frame.get("acme/room").expect("entry").bytes, 170, "100 + 30 + 40");
		assert_eq!(traffic.inner.nodes.len(), 3, "the old epoch lingers for its grace");

		tokio::time::advance(GRACE).await;
		settle(&mut traffic).await;
		assert_eq!(traffic.inner.nodes.len(), 2, "the old epoch folded");
		assert_eq!(retired_bytes(&traffic, "acme/room"), Some(100));
	}

	#[tokio::test(start_paused = true)]
	async fn replaced_epoch_keeps_its_own_counters() {
		// A node restarts while its old instance is still announced. The newer
		// epoch replaces the old one at the same path; the old entry keeps its
		// last contribution and never reads the new epoch, so nothing counts
		// twice while the old instance keeps publishing.
		let origin = produce_origin();
		let old_a = node_producer(&origin, "a");
		let node_b = node_producer(&origin, "b");

		let _old_fa = feed(&old_a, Tier::default(), "acme", "acme/room", 100).await;
		let _fb = feed(&node_b, Tier::default(), "acme", "acme/room", 40).await;
		drive_tick().await;

		let agg = Consumer::new(origin.consume(), Config::new().with_grace(GRACE));
		let mut traffic = traffic(&agg);
		read_until_bytes(&mut traffic, "acme/room", 140).await;

		let new_a = node_producer(&origin, "a");
		let _new_fa = feed(&new_a, Tier::default(), "acme", "acme/room", 30).await;
		drive_tick().await;

		let frame = read_monotonic_until(&mut traffic, "acme/room", 140, 170).await;
		assert_eq!(frame.get("acme/room").expect("entry").bytes, 170, "100 + 30 + 40");

		for _ in 0..3 {
			drive_tick().await;
			if let Some(frame) = settle(&mut traffic).await {
				assert_eq!(frame.get("acme/room").expect("entry").bytes, 170, "counted once");
			}
		}
		assert_eq!(traffic.inner.nodes.len(), 3, "one entry per epoch");
	}

	#[test]
	fn a_wire_only_drop_starts_a_new_presence_segment() {
		for received in [false, true] {
			let (mut merged, key) = hand_merged::<Presence>();
			let value = |wire| {
				let mut presence = Presence::default();
				presence.sessions_started = 1;
				if received {
					presence.wire_rx = wire;
				} else {
					presence.wire_tx = wire;
				}
				presence
			};
			let node = merged.nodes.get_mut(&key).unwrap();
			node.observe(BTreeMap::from([(String::new(), value(10))]));
			node.observe(BTreeMap::from([(String::new(), value(3))]));
			let combined = &merged.merged()[""];
			assert_eq!(if received { combined.wire_rx } else { combined.wire_tx }, 13);
			assert_eq!(combined.active(), 1, "retiring closes the earlier segment");
		}
	}

	#[test]
	fn outgoing_epoch_metadata_migrates_without_choosing_a_grace_entry() {
		let origin = produce_origin();
		let path = Path::from_static(".stats/node/a");
		let a: Epoch = "01900000-0000-7000-8000-000000000001".parse().unwrap();
		let b: Epoch = "01900000-0000-7000-8000-000000000002".parse().unwrap();
		let c: Epoch = "01900000-0000-7000-8000-000000000003".parse().unwrap();
		let d: Epoch = "01900000-0000-7000-8000-000000000004".parse().unwrap();
		let mut merged =
			Merged::<Keyed<Traffic>>::new(origin.consume(), &Config::new(), "traffic".into(), Encoding::Plain);
		let update = |epoch: Option<Epoch>| moq_net::announce::Announce {
			prefix: path.clone(),
			captures: None,
			route: {
				let mut route = origin::Route::default();
				route.epoch = epoch;
				route
			},
		};
		for (epoch, bytes) in [(a.clone(), 100), (b.clone(), 30), (c.clone(), 7)] {
			merged.apply_announce(update(Some(epoch.clone())), true);
			merged
				.nodes
				.get_mut(&(path.clone(), Some(epoch.clone())))
				.unwrap()
				.shape
				.last = Some(BTreeMap::from([("room".into(), {
				let mut traffic = Traffic::default();
				traffic.bytes = bytes;
				traffic
			})]));
			merged.apply_announce(update(Some(epoch)), false);
		}
		assert_eq!(merged.merged()["room"].bytes, 137);
		merged.apply_announce(update(None), true);
		assert_eq!(
			merged.nodes.len(),
			3,
			"only the immediately outgoing C becomes unversioned"
		);
		assert!(merged.nodes.contains_key(&(path.clone(), Some(a.clone()))));
		assert!(merged.nodes.contains_key(&(path.clone(), Some(b.clone()))));
		assert!(!merged.nodes.contains_key(&(path.clone(), Some(c.clone()))));
		assert_eq!(
			merged.merged()["room"].bytes,
			137,
			"metadata loss preserves the last snapshot"
		);

		// A fresh unversioned snapshot replaces only the outgoing contribution.
		merged.nodes.get_mut(&(path.clone(), None)).unwrap().shape.last = Some(BTreeMap::from([("room".into(), {
			let mut traffic = Traffic::default();
			traffic.bytes = 8;
			traffic
		})]));
		merged.apply_announce(update(None), false);
		merged.apply_announce(update(Some(a.clone())), true);
		assert_eq!(
			merged.nodes.len(),
			3,
			"recovering A retains both outgoing C and grace B"
		);
		assert!(merged.nodes.contains_key(&(path.clone(), Some(c))));
		assert_eq!(
			merged.nodes[&(path.clone(), Some(a.clone()))]
				.shape
				.last
				.as_ref()
				.unwrap()["room"]
				.bytes,
			100
		);
		assert_eq!(
			merged.merged()["room"].bytes,
			138,
			"retained A is not overwritten by outgoing C"
		);

		merged.apply_announce(update(Some(a.clone())), false);
		merged.apply_announce(update(None), true);
		merged.apply_announce(update(None), false);
		merged.apply_announce(update(Some(d.clone())), true);
		assert_eq!(
			merged.nodes.len(),
			4,
			"a distinct recovered epoch gets its own contribution"
		);
		assert!(merged.nodes.contains_key(&(path.clone(), Some(a))));
		assert!(merged.nodes.contains_key(&(path.clone(), Some(d))));
		assert!(!merged.nodes.contains_key(&(path, None)));
		assert_eq!(
			merged.merged()["room"].bytes,
			138,
			"known grace entries remain counted once each"
		);
	}

	/// A known replacement must not overwrite the outgoing known epoch's counters.
	#[test]
	fn replacement_between_announce_drain_and_reader_retry() {
		replacement_while_reader_retries(false);
	}

	/// Missing metadata must not let the outgoing lineage consume a recovered known epoch.
	#[test]
	fn unknown_replacement_between_announce_drain_and_reader_retry() {
		replacement_while_reader_retries(true);
	}

	/// Arrange replacement on another worker after announcements drain and before retrying.
	fn replacement_while_reader_retries(drop_epoch: bool) {
		use std::sync::Barrier;

		let now = moq_clock::now();
		// The clock never advances here, so nothing may wait out the update hold.
		let mut config = origin::Config::default();
		config.update_hold = std::time::Duration::ZERO;
		let (origin, mut driver) = origin::Producer::new(config);
		let epoch_a: Epoch = "01900000-0000-7000-8000-000000000001".parse().unwrap();
		let epoch_b: Epoch = "01900000-0000-7000-8000-000000000002".parse().unwrap();
		let mut old = NodeBroadcast::new(&origin, "acme", "a");
		old.source
			.announce(origin::Route::default().with_epoch(epoch_a.clone()))
			.unwrap();
		old.publish("acme/room", 100);
		let name = group_track(moq_net::Path::from_static("acme"));
		let mut merged = Merged::<Grouped>::new(origin.consume(), &Config::new(), name.clone(), Encoding::Plain);
		let initial = futures::executor::block_on(kio::wait(|waiter| {
			driver.poll(now, waiter).unwrap();
			merged.poll_next(waiter)
		}))
		.unwrap()
		.unwrap();
		assert_eq!(initial[""].paths["acme/room"].publisher.bytes, 100);
		if drop_epoch {
			old.source.announce(origin::Route::default()).unwrap();
			let unversioned = futures::executor::block_on(kio::wait(|waiter| {
				driver.poll(now, waiter).unwrap();
				merged.poll_next(waiter)
			}))
			.unwrap()
			.unwrap();
			assert_eq!(unversioned[""].paths["acme/room"].publisher.bytes, 100);
			assert!(matches!(
				merged.nodes[&(Path::from_static(".stats/node/a"), None)].reader,
				Reader::Active(_)
			));
		}
		assert!(merged.announce.poll_next(&Waiter::noop()).is_pending());

		let ready = Barrier::new(2);
		let release = Barrier::new(2);
		std::thread::scope(|scope| {
			let worker = scope.spawn(|| {
				let mut new = NodeBroadcast::new(&origin, "acme", "a");
				new.source
					.announce(origin::Route::default().with_epoch(epoch_b.clone()))
					.unwrap();
				new.publish("acme/room", 30);
				let mut pending = origin.consume().request_broadcast(".stats/node/a", None);
				let front = futures::executor::block_on(kio::wait(|waiter| {
					driver.poll(now, waiter).unwrap();
					pending.poll_ok(waiter)
				}))
				.unwrap();
				let pending = front.track(&name).unwrap().subscribe(None);
				let subscriber = futures::executor::block_on(kio::wait(|waiter| {
					driver.poll(now, waiter).unwrap();
					pending.poll_ok(waiter)
				}))
				.unwrap();
				let mut warm = moq_json::snapshot::Consumer::<GroupFrame>::new(subscriber, Default::default());
				let frame = futures::executor::block_on(kio::wait(|waiter| {
					driver.poll(now, waiter).unwrap();
					warm.poll_latest(waiter)
				}))
				.unwrap()
				.unwrap();
				assert_eq!(frame.value.tiers[""].paths["acme/room"].publisher.bytes, 30);
				ready.wait();
				release.wait();
				(new, front, warm, driver)
			});
			ready.wait();
			let key = (Path::from_static(".stats/node/a"), (!drop_epoch).then_some(epoch_a));
			let node = merged.nodes.get_mut(&key).unwrap();
			advance(node, &merged.origin, &merged.config, &name, &Waiter::noop());
			let paths = node.shape.paths.last.as_ref().unwrap();
			let old_bytes = paths[&(String::new(), "acme/room".to_string())].publisher.bytes;
			// Release before asserting, so a failed regression also joins its worker.
			release.wait();
			let (_new, _front, _warm, mut driver) = worker.join().unwrap();
			let combined = futures::executor::block_on(kio::wait(|waiter| {
				driver.poll(now, waiter).unwrap();
				merged.poll_next(waiter)
			}))
			.unwrap()
			.unwrap();
			assert_eq!(combined[""].paths["acme/room"].publisher.bytes, 130);
			assert_eq!(old_bytes, 100, "the replacement frame overwrote the old epoch");
		});
	}

	#[tokio::test(start_paused = true)]
	async fn unversioned_restart_regresses_the_total() {
		// A node whose route carries no epoch (an older producer, or a session
		// older than moq-lite 07) returns at the same path with a fresh counter:
		// it replaces its contribution wholesale, and the total regresses once,
		// the fresh-segment contract.
		let origin = produce_origin();
		let mut node_a = NodeBroadcast::new(&origin, "acme", "a");
		let mut node_b = NodeBroadcast::new(&origin, "acme", "b");
		node_a.publish("acme/room", 100);
		node_b.publish("acme/room", 40);

		let agg = Consumer::new(origin.consume(), Config::new());
		let mut traffic = traffic(&agg);
		read_until_bytes(&mut traffic, "acme/room", 140).await;

		drop(node_a);
		settle(&mut traffic).await;
		let mut node_a = NodeBroadcast::new(&origin, "acme", "a");
		node_a.publish("acme/room", 30);

		// An earlier frame may retire A's live gauges first.
		loop {
			let frame = traffic.next().await.expect("read").expect("frame");
			if frame.get("acme/room").map(|t| t.bytes) == Some(70) {
				break;
			}
		}
	}

	#[tokio::test(start_paused = true)]
	async fn reader_failure_keeps_the_traffic_total() {
		// A node's reader failing while its broadcast is still announced keeps
		// its last contribution in the total.
		let origin = produce_origin();
		let mut node_a = NodeBroadcast::new(&origin, "acme", "a");
		let mut node_b = NodeBroadcast::new(&origin, "acme", "b");
		node_a.publish("acme/room", 100);
		node_b.publish("acme/room", 40);

		let agg = Consumer::new(origin.consume(), Config::new());
		let mut traffic = traffic(&agg);
		read_until_bytes(&mut traffic, "acme/room", 140).await;

		// Node A's subscription errors under its still-announced broadcast.
		node_a.fail_traffic();

		// Node B reports more traffic; node A's kept 100 stays in the total.
		node_b.publish("acme/other", 10);

		let frame = read_until_bytes(&mut traffic, "acme/other", 10).await;
		assert_eq!(
			frame.get("acme/room").map(|t| t.bytes),
			Some(140),
			"the failed node's contribution stays in the total",
		);
	}

	/// Let the origin deliver pending announces, then poll `traffic` once so it
	/// stamps departures and folds expired nodes. Returns a frame if one was
	/// ready.
	async fn settle(traffic: &mut TrafficConsumer) -> Option<TrafficFrame> {
		use futures::FutureExt;
		for _ in 0..8 {
			tokio::task::yield_now().await;
		}
		traffic
			.next()
			.now_or_never()
			.map(|frame| frame.expect("read").expect("frame"))
	}

	/// Node A (100) and node B (40) merged to 140, then node A departs and the
	/// aggregator notices. Returns A's frame so a test can bring it back with its
	/// counters intact, plus the still-live node B.
	async fn depart_node_a(origin: &origin::Producer, traffic: &mut TrafficConsumer) -> (TrafficFrame, NodeBroadcast) {
		let mut node_a = NodeBroadcast::new(origin, "acme", "a");
		let mut node_b = NodeBroadcast::new(origin, "acme", "b");
		node_a.publish("acme/room", 100);
		node_b.publish("acme/room", 40);
		read_until_bytes(traffic, "acme/room", 140).await;

		let frame = node_a.frame.clone();
		drop(node_a);
		settle(traffic).await;
		assert_eq!(traffic.inner.nodes.len(), 2, "a departed node lingers for its grace");
		(frame, node_b)
	}

	const GRACE: Duration = Duration::from_secs(60);

	#[tokio::test(start_paused = true)]
	async fn return_within_grace_resumes_counters() {
		// A node returning within the grace with its counters intact replaces
		// its own kept contribution: no traffic counts twice.
		let origin = produce_origin();
		let agg = Consumer::new(origin.consume(), Config::new().with_grace(GRACE));
		let mut traffic = traffic(&agg);
		let (frame, _node_b) = depart_node_a(&origin, &mut traffic).await;

		tokio::time::advance(GRACE - Duration::from_secs(1)).await;
		let mut node_a = NodeBroadcast::new(&origin, "acme", "a");
		node_a.frame = frame;
		node_a.publish("acme/room", 20);

		let frame = read_monotonic_until(&mut traffic, "acme/room", 140, 160).await;
		assert_eq!(frame.get("acme/room").expect("entry").bytes, 160, "100 + 20 + 40");
		assert!(traffic.inner.retired.is_empty(), "nothing folded");
	}

	#[tokio::test(start_paused = true)]
	async fn return_after_grace_counts_twice() {
		// After the grace, a departed node folds into the retired total and its
		// entry is dropped. If the same path then returns with its counters
		// intact, its pre-departure traffic counts twice: the accepted, bounded
		// cost of forgetting it. The total still never regresses.
		let origin = produce_origin();
		let agg = Consumer::new(origin.consume(), Config::new().with_grace(GRACE));
		let mut traffic = traffic(&agg);
		let (frame, _node_b) = depart_node_a(&origin, &mut traffic).await;

		tokio::time::advance(GRACE).await;
		assert!(
			settle(&mut traffic).await.is_none(),
			"a fold leaves the merged view alone"
		);
		assert_eq!(traffic.inner.nodes.len(), 1, "the departed node folded");
		assert_eq!(retired_bytes(&traffic, "acme/room"), Some(100));

		let mut node_a = NodeBroadcast::new(&origin, "acme", "a");
		node_a.frame = frame;
		node_a.publish("acme/room", 20);

		let frame = read_monotonic_until(&mut traffic, "acme/room", 140, 260).await;
		assert_eq!(
			frame.get("acme/room").expect("entry").bytes,
			260,
			"retired 100 + returned 120 + 40"
		);
	}

	#[tokio::test(start_paused = true)]
	async fn restart_after_grace_adds_to_the_total() {
		// A node returning after the grace with fresh counters (it restarted)
		// adds to the retired total instead of regressing it. No poll runs
		// between the grace elapsing and the return, so the return itself must
		// fold the expired entry.
		let origin = produce_origin();
		let agg = Consumer::new(origin.consume(), Config::new().with_grace(GRACE));
		let mut traffic = traffic(&agg);
		let (_frame, _node_b) = depart_node_a(&origin, &mut traffic).await;

		tokio::time::advance(GRACE).await;

		let mut node_a = NodeBroadcast::new(&origin, "acme", "a");
		node_a.publish("acme/room", 30);

		let frame = read_monotonic_until(&mut traffic, "acme/room", 140, 170).await;
		assert_eq!(frame.get("acme/room").expect("entry").bytes, 170, "100 + 30 + 40");
	}

	#[tokio::test(start_paused = true)]
	async fn failed_reader_does_not_fold_while_announced() {
		// A node whose reader failed under a still-announced broadcast keeps its
		// own entry past the grace: only an unannounced node is forgotten.
		let origin = produce_origin();
		let mut node_a = NodeBroadcast::new(&origin, "acme", "a");
		node_a.publish("acme/room", 100);

		let agg = Consumer::new(origin.consume(), Config::new().with_grace(GRACE));
		let mut traffic = traffic(&agg);
		read_until_bytes(&mut traffic, "acme/room", 100).await;

		node_a.fail_traffic();
		settle(&mut traffic).await;
		tokio::time::advance(GRACE * 2).await;
		settle(&mut traffic).await;
		assert_eq!(traffic.inner.nodes.len(), 1, "still announced, so not folded");
		assert!(traffic.inner.retired.is_empty());
	}

	#[tokio::test(start_paused = true)]
	async fn churn_keeps_nodes_bounded() {
		// Distinct nodes come and go, as epochs make every restart do. The
		// node map holds only the live node plus those within their grace, while
		// the merged total stays exact and never regresses.
		const CHURN: u64 = 64;
		const STEP: Duration = Duration::from_secs(15);

		let origin = produce_origin();
		let agg = Consumer::new(origin.consume(), Config::new().with_grace(GRACE));
		let mut traffic = traffic(&agg);

		for i in 0..CHURN {
			let mut node = NodeBroadcast::new(&origin, "acme", &format!("n{i}"));
			node.publish("acme/room", 10);
			let want = 10 * (i + 1);
			let frame = read_monotonic_until(&mut traffic, "acme/room", 10 * i, want).await;
			assert_eq!(frame.get("acme/room").expect("entry").bytes, want);

			drop(node);
			settle(&mut traffic).await;
			tokio::time::advance(STEP).await;

			// Departures within the last grace, plus one that came due since the
			// last poll.
			let bound = (GRACE.as_secs() / STEP.as_secs() + 1) as usize;
			assert!(
				traffic.inner.nodes.len() <= bound,
				"{} nodes after {} departures",
				traffic.inner.nodes.len(),
				i + 1
			);
		}

		tokio::time::advance(GRACE).await;
		settle(&mut traffic).await;
		assert!(traffic.inner.nodes.is_empty(), "every departed node folded");
		assert_eq!(retired_bytes(&traffic, "acme/room"), Some(10 * CHURN));
	}

	#[tokio::test(start_paused = true)]
	async fn unannounce_retires_live_gauges() {
		// A departed node's totals stay in the merged view, but its live gauges
		// retire: the aggregate must not show phantom viewers or broadcasts for
		// a node that is gone.
		let origin = produce_origin();
		let mut node_a = NodeBroadcast::new(&origin, "acme", "a");

		let mut published = Traffic::default();
		published.announces_started = 2;
		published.announces_ended = 1;
		published.broadcasts_started = 3;
		published.broadcasts_ended = 1;
		published.subscriptions_started = 4;
		published.subscriptions_ended = 1;
		published.bytes = 100;
		node_a.frame.insert("acme/room".to_string(), published);
		node_a.publish("acme/room", 0);

		let agg = Consumer::new(origin.consume(), Config::new());
		let mut traffic = traffic(&agg);
		let frame = read_until_bytes(&mut traffic, "acme/room", 100).await;
		let snap = frame.get("acme/room").expect("entry");
		assert!(snap.announces_started > snap.announces_ended);
		assert_eq!(snap.active_broadcasts(), 2);
		assert_eq!(snap.active_subscriptions(), 3);

		// The node departs with those sessions still open.
		drop(node_a);

		// The totals stay; the live gauges retire.
		let frame = traffic.next().await.expect("read").expect("frame");
		let snap = frame.get("acme/room").expect("entry");
		assert_eq!(snap.bytes, 100, "cumulative totals stay");
		assert_eq!(snap.announces_started, snap.announces_ended, "no phantom announcement");
		assert_eq!(snap.active_broadcasts(), 0, "no phantom broadcasts");
		assert_eq!(snap.active_subscriptions(), 0, "no phantom subscriptions");
	}

	#[tokio::test(start_paused = true)]
	async fn merges_sessions_across_nodes() {
		// Session presence sums per stats label across nodes.
		let origin = produce_origin();
		let node_a = node_producer(&origin, "a");
		let node_b = node_producer(&origin, "b");

		// Each node needs a live broadcast to announce; the sessions ride the same
		// group.
		let _fa = feed(&node_a, Tier::default(), "acme", "acme/room", 8).await;
		let _fb = feed(&node_b, Tier::default(), "acme", "acme/room", 8).await;
		let _sa = node_a
			.registry()
			.tier(Tier::default())
			.session(moq_net::Path::from_static("acme"));
		let _sb = node_b
			.registry()
			.tier(Tier::default())
			.session(moq_net::Path::from_static("acme"));
		drive_tick().await;

		let agg = Consumer::new(origin.consume(), Config::new());
		let mut sessions = sessions(&agg);

		loop {
			let frame = sessions.next().await.expect("read").expect("frame");
			// Each feed opens one session ("acme" label) plus the explicit ones,
			// summed across both nodes.
			if frame.get("acme").map(|p| p.active()) >= Some(4) {
				break;
			}
		}
	}

	#[tokio::test(start_paused = true)]
	async fn unannounce_drops_sessions_immediately() {
		// Unlike traffic, the sessions track is a gauge: a node's unannounce must stop
		// counting its sessions immediately, not pin a stale gauge.
		let origin = produce_origin();
		let node_a = node_producer(&origin, "a");
		let node_b = node_producer(&origin, "b");

		let _fa = feed(&node_a, Tier::default(), "acme", "acme/room", 8).await;
		let fb = feed(&node_b, Tier::default(), "acme", "acme/room", 8).await;
		let _sa = node_a
			.registry()
			.tier(Tier::default())
			.session(moq_net::Path::from_static("acme"));
		let sb = node_b
			.registry()
			.tier(Tier::default())
			.session(moq_net::Path::from_static("acme"));
		drive_tick().await;

		let agg = Consumer::new(origin.consume(), Config::new());
		let mut sessions = sessions(&agg);

		loop {
			let frame = sessions.next().await.expect("read").expect("frame");
			// Each feed opens one session ("acme" label) plus the explicit ones,
			// summed across both nodes.
			if frame.get("acme").map(|p| p.active()) >= Some(4) {
				break;
			}
		}

		// Node B departs: its sessions leave the gauge at once.
		drop(fb);
		drop(sb);
		drop(node_b);
		drive_tick().await;

		let frame = sessions.next().await.expect("read").expect("frame");
		let acme = frame.get("acme").expect("node a's sessions");
		assert_eq!(acme.active(), 2, "presence drops immediately");
		assert_eq!(acme.sessions_started, 2, "the departed node's count leaves with it");
	}

	/// Read merged frames until `done` accepts one, asserting `monotonic` holds
	/// between each frame and the one before.
	async fn read_until<V: Mergeable + std::fmt::Debug>(
		merged: &mut impl Frames<V>,
		monotonic: impl Fn(&V, &V) -> bool,
		done: impl Fn(&BTreeMap<String, V>) -> bool,
	) -> BTreeMap<String, V> {
		let mut prev: Option<BTreeMap<String, V>> = None;
		loop {
			let frame = merged.frame().await;
			if let Some(prev) = &prev {
				for (key, before) in prev {
					let after = frame.get(key).cloned().unwrap_or_default();
					assert!(monotonic(before, &after), "{key} regressed: {before:?} -> {after:?}");
				}
			}
			if done(&frame) {
				return frame;
			}
			prev = Some(frame);
		}
	}

	#[tokio::test(start_paused = true)]
	async fn merges_totals_and_keeps_a_departed_node() {
		// The totals sum per tier across nodes, and a departed node keeps its
		// cumulative counters with its sessions closed.
		let origin = produce_origin();
		let node_a = node_producer(&origin, "a");
		let node_b = node_producer(&origin, "b");
		let _fa = feed(&node_a, Tier::default(), "acme", "acme/room", 100).await;
		let fb = feed(&node_b, Tier::default(), "acme", "acme/room", 40).await;
		drive_tick().await;

		let agg = Consumer::new(origin.consume(), Config::new());
		let mut totals = agg.totals();
		let grew = |before: &Totals, after: &Totals| !after.regressed(before);
		let frame = read_until(&mut totals.inner, grew, |frame| {
			frame
				.get("")
				.is_some_and(|t| t.publisher.bytes == 140 && t.sessions.active() == 2)
		})
		.await;
		assert_eq!(frame[""].publisher.subscriptions_started, 2);

		drop(fb);
		drop(node_b);
		drive_tick().await;
		let frame = read_until(&mut totals.inner, grew, |frame| frame[""].sessions.active() == 1).await;
		assert_eq!(frame[""].publisher.bytes, 140, "the departed node's bytes stay");
		assert_eq!(frame[""].sessions.sessions_started, 2);
		assert_eq!(frame[""].publisher.active_subscriptions(), 1, "no phantom subscription");
	}

	#[tokio::test(start_paused = true)]
	async fn a_relisted_group_adds_its_new_epoch() {
		// A group's track that lingers out and is listed again is a new instance
		// counted from zero under a new epoch: the reader follows the listing to
		// it, and its totals add to the old instance's instead of regressing.
		let origin = produce_origin();
		let node = Producer::new(
			produce::Config::new()
				.with_origin(origin.clone())
				.with_node(Path::from_static("a"))
				.with_depth(1)
				.with_linger(Duration::ZERO),
		);
		let agg = Consumer::new(origin.consume(), Config::new());
		let mut group = agg.group(moq_net::Path::from_static("acme"));

		let session = node
			.registry()
			.tier(Tier::default())
			.session(moq_net::Path::from_static("acme/live"));
		drive_tick().await;
		let mut started = 0;
		let mut read_until = async |want: u64, active: u64| loop {
			let frame = group.next().await.expect("read").expect("frame");
			let sessions = frame.get("").map(|group| group.sessions).unwrap_or_default();
			assert!(sessions.sessions_started >= started, "regressed: {sessions:?}");
			started = sessions.sessions_started;
			if (started, sessions.active()) == (want, active) {
				return;
			}
		};
		read_until(1, 1).await;

		// Empty past the zero linger: the track finishes and is unlisted.
		drop(session);
		for _ in 0..3 {
			drive_tick().await;
		}
		let _session = node
			.registry()
			.tier(Tier::default())
			.session(moq_net::Path::from_static("acme/live"));
		drive_tick().await;
		read_until(2, 1).await;
	}

	/// A hand-published node broadcast at `.stats/node/<node>` with a plain
	/// totals track and no epoch, so a new one at the same path is that node
	/// restarting without telling the reader.
	struct TotalsNode {
		_source: origin::Publication,
		totals: moq_json::snapshot::Producer<TotalsFrame>,
	}

	impl TotalsNode {
		fn new(origin: &origin::Producer, node: &str) -> Self {
			let source = origin
				.create_broadcast(format!(".stats/node/{node}").as_str())
				.expect("create broadcast");
			source.announce(origin::Route::default()).expect("announce");
			let track = source
				.create_track(totals_track(Encoding::Plain), timed())
				.expect("create track");
			let config = moq_json::snapshot::Config::default().with_deltas(false);
			Self {
				_source: source,
				totals: moq_json::snapshot::Producer::new(track, config),
			}
		}

		/// Publish the default tier's cumulative byte count.
		fn publish(&mut self, bytes: u64) {
			let mut totals = Totals::default();
			totals.publisher.bytes = bytes;
			self.totals
				.update(
					moq_net::Timed::from(&TotalsFrame::from([(String::new(), totals)])).at(moq_net::Timestamp::now()),
				)
				.expect("publish");
		}
	}

	#[tokio::test(start_paused = true)]
	async fn totals_never_regress_on_an_unversioned_restart() {
		// A node returning without an epoch with a lower counter reset: its old
		// readout becomes a baseline, so the merged totals keep growing.
		let origin = produce_origin();
		let mut node_a = TotalsNode::new(&origin, "a");
		let mut node_b = TotalsNode::new(&origin, "b");
		node_a.publish(100);
		node_b.publish(40);

		let agg = Consumer::new(origin.consume(), Config::new());
		let mut totals = agg.totals();
		let grew = |before: &Totals, after: &Totals| after.publisher.bytes >= before.publisher.bytes;
		read_until(&mut totals.inner, grew, |frame| frame[""].publisher.bytes == 140).await;

		drop(node_a);
		let mut node_a = TotalsNode::new(&origin, "a");
		node_a.publish(30);
		read_until(&mut totals.inner, grew, |frame| frame[""].publisher.bytes == 170).await;

		// It keeps counting from its new readout.
		node_a.publish(35);
		read_until(&mut totals.inner, grew, |frame| frame[""].publisher.bytes == 175).await;
	}

	/// A node announced at `.stats/node/a` with no epoch, ready to observe
	/// frames by hand.
	fn hand_merged<V: Mergeable>() -> (Merged<Keyed<V>>, (Path, Option<Epoch>)) {
		let origin = produce_origin();
		let path = Path::from_static(".stats/node/a");
		let mut merged = Merged::<Keyed<V>>::new(origin.consume(), &Config::new(), "p".into(), Encoding::Plain);
		merged.apply_announce(
			moq_net::announce::Announce {
				prefix: path.clone(),
				captures: None,
				route: origin::Route::default(),
			},
			true,
		);
		(merged, (path, None))
	}

	#[test]
	fn outcomes_survive_node_departure_resume_and_counter_only_reset() {
		let (mut merged, key) = hand_merged::<Presence>();
		let sample = |refused, failed| {
			let mut value = Presence::default();
			value.sessions_refused.forbidden = refused;
			value.sessions_failed.transport = failed;
			value
		};
		let path = Path::from_static(".stats/node/b");
		merged.apply_announce(
			moq_net::announce::Announce {
				prefix: path.clone(),
				captures: None,
				route: origin::Route::default(),
			},
			true,
		);
		let second = (path, None);
		merged
			.nodes
			.get_mut(&second)
			.unwrap()
			.observe(BTreeMap::from([(String::new(), sample(2, 1))]));

		merged
			.nodes
			.get_mut(&key)
			.unwrap()
			.observe(BTreeMap::from([(String::new(), sample(5, 3))]));
		merged.nodes.get_mut(&key).unwrap().depart();
		assert_eq!(merged.merged()[""], sample(7, 4));
		merged
			.nodes
			.get_mut(&key)
			.unwrap()
			.observe(BTreeMap::from([(String::new(), sample(5, 3))]));
		assert_eq!(
			merged.merged()[""],
			sample(7, 4),
			"same epoch does not count old errors again"
		);
		merged
			.nodes
			.get_mut(&key)
			.unwrap()
			.observe(BTreeMap::from([(String::new(), sample(6, 4))]));
		merged
			.nodes
			.get_mut(&key)
			.unwrap()
			.observe(BTreeMap::from([(String::new(), sample(2, 1))]));
		assert_eq!(
			merged.merged()[""],
			sample(10, 6),
			"outcome-only reset starts a fresh segment"
		);
	}

	#[test]
	fn a_reset_closes_the_old_open_pairs() {
		let (mut merged, key) = hand_merged::<Presence>();
		let presence = |started, ended| {
			let mut presence = Presence::default();
			presence.sessions_started = started;
			presence.sessions_ended = ended;
			presence
		};
		let node = merged.nodes.get_mut(&key).expect("node");
		node.observe(BTreeMap::from([(String::new(), presence(5, 3))]));
		node.observe(BTreeMap::from([(String::new(), presence(6, 4))]));
		assert_eq!(merged.merged()[""], presence(6, 4), "growth is not a reset");

		// Held again from zero: the two sessions open before the reset ended.
		let node = merged.nodes.values_mut().next().expect("node");
		node.observe(BTreeMap::from([(String::new(), presence(1, 0))]));
		assert_eq!(merged.merged()[""], presence(7, 6));
	}

	#[test]
	fn request_totals_survive_departure_resume_and_request_only_reset() {
		let (mut merged, key) = hand_merged::<Presence>();
		let request = |requests| {
			let mut presence = Presence::default();
			presence.requests = requests;
			presence
		};
		merged
			.nodes
			.get_mut(&key)
			.unwrap()
			.observe(BTreeMap::from([(String::new(), request(5))]));
		merged.nodes.get_mut(&key).unwrap().depart();
		assert_eq!(merged.merged()[""], request(5));
		merged
			.nodes
			.get_mut(&key)
			.unwrap()
			.observe(BTreeMap::from([(String::new(), request(5))]));
		assert_eq!(
			merged.merged()[""],
			request(5),
			"resuming does not admit requests again"
		);
		merged
			.nodes
			.get_mut(&key)
			.unwrap()
			.observe(BTreeMap::from([(String::new(), request(6))]));
		merged
			.nodes
			.get_mut(&key)
			.unwrap()
			.observe(BTreeMap::from([(String::new(), request(2))]));
		assert_eq!(
			merged.merged()[""],
			request(8),
			"request-only regression begins another segment"
		);
	}

	#[test]
	fn byte_only_regression_begins_another_segment() {
		let (mut merged, key) = hand_merged::<Presence>();
		let bytes = |sent, received| {
			let mut presence = Presence::default();
			presence.sent_bytes = sent;
			presence.received_bytes = received;
			presence
		};
		let node = merged.nodes.get_mut(&key).unwrap();
		node.observe(BTreeMap::from([(String::new(), bytes(6, 4))]));
		node.observe(BTreeMap::from([(String::new(), bytes(2, 1))]));
		assert_eq!(merged.merged()[""], bytes(8, 5));
	}

	#[test]
	fn a_resume_reopens_closed_pairs_without_going_down() {
		let (mut merged, key) = hand_merged::<Presence>();
		let presence = |started, ended| {
			let mut presence = Presence::default();
			presence.sessions_started = started;
			presence.sessions_ended = ended;
			presence
		};
		let node = merged.nodes.get_mut(&key).expect("node");
		node.observe(BTreeMap::from([(String::new(), presence(5, 3))]));
		assert_eq!(merged.merged()[""], presence(5, 3));

		// The node departs: its two open sessions read as ended.
		let node = merged.nodes.get_mut(&key).expect("node");
		node.depart();
		assert_eq!(merged.merged()[""], presence(5, 5));

		// The same epoch resumes with those sessions still open: they start
		// again rather than un-end, so no counter goes down.
		let node = merged.nodes.get_mut(&key).expect("node");
		node.observe(BTreeMap::from([(String::new(), presence(5, 3))]));
		assert_eq!(merged.merged()[""], presence(7, 5));
		let node = merged.nodes.get_mut(&key).expect("node");
		node.observe(BTreeMap::from([(String::new(), presence(6, 4))]));
		assert_eq!(merged.merged()[""], presence(8, 6));

		// A departure followed by a reset folds the closed readout instead.
		let node = merged.nodes.get_mut(&key).expect("node");
		node.depart();
		assert_eq!(merged.merged()[""], presence(8, 8));
		let node = merged.nodes.get_mut(&key).expect("node");
		node.observe(BTreeMap::from([(String::new(), presence(1, 0))]));
		assert_eq!(merged.merged()[""], presence(9, 8));
	}

	#[tokio::test(start_paused = true)]
	async fn traffic_resume_reopens_closed_pairs_without_going_down() {
		let origin = produce_origin();
		let mut node = NodeBroadcast::new(&origin, "acme", "a");
		let mut route = origin::Route::default();
		route.epoch = Some("01900000-0000-7000-8000-000000000001".parse().unwrap());
		node.source.announce(route.clone()).expect("announce epoch");
		let mut raw = Traffic::default();
		raw.announces_started = 5;
		raw.announces_ended = 3;
		raw.broadcasts_started = 4;
		raw.broadcasts_ended = 1;
		raw.subscriptions_started = 6;
		raw.subscriptions_ended = 2;
		raw.bytes = 100;
		node.frame.insert("acme/room".into(), raw);
		node.publish("acme/room", 0);

		let agg = Consumer::new(origin.consume(), Config::new());
		let mut traffic = traffic(&agg);
		let grew = |before: &Traffic, after: &Traffic| !after.regressed(before);
		read_until(&mut traffic, grew, |frame| frame.get("acme/room") == Some(&raw)).await;
		node.source.unannounce();
		let departed = raw.retire();
		read_until(&mut traffic, grew, |frame| {
			assert!(!frame["acme/room"].regressed(&raw));
			frame["acme/room"] == departed
		})
		.await;

		// The same publisher epoch returns with all three pairs still open.
		node.source.announce(route).expect("reannounce epoch");
		let mut expected = raw;
		expected.announces_started = 7;
		expected.announces_ended = 5;
		expected.broadcasts_started = 7;
		expected.broadcasts_ended = 4;
		expected.subscriptions_started = 10;
		expected.subscriptions_ended = 6;
		let resumed = read_until(&mut traffic, grew, |frame| {
			assert!(!frame["acme/room"].regressed(&departed), "{departed:?} -> {frame:?}");
			frame["acme/room"] == expected
		})
		.await;
		let resumed = resumed["acme/room"];
		assert_eq!(resumed.announces_started - resumed.announces_ended, 2);
		assert_eq!(resumed.active_broadcasts(), raw.active_broadcasts());
		assert_eq!(resumed.active_subscriptions(), raw.active_subscriptions());
		assert_eq!(resumed.bytes, raw.bytes, "flow counters are not reopened");

		node.publish("acme/room", 20);
		expected.bytes = 120;
		read_until(&mut traffic, grew, |frame| {
			assert!(!frame["acme/room"].regressed(&resumed));
			frame["acme/room"] == expected
		})
		.await;
	}

	#[test]
	fn a_resume_reopens_only_the_pairs_still_open() {
		let (mut merged, key) = hand_merged::<Traffic>();
		let traffic = |started, ended| {
			let mut traffic = Traffic::default();
			traffic.subscriptions_started = started;
			traffic.subscriptions_ended = ended;
			traffic
		};
		let frame = |started, ended| BTreeMap::from([("room".into(), traffic(started, ended))]);
		let node = merged.nodes.get_mut(&key).expect("node");
		node.observe(frame(5, 3));
		node.depart();
		assert_eq!(merged.merged()["room"], traffic(5, 5));

		// One of the two open subscriptions ended while the node was away.
		let node = merged.nodes.get_mut(&key).expect("node");
		node.observe(frame(5, 4));
		assert_eq!(merged.merged()["room"], traffic(6, 5));
		let node = merged.nodes.get_mut(&key).expect("node");
		node.depart();
		assert_eq!(merged.merged()["room"], traffic(6, 6));

		// Every subscription ended while away: nothing reopens.
		let node = merged.nodes.get_mut(&key).expect("node");
		node.observe(frame(5, 5));
		assert_eq!(merged.merged()["room"], traffic(6, 6));
	}

	#[test]
	fn traffic_reset_discards_reopened_pairs() {
		let (mut merged, key) = hand_merged::<Traffic>();
		let mut raw = Traffic::default();
		raw.subscriptions_started = 4;
		raw.subscriptions_ended = 1;
		raw.bytes = 100;
		let frame = || BTreeMap::from([("room".into(), raw)]);
		let node = merged.nodes.get_mut(&key).expect("node");
		node.observe(frame());
		node.depart();
		node.observe(frame());
		let mut expected = raw;
		expected.subscriptions_started = 7;
		expected.subscriptions_ended = 4;
		assert_eq!(merged.merged()["room"], expected);
		let node = merged.nodes.get_mut(&key).expect("node");
		node.depart();
		node.observe(frame());
		expected.subscriptions_started = 10;
		expected.subscriptions_ended = 7;
		assert_eq!(merged.merged()["room"], expected);

		// A route without an epoch still resets, including after a resume.
		let mut reset = Traffic::default();
		reset.subscriptions_started = 1;
		reset.bytes = 10;
		let node = merged.nodes.get_mut(&key).expect("node");
		node.depart();
		node.observe(BTreeMap::from([("room".into(), reset)]));
		assert_eq!(merged.merged()["room"], reset);

		// Removing a path also resets its counters when it returns.
		let node = merged.nodes.get_mut(&key).expect("node");
		node.depart();
		node.observe(BTreeMap::from([("room".into(), reset)]));
		node.observe(BTreeMap::new());
		assert!(merged.merged().is_empty());
		let node = merged.nodes.get_mut(&key).expect("node");
		node.observe(BTreeMap::from([("room".into(), reset)]));
		assert_eq!(merged.merged()["room"], reset);
	}

	#[test]
	fn a_resume_reopens_the_pairs_inside_totals() {
		let (mut merged, key) = hand_merged::<Totals>();
		let mut totals = Totals::default();
		totals.publisher.bytes = 10;
		totals.publisher.subscriptions_started = 4;
		totals.publisher.subscriptions_ended = 1;
		let node = merged.nodes.get_mut(&key).expect("node");
		node.observe(BTreeMap::from([(String::new(), totals)]));
		node.depart();
		let departed = merged.merged()[""];
		assert_eq!(departed.publisher.subscriptions_ended, 4);

		let node = merged.nodes.get_mut(&key).expect("node");
		node.observe(BTreeMap::from([(String::new(), totals)]));
		let resumed = merged.merged()[""];
		assert!(!resumed.regressed(&departed), "{departed:?} -> {resumed:?}");
		assert_eq!(resumed.publisher.bytes, 10, "flow counters are not reopened");
		assert_eq!(resumed.publisher.active_subscriptions(), 3);
	}

	#[tokio::test(start_paused = true)]
	async fn presence_survives_join_leave_and_restart() {
		// One label held across nodes: a node joining adds its sessions, one
		// leaving closes its sessions but keeps its count, and a restarted node
		// is a new epoch that adds to it. The merged count never goes down.
		let origin = produce_origin();
		let node_a = node_producer(&origin, "a");
		let _fa = feed(&node_a, Tier::default(), "acme", "acme/room", 8).await;
		drive_tick().await;

		let agg = Consumer::new(origin.consume(), Config::new());
		let mut presence = agg.presence(moq_net::Path::from_static("acme"));
		let grew = |before: &Presence, after: &Presence| !after.regressed(before);
		read_until(&mut presence.inner, grew, |frame| {
			frame.get("").is_some_and(|p| p.active() == 1)
		})
		.await;

		// Join.
		let node_b = node_producer(&origin, "b");
		let fb = feed(&node_b, Tier::default(), "acme", "acme/room", 8).await;
		read_until(&mut presence.inner, grew, |frame| frame[""].active() == 2).await;

		// Leave.
		drop(fb);
		drop(node_b);
		let frame = read_until(&mut presence.inner, grew, |frame| frame[""].active() == 1).await;
		assert_eq!(frame[""].sessions_started, 2);

		// Restart: a new epoch at node b's path.
		let node_b = node_producer(&origin, "b");
		let _fb = feed(&node_b, Tier::default(), "acme", "acme/room", 8).await;
		let frame = read_until(&mut presence.inner, grew, |frame| frame[""].active() == 2).await;
		assert_eq!(frame[""].sessions_started, 3);
	}

	#[tokio::test(start_paused = true)]
	async fn prefix_survives_join_leave_and_restart() {
		// One prefix held across nodes: a node joining adds its traffic and its
		// children, one leaving closes its subscriptions and drops its children
		// but keeps its rollup, and a restarted node is a new epoch that adds to
		// it. The merged rollup never goes down.
		let origin = produce_origin();
		let node_a = node_producer(&origin, "a");
		let _fa = feed(&node_a, Tier::default(), "acme", "acme/room", 8).await;
		drive_tick().await;

		let agg = Consumer::new(origin.consume(), Config::new());
		let mut prefix = agg.prefix(moq_net::Path::from_static("acme"));
		let grew = |before: &Prefix, after: &Prefix| !after.regressed(before);
		let live = |count| {
			move |frame: &PrefixFrame| {
				frame
					.get("")
					.is_some_and(|p| p.publisher.active_subscriptions() == count)
			}
		};
		let frame = read_until(&mut prefix.inner, grew, live(1)).await;
		assert_eq!(frame[""].children["room"].publisher.bytes, 8);

		// Join.
		let node_b = node_producer(&origin, "b");
		let fb = feed(&node_b, Tier::default(), "acme", "acme/other", 5).await;
		let frame = read_until(&mut prefix.inner, grew, live(2)).await;
		assert_eq!(frame[""].publisher.bytes, 13);
		assert_eq!(frame[""].children.len(), 2);

		// Leave: node b's child leaves the map, its tail stays in the rollup.
		drop(fb);
		drop(node_b);
		let frame = read_until(&mut prefix.inner, grew, live(1)).await;
		assert_eq!(frame[""].publisher.bytes, 13);
		assert_eq!(frame[""].publisher.subscriptions_started, 2);
		assert!(!frame[""].children.contains_key("other"), "{frame:?}");

		// Restart: a new epoch at node b's path.
		let node_b = node_producer(&origin, "b");
		let _fb = feed(&node_b, Tier::default(), "acme", "acme/other", 3).await;
		let frame = read_until(&mut prefix.inner, grew, |frame| {
			live(2)(frame) && frame[""].publisher.bytes == 16
		})
		.await;
		assert_eq!(frame[""].publisher.subscriptions_started, 3);
		assert_eq!(frame[""].children["other"].publisher.bytes, 3, "the new epoch's child");
	}

	#[test]
	fn a_prefix_reset_keeps_the_rollup_and_drops_the_children() {
		let (mut merged, key) = hand_merged::<Prefix>();
		let prefix = |bytes, child: Option<u64>| {
			let mut prefix = Prefix::default();
			prefix.publisher.bytes = bytes;
			prefix.own.publisher.bytes = bytes;
			if let Some(child) = child {
				let mut usage = Usage::default();
				usage.publisher.bytes = child;
				prefix.children.insert("cam".into(), usage);
			}
			prefix
		};
		let node = merged.nodes.get_mut(&key).expect("node");
		node.observe(BTreeMap::from([(String::new(), prefix(10, Some(4)))]));
		assert_eq!(merged.merged()[""], prefix(10, Some(4)));

		// A child leaving the map is not a reset.
		let node = merged.nodes.get_mut(&key).expect("node");
		node.observe(BTreeMap::from([(String::new(), prefix(10, None))]));
		assert_eq!(merged.merged()[""], prefix(10, None));

		// Held again from zero: the old rollup is a baseline, the children are
		// only the live ones.
		let node = merged.nodes.get_mut(&key).expect("node");
		node.observe(BTreeMap::from([(String::new(), prefix(3, Some(1)))]));
		assert_eq!(merged.merged()[""], {
			let mut expected = prefix(13, Some(1));
			expected.own.publisher.bytes = 13;
			expected
		});

		// A departed node keeps its rollup and drops its children.
		merged.nodes.get_mut(&key).expect("node").depart();
		assert_eq!(merged.merged()[""], {
			let mut expected = prefix(13, None);
			expected.own.publisher.bytes = 13;
			expected
		});
	}
}
