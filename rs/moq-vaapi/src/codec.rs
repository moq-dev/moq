// Vendored from discord/cros-codecs (BSD-3-Clause): H.264, and the H.265
// parser and DPB.
pub mod h264;
pub mod h265;

/// A video coding format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Codec {
	/// H.264 (AVC).
	H264,
	/// H.265 (HEVC).
	H265,
}
