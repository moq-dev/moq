//! # moq-net: Media over QUIC networking layer
//!
//! `moq-net` is the networking layer for Media over QUIC: real-time pub/sub with built-in
//! caching, fan-out, and prioritization, on top of QUIC. Sub-second latency at massive scale.
//! At session setup it negotiates one of two wire protocols: the simplified `moq-lite`
//! protocol (the default) or the full IETF `moq-transport` protocol.
//!
//! ## API
//! The API is built around Producer/Consumer pairs, with the hierarchy:
//! - [Origin]: A collection of [Broadcast]s, produced by one or more [Session]s.
//! - [Broadcast]: A collection of [Track]s, produced by a single publisher.
//! - [Track]: A collection of [Group]s, delivered out-of-order until expired.
//! - [Group]: A collection of [Frame]s, delivered in order until cancelled.
//! - [Frame]: Chunks of data with an upfront size.
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
//! This library is async-first, using [tokio] for async I/O and task management.
//! Any plain `async` method should be awaited from inside an active tokio runtime.
//! Otherwise you risk a panic.
//!
//! This requirement is being phased out as more methods grow `poll_xxx` counterparts
//! built on [`kio`], so you can drive them from custom executors without a tokio
//! runtime. You can also call them synchronously, since [`kio`] is built on the
//! standard [`std::task::Waker`] API and any [`std::task::Waker`] is a valid driver.

mod client;
pub mod coding;
mod error;
mod ietf;
mod lite;
mod model;
mod path;
mod server;
mod session;
mod setup;
mod stats;
mod version;

pub use client::*;
pub use coding::{BoundsExceeded, DecodeError, EncodeError, VarInt};
pub use error::*;
pub use model::*;
pub use path::*;
pub use server::*;
pub use session::*;
pub use stats::*;
pub use version::*;

// Re-export the bytes crate
pub use bytes;

// Re-export the kio crate, since it appears in the public API (e.g. poll_* waiters).
pub use kio;
