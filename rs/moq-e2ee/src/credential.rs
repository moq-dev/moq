//! The out-of-band broadcast credential and the derivations scoped to it alone.

use std::fmt;
use std::sync::Arc;

use aws_lc_rs::hkdf::{self, HKDF_SHA256};
use bytes::Bytes;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::epoch::Epoch;
use crate::error::Result;
use crate::generation::Generation;
use crate::limits::{KEY_LEN, NAME_LEN, PATH_LABEL, PROFILE, SECRET_LEN, check_bytes, check_u53};
use crate::name::encode;

/// What the application distributes over its own authenticated channel.
pub struct Config {
	/// Opaque bytes both ends agree on as the broadcast's end-to-end identity.
	pub context: Bytes,
	/// Selects among the credentials the application retains; rotating the secret is a new kid.
	pub kid: u64,
	/// 32 bytes from a cryptographically secure random generator.
	pub secret: [u8; SECRET_LEN],
}

#[derive(Zeroize, ZeroizeOnDrop)]
struct Secret([u8; SECRET_LEN]);

struct Inner {
	context: Bytes,
	kid: u64,
	// Retained only so it is zeroized with the credential; every derivation uses the PRK.
	_secret: Secret,
	prk: hkdf::Prk,
}

/// An immutable broadcast credential: cheap to clone, never serialized, redacted from `Debug`.
#[derive(Clone)]
pub struct Credential(Arc<Inner>);

impl fmt::Debug for Credential {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Credential")
			.field("context_len", &self.0.context.len())
			.field("kid", &self.0.kid)
			.field("secret", &"<redacted>")
			.finish()
	}
}

impl Credential {
	/// Check the bounds and extract the HKDF PRK.
	///
	/// # Errors
	///
	/// [`Error::Identity`](crate::Error::Identity) if `context` exceeds 65535 bytes or `kid` exceeds `2^53-1`.
	pub fn new(config: Config) -> Result<Self> {
		check_bytes(config.context.len())?;
		check_u53(config.kid)?;
		let secret = Secret(config.secret);
		let prk = hkdf::Salt::new(HKDF_SHA256, PROFILE.as_bytes()).extract(&secret.0);
		Ok(Self(Arc::new(Inner {
			context: config.context,
			kid: config.kid,
			_secret: secret,
			prk,
		})))
	}

	/// Broadcast context bytes.
	pub fn context(&self) -> &[u8] {
		&self.0.context
	}

	/// Key identifier.
	pub fn kid(&self) -> u64 {
		self.0.kid
	}

	/// The opaque broadcast path for a semantic broadcast name; instances publish at `<path>/<epoch>`.
	///
	/// # Errors
	///
	/// [`Error::Identity`](crate::Error::Identity) if `semantic` exceeds 65535 bytes.
	pub fn path(&self, semantic: &str) -> Result<moq_net::PathOwned> {
		let material = self.expand(&self.path_info(semantic)?)?;
		Ok(moq_net::Path::new(encode(material).as_str()).into_owned())
	}

	/// Bind an epoch, scoping every name and key derived from this credential.
	pub fn generation(&self, epoch: Epoch) -> Generation {
		Generation::new(self.clone(), epoch)
	}

	pub(crate) fn path_info(&self, semantic: &str) -> Result<Vec<u8>> {
		let mut info = Vec::with_capacity(PATH_LABEL.len() + 2 + self.0.context.len() + 8 + 2 + semantic.len());
		info.extend_from_slice(PATH_LABEL);
		append_bytes(&mut info, &self.0.context)?;
		info.extend_from_slice(&self.0.kid.to_be_bytes());
		append_bytes(&mut info, semantic.as_bytes())?;
		Ok(info)
	}

	/// HKDF-Expand 16 bytes of output for `info`.
	pub(crate) fn expand(&self, info: &[u8]) -> Result<[u8; KEY_LEN]> {
		const { assert!(KEY_LEN == NAME_LEN) };
		let info = [info];
		let okm = self.0.prk.expand(&info, Len16).map_err(|_| crate::Error::Identity)?;
		let mut out = [0u8; KEY_LEN];
		okm.fill(&mut out).map_err(|_| crate::Error::Identity)?;
		Ok(out)
	}

	#[cfg(test)]
	pub(crate) fn prk_bytes(&self) -> [u8; 32] {
		use aws_lc_rs::hmac;
		let key = hmac::Key::new(hmac::HMAC_SHA256, PROFILE.as_bytes());
		let tag = hmac::sign(&key, &self.0._secret.0);
		let mut out = [0u8; 32];
		out.copy_from_slice(tag.as_ref());
		out
	}
}

struct Len16;

impl hkdf::KeyType for Len16 {
	fn len(&self) -> usize {
		KEY_LEN
	}
}

/// Append a `bytes` field: `u16(length) || data`.
pub(crate) fn append_bytes(out: &mut Vec<u8>, data: &[u8]) -> Result<()> {
	check_bytes(data.len())?;
	out.extend_from_slice(&(data.len() as u16).to_be_bytes());
	out.extend_from_slice(data);
	Ok(())
}
