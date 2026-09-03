pub mod bandwidth;
pub mod broadcast;
pub mod cache;
pub mod frame;
pub mod group;
pub mod track;

// The origin + announce subsystem shares one implementation (a broadcast tree).
// It stays in a single private module and is surfaced as two curated public
// modules so neither leaks the other's plumbing.
#[path = "origin.rs"]
mod origin_impl;

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
// Datagram stays flat at the crate root (a small track-adjacent wire type),
// not under a role module.
pub use datagram::*;
pub use time::*;

/// Publishing broadcasts, announcing routes, and consuming both through an origin.
pub mod origin {
	pub use super::origin_impl::{
		Consumer, Cost, DRAIN_COST, Driver, Dynamic, Info, MAX_COST, Prefix, Producer, Request, Requesting, Route, Run,
	};
}

/// Subscribing to route (un)announcements from an origin.
pub mod announce {
	pub use super::origin_impl::{
		AnnounceConsumer as Consumer, AnnounceProducer as Producer, AnnounceUpdate as Update,
	};
}

// Hop identity and the `Consume` conversion trait aren't part of a role
// module; keep them flat at the crate root.
pub use origin_impl::{Consume, Hop, Hops, InvalidHop};

// The per-route request queue handed to sessions by `origin::Producer::announce_served`.
pub(crate) use origin_impl::RouteServer;

#[cfg(test)]
pub(crate) use origin_impl::ProduceTest;
