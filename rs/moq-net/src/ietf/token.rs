//! The `AUTHORIZATION TOKEN` Setup Option (draft-ietf-moq-transport-21 section 9.1.4) and
//! Message Parameter.
//!
//! The value is the Token structure of section 8.9: an Alias Type, then fields that
//! type selects. We advertise no `MAX_AUTH_TOKEN_CACHE_SIZE`, so its default of 0 means
//! no alias is ever registered and every token arrives by value.

use crate::{
	DecodeError, Error, SessionError,
	coding::{Decoder, EncodeError, Encoder},
	setup::Token,
};

use super::{Param, ParameterBytes, Parameters, Version};

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
		.map(|value| decode(value, version, true).map_err(Error::Session))
		.transpose()
}

/// Present `token` in our SETUP, by value.
#[cfg_attr(not(test), expect(dead_code))]
pub fn into_setup(params: &mut Parameters, token: &Token, version: Version) -> Result<(), EncodeError> {
	params.set_bytes(ParameterBytes::AuthorizationToken, encode(token, version)?);
	Ok(())
}

/// An `AUTHORIZATION TOKEN` parameter on a request message, decoded by the SETUP option's
/// rules except that a registration overflows the cache instead of falling back to a value.
///
/// The parameter may repeat, so a request decodes it as a `Vec` and every instance is checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestToken(pub Token);

impl Param for RequestToken {
	fn param_encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		w.bytes(&encode(&self.0, version)?)
	}

	fn param_decode(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		let value = r.bytes()?;
		Ok(Self(decode(value, version, false).map_err(DecodeError::Session)?))
	}
}

/// Encode `token` by value.
fn encode(token: &Token, version: Version) -> Result<Vec<u8>, EncodeError> {
	let mut value = Vec::new();
	let mut w = Encoder::new(&mut value, version.into());
	w.varint(USE_VALUE)?;
	w.varint(token.kind)?;
	w.slice(&token.value);
	Ok(value)
}

/// Decode a Token structure, refusing what this endpoint cannot accept.
fn decode(buf: &[u8], version: Version, setup: bool) -> Result<Token, SessionError> {
	// Section 8.9: a structure that cannot be decoded closes with KEY_VALUE_FORMATTING_ERROR.
	let malformed = |_| SessionError::KeyValueFormatting;
	let mut r = Decoder::new(buf, version.into());

	match r.varint().map_err(malformed)? {
		USE_VALUE => {}
		// With no cache, section 9.1.4 treats a registration in SETUP as a value; the alias is
		// unused.
		REGISTER if setup => {
			r.varint().map_err(malformed)?;
		}
		// Section 8.9: a registration past the cache size of 0 closes the session.
		REGISTER => return Err(SessionError::AuthTokenCacheOverflow),
		// Section 9.1.3: a cache size of 0 prohibits aliases, so none was ever registered.
		// Section 8.9 rejects the message, but 0x17 is only a session code (section 12.2), so
		// the session closes; doc/concept/standard.md records the deviation.
		DELETE | USE_ALIAS => return Err(SessionError::UnknownAuthTokenAlias),
		_ => return Err(SessionError::KeyValueFormatting),
	}

	let kind = r.varint().map_err(malformed)?;
	Ok(Token {
		kind,
		value: r.rest().to_vec(),
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::coding::{Decode, Encode};

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
		let bytes = params.encode_bytes(version).unwrap();
		Parameters::decode_slice(&bytes, version).unwrap().0
	}

	/// A raw Token structure: the alias type then its varint fields then a value.
	fn structure(version: Version, fields: &[u64], value: &[u8]) -> Parameters {
		let mut raw = Vec::new();
		let mut w = Encoder::new(&mut raw, version.into());
		for field in fields {
			w.varint(*field).unwrap();
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
			Encoder::new(&mut value, version.into()).varint(USE_VALUE).unwrap();
			Encoder::new(&mut value, version.into())
				.varint(Token::OUT_OF_BAND)
				.unwrap();

			let key = u64::from(ParameterBytes::AuthorizationToken);
			let (count, keys): (Option<u64>, [u64; 2]) = match version {
				Version::Draft14 | Version::Draft15 => (Some(2), [key, key]),
				// Delta-encoded from draft-16, so the repeat is a delta of zero.
				Version::Draft16 => (Some(2), [key, 0]),
				_ => (None, [key, 0]),
			};
			let mut raw = Vec::new();
			if let Some(count) = count {
				Encoder::new(&mut raw, version.into()).varint(count).unwrap();
			}
			for key in keys {
				Encoder::new(&mut raw, version.into()).varint(key).unwrap();
				Encoder::new(&mut raw, version.into()).bytes(&value).unwrap();
			}

			let err = Parameters::decode_slice(&raw, version).unwrap_err();
			assert!(matches!(err, crate::DecodeError::Duplicate), "{version:?}: {err:?}");
		}
	}

	/// A cache size of 0 means no alias was ever registered.
	#[test]
	fn an_alias_reference_is_an_unknown_alias() {
		for version in VERSIONS {
			for alias_type in [DELETE, USE_ALIAS] {
				let params = structure(version, &[alias_type, 7], &[]);
				let err = from_setup(&params, version).unwrap_err();
				assert!(
					matches!(err, Error::Session(SessionError::UnknownAuthTokenAlias)),
					"{version:?} {alias_type}: {err:?}"
				);
			}
		}
	}

	/// A request's token as a message parameter value: the length-prefixed structure.
	fn request(version: Version, fields: &[u64], value: &[u8]) -> Result<RequestToken, DecodeError> {
		let mut raw = Vec::new();
		let mut w = Encoder::new(&mut raw, version.into());
		for field in fields {
			w.varint(*field).unwrap();
		}
		w.slice(value);
		let mut param = Vec::new();
		Encoder::new(&mut param, version.into()).bytes(&raw).unwrap();

		let mut r = Decoder::new(&param, version.into());
		let token = RequestToken::param_decode(&mut r, version)?;
		assert!(r.is_empty(), "{version:?}");
		Ok(token)
	}

	#[test]
	fn a_request_token_round_trips_on_every_draft() {
		for version in VERSIONS {
			let mut param = Vec::new();
			RequestToken(token())
				.param_encode(&mut Encoder::new(&mut param, version.into()), version)
				.unwrap();
			let mut r = Decoder::new(&param, version.into());
			assert_eq!(
				RequestToken::param_decode(&mut r, version).unwrap(),
				RequestToken(token()),
				"{version:?}"
			);
		}
	}

	/// Only SETUP falls back to a value: on a request, a registration overflows the cache.
	#[test]
	fn a_request_registration_overflows_the_cache() {
		for version in VERSIONS {
			let err = request(version, &[REGISTER, 7, token().kind], &token().value).unwrap_err();
			assert!(
				matches!(err, DecodeError::Session(SessionError::AuthTokenCacheOverflow)),
				"{version:?}: {err:?}"
			);
		}
	}

	#[test]
	fn a_request_alias_reference_is_an_unknown_alias() {
		for version in VERSIONS {
			for alias_type in [DELETE, USE_ALIAS] {
				let err = request(version, &[alias_type, 7], &[]).unwrap_err();
				assert!(
					matches!(err, DecodeError::Session(SessionError::UnknownAuthTokenAlias)),
					"{version:?} {alias_type}: {err:?}"
				);
			}
		}
	}

	#[test]
	fn an_undecodable_request_token_is_a_formatting_error() {
		for version in VERSIONS {
			for (fields, why) in [
				(&[][..], "no alias type"),
				(&[USE_VALUE][..], "no token type"),
				(&[0x4, 0][..], "an unknown alias type"),
			] {
				let err = request(version, fields, &[]).unwrap_err();
				assert!(
					matches!(err, DecodeError::Session(SessionError::KeyValueFormatting)),
					"{version:?} {why}: {err:?}"
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
