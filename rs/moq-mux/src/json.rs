//! JSON track publish/subscribe.
//!
//! A JSON track is application data a broadcast publishes alongside its media: a chat log, a
//! telemetry feed, a status document. The catalog entry
//! ([`hang::catalog::JsonConfig`]) says how to read it, so a consumer needs the track name and
//! nothing else about the application.
//!
//! The write and read sides are shaped differently on purpose. A publisher knows its mode at
//! compile time, so [`Snapshot`] and [`Stream`] are distinct types and `append`-on-a-snapshot can't
//! be written. A consumer only learns the mode from the catalog, so there is one [`Consumer`] that
//! reads either; it exposes [`mode`](Consumer::mode) for a reader that needs to know.
//!
//! Mint a producer from the catalog:
//!
//! ```no_run
//! # use serde::{Deserialize, Serialize};
//! # #[derive(Serialize, Deserialize)]
//! # struct Message { text: String }
//! # fn example(
//! #     broadcast: &mut moq_net::broadcast::Producer,
//! #     catalog: &moq_mux::catalog::Producer,
//! # ) -> moq_mux::Result<()> {
//! let track = broadcast.create_track("chat", None)?;
//! let config = moq_mux::json::Config::default().with_compression(true);
//! let mut chat = catalog.json_stream::<Message>(track, config)?;
//! chat.append(&Message { text: "hello".to_string() })?;
//! # Ok(())
//! # }
//! ```
//!
//! The catalog entry is written when the producer is created and removed when it drops, so a track
//! is never advertised without a publisher behind it.
//!
//! Read one back off the catalog, naming it once:
//!
//! ```no_run
//! # use serde::{Deserialize, Serialize};
//! # #[derive(Serialize, Deserialize)]
//! # struct Message { text: String }
//! # async fn example(
//! #     source: &moq_mux::Source,
//! #     catalog: &moq_mux::catalog::hang::Catalog,
//! # ) -> moq_mux::Result<()> {
//! let entry = catalog.json_track("chat").expect("no chat track");
//! let mut chat = entry.subscribe::<Message>(source).await?;
//! while let Some(message) = chat.next().await? {
//!     // ...
//! }
//! # Ok(())
//! # }
//! ```

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

	/// Finish the track and retire its catalog entry.
	///
	/// Consumes the handle, so a write after finishing cannot be expressed. Leaving the entry
	/// advertising a closed track would only mislead a consumer that subscribed afterwards, so
	/// dropping the handle without finishing retires the entry too.
	pub fn finish(mut self) -> crate::Result<()> {
		Ok(self.inner.finish()?)
	}
}

/// Publishes an append-log JSON track, advertised in the catalog for as long as this handle lives.
///
/// Every [`append`](Self::append) is preserved and delivered in order. For a latest-value document,
/// use [`Snapshot`].
pub struct Stream<T, E: CatalogExt = ()> {
	inner: moq_json::stream::Producer<T>,
	name: String,

	/// Cleared when a terminal failure ends the track, which retires the catalog entry with it. An
	/// entry advertising a track that can no longer accept records only misleads a consumer that
	/// discovers it afterwards.
	rendition: Option<Rendition<E, JsonConfig>>,
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
		Self {
			inner,
			name: rendition.name().to_string(),
			rendition: Some(rendition),
		}
	}

	/// The track name, which is also the catalog key.
	pub fn name(&self) -> &str {
		&self.name
	}

	/// Create a subscriber for the underlying track.
	///
	/// Still hands one back once a failed write has ended the log: the subscriber surfaces the abort
	/// on its first read.
	pub fn consume(&self) -> moq_net::track::Subscriber {
		self.inner.consume()
	}

	/// Append one record to the log.
	///
	/// Any failure ends the track (see [`moq_json::stream::Producer::append`]) and retires the
	/// catalog entry with it.
	pub fn append(&mut self, value: &T) -> crate::Result<()> {
		let Err(err) = self.inner.append(value) else {
			return Ok(());
		};

		// The inner producer has already ended the track. Dropping the rendition retires the catalog
		// entry: waiting for the handle to drop would keep advertising a track that can no longer
		// accept records, so a consumer discovering it now would subscribe to an already-ended log.
		self.rendition = None;

		Err(err.into())
	}

	/// Finish the track and retire its catalog entry.
	///
	/// Consumes the handle, so a write after finishing cannot be expressed. Leaving the entry
	/// advertising a closed track would only mislead a consumer that subscribed afterwards, so
	/// dropping the handle without finishing retires the entry too.
	pub fn finish(mut self) -> crate::Result<()> {
		Ok(self.inner.finish()?)
	}
}

/// Reads a JSON track, in whichever mode its catalog entry declares.
///
/// One type rather than one per mode: both modes hand the caller the same thing, a sequence of `T`
/// ending when the track does, so a reader writes one loop either way. What differs is loss
/// semantics, and that is a property to ask about ([`mode`](Self::mode)) rather than a fork every
/// caller pays for. A reader that genuinely requires losslessness checks the mode and bails.
pub struct Consumer<T> {
	inner: Inner<T>,
	mode: Mode,
}

/// Which moq-json consumer is doing the reading. Private: the caller sees one `Consumer`.
enum Inner<T> {
	Snapshot(moq_json::snapshot::Consumer<T>),
	Stream(moq_json::stream::Consumer<T>),
}

impl<T: DeserializeOwned> Consumer<T> {
	/// Read an already-subscribed track as its catalog entry declares.
	///
	/// The escape hatch for a caller that resolved the subscription itself. Prefer
	/// [`Entry::subscribe`](crate::catalog::Entry::subscribe), which resolves the track (including a
	/// cross-broadcast [`broadcast`](JsonConfig::broadcast) reference) from the catalog for you.
	///
	/// Errors if the entry declares a mode or compression this build doesn't implement, which is a
	/// track a consumer must skip rather than guess at.
	pub fn from_track(track: moq_net::track::Subscriber, config: &JsonConfig) -> crate::Result<Self> {
		let compression = crate::compression(config.compression.as_ref())?;

		let inner = match &config.mode {
			Mode::Snapshot => Inner::Snapshot(moq_json::snapshot::Consumer::new(
				track,
				moq_json::snapshot::ConsumerConfig::default().with_compression(compression),
			)),
			Mode::Stream => Inner::Stream(moq_json::stream::Consumer::new(
				track,
				moq_json::stream::ConsumerConfig::default().with_compression(compression),
			)),
			other => return Err(crate::Error::UnsupportedMode(other.to_string())),
		};

		Ok(Self {
			inner,
			mode: config.mode.clone(),
		})
	}

	/// The mode the publisher chose.
	///
	/// [`Mode::Snapshot`] is lossy: intermediate values are superseded and only the newest is
	/// yielded. [`Mode::Stream`] preserves every record. Check this when the application can only
	/// work with one of them.
	pub fn mode(&self) -> &Mode {
		&self.mode
	}

	/// Get the next value, or `None` once the track ends.
	pub async fn next(&mut self) -> crate::Result<Option<T>> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Poll for the next value, without blocking.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> std::task::Poll<crate::Result<Option<T>>> {
		let result = match &mut self.inner {
			Inner::Snapshot(consumer) => consumer.poll_next(waiter),
			Inner::Stream(consumer) => consumer.poll_next(waiter),
		};

		match result {
			std::task::Poll::Ready(value) => std::task::Poll::Ready(value.map_err(Into::into)),
			std::task::Poll::Pending => std::task::Poll::Pending,
		}
	}
}

impl crate::catalog::Entry<'_, JsonConfig> {
	/// Subscribe to this track and read it as the entry declares.
	///
	/// Resolves the track through `source`, so an entry whose [`broadcast`](JsonConfig::broadcast)
	/// field names a sibling broadcast is followed rather than silently read from the catalog's own.
	///
	/// Errors if the entry declares a mode or compression this build doesn't implement.
	pub async fn subscribe<T: DeserializeOwned>(&self, source: &crate::Source) -> crate::Result<Consumer<T>> {
		let track = source
			.subscribe_track(self.config().broadcast.as_ref(), self.name())
			.await?;
		Consumer::from_track(track, self.config())
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

	/// Create the moq-net track a data producer publishes on, as a caller does for a media track.
	fn track(broadcast: &mut moq_net::broadcast::Producer, name: &str) -> moq_net::track::Producer {
		broadcast.create_track(name, None).unwrap()
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
	///
	/// One loop whichever mode the publisher chose, which is the point of the single `Consumer`.
	fn drain(mut consumer: Consumer<Value>) -> Vec<Value> {
		let waiter = kio::Waiter::noop();
		let mut out = Vec::new();
		while let Poll::Ready(Ok(Some(value))) = consumer.poll_next(&waiter) {
			out.push(value);
		}
		out
	}

	#[test]
	fn a_stream_track_roundtrips() {
		let (mut broadcast, catalog) = catalog();
		let mut chat = catalog
			.json_stream::<Value>(track(&mut broadcast, "chat"), Config::default().with_compression(true))
			.unwrap();

		let track = chat.consume();
		let expected: Vec<Value> = (0..3).map(|n| json!({ "n": n })).collect();
		for value in &expected {
			chat.append(value).unwrap();
		}
		// `finish` retires the entry, so read both off the live track first.
		let entry = entry(&catalog, "chat");
		chat.finish().unwrap();

		// The catalog is the only thing the reader is told; mode and compression come from it.
		let consumer = Consumer::from_track(track, &entry).unwrap();
		assert_eq!(consumer.mode(), &Mode::Stream);
		assert_eq!(drain(consumer), expected);
	}

	#[test]
	fn a_snapshot_track_roundtrips() {
		let (mut broadcast, catalog) = catalog();
		let mut status = catalog
			.json_snapshot::<Value>(track(&mut broadcast, "status"), Config::default())
			.unwrap();

		let track = status.consume();
		status.update(&json!({ "live": false })).unwrap();
		status.update(&json!({ "live": true })).unwrap();
		let entry = entry(&catalog, "status");
		status.finish().unwrap();

		// A late reader only sees the newest value, which is the point of snapshot mode.
		let consumer = Consumer::from_track(track, &entry).unwrap();
		assert_eq!(consumer.mode(), &Mode::Snapshot);
		assert_eq!(drain(consumer), vec![json!({ "live": true })]);
	}

	#[test]
	fn the_entry_describes_how_to_read_the_track() {
		let (mut broadcast, catalog) = catalog();
		let _chat = catalog
			.json_stream::<Value>(
				track(&mut broadcast, "chat"),
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
		let (mut broadcast, catalog) = catalog();
		let chat = catalog
			.json_stream::<Value>(track(&mut broadcast, "chat"), Config::default())
			.unwrap();
		assert!(catalog.snapshot().json.tracks.contains_key("chat"));

		drop(chat);
		assert!(
			!catalog.snapshot().json.tracks.contains_key("chat"),
			"a track with no publisher must not stay advertised"
		);
	}

	/// `finish` consumes the handle, so the entry goes with it: an entry advertising a closed track
	/// only misleads a consumer that subscribes afterwards.
	#[test]
	fn finishing_retires_the_entry_too() {
		let (mut broadcast, catalog) = catalog();
		let chat = catalog
			.json_stream::<Value>(track(&mut broadcast, "chat"), Config::default())
			.unwrap();
		assert!(catalog.snapshot().json.tracks.contains_key("chat"));

		chat.finish().unwrap();
		assert!(!catalog.snapshot().json.tracks.contains_key("chat"));
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
			catalog.json_stream::<Value>(track(&mut broadcast, "chat"), Config::default()),
			Err(crate::Error::Hang(hang::Error::Duplicate(_)))
		));
		assert_eq!(catalog.snapshot().json.tracks.get("chat"), Some(&existing));
	}

	/// The catalog is the only thing that announces a data track, so walking it is the discovery
	/// path. Each entry carries its own name, so nothing has to be threaded alongside it.
	#[test]
	fn the_catalog_enumerates_its_tracks() {
		let (mut broadcast, catalog) = catalog();
		let _chat = catalog
			.json_stream::<Value>(track(&mut broadcast, "chat"), Config::default())
			.unwrap();
		let _status = catalog
			.json_snapshot::<Value>(track(&mut broadcast, "status"), Config::default())
			.unwrap();

		let snapshot = catalog.snapshot();
		let found: Vec<(&str, &Mode)> = snapshot.json_tracks().map(|t| (t.name(), &t.config().mode)).collect();
		assert_eq!(found, vec![("chat", &Mode::Stream), ("status", &Mode::Snapshot)]);

		// A name the catalog doesn't list has no entry, rather than a config to misread.
		assert!(snapshot.json_track("nope").is_none());

		// The entry derefs to its config, so the fields read directly.
		let chat = snapshot.json_track("chat").expect("missing entry");
		assert_eq!(chat.name(), "chat");
		assert_eq!(chat.mode, Mode::Stream);
	}

	/// The whole read path: name the track once, and the entry resolves the subscription (through
	/// `Source`, so a cross-broadcast reference is followed) and says how to decode it.
	#[tokio::test]
	async fn an_entry_subscribes_and_reads() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let source = crate::source::announced(&broadcast.consume());

		let mut chat = catalog
			.json_stream::<Value>(track(&mut broadcast, "chat"), Config::default())
			.unwrap();
		chat.append(&json!({ "text": "hello" })).unwrap();

		let snapshot = catalog.snapshot();
		let entry = snapshot.json_track("chat").expect("missing entry");
		let mut consumer = entry.subscribe::<Value>(&source).await.unwrap();
		chat.finish().unwrap();

		assert_eq!(consumer.next().await.unwrap(), Some(json!({ "text": "hello" })));
		assert_eq!(consumer.next().await.unwrap(), None);
	}

	/// A value that fails to serialize never reaches the track, but the log is missing it all the
	/// same, so it is as terminal as a rejected write. The entry and the track have to agree: retiring
	/// the entry while leaving the track writable would let a later valid record land somewhere no
	/// consumer could discover.
	#[test]
	fn an_unserializable_record_ends_the_track_and_the_entry() {
		struct Record(bool);

		impl Serialize for Record {
			fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
				match self.0 {
					true => Err(serde::ser::Error::custom("cannot serialize")),
					false => serializer.serialize_u8(0),
				}
			}
		}

		let (mut broadcast, catalog) = catalog();
		let mut chat = catalog
			.json_stream::<Record>(track(&mut broadcast, "chat"), Config::default())
			.unwrap();
		let mut track = chat.consume();

		assert!(chat.append(&Record(true)).is_err());
		assert!(!catalog.snapshot().json.tracks.contains_key("chat"));

		assert!(
			chat.append(&Record(false)).is_err(),
			"a record after the failure would land on a track the retired entry no longer advertises"
		);

		let waiter = kio::Waiter::noop();
		assert!(matches!(track.poll_recv_group(&waiter), Poll::Ready(Err(_))));
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
