//! The track-free half of snapshot publishing: values in, frame payloads out.

use std::marker::PhantomData;

use bytes::Bytes;
use serde::Serialize;
use serde_json::Value;

use crate::{Diff, Result, diff};

/// Maximum frames (snapshot + deltas) in a single group before a new snapshot is forced.
///
/// Kept well below moq-net's per-group frame cap so a late joiner can always read the snapshot
/// at frame 0 before the group is evicted.
pub(super) const MAX_DELTA_FRAMES: usize = 256;

/// Configuration for an [`Encoder`], and so for the [`Producer`](super::Producer) wrapping one.
///
/// Build from [`Default`] and override fields (the struct is `#[non_exhaustive]`, so new
/// options stay additive), or chain the `with_*` setters.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ProducerConfig {
	/// Controls how aggressively the encoder emits deltas (merge patches) instead of full snapshots.
	///
	/// A ratio of `0` disables deltas: every change is encoded as a new snapshot.
	///
	/// A positive ratio enables deltas. A new snapshot is emitted once the deltas *already written*
	/// to the current group (excluding the snapshot frame) exceed `ratio` times the snapshot size.
	/// The pending delta is excluded from that check, so the one that first crosses the budget
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
	/// `false` (the default) emits plaintext JSON frames, identical on the wire to an uncompressed
	/// track. A [`Decoder`](super::Decoder) reading them must set
	/// [`ConsumerConfig::compression`](super::ConsumerConfig::compression) to match.
	pub compression: bool,
}

impl ProducerConfig {
	/// Set [`delta_ratio`](Self::delta_ratio) (a builder, since the struct is `#[non_exhaustive]`).
	pub fn with_delta_ratio(mut self, delta_ratio: u32) -> Self {
		self.delta_ratio = delta_ratio;
		self
	}

	/// Set [`compression`](Self::compression) (a builder, since the struct is `#[non_exhaustive]`).
	pub fn with_compression(mut self, compression: bool) -> Self {
		self.compression = compression;
		self
	}
}

impl Default for ProducerConfig {
	fn default() -> Self {
		Self {
			delta_ratio: 8,
			compression: false,
		}
	}
}

/// One encoded frame, and the group boundary it implies.
#[derive(Clone, Debug)]
pub struct Encoded {
	/// The frame payload, DEFLATE-compressed when [`ProducerConfig::compression`] is set.
	pub payload: Bytes,

	/// Whether this frame is a full snapshot, which must open a new group.
	///
	/// `true` means the caller writes it as the first frame of a fresh group; `false` means it is a
	/// merge patch that must be appended to the group the last snapshot opened. Mapping straight onto
	/// [`moq_mux::container::Frame::keyframe`] is the point of the name.
	///
	/// The encoder decides this, never the caller: a value that sets a field to JSON null, or whose
	/// root isn't an object, cannot be expressed as a merge patch at all, and the delta budget and
	/// frame cap force a snapshot independently of what the caller wanted.
	///
	/// [`moq_mux::container::Frame::keyframe`]: https://docs.rs/moq-mux/latest/moq_mux/container/struct.Frame.html
	pub keyframe: bool,
}

/// An encoded frame the caller has not yet acknowledged writing.
///
/// Returned by [`Encoder::update`]. Read [`payload`](Encoded::payload) and
/// [`keyframe`](Encoded::keyframe) through the [`Deref`](std::ops::Deref) to [`Encoded`], write the
/// frame, then [`commit`](Self::commit).
///
/// Dropping it uncommitted [`Encoder::reset`]s, so a frame that never reached the wire leaves the
/// encoder resynchronizing with a fresh snapshot rather than emitting deltas against a baseline no
/// consumer received. Note that this is a recovery, not a rollback: producing a delta payload
/// advances the group's DEFLATE window, and that can't be undone, so a snapshot is the only sound
/// way back. Forgetting to commit a frame that *was* written is therefore merely wasteful (one
/// redundant snapshot), never incorrect.
#[must_use = "the frame must be written and committed, or dropped to resynchronize the encoder"]
pub struct Pending<'a, T> {
	encoder: &'a mut Encoder<T>,
	encoded: Encoded,
	committed: bool,
}

impl<T> Pending<'_, T> {
	/// Acknowledge that the frame reached the wire, keeping the encoder's state.
	///
	/// Only call this once the write has actually succeeded. Committing a frame that failed to write
	/// is the one thing that corrupts the stream.
	pub fn commit(mut self) {
		self.committed = true;
	}
}

impl<T> std::ops::Deref for Pending<'_, T> {
	type Target = Encoded;

	fn deref(&self) -> &Encoded {
		&self.encoded
	}
}

impl<T> Drop for Pending<'_, T> {
	fn drop(&mut self) {
		if !self.committed {
			self.encoder.reset();
		}
	}
}

/// Encodes a JSON value into frame payloads, choosing snapshots and deltas automatically.
///
/// The track-free core of [`Producer`](super::Producer): it decides *what bytes go in a frame* and
/// *where the group boundaries fall*, and leaves writing them to the caller. Reach for it when
/// something else already owns the track, for example a
/// [`moq_mux::container::Producer`](https://docs.rs/moq-mux/latest/moq_mux/container/struct.Producer.html)
/// that is also managing a timeline and a catalog estimate:
///
/// ```ignore
/// if let Some(frame) = encoder.update(&value)? {
///     container.write(moq_mux::container::Frame {
///         timestamp,
///         duration: None,
///         payload: frame.payload.clone(),
///         keyframe: frame.keyframe,
///     })?; // an early return here drops `frame`, resetting the encoder
///     frame.commit();
/// }
/// ```
///
/// Frames must reach the wire in the order they were encoded, and a frame with
/// [`keyframe`](Encoded::keyframe) set must open a new group: both the merge patches and the
/// group-scoped DEFLATE window depend on it. [`update`](Self::update) hands back a [`Pending`]
/// rather than a bare [`Encoded`] so a frame that never reaches the wire can't silently desync the
/// encoder: dropping it uncommitted [`reset`](Self::reset)s, and the next value is encoded as a
/// fresh snapshot. Committing a frame you failed to write is the one way to corrupt the stream.
///
/// If the caller cuts a group for its own reasons (a `cut`, `seek`, or discontinuity), call
/// [`reset`](Self::reset) directly so the next value opens the new group with a snapshot.
pub struct Encoder<T> {
	config: ProducerConfig,

	/// The last encoded value, the baseline every delta is diffed against. `None` until the first
	/// snapshot, which is what makes that first [`update`](Self::update) a keyframe.
	last: Option<Value>,

	/// The current group's DEFLATE encoder (one window per group), `Some` while compressing.
	flate: Option<moq_flate::Encoder>,

	/// Bytes of deltas emitted into the current group, excluding the snapshot frame. Compressed
	/// slice sizes when compressing, raw patch sizes otherwise.
	delta_bytes: u64,

	/// Reference size the delta budget is measured against: the current group's snapshot frame.
	/// Its compressed slice size when compressing, raw otherwise.
	snapshot_len: u64,

	/// Frames emitted into the current group, snapshot included.
	group_frames: usize,

	_marker: PhantomData<fn(T)>,
}

impl<T> Encoder<T> {
	/// Create an encoder with a cold baseline, so the first [`update`](Self::update) is a snapshot.
	pub fn new(config: ProducerConfig) -> Self {
		Self {
			config,
			last: None,
			flate: None,
			delta_bytes: 0,
			snapshot_len: 0,
			group_frames: 0,
			_marker: PhantomData,
		}
	}

	/// The last encoded value, or `None` before the first snapshot.
	///
	/// This is the baseline the next delta is diffed against, which is what a caller editing the
	/// value in place needs to start from.
	pub fn value(&self) -> Option<&Value> {
		self.last.as_ref()
	}

	/// Forget the baseline, so the next [`update`](Self::update) emits a snapshot.
	///
	/// Call this whenever the caller closes the current group behind the encoder's back (a
	/// `cut`, a `seek`, a discontinuity). Without it the next value may be encoded as a delta
	/// against a DEFLATE window and a baseline that the new group doesn't carry.
	pub fn reset(&mut self) {
		self.last = None;
		self.flate = None;
		self.delta_bytes = 0;
		self.snapshot_len = 0;
		self.group_frames = 0;
	}
}

impl<T: Serialize> Encoder<T> {
	/// Encode a new value, as a snapshot or a delta.
	///
	/// Returns `None` when the value is unchanged from the last one encoded, so nothing needs to be
	/// written. Otherwise the frame comes back as a [`Pending`] the caller writes and then
	/// [`commit`](Pending::commit)s; dropping it uncommitted resynchronizes the encoder.
	pub fn update(&mut self, value: &T) -> Result<Option<Pending<'_, T>>> {
		Ok(self.encode(value)?.map(|encoded| Pending {
			encoder: self,
			encoded,
			committed: false,
		}))
	}

	/// Encode a new value into a bare frame, advancing the encoder's state.
	///
	/// The state change is what [`Pending`] guards, so this stays private: every caller goes through
	/// [`update`](Self::update) and has to say whether the frame reached the wire.
	fn encode(&mut self, value: &T) -> Result<Option<Encoded>> {
		// The first update (or the first after a `reset`) has no baseline to diff against, so it seeds
		// the stream with a snapshot.
		let Some(last) = self.last.as_ref() else {
			return self.snapshot(value).map(Some);
		};

		// Diff straight off `T`, without building a full `Value` for the new value first.
		let Diff { patch, forced_snapshot } = diff(last, value);

		// An empty object patch with no forced null means the value is unchanged: encode nothing.
		if !forced_snapshot && patch.as_object().is_some_and(serde_json::Map::is_empty) {
			return Ok(None);
		}

		// A forced snapshot (a genuine null, or a non-object root) or an exhausted delta budget starts a
		// new group; otherwise the change rides as a delta in the open one.
		if forced_snapshot || !self.delta_allowed() {
			return self.snapshot(value).map(Some);
		}

		// Compress into the per-group window only now, for a frame we are committed to emitting.
		let bytes = serde_json::to_vec(&patch)?;
		let payload = match self.flate.as_mut() {
			Some(flate) => flate.frame(&bytes),
			None => Bytes::from(bytes),
		};
		self.delta_bytes += payload.len() as u64;
		self.group_frames += 1;

		// Fold the delta into the baseline so the next diff is against the value we just encoded.
		json_patch::merge(self.last.as_mut().expect("a snapshot precedes any delta"), &patch);

		Ok(Some(Encoded {
			payload,
			keyframe: false,
		}))
	}

	/// Whether the current change may ride as a delta in the open group.
	///
	/// The budget gate measures the deltas *already emitted* (excluding the frame about to land)
	/// against the group's snapshot frame. Both are compressed sizes when compressing and raw
	/// otherwise, so the comparison is like-for-like. Because the pending frame is excluded, the delta
	/// that tips the group past `ratio * snapshot` still lands: a group overshoots by at most one delta
	/// before rolling.
	fn delta_allowed(&self) -> bool {
		let ratio = u64::from(self.config.delta_ratio);
		ratio != 0
			&& self.group_frames > 0
			&& self.group_frames < MAX_DELTA_FRAMES
			&& self.delta_bytes <= ratio * self.snapshot_len
	}

	/// Encode a full snapshot of `value`, opening a new group and reseeding the baseline.
	fn snapshot(&mut self, value: &T) -> Result<Encoded> {
		// Serialize directly from `value` so the snapshot frame preserves the type's own field order,
		// keeping the wire bytes identical to serializing `T` straight to a frame.
		let snapshot = serde_json::to_vec(value)?;

		// Read the baseline back out of those same bytes rather than serializing `value` a second
		// time, so the baseline IS the emitted snapshot by construction. A `Serialize` impl reading a
		// clock or interior mutable state would otherwise seed the baseline with a value no consumer
		// ever received, and every later delta would rebase them onto it. `T` is also only visited
		// once, which is what a caller with an expensive or effectful `Serialize` pays for.
		//
		// This trades a second walk of `T` for a parse of the bytes, so it is not automatically
		// cheaper than `to_value` (see the `baseline` benchmark); consistency is the reason.
		//
		// Both fallible steps run before any state changes, so a failure leaves the encoder exactly
		// as it was rather than half-advanced with no frame to show for it.
		let last = serde_json::from_slice(&snapshot)?;

		// Open a fresh per-group encoder (cold window) and compress the snapshot as frame 0, recording
		// its wire size as the delta anchor.
		let (payload, flate) = match self.config.compression {
			true => {
				let mut flate = moq_flate::Encoder::new();
				let payload = flate.frame(&snapshot);
				(payload, Some(flate))
			}
			false => (Bytes::from(snapshot), None),
		};

		self.snapshot_len = payload.len() as u64;
		self.delta_bytes = 0;
		self.group_frames = 1;
		self.flate = flate;
		self.last = Some(last);

		Ok(Encoded {
			payload,
			keyframe: true,
		})
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use serde_json::json;

	/// Encode a sequence of values, committing each frame, and return `(keyframe, payload_len)` per
	/// emitted frame.
	fn encode(config: ProducerConfig, values: &[Value]) -> Vec<(bool, usize)> {
		let mut encoder = Encoder::<Value>::new(config);
		let mut out = Vec::new();
		for value in values {
			if let Some(frame) = encoder.update(value).unwrap() {
				out.push((frame.keyframe, frame.payload.len()));
				frame.commit();
			}
		}
		out
	}

	/// Encode one value and commit it, returning the frame.
	fn commit(encoder: &mut Encoder<Value>, value: &Value) -> Option<Encoded> {
		let frame = encoder.update(value).unwrap()?;
		let encoded = Encoded {
			payload: frame.payload.clone(),
			keyframe: frame.keyframe,
		};
		frame.commit();
		Some(encoded)
	}

	#[test]
	fn first_update_is_a_keyframe() {
		let frames = encode(ProducerConfig::default(), &[json!({ "a": 1 })]);
		assert_eq!(frames.len(), 1);
		assert!(frames[0].0);
	}

	#[test]
	fn unchanged_value_encodes_nothing() {
		let frames = encode(ProducerConfig::default(), &[json!({ "a": 1 }), json!({ "a": 1 })]);
		assert_eq!(frames.len(), 1);
	}

	#[test]
	fn changes_ride_as_deltas() {
		let frames = encode(
			ProducerConfig::default().with_delta_ratio(100),
			&[
				json!({ "a": 1, "b": 1 }),
				json!({ "a": 1, "b": 2 }),
				json!({ "a": 1, "b": 3 }),
			],
		);
		assert_eq!(frames.iter().map(|f| f.0).collect::<Vec<_>>(), vec![true, false, false]);
	}

	#[test]
	fn deltas_off_forces_a_keyframe_per_change() {
		let frames = encode(
			ProducerConfig::default().with_delta_ratio(0),
			&[json!({ "a": 1 }), json!({ "a": 2 })],
		);
		assert_eq!(frames.iter().map(|f| f.0).collect::<Vec<_>>(), vec![true, true]);
	}

	/// A value the caller might reasonably expect to be a delta, but that merge patch can't express:
	/// setting a field to JSON null reads as a key deletion. The encoder has to override the caller
	/// here, which is why `keyframe` is a return value rather than a parameter.
	#[test]
	fn a_null_field_forces_a_keyframe() {
		let frames = encode(
			ProducerConfig::default().with_delta_ratio(100),
			&[json!({ "a": 1, "b": 1 }), json!({ "a": 1, "b": null })],
		);
		assert_eq!(frames.iter().map(|f| f.0).collect::<Vec<_>>(), vec![true, true]);
	}

	/// Same story for a root that isn't an object: there is no recursive merge patch for it.
	#[test]
	fn a_non_object_root_forces_a_keyframe() {
		let frames = encode(
			ProducerConfig::default().with_delta_ratio(100),
			&[json!({ "a": 1 }), json!([1, 2, 3])],
		);
		assert_eq!(frames.iter().map(|f| f.0).collect::<Vec<_>>(), vec![true, true]);
	}

	#[test]
	fn frame_cap_forces_a_keyframe() {
		let values: Vec<Value> = (0..=MAX_DELTA_FRAMES).map(|n| json!({ "n": n })).collect();
		let frames = encode(ProducerConfig::default().with_delta_ratio(1_000_000), &values);

		// The snapshot plus MAX_DELTA_FRAMES - 1 deltas fill the group, then the cap rolls it.
		assert_eq!(frames.len(), MAX_DELTA_FRAMES + 1);
		assert_eq!(frames.iter().filter(|f| f.0).count(), 2);
		assert!(frames[MAX_DELTA_FRAMES].0);
	}

	/// A caller that cuts the group behind the encoder's back has to say so, or the next value would
	/// be a delta against a window and a baseline the new group never carried.
	#[test]
	fn reset_forces_the_next_update_to_be_a_keyframe() {
		let mut encoder = Encoder::<Value>::new(ProducerConfig::default().with_delta_ratio(100));
		assert!(commit(&mut encoder, &json!({ "a": 1 })).unwrap().keyframe);
		assert!(!commit(&mut encoder, &json!({ "a": 2 })).unwrap().keyframe);

		encoder.reset();
		assert!(commit(&mut encoder, &json!({ "a": 3 })).unwrap().keyframe);
	}

	/// A frame the caller never wrote must not leave the encoder emitting deltas against a baseline
	/// no consumer received. Dropping the [`Pending`] uncommitted is what a failed write looks like,
	/// and it has to resynchronize on its own: a caller cannot be relied on to remember.
	#[test]
	fn an_uncommitted_frame_resynchronizes_the_encoder() {
		let mut encoder = Encoder::<Value>::new(ProducerConfig::default().with_delta_ratio(100));
		commit(&mut encoder, &json!({ "a": 1 })).unwrap();

		// The caller wrote this one and said so, so the next value can still ride as a delta.
		commit(&mut encoder, &json!({ "a": 2 })).unwrap();

		// This one fails to write, so the caller drops it without committing.
		drop(encoder.update(&json!({ "a": 3 })).unwrap().expect("a delta"));

		// The next value opens a new group with a full snapshot rather than patching a state the
		// consumer never reached.
		let recovered = commit(&mut encoder, &json!({ "a": 4 })).expect("a resynchronizing snapshot");
		assert!(recovered.keyframe);
		assert_eq!(
			serde_json::from_slice::<Value>(&recovered.payload).unwrap(),
			json!({ "a": 4 }),
			"the snapshot carries the whole value, not a patch"
		);
	}

	/// The same recovery when the very first frame is lost: the encoder must not treat the value as
	/// already published and skip it as unchanged.
	#[test]
	fn an_uncommitted_first_frame_is_reencoded() {
		let mut encoder = Encoder::<Value>::new(ProducerConfig::default());
		drop(encoder.update(&json!({ "a": 1 })).unwrap().expect("a snapshot"));

		let retried = commit(&mut encoder, &json!({ "a": 1 })).expect("the same value, re-encoded");
		assert!(retried.keyframe);
	}

	/// A reset value is republished even when it matches the last one encoded: the new group has to
	/// open with a snapshot, so "unchanged" can't mean "write nothing" there.
	#[test]
	fn reset_republishes_an_unchanged_value() {
		let mut encoder = Encoder::<Value>::new(ProducerConfig::default());
		commit(&mut encoder, &json!({ "a": 1 })).unwrap();

		encoder.reset();
		assert!(
			commit(&mut encoder, &json!({ "a": 1 }))
				.expect("a fresh snapshot")
				.keyframe
		);
	}

	#[test]
	fn compressed_deltas_reuse_the_group_window() {
		let phrase = "Media over QUIC delivers real-time latency at massive scale";
		let frames = encode(
			ProducerConfig::default().with_delta_ratio(100).with_compression(true),
			&[json!({ "note": phrase }), json!({ "note": phrase, "echo": phrase })],
		);

		// The raw patch repeats the whole phrase; compressed against the window it's a fraction.
		let raw = serde_json::to_vec(&json!({ "echo": phrase })).unwrap().len();
		assert_eq!(frames.len(), 2);
		assert!(
			frames[1].1 < raw / 2,
			"windowed delta {} vs raw patch {raw}",
			frames[1].1
		);
	}

	/// A value whose serialization changes on every call, standing in for a `Serialize` impl backed by
	/// a clock, an atomic, or interior mutable state.
	struct Ticking(std::cell::Cell<u32>);

	impl serde::Serialize for Ticking {
		fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
			use serde::ser::SerializeMap;

			let n = self.0.get();
			self.0.set(n + 1);

			let mut map = serializer.serialize_map(Some(1))?;
			map.serialize_entry("n", &n)?;
			map.end()
		}
	}

	/// The snapshot frame and the baseline must come from a single pass over the value. Serializing
	/// twice costs a second traversal, and for a value like this one it seeds the baseline with
	/// something no consumer ever received, so every later delta rebases them onto a phantom state.
	#[test]
	fn a_snapshot_serializes_its_value_once() {
		let value = Ticking(std::cell::Cell::new(0));
		let mut encoder = Encoder::<Ticking>::new(ProducerConfig::default());
		let payload = {
			let frame = encoder.update(&value).unwrap().expect("a snapshot");
			let payload = frame.payload.clone();
			frame.commit();
			payload
		};

		assert_eq!(value.0.get(), 1, "the value should be serialized exactly once");

		let emitted: Value = serde_json::from_slice(&payload).unwrap();
		assert_eq!(emitted, json!({ "n": 0 }));
		assert_eq!(encoder.value(), Some(&emitted), "the baseline must be what was emitted");
	}

	#[test]
	fn value_tracks_the_baseline() {
		let mut encoder = Encoder::<Value>::new(ProducerConfig::default().with_delta_ratio(100));
		assert_eq!(encoder.value(), None);

		commit(&mut encoder, &json!({ "a": 1, "b": 1 }));
		commit(&mut encoder, &json!({ "a": 1, "b": 2 }));

		// The delta was folded into the baseline, so it reflects what was actually published.
		assert_eq!(encoder.value(), Some(&json!({ "a": 1, "b": 2 })));
	}
}
