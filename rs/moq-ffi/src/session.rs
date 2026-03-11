use std::str::FromStr;
use std::sync::Arc;

use url::Url;

use crate::error::MoqError;
use crate::ffi::Abort;
use crate::origin::MoqOriginProducer;

struct MoqClientState {
	config: moq_native::ClientConfig,
	publish: Option<Arc<MoqOriginProducer>>,
	consume: Option<Arc<MoqOriginProducer>>,
}

#[derive(uniffi::Object)]
pub struct MoqClient {
	state: std::sync::Mutex<MoqClientState>,
	abort: Abort,
}

#[derive(uniffi::Object)]
pub struct MoqSession {
	session: moq_lite::Session,
	abort: Abort,
}

/// Initialize logging with a level string: "error", "warn", "info", "debug", "trace", or "".
///
/// Returns an error if called more than once.
#[uniffi::export]
pub fn moq_log_level(level: String) -> Result<(), MoqError> {
	use std::sync::atomic::{AtomicBool, Ordering};
	use tracing::Level;

	static INITIALIZED: AtomicBool = AtomicBool::new(false);
	if INITIALIZED.swap(true, Ordering::SeqCst) {
		return Err(MoqError::Log("logging already initialized".into()));
	}

	match level.as_str() {
		"" => moq_native::Log::default(),
		s => moq_native::Log::new(Level::from_str(s)?),
	}
	.init();

	Ok(())
}

#[uniffi::export]
impl MoqClient {
	/// Create a new MoQ client with default configuration.
	#[uniffi::constructor]
	pub fn new() -> Arc<Self> {
		Arc::new(Self {
			state: std::sync::Mutex::new(MoqClientState {
				config: moq_native::ClientConfig::default(),
				publish: None,
				consume: None,
			}),
			abort: Abort::new(),
		})
	}

	/// Disable TLS certificate verification (for development only).
	pub fn set_tls_disable_verify(&self, disable: bool) {
		self.state.lock().unwrap().config.tls.disable_verify = Some(disable);
	}

	/// Set the origin to publish local broadcasts to the remote.
	pub fn set_publish(&self, origin: Option<Arc<MoqOriginProducer>>) {
		self.state.lock().unwrap().publish = origin;
	}

	/// Set the origin to consume remote broadcasts from the remote.
	pub fn set_consume(&self, origin: Option<Arc<MoqOriginProducer>>) {
		self.state.lock().unwrap().consume = origin;
	}

	/// Connect to a MoQ server and wait for the session to be established.
	///
	/// Can be cancelled by calling `close()`.
	pub async fn connect(&self, url: String) -> Result<Arc<MoqSession>, MoqError> {
		let url = Url::parse(&url)?;

		let (config, publish, consume) = {
			let state = self.state.lock().unwrap();
			(
				state.config.clone(),
				state.publish.as_ref().map(|o| o.inner().consume()),
				state.consume.as_ref().map(|o| o.inner().clone()),
			)
		};

		tokio::select! {
			biased;
			_ = self.abort.aborted() => Err(MoqError::Cancelled),
			result = async {
				let client = config
					.init()
					.map_err(|err| MoqError::Connect(format!("{err}")))?;

				client
					.with_publish(publish)
					.with_consume(consume)
					.connect(url)
					.await
					.map_err(|err| MoqError::Connect(format!("{err}")))
			} => {
				let session = result?;
				Ok(Arc::new(MoqSession {
					session,
					abort: Abort::new(),
				}))
			}
		}
	}

	/// Cancel any outstanding `connect()` call.
	pub fn close(&self) {
		self.abort.abort();
	}
}

#[uniffi::export]
impl MoqSession {
	/// Wait until the session is closed.
	pub async fn closed(&self) -> Result<(), MoqError> {
		tokio::select! {
			biased;
			_ = self.abort.aborted() => Ok(()),
			res = self.session.closed() => res.map_err(Into::into),
		}
	}

	/// Close the session.
	pub fn close(&self) {
		self.abort.abort();
	}
}

impl Drop for MoqSession {
	fn drop(&mut self) {
		self.abort.abort();
	}
}
