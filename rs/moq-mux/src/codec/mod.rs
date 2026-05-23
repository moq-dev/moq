//! Per-codec parsing, building, and codec-shape transforms.
//!
//! One module per codec. Each owns parsing and building of the codec
//! configuration record (avcC / hvcC / av1C / AudioSpecificConfig / OpusHead),
//! along with any Annex-B → length-prefixed transforms applicable to that codec
//! ([`h264::Avc1`], [`h265::Hvc1`]) and a per-codec `Import` that publishes raw
//! bitstreams as moq broadcasts.

pub mod aac;
pub mod annexb;
pub mod av1;
pub mod h264;
pub mod h265;
pub mod opus;
