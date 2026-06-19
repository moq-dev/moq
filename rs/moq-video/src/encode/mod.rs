//! Encode captured video and publish it as a moq H.264 track.
//!
//! Entry points, high to low level:
//! - [`publish_capture`] captures and publishes a webcam (turnkey).
//! - [`Encoder`] H.264-encodes raw RGBA frames you supply, and [`Producer`]
//!   publishes the resulting packets (bring your own frames).
//! - [`Producer`] alone publishes H.264 you already encoded.
//!
//! [`Options`] / [`Kind`] / [`Config`] configure them. The decode/consume
//! counterpart (mirror of `moq-audio`'s consumer) lives in the sibling
//! [`decode`](crate::decode) module.

mod backend;
mod encoder;
mod producer;

pub use encoder::{Config, Encoder, Kind};
pub use producer::{Options, Producer, publish_capture};
