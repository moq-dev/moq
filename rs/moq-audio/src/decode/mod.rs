//! Subscribe to an encoded audio track and decode it to raw PCM.
//!
//! The decode counterpart to [`encode`](crate::encode), and the mirror of
//! `moq-video`'s [`decode`](https://docs.rs/moq-video) module.
//!
//! Entry points, high to low level:
//! - [`Consumer`] subscribes to a track and hands back decoded
//!   [`Frame`](crate::Frame)s.
//! - [`Decoder`] decodes packets you supply (bring your own payloads) into
//!   [`Decoded`] interleaved `f32` samples.
//!
//! [`Options`] keeps subscription and output policy separate from the
//! lower-level decoder [`Config`], whose [`Kind`] picks the backend: a
//! platform decoder first where one takes the track, then software (libopus,
//! PCM, and symphonia for AAC-LC).

mod backend;
mod consumer;
mod decoded;
mod decoder;

pub use consumer::{Consumer, Options, Output, Start};
pub use decoded::Decoded;
pub use decoder::{Config, Decoder, Kind};
