//! # moq-lite: Media over QUIC Transport
//!
//! `moq-lite` is designed for real-time live media delivery with sub-second latency at massive scale.
//! This is a simplified subset of the *official* Media over QUIC (MoQ) transport, focusing on the practical features.
//! ## API
//! The API is built around Producer/Consumer pairs, with the hierarchy:
//! - [Origin]: A collection of [Broadcast]s, produced by one or more [Session]s.
//! - [Broadcast]: A collection of [Track]s, produced by a single publisher.
//! - [Track]: A collection of [Group]s, delivered out-of-order until expired.
//! - [Group]: A collection of [Frame]s, delivered in order until cancelled.
//! - [Frame]: Chunks of data with an upfront size.
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
//! Many methods (but not all yet) expose `poll_xxx` variants built on
//! [`conducer`] so you can drive them from custom executors without a tokio
//! runtime. Any plain `async` method should be awaited from inside an active
//! tokio runtime — this requirement is being phased out as more methods grow
//! `poll_xxx` counterparts.
//!
//! ## Compatibility
//! **NOTE**: We purposely implement a subset of the IETF `moq-transport` specification.
//! This is meant to simplify both the API and the implementation, as there's a lot of nonsense possible in the full specification.
//!
//! The library is forwards compatible with the full specification and supports moq-transport drafts 14+.
//! Everything will work perfectly, so long as your application uses the API as defined above.
//!
//! For example, there's no concept of "sub-group" in `moq-lite`.
//! When connecting to a moq-transport implementation, we'll use `sub-group=0` for all frames.
//! If your application depends on multiple sub-groups... you might want to reconsider anyway.

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
mod version;

pub use client::*;
pub use coding::{BoundsExceeded, DecodeError, EncodeError};
pub use error::*;
pub use model::*;
pub use path::*;
pub use server::*;
pub use session::*;
pub use version::*;

// Re-export the bytes crate
pub use bytes;
