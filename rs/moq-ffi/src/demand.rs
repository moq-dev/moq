//! Watching whether anyone still wants a published track or a requested group.

use std::sync::Arc;

use crate::error::MoqError;

/// A watch-only handle to a published track's subscriber demand.
///
/// Weak: holding it neither keeps the track open nor locks the producer it came from, so a wait
/// can park here while the producer keeps publishing. Waits fail with `Closed` once the track is
/// released.
#[derive(uniffi::Object)]
pub struct MoqTrackDemand {
	inner: moq_net::track::Demand,
}

impl MoqTrackDemand {
	pub(crate) fn new(inner: moq_net::track::Demand) -> Arc<Self> {
		Arc::new(Self { inner })
	}
}

#[uniffi::export]
impl MoqTrackDemand {
	/// The name of the track this watches.
	pub fn name(&self) -> String {
		self.inner.name().to_string()
	}

	/// Whether the track has at least one active consumer right now, without waiting.
	pub fn is_used(&self) -> bool {
		self.inner.is_used()
	}

	/// Wait until the track has at least one active consumer.
	pub async fn used(&self) -> Result<(), MoqError> {
		let demand = self.inner.clone();
		crate::ffi::detached(async move { gone(demand.used().await) }).await
	}

	/// Wait until the track has no active consumers.
	pub async fn unused(&self) -> Result<(), MoqError> {
		let demand = self.inner.clone();
		crate::ffi::detached(async move { gone(demand.unused().await) }).await
	}
}

/// A watch-only handle to the callers waiting on a requested group.
///
/// Weak: holding it does not keep the request alive. The last caller to leave withdraws the
/// request, and a later fetch of the group queues a fresh one, so once unused, demand never
/// returns: drop the request. Waits fail with `Closed` once the request is released.
#[derive(uniffi::Object)]
pub struct MoqGroupDemand {
	inner: moq_net::group::Demand,
}

impl MoqGroupDemand {
	pub(crate) fn new(inner: moq_net::group::Demand) -> Arc<Self> {
		Arc::new(Self { inner })
	}
}

#[uniffi::export]
impl MoqGroupDemand {
	/// The sequence of the group this watches.
	pub fn sequence(&self) -> u64 {
		self.inner.sequence()
	}

	/// Whether the group has at least one waiting caller right now, without waiting.
	pub fn is_used(&self) -> bool {
		self.inner.is_used()
	}

	/// Wait until the group has at least one waiting caller.
	pub async fn used(&self) -> Result<(), MoqError> {
		let demand = self.inner.clone();
		crate::ffi::detached(async move { gone(demand.used().await) }).await
	}

	/// Wait until the group has no waiting callers.
	pub async fn unused(&self) -> Result<(), MoqError> {
		let demand = self.inner.clone();
		crate::ffi::detached(async move { gone(demand.unused().await) }).await
	}
}

/// Report a track or group released without an abort as [`MoqError::Closed`].
///
/// That is how a finished track or request ends, which a demand watcher expects, not the internal
/// failure `Dropped` means to a consumer. An aborted track or rejected request keeps its reason.
fn gone(result: Result<(), moq_net::Error>) -> Result<(), MoqError> {
	match result {
		Err(moq_net::Error::Dropped) => Err(MoqError::Closed),
		result => Ok(result?),
	}
}
