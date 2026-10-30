//! The track-free half of snapshot publishing: values in, frame payloads out.

use std::cell::RefCell;
use std::marker::PhantomData;
use std::sync::OnceLock;

use bytes::Bytes;
use serde::Serialize;
use serde_json::Value;

use crate::Result;

/// Maximum frames (snapshot + deltas) in a single group before a new snapshot is forced.
///
/// Kept well below moq-net's per-group frame cap so a late joiner can always read the snapshot
/// at frame 0 before the group is evicted.
pub(super) const MAX_DELTA_FRAMES: usize = 256;

/// What an [`Encoder`] keeps of the value it last emitted.
///
/// A delta is a diff against the previous value, so one has to be parsed to diff against
/// whenever deltas are possible. With [`Config::deltas`] off none ever are, and the only question
/// an update asks of the baseline is whether the value changed at all, which the encoded
/// bytes answer directly. The parse is deferred in that case, and a value that is only ever
/// published never pays for one.
enum Baseline {
	/// Deltas are possible, so the baseline is kept parsed and ready to diff against.
	Parsed(Value),

	/// Deltas are disabled. The emitted bytes (shared with the frame payload) stand in for the
	/// value, parsed only if a caller reads it back.
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
/// options stay additive), or chain [`with_deltas`](Self::with_deltas).
///
/// How long a group runs is not a codec option: a group grows until a joiner wants a fresh one,
/// judged by the track's [`refresh`](moq_net::track::Info::refresh) ratio, or until it reaches
/// the frame or byte cap.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Config {
	/// Whether changes ride as merge-patch deltas in the open group.
	///
	/// `false` makes every change its own single-frame snapshot group, identical on the wire to a
	/// plain "one JSON blob per group" track, so refresh never applies.
	///
	/// Defaults to `true`.
	pub deltas: bool,

	/// The most bytes a group may hold, snapshot and deltas together.
	///
	/// A delta that would pass it rolls a fresh snapshot instead, and a snapshot past it is refused
	/// with [`FrameTooLarge`](moq_net::Error::FrameTooLarge). Lowering it lets a test reach the limit
	/// without megabytes of JSON; a value above moq-net's cache limit is clamped to it, since moq-net
	/// refuses a larger group anyway.
	///
	/// Defaults to [`moq_net::group::MAX_CACHE_BYTES`].
	pub max_group_bytes: u64,
}

impl Config {
	/// Set [`deltas`](Self::deltas) (a builder, since the struct is `#[non_exhaustive]`).
	pub fn with_deltas(mut self, deltas: bool) -> Self {
		self.deltas = deltas;
		self
	}

	/// Set [`max_group_bytes`](Self::max_group_bytes).
	pub fn with_max_group_bytes(mut self, max_group_bytes: u64) -> Self {
		self.max_group_bytes = max_group_bytes;
		self
	}
}

impl Default for Config {
	fn default() -> Self {
		Self {
			deltas: true,
			max_group_bytes: moq_net::group::MAX_CACHE_BYTES,
		}
	}
}

/// One encoded frame, and the group boundary it implies.
#[derive(Clone, Debug)]
pub struct Encoded {
	/// The frame payload: a full JSON snapshot or a merge patch.
	pub payload: Bytes,

	/// Whether this frame is a full snapshot, which must open a new group.
	///
	/// `true` means the caller writes it as the first frame of a fresh group; `false` means it is a
	/// merge patch that must be appended to the group the last snapshot opened. Mapping straight onto
	/// [`moq_mux::container::Frame::keyframe`] is the point of the name.
	///
	/// The encoder decides this for an [`update`](Encoder::update): a value that sets a field to JSON
	/// null, or whose root isn't an object, cannot be expressed as a merge patch at all, and the
	/// group's frame and byte caps force a snapshot independently of what the caller wanted. A caller
	/// asks for one with [`snapshot`](Encoder::snapshot).
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
/// The frame's value is staged until commit, so dropping it uncommitted leaves
/// [`Encoder::value`] at the last value that reached the wire, and [`Encoder::reset`]s so the next
/// frame is a fresh snapshot rather than a delta against a group no consumer received. The group
/// state is a recovery, not a rollback: a consumer may hold a group the dropped frame was meant
/// for, so a snapshot is the only sound way back. Forgetting to commit
/// a frame that *was* written therefore costs one redundant snapshot and leaves the value a step
/// behind, but never corrupts the stream.
#[must_use = "the frame must be written and committed, or dropped to resynchronize the encoder"]
pub struct Pending<'a, T> {
	encoder: &'a mut Encoder<T>,
	encoded: Encoded,
	edit: Option<Edit>,
}

/// The baseline change staged behind a [`Pending`] frame.
enum Edit {
	/// A snapshot replaces the baseline.
	Snapshot(Baseline),
	/// A delta folds this merge patch into the baseline.
	Delta(Bytes),
	/// A snapshot restating the baseline as it is, which changes nothing.
	Restate,
}

impl<T> Pending<'_, T> {
	/// Acknowledge that the frame reached the wire, applying its value to the baseline.
	///
	/// Only call this once the write has actually succeeded. Committing a frame that failed to write
	/// is the one thing that corrupts the stream.
	pub fn commit(mut self) {
		let edit = self.edit.take().expect("pending edit");
		self.encoder.commit(edit);
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
		if self.edit.is_some() {
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
/// [`keyframe`](Encoded::keyframe) set must open a new group: the merge patches depend on it. [`update`](Self::update) hands back a [`Pending`]
/// rather than a bare [`Encoded`] so a frame that never reaches the wire can't silently desync the
/// encoder: dropping it uncommitted [`reset`](Self::reset)s, and the next value is encoded as a
/// fresh snapshot. Committing a frame you failed to write is the one way to corrupt the stream.
///
/// If the caller cuts a group for its own reasons (a `cut`, `seek`, or discontinuity), call
/// [`reset`](Self::reset) directly so the next value opens the new group with a snapshot. To open
/// one right away, such as when a joiner wants a fresh group (see
/// [`Demand::wanted`](moq_net::track::Demand::wanted)), call [`snapshot`](Self::snapshot).
pub struct Encoder<T> {
	config: Config,

	/// The last committed value, the baseline every delta is diffed against. `None` until the first
	/// snapshot commits, which is what makes that first [`update`](Self::update) a keyframe.
	last: Option<Baseline>,

	/// Reused key buffers for comparing unchanged fields without per-update allocations, and the
	/// memoized root entries that let an unchanged entry skip the baseline walk.
	scratch: RefCell<crate::diff::Scratch>,

	/// Bytes charged to the current group, snapshot included.
	group_bytes: u64,

	/// Frames emitted into the current group, snapshot included.
	group_frames: usize,

	/// [`Config::max_group_bytes`], clamped to moq-net's cache limit.
	max_group_bytes: u64,

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
			max_group_bytes: config.max_group_bytes.min(moq_net::group::MAX_CACHE_BYTES),
			config,
			last: None,
			scratch: RefCell::new(crate::diff::Scratch::memoized()),
			group_bytes: 0,
			group_frames: 0,
			resync: false,
			_marker: PhantomData,
		}
	}

	/// The last committed value, or `None` before the first snapshot commits.
	///
	/// This is the baseline the next delta is diffed against, which is what a caller editing the
	/// value in place needs to start from. A frame dropped uncommitted never reached the wire, so its
	/// value never lands here.
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
	/// against a baseline that the new group doesn't carry.
	///
	/// [`value`](Self::value) survives: the snapshot republishes it in full anyway, and it is what a
	/// caller editing in place starts from.
	pub fn reset(&mut self) {
		self.group_bytes = 0;
		self.group_frames = 0;
		self.resync = true;
	}

	/// Restate the last committed value as a full snapshot, which must open a new group.
	///
	/// For a caller that opens groups itself: call it when a joiner wants a fresh group, such as when
	/// [`Demand::wanted`](moq_net::track::Demand::wanted) passes the largest group written. The new
	/// group restates the value without the deltas before it, though that joiner still reads the
	/// cached group first. Returns `None` before the first value,
	/// when there is nothing to restate. The frame is a [`Pending`] like any other.
	pub fn snapshot(&mut self) -> Result<Option<Pending<'_, T>>> {
		let bytes = match &self.last {
			None => return Ok(None),
			// A delta-free baseline is the emitted bytes already.
			Some(Baseline::Encoded { bytes, .. }) => bytes.clone(),
			Some(Baseline::Parsed(value)) => Bytes::from(serde_json::to_vec(value)?),
		};
		// A baseline built by patches was never held to the budget as one snapshot.
		self.check_snapshot(&bytes)?;
		let encoded = self.snapshot_encoded(bytes);
		Ok(Some(Pending {
			encoder: self,
			encoded,
			edit: Some(Edit::Restate),
		}))
	}

	/// Apply a frame's staged value to the baseline once the frame reached the wire.
	fn commit(&mut self, edit: Edit) {
		match edit {
			Edit::Snapshot(last) => {
				self.last = Some(last);
				// Seeded from the snapshot rather than a diff, so no root entry is memoized against it yet.
				self.scratch.get_mut().clear_memo();
			}
			Edit::Delta(patch) => {
				// Reaching a delta means `delta_allowed`, which means deltas are on, which is what keeps
				// the baseline parsed.
				let Some(Baseline::Parsed(last)) = self.last.as_mut() else {
					unreachable!("a parsed snapshot precedes any delta")
				};
				crate::merge::apply_generated_bytes(last, &patch)
					.expect("the diff serializer emits a valid merge patch");
				self.scratch.get_mut().commit_memo();
			}
			Edit::Restate => {}
		}
	}

	/// Encode an already-serialized value as a full snapshot, staging it as the new baseline.
	fn snapshot_staged(&mut self, snapshot: Bytes) -> Result<(Encoded, Edit)> {
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
		// as it was rather than half-advanced with no frame to show for it. The size check runs
		// first, so a value no reader could hold costs no parse.
		self.check_snapshot(&snapshot)?;
		let last = if self.config.deltas {
			Baseline::Parsed(serde_json::from_slice(&snapshot)?)
		} else {
			// No delta will ever diff against this, so hold the bytes instead. They are the same
			// allocation the payload carries, so the baseline costs a refcount.
			Baseline::Encoded {
				bytes: snapshot.clone(),
				parsed: OnceLock::new(),
			}
		};

		let encoded = self.snapshot_encoded(snapshot);
		Ok((encoded, Edit::Snapshot(last)))
	}

	/// Refuse a snapshot past the group budget before anything is published, so the previous value
	/// stays readable rather than superseded by a frame moq-net would refuse on write.
	fn check_snapshot(&self, snapshot: &[u8]) -> Result<()> {
		match snapshot.len() as u64 > self.max_group_bytes {
			true => Err(moq_net::Error::FrameTooLarge.into()),
			false => Ok(()),
		}
	}

	/// Encode an already-serialized value as a full snapshot, opening a new group.
	fn snapshot_encoded(&mut self, snapshot: Bytes) -> Encoded {
		self.group_bytes = snapshot.len() as u64;
		self.group_frames = 1;
		self.resync = false;

		Encoded {
			payload: snapshot,
			keyframe: true,
		}
	}
}

impl<T: Serialize> Encoder<T> {
	/// Encode a new value, as a snapshot or a delta.
	///
	/// Returns `None` when the value is unchanged from the last one committed, so nothing needs to be
	/// written. Otherwise the frame comes back as a [`Pending`] the caller writes and then
	/// [`commit`](Pending::commit)s; dropping it uncommitted resynchronizes the encoder.
	pub fn update(&mut self, value: &T) -> Result<Option<Pending<'_, T>>> {
		Ok(self.encode(value)?.map(|(encoded, edit)| Pending {
			encoder: self,
			encoded,
			edit: Some(edit),
		}))
	}

	/// Encode a new value into a bare frame and the baseline change it stages, advancing the group.
	///
	/// The state change is what [`Pending`] guards, so this stays private: every caller goes through
	/// [`update`](Self::update) and has to say whether the frame reached the wire.
	fn encode(&mut self, value: &T) -> Result<Option<(Encoded, Edit)>> {
		// A lost frame, or a group the caller cut, leaves the consumer's state unknown. Re-seed with a
		// full snapshot even when the value is unchanged, since the frame that carried it may never
		// have landed.
		if self.resync {
			return self.encode_snapshot(value).map(Some);
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
			return self.snapshot_staged(Bytes::from(next)).map(Some);
		}

		// The first update has no baseline to diff against, so it seeds the stream with a snapshot.
		let Some(Baseline::Parsed(last)) = self.last.as_ref() else {
			return self.encode_snapshot(value).map(Some);
		};

		// Diff straight off `T`, without building a full `Value` for the new value first.
		let crate::diff::PatchBytes { patch, forced_snapshot } =
			crate::diff::bytes(last, value, &self.scratch).map_err(crate::Error::Json)?;

		// An empty object patch with no forced null means the value is unchanged: encode nothing.
		if !forced_snapshot && patch.is_empty() {
			self.scratch.get_mut().commit_memo();
			return Ok(None);
		}

		// A forced snapshot (a genuine null, or a non-object root) or a full group starts a
		// new group; otherwise the change rides as a delta in the open one.
		if forced_snapshot || !self.delta_allowed() {
			return self.encode_snapshot(value).map(Some);
		}

		// A delta is only readable while the group still holds the snapshot it applies to.
		// Admitting a patch that pushes the group past that budget would abort it
		// (`GroupTooLarge`), leaving a late subscriber with no value. Roll a fresh snapshot
		// instead, which is cheap next to losing the value.
		let payload = Bytes::from(patch);
		let charge = payload.len() as u64;
		if self.group_bytes + charge > self.max_group_bytes {
			self.reset();
			return self.encode_snapshot(value).map(Some);
		}

		self.group_bytes += charge;
		self.group_frames += 1;

		// The patch folds into the baseline on commit, so a frame that never reaches the wire leaves
		// the baseline at the last value consumers received.
		let encoded = Encoded {
			payload: payload.clone(),
			keyframe: false,
		};
		Ok(Some((encoded, Edit::Delta(payload))))
	}

	/// Whether the current change may ride as a delta in the open group: deltas are on and the group
	/// is under its frame cap. The byte cap is checked once the delta is encoded.
	fn delta_allowed(&self) -> bool {
		self.config.deltas && self.group_frames > 0 && self.group_frames < MAX_DELTA_FRAMES
	}

	/// Encode a full snapshot of `value`, opening a new group and staging it as the baseline.
	fn encode_snapshot(&mut self, value: &T) -> Result<(Encoded, Edit)> {
		// Serialize directly from `value` so the snapshot frame preserves the type's own field order,
		// keeping the wire bytes identical to serializing `T` straight to a frame.
		let snapshot = serde_json::to_vec(value)?;
		self.snapshot_staged(Bytes::from(snapshot))
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use serde_json::json;

	// Mirror the JS generator: bounded JSON trees and edits to a persistent object.
	fn property_key() -> impl proptest::strategy::Strategy<Value = String> {
		use proptest::prelude::*;
		prop_oneof![
			proptest::sample::select(vec![
				"a",
				"b",
				"c",
				"",
				"__proto__",
				"constructor",
				"prototype",
				"\0",
				"雪"
			])
			.prop_map(str::to_owned),
			"[ -~]{0,8}",
		]
	}

	fn property_value() -> proptest::strategy::BoxedStrategy<Value> {
		use proptest::prelude::*;
		let leaf = prop_oneof![
			Just(Value::Null),
			any::<bool>().prop_map(Value::Bool),
			(-1_000_000i32..=1_000_000).prop_map(|n| Value::from(f64::from(n) / 4.0)),
			prop_oneof!["[ -~]{0,16}", Just("雪\0".to_owned())].prop_map(Value::String),
		];
		leaf.prop_recursive(4, 64, 4, |inner| {
			prop_oneof![
				proptest::collection::vec(inner.clone(), 0..5).prop_map(Value::Array),
				proptest::collection::btree_map(property_key(), inner, 0..5)
					.prop_map(|map| Value::Object(map.into_iter().collect())),
			]
		})
		.boxed()
	}

	#[derive(Clone, Debug)]
	struct PropertyStep {
		value: Value,
		reset: bool,
		/// Restate the previous value with [`Encoder::snapshot`] before this step.
		restate: bool,
	}

	fn property_sequence() -> proptest::strategy::BoxedStrategy<Vec<PropertyStep>> {
		use proptest::prelude::*;
		let roots = proptest::collection::vec((property_value(), 0u8..16), 1..33).prop_map(|steps| {
			steps
				.into_iter()
				.map(|(value, reset)| PropertyStep {
					value,
					reset: reset == 0,
					restate: reset == 1,
				})
				.collect()
		});
		let edits = proptest::collection::vec((property_key(), property_value(), any::<bool>(), 0u8..16), 1..33)
			.prop_map(|steps| {
				let mut current = serde_json::Map::new();
				steps
					.into_iter()
					.map(|(key, value, remove, reset)| {
						if remove {
							current.remove(&key);
						} else {
							current.insert(key, value);
						}
						PropertyStep {
							value: Value::Object(current.clone()),
							reset: reset == 0,
							restate: reset == 1,
						}
					})
					.collect()
			});
		prop_oneof![roots, edits].boxed()
	}

	#[test]
	fn snapshot_codec_property_reconstructs_each_json_value() {
		use proptest::prelude::*;
		use proptest::test_runner::{Config as PropertyConfig, RngSeed, TestRunner};
		let setting = |name: &str, fallback: u32, min: u32| {
			let value = std::env::var(name).map_or(fallback, |value| {
				value.parse::<u32>().expect("integer property setting")
			});
			assert!(
				(min..=i32::MAX as u32).contains(&value),
				"{name} outside supported range"
			);
			value
		};
		let seed = setting("MOQ_JSON_SEED", 0x6d6f71, 0);
		let cases = setting("MOQ_JSON_CASES", 256, 1);
		eprintln!("MOQ_JSON_SEED={seed} MOQ_JSON_CASES={cases}");
		let mut runner = TestRunner::new(PropertyConfig {
			cases,
			rng_seed: RngSeed::Fixed(u64::from(seed)),
			failure_persistence: None,
			..Default::default()
		});
		let sequence = (any::<bool>(), property_sequence());
		runner
			.run(&sequence, |(deltas, steps)| {
				let mut encoder = Encoder::<Value>::new(Config::default().with_deltas(deltas));
				let mut decoder = super::super::Decoder::<Value>::new(super::super::consumer::Config::default());
				for (index, step) in steps.iter().enumerate() {
					if step.reset {
						encoder.reset();
					}
					if step.restate {
						let previous = decoder.decode().unwrap();
						match encoder.snapshot().unwrap() {
							Some(frame) => {
								prop_assert!(frame.keyframe);
								decoder.snapshot(&frame.payload).unwrap();
								frame.commit();
							}
							None => prop_assert!(index == 0, "a value was encoded before step {}", index),
						}
						prop_assert_eq!(decoder.decode().unwrap(), previous, "restate before step {}", index);
					}
					if let Some(frame) = encoder.update(&step.value).unwrap() {
						if frame.keyframe {
							decoder.snapshot(&frame.payload).unwrap();
						} else {
							decoder.delta(&frame.payload).unwrap();
						}
						frame.commit();
					}
					prop_assert_eq!(decoder.decode().unwrap(), Some(step.value.clone()), "step {}", index);
				}
				Ok(())
			})
			.unwrap();
	}

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
		let err = encoder.update(&Duplicate { duplicate: true }).err().expect("refused");
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
			Config::default().with_deltas(true),
			&[
				json!({ "a": 1, "b": 1 }),
				json!({ "a": 1, "b": 2 }),
				json!({ "a": 1, "b": 3 }),
			],
		);
		assert_eq!(frames.iter().map(|f| f.0).collect::<Vec<_>>(), vec![true, false, false]);
	}

	/// Without a joiner asking, a group runs on to the caps however much the deltas outweigh the
	/// snapshot.
	#[test]
	fn deltas_never_roll_on_their_own() {
		let big = "x".repeat(1024);
		let values: Vec<Value> = (0..32).map(|n| json!({ "n": n, "pad": format!("{n}{big}") })).collect();
		let frames = encode(Config::default(), &values);
		assert_eq!(frames.iter().filter(|f| f.0).count(), 1);
	}

	/// `snapshot` restates the retained value as a keyframe, and the next change is a delta in the
	/// group it opens.
	#[test]
	fn snapshot_restates_the_retained_value() {
		for deltas in [true, false] {
			let mut encoder = Encoder::<Value>::new(Config::default().with_deltas(deltas));
			assert!(encoder.snapshot().unwrap().is_none(), "nothing to restate yet");

			commit(&mut encoder, &json!({ "a": 1 })).unwrap();
			commit(&mut encoder, &json!({ "a": 2 })).unwrap();

			let frame = encoder.snapshot().unwrap().unwrap();
			assert!(frame.keyframe);
			let mut decoder = super::super::Decoder::<Value>::new(super::super::consumer::Config::default());
			decoder.snapshot(&frame.payload).unwrap();
			frame.commit();
			assert_eq!(decoder.decode().unwrap(), Some(json!({ "a": 2 })));

			let next = commit(&mut encoder, &json!({ "a": 3 })).unwrap();
			assert_eq!(next.keyframe, !deltas, "deltas={deltas}");
			match next.keyframe {
				true => decoder.snapshot(&next.payload).unwrap(),
				false => decoder.delta(&next.payload).unwrap(),
			}
			assert_eq!(decoder.decode().unwrap(), Some(json!({ "a": 3 })));
		}
	}

	/// A restatement that never reached the wire resynchronizes like any other lost frame.
	#[test]
	fn an_uncommitted_snapshot_resynchronizes() {
		let mut encoder = Encoder::<Value>::new(Config::default());
		commit(&mut encoder, &json!({ "a": 1 })).unwrap();
		drop(encoder.snapshot().unwrap().unwrap());
		assert!(commit(&mut encoder, &json!({ "a": 2 })).unwrap().keyframe);
	}

	#[test]
	fn deltas_off_forces_a_keyframe_per_change() {
		let frames = encode(
			Config::default().with_deltas(false),
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
			Config::default().with_deltas(false),
			&[json!({ "a": 1 }), json!({ "a": 1 }), json!({ "a": 1 })],
		);
		assert_eq!(frames.len(), 1);
	}

	/// Field order is part of the encoding, so a byte baseline only answers "unchanged" correctly
	/// because `T` serializes deterministically. Same keys, different values, must still emit.
	#[test]
	fn deltas_off_detects_a_change_under_the_same_keys() {
		let frames = encode(
			Config::default().with_deltas(false),
			&[json!({ "a": 1, "b": 2 }), json!({ "a": 1, "b": 3 })],
		);
		assert_eq!(frames.len(), 2);
	}

	/// The byte baseline is parsed on demand, so `value` (and so `Producer::modify`, which seeds an
	/// edit from it) keeps working with deltas off. Dropping the baseline instead would make
	/// `modify` start from `T::default()` and publish a document with every other field missing.
	#[test]
	fn deltas_off_still_exposes_the_value() {
		let mut encoder = Encoder::<Value>::new(Config::default().with_deltas(false));
		assert_eq!(encoder.value(), None);

		commit(&mut encoder, &json!({ "a": 1, "b": 2 })).unwrap();
		assert_eq!(encoder.value(), Some(&json!({ "a": 1, "b": 2 })));

		commit(&mut encoder, &json!({ "a": 1, "b": 3 })).unwrap();
		assert_eq!(encoder.value(), Some(&json!({ "a": 1, "b": 3 })));
	}

	/// A value the caller might reasonably expect to be a delta, but that merge patch can't express:
	/// setting a field to JSON null reads as a key deletion. The encoder has to override the caller
	/// here, which is why `keyframe` is a return value rather than a parameter.
	#[test]
	fn a_null_field_forces_a_keyframe() {
		let frames = encode(
			Config::default().with_deltas(true),
			&[json!({ "a": 1, "b": 1 }), json!({ "a": 1, "b": null })],
		);
		assert_eq!(frames.iter().map(|f| f.0).collect::<Vec<_>>(), vec![true, true]);
	}

	/// Same story for a root that isn't an object: there is no recursive merge patch for it.
	#[test]
	fn a_non_object_root_forces_a_keyframe() {
		let frames = encode(
			Config::default().with_deltas(true),
			&[json!({ "a": 1 }), json!([1, 2, 3])],
		);
		assert_eq!(frames.iter().map(|f| f.0).collect::<Vec<_>>(), vec![true, true]);
	}

	#[test]
	fn frame_cap_forces_a_keyframe() {
		let values: Vec<Value> = (0..=MAX_DELTA_FRAMES).map(|n| json!({ "n": n })).collect();
		let frames = encode(Config::default().with_deltas(true), &values);

		// The snapshot plus MAX_DELTA_FRAMES - 1 deltas fill the group, then the cap rolls it.
		assert_eq!(frames.len(), MAX_DELTA_FRAMES + 1);
		assert_eq!(frames.iter().filter(|f| f.0).count(), 2);
		assert!(frames[MAX_DELTA_FRAMES].0);
	}

	/// A caller that cuts the group behind the encoder's back has to say so, or the next value would
	/// be a delta against a window and a baseline the new group never carried.
	#[test]
	fn reset_forces_the_next_update_to_be_a_keyframe() {
		let mut encoder = Encoder::<Value>::new(Config::default().with_deltas(true));
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
		let mut encoder = Encoder::<Value>::new(Config::default().with_deltas(true));
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

	/// A delta is only readable while its group still holds the snapshot, so one that would push the
	/// group past its byte budget rolls a fresh snapshot instead of joining it.
	#[test]
	fn a_delta_past_the_budget_rolls_a_snapshot() {
		let value = json!({ "v": "x".repeat(1000) });
		let patched = json!({ "v": "y".repeat(1000) });

		let budget = (serde_json::to_vec(&value).unwrap().len() + 500) as u64;
		let mut encoder = Encoder::<Value>::new(Config::default().with_max_group_bytes(budget));
		assert!(commit(&mut encoder, &value).unwrap().keyframe);
		let rolled = commit(&mut encoder, &patched).expect("a rolled snapshot");
		assert!(
			rolled.keyframe,
			"a 1 KB patch must not ride in a group with 500 bytes left"
		);
		assert_eq!(
			serde_json::from_slice::<Value>(&rolled.payload).unwrap(),
			patched,
			"the snapshot carries the whole value, not a patch"
		);

		// The rolled group is charged afresh, so a small change rides as a delta again.
		let next = commit(&mut encoder, &json!({ "v": "y".repeat(1000), "q": 1 })).unwrap();
		assert!(!next.keyframe);
	}

	/// A snapshot past the budget is refused before its baseline is parsed, with or without deltas.
	#[test]
	fn a_snapshot_past_the_budget_is_refused() {
		for deltas in [false, true] {
			let config = Config::default().with_deltas(deltas).with_max_group_bytes(100);
			let mut encoder = Encoder::<Value>::new(config);
			assert!(
				matches!(
					encoder.update(&json!({ "v": "x".repeat(1000) })),
					Err(crate::Error::Net(moq_net::Error::FrameTooLarge))
				),
				"deltas {deltas}"
			);
			assert!(
				encoder.value().is_none(),
				"a refused snapshot leaves the encoder as it was"
			);
		}
	}

	/// A budget above moq-net's cache limit is clamped to it.
	#[test]
	fn the_budget_is_clamped_to_the_cache_limit() {
		let encoder = Encoder::<Value>::new(Config::default().with_max_group_bytes(u64::MAX));
		assert_eq!(encoder.max_group_bytes, moq_net::group::MAX_CACHE_BYTES);
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
		let mut encoder = Encoder::new(Config::default().with_deltas(true));
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

		let config = Config::default().with_deltas(true);
		let (plain, repeat) = (Doc { repeat: false }, Doc { repeat: true });

		// A new entry: the first diff after a snapshot has nothing memoized yet.
		let mut encoder = Encoder::<Doc>::new(config.clone());
		encoder.update(&plain).unwrap().expect("a snapshot").commit();
		let err = encoder.update(&repeat).err().expect("refused");
		assert!(err.to_string().contains("duplicate JSON object key"), "{err}");

		// A memoized entry whose scalar becomes an object.
		let mut encoder = Encoder::<Doc>::new(config);
		encoder.update(&plain).unwrap().expect("a snapshot").commit();
		assert!(encoder.update(&plain).unwrap().is_none(), "unchanged, now memoized");
		let err = encoder.update(&repeat).err().expect("refused");
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
		for seed in 1..=3 {
			let config = Config::default().with_deltas(true);
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
		let mut encoder = Encoder::<Value>::new(Config::default().with_deltas(true));
		assert_eq!(encoder.value(), None);

		commit(&mut encoder, &json!({ "a": 1, "b": 1 }));
		commit(&mut encoder, &json!({ "a": 1, "b": 2 }));

		// The delta was folded into the baseline, so it reflects what was actually published.
		assert_eq!(encoder.value(), Some(&json!({ "a": 1, "b": 2 })));

		// A frame that never reached the wire leaves the value at the last one that did.
		drop(encoder.update(&json!({ "a": 1, "b": 3 })).unwrap());
		assert_eq!(encoder.value(), Some(&json!({ "a": 1, "b": 2 })));
	}
}
