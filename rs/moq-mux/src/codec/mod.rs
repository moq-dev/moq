//! Per-codec parsing, building, and codec-shape transforms.
//!
//! One module per codec. Each owns parsing and building of the codec
//! configuration record (avcC / hvcC / av1C / AudioSpecificConfig / OpusHead),
//! along with any Annex-B → length-prefixed transforms applicable to that codec
//! ([`h264::Avc1`], [`h265::Hvc1`]). Importers and exporters route through
//! these modules instead of inlining codec parsing.

pub mod aac;
pub mod av1;
pub mod h264;
pub mod h265;
pub mod opus;

pub use h264::Avc1;
pub use h265::Hvc1;
