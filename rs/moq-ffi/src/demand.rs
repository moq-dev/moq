//! Watching what subscribers want of a published track.

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

/// Report a track released without an abort as [`MoqError::Closed`].
///
/// That is how a finished track ends, which a demand watcher expects, not the internal failure
/// `Dropped` means to a consumer. An aborted track keeps its reason.
fn gone<T>(result: Result<T, moq_net::Error>) -> Result<T, MoqError> {
	match result {
		Err(moq_net::Error::Dropped) => Err(MoqError::Closed),
		result => Ok(result?),
	}
}
