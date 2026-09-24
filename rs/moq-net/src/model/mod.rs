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
// The failover state machine origin fronts run; pure, so its transitions are
// tested without a runtime.
mod front;

mod bytes;
pub(crate) mod clock;
mod datagram;
mod requests;
pub(crate) mod resume;
mod subscription;
mod time;
mod weak_cache;

#[cfg(test)]
pub(crate) mod test_tracing;

pub(crate) use requests::Requests;
pub(crate) use weak_cache::{WeakCache, WeakEntry};

pub use bytes::*;
pub(crate) use subscription::Cap;
// Datagram stays flat at the crate root (a small track-adjacent wire type),
// not under a role module.
pub use datagram::*;
pub use time::*;

/// Publishing broadcasts, announcing routes, and consuming both through an origin.
pub mod origin {
	pub use super::origin_impl::{Config, Consumer, Cost, Driver, Dynamic, Producer, Request, Requesting, Route};
}

/// Subscribing to route (un)announcements from an origin.
pub mod announce {
	pub use super::origin_impl::{Announce, AnnounceConsumer as Consumer, AnnounceEvent as Event};
}

// Hop identity and the `Consume` conversion trait aren't part of a role
// module; keep them flat at the crate root.
pub use origin_impl::{Consume, Hop, Hops, InvalidHop};

// The announce-interest prefixes a scope needs on a prefix-shaped wire.
pub(crate) use origin_impl::interest_prefixes;

// Held by a session until the peer's initial announce set has landed.
pub(crate) use origin_impl::{Quiet, Replaying};

// The advertise-only route guard, for tests shaping the route table.
#[cfg(test)]
pub(crate) use origin_impl::AnnounceProducer;

#[cfg(test)]
pub(crate) use origin_impl::ProduceTest;
