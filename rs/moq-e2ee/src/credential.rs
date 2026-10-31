//! The out-of-band broadcast credential and the derivations scoped to it alone.

use std::fmt;
use std::sync::Arc;

use aws_lc_rs::hkdf::{self, HKDF_SHA256};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::Result;
use crate::generation;
use crate::limits::{PATH_LABEL, PATH_LEN, PROFILE, SECRET_LEN, check_bytes, check_u53};
use moq_net::Epoch;

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

	/// The opaque broadcast path for a semantic broadcast name; each instance announces its epoch on the route.
	///
	/// # Errors
	///
	/// [`Error::Identity`](crate::Error::Identity) if `semantic` exceeds 65535 bytes.
	pub fn path(&self, semantic: &str) -> Result<moq_net::Path> {
		let material: [u8; PATH_LEN] = self.expand(&self.path_info(semantic)?);
		// A fixed-length encoding, far under the path limit.
		Ok(moq_net::Path::new(&URL_SAFE_NO_PAD.encode(material)).expect("a fixed-length path fits"))
	}

	/// Mint a fresh epoch and a producing generation whose clones share track claims.
	///
	/// The generation belongs to this publisher instance alone. A cloned process image (a
	/// `fork` without `exec`, a restored snapshot or checkpoint) copies its epoch and nonce
	/// counters, so each copy must mint its own before it encrypts.
	pub fn produce(&self) -> generation::Producer {
		generation::Producer::new(self.clone(), Epoch::mint())
	}

	/// Bind a discovered epoch as a consume-only generation.
	pub fn consume(&self, epoch: Epoch) -> generation::Consumer {
		generation::Consumer::new(self.clone(), epoch)
	}

	pub(crate) fn path_info(&self, semantic: &str) -> Result<Vec<u8>> {
		let mut info = Vec::with_capacity(PATH_LABEL.len() + 2 + self.0.context.len() + 8 + 2 + semantic.len());
		info.extend_from_slice(PATH_LABEL);
		append_bytes(&mut info, &self.0.context)?;
		info.extend_from_slice(&self.0.kid.to_be_bytes());
		append_bytes(&mut info, semantic.as_bytes())?;
		Ok(info)
	}

	/// HKDF-Expand `N` bytes of output for `info`.
	pub(crate) fn expand<const N: usize>(&self, info: &[u8]) -> [u8; N] {
		const { assert!(N <= 255 * 32, "HKDF-SHA-256 output limit") };
		let info = [info];
		let okm = self.0.prk.expand(&info, Len(N)).expect("HKDF length within the limit");
		let mut out = [0u8; N];
		okm.fill(&mut out).expect("HKDF fills its own length");
		out
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

struct Len(usize);

impl hkdf::KeyType for Len {
	fn len(&self) -> usize {
		self.0
	}
}

/// Append a `bytes` field: `u16(length) || data`.
pub(crate) fn append_bytes(out: &mut Vec<u8>, data: &[u8]) -> Result<()> {
	check_bytes(data.len())?;
	out.extend_from_slice(&(data.len() as u16).to_be_bytes());
	out.extend_from_slice(data);
	Ok(())
}
