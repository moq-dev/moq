use std::fmt;

use rand::Rng;

use crate::Version;
use crate::coding::{Decode, DecodeError, Encode, EncodeError};

/// A unique identifier for an origin, encoded as a varint on the wire.
///
/// Must be a non-zero 62-bit value (1 <= value < 2^62).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct OriginId(u64);

/// The maximum valid OriginId value (2^62 - 1).
const ORIGIN_ID_MAX: u64 = (1u64 << 62) - 1;

impl OriginId {
	/// A placeholder value used when the actual OriginId is unknown (e.g., Lite03 hop placeholders).
	pub const UNKNOWN: Self = Self(0);

	/// Generate a random non-zero 62-bit origin ID.
	pub fn random() -> Self {
		let mut rng = rand::rng();
		let value = rng.random_range(1..(1u64 << 62));
		Self(value)
	}

	/// Get the inner u64 value.
	pub fn into_inner(self) -> u64 {
		self.0
	}
}

impl TryFrom<u64> for OriginId {
	type Error = InvalidOriginId;

	fn try_from(value: u64) -> Result<Self, Self::Error> {
		if value == 0 || value > ORIGIN_ID_MAX {
			return Err(InvalidOriginId(value));
		}
		Ok(Self(value))
	}
}

/// Error returned when constructing an OriginId with an invalid value.
#[derive(Debug, Clone)]
pub struct InvalidOriginId(pub u64);

impl fmt::Display for InvalidOriginId {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "invalid OriginId: {} (must be 1 <= value < 2^62)", self.0)
	}
}

impl std::error::Error for InvalidOriginId {}

impl fmt::Display for OriginId {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		self.0.fmt(f)
	}
}

impl Encode<Version> for OriginId {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		self.0.encode(w, version)
	}
}

impl Decode<Version> for OriginId {
	fn decode<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let value = u64::decode(r, version)?;
		Self::try_from(value).map_err(|_| DecodeError::InvalidValue)
	}
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for OriginId {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: serde::Deserializer<'de>,
	{
		let value = u64::deserialize(deserializer)?;
		Self::try_from(value).map_err(serde::de::Error::custom)
	}
}

use std::{
	collections::HashMap,
	sync::atomic::{AtomicU64, Ordering},
};
use tokio::sync::mpsc;
use web_async::Lock;

use std::time::Duration;

use super::BroadcastConsumer;
use crate::{AsPath, Broadcast, BroadcastProducer, Error, Path, PathOwned};

/// Delay before reannouncing a promoted backup broadcast.
/// This avoids churn when a cascade of closures propagates through the network.
const REANNOUNCE_HOLD_DOWN: Duration = Duration::from_millis(250);

static NEXT_CONSUMER_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ConsumerId(u64);

impl ConsumerId {
	fn new() -> Self {
		Self(NEXT_CONSUMER_ID.fetch_add(1, Ordering::Relaxed))
	}
}

// If there are multiple broadcasts with the same path, we use the most recent one but keep the others around.
struct OriginBroadcast {
	path: PathOwned,
	active: BroadcastConsumer,
	backup: Vec<BroadcastConsumer>,
}

#[derive(Clone)]
struct OriginConsumerNotify {
	root: PathOwned,
	tx: mpsc::UnboundedSender<OriginAnnounce>,
}

impl OriginConsumerNotify {
	fn announce(&self, path: impl AsPath, broadcast: BroadcastConsumer) {
		let path = path.as_path().strip_prefix(&self.root).unwrap().to_owned();
		self.tx.send((path, broadcast)).expect("consumer closed");
	}
}

struct NotifyNode {
	parent: Option<Lock<NotifyNode>>,

	// Consumers that are subscribed to this node.
	// We store a consumer ID so we can remove it easily when it closes.
	consumers: HashMap<ConsumerId, OriginConsumerNotify>,
}

impl NotifyNode {
	fn new(parent: Option<Lock<NotifyNode>>) -> Self {
		Self {
			parent,
			consumers: HashMap::new(),
		}
	}

	fn announce(&mut self, path: impl AsPath, broadcast: &BroadcastConsumer) {
		for consumer in self.consumers.values() {
			consumer.announce(path.as_path(), broadcast.clone());
		}

		if let Some(parent) = &self.parent {
			parent.lock().announce(path, broadcast);
		}
	}
}

struct OriginNode {
	// The broadcast that is published to this node.
	broadcast: Option<OriginBroadcast>,

	// Nested nodes, one level down the tree.
	nested: HashMap<String, Lock<OriginNode>>,

	// Unfortunately, to notify consumers we need to traverse back up the tree.
	notify: Lock<NotifyNode>,
}

impl OriginNode {
	fn new(parent: Option<Lock<NotifyNode>>) -> Self {
		Self {
			broadcast: None,
			nested: HashMap::new(),
			notify: Lock::new(NotifyNode::new(parent)),
		}
	}

	fn leaf(&mut self, path: &Path) -> Lock<OriginNode> {
		let (dir, rest) = path.next_part().expect("leaf called with empty path");

		let next = self.entry(dir);
		if rest.is_empty() { next } else { next.lock().leaf(&rest) }
	}

	fn entry(&mut self, dir: &str) -> Lock<OriginNode> {
		match self.nested.get(dir) {
			Some(next) => next.clone(),
			None => {
				let next = Lock::new(OriginNode::new(Some(self.notify.clone())));
				self.nested.insert(dir.to_string(), next.clone());
				next
			}
		}
	}

	fn publish(&mut self, full: impl AsPath, broadcast: &BroadcastConsumer, relative: impl AsPath) {
		let full = full.as_path();
		let rest = relative.as_path();

		// If the path has a directory component, then publish it to the nested node.
		if let Some((dir, relative)) = rest.next_part() {
			// Not using entry to avoid allocating a string most of the time.
			self.entry(dir).lock().publish(&full, broadcast, &relative);
		} else if let Some(existing) = &mut self.broadcast {
			// This node is a leaf with an existing broadcast.

			// Prefix check: if the existing broadcast's hops are a strict prefix of the new one,
			// the new broadcast is routing through us (loop). Reject it.
			// Identical hop lists are not treated as loops (could be a re-announcement).
			if !existing.active.info.hops.is_empty()
				&& broadcast.info.hops.len() > existing.active.info.hops.len()
				&& broadcast.info.hops.starts_with(&existing.active.info.hops)
			{
				tracing::debug!(broadcast = %full, "rejecting broadcast: hops are prefix of existing");
				return;
			}

			if broadcast.info.hops.len() < existing.active.info.hops.len() {
				// New broadcast has fewer hops, so it becomes active.
				let old = existing.active.clone();
				existing.active = broadcast.clone();
				existing.backup.push(old);
				self.notify.lock().announce(full, broadcast);
			} else {
				// Same or more hops, just add to backup.
				existing.backup.push(broadcast.clone());
			}
		} else {
			// This node is a leaf with no existing broadcast.
			self.broadcast = Some(OriginBroadcast {
				path: full.to_owned(),
				active: broadcast.clone(),
				backup: Vec::new(),
			});
			self.notify.lock().announce(full, broadcast);
		}
	}

	fn consume(&mut self, id: ConsumerId, mut notify: OriginConsumerNotify) {
		self.consume_initial(&mut notify);
		self.notify.lock().consumers.insert(id, notify);
	}

	fn consume_initial(&mut self, notify: &mut OriginConsumerNotify) {
		if let Some(broadcast) = &self.broadcast {
			notify.announce(&broadcast.path, broadcast.active.clone());
		}

		// Recursively subscribe to all nested nodes.
		for nested in self.nested.values() {
			nested.lock().consume_initial(notify);
		}
	}

	fn consume_broadcast(&self, rest: impl AsPath) -> Option<BroadcastConsumer> {
		let rest = rest.as_path();

		if let Some((dir, rest)) = rest.next_part() {
			let node = self.nested.get(dir)?.lock();
			node.consume_broadcast(&rest)
		} else {
			self.broadcast.as_ref().map(|b| b.active.clone())
		}
	}

	fn unconsume(&mut self, id: ConsumerId) {
		self.notify.lock().consumers.remove(&id).expect("consumer not found");
		if self.is_empty() {
			//tracing::warn!("TODO: empty node; memory leak");
			// This happens when consuming a path that is not being broadcasted.
		}
	}

	/// Remove a broadcast from this node.
	///
	/// Returns `Some(promoted)` if a backup was promoted to active and needs a delayed reannounce.
	/// Unannounces immediately if there are no backups.
	fn remove(
		&mut self,
		full: impl AsPath,
		broadcast: BroadcastConsumer,
		relative: impl AsPath,
	) -> Option<BroadcastConsumer> {
		let full = full.as_path();
		let relative = relative.as_path();

		if let Some((dir, relative)) = relative.next_part() {
			let nested = self.entry(dir);
			let mut locked = nested.lock();
			let result = locked.remove(&full, broadcast, &relative);

			if locked.is_empty() {
				drop(locked);
				self.nested.remove(dir);
			}

			return result;
		}

		let entry = match &mut self.broadcast {
			Some(existing) => existing,
			None => return None,
		};

		// See if we can remove the broadcast from the backup list.
		let pos = entry.backup.iter().position(|b| b.is_clone(&broadcast));
		if let Some(pos) = pos {
			entry.backup.remove(pos);
			return None;
		}

		// Okay so it must be the active broadcast or else we fucked up.
		assert!(entry.active.is_clone(&broadcast));

		// If there's a backup broadcast, pick the one with fewest hops (most recent as tiebreaker).
		if !entry.backup.is_empty() {
			// Reverse enumerate so that ties prefer the most recently added (last in vec).
			let best = entry
				.backup
				.iter()
				.enumerate()
				.rev()
				.min_by_key(|(_, b)| b.info.hops.len())
				.map(|(i, _)| i)
				.unwrap();
			let active = entry.backup.swap_remove(best);
			entry.active = active.clone();

			// Don't reannounce immediately — return the promoted backup so the caller
			// can schedule a delayed reannounce (hold-down timer) to avoid churn when
			// a cascade of closures is propagating through the network.
			Some(active)
		} else {
			// No more backups, so remove the entry.
			self.broadcast = None;
			None
		}
	}

	/// Announce a promoted backup if it's still the active broadcast.
	///
	/// Called after the hold-down delay. If a better broadcast arrived in the meantime
	/// (via publish with fewer hops), the promoted one will no longer be active and
	/// this is a no-op.
	fn maybe_reannounce(&mut self, full: impl AsPath, relative: impl AsPath, promoted: &BroadcastConsumer) {
		let full = full.as_path();
		let relative = relative.as_path();

		if let Some((dir, relative)) = relative.next_part() {
			if let Some(nested) = self.nested.get(dir) {
				nested.lock().maybe_reannounce(&full, &relative, promoted);
			}
		} else if let Some(entry) = &self.broadcast
			&& entry.active.is_clone(promoted)
		{
			self.notify.lock().announce(full, &entry.active);
		}
	}

	fn is_empty(&self) -> bool {
		self.broadcast.is_none() && self.nested.is_empty() && self.notify.lock().consumers.is_empty()
	}
}

#[derive(Clone)]
struct OriginNodes {
	nodes: Vec<(PathOwned, Lock<OriginNode>)>,
}

impl OriginNodes {
	// Returns nested roots that match the prefixes.
	// TODO enforce that prefixes can't overlap.
	pub fn select(&self, prefixes: &[Path]) -> Option<Self> {
		let mut roots = Vec::new();

		for (root, state) in &self.nodes {
			for prefix in prefixes {
				if root.has_prefix(prefix) {
					// Keep the existing node if we're allowed to access it.
					roots.push((root.to_owned(), state.clone()));
					continue;
				}

				if let Some(suffix) = prefix.strip_prefix(root) {
					// If the requested prefix is larger than the allowed prefix, then we further scope it.
					let nested = state.lock().leaf(&suffix);
					roots.push((prefix.to_owned(), nested));
				}
			}
		}

		if roots.is_empty() {
			None
		} else {
			Some(Self { nodes: roots })
		}
	}

	pub fn root(&self, new_root: impl AsPath) -> Option<Self> {
		let new_root = new_root.as_path();
		let mut roots = Vec::new();

		if new_root.is_empty() {
			return Some(self.clone());
		}

		for (root, state) in &self.nodes {
			if let Some(suffix) = root.strip_prefix(&new_root) {
				// If the old root is longer than the new root, shorten the keys.
				roots.push((suffix.to_owned(), state.clone()));
			} else if let Some(suffix) = new_root.strip_prefix(root) {
				// If the new root is longer than the old root, add a new root.
				// NOTE: suffix can't be empty
				let nested = state.lock().leaf(&suffix);
				roots.push(("".into(), nested));
			}
		}

		if roots.is_empty() {
			None
		} else {
			Some(Self { nodes: roots })
		}
	}

	// Returns the root that has this prefix.
	pub fn get(&self, path: impl AsPath) -> Option<(Lock<OriginNode>, PathOwned)> {
		let path = path.as_path();

		for (root, state) in &self.nodes {
			if let Some(suffix) = path.strip_prefix(root) {
				return Some((state.clone(), suffix.to_owned()));
			}
		}

		None
	}
}

impl Default for OriginNodes {
	fn default() -> Self {
		Self {
			nodes: vec![("".into(), Lock::new(OriginNode::new(None)))],
		}
	}
}

/// A broadcast path and its associated consumer.
pub type OriginAnnounce = (PathOwned, BroadcastConsumer);

/// A boxed future that resolves when a broadcast closes, yielding (path, broadcast).
pub type OriginClosureFuture =
	std::pin::Pin<Box<dyn std::future::Future<Output = (PathOwned, BroadcastConsumer)> + Send>>;

/// A collection of broadcasts that can be published and subscribed to.
pub struct Origin {}

impl Origin {
	pub fn produce() -> OriginProducer {
		OriginProducer::new()
	}
}

/// Announces broadcasts to consumers over the network.
#[derive(Clone)]
pub struct OriginProducer {
	/// A unique identifier for this origin.
	id: OriginId,

	// The roots of the tree that we are allowed to publish.
	// A path of "" means we can publish anything.
	nodes: OriginNodes,

	/// The prefix that is automatically stripped from all paths.
	root: PathOwned,
}

impl Default for OriginProducer {
	fn default() -> Self {
		Self {
			id: OriginId::random(),
			nodes: OriginNodes::default(),
			root: PathOwned::default(),
		}
	}
}

impl OriginProducer {
	pub fn new() -> Self {
		Self::default()
	}

	/// Set the origin ID.
	pub fn with_id(mut self, id: OriginId) -> Self {
		self.id = id;
		self
	}

	/// Get the origin ID.
	pub fn id(&self) -> OriginId {
		self.id
	}

	/// Create and publish a new broadcast, returning the producer.
	///
	/// This is a helper method when you only want to publish a broadcast to a single origin.
	/// Returns [None] if the broadcast is not allowed to be published.
	pub fn create_broadcast(&self, path: impl AsPath) -> Option<BroadcastProducer> {
		let broadcast = Broadcast::new().produce();
		self.publish_broadcast(path, broadcast.consume()).then_some(broadcast)
	}

	/// Publish a broadcast, announcing it to all consumers.
	///
	/// The broadcast will be unannounced when it is closed.
	/// If there is already a broadcast with the same path and more hops, it will be replaced and reannounced.
	/// If the old broadcast is closed before the new one, the new broadcast will be reannounced after a hold-down delay.
	/// If the new broadcast is closed before the old one, then nothing will happen.
	///
	/// Returns false if the broadcast is not allowed to be published.
	pub fn publish_broadcast(&self, path: impl AsPath, broadcast: BroadcastConsumer) -> bool {
		let path = path.as_path();

		if broadcast.info.hops.len() > 32 {
			return false;
		}

		let (root, rest) = match self.nodes.get(&path) {
			Some(root) => root,
			None => return false,
		};

		let full = self.root.join(&path);

		root.lock().publish(&full, &broadcast, &rest);
		let root = root.clone();

		web_async::spawn(async move {
			broadcast.closed().await;
			let promoted = root.lock().remove(&full, broadcast, &rest);

			if let Some(promoted) = promoted {
				// Hold-down timer: delay the reannounce to avoid churn when a cascade
				// of closures propagates through the network. If a better path arrives
				// during this window, it will reannounce immediately and this becomes a no-op.
				tokio::time::sleep(REANNOUNCE_HOLD_DOWN).await;
				root.lock().maybe_reannounce(&full, &rest, &promoted);
			}
		});

		true
	}

	/// Returns a new OriginProducer where all published broadcasts MUST match one of the prefixes.
	///
	/// Returns None if there are no legal prefixes.
	pub fn with_filter(&self, prefixes: &[Path]) -> Option<OriginProducer> {
		Some(OriginProducer {
			id: self.id,
			nodes: self.nodes.select(prefixes)?,
			root: self.root.clone(),
		})
	}

	/// Subscribe to all announced broadcasts.
	pub fn consume(&self) -> OriginConsumer {
		OriginConsumer::new(self.root.clone(), self.nodes.clone())
	}

	/// Returns a new OriginProducer that automatically strips out the provided prefix.
	///
	/// Returns None if the provided root is not authorized; when with_filter was already used without a wildcard.
	pub fn with_root(&self, prefix: impl AsPath) -> Option<Self> {
		let prefix = prefix.as_path();

		Some(Self {
			id: self.id,
			root: self.root.join(&prefix).to_owned(),
			nodes: self.nodes.root(&prefix)?,
		})
	}

	/// Returns the root that is automatically stripped from all paths.
	pub fn root(&self) -> &Path<'_> {
		&self.root
	}

	pub fn allowed(&self) -> impl Iterator<Item = &Path<'_>> {
		self.nodes.nodes.iter().map(|(root, _)| root)
	}

	/// Converts a relative path to an absolute path.
	pub fn absolute(&self, path: impl AsPath) -> Path<'_> {
		self.root.join(path)
	}
}

/// Consumes announced broadcasts matching against an optional prefix.
///
/// NOTE: Clone is expensive, try to avoid it.
pub struct OriginConsumer {
	id: ConsumerId,
	nodes: OriginNodes,
	updates: mpsc::UnboundedReceiver<OriginAnnounce>,

	/// A prefix that is automatically stripped from all paths.
	root: PathOwned,
}

impl OriginConsumer {
	fn new(root: PathOwned, nodes: OriginNodes) -> Self {
		let (tx, rx) = mpsc::unbounded_channel();

		let id = ConsumerId::new();

		for (_, state) in &nodes.nodes {
			let notify = OriginConsumerNotify {
				root: root.clone(),
				tx: tx.clone(),
			};
			state.lock().consume(id, notify);
		}

		Self {
			id,
			nodes,
			updates: rx,
			root,
		}
	}

	/// Returns the next announced broadcast and its path relative to this consumer's root.
	///
	/// If the same path is announced twice, the new broadcast replaces the old one.
	/// The old broadcast's `closed()` future will resolve when it is no longer active.
	/// Returns `Err(Error::Dropped)` if the consumer is closed.
	///
	/// Note: The returned path has had this consumer's root prefix stripped.
	pub async fn announced(&mut self) -> Result<OriginAnnounce, Error> {
		self.updates.recv().await.ok_or(Error::Dropped)
	}

	/// Returns the next announced broadcast without blocking.
	///
	/// Returns None if there is no update available.
	pub fn try_announced(&mut self) -> Option<OriginAnnounce> {
		self.updates.try_recv().ok()
	}

	/// Get a specific broadcast by path, returning immediately.
	///
	/// Returns None if the path hasn't been announced yet.
	pub fn try_consume_broadcast(&self, path: impl AsPath) -> Option<BroadcastConsumer> {
		let path = path.as_path();
		let (root, rest) = self.nodes.get(&path)?;
		let state = root.lock();
		state.consume_broadcast(&rest)
	}

	/// Get a specific broadcast by path, waiting for it to be announced if needed.
	pub async fn consume_broadcast(&self, path: impl AsPath) -> Result<BroadcastConsumer, Error> {
		let path = path.as_path();
		if let Some(bc) = self.try_consume_broadcast(&path) {
			return Ok(bc);
		}
		let mut scoped = self.with_filter(std::slice::from_ref(&path)).ok_or(Error::NotFound)?;
		loop {
			let (announced_path, broadcast) = scoped.announced().await?;
			if announced_path == path {
				return Ok(broadcast);
			}
			// Skip descendant announcements that don't match the exact path.
		}
	}

	/// Returns a new OriginConsumer that only consumes broadcasts matching one of the prefixes.
	///
	/// Returns None if there are no legal prefixes (would always return None).
	pub fn with_filter(&self, prefixes: &[Path]) -> Option<OriginConsumer> {
		Some(OriginConsumer::new(self.root.clone(), self.nodes.select(prefixes)?))
	}

	/// Returns a new OriginConsumer that automatically strips out the provided prefix.
	///
	/// Returns None if the provided root is not authorized; when with_filter was already used without a wildcard.
	pub fn with_root(&self, prefix: impl AsPath) -> Option<Self> {
		let prefix = prefix.as_path();

		Some(Self::new(self.root.join(&prefix).to_owned(), self.nodes.root(&prefix)?))
	}

	/// Returns the prefix that is automatically stripped from all paths.
	pub fn root(&self) -> &Path<'_> {
		&self.root
	}

	pub fn allowed(&self) -> impl Iterator<Item = &Path<'_>> {
		self.nodes.nodes.iter().map(|(root, _)| root)
	}

	/// Converts a relative path to an absolute path.
	pub fn absolute(&self, path: impl AsPath) -> Path<'_> {
		self.root.join(path)
	}
}

impl Drop for OriginConsumer {
	fn drop(&mut self) {
		for (_, root) in &self.nodes.nodes {
			root.lock().unconsume(self.id);
		}
	}
}

impl Clone for OriginConsumer {
	fn clone(&self) -> Self {
		OriginConsumer::new(self.root.clone(), self.nodes.clone())
	}
}

#[cfg(test)]
use futures::FutureExt;

#[cfg(test)]
impl OriginConsumer {
	pub fn assert_next(&mut self, expected: impl AsPath, broadcast: &BroadcastConsumer) {
		let expected = expected.as_path();
		let (path, active) = self
			.announced()
			.now_or_never()
			.expect("next blocked")
			.expect("announced returned error");
		assert_eq!(path, expected, "wrong path");
		assert!(active.is_clone(broadcast), "should be the same broadcast");
	}

	pub fn assert_try_next(&mut self, expected: impl AsPath, broadcast: &BroadcastConsumer) {
		let expected = expected.as_path();
		let (path, active) = self.try_announced().expect("no next");
		assert_eq!(path, expected, "wrong path");
		assert!(active.is_clone(broadcast), "should be the same broadcast");
	}

	pub fn assert_next_wait(&mut self) {
		if let Some(res) = self.announced().now_or_never() {
			panic!("next should block: got {:?}", res.map(|(path, _)| path));
		}
	}
}

#[cfg(test)]
mod tests {
	use crate::Broadcast;

	use super::*;

	#[tokio::test]
	async fn test_announce() {
		tokio::time::pause();

		let origin = Origin::produce();
		let broadcast1 = Broadcast::new().produce();
		let broadcast2 = Broadcast::new().produce();

		let mut consumer1 = origin.consume();
		// Make a new consumer that should get it.
		consumer1.assert_next_wait();

		// Publish the first broadcast.
		origin.publish_broadcast("test1", broadcast1.consume());

		consumer1.assert_next("test1", &broadcast1.consume());
		consumer1.assert_next_wait();

		// Make a new consumer that should get the existing broadcast.
		// But we don't consume it yet.
		let mut consumer2 = origin.consume();

		// Publish the second broadcast.
		origin.publish_broadcast("test2", broadcast2.consume());

		consumer1.assert_next("test2", &broadcast2.consume());
		consumer1.assert_next_wait();

		consumer2.assert_next("test1", &broadcast1.consume());
		consumer2.assert_next("test2", &broadcast2.consume());
		consumer2.assert_next_wait();

		// Close the first broadcast.
		let bc1 = broadcast1.consume();
		drop(broadcast1);

		// Wait for the async task to run.
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;

		// No unannounce message is sent; instead, the broadcast's closed() resolves.
		// Verify consumers don't receive anything.
		consumer1.assert_next_wait();
		consumer2.assert_next_wait();

		// The closed broadcast should be detected.
		assert!(bc1.closed().now_or_never().is_some());

		// And a new consumer only gets the last broadcast.
		let mut consumer3 = origin.consume();
		consumer3.assert_next("test2", &broadcast2.consume());
		consumer3.assert_next_wait();

		// Close the other producer and make sure it cleans up
		let bc2 = broadcast2.consume();
		drop(broadcast2);

		// Wait for the async task to run.
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;

		// No unannounce messages, just verify broadcasts are closed.
		consumer1.assert_next_wait();
		consumer2.assert_next_wait();
		consumer3.assert_next_wait();
		assert!(bc2.closed().now_or_never().is_some());
	}

	#[tokio::test]
	async fn test_duplicate() {
		tokio::time::pause();

		let origin = Origin::produce();

		// All same hops (0), so first becomes active, rest go to backup.
		let broadcast1 = Broadcast::new().produce();
		let broadcast2 = Broadcast::new().produce();
		let broadcast3 = Broadcast::new().produce();

		let consumer1 = broadcast1.consume();
		let consumer2 = broadcast2.consume();
		let consumer3 = broadcast3.consume();

		let mut consumer = origin.consume();

		origin.publish_broadcast("test", consumer1.clone());
		origin.publish_broadcast("test", consumer2.clone());
		origin.publish_broadcast("test", consumer3.clone());
		assert!(consumer.try_consume_broadcast("test").is_some());

		// Only the first publish triggers an announce (same hops = no reannounce).
		consumer.assert_next("test", &consumer1);
		consumer.assert_next_wait();

		// Drop a backup, nothing should change.
		drop(broadcast2);

		// Wait for the async cleanup task to run.
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;

		assert!(consumer.try_consume_broadcast("test").is_some());
		consumer.assert_next_wait();

		// Drop the active — backup is promoted but reannounce is delayed (hold-down timer).
		drop(broadcast1);

		// Wait for the remove task to run, but not the hold-down.
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
		consumer.assert_next_wait();

		// Advance past the hold-down timer. Now the reannounce should fire.
		tokio::time::sleep(REANNOUNCE_HOLD_DOWN + tokio::time::Duration::from_millis(1)).await;

		assert!(consumer.try_consume_broadcast("test").is_some());
		consumer.assert_next("test", &consumer3);

		// Drop the final broadcast, no more messages.
		drop(broadcast3);

		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
		assert!(consumer.try_consume_broadcast("test").is_none());

		// No unannounce message, just verify the broadcast is gone.
		consumer.assert_next_wait();
	}

	#[tokio::test]
	async fn test_duplicate_reverse() {
		tokio::time::pause();

		let origin = Origin::produce();
		let broadcast1 = Broadcast::new().produce();
		let broadcast2 = Broadcast::new().produce();

		origin.publish_broadcast("test", broadcast1.consume());
		origin.publish_broadcast("test", broadcast2.consume());
		assert!(origin.consume().try_consume_broadcast("test").is_some());

		// This is harder, dropping the new broadcast first.
		drop(broadcast2);

		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
		assert!(origin.consume().try_consume_broadcast("test").is_some());

		drop(broadcast1);

		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
		assert!(origin.consume().try_consume_broadcast("test").is_none());
	}

	/// Create a hops vector with `n` random OriginIds for testing.
	/// Each call returns a completely independent set of IDs.
	fn test_hops(n: usize) -> Vec<OriginId> {
		(0..n).map(|_| OriginId::random()).collect()
	}

	#[tokio::test]
	async fn test_hops_ordering() {
		tokio::time::pause();

		let origin = Origin::produce();

		// Publish a broadcast with 3 hops.
		let far = Broadcast::new().with_hops(test_hops(3)).produce();
		let far_consumer = far.consume();

		let mut consumer = origin.consume();

		origin.publish_broadcast("test", far_consumer.clone());
		consumer.assert_next("test", &far_consumer);
		consumer.assert_next_wait();

		// Now publish a closer broadcast (1 hop). It should replace the active and reannounce immediately.
		let close = Broadcast::new().with_hops(test_hops(1)).produce();
		let close_consumer = close.consume();

		origin.publish_broadcast("test", close_consumer.clone());
		consumer.assert_next("test", &close_consumer);
		consumer.assert_next_wait();

		// Publish a broadcast with more hops (5). Should go to backup silently.
		let farther = Broadcast::new().with_hops(test_hops(5)).produce();
		let farther_consumer = farther.consume();

		origin.publish_broadcast("test", farther_consumer.clone());
		consumer.assert_next_wait();

		// Drop the active (1 hop). Best backup is 3 hops. Reannounce is delayed.
		drop(close);
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
		consumer.assert_next_wait();

		// After the hold-down, the 3-hop backup is reannounced.
		tokio::time::sleep(REANNOUNCE_HOLD_DOWN + tokio::time::Duration::from_millis(1)).await;
		consumer.assert_next("test", &far_consumer);

		// Drop the 3-hop broadcast. Best backup is 5 hops. Reannounce is delayed.
		drop(far);
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
		consumer.assert_next_wait();

		tokio::time::sleep(REANNOUNCE_HOLD_DOWN + tokio::time::Duration::from_millis(1)).await;
		consumer.assert_next("test", &farther_consumer);

		// Drop the last one. No message sent.
		drop(farther);
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
		consumer.assert_next_wait();
	}

	#[tokio::test]
	async fn test_hops_same_no_reannounce() {
		let origin = Origin::produce();

		let b1 = Broadcast::new().with_hops(test_hops(2)).produce();
		let b1c = b1.consume();

		let mut consumer = origin.consume();

		origin.publish_broadcast("test", b1c.clone());
		consumer.assert_next("test", &b1c);

		// Publish another broadcast with same hops. Should go to backup, no reannounce.
		let b2 = Broadcast::new().with_hops(test_hops(2)).produce();
		let _b2c = b2.consume();

		origin.publish_broadcast("test", _b2c.clone());
		consumer.assert_next_wait();
	}

	/// When the active closes and a backup is promoted, a better publish arriving
	/// during the hold-down should reannounce immediately and cancel the delayed one.
	#[tokio::test]
	async fn test_hold_down_superseded_by_better_publish() {
		tokio::time::pause();

		let origin = Origin::produce();

		let b1 = Broadcast::new().with_hops(test_hops(1)).produce();
		let b1c = b1.consume();
		let b2 = Broadcast::new().with_hops(test_hops(3)).produce();
		let b2c = b2.consume();

		let mut consumer = origin.consume();

		origin.publish_broadcast("test", b1c.clone());
		origin.publish_broadcast("test", b2c.clone());
		consumer.assert_next("test", &b1c);
		consumer.assert_next_wait();

		// Drop the active (1 hop). Backup (3 hops) is promoted, hold-down starts.
		drop(b1);
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
		consumer.assert_next_wait(); // No reannounce yet.

		// During hold-down, a better broadcast arrives (0 hops).
		let b3 = Broadcast::new().with_hops(test_hops(0)).produce();
		let b3c = b3.consume();
		origin.publish_broadcast("test", b3c.clone());

		// The better broadcast reannounces immediately.
		consumer.assert_next("test", &b3c);
		consumer.assert_next_wait();

		// After the hold-down expires, nothing happens (superseded).
		tokio::time::sleep(REANNOUNCE_HOLD_DOWN).await;
		consumer.assert_next_wait();
	}

	/// When the active closes and the promoted backup also closes during the hold-down,
	/// we should unannounce immediately.
	#[tokio::test]
	async fn test_hold_down_backup_also_closes() {
		tokio::time::pause();

		let origin = Origin::produce();

		let b1 = Broadcast::new().produce();
		let b1c = b1.consume();
		let b2 = Broadcast::new().produce();
		let b2c = b2.consume();

		let mut consumer = origin.consume();

		origin.publish_broadcast("test", b1c.clone());
		origin.publish_broadcast("test", b2c.clone());
		consumer.assert_next("test", &b1c);
		consumer.assert_next_wait();

		// Drop the active. Backup promoted, hold-down starts.
		drop(b1);
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
		consumer.assert_next_wait();

		// Drop the promoted backup during the hold-down.
		drop(b2);
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;

		// No message sent — last broadcast gone.
		consumer.assert_next_wait();
		assert!(origin.consume().try_consume_broadcast("test").is_none());

		// After the hold-down expires, nothing happens.
		tokio::time::sleep(REANNOUNCE_HOLD_DOWN).await;
		consumer.assert_next_wait();
	}

	/// Cascading closures: active closes, backup promoted, backup closes too.
	/// The hold-down prevents churn — downstream only sees the final state.
	#[tokio::test]
	async fn test_hold_down_cascade() {
		tokio::time::pause();

		let origin = Origin::produce();

		let b1 = Broadcast::new().with_hops(test_hops(1)).produce();
		let b1c = b1.consume();
		let b2 = Broadcast::new().with_hops(test_hops(2)).produce();
		let b2c = b2.consume();
		let b3 = Broadcast::new().with_hops(test_hops(3)).produce();
		let b3c = b3.consume();

		let mut consumer = origin.consume();

		origin.publish_broadcast("test", b1c.clone());
		origin.publish_broadcast("test", b2c.clone());
		origin.publish_broadcast("test", b3c.clone());
		consumer.assert_next("test", &b1c);
		consumer.assert_next_wait();

		// Drop b1 (active). b2 promoted, hold-down starts.
		drop(b1);
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
		consumer.assert_next_wait();

		// Drop b2 (promoted) during hold-down. b3 promoted, new hold-down starts.
		drop(b2);
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
		consumer.assert_next_wait();

		// After the hold-down, b3 is reannounced.
		tokio::time::sleep(REANNOUNCE_HOLD_DOWN + tokio::time::Duration::from_millis(1)).await;
		consumer.assert_next("test", &b3c);
		consumer.assert_next_wait();
	}

	#[tokio::test]
	async fn test_double_publish() {
		tokio::time::pause();

		let origin = Origin::produce();
		let broadcast = Broadcast::new().produce();

		// Ensure it doesn't crash.
		origin.publish_broadcast("test", broadcast.consume());
		origin.publish_broadcast("test", broadcast.consume());

		assert!(origin.consume().try_consume_broadcast("test").is_some());

		drop(broadcast);

		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
		assert!(origin.consume().try_consume_broadcast("test").is_none());
	}
	#[tokio::test]
	async fn test_consume_broadcast_async() {
		use tokio::sync::oneshot;

		let origin = Origin::produce();
		let consumer = origin.consume();

		let (tx, rx) = oneshot::channel();

		// consume_broadcast should wait for the broadcast to appear.
		tokio::spawn(async move {
			let result = consumer.consume_broadcast("test").await;
			tx.send(result).ok();
		});

		// Give the task a chance to start waiting.
		tokio::task::yield_now().await;

		// Now publish the broadcast.
		let broadcast = Broadcast::new().produce();
		origin.publish_broadcast("test", broadcast.consume());

		// The async consume_broadcast should resolve.
		let result = rx.await.unwrap();
		assert!(result.is_ok());
		assert!(result.unwrap().is_clone(&broadcast.consume()));
	}

	#[tokio::test]
	async fn test_consume_broadcast_sync() {
		let origin = Origin::produce();
		let broadcast = Broadcast::new().produce();

		// try_consume_broadcast returns None when not yet published.
		let consumer = origin.consume();
		assert!(consumer.try_consume_broadcast("test").is_none());

		// After publishing, returns Some.
		origin.publish_broadcast("test", broadcast.consume());
		assert!(consumer.try_consume_broadcast("test").is_some());
	}

	// There was a tokio bug where only the first 127 broadcasts would be received instantly.
	#[tokio::test]
	#[should_panic]
	async fn test_128() {
		let origin = Origin::produce();
		let broadcast = Broadcast::new().produce();

		let mut consumer = origin.consume();
		for i in 0..256 {
			origin.publish_broadcast(format!("test{i}"), broadcast.consume());
		}

		for i in 0..256 {
			consumer.assert_next(format!("test{i}"), &broadcast.consume());
		}
	}

	#[tokio::test]
	async fn test_128_fix() {
		let origin = Origin::produce();
		let broadcast = Broadcast::new().produce();

		let mut consumer = origin.consume();
		for i in 0..256 {
			origin.publish_broadcast(format!("test{i}"), broadcast.consume());
		}

		for i in 0..256 {
			// try_next does not have the same issue because it's synchronous.
			consumer.assert_try_next(format!("test{i}"), &broadcast.consume());
		}
	}

	#[tokio::test]
	async fn test_with_root_basic() {
		let origin = Origin::produce();
		let broadcast = Broadcast::new().produce();

		// Create a producer with root "/foo"
		let foo_producer = origin.with_root("foo").expect("should create root");
		assert_eq!(foo_producer.root().as_str(), "foo");

		let mut consumer = origin.consume();

		// When publishing to "bar/baz", it should actually publish to "foo/bar/baz"
		assert!(foo_producer.publish_broadcast("bar/baz", broadcast.consume()));
		// The original consumer should see the full path
		consumer.assert_next("foo/bar/baz", &broadcast.consume());

		// A consumer created from the rooted producer should see the stripped path
		let mut foo_consumer = foo_producer.consume();
		foo_consumer.assert_next("bar/baz", &broadcast.consume());
	}

	#[tokio::test]
	async fn test_with_root_nested() {
		let origin = Origin::produce();
		let broadcast = Broadcast::new().produce();

		// Create nested roots
		let foo_producer = origin.with_root("foo").expect("should create foo root");
		let foo_bar_producer = foo_producer.with_root("bar").expect("should create bar root");
		assert_eq!(foo_bar_producer.root().as_str(), "foo/bar");

		let mut consumer = origin.consume();

		// Publishing to "baz" should actually publish to "foo/bar/baz"
		assert!(foo_bar_producer.publish_broadcast("baz", broadcast.consume()));
		// The original consumer sees the full path
		consumer.assert_next("foo/bar/baz", &broadcast.consume());

		// Consumer from foo_bar_producer sees just "baz"
		let mut foo_bar_consumer = foo_bar_producer.consume();
		foo_bar_consumer.assert_next("baz", &broadcast.consume());
	}

	#[tokio::test]
	async fn test_with_filter_allows() {
		let origin = Origin::produce();
		let broadcast = Broadcast::new().produce();

		// Create a producer that can only publish to "allowed" paths
		let limited_producer = origin
			.with_filter(&["allowed/path1".into(), "allowed/path2".into()])
			.expect("should create limited producer");

		// Should be able to publish to allowed paths
		assert!(limited_producer.publish_broadcast("allowed/path1", broadcast.consume()));
		assert!(limited_producer.publish_broadcast("allowed/path1/nested", broadcast.consume()));
		assert!(limited_producer.publish_broadcast("allowed/path2", broadcast.consume()));

		// Should not be able to publish to disallowed paths
		assert!(!limited_producer.publish_broadcast("notallowed", broadcast.consume()));
		assert!(!limited_producer.publish_broadcast("allowed", broadcast.consume())); // Parent of allowed path
		assert!(!limited_producer.publish_broadcast("other/path", broadcast.consume()));
	}

	#[tokio::test]
	async fn test_with_filter_empty() {
		let origin = Origin::produce();

		// Creating a producer with no allowed paths should return None
		assert!(origin.with_filter(&[]).is_none());
	}

	#[tokio::test]
	async fn test_consume_with_filter() {
		let origin = Origin::produce();
		let broadcast1 = Broadcast::new().produce();
		let broadcast2 = Broadcast::new().produce();
		let broadcast3 = Broadcast::new().produce();

		let mut consumer = origin.consume();

		// Publish to different paths
		origin.publish_broadcast("allowed", broadcast1.consume());
		origin.publish_broadcast("allowed/nested", broadcast2.consume());
		origin.publish_broadcast("notallowed", broadcast3.consume());

		// Create a consumer that only sees "allowed" paths
		let mut limited_consumer = origin
			.consume()
			.with_filter(&["allowed".into()])
			.expect("should create limited consumer");

		// Should only receive broadcasts under "allowed"
		limited_consumer.assert_next("allowed", &broadcast1.consume());
		limited_consumer.assert_next("allowed/nested", &broadcast2.consume());
		limited_consumer.assert_next_wait(); // Should not see "notallowed"

		// Unscoped consumer should see all
		consumer.assert_next("allowed", &broadcast1.consume());
		consumer.assert_next("allowed/nested", &broadcast2.consume());
		consumer.assert_next("notallowed", &broadcast3.consume());
	}

	#[tokio::test]
	async fn test_consume_with_filter_multiple_prefixes() {
		let origin = Origin::produce();
		let broadcast1 = Broadcast::new().produce();
		let broadcast2 = Broadcast::new().produce();
		let broadcast3 = Broadcast::new().produce();

		origin.publish_broadcast("foo/test", broadcast1.consume());
		origin.publish_broadcast("bar/test", broadcast2.consume());
		origin.publish_broadcast("baz/test", broadcast3.consume());

		// Consumer that only sees "foo" and "bar" paths
		let mut limited_consumer = origin
			.consume()
			.with_filter(&["foo".into(), "bar".into()])
			.expect("should create limited consumer");

		limited_consumer.assert_next("foo/test", &broadcast1.consume());
		limited_consumer.assert_next("bar/test", &broadcast2.consume());
		limited_consumer.assert_next_wait(); // Should not see "baz/test"
	}

	#[tokio::test]
	async fn test_with_root_and_with_filter() {
		let origin = Origin::produce();
		let broadcast = Broadcast::new().produce();

		// User connects to /foo root
		let foo_producer = origin.with_root("foo").expect("should create foo root");

		// Limit them to publish only to "bar" and "goop/pee" within /foo
		let limited_producer = foo_producer
			.with_filter(&["bar".into(), "goop/pee".into()])
			.expect("should create limited producer");

		let mut consumer = origin.consume();

		// Should be able to publish to foo/bar and foo/goop/pee (but user sees as bar and goop/pee)
		assert!(limited_producer.publish_broadcast("bar", broadcast.consume()));
		assert!(limited_producer.publish_broadcast("bar/nested", broadcast.consume()));
		assert!(limited_producer.publish_broadcast("goop/pee", broadcast.consume()));
		assert!(limited_producer.publish_broadcast("goop/pee/nested", broadcast.consume()));

		// Should not be able to publish outside allowed paths
		assert!(!limited_producer.publish_broadcast("baz", broadcast.consume()));
		assert!(!limited_producer.publish_broadcast("goop", broadcast.consume())); // Parent of allowed
		assert!(!limited_producer.publish_broadcast("goop/other", broadcast.consume()));

		// Original consumer sees full paths
		consumer.assert_next("foo/bar", &broadcast.consume());
		consumer.assert_next("foo/bar/nested", &broadcast.consume());
		consumer.assert_next("foo/goop/pee", &broadcast.consume());
		consumer.assert_next("foo/goop/pee/nested", &broadcast.consume());
	}

	#[tokio::test]
	async fn test_with_root_and_consume_with_filter() {
		let origin = Origin::produce();
		let broadcast1 = Broadcast::new().produce();
		let broadcast2 = Broadcast::new().produce();
		let broadcast3 = Broadcast::new().produce();

		// Publish broadcasts
		origin.publish_broadcast("foo/bar/test", broadcast1.consume());
		origin.publish_broadcast("foo/goop/pee/test", broadcast2.consume());
		origin.publish_broadcast("foo/other/test", broadcast3.consume());

		// User connects to /foo root
		let foo_producer = origin.with_root("foo").expect("should create foo root");

		// Create consumer limited to "bar" and "goop/pee" within /foo
		let mut limited_consumer = foo_producer
			.consume()
			.with_filter(&["bar".into(), "goop/pee".into()])
			.expect("should create limited consumer");

		// Should only see allowed paths (without foo prefix)
		limited_consumer.assert_next("bar/test", &broadcast1.consume());
		limited_consumer.assert_next("goop/pee/test", &broadcast2.consume());
		limited_consumer.assert_next_wait(); // Should not see "other/test"
	}

	#[tokio::test]
	async fn test_with_root_unauthorized() {
		let origin = Origin::produce();

		// First limit the producer to specific paths
		let limited_producer = origin
			.with_filter(&["allowed".into()])
			.expect("should create limited producer");

		// Trying to create a root outside allowed paths should fail
		assert!(limited_producer.with_root("notallowed").is_none());

		// But creating a root within allowed paths should work
		let allowed_root = limited_producer
			.with_root("allowed")
			.expect("should create allowed root");
		assert_eq!(allowed_root.root().as_str(), "allowed");
	}

	#[tokio::test]
	async fn test_wildcard_permission() {
		let origin = Origin::produce();
		let broadcast = Broadcast::new().produce();

		// Producer with root access (empty string means wildcard)
		let root_producer = origin.clone();

		// Should be able to publish anywhere
		assert!(root_producer.publish_broadcast("any/path", broadcast.consume()));
		assert!(root_producer.publish_broadcast("other/path", broadcast.consume()));

		// Can create any root
		let foo_producer = root_producer.with_root("foo").expect("should create any root");
		assert_eq!(foo_producer.root().as_str(), "foo");
	}

	#[tokio::test]
	async fn test_consume_broadcast_with_permissions() {
		let origin = Origin::produce();
		let broadcast1 = Broadcast::new().produce();
		let broadcast2 = Broadcast::new().produce();

		origin.publish_broadcast("allowed/test", broadcast1.consume());
		origin.publish_broadcast("notallowed/test", broadcast2.consume());

		// Create limited consumer
		let limited_consumer = origin
			.consume()
			.with_filter(&["allowed".into()])
			.expect("should create limited consumer");

		// Should be able to get allowed broadcast
		let result = limited_consumer.try_consume_broadcast("allowed/test");
		assert!(result.is_some());
		assert!(result.unwrap().is_clone(&broadcast1.consume()));

		// Should not be able to get disallowed broadcast
		assert!(limited_consumer.try_consume_broadcast("notallowed/test").is_none());

		// Original consumer can get both
		let consumer = origin.consume();
		assert!(consumer.try_consume_broadcast("allowed/test").is_some());
		assert!(consumer.try_consume_broadcast("notallowed/test").is_some());
	}

	#[tokio::test]
	async fn test_nested_paths_with_permissions() {
		let origin = Origin::produce();
		let broadcast = Broadcast::new().produce();

		// Create producer limited to "a/b/c"
		let limited_producer = origin
			.with_filter(&["a/b/c".into()])
			.expect("should create limited producer");

		// Should be able to publish to exact path and nested paths
		assert!(limited_producer.publish_broadcast("a/b/c", broadcast.consume()));
		assert!(limited_producer.publish_broadcast("a/b/c/d", broadcast.consume()));
		assert!(limited_producer.publish_broadcast("a/b/c/d/e", broadcast.consume()));

		// Should not be able to publish to parent or sibling paths
		assert!(!limited_producer.publish_broadcast("a", broadcast.consume()));
		assert!(!limited_producer.publish_broadcast("a/b", broadcast.consume()));
		assert!(!limited_producer.publish_broadcast("a/b/other", broadcast.consume()));
	}

	#[tokio::test]
	async fn test_multiple_consumers_with_different_permissions() {
		let origin = Origin::produce();
		let broadcast1 = Broadcast::new().produce();
		let broadcast2 = Broadcast::new().produce();
		let broadcast3 = Broadcast::new().produce();

		// Publish to different paths
		origin.publish_broadcast("foo/test", broadcast1.consume());
		origin.publish_broadcast("bar/test", broadcast2.consume());
		origin.publish_broadcast("baz/test", broadcast3.consume());

		// Create consumers with different permissions
		let mut foo_consumer = origin
			.consume()
			.with_filter(&["foo".into()])
			.expect("should create foo consumer");

		let mut bar_consumer = origin
			.consume()
			.with_filter(&["bar".into()])
			.expect("should create bar consumer");

		let mut foobar_consumer = origin
			.consume()
			.with_filter(&["foo".into(), "bar".into()])
			.expect("should create foobar consumer");

		// Each consumer should only see their allowed paths
		foo_consumer.assert_next("foo/test", &broadcast1.consume());
		foo_consumer.assert_next_wait();

		bar_consumer.assert_next("bar/test", &broadcast2.consume());
		bar_consumer.assert_next_wait();

		foobar_consumer.assert_next("foo/test", &broadcast1.consume());
		foobar_consumer.assert_next("bar/test", &broadcast2.consume());
		foobar_consumer.assert_next_wait();
	}

	#[tokio::test]
	async fn test_select_with_empty_prefix() {
		let origin = Origin::produce();
		let broadcast1 = Broadcast::new().produce();
		let broadcast2 = Broadcast::new().produce();

		// User with root "demo" allowed to subscribe to "worm-node" and "foobar"
		let demo_producer = origin.with_root("demo").expect("should create demo root");
		let limited_producer = demo_producer
			.with_filter(&["worm-node".into(), "foobar".into()])
			.expect("should create limited producer");

		// Publish some broadcasts
		assert!(limited_producer.publish_broadcast("worm-node/test", broadcast1.consume()));
		assert!(limited_producer.publish_broadcast("foobar/test", broadcast2.consume()));

		// with_filter with empty prefix should keep the exact same "worm-node" and "foobar" nodes
		let mut consumer = limited_producer
			.consume()
			.with_filter(&["".into()])
			.expect("should create consumer with empty prefix");

		// Should still see both broadcasts
		consumer.assert_next("worm-node/test", &broadcast1.consume());
		consumer.assert_next("foobar/test", &broadcast2.consume());
		consumer.assert_next_wait();
	}

	#[tokio::test]
	async fn test_select_narrowing_scope() {
		let origin = Origin::produce();
		let broadcast1 = Broadcast::new().produce();
		let broadcast2 = Broadcast::new().produce();
		let broadcast3 = Broadcast::new().produce();

		// User with root "demo" allowed to subscribe to "worm-node" and "foobar"
		let demo_producer = origin.with_root("demo").expect("should create demo root");
		let limited_producer = demo_producer
			.with_filter(&["worm-node".into(), "foobar".into()])
			.expect("should create limited producer");

		// Publish broadcasts at different levels
		assert!(limited_producer.publish_broadcast("worm-node", broadcast1.consume()));
		assert!(limited_producer.publish_broadcast("worm-node/foo", broadcast2.consume()));
		assert!(limited_producer.publish_broadcast("foobar/bar", broadcast3.consume()));

		// Test 1: with_filter("worm-node") should result in a single "" node with contents of "worm-node" ONLY
		let mut worm_consumer = limited_producer
			.consume()
			.with_filter(&["worm-node".into()])
			.expect("should create worm-node consumer");

		// Should see worm-node content with paths stripped to ""
		worm_consumer.assert_next("worm-node", &broadcast1.consume());
		worm_consumer.assert_next("worm-node/foo", &broadcast2.consume());
		worm_consumer.assert_next_wait(); // Should NOT see foobar content

		// Test 2: with_filter("worm-node/foo") should result in a "" node with contents of "worm-node/foo"
		let mut foo_consumer = limited_producer
			.consume()
			.with_filter(&["worm-node/foo".into()])
			.expect("should create worm-node/foo consumer");

		foo_consumer.assert_next("worm-node/foo", &broadcast2.consume());
		foo_consumer.assert_next_wait(); // Should NOT see other content
	}

	#[tokio::test]
	async fn test_select_multiple_roots_with_empty_prefix() {
		let origin = Origin::produce();
		let broadcast1 = Broadcast::new().produce();
		let broadcast2 = Broadcast::new().produce();
		let broadcast3 = Broadcast::new().produce();

		// Producer with multiple allowed roots
		let limited_producer = origin
			.with_filter(&["app1".into(), "app2".into(), "shared".into()])
			.expect("should create limited producer");

		// Publish to each root
		assert!(limited_producer.publish_broadcast("app1/data", broadcast1.consume()));
		assert!(limited_producer.publish_broadcast("app2/config", broadcast2.consume()));
		assert!(limited_producer.publish_broadcast("shared/resource", broadcast3.consume()));

		// with_filter with empty prefix should maintain all roots
		let mut consumer = limited_producer
			.consume()
			.with_filter(&["".into()])
			.expect("should create consumer with empty prefix");

		// Should see all broadcasts from all roots
		consumer.assert_next("app1/data", &broadcast1.consume());
		consumer.assert_next("app2/config", &broadcast2.consume());
		consumer.assert_next("shared/resource", &broadcast3.consume());
		consumer.assert_next_wait();
	}

	#[tokio::test]
	async fn test_with_filter_with_empty_prefix() {
		let origin = Origin::produce();
		let broadcast = Broadcast::new().produce();

		// Producer with specific allowed paths
		let limited_producer = origin
			.with_filter(&["services/api".into(), "services/web".into()])
			.expect("should create limited producer");

		// with_filter with empty prefix should keep the same restrictions
		let same_producer = limited_producer
			.with_filter(&["".into()])
			.expect("should create producer with empty prefix");

		// Should still have the same publishing restrictions
		assert!(same_producer.publish_broadcast("services/api", broadcast.consume()));
		assert!(same_producer.publish_broadcast("services/web", broadcast.consume()));
		assert!(!same_producer.publish_broadcast("services/db", broadcast.consume()));
		assert!(!same_producer.publish_broadcast("other", broadcast.consume()));
	}

	#[tokio::test]
	async fn test_select_narrowing_to_deeper_path() {
		let origin = Origin::produce();
		let broadcast1 = Broadcast::new().produce();
		let broadcast2 = Broadcast::new().produce();
		let broadcast3 = Broadcast::new().produce();

		// Producer with broad permission
		let limited_producer = origin
			.with_filter(&["org".into()])
			.expect("should create limited producer");

		// Publish at various depths
		assert!(limited_producer.publish_broadcast("org/team1/project1", broadcast1.consume()));
		assert!(limited_producer.publish_broadcast("org/team1/project2", broadcast2.consume()));
		assert!(limited_producer.publish_broadcast("org/team2/project1", broadcast3.consume()));

		// Narrow down to team2 only
		let mut team2_consumer = limited_producer
			.consume()
			.with_filter(&["org/team2".into()])
			.expect("should create team2 consumer");

		team2_consumer.assert_next("org/team2/project1", &broadcast3.consume());
		team2_consumer.assert_next_wait(); // Should NOT see team1 content

		// Further narrow down to team1/project1
		let mut project1_consumer = limited_producer
			.consume()
			.with_filter(&["org/team1/project1".into()])
			.expect("should create project1 consumer");

		// Should only see project1 content at root
		project1_consumer.assert_next("org/team1/project1", &broadcast1.consume());
		project1_consumer.assert_next_wait();
	}

	#[tokio::test]
	async fn test_select_with_non_matching_prefix() {
		let origin = Origin::produce();

		// Producer with specific allowed paths
		let limited_producer = origin
			.with_filter(&["allowed/path".into()])
			.expect("should create limited producer");

		// Trying to with_filter with a completely different prefix should return None (consumer)
		assert!(
			limited_producer
				.consume()
				.with_filter(&["different/path".into()])
				.is_none()
		);

		// Similarly for with_filter (producer)
		assert!(limited_producer.with_filter(&["other/path".into()]).is_none());
	}

	// Regression test for https://github.com/moq-dev/moq/issues/910
	// with_root panics when String has trailing slash (AsPath for String skips normalization)
	#[tokio::test]
	async fn test_with_root_trailing_slash_consumer() {
		let origin = Origin::produce();

		// Use an owned String so the trailing slash is NOT normalized away.
		let prefix = "some_prefix/".to_string();
		let mut consumer = origin.consume().with_root(prefix).unwrap();

		let b = origin.create_broadcast("some_prefix/test").unwrap();
		consumer.assert_next("test", &b.consume());
	}

	// Same issue but for the producer side of with_root
	#[tokio::test]
	async fn test_with_root_trailing_slash_producer() {
		let origin = Origin::produce();

		// Use an owned String so the trailing slash is NOT normalized away.
		let prefix = "some_prefix/".to_string();
		let rooted = origin.with_root(prefix).unwrap();

		let b = rooted.create_broadcast("test").unwrap();

		let mut consumer = rooted.consume();
		consumer.assert_next("test", &b.consume());
	}

	// Verify close doesn't panic with trailing slash
	#[tokio::test]
	async fn test_with_root_trailing_slash_close() {
		tokio::time::pause();

		let origin = Origin::produce();

		let prefix = "some_prefix/".to_string();
		let mut consumer = origin.consume().with_root(prefix).unwrap();

		let b = origin.create_broadcast("some_prefix/test").unwrap();
		let bc = b.consume();
		consumer.assert_next("test", &bc);

		// Drop the broadcast producer to trigger close
		drop(b);
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;

		// No unannounce message, just verify the broadcast is closed.
		consumer.assert_next_wait();
		assert!(bc.closed().now_or_never().is_some());
	}

	#[tokio::test]
	async fn test_select_maintains_access_with_wider_prefix() {
		let origin = Origin::produce();
		let broadcast1 = Broadcast::new().produce();
		let broadcast2 = Broadcast::new().produce();

		// Setup: user with root "demo" allowed to subscribe to specific paths
		let demo_producer = origin.with_root("demo").expect("should create demo root");
		let user_producer = demo_producer
			.with_filter(&["worm-node".into(), "foobar".into()])
			.expect("should create user producer");

		// Publish some data
		assert!(user_producer.publish_broadcast("worm-node/data", broadcast1.consume()));
		assert!(user_producer.publish_broadcast("foobar", broadcast2.consume()));

		// Key test: with_filter with "" should maintain access to allowed roots
		let mut consumer = user_producer
			.consume()
			.with_filter(&["".into()])
			.expect("with_filter with empty prefix should not fail when user has specific permissions");

		// Should still receive broadcasts from allowed paths
		consumer.assert_next("worm-node/data", &broadcast1.consume());
		consumer.assert_next("foobar", &broadcast2.consume());
		consumer.assert_next_wait();

		// Also test that we can still narrow the scope
		let mut narrow_consumer = user_producer
			.consume()
			.with_filter(&["worm-node".into()])
			.expect("should be able to narrow scope to worm-node");

		narrow_consumer.assert_next("worm-node/data", &broadcast1.consume());
		narrow_consumer.assert_next_wait(); // Should not see foobar
	}
}
