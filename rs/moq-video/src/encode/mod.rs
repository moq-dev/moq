//! Encode captured video and publish it as a moq H.264 track.
//!
//! High-level entry: [`publish_capture`] (and the [`Producer`] / [`Options`]
//! it builds on). The raw codec building blocks live in [`encoder`]. The
//! decode/consume counterpart (mirror of `moq-audio`'s consumer) will land in
//! a sibling `decode` module.

pub mod encoder;
mod producer;

pub use producer::{Options, Producer, publish_capture};
