pub mod bandwidth;
pub mod broadcast;
pub mod cache;
pub mod datagram;
pub mod frame;
pub mod group;
pub mod socket;
pub mod track;

// The origin + announce subsystem shares one implementation (a route table).
// It stays in a single private module and is surfaced as two curated public
// modules so neither leaks the other's plumbing.
#[path = "origin.rs"]
mod origin_impl;
// The app-level announcement listing over a route stream.
mod announced;
// Reduces the routes covering one path to the one serving it.
mod follow;
// The failover state machine origin fronts run; pure, so its transitions are
// tested without a runtime.
mod front;

mod aborts;
mod bytes;
mod clock;
mod delivered;
mod expiry;
// The early exit a read nothing woke takes.
mod idle;
pub(crate) mod record;
mod skipped;
// How far a group's writer reads ahead of its fastest reader.
pub(crate) mod pace;
// The DEFLATE codec behind compressed tracks.
mod flate;
pub(crate) mod object;
mod request_queue;
mod resume;
// Resume checked against a model of one logical track.
#[cfg(all(test, feature = "async"))]
mod resume_model;
mod subscription;
mod time;
mod timed;
mod weak_cache;

use request_queue::RequestQueue;
use weak_cache::{WeakCache, WeakEntry};

pub use bytes::*;
pub use time::*;
pub use timed::Timed;

/// Routing broadcasts into an origin, and consuming them through it.
pub mod origin {
	pub use super::follow::Following;
	#[cfg(all(feature = "async", any(test, feature = "fuzz")))]
	pub(crate) use super::origin_impl::Footprint;
	pub(crate) use super::origin_impl::HiddenWithheld;
	#[cfg(feature = "async")]
	pub(crate) use super::origin_impl::HiddenWithholding;
	#[cfg(feature = "async")]
	// private: capability gap
	pub(crate) use super::origin_impl::Lookup;
	#[cfg(feature = "async")]
	pub(crate) use super::origin_impl::Mounted;
	#[cfg(feature = "async")]
	pub(crate) use super::origin_impl::{Lane, NodeFilter};
	pub use super::origin_impl::{
		Claim, Config, Consumer, Cost, Driver, Link, Preference, Producer, Request, Requesting, Route, RouteInfo,
		RouteOptions, Source,
	};
	pub use crate::route::RouteKey;

	// Relay-level: every route change, for forwarding and stats. Apps read
	// `announce` and `follow` instead.
	pub use super::origin_impl::{RouteEvent, RouteKind, Routes};
}

/// Listing the paths announced on an origin.
pub mod announce {
	pub use super::announced::{AnnounceConsumer as Consumer, AnnounceEvent as Event};
	pub use super::origin_impl::Announce;
}

// The `Consume` conversion trait isn't part of a role module; keep it flat at the
// crate root.
pub use origin_impl::Consume;

// Hop chains are transit facts the origin and sessions fill in; applications see
// plain ids.
pub(crate) use origin_impl::{Hop, Hops, InvalidHop};

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
