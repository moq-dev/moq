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
//! // JSON is small and repetitive, so compress it on the wire.
//! let info = moq_net::track::Info::default().with_compress(true);
//! let track = broadcast.create_track("chat", info)?;
//! let mut chat = catalog.json_stream::<Message>(track, moq_mux::json::Config::default())?;
//! chat.append(&Message { text: "hello".to_string() })?;
//! # Ok(())
//! # }
//! ```
//!
//! The catalog entry is written when the producer is created and removed when it drops, so a track
//! is never advertised without a publisher behind it.
//!
//! A bare value stays untimed; only a timed track accepts an explicit capture time.
//!
//! A value timed on the broadcast [`Clock`](crate::Clock) is written at that timestamp as given,
//! and the entry advertises how late values reach the transport as its `jitter` and `delay`, the
//! way a media rendition does. A capture [`Instant`](kio::clock::Instant) converts with
//! [`Clock::capture`](crate::Clock::capture) on the catalog's clock:
//!
//! ```no_run
//! # fn example(
//! #     catalog: &moq_mux::catalog::Producer,
//! #     gps: &mut moq_mux::json::Stream<serde_json::Value>,
//! #     fix: serde_json::Value,
//! #     received: kio::clock::Instant,
//! # ) -> moq_mux::Result<()> {
//! let at = catalog.clock().capture(received)?;
//! gps.append(moq_net::Timed::from(&fix).at(at))?;
//! # Ok(())
//! # }
//! ```
//!
//! A timestamp carried over from elsewhere, such as a source's own timestamp or the `at` of a
//! consumed [`Timed`] the value was derived from, is published unchanged too. It lines up with
//! media only on a broadcast sharing the source's clock mapping.
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
//! let config = catalog.json.tracks.get("chat").expect("no chat track");
//! let entry = moq_mux::catalog::Entry::new("chat", config);
//! let mut chat = entry.subscribe::<Message>(source).await?;
//! while let Some(message) = chat.next().await? {
//!     // ...
//! }
//! # Ok(())
//! # }
//! ```

use moq_net::Timed;
use serde::Serialize;
use serde::de::DeserializeOwned;

use hang::catalog::{JsonConfig, Mode, Reference};

use crate::catalog::hang::CatalogExt;
use crate::catalog::{IntoRendition, Listing, RenditionConfig};

/// Everything a JSON track declares about itself, beyond its mode and name.
///
/// Start from [`default`](Default::default) and chain the setters. The mode is not in here: it is
/// fixed by which producer you create. To list the track in an application's own catalog section
/// instead of `json`, pass that section's entry (see [`IntoRendition`]).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Config {
	/// An optional identifier for the shape of each value, typically a JSON Schema URL.
	pub schema: Option<String>,
}

impl Config {
	/// Set [`schema`](Self::schema) (a builder, since the struct is `#[non_exhaustive]`).
	pub fn with_schema(mut self, schema: impl Into<String>) -> Self {
		self.schema = Some(schema.into());
		self
	}
}

/// How a [`Snapshot`] encodes its frames, beyond what its catalog entry declares.
///
/// Start from [`default`](Default::default) and chain the setters. None of this is written into
/// the catalog entry: every consumer decodes the frames the same way, so a reader needs nothing
/// from the entry to follow them.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Options {
	/// Whether changes ride as merge-patch deltas (see [`moq_json::snapshot::Config::deltas`]).
	/// Defaults to `true`.
	pub deltas: bool,
}

impl Default for Options {
	fn default() -> Self {
		Self { deltas: true }
	}
}

impl Options {
	/// Set [`deltas`](Self::deltas) (a builder, since the struct is `#[non_exhaustive]`).
	pub fn with_deltas(mut self, deltas: bool) -> Self {
		self.deltas = deltas;
		self
	}
}

impl<E: CatalogExt> IntoRendition<E, JsonConfig> for Config {
	type Config = JsonConfig;

	fn into_rendition(self) -> JsonConfig {
		// The producer overwrites the mode with the one it publishes in.
		let mut entry = JsonConfig::new(Mode::Snapshot);
		entry.schema = self.schema;
		entry
	}
}

/// Fix `config`'s mode.
///
/// Errors on a `broadcast` reference, which would point consumers away from the track this
/// publishes.
fn prepare(config: &mut impl AsMut<JsonConfig>, mode: Mode) -> crate::Result<()> {
	let json = config.as_mut();
	if json.broadcast.is_some() {
		return Err(crate::Error::ForeignBroadcast);
	}
	json.mode = mode;
	Ok(())
}

/// Publishes a latest-value JSON track, advertised in the catalog for as long as this handle lives.
///
/// Every [`update`](Self::update) supersedes the last, so a consumer reads only the newest value.
/// For a log where every record survives, use [`Stream`].
pub struct Snapshot<T> {
	inner: moq_json::snapshot::Producer<T>,
	listing: Listing,
}

impl<T: Serialize> Snapshot<T> {
	pub(crate) fn new<E: CatalogExt, C: RenditionConfig<E> + AsMut<JsonConfig>>(
		track: moq_net::track::Producer,
		rendition: crate::catalog::Rendition<E, C>,
		mut config: C,
		options: Options,
	) -> crate::Result<Self> {
		prepare(&mut config, Mode::Snapshot)?;
		let json = moq_json::snapshot::Config::default().with_deltas(options.deltas);
		let inner = moq_json::snapshot::Producer::new(track, json);
		let listing = Listing::new(rendition, config)?;
		Ok(Self { inner, listing })
	}

	/// The track name, which is also the catalog key.
	pub fn name(&self) -> &str {
		self.listing.name()
	}

	/// Create a subscriber for the underlying track.
	pub fn consume(&self) -> moq_net::track::Subscriber {
		self.inner.consume()
	}

	/// A watch-only handle to whether this track has subscribers.
	pub fn demand(&self) -> moq_net::track::Demand {
		self.inner.demand()
	}

	/// Wait for a joiner to want a fresh group, then restate the value in one; see
	/// [`moq_json::snapshot::Producer::refresh`].
	///
	/// A restatement is not a new value, so it measures neither `jitter` nor `delay`.
	pub async fn refresh(&self) -> crate::Result<()> {
		Ok(self.inner.refresh().await?)
	}

	/// Poll for [`refresh`](Self::refresh), for an owner driving the producer synchronously.
	pub fn poll_refresh(&self, waiter: &kio::Waiter) -> std::task::Poll<crate::Result<()>> {
		self.inner.poll_refresh(waiter).map_err(Into::into)
	}

	/// Publish a new value, superseding the previous one.
	///
	/// A value timed on the broadcast clock is written at that timestamp and measures the entry's
	/// `jitter` and `delay`; one ahead of now measures as zero delay. An unchanged
	/// value writes nothing and measures nothing.
	pub fn update<'a>(&mut self, value: impl Into<Timed<&'a T>>) -> crate::Result<()>
	where
		T: 'a,
	{
		let value = value.into();
		let captured = value.at;
		match self.inner.update(value)? {
			Some(size) => self.listing.record(size, captured),
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

	/// Abort the track with `err`, which consumers see in place of a clean end, and retire its
	/// catalog entry.
	pub fn abort(self, err: moq_net::Error) -> crate::Result<()> {
		Ok(self.inner.abort(err)?)
	}
}

/// Publishes an append-log JSON track, advertised in the catalog for as long as this handle lives.
///
/// Every [`append`](Self::append) is preserved and delivered in order. For a latest-value document,
/// use [`Snapshot`].
pub struct Stream<T> {
	inner: moq_json::stream::Producer<T>,
	name: String,

	/// Cleared when a terminal failure ends the track, which retires the catalog entry with it. An
	/// entry advertising a track that can no longer accept records only misleads a consumer that
	/// discovers it afterwards.
	listing: Option<Listing>,
}

impl<T: Serialize> Stream<T> {
	pub(crate) fn new<E: CatalogExt, C: RenditionConfig<E> + AsMut<JsonConfig>>(
		track: moq_net::track::Producer,
		rendition: crate::catalog::Rendition<E, C>,
		mut config: C,
	) -> crate::Result<Self> {
		prepare(&mut config, Mode::Stream)?;
		let inner = moq_json::stream::Producer::new(track, moq_json::stream::Config::default());
		let listing = Listing::new(rendition, config)?;
		Ok(Self {
			inner,
			name: listing.name().to_string(),
			listing: Some(listing),
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

	/// A watch-only handle to whether this track has subscribers.
	pub fn demand(&self) -> moq_net::track::Demand {
		self.inner.demand()
	}

	/// Append one record to the log.
	///
	/// A record that cannot be written ends the track (see [`moq_json::stream::Producer::append`])
	/// and retires the catalog entry with it. A record refused with
	/// [`moq_net::Error::FrameTooLarge`], [`moq_net::Error::GroupTooLarge`], or
	/// [`moq_net::Error::TimestampMismatch`] leaves both intact, since nothing was written. A catalog error publishing the measured bitrate is returned after the record was written, so the track
	/// stays open and a retry would duplicate it.
	pub fn append<'a>(&mut self, value: impl Into<Timed<&'a T>>) -> crate::Result<()>
	where
		T: 'a,
	{
		let value = value.into();
		let captured = value.at;
		let size = match self.inner.append(value) {
			Ok(size) => size,
			// Refused before anything was written, so the log and its entry carry on.
			Err(
				err @ moq_json::Error::Net(
					moq_net::Error::FrameTooLarge | moq_net::Error::GroupTooLarge | moq_net::Error::TimestampMismatch,
				),
			) => {
				return Err(err.into());
			}
			Err(err) => {
				// The inner producer has already ended the track. Dropping the listing retires the
				// catalog entry: waiting for the handle to drop would keep advertising a track that
				// can no longer accept records, so a consumer discovering it now would subscribe to
				// an already-ended log.
				self.listing = None;
				return Err(err.into());
			}
		};

		match &mut self.listing {
			Some(listing) => listing.record(size, captured),
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

	/// Abort the track with `err`, which consumers see in place of a clean end, and retire its
	/// catalog entry.
	pub fn abort(self, err: moq_net::Error) -> crate::Result<()> {
		Ok(self.inner.abort(err)?)
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
	/// Errors if the entry declares a mode this build doesn't implement, which is a track a consumer
	/// must skip rather than guess at.
	pub fn from_track(track: moq_net::track::Subscriber, config: &JsonConfig) -> crate::Result<Self> {
		let inner = match &config.mode {
			Mode::Snapshot => Inner::Snapshot(moq_json::snapshot::Consumer::new(track, Default::default())),
			Mode::Stream => Inner::Stream(moq_json::stream::Consumer::new(track, Default::default())),
			other => return Err(crate::Error::UnsupportedMode(other.to_string())),
		};

		Ok(Self {
			inner,
			mode: config.mode.clone(),
		})
	}

	/// The mode the publisher chose.
	///
	/// [`Mode::Snapshot`] is lossy: groups may be skipped once superseded beyond the subscription
	/// drift budget. [`Mode::Stream`] preserves every record a reader keeps up with; one that falls
	/// behind a group's retained tail sees `Evicted`. Check this when the application can only
	/// work with one of them.
	pub fn mode(&self) -> &Mode {
		&self.mode
	}

	/// Get the next value, or `None` once the track ends.
	pub async fn next(&mut self) -> crate::Result<Option<Timed<T>>> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Poll for the next value, without blocking.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> std::task::Poll<crate::Result<Option<Timed<T>>>> {
		let result = match &mut self.inner {
			Inner::Snapshot(consumer) => consumer.poll_next(waiter),
			Inner::Stream(consumer) => consumer.poll_next(waiter),
		};

		match result {
			std::task::Poll::Ready(value) => std::task::Poll::Ready(value.map_err(Into::into)),
			std::task::Poll::Pending => std::task::Poll::Pending,
		}
	}

	/// Skip to the newest snapshot state; stream mode returns `UnsupportedMode`.
	pub async fn latest(&mut self) -> crate::Result<Option<Timed<T>>> {
		kio::wait(|waiter| self.poll_latest(waiter)).await
	}

	/// Poll for the newest snapshot state without discarding records from a lossless stream.
	pub fn poll_latest(&mut self, waiter: &kio::Waiter) -> std::task::Poll<crate::Result<Option<Timed<T>>>> {
		match &mut self.inner {
			Inner::Snapshot(consumer) => consumer.poll_latest(waiter).map(|result| result.map_err(Into::into)),
			Inner::Stream(_) => std::task::Poll::Ready(Err(crate::Error::UnsupportedMode(self.mode.to_string()))),
		}
	}
}

impl crate::catalog::Entry<'_, JsonConfig> {
	/// Subscribe to this track and read it as the entry declares.
	///
	/// Resolves the track through `source`, so an entry whose [`broadcast`](JsonConfig::broadcast)
	/// field names a sibling broadcast is followed rather than silently read from the catalog's own.
	///
	/// Errors if the entry declares a mode this build doesn't implement.
	pub async fn subscribe<T: DeserializeOwned>(&self, source: &crate::Source) -> crate::Result<Consumer<T>> {
		let track = source
			.subscribe_track(
				self.config().broadcast.as_ref(),
				self.config().epoch.as_ref(),
				self.config().track(self.name()),
			)
			.await?;
		Consumer::from_track(track, self.config())
	}
}

#[cfg(test)]
mod test {
	use std::task::Poll;

	use serde_json::{Value, json};

	use super::*;

	/// A millisecond track: the frames written here carry timestamps.
	fn timed() -> moq_net::track::Info {
		moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
	}

	fn catalog() -> (moq_net::broadcast::Producer, crate::catalog::Producer) {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		(broadcast, catalog)
	}

	/// Create the moq-net track a data producer publishes on, as a caller does for a media track.
	fn track(broadcast: &mut moq_net::broadcast::Producer, name: &str) -> moq_net::track::Producer {
		broadcast
			.create_track(name, moq_net::track::Info::default().with_compress(true))
			.unwrap()
	}

	fn timed_track(broadcast: &mut moq_net::broadcast::Producer, name: &str) -> moq_net::track::Producer {
		broadcast.create_track(name, timed().with_compress(true)).unwrap()
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
		while let Poll::Ready(Ok(Some(value))) = if consumer.mode() == &Mode::Snapshot {
			consumer.poll_latest(&waiter)
		} else {
			consumer.poll_next(&waiter)
		} {
			out.push(value.value);
		}
		out
	}

	#[test]
	fn a_stream_track_roundtrips() {
		let (mut broadcast, catalog) = catalog();
		let mut chat = catalog
			.json_stream::<Value>(track(&mut broadcast, "chat"), Config::default())
			.unwrap();

		let track = chat.consume();
		let expected: Vec<Value> = (0..3).map(|n| json!({ "n": n })).collect();
		for value in &expected {
			chat.append(value).unwrap();
		}
		// `finish` retires the entry, so read both off the live track first.
		let entry = entry(&catalog, "chat");
		chat.finish().unwrap();

		// The catalog is the only thing the reader is told; the mode comes from it.
		let consumer = Consumer::from_track(track, &entry).unwrap();
		assert_eq!(consumer.mode(), &Mode::Stream);
		assert_eq!(drain(consumer), expected);
	}

	#[test]
	fn a_snapshot_track_roundtrips() {
		let (mut broadcast, catalog) = catalog();
		let mut status = catalog
			.json_snapshot::<Value>(track(&mut broadcast, "status"), Config::default(), Options::default())
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

	/// A track registered before an importer's first frame fixes the clock, so an importer starting
	/// an hour in shifts onto it: the publisher keeps stamping on the clock it started with.
	#[test]
	fn an_importer_joins_the_data_tracks_clock() {
		let (mut broadcast, catalog) = catalog();
		let mut status = catalog
			.json_snapshot::<Value>(
				timed_track(&mut broadcast, "status"),
				Config::default(),
				Options::default(),
			)
			.unwrap();
		let mut chat = catalog
			.json_stream::<Value>(timed_track(&mut broadcast, "chat"), Config::default())
			.unwrap();
		let mut subscribers = [status.consume(), chat.consume()];

		let published = catalog.snapshot().clock;
		let before = catalog.clock().now();
		// The stream starts an hour in, far from the ten seconds a fresh clock reads.
		let first = catalog
			.timebase()
			.shift(moq_net::Timestamp::from_micros(3_600_000_000).unwrap())
			.unwrap();
		assert_eq!(catalog.snapshot().clock, published, "the clock never moves");
		assert!(before <= first, "the importer lands at now: {before:?} {first:?}");
		status
			.update(Timed::from(&json!({ "live": true })).at(catalog.clock().now()))
			.unwrap();
		chat.append(Timed::from(&json!({ "n": 1 })).at(catalog.clock().now()))
			.unwrap();
		let after = catalog.clock().now();

		let waiter = kio::Waiter::noop();
		for subscriber in &mut subscribers {
			let mut stamps = Vec::new();
			while let Poll::Ready(Ok(Some(mut group))) = subscriber.poll_recv_group(&waiter) {
				while let Poll::Ready(Ok(Some(frame))) = group.poll_read_frame(&waiter) {
					stamps.push(frame.timestamp.unwrap().as_millis());
				}
			}
			assert_eq!(stamps.len(), 1);
			assert!(
				// The track stores milliseconds.
				first.as_millis() <= stamps[0] && stamps[0] <= after.as_millis(),
				"{first:?} {stamps:?} {after:?}"
			);
		}
	}

	/// `deltas` reaches the encoder whatever the catalog entry, including one that is not a
	/// [`Config`]. Without deltas each value is its own group; with them the next value joins
	/// that group.
	#[test]
	fn the_deltas_option_reaches_the_encoder() {
		let (mut broadcast, catalog) = catalog();
		let mut full = catalog
			.json_snapshot::<Value>(
				track(&mut broadcast, "full"),
				JsonConfig::new(Mode::Snapshot),
				Options::default().with_deltas(false),
			)
			.unwrap();
		let mut delta = catalog
			.json_snapshot::<Value>(track(&mut broadcast, "delta"), Config::default(), Options::default())
			.unwrap();

		let mut full_track = full.consume();
		let mut delta_track = delta.consume();
		for value in [json!({ "n": 1 }), json!({ "n": 2 })] {
			full.update(&value).unwrap();
			delta.update(&value).unwrap();
		}
		full.finish().unwrap();
		delta.finish().unwrap();

		// The default subscription budget keeps only the latest group, aging untimed groups by
		// wall clock. Without deltas the second value rolled a new group, so that group holds one
		// frame. With deltas the second value joins the same group.
		assert_eq!(ready_groups(&mut full_track), vec![1]);
		assert_eq!(ready_groups(&mut delta_track), vec![2]);
	}

	fn ready_groups(subscriber: &mut moq_net::track::Subscriber) -> Vec<usize> {
		let waiter = kio::Waiter::noop();
		let mut counts = Vec::new();
		loop {
			match subscriber.poll_recv_group(&waiter) {
				Poll::Ready(Ok(Some(group))) => counts.push(group.frame_count()),
				Poll::Ready(Ok(None)) | Poll::Pending => return counts,
				Poll::Ready(Err(err)) => panic!("group ended in error: {err}"),
			}
		}
	}

	#[test]
	fn the_entry_describes_how_to_read_the_track() {
		let (mut broadcast, catalog) = catalog();
		let _chat = catalog
			.json_stream::<Value>(
				track(&mut broadcast, "chat"),
				Config::default().with_schema("https://example.com/chat.schema.json"),
			)
			.unwrap();

		let entry = entry(&catalog, "chat");
		assert_eq!(entry.mode, Mode::Stream);
		assert_eq!(entry.schema.as_deref(), Some("https://example.com/chat.schema.json"));
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
		existing.broadcast = Some(moq_net::path::Relative::new("source").unwrap());

		let mut seed = crate::catalog::hang::Catalog::<()>::default();
		seed.json.tracks.insert("chat".to_string(), existing.clone());
		let config = crate::catalog::Config::default().with_catalog(seed);
		let catalog = crate::catalog::Producer::new(&mut broadcast, config).unwrap();

		// Nothing local holds the track name, so `create_track` alone would have let this through.
		assert!(matches!(
			catalog.json_stream::<Value>(track(&mut broadcast, "chat"), Config::default()),
			Err(crate::Error::Hang(hang::Error::Duplicate(_)))
		));
		assert_eq!(catalog.snapshot().json.tracks.get("chat"), Some(&existing));
	}

	/// Writes fill an absent bitrate; one the publisher supplied is left alone.
	#[test]
	fn writes_fill_an_absent_bitrate() {
		let (mut broadcast, catalog) = catalog();
		let mut gps = catalog
			.json_stream::<Value>(track(&mut broadcast, "gps"), Config::default())
			.unwrap();
		let mut supplied = JsonConfig::new(Mode::Stream);
		supplied.bitrate = Some(4_200);
		let mut status = catalog
			.json_snapshot::<Value>(track(&mut broadcast, "status"), supplied, Options::default())
			.unwrap();

		// 40ms records of 500 bytes: 100 kbps, over more than the bitrate window.
		for i in 0..60u64 {
			let now = moq_net::Timestamp::from_micros(i * 40_000).unwrap();
			gps.listing.as_mut().unwrap().record_at(now, 500, None).unwrap();
			status.listing.record_at(now, 500, None).unwrap();
		}

		assert_eq!(entry(&catalog, "gps").bitrate, Some(100_000));
		assert_eq!(entry(&catalog, "status").bitrate, Some(4_200));
		assert_eq!(entry(&catalog, "gps").jitter, None, "write spacing is not jitter");
	}

	/// An unchanged snapshot value writes no frame, so its capture time must not measure a write
	/// that never happened.
	#[test]
	fn an_unchanged_snapshot_measures_nothing() {
		let (mut broadcast, catalog) = catalog();
		let mut gps = catalog
			.json_stream::<Value>(timed_track(&mut broadcast, "gps"), Config::default())
			.unwrap();
		let mut status = catalog
			.json_snapshot::<Value>(
				timed_track(&mut broadcast, "status"),
				Config::default(),
				Options::default(),
			)
			.unwrap();
		let now = catalog.clock().now();
		let value = serde_json::json!({ "armed": true });
		status.update(Timed::from(&value).at(now)).unwrap();
		gps.append(Timed::from(&serde_json::json!({ "lat": 0 })).at(now))
			.unwrap();
		let stale = now
			.checked_sub(moq_net::Timestamp::from_micros(1_000_000).unwrap())
			.unwrap();
		status.update(Timed::from(&value).at(stale)).unwrap();
		assert_eq!(entry(&catalog, "status").delay, None);

		// The same stale capture on a changed value is written behind gps.
		let changed = serde_json::json!({ "armed": false });
		status.update(Timed::from(&changed).at(stale)).unwrap();
		let delay = entry(&catalog, "status").delay.expect("a capture behind gps is delay");
		assert!(delay >= std::time::Duration::from_secs(1), "{delay:?}");
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
			.json_snapshot::<Value>(track(&mut broadcast, "status"), Config::default(), Options::default())
			.unwrap();

		let snapshot = catalog.snapshot();
		let found: Vec<(&str, &Mode)> = snapshot
			.json
			.tracks
			.iter()
			.map(|(name, config)| (name.as_str(), &config.mode))
			.collect();
		assert_eq!(found, vec![("chat", &Mode::Stream), ("status", &Mode::Snapshot)]);

		// A name the catalog doesn't list has no entry, rather than a config to misread.
		assert!(!snapshot.json.tracks.contains_key("nope"));

		// Pairing the map key with its config gives consumers one subscription handle.
		let chat = crate::catalog::Entry::new("chat", snapshot.json.tracks.get("chat").expect("missing entry"));
		assert_eq!(chat.name(), "chat");
		assert_eq!(chat.mode, Mode::Stream);
	}

	/// The whole read path: name the track once, and the entry resolves the subscription (through
	/// `Source`, so a cross-broadcast reference is followed) and says how to decode it.
	#[tokio::test]
	async fn an_entry_subscribes_and_reads() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let source = crate::source::announced(&broadcast.consume()).await;

		let mut chat = catalog
			.json_stream::<Value>(track(&mut broadcast, "chat"), Config::default())
			.unwrap();
		chat.append(&json!({ "text": "hello" })).unwrap();

		let snapshot = catalog.snapshot();
		let entry = crate::catalog::Entry::new("chat", snapshot.json.tracks.get("chat").expect("missing entry"));
		let mut consumer = entry.subscribe::<Value>(&source).await.unwrap();
		chat.finish().unwrap();

		assert!(matches!(
			consumer.poll_latest(&kio::Waiter::noop()),
			Poll::Ready(Err(crate::Error::UnsupportedMode(_)))
		));
		assert_eq!(
			consumer.next().await.unwrap().map(|value| value.value),
			Some(json!({ "text": "hello" }))
		);
		assert_eq!(consumer.next().await.unwrap().map(|value| value.value), None);
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

	/// A record past the frame budget is refused before anything is written, so the log stays
	/// advertised and the next record still lands.
	#[test]
	fn an_oversized_record_keeps_the_track_and_the_entry() {
		let (mut broadcast, catalog) = catalog();
		let mut chat = catalog
			.json_stream::<Value>(track(&mut broadcast, "chat"), Config::default())
			.unwrap();
		let track = chat.consume();

		let oversized = Value::String("x".repeat(moq_net::group::MAX_CACHE_BYTES as usize));
		assert!(matches!(
			chat.append(&oversized),
			Err(crate::Error::Json(moq_json::Error::Net(moq_net::Error::FrameTooLarge)))
		));
		assert!(catalog.snapshot().json.tracks.contains_key("chat"));

		chat.append(&json!({ "n": 1 })).unwrap();
		let entry = entry(&catalog, "chat");
		chat.finish().unwrap();
		assert_eq!(
			drain(Consumer::from_track(track, &entry).unwrap()),
			vec![json!({ "n": 1 })]
		);
	}

	/// A consumer that can't tell a log from a latest-value document would silently drop records,
	/// so an unreadable entry is an error rather than a guess.
	#[test]
	fn an_unrecognized_mode_is_refused() {
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("chat", timed())
			.unwrap();

		let config = JsonConfig::new(Mode::Unknown("windowed".to_string()));
		assert!(matches!(
			Consumer::<Value>::from_track(track.subscribe(None), &config),
			Err(crate::Error::UnsupportedMode(_))
		));
	}
}
