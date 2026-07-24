//! The aggregating half: fold a group's per-node stats broadcasts into one view.
//!
//! A single-broadcast [`Consumer`](crate::Consumer) reads one
//! `<prefix>/<group>/node/<node>` broadcast. This reader watches an origin's
//! announce stream for *every* node broadcast in a group and folds their
//! cumulative counters into one merged frame per `(tier, role)`, so a downstream
//! sees a project's whole live traffic as if it came from a single node.

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
/// group, summing the cumulative counters per key. A node dropping out (its
/// broadcast unannounces) removes its contribution, so the merged counter
/// regresses: downstream treats that decrease as a fresh segment, the same
/// reset contract a single node's own restart follows.
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
			inner: Merged::new(self.origin.announced(), &self.config, name),
		}
	}

	/// A merged reader over the sessions track for `tier`; see [`Self::traffic`].
	pub fn sessions(&self, tier: &Tier) -> SessionsConsumer {
		let name = sessions_track(tier, self.config.compression);
		SessionsConsumer {
			inner: Merged::new(self.origin.announced(), &self.config, name),
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

	/// Poll for the next merged frame; the `poll_*` counterpart to [`Self::next`].
	pub fn poll_next(&mut self, waiter: &Waiter) -> Poll<Result<Option<TrafficFrame>>> {
		self.inner.poll_next(waiter)
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

	/// Poll for the next merged frame; the `poll_*` counterpart to [`Self::next`].
	pub fn poll_next(&mut self, waiter: &Waiter) -> Poll<Result<Option<SessionsFrame>>> {
		self.inner.poll_next(waiter)
	}
}

/// A per-key counter that folds across nodes: the two wire counter types.
trait Mergeable: serde::de::DeserializeOwned + Default + Copy + 'static {
	/// Fold `other` into `acc`.
	fn merge(acc: &mut Self, other: Self);
}

impl Mergeable for Traffic {
	fn merge(acc: &mut Self, other: Self) {
		acc.add(other);
	}
}

impl Mergeable for Presence {
	fn merge(acc: &mut Self, other: Self) {
		acc.add(other);
	}
}

/// One node's subscription to the merged track.
enum Reader<V: Mergeable> {
	/// Awaiting the subscription handshake.
	Subscribing(Pending<Subscribing>),
	/// Reading frames. Boxed: the snapshot consumer dwarfs the other variants,
	/// and one lives per node in a map.
	Active(Box<moq_json::snapshot::Consumer<BTreeMap<String, V>>>),
	/// The subscription failed or the track ended; contributes its last value
	/// (if any) until the node unannounces.
	Ended,
}

/// One node's reader plus the last frame it produced (the value folded into the
/// merged view).
struct Node<V: Mergeable> {
	reader: Reader<V>,
	last: Option<BTreeMap<String, V>>,
}

/// Watches a group's node announces and folds one track across all of them.
struct Merged<V: Mergeable> {
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
	fn new(announce: moq_net::announce::Consumer, config: &Config, name: String) -> Self {
		Self {
			announce,
			prefix: config.prefix.clone(),
			depth: config.depth,
			name,
			config: moq_json::snapshot::ConsumerConfig::default().with_compression(config.compression),
			nodes: HashMap::new(),
		}
	}

	/// Poll for the next merged frame. Returns `Ready(Some(_))` whenever the
	/// merged view changed (a node produced a frame, appeared, or dropped),
	/// `Ready(None)` once the announce stream closes, else `Pending`.
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
		for node in self.nodes.values_mut() {
			changed |= advance(node, config, name, waiter);
		}

		if changed {
			Poll::Ready(Ok(Some(self.merged())))
		} else {
			Poll::Pending
		}
	}

	/// Apply one announce update to the node set. Returns whether the merged view
	/// changed (only a drop or a replacement of a node that had a value does).
	fn apply_announce(&mut self, update: moq_net::announce::Update) -> bool {
		let moq_net::announce::Update { path, broadcast } = update;
		let absolute = self.announce.absolute(&path).to_owned();

		// Only fold node-category broadcasts; skip sibling categories a producer
		// may publish under the same prefix.
		if parse_node_path(&self.prefix, self.depth, &absolute).is_none() {
			return false;
		}

		match broadcast {
			Some(broadcast) => match broadcast.track(&self.name) {
				Ok(track) => {
					let node = Node {
						reader: Reader::Subscribing(track.subscribe(None)),
						last: None,
					};
					// A replacement (failover) drops the old value until the new
					// subscription catches up.
					self.nodes.insert(absolute, node).is_some_and(|old| old.last.is_some())
				}
				Err(err) => {
					tracing::debug!(?err, node = %absolute, name = %self.name, "stats: node missing track");
					self.nodes.remove(&absolute).is_some_and(|old| old.last.is_some())
				}
			},
			None => self.nodes.remove(&absolute).is_some_and(|old| old.last.is_some()),
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
	config: &moq_json::snapshot::ConsumerConfig,
	name: &str,
	waiter: &Waiter,
) -> bool {
	let mut changed = false;
	loop {
		match &mut node.reader {
			Reader::Subscribing(pending) => match pending.poll_ok(waiter) {
				Poll::Ready(Ok(subscriber)) => {
					node.reader =
						Reader::Active(Box::new(moq_json::snapshot::Consumer::new(subscriber, config.clone())));
				}
				Poll::Ready(Err(err)) => {
					tracing::debug!(?err, name, "stats: node subscribe failed");
					node.reader = Reader::Ended;
					return changed;
				}
				Poll::Pending => return changed,
			},
			Reader::Active(reader) => match reader.poll_next(waiter) {
				Poll::Ready(Ok(Some(frame))) => {
					node.last = Some(frame);
					changed = true;
				}
				Poll::Ready(Ok(None)) => {
					node.reader = Reader::Ended;
					return changed;
				}
				Poll::Ready(Err(err)) => {
					// One bad node must not tear down the whole merged view; keep
					// its last value and stop reading it.
					tracing::debug!(?err, name, "stats: node read error");
					node.reader = Reader::Ended;
					return changed;
				}
				Poll::Pending => return changed,
			},
			Reader::Ended => return changed,
		}
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use moq_net::{Origin, PathOwned, Timestamp, announce, broadcast, origin, track};

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
		let feed_origin = Origin::random().produce();
		let egress = feed_origin.consume().with_stats(ctx.clone());

		let mut announced = egress.announced();
		let mut source = feed_origin
			.create_broadcast(path, broadcast::Route::announced())
			.expect("create_broadcast");
		let mut track = source.create_track("video", None).expect("create_track");

		// Let the origin's source watcher attach and announce.
		tokio::time::sleep(Duration::from_millis(1)).await;
		tokio::time::sleep(Duration::from_millis(1)).await;

		let announce::Update { broadcast, .. } = announced.next().await.expect("announce");
		let consumer = broadcast.expect("active");
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

	#[tokio::test(start_paused = true)]
	async fn merges_traffic_across_nodes() {
		// Two nodes each serve the same broadcast; the merged view sums their
		// cumulative counters per path.
		let origin = Origin::random().produce();
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
	async fn node_drop_regresses_the_merged_view() {
		// Dropping a node unannounces its broadcast; its contribution leaves the
		// sum, so the merged counter regresses (a fresh segment downstream).
		let origin = Origin::random().produce();
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

		// The merged view drops back to node A's contribution alone.
		loop {
			let frame = traffic.next().await.expect("read").expect("frame");
			if frame.get("acme/room").map(|t| t.bytes) == Some(100) {
				break;
			}
		}
	}

	#[tokio::test(start_paused = true)]
	async fn merges_sessions_across_nodes() {
		// Session presence sums per auth root across nodes.
		let origin = Origin::random().produce();
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
}
