//! Signed broadcasts, named by the application path both ends agree on.
//!
//! A relay may mount a broadcast under another prefix, so the path a signature binds is the one
//! the application names, never the path the transport reports.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::error::{Error, Result};
use crate::key::{PublicKey, SecretKey};
use crate::scope::Scope;
use crate::track;

/// The publishing side of a signed broadcast.
///
/// Mints a fresh identity, so a restart never signs a sequence an earlier instance already signed.
/// Announce the wrapped broadcast with [`Self::identity`] on its route.
#[derive(Clone)]
pub struct Producer {
	inner: moq_net::broadcast::Producer,
	key: SecretKey,
	path: moq_net::Path,
	identity: moq_net::Identity,
	// Each track's lowest unsigned sequence, kept across the track's replacements.
	next: Arc<Mutex<HashMap<String, Arc<Mutex<u64>>>>>,
}

impl Producer {
	/// Sign `inner` with `key`, as the broadcast at application path `path`.
	pub fn new(inner: moq_net::broadcast::Producer, key: SecretKey, path: moq_net::Path) -> Self {
		Self {
			inner,
			key,
			path,
			identity: moq_net::Identity::random(),
			next: Arc::default(),
		}
	}

	/// The identity this instance signs under, to announce on the broadcast's route.
	pub fn identity(&self) -> &moq_net::Identity {
		&self.identity
	}

	/// Create a track whose frames are signed.
	///
	/// # Errors
	///
	/// A net error if the broadcast already holds the name or has ended.
	pub fn create_track(&self, name: &str, info: impl Into<Option<moq_net::track::Info>>) -> Result<track::Producer> {
		let inner = self.inner.create_track(name, info)?;
		let scope = Scope::new(&self.path, &self.identity, name);
		let next = Arc::clone(
			self.next
				.lock()
				.expect("signed sequences")
				.entry(name.to_owned())
				.or_default(),
		);
		Ok(track::Producer::new(inner, self.key.clone(), scope, next))
	}
}

impl std::fmt::Debug for Producer {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("broadcast::Producer")
			.field("key", &self.key.public())
			.field("path", &self.path)
			.field("identity", &self.identity)
			.finish()
	}
}

/// The subscribing side of a signed broadcast.
///
/// Every frame must verify against the expected publisher key under the identity of the route the
/// broadcast was resolved through. The route comes from the relay, so this checks that frames
/// belong together, not that the instance is live: a relay can replay a recorded instance whole.
#[derive(Clone)]
pub struct Consumer {
	inner: moq_net::broadcast::Consumer,
	key: PublicKey,
	path: moq_net::Path,
	identity: moq_net::Identity,
}

impl Consumer {
	/// Verify `inner` against `key`, as the broadcast at application path `path`.
	///
	/// # Errors
	///
	/// [`Error::Identity`] if `inner` was not resolved through a route that carries its
	/// publisher's identity: one a relay [minted](moq_net::Identity::is_minted) for a wire without
	/// identities names the announcement, not the publisher.
	pub fn new(inner: moq_net::broadcast::Consumer, key: PublicKey, path: moq_net::Path) -> Result<Self> {
		let identity = inner
			.info()
			.identity
			.clone()
			.filter(|identity| !identity.is_minted())
			.ok_or(Error::Identity)?;
		Ok(Self {
			inner,
			key,
			path,
			identity,
		})
	}

	/// The identity frames must be signed under.
	pub fn identity(&self) -> &moq_net::Identity {
		&self.identity
	}

	/// Subscribe to a track whose frames must verify.
	///
	/// # Errors
	///
	/// A net error if the broadcast has ended.
	pub fn subscribe(
		&self,
		name: &str,
		subscription: impl Into<Option<moq_net::track::Subscription>>,
	) -> Result<kio::Pending<track::Subscribing>> {
		let inner = self.inner.track(name)?;
		let scope = Scope::new(&self.path, &self.identity, name);
		Ok(kio::Pending::new(track::Subscribing::new(
			inner.subscribe(subscription),
			self.key,
			scope,
		)))
	}
}

impl std::fmt::Debug for Consumer {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("broadcast::Consumer")
			.field("key", &self.key)
			.field("path", &self.path)
			.field("identity", &self.identity)
			.finish()
	}
}
