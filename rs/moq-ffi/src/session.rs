use std::sync::Arc;

use moq_net::Session;
use url::Url;

use crate::error::MoqError;
use crate::ffi::Task;
use crate::origin::{MoqOriginConsumer, MoqOriginProducer};

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

		let publish = self.publish.as_ref().map(|o| o.inner().clone());
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
	/// If neither [`set_publish`](Self::set_publish) nor
	/// [`set_consume`](Self::set_consume) was called on this client, the
	/// underlying moq-net layer auto-creates a fresh origin and wires it
	/// as both sides. The producer and consumer sides are then accessible
	/// via [`MoqSession::publisher`] and [`MoqSession::consumer`] so the
	/// caller never has to construct a [`MoqOriginProducer`] themselves.
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
	publisher: Arc<MoqOriginProducer>,
	consumer: Arc<MoqOriginConsumer>,
}

impl MoqSession {
	pub(crate) fn new(session: moq_net::Session) -> Self {
		// Eagerly wrap the always-set origin sides so each
		// publisher()/consumer() call hands back the same Arc.
		let publisher = Arc::new(MoqOriginProducer::from_inner(session.publisher().clone()));
		let consumer = Arc::new(MoqOriginConsumer::from_inner(session.consumer().clone()));
		Self {
			inner: Some(session.clone()),
			closed: Task::new(session),
			publisher,
			consumer,
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

	/// The publish-side origin: where local broadcasts get advertised
	/// to the remote. Either the producer the caller wired via
	/// `set_publish` / `set_consume` before connect/accept, or one
	/// auto-created if neither was set.
	pub fn publisher(&self) -> Arc<MoqOriginProducer> {
		self.publisher.clone()
	}

	/// The subscribe-side origin: a read handle for receiving
	/// announcements pushed by the remote. Either derived from the
	/// origin the caller wired via `set_consume`, or auto-created if
	/// neither was set.
	pub fn consumer(&self) -> Arc<MoqOriginConsumer> {
		self.consumer.clone()
	}
}
