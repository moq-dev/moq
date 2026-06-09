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

		// Wire stable publish/consume origins so they survive reconnects: the caller's if set,
		// otherwise a shared origin (matching moq-net's default of wiring one origin as both sides).
		let (publish, consume) = self.origins();

		// Reconnect with backoff by default; wait for the first established session before returning.
		let mut connection = client
			.with_publisher(publish.clone())
			.with_consumer(consume.clone())
			.connect(url);
		connection
			.status()
			.await
			.map_err(|err| MoqError::Connect(format!("{err}")))?;

		Ok(Arc::new(MoqSession::reconnecting(connection, publish, consume)))
	}

	/// The publish/consume origins to wire, held so they stay stable across reconnects.
	fn origins(&self) -> (moq_net::OriginProducer, moq_net::OriginProducer) {
		match (self.publish.as_ref(), self.consume.as_ref()) {
			(Some(publish), Some(consume)) => (publish.inner().clone(), consume.inner().clone()),
			(Some(publish), None) => (publish.inner().clone(), moq_net::Origin::random().produce()),
			(None, Some(consume)) => (moq_net::Origin::random().produce(), consume.inner().clone()),
			(None, None) => {
				let shared = moq_net::Origin::random().produce();
				(shared.clone(), shared)
			}
		}
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
	/// The returned session reconnects with exponential backoff if it later drops; call
	/// `shutdown()` (or `cancel()`) on it to stop.
	///
	/// If neither [`set_publish`](Self::set_publish) nor
	/// [`set_consume`](Self::set_consume) was called on this client, a fresh origin is created and
	/// wired as both sides. The producer and consumer sides are then accessible via
	/// [`MoqSession::publisher`] and [`MoqSession::consumer`] so the caller never has to construct a
	/// [`MoqOriginProducer`] themselves.
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
	kind: Kind,
	publisher: Arc<MoqOriginProducer>,
	consumer: Arc<MoqOriginConsumer>,
}

// `MoqSession` is always handed out behind an `Arc`, so the size gap between variants is moot.
#[allow(clippy::large_enum_variant)]
enum Kind {
	/// Client side: a reconnecting connection. `closed` resolves only when it permanently stops.
	Connection(Arc<moq_native::Connection>),
	/// Server side: a single accepted session, with no client connection to reconnect.
	Session {
		inner: Option<moq_net::Session>,
		closed: Task<Session>,
	},
}

impl MoqSession {
	/// Wrap a single server-accepted session (no reconnection).
	pub(crate) fn new(session: moq_net::Session) -> Self {
		// Eagerly wrap the always-set origin sides so each
		// publisher()/consumer() call hands back the same Arc.
		let publisher = Arc::new(MoqOriginProducer::from_inner(session.publisher().clone()));
		let consumer = Arc::new(MoqOriginConsumer::from_inner(session.consumer().clone()));
		Self {
			kind: Kind::Session {
				inner: Some(session.clone()),
				closed: Task::new(session),
			},
			publisher,
			consumer,
		}
	}

	/// Wrap a client connection that reconnects with backoff. `publish`/`consume` are the wired
	/// origins, stable across reconnects, so the publisher/consumer handles stay valid.
	pub(crate) fn reconnecting(
		connection: moq_native::Connection,
		publish: moq_net::OriginProducer,
		consume: moq_net::OriginProducer,
	) -> Self {
		let publisher = Arc::new(MoqOriginProducer::from_inner(publish));
		let consumer = Arc::new(MoqOriginConsumer::from_inner(consume.consume()));
		Self {
			kind: Kind::Connection(Arc::new(connection)),
			publisher,
			consumer,
		}
	}
}

impl Drop for MoqSession {
	fn drop(&mut self) {
		let _guard = crate::ffi::RUNTIME.enter();
		// Server: drop the held session. Client: the last `Arc<Connection>` drop aborts the loop.
		if let Kind::Session { inner, .. } = &mut self.kind {
			inner.take();
		}
	}
}

#[uniffi::export]
impl MoqSession {
	/// Wait until the session is closed. For a reconnecting client, this only resolves once the
	/// connection permanently stops (gives up, or `cancel`/`shutdown` is called).
	pub async fn closed(&self) -> Result<(), MoqError> {
		match &self.kind {
			// Drive on the shared runtime so the C callback keeps its thread affinity.
			Kind::Connection(connection) => {
				let connection = connection.clone();
				match crate::ffi::RUNTIME
					.spawn(async move { connection.closed().await })
					.await
				{
					Ok(res) => res.map_err(|err| MoqError::Connect(format!("{err:#}"))),
					Err(err) => Err(MoqError::Task(err)),
				}
			}
			Kind::Session { closed, .. } => {
				closed
					.run(|session| async move { session.closed().await.map_err(Into::into) })
					.await
			}
		}
	}

	/// Stop the connection and close the current session with the given error code.
	pub fn cancel(&self, code: u32) {
		let _guard = crate::ffi::RUNTIME.enter();
		match &self.kind {
			Kind::Connection(connection) => connection.close(moq_net::Error::Remote(code)),
			Kind::Session { inner, .. } => {
				if let Some(inner) = inner {
					inner.clone().close(moq_net::Error::Remote(code));
				}
			}
		}
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
