//! FLV (Flash Video / RTMP container).
//!
//! An interchange format only, not a wire format: [`Import`] demuxes an FLV byte
//! stream into a broadcast and [`Export`] muxes a broadcast back into FLV. Only
//! the modern FLV payload is handled: H.264 (AVC) video carried as
//! length-prefixed NALU with an out-of-band `AVCDecoderConfigurationRecord`
//! (avcC), and AAC audio carried raw with an out-of-band `AudioSpecificConfig`.
//! Both records pass straight through as the catalog `description`, and the
//! sample bytes already match the [`Legacy`](crate::catalog::hang::Container)
//! container, so no codec transform is needed. Other legacy codecs (VP6, MP3,
//! Speex, …) and the enhanced E-RTMP FourCC payloads are logged and dropped on
//! import, and rejected on export.

mod export;
mod import;

pub use export::*;
pub use import::*;

#[cfg(test)]
mod export_test;
#[cfg(test)]
mod import_test;

/// FLV tag type: audio.
const TAG_AUDIO: u8 = 8;
/// FLV tag type: video.
const TAG_VIDEO: u8 = 9;
/// FLV tag type: script data (`onMetaData`, etc.).
const TAG_SCRIPT: u8 = 18;

/// Bytes in an FLV tag header (type, data size, timestamp, stream id).
const TAG_HEADER_LEN: usize = 11;
/// Bytes in the `PreviousTagSize` field that trails every tag.
const PREV_TAG_SIZE_LEN: usize = 4;
/// Bytes in the FLV file header preceding the first `PreviousTagSize`.
const FILE_HEADER_LEN: usize = 9;

/// Video CodecID for H.264 (low nibble of a video tag's first byte).
const VIDEO_CODEC_AVC: u8 = 7;
/// SoundFormat for AAC (high nibble of an audio tag's first byte).
const AUDIO_FORMAT_AAC: u8 = 10;

/// Video FrameType for a keyframe.
const FRAME_TYPE_KEY: u8 = 1;
/// Video FrameType for an inter frame.
const FRAME_TYPE_INTER: u8 = 2;

/// AVCPacketType for the `AVCDecoderConfigurationRecord`.
const AVC_SEQUENCE_HEADER: u8 = 0;
/// AVCPacketType for a length-prefixed NALU access unit.
const AVC_NALU: u8 = 1;

/// AACPacketType for the `AudioSpecificConfig`.
const AAC_SEQUENCE_HEADER: u8 = 0;
/// AACPacketType for a raw AAC frame.
const AAC_RAW: u8 = 1;

/// Standard first byte of an AAC audio tag: SoundFormat 10, SoundRate 3
/// (44 kHz flag), SoundSize 1 (16-bit), SoundType 1 (stereo). The real rate and
/// channel layout live in the `AudioSpecificConfig`, so these bits are nominal.
const AAC_AUDIO_TAG_HEADER: u8 = (AUDIO_FORMAT_AAC << 4) | (3 << 2) | (1 << 1) | 1;

/// Read a 24-bit big-endian unsigned integer.
fn read_u24(b: &[u8]) -> u32 {
	((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32)
}

/// Read a 24-bit big-endian signed integer (FLV composition time offset).
fn read_i24(b: &[u8]) -> i32 {
	let v = read_u24(b) as i32;
	if v & 0x80_0000 != 0 { v - 0x100_0000 } else { v }
}
