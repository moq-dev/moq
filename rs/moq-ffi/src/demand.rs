//! Watching what subscribers want of a published track.

use std::sync::Arc;

use crate::error::MoqError;

/// A watch-only handle to whether a published track has subscribers, and when they ask for a new
/// group.
///
/// Weak: holding it neither keeps the track open nor locks the producer it came from, so a wait
/// can park here while the producer keeps publishing. Waits fail with `Closed` once the track is
/// released.
#[derive(uniffi::Object)]
pub struct MoqTrackDemand {
	inner: moq_net::track::Demand,
	// Each poll advances one shared cursor; a parked call never holds it locked.
	changes: crate::ffi::Polled<moq_net::track::Demand>,
}

impl MoqTrackDemand {
	pub(crate) fn new(inner: moq_net::track::Demand) -> Arc<Self> {
		let changes = crate::ffi::Polled::new(inner.clone());
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

	/// Whether anyone consumes the track: a subscription, query, fetch, or a group one of them
	/// was handed and has not finished reading.
	pub fn wanted(&self) -> bool {
		self.inner.wanted()
	}

	/// Wait for `wanted` to change and return the new value. The first call returns at once.
	///
	/// End a track nobody wants with its producer's `abort_unwanted`, which keeps it for a reader
	/// that joined since.
	pub async fn wanted_changed(&self) -> Result<bool, MoqError> {
		self.changes
			.wait(|changes, waiter| changes.poll_wanted_changed(waiter).map(gone))
			.await
	}

	/// Wait until a subscriber asks for a new group: the first group of a track with none, or a
	/// join or refresh judged by the track's [`MoqTrackRefresh`](crate::producer::MoqTrackRefresh)
	/// once its interval has passed. Returns at once while the request is pending, and the next
	/// group written answers it, so loop: wait here, then write a group that starts with a
	/// keyframe or a snapshot.
	pub async fn refresh(&self) -> Result<(), MoqError> {
		crate::ffi::run(kio::wait(|waiter| self.inner.poll_refresh(waiter).map(gone))).await
	}
}

#[cfg(test)]
impl MoqTrackDemand {
	/// Wait until some subscriber wants the track.
	pub(crate) async fn until_wanted(&self) -> Result<(), MoqError> {
		while !self.wanted_changed().await? {}
		Ok(())
	}

	/// Wait until no subscriber wants the track.
	pub(crate) async fn until_unwanted(&self) -> Result<(), MoqError> {
		while self.wanted_changed().await? {}
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
