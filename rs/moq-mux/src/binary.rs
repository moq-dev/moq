//! Binary track publish/subscribe.
//!
//! A binary track is application data a broadcast publishes as opaque payloads: a thumbnail, a
//! serialized state blob, a sequence of samples in some application format. The catalog entry
//! ([`hang::catalog::BinaryConfig`]) says how to read it, so a consumer needs the track name and
//! nothing else about the application. Reach for [`json`](crate::json) instead when the payloads
//! are JSON, so a generic consumer can read them.
//!
//! The write and read sides are shaped differently on purpose. A publisher knows its mode at
//! compile time, so [`Snapshot`] and [`Stream`] are distinct types and `append`-on-a-snapshot can't
//! be written. A consumer only learns the mode from the catalog, so [`Consumer`] is one enum it
//! matches on.
//!
//! Mint a producer from the catalog:
//!
//! ```no_run
//! # fn example(catalog: &moq_mux::catalog::Producer, jpeg: bytes::Bytes) -> moq_mux::Result<()> {
//! let config = moq_mux::binary::Config::default().with_mime("image/jpeg");
//! let mut thumbnail = catalog.binary_snapshot("thumbnail", config)?;
//! thumbnail.update(jpeg)?;
//! # Ok(())
//! # }
//! ```
//!
//! The catalog entry is written when the producer is created and removed when it drops, so a track
//! is never advertised without a publisher behind it.

use bytes::Bytes;

use hang::catalog::{BinaryConfig, Compression, Mode};

use crate::catalog::Rendition;
use crate::catalog::hang::CatalogExt;

/// Everything a binary track declares about itself, beyond its mode and name.
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

	/// An optional media type for each payload (e.g. `image/jpeg`).
	pub mime: Option<String>,
}

impl Config {
	/// Set [`compression`](Self::compression) (a builder, since the struct is `#[non_exhaustive]`).
	pub fn with_compression(mut self, compression: bool) -> Self {
		self.compression = compression;
		self
	}

	/// Set [`mime`](Self::mime) (a builder, since the struct is `#[non_exhaustive]`).
	pub fn with_mime(mut self, mime: impl Into<String>) -> Self {
		self.mime = Some(mime.into());
		self
	}

	/// The catalog entry describing a track published under this config in `mode`.
	pub(crate) fn entry(&self, mode: Mode) -> BinaryConfig {
		let mut entry = BinaryConfig::new(mode);
		entry.compression = self.compression.then_some(Compression::Deflate);
		entry.mime = self.mime.clone();
		entry
	}
}

/// Publishes a latest-value binary track, advertised in the catalog for as long as this handle
/// lives.
///
/// Every [`update`](Self::update) supersedes the last, so a consumer reads only the newest payload.
/// For a log where every payload survives, use [`Stream`].
pub struct Snapshot<E: CatalogExt = ()> {
	inner: moq_binary::snapshot::Producer,
	rendition: Rendition<E, BinaryConfig>,
}

impl<E: CatalogExt> Snapshot<E> {
	pub(crate) fn new(
		track: moq_net::track::Producer,
		mut rendition: Rendition<E, BinaryConfig>,
		config: &Config,
	) -> Self {
		let inner = moq_binary::snapshot::Producer::new(
			track,
			moq_binary::snapshot::ProducerConfig::default().with_compression(config.compression),
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

	/// Publish a new payload, superseding the previous one.
	pub fn update(&mut self, payload: impl Into<Bytes>) -> crate::Result<()> {
		Ok(self.inner.update(payload)?)
	}

	/// Finish the track. The catalog entry is removed when this handle drops.
	pub fn finish(&mut self) -> crate::Result<()> {
		Ok(self.inner.finish()?)
	}
}

/// Publishes an append-log binary track, advertised in the catalog for as long as this handle lives.
///
/// Every [`append`](Self::append) is preserved and delivered in order. For a latest-value payload,
/// use [`Snapshot`].
pub struct Stream<E: CatalogExt = ()> {
	inner: moq_binary::stream::Producer,
	rendition: Rendition<E, BinaryConfig>,
}

impl<E: CatalogExt> Stream<E> {
	pub(crate) fn new(
		track: moq_net::track::Producer,
		mut rendition: Rendition<E, BinaryConfig>,
		config: &Config,
	) -> Self {
		let inner = moq_binary::stream::Producer::new(
			track,
			moq_binary::stream::ProducerConfig::default().with_compression(config.compression),
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

	/// Append one payload to the log.
	pub fn append(&mut self, payload: impl Into<Bytes>) -> crate::Result<()> {
		Ok(self.inner.append(payload)?)
	}

	/// Finish the track. The catalog entry is removed when this handle drops.
	pub fn finish(&mut self) -> crate::Result<()> {
		Ok(self.inner.finish()?)
	}
}

/// Reads a binary track in whichever mode its catalog entry declares.
///
/// One type rather than two because the mode is the publisher's choice, learned at runtime. Match
/// on it, or narrow with [`snapshot`](Self::snapshot) / [`stream`](Self::stream) when the
/// application only handles one.
#[non_exhaustive]
pub enum Consumer {
	/// The track is a latest-value payload ([`Mode::Snapshot`]).
	Snapshot(moq_binary::snapshot::Consumer),

	/// The track is an append log ([`Mode::Stream`]).
	Stream(moq_binary::stream::Consumer),
}

impl Consumer {
	/// Subscribe to track `name` in `broadcast`, reading it as its catalog entry declares.
	///
	/// The entry supplies the mode and compression, so a reader can't pair the wrong ones with the
	/// track. An entry whose [`broadcast`](BinaryConfig::broadcast) names a different broadcast must
	/// be resolved first (see [`Source`](crate::Source)) and then read through
	/// [`from_track`](Self::from_track).
	///
	/// Errors if the entry declares a mode or compression this build doesn't implement, which is a
	/// track a consumer must skip rather than guess at.
	pub async fn subscribe(
		broadcast: &moq_net::broadcast::Consumer,
		name: &str,
		config: &BinaryConfig,
	) -> crate::Result<Self> {
		let track = broadcast.track(name)?.subscribe(None).await?;
		Self::from_track(track, config)
	}

	/// Read an already-subscribed track as its catalog entry declares.
	///
	/// The counterpart to [`subscribe`](Self::subscribe) for a caller that resolved the track
	/// itself, typically to honor a cross-broadcast [`broadcast`](BinaryConfig::broadcast) reference.
	pub fn from_track(track: moq_net::track::Subscriber, config: &BinaryConfig) -> crate::Result<Self> {
		let compression = crate::compression(config.compression.as_ref())?;

		Ok(match &config.mode {
			Mode::Snapshot => Self::Snapshot(moq_binary::snapshot::Consumer::new(
				track,
				moq_binary::snapshot::ConsumerConfig::default().with_compression(compression),
			)),
			Mode::Stream => Self::Stream(moq_binary::stream::Consumer::new(
				track,
				moq_binary::stream::ConsumerConfig::default().with_compression(compression),
			)),
			other => return Err(crate::Error::UnsupportedMode(other.as_str().to_string())),
		})
	}

	/// The snapshot consumer, or `None` if the track is an append log.
	pub fn snapshot(self) -> Option<moq_binary::snapshot::Consumer> {
		match self {
			Self::Snapshot(consumer) => Some(consumer),
			_ => None,
		}
	}

	/// The stream consumer, or `None` if the track is a latest-value payload.
	pub fn stream(self) -> Option<moq_binary::stream::Consumer> {
		match self {
			Self::Stream(consumer) => Some(consumer),
			_ => None,
		}
	}
}

#[cfg(test)]
mod test {
	use std::task::Poll;

	use super::*;

	fn catalog() -> (moq_net::broadcast::Producer, crate::catalog::Producer) {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		(broadcast, catalog)
	}

	fn entry(catalog: &crate::catalog::Producer, name: &str) -> BinaryConfig {
		catalog
			.snapshot()
			.binary
			.tracks
			.get(name)
			.expect("missing catalog entry")
			.clone()
	}

	/// Drain every payload a consumer currently has, without blocking.
	fn drain(consumer: Consumer) -> Vec<Bytes> {
		let waiter = kio::Waiter::noop();
		let mut out = Vec::new();
		match consumer {
			Consumer::Snapshot(mut c) => {
				while let Poll::Ready(Ok(Some(payload))) = c.poll_next(&waiter) {
					out.push(payload);
				}
			}
			Consumer::Stream(mut c) => {
				while let Poll::Ready(Ok(Some(payload))) = c.poll_next(&waiter) {
					out.push(payload);
				}
			}
		}
		out
	}

	#[test]
	fn a_stream_track_roundtrips() {
		let (_broadcast, catalog) = catalog();
		let mut samples = catalog
			.binary_stream("samples", Config::default().with_compression(true))
			.unwrap();

		let expected: Vec<Bytes> = (0..3u8).map(|n| Bytes::from(vec![n; 8])).collect();
		for payload in &expected {
			samples.append(payload.clone()).unwrap();
		}
		samples.finish().unwrap();

		let consumer = Consumer::from_track(samples.consume(), &entry(&catalog, "samples")).unwrap();
		assert!(matches!(consumer, Consumer::Stream(_)));
		assert_eq!(drain(consumer), expected);
	}

	#[test]
	fn a_snapshot_track_roundtrips() {
		let (_broadcast, catalog) = catalog();
		let mut thumbnail = catalog
			.binary_snapshot("thumbnail", Config::default().with_mime("image/jpeg"))
			.unwrap();

		thumbnail.update(&b"old"[..]).unwrap();
		thumbnail.update(&b"new"[..]).unwrap();
		thumbnail.finish().unwrap();

		let entry = entry(&catalog, "thumbnail");
		assert_eq!(entry.mode, Mode::Snapshot);
		assert_eq!(entry.compression, None);
		assert_eq!(entry.mime.as_deref(), Some("image/jpeg"));

		let consumer = Consumer::from_track(thumbnail.consume(), &entry).unwrap();
		assert_eq!(drain(consumer), vec![Bytes::from_static(b"new")]);
	}

	#[test]
	fn dropping_the_producer_retires_the_entry() {
		let (_broadcast, catalog) = catalog();
		let thumbnail = catalog.binary_snapshot("thumbnail", Config::default()).unwrap();
		assert!(catalog.snapshot().binary.tracks.contains_key("thumbnail"));

		drop(thumbnail);
		assert!(
			!catalog.snapshot().binary.tracks.contains_key("thumbnail"),
			"a track with no publisher must not stay advertised"
		);
	}

	/// The two sections are separate namespaces, so a JSON and a binary track may share a name in
	/// the catalog. They cannot share the moq-net track it names, though.
	#[test]
	fn a_name_taken_by_another_track_is_refused() {
		let (_broadcast, catalog) = catalog();
		let _first = catalog.binary_snapshot("data", Config::default()).unwrap();
		assert!(catalog.binary_stream("data", Config::default()).is_err());
	}
}
