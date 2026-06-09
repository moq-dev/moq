//! Native video capture, encoding, and publishing for Media over QUIC.
//!
//! Counterpart to [`moq-audio`](https://crates.io/crates/moq-audio) for
//! video tracks. Sits on top of [`moq_mux`] and [`hang`] and adds the
//! native pieces a desktop/CLI publisher needs:
//!
//! - [`Camera`] captures a webcam via libavdevice (avfoundation / v4l2 /
//!   dshow) and yields decoded frames.
//! - [`Encoder`] turns those frames into Annex-B H.264, preferring a
//!   platform hardware encoder (`h264_videotoolbox` / `h264_nvenc` /
//!   `h264_vaapi`) and falling back to software.
//! - [`VideoProducer`] wires the encoder into
//!   [`moq_mux::codec::h264::Import`], which handles catalog registration
//!   and frame publishing.
//! - [`publish_camera`] is the one-call capture-encode-publish loop the CLI
//!   uses; run it on a blocking thread.
//!
//! Decode/consume (the mirror of `moq-audio`'s `AudioConsumer`) is not
//! implemented yet; native subscribers can keep using `moq_mux` directly.

mod capture;
mod encoder;
mod error;
mod producer;

/// Re-export so callers can name the frame/pixel types in our signatures
/// without taking their own ffmpeg dependency.
pub use ffmpeg_next as ffmpeg;

pub use capture::{Camera, CameraConfig};
pub use encoder::{Encoder, EncoderConfig, EncoderKind};
pub use error::Error;
pub use producer::{CameraPublishConfig, VideoProducer, publish_camera};
