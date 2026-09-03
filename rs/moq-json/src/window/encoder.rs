//! The track-free half of window publishing: window edits in, frame payloads out.

use std::collections::VecDeque;
use std::marker::PhantomData;

use bytes::Bytes;
use serde::Serialize;
use serde_json::Value;

use super::op::{Header, Op};
use crate::{Error, Result};

/// Frames (header included) in one group before a new group is forced, matching
/// [`snapshot`](crate::snapshot)'s cap. Kept well below moq-net's per-group frame cap so a late
/// joiner can always read the header at frame 0.
pub(super) const MAX_GROUP_FRAMES: usize = 256;

/// Largest index represented exactly by both Rust and JavaScript implementations.
pub(super) const MAX_INDEX: u64 = (1 << 53) - 1;

/// Configuration for an [`Encoder`] and the [`Producer`](super::Producer) wrapping one.
///
/// Build from [`Default`] and override fields (the struct is `#[non_exhaustive]`, so new options
/// stay additive), or chain the `with_*` setters.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ProducerConfig {
	/// How much the ops in a group may cost before a fresh group is emitted.
	///
	/// A new group opens once the pushes and pops *already written* exceed `op_ratio` times the
	/// size of the group's header frame. The pending op is excluded from that check, so the one that
	/// tips the group over budget still lands: a group overshoots by at most one op before rolling.
	/// `0` disables ops entirely, so every edit is its own single-frame group.
	///
	/// This is the window's counterpart to
	/// [`snapshot::ProducerConfig::delta_ratio`](crate::snapshot::ProducerConfig::delta_ratio), and
	/// the same trade: a bigger ratio spends less on headers and makes a late joiner read more ops.
	///
	/// Defaults to `8`.
	pub op_ratio: u32,

	/// Compress each group as one sync-flushed DEFLATE stream, so every op reuses the header and the
	/// ops before it as context.
	///
	/// `false` (the default) emits plaintext JSON frames. A [`Decoder`](super::Decoder) reading them
	/// must set [`ConsumerConfig::compression`](super::ConsumerConfig::compression) to match.
	pub compression: bool,

	/// Maximum records retained and repeated in a group checkpoint.
	///
	/// `None` (the default) repeats the complete window. A bound keeps checkpoints finite for an
	/// unbounded window: readers following every group retain earlier records, while one joining a
	/// later group receives [`Event::Skip`](super::Event::Skip) for the omitted prefix.
	pub checkpoint_records: Option<usize>,
}

impl Default for ProducerConfig {
	fn default() -> Self {
		Self {
			op_ratio: 8,
			compression: false,
			checkpoint_records: None,
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

	/// Set [`checkpoint_records`](Self::checkpoint_records). Must be at least one.
	pub fn with_checkpoint_records(mut self, checkpoint_records: usize) -> Self {
		assert!(checkpoint_records > 0, "checkpoint_records must be positive");
		self.checkpoint_records = Some(checkpoint_records);
		self
	}
}

/// One encoded frame, and the group boundary it implies.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Encoded {
	/// The frame payload, DEFLATE-compressed when [`ProducerConfig::compression`] is set.
	pub payload: Bytes,

	/// Whether this frame is a group header, which must open a new group.
	///
	/// The encoder decides this, never the caller: the op budget and the frame cap force a new group
	/// independently of which edit was requested.
	pub keyframe: bool,
}

/// An encoded frame the caller has not yet acknowledged writing.
///
/// Write the frame, then [`commit`](Self::commit). The edit is staged until commit, so dropping the
/// frame leaves the window unchanged and makes the next frame open a new group.
#[must_use = "write the frame, then commit it"]
pub struct Pending<'a, T> {
	encoder: &'a mut Encoder<T>,
	encoded: Encoded,
	edit: Option<Edit>,
}

/// The window mutation staged behind a [`Pending`] frame.
enum Edit {
	Push(Value),
	Pop(u64),
}

impl<T> std::ops::Deref for Pending<'_, T> {
	type Target = Encoded;

	fn deref(&self) -> &Encoded {
		&self.encoded
	}
}

impl<T> Pending<'_, T> {
	/// Acknowledge that the frame reached the wire, applying its edit to the retained window.
	///
	/// Only call this once the write has actually succeeded.
	pub fn commit(mut self) {
		let edit = self.edit.take().expect("pending edit");
		self.encoder.commit(edit);
	}
}

impl<T> Drop for Pending<'_, T> {
	fn drop(&mut self) {
		if self.edit.is_some() {
			self.encoder.resync();
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

	/// The decodable checkpoint suffix. With no checkpoint bound this is the complete window.
	window: VecDeque<Value>,

	/// Absolute index of the oldest logically retained record.
	offset: u64,

	/// Absolute index of `window.front()`, which may follow `offset` in checkpoint mode.
	start: u64,

	/// The current group's DEFLATE encoder (one window per group), `Some` while compressing.
	flate: Option<moq_flate::Encoder>,

	/// Bytes of pushes and pops emitted into the current group, excluding its header frame.
	op_bytes: u64,

	/// Reference size the op budget is measured against: the current group's header frame.
	header_len: u64,

	/// Frames emitted into the current group, header included.
	group_frames: usize,

	/// Whether the next frame must be a header because a frame was lost. Kept separate from the
	/// window, which a resync must never discard.
	resync: bool,

	_marker: PhantomData<fn(T)>,
}

impl<T> Encoder<T> {
	/// Create an encoder with an empty window, so the first edit opens a group.
	pub fn new(config: ProducerConfig) -> Self {
		assert!(
			config.checkpoint_records != Some(0),
			"checkpoint_records must be positive"
		);
		Self {
			config,
			window: VecDeque::new(),
			offset: 0,
			start: 0,
			flate: None,
			op_bytes: 0,
			header_len: 0,
			group_frames: 0,
			resync: true,
			_marker: PhantomData,
		}
	}

	/// The retained checkpoint suffix, oldest first.
	///
	/// This is the complete window unless [`ProducerConfig::checkpoint_records`] is set.
	pub fn window(&self) -> Vec<Value> {
		self.window.iter().cloned().collect()
	}

	/// Absolute index of the oldest retained record, and of the next one to be pushed.
	pub fn range(&self) -> std::ops::Range<u64> {
		self.offset..self.start + self.window.len() as u64
	}

	/// Discard group-local state after an encoded frame did not reach the wire.
	fn resync(&mut self) {
		self.flate = None;
		self.op_bytes = 0;
		self.header_len = 0;
		self.group_frames = 0;
		self.resync = true;
	}

	/// Apply an edit after its encoded frame reached the wire.
	fn commit(&mut self, edit: Edit) {
		match edit {
			Edit::Push(record) => {
				self.window.push_back(record);
				if let Some(limit) = self.config.checkpoint_records {
					while self.window.len() > limit {
						self.window.pop_front();
						self.start += 1;
					}
				}
			}
			Edit::Pop(count) => {
				let offset = self.offset + count;
				let stored = offset.saturating_sub(self.start).min(self.window.len() as u64);
				self.window.drain(..stored as usize);
				self.start += stored;
				self.offset = offset;
			}
		}
	}

	/// Whether the pending edit may ride as an op in the open group.
	fn op_allowed(&self) -> bool {
		let ratio = u64::from(self.config.op_ratio);
		ratio != 0
			&& self.group_frames > 0
			&& self.group_frames < MAX_GROUP_FRAMES
			&& self.op_bytes <= ratio * self.header_len
	}

	/// Reject plaintext that the paired DEFLATE decoder could not produce.
	fn validate_plaintext(len: usize, kind: &str) -> Result<()> {
		if u64::try_from(len).unwrap_or(u64::MAX) > moq_flate::DEFAULT_MAX_FRAME_SIZE {
			return Err(Error::Json(format!(
				"window {kind} exceeds the decoder's decompressed size limit"
			)));
		}
		Ok(())
	}

	/// Compress an already-serialized op into the open group, charging it to the budget.
	fn frame(&mut self, bytes: Vec<u8>) -> Result<Encoded> {
		Self::validate_plaintext(bytes.len(), "frame")?;
		let payload = match self.flate.as_mut() {
			Some(flate) => flate.frame(&bytes),
			None => Bytes::from(bytes),
		};

		self.op_bytes += payload.len() as u64;
		self.group_frames += 1;

		Ok(Encoded {
			payload,
			keyframe: false,
		})
	}

	/// Emit an op when the header will remain cached, otherwise restate the window in a new group.
	fn emit_op(&mut self, bytes: Vec<u8>) -> Result<Option<Encoded>> {
		let encoded = self.frame(bytes)?;
		let group_bytes = self.header_len.saturating_add(self.op_bytes);
		if group_bytes > moq_net::group::MAX_CACHE_BYTES {
			self.resync();
			Ok(None)
		} else {
			Ok(Some(encoded))
		}
	}

	/// Serialize a bounded suffix before mutably borrowing the compression state.
	fn header<'a>(
		config: &ProducerConfig,
		offset: u64,
		start: u64,
		len: usize,
		records: impl Iterator<Item = &'a Value>,
	) -> Result<Vec<u8>> {
		let skip = config
			.checkpoint_records
			.map(|limit| len.saturating_sub(limit))
			.unwrap_or_default();
		let start = start
			.checked_add(skip as u64)
			.ok_or_else(|| Error::Json("window checkpoint exceeds u64".into()))?;
		let header = Header {
			offset,
			start: (start != offset).then_some(start),
			records: records.skip(skip).collect(),
		};
		Ok(serde_json::to_vec(&header)?)
	}

	/// Encode the header restating the whole window and opening a new group.
	fn emit_header(&mut self, bytes: Vec<u8>) -> Result<Encoded> {
		Self::validate_plaintext(bytes.len(), "header")?;

		// Open a fresh per-group encoder (cold window) and compress the header as frame 0, recording
		// its wire size as the op budget's anchor.
		let (payload, flate) = match self.config.compression {
			true => {
				let mut flate = moq_flate::Encoder::new();
				let payload = flate.frame(&bytes);
				(payload, Some(flate))
			}
			false => (Bytes::from(bytes), None),
		};
		if payload.len() as u64 > moq_net::group::MAX_CACHE_BYTES {
			return Err(Error::Json("window header exceeds the group cache limit".into()));
		}

		self.header_len = payload.len() as u64;
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
	/// pop into the open group, or a header restating what is left in a new group.
	pub fn pop(&mut self, count: u64) -> Result<Option<Pending<'_, T>>> {
		let count = count.min(self.range().end - self.offset);
		if count == 0 {
			return Ok(None);
		}

		let offset = self.offset + count;
		let stored = offset.saturating_sub(self.start).min(self.window.len() as u64) as usize;
		let start = self.start + stored as u64;
		let encoded = match self.resync || !self.op_allowed() {
			true => {
				let bytes = Self::header(
					&self.config,
					offset,
					start,
					self.window.len() - stored,
					self.window.iter().skip(stored),
				)?;
				self.emit_header(bytes)?
			}
			false => {
				let bytes = serde_json::to_vec(&Op::<&Value>::Pop(count))?;
				match self.emit_op(bytes)? {
					Some(encoded) => encoded,
					None => {
						let bytes = Self::header(
							&self.config,
							offset,
							start,
							self.window.len() - stored,
							self.window.iter().skip(stored),
						)?;
						self.emit_header(bytes)?
					}
				}
			}
		};

		Ok(Some(self.pending(encoded, Edit::Pop(count))))
	}

	/// Wrap an encoded frame so the caller has to say whether it reached the wire.
	fn pending(&mut self, encoded: Encoded, edit: Edit) -> Pending<'_, T> {
		Pending {
			encoder: self,
			encoded,
			edit: Some(edit),
		}
	}
}

impl<T: Serialize> Encoder<T> {
	/// Append one record to the back of the window.
	///
	/// Emits a push into the open group, or a header restating the window (the new record included)
	/// when the op budget is spent or a frame was lost.
	pub fn push(&mut self, value: &T) -> Result<Pending<'_, T>> {
		// Serialize before touching the window, so a value that can't be encoded leaves the encoder
		// exactly as it was. Reading the record back out of its own bytes keeps the stored copy
		// identical to what a push would have put on the wire.
		let bytes = serde_json::to_vec(value)?;
		let record: Value = serde_json::from_slice(&bytes)?;
		if self.range().end >= MAX_INDEX {
			return Err(crate::Error::Json("window index exceeds the safe integer range".into()));
		}

		let encoded = match self.resync || !self.op_allowed() {
			true => {
				let bytes = Self::header(
					&self.config,
					self.offset,
					self.start,
					self.window.len() + 1,
					self.window.iter().chain(std::iter::once(&record)),
				)?;
				self.emit_header(bytes)?
			}
			false => {
				let bytes = serde_json::to_vec(&Op::Push(&record))?;
				match self.emit_op(bytes)? {
					Some(encoded) => encoded,
					None => {
						let bytes = Self::header(
							&self.config,
							self.offset,
							self.start,
							self.window.len() + 1,
							self.window.iter().chain(std::iter::once(&record)),
						)?;
						self.emit_header(bytes)?
					}
				}
			}
		};

		Ok(self.pending(encoded, Edit::Push(record)))
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn an_op_that_would_evict_the_header_rolls_first() {
		let mut encoder = Encoder::<String>::new(ProducerConfig::default().with_op_ratio(u32::MAX));
		let first = "a".repeat(16 * 1024 * 1024);
		let next = "b".repeat(15 * 1024 * 1024);

		let frame = encoder.push(&first).unwrap();
		assert!(frame.keyframe);
		frame.commit();

		let frame = encoder.push(&next).unwrap();
		assert!(!frame.keyframe);
		frame.commit();

		let frame = encoder.pop(1).unwrap().unwrap();
		assert!(!frame.keyframe);
		frame.commit();

		let frame = encoder.push(&next).unwrap();
		assert!(frame.keyframe);
		assert!(frame.payload.len() < moq_net::group::MAX_CACHE_BYTES as usize);
		frame.commit();
	}

	#[test]
	fn an_uncommitted_edit_leaves_the_window_unchanged() {
		let mut encoder = Encoder::<u64>::new(ProducerConfig::default());

		drop(encoder.push(&1).unwrap());
		assert!(encoder.window().is_empty());

		let frame = encoder.push(&2).unwrap();
		assert!(frame.keyframe);
		frame.commit();
		assert_eq!(encoder.window(), vec![Value::from(2)]);

		drop(encoder.pop(1).unwrap().unwrap());
		assert_eq!(encoder.window(), vec![Value::from(2)]);
	}

	#[test]
	fn a_header_larger_than_the_group_cache_is_rejected() {
		let mut encoder = Encoder::<String>::new(ProducerConfig::default());
		let record = "x".repeat(moq_net::group::MAX_CACHE_BYTES as usize);

		let err = encoder.push(&record).err().expect("oversized header should fail");
		assert!(err.to_string().contains("group cache limit"));
		assert!(encoder.window().is_empty());

		let frame = encoder.push(&"ok".to_string()).unwrap();
		assert!(frame.keyframe);
	}

	#[test]
	fn plaintext_is_bounded_by_the_decoder_limit() {
		let len = usize::try_from(moq_flate::DEFAULT_MAX_FRAME_SIZE + 1).unwrap();
		assert!(Encoder::<()>::validate_plaintext(len, "frame").is_err());
	}
}
