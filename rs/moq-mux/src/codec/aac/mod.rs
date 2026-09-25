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

	#[error("reserved channelConfiguration: {0}")]
	ReservedChannelConfig(u8),

	#[error("channelConfiguration 0 is unsupported for audioObjectType {0}")]
	ProgramConfigUnsupported(u8),

	#[error("channelConfiguration 0 without a program config element leading the first raw data block")]
	ProgramConfigMissing,

	#[error("program config element truncated")]
	ProgramConfigTruncated,

	#[error("program config element declares no channels")]
	ProgramConfigEmpty,
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
	/// (object_type == 31), and explicit sample rates (freq_index == 15). The
	/// fields are bit-packed and not byte-aligned, so a bit reader is required:
	/// with an explicit 24-bit rate the channelConfiguration lands mid-byte after
	/// it. A channelConfiguration of 0 takes the count from the program config element
	/// that follows, and a reserved one is refused. Any SBR/PS extension bits after the
	/// core fields are consumed.
	pub fn parse<T: Buf>(buf: &mut T) -> Result<Self> {
		if buf.remaining() < 2 {
			return Err(Error::ConfigTooShort);
		}

		let mut reader = BitReader::new(buf);
		let object_type = read_object_type(&mut reader)?;

		// samplingFrequencyIndex: 4 bits; index 15 means an explicit 24-bit rate follows.
		let freq_index = reader.read(4, Error::IncompleteConfig)? as u8;
		let sample_rate = if freq_index == 15 {
			reader.read(24, Error::ExplicitSampleRateTooShort)?
		} else {
			*SAMPLE_RATES
				.get(freq_index as usize)
				.ok_or(Error::UnsupportedSampleRateIndex(freq_index))?
		};

		// channelConfiguration: 4 bits, immediately after the (possibly explicit) rate.
		let channel_config = reader.read(4, Error::IncompleteConfig)? as u8;
		let channel_count = match channel_config {
			0 => {
				// Explicit SBR and PS name their core object type after an extension rate; the
				// GASpecificConfig carrying the program config element follows that core type.
				let mut core = object_type;
				if matches!(object_type, 5 | 29) {
					if reader.read(4, Error::IncompleteConfig)? == 15 {
						reader.read(24, Error::IncompleteConfig)?;
					}
					core = read_object_type(&mut reader)?;
					if core == 22 {
						// extensionChannelConfiguration, only for ER BSAC.
						reader.read(4, Error::IncompleteConfig)?;
					}
				}
				if !GENERAL_AUDIO.contains(&core) {
					return Err(Error::ProgramConfigUnsupported(core));
				}

				// GASpecificConfig: frameLengthFlag, dependsOnCoreCoder (then a 14-bit
				// coreCoderDelay), and extensionFlag precede the element.
				reader.read(1, Error::IncompleteConfig)?;
				if reader.read(1, Error::IncompleteConfig)? == 1 {
					reader.read(14, Error::IncompleteConfig)?;
				}
				reader.read(1, Error::IncompleteConfig)?;
				program_config(&mut reader)?
			}
			_ => channel_count_from_config(channel_config)?,
		};

		// AudioSpecificConfig can carry variable-length extensions (SBR, PS, etc.).
		// We've extracted the essential fields; drain the rest so the buffer is advanced.
		if buf.remaining() > 0 {
			buf.advance(buf.remaining());
		}

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

/// Build the AudioSpecificConfig for a stream that signals its fields per frame, as ADTS does.
///
/// A `channel_config` of 0 means a program config element leads `block`, the stream's first raw
/// data block; it moves into the config, so the config describes the channels on its own. Any
/// other value leaves `block` unread. An element placed anywhere else is refused: reaching past
/// the channel data needs a full Huffman decode.
pub(crate) fn in_band_config(profile: u8, sample_rate: u32, channel_config: u8, block: &[u8]) -> Result<Bytes> {
	let mut out = BitWriter::default();
	out.write(5, u32::from(profile & 0x1F));
	match SAMPLE_RATES.iter().position(|&rate| rate == sample_rate) {
		Some(index) => out.write(4, index as u32),
		None => {
			out.write(4, 15);
			out.write(24, sample_rate);
		}
	}
	out.write(4, u32::from(channel_config));

	if channel_config == 0 {
		// GASpecificConfig: frameLengthFlag, dependsOnCoreCoder, and extensionFlag, all clear.
		out.write(3, 0);

		let mut block = block;
		let mut reader = BitReader::new(&mut block);
		if reader.read(3, Error::ProgramConfigMissing)? != ID_PCE {
			return Err(Error::ProgramConfigMissing);
		}
		reader.record = Some(out);
		program_config(&mut reader)?;
		out = reader.record.take().expect("recording set above");
	}

	Ok(Bytes::from(out.bytes))
}

/// The raw data block element ID of a program config element (ISO 14496-3 Table 4.85).
const ID_PCE: u32 = 5;

/// The audioObjectTypes whose specific config is a GASpecificConfig (ISO 14496-3 §1.6.2.1), the
/// only ones where channelConfiguration 0 means a program config element follows.
const GENERAL_AUDIO: [u8; 12] = [1, 2, 3, 4, 6, 7, 17, 19, 20, 21, 22, 23];

/// Read an audioObjectType: 5 bits, escaped to 6 more when it reads 31.
fn read_object_type<T: Buf>(reader: &mut BitReader<T>) -> Result<u8> {
	let object_type = reader.read(5, Error::ConfigTooShort)? as u8;
	if object_type == 31 {
		return Ok(32 + reader.read(6, Error::ExtendedConfigTooShort)? as u8);
	}
	Ok(object_type)
}

/// Walk a program_config_element (ISO 14496-3 Table 4.2) to the number of channels it outputs.
///
/// Its byte alignment is relative to where the reader started: the AudioSpecificConfig, or the
/// raw data block.
fn program_config<T: Buf>(reader: &mut BitReader<T>) -> Result<u32> {
	let mut read = |n| reader.read(n, Error::ProgramConfigTruncated);

	// element_instance_tag, object_type, sampling_frequency_index.
	read(10)?;
	let (front, side, back) = (read(4)?, read(4)?, read(4)?);
	let (lfe, assoc, cc) = (read(2)?, read(3)?, read(4)?);
	// Mono and stereo mixdowns each name an element; a matrix mixdown an index and a flag.
	for bits in [4, 4, 3] {
		if read(1)? == 1 {
			read(bits)?;
		}
	}

	let mut channels = lfe;
	for _ in 0..front + side + back {
		// A channel pair element carries two channels, a single channel element one.
		channels += 1 + read(1)?;
		read(4)?;
	}
	// Tags of the LFE and data elements, then each coupling channel element's switch flag and tag.
	for _ in 0..lfe + assoc {
		read(4)?;
	}
	for _ in 0..cc {
		read(5)?;
	}

	reader.align();
	let comment = reader.read(8, Error::ProgramConfigTruncated)?;
	for _ in 0..comment {
		reader.read(8, Error::ProgramConfigTruncated)?;
	}

	if channels == 0 {
		return Err(Error::ProgramConfigEmpty);
	}
	Ok(channels)
}

/// The 13 standard AAC sampling frequencies, indexed by samplingFrequencyIndex
/// (ISO 14496-3 Table 1.18). Index 15 is the escape for an explicit 24-bit rate.
const SAMPLE_RATES: [u32; 13] = [
	96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
];

/// MSB-first bit reader that pulls bytes from a [`Buf`] on demand.
///
/// AudioSpecificConfig is bit-packed: an explicit 24-bit sample rate pushes the
/// following channelConfiguration off byte boundaries, so the fields can't be
/// read a whole byte at a time.
struct BitReader<'a, T: Buf> {
	buf: &'a mut T,
	current: u8,
	bits_left: u8,
	/// Every bit read is copied here when set, with alignment redone on the writer's own bytes.
	record: Option<BitWriter>,
}

impl<'a, T: Buf> BitReader<'a, T> {
	fn new(buf: &'a mut T) -> Self {
		Self {
			buf,
			current: 0,
			bits_left: 0,
			record: None,
		}
	}

	/// Skip to the next byte boundary.
	fn align(&mut self) {
		self.bits_left = 0;
		if let Some(record) = &mut self.record {
			record.align();
		}
	}

	/// Read `n` bits (n <= 32) MSB-first, returning `short` if the buffer runs dry.
	fn read(&mut self, n: u8, short: Error) -> Result<u32> {
		let mut value = 0u32;
		for _ in 0..n {
			if self.bits_left == 0 {
				if !self.buf.has_remaining() {
					return Err(short);
				}
				self.current = self.buf.get_u8();
				self.bits_left = 8;
			}
			self.bits_left -= 1;
			value = (value << 1) | u32::from((self.current >> self.bits_left) & 1);
		}
		if let Some(record) = &mut self.record {
			record.write(n, value);
		}
		Ok(value)
	}
}

/// MSB-first bit writer, the inverse of [`BitReader`].
#[derive(Default)]
struct BitWriter {
	bytes: Vec<u8>,
	/// Bits used in the last byte; 0 when the next write starts a new one.
	used: u8,
}

impl BitWriter {
	/// Write the low `n` bits (n <= 32) of `value`, MSB-first.
	fn write(&mut self, n: u8, value: u32) {
		for i in (0..n).rev() {
			if self.used == 0 {
				self.bytes.push(0);
			}
			let bit = ((value >> i) & 1) as u8;
			*self.bytes.last_mut().expect("pushed above") |= bit << (7 - self.used);
			self.used = (self.used + 1) % 8;
		}
	}

	/// Pad the last byte with zeros.
	fn align(&mut self) {
		self.used = 0;
	}
}

/// Map an AAC `channel_config` (ISO 14496-3 Table 1.19) to its channel count, for every value but
/// 0, which a program config element describes instead. Configs 1..=6 are identity. 7, 12, and 14
/// are 8 channels (7.1 with wide fronts, rear surrounds, or front heights), 11 is 6.1, and 13 is
/// 22.2. The rest are reserved.
fn channel_count_from_config(channel_config: u8) -> Result<u32> {
	match channel_config {
		1..=6 => Ok(channel_config as u32),
		7 | 12 | 14 => Ok(8),
		11 => Ok(7),
		13 => Ok(24),
		_ => Err(Error::ReservedChannelConfig(channel_config)),
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
		// A non-standard rate (no freq_index) forces the explicit 24-bit form, where
		// channelConfiguration lands mid-byte after the rate. A byte-aligned parser
		// misreads both fields; the bit reader round-trips them.
		let cfg = Config {
			profile: 2,
			sample_rate: 44_056, // not in the standard table
			channel_count: 2,
		};
		let encoded = cfg.encode();
		assert_eq!(encoded.len(), 5, "explicit-rate config is 5 bytes");

		let parsed = Config::parse(&mut encoded.as_ref()).unwrap();
		assert_eq!(parsed.profile, 2);
		assert_eq!(parsed.sample_rate, 44_056);
		assert_eq!(parsed.channel_count, 2);
	}

	#[test]
	fn parses_extended_object_type() {
		// audioObjectType 31 escapes to a 6-bit extended type. Bytes encode
		// AOT=31, ext=4 (-> object_type 36), freq_index=3 (48000), channel_config=2,
		// which straddle byte boundaries: 11111 000100 0011 0010 + padding.
		let buf: [u8; 3] = [0xF8, 0x86, 0x40];
		let cfg = Config::parse(&mut buf.as_slice()).unwrap();
		assert_eq!(cfg.profile, 36);
		assert_eq!(cfg.sample_rate, 48_000);
		assert_eq!(cfg.channel_count, 2);
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

	/// The AudioSpecificConfig ffmpeg 9.0.1 writes for quad (`-af pan=quad|...`, native `aac`
	/// encoder, `.m4a`): channelConfiguration 0 and a program config element with two channel
	/// pair elements, front and back, then a "Lavc63.1.101" comment and an SBR sync extension.
	const FFMPEG_QUAD_ASC: [u8; 24] = [
		0x11, 0x80, 0x04, 0xC4, 0x04, 0x00, 0x21, 0x10, 0x0C, 0x4C, 0x61, 0x76, 0x63, 0x36, 0x33, 0x2E, 0x31, 0x2E,
		0x31, 0x30, 0x31, 0x56, 0xE5, 0x00,
	];

	/// Write a program config element with `front`, `side`, and `back` elements (true for a
	/// channel pair) and `lfe` LFE elements, starting at the writer's position.
	fn write_pce(out: &mut BitWriter, front: &[bool], side: &[bool], back: &[bool], lfe: u32) {
		// element_instance_tag, object_type (LC), sampling_frequency_index (48 kHz).
		out.write(4, 0);
		out.write(2, 1);
		out.write(4, 3);
		out.write(4, front.len() as u32);
		out.write(4, side.len() as u32);
		out.write(4, back.len() as u32);
		out.write(2, lfe);
		// No data or coupling elements, and no mixdowns.
		out.write(3, 0);
		out.write(4, 0);
		out.write(3, 0);
		for (tag, &cpe) in front.iter().chain(side).chain(back).enumerate() {
			out.write(1, cpe.into());
			out.write(4, tag as u32);
		}
		for tag in 0..lfe {
			out.write(4, tag);
		}
		out.align();
		// A one-byte comment.
		out.write(8, 1);
		out.write(8, b'x'.into());
	}

	/// An AudioSpecificConfig with channelConfiguration 0 around [`write_pce`]'s element.
	fn pce_asc(object_type: u8, front: &[bool], side: &[bool], back: &[bool], lfe: u32) -> Vec<u8> {
		let mut out = BitWriter::default();
		out.write(5, object_type.into());
		out.write(4, 3);
		out.write(4, 0);
		out.write(3, 0);
		write_pce(&mut out, front, side, back, lfe);
		out.bytes
	}

	#[test]
	fn parses_ffmpeg_program_config_element() {
		let cfg = Config::parse(&mut FFMPEG_QUAD_ASC.as_slice()).unwrap();
		assert_eq!(cfg.profile, 2);
		assert_eq!(cfg.sample_rate, 48_000);
		assert_eq!(cfg.channel_count, 4, "two channel pair elements");
	}

	#[test]
	fn parses_program_config_element_counts() {
		// 5.1: a single and a pair in front, a pair in back, and an LFE.
		let asc = pce_asc(2, &[false, true], &[], &[true], 1);
		assert_eq!(Config::parse(&mut asc.as_slice()).unwrap().channel_count, 6);

		// Side elements count too: 3/2/2 with no LFE.
		let asc = pce_asc(2, &[false, true], &[true], &[true], 0);
		assert_eq!(Config::parse(&mut asc.as_slice()).unwrap().channel_count, 7);
	}

	#[test]
	fn parses_program_config_element_behind_explicit_sbr() {
		// audioObjectType 5 (SBR), 24 kHz core, channelConfiguration 0, a 48 kHz extension
		// rate, then the core type (LC) whose GASpecificConfig carries the element.
		let mut out = BitWriter::default();
		out.write(5, 5);
		out.write(4, 6);
		out.write(4, 0);
		out.write(4, 3);
		out.write(5, 2);
		out.write(3, 0);
		write_pce(&mut out, &[true], &[], &[true], 0);
		let cfg = Config::parse(&mut out.bytes.as_slice()).unwrap();
		assert_eq!(cfg.profile, 5);
		assert_eq!(cfg.channel_count, 4);
	}

	#[test]
	fn refuses_bad_program_config_elements() {
		let asc = pce_asc(2, &[], &[], &[], 0);
		assert!(matches!(
			Config::parse(&mut asc.as_slice()),
			Err(Error::ProgramConfigEmpty)
		));

		let asc = pce_asc(2, &[true], &[], &[], 0);
		assert!(matches!(
			Config::parse(&mut &asc[..asc.len() - 1]),
			Err(Error::ProgramConfigTruncated)
		));

		// ALS (36) describes its channels in its own specific config, not a PCE.
		let mut out = BitWriter::default();
		out.write(5, 31);
		out.write(6, 4);
		out.write(4, 3);
		out.write(4, 0);
		out.write(8, 0);
		assert!(matches!(
			Config::parse(&mut out.bytes.as_slice()),
			Err(Error::ProgramConfigUnsupported(36))
		));
	}

	#[test]
	fn channel_config_dispositions() {
		let expected = [1, 2, 3, 4, 5, 6, 8];
		for (config, count) in (1..=7).zip(expected) {
			let asc = [0x11, 0x80 | (config << 3)];
			assert_eq!(Config::parse(&mut asc.as_slice()).unwrap().channel_count, count);
		}

		for (config, count) in [(11, 7), (12, 8), (13, 24), (14, 8)] {
			let asc = [0x11, 0x80 | (config << 3)];
			assert_eq!(Config::parse(&mut asc.as_slice()).unwrap().channel_count, count);
		}

		for config in [8, 9, 10, 15] {
			let asc = [0x11, 0x80 | (config << 3)];
			assert!(matches!(
				Config::parse(&mut asc.as_slice()),
				Err(Error::ReservedChannelConfig(c)) if c == config
			));
		}
	}

	#[test]
	fn in_band_config_moves_the_program_config_element() {
		// A raw data block leading with ID_PCE, which puts the element 3 bits off the byte grid
		// the AudioSpecificConfig puts it on, so the copy has to redo the alignment.
		let mut block = BitWriter::default();
		block.write(3, ID_PCE);
		write_pce(&mut block, &[false, true], &[], &[true], 1);
		// The channel elements that follow are never read.
		block.write(8, 0xFF);

		let asc = in_band_config(2, 48_000, 0, &block.bytes).unwrap();
		assert_eq!(asc, pce_asc(2, &[false, true], &[], &[true], 1));
		assert_eq!(Config::parse(&mut asc.as_ref()).unwrap().channel_count, 6);
	}

	#[test]
	fn in_band_config_refuses_a_block_without_a_leading_pce() {
		// ID_CPE first: any element past the channel data is out of reach.
		assert!(matches!(
			in_band_config(2, 48_000, 0, &[0x20, 0x00]),
			Err(Error::ProgramConfigMissing)
		));
		assert!(matches!(
			in_band_config(2, 48_000, 0, &[]),
			Err(Error::ProgramConfigMissing)
		));
	}

	#[test]
	fn in_band_config_matches_encode() {
		// Without a PCE the block is unread and the config is the plain two-byte form.
		let asc = in_band_config(2, 44_100, 2, &[]).unwrap();
		let encoded = Config {
			profile: 2,
			sample_rate: 44_100,
			channel_count: 2,
		}
		.encode();
		assert_eq!(asc, encoded);
	}

	#[test]
	fn unsupported_channel_count_falls_back_to_stereo_config() {
		assert_eq!(channel_config_from_count(9), 2);
	}
}
