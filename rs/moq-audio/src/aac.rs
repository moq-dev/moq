//! AAC constraints, the sibling of the `opus` and `pcm` modules.
//!
//! Only the decode side exists: there is no Rust AAC encoder, so this crate
//! publishes Opus or PCM and reads AAC that a gateway produced.

use crate::Error;

/// The audioObjectType of AAC-LC (ISO 14496-3 Table 1.17), the one profile we decode.
const PROFILE_LC: u8 = 2;

/// Reject what the decoder can't open, by the numbers the config actually carries.
fn validate(profile: u8, channel_count: u32) -> Result<(), Error> {
	if profile != PROFILE_LC {
		return Err(Error::Unsupported(format!(
			"only AAC-LC is supported (got mp4a.40.{profile})"
		)));
	}
	if !matches!(channel_count, 1 | 2) {
		return Err(Error::Unsupported(format!(
			"aac decoding is limited to mono and stereo (got {channel_count} channels)"
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

			// Synthesis is lossy: the encoder masks the object type to the five bits
			// the field has and rewrites a channel count the config table can't name.
			// A rendition we can't decode would come back out of it looking like
			// decodable stereo, so check the catalog's own numbers first.
			validate(profile, catalog.channel_count)?;

			moq_mux::codec::aac::Config {
				profile,
				sample_rate: catalog.sample_rate,
				channel_count: catalog.channel_count,
			}
			.encode()
		}
	};

	let parsed = moq_mux::codec::aac::Config::parse(&mut description.as_ref()).map_err(moq_mux::Error::from)?;
	validate(parsed.profile, parsed.channel_count)?;

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
		// HE-AAC (5), 44100 Hz, stereo. Only this leading-object-type form is
		// caught: an LC-leading config that declares SBR further in reads as LC.
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
