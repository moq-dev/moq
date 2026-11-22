//! AAC constraints, the sibling of the `opus` and `pcm` modules.
//!
//! The decode side. There is no Rust AAC encoder, so `encode` produces AAC only
//! through a platform backend and otherwise reads AAC that a gateway produced.

use crate::Error;

/// The audioObjectType of AAC-LC (ISO 14496-3 Table 1.17), the one profile we decode.
const PROFILE_LC: u8 = 2;

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
/// fields would drop any extension it carries. Without one, the decoder's config is
/// synthesized from the declared profile, rate, and channels. Legacy and LOC packets
/// then carry ADTS headers that must agree with it; CMAF samples remain raw.
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

			let config = moq_mux::codec::aac::Config::new(profile, catalog.sample_rate, catalog.channel_count);

			// Only AAC-LC decodes, and synthesis still truncates the sample rate to 24
			// bits. Encode refuses a channel count that does not pick a
			// channelConfiguration, but check the catalog first so the error stays the
			// decoder's.
			validate(&config)?;

			config.encode().map_err(moq_mux::Error::from)?
		}
	};

	let parsed = moq_mux::codec::aac::Config::parse(&mut description.as_ref())
		.map_err(|err| Error::Unsupported(format!("invalid AAC description: {err}")))?;
	// HE-AAC has two spellings: one leads with SBR or PS, and the backward-compatible one
	// leads with LC and hides SBR in a sync extension after the core. The parse reports both
	// as profile 5 or 29, so `validate` refuses either. Symphonia reads that extension only
	// when the leading object type is already SBR or PS, so a stream accepted here would be
	// quietly half-decoded.
	validate(&parsed)?;

	Ok(description)
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
		// backward-compatible spelling parses as profile 5 too.
		let err = description(&catalog(5, 44_100, 2), 5).unwrap_err();
		assert!(matches!(err, Error::Unsupported(msg) if msg.contains("mp4a.40.5")));
	}

	#[test]
	fn rejects_an_extended_profile_before_synthesis() {
		// mp4a.40.34 is MP3-in-MP4, which this decoder can't play: validate refuses
		// any profile but AAC-LC before a config is synthesized.
		let err = description(&catalog(34, 44_100, 2), 34).unwrap_err();
		assert!(matches!(err, Error::Unsupported(msg) if msg.contains("mp4a.40.34")));
	}

	#[test]
	fn rejects_more_than_stereo() {
		// 12 has no channelConfiguration; 6 does, but the decoder only opens mono and stereo.
		assert!(matches!(
			description(&catalog(2, 48_000, 12), 2),
			Err(Error::Unsupported(msg)) if msg.contains("limited to mono and stereo")
		));
		assert!(matches!(
			description(&catalog(2, 48_000, 6), 2),
			Err(Error::Unsupported(msg)) if msg.contains("limited to mono and stereo")
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
		assert!(matches!(err, Error::Unsupported(msg) if msg.contains("mp4a.40.5")));
	}

	#[test]
	fn rejects_sbr_followed_by_an_unknown_field() {
		// The backward-compatible SBR signaling, then padding where a PS sync extension could
		// follow: the SBR still counts.
		let mut config = catalog(2, 44_100, 2);
		config.description = Some(bytes::Bytes::from_static(&[0x12, 0x10, 0x56, 0xE5, 0x98, 0x00, 0x00]));

		let err = description(&config, 2).unwrap_err();
		assert!(matches!(err, Error::Unsupported(msg) if msg.contains("mp4a.40.5")));
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
		let mut config = catalog(2, 48_000, 2);
		config.description = Some(bytes::Bytes::from_static(&[0x2B, 0x11, 0x88, 0x00]));

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
