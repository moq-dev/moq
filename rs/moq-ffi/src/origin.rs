use std::sync::Arc;

use crate::consumer::MoqBroadcastConsumer;
use crate::error::MoqError;
use crate::producer::MoqBroadcastProducer;

#[derive(uniffi::Object)]
pub struct MoqOriginProducer {
	inner: moq_lite::OriginProducer,
}

#[derive(uniffi::Object)]
pub struct MoqOriginConsumer {
	inner: moq_lite::OriginConsumer,
}

#[derive(uniffi::Object)]
pub struct MoqAnnounced {
	inner: Arc<tokio::sync::Mutex<moq_lite::OriginConsumer>>,
	close: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
	close_rx: Arc<tokio::sync::Mutex<tokio::sync::oneshot::Receiver<()>>>,
}

/// A broadcast announcement from an origin.
#[derive(uniffi::Object)]
pub struct MoqAnnouncement {
	path: String,
	broadcast: MoqBroadcastConsumer,
}

/// Waits for a specific broadcast to be announced.
#[derive(uniffi::Object)]
pub struct MoqAnnouncedBroadcast {
	inner: Arc<tokio::sync::Mutex<moq_lite::OriginConsumer>>,
	close: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
	close_rx: Arc<tokio::sync::Mutex<tokio::sync::oneshot::Receiver<()>>>,
}

/// Create a new origin for publishing and/or consuming broadcasts.
#[uniffi::export]
pub fn moq_origin_create() -> Arc<MoqOriginProducer> {
	Arc::new(MoqOriginProducer {
		inner: moq_lite::OriginProducer::default(),
	})
}

impl MoqOriginProducer {
	pub(crate) fn inner(&self) -> &moq_lite::OriginProducer {
		&self.inner
	}
}

#[uniffi::export]
impl MoqOriginProducer {
	/// Create a consumer for this origin.
	pub fn consume(&self) -> Arc<MoqOriginConsumer> {
		Arc::new(MoqOriginConsumer {
			inner: self.inner.consume(),
		})
	}

	/// Publish a broadcast to this origin under the given path.
	pub fn publish(&self, path: String, broadcast: &MoqBroadcastProducer) -> Result<(), MoqError> {
		let consumer = broadcast.consume()?;
		self.inner.publish_broadcast(path.as_str(), consumer);
		Ok(())
	}
}

#[uniffi::export]
impl MoqOriginConsumer {
	/// Subscribe to all broadcast announcements under a prefix.
	pub fn announced(&self, prefix: String) -> Result<Arc<MoqAnnounced>, MoqError> {
		let origin = self.inner.clone().with_root(prefix).ok_or(MoqError::Unauthorized)?;
		let (tx, rx) = tokio::sync::oneshot::channel();
		Ok(Arc::new(MoqAnnounced {
			inner: Arc::new(tokio::sync::Mutex::new(origin)),
			close: std::sync::Mutex::new(Some(tx)),
			close_rx: Arc::new(tokio::sync::Mutex::new(rx)),
		}))
	}

	/// Wait for a specific broadcast to be announced by path.
	pub fn announced_broadcast(&self, path: String) -> Result<Arc<MoqAnnouncedBroadcast>, MoqError> {
		let origin = self.inner.clone().with_root(path).ok_or(MoqError::Unauthorized)?;
		let (tx, rx) = tokio::sync::oneshot::channel();
		Ok(Arc::new(MoqAnnouncedBroadcast {
			inner: Arc::new(tokio::sync::Mutex::new(origin)),
			close: std::sync::Mutex::new(Some(tx)),
			close_rx: Arc::new(tokio::sync::Mutex::new(rx)),
		}))
	}
}

// ---- MoqAnnounced ----

#[uniffi::export]
impl MoqAnnounced {
	/// Get the next broadcast announcement. Returns `None` when the origin is closed.
	///
	/// Use `broadcast.closed()` to learn when a broadcast is unannounced.
	pub async fn next(&self) -> Result<Option<Arc<MoqAnnouncement>>, MoqError> {
		let mut consumer = self.inner.lock().await;
		let mut close_rx = self.close_rx.lock().await;
		loop {
			tokio::select! {
				biased;
				_ = &mut *close_rx => return Ok(None),
				result = consumer.announced() => match result {
					Some((path, Some(broadcast))) => {
						return Ok(Some(Arc::new(MoqAnnouncement {
							path: path.to_string(),
							broadcast: MoqBroadcastConsumer::new(broadcast),
						})));
					}
					Some((_path, None)) => continue,
					None => return Ok(None),
				}
			}
		}
	}

	/// Close this stream, causing any pending `next()` to return `None`.
	pub fn close(&self) {
		if let Some(sender) = self.close.lock().unwrap().take() {
			let _ = sender.send(());
		}
	}
}

#[uniffi::export]
impl MoqAnnouncement {
	/// The path of the announced broadcast.
	pub fn path(&self) -> String {
		self.path.clone()
	}

	/// The broadcast consumer.
	pub fn broadcast(&self) -> Arc<MoqBroadcastConsumer> {
		Arc::new(self.broadcast.clone())
	}
}

// ---- MoqAnnouncedBroadcast ----

#[uniffi::export]
impl MoqAnnouncedBroadcast {
	/// Wait until the broadcast is announced. Returns `Closed` if cancelled or the origin is closed.
	///
	/// Use `broadcast.closed()` to learn when a broadcast is unannounced.
	pub async fn broadcast(&self) -> Result<Arc<MoqBroadcastConsumer>, MoqError> {
		let mut consumer = self.inner.lock().await;
		let mut close_rx = self.close_rx.lock().await;

		loop {
			tokio::select! {
				biased;
				_ = &mut *close_rx => return Err(MoqError::Closed),
				result = consumer.announced() => match result {
					Some((_path, Some(broadcast))) => {
						return Ok(Arc::new(MoqBroadcastConsumer::new(broadcast)));
					}
					Some((_path, None)) => {
						// Unannounced — keep waiting for re-announcement
						continue;
					}
					None => return Err(MoqError::Closed),
				}
			}
		}
	}

	/// Close this, causing any pending `broadcast()` call to return `Closed`.
	pub fn close(&self) {
		if let Some(sender) = self.close.lock().unwrap().take() {
			let _ = sender.send(());
		}
	}
}
