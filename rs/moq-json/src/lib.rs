//! Snapshot/delta JSON publishing over [`moq-net`](moq_net) tracks.
//!
//! A JSON value is published over a track as a series of groups, where each group is
//! self-contained: its first frame is a full snapshot and any following frames are
//! [RFC 7396](https://www.rfc-editor.org/rfc/rfc7396.html) JSON Merge Patch deltas applied in
//! order. A consumer jumps to the newest group, reads the snapshot, and applies the deltas, so
//! a late joiner never needs older groups.
//!
//! Deltas are controlled by [`ProducerConfig::delta_ratio`]. A ratio of `0` disables them, so every
//! change is a fresh snapshot group, matching a plain "one JSON blob per group" track.

mod diff;

use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::Poll;

use bytes::Bytes;
use moq_flate::{Decoder, Encoder};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

pub use crate::diff::{Diff, diff};

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

	/// A compressed frame could not be decoded (malformed, truncated, or oversized).
	#[error(transparent)]
	Flate(#[from] moq_flate::Error),
}

impl From<serde_json::Error> for Error {
	fn from(err: serde_json::Error) -> Self {
		Error::Json(err.to_string())
	}
}

/// A [`Result`](std::result::Result) using this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Configuration for a [`Producer`].
///
/// Build from [`Default`] and override fields (the struct is `#[non_exhaustive]`, so new
/// options stay additive): `let mut config = ProducerConfig::default(); config.delta_ratio = 0;`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ProducerConfig {
	/// Controls how aggressively the producer emits deltas (merge patches) instead of full snapshots.
	///
	/// A ratio of `0` disables deltas: every change is published as a new snapshot group.
	///
	/// A positive ratio enables deltas. A new snapshot group is started once the deltas *already
	/// written* to the current group (excluding the snapshot frame) exceed `ratio` times the snapshot
	/// size. The pending delta is excluded from that check, so the one that first crosses the budget
	/// still lands before the group rolls. So `1` allows roughly one snapshot's worth of deltas before
	/// rolling, and a larger ratio tolerates more.
	///
	/// When [`compression`](Self::compression) is on, both sides of the comparison are measured on
	/// the *compressed* frame sizes (the real wire cost).
	///
	/// Defaults to `8`.
	pub delta_ratio: u32,

	/// Compress each group as one sync-flushed DEFLATE stream, so deltas reuse the snapshot as
	/// context and shrink sharply.
	///
	/// `false` (the default) writes plaintext JSON frames, identical on the wire to an uncompressed
	/// track. A [`Consumer`] reading the track must set [`ConsumerConfig::compression`] to match.
	pub compression: bool,
}

impl Default for ProducerConfig {
	fn default() -> Self {
		Self {
			delta_ratio: 8,
			compression: false,
		}
	}
}

/// Configuration for a [`Consumer`].
///
/// Build from [`Default`] and override fields (the struct is `#[non_exhaustive]`, so new options
/// stay additive).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ConsumerConfig {
	/// Whether the track's frames are DEFLATE-compressed. Must match the producer's
	/// [`ProducerConfig::compression`]. Defaults to `false`.
	pub compression: bool,
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
	pub fn new(track: moq_net::TrackProducer, config: ProducerConfig) -> Self {
		Self {
			inner: Arc::new(Mutex::new(Inner {
				track,
				group: None,
				encoder: None,
				last: None,
				delta_bytes: 0,
				snapshot_len: 0,
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
		self.inner.lock().unwrap().update(value)
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

		// We already hold the lock, so publish through the held guard rather than re-locking.
		let _ = self.inner.update(&self.value);
	}
}

/// Shared publishing state behind [`Producer`]'s `Arc<Mutex>`.
struct Inner {
	track: moq_net::TrackProducer,
	group: Option<moq_net::GroupProducer>,
	// Per-group DEFLATE encoder, `Some` while a compressed group is open (recreated per group).
	encoder: Option<Encoder>,
	last: Option<Value>,
	// Bytes of deltas accumulated in the current group, excluding the snapshot frame. Compressed
	// slice sizes when compressing, raw patch sizes otherwise.
	delta_bytes: u64,
	// Reference size the delta budget is measured against: the current group's snapshot frame.
	// Its compressed slice size when compressing, raw otherwise.
	snapshot_len: u64,
	group_frames: usize,
	config: ProducerConfig,
}

impl Inner {
	fn update<T: Serialize>(&mut self, value: &T) -> Result<()> {
		// The first publish (or the first after `finish`) has no baseline to diff against, so it seeds
		// the stream with a snapshot.
		let Some(last) = self.last.as_ref() else {
			return self.snapshot(value);
		};

		// Diff straight off `T`, without building a full `Value` for the new value first.
		let Diff { patch, forced_snapshot } = diff(last, value);

		// An empty object patch with no forced null means the value is unchanged: publish nothing.
		if !forced_snapshot && patch.as_object().is_some_and(serde_json::Map::is_empty) {
			return Ok(());
		}

		// A forced snapshot (a genuine null, or a non-object root) or an exhausted delta budget rolls a
		// new group; otherwise the change rides as a delta in the open group.
		if forced_snapshot || !self.delta_allowed() {
			return self.snapshot(value);
		}

		// Compress into the per-group window only now, for a frame we are committed to writing.
		let bytes = serde_json::to_vec(&patch)?;
		let slice = match self.encoder.as_mut() {
			Some(encoder) => encoder.frame(&bytes),
			None => Bytes::from(bytes),
		};
		let len = slice.len() as u64;
		self.group
			.as_mut()
			.expect("delta_allowed guarantees an open group")
			.write_frame_now(slice)?;
		self.delta_bytes += len;
		self.group_frames += 1;

		// Fold the delta into the baseline so the next diff is against the value we just published.
		json_patch::merge(self.last.as_mut().expect("a snapshot precedes any delta"), &patch);
		Ok(())
	}

	/// Whether the current change may ride as a delta in the open group.
	///
	/// The budget gate measures the deltas *already written* (excluding the frame about to land)
	/// against the group's snapshot frame. Both are compressed sizes when compressing and raw
	/// otherwise, so the comparison is like-for-like. Because the pending frame is excluded, the delta
	/// that tips the group past `ratio * snapshot` still lands: a group overshoots by at most one delta
	/// before rolling.
	fn delta_allowed(&self) -> bool {
		let ratio = self.config.delta_ratio as u64;
		ratio != 0
			&& self.group.is_some()
			&& self.group_frames < MAX_DELTA_FRAMES
			&& self.delta_bytes <= ratio * self.snapshot_len
	}

	/// Start a new group with a full snapshot of `value` as its first frame, and reseed the baseline.
	fn snapshot<T: Serialize>(&mut self, value: &T) -> Result<()> {
		// Serialize directly from `value` so the snapshot frame preserves the type's own field order,
		// keeping the wire bytes identical to serializing `T` straight to a frame.
		let snapshot = serde_json::to_vec(value)?;

		// The previous group is complete; no more frames will be appended to it.
		if let Some(mut group) = self.group.take() {
			group.finish()?;
		}

		let mut group = self.track.append_group()?;

		// Open a fresh per-group encoder (cold window) and compress the snapshot as frame 0, recording
		// its wire size as the delta anchor.
		let (slice, encoder) = if self.config.compression {
			let mut encoder = Encoder::new();
			let slice = encoder.frame(&snapshot);
			(slice, Some(encoder))
		} else {
			(Bytes::from(snapshot), None)
		};
		self.snapshot_len = slice.len() as u64;
		group.write_frame_now(slice)?;
		self.delta_bytes = 0;
		self.group_frames = 1;
		self.encoder = encoder;

		if self.config.delta_ratio != 0 {
			// Keep the group (and its encoder) open so future deltas can be appended.
			self.group = Some(group);
		} else {
			// Deltas disabled: one frame per group, identical to a plain JSON track.
			self.encoder = None;
			group.finish()?;
		}

		// Reseed the baseline with the full new value for the next diff.
		self.last = Some(serde_json::to_value(value)?);
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
	// Whether frames are DEFLATE-compressed, matching the producer's [`ProducerConfig::compression`].
	compressed: bool,
	// Per-group DEFLATE decoder, built lazily on the first compressed frame of a group.
	decoder: Option<Decoder>,
	current: Option<Value>,
	frames_read: usize,
	_marker: PhantomData<fn() -> T>,
}

impl<T: DeserializeOwned> Consumer<T> {
	/// Create a consumer reading from the given track subscriber.
	///
	/// Set [`ConsumerConfig::compression`] to read a track written by a producer with
	/// [`ProducerConfig::compression`] on.
	pub fn new(track: moq_net::TrackSubscriber, config: ConsumerConfig) -> Self {
		Self {
			track,
			group: None,
			compressed: config.compression,
			decoder: None,
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
	/// Jumps to the newest group, reads its snapshot, and applies deltas in order. All frames already
	/// buffered in the group are applied in one poll but only the resulting *latest* value is yielded:
	/// the intermediate reconstructions are stale, so a late joiner (or any consumer that has fallen
	/// behind) catches up to the head in a single step instead of replaying every superseded state.
	/// Frames must still be decoded in order (the DEFLATE window and merge patches are sequential);
	/// only the per-frame deserialize and yield are skipped. Switching to a newer group discards the
	/// older one.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<T>>> {
		// Drain to the newest group, resetting reconstruction state whenever we switch.
		let track_finished = loop {
			match self.track.poll_next_group(waiter)? {
				Poll::Ready(Some(group)) => {
					self.group = Some(group);
					self.current = None;
					self.frames_read = 0;
					// Each group is its own compressed stream, so reset the decoder state.
					self.decoder = None;
				}
				Poll::Ready(None) => break true,
				Poll::Pending => break false,
			}
		};

		// Apply every frame currently buffered in the group, tracking whether any moved us forward and
		// whether the group is still open with nothing buffered yet (vs. exhausted).
		// `poll_read_frame` returns an owned `Poll`, so the borrow of `self.group` ends before the
		// match arms, leaving `apply` (and clearing the group) free to take `&mut self`.
		let mut advanced = false;
		let mut group_pending = false;
		while let Some(group) = &mut self.group {
			match group.poll_read_frame(waiter)? {
				Poll::Ready(Some(frame)) => {
					self.apply(frame)?;
					advanced = true;
				}
				// The current group is exhausted; wait for a newer one.
				Poll::Ready(None) => {
					self.group = None;
					break;
				}
				// The group is still open but has nothing buffered yet.
				Poll::Pending => {
					group_pending = true;
					break;
				}
			}
		}

		if advanced {
			// Deserialize once, from the head of the backlog we just drained.
			return Poll::Ready(Ok(Some(self.reconstruct()?)));
		}

		// An open group may still deliver frames even after the track finishes (it was appended before
		// the finish), so wait on it rather than ending the stream.
		if group_pending {
			return Poll::Pending;
		}

		if track_finished {
			Poll::Ready(Ok(None))
		} else {
			Poll::Pending
		}
	}

	/// Decompress a frame slice, or pass it through when the track is uncompressed.
	///
	/// The per-group decoder is built lazily on the first compressed frame and advanced by every
	/// following frame, so the shared DEFLATE window carries across the group's snapshot and deltas.
	fn decode(&mut self, slice: Bytes) -> Result<Bytes> {
		if !self.compressed {
			return Ok(slice);
		}

		let decoder = self.decoder.get_or_insert_with(Decoder::new);
		Ok(decoder.frame(&slice)?)
	}

	/// Apply one frame to the in-progress value: frame 0 of a group is a snapshot, the rest are merge
	/// patches. Updates internal state only; call [`reconstruct`](Self::reconstruct) to materialize `T`.
	fn apply(&mut self, frame: Bytes) -> Result<()> {
		let frame = self.decode(frame)?;
		if self.frames_read == 0 {
			self.current = Some(serde_json::from_slice(&frame)?);
		} else {
			let patch: Value = serde_json::from_slice(&frame)?;
			let current = self.current.as_mut().expect("a snapshot precedes any delta");
			json_patch::merge(current, &patch);
		}
		self.frames_read += 1;
		Ok(())
	}

	/// Materialize the current reconstructed value into `T`. Call only after at least one frame has
	/// been applied in the current group.
	fn reconstruct(&self) -> Result<T> {
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

	/// An uncompressed config with the given delta ratio.
	fn cfg(delta_ratio: u32) -> ProducerConfig {
		ProducerConfig {
			delta_ratio,
			..Default::default()
		}
	}

	/// A DEFLATE-compressed config with the given delta ratio.
	fn cfg_deflate(delta_ratio: u32) -> ProducerConfig {
		ProducerConfig {
			delta_ratio,
			compression: true,
		}
	}

	/// A consumer reading compressed frames.
	fn deflate_consumer(track: moq_net::TrackSubscriber) -> Consumer<Value> {
		Consumer::new(track, ConsumerConfig { compression: true })
	}

	fn producer(config: ProducerConfig) -> (Producer<Value>, moq_net::TrackSubscriber) {
		let track = moq_net::BroadcastInfo::new()
			.produce()
			.create_track("test", None)
			.unwrap();
		let consumer = track.subscribe(None);
		(Producer::new(track, config), consumer)
	}

	/// Drain every value currently available from a plaintext consumer without blocking.
	fn drain(track: moq_net::TrackSubscriber) -> Vec<Value> {
		drain_with(Consumer::<Value>::new(track, ConsumerConfig::default()))
	}

	/// Drain every value currently available from an already-built consumer without blocking.
	fn drain_with(mut consumer: Consumer<Value>) -> Vec<Value> {
		let waiter = kio::Waiter::noop();
		let mut out = Vec::new();
		while let Poll::Ready(Ok(Some(value))) = consumer.poll_next(&waiter) {
			out.push(value);
		}
		out
	}

	#[test]
	fn deltas_off_snapshot_per_group() {
		let (mut producer, track) = producer(cfg(0));
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
		let (mut producer, track) = producer(ProducerConfig::default());
		let mut consumer = Consumer::<Value>::new(track, ConsumerConfig::default());
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
		let (mut producer, track) = producer(ProducerConfig::default());
		producer.update(&json!({ "a": 1 })).unwrap();
		producer.update(&json!({ "a": 1 })).unwrap();
		producer.finish().unwrap();

		assert_eq!(track.latest(), Some(0));
		assert_eq!(drain(track), vec![json!({ "a": 1 })]);
	}

	#[test]
	fn deltas_share_one_group() {
		let config = cfg(100);
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
		// A ratio of 1 budgets deltas up to one snapshot (equal 7-byte frames => 7 bytes). The gate
		// checks the deltas already written, so the delta that tips the group over budget still lands
		// (a one-frame overshoot): group 0 takes two deltas (14 bytes) before the fourth update rolls
		// group 1. (Still distinct from 0, which disables deltas entirely.)
		let config = cfg(1);
		let (mut producer, track) = producer(config);
		producer.update(&json!({ "a": 1 })).unwrap(); // snapshot, group 0
		producer.update(&json!({ "a": 2 })).unwrap(); // delta, group 0 (deltas = 7)
		producer.update(&json!({ "a": 3 })).unwrap(); // delta, group 0 (deltas = 14, now over budget)
		producer.update(&json!({ "a": 4 })).unwrap(); // budget already exceeded, rolls group 1
		producer.finish().unwrap();

		assert_eq!(track.latest(), Some(1));
	}

	#[test]
	fn deltas_stay_within_ratio_times_snapshot() {
		// The budget covers only the deltas, not the snapshot frame, measured against the group's
		// snapshot size. Single-digit values keep every frame at a constant 7 bytes (`{"n":N}`), so
		// `ratio = 8` budgets 56 bytes of deltas. The gate checks the deltas already written, so the
		// group keeps filling until the accumulated deltas first exceed 56 (nine deltas = 63 bytes) and
		// the next update rolls (a one-frame overshoot past the 56-byte budget).
		let config = cfg(8);
		let (mut producer, track) = producer(config);
		for n in 0..=10 {
			producer.update(&json!({ "n": n })).unwrap();
		}
		producer.finish().unwrap();

		// Group 0 carries the snapshot plus 9 deltas (10 frames); the 10th delta opens group 1.
		assert_eq!(track.latest(), Some(1));
		assert_eq!(drain(track).last().unwrap(), &json!({ "n": 10 }));
	}

	#[test]
	fn array_change_is_delta() {
		let config = cfg(100);
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
		let config = cfg(1_000_000);
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
		let config = cfg(100);
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

		let track = moq_net::BroadcastInfo::new()
			.produce()
			.create_track("test", None)
			.unwrap();
		let consumer = track.subscribe(None);
		let mut producer = Producer::<Doc>::new(track, ProducerConfig::default());

		// First owner sets its field.
		producer.lock().video = Some("v1".to_string());

		// Second owner starts from the latest value and adds its own field without clobbering.
		producer.lock().scte35 = Some(42);

		// Locking without mutating publishes nothing (the guard stays clean).
		let _ = producer.lock();

		producer.finish().unwrap();

		let mut consumer = Consumer::<Doc>::new(consumer, ConsumerConfig::default());
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
		// A tight ratio fills group 0 with a couple of deltas, then forces a later update into a new
		// snapshot group (the gate overshoots the budget by one delta before rolling).
		let config = cfg(1);
		let (mut producer, track) = producer(config);
		let observer = producer.consume();
		let mut consumer = Consumer::<Value>::new(track, ConsumerConfig::default());
		let waiter = kio::Waiter::noop();

		producer.update(&json!({ "a": 1 })).unwrap(); // snapshot, group 0
		match consumer.poll_next(&waiter) {
			Poll::Ready(Ok(Some(value))) => assert_eq!(value, json!({ "a": 1 })),
			other => panic!("expected first value, got {other:?}"),
		}

		producer.update(&json!({ "a": 2 })).unwrap(); // delta in group 0 (deltas = 7)
		producer.update(&json!({ "a": 3 })).unwrap(); // delta in group 0 (deltas = 14, now over budget)
		producer.update(&json!({ "a": 4 })).unwrap(); // budget already exceeded, rolls group 1
		producer.finish().unwrap();
		assert_eq!(observer.latest(), Some(1));

		// The consumer jumps to the newest group and never yields a stale value.
		let mut last = None;
		while let Poll::Ready(Ok(Some(value))) = consumer.poll_next(&waiter) {
			last = Some(value);
		}
		assert_eq!(last.unwrap(), json!({ "a": 4 }));
	}

	#[test]
	fn open_group_pends_after_track_finish() {
		// A group appended before the track finishes may still deliver frames, so the consumer must
		// keep waiting on it rather than ending the stream. Regression for the backlog-collapse poll.
		let mut track = moq_net::BroadcastInfo::new()
			.produce()
			.create_track("test", None)
			.unwrap();
		let mut group = track.append_group().unwrap();
		let consumer_track = track.subscribe(None);
		track.finish().unwrap();

		let mut consumer = Consumer::<Value>::new(consumer_track, ConsumerConfig::default());
		let waiter = kio::Waiter::noop();

		// Track is finished but the open group is empty: pending, not end-of-stream.
		assert!(matches!(consumer.poll_next(&waiter), Poll::Pending));

		group
			.write_frame_now(Bytes::from(serde_json::to_vec(&json!({ "a": 1 })).unwrap()))
			.unwrap();
		group.finish().unwrap();

		match consumer.poll_next(&waiter) {
			Poll::Ready(Ok(Some(value))) => assert_eq!(value, json!({ "a": 1 })),
			other => panic!("expected the catalog value, got {other:?}"),
		}
	}

	#[test]
	fn late_joiner_collapses_backlog_to_latest() {
		// A whole group's worth of snapshot + deltas is buffered before the consumer reads. It should
		// apply them all but yield only the latest value once, not replay every superseded state.
		let (mut producer, track) = producer(cfg(100));
		for n in 0..=20 {
			producer.update(&json!({ "n": n })).unwrap();
		}
		producer.finish().unwrap();

		// One group (ratio is generous), so a single poll drains the backlog into one yield.
		assert_eq!(track.latest(), Some(0));
		let values = drain(track);
		assert_eq!(
			values,
			vec![json!({ "n": 20 })],
			"backlog should collapse to the latest value"
		);
	}

	#[test]
	fn compressed_late_joiner_collapses_backlog_to_latest() {
		// Same collapse, exercising the lazy decoder replaying the group's slices to warm its window.
		let (mut producer, track) = producer(cfg_deflate(100));
		for n in 0..=20 {
			producer.update(&json!({ "n": n })).unwrap();
		}
		producer.finish().unwrap();

		assert_eq!(track.latest(), Some(0));
		let values = drain_with(deflate_consumer(track));
		assert_eq!(
			values,
			vec![json!({ "n": 20 })],
			"compressed backlog should collapse to the latest"
		);
	}

	#[test]
	fn compressed_snapshot_per_group_roundtrips() {
		let (mut producer, track) = producer(cfg_deflate(0));
		producer.update(&json!({ "a": 1 })).unwrap();
		producer.update(&json!({ "a": 2 })).unwrap();
		producer.finish().unwrap();

		// Deltas disabled: one compressed snapshot per group, latest reconstructs identically.
		assert_eq!(track.latest(), Some(1));
		let values = drain_with(deflate_consumer(track));
		assert_eq!(values, vec![json!({ "a": 2 })]);
	}

	#[test]
	fn compressed_deltas_share_one_group() {
		let (mut producer, track) = producer(cfg_deflate(100));
		producer.update(&json!({ "a": 1, "b": 1 })).unwrap();
		producer.update(&json!({ "a": 1, "b": 2 })).unwrap();
		producer.update(&json!({ "a": 1, "b": 3 })).unwrap();
		producer.finish().unwrap();

		// Snapshot + deltas in one group, each frame decompressed against the shared window.
		assert_eq!(track.latest(), Some(0));
		let values = drain_with(deflate_consumer(track));
		assert_eq!(values.last().unwrap(), &json!({ "a": 1, "b": 3 }));
	}

	#[test]
	fn compressed_late_joiner_reconstructs_from_deltas() {
		let (mut producer, track) = producer(cfg_deflate(100));
		producer.update(&json!({ "a": 1, "b": 1 })).unwrap();
		producer.update(&json!({ "a": 1, "b": 2 })).unwrap();
		producer.update(&json!({ "a": 5, "b": 2 })).unwrap();
		producer.finish().unwrap();

		// A consumer created only now rebuilds the final value from the compressed snapshot + deltas.
		let values = drain_with(deflate_consumer(track));
		assert_eq!(values.last().unwrap(), &json!({ "a": 5, "b": 2 }));
	}

	#[test]
	fn compressed_deltas_roll_on_compressed_budget() {
		// With compression the budget is measured on compressed frame sizes: `snapshot_len` and
		// `delta_bytes` are the compressed slice lengths, not the raw JSON. A tight ratio over many
		// distinct updates must therefore roll at least one group, and a late joiner must still rebuild
		// the final value across the compressed group boundary (per-group decoder reset). Guards against
		// the budget regressing to raw lengths.
		let (mut producer, track) = producer(cfg_deflate(2));
		for n in 0..=40 {
			producer.update(&json!({ "n": n })).unwrap();
		}
		producer.finish().unwrap();

		assert!(
			track.latest().unwrap() > 0,
			"a tight ratio should roll at least one compressed group"
		);
		assert_eq!(drain_with(deflate_consumer(track)).last().unwrap(), &json!({ "n": 40 }));
	}

	#[test]
	fn compression_shrinks_wire_frames() {
		// A repetitive payload should serialize to fewer wire bytes compressed than plaintext.
		let value = json!({ "renditions": ["video".repeat(50), "video".repeat(50), "video".repeat(50)] });

		let plaintext_bytes = wire_frame_len(cfg(0), &value);
		let compressed_bytes = wire_frame_len(cfg_deflate(0), &value);
		assert!(
			compressed_bytes < plaintext_bytes,
			"compressed frame {compressed_bytes} should be smaller than plaintext {plaintext_bytes}"
		);
	}

	#[test]
	fn compressed_deltas_reuse_window() {
		// The shared per-group window is the whole point: a delta that restates content already in
		// the snapshot compresses to far fewer bytes than the raw patch.
		let (mut producer, mut track) = producer(cfg_deflate(100));
		let phrase = "Media over QUIC delivers real-time latency at massive scale";
		producer.update(&json!({ "note": phrase })).unwrap();
		producer.update(&json!({ "note": phrase, "echo": phrase })).unwrap();
		producer.finish().unwrap();

		// Both frames land in group 0; read the delta (frame 1) verbatim.
		let waiter = kio::Waiter::noop();
		let Poll::Ready(Ok(Some(mut group))) = track.poll_next_group(&waiter) else {
			panic!("expected a group");
		};
		let mut frames = Vec::new();
		while let Poll::Ready(Ok(Some(frame))) = group.poll_read_frame(&waiter) {
			frames.push(frame);
		}
		assert_eq!(frames.len(), 2, "snapshot + one delta in a single group");

		// The raw patch repeats the whole phrase; compressed against the window it's a fraction.
		let raw_delta = serde_json::to_vec(&json!({ "echo": phrase })).unwrap();
		assert!(
			frames[1].len() < raw_delta.len() / 2,
			"windowed delta {} should be far below the raw patch {}",
			frames[1].len(),
			raw_delta.len()
		);
	}

	/// Publish a single value and return the byte length of the resulting (frame 0) wire frame.
	fn wire_frame_len(config: ProducerConfig, value: &Value) -> usize {
		let (mut producer, mut track) = producer(config);
		producer.update(value).unwrap();
		producer.finish().unwrap();

		let waiter = kio::Waiter::noop();
		let Poll::Ready(Ok(Some(mut group))) = track.poll_next_group(&waiter) else {
			panic!("expected a group");
		};
		// Read the stored (possibly compressed) frame bytes verbatim, without reconstructing JSON.
		let Poll::Ready(Ok(Some(frame))) = group.poll_read_frame(&waiter) else {
			panic!("expected a frame");
		};
		frame.len()
	}
}
