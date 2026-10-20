//! Watching what subscribers want of a published track, or whether a requested group is still wanted.

use std::sync::Arc;

use crate::error::MoqError;

/// A watch-only handle to the group a published track's subscribers want next.
///
/// Weak: holding it neither keeps the track open nor locks the producer it came from, so a wait
/// can park here while the producer keeps publishing. Waits fail with `Closed` once the track is
/// released.
#[derive(uniffi::Object)]
pub struct MoqTrackDemand {
	inner: moq_net::track::Demand,
	// The watcher `wanted_changed` advances, kept apart so concurrent waits queue in turn.
	changes: Arc<tokio::sync::Mutex<moq_net::track::Demand>>,
}

impl MoqTrackDemand {
	pub(crate) fn new(inner: moq_net::track::Demand) -> Arc<Self> {
		let changes = Arc::new(tokio::sync::Mutex::new(inner.clone()));
		Arc::new(Self { inner, changes })
	}

	pub(crate) fn inner(&self) -> &moq_net::track::Demand {
		&self.inner
	}
}

#[uniffi::export]
impl MoqTrackDemand {
	/// The name of the track this watches.
	pub fn name(&self) -> String {
		self.inner.name().to_string()
	}

	/// The group subscribers want next: null while nobody consumes the track, otherwise the
	/// sequence the next group should have at least. A value past the newest group asks for a
	/// new group now, typically a keyframe.
	pub fn wanted(&self) -> Option<u64> {
		self.inner.wanted()
	}

	/// Wait for `wanted` to change and return the new value. The first call returns at once.
	pub async fn wanted_changed(&self) -> Result<Option<u64>, MoqError> {
		let changes = self.changes.clone();
		crate::ffi::detached(async move { gone(changes.lock().await.wanted_changed().await) }).await
	}
}

#[cfg(test)]
impl MoqTrackDemand {
	/// Wait until some subscriber wants the track.
	pub(crate) async fn until_wanted(&self) -> Result<(), MoqError> {
		while self.wanted_changed().await?.is_none() {}
		Ok(())
	}

	/// Wait until no subscriber wants the track.
	pub(crate) async fn until_unwanted(&self) -> Result<(), MoqError> {
		while self.wanted_changed().await?.is_some() {}
		Ok(())
	}
}

/// A watch-only handle to the callers waiting on a requested group.
///
/// Weak: holding it does not keep the request alive. The last caller to leave withdraws the
/// request, and a later fetch of the group queues a fresh one, so once unused, demand never
/// returns: drop the request. Waits fail with `Closed` after the request is accepted or dropped,
/// and keep the rejection reason after it is rejected.
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
fn gone<T>(result: Result<T, moq_net::Error>) -> Result<T, MoqError> {
	match result {
		Err(moq_net::Error::Dropped) => Err(MoqError::Closed),
		result => Ok(result?),
	}
}
