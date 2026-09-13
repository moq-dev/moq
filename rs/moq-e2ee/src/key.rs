use std::fmt;

use bytes::Bytes;
use zeroize::Zeroize;

use crate::credential::{Credential, Domain, PhysicalName};
use crate::error::{Error, Result};
use crate::limits::{KEY_LEN, MAX_INVOCATIONS, MAX_PLAINTEXT_BYTES, TAG_LEN};
use crate::protect::{open, protect};

/// Per-track, per-domain AES-128-GCM key with invocation and byte accounting.
///
/// Not `Clone`: cloning would duplicate the exhaustion counters. [`Debug`] redacts the key.
pub struct TrackKey {
	physical: PhysicalName,
	domain: Domain,
	bytes: [u8; KEY_LEN],
	invocations: u64,
	plaintext_bytes: u64,
}

impl Drop for TrackKey {
	fn drop(&mut self) {
		self.bytes.zeroize();
	}
}

impl fmt::Debug for TrackKey {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("TrackKey")
			.field("physical", &self.physical)
			.field("domain", &self.domain)
			.field("key", &"<redacted>")
			.field("invocations", &self.invocations)
			.field("plaintext_bytes", &self.plaintext_bytes)
			.finish()
	}
}

impl TrackKey {
	/// Derive the key for this physical name and domain.
	///
	/// # Errors
	///
	/// [`Error::Identity`] if derivation fails.
	pub fn derive(credential: &Credential, physical: &PhysicalName, domain: Domain) -> Result<Self> {
		let bytes = credential.key_bytes(physical, domain)?;
		Ok(Self {
			physical: physical.clone(),
			domain,
			bytes,
			invocations: 0,
			plaintext_bytes: 0,
		})
	}

	/// The physical track name this key was derived for.
	pub fn physical_name(&self) -> &PhysicalName {
		&self.physical
	}

	/// The key domain.
	pub fn domain(&self) -> Domain {
		self.domain
	}

	/// Raw key bytes. For known-answer tests; never log or serialize the return.
	pub fn key_bytes(&self) -> [u8; KEY_LEN] {
		self.bytes
	}

	/// AEAD operations counted against this key so far.
	pub fn invocations(&self) -> u64 {
		self.invocations
	}

	/// Plaintext bytes counted against this key so far.
	pub fn plaintext_bytes(&self) -> u64 {
		self.plaintext_bytes
	}

	pub(crate) fn prepare(&self, plaintext_len: usize, payload_limit: usize) -> Result<()> {
		if plaintext_len.saturating_add(TAG_LEN) > payload_limit {
			return Err(Error::Oversize);
		}
		self.prepare_usage(plaintext_len)
	}

	fn prepare_usage(&self, plaintext_len: usize) -> Result<()> {
		if self.invocations >= MAX_INVOCATIONS {
			return Err(Error::Exhausted);
		}
		let add = u64::try_from(plaintext_len).map_err(|_| Error::Oversize)?;
		if self.plaintext_bytes.saturating_add(add) > MAX_PLAINTEXT_BYTES {
			return Err(Error::Exhausted);
		}
		Ok(())
	}

	fn commit(&mut self, plaintext_len: usize) {
		self.invocations += 1;
		self.plaintext_bytes += plaintext_len as u64;
	}

	/// Encrypt, counting this invocation and its plaintext bytes.
	///
	/// # Errors
	///
	/// [`Error::Exhausted`], [`Error::Oversize`], or [`Error::Identity`].
	pub fn protect(&mut self, group: u64, frame: u64, plaintext: &[u8], payload_limit: usize) -> Result<Bytes> {
		self.prepare(plaintext.len(), payload_limit)?;
		let payload = protect(&self.bytes, group, frame, plaintext, payload_limit)?;
		self.commit(plaintext.len());
		Ok(payload)
	}

	/// Decrypt, counting this invocation and the recovered plaintext bytes.
	///
	/// # Errors
	///
	/// [`Error::Exhausted`], [`Error::Oversize`], [`Error::Identity`], or [`Error::Authentication`].
	pub fn open(&mut self, group: u64, frame: u64, payload: &[u8], payload_limit: usize) -> Result<Bytes> {
		if payload.len() < TAG_LEN || payload.len() > payload_limit {
			return Err(Error::Oversize);
		}
		let plaintext_len = payload.len() - TAG_LEN;
		self.prepare_usage(plaintext_len)?;
		let plaintext = open(&self.bytes, group, frame, payload, payload_limit)?;
		self.commit(plaintext.len());
		Ok(plaintext)
	}

	#[cfg(test)]
	pub(crate) fn set_usage(&mut self, invocations: u64, plaintext_bytes: u64) {
		self.invocations = invocations;
		self.plaintext_bytes = plaintext_bytes;
	}
}
