//! Media demuxers for MoQ.
//!
//! This crate provides modules for converting existing media formats into MoQ broadcasts.
//! It supports various container and codec formats, optionally enabled via feature flags.
//!
//! **Feature flags:**
//! - `aac`: Raw AAC frames (not ADTS).
//! - `opus`: Raw Opus frames (not Ogg).
//! - `avc3`: H.264 with inline SPS/PPS.
//! - `fmp4`: fMP4/CMAF container.
//! - `hev1`: H.265 with inline SPS/PPS.
//! - `hls`: HLS playlist.
//!
//! The [Decoder] module provides a generic interface for importing a stream of media.
//! If you know the format in advance, use the specific decoder instead.

#[cfg(feature = "aac")]
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
#[cfg(feature = "opus")]
mod opus;

#[cfg(feature = "aac")]
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
#[cfg(feature = "opus")]
pub use opus::*;
