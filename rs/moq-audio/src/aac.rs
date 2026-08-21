//! AAC constraints, the sibling of the `opus` and `pcm` modules.
//!
//! Only the decode side exists: there is no Rust AAC encoder, so this crate
//! publishes Opus or PCM and reads AAC that a gateway produced.

use crate::Error;

/// The audioObjectType of AAC-LC (ISO 14496-3 Table 1.17), the one profile we decode.
const PROFILE_LC: u8 = 2;

/// The audioObjectTypes that mean HE-AAC: SBR alone, and SBR plus parametric stereo.
const OBJECT_TYPE_SBR: u8 = 5;
const OBJECT_TYPE_PS: u8 = 29;

/// The 11-bit pattern introducing an AudioSpecificConfig extension (ISO 14496-3).
const SYNC_EXTENSION: u32 = 0x2B7;

/// The widest sample rate an AudioSpecificConfig can name: the escape from the
/// frequency table is a 24-bit field.
const MAX_SAMPLE_RATE: u32 = 0xFF_FFFF;

/// Reject what the decoder can't open, by the numbers a config actually carries.
fn validate(config: &moq_mux::codec::aac::Config) -> Result<(), Error> {
	if config.profile != PROFILE_LC {
		return Err(Error::Unsupported(format!(
			"only AAC-LC is supported (got mp4a.40.{})",
			config.profile
		)));
	}
	if !matches!(config.channel_count, 1 | 2) {
		return Err(Error::Unsupported(format!(
			"aac decoding is limited to mono and stereo (got {} channels)",
			config.channel_count
		)));
	}
	if config.sample_rate > MAX_SAMPLE_RATE {
		return Err(Error::Unsupported(format!(
			"aac sample rate must fit 24 bits (got {})",
			config.sample_rate
		)));
	}
	Ok(())
}

/// The AudioSpecificConfig to open a decoder with, validated as AAC-LC.
///
/// Prefers the catalog `description` verbatim, since re-encoding the parsed
/// fields would drop any extension it carries. A catalog without one (an MSF
/// track, or a producer that only filled in the plain fields) gets one
/// synthesized from the declared profile, rate, and channel count.
///
/// The profile is read back out of the resulting config rather than taken from
/// the catalog codec string: the description is what the decoder acts on, and
/// the two disagree when a gateway copies through a codec string it never parsed.
pub(crate) fn description(catalog: &hang::catalog::AudioConfig, profile: u8) -> Result<bytes::Bytes, Error> {
	let description = match &catalog.description {
		Some(description) => {
			// A single byte can't hold even the object type and rate index. Symphonia
			// rejects it too, but as an opaque "invalid data" from the decode path.
			if description.len() < 2 {
				return Err(Error::Unsupported(format!(
					"aac description must be at least 2 bytes (got {})",
					description.len()
				)));
			}
			description.clone()
		}
		None => {
			if catalog.sample_rate == 0 || catalog.channel_count == 0 {
				return Err(Error::Unsupported(
					"aac catalog without a description must declare a sample rate and channel count".into(),
				));
			}

			let config = moq_mux::codec::aac::Config {
				profile,
				sample_rate: catalog.sample_rate,
				channel_count: catalog.channel_count,
			};

			// Synthesis is lossy in every field: the encoder masks the object type to
			// the five bits it has, rewrites a channel count the config table can't
			// name, and drops the sample rate's bits past 24. A rendition we can't
			// decode would come back out of it looking like decodable stereo, so
			// check the catalog's own numbers before encoding them.
			validate(&config)?;

			config.encode()
		}
	};

	let parsed = moq_mux::codec::aac::Config::parse(&mut description.as_ref()).map_err(moq_mux::Error::from)?;
	validate(&parsed)?;

	// HE-AAC has two spellings. `validate` catches the one that leads with SBR or
	// PS; this catches the backward-compatible one, which leads with LC so that an
	// LC-only decoder can play the core and hides the SBR in a sync extension
	// after it. Symphonia reads that extension only when the leading object type
	// is already SBR or PS, so without this the same stream would be rejected or
	// quietly half-decoded depending on how its encoder chose to signal it.
	if declares_sbr(&description) {
		return Err(Error::Unsupported(
			"only AAC-LC is supported (the config declares SBR after the core)".into(),
		));
	}

	Ok(description)
}

/// Whether an AudioSpecificConfig carries a sync extension declaring SBR or PS.
///
/// Anything it can't walk reads as no: the config is then whatever the leading
/// object type said it was, which is what the rest of this module already acts on.
fn declares_sbr(description: &[u8]) -> bool {
	fn scan(bits: &mut Bits) -> Option<bool> {
		// Only an LC-leading config gets this far, the others being rejected above.
		if object_type(bits)? != PROFILE_LC {
			return Some(false);
		}

		if bits.read(4)? == 15 {
			// samplingFrequencyIndex 15 escapes to an explicit rate.
			bits.read(24)?;
		}

		// channelConfiguration 0 means a program config element follows, whose
		// length this doesn't walk. Symphonia rejects that config anyway.
		if bits.read(4)? == 0 {
			return Some(false);
		}

		// GASpecificConfig, in the shape AAC-LC gives it: frameLengthFlag, then a
		// core coder delay only when one is declared, then an extension flag that
		// carries a single further bit for this object type.
		bits.read(1)?;
		if bits.read(1)? == 1 {
			bits.read(14)?;
		}
		if bits.read(1)? == 1 {
			bits.read(1)?;
		}

		// The sync extension itself. Padding can't be mistaken for it: the pattern
		// has to match, name SBR or PS, and then set the flag.
		if bits.left() < 16 || bits.read(11)? != SYNC_EXTENSION {
			return Some(false);
		}
		if !matches!(object_type(bits)?, OBJECT_TYPE_SBR | OBJECT_TYPE_PS) {
			return Some(false);
		}

		Some(bits.read(1)? == 1)
	}

	scan(&mut Bits::new(description)).unwrap_or(false)
}

/// An audioObjectType, which escapes to a second field once it runs out of room.
fn object_type(bits: &mut Bits) -> Option<u8> {
	match bits.read(5)? {
		31 => Some(32 + bits.read(6)? as u8),
		value => Some(value as u8),
	}
}

/// A big-endian bit cursor, which is how an AudioSpecificConfig is packed.
struct Bits<'a> {
	data: &'a [u8],
	pos: usize,
}

impl<'a> Bits<'a> {
	fn new(data: &'a [u8]) -> Self {
		Self { data, pos: 0 }
	}

	fn left(&self) -> usize {
		self.data.len() * 8 - self.pos
	}

	/// The next `count` bits, or `None` when the config is shorter than that.
	fn read(&mut self, count: usize) -> Option<u32> {
		if count > 32 || self.left() < count {
			return None;
		}

		let mut value = 0;
		for _ in 0..count {
			let bit = (self.data[self.pos / 8] >> (7 - self.pos % 8)) & 1;
			value = (value << 1) | u32::from(bit);
			self.pos += 1;
		}

		Some(value)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn catalog(profile: u8, sample_rate: u32, channels: u32) -> hang::catalog::AudioConfig {
		hang::catalog::AudioConfig::new(hang::catalog::AAC { profile }, sample_rate, channels)
	}

	#[test]
	fn description_is_kept_verbatim() {
		let mut config = catalog(2, 44_100, 1);
		// AAC-LC with an explicit 24-bit rate: re-encoding the parsed fields would
		// produce the 2-byte form instead.
		config.description = Some(bytes::Bytes::from_static(&[0x17, 0x80, 0x56, 0x22, 0x08]));

		let description = description(&config, 2).unwrap();
		assert_eq!(description.as_ref(), &[0x17, 0x80, 0x56, 0x22, 0x08]);
	}

	#[test]
	fn description_is_synthesized_from_catalog_fields() {
		// AAC-LC (2), 44100 Hz (index 4), mono (config 1).
		let description = description(&catalog(2, 44_100, 1), 2).unwrap();
		assert_eq!(description.as_ref(), &[0x12, 0x08]);
	}

	#[test]
	fn rejects_he_aac() {
		// HE-AAC (5), 44100 Hz, stereo: the form that says so up front. The
		// backward-compatible spelling is caught by `declares_sbr` instead.
		let err = description(&catalog(5, 44_100, 2), 5).unwrap_err();
		assert!(matches!(err, Error::Unsupported(msg) if msg.contains("mp4a.40.5")));
	}

	#[test]
	fn rejects_an_extended_profile_before_synthesis_masks_it() {
		// mp4a.40.34 is MP3-in-MP4. The 5-bit object type field can't hold 34, so
		// synthesizing a config would mask it down to 2 and read back as AAC-LC.
		let err = description(&catalog(34, 44_100, 2), 34).unwrap_err();
		assert!(matches!(err, Error::Unsupported(msg) if msg.contains("mp4a.40.34")));
	}

	#[test]
	fn rejects_more_than_stereo() {
		// Synthesis would rewrite an unnameable count to stereo; 6 is nameable but
		// still past what the decoder opens.
		assert!(matches!(
			description(&catalog(2, 48_000, 12), 2),
			Err(Error::Unsupported(_))
		));
		assert!(matches!(
			description(&catalog(2, 48_000, 6), 2),
			Err(Error::Unsupported(_))
		));
	}

	#[test]
	fn rejects_a_sample_rate_the_config_cannot_hold() {
		// Past 24 bits the encoder keeps the low bits and drops the rest, so this
		// would synthesize a plausible 44100 Hz config.
		let err = description(&catalog(2, 0x0100_AC44, 2), 2).unwrap_err();
		assert!(matches!(err, Error::Unsupported(msg) if msg.contains("24 bits")));
	}

	#[test]
	fn rejects_backward_compatible_he_aac() {
		// AAC-LC 44100 stereo, then a sync extension naming SBR with the flag set:
		// the signaling an encoder uses so an LC-only decoder can still play the
		// core. Leading object type 2, so only the extension gives it away.
		let mut config = catalog(2, 44_100, 2);
		config.description = Some(bytes::Bytes::from_static(&[0x12, 0x10, 0x56, 0xE5, 0x98]));

		let err = description(&config, 2).unwrap_err();
		assert!(matches!(err, Error::Unsupported(msg) if msg.contains("SBR")));
	}

	#[test]
	fn accepts_a_sync_extension_that_declares_no_sbr() {
		// The same config with sbrPresentFlag clear: an extension is present, but it
		// says there is no SBR, so this really is plain AAC-LC.
		let mut config = catalog(2, 44_100, 2);
		config.description = Some(bytes::Bytes::from_static(&[0x12, 0x10, 0x56, 0xE5, 0x18]));

		assert!(description(&config, 2).is_ok());
	}

	#[test]
	fn rejects_description_disagreeing_with_the_codec_string() {
		// The catalog claims LC while the description says HE-AAC; the description wins.
		let mut config = catalog(2, 44_100, 2);
		config.description = Some(bytes::Bytes::from_static(&[0x2A, 0x10]));

		assert!(matches!(description(&config, 2), Err(Error::Unsupported(_))));
	}

	#[test]
	fn rejects_truncated_description() {
		let mut config = catalog(2, 44_100, 1);
		config.description = Some(bytes::Bytes::from_static(&[0x12]));

		assert!(matches!(description(&config, 2), Err(Error::Unsupported(_))));
	}

	#[test]
	fn rejects_missing_shape() {
		assert!(matches!(description(&catalog(2, 0, 1), 2), Err(Error::Unsupported(_))));
		assert!(matches!(
			description(&catalog(2, 44_100, 0), 2),
			Err(Error::Unsupported(_))
		));
	}
}
