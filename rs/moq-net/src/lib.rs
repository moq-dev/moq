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
//! This library is async-first, but it never spawns onto a global executor and
//! never reaches for ambient state. [`Client::connect`] and [`Server::accept`]
//! take a [`Runtime`], which supplies the two things a session cannot do alone:
//! run its protocol [`runtime::Machine`] to completion and arm its timers. The
//! machine holds no session handle, so the transport still closes when the last
//! [`Session`] clone drops (or on [`Session::abort`]), which in turn finishes
//! the machine. Each transport ships its runtime (`moq-tokio`, `moq-wasm`, a
//! thread-per-core io_uring runtime), and `runtime::Test` is the
//! deterministic one for tests; see the [`runtime`] module.
//!
//! Origins follow the caller-driven pattern: [`origin::Producer::new`] returns a
//! `(Producer, Driver)` pair, [`origin::Driver::run`] takes the [`Timers`] the
//! origin's deadlines arm against, and the returned [`origin::Run`] future runs
//! the lifecycle work (route changes, track serving, teardown). Native tokio
//! applications can use `moq_tokio::origin::spawn` instead of running the
//! driver by hand.
//!
//! The crate has no tokio dependency: every future is built on [`kio`]
//! (plain [`std::task::Waker`] plumbing) and `futures`, so any executor can poll
//! them, and the `poll_xxx` counterparts can be stepped synchronously with a
//! [`kio::Waiter`]. Purely model-layer methods (tracks, groups, frames,
//! origins) never arm a timer and need no [`Runtime`] at all; they read the
//! crate's ambient clock for passive stamps (arrival times, cache ticks).

#![warn(missing_docs)]
// The browser transport is `!Send`, so on wasm the shared state behind these `Arc`s is
// too and clippy suggests `Rc`. The same code is genuinely cross-thread on native, so
// `Arc` stays and the lint is unactionable here.
#![cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]

mod client;
mod coding;
mod error;
pub mod goaway;
// Not part of the public API: compiled only for the crate's own tests and for the
// `fuzz/` harness.
#[cfg(any(test, feature = "fuzz"))]
#[doc(hidden)]
pub mod fuzz;
mod ietf;
mod lite;
mod model;
mod path;
mod server;
mod session;
mod setup;
mod util;
mod version;

pub mod runtime;
pub mod stats;
pub mod transport;

pub use client::*;
pub use coding::{BoundsExceeded, DecodeError, EncodeError, VarInt};
pub use error::*;
/// The session direction a client advertises in its SETUP (moq-lite-05+).
pub use lite::Role;
pub use model::*;
pub use path::*;
pub use runtime::{Runtime, Timers};
pub use server::*;
pub use session::*;
pub use version::*;

// Re-export the bytes crate
pub use bytes;

// Re-export the transport trait, since it bounds the Client/Server entry points.
pub use web_transport_trait;

// Re-export the kio crate, since it appears in the public API (e.g. poll_* waiters).
pub use kio;
