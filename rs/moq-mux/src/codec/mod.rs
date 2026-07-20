//! Codecs.
//!
//! One submodule per codec. Each owns parsers and builders for the
//! codec's configuration record (avcC, hvcC, av1C, AudioSpecificConfig,
//! OpusHead), any inline-to-out-of-band transforms applicable to that
//! codec, and an `Import` type that publishes a raw bitstream as a moq
//! broadcast.

pub mod aac;
pub(crate) mod ac3;
pub mod annexb;
pub mod av1;
pub(crate) mod eac3;
pub mod flac;
pub mod h264;
pub mod h265;
pub(crate) mod legacy;
pub(crate) mod mp2;
pub mod mp3;
pub mod opus;
pub(crate) mod video;
pub mod vp8;
pub mod vp9;
