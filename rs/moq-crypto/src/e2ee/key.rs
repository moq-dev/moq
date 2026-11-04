use std::fmt;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use rind::aead;

use crate::e2ee::error::{Error, Result};
use crate::e2ee::limits::{MAX_INVOCATIONS, MAX_PLAINTEXT_BYTES, TAG_LEN, check_u53};
use crate::e2ee::protect::{nonce, open, protect};

/// A semantic name's key in one domain and its next unallocated sequence; clones share their
/// accounting.
#[derive(Clone, Debug)]
pub(crate) struct Claim {
	pub key: Arc<Mutex<TrackKey>>,
	// The high-water mark plus one, kept for the generation's life so no sequence repeats.
	// Locked before the key.
	pub next: Arc<Mutex<u64>>,
}

impl Claim {
	pub(crate) fn new(key: TrackKey) -> Self {
		Self {
			key: Arc::new(Mutex::new(key)),
			next: Arc::default(),
		}
	}

	/// Allocate `sequence`, or the next one if `None`, refusing one already allocated or one
	/// that cannot be represented. Locked across `body`, so a concurrent producer of this name
	/// cannot take the sequence, and committed only if `body` succeeds.
	pub(crate) fn allocate<T>(&self, sequence: Option<u64>, body: impl FnOnce(u64) -> Result<T>) -> Result<(u64, T)> {
		let mut next = self.next.lock().expect("claim sequence");
		let sequence = sequence.unwrap_or(*next);
		if sequence < *next {
			return Err(Error::Reuse);
		}
		check_u53(sequence)?;
		let value = body(sequence)?;
		*next = sequence + 1;
		Ok((sequence, value))
	}
}

/// Per-track, per-domain AES-128-GCM key with invocation and byte accounting.
///
/// Not `Clone`: cloning would duplicate the exhaustion counters.
pub(crate) struct TrackKey {
	key: aead::Key,
	invocations: u64,
	plaintext_bytes: u64,
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
	pub(crate) fn new(key: aead::Key) -> Self {
		Self {
			key,
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
		// Refusals check identity, then size, then budget, the same order in every core.
		nonce(group, frame)?;
		if plaintext.len().saturating_add(TAG_LEN) > payload_limit {
			return Err(Error::Oversize);
		}
		self.reserve(plaintext.len())?;
		let payload = protect(&self.key, group, frame, plaintext, payload_limit)?;
		self.commit(plaintext.len());
		Ok(payload)
	}

	/// Decrypt, counting the attempt whether or not the tag verifies.
	pub(crate) fn open(&mut self, group: u64, frame: u64, payload: &[u8], payload_limit: usize) -> Result<Bytes> {
		nonce(group, frame)?;
		if payload.len() < TAG_LEN || payload.len() > payload_limit {
			return Err(Error::Oversize);
		}
		let plaintext_len = payload.len() - TAG_LEN;
		self.reserve(plaintext_len)?;
		let result = open(&self.key, group, frame, payload, payload_limit);
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
