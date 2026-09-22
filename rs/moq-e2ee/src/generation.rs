use std::collections::HashSet;
use std::fmt;
use std::sync::{Arc, Mutex};

use crate::credential::{Credential, append_bytes};
use crate::epoch::Epoch;
use crate::error::{Error, Result};
use crate::key::TrackKey;
use crate::limits::{KEY_LABEL, KEY_LEN, NAME_LABEL};
use crate::name::{Name, encode};
use crate::track;

/// Grouped-frame versus datagram key domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Domain {
	Group = 0x00,
	Datagram = 0x01,
}

struct Inner {
	credential: Credential,
	epoch: Epoch,
	// Physical names this generation has produced. Never released: a track reopened
	// under the same epoch would restart its sequences and repeat nonces.
	claims: Mutex<HashSet<Name>>,
}

/// One credential under one epoch: the scope of every name, key, and nonce.
///
/// Clones share the publisher claims, so a physical track is produced at most once per generation.
#[derive(Clone)]
pub struct Generation(Arc<Inner>);

impl fmt::Debug for Generation {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Generation")
			.field("credential", &self.0.credential)
			.field("epoch", &self.0.epoch)
			.finish()
	}
}

impl Generation {
	pub(crate) fn new(credential: Credential, epoch: Epoch) -> Self {
		Self(Arc::new(Inner {
			credential,
			epoch,
			claims: Mutex::new(HashSet::new()),
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

	/// Protect a net track whose name is a physical name, claiming it for this generation.
	///
	/// # Errors
	///
	/// [`Error::Identity`] if the track name is not a physical name, [`Error::Reuse`] if
	/// this generation already produced it.
	pub fn produce(&self, track: moq_net::track::Producer) -> Result<track::Producer> {
		let name: Name = track.name().parse()?;
		if !self.0.claims.lock().expect("claims").insert(name) {
			return Err(Error::Reuse);
		}
		Ok(track::Producer::new(
			track,
			self.key(&name, Domain::Group)?,
			self.key(&name, Domain::Datagram)?,
		))
	}

	/// Open a net subscription whose track name is a physical name.
	///
	/// # Errors
	///
	/// [`Error::Identity`] if the track name is not a physical name.
	pub fn consume(&self, track: moq_net::track::Subscriber) -> Result<track::Consumer> {
		let name: Name = track.name().parse()?;
		Ok(track::Consumer::new(
			track,
			self.key(&name, Domain::Group)?,
			self.key(&name, Domain::Datagram)?,
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
