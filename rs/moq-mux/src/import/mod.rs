//! Pull external media into a moq broadcast.
//!
//! Submodules expose codec-specific producers that take raw bitstreams (or container-wrapped
//! streams) and publish them as hang-protocol tracks alongside a catalog.
//!
//! ## Choosing an entry point
//!
//! - If you know the codec/container in advance, use the dedicated producer
//!   ([`Aac`], [`Avc1`], [`Avc3`], [`Av01`], [`Hev1`], [`Opus`], [`Fmp4`], [`Hls`]).
//! - If you only know the wrapping container, use [`Framed`] (frame boundaries known —
//!   e.g. fMP4) or [`Stream`] (raw byte stream, no framing — e.g. piped Annex B H.264).
//!
//! Codec producers publish through [`catalog::Producer`](crate::catalog::Producer), which
//! manages the hang and MSF catalog tracks; per-track encoding goes through
//! [`Producer<C>`](crate::container::Producer), which dispatches to a
//! [`Container`](crate::container::Container) implementation.

mod aac;
pub(crate) mod annexb;
mod av01;
mod avc1;
mod avc3;
mod fmp4;
mod framed;
mod hev1;
mod hls;
mod jitter;
mod mkv;
mod opus;
mod stream;

pub use aac::*;
pub use av01::*;
pub use avc1::*;
pub use avc3::*;
pub use fmp4::*;
pub use framed::*;
pub use hev1::*;
pub use hls::*;
pub use mkv::*;
pub use opus::*;
pub use stream::*;

#[cfg(test)]
mod test;
