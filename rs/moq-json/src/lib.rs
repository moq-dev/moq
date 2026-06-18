//! Snapshot/delta JSON publishing over [`moq-net`](moq_net) tracks.
//!
//! A JSON value is published over a track as a series of groups, where each group is
//! self-contained: its first frame is a full snapshot and any following frames are
//! [RFC 7396](https://www.rfc-editor.org/rfc/rfc7396.html) JSON Merge Patch deltas applied in
//! order. A consumer jumps to the newest group, reads the snapshot, and applies the deltas, so
//! a late joiner never needs older groups.
//!
//! Deltas are controlled by [`Config::delta_ratio`]. A ratio of `0` disables them, so every
//! change is a fresh snapshot group, matching a plain "one JSON blob per group" track.

mod diff;

use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::Poll;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::diff::diff;

/// Maximum frames (snapshot + deltas) in a single group before a new snapshot is forced.
///
/// Kept well below moq-net's per-group frame cap so a late joiner can always read the snapshot
/// at frame 0 before the group is evicted.
const MAX_DELTA_FRAMES: usize = 256;

/// Errors produced while publishing or consuming JSON.
#[derive(thiserror::Error, Debug, Clone)]
#[non_exhaustive]
pub enum Error {
	/// An error from the underlying track.
	#[error(transparent)]
	Net(#[from] moq_net::Error),

	/// A value failed to serialize, deserialize, or apply as a merge patch.
	///
	/// Stored as a string since [`serde_json::Error`] is not [`Clone`].
	#[error("json: {0}")]
	Json(String),
}

impl From<serde_json::Error> for Error {
	fn from(err: serde_json::Error) -> Self {
		Error::Json(err.to_string())
	}
}

/// A [`Result`](std::result::Result) using this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Configuration for a [`Producer`].
#[derive(Debug, Clone)]
pub struct Config {
	/// Controls how aggressively the producer emits deltas (merge patches) instead of full snapshots.
	///
	/// A ratio of `0` disables deltas: every change is published as a new snapshot group.
	///
	/// A positive ratio enables deltas. A delta is appended to the current group as long as the
	/// accumulated deltas (excluding the snapshot frame) stay within `ratio` times the size of a
	/// fresh snapshot; otherwise a new snapshot group is started. So `1` allows deltas totalling up
	/// to one snapshot before rolling, and a larger ratio tolerates more deltas per snapshot.
	///
	/// Defaults to `8`.
	pub delta_ratio: u32,
}

impl Default for Config {
	fn default() -> Self {
		Self { delta_ratio: 8 }
	}
}

/// Publishes a JSON value over a track, choosing snapshots and deltas automatically.
///
/// Cheaply clonable: clones share one underlying track and publishing state, like other MoQ
/// producers.
pub struct Producer<T> {
	inner: Arc<Mutex<Inner>>,
	_marker: PhantomData<fn(T)>,
}

impl<T> Clone for Producer<T> {
	fn clone(&self) -> Self {
		Self {
			inner: self.inner.clone(),
			_marker: PhantomData,
		}
	}
}

impl<T> Producer<T> {
	/// Create a subscriber for the underlying track.
	pub fn consume(&self) -> moq_net::TrackSubscriber {
		self.inner.lock().unwrap().track.subscribe(None)
	}
}

impl<T: Serialize> Producer<T> {
	/// Create a producer that publishes to the given track.
	pub fn new(track: moq_net::TrackProducer, config: Config) -> Self {
		Self {
			inner: Arc::new(Mutex::new(Inner {
				track,
				group: None,
				last: None,
				delta_bytes: 0,
				group_frames: 0,
				config,
			})),
			_marker: PhantomData,
		}
	}

	/// Publish a new value, emitting a snapshot or a delta automatically.
	///
	/// Does nothing if the value is unchanged from the previous publish.
	pub fn update(&mut self, value: &T) -> Result<()> {
		let json = serde_json::to_value(value)?;
		// Serialize the value directly (not via `json`) so a snapshot preserves the type's own
		// field order, keeping the wire bytes identical to serializing `T` straight to a frame.
		let snapshot = serde_json::to_vec(value)?;
		self.inner.lock().unwrap().update(json, snapshot)
	}

	/// Lock the current value for in-place editing, publishing on drop.
	///
	/// The returned [`Guard`] derefs to the last-published value (or `T::default()` if nothing has
	/// been published yet). Editing it through [`DerefMut`] marks the guard dirty; when a dirty
	/// guard drops it publishes the result, a no-op if unchanged.
	///
	/// This is the counterpart to a callback: hold the guard, mutate, drop. The guard holds the
	/// producer's lock for its lifetime, so independent owners are serialized: each one starts from
	/// the latest value and their changes compose instead of clobbering. Don't hold a guard across
	/// an `.await`, since that keeps the lock held while suspended.
	pub fn lock(&mut self) -> Guard<'_, T>
	where
		T: Default + DeserializeOwned,
	{
		let inner = self.inner.lock().unwrap();
		let value = inner
			.last
			.as_ref()
			.and_then(|last| serde_json::from_value(last.clone()).ok())
			.unwrap_or_default();

		Guard {
			inner,
			value,
			dirty: false,
		}
	}

	/// Finish the track, closing any open group.
	pub fn finish(&mut self) -> Result<()> {
		self.inner.lock().unwrap().finish()
	}
}

/// An RAII editing guard returned by [`Producer::lock`].
///
/// Holds the producer's lock for its lifetime and derefs to the current value. Mutating it through
/// [`DerefMut`] marks it dirty, and dropping a dirty guard publishes the edited value.
pub struct Guard<'a, T: Serialize> {
	inner: MutexGuard<'a, Inner>,
	value: T,
	dirty: bool,
}

impl<T: Serialize> Deref for Guard<'_, T> {
	type Target = T;

	fn deref(&self) -> &T {
		&self.value
	}
}

impl<T: Serialize> DerefMut for Guard<'_, T> {
	fn deref_mut(&mut self) -> &mut T {
		self.dirty = true;
		&mut self.value
	}
}

impl<T: Serialize> Drop for Guard<'_, T> {
	fn drop(&mut self) {
		if !self.dirty {
			return;
		}

		let Ok(json) = serde_json::to_value(&self.value) else {
			return;
		};
		let Ok(snapshot) = serde_json::to_vec(&self.value) else {
			return;
		};

		// We already hold the lock, so publish through the held guard rather than re-locking.
		let _ = self.inner.update(json, snapshot);
	}
}

/// Shared publishing state behind [`Producer`]'s `Arc<Mutex>`.
struct Inner {
	track: moq_net::TrackProducer,
	group: Option<moq_net::GroupProducer>,
	last: Option<Value>,
	// Bytes of deltas accumulated in the current group, excluding the snapshot frame.
	delta_bytes: u64,
	group_frames: usize,
	config: Config,
}

impl Inner {
	fn update(&mut self, json: Value, snapshot: Vec<u8>) -> Result<()> {
		if self.last.as_ref() == Some(&json) {
			return Ok(());
		}

		match self.delta(&json, snapshot.len())? {
			Some(delta) => {
				let group = self.group.as_mut().expect("delta requires an open group");
				let len = delta.len() as u64;
				group.write_frame(delta)?;
				self.delta_bytes += len;
				self.group_frames += 1;
			}
			None => self.snapshot(snapshot)?,
		}

		self.last = Some(json);
		Ok(())
	}

	/// Serialize a delta if deltas are enabled and appending one keeps the group within budget;
	/// otherwise `None`, signalling that a fresh snapshot should be published instead.
	fn delta(&self, value: &Value, snapshot_len: usize) -> Result<Option<Vec<u8>>> {
		let ratio = self.config.delta_ratio;
		if ratio == 0 {
			return Ok(None);
		}
		let Some(last) = &self.last else {
			return Ok(None);
		};
		if self.group.is_none() || self.group_frames >= MAX_DELTA_FRAMES {
			return Ok(None);
		}

		let diff = diff(last, value);
		if diff.forced_snapshot {
			return Ok(None);
		}

		let delta = serde_json::to_vec(&diff.patch)?;

		// Roll a snapshot once the deltas would outgrow the budget (snapshot frame excluded).
		let projected = self.delta_bytes + delta.len() as u64;
		if projected > ratio as u64 * snapshot_len as u64 {
			return Ok(None);
		}

		Ok(Some(delta))
	}

	/// Start a new group with a full snapshot as its first frame.
	fn snapshot(&mut self, snapshot: Vec<u8>) -> Result<()> {
		// The previous group is complete; no more frames will be appended to it.
		if let Some(mut group) = self.group.take() {
			group.finish()?;
		}

		let mut group = self.track.append_group()?;
		group.write_frame(snapshot)?;
		self.delta_bytes = 0;
		self.group_frames = 1;

		if self.config.delta_ratio != 0 {
			// Keep the group open so future deltas can be appended.
			self.group = Some(group);
		} else {
			// Deltas disabled: one frame per group, identical to a plain JSON track.
			group.finish()?;
		}

		Ok(())
	}

	fn finish(&mut self) -> Result<()> {
		if let Some(mut group) = self.group.take() {
			group.finish()?;
		}
		self.track.finish()?;
		Ok(())
	}
}

/// Consumes a JSON value from a track, reconstructing it from snapshots and deltas.
pub struct Consumer<T> {
	track: moq_net::TrackSubscriber,
	group: Option<moq_net::GroupConsumer>,
	current: Option<Value>,
	frames_read: usize,
	_marker: PhantomData<fn() -> T>,
}

impl<T: DeserializeOwned> Consumer<T> {
	/// Create a consumer reading from the given track subscriber.
	pub fn new(track: moq_net::TrackSubscriber) -> Self {
		Self {
			track,
			group: None,
			current: None,
			frames_read: 0,
			_marker: PhantomData,
		}
	}

	/// Get the next reconstructed value, or `None` once the track ends.
	pub async fn next(&mut self) -> Result<Option<T>>
	where
		T: Unpin,
	{
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Poll for the next reconstructed value, without blocking.
	///
	/// Jumps to the newest group, reads its snapshot, and applies deltas in order, yielding the
	/// reconstructed value after each frame. Switching to a newer group discards the older one.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<T>>> {
		// Drain to the newest group, resetting reconstruction state whenever we switch.
		let track_finished = loop {
			match self.track.poll_next_group(waiter)? {
				Poll::Ready(Some(group)) => {
					self.group = Some(group);
					self.current = None;
					self.frames_read = 0;
				}
				Poll::Ready(None) => break true,
				Poll::Pending => break false,
			}
		};

		if let Some(group) = &mut self.group {
			match group.poll_read_frame(waiter)? {
				Poll::Ready(Some(frame)) => return Poll::Ready(Ok(Some(self.apply(frame)?))),
				// The current group is exhausted; wait for a newer one.
				Poll::Ready(None) => self.group = None,
				Poll::Pending => return Poll::Pending,
			}
		}

		if track_finished {
			Poll::Ready(Ok(None))
		} else {
			Poll::Pending
		}
	}

	/// Apply one frame: frame 0 of a group is a snapshot, the rest are merge patches.
	fn apply(&mut self, frame: bytes::Bytes) -> Result<T> {
		if self.frames_read == 0 {
			self.current = Some(serde_json::from_slice(&frame)?);
		} else {
			let patch: Value = serde_json::from_slice(&frame)?;
			let current = self.current.as_mut().expect("a snapshot precedes any delta");
			json_patch::merge(current, &patch);
		}
		self.frames_read += 1;

		let current = self
			.current
			.as_ref()
			.expect("a value is present after applying a frame");
		Ok(serde_json::from_value(current.clone())?)
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use serde_json::json;

	fn producer(config: Config) -> (Producer<Value>, moq_net::TrackSubscriber) {
		let track = moq_net::TrackProducer::new("test", None);
		let consumer = track.subscribe(None);
		(Producer::new(track, config), consumer)
	}

	/// Drain every value currently available from a consumer without blocking.
	fn drain(track: moq_net::TrackSubscriber) -> Vec<Value> {
		let mut consumer = Consumer::<Value>::new(track);
		let waiter = kio::Waiter::noop();
		let mut out = Vec::new();
		while let Poll::Ready(Ok(Some(value))) = consumer.poll_next(&waiter) {
			out.push(value);
		}
		out
	}

	#[test]
	fn deltas_off_snapshot_per_group() {
		let (mut producer, track) = producer(Config { delta_ratio: 0 });
		producer.update(&json!({ "a": 1 })).unwrap();
		producer.update(&json!({ "a": 2 })).unwrap();
		producer.finish().unwrap();

		// Two updates => two groups, each a full snapshot. A consumer that joins after both
		// exist only sees the latest, like the existing catalog consumer.
		assert_eq!(track.latest(), Some(1));
		assert_eq!(drain(track), vec![json!({ "a": 2 })]);
	}

	#[test]
	fn live_consumer_sees_each_update() {
		let (mut producer, track) = producer(Config::default());
		let mut consumer = Consumer::<Value>::new(track);
		let waiter = kio::Waiter::noop();

		for n in 1..=3 {
			producer.update(&json!({ "a": n })).unwrap();
			match consumer.poll_next(&waiter) {
				Poll::Ready(Ok(Some(value))) => assert_eq!(value, json!({ "a": n })),
				other => panic!("expected value, got {other:?}"),
			}
		}
	}

	#[test]
	fn unchanged_value_writes_nothing() {
		let (mut producer, track) = producer(Config::default());
		producer.update(&json!({ "a": 1 })).unwrap();
		producer.update(&json!({ "a": 1 })).unwrap();
		producer.finish().unwrap();

		assert_eq!(track.latest(), Some(0));
		assert_eq!(drain(track), vec![json!({ "a": 1 })]);
	}

	#[test]
	fn deltas_share_one_group() {
		let config = Config { delta_ratio: 100 };
		let (mut producer, track) = producer(config);
		producer.update(&json!({ "a": 1, "b": 1 })).unwrap();
		producer.update(&json!({ "a": 1, "b": 2 })).unwrap();
		producer.update(&json!({ "a": 1, "b": 3 })).unwrap();
		producer.finish().unwrap();

		// All updates fit in a single group as snapshot + deltas.
		assert_eq!(track.latest(), Some(0));
		let values = drain(track);
		assert_eq!(values.last().unwrap(), &json!({ "a": 1, "b": 3 }));
	}

	#[test]
	fn tight_ratio_rolls_snapshots() {
		// A ratio of 1 admits deltas only up to the snapshot size: with equal 7-byte frames that is a
		// single delta per group, so it rolls every other update. (Still distinct from 0, which would
		// disable deltas entirely and never produce the group-0 delta below.)
		let config = Config { delta_ratio: 1 };
		let (mut producer, track) = producer(config);
		producer.update(&json!({ "a": 1 })).unwrap(); // snapshot, group 0
		producer.update(&json!({ "a": 2 })).unwrap(); // delta, group 0
		producer.update(&json!({ "a": 3 })).unwrap(); // exceeds budget, rolls group 1
		producer.update(&json!({ "a": 4 })).unwrap(); // delta, group 1
		producer.finish().unwrap();

		assert_eq!(track.latest(), Some(1));
	}

	#[test]
	fn deltas_stay_within_ratio_times_snapshot() {
		// The budget covers only the deltas, not the snapshot frame, measured against the current
		// snapshot size. Single-digit values keep every frame at a constant 7 bytes (`{"n":N}`), so a
		// `ratio = 8` admits 8 deltas (8x the snapshot, the inclusive limit) on top of the snapshot
		// before the 9th delta rolls.
		let config = Config { delta_ratio: 8 };
		let (mut producer, track) = producer(config);
		for n in 0..=9 {
			producer.update(&json!({ "n": n })).unwrap();
		}
		producer.finish().unwrap();

		// Group 0 carries the snapshot plus 8 deltas (9 frames); the 9th delta opens group 1.
		assert_eq!(track.latest(), Some(1));
		assert_eq!(drain(track).last().unwrap(), &json!({ "n": 9 }));
	}

	#[test]
	fn array_change_is_delta() {
		let config = Config { delta_ratio: 100 };
		let (mut producer, track) = producer(config);
		producer.update(&json!({ "list": [1, 2] })).unwrap();
		producer.update(&json!({ "list": [1, 2, 3] })).unwrap();
		producer.finish().unwrap();

		// The array is replaced wholesale in a delta, so it stays in the same group.
		assert_eq!(track.latest(), Some(0));
		assert_eq!(drain(track).last().unwrap(), &json!({ "list": [1, 2, 3] }));
	}

	#[test]
	fn frame_cap_rolls_snapshot() {
		let config = Config { delta_ratio: 1_000_000 };
		let (mut producer, track) = producer(config);
		// First update is the snapshot (frame 0); then MAX_DELTA_FRAMES - 1 deltas fill the group.
		for i in 0..=MAX_DELTA_FRAMES {
			producer.update(&json!({ "n": i })).unwrap();
		}
		producer.finish().unwrap();

		// The frame cap forced exactly one extra snapshot group despite the huge ratio.
		assert_eq!(track.latest(), Some(1));
		assert_eq!(drain(track).last().unwrap(), &json!({ "n": MAX_DELTA_FRAMES }));
	}

	#[test]
	fn late_joiner_reconstructs_from_deltas() {
		let config = Config { delta_ratio: 100 };
		let (mut producer, track) = producer(config);
		producer.update(&json!({ "a": 1, "b": 1 })).unwrap();
		producer.update(&json!({ "a": 1, "b": 2 })).unwrap();
		producer.update(&json!({ "a": 5, "b": 2 })).unwrap();
		producer.finish().unwrap();

		// A consumer created only now still rebuilds the final value from snapshot + deltas.
		assert_eq!(drain(track).last().unwrap(), &json!({ "a": 5, "b": 2 }));
	}

	#[test]
	fn lock_composes_independent_owners() {
		// Mirrors the catalog use case: separate owners each edit their own field through the guard.
		#[derive(serde::Serialize, serde::Deserialize, Default, PartialEq, Debug)]
		struct Doc {
			#[serde(skip_serializing_if = "Option::is_none")]
			video: Option<String>,
			#[serde(skip_serializing_if = "Option::is_none")]
			scte35: Option<u32>,
		}

		let track = moq_net::TrackProducer::new("test", None);
		let consumer = track.subscribe(None);
		let mut producer = Producer::<Doc>::new(track, Config::default());

		// First owner sets its field.
		producer.lock().video = Some("v1".to_string());

		// Second owner starts from the latest value and adds its own field without clobbering.
		producer.lock().scte35 = Some(42);

		// Locking without mutating publishes nothing (the guard stays clean).
		let _ = producer.lock();

		producer.finish().unwrap();

		let mut consumer = Consumer::<Doc>::new(consumer);
		let waiter = kio::Waiter::noop();
		let mut last = None;
		while let Poll::Ready(Ok(Some(value))) = consumer.poll_next(&waiter) {
			last = Some(value);
		}
		assert_eq!(
			last.unwrap(),
			Doc {
				video: Some("v1".to_string()),
				scte35: Some(42),
			}
		);
	}

	#[test]
	fn newer_group_supersedes_in_progress_reconstruction() {
		// A tight ratio lets one delta fit, then forces the next update into a new snapshot group.
		let config = Config { delta_ratio: 1 };
		let (mut producer, track) = producer(config);
		let observer = producer.consume();
		let mut consumer = Consumer::<Value>::new(track);
		let waiter = kio::Waiter::noop();

		producer.update(&json!({ "a": 1 })).unwrap(); // snapshot, group 0
		match consumer.poll_next(&waiter) {
			Poll::Ready(Ok(Some(value))) => assert_eq!(value, json!({ "a": 1 })),
			other => panic!("expected first value, got {other:?}"),
		}

		producer.update(&json!({ "a": 2 })).unwrap(); // delta in group 0
		producer.update(&json!({ "a": 3 })).unwrap(); // exceeds budget, rolls group 1
		producer.finish().unwrap();
		assert_eq!(observer.latest(), Some(1));

		// The consumer jumps to the newest group and never yields a stale value.
		let mut last = None;
		while let Poll::Ready(Ok(Some(value))) = consumer.poll_next(&waiter) {
			last = Some(value);
		}
		assert_eq!(last.unwrap(), json!({ "a": 3 }));
	}
}
