//! The track-free half of window publishing: window edits in, frame payloads out.

use std::collections::VecDeque;
use std::marker::PhantomData;

use bytes::Bytes;
use serde::Serialize;
use serde_json::Value;

use super::op::Op;
use crate::Result;

/// Frames (reset included) in one group before a reset is forced, matching
/// [`snapshot`](crate::snapshot)'s cap. Kept well below moq-net's per-group frame cap so a late
/// joiner can always read the reset at frame 0.
pub(super) const MAX_GROUP_FRAMES: usize = 256;

/// Configuration for an [`Encoder`] and the [`Producer`](super::Producer) wrapping one.
///
/// Build from [`Default`] and override fields (the struct is `#[non_exhaustive]`, so new options
/// stay additive), or chain the `with_*` setters.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ProducerConfig {
	/// How much the ops in a group may cost before a fresh reset is emitted.
	///
	/// A new group opens once the pushes and pops *already written* exceed `op_ratio` times the
	/// size of the group's reset frame. The pending op is excluded from that check, so the one that
	/// tips the group over budget still lands: a group overshoots by at most one op before rolling.
	/// `0` disables ops entirely, so every edit is its own single-frame reset.
	///
	/// This is the window's counterpart to
	/// [`snapshot::ProducerConfig::delta_ratio`](crate::snapshot::ProducerConfig::delta_ratio), and
	/// the same trade: a bigger ratio spends less on resets and makes a late joiner read more ops.
	///
	/// Defaults to `8`.
	pub op_ratio: u32,

	/// Compress each group as one sync-flushed DEFLATE stream, so every op reuses the reset and the
	/// ops before it as context.
	///
	/// `false` (the default) emits plaintext JSON frames. A [`Decoder`](super::Decoder) reading them
	/// must set [`ConsumerConfig::compression`](super::ConsumerConfig::compression) to match.
	pub compression: bool,
}

impl Default for ProducerConfig {
	fn default() -> Self {
		Self {
			op_ratio: 8,
			compression: false,
		}
	}
}

impl ProducerConfig {
	/// Set [`op_ratio`](Self::op_ratio) (a builder, since the struct is `#[non_exhaustive]`).
	pub fn with_op_ratio(mut self, op_ratio: u32) -> Self {
		self.op_ratio = op_ratio;
		self
	}

	/// Set [`compression`](Self::compression) (a builder, since the struct is `#[non_exhaustive]`).
	pub fn with_compression(mut self, compression: bool) -> Self {
		self.compression = compression;
		self
	}
}

/// One encoded frame, and the group boundary it implies.
#[derive(Clone, Debug)]
pub struct Encoded {
	/// The frame payload, DEFLATE-compressed when [`ProducerConfig::compression`] is set.
	pub payload: Bytes,

	/// Whether this frame is a reset, which must open a new group.
	///
	/// The encoder decides this, never the caller: the op budget and the frame cap force a reset
	/// independently of which edit was requested.
	pub keyframe: bool,
}

/// An encoded frame the caller has not yet acknowledged writing.
///
/// Write the frame, then [`commit`](Self::commit). A frame that never reaches the wire leaves the
/// consumer's view behind the producer's window, so dropping it uncommitted
/// [`reset`](Encoder::reset)s the encoder and the next frame restates the whole window.
///
/// The window itself is not rolled back. It is the producer's truth, and the edit really happened;
/// only the consumer's knowledge of it is lost, which the next reset repairs.
#[must_use = "write the frame, then commit it"]
pub struct Pending<'a, T> {
	encoder: &'a mut Encoder<T>,
	encoded: Encoded,
	committed: bool,
}

impl<T> std::ops::Deref for Pending<'_, T> {
	type Target = Encoded;

	fn deref(&self) -> &Encoded {
		&self.encoded
	}
}

impl<T> Pending<'_, T> {
	/// Acknowledge that the frame reached the wire, keeping the encoder's state.
	///
	/// Only call this once the write has actually succeeded.
	pub fn commit(mut self) {
		self.committed = true;
	}
}

impl<T> Drop for Pending<'_, T> {
	fn drop(&mut self) {
		if !self.committed {
			self.encoder.reset();
		}
	}
}

/// Encodes window edits into frame payloads, deciding where the group boundaries fall.
///
/// The track-free core of [`Producer`](super::Producer). It owns the retained window, so it can
/// restate it whenever a group rolls; that restatement is the whole point of the mode, and is what
/// an append-only log cannot do.
///
/// Frames must reach the wire in the order they were encoded, and a frame with
/// [`keyframe`](Encoded::keyframe) set must open a new group: both the positional indices and the
/// group-scoped DEFLATE window depend on it.
pub struct Encoder<T> {
	config: ProducerConfig,

	/// The retained window. Records are stored decoded so a reset can restate them, and so a record
	/// serializes identically whether it reaches a reader as a push or in a later reset.
	window: VecDeque<Value>,

	/// Absolute index of `window.front()`. Only a reset puts this on the wire.
	offset: u64,

	/// The current group's DEFLATE encoder (one window per group), `Some` while compressing.
	flate: Option<moq_flate::Encoder>,

	/// Bytes of pushes and pops emitted into the current group, excluding its reset frame.
	op_bytes: u64,

	/// Reference size the op budget is measured against: the current group's reset frame.
	reset_len: u64,

	/// Frames emitted into the current group, reset included.
	group_frames: usize,

	/// Whether the next frame must be a reset, because a frame was lost or the caller rolled the
	/// group. Kept separate from the window, which a resync must never discard.
	resync: bool,

	_marker: PhantomData<fn(T)>,
}

impl<T> Encoder<T> {
	/// Create an encoder with an empty window, so the first edit emits a reset.
	pub fn new(config: ProducerConfig) -> Self {
		Self {
			config,
			window: VecDeque::new(),
			offset: 0,
			flate: None,
			op_bytes: 0,
			reset_len: 0,
			group_frames: 0,
			resync: true,
			_marker: PhantomData,
		}
	}

	/// The retained window, oldest first.
	///
	/// The encoder holds this to restate it on a roll, so a caller needs no parallel copy.
	pub fn window(&self) -> impl ExactSizeIterator<Item = &Value> {
		self.window.iter()
	}

	/// Absolute index of the oldest retained record, and of the next one to be pushed.
	pub fn range(&self) -> std::ops::Range<u64> {
		self.offset..self.offset + self.window.len() as u64
	}

	/// Force the next frame to be a reset.
	///
	/// Call this whenever the caller closes the current group behind the encoder's back. Without it
	/// the next frame may be a push against a DEFLATE window and an index base the new group does
	/// not carry.
	///
	/// The window survives: the reset restates it in full anyway.
	pub fn reset(&mut self) {
		self.flate = None;
		self.op_bytes = 0;
		self.reset_len = 0;
		self.group_frames = 0;
		self.resync = true;
	}

	/// Whether the pending edit may ride as an op in the open group.
	fn op_allowed(&self) -> bool {
		let ratio = u64::from(self.config.op_ratio);
		ratio != 0
			&& self.group_frames > 0
			&& self.group_frames < MAX_GROUP_FRAMES
			&& self.op_bytes <= ratio * self.reset_len
	}

	/// Compress an already-serialized op into the open group, charging it to the budget.
	fn frame(&mut self, bytes: Vec<u8>) -> Encoded {
		let payload = match self.flate.as_mut() {
			Some(flate) => flate.frame(&bytes),
			None => Bytes::from(bytes),
		};

		self.op_bytes += payload.len() as u64;
		self.group_frames += 1;

		Encoded {
			payload,
			keyframe: false,
		}
	}

	/// Encode a reset restating the whole window, opening a new group.
	fn emit_reset(&mut self) -> Result<Encoded> {
		let records: Vec<&Value> = self.window.iter().collect();
		let bytes = serde_json::to_vec(&Op::Reset {
			offset: self.offset,
			records,
		})?;

		// Open a fresh per-group encoder (cold window) and compress the reset as frame 0, recording
		// its wire size as the op budget's anchor.
		let (payload, flate) = match self.config.compression {
			true => {
				let mut flate = moq_flate::Encoder::new();
				let payload = flate.frame(&bytes);
				(payload, Some(flate))
			}
			false => (Bytes::from(bytes), None),
		};

		self.reset_len = payload.len() as u64;
		self.op_bytes = 0;
		self.group_frames = 1;
		self.flate = flate;
		self.resync = false;

		Ok(Encoded {
			payload,
			keyframe: true,
		})
	}

	/// Drop `count` records from the front of the window.
	///
	/// Returns `None` when there is nothing to drop, so a caller can trim unconditionally. Emits a
	/// pop into the open group, or a reset restating what is left.
	pub fn pop(&mut self, count: u64) -> Result<Option<Pending<'_, T>>> {
		let count = count.min(self.window.len() as u64);
		if count == 0 {
			return Ok(None);
		}

		self.window.drain(..count as usize);
		self.offset += count;

		let encoded = match self.resync || !self.op_allowed() {
			true => self.emit_reset()?,
			false => {
				let bytes = serde_json::to_vec(&Op::<&Value>::Pop(count))?;
				self.frame(bytes)
			}
		};

		Ok(Some(self.pending(encoded)))
	}

	/// Wrap an encoded frame so the caller has to say whether it reached the wire.
	fn pending(&mut self, encoded: Encoded) -> Pending<'_, T> {
		Pending {
			encoder: self,
			encoded,
			committed: false,
		}
	}
}

impl<T: Serialize> Encoder<T> {
	/// Append one record to the back of the window.
	///
	/// Emits a push into the open group, or a reset restating the window (the new record included)
	/// when the op budget is spent or a frame was lost.
	pub fn push(&mut self, value: &T) -> Result<Pending<'_, T>> {
		// Serialize before touching the window, so a value that can't be encoded leaves the encoder
		// exactly as it was. Reading the record back out of its own bytes keeps the stored copy
		// identical to what a push would have put on the wire.
		let bytes = serde_json::to_vec(value)?;
		let record: Value = serde_json::from_slice(&bytes)?;

		self.window.push_back(record);

		let encoded = match self.resync || !self.op_allowed() {
			true => self.emit_reset()?,
			false => {
				// Serialize the record out of the window rather than the caller's value, so its bytes
				// match what a later reset would restate.
				let bytes = serde_json::to_vec(&Op::Push(self.window.back().expect("just pushed")))?;
				self.frame(bytes)
			}
		};

		Ok(self.pending(encoded))
	}
}
