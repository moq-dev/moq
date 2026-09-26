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

	/// The channel count is zero, or more than the channel mapping family allows:
	/// family 0 covers mono/stereo and family 1 up to eight channels.
	#[error("channel mapping family does not allow {0} channels")]
	UnsupportedChannelCount(u32),

	/// A nonzero channel mapping family without its complete mapping table.
	#[error("OpusHead channel mapping table is truncated")]
	MappingTableTooShort,

	/// The channel mapping table names no streams, more coupled streams than
	/// streams, or a channel index past the decoded streams.
	#[error("invalid OpusHead channel mapping table")]
	InvalidMappingTable,

	/// No longer returned, since [`Config::encode`] emits every family; kept so
	/// matching on it still compiles until the next breaking release.
	#[error("cannot encode channel mapping family {0}")]
	UnsupportedMappingFamily(u8),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Typed Opus configuration mirroring the parsed fields of an OpusHead packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Config {
	/// Original input sample rate in Hz.
	pub sample_rate: u32,
	/// Number of output channels.
	pub channel_count: u32,
	/// Number of decoded 48 kHz samples to discard at stream start.
	pub pre_skip: u16,
	/// The channel mapping table, or `None` for family 0 (mono/stereo, one stream).
	pub mapping: Option<Mapping>,
}

/// An OpusHead channel mapping table (RFC 7845 §5.1.1), present for every family but 0.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Mapping {
	family: u8,
	streams: u8,
	coupled: u8,
	channels: u8,
	// Sized for the most channels any family allows, so `Config` stays `Copy`.
	table: [u8; 255],
}

/// The family 1 stream counts and table for 1 to 8 channels, as libopus and
/// RFC 7845 §5.1.1.2 lay out the Vorbis channel orders.
const VORBIS: [(u8, u8, &[u8]); 8] = [
	(1, 0, &[0]),
	(1, 1, &[0, 1]),
	(2, 1, &[0, 2, 1]),
	(2, 2, &[0, 1, 2, 3]),
	(3, 2, &[0, 4, 1, 2, 3]),
	(4, 2, &[0, 4, 1, 2, 3, 5]),
	(4, 3, &[0, 4, 1, 2, 3, 5, 6]),
	(5, 3, &[0, 6, 1, 2, 3, 4, 5, 7]),
];

impl Mapping {
	/// The family 1 mapping a surround encoder uses for `channels` in Vorbis
	/// order, for sources that name only a channel count.
	pub(crate) fn vorbis(channels: u8) -> Result<Self> {
		let (streams, coupled, order) = *channels
			.checked_sub(1)
			.and_then(|index| VORBIS.get(index as usize))
			.ok_or(Error::UnsupportedChannelCount(channels as u32))?;

		let mut table = [0u8; 255];
		table[..order.len()].copy_from_slice(order);
		Ok(Self {
			family: 1,
			streams,
			coupled,
			channels,
			table,
		})
	}

	fn parse<T: Buf>(buf: &mut T, family: u8, channels: u8) -> Result<Self> {
		if buf.remaining() < 2 + channels as usize {
			return Err(Error::MappingTableTooShort);
		}
		let streams = buf.get_u8();
		let coupled = buf.get_u8();
		if streams == 0 || coupled > streams || streams as u32 + coupled as u32 > 255 {
			return Err(Error::InvalidMappingTable);
		}

		let mut table = [0u8; 255];
		for entry in &mut table[..channels as usize] {
			*entry = buf.get_u8();
			// 255 marks a silent channel.
			if *entry != 255 && *entry >= streams + coupled {
				return Err(Error::InvalidMappingTable);
			}
		}

		Ok(Self {
			family,
			streams,
			coupled,
			channels,
			table,
		})
	}

	/// The channel mapping family: 1 is the Vorbis speaker order, 2 and 3 are
	/// ambisonics, and 255 is channels with no declared position.
	pub fn family(&self) -> u8 {
		self.family
	}

	/// Number of Opus streams in each packet.
	pub fn streams(&self) -> u8 {
		self.streams
	}

	/// How many of those streams are coupled (stereo); they come first.
	pub fn coupled(&self) -> u8 {
		self.coupled
	}

	/// The decoded channel feeding each output channel, 255 for silence.
	pub fn table(&self) -> &[u8] {
		&self.table[..self.channels as usize]
	}
}

impl std::fmt::Debug for Mapping {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Mapping")
			.field("family", &self.family)
			.field("streams", &self.streams)
			.field("coupled", &self.coupled)
			.field("table", &self.table())
			.finish()
	}
}

impl Config {
	/// Build a mono/stereo Opus config with no pre-skip.
	pub fn new(sample_rate: u32, channel_count: u32) -> Self {
		Self {
			sample_rate,
			channel_count,
			pre_skip: 0,
			mapping: None,
		}
	}

	/// Set the number of decoded 48 kHz samples to discard at stream start.
	pub fn with_pre_skip(mut self, pre_skip: u16) -> Self {
		self.pre_skip = pre_skip;
		self
	}

	/// Parse an OpusHead buffer (RFC 7845 §5.1).
	///
	/// Verifies the magic signature; reads channel count, pre-skip, sample rate,
	/// and the channel mapping, refusing a channel count the family forbids or an
	/// inconsistent mapping table; ignores gain. Any trailing bytes are consumed.
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
		let pre_skip = buf.get_u16_le();
		let sample_rate = buf.get_u32_le();
		buf.advance(2); // Skip gain until if/when we support it.
		let family = buf.get_u8();

		let max_channels = match family {
			0 => 2,
			1 => 8,
			_ => 255,
		};
		if channel_count == 0 || channel_count > max_channels {
			return Err(Error::UnsupportedChannelCount(channel_count));
		}

		let mapping = match family {
			0 => None,
			family => Some(Mapping::parse(buf, family, channel_count as u8)?),
		};

		if buf.remaining() > 0 {
			buf.advance(buf.remaining());
		}

		Ok(Self {
			sample_rate,
			channel_count,
			pre_skip,
			mapping,
		})
	}

	/// Encode an OpusHead packet (RFC 7845 §5.1) with zero gain, followed by
	/// the channel mapping table when there is one.
	///
	/// Errors with [`Error::UnsupportedChannelCount`] when `channel_count` is not
	/// 1 or 2 without a `mapping` (family 0 is only defined for mono/stereo), or
	/// is not the mapping's own channel count.
	pub fn encode(&self) -> Result<Bytes> {
		let valid = match &self.mapping {
			None => (1..=2).contains(&self.channel_count),
			Some(mapping) => self.channel_count == mapping.channels as u32,
		};
		if !valid {
			return Err(Error::UnsupportedChannelCount(self.channel_count));
		}

		let mut head = Vec::with_capacity(21 + self.channel_count as usize);
		head.extend_from_slice(b"OpusHead");
		head.push(1); // version
		head.push(self.channel_count as u8);
		head.extend_from_slice(&self.pre_skip.to_le_bytes());
		head.extend_from_slice(&self.sample_rate.to_le_bytes());
		head.extend_from_slice(&0i16.to_le_bytes()); // output gain
		match &self.mapping {
			None => head.push(0),
			Some(mapping) => {
				head.extend_from_slice(&[mapping.family, mapping.streams, mapping.coupled]);
				head.extend_from_slice(mapping.table());
			}
		}
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

	/// A 19-byte head with the given channel count and mapping family.
	fn head(channels: u8, family: u8) -> Vec<u8> {
		let mut head = Config::new(48_000, 2).encode().unwrap().to_vec();
		head[9] = channels;
		head[18] = family;
		head
	}

	#[test]
	fn parses_a_mapping_table() {
		// 5.1 in family 1: four streams, two coupled, Vorbis order.
		let mut bytes = head(6, 1);
		bytes.extend_from_slice(&[4, 2, 0, 4, 1, 2, 3, 5]);
		let parsed = Config::parse(&mut bytes.as_slice()).unwrap();
		assert_eq!(parsed.channel_count, 6);

		let mapping = parsed.mapping.unwrap();
		assert_eq!(mapping.family(), 1);
		assert_eq!(mapping.streams(), 4);
		assert_eq!(mapping.coupled(), 2);
		assert_eq!(mapping.table(), &[0, 4, 1, 2, 3, 5]);

		// The table survives a re-encode.
		assert_eq!(parsed.encode().unwrap(), bytes);
	}

	#[test]
	fn encodes_the_vorbis_mappings() {
		for channels in 1..=8u8 {
			let mut config = Config::new(48_000, channels as u32);
			config.mapping = Some(Mapping::vorbis(channels).unwrap());
			let head = config.encode().unwrap();
			assert_eq!(head.len(), 21 + channels as usize, "{channels} channels");
			assert_eq!(
				Config::parse(&mut head.as_ref()).unwrap(),
				config,
				"{channels} channels"
			);
		}

		// 5.1 is the table libopus and ffmpeg write.
		let five_one = Mapping::vorbis(6).unwrap();
		assert_eq!((five_one.streams(), five_one.coupled()), (4, 2));
		assert_eq!(five_one.table(), &[0, 4, 1, 2, 3, 5]);

		assert!(matches!(Mapping::vorbis(0), Err(Error::UnsupportedChannelCount(0))));
		assert!(matches!(Mapping::vorbis(9), Err(Error::UnsupportedChannelCount(9))));

		// The channel count must agree with the table.
		let mut config = Config::new(48_000, 5);
		config.mapping = Some(five_one);
		assert!(matches!(config.encode(), Err(Error::UnsupportedChannelCount(5))));
	}

	#[test]
	fn parse_rejects_channel_counts_the_family_does_not_allow() {
		for (channels, family) in [(0, 0), (3, 0), (0, 1), (9, 1), (0, 255)] {
			let mut bytes = head(channels, family);
			bytes.extend_from_slice(&[1, 0]);
			bytes.extend(std::iter::repeat_n(0, channels as usize));
			assert!(
				matches!(
					Config::parse(&mut bytes.as_slice()),
					Err(Error::UnsupportedChannelCount(_))
				),
				"{channels} channels in family {family}"
			);
		}
	}

	#[test]
	fn parse_rejects_a_bad_mapping_table() {
		// Family 1 promising a table that is not there, or only half of it.
		assert!(matches!(
			Config::parse(&mut head(2, 1).as_slice()),
			Err(Error::MappingTableTooShort)
		));
		let mut short = head(2, 1);
		short.extend_from_slice(&[1, 1, 0]);
		assert!(matches!(
			Config::parse(&mut short.as_slice()),
			Err(Error::MappingTableTooShort)
		));

		for table in [
			// No streams.
			[0, 0, 0, 1],
			// More coupled streams than streams.
			[1, 2, 0, 1],
			// A channel index past the two decoded channels.
			[1, 1, 0, 2],
		] {
			let mut bytes = head(2, 1);
			bytes.extend_from_slice(&table);
			assert!(
				matches!(Config::parse(&mut bytes.as_slice()), Err(Error::InvalidMappingTable)),
				"{table:?}"
			);
		}

		// 255 is a silent channel, not an index.
		let mut silent = head(2, 1);
		silent.extend_from_slice(&[1, 0, 0, 255]);
		assert_eq!(
			Config::parse(&mut silent.as_slice()).unwrap().mapping.unwrap().table(),
			&[0, 255]
		);
	}
}
