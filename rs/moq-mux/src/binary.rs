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
//! let config = catalog.binary.tracks.get("thumbnail").expect("no thumbnail track");
//! let entry = moq_mux::catalog::Entry::new("thumbnail", config);
//! let mut thumbnail = entry.subscribe(source).await?;
//! while let Some(jpeg) = thumbnail.next().await? {
//!     // ...
//! }
//! # Ok(())
//! # }
//! ```

use std::marker::PhantomData;

use bytes::Bytes;

use hang::catalog::{BinaryConfig, Compression, Mode};

use crate::catalog::hang::CatalogExt;
use crate::catalog::{IntoRendition, Listing, RenditionConfig};

/// Everything a binary track declares about itself, beyond its mode and name.
///
/// Start from [`default`](Default::default) and chain the setters. The mode is not in here: it is
/// fixed by which producer you create. To list the track in an application's own catalog section
/// instead of `binary`, pass that section's entry (see [`IntoRendition`]).
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
}

impl<E: CatalogExt> IntoRendition<E, BinaryConfig> for Config {
	type Config = BinaryConfig;

	fn into_rendition(self) -> BinaryConfig {
		// The producer overwrites the mode with the one it publishes in.
		let mut entry = BinaryConfig::new(Mode::Snapshot);
		entry.compression = self.compression.then_some(Compression::Deflate);
		entry.mime = self.mime;
		entry
	}
}

/// Fix `config`'s mode and return whether its frames are compressed.
///
/// Errors on a compression this build can't write, rather than advertising one the frames don't use.
fn prepare(config: &mut impl AsMut<BinaryConfig>, mode: Mode) -> crate::Result<bool> {
	let binary = config.as_mut();
	binary.mode = mode;
	crate::compression(binary.compression.as_ref())
}

/// Publishes a latest-value binary track, advertised in the catalog for as long as this handle
/// lives.
///
/// Every [`update`](Self::update) supersedes the last, so a consumer reads only the newest payload.
/// For a log where every payload survives, use [`Stream`].
pub struct Snapshot<E: CatalogExt = ()> {
	inner: moq_binary::snapshot::Producer,
	listing: Listing,
	/// Which catalog the entry lives in. The entry's own type is erased by `Listing`.
	_catalog: PhantomData<fn() -> E>,
}

impl<E: CatalogExt> Snapshot<E> {
	pub(crate) fn new<C: RenditionConfig<E> + AsMut<BinaryConfig>>(
		track: moq_net::track::Producer,
		rendition: crate::catalog::Rendition<E, C>,
		mut config: C,
	) -> crate::Result<Self> {
		let mut binary = moq_binary::snapshot::Config::default();
		if prepare(&mut config, Mode::Snapshot)? {
			binary.compression = moq_binary::Compression::Deflate;
		}
		let inner = moq_binary::snapshot::Producer::new(track, binary);
		let listing = Listing::new(rendition, config)?;
		Ok(Self {
			inner,
			listing,
			_catalog: PhantomData,
		})
	}

	/// The track name, which is also the catalog key.
	pub fn name(&self) -> &str {
		self.listing.name()
	}

	/// Create a subscriber for the underlying track.
	pub fn consume(&self) -> moq_net::track::Subscriber {
		self.inner.consume()
	}

	/// Publish a new payload, superseding the previous one.
	pub fn update(&mut self, payload: impl Into<Bytes>) -> crate::Result<()> {
		let payload = payload.into();
		let len = payload.len();
		self.inner.update(payload)?;
		self.listing.record(|| len)
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
	listing: Option<Listing>,
	/// Which catalog the entry lives in. The entry's own type is erased by `Listing`.
	_catalog: PhantomData<fn() -> E>,
}

impl<E: CatalogExt> Stream<E> {
	pub(crate) fn new<C: RenditionConfig<E> + AsMut<BinaryConfig>>(
		track: moq_net::track::Producer,
		rendition: crate::catalog::Rendition<E, C>,
		mut config: C,
	) -> crate::Result<Self> {
		let mut binary = moq_binary::stream::Config::default();
		if prepare(&mut config, Mode::Stream)? {
			binary.compression = moq_binary::Compression::Deflate;
		}
		let inner = moq_binary::stream::Producer::new(track, binary);
		let listing = Listing::new(rendition, config)?;
		Ok(Self {
			inner,
			name: listing.name().to_string(),
			listing: Some(listing),
			_catalog: PhantomData,
		})
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
		let payload = payload.into();
		let len = payload.len();
		if let Err(err) = self.inner.append(payload) {
			// The inner producer has already closed the track. Dropping the listing retires the
			// catalog entry too: waiting for the handle to drop would keep advertising a track that
			// can no longer accept records, so a consumer discovering it now would subscribe to an
			// already-ended log.
			self.listing = None;
			return Err(err.into());
		}

		match &mut self.listing {
			Some(listing) => listing.record(|| len),
			None => Ok(()),
		}
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
			Mode::Snapshot => {
				let mut binary = moq_binary::snapshot::Config::default();
				if compression {
					binary.compression = moq_binary::Compression::Deflate;
				}
				Inner::Snapshot(moq_binary::snapshot::Consumer::new(track, binary))
			}
			Mode::Stream => {
				let mut binary = moq_binary::stream::Config::default();
				if compression {
					binary.compression = moq_binary::Compression::Deflate;
				}
				Inner::Stream(moq_binary::stream::Consumer::new(track, binary))
			}
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
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
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

	/// A data track listed in an application's own section, beside its own per-track fields.
	mod section {
		use std::collections::BTreeMap;

		use serde::{Deserialize, Serialize};

		use super::*;
		use crate::catalog::Estimate;
		use crate::catalog::hang::Catalog;

		#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
		struct Ext {
			#[serde(rename = "com.example.mavlink", default, skip_serializing_if = "BTreeMap::is_empty")]
			mavlink: BTreeMap<String, Mavlink>,
		}

		impl CatalogExt for Ext {}

		#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
		struct Mavlink {
			#[serde(flatten)]
			binary: BinaryConfig,
			sysid: u8,
		}

		impl AsMut<BinaryConfig> for Mavlink {
			fn as_mut(&mut self) -> &mut BinaryConfig {
				&mut self.binary
			}
		}

		impl RenditionConfig<Ext> for Mavlink {
			fn insert(self, catalog: &mut Catalog<Ext>, name: &str) {
				catalog.ext.mavlink.insert(name.to_string(), self);
			}
			fn get_mut<'a>(catalog: &'a mut Catalog<Ext>, name: &str) -> Option<&'a mut Self> {
				catalog.ext.mavlink.get_mut(name)
			}
			fn remove(catalog: &mut Catalog<Ext>, name: &str) {
				catalog.ext.mavlink.remove(name);
			}

			fn detects() -> bool {
				true
			}
			fn estimate(&self) -> Estimate {
				Estimate::default()
					.with_bitrate(self.binary.bitrate)
					.with_jitter(self.binary.jitter)
			}
			fn set_estimate(&mut self, estimate: Estimate) {
				self.binary.bitrate = estimate.bitrate;
				self.binary.jitter = estimate.jitter;
			}
		}

		fn mavlink(sysid: u8) -> Mavlink {
			let mut binary = BinaryConfig::new(Mode::Snapshot);
			binary.compression = Some(Compression::Deflate);
			Mavlink { binary, sysid }
		}

		fn catalog() -> (moq_net::broadcast::Producer, crate::catalog::Producer<Ext>) {
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let config = crate::catalog::Config::default().with_catalog(Catalog::<Ext>::default());
			let catalog = crate::catalog::Producer::new(&mut broadcast, config).unwrap();
			(broadcast, catalog)
		}

		/// The producer fixes the mode and keeps the application's fields; a `Catalog<Ext>` consumer
		/// reads the entry back and subscribes through its embedded config alone.
		#[tokio::test]
		async fn roundtrips_through_a_catalog_consumer() {
			let (mut broadcast, catalog) = catalog();
			let source = crate::source::announced(&broadcast.consume());
			let mut consumer = catalog.consume().unwrap();

			let mut telemetry = catalog
				.binary_stream(track(&mut broadcast, "telemetry"), mavlink(7))
				.unwrap();
			telemetry.append(&b"heartbeat"[..]).unwrap();

			let published = consumer.next().await.unwrap().expect("catalog published");
			let entry = published.ext.mavlink.get("telemetry").expect("missing entry");
			assert_eq!(entry.sysid, 7, "the application's fields survive");
			assert_eq!(entry.binary.mode, Mode::Stream, "the producer fixes the mode");
			assert_eq!(entry.binary.compression, Some(Compression::Deflate));
			assert!(published.binary.tracks.is_empty(), "not listed in the binary section");

			let mut reader = crate::catalog::Entry::new("telemetry", &entry.binary)
				.subscribe(&source)
				.await
				.unwrap();
			telemetry.finish().unwrap();
			assert_eq!(reader.next().await.unwrap(), Some(Bytes::from_static(b"heartbeat")));
			assert_eq!(reader.next().await.unwrap(), None);
		}

		#[test]
		fn dropping_the_producer_retires_the_entry() {
			let (mut broadcast, catalog) = catalog();
			let telemetry = catalog
				.binary_snapshot(track(&mut broadcast, "telemetry"), mavlink(1))
				.unwrap();
			assert!(catalog.snapshot().ext.mavlink.contains_key("telemetry"));

			drop(telemetry);
			assert!(!catalog.snapshot().ext.mavlink.contains_key("telemetry"));
		}

		/// The name is owned per section, so an entry already in the application's section refuses
		/// a second producer without touching the first.
		#[test]
		fn a_duplicate_name_is_refused() {
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let mut seed = Catalog::<Ext>::default();
			seed.ext.mavlink.insert("telemetry".to_string(), mavlink(1));
			let config = crate::catalog::Config::default().with_catalog(seed);
			let catalog = crate::catalog::Producer::new(&mut broadcast, config).unwrap();

			assert!(matches!(
				catalog.binary_stream(track(&mut broadcast, "telemetry"), mavlink(2)),
				Err(crate::Error::Hang(hang::Error::Duplicate(_)))
			));
			assert_eq!(catalog.snapshot().ext.mavlink["telemetry"].sysid, 1);
		}

		/// A compression this build can't write is refused rather than advertised over plain frames.
		#[test]
		fn an_unknown_compression_is_refused() {
			let (mut broadcast, catalog) = catalog();
			let mut entry = mavlink(1);
			entry.binary.compression = Some(Compression::Unknown("zstd".to_string()));

			assert!(matches!(
				catalog.binary_stream(track(&mut broadcast, "telemetry"), entry),
				Err(crate::Error::UnsupportedCompression(_))
			));
			assert!(catalog.snapshot().ext.mavlink.is_empty());
		}

		/// Writes fill an absent bitrate, through the entry's embedded config.
		#[test]
		fn detects_bitrate() {
			let (mut broadcast, catalog) = catalog();
			let mut telemetry = catalog
				.binary_stream(track(&mut broadcast, "telemetry"), mavlink(1))
				.unwrap();

			// 40ms payloads of 5 kB: 1 Mbps, over more than the bitrate window.
			for i in 0..60u64 {
				let now = moq_net::Timestamp::from_micros(i * 40_000).unwrap();
				telemetry.listing.as_mut().unwrap().record_at(now, 5_000).unwrap();
			}

			let entry = &catalog.snapshot().ext.mavlink["telemetry"];
			assert_eq!(entry.binary.bitrate, Some(1_000_000));
			assert_eq!(entry.binary.jitter, None, "write spacing is not a flush delay");
		}
	}
}
