//! Watching whether anyone is subscribed to a published track.

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

/// Report a track released without an abort as [`MoqError::Closed`].
///
/// That is how a finished track ends, which a demand watcher expects, not the internal failure
/// `Dropped` means to a consumer. An aborted track keeps its reason.
fn gone(result: Result<(), moq_net::Error>) -> Result<(), MoqError> {
	match result {
		Err(moq_net::Error::Dropped) => Err(MoqError::Closed),
		result => Ok(result?),
	}
}
