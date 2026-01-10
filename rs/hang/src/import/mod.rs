mod aac;
mod annexb;
mod avc3;
mod decoder;
mod fmp4;
mod hev1;
mod hls;
mod opus;

pub use aac::*;
pub use avc3::*;
pub use decoder::*;
pub use fmp4::*;
pub use hev1::*;
pub use hls::*;
pub use opus::*;

// TODO this should be configurable
pub const DEFAULT_MAX_LATENCY: moq_lite::Time = moq_lite::Time::from_secs_unchecked(30);
