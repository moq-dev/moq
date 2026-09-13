use std::fmt;
use std::sync::Arc;

use aws_lc_rs::hkdf::{self, HKDF_SHA256};
use aws_lc_rs::hmac;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::{Error, Result};
use crate::limits::{
	DOMAIN_DATAGRAM, DOMAIN_GROUP, KEY_LABEL, KEY_LEN, NAME_LABEL, NAME_LEN, PHYSICAL_NAME_LEN, PROFILE, SALT,
	SECRET_LEN, check_bytes, check_u53,
};

/// Grouped-frame versus datagram key domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Domain {
	/// Grouped frames (`0x00`).
	Group,
	/// Datagrams (`0x01`).
	Datagram,
}

impl Domain {
	/// The single-byte domain identifier.
	pub const fn as_u8(self) -> u8 {
		match self {
			Self::Group => DOMAIN_GROUP,
			Self::Datagram => DOMAIN_DATAGRAM,
		}
	}

	#[cfg(test)]
	pub(crate) fn from_u8(value: u8) -> Result<Self> {
		match value {
			DOMAIN_GROUP => Ok(Self::Group),
			DOMAIN_DATAGRAM => Ok(Self::Datagram),
			_ => Err(Error::Identity),
		}
	}
}

/// Application pin for `(profile, generation, kid)`.
///
/// A relay-replayed catalog is never a freshness authority.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Pin {
	generation: u64,
	kid: u64,
}

impl Pin {
	/// Pin these generation and kid values under [`PROFILE`].
	///
	/// # Errors
	///
	/// [`Error::Identity`] if either integer exceeds `2^53-1`.
	pub fn new(generation: u64, kid: u64) -> Result<Self> {
		check_u53(generation)?;
		check_u53(kid)?;
		Ok(Self { generation, kid })
	}

	/// The pinned generation.
	pub fn generation(&self) -> u64 {
		self.generation
	}

	/// The pinned kid.
	pub fn kid(&self) -> u64 {
		self.kid
	}
}

#[derive(Zeroize, ZeroizeOnDrop)]
struct Secret([u8; SECRET_LEN]);

impl fmt::Debug for Secret {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str("<redacted>")
	}
}

struct Inner {
	context: Bytes,
	generation: u64,
	kid: u64,
	secret: Secret,
	prk: hkdf::Prk,
}

impl fmt::Debug for Inner {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Credential")
			.field("profile", &PROFILE)
			.field("context_len", &self.context.len())
			.field("generation", &self.generation)
			.field("kid", &self.kid)
			.field("secret", &self.secret)
			.finish()
	}
}

/// Immutable out-of-band credential for one broadcast generation.
///
/// Cheaply cloneable: clones share the secret and derived PRK. Publication is exclusive
/// via [`crate::Publication`], not by cloning this value.
#[derive(Clone)]
pub struct Credential(Arc<Inner>);

impl fmt::Debug for Credential {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		self.0.fmt(f)
	}
}

impl Credential {
	/// Check the profile, bounds, and secret, then extract the PRK.
	///
	/// # Errors
	///
	/// [`Error::UnsupportedProfile`], [`Error::InvalidSecret`], or [`Error::Identity`].
	pub fn new(
		profile: &str,
		context: impl Into<Bytes>,
		generation: u64,
		kid: u64,
		secret: impl AsRef<[u8]>,
	) -> Result<Self> {
		if profile != PROFILE {
			return Err(Error::UnsupportedProfile);
		}
		let context = context.into();
		check_bytes(context.len())?;
		check_u53(generation)?;
		check_u53(kid)?;
		let secret_bytes = secret.as_ref();
		if secret_bytes.len() != SECRET_LEN {
			return Err(Error::InvalidSecret);
		}
		let mut secret = Secret([0u8; SECRET_LEN]);
		secret.0.copy_from_slice(secret_bytes);
		let salt = hkdf::Salt::new(HKDF_SHA256, SALT);
		let prk = salt.extract(&secret.0);
		Ok(Self(Arc::new(Inner {
			context,
			generation,
			kid,
			secret,
			prk,
		})))
	}

	/// Fill a 32-byte secret from the platform CSPRNG and build a credential.
	///
	/// # Errors
	///
	/// [`Error::Identity`] if `context`, `generation`, or `kid` is out of range.
	pub fn generate(context: impl Into<Bytes>, generation: u64, kid: u64) -> Result<Self> {
		let mut secret = [0u8; SECRET_LEN];
		aws_lc_rs::rand::fill(&mut secret).expect("CSPRNG");
		let cred = Self::new(PROFILE, context, generation, kid, secret)?;
		secret.zeroize();
		Ok(cred)
	}

	/// The profile string, always [`PROFILE`] after construction.
	pub fn profile(&self) -> &'static str {
		PROFILE
	}

	/// Broadcast context bytes.
	pub fn context(&self) -> &[u8] {
		&self.0.context
	}

	/// Generation counter.
	pub fn generation(&self) -> u64 {
		self.0.generation
	}

	/// Key identifier.
	pub fn kid(&self) -> u64 {
		self.0.kid
	}

	/// Pin matching this credential's generation and kid.
	pub fn pin(&self) -> Pin {
		Pin {
			generation: self.0.generation,
			kid: self.0.kid,
		}
	}

	/// Refuse a credential that is not this pin.
	///
	/// # Errors
	///
	/// [`Error::PinnedMismatch`] if generation or kid differ.
	pub fn check_pin(&self, pin: &Pin) -> Result<()> {
		if self.0.generation == pin.generation && self.0.kid == pin.kid {
			Ok(())
		} else {
			Err(Error::PinnedMismatch)
		}
	}

	/// HKDF-Extract PRK bytes (32), for known-answer tests.
	pub fn prk_bytes(&self) -> [u8; 32] {
		let key = hmac::Key::new(hmac::HMAC_SHA256, SALT);
		let tag = hmac::sign(&key, &self.0.secret.0);
		let mut out = [0u8; 32];
		out.copy_from_slice(tag.as_ref());
		out
	}

	pub(crate) fn prk(&self) -> &hkdf::Prk {
		&self.0.prk
	}

	/// HKDF info for a physical name.
	///
	/// # Errors
	///
	/// [`Error::Identity`] if `semantic_name` exceeds 65535 bytes.
	pub fn name_info(&self, semantic_name: &[u8]) -> Result<Vec<u8>> {
		check_bytes(semantic_name.len())?;
		let mut info =
			Vec::with_capacity(NAME_LABEL.len() + 2 + self.0.context.len() + 8 + 8 + 2 + semantic_name.len());
		info.extend_from_slice(NAME_LABEL);
		append_bytes(&mut info, &self.0.context)?;
		info.extend_from_slice(&self.0.generation.to_be_bytes());
		info.extend_from_slice(&self.0.kid.to_be_bytes());
		append_bytes(&mut info, semantic_name)?;
		Ok(info)
	}

	/// HKDF info for a track/domain key.
	///
	/// # Errors
	///
	/// [`Error::Identity`] if the physical name is not 22 ASCII bytes.
	pub fn key_info(&self, physical: &PhysicalName, domain: Domain) -> Result<Vec<u8>> {
		let mut info =
			Vec::with_capacity(KEY_LABEL.len() + 2 + self.0.context.len() + 8 + 8 + 2 + PHYSICAL_NAME_LEN + 1);
		info.extend_from_slice(KEY_LABEL);
		append_bytes(&mut info, &self.0.context)?;
		info.extend_from_slice(&self.0.generation.to_be_bytes());
		info.extend_from_slice(&self.0.kid.to_be_bytes());
		append_bytes(&mut info, physical.as_bytes())?;
		info.push(domain.as_u8());
		Ok(info)
	}

	/// Derive the opaque physical track name for a semantic name.
	///
	/// # Errors
	///
	/// [`Error::Identity`] if `semantic_name` exceeds 65535 bytes.
	pub fn physical_name(&self, semantic_name: impl AsRef<[u8]>) -> Result<PhysicalName> {
		let info = self.name_info(semantic_name.as_ref())?;
		let material = expand16(self.prk(), &info)?;
		Ok(PhysicalName::from_material(material))
	}

	/// Derive the 16-byte AEAD key for a physical name and domain.
	///
	/// # Errors
	///
	/// [`Error::Identity`] if derivation fails.
	pub fn key_bytes(&self, physical: &PhysicalName, domain: Domain) -> Result<[u8; KEY_LEN]> {
		let info = self.key_info(physical, domain)?;
		expand16(self.prk(), &info)
	}

	/// Start exclusive publication of this generation.
	///
	/// Claims `(context, generation, kid)` in a process-global set that is never
	/// evicted, so restarting publication under the same generation in this process
	/// is refused even after the [`crate::Publication`] is dropped.
	///
	/// # Errors
	///
	/// [`Error::Reuse`] if this `(context, generation, kid)` has already been published
	/// in this process.
	pub fn publish(&self) -> Result<crate::Publication> {
		crate::Publication::new(self.clone())
	}
}

/// Opaque physical track name: 22-character unpadded base64url.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PhysicalName([u8; PHYSICAL_NAME_LEN]);

impl PhysicalName {
	fn from_material(material: [u8; NAME_LEN]) -> Self {
		let mut out = [0u8; PHYSICAL_NAME_LEN];
		let n = URL_SAFE_NO_PAD
			.encode_slice(material, &mut out)
			.expect("16 bytes encode to 22 base64url characters");
		debug_assert_eq!(n, PHYSICAL_NAME_LEN);
		Self(out)
	}

	/// Parse a 22-character unpadded base64url name.
	///
	/// # Errors
	///
	/// [`Error::Identity`] if the string is not a valid physical name.
	pub fn parse(name: &str) -> Result<Self> {
		if name.len() != PHYSICAL_NAME_LEN || !name.is_ascii() {
			return Err(Error::Identity);
		}
		let mut material = [0u8; NAME_LEN];
		let n = URL_SAFE_NO_PAD
			.decode_slice(name.as_bytes(), &mut material)
			.map_err(|_| Error::Identity)?;
		if n != NAME_LEN {
			return Err(Error::Identity);
		}
		let mut out = [0u8; PHYSICAL_NAME_LEN];
		out.copy_from_slice(name.as_bytes());
		Ok(Self(out))
	}

	/// The 22 ASCII characters.
	pub fn as_str(&self) -> &str {
		std::str::from_utf8(&self.0).expect("base64url is ASCII")
	}

	/// The 22 ASCII bytes.
	pub fn as_bytes(&self) -> &[u8] {
		&self.0
	}
}

impl fmt::Debug for PhysicalName {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_tuple("PhysicalName").field(&self.as_str()).finish()
	}
}

impl fmt::Display for PhysicalName {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(self.as_str())
	}
}

impl AsRef<str> for PhysicalName {
	fn as_ref(&self) -> &str {
		self.as_str()
	}
}

struct Len16;

impl hkdf::KeyType for Len16 {
	fn len(&self) -> usize {
		KEY_LEN
	}
}

fn expand16(prk: &hkdf::Prk, info: &[u8]) -> Result<[u8; KEY_LEN]> {
	let info = [info];
	let okm = prk.expand(&info, Len16).map_err(|_| Error::Identity)?;
	let mut out = [0u8; KEY_LEN];
	okm.fill(&mut out).map_err(|_| Error::Identity)?;
	Ok(out)
}

fn append_bytes(out: &mut Vec<u8>, data: &[u8]) -> Result<()> {
	check_bytes(data.len())?;
	out.extend_from_slice(&(data.len() as u16).to_be_bytes());
	out.extend_from_slice(data);
	Ok(())
}
