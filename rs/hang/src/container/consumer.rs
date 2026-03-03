use std::collections::VecDeque;

use buf_list::BufList;
use futures::{StreamExt, stream::FuturesUnordered};

use super::{Frame, Timestamp};
use crate::Error;

/// A consumer for hang-formatted media tracks with timestamp reordering.
///
/// This wraps a `moq_lite::TrackConsumer` and adds hang-specific functionality
/// like timestamp decoding, latency management, and frame buffering.
///
/// ## Latency Management
///
/// The consumer can skip groups that are too far behind to maintain low latency.
/// Configure the maximum acceptable delay through the consumer's latency settings.
pub struct OrderedConsumer {
	pub track: moq_lite::TrackConsumer,

	// The current group that we are reading from.
	current: Option<GroupReader>,

	// Future groups that we are monitoring, deciding based on [latency] whether to skip.
	pending: VecDeque<GroupReader>,

	// The maximum timestamp seen thus far, or zero because that's easier than None.
	max_timestamp: Timestamp,

	// The maximum buffer size before skipping a group.
	max_latency: std::time::Duration,
}

impl OrderedConsumer {
	/// Create a new OrderedConsumer wrapping the given moq-lite consumer.
	pub fn new(track: moq_lite::TrackConsumer, max_latency: std::time::Duration) -> Self {
		Self {
			track,
			current: None,
			pending: VecDeque::new(),
			max_timestamp: Timestamp::default(),
			max_latency,
		}
	}

	/// Read the next frame from the track.
	///
	/// This method handles timestamp decoding, group ordering, and latency management
	/// automatically. It will skip groups that are too far behind to maintain the
	/// configured latency target.
	///
	/// Returns `None` when the track has ended.
	pub async fn read(&mut self) -> Result<Option<Frame>, Error> {
		let latency = self.max_latency.try_into()?;
		loop {
			let cutoff = self.max_timestamp.checked_add(latency)?;

			// Keep track of all pending groups, buffering until we detect a timestamp far enough in the future.
			// This is a race; only the first group will succeed.
			// TODO is there a way to do this without FuturesUnordered?
			let mut buffering = FuturesUnordered::new();
			for (index, pending) in self.pending.iter_mut().enumerate() {
				buffering.push(async move { (index, pending.buffer_until(cutoff).await) })
			}

			tokio::select! {
				biased;
				Some(res) = async { Some(self.current.as_mut()?.read().await) } => {
					drop(buffering);

					match res {
						// Got the next frame.
						Ok(Some(frame)) => {
							tracing::trace!(?frame, "read frame");
							self.max_timestamp = self.max_timestamp.max(frame.timestamp);
							return Ok(Some(frame));
						}
						Ok(None) | Err(_) => {
							// Group ended, instantly move to the next group.
							// We don't care about errors, which will happen if the group is closed early.
							self.current = self.pending.pop_front();
							continue;
						}
					};
				},
				Some(res) = async { self.track.next_group().await.transpose() } => {
					let group = GroupReader::new(res?);
					drop(buffering);

					match self.current.as_ref() {
						Some(current) if group.info.sequence < current.info.sequence => {
							// Ignore old groups
							tracing::debug!(old = ?group.info.sequence, current = ?current.info.sequence, "skipping old group");
						},
						Some(_) => {
							// Insert into pending based on the sequence number ascending.
							let index = self.pending.partition_point(|g| g.info.sequence < group.info.sequence);
							self.pending.insert(index, group);
						},
						None => self.current = Some(group),
					};
				},
				Some((index, timestamp)) = buffering.next() => {
					if self.current.is_some() {
						tracing::debug!(old = ?self.max_timestamp, new = ?timestamp, buffer = ?self.max_latency, "skipping slow group");
					}

					drop(buffering);

					if index > 0 {
						self.pending.drain(0..index);
						tracing::debug!(count = index, "skipping additional groups");
					}

					self.current = self.pending.pop_front();
				}
				else => return Ok(None),
			}
		}
	}

	/// Set the maximum latency tolerance for this consumer.
	///
	/// Groups with timestamps older than `max_timestamp - max_latency` will be skipped.
	pub fn set_max_latency(&mut self, max: std::time::Duration) {
		self.max_latency = max;
	}

	/// Wait until the track is closed.
	pub async fn closed(&self) -> Result<(), Error> {
		Ok(self.track.closed().await?)
	}
}

impl From<OrderedConsumer> for moq_lite::TrackConsumer {
	fn from(inner: OrderedConsumer) -> Self {
		inner.track
	}
}

impl std::ops::Deref for OrderedConsumer {
	type Target = moq_lite::TrackConsumer;

	fn deref(&self) -> &Self::Target {
		&self.track
	}
}

/// Internal reader for a group of frames.
struct GroupReader {
	// The group.
	group: moq_lite::GroupConsumer,

	// The current frame index
	index: usize,

	// The any buffered frames in the group.
	buffered: VecDeque<Frame>,

	// The max timestamp in the group
	max_timestamp: Option<Timestamp>,
}

impl GroupReader {
	fn new(group: moq_lite::GroupConsumer) -> Self {
		Self {
			group,
			index: 0,
			buffered: VecDeque::new(),
			max_timestamp: None,
		}
	}

	async fn read(&mut self) -> Result<Option<Frame>, Error> {
		if let Some(frame) = self.buffered.pop_front() {
			Ok(Some(frame))
		} else {
			self.read_unbuffered().await
		}
	}

	async fn read_unbuffered(&mut self) -> Result<Option<Frame>, Error> {
		let Some(mut frame) = self.group.next_frame().await? else {
			return Ok(None);
		};
		let payload = frame.read_chunks().await?;

		let mut payload = BufList::from_iter(payload);

		let timestamp = Timestamp::decode(&mut payload)?;

		let frame = Frame {
			keyframe: (self.index == 0),
			timestamp,
			payload,
		};

		self.index += 1;
		self.max_timestamp = Some(self.max_timestamp.unwrap_or_default().max(timestamp));

		Ok(Some(frame))
	}

	// Keep reading and buffering new frames, returning when `max` is larger than or equal to the cutoff.
	// This will BLOCK FOREVER if the group has ended early; it's intended to be used within select!
	async fn buffer_until(&mut self, cutoff: Timestamp) -> Timestamp {
		loop {
			match self.max_timestamp {
				Some(timestamp) if timestamp >= cutoff => return timestamp,
				_ => (),
			}

			match self.read_unbuffered().await {
				Ok(Some(frame)) => self.buffered.push_back(frame),
				// Otherwise block forever so we don't return from FuturesUnordered
				_ => std::future::pending().await,
			}
		}
	}
}

impl std::ops::Deref for GroupReader {
	type Target = moq_lite::GroupConsumer;

	fn deref(&self) -> &Self::Target {
		&self.group
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::time::Duration;

	use bytes::Bytes;

	fn ts(micros: u64) -> Timestamp {
		Timestamp::from_micros(micros).unwrap()
	}

	/// Write a finished group with explicit sequence and timestamps.
	/// First frame is marked as keyframe by the consumer (index == 0).
	fn write_group(track: &mut moq_lite::TrackProducer, sequence: u64, timestamps: &[Timestamp]) {
		let mut group = track.create_group(moq_lite::Group { sequence }).unwrap();
		for &timestamp in timestamps {
			let frame = Frame {
				keyframe: false, // ignored by encode; consumer sets keyframe based on index
				timestamp,
				payload: BufList::from_iter(vec![Bytes::from_static(&[0xDE, 0xAD])]),
			};
			frame.encode(&mut group).unwrap();
		}
		group.finish().unwrap();
	}

	/// Drain all available frames with a per-read timeout.
	async fn read_all(consumer: &mut OrderedConsumer) -> Result<Vec<Frame>, crate::Error> {
		let mut frames = Vec::new();
		loop {
			match tokio::time::timeout(Duration::from_millis(200), consumer.read()).await {
				Ok(Ok(Some(frame))) => frames.push(frame),
				Ok(Ok(None)) => break,
				Ok(Err(e)) => return Err(e),
				Err(_) => panic!(
					"read_all: OrderedConsumer::read timed out after 200ms ({} frames collected so far)",
					frames.len()
				),
			}
		}
		Ok(frames)
	}

	// ---- Basic Reading ----

	#[tokio::test]
	async fn read_single_group() {
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(500));

		write_group(&mut track, 0, &[ts(0)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1);
		assert_eq!(frames[0].timestamp, ts(0));
		assert!(frames[0].keyframe);

		// Next read returns None (track ended)
		assert!(consumer.read().await.unwrap().is_none());
	}

	#[tokio::test]
	async fn read_multiple_frames_single_group() {
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(500));

		write_group(&mut track, 0, &[ts(0), ts(33_000), ts(66_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 3);
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(33_000));
		assert_eq!(frames[2].timestamp, ts(66_000));

		// Only first frame is keyframe
		assert!(frames[0].keyframe);
		assert!(!frames[1].keyframe);
		assert!(!frames[2].keyframe);
	}

	#[tokio::test]
	async fn read_multiple_groups_within_latency() {
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(500));

		// 5 groups, 20ms spacing. Total span = 80ms, well within 500ms latency.
		for i in 0..5u64 {
			write_group(&mut track, i, &[ts(i * 20_000)]);
		}
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 5);
	}

	// ---- Latency Skipping ----
	//
	// These tests use an "unfinished group 0" pattern: group 0 is created but not
	// finished, causing the consumer's biased select! to block on current.read().
	// Meanwhile, subsequent finished groups accumulate in the pending queue via
	// next_group, allowing buffer_until to trigger latency-based skipping.

	#[tokio::test]
	async fn latency_skip_delivers_recent_groups() {
		tokio::time::pause();
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(100));

		// Group 0: 5 frames, NOT finished (blocks consumer)
		let mut group0 = track.create_group(moq_lite::Group { sequence: 0 }).unwrap();
		for f in 0..5u64 {
			Frame {
				keyframe: false,
				timestamp: ts(f * 2_000),
				payload: BufList::from_iter(vec![Bytes::from_static(&[0xDE, 0xAD])]),
			}
			.encode(&mut group0)
			.unwrap();
		}

		// Groups 1-19: finished, 15ms spacing, 5 frames each
		for g in 1..20u64 {
			let timestamps: Vec<_> = (0..5).map(|f| ts(g * 15_000 + f * 2_000)).collect();
			write_group(&mut track, g, &timestamps);
		}
		track.finish().unwrap();

		// Finish group 0 after consumer has had time to accumulate pending groups
		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(50)).await;
			group0.finish().unwrap();
		});

		let frames = read_all(&mut consumer).await.unwrap();
		// Group 0's 5 frames + some later groups (earlier ones skipped by latency)
		assert!(frames.len() >= 25, "Expected >= 25 frames, got {}", frames.len());
		finisher.await.expect("finisher task panicked");
	}

	#[tokio::test]
	async fn zero_latency_skips_aggressively() {
		tokio::time::pause();
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::ZERO);

		// Group 0: 1 frame at a HIGH timestamp, NOT finished.
		// This makes the cutoff high (max_timestamp + 0 = 400ms), so
		// buffer_until blocks for groups whose timestamps are < 400ms.
		let mut group0 = track.create_group(moq_lite::Group { sequence: 0 }).unwrap();
		Frame {
			keyframe: false,
			timestamp: ts(400_000),
			payload: BufList::from_iter(vec![Bytes::from_static(&[0xDE, 0xAD])]),
		}
		.encode(&mut group0)
		.unwrap();

		// Groups 1-9: finished, 50ms spacing, 3 frames each
		// Groups 1-7 have max timestamps < 400ms (blocked by buffer_until)
		// Group 8+ have timestamps >= 400ms (trigger latency skip)
		for g in 1..10u64 {
			let timestamps: Vec<_> = (0..3).map(|f| ts(g * 50_000 + f * 5_000)).collect();
			write_group(&mut track, g, &timestamps);
		}
		track.finish().unwrap();

		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(50)).await;
			group0.finish().unwrap();
		});

		let frames = read_all(&mut consumer).await.unwrap();
		// Group 0's 1 frame + skipped groups 1-7 + groups 8-9 (6 frames).
		// Total without skip = 28. With skip, should be <= 7.
		assert!(
			frames.len() < 28,
			"Expected skipping with 0ms latency, got {} frames",
			frames.len()
		);
		assert!(!frames.is_empty(), "Expected at least some frames");
		finisher.await.expect("finisher task panicked");
	}

	#[tokio::test]
	async fn latency_skip_correctness() {
		tokio::time::pause();
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(100));

		// Group 0: 1 frame, NOT finished
		let mut group0 = track.create_group(moq_lite::Group { sequence: 0 }).unwrap();
		Frame {
			keyframe: false,
			timestamp: ts(0),
			payload: BufList::from_iter(vec![Bytes::from_static(&[0xDE, 0xAD])]),
		}
		.encode(&mut group0)
		.unwrap();

		// Groups 1-9: 30ms spacing, 1 frame each
		for g in 1..10u64 {
			write_group(&mut track, g, &[ts(g * 30_000)]);
		}
		track.finish().unwrap();

		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(50)).await;
			group0.finish().unwrap();
		});

		let frames = read_all(&mut consumer).await.unwrap();
		assert!(!frames.is_empty(), "Expected at least some frames");

		// Some groups should have been skipped (fewer frames than total 10)
		let post_skip: Vec<_> = frames.iter().filter(|f| f.timestamp > ts(0)).collect();
		assert!(
			post_skip.len() < 9,
			"Expected some groups to be skipped, got {} post-skip frames",
			post_skip.len()
		);

		// Post-skip frames should span a bounded range
		if post_skip.len() >= 2 {
			let max_ts = post_skip.iter().map(|f| f.timestamp).max().unwrap();
			let min_ts = post_skip.iter().map(|f| f.timestamp).min().unwrap();
			let span_micros = max_ts.as_micros() - min_ts.as_micros();
			let total_span = 9u128 * 30_000; // full span without skipping
			assert!(
				span_micros < total_span,
				"Post-skip span {}us should be less than total span {}us",
				span_micros,
				total_span
			);
		}
		finisher.await.expect("finisher task panicked");
	}

	// ---- Group Ordering ----

	#[tokio::test]
	async fn groups_delivered_in_sequence_order() {
		tokio::time::pause();
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(500));

		// Group 0: 1 frame, NOT finished (blocks consumer, lets groups 2 and 1 accumulate in pending)
		let mut group0 = track.create_group(moq_lite::Group { sequence: 0 }).unwrap();
		Frame {
			keyframe: false,
			timestamp: ts(0),
			payload: BufList::from_iter(vec![Bytes::from_static(&[0xDE, 0xAD])]),
		}
		.encode(&mut group0)
		.unwrap();

		// Write groups 2 then 1 (out of sequence order)
		write_group(&mut track, 2, &[ts(60_000)]);
		write_group(&mut track, 1, &[ts(30_000)]);
		track.finish().unwrap();

		// Finish group 0 so the consumer can proceed to pending groups
		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(10)).await;
			group0.finish().unwrap();
		});

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 3);

		// Pending queue sorts by sequence, so delivery order is 0, 1, 2
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(30_000));
		assert_eq!(frames[2].timestamp, ts(60_000));
		finisher.await.expect("finisher task panicked");
	}

	#[tokio::test]
	async fn adjacent_group_flushed_immediately() {
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(500));

		write_group(&mut track, 0, &[ts(0)]);
		write_group(&mut track, 1, &[ts(30_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 2);
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(30_000));
	}

	// ---- B-frames ----

	#[tokio::test]
	async fn bframes_within_group() {
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(500));

		// B-frame decode order: timestamps [0, 66ms, 33ms]
		write_group(&mut track, 0, &[ts(0), ts(66_000), ts(33_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 3);
		// Delivered in write order (decode order), not presentation order
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(66_000));
		assert_eq!(frames[2].timestamp, ts(33_000));
	}

	// ---- Track Lifecycle ----

	#[tokio::test]
	async fn empty_track_returns_none() {
		tokio::time::pause();
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(500));

		track.finish().unwrap();

		let result = tokio::time::timeout(Duration::from_millis(200), consumer.read()).await;
		match result {
			Ok(Ok(None)) => {} // expected: track ended
			Ok(Ok(Some(_))) => panic!("expected None for empty track, got Some"),
			Ok(Err(e)) => panic!("expected None for empty track, got error: {e}"),
			Err(_) => panic!("should not hang on empty track"),
		}
	}

	#[tokio::test]
	async fn track_closed_with_error() {
		tokio::time::pause();
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(500));

		write_group(&mut track, 0, &[ts(0)]);
		track.abort(moq_lite::Error::Cancel).unwrap();

		// Consumer should not hang; it should return frames or error gracefully
		let result = tokio::time::timeout(Duration::from_millis(500), async {
			let mut frames = Vec::new();
			while let Ok(Some(frame)) = consumer.read().await {
				frames.push(frame);
			}
			frames
		})
		.await;

		assert!(result.is_ok(), "Consumer should not hang after track error");
	}

	#[tokio::test]
	async fn closed_resolves_when_track_ends() {
		tokio::time::pause();
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(500));

		// closed() should not resolve yet
		assert!(
			tokio::time::timeout(Duration::from_millis(50), consumer.closed())
				.await
				.is_err()
		);

		// finish() + drop triggers the Closed/Dropped state that closed() waits for
		track.finish().unwrap();
		drop(track);

		// closed() should resolve now
		tokio::time::timeout(Duration::from_millis(200), consumer.closed())
			.await
			.expect("timeout expired waiting for closed()")
			.expect("consumer.closed() returned an error");
	}

	// ---- Gap Recovery ----

	#[tokio::test]
	async fn gap_in_group_sequence_recovery() {
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(100));

		// Groups 0, 1 then skip 2, write 3-6
		write_group(&mut track, 0, &[ts(0), ts(20_000)]);
		write_group(&mut track, 1, &[ts(40_000), ts(60_000)]);
		// Gap at group 2
		write_group(&mut track, 3, &[ts(120_000), ts(140_000)]);
		write_group(&mut track, 4, &[ts(160_000), ts(180_000)]);
		write_group(&mut track, 5, &[ts(200_000), ts(220_000)]);
		write_group(&mut track, 6, &[ts(240_000), ts(260_000)]);
		track.finish().unwrap();

		// Consumer must not deadlock on the missing group 2
		let frames = read_all(&mut consumer).await.unwrap();
		assert!(frames.len() >= 4, "Expected >= 4 frames, got {}", frames.len());
	}

	#[tokio::test]
	async fn gap_at_start_of_sequence() {
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(80));

		// First group at sequence 5 (simulating joining mid-stream), gap at 6
		write_group(&mut track, 5, &[ts(0), ts(20_000)]);
		write_group(&mut track, 7, &[ts(80_000), ts(100_000)]);
		write_group(&mut track, 8, &[ts(120_000), ts(140_000)]);
		write_group(&mut track, 9, &[ts(160_000), ts(180_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert!(frames.len() >= 4, "Expected >= 4 frames, got {}", frames.len());
	}

	// ---- Frame Decoding ----

	#[tokio::test]
	async fn frame_timestamp_and_keyframe_decoding() {
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(500));

		write_group(&mut track, 0, &[ts(0), ts(33_333), ts(66_666)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 3);

		assert_eq!(frames[0].timestamp, ts(0));
		assert!(frames[0].keyframe);

		assert_eq!(frames[1].timestamp, ts(33_333));
		assert!(!frames[1].keyframe);

		assert_eq!(frames[2].timestamp, ts(66_666));
		assert!(!frames[2].keyframe);
	}

	#[tokio::test]
	async fn frame_payload_preserved() {
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(500));

		let payload_bytes = vec![0x01, 0x02, 0x03, 0x04, 0x05];
		let mut group = track.create_group(moq_lite::Group { sequence: 0 }).unwrap();
		Frame {
			keyframe: false,
			timestamp: ts(0),
			payload: BufList::from_iter(vec![Bytes::from(payload_bytes.clone())]),
		}
		.encode(&mut group)
		.unwrap();
		group.finish().unwrap();
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1);

		use bytes::Buf;
		let mut received = Vec::new();
		let mut payload = frames[0].payload.clone();
		while payload.has_remaining() {
			received.push(payload.get_u8());
		}
		assert_eq!(received, payload_bytes);
	}

	// ---- Regression ----

	/// Regression test for de92d2c7: buffer_until previously called self.read()
	/// instead of self.read_unbuffered(), causing an infinite loop when a pending
	/// group had buffered frames from a prior (dropped) buffer_until call.
	///
	/// Setup: group 0 is unfinished (blocks current.read), group 1 is finished
	/// (accumulates buffered frames via buffer_until). A delayed group 2 arrival
	/// causes the select! to restart, creating a new buffer_until for group 1
	/// which now has buffered frames. With the fix, read_unbuffered returns None
	/// and blocks; with the bug, read() re-reads buffered frames infinitely.
	#[tokio::test]
	async fn no_infinite_loop_with_buffered_frames() {
		tokio::time::pause();
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_secs(10));

		// Group 0: 1 frame, NOT finished
		let mut group0 = track.create_group(moq_lite::Group { sequence: 0 }).unwrap();
		Frame {
			keyframe: false,
			timestamp: ts(0),
			payload: BufList::from_iter(vec![Bytes::from_static(&[0xDE, 0xAD])]),
		}
		.encode(&mut group0)
		.unwrap();

		// Group 1: finished (buffer_until will buffer its frames)
		write_group(&mut track, 1, &[ts(100_000)]);

		let finisher = tokio::spawn(async move {
			// After consumer has buffered group 1's frames via buffer_until...
			tokio::time::sleep(Duration::from_millis(20)).await;
			// Write group 2: next_group fires, drops current buffer_until for group 1
			write_group(&mut track, 2, &[ts(200_000)]);
			// Then finish group 0: consumer proceeds, re-creates buffer_until for group 1
			tokio::time::sleep(Duration::from_millis(20)).await;
			group0.finish().unwrap();
			track.finish().unwrap();
		});

		// Must complete within 2 seconds (with the bug, this would hang)
		let frames = tokio::time::timeout(Duration::from_secs(2), async {
			let mut frames = Vec::new();
			while let Some(frame) = consumer.read().await.unwrap() {
				frames.push(frame);
			}
			frames
		})
		.await
		.expect("consumer hung — possible infinite loop regression");

		assert_eq!(frames.len(), 3);
		finisher.await.expect("finisher task panicked");
	}

	// ---- Edge Cases ----

	#[tokio::test]
	async fn large_timestamps() {
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_secs(3700));

		// 1 hour = 3,600,000,000 microseconds
		let one_hour = 3_600_000_000u64;
		write_group(&mut track, 0, &[ts(one_hour)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1);
		assert_eq!(frames[0].timestamp, ts(one_hour));
		assert_eq!(frames[0].timestamp.as_micros(), one_hour as u128);
	}

	#[tokio::test]
	async fn set_max_latency_changes_behavior() {
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_secs(10));

		write_group(&mut track, 0, &[ts(0)]);
		track.finish().unwrap();

		// Read with initial large latency
		let frame = consumer.read().await.unwrap().unwrap();
		assert_eq!(frame.timestamp, ts(0));

		// Change latency — verify it doesn't panic and consumer still works
		consumer.set_max_latency(Duration::from_millis(100));

		// Track is already finished, so next read returns None
		assert!(consumer.read().await.unwrap().is_none());
	}

	/// Verify max_timestamp tracks the true maximum through B-frame reordering.
	///
	/// With B-frame decode order [0, 66ms, 33ms], the bug assigned max_timestamp = 33ms
	/// (last frame) instead of 66ms (true max). This lowered the latency cutoff, causing
	/// premature skipping of subsequent groups under tight latency settings.
	///
	/// Setup: group 0 is unfinished with B-frames, group 1 at ts(100ms), latency = 40ms.
	/// Bug:   cutoff = 33ms + 40ms = 73ms → group 1's buffer_until sees 100ms >= 73ms → skip
	/// Fix:   cutoff = 66ms + 40ms = 106ms → 100ms < 106ms → no skip, all groups delivered
	#[tokio::test]
	async fn max_timestamp_tracks_through_bframes() {
		tokio::time::pause();
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(40));

		// Group 0: B-frame decode order [0, 66ms, 33ms], NOT finished (blocks consumer)
		let mut group0 = track.create_group(moq_lite::Group { sequence: 0 }).unwrap();
		for &timestamp in &[ts(0), ts(66_000), ts(33_000)] {
			Frame {
				keyframe: false,
				timestamp,
				payload: BufList::from_iter(vec![Bytes::from_static(&[0xDE, 0xAD])]),
			}
			.encode(&mut group0)
			.unwrap();
		}

		// Group 1: finished, at ts(100ms)
		write_group(&mut track, 1, &[ts(100_000)]);
		track.finish().unwrap();

		// Finish group 0 after consumer has had time to accumulate pending groups
		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(50)).await;
			group0.finish().unwrap();
		});

		let frames = tokio::time::timeout(Duration::from_secs(2), async {
			let mut frames = Vec::new();
			while let Some(frame) = consumer.read().await.unwrap() {
				frames.push(frame);
			}
			frames
		})
		.await
		.expect("consumer hung — max_timestamp regression");

		assert_eq!(frames.len(), 4, "Expected all 4 frames, got {}", frames.len());
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(66_000));
		assert_eq!(frames[2].timestamp, ts(33_000));
		assert_eq!(frames[3].timestamp, ts(100_000));
		finisher.await.expect("finisher task panicked");
	}
}
