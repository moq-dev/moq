//! Media muxers and demuxers for MoQ.
//!
//! This crate provides modules for converting existing media formats into MoQ broadcasts.
//! It supports various container formats like fMP4/CMAF and HLS.

mod aac;
#[cfg(any(feature = "h264", feature = "h265"))]
mod annexb;
#[cfg(feature = "h264")]
mod avc3;
mod decoder;
#[cfg(feature = "mp4")]
mod fmp4;
#[cfg(feature = "h265")]
mod hev1;
#[cfg(feature = "hls")]
mod hls;
mod opus;

pub use aac::*;
#[cfg(feature = "h264")]
pub use avc3::*;
pub use decoder::*;
#[cfg(feature = "mp4")]
pub use fmp4::*;
#[cfg(feature = "h265")]
pub use hev1::*;
#[cfg(feature = "hls")]
pub use hls::*;
pub use opus::*;
