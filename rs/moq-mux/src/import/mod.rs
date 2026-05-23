//! Pull external media into a moq broadcast.
//!
//! Submodules expose container producers that take wrapped media streams and
//! publish them as hang-protocol tracks alongside a catalog. Codec-specific
//! importers (raw AAC, raw Opus, raw H.264, …) live under [`crate::codec`]
//! instead.
//!
//! ## Choosing an entry point
//!
//! - If you know the codec/container in advance, use the dedicated importer
//!   (e.g. [`crate::codec::aac::import::Import`], [`crate::codec::h264::import::Import`],
//!   [`Fmp4`], [`Hls`]).
//! - If you only know the wrapping container, use [`Framed`] (frame
//!   boundaries known — e.g. fMP4) or [`Stream`] (raw byte stream, no
//!   framing — e.g. piped Annex B H.264).
//!
//! Codec producers publish through [`hang::Producer`](crate::catalog::hang::Producer),
//! which manages the hang and MSF catalog tracks; per-track encoding goes
//! through [`Producer<C>`](crate::container::Producer), which dispatches to a
//! [`Container`](crate::container::Container) implementation.

mod fmp4;
mod framed;
mod hls;
mod mkv;
mod stream;

pub use fmp4::*;
pub use framed::*;
pub use hls::*;
pub use mkv::*;
pub use stream::*;

#[cfg(test)]
mod test;
