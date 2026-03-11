use std::str::FromStr;
use std::sync::Arc;

use url::Url;

use crate::error::MoqError;
use crate::ffi;
use crate::origin::MoqOriginProducer;

struct MoqClientState {
	config: moq_native::ClientConfig,
	publish: Option<Arc<MoqOriginProducer>>,
	consume: Option<Arc<MoqOriginProducer>>,
}

#[derive(uniffi::Object)]
pub struct MoqClient {
	state: std::sync::Mutex<MoqClientState>,
}

#[derive(uniffi::Object)]
pub struct MoqConnecting {
	close: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
	established_rx: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<Result<(), MoqError>>>>,
	task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<Result<(), MoqError>>>>,
}

#[derive(uniffi::Object)]
pub struct MoqSession {
	close: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
	task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<Result<(), MoqError>>>>,
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

	/// Connect to a MoQ server. Returns a `MoqConnecting` that can be awaited or cancelled.
	pub fn connect(&self, url: String) -> Result<Arc<MoqConnecting>, MoqError> {
		let url = Url::parse(&url)?;
		let state = self.state.lock().unwrap();
		let config = state.config.clone();
		let publish_consumer = state.publish.as_ref().map(|o| o.inner().consume());
		let consume_producer = state.consume.as_ref().map(|o| o.inner().clone());
		drop(state);

		let (close_tx, mut close_rx) = tokio::sync::oneshot::channel();
		let (established_tx, established_rx) = tokio::sync::oneshot::channel();

		let task = ffi::HANDLE.spawn(async move {
			// Phase 1: Connect (cancellable via close signal)
			let session = tokio::select! {
				biased;
				_ = &mut close_rx => {
					let _ = established_tx.send(Err(MoqError::Cancelled));
					return Ok(());
				}
				result = async {
					let client = config
						.init()
						.map_err(|err| MoqError::Connect(format!("{err}")))?;

					client
						.with_publish(publish_consumer)
						.with_consume(consume_producer)
						.connect(url)
						.await
						.map_err(|err| MoqError::Connect(format!("{err}")))
				} => {
					match result {
						Ok(session) => {
							let _ = established_tx.send(Ok(()));
							session
						}
						Err(e) => {
							let _ = established_tx.send(Err(e));
							return Ok(());
						}
					}
				}
			};

			// Phase 2: Drive session (cancellable via same close signal)
			tokio::select! {
				_ = close_rx => Ok(()),
				res = session.closed() => res.map_err(Into::into),
			}
		});

		Ok(Arc::new(MoqConnecting {
			close: std::sync::Mutex::new(Some(close_tx)),
			established_rx: tokio::sync::Mutex::new(Some(established_rx)),
			task: tokio::sync::Mutex::new(Some(task)),
		}))
	}
}

#[uniffi::export]
impl MoqConnecting {
	/// Wait for the connection to be established.
	pub async fn established(&self) -> Result<Arc<MoqSession>, MoqError> {
		let rx = self
			.established_rx
			.lock()
			.await
			.take()
			.ok_or_else(|| MoqError::Closed)?;

		let result = rx.await.map_err(|_| MoqError::Closed)?;
		result?;

		// Move close + task handles into the new MoqSession
		let close = self.close.lock().unwrap().take();
		let task = self.task.lock().await.take();

		Ok(Arc::new(MoqSession {
			close: std::sync::Mutex::new(close),
			task: tokio::sync::Mutex::new(task),
		}))
	}

	/// Cancel the connection attempt.
	pub fn close(&self) {
		if let Some(sender) = self.close.lock().unwrap().take() {
			let _ = sender.send(());
		}
	}
}

impl Drop for MoqConnecting {
	fn drop(&mut self) {
		self.close();
	}
}

#[uniffi::export]
impl MoqSession {
	/// Wait until the session is closed.
	pub async fn closed(&self) -> Result<(), MoqError> {
		let task = self.task.lock().await.take();
		if let Some(task) = task {
			task.await??;
		}
		Ok(())
	}

	/// Close the session.
	pub fn close(&self) {
		if let Some(sender) = self.close.lock().unwrap().take() {
			let _ = sender.send(());
		}
	}
}

impl Drop for MoqSession {
	fn drop(&mut self) {
		self.close();
	}
}
