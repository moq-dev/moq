//! # hang: WebCodecs compatible media encoding for MoQ
//!
//! Media-specific library built on [`moq_lite`] for streaming audio and video with WebCodecs.
//!
//! ## Overview
//!
//! `hang` adds media support to the generic [`moq_lite`] transport:
//!
//! - **Catalog**: JSON track containing codec info and track metadata, updated live as tracks change.
//! - **Container**: Simple frame format consisting of timestamp (microseconds) + codec bitstream payload.
//! - **Import**: Import fMP4/CMAF files into hang broadcasts via the [`import`] module.
//! - **Decode** (feature: `decode`): Decode compressed frames to raw audio/video data using FFmpeg.
//! - **Render** (feature: `render`): Render decoded video frames using wgpu GPU acceleration.
//!
//! ## Frame Container
//!
//! Each frame consists of:
//! - Timestamp (u64): presentation time in microseconds
//! - Payload: raw encoded codec data (H.264, Opus, etc.)
//!
//! This simple format works directly with WebCodecs APIs in browsers.
//!
//! ## Feature Flags
//!
//! - `decode`: Enable frame decoding using FFmpeg (requires system FFmpeg installation)
//! - `render`: Enable GPU-accelerated video rendering using wgpu
//! - `playback`: Enable both `decode` and `render` for complete native playback support
//!
mod error;

pub mod catalog;
pub mod import;
pub mod model;

#[cfg(any(feature = "decode", feature = "encode"))]
pub mod av;

#[cfg(feature = "decode")]
pub mod decode;

#[cfg(feature = "encode")]
pub mod encode;

#[cfg(feature = "render")]
pub mod render;

// export the moq-lite version in use
pub use moq_lite;

pub use error::*;
pub use model::*;
