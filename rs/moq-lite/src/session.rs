use std::{future::Future, pin::Pin, sync::Arc};

use crate::{Error, Version};

/// A MoQ transport session, wrapping a WebTransport connection.
///
/// Created via:
/// - [`crate::Client::connect`] for clients.
/// - [`crate::Server::accept`] for servers.
#[derive(Clone)]
pub struct Session {
	session: Arc<dyn SessionInner>,
	version: Version,
	closed: bool,
}

impl Session {
	pub(super) fn new<S: web_transport_trait::Session>(session: S, version: Version) -> Self {
		Self {
			session: Arc::new(session),
			version,
			closed: false,
		}
	}

	/// Returns the negotiated protocol version.
	pub fn version(&self) -> Version {
		self.version
	}

	/// Close the underlying transport session.
	pub fn close(&mut self, err: Error) {
		if self.closed {
			return;
		}
		self.closed = true;
		self.session.close(err.to_code(), err.to_string().as_ref());
	}

	/// Block until the transport session is closed.
	// TODO Remove the Result the next time we make a breaking change.
	pub async fn closed(&self) -> Result<(), Error> {
		self.session.closed().await;
		Err(Error::Transport)
	}
}

impl Drop for Session {
	fn drop(&mut self) {
		if !self.closed {
			self.session.close(Error::Cancel.to_code(), "dropped");
		}
	}
}

// We use a wrapper type that is dyn-compatible to remove the generic bounds from Session.
trait SessionInner: Send + Sync {
	fn close(&self, code: u32, reason: &str);
	fn closed(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

impl<S: web_transport_trait::Session> SessionInner for S {
	fn close(&self, code: u32, reason: &str) {
		S::close(self, code, reason);
	}

	fn closed(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
		Box::pin(async move {
			let _ = S::closed(self).await;
		})
	}
}
