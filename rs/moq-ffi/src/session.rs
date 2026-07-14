use std::sync::Arc;

use moq_net::Session;
use url::Url;

use crate::error::MoqError;
use crate::ffi::Task;
use crate::origin::MoqOriginProducer;

struct Client {
	config: moq_native::ClientConfig,
	publish: Option<Arc<MoqOriginProducer>>,
	consume: Option<Arc<MoqOriginProducer>>,
}

impl Client {
	async fn connect(&self, url: Url) -> Result<Arc<MoqSession>, MoqError> {
		let client = self.config.clone().init().map_err(map_connect_error)?;

		let publish = self.publish.as_ref().map(|o| o.inner().consume());
		let consume = self.consume.as_ref().map(|o| o.inner().clone());

		let session = client
			.with_publish(publish)
			.with_consume(consume)
			.connect(url)
			.await
			.map_err(map_connect_error)?;

		Ok(Arc::new(MoqSession::new(session)))
	}
}

fn map_connect_error(err: moq_native::Error) -> MoqError {
	match err.connect_error() {
		Some(moq_native::ConnectError::Unauthorized) => MoqError::Unauthorized,
		Some(moq_native::ConnectError::Forbidden) => MoqError::Forbidden,
		_ => MoqError::Connect(format!("{err}")),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn maps_native_auth_connect_errors() {
		assert!(matches!(
			map_connect_error(moq_native::ConnectError::Unauthorized.into()),
			MoqError::Unauthorized
		));
		assert!(matches!(
			map_connect_error(moq_native::ConnectError::Forbidden.into()),
			MoqError::Forbidden
		));
	}

	#[test]
	fn sets_tls_system_roots() {
		let client = MoqClient::new();

		client.set_tls_system_roots(true);
		{
			let state = client.task.lock().expect("client state should be available");
			assert_eq!(state.config.tls.system_roots, Some(true));
		}

		client.set_tls_system_roots(false);
		let state = client.task.lock().expect("client state should be available");
		assert_eq!(state.config.tls.system_roots, Some(false));
	}

	#[test]
	fn sets_tls_client_cert_and_key() {
		let client = MoqClient::new();

		client.set_tls_cert(Some("cert.pem".into()));
		client.set_tls_key(Some("key.pem".into()));
		{
			let state = client.task.lock().expect("client state should be available");
			assert_eq!(state.config.tls.cert.as_deref(), Some(std::path::Path::new("cert.pem")));
			assert_eq!(state.config.tls.key.as_deref(), Some(std::path::Path::new("key.pem")));
		}

		client.set_tls_cert(None);
		client.set_tls_key(None);
		let state = client.task.lock().expect("client state should be available");
		assert_eq!(state.config.tls.cert, None);
		assert_eq!(state.config.tls.key, None);
	}
}

#[derive(uniffi::Object)]
pub struct MoqClient {
	task: Task<Client>,
}

#[uniffi::export]
impl MoqClient {
	/// Create a new MoQ client with default configuration.
	#[uniffi::constructor]
	pub fn new() -> Arc<Self> {
		let _guard = crate::ffi::RUNTIME.enter();
		Arc::new(Self {
			task: Task::new(Client {
				config: moq_native::ClientConfig::default(),
				publish: None,
				consume: None,
			}),
		})
	}

	/// Disable TLS certificate verification (for development only).
	pub fn set_tls_disable_verify(&self, disable: bool) {
		if let Some(mut state) = self.task.lock() {
			state.config.tls.disable_verify = Some(disable);
		}
	}

	/// Trust these PEM root certificate file(s) instead of the system roots.
	///
	/// Pass the paths to PEM-encoded CA certificates. An empty list restores the
	/// default behavior of using the platform's native root store.
	pub fn set_tls_roots(&self, paths: Vec<String>) {
		if let Some(mut state) = self.task.lock() {
			state.config.tls.root = paths.into_iter().map(Into::into).collect();
		}
	}

	/// Configure whether to also trust the platform's native root certificates.
	///
	/// By default, system roots are trusted only when no custom roots are configured.
	/// Set this to `true` to trust system roots in addition to roots from
	/// `set_tls_roots`, or `false` to trust only custom roots.
	pub fn set_tls_system_roots(&self, system_roots: bool) {
		if let Some(mut state) = self.task.lock() {
			state.config.tls.system_roots = Some(system_roots);
		}
	}

	/// Pin the peer to a certificate with one of these SHA-256 fingerprints, encoded as hex.
	///
	/// This is the native equivalent of the browser's WebTransport `serverCertificateHashes`
	/// and accepts the same values a server reports (see `MoqServer.cert_fingerprints`). Use it
	/// to trust a self-signed certificate without disabling verification. An empty list clears
	/// any pinned fingerprints.
	pub fn set_tls_fingerprints(&self, fingerprints: Vec<String>) {
		if let Some(mut state) = self.task.lock() {
			state.config.tls.fingerprint = fingerprints;
		}
	}

	/// Present this PEM certificate chain when the relay requires mTLS.
	///
	/// Only certificates are read from the file; any private keys are ignored. Must be
	/// paired with `set_tls_key`, otherwise `connect` fails with an incomplete-auth error.
	/// Pass `None` to clear a previously set path.
	pub fn set_tls_cert(&self, path: Option<String>) {
		if let Some(mut state) = self.task.lock() {
			state.config.tls.cert = path.map(Into::into);
		}
	}

	/// Present this PEM private key when the relay requires mTLS.
	///
	/// Only the private key is read from the file; any certificates are ignored. Must be
	/// paired with `set_tls_cert`, otherwise `connect` fails with an incomplete-auth error.
	/// Pass `None` to clear a previously set path.
	pub fn set_tls_key(&self, path: Option<String>) {
		if let Some(mut state) = self.task.lock() {
			state.config.tls.key = path.map(Into::into);
		}
	}

	/// Set the local UDP socket bind address. Defaults to `[::]:0`.
	///
	/// Returns an error if the address cannot be parsed.
	pub fn set_bind(&self, addr: String) -> Result<(), MoqError> {
		let parsed: std::net::SocketAddr = addr
			.parse()
			.map_err(|err| MoqError::Bind(format!("invalid bind address: {err}")))?;
		if let Some(mut state) = self.task.lock() {
			state.config.bind = parsed;
		}
		Ok(())
	}

	/// Set the origin to publish local broadcasts to the remote.
	pub fn set_publish(&self, origin: Option<Arc<MoqOriginProducer>>) {
		if let Some(mut state) = self.task.lock() {
			state.publish = origin;
		}
	}

	/// Set the origin to consume remote broadcasts from the remote.
	pub fn set_consume(&self, origin: Option<Arc<MoqOriginProducer>>) {
		if let Some(mut state) = self.task.lock() {
			state.consume = origin;
		}
	}

	/// Connect to a MoQ server and wait for the session to be established.
	///
	/// Can be cancelled by calling `cancel()`.
	pub async fn connect(&self, url: String) -> Result<Arc<MoqSession>, MoqError> {
		let url = Url::parse(&url)?;

		self.task.run(|state| async move { state.connect(url).await }).await
	}

	/// Cancel all current and future `connect()` calls.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}

#[derive(uniffi::Object)]
pub struct MoqSession {
	inner: Option<moq_net::Session>,
	closed: Task<Session>,
}

impl MoqSession {
	pub(crate) fn new(session: moq_net::Session) -> Self {
		Self {
			inner: Some(session.clone()),
			closed: Task::new(session),
		}
	}
}

impl Drop for MoqSession {
	fn drop(&mut self) {
		let _guard = crate::ffi::RUNTIME.enter();
		self.inner.take();
	}
}

#[uniffi::export]
impl MoqSession {
	/// Wait until the session is closed.
	pub async fn closed(&self) -> Result<(), MoqError> {
		// We have a task to run all of the closed calls juuuuust so they use the same tokio runtime.
		self.closed
			.run(|session| async move { session.closed().await.map_err(Into::into) })
			.await
	}

	/// Close the session with the given error code.
	pub fn cancel(&self, code: u32) {
		let _guard = crate::ffi::RUNTIME.enter();
		if let Some(inner) = &self.inner {
			inner.clone().close(moq_net::Error::Remote(code));
		}
		// NOTE: we don't abort the closed Task because it will be aborted via above ^
		// We'll get a slightly better error message instead of Cancelled.
	}

	/// Graceful shutdown. Equivalent to `cancel(0)`. Documents the
	/// convention that code 0 means "no error" so callers don't have to
	/// pick one. Named `shutdown` (not `close`) because UniFFI's Kotlin
	/// generator already emits an `AutoCloseable.close()` that releases
	/// the FFI handle, and shadowing it would silently mean a different
	/// thing per binding.
	pub fn shutdown(&self) {
		self.cancel(0);
	}
}
