//! # moq-lite: Media over QUIC Transport
//!
//! `moq-lite` is designed for real-time live media delivery with sub-second latency at massive scale.
//! This is a simplified subset of the *official* Media over QUIC (MoQ) transport, focusing on the practical features.
//!
//! **NOTE**: While compatible with a subset of the IETF MoQ specification (see [ietf::Version]), many features are not supported on purpose.
//! Additionally, the IETF standard is immature and up to interpretation, so many implementations are not compatible anyway.
//! I highly highly highly recommend using `moq-lite` instead of the IETF standard until at least draft-30.
//!
//! ## API
//!
//! The API is built around Producer/Consumer pairs, with the hierarchy:
//! - [Origin]: A collection of [Broadcast]s, produced by one or more [Session]s.
//! - [Broadcast]: A collection of [Track]s, produced by a single publisher.
//! - [Track]: A collection of [Group]s, delivered out-of-order until expired.
//! - [Group]: A collection of [Frame]s, delivered in order until cancelled.
//!
//! To publish media, create:
//! - [Origin::produce] to get an [OriginProducer] and [OriginConsumer] pair.
//! - [OriginProducer::create_broadcast] to create a [BroadcastProducer].
//! - [BroadcastProducer::create_track] to create a [TrackProducer] for each track.
//! - [TrackProducer::append_group] for each Group of Pictures (GOP) or audio frames.
//! - [GroupProducer::write_frame] to write each encoded frame in the group.
//!
//! To consume media, create:
//! - [Origin::produce] to get an [OriginProducer] and [OriginConsumer] pair.
//! - [OriginConsumer::announced] to discover new [BroadcastConsumer]s as they're announced.
//! - [BroadcastConsumer::subscribe_track] to get a [TrackConsumer] for a specific track.
//! - [TrackConsumer::next_group] to receive the next available group.
//! - [GroupConsumer::read_frame] to read each frame in the group.
//!
//! ## Advanced Usage
//!
//! - Use [FrameProducer] and [FrameConsumer] for chunked frame writes/reads without allocating entire frames (useful for relaying).
//! - Use [TrackProducer::create_group] instead of [TrackProducer::append_group] to produce groups out-of-order.

mod client;
mod error;
mod model;
mod path;
mod server;
mod session;
mod setup;
mod version;

pub mod coding;
pub mod ietf;
pub mod lite;

pub use client::*;
pub use error::*;
pub use model::*;
pub use path::*;
pub use server::*;
pub use session::*;
pub use version::*;
