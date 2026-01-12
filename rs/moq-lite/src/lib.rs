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
//! For example, a media encoder could create:
//! - [Origin::produce], using the [OriginConsumer] with [Session::connect] to announce our broadcasts over the network.
//! - [OriginProducer::create_broadcast] to create one or more [BroadcastProducer]s.
//! - [BroadcastProducer::create_track] for each track in the broadcast.
//! - [TrackProducer::append_group] for each Group of Pictures (each I-frame) or audio frame.
//! - [GroupProducer::write_frame] for each frame in the group.
//!
//! It's similar but in reverse for consuming media:
//! - [Origin::produce], using the [OriginProducer] with [Session::connect] to consume broadcasts over the network.
//! - [OriginConsumer::announced] to discover [BroadcastConsumer]s over the network.
//! - [BroadcastConsumer::subscribe_track] to subscribe to a [TrackConsumer] for a specific track.
//! - [TrackConsumer::next_group] to block until the next group is available.
//! - [GroupConsumer::read_frame] to block until the next frame is available.
//!
//! There's a boatload of helper methods so your experience will vary.
//!
//! For example, there's actually a [FrameProducer] and [FrameConsumer] for performing chunked writes and reads.
//! This is useful if you're streaming data over the network (ex. a relay) and don't want to allocate a whole frame at once.
//! Likewise you can [TrackProducer::create_group] instead of [TrackProducer::append_group] if you want to produce out-of-order.

mod error;
mod model;
mod path;
mod session;
mod setup;

pub mod coding;
pub mod ietf;
pub mod lite;

pub use error::*;
pub use model::*;
pub use path::*;
pub use session::*;
