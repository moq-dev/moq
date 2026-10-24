//! Generic JSON tracks over the FFI boundary.
//!
//! Wraps [`moq_json`] so native callers can publish and consume JSON on a track, in either mode:
//! `snapshot` (lossy latest-value, RFC 7396 merge-patch deltas) or `stream` (lossless
//! append-log). Each type is constructed from the track it wraps, so a track accepted from a
//! request works as well as one created by name. Values cross the boundary as JSON strings; the
//! caller parses and serializes on its own side.

use std::sync::Arc;

use serde_json::Value;

use crate::consumer::{MoqTrackConsumer, timestamp_us};
use crate::demand::MoqTrackDemand;
use crate::error::MoqError;
use crate::ffi::Task;
use crate::producer::{MoqBroadcastProducer, MoqTrackProducer};

/// Options for a JSON snapshot track (lossy latest-value mode).
///
/// The same config is passed to both the producer and the consumer, but the consumer reads only
/// [`compression`](Self::compression); [`deltas`](Self::deltas) is producer-only.
#[derive(Clone, uniffi::Record)]
pub struct MoqJsonSnapshotConfig {
	/// Encode updates as deltas against the group's snapshot. When off, each update opens a new
	/// group with a full snapshot. How long a group runs is set by the track's `refresh` ratio
	/// instead. Ignored by the consumer.
	#[uniffi(default = true)]
	pub deltas: bool,

	/// DEFLATE-compress each group. Must match on the producer and consumer.
	#[uniffi(default = false)]
	pub compression: bool,
}

fn compression(on: bool) -> moq_json::Compression {
	if on {
		moq_json::Compression::Deflate
	} else {
		moq_json::Compression::None
	}
}

impl From<MoqJsonSnapshotConfig> for moq_json::snapshot::Config {
	fn from(config: MoqJsonSnapshotConfig) -> Self {
		let mut out = moq_json::snapshot::Config::default();
		out.deltas = config.deltas;
		out.compression = compression(config.compression);
		out
	}
}

impl From<MoqJsonSnapshotConfig> for moq_json::snapshot::consumer::Config {
	fn from(config: MoqJsonSnapshotConfig) -> Self {
		let mut out = moq_json::snapshot::consumer::Config::default();
		out.compression = compression(config.compression);
		out
	}
}

/// Options for a JSON stream track (lossless append-log mode).
///
/// The same config is passed to both the producer and the consumer.
#[derive(Clone, uniffi::Record)]
pub struct MoqJsonStreamConfig {
	/// DEFLATE-compress the group. Must match on the producer and consumer.
	#[uniffi(default = false)]
	pub compression: bool,
}

impl From<MoqJsonStreamConfig> for moq_json::stream::Config {
	fn from(config: MoqJsonStreamConfig) -> Self {
		let mut out = moq_json::stream::Config::default();
		out.compression = compression(config.compression);
		out
	}
}

#[cfg(test)]
mod tests {
	// The `#[uniffi(default = ...)]` attributes have to be literals, so they restate moq-json's
	// own defaults. Every binding inherits those literals, so a drift here would silently give
	// each wrapper different behavior than the Rust API.
	#[test]
	fn record_defaults_match_moq_json() {
		let snapshot = moq_json::snapshot::Config::default();
		assert!(snapshot.deltas, "update #[uniffi(default)] on deltas");
		assert_eq!(
			snapshot.compression,
			moq_json::Compression::None,
			"update #[uniffi(default)] on compression"
		);
		assert_eq!(
			moq_json::stream::Config::default().compression,
			moq_json::Compression::None,
			"update #[uniffi(default)] on MoqJsonStreamConfig::compression"
		);
	}
}

// ---- Snapshot ----

/// Publishes a JSON value that consumers see as a single latest state.
///
/// A join that finds more deltas than the track's `refresh` ratio allows gets the value restated
/// in a fresh group, by a task this producer runs until it is finished or dropped. That joiner
/// still reads the cached group first.
#[derive(uniffi::Object)]
pub struct MoqJsonSnapshotProducer {
	inner: Arc<std::sync::Mutex<Option<moq_mux::json::Snapshot<Value>>>>,

	/// Closed on finish or drop, which stops the refresh task.
	alive: kio::Producer<()>,
}

impl MoqJsonSnapshotProducer {
	/// Answer joiners until the producer is finished or dropped, or a refresh fails.
	///
	/// A failed restatement only stops the refreshes: the error that caused it (a closed track, a
	/// rejected write) reaches the caller through its next update.
	async fn refresh(inner: Arc<std::sync::Mutex<Option<moq_mux::json::Snapshot<Value>>>>, alive: kio::Consumer<()>) {
		loop {
			let stop = kio::wait(|waiter| {
				if alive.poll_closed(waiter).is_ready() {
					return std::task::Poll::Ready(true);
				}
				match inner.lock().unwrap().as_ref() {
					Some(producer) => producer.poll_refresh(waiter).map(|res| res.is_err()),
					None => std::task::Poll::Ready(true),
				}
			})
			.await;
			if stop {
				return;
			}
		}
	}
}

impl Drop for MoqJsonSnapshotProducer {
	fn drop(&mut self) {
		// The refresh task shares `inner`, so drop the producer here rather than whenever the task
		// next wakes: dropping it retires the catalog entry.
		self.inner.lock().unwrap().take();
	}
}

#[uniffi::export]
impl MoqJsonSnapshotProducer {
	/// Publish `track` as a JSON snapshot track (lossy latest-value), advertised in `broadcast`'s catalog.
	///
	/// `track` must come from `broadcast`; otherwise the catalog advertises a track the broadcast
	/// does not carry.
	///
	/// The catalog carries `json.tracks.<name>` (`mode: snapshot`, and `compression: deflate` when
	/// set) for as long as the producer lives; finishing or dropping it retires the entry. Takes
	/// over `track`, whose handle is closed afterward. Errors if the catalog already carries an
	/// entry under the track's name.
	#[uniffi::constructor]
	pub fn new(
		broadcast: &MoqBroadcastProducer,
		track: &MoqTrackProducer,
		config: MoqJsonSnapshotConfig,
	) -> Result<Arc<Self>, MoqError> {
		let _guard = crate::ffi::enter();
		let config = moq_mux::json::Config::default()
			.with_compression(config.compression)
			.with_deltas(config.deltas);
		let producer = track
			.adopt(|track| broadcast.with_state(|state| Ok(state.catalog.json_snapshot::<Value>(track, config)?)))?;
		let inner = Arc::new(std::sync::Mutex::new(Some(producer)));
		let alive = kio::Producer::new(());
		crate::ffi::spawn(Self::refresh(inner.clone(), alive.consume()));
		Ok(Arc::new(Self { inner, alive }))
	}

	/// Publish a new value, encoded as a snapshot or delta automatically. `value` is a JSON
	/// document. A no-op if unchanged from the previous update.
	/// `timestamp_us` is on the broadcast clock; null publishes untimed.
	#[uniffi::method(default(timestamp_us = None))]
	pub fn update(&self, value: String, timestamp_us: Option<u64>) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let value: Value = serde_json::from_str(&value)?;
		let mut guard = self.inner.lock().unwrap();
		let producer = guard.as_mut().ok_or(MoqError::Closed)?;
		producer.update(moq_net::Timed {
			value: &value,
			at: timestamp_us.map(moq_net::Timestamp::from_micros).transpose()?,
		})?;
		Ok(())
	}

	/// A watch-only handle to whether this track has subscribers.
	pub fn demand(&self) -> Result<Arc<MoqTrackDemand>, MoqError> {
		let guard = self.inner.lock().unwrap();
		Ok(MoqTrackDemand::new(guard.as_ref().ok_or(MoqError::Closed)?.demand()))
	}

	/// Finish the track, closing any open group.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let producer = self.inner.lock().unwrap().take().ok_or(MoqError::Closed)?;
		let _ = self.alive.close();
		producer.finish()?;
		Ok(())
	}
}

/// A JSON value read from a track, with the timestamp of the frame that carried it.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct MoqJsonValue {
	/// The JSON document.
	pub value: String,
	/// The frame's timestamp on the broadcast clock in microseconds, or null when untimed.
	/// Rounded down to a whole microsecond, so a tick on a track like 90 kHz may not survive a
	/// read and republish exactly.
	pub timestamp_us: Option<u64>,
}

impl MoqJsonValue {
	fn new(timed: Option<moq_net::Timed<Value>>) -> Result<Option<Self>, MoqError> {
		let Some(timed) = timed else { return Ok(None) };
		Ok(Some(Self {
			value: serde_json::to_string(&timed.value)?,
			timestamp_us: timed.at.map(timestamp_us).transpose()?,
		}))
	}
}

struct SnapshotConsumer {
	inner: moq_json::snapshot::Consumer<Value>,
}

/// Consumes a JSON snapshot track, yielding each reconstructed state or skipping to the newest.
#[derive(uniffi::Object)]
pub struct MoqJsonSnapshotConsumer {
	task: Task<SnapshotConsumer>,
}

#[uniffi::export]
impl MoqJsonSnapshotConsumer {
	/// Read `track` as a JSON snapshot track (lossy latest-value).
	///
	/// Pass the same [`MoqJsonSnapshotConfig::compression`] the producer used. Takes over
	/// `track`, whose handle is closed afterward; errors if it has already read a group.
	#[uniffi::constructor]
	pub fn new(track: &MoqTrackConsumer, config: MoqJsonSnapshotConfig) -> Result<Arc<Self>, MoqError> {
		let _guard = crate::ffi::enter();
		let (track, driver) = track.take()?;
		let consumer = moq_json::snapshot::Consumer::<Value>::new(track, config.into());
		Ok(Arc::new(Self {
			task: Task::new(SnapshotConsumer { inner: consumer }).supervised(driver),
		}))
	}

	/// Get the next state in order. Returns `None` once the track ends.
	///
	/// Groups older than the subscription's `max_delay_us` are skipped, so a playhead
	/// reader sets that budget to its distance behind live.
	pub async fn next(&self) -> Result<Option<MoqJsonValue>, MoqError> {
		self.task
			.run(|mut state| async move { MoqJsonValue::new(state.inner.next().await?) })
			.await
	}

	/// Skip to the newest state, or wait for one. Returns `None` once the track ends.
	///
	/// Skipped states are gone; a later `next()` continues after this one.
	pub async fn latest(&self) -> Result<Option<MoqJsonValue>, MoqError> {
		self.task
			.run(|mut state| async move { MoqJsonValue::new(state.inner.latest().await?) })
			.await
	}

	/// Cancel all current and future `next()` and `latest()` calls.
	///
	/// Terminal: the subscription is released here, not when the handle is.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}

// ---- Stream ----

/// Publishes an ordered log of JSON records, one record per append.
#[derive(uniffi::Object)]
pub struct MoqJsonStreamProducer {
	inner: std::sync::Mutex<Option<moq_mux::json::Stream<Value>>>,
}

#[uniffi::export]
impl MoqJsonStreamProducer {
	/// Publish `track` as a JSON stream track (lossless append-log), advertised in `broadcast`'s catalog.
	///
	/// `track` must come from `broadcast`; otherwise the catalog advertises a track the broadcast
	/// does not carry.
	///
	/// The catalog carries `json.tracks.<name>` (`mode: stream`) for as long as the producer
	/// lives. Takes over `track`, whose handle is closed afterward. Errors if the catalog already
	/// carries an entry under the track's name.
	#[uniffi::constructor]
	pub fn new(
		broadcast: &MoqBroadcastProducer,
		track: &MoqTrackProducer,
		config: MoqJsonStreamConfig,
	) -> Result<Arc<Self>, MoqError> {
		let _guard = crate::ffi::enter();
		let config = moq_mux::json::Config::default().with_compression(config.compression);
		let producer = track
			.adopt(|track| broadcast.with_state(|state| Ok(state.catalog.json_stream::<Value>(track, config)?)))?;
		Ok(Arc::new(Self {
			inner: std::sync::Mutex::new(Some(producer)),
		}))
	}

	/// Append one record to the log. `value` is a JSON document.
	/// `timestamp_us` is on the broadcast clock; null publishes untimed.
	#[uniffi::method(default(timestamp_us = None))]
	pub fn append(&self, value: String, timestamp_us: Option<u64>) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let value: Value = serde_json::from_str(&value)?;
		let mut guard = self.inner.lock().unwrap();
		let producer = guard.as_mut().ok_or(MoqError::Closed)?;
		producer.append(moq_net::Timed {
			value: &value,
			at: timestamp_us.map(moq_net::Timestamp::from_micros).transpose()?,
		})?;
		Ok(())
	}

	/// A watch-only handle to whether this track has subscribers.
	pub fn demand(&self) -> Result<Arc<MoqTrackDemand>, MoqError> {
		let guard = self.inner.lock().unwrap();
		Ok(MoqTrackDemand::new(guard.as_ref().ok_or(MoqError::Closed)?.demand()))
	}

	/// Finish the track, closing the group.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let producer = self.inner.lock().unwrap().take().ok_or(MoqError::Closed)?;
		producer.finish()?;
		Ok(())
	}
}

struct StreamConsumer {
	inner: moq_json::stream::Consumer<Value>,
}

/// Consumes an ordered log of JSON records, yielding every record in order.
#[derive(uniffi::Object)]
pub struct MoqJsonStreamConsumer {
	task: Task<StreamConsumer>,
}

#[uniffi::export]
impl MoqJsonStreamConsumer {
	/// Read `track` as a JSON stream track (lossless append-log).
	///
	/// Pass the same [`MoqJsonStreamConfig::compression`] the producer used. Takes over `track`,
	/// whose handle is closed afterward; errors if it has already read a group.
	#[uniffi::constructor]
	pub fn new(track: &MoqTrackConsumer, config: MoqJsonStreamConfig) -> Result<Arc<Self>, MoqError> {
		let _guard = crate::ffi::enter();
		let (track, driver) = track.take()?;
		let consumer = moq_json::stream::Consumer::<Value>::new(track, config.into());
		Ok(Arc::new(Self {
			task: Task::new(StreamConsumer { inner: consumer }).supervised(driver),
		}))
	}

	/// Get the next record. Returns `None` once the track ends.
	pub async fn next(&self) -> Result<Option<MoqJsonValue>, MoqError> {
		self.task
			.run(|mut state| async move { MoqJsonValue::new(state.inner.next().await?) })
			.await
	}

	/// Cancel all current and future `next()` calls.
	///
	/// Terminal: the subscription is released here, not when the handle is.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}
