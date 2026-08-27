//! JSON track publish/subscribe.
//!
//! A JSON track is application data a broadcast publishes alongside its media: a chat log, a
//! telemetry feed, a status document. The catalog entry
//! ([`hang::catalog::JsonConfig`]) says how to read it, so a consumer needs the track name and
//! nothing else about the application.
//!
//! The write and read sides are shaped differently on purpose. A publisher knows its mode at
//! compile time, so [`Snapshot`] and [`Stream`] are distinct types and `append`-on-a-snapshot can't
//! be written. A consumer only learns the mode from the catalog, so [`Consumer`] is one enum it
//! matches on.
//!
//! Mint a producer from the catalog:
//!
//! ```no_run
//! # use serde::{Deserialize, Serialize};
//! # #[derive(Serialize, Deserialize)]
//! # struct Message { text: String }
//! # fn example(catalog: &moq_mux::catalog::Producer) -> moq_mux::Result<()> {
//! let mut chat = catalog.json_stream::<Message>("chat", moq_mux::json::Config::default().with_compression(true))?;
//! chat.append(&Message { text: "hello".to_string() })?;
//! # Ok(())
//! # }
//! ```
//!
//! The catalog entry is written when the producer is created and removed when it drops, so a track
//! is never advertised without a publisher behind it.

use serde::Serialize;
use serde::de::DeserializeOwned;

use hang::catalog::{Compression, JsonConfig, Mode};

use crate::catalog::Rendition;
use crate::catalog::hang::CatalogExt;

/// Everything a JSON track declares about itself, beyond its mode and name.
///
/// Start from [`default`](Default::default) and chain the setters. The mode is not in here: it is
/// fixed by which producer you create.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Config {
	/// DEFLATE-compress the track's frames, advertised as
	/// [`Compression::Deflate`].
	///
	/// The catalog flag is what a consumer reads, so the track name needs no `.z` suffix.
	pub compression: bool,

	/// An optional identifier for the shape of each value, typically a JSON Schema URL.
	pub schema: Option<String>,
}

impl Config {
	/// Set [`compression`](Self::compression) (a builder, since the struct is `#[non_exhaustive]`).
	pub fn with_compression(mut self, compression: bool) -> Self {
		self.compression = compression;
		self
	}

	/// Set [`schema`](Self::schema) (a builder, since the struct is `#[non_exhaustive]`).
	pub fn with_schema(mut self, schema: impl Into<String>) -> Self {
		self.schema = Some(schema.into());
		self
	}

	/// The catalog entry describing a track published under this config in `mode`.
	pub(crate) fn entry(&self, mode: Mode) -> JsonConfig {
		let mut entry = JsonConfig::new(mode);
		entry.compression = self.compression.then_some(Compression::Deflate);
		entry.schema = self.schema.clone();
		entry
	}
}

/// Publishes a latest-value JSON track, advertised in the catalog for as long as this handle lives.
///
/// Every [`update`](Self::update) supersedes the last, so a consumer reads only the newest value.
/// For a log where every record survives, use [`Stream`].
pub struct Snapshot<T, E: CatalogExt = ()> {
	inner: moq_json::snapshot::Producer<T>,
	rendition: Rendition<E, JsonConfig>,
}

impl<T: Serialize, E: CatalogExt> Snapshot<T, E> {
	pub(crate) fn new(
		track: moq_net::track::Producer,
		mut rendition: Rendition<E, JsonConfig>,
		config: &Config,
	) -> Self {
		let inner = moq_json::snapshot::Producer::new(
			track,
			moq_json::snapshot::ProducerConfig::default().with_compression(config.compression),
		);
		rendition.set(config.entry(Mode::Snapshot));
		Self { inner, rendition }
	}

	/// The track name, which is also the catalog key.
	pub fn name(&self) -> &str {
		self.rendition.name()
	}

	/// Create a subscriber for the underlying track.
	pub fn consume(&self) -> moq_net::track::Subscriber {
		self.inner.consume()
	}

	/// Publish a new value, superseding the previous one.
	pub fn update(&mut self, value: &T) -> crate::Result<()> {
		Ok(self.inner.update(value)?)
	}

	/// Finish the track. The catalog entry is removed when this handle drops.
	pub fn finish(&mut self) -> crate::Result<()> {
		Ok(self.inner.finish()?)
	}
}

/// Publishes an append-log JSON track, advertised in the catalog for as long as this handle lives.
///
/// Every [`append`](Self::append) is preserved and delivered in order. For a latest-value document,
/// use [`Snapshot`].
pub struct Stream<T, E: CatalogExt = ()> {
	inner: moq_json::stream::Producer<T>,
	rendition: Rendition<E, JsonConfig>,
}

impl<T: Serialize, E: CatalogExt> Stream<T, E> {
	pub(crate) fn new(
		track: moq_net::track::Producer,
		mut rendition: Rendition<E, JsonConfig>,
		config: &Config,
	) -> Self {
		let inner = moq_json::stream::Producer::new(
			track,
			moq_json::stream::ProducerConfig::default().with_compression(config.compression),
		);
		rendition.set(config.entry(Mode::Stream));
		Self { inner, rendition }
	}

	/// The track name, which is also the catalog key.
	pub fn name(&self) -> &str {
		self.rendition.name()
	}

	/// Create a subscriber for the underlying track.
	pub fn consume(&self) -> moq_net::track::Subscriber {
		self.inner.consume()
	}

	/// Append one record to the log.
	pub fn append(&mut self, value: &T) -> crate::Result<()> {
		Ok(self.inner.append(value)?)
	}

	/// Finish the track. The catalog entry is removed when this handle drops.
	pub fn finish(&mut self) -> crate::Result<()> {
		Ok(self.inner.finish()?)
	}
}

/// Reads a JSON track in whichever mode its catalog entry declares.
///
/// One type rather than two because the mode is the publisher's choice, learned at runtime. Match
/// on it, or narrow with [`snapshot`](Self::snapshot) / [`stream`](Self::stream) when the
/// application only handles one.
#[non_exhaustive]
pub enum Consumer<T> {
	/// The track is a latest-value document ([`Mode::Snapshot`]).
	Snapshot(moq_json::snapshot::Consumer<T>),

	/// The track is an append log ([`Mode::Stream`]).
	Stream(moq_json::stream::Consumer<T>),
}

impl<T: DeserializeOwned> Consumer<T> {
	/// Subscribe to track `name` in `broadcast`, reading it as its catalog entry declares.
	///
	/// The entry supplies the mode and compression, so a reader can't pair the wrong ones with the
	/// track. An entry whose [`broadcast`](JsonConfig::broadcast) names a different broadcast must be
	/// resolved first (see [`Source`](crate::Source)) and then read through
	/// [`from_track`](Self::from_track).
	///
	/// Errors if the entry declares a mode or compression this build doesn't implement, which is a
	/// track a consumer must skip rather than guess at.
	pub async fn subscribe(
		broadcast: &moq_net::broadcast::Consumer,
		name: &str,
		config: &JsonConfig,
	) -> crate::Result<Self> {
		let track = broadcast.track(name)?.subscribe(None).await?;
		Self::from_track(track, config)
	}

	/// Read an already-subscribed track as its catalog entry declares.
	///
	/// The counterpart to [`subscribe`](Self::subscribe) for a caller that resolved the track
	/// itself, typically to honor a cross-broadcast [`broadcast`](JsonConfig::broadcast) reference.
	pub fn from_track(track: moq_net::track::Subscriber, config: &JsonConfig) -> crate::Result<Self> {
		let compression = crate::compression(config.compression.as_ref())?;

		Ok(match &config.mode {
			Mode::Snapshot => Self::Snapshot(moq_json::snapshot::Consumer::new(
				track,
				moq_json::snapshot::ConsumerConfig::default().with_compression(compression),
			)),
			Mode::Stream => Self::Stream(moq_json::stream::Consumer::new(
				track,
				moq_json::stream::ConsumerConfig::default().with_compression(compression),
			)),
			other => return Err(crate::Error::UnsupportedMode(other.as_str().to_string())),
		})
	}

	/// The snapshot consumer, or `None` if the track is an append log.
	pub fn snapshot(self) -> Option<moq_json::snapshot::Consumer<T>> {
		match self {
			Self::Snapshot(consumer) => Some(consumer),
			_ => None,
		}
	}

	/// The stream consumer, or `None` if the track is a latest-value document.
	pub fn stream(self) -> Option<moq_json::stream::Consumer<T>> {
		match self {
			Self::Stream(consumer) => Some(consumer),
			_ => None,
		}
	}
}

#[cfg(test)]
mod test {
	use std::task::Poll;

	use serde_json::{Value, json};

	use super::*;

	fn catalog() -> (moq_net::broadcast::Producer, crate::catalog::Producer) {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		(broadcast, catalog)
	}

	fn entry(catalog: &crate::catalog::Producer, name: &str) -> JsonConfig {
		catalog
			.snapshot()
			.json
			.tracks
			.get(name)
			.expect("missing catalog entry")
			.clone()
	}

	/// Drain every value a consumer currently has, without blocking.
	fn drain(consumer: Consumer<Value>) -> Vec<Value> {
		let waiter = kio::Waiter::noop();
		let mut out = Vec::new();
		match consumer {
			Consumer::Snapshot(mut c) => {
				while let Poll::Ready(Ok(Some(value))) = c.poll_next(&waiter) {
					out.push(value);
				}
			}
			Consumer::Stream(mut c) => {
				while let Poll::Ready(Ok(Some(value))) = c.poll_next(&waiter) {
					out.push(value);
				}
			}
		}
		out
	}

	#[test]
	fn a_stream_track_roundtrips() {
		let (_broadcast, catalog) = catalog();
		let mut chat = catalog
			.json_stream::<Value>("chat", Config::default().with_compression(true))
			.unwrap();

		let expected: Vec<Value> = (0..3).map(|n| json!({ "n": n })).collect();
		for value in &expected {
			chat.append(value).unwrap();
		}
		chat.finish().unwrap();

		// The catalog is the only thing the reader is told; mode and compression come from it.
		let consumer = Consumer::from_track(chat.consume(), &entry(&catalog, "chat")).unwrap();
		assert!(matches!(consumer, Consumer::Stream(_)));
		assert_eq!(drain(consumer), expected);
	}

	#[test]
	fn a_snapshot_track_roundtrips() {
		let (_broadcast, catalog) = catalog();
		let mut status = catalog.json_snapshot::<Value>("status", Config::default()).unwrap();

		status.update(&json!({ "live": false })).unwrap();
		status.update(&json!({ "live": true })).unwrap();
		status.finish().unwrap();

		// A late reader only sees the newest value, which is the point of snapshot mode.
		let consumer = Consumer::from_track(status.consume(), &entry(&catalog, "status")).unwrap();
		assert!(matches!(consumer, Consumer::Snapshot(_)));
		assert_eq!(drain(consumer), vec![json!({ "live": true })]);
	}

	#[test]
	fn the_entry_describes_how_to_read_the_track() {
		let (_broadcast, catalog) = catalog();
		let _chat = catalog
			.json_stream::<Value>(
				"chat",
				Config::default()
					.with_compression(true)
					.with_schema("https://example.com/chat.schema.json"),
			)
			.unwrap();

		let entry = entry(&catalog, "chat");
		assert_eq!(entry.mode, Mode::Stream);
		assert_eq!(entry.compression, Some(Compression::Deflate));
		assert_eq!(entry.schema.as_deref(), Some("https://example.com/chat.schema.json"));
		// The flag is authoritative, so the track name carries no `.z` suffix.
		assert_eq!(_chat.name(), "chat");
	}

	#[test]
	fn dropping_the_producer_retires_the_entry() {
		let (_broadcast, catalog) = catalog();
		let chat = catalog.json_stream::<Value>("chat", Config::default()).unwrap();
		assert!(catalog.snapshot().json.tracks.contains_key("chat"));

		drop(chat);
		assert!(
			!catalog.snapshot().json.tracks.contains_key("chat"),
			"a track with no publisher must not stay advertised"
		);
	}

	/// A catalog entry can exist with no local track behind it, e.g. one seeded at construction that
	/// references a sibling broadcast. Publishing over that name would replace what the entry pointed
	/// at and then retire it entirely on drop, so the name counts as taken.
	#[test]
	fn an_existing_catalog_entry_is_not_replaced() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();

		let mut existing = JsonConfig::new(Mode::Snapshot);
		existing.broadcast = Some(moq_net::PathRelativeOwned::new("source"));

		let mut seed = crate::catalog::hang::Catalog::<()>::default();
		seed.json.tracks.insert("chat".to_string(), existing.clone());
		let catalog = crate::catalog::Producer::with_catalog(&mut broadcast, seed).unwrap();

		// Nothing local holds the track name, so `create_track` alone would have let this through.
		assert!(matches!(
			catalog.json_stream::<Value>("chat", Config::default()),
			Err(crate::Error::Hang(hang::Error::Duplicate(_)))
		));
		assert_eq!(catalog.snapshot().json.tracks.get("chat"), Some(&existing));
	}

	/// A consumer that can't tell a log from a latest-value document would silently drop records,
	/// so an unreadable entry is an error rather than a guess.
	#[test]
	fn an_unrecognized_mode_or_compression_is_refused() {
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("chat", None)
			.unwrap();

		let mut config = JsonConfig::new(Mode::Unknown("windowed".to_string()));
		assert!(matches!(
			Consumer::<Value>::from_track(track.subscribe(None), &config),
			Err(crate::Error::UnsupportedMode(_))
		));

		config.mode = Mode::Stream;
		config.compression = Some(Compression::Unknown("zstd".to_string()));
		assert!(matches!(
			Consumer::<Value>::from_track(track.subscribe(None), &config),
			Err(crate::Error::UnsupportedCompression(_))
		));
	}
}
