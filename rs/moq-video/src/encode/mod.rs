//! Encode captured video and publish it as a moq video track.
//!
//! The output codec is selected via [`Codec`] (H.264 or H.265); see its docs
//! for which backends cover each on this platform.
//!
//! Entry points, high to low level:
//! - `publish_capture` captures and publishes a webcam (turnkey). Requires the
//!   `capture` feature.
//! - [`Encoder`] encodes raw [`Frame`](crate::Frame)s you supply into
//!   [`Encoded`] access units, and [`Producer`] publishes those (bring your own
//!   frames). Build both for the same [`Codec`].
//! - [`Sink`] is an [`Encoder`] that owns the thread it runs on, for an encoder
//!   outliving a single thread's stack (a shared object, an FFI handle, a task
//!   that migrates between workers).
//! - [`Producer`] alone publishes frames you already encoded.
//!
//! A [`Producer`] advertises its catalog rendition from a [`Hint`] as soon as
//! the track exists, so a subscriber can discover a track nothing has encoded
//! yet. That's what makes on-demand encoding possible at all.
//!
//! [`Options`] / [`Kind`] / [`Config`] configure them. The decode/consume
//! counterpart (mirror of `moq-audio`'s consumer) lives in the sibling
//! [`decode`](crate::decode) module.
//!
//! [`rate`] holds the policy mapping a congestion-control bandwidth estimate
//! onto the encoder's bitrate, which `publish_capture` drives for you.

mod backend;
mod encoded;
mod encoder;
mod hint;
mod producer;
mod sink;

pub mod rate;

pub use encoded::Encoded;
pub use encoder::{Codec, Config, Encoder, Kind};
pub use hint::{Hint, h264_level, h265_level};
pub use producer::Producer;
#[cfg(feature = "capture")]
pub use producer::{Options, publish_capture};
pub use sink::Sink;
