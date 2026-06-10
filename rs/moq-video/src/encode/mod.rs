//! Encode captured video and publish it as a moq H.264 track.
//!
//! The decode/consume counterpart (mirror of `moq-audio`'s consumer) will
//! land in a sibling `decode` module.

mod encoder;
mod producer;

pub use encoder::{Encoder, EncoderConfig, EncoderKind};
pub use producer::{CameraConfig, VideoProducer, publish_camera};
