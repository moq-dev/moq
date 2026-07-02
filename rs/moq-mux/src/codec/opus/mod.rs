//! Opus.
//!
//! RFC 7845 OpusHead parse and encode lives in [`Config`]. [`Import`]
//! publishes raw Opus frames (no Ogg framing) to a moq broadcast.

mod import;

pub use import::*;

use bytes::{Buf, Bytes};

const OPUS_HEAD: u64 = u64::from_be_bytes(*b"OpusHead");

/// Opus parsing errors.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// The OpusHead packet was shorter than the 19-byte minimum (RFC 7845 §5.1).
	#[error("OpusHead must be at least 19 bytes")]
	HeadTooShort,

	/// The packet did not start with the `OpusHead` magic signature.
	#[error("invalid OpusHead signature")]
	InvalidSignature,

	/// [`Config::encode`] was asked to emit an OpusHead for a channel count other
	/// than mono or stereo; channel mapping family 0 only covers 1 or 2 channels.
	#[error("channel mapping family 0 only supports mono/stereo (got {0} channels)")]
	UnsupportedChannelCount(u32),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Typed Opus configuration mirroring the parsed fields of an OpusHead packet.
pub struct Config {
	pub sample_rate: u32,
	pub channel_count: u32,
}

impl Config {
	/// Parse an OpusHead buffer (RFC 7845 §5.1).
	///
	/// Verifies the magic signature; reads channel count and sample rate;
	/// ignores pre-skip, gain, and channel mapping. Any trailing bytes are
	/// consumed.
	pub fn parse<T: Buf>(buf: &mut T) -> Result<Self> {
		if buf.remaining() < 19 {
			return Err(Error::HeadTooShort);
		}
		let signature = buf.get_u64();
		if signature != OPUS_HEAD {
			return Err(Error::InvalidSignature);
		}

		buf.advance(1); // Skip version
		let channel_count = buf.get_u8() as u32;
		buf.advance(2); // Skip pre-skip
		let sample_rate = buf.get_u32_le();

		// Skip gain, channel mapping until if/when we support them.
		if buf.remaining() > 0 {
			buf.advance(buf.remaining());
		}

		Ok(Self {
			sample_rate,
			channel_count,
		})
	}

	/// Encode the minimal OpusHead packet (19 bytes; channel mapping family
	/// 0, zero pre-skip and gain).
	///
	/// Errors with [`Error::UnsupportedChannelCount`] unless `channel_count` is 1
	/// or 2, since mapping family 0 is only defined for mono/stereo per RFC 7845 §5.1.
	/// Multi-channel streams need family 1 with a channel mapping table, which
	/// this helper does not emit.
	pub fn encode(&self) -> Result<Bytes> {
		if !(1..=2).contains(&self.channel_count) {
			return Err(Error::UnsupportedChannelCount(self.channel_count));
		}
		let mut head = Vec::with_capacity(19);
		head.extend_from_slice(b"OpusHead");
		head.push(1); // version
		head.push(self.channel_count as u8);
		head.extend_from_slice(&0u16.to_le_bytes()); // pre-skip
		head.extend_from_slice(&self.sample_rate.to_le_bytes());
		head.extend_from_slice(&0i16.to_le_bytes()); // output gain
		head.push(0); // channel mapping family (0 = mono/stereo)
		Ok(Bytes::from(head))
	}
}

/// Number of 48 kHz samples in an Opus packet, read from its TOC byte (RFC 6716 §3.1).
///
/// MPEG-TS aggregates several Opus packets into one PES, so the importer advances each
/// packet's timestamp by this. Opus timing is always reckoned at 48 kHz regardless of the
/// encoder's internal bandwidth. Returns `None` for an empty packet or a code-3 packet
/// missing its frame-count byte.
pub(crate) fn packet_samples(packet: &[u8]) -> Option<u32> {
	let toc = *packet.first()?;
	let frames = match toc & 0b11 {
		0 => 1,
		1 | 2 => 2,
		// Code 3: the frame count is the low 6 bits of the following byte.
		_ => (packet.get(1)? & 0b0011_1111) as u32,
	};
	Some(config_samples(toc >> 3) * frames)
}

/// 48 kHz samples per frame for an Opus TOC config index (0..=31), per RFC 6716 Table 1.
fn config_samples(config: u8) -> u32 {
	match config {
		// SILK NB/MB/WB: 10, 20, 40, 60 ms.
		0 | 4 | 8 => 480,
		1 | 5 | 9 => 960,
		2 | 6 | 10 => 1920,
		3 | 7 | 11 => 2880,
		// Hybrid SWB/FB: 10, 20 ms.
		12 | 14 => 480,
		13 | 15 => 960,
		// CELT NB/WB/SWB/FB: 2.5, 5, 10, 20 ms.
		16 | 20 | 24 | 28 => 120,
		17 | 21 | 25 | 29 => 240,
		18 | 22 | 26 | 30 => 480,
		// 19, 23, 27, 31 are the 20 ms CELT configs.
		_ => 960,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn packet_samples_reads_toc() {
		// config 16 (CELT NB 2.5 ms = 120 samples), code 0 (1 frame).
		assert_eq!(packet_samples(&[16 << 3]), Some(120));
		// config 3 (SILK NB 60 ms = 2880), code 0.
		assert_eq!(packet_samples(&[3 << 3]), Some(2880));
		// config 1 (SILK NB 20 ms = 960), code 1 (2 frames) -> 1920.
		assert_eq!(packet_samples(&[(1 << 3) | 1]), Some(1920));
		// config 1, code 3 with 4 frames -> 3840.
		assert_eq!(packet_samples(&[(1 << 3) | 3, 4]), Some(3840));
		assert_eq!(packet_samples(&[]), None);
	}

	#[test]
	fn parses_valid_opus_head() {
		let cfg = Config {
			sample_rate: 48000,
			channel_count: 2,
		};
		let encoded = cfg.encode().unwrap();
		assert_eq!(encoded.len(), 19);
		let parsed = Config::parse(&mut encoded.as_ref()).unwrap();
		assert_eq!(parsed.sample_rate, 48000);
		assert_eq!(parsed.channel_count, 2);
	}

	#[test]
	fn parse_rejects_invalid_signature() {
		let mut bytes = Config {
			sample_rate: 48000,
			channel_count: 1,
		}
		.encode()
		.unwrap()
		.to_vec();
		bytes[0] = b'X';
		assert!(Config::parse(&mut bytes.as_slice()).is_err());
	}

	#[test]
	fn encode_rejects_multichannel() {
		let err = Config {
			sample_rate: 48000,
			channel_count: 6,
		}
		.encode()
		.unwrap_err();
		assert!(matches!(err, Error::UnsupportedChannelCount(6)));
	}
}
