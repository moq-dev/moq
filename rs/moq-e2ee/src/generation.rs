//! Producing generations mint an epoch; consuming generations bind a discovered one.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fmt;
use std::sync::{Arc, Mutex};

use crate::credential::{Credential, append_bytes};
use crate::error::{Error, Result};
use crate::key::TrackKey;
use crate::limits::{KEY_LABEL, KEY_LEN, NAME_LABEL};
use crate::name::{Name, encode};
use crate::track;
use moq_net::Epoch;

/// Grouped-frame versus datagram key domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Domain {
	Group = 0x00,
	Datagram = 0x01,
}

struct Inner {
	credential: Credential,
	epoch: Epoch,
	// Never evicted: re-deriving a key would reset its budget. Bounded by the semantic
	// names the application produces or subscribes to, for as long as it holds the generation.
	keys: Mutex<HashMap<Name, track::Keys>>,
}

/// A consume-only generation bound to a discovered epoch.
///
/// Every subscription through it and its clones shares each key's AEAD budget, so
/// resubscribing cannot reset it. The budget lives as long as the generation; binding
/// the epoch again with [`Credential::consume`] starts another.
///
/// A discovered epoch cannot produce tracks:
///
/// ```compile_fail
/// fn encrypt(generation: moq_e2ee::generation::Consumer, broadcast: moq_net::broadcast::Producer) {
///     moq_e2ee::broadcast::Producer::new(broadcast, generation);
/// }
/// ```
#[derive(Clone)]
pub struct Consumer(Arc<Inner>);

impl fmt::Debug for Consumer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Consumer")
			.field("credential", &self.0.credential)
			.field("epoch", &self.0.epoch)
			.finish()
	}
}

impl Consumer {
	pub(crate) fn new(credential: Credential, epoch: Epoch) -> Self {
		Self(Arc::new(Inner {
			credential,
			epoch,
			keys: Mutex::default(),
		}))
	}

	/// The credential this generation derives from.
	pub fn credential(&self) -> &Credential {
		&self.0.credential
	}

	/// The epoch this generation is scoped to.
	pub fn epoch(&self) -> &Epoch {
		&self.0.epoch
	}

	/// The opaque physical track name for a semantic track name.
	///
	/// # Errors
	///
	/// [`Error::Identity`] if `semantic` exceeds 65535 bytes.
	pub fn name(&self, semantic: &str) -> Result<Name> {
		let material = self.0.credential.expand(&self.name_info(semantic)?)?;
		Ok(encode(material))
	}

	/// Open a net subscription whose track name is a physical name.
	///
	/// # Errors
	///
	/// [`Error::Identity`] if the track name is not a physical name.
	pub(crate) fn consume(&self, track: moq_net::track::Subscriber) -> Result<track::Consumer> {
		let name: Name = track.name().parse()?;
		let keys = self.keys(name)?;
		Ok(track::Consumer::new(track, keys))
	}

	/// The keys for `name`, shared with every earlier subscription to it.
	pub(crate) fn keys(&self, name: Name) -> Result<track::Keys> {
		let mut keys = self.0.keys.lock().expect("generation keys");
		let entry = match keys.entry(name) {
			Entry::Occupied(entry) => entry.into_mut(),
			Entry::Vacant(entry) => {
				let derived = self.derive(entry.key())?;
				entry.insert(derived)
			}
		};
		Ok(entry.clone())
	}

	/// Derive fresh keys for `name`, with their own accounting.
	fn derive(&self, name: &Name) -> Result<track::Keys> {
		Ok(track::Keys::new(
			self.key(name, Domain::Group)?,
			self.key(name, Domain::Datagram)?,
		))
	}

	pub(crate) fn key(&self, name: &Name, domain: Domain) -> Result<TrackKey> {
		Ok(TrackKey::new(self.key_bytes(name, domain)?))
	}

	pub(crate) fn key_bytes(&self, name: &Name, domain: Domain) -> Result<[u8; KEY_LEN]> {
		self.0.credential.expand(&self.key_info(name, domain)?)
	}

	pub(crate) fn name_info(&self, semantic: &str) -> Result<Vec<u8>> {
		let mut info = Vec::with_capacity(NAME_LABEL.len() + 64 + semantic.len());
		info.extend_from_slice(NAME_LABEL);
		self.scope(&mut info)?;
		append_bytes(&mut info, semantic.as_bytes())?;
		Ok(info)
	}

	pub(crate) fn key_info(&self, name: &Name, domain: Domain) -> Result<Vec<u8>> {
		let mut info = Vec::with_capacity(KEY_LABEL.len() + 64 + name.as_str().len() + 1);
		info.extend_from_slice(KEY_LABEL);
		self.scope(&mut info)?;
		append_bytes(&mut info, name.as_str().as_bytes())?;
		info.push(domain as u8);
		Ok(info)
	}

	/// The fields every epoch-scoped derivation binds: `bytes(context) || bytes(epoch) || u64(kid)`.
	fn scope(&self, info: &mut Vec<u8>) -> Result<()> {
		append_bytes(info, self.0.credential.context())?;
		append_bytes(info, self.0.epoch.as_str().as_bytes())?;
		info.extend_from_slice(&self.0.credential.kid().to_be_bytes());
		Ok(())
	}
}

/// A publisher-minted generation whose clones share permanent track claims.
#[derive(Clone, Debug)]
pub struct Producer {
	// Its keys are the claims: never released, since reopening a track would restart its nonce counters.
	consumer: Consumer,
}

impl Producer {
	pub(crate) fn new(credential: Credential, epoch: Epoch) -> Self {
		Self {
			consumer: Consumer::new(credential, epoch),
		}
	}

	/// The credential this generation derives from.
	pub fn credential(&self) -> &Credential {
		self.consumer.credential()
	}

	/// The epoch minted for this publisher instance.
	pub fn epoch(&self) -> &Epoch {
		self.consumer.epoch()
	}

	/// Derive the opaque physical track name, refusing semantic names longer than 65535 bytes.
	pub fn name(&self, semantic: &str) -> Result<Name> {
		self.consumer.name(semantic)
	}

	/// Protect a net track whose name is a physical name, claiming it for this generation.
	///
	/// # Errors
	///
	/// [`Error::Identity`] if the track name is not a physical name, [`Error::Reuse`] if
	/// this generation already produced it. Either way `track` is aborted.
	pub(crate) fn produce(&self, track: moq_net::track::Producer) -> Result<track::Producer> {
		match self.claim(track.name()) {
			Ok(keys) => Ok(track::Producer::new(track, keys)),
			// Its subscribers should see the refusal, not the track vanish.
			Err(err) => {
				let _ = track.abort(moq_net::Error::Cancel);
				Err(err)
			}
		}
	}

	/// Claim a physical name for good, returning its keys.
	///
	/// # Errors
	///
	/// [`Error::Identity`] if `name` is not a physical name, [`Error::Reuse`] if this
	/// generation already claimed it.
	pub(crate) fn claim(&self, name: &str) -> Result<track::Keys> {
		let name: Name = name.parse()?;
		let mut keys = self.consumer.0.keys.lock().expect("generation keys");
		let Entry::Vacant(entry) = keys.entry(name) else {
			return Err(Error::Reuse);
		};
		let derived = self.consumer.derive(entry.key())?;
		Ok(entry.insert(derived).clone())
	}
}
