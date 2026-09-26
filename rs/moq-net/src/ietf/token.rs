//! The `AUTHORIZATION TOKEN` Setup Option (draft-ietf-moq-transport-21 section 9.1.4).
//!
//! The value is the Token structure of section 8.9: an Alias Type, then fields that
//! type selects. We advertise no `MAX_AUTH_TOKEN_CACHE_SIZE`, so its default of 0 means
//! no alias is ever registered and every token arrives by value.

use crate::{
	Error, SessionError,
	coding::{Decode, Encode, EncodeError},
	setup::Token,
};

use super::{ParameterBytes, Parameters, Version};

/// Retire a registered alias.
const DELETE: u64 = 0x0;
/// Register an alias for this type and value, then use them.
const REGISTER: u64 = 0x1;
/// Use the type and value a registered alias names.
const USE_ALIAS: u64 = 0x2;
/// Use the type and value carried inline.
const USE_VALUE: u64 = 0x3;

/// The token the peer's SETUP presented, if any.
///
/// A second token is already refused as a duplicate option by [`Parameters`]: one
/// credential per connection.
pub fn from_setup(params: &Parameters, version: Version) -> Result<Option<Token>, Error> {
	params
		.get_bytes(ParameterBytes::AuthorizationToken)
		.map(|value| decode(value, version))
		.transpose()
}

/// Present `token` in our SETUP, by value.
#[cfg_attr(not(test), expect(dead_code))]
pub fn into_setup(params: &mut Parameters, token: &Token, version: Version) -> Result<(), EncodeError> {
	let mut value = Vec::new();
	USE_VALUE.encode(&mut value, version)?;
	token.kind.encode(&mut value, version)?;
	value.extend_from_slice(&token.value);
	params.set_bytes(ParameterBytes::AuthorizationToken, value);
	Ok(())
}

/// Decode a Token structure, refusing what a SETUP cannot carry.
fn decode(mut buf: &[u8], version: Version) -> Result<Token, Error> {
	// Section 8.9: a structure that cannot be decoded closes with KEY_VALUE_FORMATTING_ERROR.
	let malformed = |_| Error::Session(SessionError::KeyValueFormatting);

	match u64::decode(&mut buf, version).map_err(malformed)? {
		USE_VALUE => {}
		// With no cache, section 9.1.4 treats a registration as a value; the alias is unused.
		REGISTER => {
			u64::decode(&mut buf, version).map_err(malformed)?;
		}
		// Section 9.1.4: nothing can have been registered before SETUP.
		DELETE | USE_ALIAS => return Err(Error::ProtocolViolation),
		_ => return Err(Error::Session(SessionError::KeyValueFormatting)),
	}

	let kind = u64::decode(&mut buf, version).map_err(malformed)?;
	Ok(Token {
		kind,
		value: buf.to_vec(),
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	const VERSIONS: [Version; 9] = [
		Version::Draft14,
		Version::Draft15,
		Version::Draft16,
		Version::Draft17,
		Version::Draft18,
		Version::Draft19,
		Version::Draft20,
		Version::Draft21,
		Version::Draft22,
	];

	fn token() -> Token {
		// A kind past one varint byte and a value that is not text, so neither is mistaken
		// for the other and no codec can get away with assuming UTF-8.
		Token {
			kind: 300,
			value: vec![0x00, 0xff, 0x03, 0x80, b'j'],
		}
	}

	/// The option as it arrives, after a trip through the SETUP parameter block.
	fn received(params: &Parameters, version: Version) -> Parameters {
		let mut bytes = params.encode_bytes(version).unwrap();
		Parameters::decode(&mut bytes, version).unwrap()
	}

	/// A raw Token structure: the alias type then its varint fields then a value.
	fn structure(version: Version, fields: &[u64], value: &[u8]) -> Parameters {
		let mut raw = Vec::new();
		for field in fields {
			field.encode(&mut raw, version).unwrap();
		}
		raw.extend_from_slice(value);
		let mut params = Parameters::default();
		params.set_bytes(ParameterBytes::AuthorizationToken, raw);
		received(&params, version)
	}

	#[test]
	fn use_value_round_trips_on_every_draft() {
		for version in VERSIONS {
			let mut params = Parameters::default();
			into_setup(&mut params, &token(), version).unwrap();
			let params = received(&params, version);
			assert_eq!(from_setup(&params, version).unwrap(), Some(token()), "{version:?}");
		}
	}

	/// The same bytes `js/net/src/ietf/token.test.ts` asserts, so the two agree on the wire.
	#[test]
	fn the_encoding_matches_the_cross_language_vector() {
		let token = Token {
			kind: 300,
			value: vec![0x00, 0xff],
		};
		for (version, expected) in [
			(Version::Draft14, [0x03, 0x41, 0x2c, 0x00, 0xff]),
			(Version::Draft17, [0x03, 0x81, 0x2c, 0x00, 0xff]),
		] {
			let mut params = Parameters::default();
			into_setup(&mut params, &token, version).unwrap();
			assert_eq!(
				params.get_bytes(ParameterBytes::AuthorizationToken),
				Some(&expected[..]),
				"{version:?}"
			);
		}
	}

	#[test]
	fn an_empty_value_is_a_token() {
		for version in VERSIONS {
			let params = structure(version, &[USE_VALUE, Token::OUT_OF_BAND], &[]);
			let expected = Token {
				kind: Token::OUT_OF_BAND,
				value: Vec::new(),
			};
			assert_eq!(from_setup(&params, version).unwrap(), Some(expected), "{version:?}");
		}
	}

	#[test]
	fn absent_is_none() {
		for version in VERSIONS {
			assert_eq!(from_setup(&Parameters::default(), version).unwrap(), None);
		}
	}

	/// We advertise no cache, so a registration is the draft's own USE_VALUE.
	#[test]
	fn register_is_a_value() {
		for version in VERSIONS {
			let params = structure(version, &[REGISTER, 7, token().kind], &token().value);
			assert_eq!(from_setup(&params, version).unwrap(), Some(token()), "{version:?}");
		}
	}

	/// One credential per connection: a second token is refused, not unioned or dropped.
	#[test]
	fn two_tokens_are_refused() {
		for version in VERSIONS {
			let mut value = Vec::new();
			USE_VALUE.encode(&mut value, version).unwrap();
			Token::OUT_OF_BAND.encode(&mut value, version).unwrap();

			let key = u64::from(ParameterBytes::AuthorizationToken);
			let (count, keys): (Option<u64>, [u64; 2]) = match version {
				Version::Draft14 | Version::Draft15 => (Some(2), [key, key]),
				// Delta-encoded from draft-16, so the repeat is a delta of zero.
				Version::Draft16 => (Some(2), [key, 0]),
				_ => (None, [key, 0]),
			};
			let mut raw = Vec::new();
			if let Some(count) = count {
				count.encode(&mut raw, version).unwrap();
			}
			for key in keys {
				key.encode(&mut raw, version).unwrap();
				value.encode(&mut raw, version).unwrap();
			}

			let err = Parameters::decode(&mut raw.as_slice(), version).unwrap_err();
			assert!(matches!(err, crate::DecodeError::Duplicate), "{version:?}: {err:?}");
		}
	}

	#[test]
	fn an_alias_reference_is_a_protocol_violation() {
		for version in VERSIONS {
			for alias_type in [DELETE, USE_ALIAS] {
				let params = structure(version, &[alias_type, 7], &[]);
				let err = from_setup(&params, version).unwrap_err();
				assert!(
					matches!(err, Error::ProtocolViolation),
					"{version:?} {alias_type}: {err:?}"
				);
			}
		}
	}

	#[test]
	fn an_undecodable_structure_is_a_formatting_error() {
		for version in VERSIONS {
			for (fields, why) in [
				(&[][..], "no alias type"),
				(&[USE_VALUE][..], "no token type"),
				(&[REGISTER][..], "no alias"),
				(&[REGISTER, 7][..], "no token type after the alias"),
				(&[0x4, 0][..], "an unknown alias type"),
			] {
				let params = structure(version, fields, &[]);
				let err = from_setup(&params, version).unwrap_err();
				assert!(
					matches!(err, Error::Session(SessionError::KeyValueFormatting)),
					"{version:?} {why}: {err:?}"
				);
			}
		}
	}
}
