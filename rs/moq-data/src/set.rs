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
//! - **snapshot** (frame 0): `varint(count)` followed by `count` repetitions of
//!   `varint(len)` then `len` item bytes.
//! - **delta** (frame 1+): a one-byte op (`+` = `0x2B` insert, `-` = `0x2D` remove) followed by the
//!   item bytes, which run to the end of the frame.

use std::borrow::Borrow;
use std::collections::HashSet;
use std::hash::Hash;
use std::task::Poll;

use bytes::{Buf, BufMut, Bytes, BytesMut};

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
/// Encoding must be deterministic and round-trip: `Item::decode(item.encode())` must equal `item`.
/// Two items are the same set member iff they're equal under [`Eq`]/[`Hash`], so distinct items must
/// encode to distinct bytes.
pub trait Item: Clone + Eq + Hash {
	/// Encode the item to its wire bytes.
	fn encode(&self) -> Bytes;

	/// Decode an item from wire bytes.
	fn decode(bytes: Bytes) -> Result<Self>
	where
		Self: Sized;
}

impl Item for String {
	fn encode(&self) -> Bytes {
		Bytes::copy_from_slice(self.as_bytes())
	}

	fn decode(bytes: Bytes) -> Result<Self> {
		String::from_utf8(bytes.into()).map_err(|err| Error::Item(err.to_string()))
	}
}

impl Item for Vec<u8> {
	fn encode(&self) -> Bytes {
		Bytes::copy_from_slice(self)
	}

	fn decode(bytes: Bytes) -> Result<Self> {
		Ok(bytes.into())
	}
}

impl Item for Bytes {
	fn encode(&self) -> Bytes {
		self.clone()
	}

	fn decode(bytes: Bytes) -> Result<Self> {
		Ok(bytes)
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

		// Encode while we still own the item, then move it into the set so the snapshot reflects it.
		let bytes = item.encode();
		self.current.insert(item);
		self.publish(INSERT, bytes)?;
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
		let bytes = removed.encode();
		self.publish(REMOVE, bytes)?;
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

	/// Publish a single change, either as a delta on the open group or a fresh snapshot group. The
	/// change is already reflected in `self.current`, so a snapshot here captures it.
	fn publish(&mut self, op: u8, item: Bytes) -> Result<()> {
		let snapshot = encode_snapshot(&self.current)?;
		let delta_len = 1 + item.len() as u64;

		if self.should_snapshot(delta_len, snapshot.len() as u64) {
			self.write_snapshot(snapshot)
		} else {
			let group = self.group.as_mut().expect("delta requires an open group");
			group.write_frame(encode_delta(op, &item))?;
			self.group_frames += 1;
			self.group_delta_bytes += delta_len;
			Ok(())
		}
	}

	fn should_snapshot(&self, delta_len: u64, snapshot_len: u64) -> bool {
		let Some(ratio) = self.config.delta_ratio else {
			return true;
		};
		if self.group.is_none() || self.group_frames >= MAX_DELTA_FRAMES {
			return true;
		}
		// Roll a snapshot once the replayed deltas would outgrow the budget relative to a snapshot.
		(self.group_delta_bytes + delta_len) as f64 > ratio * snapshot_len as f64
	}

	fn write_snapshot(&mut self, snapshot: Bytes) -> Result<()> {
		// The previous group is complete; no more frames will be appended to it.
		if let Some(mut group) = self.group.take() {
			group.finish()?;
		}

		let mut group = self.track.append_group()?;
		group.write_frame(snapshot)?;
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
			let (op, item) = decode_delta(frame)?;
			let item = T::decode(item)?;
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

/// Encode the full set as a snapshot frame: a `u32` count then each item `u32`-length-prefixed.
///
/// Lengths are big-endian `u32` rather than QUIC varints so the format stays self-contained and
/// trivially matches the JS implementation (`@moq/data`).
fn encode_snapshot<T: Item>(set: &HashSet<T>) -> Result<Bytes> {
	let count = u32::try_from(set.len()).map_err(|_| Error::Malformed("set has too many items".into()))?;

	let mut buf = BytesMut::new();
	buf.put_u32(count);
	for item in set {
		let bytes = item.encode();
		let len = u32::try_from(bytes.len()).map_err(|_| Error::Malformed("item is too large".into()))?;
		buf.put_u32(len);
		buf.put_slice(&bytes);
	}
	Ok(buf.freeze())
}

fn decode_snapshot<T: Item>(mut frame: Bytes) -> Result<HashSet<T>> {
	if frame.remaining() < 4 {
		return Err(Error::Malformed("snapshot is missing its count".into()));
	}
	let count = frame.get_u32();

	let mut set = HashSet::with_capacity(count as usize);
	for _ in 0..count {
		if frame.remaining() < 4 {
			return Err(Error::Malformed("snapshot is missing an item length".into()));
		}
		let len = frame.get_u32() as usize;
		if frame.remaining() < len {
			return Err(Error::Malformed("snapshot item runs past end of frame".into()));
		}
		set.insert(T::decode(frame.split_to(len))?);
	}
	Ok(set)
}

/// Encode a delta frame: one op byte followed by the item bytes.
fn encode_delta(op: u8, item: &Bytes) -> Bytes {
	let mut buf = BytesMut::with_capacity(1 + item.len());
	buf.put_u8(op);
	buf.put_slice(item);
	buf.freeze()
}

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

	#[test]
	fn snapshot_roundtrip() {
		let original = set(&["video", "audio", "captions"]);
		let frame = encode_snapshot(&original).unwrap();
		assert_eq!(decode_snapshot::<String>(frame).unwrap(), original);
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
}
