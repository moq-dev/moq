//! Codecs.
//!
//! One submodule per codec. Each owns parsers and builders for the
//! codec's configuration record (avcC, hvcC, av1C, AudioSpecificConfig,
//! OpusHead), any inline-to-out-of-band transforms applicable to that
//! codec, and an `Import` type that publishes a raw bitstream as a moq
//! broadcast.

pub mod aac;
pub mod annexb;
pub mod av1;
pub mod h264;
pub mod h265;
pub mod opus;
