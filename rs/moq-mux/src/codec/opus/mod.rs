//! Opus parsing and OpusHead helpers.
//!
//! Centralizes RFC 7845 OpusHead encode/decode so the import and export
//! paths share one implementation. [`import::Import`] publishes raw Opus frames.

pub mod import;

use bytes::{Buf, Bytes};

const OPUS_HEAD: u64 = u64::from_be_bytes(*b"OpusHead");

/// Typed Opus configuration mirroring the parsed fields of an OpusHead packet.
pub struct OpusConfig {
	pub sample_rate: u32,
	pub channel_count: u32,
}

impl OpusConfig {
	/// Parse an OpusHead buffer (RFC 7845 §5.1).
	///
	/// Verifies the magic signature; reads channel count and sample rate;
	/// ignores pre-skip, gain, and channel mapping. Any trailing bytes are
	/// consumed.
	pub fn parse<T: Buf>(buf: &mut T) -> anyhow::Result<Self> {
		anyhow::ensure!(buf.remaining() >= 19, "OpusHead must be at least 19 bytes");
		let signature = buf.get_u64();
		anyhow::ensure!(signature == OPUS_HEAD, "invalid OpusHead signature");

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

	/// Encode the minimal OpusHead packet (19 bytes; mono/stereo channel
	/// mapping family 0, zero pre-skip and gain).
	pub fn encode(&self) -> Bytes {
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
