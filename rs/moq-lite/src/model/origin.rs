use std::{
	collections::HashMap,
	sync::atomic::{AtomicU64, Ordering},
};
use tokio::sync::mpsc;
use web_async::Lock;

use super::BroadcastConsumer;
use crate::{AsPath, Broadcast, BroadcastProducer, Path, PathOwned};

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
		self.tx.send((path, Some(broadcast))).expect("consumer closed");
	}

	fn reannounce(&self, path: impl AsPath, broadcast: BroadcastConsumer) {
		let path = path.as_path().strip_prefix(&self.root).unwrap().to_owned();
		self.tx.send((path.clone(), None)).expect("consumer closed");
		self.tx.send((path, Some(broadcast))).expect("consumer closed");
	}

	fn unannounce(&self, path: impl AsPath) {
		let path = path.as_path().strip_prefix(&self.root).unwrap().to_owned();
		self.tx.send((path, None)).expect("consumer closed");
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

	fn reannounce(&mut self, path: impl AsPath, broadcast: &BroadcastConsumer) {
		for consumer in self.consumers.values() {
			consumer.reannounce(path.as_path(), broadcast.clone());
		}

		if let Some(parent) = &self.parent {
			parent.lock().reannounce(path, broadcast);
		}
	}

	fn unannounce(&mut self, path: impl AsPath) {
		for consumer in self.consumers.values() {
			consumer.unannounce(path.as_path());
		}

		if let Some(parent) = &self.parent {
			parent.lock().unannounce(path);
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
			let old = existing.active.clone();
			existing.active = broadcast.clone();
			existing.backup.push(old);

			self.notify.lock().reannounce(full, broadcast);
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

	// Returns true if the broadcast should be unannounced.
	fn remove(&mut self, full: impl AsPath, broadcast: BroadcastConsumer, relative: impl AsPath) {
		let full = full.as_path();
		let relative = relative.as_path();

		if let Some((dir, relative)) = relative.next_part() {
			let nested = self.entry(dir);
			let mut locked = nested.lock();
			locked.remove(&full, broadcast, &relative);

			if locked.is_empty() {
				drop(locked);
				self.nested.remove(dir);
			}
		} else {
			let entry = match &mut self.broadcast {
				Some(existing) => existing,
				None => return,
			};

			// See if we can remove the broadcast from the backup list.
			let pos = entry.backup.iter().position(|b| b.is_clone(&broadcast));
			if let Some(pos) = pos {
				entry.backup.remove(pos);
				// Nothing else to do
				return;
			}

			// Okay so it must be the active broadcast or else we fucked up.
			assert!(entry.active.is_clone(&broadcast));

			// If there's a backup broadcast, then announce it.
			if let Some(active) = entry.backup.pop() {
				entry.active = active;
				self.notify.lock().reannounce(full, &entry.active);
			} else {
				// No more backups, so remove the entry.
				self.broadcast = None;
				self.notify.lock().unannounce(full);
			}
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

/// A broadcast path and its associated consumer, or None if closed.
pub type OriginAnnounce = (PathOwned, Option<BroadcastConsumer>);

/// A collection of broadcasts that can be published and subscribed to.
pub struct Origin {}

impl Origin {
	pub fn produce() -> OriginProducer {
		OriginProducer::new()
	}
}

/// Announces broadcasts to consumers over the network.
#[derive(Clone, Default)]
pub struct OriginProducer {
	// The roots of the tree that we are allowed to publish.
	// A path of "" means we can publish anything.
	nodes: OriginNodes,

	/// The prefix that is automatically stripped from all paths.
	root: PathOwned,
}

impl OriginProducer {
	pub fn new() -> Self {
		Self::default()
	}

	/// Create and publish a new broadcast, returning the producer.
	///
	/// This is a helper method when you only want to publish a broadcast to a single origin.
	/// Returns [None] if the broadcast is not allowed to be published.
	pub fn create_broadcast(&self, path: impl AsPath) -> Option<BroadcastProducer> {
		let broadcast = Broadcast::produce();
		self.publish_broadcast(path, broadcast.consume()).then_some(broadcast)
	}

	/// Publish a broadcast, announcing it to all consumers.
	///
	/// The broadcast will be unannounced when it is closed.
	/// If there is already a broadcast with the same path, then it will be replaced and reannounced.
	/// If the old broadcast is closed before the new one, then nothing will happen.
	/// If the new broadcast is closed before the old one, then the old broadcast will be reannounced.
	///
	/// Returns false if the broadcast is not allowed to be published.
	pub fn publish_broadcast(&self, path: impl AsPath, broadcast: BroadcastConsumer) -> bool {
		let path = path.as_path();

		let (root, rest) = match self.nodes.get(&path) {
			Some(root) => root,
			None => return false,
		};

		let full = self.root.join(&path);

		root.lock().publish(&full, &broadcast, &rest);
		let root = root.clone();

		web_async::spawn(async move {
			broadcast.closed().await;
			root.lock().remove(&full, broadcast, &rest);
		});

		true
	}

	/// Returns a new OriginProducer where all published broadcasts MUST match one of the prefixes.
	///
	/// Returns None if there are no legal prefixes.
	pub fn publish_only(&self, prefixes: &[Path]) -> Option<OriginProducer> {
		Some(OriginProducer {
			nodes: self.nodes.select(prefixes)?,
			root: self.root.clone(),
		})
	}

	/// Subscribe to all announced broadcasts.
	pub fn consume(&self) -> OriginConsumer {
		OriginConsumer::new(self.root.clone(), self.nodes.clone())
	}

	/// Subscribe to all announced broadcasts matching the prefix.
	///
	/// TODO: Don't use overlapping prefixes or duplicates will be published.
	///
	/// Returns None if there are no legal prefixes.
	pub fn consume_only(&self, prefixes: &[Path]) -> Option<OriginConsumer> {
		Some(OriginConsumer::new(self.root.clone(), self.nodes.select(prefixes)?))
	}

	/// Subscribe to a specific broadcast.
	///
	/// Returns None if the broadcast is not found.
	pub fn consume_broadcast(&self, path: impl AsPath) -> Option<BroadcastConsumer> {
		let path = path.as_path();
		let (root, rest) = self.nodes.get(&path)?;
		let state = root.lock();
		state.consume_broadcast(&rest)
	}

	/// Returns a new OriginProducer that automatically strips out the provided prefix.
	///
	/// Returns None if the provided root is not authorized; when publish_only was already used without a wildcard.
	pub fn with_root(&self, prefix: impl AsPath) -> Option<Self> {
		let prefix = prefix.as_path();

		Some(Self {
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

	/// Returns the next (un)announced broadcast and the absolute path.
	///
	/// The broadcast will only be announced if it was previously unannounced.
	/// The same path won't be announced/unannounced twice, instead it will toggle.
	/// Returns None if the consumer is closed.
	///
	/// Note: The returned path is absolute and will always match this consumer's prefix.
	pub async fn announced(&mut self) -> Option<OriginAnnounce> {
		self.updates.recv().await
	}

	/// Returns the next (un)announced broadcast and the absolute path without blocking.
	///
	/// Returns None if there is no update available; NOT because the consumer is closed.
	/// You have to use `is_closed` to check if the consumer is closed.
	pub fn try_announced(&mut self) -> Option<OriginAnnounce> {
		self.updates.try_recv().ok()
	}

	pub fn consume(&self) -> Self {
		self.clone()
	}

	/// Get a specific broadcast by path.
	///
	/// TODO This should include announcement support.
	///
	/// Returns None if the path hasn't been announced yet.
	pub fn consume_broadcast(&self, path: impl AsPath) -> Option<BroadcastConsumer> {
		let path = path.as_path();
		let (root, rest) = self.nodes.get(&path)?;
		let state = root.lock();
		state.consume_broadcast(&rest)
	}

	/// Returns a new OriginConsumer that only consumes broadcasts matching one of the prefixes.
	///
	/// Returns None if there are no legal prefixes (would always return None).
	pub fn consume_only(&self, prefixes: &[Path]) -> Option<OriginConsumer> {
		Some(OriginConsumer::new(self.root.clone(), self.nodes.select(prefixes)?))
	}

	/// Returns a new OriginConsumer that automatically strips out the provided prefix.
	///
	/// Returns None if the provided root is not authorized; when consume_only was already used without a wildcard.
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
		let (path, active) = self.announced().now_or_never().expect("next blocked").expect("no next");
		assert_eq!(path, expected, "wrong path");
		assert!(active.unwrap().is_clone(broadcast), "should be the same broadcast");
	}

	pub fn assert_try_next(&mut self, expected: impl AsPath, broadcast: &BroadcastConsumer) {
		let expected = expected.as_path();
		let (path, active) = self.try_announced().expect("no next");
		assert_eq!(path, expected, "wrong path");
		assert!(active.unwrap().is_clone(broadcast), "should be the same broadcast");
	}

	pub fn assert_next_none(&mut self, expected: impl AsPath) {
		let expected = expected.as_path();
		let (path, active) = self.announced().now_or_never().expect("next blocked").expect("no next");
		assert_eq!(path, expected, "wrong path");
		assert!(active.is_none(), "should be unannounced");
	}

	pub fn assert_next_wait(&mut self) {
		if let Some(res) = self.announced().now_or_never() {
			panic!("next should block: got {:?}", res.map(|(path, _)| path));
		}
	}

	/*
	pub fn assert_next_closed(&mut self) {
		assert!(
			self.announced().now_or_never().expect("next blocked").is_none(),
			"next should be closed"
		);
	}
	*/
}

#[cfg(test)]
mod tests {
	use crate::Broadcast;

	use super::*;

	#[tokio::test]
	async fn test_announce() {
		let origin = Origin::produce();
		let broadcast1 = Broadcast::produce();
		let broadcast2 = Broadcast::produce();

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
		drop(broadcast1);

		// Wait for the async task to run.
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;

		// All consumers should get a None now.
		consumer1.assert_next_none("test1");
		consumer2.assert_next_none("test1");
		consumer1.assert_next_wait();
		consumer2.assert_next_wait();

		// And a new consumer only gets the last broadcast.
		let mut consumer3 = origin.consume();
		consumer3.assert_next("test2", &broadcast2.consume());
		consumer3.assert_next_wait();

		// Close the other producer and make sure it cleans up
		drop(broadcast2);

		// Wait for the async task to run.
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;

		consumer1.assert_next_none("test2");
		consumer2.assert_next_none("test2");
		consumer3.assert_next_none("test2");

		/* TODO close the origin consumer when the producer is dropped
		consumer1.assert_next_closed();
		consumer2.assert_next_closed();
		consumer3.assert_next_closed();
		*/
	}

	#[tokio::test]
	async fn test_duplicate() {
		let origin = Origin::produce();

		let broadcast1 = Broadcast::produce();
		let broadcast2 = Broadcast::produce();
		let broadcast3 = Broadcast::produce();

		let consumer1 = broadcast1.consume();
		let consumer2 = broadcast2.consume();
		let consumer3 = broadcast3.consume();

		let mut consumer = origin.consume();

		origin.publish_broadcast("test", consumer1.clone());
		origin.publish_broadcast("test", consumer2.clone());
		origin.publish_broadcast("test", consumer3.clone());
		assert!(consumer.consume_broadcast("test").is_some());

		consumer.assert_next("test", &consumer1);
		consumer.assert_next_none("test");
		consumer.assert_next("test", &consumer2);
		consumer.assert_next_none("test");
		consumer.assert_next("test", &consumer3);

		// Drop the backup, nothing should change.
		drop(broadcast2);

		// Wait for the async task to run.
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;

		assert!(consumer.consume_broadcast("test").is_some());
		consumer.assert_next_wait();

		// Drop the active, we should reannounce.
		drop(broadcast3);

		// Wait for the async task to run.
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;

		assert!(consumer.consume_broadcast("test").is_some());
		consumer.assert_next_none("test");
		consumer.assert_next("test", &consumer1);

		// Drop the final broadcast, we should unannounce.
		drop(broadcast1);

		// Wait for the async task to run.
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
		assert!(consumer.consume_broadcast("test").is_none());

		consumer.assert_next_none("test");
		consumer.assert_next_wait();
	}

	#[tokio::test]
	async fn test_duplicate_reverse() {
		let origin = Origin::produce();
		let broadcast1 = Broadcast::produce();
		let broadcast2 = Broadcast::produce();

		origin.publish_broadcast("test", broadcast1.consume());
		origin.publish_broadcast("test", broadcast2.consume());
		assert!(origin.consume_broadcast("test").is_some());

		// This is harder, dropping the new broadcast first.
		drop(broadcast2);

		// Wait for the cleanup async task to run.
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
		assert!(origin.consume_broadcast("test").is_some());

		drop(broadcast1);

		// Wait for the cleanup async task to run.
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
		assert!(origin.consume_broadcast("test").is_none());
	}

	#[tokio::test]
	async fn test_double_publish() {
		let origin = Origin::produce();
		let broadcast = Broadcast::produce();

		// Ensure it doesn't crash.
		origin.publish_broadcast("test", broadcast.consume());
		origin.publish_broadcast("test", broadcast.consume());

		assert!(origin.consume_broadcast("test").is_some());

		drop(broadcast);

		// Wait for the async task to run.
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
		assert!(origin.consume_broadcast("test").is_none());
	}
	// There was a tokio bug where only the first 127 broadcasts would be received instantly.
	#[tokio::test]
	#[should_panic]
	async fn test_128() {
		let origin = Origin::produce();
		let broadcast = Broadcast::produce();

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
		let broadcast = Broadcast::produce();

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
		let broadcast = Broadcast::produce();

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
		let broadcast = Broadcast::produce();

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
	async fn test_publish_only_allows() {
		let origin = Origin::produce();
		let broadcast = Broadcast::produce();

		// Create a producer that can only publish to "allowed" paths
		let limited_producer = origin
			.publish_only(&["allowed/path1".into(), "allowed/path2".into()])
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
	async fn test_publish_only_empty() {
		let origin = Origin::produce();

		// Creating a producer with no allowed paths should return None
		assert!(origin.publish_only(&[]).is_none());
	}

	#[tokio::test]
	async fn test_consume_only_filters() {
		let origin = Origin::produce();
		let broadcast1 = Broadcast::produce();
		let broadcast2 = Broadcast::produce();
		let broadcast3 = Broadcast::produce();

		let mut consumer = origin.consume();

		// Publish to different paths
		origin.publish_broadcast("allowed", broadcast1.consume());
		origin.publish_broadcast("allowed/nested", broadcast2.consume());
		origin.publish_broadcast("notallowed", broadcast3.consume());

		// Create a consumer that only sees "allowed" paths
		let mut limited_consumer = origin
			.consume_only(&["allowed".into()])
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
	async fn test_consume_only_multiple_prefixes() {
		let origin = Origin::produce();
		let broadcast1 = Broadcast::produce();
		let broadcast2 = Broadcast::produce();
		let broadcast3 = Broadcast::produce();

		origin.publish_broadcast("foo/test", broadcast1.consume());
		origin.publish_broadcast("bar/test", broadcast2.consume());
		origin.publish_broadcast("baz/test", broadcast3.consume());

		// Consumer that only sees "foo" and "bar" paths
		let mut limited_consumer = origin
			.consume_only(&["foo".into(), "bar".into()])
			.expect("should create limited consumer");

		limited_consumer.assert_next("foo/test", &broadcast1.consume());
		limited_consumer.assert_next("bar/test", &broadcast2.consume());
		limited_consumer.assert_next_wait(); // Should not see "baz/test"
	}

	#[tokio::test]
	async fn test_with_root_and_publish_only() {
		let origin = Origin::produce();
		let broadcast = Broadcast::produce();

		// User connects to /foo root
		let foo_producer = origin.with_root("foo").expect("should create foo root");

		// Limit them to publish only to "bar" and "goop/pee" within /foo
		let limited_producer = foo_producer
			.publish_only(&["bar".into(), "goop/pee".into()])
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
	async fn test_with_root_and_consume_only() {
		let origin = Origin::produce();
		let broadcast1 = Broadcast::produce();
		let broadcast2 = Broadcast::produce();
		let broadcast3 = Broadcast::produce();

		// Publish broadcasts
		origin.publish_broadcast("foo/bar/test", broadcast1.consume());
		origin.publish_broadcast("foo/goop/pee/test", broadcast2.consume());
		origin.publish_broadcast("foo/other/test", broadcast3.consume());

		// User connects to /foo root
		let foo_producer = origin.with_root("foo").expect("should create foo root");

		// Create consumer limited to "bar" and "goop/pee" within /foo
		let mut limited_consumer = foo_producer
			.consume_only(&["bar".into(), "goop/pee".into()])
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
			.publish_only(&["allowed".into()])
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
		let broadcast = Broadcast::produce();

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
		let broadcast1 = Broadcast::produce();
		let broadcast2 = Broadcast::produce();

		origin.publish_broadcast("allowed/test", broadcast1.consume());
		origin.publish_broadcast("notallowed/test", broadcast2.consume());

		// Create limited consumer
		let limited_consumer = origin
			.consume_only(&["allowed".into()])
			.expect("should create limited consumer");

		// Should be able to get allowed broadcast
		let result = limited_consumer.consume_broadcast("allowed/test");
		assert!(result.is_some());
		assert!(result.unwrap().is_clone(&broadcast1.consume()));

		// Should not be able to get disallowed broadcast
		assert!(limited_consumer.consume_broadcast("notallowed/test").is_none());

		// Original consumer can get both
		let consumer = origin.consume();
		assert!(consumer.consume_broadcast("allowed/test").is_some());
		assert!(consumer.consume_broadcast("notallowed/test").is_some());
	}

	#[tokio::test]
	async fn test_nested_paths_with_permissions() {
		let origin = Origin::produce();
		let broadcast = Broadcast::produce();

		// Create producer limited to "a/b/c"
		let limited_producer = origin
			.publish_only(&["a/b/c".into()])
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
		let broadcast1 = Broadcast::produce();
		let broadcast2 = Broadcast::produce();
		let broadcast3 = Broadcast::produce();

		// Publish to different paths
		origin.publish_broadcast("foo/test", broadcast1.consume());
		origin.publish_broadcast("bar/test", broadcast2.consume());
		origin.publish_broadcast("baz/test", broadcast3.consume());

		// Create consumers with different permissions
		let mut foo_consumer = origin
			.consume_only(&["foo".into()])
			.expect("should create foo consumer");

		let mut bar_consumer = origin
			.consume_only(&["bar".into()])
			.expect("should create bar consumer");

		let mut foobar_consumer = origin
			.consume_only(&["foo".into(), "bar".into()])
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
		let broadcast1 = Broadcast::produce();
		let broadcast2 = Broadcast::produce();

		// User with root "demo" allowed to subscribe to "worm-node" and "foobar"
		let demo_producer = origin.with_root("demo").expect("should create demo root");
		let limited_producer = demo_producer
			.publish_only(&["worm-node".into(), "foobar".into()])
			.expect("should create limited producer");

		// Publish some broadcasts
		assert!(limited_producer.publish_broadcast("worm-node/test", broadcast1.consume()));
		assert!(limited_producer.publish_broadcast("foobar/test", broadcast2.consume()));

		// consume_only with empty prefix should keep the exact same "worm-node" and "foobar" nodes
		let mut consumer = limited_producer
			.consume_only(&["".into()])
			.expect("should create consumer with empty prefix");

		// Should still see both broadcasts
		consumer.assert_next("worm-node/test", &broadcast1.consume());
		consumer.assert_next("foobar/test", &broadcast2.consume());
		consumer.assert_next_wait();
	}

	#[tokio::test]
	async fn test_select_narrowing_scope() {
		let origin = Origin::produce();
		let broadcast1 = Broadcast::produce();
		let broadcast2 = Broadcast::produce();
		let broadcast3 = Broadcast::produce();

		// User with root "demo" allowed to subscribe to "worm-node" and "foobar"
		let demo_producer = origin.with_root("demo").expect("should create demo root");
		let limited_producer = demo_producer
			.publish_only(&["worm-node".into(), "foobar".into()])
			.expect("should create limited producer");

		// Publish broadcasts at different levels
		assert!(limited_producer.publish_broadcast("worm-node", broadcast1.consume()));
		assert!(limited_producer.publish_broadcast("worm-node/foo", broadcast2.consume()));
		assert!(limited_producer.publish_broadcast("foobar/bar", broadcast3.consume()));

		// Test 1: consume_only("worm-node") should result in a single "" node with contents of "worm-node" ONLY
		let mut worm_consumer = limited_producer
			.consume_only(&["worm-node".into()])
			.expect("should create worm-node consumer");

		// Should see worm-node content with paths stripped to ""
		worm_consumer.assert_next("worm-node", &broadcast1.consume());
		worm_consumer.assert_next("worm-node/foo", &broadcast2.consume());
		worm_consumer.assert_next_wait(); // Should NOT see foobar content

		// Test 2: consume_only("worm-node/foo") should result in a "" node with contents of "worm-node/foo"
		let mut foo_consumer = limited_producer
			.consume_only(&["worm-node/foo".into()])
			.expect("should create worm-node/foo consumer");

		foo_consumer.assert_next("worm-node/foo", &broadcast2.consume());
		foo_consumer.assert_next_wait(); // Should NOT see other content
	}

	#[tokio::test]
	async fn test_select_multiple_roots_with_empty_prefix() {
		let origin = Origin::produce();
		let broadcast1 = Broadcast::produce();
		let broadcast2 = Broadcast::produce();
		let broadcast3 = Broadcast::produce();

		// Producer with multiple allowed roots
		let limited_producer = origin
			.publish_only(&["app1".into(), "app2".into(), "shared".into()])
			.expect("should create limited producer");

		// Publish to each root
		assert!(limited_producer.publish_broadcast("app1/data", broadcast1.consume()));
		assert!(limited_producer.publish_broadcast("app2/config", broadcast2.consume()));
		assert!(limited_producer.publish_broadcast("shared/resource", broadcast3.consume()));

		// consume_only with empty prefix should maintain all roots
		let mut consumer = limited_producer
			.consume_only(&["".into()])
			.expect("should create consumer with empty prefix");

		// Should see all broadcasts from all roots
		consumer.assert_next("app1/data", &broadcast1.consume());
		consumer.assert_next("app2/config", &broadcast2.consume());
		consumer.assert_next("shared/resource", &broadcast3.consume());
		consumer.assert_next_wait();
	}

	#[tokio::test]
	async fn test_publish_only_with_empty_prefix() {
		let origin = Origin::produce();
		let broadcast = Broadcast::produce();

		// Producer with specific allowed paths
		let limited_producer = origin
			.publish_only(&["services/api".into(), "services/web".into()])
			.expect("should create limited producer");

		// publish_only with empty prefix should keep the same restrictions
		let same_producer = limited_producer
			.publish_only(&["".into()])
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
		let broadcast1 = Broadcast::produce();
		let broadcast2 = Broadcast::produce();
		let broadcast3 = Broadcast::produce();

		// Producer with broad permission
		let limited_producer = origin
			.publish_only(&["org".into()])
			.expect("should create limited producer");

		// Publish at various depths
		assert!(limited_producer.publish_broadcast("org/team1/project1", broadcast1.consume()));
		assert!(limited_producer.publish_broadcast("org/team1/project2", broadcast2.consume()));
		assert!(limited_producer.publish_broadcast("org/team2/project1", broadcast3.consume()));

		// Narrow down to team2 only
		let mut team2_consumer = limited_producer
			.consume_only(&["org/team2".into()])
			.expect("should create team2 consumer");

		team2_consumer.assert_next("org/team2/project1", &broadcast3.consume());
		team2_consumer.assert_next_wait(); // Should NOT see team1 content

		// Further narrow down to team1/project1
		let mut project1_consumer = limited_producer
			.consume_only(&["org/team1/project1".into()])
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
			.publish_only(&["allowed/path".into()])
			.expect("should create limited producer");

		// Trying to consume_only with a completely different prefix should return None
		assert!(limited_producer.consume_only(&["different/path".into()]).is_none());

		// Similarly for publish_only
		assert!(limited_producer.publish_only(&["other/path".into()]).is_none());
	}

	#[tokio::test]
	async fn test_select_maintains_access_with_wider_prefix() {
		let origin = Origin::produce();
		let broadcast1 = Broadcast::produce();
		let broadcast2 = Broadcast::produce();

		// Setup: user with root "demo" allowed to subscribe to specific paths
		let demo_producer = origin.with_root("demo").expect("should create demo root");
		let user_producer = demo_producer
			.publish_only(&["worm-node".into(), "foobar".into()])
			.expect("should create user producer");

		// Publish some data
		assert!(user_producer.publish_broadcast("worm-node/data", broadcast1.consume()));
		assert!(user_producer.publish_broadcast("foobar", broadcast2.consume()));

		// Key test: consume_only with "" should maintain access to allowed roots
		let mut consumer = user_producer
			.consume_only(&["".into()])
			.expect("consume_only with empty prefix should not fail when user has specific permissions");

		// Should still receive broadcasts from allowed paths
		consumer.assert_next("worm-node/data", &broadcast1.consume());
		consumer.assert_next("foobar", &broadcast2.consume());
		consumer.assert_next_wait();

		// Also test that we can still narrow the scope
		let mut narrow_consumer = user_producer
			.consume_only(&["worm-node".into()])
			.expect("should be able to narrow scope to worm-node");

		narrow_consumer.assert_next("worm-node/data", &broadcast1.consume());
		narrow_consumer.assert_next_wait(); // Should not see foobar
	}
}
