//! Generic JSON tracks over the FFI boundary.
//!
//! Wraps [`moq_json`] so native callers can publish and consume JSON on an arbitrary named
//! track, in either mode: `snapshot` (lossy latest-value, RFC 7396 merge-patch deltas) or
//! `stream` (lossless append-log). Values cross the boundary as JSON strings; the caller parses
//! and serializes on its own side.

use std::sync::Arc;

use serde_json::Value;

use crate::consumer::MoqBroadcastConsumer;
use crate::demand::MoqTrackDemand;
use crate::error::MoqError;
use crate::ffi::Task;
use crate::producer::MoqBroadcastProducer;
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

// ---- Entry points ----

#[uniffi::export]
impl MoqBroadcastProducer {
	/// Publish a JSON snapshot track (lossy latest-value) by name, advertised in the catalog.
	///
	/// The broadcast's catalog carries `json.tracks.<name>` (`mode: snapshot`, and
	/// `compression: deflate` when set) for as long as the track lives; finishing or dropping the
	/// producer retires it. Errors if the catalog already carries an entry under `name`.
	pub fn publish_json_snapshot(
		&self,
		name: String,
		config: MoqJsonSnapshotConfig,
	) -> Result<Arc<MoqJsonSnapshotProducer>, MoqError> {
		let _guard = crate::ffi::enter();
		self.with_state(|state| {
			let track = state.broadcast.create_track(name, None)?;
			let config = moq_mux::json::Config::default()
				.with_compression(config.compression)
				.with_delta_ratio(config.delta_ratio);
			let producer = state.catalog.json_snapshot::<Value>(track, config)?;
			Ok(Arc::new(MoqJsonSnapshotProducer {
				inner: std::sync::Mutex::new(Some(producer)),
			}))
		})
	}

	/// Publish a JSON stream track (lossless append-log) by name, advertised in the catalog.
	///
	/// The broadcast's catalog carries `json.tracks.<name>` (`mode: stream`) for as long as the
	/// track lives. Errors if the catalog already carries an entry under `name`.
	pub fn publish_json_stream(
		&self,
		name: String,
		config: MoqJsonStreamConfig,
	) -> Result<Arc<MoqJsonStreamProducer>, MoqError> {
		let _guard = crate::ffi::enter();
		self.with_state(|state| {
			let track = state.broadcast.create_track(name, None)?;
			let config = moq_mux::json::Config::default().with_compression(config.compression);
			let producer = state.catalog.json_stream::<Value>(track, config)?;
			Ok(Arc::new(MoqJsonStreamProducer {
				inner: std::sync::Mutex::new(Some(producer)),
			}))
		})
	}
}

#[uniffi::export]
impl MoqBroadcastConsumer {
	/// Subscribe to a JSON snapshot track (lossy latest-value) by name.
	///
	/// Pass the same [`MoqJsonSnapshotConfig::compression`] the producer used.
	pub async fn subscribe_json_snapshot(
		&self,
		name: String,
		config: MoqJsonSnapshotConfig,
	) -> Result<Arc<MoqJsonSnapshotConsumer>, MoqError> {
		let track = self.inner().track(&name)?.subscribe(None).await?;
		let consumer = moq_json::snapshot::Consumer::<Value>::new(track, config.into());
		Ok(Arc::new(MoqJsonSnapshotConsumer {
			task: Task::new(SnapshotConsumer { inner: consumer }),
		}))
	}

	/// Subscribe to a JSON stream track (lossless append-log) by name.
	pub async fn subscribe_json_stream(
		&self,
		name: String,
		config: MoqJsonStreamConfig,
	) -> Result<Arc<MoqJsonStreamConsumer>, MoqError> {
		let track = self.inner().track(&name)?.subscribe(None).await?;
		let consumer = moq_json::stream::Consumer::<Value>::new(track, config.into());
		Ok(Arc::new(MoqJsonStreamConsumer {
			task: Task::new(StreamConsumer { inner: consumer }),
		}))
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
