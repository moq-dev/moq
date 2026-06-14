//! A [`HashSet`]-like collection synced over a [`moq-net`](moq_net) track.
//!
//! The set is published as a series of self-contained groups. A group's first frame is a full
//! snapshot of every item; each following frame is a single `+` (insert) or `-` (remove) delta
//! applied in order. A consumer jumps to the newest group, decodes the snapshot, and replays the
//! deltas, so a late joiner never needs older groups.
//!
//! Items are arbitrary binary data: any type implementing [`Item`] (encode to bytes, decode back)
//! can live in the set. [`String`], [`Vec<u8>`], and [`bytes::Bytes`] are supported out of the box;
//! a custom type can implement [`Item`] however it likes (e.g. via `serde_json`).
//!
//! # Wire format
//!
//! Every frame within a group is one of:
//!
//! - **snapshot** (frame 0): big-endian `u32(count)` followed by `count` repetitions of
//!   big-endian `u32(len)` then `len` item bytes.
//! - **delta** (frame 1+): a one-byte op (`+` = `0x2B` insert, `-` = `0x2D` remove) followed by the
//!   item bytes, which run to the end of the frame.

use std::borrow::Borrow;
use std::collections::HashSet;
use std::hash::Hash;
use std::task::Poll;

use bytes::{Buf, BufMut, Bytes};

/// One-byte op prefixing an insert delta frame.
const INSERT: u8 = b'+';
/// One-byte op prefixing a remove delta frame.
const REMOVE: u8 = b'-';

/// Maximum frames (snapshot + deltas) in a single group before a new snapshot is forced.
///
/// Kept well below moq-net's per-group frame cap so a late joiner can always read the snapshot at
/// frame 0 before the group is evicted.
const MAX_DELTA_FRAMES: usize = 256;

/// Errors produced while publishing or consuming a set.
#[derive(thiserror::Error, Debug, Clone)]
#[non_exhaustive]
pub enum Error {
	/// An error from the underlying track.
	#[error(transparent)]
	Net(#[from] moq_net::Error),

	/// A frame could not be parsed as a snapshot or delta.
	#[error("malformed frame: {0}")]
	Malformed(String),

	/// An item failed to encode or decode.
	#[error("item: {0}")]
	Item(String),
}

/// A [`Result`](std::result::Result) using this module's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// An item that can be stored in a [`Set`](Producer).
///
/// Encoding must be deterministic and round-trip: decoding what [`encode`](Item::encode) wrote must
/// reproduce `item`. Two items are the same set member iff they're equal under [`Eq`]/[`Hash`], so
/// distinct items must encode to distinct bytes.
pub trait Item: Clone + Eq + Hash {
	/// Encode the item's bytes directly into `buf`.
	///
	/// Writing into the frame buffer (rather than returning a fresh `Bytes`) keeps a string or byte
	/// vector to a single copy.
	fn encode<B: BufMut>(&self, buf: &mut B);

	/// Decode an item from `buf`, which holds exactly this item's bytes.
	///
	/// Read straight from the [`Buf`]: scalar getters (`get_u16`, ...) read in place, and
	/// `buf.copy_to_bytes(buf.remaining())` hands back a zero-copy [`Bytes`] slice of the frame.
	fn decode<B: Buf>(buf: &mut B) -> Result<Self>
	where
		Self: Sized;

	/// The number of bytes [`encode`](Item::encode) writes, used to size a frame up front.
	///
	/// The default runs `encode` against a counting `BufMut` (no allocation, no copy). Override it
	/// when the length is known directly, e.g. `self.len()`.
	fn encode_size(&self) -> usize {
		let mut sizer = crate::sizer::Sizer::default();
		self.encode(&mut sizer);
		sizer.size
	}
}

impl Item for String {
	fn encode<B: BufMut>(&self, buf: &mut B) {
		buf.put_slice(self.as_bytes());
	}

	fn decode<B: Buf>(buf: &mut B) -> Result<Self> {
		let bytes = buf.copy_to_bytes(buf.remaining());
		String::from_utf8(bytes.into()).map_err(|err| Error::Item(err.to_string()))
	}

	fn encode_size(&self) -> usize {
		self.len()
	}
}

impl Item for Vec<u8> {
	fn encode<B: BufMut>(&self, buf: &mut B) {
		buf.put_slice(self);
	}

	fn decode<B: Buf>(buf: &mut B) -> Result<Self> {
		Ok(buf.copy_to_bytes(buf.remaining()).into())
	}

	fn encode_size(&self) -> usize {
		self.len()
	}
}

impl Item for Bytes {
	fn encode<B: BufMut>(&self, buf: &mut B) {
		buf.put_slice(self);
	}

	fn decode<B: Buf>(buf: &mut B) -> Result<Self> {
		Ok(buf.copy_to_bytes(buf.remaining()))
	}

	fn encode_size(&self) -> usize {
		self.len()
	}
}

/// Configuration for a [`Producer`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Config {
	/// Controls whether changes are published as deltas instead of full snapshots.
	///
	/// `None` disables deltas: every change starts a new group with a full snapshot.
	///
	/// `Some(ratio)` enables deltas. A `+`/`-` delta is appended to the current group as long as the
	/// deltas accumulated since the last snapshot stay within `ratio` times the size of a fresh
	/// snapshot; otherwise a new snapshot group is started, bounding how much a late joiner replays.
	pub delta_ratio: Option<f64>,
}

impl Default for Config {
	fn default() -> Self {
		// Deltas on by default: the whole point of a set track is incremental add/remove.
		Self { delta_ratio: Some(2.0) }
	}
}

/// Publishes a set over a track, choosing snapshots and deltas automatically.
pub struct Producer<T: Item> {
	track: moq_net::TrackProducer,
	config: Config,

	current: HashSet<T>,
	group: Option<moq_net::GroupProducer>,
	/// Total frames in the open group, including the snapshot, for the frame cap.
	group_frames: usize,
	/// Bytes of delta frames appended since the last snapshot, for the ratio budget.
	group_delta_bytes: u64,
}

impl<T: Item> Producer<T> {
	/// Create a producer that publishes to the given track.
	pub fn new(track: moq_net::TrackProducer, config: Config) -> Self {
		Self {
			track,
			config,
			current: HashSet::new(),
			group: None,
			group_frames: 0,
			group_delta_bytes: 0,
		}
	}

	/// Insert an item, publishing a delta or snapshot. Returns `true` if it was newly inserted.
	pub fn insert(&mut self, item: T) -> Result<bool> {
		if self.current.contains(&item) {
			return Ok(false);
		}

		let delta_size = 1 + item.encode_size() as u64;
		// The snapshot size once `item` is included, computed without inserting so we keep `&item`.
		let snapshot_size = self.snapshot_size() + 4 + item.encode_size() as u64;

		if self.should_snapshot(delta_size, snapshot_size) {
			self.current.insert(item);
			self.write_snapshot()?;
		} else {
			// Write the delta straight from the reference, then move the item into the set.
			self.write_delta(INSERT, &item)?;
			self.current.insert(item);
		}
		Ok(true)
	}

	/// Remove an item, publishing a delta or snapshot. Returns `true` if it was present.
	pub fn remove<Q>(&mut self, item: &Q) -> Result<bool>
	where
		T: Borrow<Q>,
		Q: Hash + Eq + ?Sized,
	{
		// `take` removes it from the set and hands back the owned value so we can encode the delta.
		let Some(removed) = self.current.take(item) else {
			return Ok(false);
		};

		let delta_size = 1 + removed.encode_size() as u64;
		// `current` already reflects the removal.
		let snapshot_size = self.snapshot_size();

		if self.should_snapshot(delta_size, snapshot_size) {
			self.write_snapshot()?;
		} else {
			self.write_delta(REMOVE, &removed)?;
		}
		Ok(true)
	}

	/// Whether the item is currently in the set.
	pub fn contains<Q>(&self, item: &Q) -> bool
	where
		T: Borrow<Q>,
		Q: Hash + Eq + ?Sized,
	{
		self.current.contains(item)
	}

	/// The number of items currently in the set.
	pub fn len(&self) -> usize {
		self.current.len()
	}

	/// Whether the set is currently empty.
	pub fn is_empty(&self) -> bool {
		self.current.is_empty()
	}

	/// Iterate over the items currently in the set.
	pub fn iter(&self) -> impl Iterator<Item = &T> {
		self.current.iter()
	}

	/// Create a consumer for the underlying track.
	pub fn consume(&self) -> moq_net::TrackConsumer {
		self.track.consume()
	}

	/// Finish the track, closing any open group.
	pub fn finish(&mut self) -> Result<()> {
		if let Some(mut group) = self.group.take() {
			group.finish()?;
		}
		self.track.finish()?;
		Ok(())
	}

	/// The byte size of a full snapshot of the current set: a `u32` count plus each item
	/// length-prefixed. Computed from [`Item::size`] so we can size a frame without a scratch buffer.
	fn snapshot_size(&self) -> u64 {
		4 + self
			.current
			.iter()
			.map(|item| 4 + item.encode_size() as u64)
			.sum::<u64>()
	}

	fn should_snapshot(&self, delta_size: u64, snapshot_size: u64) -> bool {
		let Some(ratio) = self.config.delta_ratio else {
			return true;
		};
		if self.group.is_none() || self.group_frames >= MAX_DELTA_FRAMES {
			return true;
		}
		// Roll a snapshot once the replayed deltas would outgrow the budget relative to a snapshot.
		(self.group_delta_bytes + delta_size) as f64 > ratio * snapshot_size as f64
	}

	/// Append a `+`/`-` delta frame for one item to the open group, encoding straight into the frame.
	fn write_delta(&mut self, op: u8, item: &T) -> Result<()> {
		let size = 1 + item.encode_size() as u64;
		let group = self.group.as_mut().expect("delta requires an open group");

		let mut frame = group.create_frame(size.into())?;
		frame.put_u8(op);
		item.encode(&mut frame);
		frame.finish()?;

		self.group_frames += 1;
		self.group_delta_bytes += size;
		Ok(())
	}

	/// Start a new group whose first frame is a full snapshot of the current set, encoding straight
	/// into the frame so each item is copied just once.
	fn write_snapshot(&mut self) -> Result<()> {
		// The previous group is complete; no more frames will be appended to it.
		if let Some(mut group) = self.group.take() {
			group.finish()?;
		}

		let count = u32::try_from(self.current.len()).map_err(|_| Error::Malformed("set has too many items".into()))?;
		let mut group = self.track.append_group()?;

		let mut frame = group.create_frame(self.snapshot_size().into())?;
		frame.put_u32(count);
		for item in &self.current {
			let len = u32::try_from(item.encode_size()).map_err(|_| Error::Malformed("item is too large".into()))?;
			frame.put_u32(len);
			item.encode(&mut frame);
		}
		frame.finish()?;

		self.group_frames = 1;
		self.group_delta_bytes = 0;

		if self.config.delta_ratio.is_some() {
			// Keep the group open so future deltas can be appended.
			self.group = Some(group);
		} else {
			// Deltas disabled: one snapshot per group.
			group.finish()?;
		}

		Ok(())
	}
}

/// Consumes a set from a track, reconstructing it from snapshots and deltas.
pub struct Consumer<T> {
	track: moq_net::TrackConsumer,
	group: Option<moq_net::GroupConsumer>,
	current: HashSet<T>,
	frames_read: usize,
}

impl<T: Item> Consumer<T> {
	/// Create a consumer reading from the given track consumer.
	pub fn new(track: moq_net::TrackConsumer) -> Self {
		Self {
			track,
			group: None,
			current: HashSet::new(),
			frames_read: 0,
		}
	}

	/// Get the set after the next change, or `None` once the track ends.
	pub async fn next(&mut self) -> Result<Option<HashSet<T>>>
	where
		T: Unpin,
	{
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Poll for the set after the next change, without blocking.
	///
	/// Jumps to the newest group, decodes its snapshot, and applies deltas in order, yielding the
	/// reconstructed set after each frame. Switching to a newer group discards the older one.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<HashSet<T>>>> {
		// Drain to the newest group, resetting reconstruction state whenever we switch.
		let track_finished = loop {
			match self.track.poll_next_group(waiter)? {
				Poll::Ready(Some(group)) => {
					self.group = Some(group);
					self.current.clear();
					self.frames_read = 0;
				}
				Poll::Ready(None) => break true,
				Poll::Pending => break false,
			}
		};

		if let Some(group) = &mut self.group {
			match group.poll_read_frame(waiter)? {
				Poll::Ready(Some(frame)) => {
					self.apply(frame)?;
					return Poll::Ready(Ok(Some(self.current.clone())));
				}
				// The current group is exhausted; wait for a newer one.
				Poll::Ready(None) => self.group = None,
				Poll::Pending => return Poll::Pending,
			}
		}

		if track_finished {
			Poll::Ready(Ok(None))
		} else {
			Poll::Pending
		}
	}

	/// Apply one frame: frame 0 of a group is a snapshot, the rest are `+`/`-` deltas.
	fn apply(&mut self, frame: Bytes) -> Result<()> {
		if self.frames_read == 0 {
			self.current = decode_snapshot(frame)?;
		} else {
			let (op, mut item) = decode_delta(frame)?;
			let item = T::decode(&mut item)?;
			match op {
				INSERT => {
					self.current.insert(item);
				}
				REMOVE => {
					self.current.remove(&item);
				}
				other => return Err(Error::Malformed(format!("unknown op byte: {other:#04x}"))),
			}
		}
		self.frames_read += 1;
		Ok(())
	}
}

/// Decode a snapshot frame: a `u32` count then each item `u32`-length-prefixed.
///
/// Lengths are big-endian `u32` rather than QUIC varints so the format stays self-contained and
/// trivially matches the JS implementation (`@moq/data`).
fn decode_snapshot<T: Item>(mut frame: Bytes) -> Result<HashSet<T>> {
	if frame.remaining() < 4 {
		return Err(Error::Malformed("snapshot is missing its count".into()));
	}
	let count = frame.get_u32() as usize;

	// Every item costs at least its 4-byte length prefix, so a count larger than the remaining
	// bytes allow can't be real. Reject it before allocating so a malformed frame can't ask for a
	// huge capacity.
	if count > frame.remaining() / 4 {
		return Err(Error::Malformed("snapshot count exceeds frame bounds".into()));
	}

	let mut set = HashSet::with_capacity(count);
	for _ in 0..count {
		if frame.remaining() < 4 {
			return Err(Error::Malformed("snapshot is missing an item length".into()));
		}
		let len = frame.get_u32() as usize;
		if frame.remaining() < len {
			return Err(Error::Malformed("snapshot item runs past end of frame".into()));
		}
		set.insert(T::decode(&mut frame.split_to(len))?);
	}

	if frame.has_remaining() {
		return Err(Error::Malformed("snapshot has trailing bytes".into()));
	}

	Ok(set)
}

/// Decode a delta frame: one op byte followed by the item bytes.
fn decode_delta(mut frame: Bytes) -> Result<(u8, Bytes)> {
	if !frame.has_remaining() {
		return Err(Error::Malformed("empty delta frame".into()));
	}
	let op = frame.get_u8();
	Ok((op, frame))
}

#[cfg(test)]
mod test {
	use super::*;

	fn producer(config: Config) -> (Producer<String>, moq_net::TrackConsumer) {
		let track = moq_net::Track::new("test").produce();
		let consumer = track.consume();
		(Producer::new(track, config), consumer)
	}

	fn set(items: &[&str]) -> HashSet<String> {
		items.iter().map(|s| s.to_string()).collect()
	}

	/// Reconstruct every set a consumer yields, in order.
	fn drain(track: moq_net::TrackConsumer) -> Vec<HashSet<String>> {
		let mut consumer = Consumer::<String>::new(track);
		let waiter = kio::Waiter::noop();
		let mut out = Vec::new();
		while let Poll::Ready(Ok(Some(value))) = consumer.poll_next(&waiter) {
			out.push(value);
		}
		out
	}

	/// Build a snapshot frame for a set of strings, independent of the producer's encoder, as a
	/// decode oracle.
	fn snapshot_bytes(items: &[&str]) -> Vec<u8> {
		let mut buf = Vec::new();
		buf.extend_from_slice(&(items.len() as u32).to_be_bytes());
		for item in items {
			buf.extend_from_slice(&(item.len() as u32).to_be_bytes());
			buf.extend_from_slice(item.as_bytes());
		}
		buf
	}

	#[test]
	fn snapshot_roundtrip() {
		let frame = Bytes::from(snapshot_bytes(&["video", "audio", "captions"]));
		assert_eq!(
			decode_snapshot::<String>(frame).unwrap(),
			set(&["video", "audio", "captions"])
		);
	}

	#[test]
	fn malformed_snapshot_is_rejected() {
		// Trailing bytes past the declared items.
		let mut frame = snapshot_bytes(&["video"]);
		frame.push(0xff);
		assert!(decode_snapshot::<String>(Bytes::from(frame)).is_err());

		// A count far larger than the frame can hold must not allocate; it's rejected up front.
		let huge = Bytes::from(vec![0xff, 0xff, 0xff, 0xff]);
		assert!(decode_snapshot::<String>(huge).is_err());
	}

	#[test]
	fn deltas_off_snapshot_per_change() {
		let (mut producer, track) = producer(Config { delta_ratio: None });
		producer.insert("video".into()).unwrap();
		producer.insert("audio".into()).unwrap();
		producer.finish().unwrap();

		// Two changes => two snapshot groups; a late joiner only sees the latest full set.
		assert_eq!(track.latest(), Some(1));
		assert_eq!(drain(track).last().unwrap(), &set(&["video", "audio"]));
	}

	#[test]
	fn deltas_share_one_group() {
		let (mut producer, track) = producer(Config::default());
		producer.insert("video".into()).unwrap(); // snapshot, group 0
		producer.insert("audio".into()).unwrap(); // delta
		producer.remove("video").unwrap(); // delta
		producer.finish().unwrap();

		// All changes fit in a single group as snapshot + deltas.
		assert_eq!(track.latest(), Some(0));
		assert_eq!(drain(track).last().unwrap(), &set(&["audio"]));
	}

	#[test]
	fn redundant_insert_and_remove_write_nothing() {
		let (mut producer, track) = producer(Config::default());
		assert!(producer.insert("video".into()).unwrap());
		assert!(!producer.insert("video".into()).unwrap()); // already present
		assert!(!producer.remove("audio").unwrap()); // never present
		producer.finish().unwrap();

		// Only the first insert wrote a frame, so there's exactly one group.
		assert_eq!(track.latest(), Some(0));
		assert_eq!(drain(track).last().unwrap(), &set(&["video"]));
	}

	#[test]
	fn live_consumer_sees_each_change() {
		let (mut producer, track) = producer(Config::default());
		let mut consumer = Consumer::<String>::new(track);
		let waiter = kio::Waiter::noop();

		let next = |consumer: &mut Consumer<String>| match consumer.poll_next(&waiter) {
			Poll::Ready(Ok(Some(value))) => value,
			other => panic!("expected a set, got {other:?}"),
		};

		producer.insert("video".into()).unwrap();
		assert_eq!(next(&mut consumer), set(&["video"]));

		producer.insert("audio".into()).unwrap();
		assert_eq!(next(&mut consumer), set(&["video", "audio"]));

		producer.remove("video").unwrap();
		assert_eq!(next(&mut consumer), set(&["audio"]));
	}

	#[test]
	fn late_joiner_reconstructs_from_deltas() {
		let (mut producer, track) = producer(Config::default());
		producer.insert("a".into()).unwrap();
		producer.insert("b".into()).unwrap();
		producer.insert("c".into()).unwrap();
		producer.remove("a").unwrap();
		producer.finish().unwrap();

		// A consumer created only now still rebuilds the final set from snapshot + deltas.
		assert_eq!(drain(track).last().unwrap(), &set(&["b", "c"]));
	}

	#[test]
	fn frame_cap_rolls_snapshot() {
		// A huge ratio would otherwise keep everything in one group; the frame cap forces a roll.
		let (mut producer, track) = producer(Config {
			delta_ratio: Some(1_000_000.0),
		});

		// Snapshot (frame 0) plus MAX_DELTA_FRAMES - 1 deltas fill the first group, then one more rolls.
		for i in 0..=MAX_DELTA_FRAMES {
			producer.insert(format!("item-{i}")).unwrap();
		}
		producer.finish().unwrap();

		assert_eq!(track.latest(), Some(1));
		assert_eq!(drain(track).last().unwrap().len(), MAX_DELTA_FRAMES + 1);
	}

	#[test]
	fn binary_items_roundtrip() {
		let track = moq_net::Track::new("test").produce();
		let sub = track.consume();
		let mut producer = Producer::<Vec<u8>>::new(track, Config::default());

		producer.insert(vec![0x00, 0xff, 0x42]).unwrap();
		producer.insert(vec![0x01]).unwrap();
		producer.finish().unwrap();

		let mut consumer = Consumer::<Vec<u8>>::new(sub);
		let waiter = kio::Waiter::noop();
		let mut last = None;
		while let Poll::Ready(Ok(Some(value))) = consumer.poll_next(&waiter) {
			last = Some(value);
		}

		let expected: HashSet<Vec<u8>> = [vec![0x00, 0xff, 0x42], vec![0x01]].into_iter().collect();
		assert_eq!(last.unwrap(), expected);
	}

	#[test]
	fn custom_item_roundtrips() {
		// A user type that encodes itself directly into the frame buffer, no intermediate `Bytes`.
		#[derive(Clone, PartialEq, Eq, Hash, Debug)]
		struct Point {
			x: u16,
			y: u16,
		}

		// No `encode_size` override: the default counts the bytes via a `Sizer`.
		impl Item for Point {
			fn encode<B: BufMut>(&self, buf: &mut B) {
				buf.put_u16(self.x);
				buf.put_u16(self.y);
			}

			fn decode<B: Buf>(buf: &mut B) -> Result<Self> {
				if buf.remaining() != 4 {
					return Err(Error::Item("point must be 4 bytes".into()));
				}
				Ok(Point {
					x: buf.get_u16(),
					y: buf.get_u16(),
				})
			}
		}

		let track = moq_net::Track::new("test").produce();
		let sub = track.consume();
		let mut producer = Producer::<Point>::new(track, Config::default());
		producer.insert(Point { x: 1, y: 2 }).unwrap();
		producer.insert(Point { x: 3, y: 4 }).unwrap();
		producer.remove(&Point { x: 1, y: 2 }).unwrap();
		producer.finish().unwrap();

		let mut consumer = Consumer::<Point>::new(sub);
		let waiter = kio::Waiter::noop();
		let mut last = None;
		while let Poll::Ready(Ok(Some(value))) = consumer.poll_next(&waiter) {
			last = Some(value);
		}
		assert_eq!(last.unwrap(), [Point { x: 3, y: 4 }].into_iter().collect());
	}
}
