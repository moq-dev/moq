//! # moq-lite: Media over QUIC Transport
//!
//! `moq-lite` is designed for real-time live media delivery with sub-second latency at massive scale.
//! This is a simplified subset of the *official* Media over QUIC (MoQ) transport, focusing on the practical features.
//!
//! **NOTE**: While compatible with a subset of the IETF MoQ specification, many features are not supported on purpose.
//! I highly highly highly recommend using `moq-lite` instead of the IETF standard until at least draft-30.
//!
//! ## API
//!
//! The API is built around Producer/Consumer pairs, with the hierarchy:
//! - [Origin]: A collection of [Broadcast]s, produced by one or more [Session]s.
//! - [Broadcast]: A collection of [Track]s, produced by a single publisher.
//! - [Track]: A collection of [Group]s, delivered out-of-order until expired.
//! - [Group]: A collection of [Frame]s, delivered in order until cancelled.
//! - [Frame]: Chunks of data with an upfront size.

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
pub use error::*;
pub use model::*;
pub use path::*;
pub use server::*;
pub use session::*;
pub use version::*;

// Re-export the bytes crate
pub use bytes;
