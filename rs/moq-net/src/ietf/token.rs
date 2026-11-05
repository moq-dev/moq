//! The `AUTHORIZATION TOKEN` Setup Option (draft-ietf-moq-transport-22 section 9.1.4) and
//! Message Parameter.
//!
//! The value is the Token structure of section 8.9: an Alias Type, then fields that
//! type selects. We advertise no `MAX_AUTH_TOKEN_CACHE_SIZE`, so its default of 0 means
//! no alias is ever registered and every token arrives by value.

#[cfg(feature = "async")]
use crate::coding::Stream;
#[cfg(feature = "async")]
use crate::driver::Transport;

use crate::{
	DecodeError, Error, SessionError,
	coding::{Decoder, EncodeError, Encoder},
	setup::Token,
};

use super::{Param, ParameterBytes, Parameters, Version, error::request};

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
pub fn into_setup(params: &mut Parameters, token: &Token, version: Version) -> Result<(), EncodeError> {
	params.set_bytes(ParameterBytes::AuthorizationToken, encode(token, version)?);
	Ok(())
}

/// An `AUTHORIZATION TOKEN` parameter on a request message, decoded by the SETUP option's
/// rules except that a registration overflows the cache instead of falling back to a value.
///
/// The parameter may repeat, so a request decodes it as a `Vec` and every instance is checked.
#[derive(Clone, PartialEq, Eq)]
pub struct RequestToken(pub Token);

// A request message is logged with `Debug`, so the credential shows only its type and length.
impl std::fmt::Debug for RequestToken {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "RequestToken(kind={}, <{} bytes>)", self.0.kind, self.0.value.len())
	}
}

impl Param for RequestToken {
	fn param_encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		w.bytes(&encode(&self.0, version)?)
	}

	fn param_decode(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		let value = r.bytes()?;
		Ok(Self(decode(value, version, false).map_err(DecodeError::Session)?))
	}
}

/// The one token a request carries, if any.
///
/// Section 8.9 lets a token repeat only with a different type or value, after resolving
/// aliases, of which we accept none, so a repeat is [`DecodeError::MalformedAuthToken`].
/// Distinct tokens are legal, but authorizing on one would silently ignore the rest, so more
/// than one is refused as [`DecodeError::MultipleAuthTokens`]. Checked once every parameter
/// decoded, so a token that closes the session wins over either.
pub fn single(tokens: Vec<RequestToken>) -> Result<Option<RequestToken>, DecodeError> {
	for (i, token) in tokens.iter().enumerate() {
		if tokens[..i].contains(token) {
			return Err(DecodeError::MalformedAuthToken);
		}
	}
	let mut tokens = tokens.into_iter();
	match (tokens.next(), tokens.next()) {
		(token, None) => Ok(token),
		_ => Err(DecodeError::MultipleAuthTokens),
	}
}

/// Whether a decode error refuses only the request whose tokens caused it, leaving the
/// session alone: a repeat (section 8.9), or more than one token.
pub(super) fn refuses_request(err: &DecodeError) -> bool {
	matches!(err, DecodeError::MalformedAuthToken | DecodeError::MultipleAuthTokens)
}

/// The reason phrase for a refusal [`refuses_request`] accepts.
pub(super) fn reason(err: &DecodeError) -> &'static str {
	match err {
		DecodeError::MultipleAuthTokens => "more than one AUTHORIZATION TOKEN",
		_ => "repeated AUTHORIZATION TOKEN",
	}
}

/// Refuse a request whose tokens decoded to `err`, one [`refuses_request`] accepts, ending
/// its stream.
///
/// Section 8.9 rejects the message rather than the session.
#[cfg(feature = "async")]
pub(super) async fn reject(
	mut stream: Stream<Transport, Version>,
	kind: request::Kind,
	err: DecodeError,
	version: Version,
) -> Result<(), Error> {
	let reason = reason(&err);
	super::request_stream::buffer_error(&mut stream.writer, kind, &Error::Decode(err), reason, version)?;
	super::request_stream::deliver(&mut stream.writer).await
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

	// Every field an alias type names decodes before its meaning applies, so a truncated
	// structure is malformed whatever its type.
	match r.varint().map_err(malformed)? {
		USE_VALUE => {}
		REGISTER => {
			r.varint().map_err(malformed)?;
			// Section 8.9: a registration past the cache size of 0 closes the session. With no
			// cache, section 9.1.4 treats one in SETUP as a value and the alias is unused.
			if !setup {
				r.varint().map_err(malformed)?;
				return Err(SessionError::AuthTokenCacheOverflow);
			}
		}
		DELETE | USE_ALIAS => {
			r.varint().map_err(malformed)?;
			if !r.is_empty() {
				return Err(SessionError::KeyValueFormatting);
			}
			// Section 9.1.4: nothing can have been registered before SETUP.
			if setup {
				return Err(SessionError::ProtocolViolation);
			}
			// Section 9.1.3: a cache size of 0 prohibits aliases, so none was ever registered.
			// Section 8.9 rejects the message, but 0x17 is only a session code (section 12.2),
			// so the session closes; doc/concept/standard.md records the deviation.
			return Err(SessionError::UnknownAuthTokenAlias);
		}
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

	/// Section 8.9: tokens may repeat only with a different type or value, which is malformed
	/// over any other refusal. Distinct tokens are legal but refused: a request carries one.
	#[test]
	fn a_request_carries_at_most_one_token() {
		let token = |kind, value: &[u8]| {
			RequestToken(Token {
				kind,
				value: value.to_vec(),
			})
		};
		assert_eq!(single(Vec::new()).unwrap(), None);
		assert_eq!(single(vec![token(0, b"a")]).unwrap(), Some(token(0, b"a")));
		assert!(matches!(
			single(vec![token(0, b"a"), token(1, b"a"), token(0, b"b")]),
			Err(DecodeError::MultipleAuthTokens)
		));
		assert!(matches!(
			single(vec![token(0, b"a"), token(1, b"a"), token(0, b"a")]),
			Err(DecodeError::MalformedAuthToken)
		));
	}

	const VERSIONS: [Version; 3] = [Version::Draft18, Version::Draft20, Version::Draft22];

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
		let (version, expected) = (Version::Draft18, [0x03, 0x81, 0x2c, 0x00, 0xff]);
		let mut params = Parameters::default();
		into_setup(&mut params, &token, version).unwrap();
		assert_eq!(
			params.get_bytes(ParameterBytes::AuthorizationToken),
			Some(&expected[..]),
			"{version:?}"
		);
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

	/// A request's credential shows only its type and length in a message's `Debug`.
	#[test]
	fn a_request_token_debug_hides_the_value() {
		let debug = format!(
			"{:?}",
			RequestToken(Token {
				kind: 0,
				value: b"s3cr3t-jwt".to_vec(),
			})
		);
		assert!(!debug.contains("s3cr3t"), "{debug}");
		assert!(!debug.contains("115"), "{debug}");
		assert!(debug.contains("<10 bytes>"), "{debug}");
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
			let mut raw = Vec::new();
			// Delta-encoded, so the repeat is a delta of zero.
			for key in [key, 0] {
				Encoder::new(&mut raw, version.into()).varint(key).unwrap();
				Encoder::new(&mut raw, version.into()).bytes(&value).unwrap();
			}

			let err = Parameters::decode_slice(&raw, version).unwrap_err();
			assert!(matches!(err, crate::DecodeError::Duplicate), "{version:?}: {err:?}");
		}
	}

	/// Section 9.1.4: nothing can have been registered before SETUP, so an alias there is a
	/// protocol violation.
	#[test]
	fn a_setup_alias_reference_is_a_protocol_violation() {
		for version in VERSIONS {
			for alias_type in [DELETE, USE_ALIAS] {
				let params = structure(version, &[alias_type, 7], &[]);
				let err = from_setup(&params, version).unwrap_err();
				assert!(
					matches!(err, Error::Session(SessionError::ProtocolViolation)),
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
				(&[REGISTER][..], "a registration with no alias"),
				(&[REGISTER, 7][..], "a registration with no token type"),
				(&[DELETE][..], "a deletion with no alias"),
				(&[USE_ALIAS][..], "an alias use with no alias"),
				(&[USE_ALIAS, 7, 0][..], "an alias use with a token type"),
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
				(&[DELETE][..], "a deletion with no alias"),
				(&[USE_ALIAS][..], "an alias use with no alias"),
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
