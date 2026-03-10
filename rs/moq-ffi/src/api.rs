use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use bytes::Buf;
use url::Url;

use crate::error::MoqError;
use crate::ffi;

// ---- Catalog records ----

#[derive(uniffi::Record)]
pub struct MoqCatalog {
	pub video: HashMap<String, MoqVideoRendition>,
	pub audio: HashMap<String, MoqAudioRendition>,
	pub display_width: Option<u32>,
	pub display_height: Option<u32>,
	pub rotation: Option<f64>,
	pub flip: Option<bool>,
	pub user: Option<MoqUser>,
}

#[derive(uniffi::Record)]
pub struct MoqVideoRendition {
	pub codec: String,
	pub description: Option<Vec<u8>>,
	pub coded_width: Option<u32>,
	pub coded_height: Option<u32>,
	pub display_ratio_width: Option<u32>,
	pub display_ratio_height: Option<u32>,
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
pub struct FrameData {
	pub payload: Vec<u8>,
	pub timestamp_us: u64,
	pub keyframe: bool,
}

/// A broadcast announced by an origin.
#[derive(uniffi::Record)]
pub struct AnnouncedInfo {
	pub path: String,
	pub active: bool,
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
					coded_width: config.coded_width,
					coded_height: config.coded_height,
					display_ratio_width: config.display_ratio_width,
					display_ratio_height: config.display_ratio_height,
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

	let (display_width, display_height) = match &catalog.video.display {
		Some(d) => (Some(d.width), Some(d.height)),
		None => (None, None),
	};

	MoqCatalog {
		video,
		audio,
		display_width,
		display_height,
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

// ---- UniFFI Objects ----

#[derive(uniffi::Object)]
pub struct MoqOrigin {
	inner: moq_lite::OriginProducer,
}

#[derive(uniffi::Object)]
pub struct MoqSession {
	close: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
	task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<Result<(), MoqError>>>>,
}

#[derive(uniffi::Object)]
pub struct MoqBroadcast {
	inner: moq_lite::BroadcastConsumer,
}

#[derive(uniffi::Object)]
pub struct MoqCatalogStream {
	inner: Arc<tokio::sync::Mutex<hang::CatalogConsumer>>,
}

#[derive(uniffi::Object)]
pub struct MoqTrack {
	inner: Arc<tokio::sync::Mutex<hang::container::OrderedConsumer>>,
}

#[derive(uniffi::Object)]
pub struct MoqAnnounced {
	inner: Arc<tokio::sync::Mutex<moq_lite::OriginConsumer>>,
}

#[derive(uniffi::Object)]
pub struct MoqPublisher {
	inner: std::sync::Mutex<(moq_lite::BroadcastProducer, moq_mux::CatalogProducer)>,
}

#[derive(uniffi::Object)]
pub struct MoqMedia {
	inner: std::sync::Mutex<Option<moq_mux::import::Decoder>>,
}

// ---- Top-level functions ----

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

/// Create a new origin for publishing and/or consuming broadcasts.
#[uniffi::export]
pub fn moq_origin_create() -> Arc<MoqOrigin> {
	Arc::new(MoqOrigin {
		inner: moq_lite::OriginProducer::default(),
	})
}

/// Connect to a MoQ server.
///
/// `publish` and `consume` are optional origins for the respective directions.
#[uniffi::export]
pub async fn moq_connect(
	url: String,
	publish: Option<Arc<MoqOrigin>>,
	consume: Option<Arc<MoqOrigin>>,
) -> Result<Arc<MoqSession>, MoqError> {
	let url = Url::parse(&url)?;
	let publish_consumer = publish.map(|o| o.inner.consume());
	let consume_producer = consume.map(|o| o.inner.clone());

	let close_channel = tokio::sync::oneshot::channel();

	let task = ffi::HANDLE.spawn(async move {
		let client = moq_native::ClientConfig::default()
			.init()
			.map_err(|err| MoqError::Error {
				msg: format!("connect error: {err}"),
			})?;

		let session = client
			.with_publish(publish_consumer)
			.with_consume(consume_producer)
			.connect(url)
			.await
			.map_err(|err| MoqError::Error {
				msg: format!("connect error: {err}"),
			})?;

		tokio::select! {
			_ = close_channel.1 => Ok(()),
			res = session.closed() => res.map_err(Into::into),
		}
	});

	Ok(Arc::new(MoqSession {
		close: std::sync::Mutex::new(Some(close_channel.0)),
		task: tokio::sync::Mutex::new(Some(task)),
	}))
}

// ---- Origin ----

#[uniffi::export]
impl MoqOrigin {
	/// Consume a broadcast from this origin by path.
	pub fn consume(&self, path: String) -> Result<Arc<MoqBroadcast>, MoqError> {
		let broadcast = self
			.inner
			.consume()
			.consume_broadcast(path.as_str())
			.ok_or_else(|| MoqError::Error {
				msg: "broadcast not found".into(),
			})?;
		Ok(Arc::new(MoqBroadcast { inner: broadcast }))
	}

	/// Publish a broadcast to this origin under the given path.
	pub fn publish(&self, path: String, broadcast: &MoqPublisher) -> Result<(), MoqError> {
		let guard = broadcast.inner.lock().unwrap();
		let consumer = guard.0.consume();
		self.inner.publish_broadcast(path.as_str(), consumer);
		Ok(())
	}

	/// Subscribe to broadcast announcements on this origin.
	pub fn announced(&self) -> Arc<MoqAnnounced> {
		Arc::new(MoqAnnounced {
			inner: Arc::new(tokio::sync::Mutex::new(self.inner.consume())),
		})
	}
}

// ---- Session ----

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

// ---- Announced ----

#[uniffi::export]
impl MoqAnnounced {
	/// Get the next broadcast announcement. Returns `None` when the origin is closed.
	pub async fn next(&self) -> Result<Option<AnnouncedInfo>, MoqError> {
		let inner = self.inner.clone();
		ffi::HANDLE
			.spawn(async move {
				let mut consumer = inner.lock().await;
				match consumer.announced().await {
					Some((path, broadcast)) => Ok(Some(AnnouncedInfo {
						path: path.to_string(),
						active: broadcast.is_some(),
					})),
					None => Ok(None),
				}
			})
			.await?
	}
}

// ---- Consuming ----

#[uniffi::export]
impl MoqBroadcast {
	/// Create a catalog consumer for this broadcast.
	pub fn catalog(&self) -> Result<Arc<MoqCatalogStream>, MoqError> {
		let track = self.inner.subscribe_track(&hang::catalog::Catalog::default_track())?;
		let consumer = hang::CatalogConsumer::from(track);
		Ok(Arc::new(MoqCatalogStream {
			inner: Arc::new(tokio::sync::Mutex::new(consumer)),
		}))
	}

	/// Subscribe to a track by name, delivering frames in decode order.
	///
	/// `max_latency_ms` controls the maximum buffering before skipping a GoP.
	pub fn subscribe_track(&self, name: String, max_latency_ms: u64) -> Result<Arc<MoqTrack>, MoqError> {
		let track = self.inner.subscribe_track(&moq_lite::Track { name, priority: 0 })?;
		let latency = std::time::Duration::from_millis(max_latency_ms);
		let consumer = hang::container::OrderedConsumer::new(track, latency);
		Ok(Arc::new(MoqTrack {
			inner: Arc::new(tokio::sync::Mutex::new(consumer)),
		}))
	}
}

#[uniffi::export]
impl MoqCatalogStream {
	/// Get the next catalog update. Returns `None` when the track ends.
	pub async fn next(&self) -> Result<Option<MoqCatalog>, MoqError> {
		let inner = self.inner.clone();
		ffi::HANDLE
			.spawn(async move {
				let mut consumer = inner.lock().await;
				match consumer.next().await {
					Ok(Some(catalog)) => Ok(Some(convert_catalog(&catalog))),
					Ok(None) => Ok(None),
					Err(e) => Err(MoqError::from(e)),
				}
			})
			.await?
	}
}

#[uniffi::export]
impl MoqTrack {
	/// Get the next frame. Returns `None` when the track ends.
	pub async fn next(&self) -> Result<Option<FrameData>, MoqError> {
		let inner = self.inner.clone();
		ffi::HANDLE
			.spawn(async move {
				let mut consumer = inner.lock().await;
				match consumer.read().await {
					Ok(Some(frame)) => {
						let payload: Vec<u8> = (0..frame.payload.num_chunks())
							.filter_map(|i| frame.payload.get_chunk(i))
							.flat_map(|chunk| chunk.iter().copied())
							.collect();

						let timestamp_us: u64 =
							frame.timestamp.as_micros().try_into().map_err(|_| MoqError::Error {
								msg: "timestamp overflow".into(),
							})?;

						Ok(Some(FrameData {
							payload,
							timestamp_us,
							keyframe: frame.is_keyframe(),
						}))
					}
					Ok(None) => Ok(None),
					Err(e) => Err(MoqError::from(e)),
				}
			})
			.await?
	}
}

// ---- Publishing ----

/// Create a new broadcast for publishing media tracks.
#[uniffi::export]
pub fn moq_publish_create() -> Result<Arc<MoqPublisher>, MoqError> {
	let mut broadcast = moq_lite::BroadcastProducer::new();
	let catalog = moq_mux::CatalogProducer::new(&mut broadcast)?;
	Ok(Arc::new(MoqPublisher {
		inner: std::sync::Mutex::new((broadcast, catalog)),
	}))
}

#[uniffi::export]
impl MoqPublisher {
	/// Create a new media track for this broadcast.
	///
	/// `format` controls the encoding of `init` and frame payloads.
	pub fn media_ordered(&self, format: String, init: Vec<u8>) -> Result<Arc<MoqMedia>, MoqError> {
		let guard = self.inner.lock().unwrap();
		let format = moq_mux::import::DecoderFormat::from_str(&format).map_err(|_| MoqError::Error {
			msg: format!("unknown format: {format}"),
		})?;

		let mut buf = init.as_slice();
		let decoder =
			moq_mux::import::Decoder::new(guard.0.clone(), guard.1.clone(), format, &mut buf).map_err(|err| {
				MoqError::Error {
					msg: format!("init failed: {err}"),
				}
			})?;

		Ok(Arc::new(MoqMedia {
			inner: std::sync::Mutex::new(Some(decoder)),
		}))
	}
}

#[uniffi::export]
impl MoqMedia {
	/// Write a frame to this media track.
	///
	/// `timestamp_us` is the presentation timestamp in microseconds.
	pub fn write_frame(&self, payload: Vec<u8>, timestamp_us: u64) -> Result<(), MoqError> {
		let mut guard = self.inner.lock().unwrap();
		let decoder = guard.as_mut().ok_or_else(|| MoqError::Error {
			msg: "media closed".into(),
		})?;

		let timestamp = hang::container::Timestamp::from_micros(timestamp_us)?;
		let mut data = payload.as_slice();
		decoder
			.decode_frame(&mut data, Some(timestamp))
			.map_err(|err| MoqError::Error {
				msg: format!("decode failed: {err}"),
			})?;

		if data.has_remaining() {
			return Err(MoqError::Error {
				msg: "buffer was not fully consumed".into(),
			});
		}

		Ok(())
	}

	/// Close this media track and finalize encoding.
	pub fn close(&self) -> Result<(), MoqError> {
		let mut guard = self.inner.lock().unwrap();
		if let Some(mut decoder) = guard.take() {
			decoder.finish().map_err(|err| MoqError::Error {
				msg: format!("close failed: {err}"),
			})?;
		}
		Ok(())
	}
}
