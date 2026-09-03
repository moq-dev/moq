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
//! be written. A consumer only learns the mode from the catalog, so there is one [`Consumer`] that
//! reads either; it exposes [`mode`](Consumer::mode) for a reader that needs to know.
//!
//! Mint a producer from the catalog:
//!
//! ```no_run
//! # fn example(
//! #     broadcast: &mut moq_net::broadcast::Producer,
//! #     catalog: &moq_mux::catalog::Producer,
//! #     jpeg: bytes::Bytes,
//! # ) -> moq_mux::Result<()> {
//! let track = broadcast.create_track("thumbnail", None)?;
//! let config = moq_mux::binary::Config::default().with_mime("image/jpeg");
//! let mut thumbnail = catalog.binary_snapshot(track, config)?;
//! thumbnail.update(jpeg)?;
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
//! # async fn example(
//! #     source: &moq_mux::Source,
//! #     catalog: &moq_mux::catalog::hang::Catalog,
//! # ) -> moq_mux::Result<()> {
//! let entry = catalog.binary_track("thumbnail").expect("no thumbnail track");
//! let mut thumbnail = entry.subscribe(source).await?;
//! while let Some(jpeg) = thumbnail.next().await? {
//!     // ...
//! }
//! # Ok(())
//! # }
//! ```

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

	/// Finish the track and retire its catalog entry.
	///
	/// Consumes the handle, so a write after finishing cannot be expressed. Leaving the entry
	/// advertising a closed track would only mislead a consumer that subscribed afterwards, so
	/// dropping the handle without finishing retires the entry too.
	pub fn finish(mut self) -> crate::Result<()> {
		Ok(self.inner.finish()?)
	}
}

/// Publishes an append-log binary track, advertised in the catalog for as long as this handle lives.
///
/// Every [`append`](Self::append) is preserved and delivered in order. For a latest-value payload,
/// use [`Snapshot`].
pub struct Stream<E: CatalogExt = ()> {
	inner: moq_binary::stream::Producer,
	name: String,

	/// Cleared when a terminal failure ends the track, which retires the catalog entry with it. An
	/// entry advertising a track that can no longer accept records only misleads a consumer that
	/// discovers it afterwards.
	rendition: Option<Rendition<E, BinaryConfig>>,
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

	/// Append one payload to the log.
	///
	/// A payload that cannot be written ends the track (see
	/// [`moq_binary::stream::Producer::append`]) and retires the catalog entry with it.
	pub fn append(&mut self, payload: impl Into<Bytes>) -> crate::Result<()> {
		let Err(err) = self.inner.append(payload) else {
			return Ok(());
		};

		// The inner producer has already closed the track. Dropping the rendition retires the catalog
		// entry too: waiting for the handle to drop would keep advertising a track that can no longer
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

/// Reads a binary track, in whichever mode its catalog entry declares.
///
/// One type rather than one per mode: both modes hand the caller the same thing, a sequence of
/// payloads ending when the track does, so a reader writes one loop either way. What differs is
/// loss semantics, and that is a property to ask about ([`mode`](Self::mode)) rather than a fork
/// every caller pays for. A reader that genuinely requires losslessness checks the mode and bails.
pub struct Consumer {
	inner: Inner,
	mode: Mode,
}

/// Which moq-binary consumer is doing the reading. Private: the caller sees one `Consumer`.
enum Inner {
	Snapshot(moq_binary::snapshot::Consumer),
	Stream(moq_binary::stream::Consumer),
}

impl Consumer {
	/// Read an already-subscribed track as its catalog entry declares.
	///
	/// The escape hatch for a caller that resolved the subscription itself. Prefer
	/// [`Entry::subscribe`](crate::catalog::Entry::subscribe), which resolves the track (including a
	/// cross-broadcast [`broadcast`](BinaryConfig::broadcast) reference) from the catalog for you.
	///
	/// Errors if the entry declares a mode or compression this build doesn't implement, which is a
	/// track a consumer must skip rather than guess at.
	pub fn from_track(track: moq_net::track::Subscriber, config: &BinaryConfig) -> crate::Result<Self> {
		let compression = crate::compression(config.compression.as_ref())?;

		let inner = match &config.mode {
			Mode::Snapshot => Inner::Snapshot(moq_binary::snapshot::Consumer::new(
				track,
				moq_binary::snapshot::ConsumerConfig::default().with_compression(compression),
			)),
			Mode::Stream => Inner::Stream(moq_binary::stream::Consumer::new(
				track,
				moq_binary::stream::ConsumerConfig::default().with_compression(compression),
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
	/// [`Mode::Snapshot`] is lossy: intermediate payloads are superseded and only the newest is
	/// yielded. [`Mode::Stream`] preserves every payload. Check this when the application can only
	/// work with one of them.
	pub fn mode(&self) -> &Mode {
		&self.mode
	}

	/// Get the next payload, or `None` once the track ends.
	pub async fn next(&mut self) -> crate::Result<Option<Bytes>> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Poll for the next payload, without blocking.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> std::task::Poll<crate::Result<Option<Bytes>>> {
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

impl crate::catalog::Entry<'_, BinaryConfig> {
	/// Subscribe to this track and read it as the entry declares.
	///
	/// Resolves the track through `source`, so an entry whose [`broadcast`](BinaryConfig::broadcast)
	/// field names a sibling broadcast is followed rather than silently read from the catalog's own.
	///
	/// Errors if the entry declares a mode or compression this build doesn't implement.
	pub async fn subscribe(&self, source: &crate::Source) -> crate::Result<Consumer> {
		let track = source
			.subscribe_track(self.config().broadcast.as_ref(), self.name())
			.await?;
		Consumer::from_track(track, self.config())
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

	/// Create the moq-net track a data producer publishes on, as a caller does for a media track.
	fn track(broadcast: &mut moq_net::broadcast::Producer, name: &str) -> moq_net::track::Producer {
		broadcast.create_track(name, None).unwrap()
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
	///
	/// One loop whichever mode the publisher chose, which is the point of the single `Consumer`.
	fn drain(mut consumer: Consumer) -> Vec<Bytes> {
		let waiter = kio::Waiter::noop();
		let mut out = Vec::new();
		while let Poll::Ready(Ok(Some(payload))) = consumer.poll_next(&waiter) {
			out.push(payload);
		}
		out
	}

	#[test]
	fn a_stream_track_roundtrips() {
		let (mut broadcast, catalog) = catalog();
		let mut samples = catalog
			.binary_stream(
				track(&mut broadcast, "samples"),
				Config::default().with_compression(true),
			)
			.unwrap();

		let track = samples.consume();
		let expected: Vec<Bytes> = (0..3u8).map(|n| Bytes::from(vec![n; 8])).collect();
		for payload in &expected {
			samples.append(payload.clone()).unwrap();
		}
		// `finish` retires the entry, so read both off the live track first.
		let entry = entry(&catalog, "samples");
		samples.finish().unwrap();

		let consumer = Consumer::from_track(track, &entry).unwrap();
		assert_eq!(consumer.mode(), &Mode::Stream);
		assert_eq!(drain(consumer), expected);
	}

	#[test]
	fn a_snapshot_track_roundtrips() {
		let (mut broadcast, catalog) = catalog();
		let mut thumbnail = catalog
			.binary_snapshot(
				track(&mut broadcast, "thumbnail"),
				Config::default().with_mime("image/jpeg"),
			)
			.unwrap();

		let track = thumbnail.consume();
		thumbnail.update(&b"old"[..]).unwrap();
		thumbnail.update(&b"new"[..]).unwrap();
		let entry = entry(&catalog, "thumbnail");
		thumbnail.finish().unwrap();
		assert_eq!(entry.mode, Mode::Snapshot);
		assert_eq!(entry.compression, None);
		assert_eq!(entry.mime.as_deref(), Some("image/jpeg"));

		let consumer = Consumer::from_track(track, &entry).unwrap();
		assert_eq!(drain(consumer), vec![Bytes::from_static(b"new")]);
	}

	#[test]
	fn dropping_the_producer_retires_the_entry() {
		let (mut broadcast, catalog) = catalog();
		let thumbnail = catalog
			.binary_snapshot(track(&mut broadcast, "thumbnail"), Config::default())
			.unwrap();
		assert!(catalog.snapshot().binary.tracks.contains_key("thumbnail"));

		drop(thumbnail);
		assert!(
			!catalog.snapshot().binary.tracks.contains_key("thumbnail"),
			"a track with no publisher must not stay advertised"
		);
	}

	/// The two catalog sections are separate namespaces, so a JSON and a binary track may share a
	/// name there. The broadcast's track names are one namespace, though, so the caller's
	/// `create_track` is what refuses the second publisher.
	#[test]
	fn a_name_taken_by_another_track_is_refused() {
		let (mut broadcast, catalog) = catalog();
		let _first = catalog
			.binary_snapshot(track(&mut broadcast, "data"), Config::default())
			.unwrap();

		assert!(broadcast.create_track("data", None).is_err());
	}
}
