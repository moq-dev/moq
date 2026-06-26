//! AAC.
//!
//! ISO 14496-3 AudioSpecificConfig parse and encode lives in [`Config`].
//! [`Import`] publishes raw AAC frames (not ADTS) to a moq broadcast.

mod import;

pub use import::*;

use bytes::{Buf, Bytes};

/// AAC parsing errors.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	#[error("AudioSpecificConfig must be at least 2 bytes")]
	ConfigTooShort,

	#[error("extended audioObjectType requires 2 additional bytes")]
	ExtendedConfigTooShort,

	#[error("AudioSpecificConfig incomplete")]
	IncompleteConfig,

	#[error("explicit sample rate requires 3 additional bytes")]
	ExplicitSampleRateTooShort,

	#[error("unsupported sample rate index: {0}")]
	UnsupportedSampleRateIndex(u8),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Typed AAC configuration mirroring the relevant fields of an
/// AudioSpecificConfig.
pub struct Config {
	pub profile: u8,
	pub sample_rate: u32,
	pub channel_count: u32,
}

impl Config {
	/// Parse an AudioSpecificConfig buffer.
	///
	/// Handles basic formats (object_type < 31), extended formats
	/// (object_type == 31), and explicit sample rates (freq_index == 15).
	/// Any SBR/PS extension bytes after the core fields are consumed.
	pub fn parse<T: Buf>(buf: &mut T) -> Result<Self> {
		if buf.remaining() < 2 {
			return Err(Error::ConfigTooShort);
		}

		let data = buf.copy_to_bytes(buf.remaining());
		let mut bits = BitReader::new(&data);

		let mut object_type = bits.read(5).ok_or(Error::ConfigTooShort)? as u8;
		if object_type == 31 {
			let ext = bits.read(6).ok_or(Error::ExtendedConfigTooShort)? as u8;
			object_type = 32 + ext;
		}

		let freq_index = bits.read(4).ok_or(Error::IncompleteConfig)? as u8;
		let sample_rate = if freq_index == 15 {
			bits.read(24).ok_or(Error::ExplicitSampleRateTooShort)? as u32
		} else {
			sample_rate_from_index(freq_index)?
		};
		let channel_config = bits.read(4).ok_or(Error::IncompleteConfig)? as u8;
		let channel_count = channel_count_from_config(channel_config);

		Ok(Self {
			profile: object_type,
			sample_rate,
			channel_count,
		})
	}

	/// Encode this configuration as an AudioSpecificConfig (ISO 14496-3 §1.6.2.1).
	///
	/// Standard sample rates produce 2 bytes; non-standard rates fall back to
	/// the 5-byte form with an explicit 24-bit frequency.
	pub fn encode(&self) -> Bytes {
		// audioObjectType is a 5-bit field; mask to prevent shift overflow.
		let profile = self.profile & 0x1F;

		let freq_index: u8 = match self.sample_rate {
			96000 => 0,
			88200 => 1,
			64000 => 2,
			48000 => 3,
			44100 => 4,
			32000 => 5,
			24000 => 6,
			22050 => 7,
			16000 => 8,
			12000 => 9,
			11025 => 10,
			8000 => 11,
			7350 => 12,
			_ => 0xF, // explicit 24-bit frequency follows
		};

		let channel_config = channel_config_from_count(self.channel_count) as u64;

		if freq_index != 0xF {
			// 5 + 4 + 4 = 13 bits → 2 bytes (3 bits padding)
			let b0 = (profile << 3) | (freq_index >> 1);
			let b1 = ((freq_index & 1) << 7) | ((channel_config as u8 & 0x0F) << 3);
			Bytes::from(vec![b0, b1])
		} else {
			// 5 + 4 + 24 + 4 = 37 bits → 5 bytes (3 bits padding)
			let mut bits: u64 = 0;
			bits |= (profile as u64) << 35;
			bits |= 0xF_u64 << 31;
			bits |= (self.sample_rate as u64) << 7;
			bits |= (channel_config & 0xF) << 3;
			let all = bits.to_be_bytes();
			Bytes::copy_from_slice(&all[3..8])
		}
	}
}

struct BitReader<'a> {
	data: &'a [u8],
	offset: usize,
}

impl<'a> BitReader<'a> {
	fn new(data: &'a [u8]) -> Self {
		Self { data, offset: 0 }
	}

	fn read(&mut self, count: usize) -> Option<u64> {
		if self.offset + count > self.data.len() * 8 {
			return None;
		}

		let mut value = 0;
		for _ in 0..count {
			let byte = self.data[self.offset / 8];
			let shift = 7 - (self.offset % 8);
			value = (value << 1) | (((byte >> shift) & 1) as u64);
			self.offset += 1;
		}
		Some(value)
	}
}

fn sample_rate_from_index(freq_index: u8) -> Result<u32> {
	const SAMPLE_RATES: [u32; 13] = [
		96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
	];

	SAMPLE_RATES
		.get(freq_index as usize)
		.copied()
		.ok_or(Error::UnsupportedSampleRateIndex(freq_index))
}

/// Map an AAC `channel_config` (ISO 14496-3 Table 1.19) to its real channel count.
/// Configs 1..=6 happen to be identity (5.1 has config=6 and 6 channels). Config
/// 7 is 7.1 = 8 channels. Config 0 means "described elsewhere" — we default to
/// stereo.
fn channel_count_from_config(channel_config: u8) -> u32 {
	match channel_config {
		1..=6 => channel_config as u32,
		7 => 8,
		0 => {
			tracing::warn!("channel_config=0 (program config element) unsupported, defaulting to stereo");
			2
		}
		_ => {
			tracing::warn!(channel_config, "unsupported channel config, defaulting to stereo");
			2
		}
	}
}

/// Inverse of [`channel_count_from_config`]. Defaults to stereo for unsupported
/// counts (channel configs > 7 are reserved).
fn channel_config_from_count(channel_count: u32) -> u8 {
	match channel_count {
		1..=6 => channel_count as u8,
		8 => 7,
		_ => {
			tracing::warn!(channel_count, "unsupported channel count, defaulting to stereo");
			2
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_standard_2_byte_config() {
		// AAC-LC (profile=2), 44100 Hz (freq_index=4), stereo (channels=2).
		// b0 = 0x12 (00010 0100b → object_type=2, freq_index high 3 bits=010)
		// b1 = 0x10 (0001 0000b → freq_index low bit=0, channel_config=2, padding=000)
		let buf = vec![0x12, 0x10];
		let cfg = Config::parse(&mut buf.as_slice()).unwrap();
		assert_eq!(cfg.profile, 2);
		assert_eq!(cfg.sample_rate, 44100);
		assert_eq!(cfg.channel_count, 2);
	}

	#[test]
	fn round_trip_explicit_sample_rate() {
		let cfg = Config {
			profile: 2,
			sample_rate: 44_123,
			channel_count: 2,
		};
		let encoded = cfg.encode();
		let parsed = Config::parse(&mut encoded.as_ref()).unwrap();
		assert_eq!(parsed.profile, cfg.profile);
		assert_eq!(parsed.sample_rate, cfg.sample_rate);
		assert_eq!(parsed.channel_count, cfg.channel_count);
	}

	#[test]
	fn round_trip_5_1_channels() {
		// 5.1 surround: config=6, 6 channels.
		let cfg = Config {
			profile: 2,
			sample_rate: 48000,
			channel_count: 6,
		};
		let encoded = cfg.encode();
		let parsed = Config::parse(&mut encoded.as_ref()).unwrap();
		assert_eq!(parsed.channel_count, 6);
	}

	#[test]
	fn round_trip_7_1_channels() {
		// 7.1 surround: config=7, but 8 channels.
		let cfg = Config {
			profile: 2,
			sample_rate: 48000,
			channel_count: 8,
		};
		let encoded = cfg.encode();
		let parsed = Config::parse(&mut encoded.as_ref()).unwrap();
		assert_eq!(parsed.channel_count, 8, "7.1 surround should round-trip as 8 channels");
	}

	#[test]
	fn channel_config_zero_falls_back_to_stereo() {
		// Config 0 means "described in PCE" which we don't implement.
		assert_eq!(channel_count_from_config(0), 2);
	}

	#[test]
	fn unsupported_channel_count_falls_back_to_stereo_config() {
		assert_eq!(channel_config_from_count(9), 2);
	}
}
