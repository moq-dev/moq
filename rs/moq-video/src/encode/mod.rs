//! Encode captured video and publish it as a moq H.264 track.
//!
//! Two public entry points:
//! - [`publish_capture`] captures and publishes a webcam (turnkey).
//! - [`Producer`] publishes H.264 you already encoded yourself (bring your
//!   own packets).
//!
//! [`Options`] / [`Kind`] configure them. The raw ffmpeg encoder is kept
//! internal so the public surface stays ffmpeg-free; the decode/consume
//! counterpart (mirror of `moq-audio`'s consumer) will land in a sibling
//! `decode` module.

mod encoder;
mod producer;

pub use encoder::Kind;
pub use producer::{Options, Producer, publish_capture};
