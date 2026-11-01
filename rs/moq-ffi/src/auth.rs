//! What the peer lets a session do.

use crate::error::MoqError;
use crate::ffi::Task;

/// What the peer lets this side do, in this side's own paths.
///
/// An empty pattern list grants nothing in that direction; `**` grants everything.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MoqGrant {
	/// The broadcast patterns this side may publish to the peer.
	pub publish: Vec<String>,
	/// The broadcast patterns this side may subscribe to from the peer.
	pub subscribe: Vec<String>,
	/// How long until the grant lapses, measured when it was read, or `None` for never.
	pub expires: Option<std::time::Duration>,
}

impl From<moq_net::auth::Grant> for MoqGrant {
	fn from(grant: moq_net::auth::Grant) -> Self {
		let patterns = |patterns: &moq_net::Patterns| patterns.iter().map(ToString::to_string).collect();
		Self {
			publish: patterns(&grant.publish),
			subscribe: patterns(&grant.subscribe),
			expires: grant
				.expires
				.map(|expires| crate::ffi::duration(expires.saturating_duration_since(moq_net::time::Instant::now()))),
		}
	}
}

/// Follows the grant a session's peer gave it: the union of every token the session
/// presented.
///
/// Returned by [`MoqSession::grants`](crate::session::MoqSession::grants). AUTH is only on
/// `moq-lite-07-wip` and on moq-transport sessions that negotiate the MoQ Auth extension;
/// elsewhere the grant stays `None`.
#[derive(uniffi::Object)]
pub struct MoqGrantConsumer {
	/// The watch behind [`grant`](Self::grant), which reads without the lock.
	peek: moq_net::auth::Watch,
	/// The watch behind [`changed`](Self::changed), which tracks what it last returned.
	changed: Task<moq_net::auth::Watch>,
}

impl MoqGrantConsumer {
	pub(crate) fn new(handle: &moq_net::auth::Handle) -> Self {
		Self {
			peek: handle.grant(),
			changed: Task::new(handle.grant()),
		}
	}
}

#[uniffi::export]
impl MoqGrantConsumer {
	/// The grant now, or `None` until the peer first answers a token. A client session reads
	/// `None` while it reconnects.
	pub fn grant(&self) -> Option<MoqGrant> {
		let _guard = crate::ffi::enter();
		self.peek.peek().map(Into::into)
	}

	/// Wait for the grant to differ from the one this last returned, or held when it was
	/// made, returning it.
	///
	/// Errors once nothing will change it again: a client's connection or a server's
	/// session ended, or [`cancel`](Self::cancel). A reconnect is not an end.
	pub async fn changed(&self) -> Result<Option<MoqGrant>, MoqError> {
		self.changed
			.run(|mut watch| async move { Ok(watch.changed().await?.map(Into::into)) })
			.await
	}

	/// Cancel all current and future [`changed`](Self::changed) calls.
	pub fn cancel(&self) {
		self.changed.cancel();
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	/// Cancelling releases a wait no grant change will end.
	#[tokio::test]
	async fn cancel_ends_a_pending_wait() {
		let handle = moq_net::auth::Handle::new();
		let grants = std::sync::Arc::new(super::MoqGrantConsumer::new(&handle));
		let waiting = tokio::spawn({
			let grants = grants.clone();
			async move { grants.changed().await }
		});
		// Let the wait park before cancelling it.
		tokio::task::yield_now().await;
		grants.cancel();
		let result = kio::clock::timeout(Duration::from_secs(5), waiting)
			.await
			.expect("the wait outlived its cancel")
			.unwrap();
		assert!(matches!(result, Err(crate::error::MoqError::Cancelled)), "{result:?}");
	}
}
