//! What the peer lets a session do.

use crate::error::MoqError;
use crate::ffi::Polled;

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
/// `moq-lite-07-wip`; elsewhere the grant stays `None`.
#[derive(uniffi::Object)]
pub struct MoqGrantConsumer {
	/// The watch behind [`grant`](Self::grant), which reads without the lock.
	peek: moq_net::auth::Watch,
	/// The watch behind [`changed`](Self::changed), which tracks what it last returned.
	changed: Polled<moq_net::auth::Watch>,
}

impl MoqGrantConsumer {
	pub(crate) fn new(handle: &moq_net::auth::Handle) -> Self {
		Self {
			peek: handle.grant(),
			changed: Polled::new(handle.grant()),
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
			.wait(|watch, waiter| {
				std::task::Poll::Ready(Ok(std::task::ready!(watch.poll_changed(waiter))?.map(Into::into)))
			})
			.await
	}

	/// Cancel all current and future [`changed`](Self::changed) calls.
	pub fn cancel(&self) {
		self.changed.cancel();
	}
}

/// A token this side presented on a session, which a route or claim names with `set_auth`.
///
/// Returned by [`MoqSession::credential`](crate::session::MoqSession::credential). It stays
/// presented until [`cancel`](Self::cancel) or the last handle is released and no route or
/// claim names it, answered or not.
#[derive(uniffi::Object)]
pub struct MoqCredential {
	/// The credential itself, released on cancel.
	inner: Polled<moq_net::auth::Credential>,
}

impl MoqCredential {
	pub(crate) fn new(credential: moq_net::auth::Credential) -> Self {
		Self {
			inner: Polled::new(credential),
		}
	}

	/// The credential to name on a route or claim, refused with `Cancelled` once cancelled.
	pub(crate) fn credential(&self) -> Result<moq_net::auth::Credential, MoqError> {
		self.inner.peek(Clone::clone).ok_or(MoqError::Cancelled)
	}
}

#[uniffi::export]
impl MoqCredential {
	/// Wait for the peer's first verdict on the token: `Ok` once a peer accepts it, at once on
	/// moq-transport, where only the requests that name it carry the token.
	///
	/// The peer's refusal is a [`MoqError::Protocol`] carrying its code,
	/// [`MoqError::Unsupported`] when neither the session nor its requests can carry a token
	/// (moq-lite without AUTH), and [`MoqError::Cancelled`] once [`cancel`](Self::cancel) ran.
	pub async fn answered(&self) -> Result<(), MoqError> {
		self.inner
			.wait(|credential, waiter| credential.poll_answered(waiter).map_err(Into::into))
			.await
	}

	/// Release the credential here, not when the handle is: withdrawn at once unless a route
	/// or claim still names it, and every [`answered`](Self::answered) call ends with
	/// `Cancelled`. Terminal.
	pub fn cancel(&self) {
		self.inner.cancel();
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
