//! Media demuxers for MoQ.
//!
//! This crate provides modules for converting existing media formats into MoQ broadcasts.
//! It supports various container and codec formats.
//!
//! The [Framed] and [Stream] types provide generic interfaces for importing media.
//! [Framed] is for formats with known frame boundaries, [Stream] for unknown boundaries.
//! If you know the format in advance, use the specific codec module instead.

mod aac;
mod annexb;
mod av01;
mod avc1;
mod avc3;
mod decoder;
mod fmp4;
mod hev1;
mod hls;
mod opus;

pub use aac::*;
pub use av01::*;
pub use avc1::*;
pub use avc3::*;
pub use decoder::*;
pub use fmp4::*;
pub use hev1::*;
pub use hls::*;
pub use opus::*;

#[cfg(test)]
mod test;
