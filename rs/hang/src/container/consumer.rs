use std::collections::VecDeque;
use std::task::Poll;

use buf_list::BufList;

use super::{Frame, Timestamp};
use crate::Error;

/// A consumer for hang-formatted media tracks with timestamp reordering.
///
/// This wraps a `moq_lite::TrackConsumer` and adds hang-specific functionality
/// like timestamp decoding, latency management, and frame buffering.
///
/// ## Latency Management
///
/// The consumer monitors the timestamp span of pending groups. When the span
/// (max - min across all pending groups) exceeds `max_latency`, the consumer
/// skips forward to the oldest pending group.
pub struct OrderedConsumer {
	pub track: moq_lite::TrackConsumer,

	// The current group being read from.
	active: Option<GroupReader>,

	// Future groups with background timestamp parsers.
	pending: VecDeque<PendingGroup>,

	// The next expected group sequence number, or None before the first group.
	desired: Option<u64>,

	// The maximum buffer size before skipping a group.
	max_latency: std::time::Duration,
}

struct PendingGroup {
	reader: GroupReader,
	// Consumer side of timestamp range — poll() registers waiter, gets woken on change.
	timestamps: moq_lite::state::Consumer<TimestampRange>,
}

#[derive(Default, Clone, Debug)]
struct TimestampRange {
	min: Option<Timestamp>,
	max: Option<Timestamp>,
}

struct GroupReader {
	group: moq_lite::GroupConsumer,
	index: usize,
	buffered: VecDeque<Frame>,
	done: bool,
}

impl OrderedConsumer {
	/// Create a new OrderedConsumer wrapping the given moq-lite consumer.
	pub fn new(track: moq_lite::TrackConsumer, max_latency: std::time::Duration) -> Self {
		Self {
			track,
			active: None,
			pending: VecDeque::new(),
			desired: None,
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
		loop {
			// STEP 1: Drain buffered frames from active (sync)
			if let Some(active) = &mut self.active
				&& let Some(frame) = active.buffered.pop_front()
			{
				return Ok(Some(frame));
			}

			// STEP 2: Advance if active is fully consumed
			if self.active.as_ref().is_some_and(|a| a.done) {
				self.advance_active();
				continue;
			}

			let threshold: Timestamp = self.max_latency.try_into()?;

			// STEP 3: select! — each arm borrows different fields
			tokio::select! {
				biased;

				// (a) Read from active group
				Some(res) = async {
					Some(self.active.as_mut()?.read_unbuffered().await)
				} => {
					match res {
						Ok(Some(frame)) => return Ok(Some(frame)),
						_ => { self.advance_active(); continue; }
					}
				},

				// (b) Accept new group from track
				Some(res) = async {
					self.track.next_group().await.transpose()
				} => {
					self.insert_group(res?);
					continue;
				},

				// (c) Wait for pending span to exceed threshold.
				// Returns None when pending is empty so the branch disables
				// and the else arm can fire.
				Some(()) = async {
					if self.pending.is_empty() {
						return None;
					}
					moq_lite::waiter::waiter_fn(|waiter| {
						poll_pending_latency(&self.pending, waiter, threshold)
					}).await;
					Some(())
				} => {
					self.skip_to_oldest_pending();
					continue;
				},

				else => return Ok(None),
			}
		}
	}

	fn advance_active(&mut self) {
		self.active = None;
		if let Some(desired) = &mut self.desired {
			*desired += 1;
		}
		self.try_promote();
	}

	fn try_promote(&mut self) {
		let Some(desired) = self.desired else { return };
		loop {
			let Some(front) = self.pending.front() else { return };
			let seq = front.reader.group.info.sequence;
			if seq == desired {
				let pg = self.pending.pop_front().unwrap();
				self.active = Some(pg.reader);
				// pg.timestamps (Consumer) dropped → parser stops cooperatively
				return;
			} else if seq < desired {
				self.pending.pop_front();
				// discard old, continue loop
			} else {
				return; // seq > desired, wait
			}
		}
	}

	fn insert_group(&mut self, group: moq_lite::GroupConsumer) {
		let seq = group.info.sequence;

		match self.desired {
			None => {
				// First ever group — set desired and make active directly
				self.desired = Some(seq);
				self.active = Some(GroupReader::new(group));
				return;
			}
			Some(desired) if seq < desired => return,
			Some(desired) if seq == desired => {
				if self.active.is_none() {
					self.active = Some(GroupReader::new(group));
				}
				return;
			}
			Some(_) => {} // seq > desired, fall through to add to pending
		}

		// Clone the consumer for the background timestamp parser
		let producer = moq_lite::state::Producer::new(TimestampRange::default());
		let consumer = producer.consume();
		spawn_timestamp_parser(group.clone(), producer);

		let pg = PendingGroup {
			reader: GroupReader::new(group),
			timestamps: consumer,
		};

		let index = self.pending.partition_point(|p| p.reader.group.info.sequence < seq);
		self.pending.insert(index, pg);
	}

	fn skip_to_oldest_pending(&mut self) {
		self.active = None;
		if let Some(front) = self.pending.front() {
			self.desired = Some(front.reader.group.info.sequence);
		}
		self.try_promote();
	}

	/// Set the maximum latency tolerance for this consumer.
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

/// Check if the timestamp span of pending groups exceeds the threshold.
///
/// Registers the waiter on ALL pending group timestamp consumers so we get
/// woken when ANY parser updates its timestamps.
fn poll_pending_latency(
	pending: &VecDeque<PendingGroup>,
	waiter: &moq_lite::waiter::Waiter,
	threshold: Timestamp,
) -> Poll<()> {
	if pending.is_empty() {
		return Poll::Pending; // Nothing to check, just wait
	}

	let mut min_ts: Option<Timestamp> = None;
	let mut max_ts: Option<Timestamp> = None;

	for pg in pending {
		// poll() locks state, runs callback (reads timestamps), registers waiter if Pending.
		// If poll returns Ready(Err(Dropped)): parser finished, timestamps are final.
		// We already read them in the callback, so just continue.
		let _ = pg.timestamps.poll(waiter, |ts_ref| {
			let ts: &TimestampRange = ts_ref;
			if let Some(m) = ts.min {
				min_ts = Some(min_ts.map_or(m, |v| v.min(m)));
			}
			if let Some(m) = ts.max {
				max_ts = Some(max_ts.map_or(m, |v| v.max(m)));
			}
			Poll::<()>::Pending // Always register waiter
		});
	}

	// Check span
	if let (Some(min), Some(max)) = (min_ts, max_ts)
		&& max.as_micros().saturating_sub(min.as_micros()) >= threshold.as_micros()
	{
		return Poll::Ready(());
	}

	Poll::Pending
}

/// Background task that reads frames from a cloned GroupConsumer to discover timestamps.
fn spawn_timestamp_parser(group: moq_lite::GroupConsumer, producer: moq_lite::state::Producer<TimestampRange>) {
	web_async::spawn(async move {
		let mut group = group;
		loop {
			let Ok(Some(mut frame)) = group.next_frame().await else {
				break;
			};
			let Ok(payload) = frame.read_chunks().await else {
				break;
			};
			let mut payload = BufList::from_iter(payload);
			let Ok(timestamp) = Timestamp::decode(&mut payload) else {
				break;
			};

			// Update min/max — only uses DerefMut (and thus wakes consumers) if changed
			let Ok(mut state) = producer.modify() else { break };
			let needs_update = state.min.is_none_or(|m| timestamp < m) || state.max.is_none_or(|m| timestamp > m);
			if needs_update {
				let range = &mut *state; // DerefMut → sets modified=true → wakes on drop
				range.min = Some(range.min.map_or(timestamp, |m| m.min(timestamp)));
				range.max = Some(range.max.map_or(timestamp, |m| m.max(timestamp)));
			}
		}
		// Producer dropped here → state closes → consumers get woken with Err(Dropped)
	});
}

impl GroupReader {
	fn new(group: moq_lite::GroupConsumer) -> Self {
		Self {
			group,
			index: 0,
			buffered: VecDeque::new(),
			done: false,
		}
	}

	async fn read_unbuffered(&mut self) -> Result<Option<Frame>, Error> {
		let Some(mut frame) = self.group.next_frame().await? else {
			self.done = true;
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

		Ok(Some(frame))
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

	#[tokio::test(start_paused = true)]
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

	#[tokio::test(start_paused = true)]
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

	#[tokio::test(start_paused = true)]
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

	#[tokio::test(start_paused = true)]
	async fn latency_skip_delivers_recent_groups() {
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
		// Should have group 0's frames + at least some later groups
		assert!(frames.len() >= 10, "Expected >= 10 frames, got {}", frames.len());
		finisher.await.expect("finisher task panicked");
	}

	#[tokio::test(start_paused = true)]
	async fn zero_latency_skips_blocked_active() {
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::ZERO);

		// Group 0: 1 frame, NOT finished (blocks consumer after reading frame 0)
		let mut group0 = track.create_group(moq_lite::Group { sequence: 0 }).unwrap();
		Frame {
			keyframe: false,
			timestamp: ts(400_000),
			payload: BufList::from_iter(vec![Bytes::from_static(&[0xDE, 0xAD])]),
		}
		.encode(&mut group0)
		.unwrap();

		// Groups 1-9: finished, 50ms spacing, 3 frames each
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
		assert!(!frames.is_empty(), "Expected at least some frames");

		// With 0ms latency, the blocked group 0 is dropped as soon as pending
		// groups have any span > 0. Group 0's single frame is delivered first,
		// then the consumer skips to the oldest pending group.
		assert_eq!(frames[0].timestamp, ts(400_000), "First frame should be from group 0");
		// Second frame should be from group 1 (oldest pending), not group 0 continuation
		assert_eq!(
			frames[1].timestamp,
			ts(50_000),
			"Should skip to group 1 after group 0 blocks"
		);
		finisher.await.expect("finisher task panicked");
	}

	#[tokio::test(start_paused = true)]
	async fn latency_skip_drops_blocked_active() {
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(100));

		// Group 0: 1 frame, NOT finished (blocks consumer)
		let mut group0 = track.create_group(moq_lite::Group { sequence: 0 }).unwrap();
		Frame {
			keyframe: false,
			timestamp: ts(0),
			payload: BufList::from_iter(vec![Bytes::from_static(&[0xDE, 0xAD])]),
		}
		.encode(&mut group0)
		.unwrap();

		// Groups 1-9: 30ms spacing, 1 frame each
		// Pending span: max=270000 - min=30000 = 240000µs = 240ms > 100ms
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

		// Group 0's frame is delivered, then skip fires (pending span > 100ms).
		// Consumer drops blocked group 0, promotes oldest pending (group 1).
		assert_eq!(frames[0].timestamp, ts(0), "First frame from group 0");
		assert_eq!(frames[1].timestamp, ts(30_000), "Skip to oldest pending (group 1)");

		// All groups 1-9 delivered after the skip (finished groups consumed instantly)
		assert_eq!(frames.len(), 10, "Group 0 frame + groups 1-9");
		finisher.await.expect("finisher task panicked");
	}

	// ---- Group Ordering ----

	#[tokio::test(start_paused = true)]
	async fn groups_delivered_in_sequence_order() {
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

	#[tokio::test(start_paused = true)]
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

	#[tokio::test(start_paused = true)]
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

	#[tokio::test(start_paused = true)]
	async fn empty_track_returns_none() {
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

	#[tokio::test(start_paused = true)]
	async fn track_closed_with_error() {
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(500));

		write_group(&mut track, 0, &[ts(0)]);
		track.close(moq_lite::Error::Cancel).unwrap();

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

	#[tokio::test(start_paused = true)]
	async fn closed_resolves_when_track_ends() {
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

	#[tokio::test(start_paused = true)]
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

	#[tokio::test(start_paused = true)]
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

	#[tokio::test(start_paused = true)]
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

	#[tokio::test(start_paused = true)]
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

	/// Regression test: group 0 unfinished, group 1 finished, delayed group 2,
	/// then group 0 finishes. Should complete without hanging.
	#[tokio::test(start_paused = true)]
	async fn no_infinite_loop_with_buffered_frames() {
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

		// Group 1: finished
		write_group(&mut track, 1, &[ts(100_000)]);

		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(20)).await;
			write_group(&mut track, 2, &[ts(200_000)]);
			tokio::time::sleep(Duration::from_millis(20)).await;
			group0.finish().unwrap();
			track.finish().unwrap();
		});

		// Must complete within 2 seconds
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

	#[tokio::test(start_paused = true)]
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

	#[tokio::test(start_paused = true)]
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

	/// With span-based latency, B-frames within a single group don't cause
	/// spurious skips since we only check span across PENDING groups.
	#[tokio::test(start_paused = true)]
	async fn max_timestamp_tracks_through_bframes() {
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
		// With span-based latency, the span of one pending group is just its own range.
		// Group 1 alone: min=100ms, max=100ms, span=0 < 40ms threshold. No skip.
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

	// ---- New span-based tests ----

	/// Missing group is skipped when pending span exceeds max_latency.
	#[tokio::test(start_paused = true)]
	async fn missing_group_skipped_on_span() {
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		// max_latency = 1.5s
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(1500));

		// Group 0 finishes normally
		write_group(&mut track, 0, &[ts(0)]);

		// Group 1 never arrives (gap). Groups 2 and 3 arrive.
		// Group 2: ts=1s, Group 3: ts=3s → span = 2s > 1.5s → skip to group 2
		write_group(&mut track, 2, &[ts(1_000_000)]);
		write_group(&mut track, 3, &[ts(3_000_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		// Group 0 (1 frame) + skip to group 2 (1 frame) + group 3 (1 frame) = 3 frames
		assert_eq!(frames.len(), 3, "Expected 3 frames, got {}", frames.len());
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(1_000_000));
		assert_eq!(frames[2].timestamp, ts(3_000_000));
	}

	/// Missing group waits when pending span is small enough.
	#[tokio::test(start_paused = true)]
	async fn missing_group_waits_when_span_small() {
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(200));

		// Group 0 finishes normally
		write_group(&mut track, 0, &[ts(0)]);

		// Group 1 missing initially. Groups 2 and 3 have small span.
		// Group 2: ts=100ms, Group 3: ts=150ms → span = 50ms < 200ms → no skip
		write_group(&mut track, 2, &[ts(100_000)]);
		write_group(&mut track, 3, &[ts(150_000)]);

		// Group 1 arrives late
		let writer = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(10)).await;
			write_group(&mut track, 1, &[ts(50_000)]);
			track.finish().unwrap();
		});

		let frames = read_all(&mut consumer).await.unwrap();
		// All 4 groups delivered in order: 0, 1, 2, 3
		assert_eq!(frames.len(), 4, "Expected 4 frames, got {}", frames.len());
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(50_000));
		assert_eq!(frames[2].timestamp, ts(100_000));
		assert_eq!(frames[3].timestamp, ts(150_000));
		writer.await.expect("writer task panicked");
	}

	/// Skip targets oldest pending, not next sequential.
	#[tokio::test(start_paused = true)]
	async fn skip_to_oldest_pending() {
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(100));

		// Group 0 finishes. Desired becomes 1, which never arrives.
		write_group(&mut track, 0, &[ts(0)]);

		// Groups 3, 4, 5 arrive (not 1 or 2).
		// Span of pending: min=300ms, max=500ms → span=200ms > 100ms → skip to group 3
		write_group(&mut track, 3, &[ts(300_000)]);
		write_group(&mut track, 4, &[ts(400_000)]);
		write_group(&mut track, 5, &[ts(500_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		// Group 0 + skip to group 3 + groups 4, 5
		assert_eq!(frames.len(), 4, "Expected 4 frames, got {}", frames.len());
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(300_000)); // Skipped to group 3, not 1 or 2
		assert_eq!(frames[2].timestamp, ts(400_000));
		assert_eq!(frames[3].timestamp, ts(500_000));
	}

	/// Same scenario run multiple times produces identical output.
	#[tokio::test(start_paused = true)]
	async fn deterministic_output() {
		async fn run_scenario() -> Vec<Timestamp> {
			let mut track = moq_lite::Track::new("test").produce();
			let consumer_track = track.consume();
			let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(100));

			write_group(&mut track, 0, &[ts(0)]);
			// Gap at 1
			write_group(&mut track, 2, &[ts(200_000)]);
			write_group(&mut track, 3, &[ts(300_000)]);
			write_group(&mut track, 4, &[ts(400_000)]);
			track.finish().unwrap();

			let frames = read_all(&mut consumer).await.unwrap();
			frames.into_iter().map(|f| f.timestamp).collect()
		}

		let first = run_scenario().await;
		for _ in 0..4 {
			let result = run_scenario().await;
			assert_eq!(first, result, "Non-deterministic output detected");
		}
	}

	/// Slow active group is dropped when pending span exceeds threshold.
	#[tokio::test(start_paused = true)]
	async fn slow_active_skipped_on_span() {
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = OrderedConsumer::new(consumer_track, Duration::from_millis(100));

		// Group 0: active but unfinished (slow)
		let mut group0 = track.create_group(moq_lite::Group { sequence: 0 }).unwrap();
		Frame {
			keyframe: false,
			timestamp: ts(0),
			payload: BufList::from_iter(vec![Bytes::from_static(&[0xDE, 0xAD])]),
		}
		.encode(&mut group0)
		.unwrap();

		// Groups 1-3 arrive spanning > max_latency
		// min=100ms, max=300ms, span=200ms > 100ms
		write_group(&mut track, 1, &[ts(100_000)]);
		write_group(&mut track, 2, &[ts(200_000)]);
		write_group(&mut track, 3, &[ts(300_000)]);
		track.finish().unwrap();

		let finisher = tokio::spawn(async move {
			// Don't finish group 0 — it should get dropped by the skip
			tokio::time::sleep(Duration::from_secs(5)).await;
			group0.finish().unwrap();
		});

		let frames = tokio::time::timeout(Duration::from_secs(2), async {
			let mut frames = Vec::new();
			while let Ok(Some(frame)) = consumer.read().await {
				frames.push(frame);
			}
			frames
		})
		.await
		.expect("consumer hung waiting for slow active group");

		// Group 0 should have been dropped. Group 1 becomes active.
		assert!(!frames.is_empty());
		// First frame from group 0 was read before skip, then groups 1-3
		let has_group1 = frames.iter().any(|f| f.timestamp == ts(100_000));
		assert!(has_group1, "Expected group 1 to be delivered after skip");
		finisher.abort();
	}
}
