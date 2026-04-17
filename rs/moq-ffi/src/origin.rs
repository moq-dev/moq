use std::sync::Arc;

use crate::consumer::MoqBroadcastConsumer;
use crate::error::MoqError;
use crate::ffi::Task;
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
	task: Task<Announced>,
}

struct Announced {
	inner: moq_lite::OriginConsumer,
}

impl Announced {
	async fn next(&mut self) -> Result<Option<Arc<MoqAnnouncement>>, MoqError> {
		loop {
			match self.inner.announced().await {
				Some((path, Some(broadcast))) => {
					return Ok(Some(Arc::new(MoqAnnouncement {
						path: path.to_string(),
						broadcast: Arc::new(MoqBroadcastConsumer::new(broadcast)),
					})));
				}
				// TODO moq-lite will change to not emit None (unannounce) events here.
				Some((_path, None)) => continue,
				None => return Ok(None),
			}
		}
	}

	async fn available(&mut self) -> Result<Arc<MoqBroadcastConsumer>, MoqError> {
		loop {
			match self.inner.announced().await {
				Some((_path, Some(broadcast))) => {
					return Ok(Arc::new(MoqBroadcastConsumer::new(broadcast)));
				}
				// TODO moq-lite will change to not emit None (unannounce) events here.
				Some((_path, None)) => continue,
				None => return Err(MoqError::Closed),
			}
		}
	}
}

/// A broadcast announcement from an origin.
#[derive(uniffi::Object)]
pub struct MoqAnnouncement {
	path: String,
	broadcast: Arc<MoqBroadcastConsumer>,
}

/// Waits for a specific broadcast to be announced.
#[derive(uniffi::Object)]
pub struct MoqAnnouncedBroadcast {
	task: Task<Announced>,
}

impl MoqOriginProducer {
	pub(crate) fn inner(&self) -> &moq_lite::OriginProducer {
		&self.inner
	}
}

#[uniffi::export]
impl MoqOriginProducer {
	/// Create a new origin for publishing and/or consuming broadcasts.
	#[uniffi::constructor]
	pub fn new() -> Arc<Self> {
		let _guard = crate::ffi::RUNTIME.enter();
		Arc::new(Self {
			inner: moq_lite::OriginProducer::default(),
		})
	}

	/// Create a consumer for this origin.
	pub fn consume(&self) -> Arc<MoqOriginConsumer> {
		let _guard = crate::ffi::RUNTIME.enter();
		Arc::new(MoqOriginConsumer {
			inner: self.inner.consume(),
		})
	}

	/// Publish a broadcast to this origin under the given path.
	pub fn publish(&self, path: String, broadcast: &MoqBroadcastProducer) -> Result<(), MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let consumer = broadcast.consume_inner()?;
		if !self.inner.publish_broadcast(path.as_str(), consumer) {
			return Err(MoqError::Unauthorized);
		}
		Ok(())
	}
}

#[uniffi::export]
impl MoqOriginConsumer {
	/// Subscribe to all broadcast announcements under a prefix.
	pub fn announced(&self, prefix: String) -> Result<Arc<MoqAnnounced>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let origin = self.inner.clone().with_root(prefix).ok_or(MoqError::Unauthorized)?;
		Ok(Arc::new(MoqAnnounced {
			task: Task::new(Announced { inner: origin }),
		}))
	}

	/// Wait for a specific broadcast to be announced by path.
	pub fn announced_broadcast(&self, path: String) -> Result<Arc<MoqAnnouncedBroadcast>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let origin = self.inner.clone().with_root(path).ok_or(MoqError::Unauthorized)?;
		Ok(Arc::new(MoqAnnouncedBroadcast {
			task: Task::new(Announced { inner: origin }),
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
		self.task.run(|mut state| async move { state.next().await }).await
	}

	/// Cancel all current and future `next()` calls.
	pub fn cancel(&self) {
		self.task.cancel();
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
		self.broadcast.clone()
	}
}

// ---- MoqAnnouncedBroadcast ----

#[uniffi::export]
impl MoqAnnouncedBroadcast {
	/// Wait until the broadcast is announced. Returns `Closed` if cancelled or the origin is closed.
	///
	/// Use `broadcast.closed()` to learn when a broadcast is unannounced.
	pub async fn available(&self) -> Result<Arc<MoqBroadcastConsumer>, MoqError> {
		self.task.run(|mut state| async move { state.available().await }).await
	}

	/// Cancel all current and future `available()` calls.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}
