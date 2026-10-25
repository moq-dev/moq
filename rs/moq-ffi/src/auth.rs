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

/// Present `token` on every session of `handle`, resolving once a peer accepts it.
async fn add(handle: moq_net::auth::Handle, token: String) -> Result<moq_net::auth::Credential, MoqError> {
	// Without AUTH, moq-transport accepts a token only for the requests that name it, but
	// nothing here can name one, so it would never be sent. The answer says how the peer
	// accepted it, which a grant missing after its session drops cannot.
	match handle.add(token).await? {
		(auth, moq_net::auth::Answer::Granted) => Ok(auth),
		(_, moq_net::auth::Answer::Requests) => Err(MoqError::Unsupported),
	}
}

/// Poll `future` once without waiting.
fn poll_now<F: std::future::Future>(future: F) -> std::task::Poll<F::Output> {
	let future = std::pin::pin!(future);
	std::future::Future::poll(future, &mut std::task::Context::from_waker(std::task::Waker::noop()))
}

/// A session's authorization: the grant its peer gave it, and more tokens to present.
///
/// Returned by [`MoqSession::auth`](crate::session::MoqSession::auth). AUTH is only on
/// `moq-lite-07-wip` and on moq-transport sessions that negotiate the MoQ Auth extension;
/// elsewhere the grant stays `None`.
#[derive(uniffi::Object)]
pub struct MoqAuth {
	handle: moq_net::auth::Handle,
	/// The watch behind [`grant`](Self::grant), which reads without the lock.
	peek: moq_net::auth::Watch,
	/// The watch behind [`grant_changed`](Self::grant_changed), which tracks what this
	/// handle last reported. Concurrent callers share it, so each watcher takes its own handle.
	changed: Task<moq_net::auth::Watch>,
}

impl MoqAuth {
	pub(crate) fn new(handle: moq_net::auth::Handle) -> Self {
		// Start from the grant as it is now, so a watcher reads that with `grant` and waits
		// in `grant_changed` only for what follows, never for the same grant twice.
		let mut changed = handle.grant();
		let _ = poll_now(changed.changed());
		Self {
			peek: handle.grant(),
			changed: Task::new(changed),
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
		self.peek.peek().map(Into::into)
	}

	/// Wait for the grant to differ from the one this handle last reported, or held when it was
	/// made, returning it.
	///
	/// Calls on one handle share what it last reported; an independent watcher takes its own
	/// handle from [`MoqSession::auth`](crate::session::MoqSession::auth).
	///
	/// Errors once nothing will change it again: a client's connection or a server's session
	/// ended. A reconnect is not an end.
	pub async fn grant_changed(&self) -> Result<Option<MoqGrant>, MoqError> {
		self.changed
			.run(|mut watch| async move { Ok(watch.changed().await?.map(Into::into)) })
			.await
	}

	/// Present another token, resolving once the peer accepts it.
	///
	/// Its grant joins the union until [`MoqCredential::cancel`]. A client session presents it
	/// again on every reconnect. The peer's refusal is a [`MoqError::Protocol`] carrying its code.
	///
	/// # Errors
	///
	/// [`MoqError::Unsupported`] when the session cannot carry another token: a version
	/// without AUTH, or moq-transport without the MoQ Auth extension.
	pub async fn add(&self, token: String) -> Result<Arc<MoqCredential>, MoqError> {
		let handle = self.handle.clone();
		let token = crate::ffi::detached(add(handle, token)).await?;
		Ok(Arc::new(MoqCredential::new(token)))
	}
}

/// One token presented with [`MoqAuth::add`].
///
/// It stays presented until [`cancel`](Self::cancel), or until the handle is released.
#[derive(uniffi::Object)]
pub struct MoqCredential {
	/// The watch behind [`grant`](Self::grant), which does not keep the token presented.
	peek: moq_net::auth::Watch,
	token: Task<moq_net::auth::Credential>,
}

impl MoqCredential {
	fn new(token: moq_net::auth::Credential) -> Self {
		Self {
			peek: token.grant(),
			token: Task::new(token),
		}
	}
}

#[uniffi::export]
impl MoqCredential {
	/// This token's own grant, or `None` while the peer has not answered it or after it ended.
	pub fn grant(&self) -> Option<MoqGrant> {
		let _guard = crate::ffi::enter();
		self.peek.peek().map(Into::into)
	}

	/// Wait for the token to end: `Ok` when the peer closed it without an error, the peer's
	/// revocation as [`MoqError::Protocol`], or the end of a client's connection or a server's
	/// session as an error. A reconnect does not end it.
	pub async fn closed(&self) -> Result<(), MoqError> {
		self.token
			.run(|token| async move {
				match token.closed().await {
					moq_net::Error::Cancel => Ok(()),
					err => Err(err.into()),
				}
			})
			.await
	}

	/// Withdraw the token, removing its grant from the union.
	///
	/// Terminal: the token is withdrawn here, not when the handle is.
	pub fn cancel(&self) {
		self.token.cancel();
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use crate::session::{MoqClient, MoqClientConfig};

	const TIMEOUT: Duration = Duration::from_secs(10);

	/// Wait until `auth` reports `want`, following its changes.
	async fn await_grant(auth: &super::MoqAuth, want: Option<Vec<String>>) {
		let publish = || auth.grant().map(|grant| grant.publish);
		tokio::time::timeout(TIMEOUT, async {
			while publish() != want {
				auth.grant_changed().await.expect("grant watch ended");
			}
		})
		.await
		.unwrap_or_else(|_| panic!("timed out waiting for {want:?}, last {:?}", publish()));
	}

	/// A token the peer granted is not refused as request-only when its session drops before
	/// `add` reads the answer, though its grant then reads `None`.
	#[tokio::test]
	async fn a_grant_survives_its_session_dropping_before_add_returns() {
		let version: moq_net::Version = "moq-lite-07-wip".parse().unwrap();
		let mut config = moq_tokio::listen::Config::default();
		config.tcp.bind = Some("127.0.0.1:0".parse().unwrap());
		config.version = vec![version];
		let mut listener = config.init(Default::default()).unwrap().listen().await.unwrap();
		let addr = listener.tcp_local_addr().unwrap();

		let (drop_tx, drop_rx) = tokio::sync::oneshot::channel::<()>();
		let serving = tokio::spawn(async move {
			let mut request = listener.accept().await.expect("accept");
			// Nothing redials: the client stays disconnected once the session drops.
			drop(listener);
			let mut requests = request.requests();
			let session = request.ok().await.unwrap();
			let mut issued = Vec::new();
			for _ in 0..2 {
				let request = requests.next().await.expect("token");
				let pattern = match request.token().as_ref() {
					b"" => "setup/**",
					_ => "extra/**",
				};
				issued.push(request.accept(moq_net::auth::Grant {
					publish: pattern.parse::<moq_net::Pattern>().unwrap().into(),
					..Default::default()
				}));
			}
			let _ = drop_rx.await;
			drop((session, issued, requests));
		});

		let client = MoqClient::new(MoqClientConfig {
			versions: vec!["moq-lite-07-wip".into()],
			bind: Some("127.0.0.1:0".into()),
			..Default::default()
		})
		.unwrap();
		let session = tokio::time::timeout(TIMEOUT, client.connect(format!("tcp://{addr}")))
			.await
			.expect("connect timed out")
			.unwrap();
		let auth = session.auth();
		await_grant(&auth, Some(vec!["setup/**".into()])).await;

		// Presented and granted, but not read until the session is gone.
		let mut adding = Box::pin(super::add(auth.handle.clone(), "extra".into()));
		assert!(super::poll_now(adding.as_mut()).is_pending());
		await_grant(&auth, Some(vec!["extra/**".into(), "setup/**".into()])).await;
		drop_tx.send(()).unwrap();
		await_grant(&auth, None).await;

		let std::task::Poll::Ready(added) = super::poll_now(adding.as_mut()) else {
			panic!("add did not resolve");
		};
		let token = added.expect("a granted token refused after its session dropped");
		assert_eq!(token.grant().peek(), None, "no session is live");

		session.cancel(0);
		tokio::time::timeout(TIMEOUT, serving)
			.await
			.expect("server ended")
			.unwrap();
	}
}
