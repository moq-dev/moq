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
//!
//! The reverse direction (play / egress) uses [`TagReader`]: it splits the FLV
//! byte stream that [`moq_mux::container::flv::Export`] produces back into
//! individual tags, so each tag body can be sent to a player as an RTMP
//! audio/video message.

use bytes::{Buf, BufMut, Bytes, BytesMut};

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

/// One FLV media tag pulled out of an [`Export`](moq_mux::container::flv::Export)
/// byte stream by [`TagReader`], ready to send as an RTMP message body.
pub struct Tag {
	/// FLV tag type: [`TAG_AUDIO`] or [`TAG_VIDEO`].
	pub tag_type: u8,
	/// Tag timestamp in milliseconds (the reassembled 24-bit + extended byte).
	pub timestamp: u32,
	/// The tag body, i.e. the bytes of the RTMP audio/video message to send.
	pub body: Bytes,
}

/// Splits an FLV byte stream back into its individual tags.
///
/// The inverse of [`file_header`] + [`tag`]: feed the chunks
/// [`Export`](moq_mux::container::flv::Export) yields via [`push`](Self::push),
/// then drain whole tags with [`next`](Self::next). The leading FLV file header
/// is consumed and discarded; only the audio/video tags surface, each carrying
/// the body to forward as an RTMP message.
#[derive(Default)]
pub struct TagReader {
	/// Bytes received but not yet parsed into a complete tag.
	buf: BytesMut,
	/// Set once the FLV file header has been consumed.
	header_done: bool,
}

impl TagReader {
	/// Create an empty reader.
	pub fn new() -> Self {
		Self::default()
	}

	/// Append a chunk of the FLV byte stream.
	pub fn push(&mut self, bytes: &[u8]) {
		self.buf.extend_from_slice(bytes);
	}

	/// Pop the next complete tag, or `None` if more bytes are needed first.
	///
	/// Errors only if the stream doesn't start with the FLV signature.
	pub fn next(&mut self) -> anyhow::Result<Option<Tag>> {
		if !self.header_done {
			// File header: a 9-byte header whose last 4 bytes are the DataOffset,
			// followed by the 4-byte PreviousTagSize0.
			if self.buf.len() < FILE_HEADER_LEN {
				return Ok(None);
			}
			anyhow::ensure!(&self.buf[0..3] == b"FLV", "not an FLV stream");
			let data_offset = u32::from_be_bytes([self.buf[5], self.buf[6], self.buf[7], self.buf[8]]) as usize;
			let skip = data_offset + 4;
			if self.buf.len() < skip {
				return Ok(None);
			}
			self.buf.advance(skip);
			self.header_done = true;
		}

		// Tag header (11 bytes), body (DataSize bytes), then the 4-byte PreviousTagSize.
		if self.buf.len() < TAG_HEADER_LEN {
			return Ok(None);
		}
		let tag_type = self.buf[0];
		let data_size = ((self.buf[1] as usize) << 16) | ((self.buf[2] as usize) << 8) | (self.buf[3] as usize);
		let timestamp = ((self.buf[4] as u32) << 16)
			| ((self.buf[5] as u32) << 8)
			| (self.buf[6] as u32)
			| ((self.buf[7] as u32) << 24);

		if self.buf.len() < TAG_HEADER_LEN + data_size + 4 {
			return Ok(None);
		}

		self.buf.advance(TAG_HEADER_LEN);
		let body = self.buf.split_to(data_size).freeze();
		self.buf.advance(4); // PreviousTagSize

		Ok(Some(Tag {
			tag_type,
			timestamp,
			body,
		}))
	}
}

/// Bytes in the FLV file header (signature, version, flags, DataOffset).
const FILE_HEADER_LEN: usize = 9;
/// Bytes in an FLV tag header (type, 24-bit size, timestamp, stream id).
const TAG_HEADER_LEN: usize = 11;

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn tag_reader_splits_header_and_tags() {
		let mut reader = TagReader::new();
		reader.push(&file_header());
		// A header chunk often carries the sequence-header tag(s) too; feed two tags.
		reader.push(&tag(TAG_VIDEO, 0, &[0x17, 0x00]));
		reader.push(&tag(TAG_AUDIO, 0x01_02_03_04, &[0xaf, 0x00, 0x12]));

		let v = reader.next().unwrap().expect("video tag");
		assert_eq!(v.tag_type, TAG_VIDEO);
		assert_eq!(v.timestamp, 0);
		assert_eq!(&v.body[..], &[0x17, 0x00]);

		let a = reader.next().unwrap().expect("audio tag");
		assert_eq!(a.tag_type, TAG_AUDIO);
		assert_eq!(a.timestamp, 0x01_02_03_04);
		assert_eq!(&a.body[..], &[0xaf, 0x00, 0x12]);

		assert!(reader.next().unwrap().is_none());
	}

	#[test]
	fn tag_reader_waits_for_a_whole_tag() {
		let mut reader = TagReader::new();
		reader.push(&file_header());
		let full = tag(TAG_VIDEO, 7, &[1, 2, 3, 4, 5]);
		// Feed everything but the last byte: no complete tag yet.
		reader.push(&full[..full.len() - 1]);
		assert!(reader.next().unwrap().is_none());
		// The final byte completes it.
		reader.push(&full[full.len() - 1..]);
		let t = reader.next().unwrap().expect("tag once complete");
		assert_eq!(t.timestamp, 7);
		assert_eq!(&t.body[..], &[1, 2, 3, 4, 5]);
	}

	#[test]
	fn tag_reader_round_trips_export_style_framing() {
		// Mirrors how Export emits: header chunk, then one tag per chunk.
		let mut reader = TagReader::new();
		let mut header = BytesMut::new();
		header.extend_from_slice(&file_header());
		header.extend_from_slice(&tag(TAG_VIDEO, 0, b"seqhdr"));
		reader.push(&header);
		reader.push(&tag(TAG_VIDEO, 33, b"frame"));

		assert_eq!(&reader.next().unwrap().unwrap().body[..], b"seqhdr");
		assert_eq!(&reader.next().unwrap().unwrap().body[..], b"frame");
		assert!(reader.next().unwrap().is_none());
	}

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
