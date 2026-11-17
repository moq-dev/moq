//! The per-origin route layer, as one node's state machine.
//!
//! A forwarding node routes to other forwarding nodes by path vector, and each one
//! versions its own claims with a revision, so no stale copy revives a withdrawal a node
//! already knows ([`relay::Router`]). [`node::Node`] drives it for every lite-07 session of
//! one origin. See `rs/moq-net/verify/route-algebra/README.md` for the proof.
//!
//! The router does no I/O: the caller feeds it what each neighbour sent and drains its
//! messages and reader [`Event`]s. Neighbour keys `N` and claim attributes `A` are opaque
//! to it.

#![cfg_attr(
	not(feature = "async"),
	allow(dead_code, reason = "only the async session drives it")
)]

pub(crate) mod node;
mod prefix;
mod reader;
pub(crate) mod relay;

#[cfg(test)]
mod tests;

use std::fmt;

use crate::Path;

pub(crate) use node::Node;
#[cfg(feature = "async")]
pub(crate) use prefix::{ancestors, heads, within};
pub(crate) use relay::NodeId;

/// A claim's attributes as the router carries them down a stream.
pub(crate) trait Item: Clone + Eq + fmt::Debug {
	/// The claim as a stream answering a prefix `by` segments below the claim's own shows
	/// it. A claim covering the prefix is presented at the prefix itself, which hides how
	/// much broader it is, so the claim keeps that distance: a claim exactly at a prefix
	/// outranks every broader one there. `lowered(0)` is the claim itself.
	fn lowered(&self, by: u64) -> Self;

	/// How many segments the claim's own prefix lacks of the path it is shown at, as
	/// [`Self::lowered`] counts them: 0 for a claim exactly there, which outranks every
	/// broader one whatever their epochs or routes.
	fn above(&self) -> u64;

	/// Which run of its Instance the publisher is on: among claims of one Instance at one
	/// path and distance, a higher epoch supersedes a lower one whatever either's route
	/// costs, or whether a route reaches it, as `model::origin` ranks them. 0 for
	/// attributes that carry none.
	fn epoch(&self) -> u64;
}

/// Opaque attributes that cover nothing broader than their own path.
#[cfg(any(test, feature = "fuzz"))]
macro_rules! opaque {
	($($ty:ty),*) => {$(
		impl Item for $ty {
			fn lowered(&self, _: u64) -> Self {
				self.clone()
			}

			fn above(&self) -> u64 {
				0
			}

			fn epoch(&self) -> u64 {
				0
			}
		}
	)*};
}

#[cfg(any(test, feature = "fuzz"))]
opaque!((), u8, u32, u64);

/// A relay's routing identity.
pub(crate) type Id = u64;

/// A route metric: the sum of configured link costs.
pub(crate) type Metric = u64;

/// A forwarding node's claim revision, advancing on every change to its own claims.
pub(crate) type Rev = u64;

/// A neighbor key.
pub(crate) trait Key: Copy + Ord + fmt::Debug {}

impl Key for usize {}

impl Key for u64 {}

/// The most nodes a route crosses, its origin included: the longest hop list a
/// message carries.
pub(crate) const MAX_HOPS: usize = 32;

/// Feed `value`'s `Debug` form to `hasher`: how the exhaustive search fingerprints
/// router state without a `Hash` impl on every type it holds.
#[cfg(test)]
pub(crate) fn digest(hasher: &mut impl std::hash::Hasher, value: &impl fmt::Debug) {
	struct Feed<'a, H>(&'a mut H);
	impl<H: std::hash::Hasher> fmt::Write for Feed<'_, H> {
		fn write_str(&mut self, s: &str) -> fmt::Result {
			self.0.write(s.as_bytes());
			Ok(())
		}
	}
	fmt::write(&mut Feed(&mut *hasher), format_args!("{value:?}")).expect("Debug never fails");
	hasher.write_u8(0xff);
}

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

/// A change the reader sees: `id` is the Instance the claim serves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Event<A, K = Id> {
	pub id: K,
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
	#[error("one interest prefix inside another")]
	Nested,
	#[error("no such interest prefix")]
	UnknownPrefix,
	#[error("a neighbor connected twice")]
	Duplicate,
	#[error("a routing neighbor asked for part of the tree")]
	Partial,
	#[error("a claim outside what was asked, or anything before the answer began")]
	Unasked,
	#[error("claims or news about no route the neighbor offered")]
	Unrouted,
	#[error("a route or chain the rule never builds")]
	Malformed,
	#[error("a message the neighbor's role never sends")]
	Role,
	#[error(
		"a routing neighbor started before a session of its relay known closed: its clock went back, or two relays share an id"
	)]
	Regressed,
}
