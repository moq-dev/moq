use std::collections::VecDeque;
use std::task::{Poll, ready};

use crate::container::{Container, Frame, Timestamp};

/// A consumer for media tracks with timestamp reordering.
///
/// This wraps a `moq_lite::TrackConsumer` and adds functionality
/// like timestamp decoding, latency management, and frame buffering.
///
/// Generic over `F: Container` to support different container encodings.
///
/// ## Latency Management
///
/// The consumer can skip groups that are too far behind to maintain low latency.
/// Configure the maximum acceptable delay through the consumer's latency settings.
pub struct Consumer<F: Container> {
	track: moq_lite::TrackConsumer,

	format: F,

	// The current group that we want to read from
	current: u64,

	// Groups that we are monitoring, sorted by sequence ascending.
	pending: VecDeque<GroupBuffer>,

	// When true, we haven't returned a frame yet and need to select the first group.
	// We wait until we have at least one frame before finalizing `current`
	startup: bool,

	// The maximum buffer size before skipping a group.
	latency: std::time::Duration,
}

impl<F: Container> Consumer<F> {
	/// Create a new Consumer wrapping the given moq-lite consumer.
	pub fn new(track: moq_lite::TrackConsumer, format: F) -> Self {
		Self {
			track,
			format,
			current: 0,
			pending: VecDeque::new(),
			startup: true,
			latency: std::time::Duration::ZERO,
		}
	}

	/// Set the maximum latency tolerance.
	///
	/// Groups with timestamps older than the newest timestamp minus this value will be skipped.
	/// A value of zero (the default) skips aggressively — any group with a newer alternative is dropped.
	pub fn with_latency(mut self, latency: std::time::Duration) -> Self {
		self.latency = latency;
		self
	}

	/// Read the next frame from the track.
	///
	/// This method handles timestamp decoding, group ordering, and latency management
	/// automatically. It will skip groups that are too far behind to maintain the
	/// configured latency target.
	///
	/// Returns `None` when the track has ended.
	pub async fn read(&mut self) -> Result<Option<Frame>, F::Error> {
		conducer::wait(|waiter| self.poll_read(waiter)).await
	}

	/// Poll-based implementation of the read loop.
	///
	/// Uses a single waiter that gets registered on all relevant conducer channels,
	/// avoiding the need for `tokio::select!` or `FuturesUnordered`.
	pub fn poll_read(&mut self, waiter: &conducer::Waiter) -> Poll<Result<Option<Frame>, F::Error>> {
		// Grab any new groups from the track, recording whether the track is finished.
		let finished = self.poll_read_finish(waiter)?.is_ready();

		// On startup, we want to poll every pending group and advance self.current to the first with a frame.
		if self.startup {
			// NOTE: We loop in ascending order, so earlier groups will win the race.
			for (i, group) in self.pending.iter_mut().enumerate() {
				// We call poll_min_timestamp to try to buffer at least one frame per group.
				// This returns Ready(Ok) if there is a buffered frame.
				if !matches!(group.poll_min_timestamp(waiter, &self.format), Poll::Ready(Ok(_))) {
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
				match group.poll_read(waiter, &self.format) {
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

			// Get the current group's min timestamp as the reference for latency comparison.
			let oldest_timestamp = if let Some(current) = self.pending.front_mut()
				&& current.info.sequence <= self.current
			{
				match current.poll_min_timestamp(waiter, &self.format) {
					Poll::Ready(Ok(ts)) => Some::<std::time::Duration>(ts.into()),
					_ => None,
				}
			} else {
				None
			};

			// Find the first newer group with data (our skip target).
			let mut min_idx = None;
			for (i, group) in self.pending.iter_mut().enumerate() {
				if group.info.sequence <= self.current {
					continue;
				}

				if let Poll::Ready(Ok(_)) = group.poll_min_timestamp(waiter, &self.format) {
					min_idx = Some(i);
					break;
				}
			}

			// Find the max timestamp across all newer groups.
			let mut max_timestamp = std::time::Duration::ZERO;
			for group in self.pending.iter_mut().rev() {
				if group.info.sequence <= self.current {
					break;
				}

				if let Poll::Ready(Ok(ts)) = group.poll_max_timestamp(waiter, &self.format) {
					max_timestamp = max_timestamp.max(ts.into());
					break; // We know older groups won't be newer than this.
				}
			}

			let should_skip = if min_idx.is_some() {
				if let Some(oldest) = oldest_timestamp {
					// Current group is blocking: skip if newer groups exceed latency threshold
					max_timestamp.saturating_sub(oldest) >= self.latency
				} else {
					// Sequence gap: current group consumed but next sequence missing.
					// Only skip if track is fully received (no more groups coming).
					finished
				}
			} else {
				false
			};

			if let Some(new_idx) = min_idx
				&& should_skip
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
	fn poll_read_finish(&mut self, waiter: &conducer::Waiter) -> Poll<Result<(), F::Error>> {
		loop {
			let Some(group) = ready!(self.track.poll_recv_group(waiter)?) else {
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

	/// Set the maximum latency tolerance.
	pub fn set_latency(&mut self, latency: std::time::Duration) {
		self.latency = latency;
	}

	/// Wait until the track is closed.
	pub async fn closed(&self) -> Result<(), F::Error> {
		Ok(self.track.closed().await?)
	}

	/// Unwrap into the inner TrackConsumer.
	pub fn into_inner(self) -> moq_lite::TrackConsumer {
		self.track
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
	buffered: VecDeque<Frame>,

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
	fn poll_read<F: Container>(
		&mut self,
		waiter: &conducer::Waiter,
		format: &F,
	) -> Poll<Result<Option<Frame>, F::Error>> {
		if let Some(frame) = self.buffered.pop_front() {
			return Poll::Ready(Ok(Some(frame)));
		}

		match ready!(self.buffer_one(waiter, format)?) {
			true => Poll::Ready(Ok(Some(self.buffered.pop_front().unwrap()))),
			false => Poll::Ready(Ok(None)),
		}
	}

	// Add one more frame to the buffer if possible.
	//
	// Returns false if the group is finished.
	fn buffer_once<F: Container>(&mut self, waiter: &conducer::Waiter, format: &F) -> Poll<Result<bool, F::Error>> {
		let Some(frames) = ready!(format.poll_read(&mut self.group, waiter)?) else {
			return Poll::Ready(Ok(false));
		};

		for frame in frames {
			self.min_timestamp = Some(match self.min_timestamp {
				Some(existing) => existing.min(frame.timestamp),
				None => frame.timestamp,
			});

			self.max_timestamp = Some(match self.max_timestamp {
				Some(existing) => existing.max(frame.timestamp),
				None => frame.timestamp,
			});

			let keyframe = self.index == 0;
			self.index += 1;

			self.buffered.push_back(Frame {
				timestamp: frame.timestamp,
				payload: frame.payload,
				keyframe,
			});
		}

		Poll::Ready(Ok(true))
	}

	fn buffer_one<F: Container>(&mut self, waiter: &conducer::Waiter, format: &F) -> Poll<Result<bool, F::Error>> {
		loop {
			if !self.buffered.is_empty() {
				return Poll::Ready(Ok(true));
			}
			if !ready!(self.buffer_once(waiter, format)?) {
				return Poll::Ready(Ok(false));
			}
			// poll_read returned Some(vec![]) — loop and try again
		}
	}

	fn buffer_all<F: Container>(&mut self, waiter: &conducer::Waiter, format: &F) -> Poll<Result<(), F::Error>> {
		while ready!(self.buffer_once(waiter, format)?) {}
		Poll::Ready(Ok(()))
	}

	/// Poll for the maximum timestamp in this group.
	fn poll_max_timestamp<F: Container>(
		&mut self,
		waiter: &conducer::Waiter,
		format: &F,
	) -> Poll<Result<Timestamp, F::Error>> {
		// Keep reading more frames just to advance the max timestamp.
		let _ = self.buffer_all(waiter, format)?;

		if let Some(max) = self.max_timestamp {
			return Poll::Ready(Ok(max));
		}

		if let Poll::Ready(_frames) = self.group.poll_finished(waiter)? {
			return Poll::Ready(Err(moq_lite::Error::Decode.into()));
		}

		Poll::Pending
	}

	fn poll_min_timestamp<F: Container>(
		&mut self,
		waiter: &conducer::Waiter,
		format: &F,
	) -> Poll<Result<Timestamp, F::Error>> {
		let _ = self.buffer_one(waiter, format)?;

		if let Some(min) = self.min_timestamp {
			return Poll::Ready(Ok(min));
		}

		if let Poll::Ready(_frames) = self.group.poll_finished(waiter)? {
			return Poll::Ready(Err(moq_lite::Error::Decode.into()));
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
	use crate::hang::Legacy;
	use std::time::Duration;

	use bytes::Bytes;

	fn ts(micros: u64) -> Timestamp {
		Timestamp::from_micros(micros).unwrap()
	}

	/// Write a finished group with explicit sequence and timestamps (Legacy format).
	fn write_group(track: &mut moq_lite::TrackProducer, sequence: u64, timestamps: &[Timestamp]) {
		let mut group = track.create_group(moq_lite::Group { sequence }).unwrap();
		for &timestamp in timestamps {
			let frame = Frame {
				timestamp,
				payload: Bytes::from_static(&[0xDE, 0xAD]),
				keyframe: false,
			};
			Legacy.write(&mut group, &[frame]).unwrap();
		}
		group.finish().unwrap();
	}

	/// Drain all available frames with a per-read timeout.
	async fn read_all(consumer: &mut Consumer<Legacy>) -> Result<Vec<Frame>, hang::Error> {
		let mut frames = Vec::new();
		loop {
			match tokio::time::timeout(Duration::from_millis(200), consumer.read()).await {
				Ok(Ok(Some(frame))) => frames.push(frame),
				Ok(Ok(None)) => break,
				Ok(Err(e)) => return Err(e),
				Err(_) => panic!(
					"read_all: Consumer::read timed out after 200ms ({} frames collected so far)",
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
		let mut consumer = Consumer::new(consumer_track, Legacy).with_latency(Duration::from_millis(500));

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
		let mut consumer = Consumer::new(consumer_track, Legacy).with_latency(Duration::from_millis(500));

		write_group(&mut track, 0, &[ts(0), ts(33_000), ts(66_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 3);
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(33_000));
		assert_eq!(frames[2].timestamp, ts(66_000));

		assert!(frames[0].keyframe);
	}

	#[tokio::test]
	async fn read_multiple_groups_within_latency() {
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = Consumer::new(consumer_track, Legacy).with_latency(Duration::from_millis(500));

		// 5 groups, 20ms spacing. Total span = 80ms, well within 500ms latency.
		for i in 0..5u64 {
			write_group(&mut track, i, &[ts(i * 20_000)]);
		}
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 5);
	}

	// ---- Latency Skipping ----

	#[tokio::test]
	async fn latency_skip_delivers_recent_groups() {
		tokio::time::pause();
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = Consumer::new(consumer_track, Legacy).with_latency(Duration::from_millis(100));

		// Group 0: 5 frames, NOT finished (blocks consumer)
		let mut group0 = track.create_group(moq_lite::Group { sequence: 0 }).unwrap();
		for f in 0..5u64 {
			Legacy
				.write(
					&mut group0,
					&[Frame {
						timestamp: ts(f * 2_000),
						payload: Bytes::from_static(&[0xDE, 0xAD]),
						keyframe: false,
					}],
				)
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
		let mut consumer = Consumer::new(consumer_track, Legacy).with_latency(Duration::ZERO);

		let mut group0 = track.create_group(moq_lite::Group { sequence: 0 }).unwrap();
		Legacy
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(400_000),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
				}],
			)
			.unwrap();

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
		assert_eq!(frames.len(), 28, "Expected group 0 frame + groups 1-9");
		assert!(!frames.is_empty(), "Expected at least some frames");
		finisher.await.expect("finisher task panicked");
	}

	#[tokio::test]
	async fn latency_skip_correctness() {
		tokio::time::pause();
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = Consumer::new(consumer_track, Legacy).with_latency(Duration::from_millis(100));

		let mut group0 = track.create_group(moq_lite::Group { sequence: 0 }).unwrap();
		Legacy
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
				}],
			)
			.unwrap();

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
		assert_eq!(frames.len(), 10, "Expected group 0 frame + groups 1-9");
		assert_eq!(frames[0].timestamp, ts(0));

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
		let mut consumer = Consumer::new(consumer_track, Legacy).with_latency(Duration::from_millis(500));

		let mut group0 = track.create_group(moq_lite::Group { sequence: 0 }).unwrap();
		Legacy
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
				}],
			)
			.unwrap();

		write_group(&mut track, 2, &[ts(60_000)]);
		write_group(&mut track, 1, &[ts(30_000)]);
		track.finish().unwrap();

		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(10)).await;
			group0.finish().unwrap();
		});

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 3);
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(30_000));
		assert_eq!(frames[2].timestamp, ts(60_000));
		finisher.await.expect("finisher task panicked");
	}

	#[tokio::test]
	async fn adjacent_group_flushed_immediately() {
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = Consumer::new(consumer_track, Legacy).with_latency(Duration::from_millis(500));

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
		let mut consumer = Consumer::new(consumer_track, Legacy).with_latency(Duration::from_millis(500));

		write_group(&mut track, 0, &[ts(0), ts(66_000), ts(33_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 3);
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
		let mut consumer = Consumer::new(consumer_track, Legacy).with_latency(Duration::from_millis(500));

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
		let mut consumer = Consumer::new(consumer_track, Legacy).with_latency(Duration::from_millis(500));

		write_group(&mut track, 0, &[ts(0)]);
		track.abort(moq_lite::Error::Cancel).unwrap();

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
		let consumer = Consumer::new(consumer_track, Legacy).with_latency(Duration::from_millis(500));

		assert!(
			tokio::time::timeout(Duration::from_millis(50), consumer.closed())
				.await
				.is_err()
		);

		track.finish().unwrap();
		drop(track);

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
		let mut consumer = Consumer::new(consumer_track, Legacy).with_latency(Duration::from_millis(100));

		write_group(&mut track, 0, &[ts(0), ts(20_000)]);
		write_group(&mut track, 1, &[ts(40_000), ts(60_000)]);
		write_group(&mut track, 3, &[ts(120_000), ts(140_000)]);
		write_group(&mut track, 4, &[ts(160_000), ts(180_000)]);
		write_group(&mut track, 5, &[ts(200_000), ts(220_000)]);
		write_group(&mut track, 6, &[ts(240_000), ts(260_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert!(frames.len() >= 4, "Expected >= 4 frames, got {}", frames.len());
	}

	#[tokio::test]
	async fn gap_at_start_of_sequence() {
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = Consumer::new(consumer_track, Legacy).with_latency(Duration::from_millis(80));

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
		let mut consumer = Consumer::new(consumer_track, Legacy).with_latency(Duration::from_millis(500));

		write_group(&mut track, 0, &[ts(0), ts(33_333), ts(66_666)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 3);

		assert_eq!(frames[0].timestamp, ts(0));
		assert!(frames[0].keyframe);

		assert_eq!(frames[1].timestamp, ts(33_333));

		assert_eq!(frames[2].timestamp, ts(66_666));
	}

	#[tokio::test]
	async fn frame_payload_preserved() {
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = Consumer::new(consumer_track, Legacy).with_latency(Duration::from_millis(500));

		let payload_bytes = vec![0x01, 0x02, 0x03, 0x04, 0x05];
		let mut group = track.create_group(moq_lite::Group { sequence: 0 }).unwrap();
		Legacy
			.write(
				&mut group,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from(payload_bytes.clone()),

					keyframe: false,
				}],
			)
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

	#[tokio::test]
	async fn no_infinite_loop_with_buffered_frames() {
		tokio::time::pause();
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = Consumer::new(consumer_track, Legacy).with_latency(Duration::from_secs(10));

		let mut group0 = track.create_group(moq_lite::Group { sequence: 0 }).unwrap();
		Legacy
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
				}],
			)
			.unwrap();

		write_group(&mut track, 1, &[ts(100_000)]);

		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(20)).await;
			// Write group 2: recv_group fires, drops current buffer_until for group 1
			write_group(&mut track, 2, &[ts(200_000)]);
			tokio::time::sleep(Duration::from_millis(20)).await;
			group0.finish().unwrap();
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
		.expect("consumer hung — possible infinite loop regression");

		assert_eq!(frames.len(), 3);
		finisher.await.expect("finisher task panicked");
	}

	// ---- Edge Cases ----

	#[tokio::test]
	async fn large_timestamps() {
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = Consumer::new(consumer_track, Legacy).with_latency(Duration::from_secs(3700));

		let one_hour = 3_600_000_000u64;
		write_group(&mut track, 0, &[ts(one_hour)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1);
		assert_eq!(frames[0].timestamp, ts(one_hour));
		assert_eq!(frames[0].timestamp.as_micros(), one_hour as u128);
	}

	#[tokio::test]
	async fn set_latency_changes_behavior() {
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = Consumer::new(consumer_track, Legacy).with_latency(Duration::from_secs(10));

		write_group(&mut track, 0, &[ts(0)]);
		track.finish().unwrap();

		let frame = consumer.read().await.unwrap().unwrap();
		assert_eq!(frame.timestamp, ts(0));

		consumer.set_latency(Duration::from_millis(100));

		assert!(consumer.read().await.unwrap().is_none());
	}

	#[tokio::test]
	async fn max_timestamp_tracks_through_bframes() {
		tokio::time::pause();
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		// latency must exceed (group1_max - group0_min) = 100ms - 0ms = 100ms
		// to avoid the latency skip and test B-frame timestamp tracking.
		let mut consumer = Consumer::new(consumer_track, Legacy).with_latency(Duration::from_millis(110));

		let mut group0 = track.create_group(moq_lite::Group { sequence: 0 }).unwrap();
		for &timestamp in &[ts(0), ts(66_000), ts(33_000)] {
			Legacy
				.write(
					&mut group0,
					&[Frame {
						timestamp,
						payload: Bytes::from_static(&[0xDE, 0xAD]),
						keyframe: false,
					}],
				)
				.unwrap();
		}

		write_group(&mut track, 1, &[ts(100_000)]);
		track.finish().unwrap();

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
		let mut consumer = Consumer::new(consumer_track, Legacy).with_latency(Duration::from_millis(100));

		write_group(&mut track, 3, &[ts(0)]);
		write_group(&mut track, 5, &[ts(150_000)]);

		let mut group7 = track.create_group(moq_lite::Group { sequence: 7 }).unwrap();
		Legacy
			.write(
				&mut group7,
				&[Frame {
					timestamp: ts(300_000),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
				}],
			)
			.unwrap();

		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(50)).await;
			Legacy
				.write(
					&mut group7,
					&[Frame {
						timestamp: ts(400_000),
						payload: Bytes::from_static(&[0xBE, 0xEF]),
						keyframe: false,
					}],
				)
				.unwrap();
			group7.finish().unwrap();
			track.finish().unwrap();
		});

		let _frames = tokio::time::timeout(Duration::from_secs(2), async {
			let mut frames = Vec::new();
			while let Some(frame) = consumer.read().await.unwrap() {
				frames.push(frame);
			}
			frames
		})
		.await
		.expect("should not hang");

		finisher.await.unwrap();
	}

	#[tokio::test]
	async fn startup_skips_groups_without_data() {
		tokio::time::pause();
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = Consumer::new(consumer_track, Legacy).with_latency(Duration::from_millis(500));

		let _group5 = track.create_group(moq_lite::Group { sequence: 5 }).unwrap();
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
	}

	#[tokio::test]
	async fn startup_single_group_mid_stream() {
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = Consumer::new(consumer_track, Legacy).with_latency(Duration::from_millis(500));

		write_group(&mut track, 100, &[ts(3_000_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1);
	}

	#[tokio::test]
	async fn multiple_sequential_latency_skips() {
		tokio::time::pause();
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = Consumer::new(consumer_track, Legacy).with_latency(Duration::from_millis(50));

		let mut group0 = track.create_group(moq_lite::Group { sequence: 0 }).unwrap();
		Legacy
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xAA]),

					keyframe: false,
				}],
			)
			.unwrap();

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
		let mut consumer = Consumer::new(consumer_track, Legacy).with_latency(Duration::from_millis(100));

		let mut group0 = track.create_group(moq_lite::Group { sequence: 0 }).unwrap();
		Legacy
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xAA]),

					keyframe: false,
				}],
			)
			.unwrap();

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

	/// Regression: a single stalled group with one newer group should trigger
	/// a latency skip when the timestamp difference exceeds latency.
	/// Previously, the span was computed across newer groups only (zero for one
	/// group), so the skip never fired.
	#[tokio::test]
	async fn single_newer_group_triggers_skip() {
		tokio::time::pause();
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = Consumer::new(consumer_track, Legacy).with_latency(Duration::from_millis(100));

		// Group 0: stalled at ts=0, NOT finished
		let mut group0 = track.create_group(moq_lite::Group { sequence: 0 }).unwrap();
		Legacy
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
				}],
			)
			.unwrap();

		// Group 1: finished, 200ms ahead (well beyond 100ms latency)
		write_group(&mut track, 1, &[ts(200_000)]);
		track.finish().unwrap();

		let finisher = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(50)).await;
			group0.finish().unwrap();
		});

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 2, "Expected group 0 frame + group 1 frame");
		finisher.await.unwrap();
	}

	/// Regression: when the current group is fully consumed and the next sequence
	/// is missing (gap), the consumer should skip to the next available group
	/// once the track is fully received, rather than hanging forever.
	#[tokio::test]
	async fn single_missing_sequence_near_eof_skips() {
		tokio::time::pause();
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = Consumer::new(consumer_track, Legacy).with_latency(Duration::from_millis(100));

		// Group 0: finished normally
		write_group(&mut track, 0, &[ts(0), ts(20_000)]);
		// Group 2: finished (group 1 is missing — sequence gap)
		write_group(&mut track, 2, &[ts(200_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 3, "Expected group 0 (2 frames) + group 2 (1 frame)");
	}

	#[tokio::test]
	async fn group_error_skips_to_next() {
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = Consumer::new(consumer_track, Legacy).with_latency(Duration::from_millis(500));

		let mut group0 = track.create_group(moq_lite::Group { sequence: 0 }).unwrap();
		group0.abort(moq_lite::Error::Cancel).unwrap();

		write_group(&mut track, 1, &[ts(30_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1);
	}

	#[tokio::test]
	async fn track_finishes_while_reading() {
		tokio::time::pause();
		let mut track = moq_lite::Track::new("test").produce();
		let consumer_track = track.consume();
		let mut consumer = Consumer::new(consumer_track, Legacy).with_latency(Duration::from_millis(500));

		write_group(&mut track, 0, &[ts(0)]);

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
		let mut consumer = Consumer::new(consumer_track, Legacy).with_latency(Duration::from_millis(500));

		let mut group0 = track.create_group(moq_lite::Group { sequence: 0 }).unwrap();
		group0.finish().unwrap();

		write_group(&mut track, 1, &[ts(30_000)]);
		track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1);
	}

	// ---- Media Container ----

	#[cfg(feature = "mp4")]
	#[tokio::test]
	async fn media_container_legacy() {
		tokio::time::pause();

		let media = crate::hang::Media::try_from(&hang::catalog::Container::Legacy).unwrap();

		let mut track = moq_lite::Track::new("video").produce();
		let consumer_track = track.consume();
		let mut consumer = Consumer::new(consumer_track, media).with_latency(Duration::from_millis(500));

		// Write frames using Legacy encoding
		let mut group = track.create_group(moq_lite::Group { sequence: 0 }).unwrap();
		for i in 0..3u64 {
			let frame = Frame {
				timestamp: ts(i * 33_333),
				payload: Bytes::from_static(&[0xDE, 0xAD]),
				keyframe: false,
			};
			Legacy.write(&mut group, &[frame]).unwrap();
		}
		group.finish().unwrap();
		track.finish().unwrap();

		let mut frames = Vec::new();
		while let Some(frame) = consumer.read().await.unwrap() {
			frames.push(frame);
		}

		assert_eq!(frames.len(), 3);
		assert_eq!(frames[0].timestamp, ts(0));
		assert!(frames[0].keyframe);
		assert_eq!(frames[1].timestamp, ts(33_333));
		assert!(!frames[1].keyframe);
		assert_eq!(frames[2].timestamp, ts(66_666));
		assert!(!frames[2].keyframe);
	}
}
