//! Subscribe to a moq broadcast and decode media frames.
//!
//! [`Consumer<F>`](Consumer) wraps a single [`moq_lite::TrackConsumer`] and yields decoded
//! [`Frame`](crate::container::Frame)s in latency-bounded presentation order, generic over the
//! container format `F: Container` (typically [`Hang`](crate::container::Hang) when the format
//! is selected at runtime from a hang catalog).
//!
//! [`Muxed`] runs a [`Consumer`] per track in a broadcast, driven by a
//! [`hang::CatalogConsumer`], and merges them into a single timestamp-ordered stream.
//!
//! [`Fmp4`] re-encodes decoded frames as ISO-BMFF / CMAF moof+mdat fragments and builds the
//! merged init segment for multi-track output.

mod consumer;
mod fmp4;
mod muxed;

pub use consumer::*;
pub use fmp4::*;
pub use muxed::*;
