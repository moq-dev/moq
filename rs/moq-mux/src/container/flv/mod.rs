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
//! MP3 is handled in both generations: the legacy SoundFormat 2 tag and the
//! E-RTMP `.mp3` FourCC on import, and the legacy SoundFormat 2 tag on export.
//!
//! Each codec's config record passes straight through as the catalog
//! `description` (or, for the in-band codecs VP9 / MP3 / AC-3 / E-AC-3, is read
//! from the frame), and the sample bytes already match the
//! [`Legacy`](crate::catalog::hang::Container) container, so no codec transform is
//! needed. Other legacy codecs (VP6, Speex, …) and the E-RTMP FLAC audio are
//! rejected on import and export.

mod export;
mod import;

pub use export::*;
pub use import::*;

/// FLV export errors.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// A decode or presentation time falls before the stream's origin or past FLV's 32-bit
	/// millisecond timestamp.
	#[error("FLV timestamp before the stream's origin or past 32 bits")]
	TimestampRange,

	/// A composition time offset does not fit FLV's signed 24-bit field.
	#[error("FLV composition time exceeds signed 24 bits")]
	CompositionTimeRange,

	/// A tag body exceeds FLV's 24-bit `DataSize` field (16 MiB).
	#[error("FLV tag body of {0} bytes exceeds the 24-bit DataSize limit")]
	TagTooLarge(usize),

	/// More renditions of one kind than FLV's 8-bit multitrack id can number.
	#[error("too many FLV {0} tracks")]
	TooManyTracks(&'static str),

	/// A codec that needs a sequence-header tag has no config record.
	#[error("FLV track '{0}' missing its codec config record")]
	MissingConfigRecord(String),

	/// A rendition moved to a track with another codec after the stream header fixed its tags.
	#[error("FLV track '{0}' moved to another codec after the stream header")]
	CodecChanged(String),

	/// A rendition's container is not one whose frames FLV tags carry as is.
	#[error("FLV export does not support container '{container}' on {kind} track '{name}'")]
	UnsupportedContainer {
		/// `video` or `audio`.
		kind: &'static str,
		/// The rendition name.
		name: String,
		/// The catalog container kind.
		container: String,
	},

	/// The video codec has no FLV or enhanced-RTMP mapping.
	#[error("FLV export does not support video codec {0}")]
	UnsupportedVideoCodec(String),

	/// The audio codec has no FLV or enhanced-RTMP mapping.
	#[error("FLV export does not support audio codec {0}")]
	UnsupportedAudioCodec(String),
}

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
/// SoundFormat for MP3 (high nibble of an audio tag's first byte).
const AUDIO_FORMAT_MP3: u8 = 2;

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

/// First byte of an MP3 audio tag: SoundFormat 2, with the same nominal
/// SoundRate / SoundSize / SoundType bits as AAC. The real rate and channel
/// layout live in the MP3 frame header that follows, so these bits are nominal.
const MP3_AUDIO_TAG_HEADER: u8 = (AUDIO_FORMAT_MP3 << 4) | (3 << 2) | (1 << 1) | 1;

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
/// Multitrack framing: the next byte carries the `AvMultitrackType` plus the real
/// `VideoPacketType`, and each track payload is tagged with its own track id.
const VIDEO_PACKET_MULTITRACK: u8 = 5;

/// Enhanced-RTMP audio signaling: SoundFormat 9 in the high nibble of an audio
/// tag's first byte switches to a FourCC codec + packet type.
const AUDIO_FORMAT_EX: u8 = 9;

/// Enhanced audio `AudioPacketType` (low nibble of an ex-audio tag's first byte).
const AUDIO_PACKET_SEQUENCE_START: u8 = 0;
const AUDIO_PACKET_CODED_FRAMES: u8 = 1;
const AUDIO_PACKET_SEQUENCE_END: u8 = 2;
const AUDIO_PACKET_MULTICHANNEL_CONFIG: u8 = 4;
/// Multitrack framing, mirroring [`VIDEO_PACKET_MULTITRACK`] for audio.
const AUDIO_PACKET_MULTITRACK: u8 = 5;

/// `AvMultitrackType` (high nibble of the multitrack framing byte): how the
/// per-track payloads that follow are laid out.
///
/// - `OneTrack`: a single track, tagged with its id; the payload runs to the end
///   of the tag (no per-track size).
/// - `ManyTracks`: several tracks sharing one codec, each length-prefixed.
/// - `ManyTracksManyCodecs`: several tracks, each carrying its own FourCC and
///   length prefix.
const MULTITRACK_ONE_TRACK: u8 = 0;
const MULTITRACK_MANY_TRACKS: u8 = 1;
const MULTITRACK_MANY_TRACKS_MANY_CODECS: u8 = 2;

/// Read a 24-bit big-endian unsigned integer.
fn read_u24(b: &[u8]) -> u32 {
	((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32)
}

/// Read a 24-bit big-endian signed integer (FLV composition time offset).
fn read_i24(b: &[u8]) -> i32 {
	let v = read_u24(b) as i32;
	if v & 0x80_0000 != 0 { v - 0x100_0000 } else { v }
}
