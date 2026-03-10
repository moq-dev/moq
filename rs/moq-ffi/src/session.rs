use std::str::FromStr;
use std::sync::Arc;

use url::Url;

use crate::error::MoqError;
use crate::ffi;
use crate::origin::MoqOriginProducer;

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
#[uniffi::export]
pub fn moq_log_level(level: String) -> Result<(), MoqError> {
	use tracing::Level;
	match level.as_str() {
		"" => moq_native::Log::default(),
		s => moq_native::Log::new(Level::from_str(s)?),
	}
	.init();
	Ok(())
}

/// Connect to a MoQ server. Returns a `MoqConnecting` that can be awaited or cancelled.
#[uniffi::export]
pub fn moq_connect(
	url: String,
	publish: Option<Arc<MoqOriginProducer>>,
	consume: Option<Arc<MoqOriginProducer>>,
) -> Result<Arc<MoqConnecting>, MoqError> {
	let url = Url::parse(&url)?;
	let publish_consumer = publish.map(|o| o.inner().consume());
	let consume_producer = consume.map(|o| o.inner().clone());

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
				let client = moq_native::ClientConfig::default()
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
