//! The track-free half of snapshot publishing: values in, frame payloads out.

use std::cell::RefCell;
use std::marker::PhantomData;
use std::sync::OnceLock;

use bytes::Bytes;
use serde::Serialize;
use serde_json::Value;

use crate::{Compression, Result};

/// Maximum frames (snapshot + deltas) in a single group before a new snapshot is forced.
///
/// Kept well below moq-net's per-group frame cap so a late joiner can always read the snapshot
/// at frame 0 before the group is evicted.
pub(super) const MAX_DELTA_FRAMES: usize = 256;

/// What an [`Encoder`] keeps of the value it last emitted.
///
/// A delta is a diff against the previous value, so one has to be parsed to diff against
/// whenever deltas are possible. With `delta_ratio = 0` none ever are, and the only question
/// an update asks of the baseline is whether the value changed at all, which the encoded
/// bytes answer directly. The parse is deferred in that case, and a value that is only ever
/// published never pays for one.
enum Baseline {
	/// Deltas are possible, so the baseline is kept parsed and ready to diff against.
	Parsed(Value),

	/// Deltas are disabled. The emitted bytes (shared with the frame payload when not
	/// compressing) stand in for the value, parsed only if a caller reads it back.
	Encoded {
		bytes: Bytes,
		parsed: OnceLock<Option<Value>>,
	},
}

impl Baseline {
	/// The baseline as a parsed value, parsing the encoded bytes on first use.
	fn value(&self) -> Option<&Value> {
		match self {
			Self::Parsed(value) => Some(value),
			// Serialized by us, so this parses unless the caller's `Serialize` emitted
			// something `serde_json` will not read back.
			Self::Encoded { bytes, parsed } => parsed.get_or_init(|| serde_json::from_slice(bytes).ok()).as_ref(),
		}
	}
}

/// Codec options for an [`Encoder`], and so for the [`Producer`](super::Producer) wrapping one.
///
/// Build from [`Default`] and override fields (the struct is `#[non_exhaustive]`, so new
/// options stay additive), or chain [`with_delta_ratio`](Self::with_delta_ratio).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Config {
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
	/// When [`compression`](Self::compression) is [`Compression::Deflate`], both sides of the
	/// comparison are measured on the *compressed* frame sizes (the real wire cost).
	///
	/// Defaults to `8`.
	pub delta_ratio: u32,

	/// Compress each group as one sync-flushed DEFLATE stream, so deltas reuse the snapshot as
	/// context and shrink sharply.
	///
	/// [`Compression::None`] (the default) emits plaintext JSON frames, identical on the wire to an
	/// uncompressed track. A [`Decoder`](super::Decoder) reading them must set the same
	/// [`compression`](Self::compression).
	pub compression: Compression,
}

impl Config {
	/// Set [`delta_ratio`](Self::delta_ratio) (a builder, since the struct is `#[non_exhaustive]`).
	pub fn with_delta_ratio(mut self, delta_ratio: u32) -> Self {
		self.delta_ratio = delta_ratio;
		self
	}
}

impl Default for Config {
	fn default() -> Self {
		Self {
			delta_ratio: 8,
			compression: Compression::None,
		}
	}
}

/// One encoded frame, and the group boundary it implies.
#[derive(Clone, Debug)]
pub struct Encoded {
	/// The frame payload, DEFLATE-compressed when [`Config::compression`] is [`Compression::Deflate`].
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
	config: Config,

	/// The last encoded value, the baseline every delta is diffed against. `None` until the first
	/// snapshot, which is what makes that first [`update`](Self::update) a keyframe.
	last: Option<Baseline>,

	/// Reused key buffers for comparing unchanged fields without per-update allocations, and the
	/// memoized root entries that let an unchanged entry skip the baseline walk.
	scratch: RefCell<crate::diff::Scratch>,

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

	/// Whether the next frame has to be a full snapshot, because a frame was lost or the caller cut
	/// the group. Kept separate from [`last`](Self::last) so a resync doesn't erase the value: that
	/// field is also what [`Producer::modify`](super::Producer::modify) seeds an edit from, and dropping
	/// it there would publish a document with every other field missing.
	resync: bool,

	_marker: PhantomData<fn(T)>,
}

impl<T> Encoder<T> {
	/// Create an encoder with a cold baseline, so the first [`update`](Self::update) is a snapshot.
	pub fn new(config: Config) -> Self {
		Self {
			config,
			last: None,
			scratch: RefCell::new(crate::diff::Scratch::memoized()),
			flate: None,
			delta_bytes: 0,
			snapshot_len: 0,
			group_frames: 0,
			resync: false,
			_marker: PhantomData,
		}
	}

	/// The last encoded value, or `None` before the first snapshot.
	///
	/// This is the baseline the next delta is diffed against, which is what a caller editing the
	/// value in place needs to start from.
	///
	/// With deltas disabled the baseline is held as the encoded bytes, so the first call parses
	/// them; the result is cached, and callers that never read the value never pay for it.
	pub fn value(&self) -> Option<&Value> {
		self.last.as_ref()?.value()
	}

	/// Force the next [`update`](Self::update) to emit a full snapshot, even for an unchanged value.
	///
	/// Call this whenever the caller closes the current group behind the encoder's back (a
	/// `cut`, a `seek`, a discontinuity). Without it the next value may be encoded as a delta
	/// against a DEFLATE window and a baseline that the new group doesn't carry.
	///
	/// [`value`](Self::value) survives: the snapshot republishes it in full anyway, and it is what a
	/// caller editing in place starts from.
	pub fn reset(&mut self) {
		self.flate = None;
		self.delta_bytes = 0;
		self.snapshot_len = 0;
		self.group_frames = 0;
		self.resync = true;
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
		// A lost frame, or a group the caller cut, leaves the consumer's state unknown. Re-seed with a
		// full snapshot even when the value is unchanged, since the frame that carried it may never
		// have landed.
		if self.resync {
			return self.snapshot(value).map(Some);
		}

		// With deltas disabled there is nothing to diff, so the only question is whether the value
		// changed: compare the encodings rather than parsing a baseline to diff against. The bytes
		// are handed straight to the snapshot when it did change, so an update still serializes
		// `T` exactly once.
		if let Some(Baseline::Encoded { bytes, .. }) = self.last.as_ref() {
			let bytes = bytes.clone();
			let next = serde_json::to_vec(value)?;
			if next.as_slice() == bytes.as_ref() {
				return Ok(None);
			}
			return self.snapshot_encoded(next).map(Some);
		}

		// The first update has no baseline to diff against, so it seeds the stream with a snapshot.
		let Some(Baseline::Parsed(last)) = self.last.as_ref() else {
			return self.snapshot(value).map(Some);
		};

		// Diff straight off `T`, without building a full `Value` for the new value first.
		let crate::diff::PatchBytes { patch, forced_snapshot } =
			crate::diff::bytes(last, value, &self.scratch).map_err(crate::Error::Json)?;

		// An empty object patch with no forced null means the value is unchanged: encode nothing.
		if !forced_snapshot && patch.is_empty() {
			self.scratch.get_mut().commit_memo();
			return Ok(None);
		}

		// A forced snapshot (a genuine null, or a non-object root) or an exhausted delta budget starts a
		// new group; otherwise the change rides as a delta in the open one.
		if forced_snapshot || !self.delta_allowed() {
			return self.snapshot(value).map(Some);
		}

		// Compress into the per-group window only now, for a frame we are committed to emitting.
		let bytes = Bytes::from(patch);

		// Same cap as a snapshot, on the patch's plaintext: a delta that decompresses past the
		// consumer's limit makes the whole group unreadable, since there is no keyframe after it to
		// resynchronize on. Rejecting here leaves the encoder to reset and the group as it was.
		if self.config.compression.is_deflate() && bytes.len() as u64 > moq_flate::DEFAULT_MAX_FRAME_SIZE {
			return Err(moq_flate::Error::TooLarge(moq_flate::DEFAULT_MAX_FRAME_SIZE).into());
		}
		let payload = match self.flate.as_mut() {
			Some(flate) => flate.frame(&bytes),
			None => bytes.clone(),
		};

		// A delta is only readable while the group still holds the snapshot it applies to.
		// Admitting a patch that pushes the group past that budget would abort it
		// (`GroupTooLarge`), leaving a late subscriber with no value. Roll a fresh snapshot
		// instead, which is cheap next to losing the value.
		//
		// Measured on the encoded payload rather than the plaintext: a sync-flushed DEFLATE frame can
		// come out slightly larger than its input, so the plaintext is not an upper bound. Compressing
		// first advances the window, but [`Self::snapshot`] opens a fresh one, so an over-budget delta
		// costs only the wasted compression.
		if self.snapshot_len + self.delta_bytes + payload.len() as u64 > moq_net::group::MAX_CACHE_BYTES {
			return self.snapshot(value).map(Some);
		}

		self.delta_bytes += payload.len() as u64;
		self.group_frames += 1;

		// Fold the delta into the baseline so the next diff is against the value we just encoded.
		// Reaching a delta means `delta_allowed`, which means a non-zero ratio, which is what keeps
		// the baseline parsed.
		let Some(Baseline::Parsed(last)) = self.last.as_mut() else {
			unreachable!("a parsed snapshot precedes any delta")
		};
		crate::merge::apply_generated_bytes(last, &bytes)?;
		self.scratch.get_mut().commit_memo();

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
		self.snapshot_encoded(snapshot)
	}

	/// [`snapshot`](Self::snapshot) for a value that is already serialized, so an update that
	/// encoded `T` to compare it against a byte baseline does not encode it a second time.
	fn snapshot_encoded(&mut self, snapshot: Vec<u8>) -> Result<Encoded> {
		// Every consumer decodes with moq-flate's default output cap, so a value past it would be
		// unreadable however small it compresses to. Reject it before anything is published, so the
		// previously published value stands rather than being superseded by one nothing can read.
		if self.config.compression.is_deflate() && snapshot.len() as u64 > moq_flate::DEFAULT_MAX_FRAME_SIZE {
			return Err(moq_flate::Error::TooLarge(moq_flate::DEFAULT_MAX_FRAME_SIZE).into());
		}

		// With deltas possible, read the baseline back out of those same bytes rather than
		// serializing `value` a second time, so the baseline IS the emitted snapshot by
		// construction. A `Serialize` impl reading a clock or interior mutable state would otherwise
		// seed the baseline with a value no consumer ever received, and every later delta would
		// rebase them onto it. `T` is also only visited once, which is what a caller with an
		// expensive or effectful `Serialize` pays for.
		//
		// That trades a second walk of `T` for a parse of the bytes, so it is not automatically
		// cheaper than `to_value` (see the `baseline` benchmark); consistency is the reason. With
		// deltas off there is no diff to rebase and no reason to pay it at all.
		//
		// Every fallible step runs before any state changes, so a failure leaves the encoder exactly
		// as it was rather than half-advanced with no frame to show for it.
		let snapshot = Bytes::from(snapshot);
		let last = if self.config.delta_ratio == 0 {
			// No delta will ever diff against this, so hold the bytes instead. Uncompressed, they are
			// the same allocation the payload carries, so the baseline costs a refcount.
			Baseline::Encoded {
				bytes: snapshot.clone(),
				parsed: OnceLock::new(),
			}
		} else {
			Baseline::Parsed(serde_json::from_slice(&snapshot)?)
		};

		// Open a fresh per-group encoder (cold window) and compress the snapshot as frame 0, recording
		// its wire size as the delta anchor.
		let (payload, flate) = match self.config.compression {
			Compression::Deflate => {
				let mut flate = moq_flate::Encoder::new();
				let payload = flate.frame(&snapshot);
				(payload, Some(flate))
			}
			Compression::None => (snapshot, None),
		};

		self.snapshot_len = payload.len() as u64;
		self.delta_bytes = 0;
		self.group_frames = 1;
		self.flate = flate;
		self.last = Some(last);
		self.resync = false;
		// Seeded from the snapshot rather than a diff, so no root entry is memoized against it yet.
		self.scratch.get_mut().clear_memo();

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

	#[test]
	fn duplicate_serialized_keys_are_refused() {
		use serde::ser::SerializeMap;
		struct Duplicate {
			duplicate: bool,
		}
		impl Serialize for Duplicate {
			fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
				let mut map = serializer.serialize_map(Some(2 + usize::from(self.duplicate)))?;
				map.serialize_entry("a", &1)?;
				map.serialize_entry("b", &2)?;
				if self.duplicate {
					map.serialize_entry("a", &3)?;
				}
				map.end()
			}
		}
		let mut encoder = Encoder::<Duplicate>::new(Config::default());
		encoder
			.update(&Duplicate { duplicate: false })
			.unwrap()
			.unwrap()
			.commit();
		let err = encoder.encode(&Duplicate { duplicate: true }).unwrap_err();
		assert!(err.to_string().contains("duplicate JSON object key"));
	}

	/// Encode a sequence of values, committing each frame, and return `(keyframe, payload_len)` per
	/// emitted frame.
	fn encode(config: Config, values: &[Value]) -> Vec<(bool, usize)> {
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
		let frames = encode(Config::default(), &[json!({ "a": 1 })]);
		assert_eq!(frames.len(), 1);
		assert!(frames[0].0);
	}

	#[test]
	fn unchanged_value_encodes_nothing() {
		let frames = encode(Config::default(), &[json!({ "a": 1 }), json!({ "a": 1 })]);
		assert_eq!(frames.len(), 1);
	}

	#[test]
	fn changes_ride_as_deltas() {
		let frames = encode(
			Config::default().with_delta_ratio(100),
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
			Config::default().with_delta_ratio(0),
			&[json!({ "a": 1 }), json!({ "a": 2 })],
		);
		assert_eq!(frames.iter().map(|f| f.0).collect::<Vec<_>>(), vec![true, true]);
	}

	/// Deltas off keeps the baseline as bytes rather than a parsed value, so the unchanged check
	/// runs on the encoding. It still has to suppress a republish, or every stats tick would
	/// re-emit an identical frame.
	#[test]
	fn deltas_off_still_skips_an_unchanged_value() {
		let frames = encode(
			Config::default().with_delta_ratio(0),
			&[json!({ "a": 1 }), json!({ "a": 1 }), json!({ "a": 1 })],
		);
		assert_eq!(frames.len(), 1);
	}

	/// Field order is part of the encoding, so a byte baseline only answers "unchanged" correctly
	/// because `T` serializes deterministically. Same keys, different values, must still emit.
	#[test]
	fn deltas_off_detects_a_change_under_the_same_keys() {
		let frames = encode(
			Config::default().with_delta_ratio(0),
			&[json!({ "a": 1, "b": 2 }), json!({ "a": 1, "b": 3 })],
		);
		assert_eq!(frames.len(), 2);
	}

	/// The byte baseline is parsed on demand, so `value` (and so `Producer::modify`, which seeds an
	/// edit from it) keeps working with deltas off. Dropping the baseline instead would make
	/// `modify` start from `T::default()` and publish a document with every other field missing.
	#[test]
	fn deltas_off_still_exposes_the_value() {
		let mut encoder = Encoder::<Value>::new(Config::default().with_delta_ratio(0));
		assert_eq!(encoder.value(), None);

		commit(&mut encoder, &json!({ "a": 1, "b": 2 })).unwrap();
		assert_eq!(encoder.value(), Some(&json!({ "a": 1, "b": 2 })));

		commit(&mut encoder, &json!({ "a": 1, "b": 3 })).unwrap();
		assert_eq!(encoder.value(), Some(&json!({ "a": 1, "b": 3 })));
	}

	/// Compressing shares no allocation between the baseline and the payload, so the byte baseline
	/// has to hold the plaintext rather than the compressed frame.
	#[test]
	fn deltas_off_while_compressing_keeps_the_plaintext_baseline() {
		let mut config = Config::default().with_delta_ratio(0);
		config.compression = Compression::Deflate;

		let mut encoder = Encoder::<Value>::new(config);
		commit(&mut encoder, &json!({ "a": 1 })).unwrap();
		assert_eq!(encoder.value(), Some(&json!({ "a": 1 })));
		assert!(commit(&mut encoder, &json!({ "a": 1 })).is_none());
	}

	/// A value the caller might reasonably expect to be a delta, but that merge patch can't express:
	/// setting a field to JSON null reads as a key deletion. The encoder has to override the caller
	/// here, which is why `keyframe` is a return value rather than a parameter.
	#[test]
	fn a_null_field_forces_a_keyframe() {
		let frames = encode(
			Config::default().with_delta_ratio(100),
			&[json!({ "a": 1, "b": 1 }), json!({ "a": 1, "b": null })],
		);
		assert_eq!(frames.iter().map(|f| f.0).collect::<Vec<_>>(), vec![true, true]);
	}

	/// Same story for a root that isn't an object: there is no recursive merge patch for it.
	#[test]
	fn a_non_object_root_forces_a_keyframe() {
		let frames = encode(
			Config::default().with_delta_ratio(100),
			&[json!({ "a": 1 }), json!([1, 2, 3])],
		);
		assert_eq!(frames.iter().map(|f| f.0).collect::<Vec<_>>(), vec![true, true]);
	}

	#[test]
	fn frame_cap_forces_a_keyframe() {
		let values: Vec<Value> = (0..=MAX_DELTA_FRAMES).map(|n| json!({ "n": n })).collect();
		let frames = encode(Config::default().with_delta_ratio(1_000_000), &values);

		// The snapshot plus MAX_DELTA_FRAMES - 1 deltas fill the group, then the cap rolls it.
		assert_eq!(frames.len(), MAX_DELTA_FRAMES + 1);
		assert_eq!(frames.iter().filter(|f| f.0).count(), 2);
		assert!(frames[MAX_DELTA_FRAMES].0);
	}

	/// A caller that cuts the group behind the encoder's back has to say so, or the next value would
	/// be a delta against a window and a baseline the new group never carried.
	#[test]
	fn reset_forces_the_next_update_to_be_a_keyframe() {
		let mut encoder = Encoder::<Value>::new(Config::default().with_delta_ratio(100));
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
		let mut encoder = Encoder::<Value>::new(Config::default().with_delta_ratio(100));
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
		let mut encoder = Encoder::<Value>::new(Config::default());
		drop(encoder.update(&json!({ "a": 1 })).unwrap().expect("a snapshot"));

		let retried = commit(&mut encoder, &json!({ "a": 1 })).expect("the same value, re-encoded");
		assert!(retried.keyframe);
	}

	/// A reset value is republished even when it matches the last one encoded: the new group has to
	/// open with a snapshot, so "unchanged" can't mean "write nothing" there.
	#[test]
	fn reset_republishes_an_unchanged_value() {
		let mut encoder = Encoder::<Value>::new(Config::default());
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
			Config {
				delta_ratio: 100,
				compression: Compression::Deflate,
			},
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
		let mut encoder = Encoder::<Ticking>::new(Config::default());
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

	/// A root entry the memo has not seen yet is diffed from the bytes the memo recorded, not
	/// serialized again: a second pass could disagree with the first, leaving the memo describing a
	/// value the baseline never held.
	#[test]
	fn a_delta_serializes_each_entry_once() {
		let value = std::collections::BTreeMap::from([("row", Ticking(std::cell::Cell::new(0)))]);
		let mut encoder = Encoder::new(Config::default().with_delta_ratio(100));
		encoder.update(&value).unwrap().expect("a snapshot").commit();

		let frame = encoder.update(&value).unwrap().expect("a delta");
		assert!(!frame.keyframe);
		let emitted: Value = serde_json::from_slice(&frame.payload).unwrap();
		frame.commit();

		assert_eq!(
			value["row"].0.get(),
			2,
			"each update should serialize the entry exactly once"
		);
		assert_eq!(emitted, json!({ "row": { "n": 1 } }));
		assert_eq!(encoder.value(), Some(&emitted), "the baseline must be what was emitted");
	}

	/// A key repeated below the root is refused whether the memo meets it in a new entry or in a
	/// value replaced wholesale, as the value diff refuses it. Letting one into the memo would pair
	/// the repeats by position, where the consumer keeps the last.
	#[test]
	fn a_repeated_nested_key_is_refused_through_the_memo() {
		use serde::ser::SerializeMap;

		/// `{"row": {"o": ..}}`, where `o` is `1` or an object that repeats a key.
		struct Doc {
			repeat: bool,
		}
		struct Row<'a>(&'a Doc);
		struct Repeat;

		impl Serialize for Repeat {
			fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
				let mut map = serializer.serialize_map(Some(2))?;
				map.serialize_entry("x", &1)?;
				map.serialize_entry("x", &2)?;
				map.end()
			}
		}
		impl Serialize for Row<'_> {
			fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
				let mut map = serializer.serialize_map(Some(1))?;
				match self.0.repeat {
					true => map.serialize_entry("o", &Repeat)?,
					false => map.serialize_entry("o", &1)?,
				}
				map.end()
			}
		}
		impl Serialize for Doc {
			fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
				let mut map = serializer.serialize_map(Some(1))?;
				map.serialize_entry("row", &Row(self))?;
				map.end()
			}
		}

		let config = Config::default().with_delta_ratio(100);
		let (plain, repeat) = (Doc { repeat: false }, Doc { repeat: true });

		// A new entry: the first diff after a snapshot has nothing memoized yet.
		let mut encoder = Encoder::<Doc>::new(config.clone());
		encoder.update(&plain).unwrap().expect("a snapshot").commit();
		let err = encoder.encode(&repeat).unwrap_err();
		assert!(err.to_string().contains("duplicate JSON object key"), "{err}");

		// A memoized entry whose scalar becomes an object.
		let mut encoder = Encoder::<Doc>::new(config);
		encoder.update(&plain).unwrap().expect("a snapshot").commit();
		assert!(encoder.update(&plain).unwrap().is_none(), "unchanged, now memoized");
		let err = encoder.encode(&repeat).unwrap_err();
		assert!(err.to_string().contains("duplicate JSON object key"), "{err}");
	}

	/// A root object whose entries serialize in the order given, sorted or not.
	struct Rows(Vec<(String, Value)>);

	impl Serialize for Rows {
		fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
			serializer.collect_map(self.0.iter().map(|(key, value)| (key, value)))
		}
	}

	/// A deterministic xorshift, so a failure replays.
	struct Rng(u64);

	impl Rng {
		fn below(&mut self, n: u64) -> u64 {
			self.0 ^= self.0 << 13;
			self.0 ^= self.0 >> 7;
			self.0 ^= self.0 << 17;
			self.0 % n
		}

		/// A row value covering what the memo has to get right: nested objects that gain and lose
		/// keys, values that change type, nulls in and out of arrays, and strings that look like JSON.
		fn row(&mut self) -> Value {
			let strings = ["plain", "q\"uote", "back\\slash", "},{\"x\":1", "null", "a:b,c"];
			let mut row = serde_json::Map::new();
			row.insert(
				"n".into(),
				match self.below(30) {
					0 => Value::Null,
					n => json!(n % 4),
				},
			);
			if self.below(4) > 0 {
				row.insert("s".into(), json!(strings[self.below(strings.len() as u64) as usize]));
			}
			let mut nested = serde_json::Map::new();
			nested.insert("a".into(), json!(self.below(3)));
			if self.below(3) == 0 {
				nested.insert("b".into(), json!([self.below(2), null]));
			}
			if self.below(40) == 0 {
				nested.insert("c".into(), Value::Null);
			}
			row.insert("o".into(), Value::Object(nested));
			row.insert(
				"t".into(),
				match self.below(5) {
					0 => json!({ "k": self.below(2) }),
					1 => json!({}),
					2 => json!([{ "k": null }]),
					3 => json!(1.5 + self.below(2) as f64),
					_ => json!("t"),
				},
			);
			if self.below(60) == 0 {
				row.insert("z".into(), Value::Null);
			}
			Value::Object(row)
		}
	}

	/// The memo is a shortcut past the value diff, so it must never change a frame: every payload and
	/// keyframe has to match an encoder diffing without it, through inserts, deletions, reorders,
	/// shape changes, forced snapshots, and group rolls.
	#[test]
	fn memo_matches_the_value_diff() {
		for (seed, compression) in [
			(1, Compression::None),
			(2, Compression::Deflate),
			(3, Compression::None),
		] {
			let mut config = Config::default().with_delta_ratio(2);
			config.compression = compression;
			let mut memoized = Encoder::<Rows>::new(config.clone());
			let mut plain = Encoder::<Rows>::new(config);
			plain.scratch = RefCell::new(crate::diff::Scratch::default());

			let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ seed);
			let mut rows: Vec<(String, Value)> = (0..40).map(|i| (format!("row-{i:03}"), rng.row())).collect();
			let mut emitted = 0;
			for tick in 0..400 {
				for row in rows.iter_mut() {
					if rng.below(4) == 0 {
						row.1 = rng.row();
					}
				}
				if rng.below(3) == 0 {
					let index = rng.below(rows.len() as u64) as usize;
					rows.remove(index);
				}
				if rng.below(3) == 0 {
					rows.push((format!("row-{:03}", 40 + rng.below(40)), rng.row()));
				}
				rows.sort_by(|a, b| a.0.cmp(&b.0));
				rows.dedup_by(|a, b| a.0 == b.0);
				// Now and then, a root that stops ascending.
				if seed == 3 && rng.below(10) == 0 {
					let (a, b) = (
						rng.below(rows.len() as u64) as usize,
						rng.below(rows.len() as u64) as usize,
					);
					rows.swap(a, b);
				}

				let value = Rows(rows.clone());
				let want = plain.update(&value).unwrap().map(|frame| {
					let encoded = (*frame).clone();
					frame.commit();
					encoded
				});
				let got = memoized.update(&value).unwrap().map(|frame| {
					let encoded = (*frame).clone();
					frame.commit();
					encoded
				});
				match (want, got) {
					(None, None) => {}
					(Some(want), Some(got)) => {
						assert_eq!(got.keyframe, want.keyframe, "seed {seed} tick {tick}: keyframe");
						assert_eq!(got.payload, want.payload, "seed {seed} tick {tick}: payload");
						emitted += usize::from(!got.keyframe);
					}
					(want, got) => panic!("seed {seed} tick {tick}: {want:?} vs {got:?}"),
				}
				assert_eq!(memoized.value(), plain.value(), "seed {seed} tick {tick}: baseline");
			}
			assert!(emitted > 100, "seed {seed}: only {emitted} deltas exercised the memo");
		}
	}

	#[test]
	fn value_tracks_the_baseline() {
		let mut encoder = Encoder::<Value>::new(Config::default().with_delta_ratio(100));
		assert_eq!(encoder.value(), None);

		commit(&mut encoder, &json!({ "a": 1, "b": 1 }));
		commit(&mut encoder, &json!({ "a": 1, "b": 2 }));

		// The delta was folded into the baseline, so it reflects what was actually published.
		assert_eq!(encoder.value(), Some(&json!({ "a": 1, "b": 2 })));
	}
}
