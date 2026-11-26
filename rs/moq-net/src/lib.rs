//! # moq-net: Media over QUIC networking layer
//!
//! `moq-net` is the networking layer for Media over QUIC: real-time pub/sub with built-in
//! caching, fan-out, and prioritization, on top of QUIC. Sub-second latency at massive scale.
//! At session setup it negotiates one of two wire protocols: the simplified `moq-lite`
//! protocol (the default) or the full IETF `moq-transport` protocol.
//!
//! ## Features
//! The `async` feature, off by default, enables the generated async wrappers for the `poll_*`
//! methods, the announcement `Stream` adapter, `time::run`, and the Lite/IETF sessions
//! (`Client`, `Server`, `Session`, and the session `Driver`). Without it, use the model's
//! `poll_*` methods and [`origin::Driver`] with explicit time. Origin serving stays available
//! without a native runtime; no executor or clock is chosen by the model.
//!
//! ## API
//! The API is built around Producer/Consumer pairs, with the hierarchy:
//! - [origin::Consumer]: A collection of [broadcast::Consumer]s, produced by one or more sessions.
//! - [broadcast::Consumer]: A collection of [track::Consumer]s, produced by a single publisher.
//! - [track::Consumer]: A collection of [group::Info]s, delivered out-of-order until expired.
//! - [group::Info]: A collection of [frame::Info]s, delivered in order until cancelled.
//! - [frame::Info]: Chunks of data with an upfront size.
//!
//! Each level lives in its own module (`broadcast`, `track`, `group`, `frame`, `origin`,
//! `announce`) that owns the short `Producer` / `Consumer` / `Info` names.
//!
//! Traffic counters for the levels above live in [`stats`]: build a [`stats::Registry`]
//! and hand each session a [`stats::Session`] via `Client::with_stats` /
//! `Server::with_stats`. Publishing the counters as MoQ broadcasts lives in the
//! `moq-stats` crate.
//!
//! ## Wire invariants
//! - **Stream identities**: a subscription stays on its starting identity while that identity
//!   has a route, even after a newer route wins the path. When none remains, it ends with an
//!   error (never a clean end, never stitched onto another identity). A clean end means only a
//!   deliberate publisher finish; apps restart on announcements, never request errors.
//! - **Live position**: largest group / live edge comes only from an open upstream
//!   subscription (`SUBSCRIBE_OK`); a copy with none open waits, and a partial group hit still
//!   asks upstream for missing frames.
//! - **Group expiry**: groups never overlap in time (frames within a group may reorder for
//!   B-frames). Expire a group by timestamp (from `GROUP_END` once whole, else lowest timestamp
//!   known in any higher group) and by wall clock once its successor arrived longer ago than
//!   the budget; the latest group never expires.
//! - **Route switch**: for the same identity, split at one boundary (largest group any copy
//!   holds, past its contiguous frames): the new subscription starts there while the old caps
//!   there and drains below within budget, delivering each frame once from whichever arrives
//!   first.
//!
//! ## Compatibility
//! The API exposes the intersection of features supported by both protocols, intentionally
//! keeping it small rather than polluting it with half-baked features.
//!
//! The library is forwards-compatible with the full IETF specification and supports
//! moq-transport drafts 18+ via version negotiation. Everything will work perfectly,
//! so long as your application uses the API as defined above.
//!
//! For example, there's no concept of "sub-group". When connecting to a moq-transport
//! implementation, we use `sub-group=0` for all frames and silently drop any received
//! frames not in `sub-group=0`. If your application genuinely needs multiple sub-groups,
//! tell me *why* and we can figure something out.
//!
//! ## Producers and Consumers
//! Each level of the hierarchy is split into a Producer / Consumer pair:
//! - The **Producer** is the writer: it appends new state (publishes a broadcast,
//!   starts a group, writes frames, closes a track).
//! - The **Consumer** is a reader: each consumer holds its own independent view
//!   of the producer's state, with its own cursor through the stream.
//!
//! Both halves are cheaply clonable so you can hand out multiple handles. Cloning
//! a consumer creates another reader (each at its own cursor); cloning a producer
//! gives another writer that contributes to the same shared state. Closing the
//! last producer signals consumers that no more updates are coming.
//!
//! ## Driving and time
//! Session and origin drivers never spawn tasks or read the clock.
//! Minting an [`Identity`] uses secure randomness. `Client::connect` and
//! `Server::accept` take an initial [`time::Instant`] and return
//! `(Session, Driver)`. Poll the `Driver` with the current instant and a
//! [`kio::Waiter`], then wake on external activity or at the deadline it
//! returns; `time::run` does exactly that on tokio or the browser. The last
//! `Session` drop requests closure; dropping the driver cancels it.
//!
//! [`origin::Producer::new`] also returns a producer and driver. Its driver runs
//! route changes, serving, linger, teardown, and the origin's cache expiration.
//! Standalone caches expose [`cache::Pool::poll`]. Frame read/write methods
//! clear their expiration timestamp for the next cleanup pass. Datagram tracks keep a
//! short buffer and no cache; model read/write APIs take no time argument, though a frame write stamps
//! its arrival from the ambient clock ([`frame::Frame::arrived`]).
//!
//! Both drivers implement [`time::Driver`]. `moq-uring` drives thread-local
//! transports on its own timer heap. Tests advance time by supplying a later
//! instant.

#![warn(missing_docs)]
#![cfg_attr(
	test,
	allow(
		clippy::redundant_closure,
		reason = "`Decode`'s buffer lifetime is a trait parameter, so `T::decode` names one lifetime, and only a closure passes where a test helper takes any"
	)
)]

// Sessions hand out the auth, GOAWAY, and SETUP handles, so without `async` they are
// unobtainable and stay private to the model and wire code that still uses them.
#[cfg(feature = "async")]
pub mod auth;
#[cfg(not(feature = "async"))]
#[allow(dead_code, unused_imports, reason = "async-only")]
mod auth;
#[cfg(feature = "async")]
mod client;
mod coding;
#[cfg(feature = "async")]
mod discovery;
#[cfg(feature = "async")]
mod driver;
mod error;
#[cfg(all(feature = "async", any(test, feature = "fuzz")))]
pub mod fuzz;
#[cfg(feature = "async")]
pub mod goaway;
#[cfg(not(feature = "async"))]
#[allow(dead_code, unused_imports, reason = "async-only")]
mod goaway;
mod identity;
mod ietf;
mod ledger;
mod lite;
mod model;
pub mod path;
#[cfg(feature = "async")]
mod priority;
#[cfg(feature = "async")]
mod recv;
mod route;
#[cfg(feature = "async")]
pub mod setup;
#[cfg(not(feature = "async"))]
#[allow(dead_code, unused_imports, reason = "async-only")]
mod setup;
#[cfg(feature = "async")]
mod tail;
#[cfg(all(test, feature = "async"))]
mod test_interop;
#[cfg(test)]
mod test_tracing;
mod util;
mod version;

#[cfg(feature = "async")]
pub mod reconnect;
#[cfg(feature = "async")]
pub mod server;
#[cfg(feature = "async")]
pub mod session;
pub mod stats;
pub mod time;
pub mod transport;

#[cfg(feature = "async")]
pub use client::*;
pub use coding::{BoundsExceeded, DecodeError, EncodeError, varint};
#[cfg(feature = "async")]
pub use driver::Driver;
pub use error::*;
pub use identity::{Identity, InvalidIdentity};
pub use model::*;
pub use path::{InvalidPattern, InvalidPrefix, Path, Pattern, Patterns, Prefix, Prefixes};
#[cfg(feature = "async")]
pub use reconnect::Reconnect;
#[cfg(feature = "async")]
pub use server::Server;
#[cfg(feature = "async")]
pub use session::Session;
pub use version::*;

/// This crate's product token, `moq-net-rs/<version>`, which SETUP advertises by default.
///
/// A wrapper prepends its own token, space-separated, outermost first:
/// `format!("moq-ffi/{v} {}", moq_net::IMPLEMENTATION)`.
pub const IMPLEMENTATION: &str = concat!("moq-net-rs/", env!("CARGO_PKG_VERSION"));
