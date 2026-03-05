use std::collections::VecDeque;
use std::task::{Poll, ready};

use buf_list::BufList;

use super::{Frame, Timestamp};
use crate::Error;

/// A frame returned by [`OrderedConsumer::read()`] with group context.
#[derive(Clone, Debug)]
pub struct OrderedFrame {
	/// The presentation timestamp for this frame.
	pub timestamp: Timestamp,

	/// The encoded media data for this frame, split into chunks.
	pub payload: BufList,

	/// The group sequence number this frame belongs to.
	pub group: u64,

	/// The frame index within the group (0 = first frame in the group).
	///
	/// With duration-based grouping (e.g. audio), the first frame is not
	/// necessarily a keyframe — it only denotes position within the group.
	pub index: usize,
}

impl OrderedFrame {
	/// Returns true if this is the first frame in the group (index 0).
	pub fn is_keyframe(&self) -> bool {
		self.index == 0
	}
}

/// Lossy conversion: discards ordering metadata (`group` and `frame` fields).
impl From<OrderedFrame> for Frame {
	fn from(ordered: OrderedFrame) -> Self {
		Frame {
			timestamp: ordered.timestamp,
			payload: ordered.payload,
		}
	}
}

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

	// The current group that we want to read from
	current: u64,

	// Groups that we are monitoring, sorted by sequence ascending.
	pending: VecDeque<GroupBuffer>,

	// When true, we haven't returned a frame yet and need to select the first group.
	// We wait until we have at least one frame before finalizing `current`
	startup: bool,

	// The maximum buffer size before skipping a group.
	max_latency: std::time::Duration,
}

impl OrderedConsumer {
	/// Create a new OrderedConsumer wrapping the given moq-lite consumer.
	pub fn new(track: moq_lite::TrackConsumer, max_latency: std::time::Duration) -> Self {
		Self {
			track,
			current: 0,
			pending: VecDeque::new(),
			startup: true,
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
	pub async fn read(&mut self) -> Result<Option<OrderedFrame>, Error> {
		conducer::wait(|waiter| self.poll_read(waiter)).await
	}

	/// Poll-based implementation of the read loop.
	///
	/// Uses a single waiter that gets registered on all relevant conducer channels,
	/// avoiding the need for `tokio::select!` or `FuturesUnordered`.
	fn poll_read(&mut self, waiter: &conducer::Waiter) -> Poll<Result<Option<OrderedFrame>, Error>> {
		// Grab any new groups from the track, recording whether the track is finished.
		let finished = self.poll_read_finish(waiter)?.is_ready();

		// On startup, we want to poll every pending group and advance self.current to the first with a frame.
		if self.startup {
			// NOTE: We loop in ascending order, so earlier groups will win the race.
			for (i, group) in self.pending.iter_mut().enumerate() {
				// We call poll_min_timestamp to try to buffer at least one frame per group.
				// This returns Ready(Ok) if there is a buffered frame.
				if !matches!(group.poll_min_timestamp(waiter), Poll::Ready(Ok(_))) {
					continue;
				}

				// Start reading from this group and skip any previous groups.
				self.current = group.info.sequence;
				self.startup = false;
				self.pending.drain(0..i);
				break;
			}
		}

		loop {
			// Return the next frame from the current group if possible.
			// If the current group is finished or errored, advance to the next group.
			while let Some(group) = self.pending.front_mut()
				&& group.info.sequence <= self.current
			{
				match group.poll_read(waiter) {
					Poll::Ready(Ok(Some(frame))) => return Poll::Ready(Ok(Some(frame))),
					// Still blocked on this group, don't skip it yet.
					Poll::Pending => break,
					Poll::Ready(Err(e)) => {
						tracing::warn!(error = ?e, "error reading current group, skipping");
					}
					// No more frames, advance to next group.
					Poll::Ready(Ok(None)) => {}
				}

				self.pending.pop_front();
				self.current += 1
			}

			// Loop in ascending order to get the min, avoiding spurious wakeups.
			let mut min_timestamp = std::time::Duration::MAX;
			let mut min_idx = None;

			for (i, group) in self.pending.iter_mut().enumerate() {
				if group.info.sequence <= self.current {
					continue;
				}

				if let Poll::Ready(Ok(ts)) = group.poll_min_timestamp(waiter) {
					min_timestamp = min_timestamp.min(ts.into());
					min_idx = Some(i);
					break; // We know future groups won't be older than this.
				}
			}

			// Loop in descending order to get the max, avoiding spurious wakeups.
			let mut max_timestamp = std::time::Duration::ZERO;
			for group in self.pending.iter_mut().rev() {
				if group.info.sequence <= self.current {
					break;
				}

				if let Poll::Ready(Ok(ts)) = group.poll_max_timestamp(waiter) {
					max_timestamp = max_timestamp.max(ts.into());
					break; // We know older groups won't be newer than this.
				}
			}

			if let Some(new_idx) = min_idx
				&& max_timestamp.saturating_sub(min_timestamp) >= self.max_latency
			{
				self.pending.drain(0..new_idx);
				let new_current = self.pending.front().map(|g| g.info.sequence).unwrap();

				tracing::debug!(old = self.current, new = new_current, "skipping slow groups");

				self.current = new_current;
				continue;
			}

			if finished && self.pending.is_empty() {
				return Poll::Ready(Ok(None));
			}

			return Poll::Pending;
		}
	}

	// Reads any new groups from the track until we're completely finished.
	//
	// Returns Pending until all groups have been consumed.
	fn poll_read_finish(&mut self, waiter: &conducer::Waiter) -> Poll<Result<(), Error>> {
		loop {
			let Some(group) = ready!(self.track.poll_next_group(waiter)?) else {
				// Track is finished.
				return Poll::Ready(Ok(()));
			};

			let reader = GroupBuffer::new(group);
			if reader.group.info.sequence < self.current {
				tracing::debug!(
					old = ?reader.group.info.sequence,
					current = ?self.current,
					"skipping old group"
				);
				continue;
			}

			let idx = self
				.pending
				.partition_point(|g| g.group.info.sequence < reader.group.info.sequence);
			self.pending.insert(idx, reader);
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
///
/// Handles two-phase frame reading (get FrameConsumer, then read all data),
/// timestamp parsing, and min/max timestamp tracking for latency decisions.
struct GroupBuffer {
	group: moq_lite::GroupConsumer,

	// The current frame index within the group.
	index: usize,

	// Read frames that haven't been consumed yet.
	buffered: VecDeque<OrderedFrame>,

	// The minimum timestamp in the group.
	min_timestamp: Option<Timestamp>,

	// The maximum timestamp in the group.
	max_timestamp: Option<Timestamp>,
}

impl GroupBuffer {
	fn new(group: moq_lite::GroupConsumer) -> Self {
		Self {
			group,
			index: 0,
			buffered: VecDeque::new(),
			max_timestamp: None,
			min_timestamp: None,
		}
	}

	/// Poll for the next frame from this group.
	pub fn poll_read(&mut self, waiter: &conducer::Waiter) -> Poll<Result<Option<OrderedFrame>, Error>> {
		if let Some(frame) = self.buffered.pop_front() {
			return Poll::Ready(Ok(Some(frame)));
		}

		match ready!(self.buffer_one(waiter)?) {
			true => Poll::Ready(Ok(Some(self.buffered.pop_front().unwrap()))),
			false => Poll::Ready(Ok(None)),
		}
	}

	// Add one more frame to the buffer if possible.
	//
	// Returns false if the track is finished.
	fn buffer_once(&mut self, waiter: &conducer::Waiter) -> Poll<Result<bool, Error>> {
		let Some(chunks) = ready!(self.group.poll_read_frame_chunks(waiter)?) else {
			return Poll::Ready(Ok(false));
		};

		let mut payload = BufList::from_iter(chunks);
		let timestamp = Timestamp::decode(&mut payload)?;

		self.min_timestamp = Some(match self.min_timestamp {
			Some(existing) => existing.min(timestamp),
			None => timestamp,
		});

		self.max_timestamp = Some(match self.max_timestamp {
			Some(existing) => existing.max(timestamp),
			None => timestamp,
		});

		let index = self.index;
		self.index += 1;

		self.buffered.push_back(OrderedFrame {
			timestamp,
			payload,
			group: self.group.info.sequence,
			index,
		});

		Poll::Ready(Ok(true))
	}

	fn buffer_one(&mut self, waiter: &conducer::Waiter) -> Poll<Result<bool, Error>> {
		if self.buffered.is_empty() {
			self.buffer_once(waiter)
		} else {
			Poll::Ready(Ok(true))
		}
	}

	fn buffer_all(&mut self, waiter: &conducer::Waiter) -> Poll<Result<(), Error>> {
		while ready!(self.buffer_once(waiter)?) {}
		Poll::Ready(Ok(()))
	}

	/// Poll for the maximum timestamp in this group.
	pub fn poll_max_timestamp(&mut self, waiter: &conducer::Waiter) -> Poll<Result<Timestamp, Error>> {
		// Keep reading more frames just to advance the max timestamp.
		let _ = self.buffer_all(waiter)?;

		if let Some(max) = self.max_timestamp {
			return Poll::Ready(Ok(max));
		}

		if let Poll::Ready(_frames) = self.group.poll_finished(waiter)? {
			return Poll::Ready(Err(Error::EmptyGroup));
		}

		Poll::Pending
	}

	pub fn poll_min_timestamp(&mut self, waiter: &conducer::Waiter) -> Poll<Result<Timestamp, Error>> {
		let _ = self.buffer_one(waiter)?;

		if let Some(min) = self.min_timestamp {
			return Poll::Ready(Ok(min));
		}

		if let Poll::Ready(_frames) = self.group.poll_finished(waiter)? {
			return Poll::Ready(Err(Error::EmptyGroup));
		}

		Poll::Pending
	}
}

impl std::ops::Deref for GroupBuffer {
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
	fn write_group(track: &mut moq_lite::TrackProducer, sequence: u64, timestamps: &[Timestamp]) {
		let mut group = track.create_group(moq_lite::Group { sequence }).unwrap();
		for &timestamp in timestamps {
			let frame = Frame {
				timestamp,
				payload: BufList::from_iter(vec![Bytes::from_static(&[0xDE, 0xAD])]),
			};
			frame.encode(&mut group).unwrap();
		}
		group.finish().unwrap();
	}

	/// Drain all available frames with a per-read timeout.
	async fn read_all(consumer: &mut OrderedConsumer) -> Result<Vec<OrderedFrame>, crate::Error> {
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
		assert_eq!(frames[0].index, 0);

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

		assert_eq!(frames[0].index, 0);
		assert_eq!(frames[1].index, 1);
		assert_eq!(frames[2].index, 2);
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
	// These tests verify that the poll-based latency skip logic correctly
	// promotes pending groups to current when the timestamp span exceeds
	// max_latency.

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
		// The latency skip bridges past the blocking group 0 to the nearest
		// pending group with data. All subsequent finished groups are delivered
		// instantly. Group 0's 1 frame + groups 1-9 (3 frames each) = 28.
		assert_eq!(frames.len(), 28, "Expected group 0 frame + groups 1-9");
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

		// The latency skip bridges past the blocking group 0 to the nearest
		// pending group with data. All subsequent groups are delivered since
		// they can be read instantly (already finished). Group 0's frame (ts=0)
		// is returned before the skip, then groups 1-9 after.
		assert_eq!(frames.len(), 10, "Expected group 0 frame + groups 1-9");
		assert_eq!(frames[0].timestamp, ts(0));

		// Groups should be delivered in sequence order
		for i in 1..10u64 {
			assert_eq!(frames[i as usize].timestamp, ts(i * 30_000));
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
	async fn frame_timestamp_and_index_decoding() {
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(500));

		write_group(&mut track, 0, &[ts(0), ts(33_333), ts(66_666)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 3);

		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[0].index, 0);

		assert_eq!(frames[1].timestamp, ts(33_333));
		assert_eq!(frames[1].index, 1);

		assert_eq!(frames[2].timestamp, ts(66_666));
		assert_eq!(frames[2].index, 2);
	}

	#[tokio::test]
	async fn frame_payload_preserved() {
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(500));

		let payload_bytes = vec![0x01, 0x02, 0x03, 0x04, 0x05];
		let mut group = track.create_group(moq_lite::Group { sequence: 0 }).unwrap();
		Frame {
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

	/// Regression test for de92d2c7: the old select!-based implementation had an
	/// infinite loop when a pending group had buffered frames from a prior
	/// (dropped) buffer_until call.
	///
	/// The poll-based rewrite avoids this by design: frames are only read on-demand,
	/// and buffered frames are consumed before polling for new ones.
	#[tokio::test]
	async fn no_infinite_loop_with_buffered_frames() {
		tokio::time::pause();
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_secs(10));

		// Group 0: 1 frame, NOT finished
		let mut group0 = track.create_group(moq_lite::Group { sequence: 0 }).unwrap();
		Frame {
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

	// ---- Startup Behavior ----

	#[tokio::test]
	async fn startup_selects_earliest_group() {
		tokio::time::pause();
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		// max_latency = 100ms.
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(100));

		// Groups 3, 5, 7 — non-sequential with gaps.
		// After startup selects group 3 (earliest with data), consumer reads it,
		// then blocks on gap (waiting for group 4 which never arrives).
		write_group(&mut track, 3, &[ts(0)]);
		write_group(&mut track, 5, &[ts(150_000)]);

		// Group 7: write one frame now, push a second later to trigger the latency skip.
		let mut group7 = track.create_group(moq_lite::Group { sequence: 7 }).unwrap();
		Frame {
			timestamp: ts(300_000),
			payload: BufList::from_iter(vec![Bytes::from_static(&[0xDE, 0xAD])]),
		}
		.encode(&mut group7)
		.unwrap();

		let finisher = tokio::spawn(async move {
			// Wait for the consumer to process groups 3 and 5, then push
			// a second frame on group 7 with a high enough timestamp to
			// trigger the latency skip past the gap at group 6.
			tokio::time::sleep(Duration::from_millis(50)).await;
			Frame {
				timestamp: ts(400_000),
				payload: BufList::from_iter(vec![Bytes::from_static(&[0xBE, 0xEF])]),
			}
			.encode(&mut group7)
			.unwrap();
			group7.finish().unwrap();
			track.finish().unwrap();
		});

		let frames = tokio::time::timeout(Duration::from_secs(2), async {
			let mut frames = Vec::new();
			while let Some(frame) = consumer.read().await.unwrap() {
				frames.push(frame);
			}
			frames
		})
		.await
		.expect("should not hang");

		// Startup picks group 3 (earliest with data), reads it.
		// Blocks on gap at 4. Latency skip: min(5)=150ms, max(7)=400ms → skip to 5.
		// Reads group 5, blocks on gap at 6. Another skip to group 7.
		assert_eq!(frames[0].group, 3);
		assert_eq!(frames[1].group, 5);
		assert!(frames.iter().skip(2).all(|f| f.group == 7));
		finisher.await.unwrap();
	}

	#[tokio::test]
	async fn startup_skips_groups_without_data() {
		tokio::time::pause();
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(500));

		// Group 5: no frames written yet (pending)
		let _group5 = track.create_group(moq_lite::Group { sequence: 5 }).unwrap();
		// Group 7: has data
		write_group(&mut track, 7, &[ts(210_000)]);
		track.finish().unwrap();

		let frames = tokio::time::timeout(Duration::from_millis(500), async {
			let mut frames = Vec::new();
			while let Some(frame) = consumer.read().await.unwrap() {
				frames.push(frame);
			}
			frames
		})
		.await
		.expect("should not hang");

		assert!(!frames.is_empty());
		// Group 7 should be selected since group 5 has no data.
		assert_eq!(frames[0].group, 7);
	}

	#[tokio::test]
	async fn startup_single_group_mid_stream() {
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(500));

		// Only group 100 exists.
		write_group(&mut track, 100, &[ts(3_000_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1);
		assert_eq!(frames[0].group, 100);
	}

	#[tokio::test]
	async fn multiple_sequential_latency_skips() {
		tokio::time::pause();
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(50));

		// Group 0: blocks
		let mut group0 = track.create_group(moq_lite::Group { sequence: 0 }).unwrap();
		Frame {
			timestamp: ts(0),
			payload: BufList::from_iter(vec![Bytes::from_static(&[0xAA])]),
		}
		.encode(&mut group0)
		.unwrap();

		// Groups 1-3: each 100ms apart, triggering skips (> 50ms latency)
		write_group(&mut track, 1, &[ts(100_000)]);
		write_group(&mut track, 2, &[ts(200_000)]);
		write_group(&mut track, 3, &[ts(300_000)]);
		track.finish().unwrap();

		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(20)).await;
			group0.finish().unwrap();
		});

		let frames = read_all(&mut consumer).await.unwrap();
		assert!(!frames.is_empty());
		finisher.await.unwrap();
	}

	#[tokio::test]
	async fn latency_skip_boundary_exact() {
		tokio::time::pause();
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(100));

		// Group 0: blocks
		let mut group0 = track.create_group(moq_lite::Group { sequence: 0 }).unwrap();
		Frame {
			timestamp: ts(0),
			payload: BufList::from_iter(vec![Bytes::from_static(&[0xAA])]),
		}
		.encode(&mut group0)
		.unwrap();

		// Group 1: exactly 100ms span (>= max_latency should trigger skip)
		write_group(&mut track, 1, &[ts(100_000)]);
		track.finish().unwrap();

		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(20)).await;
			group0.finish().unwrap();
		});

		let frames = read_all(&mut consumer).await.unwrap();
		assert!(!frames.is_empty());
		finisher.await.unwrap();
	}

	#[tokio::test]
	async fn group_error_skips_to_next() {
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(500));

		// Group 0: aborted
		let mut group0 = track.create_group(moq_lite::Group { sequence: 0 }).unwrap();
		group0.abort(moq_lite::Error::Cancel).unwrap();

		// Group 1: valid
		write_group(&mut track, 1, &[ts(30_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1);
		assert_eq!(frames[0].group, 1);
	}

	#[tokio::test]
	async fn track_finishes_while_reading() {
		tokio::time::pause();
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(500));

		write_group(&mut track, 0, &[ts(0)]);

		// Finish the track after a delay, simulating incremental arrival.
		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(20)).await;
			write_group(&mut track, 1, &[ts(30_000)]);
			tokio::time::sleep(Duration::from_millis(20)).await;
			track.finish().unwrap();
		});

		let frames = tokio::time::timeout(Duration::from_secs(2), async {
			let mut frames = Vec::new();
			while let Some(frame) = consumer.read().await.unwrap() {
				frames.push(frame);
			}
			frames
		})
		.await
		.expect("consumer should not hang");

		assert_eq!(frames.len(), 2);
		finisher.await.unwrap();
	}

	#[tokio::test]
	async fn empty_group_advances() {
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(500));

		// Group 0: empty (no frames, just finished)
		let mut group0 = track.create_group(moq_lite::Group { sequence: 0 }).unwrap();
		group0.finish().unwrap();

		// Group 1: has data
		write_group(&mut track, 1, &[ts(30_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1);
		assert_eq!(frames[0].group, 1);
	}
}
