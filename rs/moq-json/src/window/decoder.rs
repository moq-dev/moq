//! The track-free half of window consumption: frame payloads in, window events out.

use std::collections::VecDeque;

use serde::de::DeserializeOwned;

use super::encoder::MAX_INDEX;
use super::op::{Header, Op};
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
	Push {
		/// Absolute index assigned to the record.
		index: u64,
		/// The decoded record.
		value: T,
	},

	/// These records left the window.
	Pop(std::ops::Range<u64>),

	/// These records existed but will never be delivered: they were pushed and dropped while this
	/// consumer was behind.
	Skip(std::ops::Range<u64>),
}

/// An event ready to return, or a header's unseen records waiting to become push events.
enum Queued<T> {
	Event(Event<T>),
	Push { index: u64, records: std::vec::IntoIter<T> },
}

/// Decodes one MoQ group's frames while borrowing the continuous window state.
pub struct Group<'a, T> {
	decoder: &'a mut Decoder<T>,
	codec: Codec,
}

/// Group-local decoding state used by both [`Group`] and [`Consumer`](super::Consumer).
pub(super) struct Codec {
	/// The group's DEFLATE decoder, `Some` while reading compressed frames.
	flate: Option<moq_flate::Decoder>,

	/// Whether the required frame-zero header has been decoded.
	positioned: bool,
}

impl Codec {
	pub(super) fn new() -> Self {
		Self {
			flate: None,
			positioned: false,
		}
	}
}

/// Reconstructs window events from frame payloads.
///
/// The track-free core of [`Consumer`](super::Consumer). It tracks indices, not contents: it knows
/// where the window starts and how far it has delivered, which is all it needs to turn a header into
/// the pushes, pops, and skips the reader has not already been told about.
///
/// Group rolls are invisible here on purpose. A header restates the window, and this decoder emits
/// only what is new, so a reader sees one continuous stream of edits no matter how often the
/// publisher rolled for compression's sake.
pub struct Decoder<T> {
	config: ConsumerConfig,

	/// Absolute index of the window's front, once a group header has positioned us.
	front: u64,

	/// Records currently in the window.
	len: u64,

	/// Next index to deliver, or `None` before the first header. A fresh consumer adopts the first
	/// header's offset rather than skipping everything that came before it.
	delivered: Option<u64>,

	/// Events produced by the frames decoded so far, oldest first.
	events: VecDeque<Queued<T>>,
}

impl<T> Decoder<T> {
	/// Create a decoder that has not yet been positioned by a group header.
	pub fn new(config: ConsumerConfig) -> Self {
		Self {
			config,
			front: 0,
			len: 0,
			delivered: None,
			events: VecDeque::new(),
		}
	}

	/// Borrow this decoder for one MoQ group.
	pub fn group(&mut self) -> Group<'_, T> {
		Group {
			decoder: self,
			codec: Codec::new(),
		}
	}

	/// Take the next event produced by the frames decoded so far.
	///
	/// Returns `None` once the queue is drained, which is a request for more frames rather than the
	/// end of anything: [`Group::decode`] refills it. Deliberately not [`Iterator`], whose
	/// `None` a caller would reasonably read as exhausted.
	pub fn next_event(&mut self) -> Option<Event<T>> {
		match self.events.pop_front()? {
			Queued::Event(event) => Some(event),
			Queued::Push { index, mut records } => {
				let value = records.next().expect("queued push batch is not empty");
				if !records.as_slice().is_empty() {
					self.events.push_front(Queued::Push {
						index: index + 1,
						records,
					});
				}
				Some(Event::Push { index, value })
			}
		}
	}

	/// Absolute index of the oldest record in the window, and of the next to arrive.
	pub fn range(&self) -> std::ops::Range<u64> {
		self.front..self.front + self.len
	}
}

impl<T: DeserializeOwned> Decoder<T> {
	/// Decode one frame, queueing the events it implies.
	pub(super) fn decode(&mut self, group: &mut Codec, payload: &[u8]) -> Result<()> {
		let inflated = match self.config.compression {
			true => Some(group.flate.get_or_insert_with(moq_flate::Decoder::new).frame(payload)?),
			false => None,
		};
		let bytes = inflated.as_deref().unwrap_or(payload);

		if !group.positioned {
			let header: Header<T> = serde_path_to_error::deserialize(&mut serde_json::Deserializer::from_slice(bytes))
				.map_err(|err| Error::Json(err.to_string()))?;
			self.apply_header(header.offset, header.start.unwrap_or(header.offset), header.records)?;
			group.positioned = true;
			return Ok(());
		}

		match serde_path_to_error::deserialize(&mut serde_json::Deserializer::from_slice(bytes))
			.map_err(|err| Error::Json(err.to_string()))?
		{
			Op::Push(record) => self.apply_push(record),
			Op::Pop(count) => self.apply_pop(count),
		}
	}

	/// Apply a logical window range and its decodable suffix.
	fn apply_header(&mut self, offset: u64, start: u64, records: Vec<T>) -> Result<()> {
		if offset > MAX_INDEX {
			return Err(Error::Json("window offset exceeds the safe integer range".into()));
		}
		if start < offset {
			return Err(Error::Json("window checkpoint starts before its offset".into()));
		}
		let len = u64::try_from(records.len()).map_err(|_| Error::Json("window length exceeds u64".into()))?;
		let end = start
			.checked_add(len)
			.filter(|end| *end <= MAX_INDEX)
			.ok_or_else(|| Error::Json("window range exceeds the safe integer range".into()))?;

		let delivered = match self.delivered {
			// First position: adopt the publisher's offset rather than skipping all of history.
			None => {
				if offset < start {
					self.events.push_back(Queued::Event(Event::Skip(offset..start)));
				}
				offset
			}
			Some(delivered) => {
				if offset < self.front || end < delivered {
					return Err(Error::Json("window header moved backwards".into()));
				}

				// Records that left the window while we were away. Those we had delivered are pops; those
				// we never saw are skips. Keep each gap compact: the offset is untrusted and may jump by
				// far more indices than a consumer could materialize individually.
				let popped = self.front..delivered.min(offset);
				if !popped.is_empty() {
					self.events.push_back(Queued::Event(Event::Pop(popped)));
				}
				let skipped = delivered..start;
				if !skipped.is_empty() {
					self.events.push_back(Queued::Event(Event::Skip(skipped)));
				}
				delivered
			}
		};

		// Keep the unseen tail as one batch and materialize each push only when the caller asks for it.
		let skip = usize::try_from(delivered.saturating_sub(start))
			.map_err(|_| Error::Json("window length exceeds usize".into()))?;
		let mut records = records.into_iter();
		if skip > 0 {
			records.nth(skip - 1);
		}
		if !records.as_slice().is_empty() {
			self.events.push_back(Queued::Push {
				index: start + skip as u64,
				records,
			});
		}

		self.front = offset;
		self.len = end - offset;
		self.delivered = Some(delivered.max(end));
		Ok(())
	}

	/// One record joined the back.
	fn apply_push(&mut self, record: T) -> Result<()> {
		let delivered = self.delivered.expect("group header positioned the decoder");

		let index = self
			.front
			.checked_add(self.len)
			.ok_or_else(|| Error::Json("window range exceeds u64".into()))?;
		let end = index
			.checked_add(1)
			.filter(|end| *end <= MAX_INDEX)
			.ok_or_else(|| Error::Json("window range exceeds the safe integer range".into()))?;
		self.len = end - self.front;

		if index >= delivered {
			self.events
				.push_back(Queued::Event(Event::Push { index, value: record }));
			self.delivered = Some(end);
		}

		Ok(())
	}

	/// Records left the front.
	fn apply_pop(&mut self, count: u64) -> Result<()> {
		let delivered = self.delivered.expect("group header positioned the decoder");
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
			self.events.push_back(Queued::Event(Event::Pop(popped)));
		}
		let skipped = delivered.max(self.front)..end;
		if !skipped.is_empty() {
			self.events.push_back(Queued::Event(Event::Skip(skipped)));
		}

		self.front = end;
		self.len -= count;
		self.delivered = Some(delivered.max(self.front));

		Ok(())
	}
}

impl<T> Group<'_, T> {
	/// Take the next event produced by this group's frames so far.
	pub fn next_event(&mut self) -> Option<Event<T>> {
		self.decoder.next_event()
	}

	/// Absolute index of the oldest record in the window, and of the next to arrive.
	pub fn range(&self) -> std::ops::Range<u64> {
		self.decoder.range()
	}
}

impl<T: DeserializeOwned> Group<'_, T> {
	/// Decode the next frame in this group.
	pub fn decode(&mut self, payload: &[u8]) -> Result<()> {
		self.decoder.decode(&mut self.codec, payload)
	}
}
