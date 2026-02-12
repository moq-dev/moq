use crate::coding::*;

use std::{fmt, ops::Deref};

/// A version number negotiated during the setup.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version(pub u64);

impl From<u64> for Version {
	fn from(v: u64) -> Self {
		Self(v)
	}
}

impl From<Version> for u64 {
	fn from(v: Version) -> Self {
		v.0
	}
}

impl<V> Decode<V> for Version {
	/// Decode the version number.
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		let v = u64::decode(r, version)?;
		Ok(Self(v))
	}
}

impl<V> Encode<V> for Version {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) {
		self.0.encode(w, version);
	}
}

impl fmt::Debug for Version {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		self.0.fmt(f)
	}
}

/// A list of versions in preferred order.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Versions(Vec<Version>);

impl<V: Clone> Decode<V> for Versions {
	/// Decode the version list.
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		let count = u64::decode(r, version.clone())?;
		let mut vs = Vec::new();

		for _ in 0..count {
			let v = Version::decode(r, version.clone())?;
			vs.push(v);
		}

		Ok(Self(vs))
	}
}

impl<V: Clone> Encode<V> for Versions {
	/// Encode the version list.
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) {
		self.0.len().encode(w, version.clone());

		for v in &self.0 {
			v.encode(w, version.clone());
		}
	}
}

impl Deref for Versions {
	type Target = Vec<Version>;

	fn deref(&self) -> &Self::Target {
		&self.0
	}
}

impl From<Vec<Version>> for Versions {
	fn from(vs: Vec<Version>) -> Self {
		Self(vs)
	}
}

impl<const N: usize> From<[Version; N]> for Versions {
	fn from(vs: [Version; N]) -> Self {
		Self(vs.to_vec())
	}
}

impl fmt::Debug for Versions {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_list().entries(self.0.iter()).finish()
	}
}
