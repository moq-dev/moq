//! Producing generations mint an epoch; consuming generations bind a discovered one.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use crate::e2ee::Epoch;
use crate::e2ee::credential::{Credential, append_bytes};
use crate::e2ee::error::Result;
use crate::e2ee::key::{Claim, TrackKey};
use crate::e2ee::limits::{KEY_LABEL, NAME_KEY_LABEL, NAME_MAC_LABEL};
use crate::e2ee::name::{self, Name};
use crate::e2ee::{datagram, track};
use rind::{aead, hmac};

/// Grouped-frame versus datagram key domain.
///
/// A stream track and a datagram track of one semantic name share a physical name but allocate
/// sequences independently, so the domain alone keeps a group's frame 0 and the datagram at
/// that sequence from sharing a key and nonce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Domain {
	Group = 0x00,
	Datagram = 0x01,
}

struct Inner {
	credential: Credential,
	epoch: Epoch,
	names: name::Keys,
	// One map per domain, keyed by semantic name. Never evicted: re-deriving a key would reset its
	// budget and, for a producer, its sequences. Bounded by the semantic names the application
	// produces, serves, or subscribes to, for as long as it holds the generation.
	groups: Mutex<HashMap<String, Claim>>,
	datagrams: Mutex<HashMap<String, Claim>>,
}

impl Inner {
	fn claims(&self, domain: Domain) -> &Mutex<HashMap<String, Claim>> {
		match domain {
			Domain::Group => &self.groups,
			Domain::Datagram => &self.datagrams,
		}
	}
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
/// fn encrypt(generation: moq_crypto::e2ee::generation::Consumer, broadcast: moq_net::broadcast::Producer) {
///     moq_crypto::e2ee::broadcast::Producer::new(broadcast, generation);
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
		let names = name::Keys::new(
			credential.derive(&name_mac_info(&credential, &epoch), &hmac::HMAC_SHA256),
			credential.derive(&name_key_info(&credential, &epoch), &aead::AES_128_GCM),
		);
		Self(Arc::new(Inner {
			credential,
			epoch,
			names,
			groups: Mutex::default(),
			datagrams: Mutex::default(),
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
	/// [`Error::Identity`](crate::e2ee::Error::Identity) if `semantic` exceeds 65535 bytes.
	pub fn name(&self, semantic: &str) -> Result<Name> {
		self.0.names.seal(semantic)
	}

	/// Decrypt a net subscription to the physical name of `semantic`, whose keys it binds.
	///
	/// # Errors
	///
	/// [`Error::Identity`](crate::e2ee::Error::Identity) if `semantic` exceeds 65535 bytes.
	pub(crate) fn consume(&self, track: moq_net::track::Subscriber, semantic: &str) -> Result<track::Consumer> {
		Ok(track::Consumer::new(track, self.keys(semantic, Domain::Group)?.key))
	}

	/// Decrypt a net datagram track named by the physical name of `semantic`, whose datagram key it binds.
	///
	/// # Errors
	///
	/// [`Error::Identity`](crate::e2ee::Error::Identity) if `semantic` exceeds 65535 bytes.
	pub(crate) fn consume_datagrams(
		&self,
		track: moq_net::datagram::Consumer,
		semantic: &str,
	) -> Result<datagram::Consumer> {
		Ok(datagram::Consumer::new(
			track,
			self.keys(semantic, Domain::Datagram)?.key,
		))
	}

	/// The `domain` key for `semantic`, shared with every earlier subscription to or claim of it.
	pub(crate) fn keys(&self, semantic: &str, domain: Domain) -> Result<Claim> {
		let mut claims = self.0.claims(domain).lock().expect("generation keys");
		if let Some(claim) = claims.get(semantic) {
			return Ok(claim.clone());
		}
		let derived = Claim::new(self.key(semantic, domain)?);
		Ok(claims.entry(semantic.to_string()).or_insert(derived).clone())
	}

	pub(crate) fn key(&self, semantic: &str, domain: Domain) -> Result<TrackKey> {
		let info = self.key_info(semantic, domain)?;
		Ok(TrackKey::new(self.0.credential.derive(&info, &aead::AES_128_GCM)))
	}

	pub(crate) fn key_info(&self, semantic: &str, domain: Domain) -> Result<Vec<u8>> {
		let mut info = Vec::with_capacity(KEY_LABEL.len() + 64 + semantic.len() + 1);
		info.extend_from_slice(KEY_LABEL);
		scope(&mut info, &self.0.credential, &self.0.epoch);
		append_bytes(&mut info, semantic.as_bytes())?;
		info.push(domain as u8);
		Ok(info)
	}
}

/// `"moq-e2ee-00 name-mac" || scope`.
pub(crate) fn name_mac_info(credential: &Credential, epoch: &Epoch) -> Vec<u8> {
	let mut info = NAME_MAC_LABEL.to_vec();
	scope(&mut info, credential, epoch);
	info
}

/// `"moq-e2ee-00 name-key" || scope`.
pub(crate) fn name_key_info(credential: &Credential, epoch: &Epoch) -> Vec<u8> {
	let mut info = NAME_KEY_LABEL.to_vec();
	scope(&mut info, credential, epoch);
	info
}

/// The fields every epoch-scoped derivation binds: `bytes(context) || bytes(epoch) || u64(kid)`.
fn scope(info: &mut Vec<u8>, credential: &Credential, epoch: &Epoch) {
	// The credential bounds its context and an epoch is 36 bytes of UUID text.
	append_bytes(info, credential.context()).expect("bounded context");
	append_bytes(info, epoch.as_str().as_bytes()).expect("bounded epoch");
	info.extend_from_slice(&credential.kid().to_be_bytes());
}

/// A publisher-minted generation whose clones share permanent track claims.
///
/// A claim holds a semantic name's key for stream or datagram tracks and that kind's sequence
/// high-water mark, so a track created, reserved, or served again under the name resumes above
/// every sequence its kind allocated before, on any broadcast of the generation.
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

	/// The consuming half, which shares this generation's claims and key accounting.
	pub(crate) fn consumer(&self) -> &Consumer {
		&self.consumer
	}

	/// Recover the semantic name of a physical name this generation issued.
	///
	/// # Errors
	///
	/// [`Error::Authentication`](crate::e2ee::Error::Authentication) if this generation did not issue `name`.
	pub(crate) fn open(&self, name: &Name) -> Result<String> {
		self.consumer.0.names.open(name)
	}

	/// Protect a net track named by the physical name of `semantic`, claiming it for this generation.
	///
	/// # Errors
	///
	/// [`Error::Identity`](crate::e2ee::Error::Identity) if `semantic` exceeds 65535 bytes, and then `track` is aborted.
	pub(crate) fn produce(&self, track: moq_net::track::Producer, semantic: &str) -> Result<track::Producer> {
		match self.claim(semantic, Domain::Group) {
			Ok(claim) => Ok(track::Producer::new(track, claim)),
			// Its subscribers should see the refusal, not the track vanish.
			Err(err) => {
				let _ = track.abort(moq_net::Error::Cancel);
				Err(err)
			}
		}
	}

	/// Protect a net datagram track named by the physical name of `semantic`, claiming it for
	/// this generation.
	///
	/// # Errors
	///
	/// [`Error::Identity`](crate::e2ee::Error::Identity) if `semantic` exceeds 65535 bytes, and then `track` is aborted.
	pub(crate) fn produce_datagrams(
		&self,
		track: moq_net::datagram::Producer,
		semantic: &str,
	) -> Result<datagram::Producer> {
		match self.claim(semantic, Domain::Datagram) {
			Ok(claim) => Ok(datagram::Producer::new(track, claim)),
			Err(err) => {
				track.abort(moq_net::Error::Cancel);
				Err(err)
			}
		}
	}

	/// Claim a semantic name in `domain`, and so its physical name, for good, returning its key
	/// and sequences. Claiming it again resumes the earlier claim.
	///
	/// # Errors
	///
	/// [`Error::Identity`](crate::e2ee::Error::Identity) if `semantic` exceeds 65535 bytes.
	pub(crate) fn claim(&self, semantic: &str, domain: Domain) -> Result<Claim> {
		self.consumer.keys(semantic, domain)
	}

	/// The `domain` key for `semantic` without claiming it: its claim's if it has one, otherwise
	/// a fresh one that holds nothing until [`Self::adopt`].
	///
	/// # Errors
	///
	/// [`Error::Identity`](crate::e2ee::Error::Identity) if `semantic` exceeds 65535 bytes.
	pub(crate) fn peek(&self, semantic: &str, domain: Domain) -> Result<Claim> {
		if let Some(claim) = self
			.consumer
			.0
			.claims(domain)
			.lock()
			.expect("generation keys")
			.get(semantic)
		{
			return Ok(claim.clone());
		}
		Ok(Claim::new(self.consumer.key(semantic, domain)?))
	}

	/// Claim `semantic` in `domain` with a key from [`Self::peek`], resuming any claim made
	/// since instead.
	pub(crate) fn adopt(&self, semantic: &str, domain: Domain, claim: Claim) -> Claim {
		let mut claims = self.consumer.0.claims(domain).lock().expect("generation keys");
		claims.entry(semantic.to_string()).or_insert(claim).clone()
	}

	/// How many semantic names this generation holds keys for, across both domains.
	#[cfg(test)]
	pub(crate) fn claims(&self) -> usize {
		let inner = &self.consumer.0;
		[Domain::Group, Domain::Datagram]
			.into_iter()
			.map(|domain| inner.claims(domain).lock().expect("generation keys").len())
			.sum()
	}
}
