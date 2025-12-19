mod aac;
mod avc3;
mod decoder;
mod fmp4;
mod hls;

pub use aac::*;
pub use avc3::*;
pub use decoder::*;
pub use fmp4::*;
pub use hls::*;

// TODO this should be configurable
pub const DEFAULT_MAX_LATENCY: std::time::Duration = std::time::Duration::from_millis(10000);
