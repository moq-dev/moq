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
		let client = self
			.config
			.clone()
			.init()
			.map_err(|err| MoqError::Connect(format!("{err}")))?;

		let publish = self.publish.as_ref().map(|o| o.inner().consume());
		let consume = self.consume.as_ref().map(|o| o.inner().clone());

		let session = client
			.with_publish(publish)
			.with_consume(consume)
			.connect(url)
			.await
			.map_err(|err| MoqError::Connect(format!("{err}")))?;

		Ok(Arc::new(MoqSession::new(session)))
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
