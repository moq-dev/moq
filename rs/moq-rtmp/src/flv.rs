//! Synthesize an FLV byte stream from RTMP audio/video messages.
//!
//! RTMP carries media as messages whose payloads are exactly FLV tag *bodies*:
//! an audio message (type 8) is an FLV AUDIODATA body, a video message (type 9)
//! is an FLV VIDEODATA body. moq-mux's [`flv::Import`](moq_mux::container::flv)
//! consumes a whole FLV byte stream (file header + framed tags), so to reuse it
//! we re-wrap each RTMP message in the FLV file/tag framing it expects rather
//! than demuxing RTMP ourselves. That demuxer handles both legacy (H.264 / AAC)
//! and enhanced-RTMP (HEVC / AV1 / VP9 / Opus / AC-3) payloads, so this framing
//! is codec-agnostic.
//!
//! See `moq-mux/src/container/flv` for the matching reader; the field layout
//! here mirrors what it parses (11-byte tag header, 24-bit + 8-bit extended
//! millisecond timestamp, trailing `PreviousTagSize`).

use bytes::{BufMut, Bytes, BytesMut};

/// FLV tag type for audio data (matches moq-mux's `TAG_AUDIO`).
pub const TAG_AUDIO: u8 = 8;
/// FLV tag type for video data (matches moq-mux's `TAG_VIDEO`).
pub const TAG_VIDEO: u8 = 9;

/// The 9-byte FLV file header plus its 4-byte `PreviousTagSize0`, emitted once
/// at the start of the synthesized stream before any tag.
///
/// `46 4C 56` = "FLV", version 1, flags `0x05` (audio + video present),
/// data offset 9, then a zero `PreviousTagSize0`. moq-mux only checks the
/// "FLV" magic and the data offset, but we emit a spec-correct header anyway.
pub fn file_header() -> Bytes {
	Bytes::from_static(&[
		b'F', b'L', b'V', // signature
		0x01, // version
		0x05, // flags: audio (bit 2) + video (bit 0)
		0x00, 0x00, 0x00, 0x09, // data offset (header length)
		0x00, 0x00, 0x00, 0x00, // PreviousTagSize0
	])
}

/// Frame one RTMP message body as an FLV tag: the 11-byte tag header (type,
/// 24-bit data size, 24-bit + 8-bit extended timestamp, 24-bit stream id = 0),
/// the body, then the 4-byte `PreviousTagSize` trailer (header + body length).
///
/// `timestamp` is the RTMP message timestamp in milliseconds; FLV splits it into
/// a low 24 bits and a high "extended" byte, exactly how moq-mux reassembles it.
pub fn tag(tag_type: u8, timestamp: u32, body: &[u8]) -> Bytes {
	let data_size = body.len();
	let mut buf = BytesMut::with_capacity(11 + data_size + 4);

	buf.put_u8(tag_type);
	// 24-bit data size, big-endian.
	buf.put_u8((data_size >> 16) as u8);
	buf.put_u8((data_size >> 8) as u8);
	buf.put_u8(data_size as u8);
	// Timestamp: low 24 bits big-endian, then the extended (most significant) byte.
	buf.put_u8((timestamp >> 16) as u8);
	buf.put_u8((timestamp >> 8) as u8);
	buf.put_u8(timestamp as u8);
	buf.put_u8((timestamp >> 24) as u8);
	// Stream id is always 0.
	buf.put_u8(0);
	buf.put_u8(0);
	buf.put_u8(0);

	buf.put_slice(body);

	// PreviousTagSize: the size of the tag header + body that precedes it.
	buf.put_u32(11 + data_size as u32);

	buf.freeze()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn tag_layout_roundtrips_timestamp_and_size() {
		let body = [0x17, 0x00, 0x00, 0x00, 0x00, 0xde, 0xad];
		let ts = 0x01_02_03_04; // exercises the extended byte
		let t = tag(TAG_VIDEO, ts, &body);

		assert_eq!(t[0], TAG_VIDEO);
		// 24-bit data size.
		let size = ((t[1] as usize) << 16) | ((t[2] as usize) << 8) | (t[3] as usize);
		assert_eq!(size, body.len());
		// Timestamp = low 24 bits | extended byte << 24, matching the moq-mux reader.
		let read = ((t[4] as u32) << 16) | ((t[5] as u32) << 8) | (t[6] as u32) | ((t[7] as u32) << 24);
		assert_eq!(read, ts);
		// Stream id is zero.
		assert_eq!(&t[8..11], &[0, 0, 0]);
		// Body follows the 11-byte header.
		assert_eq!(&t[11..11 + body.len()], &body);
		// Trailing PreviousTagSize = header + body.
		let prev = u32::from_be_bytes([t[t.len() - 4], t[t.len() - 3], t[t.len() - 2], t[t.len() - 1]]);
		assert_eq!(prev, 11 + body.len() as u32);
	}

	#[test]
	fn file_header_has_flv_magic() {
		let h = file_header();
		assert_eq!(&h[0..3], b"FLV");
		assert_eq!(h.len(), 13);
	}
}
