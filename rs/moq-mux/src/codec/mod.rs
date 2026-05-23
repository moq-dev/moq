//! Codecs.
//!
//! One submodule per codec. Each owns its config-record parser (avcC,
//! hvcC, …), any Annex-B → length-prefixed transforms, and an `Import`
//! that publishes raw bitstreams into a moq broadcast.

pub mod aac;
pub mod annexb;
pub mod av1;
pub mod h264;
pub mod h265;
pub mod opus;
