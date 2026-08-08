//! Sliding-window JSON publishing over [`moq-net`](moq_net) tracks.
//!
//! An ordered log that grows at the tail and shrinks at the head, like an HLS media playlist:
//! [`Producer::append`] adds a record as content becomes available and [`Producer::trim`] drops
//! records from the head as it stops being available. The counterpart to [`stream`](crate::stream),
//! which only ever appends, and to [`snapshot`](crate::snapshot), which keeps no history at all.
//!
//! Every record has a stable **position**: the count of records ever appended before it. Positions
//! never repeat, so a record is identified across group rolls and trims, exactly like an HLS media
//! sequence number.
//!
//! On the wire each group is self-contained. Frame 0 is a snapshot of the live window
//! (`{"offset":N,"values":[...]}`) and each later frame is one operation (`{"append":V}` or
//! `{"trim":N}`). A new group rolls once the records trimmed since the snapshot outnumber those
//! still in the window, which bounds the total bytes at roughly twice an append-only log while
//! keeping every group independently readable.
//!
//! That rolling is what a [`stream`](crate::stream) track cannot do: its whole log rides one group,
//! so a consumer always starts at frame 0 and the track breaks (with
//! [`moq_net::Error::Lagged`]) once the earliest frames age out. Here a late joiner reads the
//! *newest* group and needs nothing older, so old groups can be dropped freely.

use std::collections::VecDeque;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};
use std::task::Poll;

use bytes::Bytes;
use moq_flate::{Decoder, Encoder};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::{Error, Result};

/// Maximum frames (snapshot plus operations) in one group before a snapshot is forced.
///
/// Kept well below moq-net's per-group frame cap so a late joiner can always read the snapshot at
/// frame 0 before the group is evicted.
const MAX_GROUP_FRAMES: usize = 256;

/// The largest position the wire format allows: 2^53-1.
///
/// The bound comes from the format, not from Rust: a receiver whose JSON numbers are IEEE 754
/// doubles (the usual case, and what the JS implementation uses) counts exactly only up to here.
/// Enforcing it also keeps the tail arithmetic well clear of a `u64` overflow, which would panic in
/// debug and silently wrap every later position in release.
const MAX_POSITION: u64 = (1 << 53) - 1;

/// Configuration for a window [`Producer`].
///
/// Build from [`Default`] and override fields (the struct is `#[non_exhaustive]`, so new options
/// stay additive), or chain the `with_*` setters.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ProducerConfig {
	/// Compress each group as one sync-flushed DEFLATE stream, so operations reuse the snapshot as
	/// context and shrink sharply.
	///
	/// `false` (the default) writes plaintext JSON frames. A [`Consumer`] reading the track must set
	/// [`ConsumerConfig::compression`] to match.
	pub compression: bool,
}

impl ProducerConfig {
	/// Set [`compression`](Self::compression) (a builder, since the struct is `#[non_exhaustive]`).
	pub fn with_compression(mut self, compression: bool) -> Self {
		self.compression = compression;
		self
	}
}

/// Configuration for a window [`Consumer`].
///
/// Build from [`Default`] and override fields (the struct is `#[non_exhaustive]`, so new options
/// stay additive), or chain the `with_*` setters.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ConsumerConfig {
	/// Whether the track's frames are DEFLATE-compressed. Must match the producer's
	/// [`ProducerConfig::compression`]. Defaults to `false`.
	pub compression: bool,
}

impl ConsumerConfig {
	/// Set [`compression`](Self::compression) (a builder, since the struct is `#[non_exhaustive]`).
	pub fn with_compression(mut self, compression: bool) -> Self {
		self.compression = compression;
		self
	}
}

/// The snapshot at frame 0 of every group, as written.
#[derive(Serialize)]
struct SnapshotOut<'a> {
	offset: u64,
	values: &'a VecDeque<Box<RawValue>>,
}

/// An append operation, as written.
#[derive(Serialize)]
struct AppendOut<'a> {
	append: &'a RawValue,
}

/// A trim operation, as written.
#[derive(Serialize)]
struct TrimOut {
	trim: usize,
}

/// The snapshot at frame 0 of every group, as read.
///
/// Values stay raw so only the records the consumer hasn't already seen are deserialized.
#[derive(Deserialize)]
struct SnapshotIn {
	offset: u64,
	values: Vec<Box<RawValue>>,
}

/// One operation frame, as read. Unknown members are ignored, so a frame carrying neither key is a
/// no-op rather than an error (a forward-compatible extension).
#[derive(Deserialize)]
struct OpIn {
	#[serde(default, deserialize_with = "present")]
	append: Option<Box<RawValue>>,
	#[serde(default, deserialize_with = "present")]
	trim: Option<usize>,
}

/// Deserialize a *present* member into `Some`, even when its value is JSON `null`.
///
/// A plain `Option` collapses an explicit null into `None`, which would make an appended `null`
/// record indistinguishable from an absent `append` member: the record would be dropped and, worse,
/// the tail would not advance, shifting every later position in the group. A snapshot preserves a
/// null record (its values decode as raw JSON), so without this the same record survives a group
/// roll but vanishes as an append.
fn present<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
	D: serde::Deserializer<'de>,
	T: Deserialize<'de>,
{
	T::deserialize(deserializer).map(Some)
}

/// Publishes a sliding window of JSON records over a track.
///
/// Cheaply clonable: clones share one underlying track and window state, so several owners append
/// into a single ordered log.
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
	pub fn consume(&self) -> moq_net::track::Subscriber {
		self.inner.lock().unwrap().track.subscribe(None)
	}

	/// The position of the oldest record still in the window: the number of records trimmed over
	/// the log's lifetime.
	pub fn offset(&self) -> u64 {
		self.inner.lock().unwrap().offset
	}

	/// The number of records currently in the window.
	pub fn len(&self) -> usize {
		self.inner.lock().unwrap().window.len()
	}

	/// Whether the window is empty.
	pub fn is_empty(&self) -> bool {
		self.len() == 0
	}
}

impl<T: Serialize> Producer<T> {
	/// Create a producer that publishes to the given track.
	pub fn new(track: moq_net::track::Producer, config: ProducerConfig) -> Self {
		Self {
			inner: Arc::new(Mutex::new(Inner {
				track,
				group: None,
				encoder: None,
				window: VecDeque::new(),
				offset: 0,
				trimmed: 0,
				frames: 0,
				config,
			})),
			_marker: PhantomData,
		}
	}

	/// Append one record to the tail of the window, returning the position it was assigned.
	pub fn append(&mut self, value: &T) -> Result<u64> {
		self.inner.lock().unwrap().append(value)
	}

	/// Remove the oldest `count` records from the head of the window.
	///
	/// A `count` of 0 does nothing. Errors if it exceeds the number of records in the window, since
	/// trimming past the head would desync every consumer's positions.
	pub fn trim(&mut self, count: usize) -> Result<()> {
		self.inner.lock().unwrap().trim(count)
	}

	/// Finish the track, closing any open group.
	pub fn finish(&mut self) -> Result<()> {
		self.inner.lock().unwrap().finish()
	}
}

/// Shared publishing state behind [`Producer`]'s `Arc<Mutex>`.
struct Inner {
	track: moq_net::track::Producer,
	group: Option<moq_net::group::Producer>,
	// The open group's DEFLATE encoder (one window per group), `Some` while compressing.
	encoder: Option<Encoder>,
	// Every record currently in the window, oldest first, kept so a new group can snapshot it.
	window: VecDeque<Box<RawValue>>,
	// The position of `window[0]`: how many records have been trimmed for the log's lifetime.
	offset: u64,
	// Records trimmed since the open group's snapshot, the roll trigger.
	trimmed: usize,
	frames: usize,
	config: ProducerConfig,
}

impl Inner {
	fn append<T: Serialize>(&mut self, value: &T) -> Result<u64> {
		let raw = serde_json::value::to_raw_value(value)?;
		let position = self.offset + self.window.len() as u64;
		// Refuse to publish a position a consumer would reject: the window would be unreadable.
		if position >= MAX_POSITION {
			return Err(Error::Window(format!(
				"append at position {position} reaches the maximum {MAX_POSITION}"
			)));
		}
		self.window.push_back(raw);

		let published = if self.needs_snapshot() {
			self.snapshot()
		} else {
			// The record was just pushed, so the back is the one to advertise.
			serde_json::to_vec(&AppendOut {
				append: self.window.back().expect("just pushed"),
			})
			.map_err(Error::from)
			.and_then(|payload| self.write(payload))
		};

		match published {
			Ok(()) => Ok(position),
			Err(err) => {
				// A publish that failed (a finished or aborted track) must leave the window as it
				// was: `offset`, `len`, and the next position all read from it, so keeping a record
				// nobody received would report a window that does not exist.
				self.window.pop_back();
				Err(err)
			}
		}
	}

	fn trim(&mut self, count: usize) -> Result<()> {
		if count == 0 {
			return Ok(());
		}
		if count > self.window.len() {
			return Err(Error::Window(format!(
				"trim of {count} exceeds the {} records in the window",
				self.window.len()
			)));
		}

		let removed: Vec<Box<RawValue>> = self.window.drain(..count).collect();
		self.offset += count as u64;
		self.trimmed += count;

		let published = if self.needs_snapshot() {
			self.snapshot()
		} else {
			serde_json::to_vec(&TrimOut { trim: count })
				.map_err(Error::from)
				.and_then(|payload| self.write(payload))
		};

		if let Err(err) = published {
			// Same rollback as `append`: an unpublished trim must not move the head.
			self.offset -= count as u64;
			self.trimmed -= count;
			for raw in removed.into_iter().rev() {
				self.window.push_front(raw);
			}
			return Err(err);
		}
		Ok(())
	}

	/// Whether the next operation should roll a fresh snapshot group instead.
	///
	/// Rolls once the records trimmed since the snapshot outnumber those still in the window: the
	/// group is then mostly dead weight for a new subscriber, and a fresh snapshot costs no more
	/// than what has already been trimmed. That bounds the total bytes at roughly twice an
	/// append-only log.
	fn needs_snapshot(&self) -> bool {
		self.group.is_none() || self.frames >= MAX_GROUP_FRAMES || self.trimmed > self.window.len()
	}

	/// Start a new group whose frame 0 snapshots the whole window.
	fn snapshot(&mut self) -> Result<()> {
		let payload = serde_json::to_vec(&SnapshotOut {
			offset: self.offset,
			values: &self.window,
		})?;

		// The previous group is complete; no more frames will be appended to it.
		if let Some(mut group) = self.group.take() {
			group.finish()?;
		}

		let mut group = self.track.append_group()?;
		let (slice, encoder) = match self.config.compression {
			true => {
				let mut encoder = Encoder::new();
				let slice = encoder.frame(&payload);
				(slice, Some(encoder))
			}
			false => (Bytes::from(payload), None),
		};
		group.write_frame(moq_net::Timestamp::now(), slice)?;

		self.group = Some(group);
		self.encoder = encoder;
		self.frames = 1;
		self.trimmed = 0;
		Ok(())
	}

	/// Write one operation frame into the open group.
	fn write(&mut self, payload: Vec<u8>) -> Result<()> {
		let slice = match self.encoder.as_mut() {
			Some(encoder) => encoder.frame(&payload),
			None => Bytes::from(payload),
		};
		self.group
			.as_mut()
			.expect("needs_snapshot guarantees an open group")
			.write_frame(moq_net::Timestamp::now(), slice)?;
		self.frames += 1;
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

/// One change to the window, yielded by a [`Consumer`].
///
/// `#[non_exhaustive]` because the wire format is explicitly extensible: a frame carrying an
/// operation this revision does not know is ignored rather than rejected, so a later revision can
/// add one. Match with a wildcard arm and skip what you don't recognize.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Update<T> {
	/// A record was appended at the tail.
	Append {
		/// The record's position: the count of records ever appended before it.
		position: u64,
		/// The record itself.
		value: T,
	},

	/// Records were removed from the head; everything before `offset` is gone.
	Trim {
		/// The position of the oldest record still in the window.
		offset: u64,
	},
}

/// Reads a sliding window of JSON records from a track, yielding each change in order.
///
/// Starts at the newest group's snapshot, so a late joiner sees an [`Update::Append`] per record
/// currently in the window and then follows along live. Switching to a newer group is lossless:
/// positions identify what has already been yielded, so records are never repeated.
///
/// [`Update::Trim`] only ever retracts records this consumer was told about, so joining a window
/// that has already trimmed does not report the records it missed. Those show up instead as a
/// jump in the first [`Update::Append`]'s `position`; whether a gap matters is up to the
/// application.
pub struct Consumer<T> {
	track: moq_net::track::Subscriber,
	group: Option<moq_net::group::Consumer>,
	compressed: bool,
	// The open group's DEFLATE decoder (one window per group), built on its first frame.
	decoder: Option<Decoder>,
	// Frames read from the open group; frame 0 is its snapshot.
	frames: usize,
	// The position of the next record this consumer has not yielded yet.
	next: u64,
	// The head offset already reported through an `Update::Trim`, or `None` until the first
	// snapshot establishes where this consumer joined.
	reported: Option<u64>,
	// The head offset of the group being followed, advanced by its trims.
	head: u64,
	// The position one past the last record the group being followed has appended.
	tail: u64,
	// Updates decoded but not yet yielded; one poll can decode a whole snapshot.
	pending: VecDeque<Update<T>>,
	_marker: PhantomData<fn() -> T>,
}

impl<T: DeserializeOwned> Consumer<T> {
	/// Create a consumer reading from the given track subscriber.
	///
	/// Set [`ConsumerConfig::compression`] to read a track written by a producer with
	/// [`ProducerConfig::compression`] on.
	pub fn new(track: moq_net::track::Subscriber, config: ConsumerConfig) -> Self {
		Self {
			track,
			group: None,
			compressed: config.compression,
			decoder: None,
			frames: 0,
			next: 0,
			reported: None,
			head: 0,
			tail: 0,
			pending: VecDeque::new(),
			_marker: PhantomData,
		}
	}

	/// Get the next update, or `None` once the track ends.
	pub async fn next(&mut self) -> Result<Option<Update<T>>>
	where
		T: Unpin,
	{
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Poll for the next update, without blocking.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Update<T>>>> {
		loop {
			if let Some(update) = self.pending.pop_front() {
				return Poll::Ready(Ok(Some(update)));
			}

			// Skip to the newest group; anything older carries nothing it lacks.
			let track_finished = loop {
				match self.track.poll_next_group(waiter)? {
					Poll::Ready(Some(group)) => {
						self.group = Some(group);
						self.decoder = None;
						self.frames = 0;
					}
					Poll::Ready(None) => break true,
					Poll::Pending => break false,
				}
			};

			// Decode frames until something is ready to yield, or the group blocks or ends.
			let mut group_ended = false;
			let mut group_pending = false;
			while self.pending.is_empty() {
				let Some(group) = &mut self.group else { break };
				match group.poll_read_frame(waiter) {
					Poll::Ready(Ok(Some(frame))) => self.apply(frame.payload)?,
					Poll::Ready(Ok(None)) => {
						// Every group opens with a snapshot, so one that ends cleanly having
						// delivered nothing is malformed. Treating it as an ordinary end would let
						// it masquerade as a clean end of track and silently lose the window the
						// publisher advertised. (A group that *errors* before frame 0 is a different
						// case: that is a lost group, handled below as a resync.)
						if self.frames == 0 {
							return Poll::Ready(Err(Error::Window("a group carried no snapshot".to_string())));
						}
						self.group = None;
						group_ended = true;
						break;
					}
					// The group died under us: evicted, aged out, or read past its cache. Groups
					// here are self-contained and disposable, so this is a resync point rather than
					// the end of the track. Failing instead would strand a slow consumer (the HLS
					// timeline watcher among them) on a track that is still publishing.
					Poll::Ready(Err(err)) => {
						tracing::debug!(%err, "window group lost; resyncing from the next snapshot");
						self.group = None;
						self.decoder = None;
						group_ended = true;
						break;
					}
					Poll::Pending => {
						group_pending = true;
						break;
					}
				}
			}

			if !self.pending.is_empty() {
				continue;
			}
			// The group ended without yielding anything: loop once more to pick up a newer one.
			// That poll either finds one or reports Pending, so this can't spin.
			if group_ended {
				continue;
			}
			// An open group may still deliver frames after the track finishes (it was appended
			// before), so wait on it rather than ending the stream.
			if group_pending || !track_finished {
				return Poll::Pending;
			}
			return Poll::Ready(Ok(None));
		}
	}

	/// Decompress a frame slice, or pass it through when the track is uncompressed.
	fn decode(&mut self, slice: Bytes) -> Result<Bytes> {
		if !self.compressed {
			return Ok(slice);
		}
		let decoder = self.decoder.get_or_insert_with(Decoder::new);
		Ok(decoder.frame(&slice)?)
	}

	/// Apply one frame: frame 0 of a group is a snapshot, the rest are operations.
	fn apply(&mut self, frame: Bytes) -> Result<()> {
		let frame = self.decode(frame)?;
		match self.frames {
			0 => self.apply_snapshot(&frame)?,
			_ => self.apply_op(&frame)?,
		}
		self.frames += 1;
		Ok(())
	}

	/// Adopt a group's snapshot, yielding only what this consumer hasn't seen.
	fn apply_snapshot(&mut self, frame: &[u8]) -> Result<()> {
		let snapshot: SnapshotIn = serde_json::from_slice(frame)?;

		// A peer picks both the offset and the record count, so bound the whole span, not just the
		// offset: an in-range offset with enough records still lands the tail past MAX_POSITION and
		// yields positions the wire format (and a JSON-number receiver) cannot represent. The
		// checked add also keeps a hostile offset from overflowing u64, which panics in debug and
		// silently wraps every later position to zero in release.
		let tail = snapshot
			.offset
			.checked_add(snapshot.values.len() as u64)
			.filter(|tail| *tail <= MAX_POSITION);
		let Some(tail) = tail else {
			return Err(Error::Window(format!(
				"snapshot at offset {} with {} records runs past the maximum position {MAX_POSITION}",
				snapshot.offset,
				snapshot.values.len()
			)));
		};

		self.head = snapshot.offset;
		self.tail = tail;
		self.report_trim();

		for (index, raw) in snapshot.values.into_iter().enumerate() {
			let position = snapshot.offset + index as u64;
			// Already yielded from an older group: the position is what makes the switch lossless.
			if position < self.next {
				continue;
			}
			self.push_append(position, &raw)?;
		}
		Ok(())
	}

	/// Apply one operation frame from the group being followed.
	fn apply_op(&mut self, frame: &[u8]) -> Result<()> {
		let op: OpIn = serde_json::from_slice(frame)?;

		// A frame carries at most one operation. Applying both would append and retract in an order
		// the format never defines, so reject it instead of guessing. A frame carrying *neither* is
		// not an error: that is how an operation added by a later revision reads here.
		if op.append.is_some() && op.trim.is_some() {
			return Err(Error::Window("a frame carries both an append and a trim".to_string()));
		}

		if let Some(raw) = op.append {
			let position = self.tail;
			if position >= MAX_POSITION {
				return Err(Error::Window(format!(
					"append at position {position} reaches the maximum {MAX_POSITION}"
				)));
			}
			self.tail += 1;
			if position >= self.next {
				self.push_append(position, &raw)?;
			}
		}

		if let Some(count) = op.trim {
			// A peer picks this count, so add with a checked op: `head + count` would otherwise
			// overflow before the range test could reject it.
			let count = count as u64;
			let head = self.head.checked_add(count).filter(|head| *head <= self.tail);
			let Some(head) = head else {
				return Err(Error::Window(format!(
					"trim of {count} exceeds the {} records in the window",
					self.tail - self.head
				)));
			};
			self.head = head;
			self.report_trim();
		}

		Ok(())
	}

	/// Emit a trim if the followed group's head has moved past what was last reported.
	///
	/// A trim says "records you were told about are gone", so the first snapshot only adopts its
	/// offset: a consumer joining a window that already trimmed never held those records, and
	/// reporting them would be noise. Later movement is guarded too, since a newer group's snapshot
	/// repeats a head this consumer already reported.
	fn report_trim(&mut self) {
		match self.reported {
			None => self.reported = Some(self.head),
			Some(reported) if self.head > reported => {
				self.reported = Some(self.head);
				self.pending.push_back(Update::Trim { offset: self.head });
			}
			Some(_) => {}
		}
	}

	/// Deserialize a record and queue it, advancing the yield cursor past it.
	fn push_append(&mut self, position: u64, raw: &RawValue) -> Result<()> {
		let value = serde_json::from_str(raw.get())?;
		self.next = position + 1;
		self.pending.push_back(Update::Append { position, value });
		Ok(())
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use serde_json::{Value, json};

	fn producer(config: ProducerConfig) -> (Producer<Value>, moq_net::track::Subscriber) {
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", None)
			.unwrap();
		let consumer = track.subscribe(None);
		(Producer::new(track, config), consumer)
	}

	fn compressed() -> ProducerConfig {
		ProducerConfig { compression: true }
	}

	fn consumer(track: moq_net::track::Subscriber, compression: bool) -> Consumer<Value> {
		Consumer::new(track, ConsumerConfig { compression })
	}

	/// Drain every update currently available without blocking.
	fn drain(mut consumer: Consumer<Value>) -> Vec<Update<Value>> {
		let waiter = kio::Waiter::noop();
		let mut out = Vec::new();
		while let Poll::Ready(Ok(Some(update))) = consumer.poll_next(&waiter) {
			out.push(update);
		}
		out
	}

	/// The records a consumer would hold after applying every update, as (position, value).
	fn replay(updates: &[Update<Value>]) -> Vec<(u64, Value)> {
		let mut records: Vec<(u64, Value)> = Vec::new();
		for update in updates {
			match update {
				Update::Append { position, value } => records.push((*position, value.clone())),
				Update::Trim { offset } => records.retain(|(position, _)| position >= offset),
			}
		}
		records
	}

	fn append(position: u64, n: i64) -> Update<Value> {
		Update::Append {
			position,
			value: json!({ "n": n }),
		}
	}

	#[test]
	fn appends_carry_positions() {
		let (mut producer, track) = producer(ProducerConfig::default());
		for n in 0..3 {
			assert_eq!(producer.append(&json!({ "n": n })).unwrap(), n as u64);
		}
		producer.finish().unwrap();

		assert_eq!(
			drain(consumer(track, false)),
			vec![append(0, 0), append(1, 1), append(2, 2)]
		);
	}

	#[test]
	fn trim_removes_from_the_head() {
		let (mut producer, track) = producer(ProducerConfig::default());
		for n in 0..4 {
			producer.append(&json!({ "n": n })).unwrap();
		}
		producer.trim(2).unwrap();
		assert_eq!(producer.offset(), 2);
		assert_eq!(producer.len(), 2);
		producer.finish().unwrap();

		// A live consumer sees every append, then the trim.
		let updates = drain(consumer(track, false));
		assert_eq!(updates.last().unwrap(), &Update::Trim { offset: 2 });
		assert_eq!(replay(&updates), vec![(2, json!({ "n": 2 })), (3, json!({ "n": 3 }))]);
	}

	#[test]
	fn late_joiner_reads_the_window_not_the_history() {
		let (mut producer, track) = producer(ProducerConfig::default());
		for n in 0..6 {
			producer.append(&json!({ "n": n })).unwrap();
		}
		producer.trim(4).unwrap();
		producer.finish().unwrap();

		// Only the surviving records, at their original positions: no trace of 0..4.
		let updates = drain(consumer(track, false));
		assert_eq!(updates, vec![append(4, 4), append(5, 5)]);
	}

	#[test]
	fn a_failed_publish_leaves_the_window_untouched() {
		// `finish` only borrows, since a `Producer` is a shared handle, so a later append is
		// expressible. It must fail without moving the window: `offset`, `len`, and the next
		// position all read from it, so a phantom record would report a window nobody received.
		let (mut producer, _track) = producer(ProducerConfig::default());
		producer.append(&json!({ "n": 0 })).unwrap();
		producer.append(&json!({ "n": 1 })).unwrap();
		producer.finish().unwrap();

		assert!(matches!(producer.append(&json!({ "n": 2 })), Err(Error::Net(_))));
		assert_eq!(producer.len(), 2, "a rejected append must not grow the window");
		assert_eq!(producer.offset(), 0);

		assert!(matches!(producer.trim(1), Err(Error::Net(_))));
		assert_eq!(producer.len(), 2, "a rejected trim must not shrink the window");
		assert_eq!(producer.offset(), 0, "a rejected trim must not move the head");
	}

	#[test]
	fn trim_past_the_window_is_rejected() {
		let (mut producer, _track) = producer(ProducerConfig::default());
		producer.append(&json!({ "n": 0 })).unwrap();
		assert!(matches!(producer.trim(2), Err(Error::Window(_))));
		// A rejected trim leaves the window untouched.
		assert_eq!(producer.len(), 1);
		assert_eq!(producer.offset(), 0);
		// A zero trim is a no-op, not an error.
		producer.trim(0).unwrap();
	}

	#[test]
	fn heavy_trimming_rolls_a_group() {
		let (mut producer, track) = producer(ProducerConfig::default());
		// A steady sliding window of two records: append, append, then one in/one out.
		producer.append(&json!({ "n": 0 })).unwrap();
		producer.append(&json!({ "n": 1 })).unwrap();
		for n in 2..12 {
			producer.append(&json!({ "n": n })).unwrap();
			producer.trim(1).unwrap();
		}
		producer.finish().unwrap();

		// Trims outgrew the window repeatedly, so the log rolled instead of growing forever.
		assert!(track.latest().unwrap() > 0, "a trimming window should roll groups");

		// And a late joiner still reads exactly the live window.
		let updates = drain(consumer(track, false));
		assert_eq!(
			replay(&updates),
			vec![(10, json!({ "n": 10 })), (11, json!({ "n": 11 }))]
		);
	}

	#[test]
	fn frame_cap_rolls_a_group() {
		let (mut producer, track) = producer(ProducerConfig::default());
		for n in 0..=(MAX_GROUP_FRAMES as i64) {
			producer.append(&json!({ "n": n })).unwrap();
		}
		producer.finish().unwrap();

		assert_eq!(track.latest(), Some(1), "the frame cap forces exactly one roll");
		assert_eq!(drain(consumer(track, false)).len(), MAX_GROUP_FRAMES + 1);
	}

	#[test]
	fn a_slow_consumer_converges_after_draining_a_stale_snapshot() {
		// A snapshot queues its whole window at once, so a consumer can be mid-drain when a newer
		// group rolls with the head already trimmed. Draining the stale queue first is correct: the
		// records were real when the snapshot was written, and the newer snapshot's head retracts
		// them on arrival, so the consumer converges on the live window without losing a record.
		//
		// Dropping the queue on the switch instead would lose records outright: `push_append`
		// advances `next` when it *queues* a record, so the newer snapshot would skip every queued
		// position as already yielded.
		let (mut producer, track) = producer(ProducerConfig::default());
		let waiter = kio::Waiter::noop();

		for n in 0..6 {
			producer.append(&json!({ "n": n })).unwrap();
		}

		// Join late, so frame 0 is a snapshot that queues the whole window in one go.
		let mut consumer = consumer(track, false);
		let first = consumer.poll_next(&waiter);
		assert!(matches!(
			first,
			Poll::Ready(Ok(Some(Update::Append { position: 0, .. })))
		));

		// Roll a new group behind its back, trimming the head it is still holding queued.
		for n in 6..12 {
			producer.append(&json!({ "n": n })).unwrap();
			producer.trim(1).unwrap();
		}
		producer.finish().unwrap();

		let mut seen = vec![Update::Append {
			position: 0,
			value: json!({ "n": 0 }),
		}];
		while let Poll::Ready(Ok(Some(update))) = consumer.poll_next(&waiter) {
			seen.push(update);
		}

		let positions: Vec<u64> = seen
			.iter()
			.filter_map(|u| match u {
				Update::Append { position, .. } => Some(*position),
				Update::Trim { .. } => None,
			})
			.collect();

		let mut sorted = positions.clone();
		sorted.sort_unstable();
		sorted.dedup();
		assert_eq!(positions, sorted, "a position was repeated or went backwards");
		assert_eq!(positions.first(), Some(&0), "the stale queue must still be delivered");
		assert_eq!(positions.last(), Some(&11), "the newest record must arrive");

		// And the retractions land, so the consumer ends up holding exactly the live window.
		let held: Vec<u64> = replay(&seen).into_iter().map(|(position, _)| position).collect();
		assert_eq!(held, vec![6, 7, 8, 9, 10, 11]);
	}

	#[test]
	fn group_roll_is_lossless_for_a_live_consumer() {
		// A consumer following live must not see a record twice when the producer rolls a group
		// and re-snapshots the window it already holds.
		let (mut producer, track) = producer(ProducerConfig::default());
		let mut consumer = consumer(track, false);
		let waiter = kio::Waiter::noop();

		let mut seen = Vec::new();
		producer.append(&json!({ "n": 0 })).unwrap();
		producer.append(&json!({ "n": 1 })).unwrap();
		while let Poll::Ready(Ok(Some(update))) = consumer.poll_next(&waiter) {
			seen.push(update);
		}

		// Force rolls while the consumer keeps up.
		for n in 2..12 {
			producer.append(&json!({ "n": n })).unwrap();
			producer.trim(1).unwrap();
			while let Poll::Ready(Ok(Some(update))) = consumer.poll_next(&waiter) {
				seen.push(update);
			}
		}
		producer.finish().unwrap();

		let positions: Vec<u64> = seen
			.iter()
			.filter_map(|u| match u {
				Update::Append { position, .. } => Some(*position),
				Update::Trim { .. } => None,
			})
			.collect();
		let mut sorted = positions.clone();
		sorted.dedup();
		assert_eq!(positions, sorted, "a roll must not repeat a record: {positions:?}");
		assert_eq!(positions, (0..12).collect::<Vec<_>>());
		assert_eq!(replay(&seen), vec![(10, json!({ "n": 10 })), (11, json!({ "n": 11 }))]);
	}

	#[test]
	fn late_joiner_sees_a_gap_as_a_position_jump() {
		let (mut producer, track) = producer(ProducerConfig::default());
		for n in 0..4 {
			producer.append(&json!({ "n": n })).unwrap();
		}
		producer.trim(3).unwrap();

		// Joining now, the first record seen is at position 3: the jump is the gap signal.
		let updates = drain(consumer(track, false));
		match updates.first().unwrap() {
			Update::Append { position, .. } => assert_eq!(*position, 3),
			other => panic!("expected an append, got {other:?}"),
		}
		producer.finish().unwrap();
	}

	#[test]
	fn compressed_roundtrip() {
		let (mut producer, track) = producer(compressed());
		for n in 0..20 {
			producer.append(&json!({ "group": n, "pts": n * 2_000 })).unwrap();
		}
		producer.trim(15).unwrap();
		producer.finish().unwrap();

		let updates = drain(consumer(track, true));
		assert_eq!(replay(&updates).len(), 5);
		assert_eq!(replay(&updates)[0].0, 15);
	}

	#[test]
	fn compressed_window_shrinks_records() {
		// The shared per-group window is the point: a repetitive record compresses to a fraction of
		// its raw size once the snapshot has seeded the window.
		let (mut producer, mut track) = producer(compressed());
		for n in 0..8 {
			producer.append(&json!({ "group": n, "pts": n * 2_000 })).unwrap();
		}
		producer.finish().unwrap();

		let waiter = kio::Waiter::noop();
		let Poll::Ready(Ok(Some(mut group))) = track.poll_next_group(&waiter) else {
			panic!("expected a group");
		};
		let mut sizes = Vec::new();
		while let Poll::Ready(Ok(Some(frame))) = group.poll_read_frame(&waiter) {
			sizes.push(frame.payload.len());
		}
		assert_eq!(sizes.len(), 8, "one snapshot plus seven appends");

		let raw = serde_json::to_vec(&json!({ "append": { "group": 7, "pts": 14_000 } }))
			.unwrap()
			.len();
		assert!(
			*sizes.last().unwrap() < raw / 2,
			"windowed record {} should be far below its raw size {raw}",
			sizes.last().unwrap()
		);
	}

	#[test]
	fn a_null_record_keeps_its_position() {
		// `null` is a valid JSON record. If it decodes as "no append", the record is lost AND the
		// tail stalls, shifting every later position. The snapshot path preserves it either way, so
		// getting this wrong makes a record appear or vanish depending on which frame carried it.
		let (mut producer, track) = producer(ProducerConfig::default());
		producer.append(&json!({ "n": 0 })).unwrap();
		producer.append(&Value::Null).unwrap();
		producer.append(&json!({ "n": 2 })).unwrap();
		producer.finish().unwrap();

		assert_eq!(
			drain(consumer(track, false)),
			vec![
				append(0, 0),
				Update::Append {
					position: 1,
					value: Value::Null
				},
				append(2, 2),
			]
		);
	}

	#[test]
	fn a_null_record_appended_after_a_roll_keeps_its_position() {
		let (mut producer, track) = producer(ProducerConfig::default());
		producer.append(&Value::Null).unwrap();
		producer.append(&json!({ "n": 1 })).unwrap();
		producer.trim(1).unwrap();
		producer.append(&json!({ "n": 2 })).unwrap();
		// Trims now outnumber the window, so this rolls: the survivors come from a fresh snapshot.
		producer.trim(1).unwrap();
		producer.append(&Value::Null).unwrap();
		producer.finish().unwrap();

		// A late joiner starts at the newest group's snapshot (offset 2), then reads the null as an
		// append. A stalled tail here would have put the null at position 2, colliding with `n: 2`.
		assert_eq!(
			drain(consumer(track, false)),
			vec![
				append(2, 2),
				Update::Append {
					position: 3,
					value: Value::Null
				},
			]
		);
	}

	#[test]
	fn a_null_record_inside_a_snapshot_decodes() {
		// The other half of the round trip: a null carried by frame 0 rather than by an append.
		// Written by hand so the decoder is checked against the byte shape, not just our producer.
		let mut track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", None)
			.unwrap();
		let subscriber = track.subscribe(None);
		let mut group = track.append_group().unwrap();
		for frame in [r#"{"offset":4,"values":[{"n":4},null,{"n":6}]}"#, r#"{"append":null}"#] {
			group
				.write_frame(moq_net::Timestamp::ZERO, Bytes::from(frame.as_bytes().to_vec()))
				.unwrap();
		}
		group.finish().unwrap();
		track.finish().unwrap();

		assert_eq!(
			drain(consumer(subscriber, false)),
			vec![
				append(4, 4),
				Update::Append {
					position: 5,
					value: Value::Null
				},
				append(6, 6),
				Update::Append {
					position: 7,
					value: Value::Null
				},
			]
		);
	}

	#[test]
	fn an_out_of_range_snapshot_offset_is_rejected() {
		// A peer picks the offset. Past MAX_POSITION the tail addition overflows: a debug build
		// panics and a release build wraps every later position to zero.
		let mut track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", None)
			.unwrap();
		let subscriber = track.subscribe(None);
		let mut group = track.append_group().unwrap();
		let frame = format!(r#"{{"offset":{},"values":[null]}}"#, u64::MAX);
		group
			.write_frame(moq_net::Timestamp::ZERO, Bytes::from(frame.into_bytes()))
			.unwrap();
		group.finish().unwrap();
		track.finish().unwrap();

		match consumer(subscriber, false).poll_next(&kio::Waiter::noop()) {
			Poll::Ready(Err(Error::Window(msg))) => assert!(msg.contains("maximum position"), "{msg}"),
			other => panic!("expected an out-of-range offset to be rejected, got {other:?}"),
		}
	}

	#[test]
	fn a_snapshot_spanning_past_the_limit_is_rejected() {
		// An in-range offset with enough records still lands the tail past the limit, so the span
		// has to be checked, not just the offset.
		let mut track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", None)
			.unwrap();
		let subscriber = track.subscribe(None);
		let mut group = track.append_group().unwrap();
		let frame = format!(r#"{{"offset":{},"values":[{{"n":0}},{{"n":1}}]}}"#, MAX_POSITION);
		group
			.write_frame(moq_net::Timestamp::ZERO, Bytes::from(frame.into_bytes()))
			.unwrap();
		group.finish().unwrap();
		track.finish().unwrap();

		match consumer(subscriber, false).poll_next(&kio::Waiter::noop()) {
			Poll::Ready(Err(Error::Window(msg))) => assert!(msg.contains("maximum position"), "{msg}"),
			other => panic!("expected an out-of-range span to be rejected, got {other:?}"),
		}
	}

	#[test]
	fn a_group_with_no_snapshot_is_rejected() {
		// A group that ends cleanly having carried nothing is malformed: every group opens with a
		// snapshot. Reporting it as a clean end would lose the window the publisher advertised.
		let mut track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", None)
			.unwrap();
		let subscriber = track.subscribe(None);
		track.append_group().unwrap().finish().unwrap();
		track.finish().unwrap();

		match consumer(subscriber, false).poll_next(&kio::Waiter::noop()) {
			Poll::Ready(Err(Error::Window(msg))) => assert!(msg.contains("no snapshot"), "{msg}"),
			other => panic!("expected an empty group to be rejected, got {other:?}"),
		}
	}

	#[test]
	fn a_lost_group_resyncs_instead_of_ending_the_stream() {
		// Groups here are self-contained and disposable, so one dying under a slow consumer is a
		// resync point, not the end of the track. Failing would strand the HLS timeline watcher on
		// a track that is still publishing.
		let (mut producer, track) = producer(ProducerConfig::default());
		let mut consumer = consumer(track, false);
		let waiter = kio::Waiter::noop();

		producer.append(&json!({ "n": 0 })).unwrap();
		match consumer.poll_next(&waiter) {
			Poll::Ready(Ok(Some(update))) => assert_eq!(update, append(0, 0)),
			other => panic!("expected the first record, got {other:?}"),
		}

		// Kill the group the consumer is parked on, the way cache eviction does.
		producer
			.inner
			.lock()
			.unwrap()
			.group
			.take()
			.expect("a group is open")
			.abort(moq_net::Error::Evicted)
			.unwrap();
		assert!(matches!(consumer.poll_next(&waiter), Poll::Pending), "not an error");

		// The publisher rolls a fresh snapshot group and the consumer picks up from it.
		producer.append(&json!({ "n": 1 })).unwrap();
		producer.finish().unwrap();

		let mut seen = Vec::new();
		while let Poll::Ready(Ok(Some(update))) = consumer.poll_next(&waiter) {
			seen.push(update);
		}
		assert_eq!(seen, vec![append(1, 1)], "resynced from the next snapshot");
	}

	#[test]
	fn an_overflowing_trim_is_rejected() {
		// Same for the trim count: `head + count` must not wrap before the range test sees it.
		let mut track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", None)
			.unwrap();
		let subscriber = track.subscribe(None);
		let mut group = track.append_group().unwrap();
		for frame in [
			r#"{"offset":0,"values":[{"n":0}]}"#.to_string(),
			format!(r#"{{"trim":{}}}"#, u64::MAX),
		] {
			group
				.write_frame(moq_net::Timestamp::ZERO, Bytes::from(frame.into_bytes()))
				.unwrap();
		}
		group.finish().unwrap();
		track.finish().unwrap();

		let mut consumer = consumer(subscriber, false);
		let waiter = kio::Waiter::noop();
		match consumer.poll_next(&waiter) {
			Poll::Ready(Ok(Some(update))) => assert_eq!(update, append(0, 0)),
			other => panic!("expected the snapshot record, got {other:?}"),
		}
		match consumer.poll_next(&waiter) {
			Poll::Ready(Err(Error::Window(msg))) => assert!(msg.contains("exceeds"), "{msg}"),
			other => panic!("expected an overflowing trim to be rejected, got {other:?}"),
		}
	}

	#[test]
	fn a_frame_carrying_both_operations_is_rejected() {
		// Applying both would append and retract in an order the format never defines, so it is
		// malformed rather than something to guess at. A frame carrying *neither* stays a no-op:
		// that is how a future operation reads here.
		let mut track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", None)
			.unwrap();
		let subscriber = track.subscribe(None);
		let mut group = track.append_group().unwrap();
		for frame in [r#"{"offset":0,"values":[{"n":0}]}"#, r#"{"append":{"n":1},"trim":1}"#] {
			group
				.write_frame(moq_net::Timestamp::ZERO, Bytes::from(frame.as_bytes().to_vec()))
				.unwrap();
		}
		group.finish().unwrap();
		track.finish().unwrap();

		let mut consumer = consumer(subscriber, false);
		let waiter = kio::Waiter::noop();
		// The snapshot record lands first, then the malformed frame surfaces.
		match consumer.poll_next(&waiter) {
			Poll::Ready(Ok(Some(update))) => assert_eq!(update, append(0, 0)),
			other => panic!("expected the snapshot record, got {other:?}"),
		}
		match consumer.poll_next(&waiter) {
			Poll::Ready(Err(Error::Window(msg))) => assert!(msg.contains("both"), "{msg}"),
			other => panic!("expected a malformed-frame error, got {other:?}"),
		}
	}

	#[test]
	fn unknown_operation_is_ignored() {
		// A frame carrying neither key is a forward-compatible extension, not an error.
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", None)
			.unwrap();
		let subscriber = track.subscribe(None);
		let mut producer = Producer::<Value>::new(track, ProducerConfig::default());
		producer.append(&json!({ "n": 0 })).unwrap();

		// Write a future operation directly into the open group.
		producer
			.inner
			.lock()
			.unwrap()
			.write(serde_json::to_vec(&json!({ "rewind": 1 })).unwrap())
			.unwrap();
		producer.append(&json!({ "n": 1 })).unwrap();
		producer.finish().unwrap();

		assert_eq!(drain(consumer(subscriber, false)), vec![append(0, 0), append(1, 1)]);
	}

	#[test]
	fn empty_window_snapshots_cleanly() {
		let (mut producer, track) = producer(ProducerConfig::default());
		producer.append(&json!({ "n": 0 })).unwrap();
		producer.trim(1).unwrap();
		assert!(producer.is_empty());
		// The offset is preserved across an empty window, so positions keep counting.
		assert_eq!(producer.offset(), 1);
		assert_eq!(producer.append(&json!({ "n": 1 })).unwrap(), 1);
		producer.finish().unwrap();

		assert_eq!(drain(consumer(track, false)), vec![append(1, 1)]);
	}

	#[test]
	fn a_trim_retracts_what_was_yielded() {
		// A consumer told about a record is told when it goes away. (The converse, that a consumer
		// which never held the record gets no trim, is what `late_joiner_reads_the_window_not_the
		// _history` asserts: after a roll the snapshot starts past those records.)
		let (mut producer, track) = producer(ProducerConfig::default());
		let mut live = consumer(track, false);
		let waiter = kio::Waiter::noop();

		producer.append(&json!({ "n": 0 })).unwrap();
		producer.append(&json!({ "n": 1 })).unwrap();
		let mut seen = Vec::new();
		while let Poll::Ready(Ok(Some(update))) = live.poll_next(&waiter) {
			seen.push(update);
		}
		assert_eq!(seen, vec![append(0, 0), append(1, 1)]);

		producer.trim(1).unwrap();
		producer.finish().unwrap();
		while let Poll::Ready(Ok(Some(update))) = live.poll_next(&waiter) {
			seen.push(update);
		}
		assert_eq!(seen.last().unwrap(), &Update::Trim { offset: 1 });
		assert_eq!(replay(&seen), vec![(1, json!({ "n": 1 }))]);
	}

	#[test]
	fn open_group_pends_after_track_finish() {
		// A group appended before the track finishes may still deliver frames, so the consumer
		// must keep waiting on it rather than ending the stream.
		let mut track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", None)
			.unwrap();
		let mut group = track.append_group().unwrap();
		let subscriber = track.subscribe(None);
		track.finish().unwrap();

		let mut consumer = consumer(subscriber, false);
		let waiter = kio::Waiter::noop();
		assert!(matches!(consumer.poll_next(&waiter), Poll::Pending));

		let snapshot = json!({ "offset": 0, "values": [{ "n": 7 }] });
		group
			.write_frame(
				moq_net::Timestamp::ZERO,
				Bytes::from(serde_json::to_vec(&snapshot).unwrap()),
			)
			.unwrap();
		group.finish().unwrap();

		match consumer.poll_next(&waiter) {
			Poll::Ready(Ok(Some(update))) => assert_eq!(update, append(0, 7)),
			other => panic!("expected the snapshot record, got {other:?}"),
		}
	}
}
