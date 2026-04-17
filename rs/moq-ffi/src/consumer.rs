use std::sync::Arc;

use bytes::Buf;

use crate::error::MoqError;
use crate::ffi::Task;
use crate::media::*;

#[derive(Clone, uniffi::Object)]
pub struct MoqBroadcastConsumer {
	inner: moq_lite::BroadcastConsumer,
}

impl MoqBroadcastConsumer {
	pub(crate) fn new(inner: moq_lite::BroadcastConsumer) -> Self {
		Self { inner }
	}
}

#[derive(uniffi::Object)]
pub struct MoqCatalogConsumer {
	task: Task<Catalog>,
}

struct Catalog {
	inner: hang::CatalogConsumer,
}

impl Catalog {
	async fn next(&mut self) -> Result<Option<MoqCatalog>, MoqError> {
		match self.inner.next().await {
			Ok(Some(catalog)) => Ok(Some(convert_catalog(&catalog))),
			Ok(None) => Ok(None),
			Err(e) => Err(e.into()),
		}
	}
}

#[derive(uniffi::Object)]
pub struct MoqMediaConsumer {
	task: Task<Media>,
}

struct Media {
	inner: moq_mux::ordered::Consumer<hang::catalog::Container>,
}

impl Media {
	async fn next(&mut self) -> Result<Option<MoqFrame>, MoqError> {
		let frame = self.inner.read().await?;

		let Some(frame) = frame else {
			return Ok(None);
		};

		let timestamp_us: u64 = frame
			.timestamp
			.as_micros()
			.try_into()
			.map_err(|_| MoqError::Codec("timestamp overflow".into()))?;

		let mut buf = frame.payload;
		let payload = buf.copy_to_bytes(buf.remaining()).to_vec();

		Ok(Some(MoqFrame {
			payload,
			timestamp_us,
			keyframe: frame.keyframe,
		}))
	}
}

// ---- Broadcast ----

#[uniffi::export]
impl MoqBroadcastConsumer {
	/// Subscribe to the catalog for this broadcast.
	pub fn subscribe_catalog(&self) -> Result<Arc<MoqCatalogConsumer>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let track = self.inner.subscribe_track(&hang::catalog::Catalog::default_track())?;
		let consumer = hang::CatalogConsumer::from(track);
		Ok(Arc::new(MoqCatalogConsumer {
			task: Task::new(Catalog { inner: consumer }),
		}))
	}

	/// Subscribe to a raw track by name — same pattern as moq-boy's command/status tracks.
	///
	/// Frames are returned as plain byte payloads with no codec or container parsing.
	pub fn subscribe_raw(&self, name: String) -> Result<Arc<MoqRawConsumer>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let track = self.inner.subscribe_track(&moq_lite::Track { name, priority: 0 })?;
		Ok(Arc::new(MoqRawConsumer::new(track)))
	}

	/// Subscribe to a track by name, delivering frames in decode order.
	///
	/// `container` is the track container from the catalog.
	/// `max_latency_ms` controls the maximum buffering before skipping a GoP.
	pub fn subscribe_media(
		&self,
		name: String,
		container: Container,
		max_latency_ms: u64,
	) -> Result<Arc<MoqMediaConsumer>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let track = self.inner.subscribe_track(&moq_lite::Track { name, priority: 0 })?;
		let container: hang::catalog::Container = container.into();
		let latency = std::time::Duration::from_millis(max_latency_ms);
		let consumer = moq_mux::ordered::Consumer::new(track, container).with_latency(latency);
		Ok(Arc::new(MoqMediaConsumer {
			task: Task::new(Media { inner: consumer }),
		}))
	}
}

// ---- Raw Consumer ----

struct RawInner {
	track: moq_lite::TrackConsumer,
}

impl RawInner {
	async fn next_group(&mut self) -> Result<Option<moq_lite::GroupConsumer>, MoqError> {
		Ok(self.track.next_group().await?)
	}
}

#[derive(uniffi::Object)]
pub struct MoqRawConsumer {
	task: Task<RawInner>,
}

impl MoqRawConsumer {
	pub(crate) fn new(track: moq_lite::TrackConsumer) -> Self {
		Self {
			task: Task::new(RawInner { track }),
		}
	}
}

#[uniffi::export]
impl MoqRawConsumer {
	/// Return the next group in order. Returns `None` when the track ends.
	///
	/// NOTE: Groups may be skipped if the reader falls behind or groups expire.
	pub async fn next_group(&self) -> Result<Option<Arc<MoqRawGroupConsumer>>, MoqError> {
		self.task
			.run(|mut state| async move {
				Ok(state.next_group().await?.map(|group| {
					Arc::new(MoqRawGroupConsumer {
						sequence: group.info.sequence,
						task: Task::new(RawGroupInner { group }),
					})
				}))
			})
			.await
	}

	pub fn cancel(&self) {
		self.task.cancel();
	}
}

struct RawGroupInner {
	group: moq_lite::GroupConsumer,
}

impl RawGroupInner {
	async fn read_frame(&mut self) -> Result<Option<Vec<u8>>, MoqError> {
		Ok(self.group.read_frame().await?.map(|b| b.to_vec()))
	}
}

#[derive(uniffi::Object)]
pub struct MoqRawGroupConsumer {
	sequence: u64,
	task: Task<RawGroupInner>,
}

impl MoqRawGroupConsumer {
	pub(crate) fn new(group: moq_lite::GroupConsumer) -> Self {
		Self {
			sequence: group.info.sequence,
			task: Task::new(RawGroupInner { group }),
		}
	}
}

#[uniffi::export]
impl MoqRawGroupConsumer {
	/// The sequence number of this group within the track.
	pub fn sequence(&self) -> u64 {
		self.sequence
	}

	/// Read the next frame in this group. Returns `None` when the group ends.
	pub async fn read_frame(&self) -> Result<Option<Vec<u8>>, MoqError> {
		self.task.run(|mut state| async move { state.read_frame().await }).await
	}

	pub fn cancel(&self) {
		self.task.cancel();
	}
}

// ---- Catalog Consumer ----

#[uniffi::export]
impl MoqCatalogConsumer {
	/// Get the next catalog update. Returns `None` when the track ends or is closed.
	pub async fn next(&self) -> Result<Option<MoqCatalog>, MoqError> {
		self.task.run(|mut state| async move { state.next().await }).await
	}

	/// Cancel all current and future `next()` calls.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}

// ---- Media Consumer ----

#[uniffi::export]
impl MoqMediaConsumer {
	/// Get the next frame. Returns `None` when the track ends or is closed.
	pub async fn next(&self) -> Result<Option<MoqFrame>, MoqError> {
		self.task.run(|mut state| async move { state.next().await }).await
	}

	/// Cancel all current and future `next()` calls.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}
