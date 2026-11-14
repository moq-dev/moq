//! The per-origin route layer, as one node's state machine.
//!
//! Routing follows the path rule the routes epic's Plan item 27 picked
//! ([`path::Router`]): a route per publisher and prefix, believed by its own path. Each
//! publisher versions its own claims with a revision, and a node merges every
//! neighbour's certified view claim by claim, so no stale copy revives a withdrawal it
//! already knows. [`node::Node`] drives it for every lite-07 session of one origin. See
//! `quest/m2/cluster-routes/README.md` for the contract.
//!
//! The router does no I/O: the caller feeds it what each neighbour sent and drains its
//! messages and reader [`Event`]s. Neighbour keys `N` and claim attributes `A` are opaque
//! to it.

#![cfg_attr(
	not(feature = "async"),
	allow(dead_code, reason = "only the async session drives it")
)]

pub(crate) mod node;
pub(crate) mod path;
mod proof;
mod reader;
mod rtt;
mod view;

#[cfg(test)]
mod tests;

use std::{fmt, sync::Arc};

use crate::Path;

pub(crate) use node::Node;
#[cfg(feature = "async")]
pub(crate) use proof::EXPORTER_LABEL;
pub use proof::RouteKey;
pub(crate) use proof::{Proof, Transcript};
#[cfg(feature = "async")]
pub(crate) use rtt::LinkRtt;
pub(crate) use view::{View, ancestors, within};

/// An opaque routing identity, declared by an origin or derived by [`Scope`].
pub(crate) type Id = u64;

/// Set on every identity a [`Scope`] qualifies, so any node recognizes a
/// qualified key and refuses to qualify it again.
pub(crate) const QUALIFIED: Id = 1 << 63;

/// The largest identity a node generates: the 53 bits a generated hop id keeps, so
/// a generated identity travels as itself on a lite-06 hop chain (see `node::hop`).
pub(crate) const GENERATED: Id = (1 << 53) - 1;

/// A route metric: the sum of configured link costs.
pub(crate) type Metric = u64;

/// An origin's claim revision, advancing on every change to its own claims.
pub(crate) type Rev = u64;

/// The largest lite-07 varint. No origin ever advertises it as a revision, rotating its
/// identity instead, so every revision leaves room for ANNOUNCE_FLOOR's offset.
pub(crate) const MAX: u64 = u64::MAX;

/// A neighbor key.
pub(crate) trait Key: Copy + Ord + fmt::Debug {}

impl Key for usize {}

impl Key for u64 {}

/// One side's configuration of a link to a neighbor.
#[derive(Clone)]
pub(crate) struct Link {
	/// Strictly positive: a zero-cost link would leave a path as cheap as the one it
	/// extends, so selection would not converge.
	pub cost: Metric,
	/// The application's local priority for this connection: a path ranks by the lowest
	/// on it, higher first, before cost.
	pub priority: i8,
	/// The neighbor is upstream: routes learned here are down-only, and no
	/// down-only route is sent back.
	pub upstream: bool,
	/// Whether this neighbor is sent routes learned from other neighbors.
	pub proxy: bool,
	/// Set when the neighbor publishes under a scoped grant.
	pub scope: Option<Scope>,
}

impl Default for Link {
	fn default() -> Self {
		Self {
			cost: 1,
			priority: 0,
			upstream: false,
			proxy: true,
			scope: None,
		}
	}
}

/// A scoped neighbor's grant: every identity it sends is filed under a
/// qualified key, and its claims count only within the grant's regions.
#[derive(Clone)]
pub(crate) struct Scope {
	/// The prefixes the grant covers.
	pub regions: Vec<Path>,
	/// Derives the qualified key for an identity the neighbor names. It must
	/// set [`QUALIFIED`].
	pub qualify: Arc<dyn Fn(Id) -> Id + Send + Sync>,
}

impl fmt::Debug for Scope {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Scope")
			.field("regions", &self.regions)
			.finish_non_exhaustive()
	}
}

/// Qualifies the identities a scoped neighbor names under `key`: a hash of the key and
/// the identity, collision-resistant at 64 bits, with [`QUALIFIED`] set. Relays proving
/// the same key for a neighbor file its identities alike.
pub(crate) fn qualifier(key: [u8; 32]) -> Arc<dyn Fn(Id) -> Id + Send + Sync> {
	Arc::new(move |id: Id| {
		let mut ctx = rind::digest::Context::new(&rind::digest::SHA256);
		ctx.update(b"moq-lite qualified node id");
		ctx.update(&key);
		ctx.update(&id.to_le_bytes());
		let digest = ctx.finish();
		let mut bytes = [0; 8];
		bytes.copy_from_slice(&digest.as_ref()[..8]);
		u64::from_le_bytes(bytes) | QUALIFIED
	})
}

/// The most nodes a route crosses, its origin included: the longest hop list a
/// message carries.
pub(crate) const MAX_HOPS: usize = 32;

/// How a node's reader sees one announced claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Shown<A> {
	pub item: A,
	/// Whether the node has a usable route to the claim's origin through a
	/// neighbor certifying it.
	pub available: bool,
	/// Whether the claim sits at the root of a region its view was presented at,
	/// where it stands for whichever claim covers that region.
	pub presented: bool,
}

/// A change a node's announcement reader sees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
	Start,
	/// The claim's attributes changed.
	Update,
	/// The origin withdrew the claim, as far as this node knows.
	End,
	/// The node let go of the claim: no usable or recovering route certifies it.
	Offline,
	/// The node stopped watching the claim.
	Unwatched,
	Available,
	Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Event<A> {
	pub id: Id,
	pub claim: Path,
	pub kind: Kind,
	pub item: A,
	/// How many times this node forgot this claim, by collecting or retiring
	/// it, before this event: fixtures excuse a stale replay only after one.
	#[cfg(test)]
	pub forgot: u32,
}

/// A refused input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum Error {
	#[error("zero-cost link")]
	ZeroCost,
	#[error("unknown neighbor")]
	UnknownNeighbor,
	#[error("no declared identity")]
	NoIdentity,
}
