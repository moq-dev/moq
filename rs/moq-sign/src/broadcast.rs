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
/// Mints a fresh epoch, so a restart never signs a sequence an earlier instance already signed.
/// Announce the wrapped broadcast with [`Self::epoch`] on its route.
#[derive(Clone)]
pub struct Producer {
	inner: moq_net::broadcast::Producer,
	key: SecretKey,
	path: moq_net::Path,
	epoch: moq_net::Epoch,
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
			epoch: moq_net::Epoch::mint(),
			next: Arc::default(),
		}
	}

	/// The epoch this instance signs under, to announce on the broadcast's route.
	pub fn epoch(&self) -> &moq_net::Epoch {
		&self.epoch
	}

	/// Create a track whose frames are signed.
	///
	/// # Errors
	///
	/// A net error if the broadcast already holds the name or has ended.
	pub fn create_track(&self, name: &str, info: impl Into<Option<moq_net::track::Info>>) -> Result<track::Producer> {
		let inner = self.inner.create_track(name, info)?;
		let scope = Scope::new(&self.path, &self.epoch, name);
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
			.field("epoch", &self.epoch)
			.finish()
	}
}

/// The subscribing side of a signed broadcast.
///
/// Every frame must verify against the expected publisher key under the epoch of the route the
/// broadcast was resolved through. The route comes from the relay, so this checks that frames
/// belong together, not that the instance is live: a relay can replay a recorded instance whole.
#[derive(Clone)]
pub struct Consumer {
	inner: moq_net::broadcast::Consumer,
	key: PublicKey,
	path: moq_net::Path,
	epoch: moq_net::Epoch,
}

impl Consumer {
	/// Verify `inner` against `key`, as the broadcast at application path `path`.
	///
	/// # Errors
	///
	/// [`Error::Epoch`] if `inner` was not resolved through a route that names its epoch.
	pub fn new(inner: moq_net::broadcast::Consumer, key: PublicKey, path: moq_net::Path) -> Result<Self> {
		let epoch = inner.info().epoch.clone().ok_or(Error::Epoch)?;
		Ok(Self {
			inner,
			key,
			path,
			epoch,
		})
	}

	/// The epoch frames must be signed under.
	pub fn epoch(&self) -> &moq_net::Epoch {
		&self.epoch
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
		let scope = Scope::new(&self.path, &self.epoch, name);
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
			.field("epoch", &self.epoch)
			.finish()
	}
}
