use std::sync::Arc;

use crate::cache::MoqCache;
use crate::consumer::MoqBroadcastConsumer;
use crate::error::MoqError;
use crate::ffi::Task;
use crate::producer::MoqBroadcastProducer;

#[derive(uniffi::Object)]
pub struct MoqOriginProducer {
	inner: moq_net::OriginProducer,
}

#[derive(uniffi::Object)]
pub struct MoqOriginConsumer {
	inner: moq_net::OriginConsumer,
}

#[derive(uniffi::Object)]
pub struct MoqAnnounced {
	task: Task<Announced>,
}

struct Announced {
	inner: moq_net::AnnounceConsumer,
}

impl Announced {
	async fn next(&mut self) -> Result<Option<Arc<MoqAnnouncement>>, MoqError> {
		loop {
			match self.inner.next().await {
				// Active and Restart both carry a broadcast; skip unannounce events.
				Some((path, event)) => {
					let Some(broadcast) = event.broadcast() else {
						continue;
					};
					return Ok(Some(Arc::new(MoqAnnouncement {
						path: path.to_string(),
						broadcast: Arc::new(MoqBroadcastConsumer::new(broadcast)),
					})));
				}
				None => return Ok(None),
			}
		}
	}

	async fn available(&mut self) -> Result<Arc<MoqBroadcastConsumer>, MoqError> {
		loop {
			match self.inner.next().await {
				// Active and Restart both carry a broadcast; skip unannounce events.
				Some((_path, event)) => match event.broadcast() {
					Some(broadcast) => return Ok(Arc::new(MoqBroadcastConsumer::new(broadcast))),
					None => continue,
				},
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
	pub(crate) fn inner(&self) -> &moq_net::OriginProducer {
		&self.inner
	}

	/// Wrap an existing `moq_net::OriginProducer` (e.g. one auto-created
	/// during `MoqClient::connect`) so it can cross the FFI boundary.
	pub(crate) fn from_inner(inner: moq_net::OriginProducer) -> Self {
		Self { inner }
	}
}

impl MoqOriginConsumer {
	pub(crate) fn from_inner(inner: moq_net::OriginConsumer) -> Self {
		Self { inner }
	}
}

#[uniffi::export]
impl MoqOriginProducer {
	/// Create a new origin for publishing and/or consuming broadcasts.
	#[uniffi::constructor]
	pub fn new() -> Arc<Self> {
		let _guard = crate::ffi::RUNTIME.enter();
		Arc::new(Self {
			inner: moq_net::Origin::random().produce(),
		})
	}

	/// Attach a shared [`MoqCache`] cascaded onto broadcasts this origin *creates* via its
	/// `create_broadcast` path (and their tracks). Pass the same [`MoqCache`] elsewhere to pool one
	/// budget. Returns a new handle over the same origin.
	///
	/// Note this does not affect broadcasts created separately via
	/// [`MoqBroadcastProducer::new`](crate::producer::MoqBroadcastProducer::new) and then announced;
	/// attach the cache to those broadcasts with
	/// [`MoqBroadcastProducer::with_cache`](crate::producer::MoqBroadcastProducer::with_cache).
	pub fn with_cache(&self, cache: &MoqCache) -> Arc<Self> {
		let _guard = crate::ffi::RUNTIME.enter();
		Arc::new(Self {
			inner: self.inner.clone().with_cache(cache.inner()),
		})
	}

	/// Create a consumer for this origin.
	pub fn consume(&self) -> Arc<MoqOriginConsumer> {
		let _guard = crate::ffi::RUNTIME.enter();
		Arc::new(MoqOriginConsumer {
			inner: self.inner.consume(),
		})
	}

	/// Announce a broadcast to this origin under the given path so
	/// subscribers can discover it. Named `announce` (not `publish`) so
	/// the `MoqSession::publisher().announce(...)` chain doesn't stutter
	/// "publisher.publish".
	pub fn announce(&self, path: String, broadcast: &MoqBroadcastProducer) -> Result<(), MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let consumer = broadcast.consume_inner()?;
		// Surfaces Error::Unauthorized (out of scope) via the MoqError::Protocol conversion.
		let publish = self.inner.publish_broadcast(path.as_str(), &consumer)?;

		// Auto-unannounce when the broadcast closes (all producers dropped). The origin no longer
		// watches closure itself, so the spawn lives here at the runtime-bound FFI boundary.
		tokio::spawn(async move {
			consumer.closed().await;
			drop(publish);
		});

		Ok(())
	}
}

#[uniffi::export]
impl MoqOriginConsumer {
	/// Subscribe to all broadcast announcements under a prefix.
	pub fn announced(&self, prefix: String) -> Result<Arc<MoqAnnounced>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let origin = self.inner.with_root(prefix).ok_or(MoqError::Unauthorized)?;
		Ok(Arc::new(MoqAnnounced {
			task: Task::new(Announced {
				inner: origin.announced(),
			}),
		}))
	}

	/// Wait for a specific broadcast to be announced by path.
	pub fn announced_broadcast(&self, path: String) -> Result<Arc<MoqAnnouncedBroadcast>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let origin = self.inner.with_root(path).ok_or(MoqError::Unauthorized)?;
		Ok(Arc::new(MoqAnnouncedBroadcast {
			task: Task::new(Announced {
				inner: origin.announced(),
			}),
		}))
	}

	/// Request a broadcast by path, resolving as soon as it can be served.
	///
	/// Returns the announced broadcast immediately if one exists; otherwise falls back to a
	/// dynamic handler on the origin (if any) and resolves once it serves the broadcast, or
	/// errors if nothing can serve it. Unlike `announced_broadcast`, this does *not* wait
	/// indefinitely for a future announcement: it resolves or fails based on what is
	/// announced now plus any dynamic fallback. Drop the returned future to cancel.
	pub async fn request_broadcast(&self, path: String) -> Result<Arc<MoqBroadcastConsumer>, MoqError> {
		let broadcast = self.inner.request_broadcast(path.as_str()).await?;
		Ok(Arc::new(MoqBroadcastConsumer::new(broadcast)))
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
