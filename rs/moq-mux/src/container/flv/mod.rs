//! FLV (Flash Video / RTMP container).
//!
//! An interchange format only, not a wire format: [`Import`] demuxes an FLV byte
//! stream into a broadcast and [`Export`] muxes a broadcast back into FLV. Two
//! payload generations are handled:
//!
//! - **Legacy FLV/RTMP**: H.264 (AVC) video as length-prefixed NALU with an
//!   out-of-band `AVCDecoderConfigurationRecord` (avcC), and AAC audio with an
//!   out-of-band `AudioSpecificConfig`.
//! - **Enhanced RTMP (E-RTMP)**: the FourCC payloads for HEVC (`hvc1`), AV1
//!   (`av01`), VP9 (`vp09`), Opus (`Opus`), AC-3 (`ac-3`), and E-AC-3 (`ec-3`).
//!
//! Each codec's config record passes straight through as the catalog
//! `description` (or, for the in-band codecs VP9 / AC-3 / E-AC-3, is read from the
//! frame), and the sample bytes already match the
//! [`Legacy`](crate::catalog::hang::Container) container, so no codec transform is
//! needed. Other legacy codecs (VP6, MP3, Speex, …) and the E-RTMP FLAC / MP3
//! audio are logged and dropped on import, and rejected on export.

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

/// Enhanced-RTMP (E-RTMP) video signaling: a set high bit on a video tag's
/// first byte switches from a legacy CodecID to a FourCC codec + packet type.
const VIDEO_EX_HEADER: u8 = 0x80;

/// Enhanced video `VideoPacketType` (low nibble of an ex-video tag's first byte).
const VIDEO_PACKET_SEQUENCE_START: u8 = 0;
const VIDEO_PACKET_CODED_FRAMES: u8 = 1;
const VIDEO_PACKET_SEQUENCE_END: u8 = 2;
/// Coded frames with the composition-time offset omitted (always zero).
const VIDEO_PACKET_CODED_FRAMES_X: u8 = 3;
const VIDEO_PACKET_METADATA: u8 = 4;

/// Enhanced-RTMP audio signaling: SoundFormat 9 in the high nibble of an audio
/// tag's first byte switches to a FourCC codec + packet type.
const AUDIO_FORMAT_EX: u8 = 9;

/// Enhanced audio `AudioPacketType` (low nibble of an ex-audio tag's first byte).
const AUDIO_PACKET_SEQUENCE_START: u8 = 0;
const AUDIO_PACKET_CODED_FRAMES: u8 = 1;
const AUDIO_PACKET_SEQUENCE_END: u8 = 2;
const AUDIO_PACKET_MULTICHANNEL_CONFIG: u8 = 4;

/// Read a 24-bit big-endian unsigned integer.
fn read_u24(b: &[u8]) -> u32 {
	((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32)
}

/// Read a 24-bit big-endian signed integer (FLV composition time offset).
fn read_i24(b: &[u8]) -> i32 {
	let v = read_u24(b) as i32;
	if v & 0x80_0000 != 0 { v - 0x100_0000 } else { v }
}
