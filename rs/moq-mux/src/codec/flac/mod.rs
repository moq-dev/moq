//! FLAC.
//!
//! Parse and serialize the FLAC `STREAMINFO` metadata block (RFC 9639 §8.2),
//! the codec's configuration record. [`Config::description`] builds the
//! WebCodecs `description` (the `fLaC` stream marker followed by the STREAMINFO
//! block) so a browser can initialize a decoder from the catalog alone.
//! [`Import`] publishes raw FLAC frames to a moq broadcast.

mod import;

pub use import::*;

use bytes::{Buf, BufMut, Bytes};

/// The four-byte stream marker that opens a native FLAC bitstream and the
/// WebCodecs `description`.
const MARKER: [u8; 4] = *b"fLaC";

/// Length of a STREAMINFO metadata block body in bytes (RFC 9639 §8.2).
const STREAMINFO_LEN: usize = 34;

/// FLAC parsing errors.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// The buffer ended before a full STREAMINFO (or its enclosing header) could be read.
	#[error("buffer too short for FLAC STREAMINFO")]
	Short,

	/// A FLAC header did not begin with the `fLaC` stream marker.
	#[error("invalid FLAC stream marker")]
	InvalidMarker,

	/// The first metadata block was not STREAMINFO, which RFC 9639 §8.1 requires.
	#[error("first metadata block is not STREAMINFO")]
	MissingStreamInfo,
}

pub type Result<T> = std::result::Result<T, Error>;

/// Typed FLAC configuration mirroring the fields of a STREAMINFO metadata block
/// (RFC 9639 §8.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
	/// Minimum block size in samples.
	pub min_block_size: u16,
	/// Maximum block size in samples.
	pub max_block_size: u16,
	/// Minimum frame size in bytes (0 = unknown).
	pub min_frame_size: u32,
	/// Maximum frame size in bytes (0 = unknown).
	pub max_frame_size: u32,
	/// Sample rate in Hz (20-bit field, so 1..=1_048_575).
	pub sample_rate: u32,
	/// Channel count (1..=8).
	pub channel_count: u32,
	/// Bits per sample (4..=32).
	pub bits_per_sample: u32,
	/// Total interchannel samples (36-bit field, 0 = unknown).
	pub total_samples: u64,
	/// MD5 of the unencoded audio (all-zero = unknown).
	pub md5: [u8; 16],
}

impl Config {
	/// Parse a 34-byte STREAMINFO body (RFC 9639 §8.2), without the `fLaC`
	/// marker or metadata block header.
	pub fn parse_stream_info<T: Buf>(buf: &mut T) -> Result<Self> {
		if buf.remaining() < STREAMINFO_LEN {
			return Err(Error::Short);
		}

		let min_block_size = buf.get_u16();
		let max_block_size = buf.get_u16();
		let min_frame_size = buf.get_uint(3) as u32;
		let max_frame_size = buf.get_uint(3) as u32;

		// Sample rate (20), channels-1 (3), bits-1 (5), and total samples (36) are
		// packed into a single 64-bit field.
		let packed = buf.get_u64();
		let sample_rate = (packed >> 44) as u32;
		let channel_count = (((packed >> 41) & 0x7) as u32) + 1;
		let bits_per_sample = (((packed >> 36) & 0x1f) as u32) + 1;
		let total_samples = packed & 0xF_FFFF_FFFF;

		let mut md5 = [0u8; 16];
		buf.copy_to_slice(&mut md5);

		Ok(Self {
			min_block_size,
			max_block_size,
			min_frame_size,
			max_frame_size,
			sample_rate,
			channel_count,
			bits_per_sample,
			total_samples,
			md5,
		})
	}

	/// Parse a FLAC header: the `fLaC` stream marker followed by the STREAMINFO
	/// metadata block. This is the Matroska `A_FLAC` CodecPrivate / WebCodecs
	/// `description` layout. Trailing metadata blocks (Vorbis comments, etc.) are
	/// ignored.
	pub fn parse<T: Buf>(buf: &mut T) -> Result<Self> {
		if buf.remaining() < 4 {
			return Err(Error::Short);
		}
		let mut marker = [0u8; 4];
		buf.copy_to_slice(&mut marker);
		if marker != MARKER {
			return Err(Error::InvalidMarker);
		}

		if buf.remaining() < 4 {
			return Err(Error::Short);
		}
		// Metadata block header: 1-bit last flag, 7-bit block type, 24-bit length.
		let header = buf.get_u32();
		let block_type = ((header >> 24) & 0x7f) as u8;
		if block_type != 0 {
			return Err(Error::MissingStreamInfo);
		}

		Self::parse_stream_info(buf)
	}

	/// Serialize the 34-byte STREAMINFO body.
	pub fn encode_stream_info(&self) -> Bytes {
		let mut buf = Vec::with_capacity(STREAMINFO_LEN);
		buf.put_u16(self.min_block_size);
		buf.put_u16(self.max_block_size);
		buf.put_uint(self.min_frame_size as u64, 3);
		buf.put_uint(self.max_frame_size as u64, 3);

		let packed = ((self.sample_rate as u64 & 0xF_FFFF) << 44)
			| ((self.channel_count.saturating_sub(1) as u64 & 0x7) << 41)
			| ((self.bits_per_sample.saturating_sub(1) as u64 & 0x1f) << 36)
			| (self.total_samples & 0xF_FFFF_FFFF);
		buf.put_u64(packed);
		buf.put_slice(&self.md5);

		buf.into()
	}

	/// Build the WebCodecs `description`: the `fLaC` stream marker, a metadata
	/// block header flagged as the final block, and the STREAMINFO body. This is
	/// the `description` a FLAC `AudioDecoder` expects per the WebCodecs FLAC
	/// registration.
	pub fn description(&self) -> Bytes {
		let stream_info = self.encode_stream_info();
		let mut buf = Vec::with_capacity(MARKER.len() + 4 + stream_info.len());
		buf.put_slice(&MARKER);
		// Last-metadata-block flag (0x80) set, block type 0 (STREAMINFO).
		buf.put_u8(0x80);
		buf.put_uint(stream_info.len() as u64, 3);
		buf.put_slice(&stream_info);
		buf.into()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn sample() -> Config {
		Config {
			min_block_size: 4608,
			max_block_size: 4608,
			min_frame_size: 16,
			max_frame_size: 9102,
			sample_rate: 44_100,
			channel_count: 2,
			bits_per_sample: 24,
			total_samples: 120_832,
			md5: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
		}
	}

	#[test]
	fn stream_info_roundtrip() {
		let cfg = sample();
		let encoded = cfg.encode_stream_info();
		assert_eq!(encoded.len(), STREAMINFO_LEN);
		let parsed = Config::parse_stream_info(&mut encoded.as_ref()).unwrap();
		assert_eq!(parsed, cfg);
	}

	#[test]
	fn description_roundtrip() {
		let cfg = sample();
		let desc = cfg.description();
		// "fLaC" + 4-byte block header + 34-byte STREAMINFO.
		assert_eq!(desc.len(), 4 + 4 + STREAMINFO_LEN);
		assert_eq!(&desc[..4], b"fLaC");
		// Final block flag set, block type 0 (STREAMINFO).
		assert_eq!(desc[4], 0x80);

		// The description parses back into the same config.
		let parsed = Config::parse(&mut desc.as_ref()).unwrap();
		assert_eq!(parsed, cfg);
	}

	#[test]
	fn parse_rejects_bad_marker() {
		let mut desc = sample().description().to_vec();
		desc[0] = b'X';
		assert!(matches!(Config::parse(&mut desc.as_slice()), Err(Error::InvalidMarker)));
	}

	#[test]
	fn parse_rejects_non_streaminfo_first_block() {
		let mut desc = sample().description().to_vec();
		// Flip the block type away from 0 (STREAMINFO) while keeping the marker.
		desc[4] = 0x80 | 0x04; // last block, type 4 (Vorbis comment)
		assert!(matches!(
			Config::parse(&mut desc.as_slice()),
			Err(Error::MissingStreamInfo)
		));
	}
}
