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
/// joiner can always read the header at frame 0, and so a roll always precedes
/// [`moq_net::Error::GroupTooLarge`].
pub(super) const MAX_GROUP_FRAMES: usize = 1024;

/// Largest index represented exactly by both Rust and JavaScript implementations.
pub(super) const MAX_INDEX: u64 = (1 << 53) - 1;

/// Configuration for an [`Encoder`] and the [`Producer`](super::Producer) wrapping one.
///
/// Build from [`Default`] and override fields (the struct is `#[non_exhaustive]`, so new options
/// stay additive), or chain the `with_*` setters.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ProducerConfig {
	/// Whether edits ride as push and pop ops in the open group.
	///
	/// `false` makes every edit its own single-frame group restating the window. The window's
	/// counterpart to [`snapshot::Config::deltas`](crate::snapshot::Config::deltas). How long a
	/// group of ops runs is not an option: it grows until a joiner wants a fresh one, judged by the
	/// track's [`refresh`](moq_net::track::Info::refresh) ratio, or until it reaches the frame or
	/// byte cap.
	///
	/// Defaults to `true`.
	pub ops: bool,

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
			ops: true,
			checkpoint_records: None,
		}
	}
}

impl ProducerConfig {
	/// Set [`ops`](Self::ops) (a builder, since the struct is `#[non_exhaustive]`).
	pub fn with_ops(mut self, ops: bool) -> Self {
		self.ops = ops;
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
	/// The frame payload: a group header or an op.
	pub payload: Bytes,

	/// Whether this frame is a group header, which must open a new group.
	///
	/// The encoder decides this for an edit: the group's frame and byte caps force a new group
	/// independently of which edit was requested. A caller asks for one with
	/// [`snapshot`](Encoder::snapshot).
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
	/// A header restating the window as it is, which changes nothing.
	Restate,
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
/// [`keyframe`](Encoded::keyframe) set must open a new group: the positional indices depend on it.
/// When a joiner wants a fresh group (see
/// [`Demand::wanted`](moq_net::track::Demand::wanted)), [`snapshot`](Self::snapshot) restates the
/// window in one.
pub struct Encoder<T> {
	config: ProducerConfig,

	/// The decodable checkpoint suffix. With no checkpoint bound this is the complete window.
	window: VecDeque<Value>,

	/// Absolute index of the oldest logically retained record.
	offset: u64,

	/// Absolute index of `window.front()`, which may follow `offset` in checkpoint mode.
	start: u64,

	/// Bytes charged to the current group, header included.
	group_bytes: u64,

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
			group_bytes: 0,
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

	/// Discard group-local state, so the next edit opens a new group with a header.
	///
	/// Call this whenever the caller closes the current group behind the encoder's back. It is also
	/// how the encoder recovers after an encoded frame did not reach the wire.
	pub fn reset(&mut self) {
		self.group_bytes = 0;
		self.group_frames = 0;
		self.resync = true;
	}

	/// Restate the retained window in a header, which must open a new group.
	///
	/// For a caller that opens groups itself: call it when a joiner wants a fresh group, such as when
	/// [`Demand::wanted`](moq_net::track::Demand::wanted) passes the largest group written, so the
	/// joiner reads the window without the ops before it. The frame is a [`Pending`] like any other.
	pub fn snapshot(&mut self) -> Result<Pending<'_, T>> {
		let bytes = Self::header(
			&self.config,
			self.offset,
			self.start,
			self.window.len(),
			self.window.iter(),
		)?;
		let encoded = self.emit_header(bytes)?;
		Ok(self.pending(encoded, Edit::Restate))
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
			Edit::Restate => {}
		}
	}

	/// Whether the pending edit may ride as an op in the open group: ops are on and the group is
	/// under its frame cap. The byte cap is checked once the op is encoded.
	fn op_allowed(&self) -> bool {
		self.config.ops && self.group_frames > 0 && self.group_frames < MAX_GROUP_FRAMES
	}

	/// Emit an op when the header will remain cached, otherwise restate the window in a new group.
	fn emit_op(&mut self, bytes: Vec<u8>) -> Result<Option<Encoded>> {
		let payload = Bytes::from(bytes);
		self.group_frames += 1;
		self.group_bytes = self.group_bytes.saturating_add(payload.len() as u64);
		if self.group_bytes > moq_net::group::MAX_CACHE_BYTES {
			self.reset();
			return Ok(None);
		}
		Ok(Some(Encoded {
			payload,
			keyframe: false,
		}))
	}

	/// Serialize a bounded suffix of the window as a group header.
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
		let payload = Bytes::from(bytes);
		if payload.len() as u64 > moq_net::group::MAX_CACHE_BYTES {
			return Err(Error::Json("window header exceeds the group cache limit".into()));
		}

		self.group_bytes = payload.len() as u64;
		self.group_frames = 1;
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
	/// when the group is full or a frame was lost.
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
		let mut encoder = Encoder::<String>::new(ProducerConfig::default().with_ops(true));
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
}
