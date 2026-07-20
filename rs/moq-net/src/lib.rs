//! # moq-net: Media over QUIC networking layer
//!
//! `moq-net` is the networking layer for Media over QUIC: real-time pub/sub with built-in
//! caching, fan-out, and prioritization, on top of QUIC. Sub-second latency at massive scale.
//! At session setup it negotiates one of two wire protocols: the simplified `moq-lite`
//! protocol (the default) or the full IETF `moq-transport` protocol.
//!
//! ## API
//! The API is built around Producer/Consumer pairs, with the hierarchy:
//! - [origin::Consumer]: A collection of [broadcast::Consumer]s, produced by one or more [Session]s.
//! - [broadcast::Consumer]: A collection of [track::Consumer]s, produced by a single publisher.
//! - [track::Consumer]: A collection of [group::Info]s, delivered out-of-order until expired.
//! - [group::Info]: A collection of [frame::Info]s, delivered in order until cancelled.
//! - [frame::Info]: Chunks of data with an upfront size.
//!
//! Each level lives in its own module (`broadcast`, `track`, `group`, `frame`, `origin`,
//! `announce`) that owns the short `Producer` / `Consumer` / `Info` names.
//!
//! Traffic counters for the levels above live in [`stats`]: build a [`stats::Registry`]
//! and hand each session a [`stats::Handle`] via [`Client::with_stats`] /
//! [`Server::with_stats`]. Publishing the counters as MoQ broadcasts lives in the
//! `moq-stats` crate.
//!
//! ## Compatibility
//! The API exposes the intersection of features supported by both protocols, intentionally
//! keeping it small rather than polluting it with half-baked features.
//!
//! The library is forwards-compatible with the full IETF specification and supports
//! moq-transport drafts 14+ via version negotiation. Everything will work perfectly,
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
//! ## Async
//! This library is async-first. [`Client::connect`] and [`Server::accept`] return a
//! `(Session, Driver)` pair: the [`Session`] is the handle, and the [`Driver`] is
//! the future that runs all of its protocol work. Nothing is spawned behind your
//! back: spawn the driver on your executor, await it in place, or step
//! [`Driver::poll`] with a [`kio::Waiter`] from your own `poll_*` function. The
//! driver holds no session handle, so the transport still closes when the last
//! [`Session`] clone drops (or on [`Session::abort`]), which in turn finishes the
//! driver.
//!
//! The crate has no direct tokio dependency: every future is built on [`kio`]
//! (plain [`std::task::Waker`] plumbing) and `futures`, so any executor can poll
//! them, and the `poll_xxx` counterparts can be stepped synchronously with a
//! [`kio::Waiter`].
//!
//! The one remaining runtime tie is time. Timers go through `web_async::time`,
//! which is backed by tokio's time driver on native (and `wasmtimer` in the
//! browser), and those timers panic when polled outside a tokio runtime. So on
//! native you still need a tokio runtime to poll a [`Driver`] (bandwidth sampling,
//! the control stream timeout, and subscription linger all sleep); purely
//! model-layer methods (tracks, groups, frames, origins) never touch a timer and
//! run on any executor.

#![warn(missing_docs)]

mod client;
mod coding;
mod error;
mod ietf;
mod lite;
mod model;
mod path;
mod server;
mod session;
mod setup;
mod util;
mod version;

pub mod stats;

pub use client::*;
pub use coding::{BoundsExceeded, DecodeError, EncodeError, VarInt};
pub use error::*;
/// The session direction a client advertises in its SETUP (moq-lite-05+).
pub use lite::Role;
pub use model::*;
pub use path::*;
pub use server::*;
pub use session::*;
pub use version::*;

// Re-export the bytes crate
pub use bytes;

// Re-export the transport trait, since it bounds the Client/Server entry points.
pub use web_transport_trait;

// Re-export the kio crate, since it appears in the public API (e.g. poll_* waiters).
pub use kio;
