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
	track: moq_lite::TrackSubscriber,
	group: Option<moq_lite::GroupConsumer>,
}

impl Catalog {
	async fn next(&mut self) -> Result<Option<MoqCatalog>, MoqError> {
		loop {
			tokio::select! {
				res = self.track.recv_group() => {
					match res? {
						Some(group) => {
							self.group = Some(group);
						}
						None => return Ok(None),
					}
				},
				Some(frame) = async { self.group.as_mut()?.read_frame().await.transpose() } => {
					self.group.take(); // We don't support deltas yet

					let frame_data = frame?;
					let json: serde_json::Map<String, serde_json::Value> =
						serde_json::from_slice(&frame_data)
							.map_err(|e| MoqError::Codec(e.to_string()))?;

					let video: hang::catalog::Video = json
						.get("video")
						.map(|v| serde_json::from_value(v.clone()))
						.transpose()
						.map_err(|e| MoqError::Codec(e.to_string()))?
						.unwrap_or_default();

					let audio: hang::catalog::Audio = json
						.get("audio")
						.map(|v| serde_json::from_value(v.clone()))
						.transpose()
						.map_err(|e| MoqError::Codec(e.to_string()))?
						.unwrap_or_default();

					return Ok(Some(convert_catalog(&video, &audio)));
				}
			}
		}
	}
}

#[derive(uniffi::Object)]
pub struct MoqMediaConsumer {
	task: Task<Media>,
}

struct Media {
	inner: moq_mux::ordered::Consumer<moq_mux::hang::Media>,
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
		let track = self
			.inner
			.subscribe_track(&hang::catalog::default_track(), moq_lite::Subscription::default())?;
		Ok(Arc::new(MoqCatalogConsumer {
			task: Task::new(Catalog { track, group: None }),
		}))
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
		let track = self
			.inner
			.subscribe_track(&moq_lite::Track::new(name), moq_lite::Subscription::default())?;
		let container: hang::catalog::Container = container.into();
		let latency = std::time::Duration::from_millis(max_latency_ms);
		let media = moq_mux::hang::Media::try_from(&container)?;
		let consumer = moq_mux::ordered::Consumer::new(track, media).with_latency(latency);
		Ok(Arc::new(MoqMediaConsumer {
			task: Task::new(Media { inner: consumer }),
		}))
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
