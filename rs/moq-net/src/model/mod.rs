pub mod bandwidth;
pub mod broadcast;
pub mod cache;
pub mod frame;
pub mod group;
pub mod track;

// The origin + announce subsystem shares one implementation (a route table).
// It stays in a single private module and is surfaced as two curated public
// modules so neither leaks the other's plumbing.
#[path = "origin.rs"]
mod origin_impl;
// Reduces the routes covering one path to the one serving it.
mod follow;
// The failover state machine origin fronts run; pure, so its transitions are
// tested without a runtime.
mod front;

mod aborts;
mod bytes;
mod clock;
pub(crate) mod datagram;
mod delivered;
mod expiry;
mod skipped;
// The DEFLATE codec behind compressed tracks.
mod flate;
pub(crate) mod object;
mod request_queue;
mod resume;
mod subscription;
mod time;
mod timed;
mod weak_cache;

#[cfg(test)]
mod test_tracing;

use request_queue::RequestQueue;
use weak_cache::{WeakCache, WeakEntry};

pub use bytes::*;
// Datagram stays flat at the crate root (a small track-adjacent wire type),
// not under a role module.
pub use datagram::*;
pub use time::*;
pub use timed::Timed;

/// Publishing broadcasts, announcing routes, and consuming both through an origin.
pub mod origin {
	pub use super::follow::AnnouncedBroadcast;
	#[cfg(feature = "async")]
	// private: capability gap
	pub(crate) use super::origin_impl::Lookup;
	pub use super::origin_impl::{
		Config, Consumer, Cost, Driver, HopLimit, Link, Preference, Prepared, Producer, Publication, Request,
		Requesting, Requests, Route, Source,
	};
}

/// Subscribing to route (un)announcements from an origin.
pub mod announce {
	pub use super::follow::Follow;
	pub use super::origin_impl::{Announce, AnnounceConsumer as Consumer, AnnounceEvent as Event};
}

// Hop identity and the `Consume` conversion trait aren't part of a role
// module; keep them flat at the crate root.
pub use origin_impl::{Consume, Hop, Hops, InvalidHop};

// The announce-interest prefixes a scope needs on a prefix-shaped wire.
#[cfg(feature = "async")]
pub(crate) use origin_impl::interest_prefixes;

// Held by a session while its peer's initial announcements arrive.
#[cfg(feature = "async")]
pub(crate) use origin_impl::Snapshot;

// What a client session asks its peer to announce, as the origin's readers change.
#[cfg(feature = "async")]
pub(crate) use origin_impl::{InterestChange, Interests};

// The advertise-only route guard, for tests shaping the route table.
#[cfg(all(test, feature = "async"))]
pub(crate) use origin_impl::AnnounceProducer;

#[cfg(all(test, feature = "async"))]
pub(crate) use origin_impl::{ProduceTest, TestSelection};
