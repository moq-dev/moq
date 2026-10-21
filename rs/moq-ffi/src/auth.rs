//! In-band authorization: what the peer lets a session do, and more tokens to present.

use std::sync::Arc;

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
	/// Microseconds until the grant lapses, measured when it was read, or `None` for never.
	pub expires_us: Option<u64>,
}

impl From<moq_net::auth::Grant> for MoqGrant {
	fn from(grant: moq_net::auth::Grant) -> Self {
		let patterns = |patterns: &moq_net::Patterns| patterns.iter().map(ToString::to_string).collect();
		Self {
			publish: patterns(&grant.publish),
			subscribe: patterns(&grant.subscribe),
			expires_us: grant.expires.map(|expires| {
				let remaining = expires.saturating_duration_since(moq_net::time::now());
				u64::try_from(remaining.as_micros()).unwrap_or(u64::MAX)
			}),
		}
	}
}

/// Where a session's tokens live: on the session itself, or on a connection that presents
/// them again on every reconnect.
#[derive(Clone)]
pub(crate) enum Handle {
	Session(moq_net::auth::Handle),
	#[cfg(not(target_arch = "wasm32"))]
	Connection(moq_tokio::connection::auth::Handle),
}

impl Handle {
	fn grant(&self) -> Watch {
		match self {
			Self::Session(handle) => Watch::Session(handle.grant()),
			#[cfg(not(target_arch = "wasm32"))]
			Self::Connection(handle) => Watch::Connection(handle.grant()),
		}
	}

	async fn add(self, token: String) -> Result<Token, MoqError> {
		match self {
			Self::Session(handle) => Ok(Token::Session(handle.add(token).await?)),
			#[cfg(not(target_arch = "wasm32"))]
			Self::Connection(handle) => Ok(Token::Connection(
				handle.add(token).await.map_err(crate::session::map_closed_error)?,
			)),
		}
	}
}

enum Watch {
	Session(moq_net::auth::Watch),
	#[cfg(not(target_arch = "wasm32"))]
	Connection(moq_tokio::connection::auth::Watch),
}

impl Watch {
	fn peek(&self) -> Option<MoqGrant> {
		match self {
			Self::Session(watch) => watch.peek(),
			#[cfg(not(target_arch = "wasm32"))]
			Self::Connection(watch) => watch.peek(),
		}
		.map(Into::into)
	}

	async fn changed(&mut self) -> Result<Option<MoqGrant>, MoqError> {
		let grant = match self {
			Self::Session(watch) => watch.changed().await?,
			#[cfg(not(target_arch = "wasm32"))]
			Self::Connection(watch) => watch.changed().await.map_err(crate::session::map_closed_error)?,
		};
		Ok(grant.map(Into::into))
	}
}

enum Token {
	Session(moq_net::auth::Auth),
	#[cfg(not(target_arch = "wasm32"))]
	Connection(moq_tokio::connection::auth::Auth),
}

impl Token {
	fn grant(&self) -> Watch {
		match self {
			Self::Session(auth) => Watch::Session(auth.grant()),
			#[cfg(not(target_arch = "wasm32"))]
			Self::Connection(auth) => Watch::Connection(auth.grant()),
		}
	}

	async fn closed(&self) -> Result<(), MoqError> {
		match self {
			Self::Session(auth) => match auth.closed().await {
				moq_net::Error::Cancel => Ok(()),
				err => Err(err.into()),
			},
			#[cfg(not(target_arch = "wasm32"))]
			Self::Connection(auth) => auth.closed().await.map_err(crate::session::map_closed_error),
		}
	}
}

/// A session's authorization: the grant its peer gave it, and more tokens to present.
///
/// Returned by [`MoqSession::auth`](crate::session::MoqSession::auth). AUTH is only on
/// `moq-lite-07-wip` and on moq-transport sessions that negotiate the MoQ Auth extension;
/// elsewhere the grant stays `None`.
#[derive(uniffi::Object)]
pub struct MoqAuth {
	handle: Handle,
	/// The watch behind [`grant`](Self::grant), which reads without the lock.
	peek: Watch,
	/// The watch behind [`grant_changed`](Self::grant_changed), which tracks what this
	/// handle last reported.
	changed: Task<Watch>,
}

impl MoqAuth {
	pub(crate) fn new(handle: Handle) -> Self {
		Self {
			peek: handle.grant(),
			changed: Task::new(handle.grant()),
			handle,
		}
	}
}

#[uniffi::export]
impl MoqAuth {
	/// The union of every token's grant, or `None` until the peer first answers one.
	///
	/// A client session reads `None` while it reconnects.
	pub fn grant(&self) -> Option<MoqGrant> {
		let _guard = crate::ffi::enter();
		self.peek.peek()
	}

	/// Wait for the grant to differ from the one this handle last reported, returning it.
	///
	/// Errors once nothing will change it again, because the session ended.
	pub async fn grant_changed(&self) -> Result<Option<MoqGrant>, MoqError> {
		self.changed.run(|mut watch| async move { watch.changed().await }).await
	}

	/// Present another token, resolving once the peer accepts it.
	///
	/// Its grant joins the union until [`MoqAuthToken::cancel`]. A client session presents it
	/// again on every reconnect. The peer's refusal is a [`MoqError::Protocol`] carrying its code.
	///
	/// # Errors
	///
	/// [`MoqError::Unsupported`] when the session cannot carry another token, such as a
	/// moq-lite version without AUTH.
	pub async fn add(&self, token: String) -> Result<Arc<MoqAuthToken>, MoqError> {
		let handle = self.handle.clone();
		let token = crate::ffi::detached(handle.add(token)).await?;
		Ok(Arc::new(MoqAuthToken::new(token)))
	}
}

/// One token presented with [`MoqAuth::add`].
///
/// It stays presented until [`cancel`](Self::cancel), or until the handle is released.
#[derive(uniffi::Object)]
pub struct MoqAuthToken {
	/// The watch behind [`grant`](Self::grant), which does not keep the token presented.
	peek: Watch,
	token: Task<Token>,
}

impl MoqAuthToken {
	fn new(token: Token) -> Self {
		Self {
			peek: token.grant(),
			token: Task::new(token),
		}
	}
}

#[uniffi::export]
impl MoqAuthToken {
	/// This token's own grant, or `None` while the peer has not answered it or after it ended.
	pub fn grant(&self) -> Option<MoqGrant> {
		let _guard = crate::ffi::enter();
		self.peek.peek()
	}

	/// Wait for the token to end: `Ok` when the peer closed it without an error, the peer's
	/// revocation as [`MoqError::Protocol`], or the session's close.
	pub async fn closed(&self) -> Result<(), MoqError> {
		self.token.run(|token| async move { token.closed().await }).await
	}

	/// Withdraw the token, removing its grant from the union.
	///
	/// Terminal: the token is withdrawn here, not when the handle is.
	pub fn cancel(&self) {
		self.token.cancel();
	}
}
