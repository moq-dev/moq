//! Subscribe to a moq broadcast and decode media frames.
//!
//! [`Muxed`] runs a [`Consumer`](crate::container::Consumer) per track in a broadcast, driven by a
//! [`crate::catalog::Consumer`], and merges them into a single timestamp-ordered stream.
//!
//! [`Fmp4`] re-encodes decoded frames as ISO-BMFF / CMAF moof+mdat fragments and builds the
//! merged init segment for multi-track output.

mod fmp4;
mod muxed;

pub use fmp4::*;
pub use muxed::*;
