//! The track-free half of window consumption: frame payloads in, window events out.

use std::collections::VecDeque;

use serde::de::DeserializeOwned;

use super::op::Op;
use crate::{Error, Result};

/// Configuration for a [`Decoder`], and so for the [`Consumer`](super::Consumer) wrapping one.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ConsumerConfig {
	/// Read frames written with
	/// [`ProducerConfig::compression`](super::ProducerConfig::compression) on.
	pub compression: bool,
}

impl ConsumerConfig {
	/// Set [`compression`](Self::compression) (a builder, since the struct is `#[non_exhaustive]`).
	pub fn with_compression(mut self, compression: bool) -> Self {
		self.compression = compression;
		self
	}
}

/// One change to the window, as the consumer sees it.
///
/// A record is `Push`ed when it first reaches this consumer. Contiguous ranges are `Pop`ped when
/// they leave the window or `Skip`ped when they were dropped before this consumer saw them.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Event<T> {
	/// This record joined the window, at this absolute index.
	Push(u64, T),

	/// These records left the window.
	Pop(std::ops::Range<u64>),

	/// These records existed but will never be delivered: they were pushed and dropped while this
	/// consumer was behind.
	Skip(std::ops::Range<u64>),
}

/// Reconstructs window events from frame payloads.
///
/// The track-free core of [`Consumer`](super::Consumer). It tracks indices, not contents: it knows
/// where the window starts and how far it has delivered, which is all it needs to turn a reset into
/// the pushes, pops, and skips the reader has not already been told about.
///
/// Group rolls are invisible here on purpose. A reset restates the window, and this decoder emits
/// only what is new, so a reader sees one continuous stream of edits no matter how often the
/// publisher rolled for compression's sake.
pub struct Decoder<T> {
	config: ConsumerConfig,

	/// The current group's DEFLATE decoder, `Some` while compressing.
	flate: Option<moq_flate::Decoder>,

	/// Absolute index of the window's front, once a reset has positioned us.
	front: u64,

	/// Records currently in the window.
	len: u64,

	/// Next index to deliver, or `None` before the first reset. A fresh consumer adopts the first
	/// reset's offset rather than skipping everything that came before it.
	delivered: Option<u64>,

	/// Whether the current group has opened with its required reset.
	positioned: bool,

	/// Events produced by the frames decoded so far, oldest first.
	events: VecDeque<Event<T>>,
}

impl<T> Decoder<T> {
	/// Create a decoder that has not yet been positioned by a reset.
	pub fn new(config: ConsumerConfig) -> Self {
		Self {
			config,
			flate: None,
			front: 0,
			len: 0,
			delivered: None,
			positioned: false,
			events: VecDeque::new(),
		}
	}

	/// Start a cold DEFLATE window, for a reader that has just moved to a new group.
	///
	/// Only the compression state resets. The index cursor deliberately survives: it is what lets
	/// the next group's reset report just the records this reader has not seen.
	pub fn reset(&mut self) {
		self.flate = None;
		self.positioned = false;
	}

	/// Take the next event produced by the frames decoded so far.
	///
	/// Returns `None` once the queue is drained, which is a request for more frames rather than the
	/// end of anything: [`decode`](Self::decode) refills it. Deliberately not [`Iterator`], whose
	/// `None` a caller would reasonably read as exhausted.
	pub fn next_event(&mut self) -> Option<Event<T>> {
		self.events.pop_front()
	}

	/// Absolute index of the oldest record in the window, and of the next to arrive.
	pub fn range(&self) -> std::ops::Range<u64> {
		self.front..self.front + self.len
	}
}

impl<T: DeserializeOwned> Decoder<T> {
	/// Decode one frame, queueing the events it implies.
	pub fn decode(&mut self, payload: &[u8]) -> Result<()> {
		let bytes = match self.config.compression {
			true => self.flate.get_or_insert_with(moq_flate::Decoder::new).frame(payload)?,
			false => bytes::Bytes::copy_from_slice(payload),
		};

		match serde_path_to_error::deserialize(&mut serde_json::Deserializer::from_slice(&bytes))
			.map_err(|err| Error::Json(err.to_string()))?
		{
			Op::Reset { offset, records } => self.apply_reset(offset, records),
			Op::Push(record) => self.apply_push(record),
			Op::Pop(count) => self.apply_pop(count),
		}
	}

	/// The window is exactly these records. Report what this reader missed, then what is new.
	fn apply_reset(&mut self, offset: u64, records: Vec<T>) -> Result<()> {
		if self.positioned {
			return Err(Error::Json("duplicate window reset in one group".into()));
		}

		let len = u64::try_from(records.len()).map_err(|_| Error::Json("window length exceeds u64".into()))?;
		let end = offset
			.checked_add(len)
			.ok_or_else(|| Error::Json("window range exceeds u64".into()))?;

		let delivered = match self.delivered {
			// First position: adopt the publisher's offset rather than skipping all of history.
			None => offset,
			Some(delivered) => {
				if offset < self.front || end < delivered {
					return Err(Error::Json("window reset moved backwards".into()));
				}

				// Records that left the window while we were away. Those we had delivered are pops; those
				// we never saw are skips. Keep each gap compact: the offset is untrusted and may jump by
				// far more indices than a consumer could materialize individually.
				let popped = self.front..delivered.min(offset);
				if !popped.is_empty() {
					self.events.push_back(Event::Pop(popped));
				}
				let skipped = delivered..offset;
				if !skipped.is_empty() {
					self.events.push_back(Event::Skip(skipped));
				}
				delivered
			}
		};

		// Deliver only the tail this reader has not seen; a reset that merely restates what it holds
		// yields nothing at all.
		for (index, record) in (offset..end).zip(records) {
			if index >= delivered {
				self.events.push_back(Event::Push(index, record));
			}
		}

		self.front = offset;
		self.len = end - offset;
		self.delivered = Some(delivered.max(end));
		self.positioned = true;

		Ok(())
	}

	/// One record joined the back.
	fn apply_push(&mut self, record: T) -> Result<()> {
		// A group always opens with a reset, so a push before one means we started mid-group.
		if !self.positioned {
			return Err(Error::MissingReset);
		}
		let delivered = self.delivered.ok_or(Error::MissingReset)?;

		let index = self
			.front
			.checked_add(self.len)
			.ok_or_else(|| Error::Json("window range exceeds u64".into()))?;
		let end = index
			.checked_add(1)
			.ok_or_else(|| Error::Json("window range exceeds u64".into()))?;
		self.len = end - self.front;

		if index >= delivered {
			self.events.push_back(Event::Push(index, record));
			self.delivered = Some(end);
		}

		Ok(())
	}

	/// Records left the front.
	fn apply_pop(&mut self, count: u64) -> Result<()> {
		if !self.positioned {
			return Err(Error::MissingReset);
		}
		let delivered = self.delivered.ok_or(Error::MissingReset)?;
		if count > self.len {
			return Err(Error::Json(format!(
				"pop of {count} exceeds the {} record(s) in the window",
				self.len
			)));
		}

		let end = self
			.front
			.checked_add(count)
			.ok_or_else(|| Error::Json("window range exceeds u64".into()))?;
		let popped = self.front..delivered.min(end);
		if !popped.is_empty() {
			self.events.push_back(Event::Pop(popped));
		}
		let skipped = delivered.max(self.front)..end;
		if !skipped.is_empty() {
			self.events.push_back(Event::Skip(skipped));
		}

		self.front = end;
		self.len -= count;
		self.delivered = Some(delivered.max(self.front));

		Ok(())
	}
}
