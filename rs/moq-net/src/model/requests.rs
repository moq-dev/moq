//! A coalescing request queue drained by dynamic handlers.
use std::{
	borrow::Borrow,
	collections::{HashMap, VecDeque},
	hash::Hash,
};

/// Requests keyed by `K`, drained in FIFO order by a pool of handlers.
///
/// This is the state shared between requesters and the `Dynamic` handlers that
/// serve them (origin broadcasts, broadcast tracks, track fetches), typically
/// inside a [`kio::Shared`] so both sides work under one lock. It encodes two
/// invariants in one place:
///
/// - A request is only queued while a handler is alive to drain it
///   ([`Self::insert`] fails otherwise), so nothing waits on an empty pool.
/// - A repeat request joins the pending entry ([`Self::join`]) instead of
///   queueing a duplicate, so N requesters cost one handler round-trip.
///
/// [`Self::pop`] hands out only the key: the entry stays pending (still
/// joinable) until the caller explicitly removes it, on resolution or via
/// [`Self::drain_queued`] when the last handler leaves.
pub(crate) struct Requests<K, V> {
	// Every pending request: queued or already handed to a handler.
	pending: HashMap<K, V>,
	// The queued (not yet popped) subset, FIFO. Every key here is in `pending`.
	order: VecDeque<K>,
	// Live handler count; gates `insert`.
	handlers: usize,
}

impl<K, V> Default for Requests<K, V> {
	fn default() -> Self {
		Self {
			pending: HashMap::new(),
			order: VecDeque::new(),
			handlers: 0,
		}
	}
}

impl<K: Clone + Eq + Hash, V> Requests<K, V> {
	/// Join the pending request for `key`, queued or already handed out.
	pub fn join<Q>(&mut self, key: &Q) -> Option<&mut V>
	where
		K: Borrow<Q>,
		Q: Eq + Hash + ?Sized,
	{
		self.pending.get_mut(key)
	}

	/// Queue a new request, handing `value` back when no handler is alive to
	/// drain it.
	///
	/// The caller should [`Self::join`] first; inserting over an existing entry
	/// would orphan it.
	pub fn insert(&mut self, key: K, value: V) -> Result<(), V> {
		if self.handlers == 0 {
			return Err(value);
		}
		let prev = self.pending.insert(key.clone(), value);
		debug_assert!(prev.is_none(), "insert over a pending request; join it instead");
		self.order.push_back(key);
		Ok(())
	}

	/// Pop the next queued key for a handler to serve. The entry stays pending
	/// (joinable) until removed.
	pub fn pop(&mut self) -> Option<K> {
		self.order.pop_front()
	}

	/// The pending request for `key`, if any.
	pub fn get<Q>(&self, key: &Q) -> Option<&V>
	where
		K: Borrow<Q>,
		Q: Eq + Hash + ?Sized,
	{
		self.pending.get(key)
	}

	/// Remove and return the pending request for `key`, if any.
	pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
	where
		K: Borrow<Q>,
		Q: Eq + Hash + ?Sized,
	{
		self.pending.remove(key)
	}

	/// Remove the pending request for `key` if `f` says it's the caller's own,
	/// leaving a newer entry that replaced it alone.
	pub fn remove_if<Q>(&mut self, key: &Q, f: impl FnOnce(&V) -> bool) -> Option<V>
	where
		K: Borrow<Q>,
		Q: Eq + Hash + ?Sized,
	{
		if !self.pending.get(key).is_some_and(f) {
			return None;
		}
		self.pending.remove(key)
	}

	/// Returns `true` if a queued (not yet popped) request exists.
	pub fn has_queued(&self) -> bool {
		!self.order.is_empty()
	}

	/// Returns `true` when nothing is pending, queued or handed out.
	#[cfg(test)]
	pub fn is_empty(&self) -> bool {
		self.pending.is_empty()
	}

	/// Count a new live handler.
	pub fn add_handler(&mut self) {
		self.handlers += 1;
	}

	/// Count a handler out, returning `true` if it was the last one.
	pub fn remove_handler(&mut self) -> bool {
		self.handlers -= 1;
		self.handlers == 0
	}

	/// Returns `true` while at least one handler is alive.
	pub fn has_handlers(&self) -> bool {
		self.handlers > 0
	}

	/// Remove and return every queued (never popped) request, so the caller can
	/// reject them when the last handler leaves. Popped entries stay pending,
	/// owned by whichever handler took them.
	pub fn drain_queued(&mut self) -> Vec<V> {
		self.order
			.drain(..)
			.filter_map(|key| self.pending.remove(&key))
			.collect()
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn insert_gated_on_handlers() {
		let mut requests = Requests::<u64, &str>::default();
		assert_eq!(requests.insert(1, "a"), Err("a"));

		requests.add_handler();
		assert!(requests.insert(1, "a").is_ok());
		assert_eq!(requests.pop(), Some(1));
		assert_eq!(requests.pop(), None);

		assert!(requests.remove_handler());
		assert_eq!(requests.insert(2, "b"), Err("b"));
	}

	#[test]
	fn popped_stays_joinable_until_removed() {
		let mut requests = Requests::<u64, &str>::default();
		requests.add_handler();
		assert!(requests.insert(1, "a").is_ok());

		assert_eq!(requests.pop(), Some(1));
		assert_eq!(requests.join(&1), Some(&mut "a"));
		assert!(!requests.has_queued());

		assert_eq!(requests.remove(&1), Some("a"));
		assert!(requests.is_empty());
	}

	#[test]
	fn remove_if_guards_identity() {
		let mut requests = Requests::<u64, &str>::default();
		requests.add_handler();
		assert!(requests.insert(1, "old").is_ok());

		// A stale owner (matching "old") must not clobber the newer entry.
		*requests.join(&1).unwrap() = "new";
		assert_eq!(requests.remove_if(&1, |v| *v == "old"), None);
		assert_eq!(requests.remove_if(&1, |v| *v == "new"), Some("new"));
	}

	#[test]
	fn drain_queued_spares_popped() {
		let mut requests = Requests::<u64, &str>::default();
		requests.add_handler();
		assert!(requests.insert(1, "handed out").is_ok());
		assert!(requests.insert(2, "queued").is_ok());
		assert_eq!(requests.pop(), Some(1));

		assert!(requests.remove_handler());
		assert_eq!(requests.drain_queued(), vec!["queued"]);

		// The handed-out request is still pending, owned by its handler.
		assert_eq!(requests.get(&1), Some(&"handed out"));
		assert!(!requests.has_queued());
	}
}
