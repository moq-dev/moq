//! Encode captured video and publish it as a moq video track.
//!
//! The output codec is selected via [`Codec`] (H.264 or H.265); see its docs
//! for which backends cover each on this platform.
//!
//! Entry points, high to low level:
//! - [`publish_capture`] captures and publishes a webcam (turnkey).
//! - [`Encoder`] encodes raw RGBA frames you supply, and [`Producer`]
//!   publishes the resulting packets (bring your own frames). Build both for the
//!   same [`Codec`].
//! - [`Producer`] alone publishes packets you already encoded.
//!
//! [`Options`] / [`Kind`] / [`Config`] configure them. The decode/consume
//! counterpart (mirror of `moq-audio`'s consumer) lives in the sibling
//! [`decode`](crate::decode) module.
//!
//! [`rate`] holds the policy mapping a congestion-control bandwidth estimate
//! onto the encoder's bitrate, which [`publish_capture`] drives for you.

mod backend;
mod encoder;
mod producer;
mod sink;

pub mod rate;

pub use encoder::{Codec, Config, Encoder, Kind};
pub use producer::{Options, Producer, publish_capture};
