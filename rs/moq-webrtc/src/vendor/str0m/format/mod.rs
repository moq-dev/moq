//! Media formats and parameters

// These really don't belong anywhere, but I guess they're kind of related
// to codecs etc.
pub use crate::vendor::str0m::packet::CodecExtra;
pub use crate::vendor::str0m::packet::H264CodecExtra;
pub use crate::vendor::str0m::packet::H265CodecExtra;
pub use crate::vendor::str0m::packet::Vp8CodecExtra;
pub use crate::vendor::str0m::packet::Vp9CodecExtra;
pub use crate::vendor::str0m::packet::Vp9PacketizerMode;
pub use crate::vendor::str0m::packet::detect_av1_keyframe;
pub use crate::vendor::str0m::packet::detect_h265_keyframe;
pub use crate::vendor::str0m::packet::detect_vp8_keyframe;
pub use crate::vendor::str0m::packet::detect_vp9_keyframe;
pub use crate::vendor::str0m::packet::{H265Level, H265Profile, H265ProfileTierLevel, H265Tier};
pub use crate::vendor::str0m::packet::{detect_h264_keyframe, detect_h264_nalu};

mod codec;
mod codec_config;
mod format_params;
mod payload_params;

pub use codec::{Codec, CodecSpec};
pub use codec_config::CodecConfig;
pub use format_params::FormatParams;
pub use payload_params::PayloadParams;
