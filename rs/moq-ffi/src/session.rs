use std::str::FromStr;
use std::sync::Arc;

use url::Url;

use crate::error::MoqError;
use crate::ffi::Task;
use crate::origin::MoqOriginProducer;

struct ClientState {
	config: moq_native::ClientConfig,
	publish: Option<Arc<MoqOriginProducer>>,
	consume: Option<Arc<MoqOriginProducer>>,
}

impl ClientState {
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

		Ok(Arc::new(MoqSession {
			task: Task::new(SessionState { inner: session }),
		}))
	}
}

struct SessionState {
	inner: moq_lite::Session,
}

impl SessionState {
	async fn closed(&self) -> Result<(), MoqError> {
		self.inner.closed().await.map_err(Into::into)
	}
}

#[derive(uniffi::Object)]
pub struct MoqClient {
	task: Task<ClientState>,
}

#[derive(uniffi::Object)]
pub struct MoqSession {
	task: Task<SessionState>,
}

/// Initialize logging with a level string: "error", "warn", "info", "debug", "trace", or "".
///
/// Returns an error if called more than once.
#[uniffi::export]
pub fn moq_log_level(level: String) -> Result<(), MoqError> {
	use std::sync::atomic::{AtomicBool, Ordering};
	use tracing::Level;

	static INITIALIZED: AtomicBool = AtomicBool::new(false);

	let log = match level.as_str() {
		"" => moq_native::Log::default(),
		s => moq_native::Log::new(Level::from_str(s)?),
	};

	if INITIALIZED.swap(true, Ordering::SeqCst) {
		return Err(MoqError::Log("logging already initialized".into()));
	}

	log.init();

	Ok(())
}

#[uniffi::export]
impl MoqClient {
	/// Create a new MoQ client with default configuration.
	#[uniffi::constructor]
	pub fn new() -> Arc<Self> {
		let _guard = Task::<()>::enter();
		Arc::new(Self {
			task: Task::new(ClientState {
				config: moq_native::ClientConfig::default(),
				publish: None,
				consume: None,
			}),
		})
	}

	/// Disable TLS certificate verification (for development only).
	pub fn set_tls_disable_verify(&self, disable: bool) {
		let _ = self.task.with(|state| {
			state.config.tls.disable_verify = Some(disable);
		});
	}

	/// Set the origin to publish local broadcasts to the remote.
	pub fn set_publish(&self, origin: Option<Arc<MoqOriginProducer>>) {
		let _ = self.task.with(|state| {
			state.publish = origin;
		});
	}

	/// Set the origin to consume remote broadcasts from the remote.
	pub fn set_consume(&self, origin: Option<Arc<MoqOriginProducer>>) {
		let _ = self.task.with(|state| {
			state.consume = origin;
		});
	}

	/// Connect to a MoQ server and wait for the session to be established.
	///
	/// Can be cancelled by calling `cancel()`.
	pub async fn connect(&self, url: String) -> Result<Arc<MoqSession>, MoqError> {
		let url = Url::parse(&url)?;

		self.task
			.run(|state| async move {
				let result = state.connect(url).await;
				(state, result)
			})
			.await
	}

	/// Cancel any outstanding `connect()` call.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}

#[uniffi::export]
impl MoqSession {
	/// Wait until the session is closed.
	pub async fn closed(&self) -> Result<(), MoqError> {
		self.task
			.run(|state| async move {
				let result = state.closed().await;
				(state, result)
			})
			.await
	}

	/// Cancel the session.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}
