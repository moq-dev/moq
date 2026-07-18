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
mod datagram;
mod requests;
pub(crate) mod resume;
mod subscription;
mod time;
mod weak_cache;

pub(crate) use requests::Requests;
pub(crate) use weak_cache::{WeakCache, WeakEntry};

pub use bytes::*;
// Datagram stays flat at the crate root (a small track-adjacent wire type),
// not under a role module.
pub use datagram::*;
pub use time::*;

/// Publishing and consuming the set of broadcasts routed through an origin.
pub mod origin {
	pub use super::origin_impl::{Consumer, Dynamic, Info, Producer, Request, Requesting};
}

/// Subscribing to broadcast (un)announcements from an origin.
pub mod announce {
	pub use super::origin_impl::{
		AnnounceConsumer as Consumer, AnnounceProducer as Producer, OriginAnnounce as Update,
	};
}

// Origin identity and the `Consume` conversion trait aren't part of a role
// module; keep them flat at the crate root.
pub use origin_impl::{Consume, InvalidOrigin, Origin, OriginList, TooManyOrigins};
