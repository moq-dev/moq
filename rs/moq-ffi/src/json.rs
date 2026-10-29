//! Generic JSON tracks over the FFI boundary.
//!
//! Wraps [`moq_json`] so native callers can publish and consume JSON on a track, in one of three
//! modes: `snapshot` (lossy latest-value, RFC 7396 merge-patch deltas), `stream` (lossless
//! append-log), or `window` (a sliding run of records pushed at the back and popped from the
//! front). Each type is constructed from the track it wraps, so a track accepted from a request
//! works as well as one created by name. Values cross the boundary as JSON strings; the caller
//! parses and serializes on its own side.

use std::sync::Arc;

use serde_json::Value;

use crate::consumer::{MoqTrackConsumer, timestamp_us};
use crate::demand::MoqTrackDemand;
use crate::error::MoqError;
use crate::ffi::Task;
use crate::producer::{Child, MoqBroadcastProducer, MoqTrackProducer};

/// Options for a JSON snapshot producer (lossy latest-value mode).
///
/// Compression is the track's: set `compress` on the [`MoqTrackInfo`](crate::producer::MoqTrackInfo)
/// the track is created with. A consumer reads either kind with no option of its own.
#[derive(Clone, uniffi::Record)]
pub struct MoqJsonSnapshotConfig {
	/// Encode updates as deltas against the group's snapshot. When off, each update opens a new
	/// group with a full snapshot. How long a group runs is set by the track's `refresh` ratio
	/// instead.
	#[uniffi(default = true)]
	pub deltas: bool,
}

/// Options for a JSON window producer (a sliding run of records).
///
/// Compression is the track's, as for [`MoqJsonSnapshotConfig`].
#[derive(Clone, uniffi::Record)]
pub struct MoqJsonWindowConfig {
	/// Maximum records retained and restated when a group rolls, or null to restate the whole
	/// window. Bounds the producer's memory for a window that is never popped: a reader that keeps
	/// up still sees every record, while one joining later gets a skip event for the older ones.
	/// Must be at least one.
	#[uniffi(default = None)]
	pub checkpoint_records: Option<u64>,
}

impl TryFrom<MoqJsonWindowConfig> for moq_json::window::ProducerConfig {
	type Error = MoqError;

	fn try_from(config: MoqJsonWindowConfig) -> Result<Self, MoqError> {
		let mut out = moq_json::window::ProducerConfig::default();
		if let Some(records) = config.checkpoint_records {
			let records = usize::try_from(records)
				.ok()
				.filter(|&records| records > 0)
				.ok_or_else(|| MoqError::JsonTrack {
					reason: format!("invalid checkpoint_records: {records}"),
				})?;
			out = out.with_checkpoint_records(records);
		}
		Ok(out)
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
		let window = moq_json::window::ProducerConfig::default();
		assert_eq!(
			window.checkpoint_records, None,
			"update #[uniffi(default)] on MoqJsonWindowConfig::checkpoint_records"
		);
	}
}

// ---- Snapshot ----

/// Publishes a JSON value that consumers see as a single latest state.
///
/// A join that finds more deltas than the track's `refresh` ratio allows gets the value restated
/// in a fresh group, by a task this producer runs until it is finished or dropped. That joiner
/// still reads the cached group first.
///
/// Released before its finish, or still open when its broadcast closes, it ends its track with
/// `Unroutable`, which readers take as the publisher leaving.
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

impl Child for MoqJsonSnapshotProducer {
	fn abort(&self, err: moq_net::Error) {
		let _guard = crate::ffi::enter();
		let producer = self.inner.lock().unwrap().take();
		let _ = self.alive.close();
		if let Some(producer) = producer {
			let _ = producer.abort(err);
		}
	}
}

impl Drop for MoqJsonSnapshotProducer {
	fn drop(&mut self) {
		self.abort(moq_net::Error::Unroutable);
	}
}

#[uniffi::export]
impl MoqJsonSnapshotProducer {
	/// Publish `track` as a JSON snapshot track (lossy latest-value), advertised in `broadcast`'s catalog.
	///
	/// `track` must come from `broadcast`; otherwise the catalog advertises a track the broadcast
	/// does not carry.
	///
	/// The catalog carries `json.tracks.<name>` (`mode: snapshot`) for as long as the producer
	/// lives; finishing or dropping it retires the entry. Takes
	/// over `track`, whose handle is closed afterward. Errors if the catalog already carries an
	/// entry under the track's name.
	#[uniffi::constructor]
	pub fn new(
		broadcast: &MoqBroadcastProducer,
		track: &MoqTrackProducer,
		config: MoqJsonSnapshotConfig,
	) -> Result<Arc<Self>, MoqError> {
		let _guard = crate::ffi::enter();
		let options = moq_mux::json::Options::default().with_deltas(config.deltas);
		let config = moq_mux::json::Config::default();
		let this = track.adopt(|track| {
			let producer =
				broadcast.with_state(|state| Ok(state.catalog.json_snapshot::<Value>(track, config, options)?))?;
			Ok(Self {
				inner: Arc::new(std::sync::Mutex::new(Some(producer))),
				alive: kio::Producer::new(()),
			})
		})?;
		crate::ffi::spawn(Self::refresh(this.inner.clone(), this.alive.consume()));
		Ok(this)
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
	/// Reads a compressed track the same as a plain one. Takes over `track`, whose handle is
	/// closed afterward; errors if it has already read a group.
	#[uniffi::constructor]
	pub fn new(track: &MoqTrackConsumer) -> Result<Arc<Self>, MoqError> {
		let _guard = crate::ffi::enter();
		let (track, driver) = track.take()?;
		let consumer = moq_json::snapshot::Consumer::<Value>::new(track, Default::default());
		Ok(Arc::new(Self {
			task: Task::new(SnapshotConsumer { inner: consumer }).supervised(driver),
		}))
	}

	/// Get the next state in order. Returns `None` once the track ends.
	///
	/// Groups older than the subscription's `max_delay` are skipped, so a playhead
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
///
/// Released before its finish, or still open when its broadcast closes, it ends its track with
/// `Unroutable`, which readers take as the publisher leaving.
#[derive(uniffi::Object)]
pub struct MoqJsonStreamProducer {
	inner: std::sync::Mutex<Option<moq_mux::json::Stream<Value>>>,
}

impl Child for MoqJsonStreamProducer {
	fn abort(&self, err: moq_net::Error) {
		let _guard = crate::ffi::enter();
		let producer = self.inner.lock().unwrap().take();
		if let Some(producer) = producer {
			let _ = producer.abort(err);
		}
	}
}

impl Drop for MoqJsonStreamProducer {
	fn drop(&mut self) {
		self.abort(moq_net::Error::Unroutable);
	}
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
	pub fn new(broadcast: &MoqBroadcastProducer, track: &MoqTrackProducer) -> Result<Arc<Self>, MoqError> {
		let _guard = crate::ffi::enter();
		let config = moq_mux::json::Config::default();
		track.adopt(|track| {
			let producer = broadcast.with_state(|state| Ok(state.catalog.json_stream::<Value>(track, config)?))?;
			Ok(Self {
				inner: std::sync::Mutex::new(Some(producer)),
			})
		})
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
	/// Reads a compressed track the same as a plain one. Takes over `track`, whose handle is
	/// closed afterward; errors if it has already read a group.
	#[uniffi::constructor]
	pub fn new(track: &MoqTrackConsumer) -> Result<Arc<Self>, MoqError> {
		let _guard = crate::ffi::enter();
		let (track, driver) = track.take()?;
		let consumer = moq_json::stream::Consumer::<Value>::new(track, Default::default());
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

// ---- Window ----

/// Publishes a sliding window of JSON records: pushed at the back, popped from the front.
///
/// Unlike a stream, which keeps every record in one group forever, a window rolls to a new group
/// that restates the retained records, so a reader can join at any point and the producer's
/// memory stays bounded by what it pops or by [`MoqJsonWindowConfig::checkpoint_records`].
///
/// A join that finds the open group's edits outweigh the track's `refresh` ratio gets the window
/// restated in a fresh group, by a task this producer runs until it is finished or dropped.
///
/// Released before its finish, or still open when its broadcast closes, it ends its track with
/// `Unroutable`, which readers take as the publisher leaving.
#[derive(uniffi::Object)]
pub struct MoqJsonWindowProducer {
	inner: Arc<std::sync::Mutex<Option<moq_json::window::Producer<Value>>>>,
	demand: moq_net::track::Demand,

	/// Closed on finish or drop, which stops the refresh task.
	alive: kio::Producer<()>,
}

impl MoqJsonWindowProducer {
	/// Answer joiners until the producer is finished or dropped, or a refresh fails.
	///
	/// A failed restatement only stops the refreshes: the error that caused it (a closed track, a
	/// rejected write) reaches the caller through its next edit.
	async fn refresh(
		inner: Arc<std::sync::Mutex<Option<moq_json::window::Producer<Value>>>>,
		alive: kio::Consumer<()>,
	) {
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

	fn edit(
		&self,
		f: impl FnOnce(&mut moq_json::window::Producer<Value>) -> moq_json::Result<()>,
	) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		Ok(f(guard.as_mut().ok_or(MoqError::Closed)?)?)
	}
}

impl Child for MoqJsonWindowProducer {
	fn abort(&self, err: moq_net::Error) {
		let _guard = crate::ffi::enter();
		let producer = self.inner.lock().unwrap().take();
		let _ = self.alive.close();
		if let Some(producer) = producer {
			let _ = producer.abort(err);
		}
	}
}

impl Drop for MoqJsonWindowProducer {
	fn drop(&mut self) {
		self.abort(moq_net::Error::Unroutable);
	}
}

#[uniffi::export]
impl MoqJsonWindowProducer {
	/// Publish `track` as a JSON window track.
	///
	/// Not advertised in the broadcast's catalog, which has no window mode; a reader names the
	/// track itself. Takes over `track`, whose handle is closed afterward. Errors if
	/// [`MoqJsonWindowConfig::checkpoint_records`] is zero.
	#[uniffi::constructor]
	pub fn new(track: &MoqTrackProducer, config: MoqJsonWindowConfig) -> Result<Arc<Self>, MoqError> {
		let _guard = crate::ffi::enter();
		let config = config.try_into()?;
		let this = track.adopt(|track| {
			Ok(Self {
				demand: track.demand(),
				inner: Arc::new(std::sync::Mutex::new(Some(moq_json::window::Producer::new(
					track, config,
				)))),
				alive: kio::Producer::new(()),
			})
		})?;
		crate::ffi::spawn(Self::refresh(this.inner.clone(), this.alive.consume()));
		Ok(this)
	}

	/// Append one record to the back of the window. `value` is a JSON document.
	/// `timestamp_us` is on the broadcast clock; null publishes untimed.
	#[uniffi::method(default(timestamp_us = None))]
	pub fn push(&self, value: String, timestamp_us: Option<u64>) -> Result<(), MoqError> {
		let value: Value = serde_json::from_str(&value)?;
		let at = timestamp_us.map(moq_net::Timestamp::from_micros).transpose()?;
		self.edit(|producer| producer.push(moq_net::Timed { value: &value, at }))
	}

	/// Drop `count` records from the front of the window, clamped to what it holds.
	/// `timestamp_us` is on the broadcast clock; null publishes untimed.
	#[uniffi::method(default(timestamp_us = None))]
	pub fn pop(&self, count: u64, timestamp_us: Option<u64>) -> Result<(), MoqError> {
		let at = timestamp_us.map(moq_net::Timestamp::from_micros).transpose()?;
		self.edit(|producer| producer.pop(moq_net::Timed { value: count, at }))
	}

	/// Close the open group, so the next push or pop opens a new one whose header restates the
	/// retained window. A no-op when no group is open.
	///
	/// Cut at a natural boundary (the end of an utterance, say) so every edit so far sits in a
	/// group no later frame extends.
	pub fn cut(&self) -> Result<(), MoqError> {
		self.edit(|producer| producer.cut())
	}

	/// A watch-only handle to whether this track has subscribers.
	pub fn demand(&self) -> Result<Arc<MoqTrackDemand>, MoqError> {
		self.inner.lock().unwrap().as_ref().ok_or(MoqError::Closed)?;
		Ok(MoqTrackDemand::new(self.demand.clone()))
	}

	/// Finish the track, closing any open group.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut producer = self.inner.lock().unwrap().take().ok_or(MoqError::Closed)?;
		let _ = self.alive.close();
		producer.finish()?;
		Ok(())
	}
}

/// One change to a JSON window, as the consumer sees it.
///
/// Ranges are half-open spans of absolute record indices, `start` inclusive and `end` exclusive.
#[derive(Clone, Debug, PartialEq, uniffi::Enum)]
pub enum MoqJsonWindowEvent {
	/// A record joined the window.
	Push {
		/// The record's absolute index.
		index: u64,
		/// The JSON document.
		value: String,
	},
	/// These records left the window.
	Pop {
		/// The first index popped.
		start: u64,
		/// One past the last index popped.
		end: u64,
	},
	/// These records existed but will never be delivered: they were pushed and dropped while this
	/// consumer was behind, or fell outside the producer's checkpoint when it joined.
	Skip {
		/// The first index skipped.
		start: u64,
		/// One past the last index skipped.
		end: u64,
	},
}

/// A window event, with the timestamp of the frame that carried it.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct MoqJsonWindowTimed {
	/// The change to the window.
	pub event: MoqJsonWindowEvent,
	/// The frame's timestamp on the broadcast clock in microseconds, or null when untimed.
	/// A record first learned of from a group's restatement (after a join, or while behind)
	/// carries that frame's time, not its original push time.
	pub timestamp_us: Option<u64>,
}

impl MoqJsonWindowTimed {
	fn new(timed: Option<moq_net::Timed<moq_json::window::Event<Value>>>) -> Result<Option<Self>, MoqError> {
		use moq_json::window::Event;

		let Some(timed) = timed else { return Ok(None) };
		let event = match timed.value {
			Event::Push { index, value } => MoqJsonWindowEvent::Push {
				index,
				value: serde_json::to_string(&value)?,
			},
			Event::Pop(range) => MoqJsonWindowEvent::Pop {
				start: range.start,
				end: range.end,
			},
			Event::Skip(range) => MoqJsonWindowEvent::Skip {
				start: range.start,
				end: range.end,
			},
			event => {
				return Err(MoqError::JsonTrack {
					reason: format!("unsupported window event: {event:?}"),
				});
			}
		};
		Ok(Some(Self {
			event,
			timestamp_us: timed.at.map(timestamp_us).transpose()?,
		}))
	}
}

struct WindowConsumer {
	inner: moq_json::window::Consumer<Value>,
}

/// Consumes a sliding window of JSON records, yielding each push, pop, and skip in order.
#[derive(uniffi::Object)]
pub struct MoqJsonWindowConsumer {
	task: Task<WindowConsumer>,
}

#[uniffi::export]
impl MoqJsonWindowConsumer {
	/// Read `track` as a JSON window track.
	///
	/// Reads a compressed track the same as a plain one. Takes over `track`, whose handle is
	/// closed afterward; errors if it has already read a group.
	#[uniffi::constructor]
	pub fn new(track: &MoqTrackConsumer) -> Result<Arc<Self>, MoqError> {
		let _guard = crate::ffi::enter();
		let (track, driver) = track.take()?;
		let consumer = moq_json::window::Consumer::<Value>::new(track, Default::default());
		Ok(Arc::new(Self {
			task: Task::new(WindowConsumer { inner: consumer }).supervised(driver),
		}))
	}

	/// Get the next change to the window. Returns `None` once the track ends.
	///
	/// Group rolls never surface: a restated record this reader already has yields nothing.
	pub async fn next(&self) -> Result<Option<MoqJsonWindowTimed>, MoqError> {
		self.task
			.run(|mut state| async move { MoqJsonWindowTimed::new(state.inner.next().await?) })
			.await
	}

	/// Cancel all current and future `next()` calls.
	///
	/// Terminal: the subscription is released here, not when the handle is.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}
