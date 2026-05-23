//! Pure per-frame codec-shape transforms.
//!
//! These are stateless-w.r.t.-broadcasts primitives that container exporters
//! (or any other consumer) compose to bridge between codec shapes. Each
//! transform takes a `Bytes` payload and returns a transformed `Bytes`,
//! caching parameter sets and exposing the synthesized configuration record
//! as a side channel.
//!
//! Current contents:
//! - [`Avc1`]: H.264 Annex-B (inline SPS/PPS) → length-prefixed + out-of-band avcC.
//! - [`Hvc1`]: H.265 Annex-B (inline VPS/SPS/PPS) → length-prefixed + out-of-band hvcC.

mod avc1;
mod hvc1;

pub use avc1::Avc1;
pub use hvc1::Hvc1;
