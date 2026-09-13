//! The aggregating half: fold a group's per-node stats broadcasts into one view.
//!
//! A single-broadcast [`Consumer`](crate::Consumer) reads one
//! `<prefix>/<group>/node/<node>` broadcast. This reader watches an origin's
//! announce stream for *every* node broadcast in a group and folds their
//! cumulative counters into one merged frame per `(tier, role)`, so a downstream
//! sees a project's whole live traffic as if it came from a single node.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::task::Poll;

use moq_net::kio::{self, Pending, Waiter};
use moq_net::stats::{Presence, Role, Tier, Traffic};
use moq_net::track::Subscribing;
use moq_net::{PathOwned, origin};

use crate::{Result, SessionsFrame, TrafficFrame, parse_node_path, sessions_track, traffic_track};

/// Configuration for an [`Consumer`]. Construct with [`Config::new`] and chain
/// the `with_*` setters.
///
/// The `prefix` and `depth` must match the producing side's
/// [`ProducerConfig`](crate::ProducerConfig): they are how announced paths are
/// recognized as node broadcasts and filtered from sibling categories under the
/// same prefix.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Config {
	/// Top-level path stats are published under (default `.stats`). Must match
	/// the producer's prefix.
	pub prefix: PathOwned,
	/// The producer's grouping depth (default `0`). Announced paths whose group
	/// is deeper than this are not recognized as node broadcasts and are
	/// skipped. Must match the producer's depth.
	pub depth: usize,
	/// Read the compressed `.json.z` tracks instead of the plain `.json` ones.
	/// Same data for a fraction of the bytes, but requires a producer that
	/// publishes them. Defaults to `false`.
	pub compression: bool,
}

impl Config {
	/// A config with default settings: the `.stats` prefix, depth `0`, and the
	/// plain `.json` tracks.
	pub fn new() -> Self {
		Self::default()
	}

	/// Override the top-level prefix (default `.stats`). Must match the producer.
	pub fn with_prefix(mut self, prefix: impl Into<PathOwned>) -> Self {
		self.prefix = prefix.into();
		self
	}

	/// Override the grouping depth (default `0`). Must match the producer.
	pub fn with_depth(mut self, depth: usize) -> Self {
		self.depth = depth;
		self
	}

	/// Read the compressed `.json.z` tracks instead of the plain `.json` ones.
	pub fn with_compression(mut self, compression: bool) -> Self {
		self.compression = compression;
		self
	}
}

impl Default for Config {
	fn default() -> Self {
		Self {
			prefix: PathOwned::from(".stats"),
			depth: 0,
			compression: false,
		}
	}
}

/// Folds a group's per-node stats broadcasts into one merged view.
///
/// Scope an [`origin::Consumer`] to a single group (e.g. `.stats/<pid>`) and
/// hand it here; each [`Self::traffic`] / [`Self::sessions`] call opens its own
/// announce cursor and subscribes to that track on every node broadcast in the
/// group, summing the cumulative counters per key. Traffic is sticky: a node
/// dropping out (its broadcast unannounces or its reader ends) keeps its last
/// contribution, so a relay that returns with its boot-lifetime counters
/// intact never looks like new traffic. Only a genuine per-node counter
/// regression (a restarted relay) regresses the merged counter, the same reset
/// contract a single node's own restart follows. Presence is not sticky: a
/// departed node stops counting sessions immediately.
pub struct Consumer {
	origin: origin::Consumer,
	config: Config,
}

impl Consumer {
	/// Wrap an origin consumer, ideally already scoped to one group. `config`'s
	/// `prefix` and `depth` must match the producing side.
	pub fn new(origin: origin::Consumer, config: Config) -> Self {
		Self { origin, config }
	}

	/// A merged reader over the traffic track for `(tier, role)`, folding every
	/// node broadcast in the group. Nodes are subscribed lazily as they announce,
	/// so this returns without a handshake.
	pub fn traffic(&self, tier: &Tier, role: Role) -> TrafficConsumer {
		let name = traffic_track(tier, role, self.config.compression);
		TrafficConsumer {
			inner: Merged::new(self.origin.clone(), &self.config, name),
		}
	}

	/// A merged reader over the sessions track for `tier`; see [`Self::traffic`].
	pub fn sessions(&self, tier: &Tier) -> SessionsConsumer {
		let name = sessions_track(tier, self.config.compression);
		SessionsConsumer {
			inner: Merged::new(self.origin.clone(), &self.config, name),
		}
	}
}

/// A merged reader over one traffic track across every node in the group. Yields
/// the latest merged [`TrafficFrame`]; a slow reader collapses intermediate
/// frames, which is safe because the counters are cumulative.
pub struct TrafficConsumer {
	inner: Merged<Traffic>,
}

impl TrafficConsumer {
	/// The next merged frame, or `None` once the announce stream ends (the
	/// source origin went away).
	pub async fn next(&mut self) -> Result<Option<TrafficFrame>> {
		kio::wait(|waiter| self.inner.poll_next(waiter)).await
	}
}

/// A merged reader over one sessions track across every node in the group; see
/// [`TrafficConsumer`].
pub struct SessionsConsumer {
	inner: Merged<Presence>,
}

impl SessionsConsumer {
	/// The next merged frame, or `None` once the announce stream ends.
	pub async fn next(&mut self) -> Result<Option<SessionsFrame>> {
		kio::wait(|waiter| self.inner.poll_next(waiter)).await
	}
}

/// A per-key counter that folds across nodes: the two wire counter types.
trait Mergeable: serde::de::DeserializeOwned + Default + Copy + 'static {
	/// Fold `other` into `acc`.
	fn merge(acc: &mut Self, other: Self);

	/// Whether a node's last contribution survives its departure. Cumulative
	/// counters do: a relay that leaves and returns with its boot-lifetime
	/// counters intact must not look like new traffic. Gauges don't: a
	/// departed node stops counting the moment it's gone.
	const STICKY: bool;

	/// Retire any outstanding live state in a kept contribution, returning
	/// whether it changed. Cumulative totals are untouched; this is how a
	/// departed node stops reporting open counters without losing its history.
	fn retire(&mut self) -> bool;
}

impl Mergeable for Traffic {
	const STICKY: bool = true;

	fn merge(acc: &mut Self, other: Self) {
		acc.add(other);
	}

	/// Close every open counter pair: a departed relay can no longer carry its
	/// broadcasts or subscriptions, so the merged view must not keep counting
	/// them as live. The cumulative counters, bytes included, stay.
	fn retire(&mut self) -> bool {
		let changed = self.announced_closed < self.announced
			|| self.broadcasts_closed < self.broadcasts
			|| self.subscriptions_closed < self.subscriptions;
		self.announced_closed = self.announced_closed.max(self.announced);
		self.broadcasts_closed = self.broadcasts_closed.max(self.broadcasts);
		self.subscriptions_closed = self.subscriptions_closed.max(self.subscriptions);
		changed
	}
}

impl Mergeable for Presence {
	const STICKY: bool = false;

	fn merge(acc: &mut Self, other: Self) {
		acc.add(other);
	}

	/// Presence is not sticky, so it never reaches here; its entry is dropped.
	fn retire(&mut self) -> bool {
		false
	}
}

/// One node's subscription to the merged track.
enum Reader<V: Mergeable> {
	/// Resolving the announced path into a broadcast. `queued` records whether
	/// the request was handed to a serving route (fixed at request time): a
	/// queued request that fails `Unroutable` was killed by its serving route
	/// retracting (the table has already changed), while an unqueued
	/// `Unroutable` means nothing serves the path at all.
	Resolving {
		pending: Pending<origin::Requesting>,
		queued: bool,
	},
	/// Awaiting the subscription handshake.
	Subscribing(Pending<Subscribing>),
	/// Reading frames. Boxed: the snapshot consumer dwarfs the other variants,
	/// and one lives per node in a map.
	Active(Box<moq_json::snapshot::Consumer<BTreeMap<String, V>>>),
	/// The subscription failed or the track ended; the node no longer reads. It
	/// lingers until it unannounces or reannounces, still contributing its last
	/// frame when [`Mergeable::STICKY`].
	Ended,
}

/// One node's reader plus the last frame it produced (the value folded into the
/// merged view).
struct Node<V: Mergeable> {
	reader: Reader<V>,
	/// The announced path, relative to the announce cursor: what a re-resolve
	/// after the reader ends requests again.
	path: PathOwned,
	last: Option<BTreeMap<String, V>>,
}

impl<V: Mergeable> Node<V> {
	/// The node's broadcast went away (unannounced, replaced by one without
	/// this track, or its subscription ended): stop reading, and keep the last
	/// frame only when sticky. A kept frame retires its live counters, so a
	/// departed node stops reporting open state while its totals stay. Returns
	/// whether the merged view changed.
	fn depart(&mut self) -> bool {
		self.reader = Reader::Ended;
		if !V::STICKY {
			return self.last.take().is_some();
		}
		let mut changed = false;
		if let Some(last) = &mut self.last {
			for value in last.values_mut() {
				changed |= value.retire();
			}
		}
		changed
	}
}

/// Watches a group's node announces and folds one track across all of them.
struct Merged<V: Mergeable> {
	/// Resolves announced node paths into broadcasts.
	origin: origin::Consumer,
	announce: moq_net::announce::Consumer,
	prefix: PathOwned,
	depth: usize,
	/// Track name subscribed on each node broadcast.
	name: String,
	config: moq_json::snapshot::ConsumerConfig,
	/// One entry per live node broadcast, keyed by absolute announced path.
	nodes: HashMap<PathOwned, Node<V>>,
}

impl<V: Mergeable> Merged<V> {
	fn new(origin: origin::Consumer, config: &Config, name: String) -> Self {
		Self {
			announce: origin.announced(),
			origin,
			prefix: config.prefix.clone(),
			depth: config.depth,
			name,
			config: moq_json::snapshot::ConsumerConfig::default().with_compression(config.compression),
			nodes: HashMap::new(),
		}
	}

	/// Poll for the next merged frame. Returns `Ready(Some(_))` whenever the
	/// merged view changed (a node produced a frame, or a non-sticky
	/// contribution left), `Ready(None)` once the announce stream closes, else
	/// `Pending`.
	fn poll_next(&mut self, waiter: &Waiter) -> Poll<Result<Option<BTreeMap<String, V>>>> {
		let mut changed = false;

		// Drain announce membership updates: add/replace on announce, drop on
		// unannounce. A closed stream ends the merged view.
		loop {
			match self.announce.poll_next(waiter) {
				Poll::Ready(Some(update)) => changed |= self.apply_announce(update),
				Poll::Ready(None) => return Poll::Ready(Ok(None)),
				Poll::Pending => break,
			}
		}

		// Advance each node's reader, collapsing any backlog to its latest frame.
		let config = &self.config;
		let name = self.name.as_str();
		let origin = &self.origin;
		for node in self.nodes.values_mut() {
			changed |= advance(node, origin, config, name, waiter);
		}

		if changed {
			Poll::Ready(Ok(Some(self.merged())))
		} else {
			Poll::Pending
		}
	}

	/// Apply one announce update to the node set. Returns whether the merged view
	/// changed (only a non-sticky contribution leaving does; a sticky one is
	/// kept).
	fn apply_announce(&mut self, update: moq_net::announce::Update) -> bool {
		let Some(prefix) = update.pattern.as_prefix() else {
			return false;
		};
		let path = moq_net::Path::new(prefix).to_owned();
		let absolute = self.announce.absolute(&path).to_owned();

		// Only fold node-category routes; skip sibling categories a producer
		// may publish under the same prefix. A route names a prefix, and node
		// broadcasts are announced at their exact path by convention.
		if parse_node_path(&self.prefix, self.depth, &absolute).is_none() {
			return false;
		}

		if update.active {
			// A route update on a node already tracked (a reprice, or a takeover
			// with different metadata) keeps the live reader: existing
			// subscriptions survive a takeover, and the reader re-resolves through
			// the current best route the moment it actually ends. Only an ended
			// node re-arms here, since a fresh route is new evidence that a
			// re-resolve could succeed. A sticky contribution carries across the
			// re-arm, holding the total until a fresh frame replaces it.
			match self.nodes.entry(absolute) {
				Entry::Occupied(mut entry) => {
					let node = entry.get_mut();
					if matches!(node.reader, Reader::Ended) {
						node.reader = resolve(&self.origin, &node.path);
					}
					false
				}
				Entry::Vacant(entry) => {
					entry.insert(Node {
						reader: resolve(&self.origin, &path),
						path,
						last: None,
					});
					false
				}
			}
		} else if V::STICKY {
			// Unannounce: keep the cumulative totals, retire the live gauges, and
			// stop reading. The entry stays so a reannounce re-arms it.
			match self.nodes.get_mut(&absolute) {
				Some(node) => node.depart(),
				None => false,
			}
		} else {
			// A gauge drops its contribution, and its entry, so a departed node
			// stops counting and its path is not retained.
			self.nodes.remove(&absolute).is_some_and(|old| old.last.is_some())
		}
	}

	/// Sum every node's last frame, per key.
	fn merged(&self) -> BTreeMap<String, V> {
		let mut acc: BTreeMap<String, V> = BTreeMap::new();
		for node in self.nodes.values() {
			if let Some(last) = &node.last {
				for (key, value) in last {
					V::merge(acc.entry(key.clone()).or_default(), *value);
				}
			}
		}
		acc
	}
}

/// Drive one node's reader as far as it goes, updating its `last` frame. Returns
/// whether that node's contribution to the merged view changed.
fn advance<V: Mergeable>(
	node: &mut Node<V>,
	origin: &origin::Consumer,
	config: &moq_json::snapshot::ConsumerConfig,
	name: &str,
	waiter: &Waiter,
) -> bool {
	let mut changed = false;
	// At most one re-resolve per call: a subscription that terminates
	// synchronously twice in a row is done, not failing over.
	let mut rearmed = false;
	loop {
		match &mut node.reader {
			Reader::Resolving { pending, queued } => match pending.poll_ok(waiter) {
				Poll::Ready(Ok(broadcast)) => match broadcast.track(name) {
					Ok(track) => node.reader = Reader::Subscribing(track.subscribe(None)),
					Err(err) => {
						tracing::debug!(?err, name, "stats: node missing track");
						return changed | node.depart();
					}
				},
				// A queued request killed by its route retracting: an identical
				// standby swaps in without any announce update, so re-resolve
				// through the already-updated table. Each retry consumed a real
				// retraction, so this cannot spin; an instant Unroutable instead
				// means nothing serves the path (the retraction that empties the
				// table also unannounces this node).
				Poll::Ready(Err(moq_net::Error::Unroutable)) if *queued => {
					node.reader = resolve(origin, &node.path);
				}
				Poll::Ready(Err(err)) => {
					tracing::debug!(?err, name, "stats: node broadcast unresolvable");
					return changed | node.depart();
				}
				Poll::Pending => return changed,
			},
			Reader::Subscribing(pending) => match pending.poll_ok(waiter) {
				Poll::Ready(Ok(subscriber)) => {
					node.reader =
						Reader::Active(Box::new(moq_json::snapshot::Consumer::new(subscriber, config.clone())));
				}
				Poll::Ready(Err(err)) => {
					tracing::debug!(?err, name, "stats: node subscribe failed");
					return changed | node.depart();
				}
				Poll::Pending => return changed,
			},
			Reader::Active(reader) => match reader.poll_next(waiter) {
				Poll::Ready(Ok(Some(frame))) => {
					node.last = Some(frame);
					changed = true;
				}
				// The subscription ended: the serving session died (a failover to
				// an identical route delivers no announce update) or the track
				// finished. A non-sticky gauge drops its last frame so a stale
				// value stops pinning the sum, while a sticky counter keeps it.
				// Then re-resolve through the current best route; an
				// authoritative refusal on the way ends the node instead.
				Poll::Ready(result @ (Ok(None) | Err(_))) => {
					if let Err(err) = result {
						// One bad node must not tear down the whole merged view;
						// re-resolve just this node and keep folding the rest.
						tracing::debug!(?err, name, "stats: node read error");
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
					node.reader = resolve(origin, &node.path);
				}
				Poll::Pending => return changed,
			},
			Reader::Ended => return changed,
		}
	}
}

/// Start resolving `path` (relative to the announce cursor) into a broadcast.
fn resolve<V: Mergeable>(origin: &origin::Consumer, path: &PathOwned) -> Reader<V> {
	let pending = origin.request_broadcast(path);
	let queued = pending.is_queued();
	Reader::Resolving { pending, queued }
}

#[cfg(test)]
mod tests {
	/// Build an origin producer, spawning its driver on the ambient runtime.
	fn produce_origin() -> moq_net::origin::Producer {
		let (producer, driver) = moq_net::origin::Producer::new(moq_net::Hop::random().into());
		if tokio::runtime::Handle::try_current().is_ok() {
			tokio::spawn(driver.run(moq_tokio::runtime::Runtime::<()>::new()));
		} else {
			// A sync test: nothing polls the driver, and dropping it would tear
			// the origin down, so leak it and rely on the synchronous half.
			std::mem::forget(driver);
		}
		producer
	}

	use std::time::Duration;

	use moq_net::{PathOwned, Timestamp, announce, broadcast, origin, track};

	use crate::{Producer, ProducerConfig};

	use super::*;

	/// A stats producer publishing one node's broadcasts on `origin`, grouped at
	/// depth 1 (so feeding a broadcast under `<group>/...` announces
	/// `.stats/<group>/node/<node>`).
	fn node_producer(origin: &origin::Producer, node: &str) -> Producer {
		Producer::new(
			ProducerConfig::new()
				.with_origin(origin.clone())
				.with_node(PathOwned::from(node.to_string()))
				.with_depth(1),
		)
	}

	/// Kept-alive handles from [`feed`]: dropping them closes the subscription and
	/// announce, bumping the `_closed` counters.
	#[allow(dead_code)]
	struct Feed {
		announced: announce::Consumer,
		source: broadcast::Producer,
		consumer: broadcast::Consumer,
		sub: track::Subscriber,
		ctx: moq_net::stats::Session,
	}

	/// Record `bytes` of egress traffic on `path` in `producer`'s registry under
	/// `tier`/`root`, by driving a throwaway tagged broadcast. The broadcast lives
	/// on its own origin so it never lands on the stats origin.
	async fn feed(producer: &Producer, tier: Tier, root: &str, path: &str, bytes: usize) -> Feed {
		let ctx = producer.registry().tier(tier).session(root);
		let feed_origin = produce_origin();
		let egress = feed_origin.consume().with_stats(ctx.clone());

		let mut announced = egress.announced();
		let mut source = feed_origin.create_broadcast(path).expect("create_broadcast");
		source.announce(origin::Route::default()).expect("announce");
		let mut track = source.create_track("video", None).expect("create_track");

		let update = announced.next().await.expect("announce");
		assert!(update.active);
		let consumer = egress.request_broadcast(path).await.expect("resolve");
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

	/// A hand-published node broadcast at `.stats/<group>/node/<node>` with a
	/// plain default-tier traffic track, so a test controls the exact frames and
	/// can fail one node's reader alone (the registry-driven producer only
	/// publishes whole broadcasts). Dropping it unannounces the node.
	#[allow(dead_code)]
	struct NodeBroadcast {
		source: broadcast::Producer,
		traffic: moq_json::snapshot::Producer<TrafficFrame>,
		track: track::Producer,
		frame: TrafficFrame,
	}

	impl NodeBroadcast {
		fn new(origin: &origin::Producer, group: &str, node: &str) -> Self {
			let path = format!(".stats/{group}/node/{node}");
			let mut source = origin.create_broadcast(path.as_str()).expect("create broadcast");
			source.announce(origin::Route::default()).expect("announce");
			let name = traffic_track(&Tier::default(), Role::Publisher, false);
			let track = source.create_track(name, None).expect("create track");
			let config = moq_json::snapshot::ProducerConfig::default().with_delta_ratio(0);
			Self {
				traffic: moq_json::snapshot::Producer::new(track.clone(), config),
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
			self.traffic.update(&self.frame).expect("publish");
		}

		/// Fail the node's reader: append a frame the snapshot decoder can't
		/// parse, so the subscription errors while the broadcast stays announced.
		fn fail_traffic(&mut self) {
			let mut group = self.track.append_group().expect("append group");
			group
				.write_frame(Timestamp::ZERO, b"not json".to_vec())
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

		let agg = Consumer::new(origin.consume(), Config::new().with_depth(1));
		let mut traffic = agg.traffic(&Tier::default(), Role::Publisher);

		let frame = read_until_bytes(&mut traffic, "acme/room", 140).await;
		let snap = frame.get("acme/room").expect("entry");
		assert_eq!(snap.bytes, 140, "bytes sum across both nodes");
		assert_eq!(snap.subscriptions, 2, "one subscription per node");
		assert_eq!(snap.broadcasts, 2, "one viewer per node");
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

		let agg = Consumer::new(origin.consume(), Config::new().with_depth(1));
		let mut traffic = agg.traffic(&Tier::default(), Role::Publisher);
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
	async fn reannounce_with_higher_counter_stays_monotonic() {
		// A node's stats session reconnects with its cumulative counters intact
		// and still growing: the total holds through the swap, then advances,
		// never dipping.
		let origin = produce_origin();
		let node_a = node_producer(&origin, "a");
		let node_b = node_producer(&origin, "b");

		let fa = feed(&node_a, Tier::default(), "acme", "acme/room", 100).await;
		let _fb = feed(&node_b, Tier::default(), "acme", "acme/room", 40).await;
		drive_tick().await;

		let agg = Consumer::new(origin.consume(), Config::new().with_depth(1));
		let mut traffic = agg.traffic(&Tier::default(), Role::Publisher);
		read_until_bytes(&mut traffic, "acme/room", 140).await;

		// Node A's broadcast goes away and comes back with a higher counter.
		drop(fa);
		drop(node_a);
		drive_tick().await;

		let node_a = node_producer(&origin, "a");
		let _fa = feed(&node_a, Tier::default(), "acme", "acme/room", 120).await;
		drive_tick().await;

		// The kept contribution holds the total at 140 until the new frame
		// replaces it, landing on 120 + 40.
		let frame = read_monotonic_until(&mut traffic, "acme/room", 140, 160).await;
		assert_eq!(frame.get("acme/room").expect("entry").bytes, 160);
	}

	#[tokio::test(start_paused = true)]
	async fn restarted_node_regresses_the_total() {
		// A node that returns with a fresh counter (it restarted) replaces its
		// contribution wholesale: the total regresses once, the existing
		// fresh-segment contract.
		let origin = produce_origin();
		let node_a = node_producer(&origin, "a");
		let node_b = node_producer(&origin, "b");

		let fa = feed(&node_a, Tier::default(), "acme", "acme/room", 100).await;
		let _fb = feed(&node_b, Tier::default(), "acme", "acme/room", 40).await;
		drive_tick().await;

		let agg = Consumer::new(origin.consume(), Config::new().with_depth(1));
		let mut traffic = agg.traffic(&Tier::default(), Role::Publisher);
		read_until_bytes(&mut traffic, "acme/room", 140).await;

		// Node A restarts: its broadcast returns with a lower counter.
		drop(fa);
		drop(node_a);
		drive_tick().await;

		let node_a = node_producer(&origin, "a");
		let _fa = feed(&node_a, Tier::default(), "acme", "acme/room", 30).await;
		drive_tick().await;

		// The restarted frame replaces A's contribution, so the total drops to
		// 30 + 40, a genuine per-node regression downstream treats as a fresh
		// segment. An earlier frame may retire A's live gauges first.
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

		let agg = Consumer::new(origin.consume(), Config::new().with_depth(1));
		let mut traffic = agg.traffic(&Tier::default(), Role::Publisher);
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

	#[tokio::test(start_paused = true)]
	async fn unannounce_retires_live_gauges() {
		// A departed node's totals stay in the merged view, but its live gauges
		// retire: the aggregate must not show phantom viewers or broadcasts for
		// a node that is gone.
		let origin = produce_origin();
		let mut node_a = NodeBroadcast::new(&origin, "acme", "a");

		let mut published = Traffic::default();
		published.announced = 2;
		published.announced_closed = 1;
		published.broadcasts = 3;
		published.broadcasts_closed = 1;
		published.subscriptions = 4;
		published.subscriptions_closed = 1;
		published.bytes = 100;
		node_a.frame.insert("acme/room".to_string(), published);
		node_a.traffic.update(&node_a.frame).expect("publish");

		let agg = Consumer::new(origin.consume(), Config::new().with_depth(1));
		let mut traffic = agg.traffic(&Tier::default(), Role::Publisher);
		let frame = read_until_bytes(&mut traffic, "acme/room", 100).await;
		let snap = frame.get("acme/room").expect("entry");
		assert!(snap.is_announced());
		assert_eq!(snap.active_broadcasts(), 2);
		assert_eq!(snap.active_subscriptions(), 3);

		// The node departs with those sessions still open.
		drop(node_a);

		// The totals stay; the live gauges retire.
		let frame = traffic.next().await.expect("read").expect("frame");
		let snap = frame.get("acme/room").expect("entry");
		assert_eq!(snap.bytes, 100, "cumulative totals stay");
		assert!(!snap.is_announced(), "no phantom announcement");
		assert_eq!(snap.active_broadcasts(), 0, "no phantom broadcasts");
		assert_eq!(snap.active_subscriptions(), 0, "no phantom subscriptions");
	}

	#[tokio::test(start_paused = true)]
	async fn merges_sessions_across_nodes() {
		// Session presence sums per auth root across nodes.
		let origin = produce_origin();
		let node_a = node_producer(&origin, "a");
		let node_b = node_producer(&origin, "b");

		// Each node needs a live broadcast to announce; the sessions ride the same
		// group.
		let _fa = feed(&node_a, Tier::default(), "acme", "acme/room", 8).await;
		let _fb = feed(&node_b, Tier::default(), "acme", "acme/room", 8).await;
		let _sa = node_a.registry().tier(Tier::default()).session("acme");
		let _sb = node_b.registry().tier(Tier::default()).session("acme");
		drive_tick().await;

		let agg = Consumer::new(origin.consume(), Config::new().with_depth(1));
		let mut sessions = agg.sessions(&Tier::default());

		loop {
			let frame = sessions.next().await.expect("read").expect("frame");
			// Each feed opens one session ("acme" root) plus the explicit ones,
			// summed across both nodes.
			if frame.get("acme").map(|p| p.active()) >= Some(4) {
				break;
			}
		}
	}

	#[tokio::test(start_paused = true)]
	async fn unannounce_drops_presence_immediately() {
		// Unlike traffic, presence is a gauge: a node's unannounce must stop
		// counting its sessions immediately, not pin a stale gauge.
		let origin = produce_origin();
		let node_a = node_producer(&origin, "a");
		let node_b = node_producer(&origin, "b");

		let _fa = feed(&node_a, Tier::default(), "acme", "acme/room", 8).await;
		let fb = feed(&node_b, Tier::default(), "acme", "acme/room", 8).await;
		let _sa = node_a.registry().tier(Tier::default()).session("acme");
		let sb = node_b.registry().tier(Tier::default()).session("acme");
		drive_tick().await;

		let agg = Consumer::new(origin.consume(), Config::new().with_depth(1));
		let mut sessions = agg.sessions(&Tier::default());

		loop {
			let frame = sessions.next().await.expect("read").expect("frame");
			// Each feed opens one session ("acme" root) plus the explicit ones,
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
		assert_eq!(
			frame.get("acme").map(|p| p.active()),
			Some(2),
			"presence drops immediately"
		);
	}
}
