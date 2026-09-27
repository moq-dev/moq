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

	/// The OpusHead major version (the upper four bits) is not one this parser
	/// understands, so none of its fields can be trusted (RFC 7845 §5.1).
	#[error("unsupported OpusHead version {0}")]
	UnsupportedVersion(u8),

	/// The channel count is zero, or more than the channel mapping family
	/// allows: family 0 (the only one [`Config::encode`] emits) covers
	/// mono/stereo, and family 1 up to eight channels.
	#[error("channel mapping family does not allow {0} channels")]
	UnsupportedChannelCount(u32),

	/// A nonzero channel mapping family without its complete mapping table.
	#[error("OpusHead channel mapping table is truncated")]
	MappingTableTooShort,

	/// The channel mapping table names no streams, more coupled streams than
	/// streams, or a channel index past the decoded streams.
	#[error("invalid OpusHead channel mapping table")]
	InvalidMappingTable,

	/// [`Config::encode`] only emits channel mapping family 0.
	#[error("cannot encode channel mapping family {0}")]
	UnsupportedMappingFamily(u8),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Typed Opus configuration mirroring the parsed fields of an OpusHead packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Config {
	/// Original input sample rate in Hz, or 0 when unknown.
	///
	/// Metadata only: Opus always runs on a 48 kHz clock, and this need not be a
	/// rate Opus can decode at (44.1 kHz is common).
	pub sample_rate: u32,
	/// Number of output channels.
	pub channel_count: u32,
	/// Number of decoded 48 kHz samples to discard at stream start.
	pub pre_skip: u16,
	/// Gain to apply to the decoded output, in Q7.8 dB.
	pub output_gain: i16,
	/// Channel mapping family; 0 is mono/stereo with no mapping table.
	pub mapping_family: u8,
}

impl Config {
	/// Build a mono/stereo Opus config with no pre-skip or gain.
	pub fn new(sample_rate: u32, channel_count: u32) -> Self {
		Self {
			sample_rate,
			channel_count,
			pre_skip: 0,
			output_gain: 0,
			mapping_family: 0,
		}
	}

	/// Set the number of decoded 48 kHz samples to discard at stream start.
	pub fn with_pre_skip(mut self, pre_skip: u16) -> Self {
		self.pre_skip = pre_skip;
		self
	}

	/// Parse an OpusHead buffer (RFC 7845 §5.1).
	///
	/// Accepts any minor version and refuses a new major version. A nonzero
	/// channel mapping family must carry a complete, consistent mapping table,
	/// which is validated but not returned. Bytes after the header are left in
	/// `buf`.
	pub fn parse<T: Buf>(buf: &mut T) -> Result<Self> {
		if buf.remaining() < 19 {
			return Err(Error::HeadTooShort);
		}
		let signature = buf.get_u64();
		if signature != OPUS_HEAD {
			return Err(Error::InvalidSignature);
		}

		let version = buf.get_u8();
		if version > 15 {
			return Err(Error::UnsupportedVersion(version));
		}
		let channel_count = buf.get_u8() as u32;
		let pre_skip = buf.get_u16_le();
		let sample_rate = buf.get_u32_le();
		let output_gain = buf.get_i16_le();
		let mapping_family = buf.get_u8();

		let max_channels = match mapping_family {
			0 => 2,
			1 => 8,
			_ => 255,
		};
		if channel_count == 0 || channel_count > max_channels {
			return Err(Error::UnsupportedChannelCount(channel_count));
		}

		if mapping_family != 0 {
			if buf.remaining() < 2 + channel_count as usize {
				return Err(Error::MappingTableTooShort);
			}
			let streams = buf.get_u8() as u32;
			let coupled = buf.get_u8() as u32;
			if streams == 0 || coupled > streams || streams + coupled > 255 {
				return Err(Error::InvalidMappingTable);
			}
			for _ in 0..channel_count {
				// 255 marks a silent channel.
				let index = buf.get_u8() as u32;
				if index != 255 && index >= streams + coupled {
					return Err(Error::InvalidMappingTable);
				}
			}
		}

		Ok(Self {
			sample_rate,
			channel_count,
			pre_skip,
			output_gain,
			mapping_family,
		})
	}

	/// Encode the minimal OpusHead packet (19 bytes; channel mapping family 0).
	///
	/// Errors unless the mapping family is 0 and `channel_count` is 1 or 2, since
	/// multichannel streams need a mapping table this helper does not emit.
	pub fn encode(&self) -> Result<Bytes> {
		if self.mapping_family != 0 {
			return Err(Error::UnsupportedMappingFamily(self.mapping_family));
		}
		if !(1..=2).contains(&self.channel_count) {
			return Err(Error::UnsupportedChannelCount(self.channel_count));
		}
		let mut head = Vec::with_capacity(19);
		head.extend_from_slice(b"OpusHead");
		head.push(1); // version
		head.push(self.channel_count as u8);
		head.extend_from_slice(&self.pre_skip.to_le_bytes());
		head.extend_from_slice(&self.sample_rate.to_le_bytes());
		head.extend_from_slice(&self.output_gain.to_le_bytes());
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
		let cfg = Config::new(48_000, 2).with_pre_skip(312);
		let encoded = cfg.encode().unwrap();
		assert_eq!(encoded.len(), 19);
		let parsed = Config::parse(&mut encoded.as_ref()).unwrap();
		assert_eq!(parsed.sample_rate, 48_000);
		assert_eq!(parsed.channel_count, 2);
		assert_eq!(parsed.pre_skip, 312);
		assert_eq!(parsed, cfg);
	}

	#[test]
	fn parse_rejects_invalid_signature() {
		let mut bytes = Config::new(48_000, 1).encode().unwrap().to_vec();
		bytes[0] = b'X';
		assert!(Config::parse(&mut bytes.as_slice()).is_err());
	}

	#[test]
	fn encode_rejects_multichannel() {
		let err = Config::new(48_000, 6).encode().unwrap_err();
		assert!(matches!(err, Error::UnsupportedChannelCount(6)));
	}

	/// A 19-byte family 0 head with the given version, channels, gain, and family.
	fn head(version: u8, channels: u8, gain: i16, family: u8) -> Vec<u8> {
		let mut head = b"OpusHead".to_vec();
		head.push(version);
		head.push(channels);
		head.extend_from_slice(&312u16.to_le_bytes());
		head.extend_from_slice(&44_100u32.to_le_bytes());
		head.extend_from_slice(&gain.to_le_bytes());
		head.push(family);
		head
	}

	#[test]
	fn parses_gain_and_input_rate() {
		let bytes = head(1, 2, -1536, 0);
		let parsed = Config::parse(&mut bytes.as_slice()).unwrap();
		assert_eq!(parsed.sample_rate, 44_100);
		assert_eq!(parsed.pre_skip, 312);
		assert_eq!(parsed.output_gain, -1536);
		assert_eq!(parsed.mapping_family, 0);

		// The gain survives a round trip.
		let encoded = parsed.encode().unwrap();
		assert_eq!(encoded.as_ref(), bytes.as_slice());
	}

	#[test]
	fn parse_accepts_minor_versions_only() {
		for version in [0, 1, 15] {
			assert!(Config::parse(&mut head(version, 2, 0, 0).as_slice()).is_ok());
		}
		assert!(matches!(
			Config::parse(&mut head(16, 2, 0, 0).as_slice()),
			Err(Error::UnsupportedVersion(16))
		));
	}

	#[test]
	fn parse_rejects_channel_counts_the_family_does_not_allow() {
		for (channels, family) in [(0, 0), (3, 0), (0, 1), (9, 1)] {
			assert!(
				matches!(
					Config::parse(&mut head(1, channels, 0, family).as_slice()),
					Err(Error::UnsupportedChannelCount(_))
				),
				"{channels} channels in family {family}"
			);
		}
	}

	#[test]
	fn parse_reads_a_mapping_table() {
		// 5.1 in family 1: four streams, two coupled, Vorbis order.
		let mut bytes = head(1, 6, 0, 1);
		bytes.extend_from_slice(&[4, 2, 0, 4, 1, 2, 3, 5]);
		bytes.extend_from_slice(b"trailing");
		let mut buf = bytes.as_slice();
		let parsed = Config::parse(&mut buf).unwrap();
		assert_eq!(parsed.channel_count, 6);
		assert_eq!(parsed.mapping_family, 1);
		assert_eq!(buf, b"trailing");

		// Encode only emits family 0.
		assert!(matches!(parsed.encode(), Err(Error::UnsupportedMappingFamily(1))));
	}

	#[test]
	fn parse_rejects_a_truncated_or_invalid_mapping_table() {
		// No table at all.
		let bytes = head(1, 2, 0, 1);
		assert!(matches!(
			Config::parse(&mut bytes.as_slice()),
			Err(Error::MappingTableTooShort)
		));

		// Table cut short of its last channel.
		let mut bytes = head(1, 2, 0, 1);
		bytes.extend_from_slice(&[1, 1, 0]);
		assert!(matches!(
			Config::parse(&mut bytes.as_slice()),
			Err(Error::MappingTableTooShort)
		));

		for table in [
			[0, 0, 0, 1], // no streams
			[1, 2, 0, 1], // more coupled than streams
			[1, 1, 0, 2], // index past the two decoded channels
		] {
			let mut bytes = head(1, 2, 0, 1);
			bytes.extend_from_slice(&table);
			assert!(
				matches!(Config::parse(&mut bytes.as_slice()), Err(Error::InvalidMappingTable)),
				"{table:?}"
			);
		}

		// 255 is a silent channel, not an index.
		let mut bytes = head(1, 2, 0, 1);
		bytes.extend_from_slice(&[1, 1, 0, 255]);
		assert!(Config::parse(&mut bytes.as_slice()).is_ok());
	}
}
