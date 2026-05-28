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
	#[error("OpusHead must be at least 19 bytes")]
	HeadTooShort,

	#[error("invalid OpusHead signature")]
	InvalidSignature,
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
	/// Panics if `channel_count > 2` — mapping family 0 is only defined for
	/// mono/stereo per RFC 7845 §5.1. Multi-channel streams need family 1
	/// with a channel mapping table, which this helper does not emit.
	pub fn encode(&self) -> Bytes {
		assert!(
			self.channel_count <= 2,
			"OpusHead mapping family 0 only supports mono/stereo (got channel_count={})",
			self.channel_count
		);
		let mut head = Vec::with_capacity(19);
		head.extend_from_slice(b"OpusHead");
		head.push(1); // version
		head.push(self.channel_count as u8);
		head.extend_from_slice(&0u16.to_le_bytes()); // pre-skip
		head.extend_from_slice(&self.sample_rate.to_le_bytes());
		head.extend_from_slice(&0i16.to_le_bytes()); // output gain
		head.push(0); // channel mapping family (0 = mono/stereo)
		Bytes::from(head)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_valid_opus_head() {
		let cfg = Config {
			sample_rate: 48000,
			channel_count: 2,
		};
		let encoded = cfg.encode();
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
		.to_vec();
		bytes[0] = b'X';
		assert!(Config::parse(&mut bytes.as_slice()).is_err());
	}

	#[test]
	#[should_panic(expected = "mapping family 0")]
	fn encode_panics_for_multichannel() {
		Config {
			sample_rate: 48000,
			channel_count: 6,
		}
		.encode();
	}
}
