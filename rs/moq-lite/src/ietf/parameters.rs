use std::collections::{HashMap, hash_map};

use bytes::Buf;
use num_enum::{FromPrimitive, IntoPrimitive};

use crate::coding::*;

use super::Version;
use super::{FilterType, Location};

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

#[derive(Default, Debug, Clone)]
pub struct Parameters {
	vars: HashMap<ParameterVarInt, u64>,
	bytes: HashMap<ParameterBytes, Vec<u8>>,
}

impl Decode<Version> for Parameters {
	fn decode<R: bytes::Buf>(mut r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let mut vars = HashMap::new();
		let mut bytes = HashMap::new();

		match version {
			Version::Draft17 => {
				// Draft17: no count prefix, read Key-Value-Pairs until buffer empty.
				// Delta-encoded types, even = varint value, odd = length-prefixed bytes.
				let mut prev_type: u64 = 0;
				let mut i = 0u64;
				while r.has_remaining() {
					if i >= MAX_PARAMS {
						return Err(DecodeError::TooMany);
					}
					let delta = u64::decode(&mut r, version)?;
					let abs = if i == 0 {
						delta
					} else {
						prev_type.checked_add(delta).ok_or(DecodeError::BoundsExceeded)?
					};
					prev_type = abs;
					i += 1;

					if abs % 2 == 0 {
						let kind = ParameterVarInt::from(abs);
						match vars.entry(kind) {
							hash_map::Entry::Occupied(_) => return Err(DecodeError::Duplicate),
							hash_map::Entry::Vacant(entry) => entry.insert(u64::decode(&mut r, version)?),
						};
					} else {
						let kind = ParameterBytes::from(abs);
						let val = Vec::<u8>::decode(&mut r, version)?;
						if val.len() > MAX_KVP_VALUE_LEN {
							return Err(DecodeError::BoundsExceeded);
						}
						match bytes.entry(kind) {
							hash_map::Entry::Occupied(_) => return Err(DecodeError::Duplicate),
							hash_map::Entry::Vacant(entry) => entry.insert(val),
						};
					}
				}
			}
			_ => {
				let count = u64::decode(r, version)?;

				if count > MAX_PARAMS {
					return Err(DecodeError::TooMany);
				}

				let mut prev_type: u64 = 0;

				for i in 0..count {
					let kind = match version {
						Version::Draft16 => {
							let delta = u64::decode(r, version)?;
							let abs = if i == 0 {
								delta
							} else {
								prev_type.checked_add(delta).ok_or(DecodeError::BoundsExceeded)?
							};
							prev_type = abs;
							abs
						}
						Version::Draft14 | Version::Draft15 => u64::decode(r, version)?,
						Version::Draft17 => unreachable!("handled above"),
					};

					if kind % 2 == 0 {
						let kind = ParameterVarInt::from(kind);
						match vars.entry(kind) {
							hash_map::Entry::Occupied(_) => return Err(DecodeError::Duplicate),
							hash_map::Entry::Vacant(entry) => entry.insert(u64::decode(&mut r, version)?),
						};
					} else {
						let kind = ParameterBytes::from(kind);
						let val = Vec::<u8>::decode(&mut r, version)?;
						if val.len() > MAX_KVP_VALUE_LEN {
							return Err(DecodeError::BoundsExceeded);
						}
						match bytes.entry(kind) {
							hash_map::Entry::Occupied(_) => return Err(DecodeError::Duplicate),
							hash_map::Entry::Vacant(entry) => entry.insert(val),
						};
					}
				}
			}
		}

		Ok(Parameters { vars, bytes })
	}
}

impl Encode<Version> for Parameters {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		let count = self.vars.len() + self.bytes.len();
		if count as u64 > MAX_PARAMS {
			return Err(EncodeError::TooMany);
		}

		match version {
			Version::Draft16 | Version::Draft17 => {
				// Draft16: count prefix + delta encoding
				// Draft17: NO count prefix + delta encoding
				if version != Version::Draft17 {
					count.encode(w, version)?;
				}

				// Collect all keys, sort, encode deltas
				enum ParamRef<'a> {
					Var(&'a u64),
					Bytes(&'a Vec<u8>),
				}
				let mut all: Vec<(u64, ParamRef)> = Vec::new();
				for (k, v) in self.vars.iter() {
					all.push((u64::from(*k), ParamRef::Var(v)));
				}
				for (k, v) in self.bytes.iter() {
					all.push((u64::from(*k), ParamRef::Bytes(v)));
				}
				all.sort_by_key(|(k, _)| *k);

				let mut prev_type: u64 = 0;
				for (idx, (kind, val)) in all.iter().enumerate() {
					let delta = if idx == 0 { *kind } else { kind - prev_type };
					prev_type = *kind;
					delta.encode(w, version)?;

					match val {
						ParamRef::Var(v) => v.encode(w, version)?,
						ParamRef::Bytes(v) => {
							if v.len() > MAX_KVP_VALUE_LEN {
								return Err(EncodeError::BoundsExceeded);
							}
							v.encode(w, version)?;
						}
					}
				}
			}
			Version::Draft14 | Version::Draft15 => {
				count.encode(w, version)?;

				for (kind, value) in self.vars.iter() {
					u64::from(*kind).encode(w, version)?;
					value.encode(w, version)?;
				}

				for (kind, value) in self.bytes.iter() {
					if value.len() > MAX_KVP_VALUE_LEN {
						return Err(EncodeError::BoundsExceeded);
					}
					u64::from(*kind).encode(w, version)?;
					value.encode(w, version)?;
				}
			}
		}

		Ok(())
	}
}

impl Parameters {
	pub fn get_varint(&self, kind: ParameterVarInt) -> Option<u64> {
		self.vars.get(&kind).copied()
	}

	pub fn set_varint(&mut self, kind: ParameterVarInt, value: u64) {
		self.vars.insert(kind, value);
	}

	#[cfg(test)]
	pub fn get_bytes(&self, kind: ParameterBytes) -> Option<&[u8]> {
		self.bytes.get(&kind).map(|v| v.as_slice())
	}

	pub fn set_bytes(&mut self, kind: ParameterBytes, value: Vec<u8>) {
		self.bytes.insert(kind, value);
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
	fn param_encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError>;
	fn param_decode<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError>;

	/// Whether this parameter should be encoded. Returns false to skip.
	fn param_present(&self) -> bool {
		true
	}
}

impl Param for u8 {
	fn param_encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match version {
			// Draft-14/15/16: u8 encoded as varint (cast to u64)
			Version::Draft14 | Version::Draft15 | Version::Draft16 => (*self as u64).encode(w, version),
			_ => Encode::encode(self, w, version),
		}
	}

	fn param_decode<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Draft14 | Version::Draft15 | Version::Draft16 => {
				let v = u64::decode(r, version)?;
				u8::try_from(v).map_err(|_| DecodeError::InvalidValue)
			}
			_ => u8::decode(r, version),
		}
	}
}

impl Param for bool {
	fn param_encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match version {
			// Draft-14/15/16: bool encoded as varint (cast to u64)
			Version::Draft14 | Version::Draft15 | Version::Draft16 => (*self as u64).encode(w, version),
			_ => Encode::encode(self, w, version),
		}
	}

	fn param_decode<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Draft14 | Version::Draft15 | Version::Draft16 => {
				let v = u64::decode(r, version)?;
				match v {
					0 => Ok(false),
					1 => Ok(true),
					_ => Err(DecodeError::InvalidValue),
				}
			}
			_ => bool::decode(r, version),
		}
	}
}

impl Param for u64 {
	fn param_encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		self.encode(w, version)
	}

	fn param_decode<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		u64::decode(r, version)
	}
}

impl Param for Location {
	fn param_encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Draft14 | Version::Draft15 | Version::Draft16 => {
				// Length-prefixed bytes containing two QUIC varints
				let mut buf = Vec::new();
				self.group.encode(&mut buf, Version::Draft15)?;
				self.object.encode(&mut buf, Version::Draft15)?;
				buf.encode(w, version)?;
				Ok(())
			}
			_ => {
				self.group.encode(w, version)?;
				self.object.encode(w, version)?;
				Ok(())
			}
		}
	}

	fn param_decode<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Draft14 | Version::Draft15 | Version::Draft16 => {
				// Length-prefixed bytes containing two QUIC varints
				let data = Vec::<u8>::decode(r, version)?;
				let mut buf = bytes::Bytes::from(data);
				let group = u64::decode(&mut buf, Version::Draft15)?;
				let object = u64::decode(&mut buf, Version::Draft15)?;
				if buf.has_remaining() {
					return Err(DecodeError::TrailingBytes);
				}
				Ok(Location { group, object })
			}
			_ => {
				let group = u64::decode(r, version)?;
				let object = u64::decode(r, version)?;
				Ok(Location { group, object })
			}
		}
	}
}

impl Param for FilterType {
	fn param_encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		let mut buf = Vec::new();
		// Use version-specific varint encoding for the inner value.
		// Fixes draft-17 interop: inner varints now use leading-ones, not QUIC.
		let sv = match version {
			Version::Draft14 | Version::Draft15 | Version::Draft16 => Version::Draft15,
			_ => version,
		};
		self.encode(&mut buf, sv)?;
		buf.encode(w, version)?;
		Ok(())
	}

	fn param_decode<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let data = Vec::<u8>::decode(r, version)?;
		let mut buf = bytes::Bytes::from(data);
		let sv = match version {
			Version::Draft14 | Version::Draft15 | Version::Draft16 => Version::Draft15,
			_ => version,
		};
		let filter = FilterType::decode(&mut buf, sv)?;
		if buf.has_remaining() {
			return Err(DecodeError::TrailingBytes);
		}
		Ok(filter)
	}
}

impl<T: Param> Param for Option<T> {
	fn param_present(&self) -> bool {
		self.is_some()
	}

	fn param_encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match self {
			Some(v) => v.param_encode(w, version),
			None => Ok(()),
		}
	}

	fn param_decode<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		Ok(Some(T::param_decode(r, version)?))
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
#[macro_export]
macro_rules! encode_params {
	($w:expr, $version:expr, $($key:expr => $val:expr),* $(,)?) => {{
		#[allow(unused_imports)]
		use $crate::coding::Encode as _;

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
		_count.encode($w, _version)?;

		#[allow(unused_mut, unused_assignments)]
		let mut _prev_key: u64 = 0;
		#[allow(unused_mut, unused_assignments)]
		let mut _first: bool = true;
		$(
			if $crate::ietf::Param::param_present(&$val) {
				let _key: u64 = $key;
				match _version {
					$crate::ietf::Version::Draft14 | $crate::ietf::Version::Draft15 => {
						_key.encode($w, _version)?;
					}
					_ => {
						let _delta = if _first { _key } else { _key - _prev_key };
						_delta.encode($w, _version)?;
					}
				}
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
/// Duplicate parameters cause `DecodeError::Duplicate`.
///
/// ```ignore
/// decode_params!(r, version,
///     0x10 => forward: Option<bool>,
///     0x20 => subscriber_priority: Option<u8>,
/// );
/// // forward: Option<bool> and subscriber_priority: Option<u8> are now in scope
/// let subscriber_priority = subscriber_priority.unwrap_or(128);
/// ```
#[macro_export]
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
			#[allow(unused_imports)]
			use $crate::coding::Decode as _;

			let _version: $crate::ietf::Version = $version;
			let _count = <u64 as $crate::coding::Decode<$crate::ietf::Version>>::decode($r, _version)?;
			if _count > 64 {
				return Err($crate::coding::DecodeError::TooMany);
			}

			#[allow(unused_mut, unused_assignments)]
			let mut _prev_key: u64 = 0;
			for _i in 0.._count {
				let _key: u64 = match _version {
					$crate::ietf::Version::Draft14 | $crate::ietf::Version::Draft15 => {
						<u64 as $crate::coding::Decode<$crate::ietf::Version>>::decode($r, _version)?
					}
					_ => {
						let _delta = <u64 as $crate::coding::Decode<$crate::ietf::Version>>::decode($r, _version)?;
						let _abs = if _i == 0 {
							_delta
						} else {
							_prev_key.checked_add(_delta).ok_or($crate::coding::DecodeError::BoundsExceeded)?
						};
						_prev_key = _abs;
						_abs
					}
				};

				match _key {
					$($key => {
						if $name.is_some() {
							return Err($crate::coding::DecodeError::Duplicate);
						}
						$name = Some(<$ty as $crate::ietf::Param>::param_decode($r, _version)?);
					})*
					_ => return Err($crate::coding::DecodeError::InvalidValue),
				}
			}
		}

		// Shadow with unwrap_or_default: Option<T> defaults to None, T defaults to T::default()
		$(#[allow(unused_variables)] let $name: $ty = $name.unwrap_or_default();)*
	};
}

#[cfg(test)]
mod tests {
	use super::*;
	use bytes::{Buf, BytesMut};

	// ---- Setup Parameters tests (unchanged) ----

	#[test]
	fn test_parameters_v16_delta_round_trip() {
		let mut params = Parameters::default();
		params.set_bytes(ParameterBytes::Path, b"/test".to_vec());
		params.set_varint(ParameterVarInt::MaxRequestId, 100);
		params.set_bytes(ParameterBytes::Implementation, b"test-impl".to_vec());

		let mut buf = BytesMut::new();
		params.encode(&mut buf, Version::Draft16).unwrap();

		let mut bytes = buf.freeze();
		let decoded = Parameters::decode(&mut bytes, Version::Draft16).unwrap();

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

		let mut buf = BytesMut::new();
		params.encode(&mut buf, Version::Draft15).unwrap();

		let mut bytes = buf.freeze();
		let decoded = Parameters::decode(&mut bytes, Version::Draft15).unwrap();

		assert_eq!(decoded.get_bytes(ParameterBytes::Path), Some(b"/test".as_ref()));
		assert_eq!(decoded.get_varint(ParameterVarInt::MaxRequestId), Some(100));
	}

	#[test]
	fn test_parameters_v17_round_trip() {
		let mut params = Parameters::default();
		params.set_bytes(ParameterBytes::Path, b"/test".to_vec());
		params.set_varint(ParameterVarInt::MaxAuthTokenCacheSize, 4096);
		params.set_bytes(ParameterBytes::Implementation, b"test-impl".to_vec());

		let mut buf = BytesMut::new();
		params.encode(&mut buf, Version::Draft17).unwrap();

		let mut bytes = buf.freeze();
		let decoded = Parameters::decode(&mut bytes, Version::Draft17).unwrap();

		assert_eq!(decoded.get_bytes(ParameterBytes::Path), Some(b"/test".as_ref()));
		assert_eq!(decoded.get_varint(ParameterVarInt::MaxAuthTokenCacheSize), Some(4096));
		assert_eq!(
			decoded.get_bytes(ParameterBytes::Implementation),
			Some(b"test-impl".as_ref())
		);
		assert!(!bytes.has_remaining());
	}

	#[test]
	fn test_parameters_v17_no_count_prefix() {
		let mut params = Parameters::default();
		params.set_bytes(ParameterBytes::Path, b"/x".to_vec());

		let mut buf15 = BytesMut::new();
		params.encode(&mut buf15, Version::Draft15).unwrap();

		let mut buf17 = BytesMut::new();
		params.encode(&mut buf17, Version::Draft17).unwrap();

		assert!(buf17.len() < buf15.len());
	}

	// ---- Message Parameter (encode_params!/decode_params!) tests ----

	fn round_trip_params(
		version: Version,
		encode_fn: impl FnOnce(&mut BytesMut, Version) -> Result<(), EncodeError>,
		decode_fn: impl FnOnce(&mut bytes::Bytes, Version) -> Result<(), DecodeError>,
	) {
		let mut buf = BytesMut::new();
		encode_fn(&mut buf, version).unwrap();
		let mut bytes = buf.freeze();
		decode_fn(&mut bytes, version).unwrap();
		assert!(!bytes.has_remaining(), "buffer not fully consumed for {version}");
	}

	#[test]
	fn test_param_u8_all_versions() {
		for version in [Version::Draft14, Version::Draft15, Version::Draft16, Version::Draft17] {
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
	fn test_param_bool_all_versions() {
		for version in [Version::Draft14, Version::Draft15, Version::Draft16, Version::Draft17] {
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
		for version in [Version::Draft14, Version::Draft15, Version::Draft16, Version::Draft17] {
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
	fn test_param_filter_type_all_versions() {
		for version in [Version::Draft14, Version::Draft15, Version::Draft16, Version::Draft17] {
			round_trip_params(
				version,
				|w, v| {
					encode_params!(w, v, 0x21 => FilterType::LargestObject);
					Ok(())
				},
				|r, v| {
					decode_params!(r, v, 0x21 => val: Option<FilterType>);
					assert_eq!(val, Some(FilterType::LargestObject));
					Ok(())
				},
			);
		}
	}

	#[test]
	fn test_param_multiple_delta_encoding() {
		for version in [Version::Draft14, Version::Draft15, Version::Draft16, Version::Draft17] {
			round_trip_params(
				version,
				|w, v| {
					encode_params!(w, v,
						0x10 => true,
						0x20 => 200u8,
						0x21 => FilterType::LargestObject,
						0x22 => 2u8,
					);
					Ok(())
				},
				|r, v| {
					decode_params!(r, v,
						0x10 => forward: Option<bool>,
						0x20 => sub_pri: Option<u8>,
						0x21 => filter: Option<FilterType>,
						0x22 => group_order: Option<u8>,
					);
					assert_eq!(forward, Some(true));
					assert_eq!(sub_pri, Some(200));
					assert_eq!(filter, Some(FilterType::LargestObject));
					assert_eq!(group_order, Some(2));
					Ok(())
				},
			);
		}
	}

	#[test]
	fn test_param_empty_set() {
		for version in [Version::Draft14, Version::Draft15, Version::Draft16, Version::Draft17] {
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
		for version in [Version::Draft14, Version::Draft15, Version::Draft16, Version::Draft17] {
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
		for version in [Version::Draft14, Version::Draft15, Version::Draft16, Version::Draft17] {
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
		for version in [Version::Draft14, Version::Draft15, Version::Draft16, Version::Draft17] {
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
		for version in [Version::Draft14, Version::Draft15, Version::Draft16, Version::Draft17] {
			let mut buf = BytesMut::new();
			1usize.encode(&mut buf, version).unwrap();
			0x10u64.encode(&mut buf, version).unwrap();
			true.param_encode(&mut buf, version).unwrap();

			let mut bytes = buf.freeze();
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
		for version in [Version::Draft14, Version::Draft15, Version::Draft16, Version::Draft17] {
			let mut buf = BytesMut::new();
			// Encode count = 2
			2usize.encode(&mut buf, version).unwrap();
			match version {
				Version::Draft16 | Version::Draft17 => {
					// First: key delta=0x20, value=100
					0x20u64.encode(&mut buf, version).unwrap();
					100u8.param_encode(&mut buf, version).unwrap();
					// Second: key delta=0 (same key 0x20), value=200
					0u64.encode(&mut buf, version).unwrap();
					200u8.param_encode(&mut buf, version).unwrap();
				}
				_ => {
					// First: key=0x20, value=100
					0x20u64.encode(&mut buf, version).unwrap();
					100u8.param_encode(&mut buf, version).unwrap();
					// Second: key=0x20, value=200
					0x20u64.encode(&mut buf, version).unwrap();
					200u8.param_encode(&mut buf, version).unwrap();
				}
			}

			let mut bytes = buf.freeze();
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
}
