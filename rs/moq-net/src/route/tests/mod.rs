//! The route layer's simulations: [`relay::Router`], the router that ships, and the
//! prototype it was measured against, each wired one per node through ordered,
//! holdable message queues, so a fixture can deliver messages out of step and replay
//! them late. Each round of deliveries takes [`HOP`] on a mocked clock.

mod algebra;
mod bakeoff;
mod compare;
mod multi;
mod multicast;
mod multicast_cost;
mod multicast_search;
mod sticky;
mod warm;
mod warmth;

use std::{
	collections::{BTreeMap, BTreeSet, VecDeque},
	sync::Arc,
	time::Duration,
};

use kio::clock::Instant;

use super::*;

pub(super) type Node = usize;

/// How long one round of deliveries takes on the mocked clock.
pub(super) const HOP: Duration = Duration::from_millis(25);

/// A contract path: a trailing `/` (a wildcard in the contract) is dropped.
pub(super) fn p(path: &str) -> Path {
	Path::new(path.strip_suffix('/').unwrap_or(path)).expect("a valid path")
}

/// How `node` names itself in a route's hops, fresh for each restart like its process.
pub(super) fn hop(node: Node, restarts: u32) -> Id {
	1_000 * (u64::from(restarts) + 1) + node as u64
}

/// Messages sent, as protocol counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Stats {
	/// Route messages: ROUTE and DEAD.
	pub route_msgs: usize,
	/// Routes those messages name: a DEAD counts itself and each route it moved.
	pub route_items: usize,
	/// Route messages that changed nothing but the hops: what naming them costs.
	pub hop_updates: usize,
	pub claim_msgs: usize,
	pub claim_items: usize,
	/// Messages delivered.
	pub delivered: usize,
}

/// How many scenarios a sweep runs: `MOQ_ROUTE_SCENARIOS`, or the gate's 150.
/// Each sweep draws its scenarios from one seeded stream, so a larger count
/// replays the gate's first and continues past them. Run a large count under
/// nextest's `sweep` profile (`just rs test -P sweep`), which waits up to an hour.
pub(super) fn scenarios() -> usize {
	std::env::var("MOQ_ROUTE_SCENARIOS").map_or(150, |count| count.parse().expect("MOQ_ROUTE_SCENARIOS"))
}

pub(super) struct XorShift(pub u64);

impl XorShift {
	pub fn next(&mut self, below: u64) -> u64 {
		self.0 ^= self.0 << 13;
		self.0 ^= self.0 >> 7;
		self.0 ^= self.0 << 17;
		self.0 % below
	}

	pub fn chance(&mut self, percent: u64) -> bool {
		self.next(100) < percent
	}
}
