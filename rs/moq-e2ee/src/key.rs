use std::fmt;

use bytes::Bytes;
use zeroize::Zeroize;

use crate::error::{Error, Result};
use crate::limits::{KEY_LEN, MAX_INVOCATIONS, MAX_PLAINTEXT_BYTES, TAG_LEN};
use crate::protect::{nonce, open, protect};

/// Per-track, per-domain AES-128-GCM key with invocation and byte accounting.
///
/// Not `Clone`: cloning would duplicate the exhaustion counters.
pub(crate) struct TrackKey {
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
			.field("key", &"<redacted>")
			.field("invocations", &self.invocations)
			.field("plaintext_bytes", &self.plaintext_bytes)
			.finish()
	}
}

impl TrackKey {
	pub(crate) fn new(bytes: [u8; KEY_LEN]) -> Self {
		Self {
			bytes,
			invocations: 0,
			plaintext_bytes: 0,
		}
	}

	/// Refuse the next operation over `plaintext_len` bytes if it would exhaust the key.
	fn reserve(&self, plaintext_len: usize) -> Result<()> {
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
	pub(crate) fn protect(&mut self, group: u64, frame: u64, plaintext: &[u8], payload_limit: usize) -> Result<Bytes> {
		if plaintext.len().saturating_add(TAG_LEN) > payload_limit {
			return Err(Error::Oversize);
		}
		self.reserve(plaintext.len())?;
		let payload = protect(&self.bytes, group, frame, plaintext, payload_limit)?;
		self.commit(plaintext.len());
		Ok(payload)
	}

	/// Decrypt, counting the attempt whether or not the tag verifies.
	pub(crate) fn open(&mut self, group: u64, frame: u64, payload: &[u8], payload_limit: usize) -> Result<Bytes> {
		if payload.len() < TAG_LEN || payload.len() > payload_limit {
			return Err(Error::Oversize);
		}
		nonce(group, frame)?;
		let plaintext_len = payload.len() - TAG_LEN;
		self.reserve(plaintext_len)?;
		let result = open(&self.bytes, group, frame, payload, payload_limit);
		// A failed open still ran AES-GCM over every block, so it spends the budget.
		self.commit(plaintext_len);
		result
	}

	#[cfg(test)]
	pub(crate) fn invocations(&self) -> u64 {
		self.invocations
	}

	#[cfg(test)]
	pub(crate) fn set_usage(&mut self, invocations: u64, plaintext_bytes: u64) {
		self.invocations = invocations;
		self.plaintext_bytes = plaintext_bytes;
	}
}
