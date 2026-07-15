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
pub(crate) mod resume;
mod subscription;
mod time;
mod weak_cache;

pub(crate) use weak_cache::{WeakCache, WeakEntry};

pub use bytes::*;
// Datagram stays flat at the crate root (a small track-adjacent wire type),
// not under a role module.
pub use datagram::*;
pub use time::*;

/// Publishing and consuming the set of broadcasts announced at an origin.
pub mod origin {
	pub use super::origin_impl::{Broadcast, Consumer, Dynamic, Info, Producer, Publish, Request, Requesting};

	// The route-serving surface (how sessions feed a broadcast reached over the
	// network) is crate-internal until an external consumer shapes it; apps see
	// only the spliced `broadcast::Consumer` and the dynamic `broadcast::Route`.
	pub(crate) use super::origin_impl::{Assignment, Assignments, Route};
}

/// Subscribing to broadcast (un)announcements from an origin.
pub mod announce {
	pub use super::origin_impl::{
		AnnounceConsumer as Consumer, AnnounceProducer as Producer, Announced as Event, OriginAnnounce as Update,
	};
}

// Origin identity and the `Consume` conversion trait aren't part of a role
// module; keep them flat at the crate root.
pub use origin_impl::{Consume, InvalidOrigin, Origin, OriginList, TooManyOrigins};
