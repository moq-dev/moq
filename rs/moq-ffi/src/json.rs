//! Generic JSON tracks over the FFI boundary.
//!
//! Wraps [`moq_json`] so native callers can publish and consume JSON on a track, in either mode:
//! `snapshot` (lossy latest-value, RFC 7396 merge-patch deltas) or `stream` (lossless
//! append-log). Each type is constructed from the track it wraps, so a track accepted from a
//! request works as well as one created by name. Values cross the boundary as JSON strings; the
//! caller parses and serializes on its own side.

use std::sync::Arc;

use serde_json::Value;

use crate::consumer::MoqTrackConsumer;
use crate::demand::MoqTrackDemand;
use crate::error::MoqError;
use crate::ffi::Task;
use crate::producer::{MoqBroadcastProducer, MoqTrackProducer};
use moq_mux::catalog::hang::Extra;

/// Options for a JSON snapshot track (lossy latest-value mode).
///
/// The same config is passed to both the producer and the consumer, but the consumer reads only
/// [`compression`](Self::compression); [`delta_ratio`](Self::delta_ratio) is producer-only.
#[derive(Clone, uniffi::Record)]
pub struct MoqJsonSnapshotConfig {
	/// How aggressively the producer emits deltas instead of full snapshots. `0` disables deltas
	/// (one snapshot per group); a positive value allows roughly that many snapshots' worth of
	/// deltas before rolling a new group. Ignored by the consumer.
	#[uniffi(default = 8)]
	pub delta_ratio: u32,

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
		out.delta_ratio = config.delta_ratio;
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
		assert_eq!(snapshot.delta_ratio, 8, "update #[uniffi(default)] on delta_ratio");
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
#[derive(uniffi::Object)]
pub struct MoqJsonSnapshotProducer {
	inner: std::sync::Mutex<Option<moq_mux::json::Snapshot<Value, Extra>>>,
}

#[uniffi::export]
impl MoqJsonSnapshotProducer {
	/// Publish `track` as a JSON snapshot track (lossy latest-value), advertised in `broadcast`'s catalog.
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
			.with_delta_ratio(config.delta_ratio);
		let producer = track
			.adopt(|track| broadcast.with_state(|state| Ok(state.catalog.json_snapshot::<Value>(track, config)?)))?;
		Ok(Arc::new(Self {
			inner: std::sync::Mutex::new(Some(producer)),
		}))
	}

	/// Publish a new value, encoded as a snapshot or delta automatically. `value` is a JSON
	/// document. A no-op if unchanged from the previous update.
	pub fn update(&self, value: String) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let value: Value = serde_json::from_str(&value)?;
		let mut guard = self.inner.lock().unwrap();
		let producer = guard.as_mut().ok_or(MoqError::Closed)?;
		producer.update(&value)?;
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
		producer.finish()?;
		Ok(())
	}
}

struct SnapshotConsumer {
	inner: moq_json::snapshot::Consumer<Value>,
}

impl SnapshotConsumer {
	async fn next(&mut self) -> Result<Option<String>, MoqError> {
		match self.inner.next().await? {
			Some(value) => Ok(Some(serde_json::to_string(&value)?)),
			None => Ok(None),
		}
	}
}

/// Consumes a JSON snapshot track, yielding the latest reconstructed value.
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
		let consumer = moq_json::snapshot::Consumer::<Value>::new(track.take()?, config.into());
		Ok(Arc::new(Self {
			task: Task::new(SnapshotConsumer { inner: consumer }),
		}))
	}

	/// Get the next value as a JSON string. Returns `None` once the track ends.
	///
	/// A consumer that has fallen behind collapses the backlog and yields only the latest value.
	pub async fn next(&self) -> Result<Option<String>, MoqError> {
		self.task.run(|mut state| async move { state.next().await }).await
	}

	/// Cancel all current and future `next()` calls.
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
	inner: std::sync::Mutex<Option<moq_mux::json::Stream<Value, Extra>>>,
}

#[uniffi::export]
impl MoqJsonStreamProducer {
	/// Publish `track` as a JSON stream track (lossless append-log), advertised in `broadcast`'s catalog.
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
	pub fn append(&self, value: String) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let value: Value = serde_json::from_str(&value)?;
		let mut guard = self.inner.lock().unwrap();
		let producer = guard.as_mut().ok_or(MoqError::Closed)?;
		producer.append(&value)?;
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

impl StreamConsumer {
	async fn next(&mut self) -> Result<Option<String>, MoqError> {
		match self.inner.next().await? {
			Some(value) => Ok(Some(serde_json::to_string(&value)?)),
			None => Ok(None),
		}
	}
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
		let consumer = moq_json::stream::Consumer::<Value>::new(track.take()?, config.into());
		Ok(Arc::new(Self {
			task: Task::new(StreamConsumer { inner: consumer }),
		}))
	}

	/// Get the next record as a JSON string. Returns `None` once the track ends.
	pub async fn next(&self) -> Result<Option<String>, MoqError> {
		self.task.run(|mut state| async move { state.next().await }).await
	}

	/// Cancel all current and future `next()` calls.
	///
	/// Terminal: the subscription is released here, not when the handle is.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}
