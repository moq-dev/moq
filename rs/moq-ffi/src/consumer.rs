use std::collections::HashMap;
use std::sync::Arc;

use bytes::Buf;

use crate::error::MoqError;
use crate::ffi::{self, Abort};

// ---- Records ----

#[derive(uniffi::Record)]
pub struct MoqDimensions {
	pub width: u32,
	pub height: u32,
}

#[derive(uniffi::Record)]
pub struct MoqCatalog {
	pub video: HashMap<String, MoqVideoRendition>,
	pub audio: HashMap<String, MoqAudioRendition>,
	pub display: Option<MoqDimensions>,
	pub rotation: Option<f64>,
	pub flip: Option<bool>,
	pub user: Option<MoqUser>,
}

#[derive(uniffi::Record)]
pub struct MoqVideoRendition {
	pub codec: String,
	pub description: Option<Vec<u8>>,
	pub coded: Option<MoqDimensions>,
	pub display_ratio: Option<MoqDimensions>,
	pub bitrate: Option<u64>,
	pub framerate: Option<f64>,
}

#[derive(uniffi::Record)]
pub struct MoqAudioRendition {
	pub codec: String,
	pub description: Option<Vec<u8>>,
	pub sample_rate: u32,
	pub channel_count: u32,
	pub bitrate: Option<u64>,
}

#[derive(uniffi::Record)]
pub struct MoqUser {
	pub id: Option<String>,
	pub name: Option<String>,
	pub avatar: Option<String>,
	pub color: Option<String>,
}

/// A decoded media frame.
#[derive(uniffi::Record)]
pub struct MoqFrame {
	pub payload: Vec<u8>,
	pub timestamp_us: u64,
	pub keyframe: bool,
}

// ---- UniFFI Objects ----

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
	inner: tokio::sync::Mutex<hang::CatalogConsumer>,
	abort: Abort,
}

#[derive(uniffi::Object)]
pub struct MoqMediaConsumer {
	inner: tokio::sync::Mutex<hang::container::OrderedConsumer>,
	abort: Abort,
}

// ---- Broadcast ----

#[uniffi::export]
impl MoqBroadcastConsumer {
	/// Subscribe to the catalog for this broadcast.
	pub fn subscribe_catalog(&self) -> Result<Arc<MoqCatalogConsumer>, MoqError> {
		let _guard = ffi::HANDLE.enter();
		let track = self.inner.subscribe_track(&hang::catalog::Catalog::default_track())?;
		let consumer = hang::CatalogConsumer::from(track);
		Ok(Arc::new(MoqCatalogConsumer {
			inner: tokio::sync::Mutex::new(consumer),
			abort: Abort::new(),
		}))
	}

	/// Subscribe to a media track by name, delivering frames in decode order.
	///
	/// `max_latency_ms` controls the maximum buffering before skipping a GoP.
	pub fn subscribe_media(&self, name: String, max_latency_ms: u64) -> Result<Arc<MoqMediaConsumer>, MoqError> {
		let _guard = ffi::HANDLE.enter();
		let track = self.inner.subscribe_track(&moq_lite::Track { name, priority: 0 })?;
		let latency = std::time::Duration::from_millis(max_latency_ms);
		let consumer = hang::container::OrderedConsumer::new(track, latency);
		Ok(Arc::new(MoqMediaConsumer {
			inner: tokio::sync::Mutex::new(consumer),
			abort: Abort::new(),
		}))
	}
}

// ---- Catalog Consumer ----

#[uniffi::export]
impl MoqCatalogConsumer {
	/// Get the next catalog update. Returns `None` when the track ends or is closed.
	pub async fn next(&self) -> Result<Option<MoqCatalog>, MoqError> {
		let mut consumer = self.inner.lock().await;
		tokio::select! {
			biased;
			_ = self.abort.aborted() => Ok(None),
			result = consumer.next() => match result {
				Ok(Some(catalog)) => Ok(Some(convert_catalog(&catalog))),
				Ok(None) => Ok(None),
				Err(e) => Err(e.into()),
			}
		}
	}

	/// Cancel this catalog stream, causing any pending `next()` to return `None`.
	pub fn cancel(&self) {
		self.abort.abort();
	}
}

impl Drop for MoqCatalogConsumer {
	fn drop(&mut self) {
		self.abort.abort();
	}
}

// ---- Media Consumer ----

#[uniffi::export]
impl MoqMediaConsumer {
	/// Get the next frame. Returns `None` when the track ends or is closed.
	pub async fn next(&self) -> Result<Option<MoqFrame>, MoqError> {
		let mut consumer = self.inner.lock().await;
		tokio::select! {
			biased;
			_ = self.abort.aborted() => Ok(None),
			result = consumer.read() => match result {
				Ok(Some(frame)) => {
					let keyframe = frame.is_keyframe();
					let timestamp_us: u64 =
						frame.timestamp.as_micros().try_into().map_err(|_| MoqError::Codec("timestamp overflow".into()))?;

					let mut buf = frame.payload;
					let payload = buf.copy_to_bytes(buf.remaining()).to_vec();

					Ok(Some(MoqFrame {
						payload,
						timestamp_us,
						keyframe,
					}))
				}
				Ok(None) => Ok(None),
				Err(e) => Err(e.into()),
			}
		}
	}

	/// Cancel this track, causing any pending `next()` call to return `None`.
	pub fn cancel(&self) {
		self.abort.abort();
	}
}

impl Drop for MoqMediaConsumer {
	fn drop(&mut self) {
		self.abort.abort();
	}
}

// ---- Conversion helpers ----

fn convert_catalog(catalog: &hang::catalog::Catalog) -> MoqCatalog {
	let video = catalog
		.video
		.renditions
		.iter()
		.map(|(name, config)| {
			(
				name.clone(),
				MoqVideoRendition {
					codec: config.codec.to_string(),
					description: config.description.as_ref().map(|d| d.to_vec()),
					coded: match (config.coded_width, config.coded_height) {
						(Some(w), Some(h)) => Some(MoqDimensions { width: w, height: h }),
						_ => None,
					},
					display_ratio: match (config.display_ratio_width, config.display_ratio_height) {
						(Some(w), Some(h)) => Some(MoqDimensions { width: w, height: h }),
						_ => None,
					},
					bitrate: config.bitrate,
					framerate: config.framerate,
				},
			)
		})
		.collect();

	let audio = catalog
		.audio
		.renditions
		.iter()
		.map(|(name, config)| {
			(
				name.clone(),
				MoqAudioRendition {
					codec: config.codec.to_string(),
					description: config.description.as_ref().map(|d| d.to_vec()),
					sample_rate: config.sample_rate,
					channel_count: config.channel_count,
					bitrate: config.bitrate,
				},
			)
		})
		.collect();

	let display = catalog.video.display.as_ref().map(|d| MoqDimensions {
		width: d.width,
		height: d.height,
	});

	MoqCatalog {
		video,
		audio,
		display,
		rotation: catalog.video.rotation,
		flip: catalog.video.flip,
		user: catalog.user.as_ref().map(|u| MoqUser {
			id: u.id.clone(),
			name: u.name.clone(),
			avatar: u.avatar.clone(),
			color: u.color.clone(),
		}),
	}
}
