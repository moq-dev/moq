use num_enum::{FromPrimitive, IntoPrimitive};

use crate::coding::*;

use super::Location;
use super::Version;

const MAX_PARAMS: u64 = 64;
/// Maximum byte value length in Key-Value-Pairs per spec Section 1.4.3.
const MAX_KVP_VALUE_LEN: usize = (1 << 16) - 1;

// ---- Setup Parameters (used in CLIENT_SETUP/SERVER_SETUP) ----

#[derive(Debug, Copy, Clone, FromPrimitive, IntoPrimitive, Eq, Hash, PartialEq)]
#[repr(u64)]
pub enum ParameterVarInt {
	/// Removed in draft-17; only used in draft-14/15/16.
	MaxRequestId = 2,
	MaxAuthTokenCacheSize = 4,
	/// HOP_ID, from the MoQ Cluster extension.
	HopId = super::cluster::HOP_ID,
	/// RELAY_COST, from the MoQ Cluster extension.
	RelayCost = super::cluster::RELAY_COST,
	/// SOLICIT, from the MoQ Solicit extension.
	Solicit = super::solicit::SOLICIT,
	/// HIDDEN, from the MoQ Hidden extension.
	Hidden = super::hidden::HIDDEN,
	/// ACTIVE_COUNT, from the MoQ Active Count extension.
	ActiveCount = super::active_count::ACTIVE_COUNT,
	#[num_enum(catch_all)]
	Unknown(u64),
}

#[derive(Debug, Copy, Clone, FromPrimitive, IntoPrimitive, Eq, Hash, PartialEq)]
#[repr(u64)]
pub enum ParameterBytes {
	Path = 1,
	AuthorizationToken = 3,
	Authority = 5,
	Implementation = 7,
	#[num_enum(catch_all)]
	Unknown(u64),
}

/// SETUP parameters, in the order they were set or decoded.
///
/// A handful at most, so a linear scan beats hashing, and the encoding is deterministic.
#[derive(Default, Debug, Clone)]
pub struct Parameters {
	vars: Vec<(ParameterVarInt, u64)>,
	bytes: Vec<(ParameterBytes, Vec<u8>)>,
}

impl Decode<Version> for Parameters {
	fn decode(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		let mut params = Self::default();

		// Draft-14/15/16 count the pairs; draft-17+ reads them until the buffer is empty.
		let count = match version {
			Version::Draft14 | Version::Draft15 | Version::Draft16 => Some(r.varint()?),
			_ => None,
		};
		if count.is_some_and(|count| count > MAX_PARAMS) {
			return Err(DecodeError::TooMany);
		}

		// Draft-16+ delta-encodes the types; even is a varint value, odd is length-prefixed bytes.
		let delta = !matches!(version, Version::Draft14 | Version::Draft15);
		let mut prev = 0u64;
		let mut i = 0u64;

		while count.map_or(!r.is_empty(), |count| i < count) {
			if i >= MAX_PARAMS {
				return Err(DecodeError::TooMany);
			}

			let kind = r.varint()?;
			let kind = match delta && i > 0 {
				true => prev.checked_add(kind).ok_or(DecodeError::BoundsExceeded)?,
				false => kind,
			};
			prev = kind;
			i += 1;

			// Unknown SETUP options may repeat, including GREASE; their values still
			// have to be well-formed Key-Value-Pairs (draft-21 section 9.1).
			if kind % 2 == 0 {
				let kind = ParameterVarInt::from(kind);
				if !matches!(kind, ParameterVarInt::Unknown(_)) && params.get_varint(kind).is_some() {
					return Err(DecodeError::Duplicate);
				}
				params.vars.push((kind, r.varint()?));
			} else {
				let kind = ParameterBytes::from(kind);
				let value = r.bytes()?;
				if value.len() > MAX_KVP_VALUE_LEN {
					return Err(DecodeError::BoundsExceeded);
				}
				if !matches!(kind, ParameterBytes::Unknown(_)) && params.get_bytes(kind).is_some() {
					return Err(DecodeError::Duplicate);
				}
				params.bytes.push((kind, value.to_vec()));
			}
		}

		Ok(params)
	}
}

impl Encode<Version> for Parameters {
	fn encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		let count = self.vars.len() + self.bytes.len();
		if count as u64 > MAX_PARAMS {
			return Err(EncodeError::TooMany);
		}
		if self.bytes.iter().any(|(_, value)| value.len() > MAX_KVP_VALUE_LEN) {
			return Err(EncodeError::BoundsExceeded);
		}

		match version {
			Version::Draft14 | Version::Draft15 => {
				w.varint(count as u64)?;

				for (kind, value) in &self.vars {
					w.varint(u64::from(*kind))?;
					w.varint(*value)?;
				}

				for (kind, value) in &self.bytes {
					w.varint(u64::from(*kind))?;
					w.bytes(value)?;
				}
			}
			_ => {
				// Draft16: count prefix + delta encoding
				// Draft17+: NO count prefix + delta encoding
				if matches!(version, Version::Draft16) {
					w.varint(count as u64)?;
				}

				enum ParamRef<'a> {
					Var(u64),
					Bytes(&'a [u8]),
				}
				let mut all: Vec<(u64, ParamRef)> = Vec::with_capacity(count);
				all.extend(self.vars.iter().map(|(k, v)| (u64::from(*k), ParamRef::Var(*v))));
				all.extend(self.bytes.iter().map(|(k, v)| (u64::from(*k), ParamRef::Bytes(v))));
				all.sort_by_key(|(k, _)| *k);

				let mut prev = 0u64;
				for (kind, value) in all {
					w.varint(kind - prev)?;
					prev = kind;

					match value {
						ParamRef::Var(v) => w.varint(v)?,
						ParamRef::Bytes(v) => w.bytes(v)?,
					}
				}
			}
		}

		Ok(())
	}
}

impl Parameters {
	/// Consume a draft-14 message parameter block, returning its first MAX_CACHE_DURATION
	/// (0x04), the one parameter we act on.
	///
	/// Draft-14 section 9.2 has a receiver ignore unrecognized parameters and allow their
	/// duplicates, and lets AUTHORIZATION TOKEN repeat, so unlike [`Parameters::decode`],
	/// which SETUP uses, a repeat is not refused.
	pub fn skip(r: &mut Decoder<'_>) -> Result<Option<u64>, DecodeError> {
		let count = r.varint()?;
		if count > MAX_PARAMS {
			return Err(DecodeError::TooMany);
		}

		let mut cache_duration = None;
		for _ in 0..count {
			// Parity frames a Key-Value-Pair: even is one varint, odd is length prefixed.
			let kind = r.varint()?;
			match kind % 2 {
				0 => {
					let value = r.varint()?;
					if kind == 0x04 {
						cache_duration.get_or_insert(value);
					}
				}
				_ => {
					let len = usize::try_from(r.varint()?).map_err(|_| DecodeError::BoundsExceeded)?;
					if len > MAX_KVP_VALUE_LEN {
						return Err(DecodeError::BoundsExceeded);
					}
					r.slice(len)?;
				}
			}
		}

		Ok(cache_duration)
	}

	pub fn get_varint(&self, kind: ParameterVarInt) -> Option<u64> {
		self.vars.iter().find(|(k, _)| *k == kind).map(|(_, v)| *v)
	}

	pub fn set_varint(&mut self, kind: ParameterVarInt, value: u64) {
		match self.vars.iter_mut().find(|(k, _)| *k == kind) {
			Some((_, v)) => *v = value,
			None => self.vars.push((kind, value)),
		}
	}

	pub fn get_bytes(&self, kind: ParameterBytes) -> Option<&[u8]> {
		self.bytes.iter().find(|(k, _)| *k == kind).map(|(_, v)| v.as_slice())
	}

	pub fn set_bytes(&mut self, kind: ParameterBytes, value: Vec<u8>) {
		match self.bytes.iter_mut().find(|(k, _)| *k == kind) {
			Some((_, v)) => *v = value,
			None => self.bytes.push((kind, value)),
		}
	}
}

// ---- Message Parameter Value Encoding ----

/// Trait for encoding/decoding parameter values with version-specific formats.
///
/// Parameter encoding differs from field encoding:
/// - Draft-14/15/16: u8 and bool are encoded as varints (cast to u64)
/// - Draft-17: type-specific encoding (u8 as raw byte, bool as raw byte, etc.)
///
/// Use `_ =>` for the newest draft behavior so future versions default forward.
pub trait Param: Sized {
	fn param_encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError>;
	fn param_decode(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError>;

	/// Whether this parameter should be encoded. Returns false to skip.
	fn param_present(&self) -> bool {
		true
	}

	/// Fold a repeat of this parameter into the value already decoded.
	///
	/// A parameter may appear once unless its definition says otherwise, so the default
	/// refuses the repeat.
	fn param_repeat(self, _next: Self) -> Result<Self, DecodeError> {
		Err(DecodeError::Duplicate)
	}
}

impl Param for u8 {
	fn param_encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		match version {
			// Draft-14/15/16: u8 encoded as varint
			Version::Draft14 | Version::Draft15 | Version::Draft16 => w.varint(u64::from(*self))?,
			_ => w.u8(*self),
		}
		Ok(())
	}

	fn param_decode(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Draft14 | Version::Draft15 | Version::Draft16 => {
				u8::try_from(r.varint()?).map_err(|_| DecodeError::InvalidValue)
			}
			_ => r.u8(),
		}
	}
}

impl Param for bool {
	fn param_encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		match version {
			// Draft-14/15/16: bool encoded as varint
			Version::Draft14 | Version::Draft15 | Version::Draft16 => w.varint(u8::from(*self).into())?,
			_ => w.bool(*self),
		}
		Ok(())
	}

	fn param_decode(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Draft14 | Version::Draft15 | Version::Draft16 => match r.varint()? {
				0 => Ok(false),
				1 => Ok(true),
				_ => Err(DecodeError::InvalidValue),
			},
			_ => r.bool(),
		}
	}
}

impl Param for u64 {
	fn param_encode(&self, w: &mut Encoder<'_>, _: Version) -> Result<(), EncodeError> {
		w.varint(*self)?;
		Ok(())
	}

	fn param_decode(r: &mut Decoder<'_>, _: Version) -> Result<Self, DecodeError> {
		r.varint()
	}
}

/// A Location parameter value, such as LARGEST_OBJECT (0x09).
///
/// Draft-16 section 9.2 serializes every Message Parameter as a Key-Value-Pair, and section
/// 9.2.2.7 calls LARGEST_OBJECT "a length-prefixed Location structure", so the two varints
/// sit inside a byte string there. Draft-17 section 9.3, and section 10.2 from draft-18 on,
/// redefines a Message Parameter as `{ Type Delta (vi64), Value (..) }` with no Length at
/// all, and lists "Location: Two consecutive varints (Group, Object)" as its own value
/// encoding beside Length-prefixed. So from draft-17 the two varints are written bare.
impl Param for Location {
	fn param_encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Draft14 | Version::Draft15 | Version::Draft16 => {
				// The drafts before 17 pin the inner varints to the draft-15 encoding,
				// matching the other length-prefixed parameters.
				let mut buf = Vec::new();
				let mut inner = Encoder::new(&mut buf, Version::Draft15.into());
				inner.varint(self.group)?;
				inner.varint(self.object)?;
				w.bytes(&buf)
			}
			_ => {
				w.varint(self.group)?;
				w.varint(self.object)?;
				Ok(())
			}
		}
	}

	fn param_decode(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Draft14 | Version::Draft15 | Version::Draft16 => {
				let mut inner = Decoder::new(r.bytes()?, Version::Draft15.into());
				let group = inner.varint()?;
				let object = inner.varint()?;
				if !inner.is_empty() {
					return Err(DecodeError::TrailingBytes);
				}
				Ok(Location { group, object })
			}
			_ => {
				let group = r.varint()?;
				let object = r.varint()?;
				Ok(Location { group, object })
			}
		}
	}
}

impl<T: Param> Param for Option<T> {
	fn param_present(&self) -> bool {
		self.is_some()
	}

	fn param_repeat(self, next: Self) -> Result<Self, DecodeError> {
		match (self, next) {
			(Some(prev), Some(next)) => Ok(Some(prev.param_repeat(next)?)),
			_ => Err(DecodeError::Duplicate),
		}
	}

	fn param_encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		match self {
			Some(v) => v.param_encode(w, version),
			None => Ok(()),
		}
	}

	fn param_decode(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		Ok(Some(T::param_decode(r, version)?))
	}
}

/// A parameter whose definition lets it repeat, such as AUTHORIZATION TOKEN (0x03) or a
/// Range Filter (0x25-0x29). Every instance is kept, in wire order.
impl<T: Param> Param for Vec<T> {
	fn param_present(&self) -> bool {
		!self.is_empty()
	}

	fn param_encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		// `encode_params!` writes the key once, so only a single instance fits behind it.
		match self.as_slice() {
			[value] => value.param_encode(w, version),
			_ => Err(EncodeError::Unsupported),
		}
	}

	fn param_decode(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		Ok(vec![T::param_decode(r, version)?])
	}

	fn param_repeat(mut self, next: Self) -> Result<Self, DecodeError> {
		self.extend(next);
		Ok(self)
	}
}

/// A length-prefixed parameter value, consumed without being interpreted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Opaque(pub Vec<u8>);

impl Param for Opaque {
	fn param_encode(&self, w: &mut Encoder<'_>, _: Version) -> Result<(), EncodeError> {
		w.bytes(&self.0)
	}

	fn param_decode(r: &mut Decoder<'_>, _: Version) -> Result<Self, DecodeError> {
		Ok(Self(r.bytes()?.to_vec()))
	}
}

/// Encode message parameters with compile-time sorted keys.
///
/// Keys must be listed in ascending order (enforced at compile time).
/// `Option<T>` values are skipped when `None`.
///
/// ```ignore
/// encode_params!(w, version,
///     0x10 => self.forward,
///     0x20 => self.subscriber_priority,
/// );
/// ```
macro_rules! encode_params {
	($w:expr, $version:expr, $($key:expr => $val:expr),* $(,)?) => {{
		#[allow(unused)]
		const _: () = {
			let _keys: &[u64] = &[$($key),*];
			let mut _i = 1;
			while _i < _keys.len() {
				assert!(_keys[_i - 1] < _keys[_i], "parameter keys must be in ascending order");
				_i += 1;
			}
		};

		let _version: $crate::ietf::Version = $version;

		#[allow(unused_mut)]
		let mut _count: usize = 0;
		$(_count += if $crate::ietf::Param::param_present(&$val) { 1 } else { 0 };)*
		$w.varint(_count as u64)?;

		#[allow(unused_mut, unused_assignments)]
		let mut _prev_key: u64 = 0;
		#[allow(unused_mut, unused_assignments)]
		let mut _first: bool = true;
		$(
			if $crate::ietf::Param::param_present(&$val) {
				let _key: u64 = $key;
				let _wire = match _version {
					$crate::ietf::Version::Draft14 | $crate::ietf::Version::Draft15 => _key,
					_ if _first => _key,
					_ => _key - _prev_key,
				};
				$w.varint(_wire)?;
				_prev_key = _key;
				_first = false;
				$crate::ietf::Param::param_encode(&$val, $w, _version)?;
			}
		)*
	}};
}

/// Decode message parameters with compile-time sorted keys.
///
/// The declared type is the final type of each variable. Use `Option<T>` for
/// optional parameters (defaults to `None` when absent) and bare types like `u8`
/// for parameters where `T::default()` is an acceptable fallback.
///
/// Unknown parameters cause `DecodeError::InvalidValue`.
/// Duplicate parameters cause `DecodeError::Duplicate`, unless the type allows a repeat
/// (see [`Param::param_repeat`]), such as `Vec<T>`.
///
/// ```ignore
/// decode_params!(r, version,
///     0x10 => forward: Option<bool>,
///     0x20 => subscriber_priority: Option<u8>,
/// );
/// // forward: Option<bool> and subscriber_priority: Option<u8> are now in scope
/// let subscriber_priority = subscriber_priority.unwrap_or(128);
/// ```
macro_rules! decode_params {
	($r:expr, $version:expr, $($key:expr => $name:ident: $ty:ty),* $(,)?) => {
		#[allow(unused)]
		const _: () = {
			let _keys: &[u64] = &[$($key),*];
			let mut _i = 1;
			while _i < _keys.len() {
				assert!(_keys[_i - 1] < _keys[_i], "parameter keys must be in ascending order");
				_i += 1;
			}
		};

		// Use internal Option wrapper for duplicate detection, then shadow with Default.
		$(#[allow(unused_mut, non_snake_case)] let mut $name: Option<$ty> = None;)*

		{
			let _version: $crate::ietf::Version = $version;
			let _count = $r.varint()?;
			if _count > 64 {
				return Err($crate::coding::DecodeError::TooMany);
			}

			#[allow(unused_mut, unused_assignments)]
			let mut _prev_key: u64 = 0;
			for _i in 0.._count {
				let _wire = $r.varint()?;
				let _key: u64 = match _version {
					$crate::ietf::Version::Draft14 | $crate::ietf::Version::Draft15 => _wire,
					_ if _i == 0 => _wire,
					_ => _prev_key.checked_add(_wire).ok_or($crate::coding::DecodeError::BoundsExceeded)?,
				};
				_prev_key = _key;

				// An if-chain rather than a `match`, so a key can be a named constant:
				// the macro captures it as an expression, which is not a legal pattern.
				$(
					if _key == $key {
						let _value = <$ty as $crate::ietf::Param>::param_decode($r, _version)?;
						$name = Some(match $name.take() {
							None => _value,
							Some(_prev) => $crate::ietf::Param::param_repeat(_prev, _value)?,
						});
						continue;
					}
				)*
				return Err($crate::coding::DecodeError::InvalidValue);
			}
		}

		// Shadow with unwrap_or_default: Option<T> defaults to None, T defaults to T::default()
		$(#[allow(unused_variables)] let $name: $ty = $name.unwrap_or_default();)*
	};
}

#[cfg(test)]
mod tests {
	use super::super::Filter;
	use super::*;

	#[test]
	fn setup_allows_repeated_unknown_options() {
		for version in [
			Version::Draft14,
			Version::Draft15,
			Version::Draft16,
			Version::Draft17,
			Version::Draft18,
			Version::Draft19,
			Version::Draft20,
			Version::Draft21,
			Version::Draft22,
		] {
			for kind in [0x20, 0x21, 0x9d, 0x11c] {
				let mut buf = Vec::new();
				let mut w = Encoder::new(&mut buf, version.into());
				if matches!(version, Version::Draft14 | Version::Draft15 | Version::Draft16) {
					w.varint(2).unwrap();
				}
				w.varint(kind).unwrap();
				for delta in [None, Some(0)] {
					if let Some(delta) = delta {
						w.varint(if matches!(version, Version::Draft14 | Version::Draft15) {
							kind
						} else {
							delta
						})
						.unwrap();
					}
					if kind % 2 == 0 {
						w.varint(1).unwrap();
					} else {
						w.bytes(b"unknown").unwrap();
					}
				}
				Parameters::decode_slice(&buf, version).expect("unknown SETUP options may repeat");
			}
		}
	}

	#[test]
	fn setup_rejects_repeated_known_options() {
		for version in [Version::Draft18, Version::Draft21, Version::Draft22] {
			for bytes in [&[4, 1, 0, 2][..], &[7, 1, b'a', 0, 1, b'b'][..]] {
				assert!(matches!(
					Parameters::decode_slice(bytes, version),
					Err(DecodeError::Duplicate)
				));
			}
		}
	}

	#[test]
	fn setup_repeated_unknown_options_still_require_complete_values() {
		for version in [Version::Draft18, Version::Draft21, Version::Draft22] {
			assert!(Parameters::decode_slice(&[0x21, 1, b'a', 0, 2, b'b'], version).is_err());
		}
	}

	#[test]
	fn group_order_parameter_rejects_values_outside_one_and_two() {
		for version in [
			Version::Draft17,
			Version::Draft18,
			Version::Draft19,
			Version::Draft20,
			Version::Draft21,
			Version::Draft22,
		] {
			for value in [0u8, 3, 255] {
				let bytes = [value];
				let mut r = Decoder::new(&bytes, version.into());
				assert!(matches!(
					super::super::GroupOrder::param_decode(&mut r, version),
					Err(DecodeError::InvalidValue)
				));
			}
			for (value, expected) in [
				(1u8, super::super::GroupOrder::Ascending),
				(2, super::super::GroupOrder::Descending),
			] {
				let bytes = [value];
				let mut r = Decoder::new(&bytes, version.into());
				assert_eq!(
					super::super::GroupOrder::param_decode(&mut r, version).unwrap(),
					expected
				);
			}
		}
	}

	#[test]
	fn legacy_group_order_zero_keeps_the_publisher_preference() {
		for version in [Version::Draft14, Version::Draft15, Version::Draft16] {
			let mut r = Decoder::new(&[0], version.into());
			assert_eq!(
				super::super::GroupOrder::param_decode(&mut r, version).unwrap(),
				super::super::GroupOrder::Descending
			);
		}
	}

	// ---- Setup Parameters tests (unchanged) ----

	#[test]
	fn test_parameters_v16_delta_round_trip() {
		let mut params = Parameters::default();
		params.set_bytes(ParameterBytes::Path, b"/test".to_vec());
		params.set_varint(ParameterVarInt::MaxRequestId, 100);
		params.set_bytes(ParameterBytes::Implementation, b"test-impl".to_vec());

		let mut buf = Vec::new();
		params
			.encode(&mut Encoder::new(&mut buf, Version::Draft16.into()), Version::Draft16)
			.unwrap();

		let mut bytes = bytes::Bytes::from(buf);
		let decoded = crate::coding::decode_buf(&mut bytes, Version::Draft16, Parameters::decode).unwrap();

		assert_eq!(decoded.get_bytes(ParameterBytes::Path), Some(b"/test".as_ref()));
		assert_eq!(decoded.get_varint(ParameterVarInt::MaxRequestId), Some(100));
		assert_eq!(
			decoded.get_bytes(ParameterBytes::Implementation),
			Some(b"test-impl".as_ref())
		);
	}

	#[test]
	fn test_parameters_v15_round_trip() {
		let mut params = Parameters::default();
		params.set_bytes(ParameterBytes::Path, b"/test".to_vec());
		params.set_varint(ParameterVarInt::MaxRequestId, 100);

		let mut buf = Vec::new();
		params
			.encode(&mut Encoder::new(&mut buf, Version::Draft15.into()), Version::Draft15)
			.unwrap();

		let mut bytes = bytes::Bytes::from(buf);
		let decoded = crate::coding::decode_buf(&mut bytes, Version::Draft15, Parameters::decode).unwrap();

		assert_eq!(decoded.get_bytes(ParameterBytes::Path), Some(b"/test".as_ref()));
		assert_eq!(decoded.get_varint(ParameterVarInt::MaxRequestId), Some(100));
	}

	#[test]
	fn test_parameters_v17_round_trip() {
		let mut params = Parameters::default();
		params.set_bytes(ParameterBytes::Path, b"/test".to_vec());
		params.set_varint(ParameterVarInt::MaxAuthTokenCacheSize, 4096);
		params.set_bytes(ParameterBytes::Implementation, b"test-impl".to_vec());

		let mut buf = Vec::new();
		params
			.encode(&mut Encoder::new(&mut buf, Version::Draft17.into()), Version::Draft17)
			.unwrap();

		let mut bytes = bytes::Bytes::from(buf);
		let decoded = crate::coding::decode_buf(&mut bytes, Version::Draft17, Parameters::decode).unwrap();

		assert_eq!(decoded.get_bytes(ParameterBytes::Path), Some(b"/test".as_ref()));
		assert_eq!(decoded.get_varint(ParameterVarInt::MaxAuthTokenCacheSize), Some(4096));
		assert_eq!(
			decoded.get_bytes(ParameterBytes::Implementation),
			Some(b"test-impl".as_ref())
		);
		assert!(bytes.is_empty());
	}

	#[test]
	fn test_parameters_v17_no_count_prefix() {
		let mut params = Parameters::default();
		params.set_bytes(ParameterBytes::Path, b"/x".to_vec());

		let mut buf15 = Vec::new();
		params
			.encode(&mut Encoder::new(&mut buf15, Version::Draft15.into()), Version::Draft15)
			.unwrap();

		let mut buf17 = Vec::new();
		params
			.encode(&mut Encoder::new(&mut buf17, Version::Draft17.into()), Version::Draft17)
			.unwrap();

		assert!(buf17.len() < buf15.len());
	}

	// ---- Message Parameter (encode_params!/decode_params!) tests ----

	fn round_trip_params(
		version: Version,
		encode_fn: impl FnOnce(&mut Encoder<'_>, Version) -> Result<(), EncodeError>,
		decode_fn: impl FnOnce(&mut Decoder<'_>, Version) -> Result<(), DecodeError>,
	) {
		let mut buf = Vec::new();
		encode_fn(&mut Encoder::new(&mut buf, version.into()), version).unwrap();
		let mut r = Decoder::new(&buf, version.into());
		decode_fn(&mut r, version).unwrap();
		assert!(r.is_empty(), "buffer not fully consumed for {version}");
	}

	#[test]
	fn test_param_u8_all_versions() {
		for version in [
			Version::Draft14,
			Version::Draft15,
			Version::Draft16,
			Version::Draft17,
			Version::Draft18,
		] {
			round_trip_params(
				version,
				|w, v| {
					encode_params!(w, v, 0x20 => 200u8);
					Ok(())
				},
				|r, v| {
					decode_params!(r, v, 0x20 => val: Option<u8>);
					assert_eq!(val, Some(200));
					Ok(())
				},
			);
		}
	}

	#[test]
	fn test_param_u8_wire_vectors() -> Result<(), EncodeError> {
		for (version, expected) in [
			(Version::Draft16, &[0x01, 0x20, 0x40, 0xff][..]),
			(Version::Draft17, &[0x01, 0x20, 0xff][..]),
			(Version::Draft18, &[0x01, 0x20, 0xff][..]),
			(Version::Draft19, &[0x01, 0x20, 0xff][..]),
		] {
			let mut buf = Vec::new();
			encode_params!(&mut Encoder::new(&mut buf, version.into()), version, 0x20 => u8::MAX);
			assert_eq!(&buf[..], expected, "{version}");

			let mut encoded = Decoder::new(expected, version.into());
			let decoded = (|| -> Result<Option<u8>, DecodeError> {
				decode_params!(&mut encoded, version, 0x20 => value: Option<u8>);
				Ok(value)
			})()
			.expect("fixed uint8 vector should decode");
			assert_eq!(decoded, Some(u8::MAX), "{version}");
			assert!(encoded.is_empty(), "{version}");
		}
		Ok(())
	}

	#[test]
	fn test_param_bool_all_versions() {
		for version in [
			Version::Draft14,
			Version::Draft15,
			Version::Draft16,
			Version::Draft17,
			Version::Draft18,
		] {
			round_trip_params(
				version,
				|w, v| {
					encode_params!(w, v, 0x10 => true);
					Ok(())
				},
				|r, v| {
					decode_params!(r, v, 0x10 => val: Option<bool>);
					assert_eq!(val, Some(true));
					Ok(())
				},
			);
		}
	}

	#[test]
	fn test_param_location_all_versions() {
		let loc = Location { group: 5, object: 3 };
		for version in [
			Version::Draft14,
			Version::Draft15,
			Version::Draft16,
			Version::Draft17,
			Version::Draft18,
		] {
			round_trip_params(
				version,
				|w, v| {
					encode_params!(w, v, 0x09 => loc.clone());
					Ok(())
				},
				|r, v| {
					decode_params!(r, v, 0x09 => val: Option<Location>);
					assert_eq!(val, Some(Location { group: 5, object: 3 }));
					Ok(())
				},
			);
		}
	}

	#[test]
	fn test_param_location_wire_vectors() -> Result<(), EncodeError> {
		let location = Location {
			group: 255,
			object: 128,
		};
		// Draft-16 wraps the two varints in a Key-Value-Pair Length; from draft-17 a
		// Message Parameter has no Length and a Location value is two bare varints. The
		// inner format differs too: QUIC-style before draft-17, leading-ones after.
		for (version, expected) in [
			(Version::Draft16, &[0x01, 0x09, 0x04, 0x40, 0xff, 0x40, 0x80][..]),
			(Version::Draft17, &[0x01, 0x09, 0x80, 0xff, 0x80, 0x80][..]),
			(Version::Draft18, &[0x01, 0x09, 0x80, 0xff, 0x80, 0x80][..]),
			(Version::Draft19, &[0x01, 0x09, 0x80, 0xff, 0x80, 0x80][..]),
			(Version::Draft20, &[0x01, 0x09, 0x80, 0xff, 0x80, 0x80][..]),
		] {
			let mut buf = Vec::new();
			encode_params!(&mut Encoder::new(&mut buf, version.into()), version, 0x09 => location.clone());
			assert_eq!(&buf[..], expected, "{version}");

			let mut encoded = Decoder::new(expected, version.into());
			let decoded = (|| -> Result<Option<Location>, DecodeError> {
				decode_params!(&mut encoded, version, 0x09 => value: Option<Location>);
				Ok(value)
			})()
			.expect("fixed Location vector should decode");
			assert_eq!(decoded, Some(location), "{version}");
			assert!(encoded.is_empty(), "{version}");
		}
		Ok(())
	}

	#[test]
	fn test_param_filter_all_versions() {
		for version in [
			Version::Draft14,
			Version::Draft15,
			Version::Draft16,
			Version::Draft17,
			Version::Draft18,
		] {
			round_trip_params(
				version,
				|w, v| {
					encode_params!(w, v, 0x21 => Filter::NextObject);
					Ok(())
				},
				|r, v| {
					decode_params!(r, v, 0x21 => val: Option<Filter>);
					assert_eq!(val, Some(Filter::NextObject));
					Ok(())
				},
			);
		}
	}

	#[test]
	fn test_param_multiple_delta_encoding() {
		for version in [
			Version::Draft14,
			Version::Draft15,
			Version::Draft16,
			Version::Draft17,
			Version::Draft18,
		] {
			round_trip_params(
				version,
				|w, v| {
					encode_params!(w, v,
						0x10 => true,
						0x20 => 200u8,
						0x21 => Filter::NextObject,
						0x22 => 2u8,
					);
					Ok(())
				},
				|r, v| {
					decode_params!(r, v,
						0x10 => forward: Option<bool>,
						0x20 => sub_pri: Option<u8>,
						0x21 => filter: Option<Filter>,
						0x22 => group_order: Option<u8>,
					);
					assert_eq!(forward, Some(true));
					assert_eq!(sub_pri, Some(200));
					assert_eq!(filter, Some(Filter::NextObject));
					assert_eq!(group_order, Some(2));
					Ok(())
				},
			);
		}
	}

	#[test]
	fn test_param_empty_set() {
		for version in [
			Version::Draft14,
			Version::Draft15,
			Version::Draft16,
			Version::Draft17,
			Version::Draft18,
		] {
			round_trip_params(
				version,
				|w, v| {
					encode_params!(w, v,);
					Ok(())
				},
				|r, v| {
					decode_params!(r, v,);
					Ok(())
				},
			);
		}
	}

	#[test]
	fn test_param_option_skip_none() {
		for version in [
			Version::Draft14,
			Version::Draft15,
			Version::Draft16,
			Version::Draft17,
			Version::Draft18,
		] {
			round_trip_params(
				version,
				|w, v| {
					let loc: Option<Location> = None;
					encode_params!(w, v,
						0x09 => loc,
						0x10 => true,
					);
					Ok(())
				},
				|r, v| {
					decode_params!(r, v,
						0x09 => loc: Option<Location>,
						0x10 => forward: Option<bool>,
					);
					assert_eq!(loc, None);
					assert_eq!(forward, Some(true));
					Ok(())
				},
			);
		}
	}

	#[test]
	fn test_param_option_encode_some() {
		for version in [
			Version::Draft14,
			Version::Draft15,
			Version::Draft16,
			Version::Draft17,
			Version::Draft18,
		] {
			round_trip_params(
				version,
				|w, v| {
					let loc = Some(Location { group: 10, object: 5 });
					encode_params!(w, v,
						0x09 => loc,
						0x10 => true,
					);
					Ok(())
				},
				|r, v| {
					decode_params!(r, v,
						0x09 => loc: Option<Location>,
						0x10 => forward: Option<bool>,
					);
					assert_eq!(loc, Some(Location { group: 10, object: 5 }));
					assert_eq!(forward, Some(true));
					Ok(())
				},
			);
		}
	}

	#[test]
	fn test_param_bare_type_defaults() {
		// Bare types use T::default() when the parameter is absent
		for version in [
			Version::Draft14,
			Version::Draft15,
			Version::Draft16,
			Version::Draft17,
			Version::Draft18,
		] {
			round_trip_params(
				version,
				|w, v| {
					// Encode only 0x10, not 0x20
					encode_params!(w, v, 0x10 => true);
					Ok(())
				},
				|r, v| {
					decode_params!(r, v,
						0x10 => forward: bool,
						0x20 => priority: u8,
					);
					assert!(forward);
					assert_eq!(priority, 0); // u8::default()
					Ok(())
				},
			);
		}
	}

	#[test]
	fn test_param_unknown_rejected() {
		// Manually encode one param at key 0x10, try to decode expecting key 0x20
		for version in [
			Version::Draft14,
			Version::Draft15,
			Version::Draft16,
			Version::Draft17,
			Version::Draft18,
		] {
			let mut buf = Vec::new();
			let mut w = Encoder::new(&mut buf, version.into());
			w.varint(1).unwrap();
			w.varint(0x10u64).unwrap();
			true.param_encode(&mut w, version).unwrap();

			let mut bytes = Decoder::new(&buf, version.into());
			let result: Result<(), DecodeError> = (|| {
				decode_params!(&mut bytes, version, 0x20 => val: Option<u8>);
				let _ = val;
				Ok(())
			})();
			assert!(
				matches!(result, Err(DecodeError::InvalidValue)),
				"expected InvalidValue for unknown param in {version}"
			);
		}
	}

	#[test]
	fn test_param_duplicate_rejected() {
		// Manually construct a buffer with duplicate key 0x20
		for version in [
			Version::Draft14,
			Version::Draft15,
			Version::Draft16,
			Version::Draft17,
			Version::Draft18,
		] {
			let mut buf = Vec::new();
			let mut w = Encoder::new(&mut buf, version.into());
			// Encode count = 2
			w.varint(2).unwrap();
			match version {
				Version::Draft14 | Version::Draft15 => {
					// Plain (non-delta) keys: first key=0x20, second key=0x20
					w.varint(0x20u64).unwrap();
					100u8.param_encode(&mut w, version).unwrap();
					w.varint(0x20u64).unwrap();
					200u8.param_encode(&mut w, version).unwrap();
				}
				_ => {
					// Delta-encoded: first delta=0x20 (abs=0x20), second delta=0 (abs=0x20)
					w.varint(0x20u64).unwrap();
					100u8.param_encode(&mut w, version).unwrap();
					w.varint(0).unwrap();
					200u8.param_encode(&mut w, version).unwrap();
				}
			}

			let mut bytes = Decoder::new(&buf, version.into());
			let result: Result<(), DecodeError> = (|| {
				decode_params!(&mut bytes, version, 0x20 => val: Option<u8>);
				let _ = val;
				Ok(())
			})();
			assert!(
				matches!(result, Err(DecodeError::Duplicate)),
				"expected Duplicate for {version}"
			);
		}
	}

	/// A parameter the draft lets repeat, such as AUTHORIZATION TOKEN, keeps every
	/// instance instead of failing the message as a duplicate.
	#[test]
	fn test_param_repeat_allowed() {
		for version in [Version::Draft15, Version::Draft16, Version::Draft17, Version::Draft20] {
			let mut buf = Vec::new();
			let mut w = Encoder::new(&mut buf, version.into());
			w.varint(2).unwrap();
			w.varint(0x03).unwrap();
			Opaque(vec![0xAA]).param_encode(&mut w, version).unwrap();
			// The second key: absolute before draft-16, a zero delta after.
			let second: u64 = if version == Version::Draft15 { 0x03 } else { 0 };
			w.varint(second).unwrap();
			Opaque(vec![0xBB]).param_encode(&mut w, version).unwrap();

			let mut r = Decoder::new(&buf, version.into());
			let tokens = (|| -> Result<Vec<Opaque>, DecodeError> {
				decode_params!(&mut r, version, 0x03 => tokens: Vec<Opaque>);
				Ok(tokens)
			})()
			.unwrap_or_else(|e| panic!("{version}: {e}"));
			assert_eq!(tokens, vec![Opaque(vec![0xAA]), Opaque(vec![0xBB])], "{version}");
			assert!(r.is_empty(), "{version}");
		}
	}

	/// Draft-14 lets AUTHORIZATION TOKEN repeat and has unknown parameters, duplicates
	/// included, ignored. The block is consumed whole either way.
	#[test]
	fn test_skip_allows_draft14_repeats() {
		#[rustfmt::skip]
		let block = [
			0x04, // Number of Parameters
			0x03, 0x01, 0xAA, // AUTHORIZATION TOKEN
			0x03, 0x01, 0xBB, // and again
			0x3E, 0x05, // an unknown varint parameter
			0x3E, 0x06, // and again
		];
		let mut r = Decoder::new(&block, Version::Draft14.into());
		Parameters::skip(&mut r).unwrap();
		assert!(r.is_empty());
	}
}
