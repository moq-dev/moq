//! This module contains the structs and functions for the MoQ catalog format
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, MutexGuard};

/// The catalog format is a JSON file that describes the tracks available in a broadcast.
use serde::{Deserialize, Serialize};

use crate::Result;
use crate::catalog::{Audio, AudioConfig, Chat, User, Video, VideoConfig};
use moq_lite::Produce;

/// A catalog track, created by a broadcaster to describe the tracks available in a broadcast.
#[serde_with::serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Catalog {
	/// Video track information with multiple renditions.
	///
	/// Contains a map of video track renditions that the viewer can choose from
	/// based on their preferences (resolution, bitrate, codec, etc).
	#[serde(default)]
	pub video: Option<Video>,

	/// Audio track information with multiple renditions.
	///
	/// Contains a map of audio track renditions that the viewer can choose from
	/// based on their preferences (codec, bitrate, language, etc).
	#[serde(default)]
	pub audio: Option<Audio>,

	/// User metadata for the broadcaster
	#[serde(default)]
	pub user: Option<User>,

	/// Chat track metadata
	#[serde(default)]
	pub chat: Option<Chat>,

	/// Preview information about the broadcast
	#[serde(default)]
	pub preview: Option<moq_lite::Track>,
}

impl Catalog {
	/// The default name for the catalog track.
	pub const DEFAULT_NAME: &str = "catalog.json";

	/// Parse a catalog from a string.
	#[allow(clippy::should_implement_trait)]
	pub fn from_str(s: &str) -> Result<Self> {
		Ok(serde_json::from_str(s)?)
	}

	/// Parse a catalog from a slice of bytes.
	pub fn from_slice(v: &[u8]) -> Result<Self> {
		Ok(serde_json::from_slice(v)?)
	}

	/// Parse a catalog from a reader.
	pub fn from_reader(reader: impl std::io::Read) -> Result<Self> {
		Ok(serde_json::from_reader(reader)?)
	}

	/// Serialize the catalog to a string.
	pub fn to_string(&self) -> Result<String> {
		Ok(serde_json::to_string(self)?)
	}

	/// Serialize the catalog to a pretty string.
	pub fn to_string_pretty(&self) -> Result<String> {
		Ok(serde_json::to_string_pretty(self)?)
	}

	/// Serialize the catalog to a vector of bytes.
	pub fn to_vec(&self) -> Result<Vec<u8>> {
		Ok(serde_json::to_vec(self)?)
	}

	/// Serialize the catalog to a writer.
	pub fn to_writer(&self, writer: impl std::io::Write) -> Result<()> {
		Ok(serde_json::to_writer(writer, self)?)
	}

	/// Produce a catalog track that describes the available media tracks.
	pub fn produce(self) -> Produce<CatalogProducer, CatalogConsumer> {
		let track = Catalog::default_track().produce();

		Produce {
			producer: CatalogProducer::new(track.producer, self),
			consumer: track.consumer.into(),
		}
	}

	pub fn default_track() -> moq_lite::Track {
		moq_lite::Track {
			name: Catalog::DEFAULT_NAME.to_string(),
			priority: 100,
		}
	}

	// A silly helpers to change None -> Some or Some -> None based on the number of renditions.
	pub fn insert_video(&mut self, name: String, config: VideoConfig) -> &mut Video {
		let mut video = self.video.take().unwrap_or_default();
		video.renditions.insert(name, config);
		self.video = Some(video);
		self.video.as_mut().unwrap()
	}

	pub fn insert_audio(&mut self, name: String, config: AudioConfig) -> &mut Audio {
		let mut audio = self.audio.take().unwrap_or_default();
		audio.renditions.insert(name, config);
		self.audio = Some(audio);
		self.audio.as_mut().unwrap()
	}

	pub fn remove_video(&mut self, name: &str) {
		let mut video = self.video.take().unwrap_or_default();
		video.renditions.remove(name);

		match video.renditions.is_empty() {
			true => self.video = None,
			false => self.video = Some(video),
		}
	}

	pub fn remove_audio(&mut self, name: &str) {
		let mut audio = self.audio.take().unwrap_or_default();
		audio.renditions.remove(name);

		match audio.renditions.is_empty() {
			true => self.audio = None,
			false => self.audio = Some(audio),
		}
	}
}

/// Produces a catalog track that describes the available media tracks.
///
/// The JSON catalog is updated when tracks are added/removed but is *not* automatically published.
/// You'll have to call [`lock`](Self::lock) to update and publish the catalog.
#[derive(Clone)]
pub struct CatalogProducer {
	/// Access to the underlying track producer.
	pub track: moq_lite::TrackProducer,
	current: Arc<Mutex<Catalog>>,
}

impl CatalogProducer {
	/// Create a new catalog producer with the given track and initial catalog.
	fn new(track: moq_lite::TrackProducer, init: Catalog) -> Self {
		Self {
			current: Arc::new(Mutex::new(init)),
			track,
		}
	}

	/// Get mutable access to the catalog, publishing it after any changes.
	pub fn lock(&mut self) -> CatalogGuard<'_> {
		CatalogGuard {
			catalog: self.current.lock().unwrap(),
			track: &mut self.track,
			updated: false,
		}
	}

	/// Create a consumer for this catalog, receiving updates as they're published.
	pub fn consume(&self) -> CatalogConsumer {
		CatalogConsumer::new(self.track.consume())
	}

	/// Finish publishing to this catalog and close the track.
	pub fn close(self) {
		self.track.close();
	}
}

impl From<moq_lite::TrackProducer> for CatalogProducer {
	fn from(inner: moq_lite::TrackProducer) -> Self {
		Self::new(inner, Catalog::default())
	}
}

/// RAII guard for modifying a catalog with automatic publishing on drop.
///
/// Obtained via [`CatalogProducer::lock`].
pub struct CatalogGuard<'a> {
	catalog: MutexGuard<'a, Catalog>,
	track: &'a mut moq_lite::TrackProducer,
	updated: bool,
}

impl<'a> Deref for CatalogGuard<'a> {
	type Target = Catalog;

	fn deref(&self) -> &Self::Target {
		&self.catalog
	}
}

impl<'a> DerefMut for CatalogGuard<'a> {
	fn deref_mut(&mut self) -> &mut Self::Target {
		self.updated = true;
		&mut self.catalog
	}
}

impl Drop for CatalogGuard<'_> {
	fn drop(&mut self) {
		// Avoid publishing if we didn't use `&mut self` at all.
		if !self.updated {
			return;
		}

		let mut group = self.track.append_group();

		// TODO decide if this should return an error, or be impossible to fail
		let frame = self.catalog.to_string().expect("invalid catalog");
		group.write_frame(frame);
		group.close();
	}
}

/// A catalog consumer, used to receive catalog updates and discover tracks.
///
/// This wraps a `moq_lite::TrackConsumer` and automatically deserializes JSON
/// catalog data to discover available audio and video tracks in a broadcast.
#[derive(Clone)]
pub struct CatalogConsumer {
	/// Access to the underlying track consumer.
	pub track: moq_lite::TrackConsumer,
	group: Option<moq_lite::GroupConsumer>,
}

impl CatalogConsumer {
	/// Create a new catalog consumer from a MoQ track consumer.
	pub fn new(track: moq_lite::TrackConsumer) -> Self {
		Self { track, group: None }
	}

	/// Get the next catalog update.
	///
	/// This method waits for the next catalog publication and returns the
	/// catalog data. If there are no more updates, `None` is returned.
	pub async fn next(&mut self) -> Result<Option<Catalog>> {
		loop {
			tokio::select! {
				res = self.track.next_group() => {
					match res? {
						Some(group) => {
							// Use the new group.
							self.group = Some(group);
						}
						// The track has ended, so we should return None.
						None => return Ok(None),
					}
				},
				Some(frame) = async { self.group.as_mut()?.read_frame().await.transpose() } => {
					self.group.take(); // We don't support deltas yet
					let catalog = Catalog::from_slice(&frame?)?;
					return Ok(Some(catalog));
				}
			}
		}
	}

	/// Wait until the catalog track is closed.
	pub async fn closed(&self) -> Result<()> {
		Ok(self.track.closed().await?)
	}
}

impl From<moq_lite::TrackConsumer> for CatalogConsumer {
	fn from(inner: moq_lite::TrackConsumer) -> Self {
		Self::new(inner)
	}
}

#[cfg(test)]
mod test {
	use std::collections::BTreeMap;

	use crate::catalog::{AudioCodec::Opus, AudioConfig, Container, H264, VideoConfig};

	use super::*;

	#[test]
	fn simple() {
		let mut encoded = r#"{
			"video": {
				"renditions": {
					"video": {
						"codec": "avc1.64001f",
						"codedWidth": 1280,
						"codedHeight": 720,
						"bitrate": 6000000,
						"framerate": 30.0,
						"container": "legacy"
					}
				},
				"priority": 1
			},
			"audio": {
				"renditions": {
					"audio": {
						"codec": "opus",
						"sampleRate": 48000,
						"numberOfChannels": 2,
						"bitrate": 128000,
						"container": "legacy"
					}
				},
				"priority": 2
			}
		}"#
		.to_string();

		encoded.retain(|c| !c.is_whitespace());

		let mut video_renditions = BTreeMap::new();
		video_renditions.insert(
			"video".to_string(),
			VideoConfig {
				codec: H264 {
					profile: 0x64,
					constraints: 0x00,
					level: 0x1f,
					inline: false,
				}
				.into(),
				description: None,
				coded_width: Some(1280),
				coded_height: Some(720),
				display_ratio_width: None,
				display_ratio_height: None,
				bitrate: Some(6_000_000),
				framerate: Some(30.0),
				optimize_for_latency: None,
				container: Container::Legacy,
				min_buffer: None,
			},
		);

		let mut audio_renditions = BTreeMap::new();
		audio_renditions.insert(
			"audio".to_string(),
			AudioConfig {
				codec: Opus,
				sample_rate: 48_000,
				channel_count: 2,
				bitrate: Some(128_000),
				description: None,
				container: Container::Legacy,
				min_buffer: None,
			},
		);

		let decoded = Catalog {
			video: Some(Video {
				renditions: video_renditions,
				priority: 1,
				display: None,
				rotation: None,
				flip: None,
			}),
			audio: Some(Audio {
				renditions: audio_renditions,
				priority: 2,
			}),
			..Default::default()
		};

		let output = Catalog::from_str(&encoded).expect("failed to decode");
		assert_eq!(decoded, output, "wrong decoded output");

		let output = decoded.to_string().expect("failed to encode");
		assert_eq!(encoded, output, "wrong encoded output");
	}
}
