use super::{Container, Frame};

/// A producer for media tracks that manages group boundaries.
///
/// Generic over `C: Container` to support different container encodings
/// (Legacy, CMAF, LOC). Use [`catalog::hang::Container`](crate::catalog::hang::Container)
/// to dispatch on the catalog at runtime.
///
/// ## Group Management
///
/// Every group must start with a keyframe. Writing a frame with `keyframe = true`
/// closes the previous group (if any) and starts a new one. Writing a non-keyframe
/// frame when no group is open is a protocol violation.
///
/// [`finish_group`](Self::finish_group) closes the current group early; the next write
/// must be a keyframe. This is useful for streams without inherent keyframes
/// (e.g. audio) that mark every Nth frame as a keyframe but want to flush the
/// current group immediately rather than waiting for the next keyframe to arrive.
///
/// ## Latency Buffering
///
/// When `latency` is zero (default), each frame is written immediately as its own
/// container frame. When non-zero, frames are buffered and flushed together when:
/// - A keyframe arrives (flushes the previous group's buffer, starts new group),
/// - The buffered duration exceeds `latency`,
/// - `finish()` is called.
///
/// This is useful for CMAF where multiple samples should be packed into one moof+mdat.
pub struct Producer<C: Container> {
	inner: moq_net::TrackProducer,
	container: C,
	group: Option<moq_net::GroupProducer>,
	buffer: Vec<Frame>,

	latency: std::time::Duration,

	/// Sequence to use for the next group opened by [`Self::write`].
	/// Set by [`Self::seek`] and consumed on the next group creation.
	pending_sequence: Option<u64>,
}

impl<C: Container> Producer<C> {
	/// Create a new Producer wrapping the given moq-lite producer.
	pub fn new(track: moq_net::TrackProducer, container: C) -> Self {
		Self {
			inner: track,
			container,
			group: None,
			buffer: Vec::new(),
			latency: std::time::Duration::ZERO,
			pending_sequence: None,
		}
	}

	/// The underlying moq-lite track producer. Read-only; mutating it directly
	/// would sidestep group/keyframe invariants.
	pub fn track(&self) -> &moq_net::TrackProducer {
		&self.inner
	}

	/// Set the maximum buffering latency.
	///
	/// When non-zero, frames are buffered and flushed together when the buffered
	/// duration exceeds this value, or when a keyframe arrives. This allows packing
	/// multiple samples into a single container frame (e.g. CMAF moof+mdat).
	///
	/// Default is zero (flush immediately).
	pub fn with_latency(mut self, latency: std::time::Duration) -> Self {
		self.latency = latency;
		self
	}

	/// Write a frame to the track.
	///
	/// A keyframe closes any open group and starts a new one. A non-keyframe extends
	/// the current group; if no group is open it returns [`MissingKeyframe`](super::MissingKeyframe),
	/// so a caller joining mid-stream can skip frames until the first keyframe.
	pub fn write(&mut self, frame: Frame) -> Result<(), C::Error> {
		// Close the current group on an explicit keyframe, passing its timestamp so
		// the previous group's last frame can borrow it as a duration boundary.
		if frame.keyframe {
			self.finish_group_at(Some(frame.timestamp))?;
		}

		// Start a new group if needed; the first frame of a group must be a keyframe.
		if self.group.is_none() {
			if !frame.keyframe {
				// No group yet and this delta can't anchor one. The caller (e.g. a
				// mid-stream join) decides whether to skip until the first keyframe.
				return Err(super::MissingKeyframe.into());
			}
			self.group = Some(match self.pending_sequence.take() {
				Some(sequence) => self.inner.create_group(moq_net::Group { sequence })?,
				None => self.inner.append_group()?,
			});
		}

		// Buffer or write the frame.
		if self.latency.is_zero() {
			let group = self.group.as_mut().unwrap();
			self.container.write(group, &[frame])?;
		} else {
			self.buffer.push(frame);

			// Flush if the buffered span has reached the latency budget. Compute
			// min/max across the buffer rather than first/last: frames within a track
			// are in *decode* order, and B-frames have non-monotonic PTS, so
			// `last - first` can shrink as a B-frame lands between two earlier-PTS
			// frames. The min/max pair captures the actual presentation span.
			if self.buffer.len() >= 2 {
				let mut iter = self.buffer.iter().map(|f| std::time::Duration::from(f.timestamp));
				let first = iter.next().unwrap();
				let (min, max) = iter.fold((first, first), |(min, max), d| (min.min(d), max.max(d)));
				if max.saturating_sub(min) >= self.latency {
					self.flush(None)?;
				}
			}
		}

		Ok(())
	}

	/// Close the current group early, flushing any buffered frames.
	///
	/// The next [`write`](Self::write) must be a keyframe.
	pub fn finish_group(&mut self) -> Result<(), C::Error> {
		self.finish_group_at(None)
	}

	/// Like [`finish_group`](Self::finish_group), but uses `next` (the timestamp of the
	/// keyframe that rolled the group over) as the duration boundary for the group's
	/// last frame. See [`flush`](Self::flush).
	fn finish_group_at(&mut self, next: Option<moq_net::Timestamp>) -> Result<(), C::Error> {
		self.flush(next)?;
		if let Some(mut group) = self.group.take() {
			group.finish()?;
		}
		Ok(())
	}

	/// Close the current group (if any) and open the next group at the given sequence.
	///
	/// The next [`write`](Self::write) must be a keyframe and will land in a group with
	/// `sequence`. Useful for joining mid-stream or signalling a discontinuity.
	pub fn seek(&mut self, sequence: u64) -> Result<(), C::Error> {
		self.finish_group()?;
		self.pending_sequence = Some(sequence);
		Ok(())
	}

	/// Flush any buffered frames into the current group without closing it.
	///
	/// `next`, when given, is the timestamp of the frame that rolled the group over
	/// (the next keyframe). The buffer's last frame is the only sample whose successor
	/// wasn't visible when it arrived, so we backfill its duration from `next` here.
	/// This adds no latency: that frame is already in hand. Containers that don't use
	/// per-frame durations (Legacy, LOC) ignore it.
	fn flush(&mut self, next: Option<moq_net::Timestamp>) -> Result<(), C::Error> {
		if self.buffer.is_empty() {
			return Ok(());
		}

		if let Some(next) = next
			&& let Some(last) = self.buffer.last_mut()
			&& last.duration.is_none()
			&& let Ok(duration) = next.checked_sub(last.timestamp)
		{
			last.duration = Some(duration);
		}

		let group = match &mut self.group {
			Some(group) => group,
			None => return Ok(()),
		};

		self.container.write(group, &self.buffer)?;
		self.buffer.clear();

		Ok(())
	}

	/// Finish the track, flushing any buffered frames and closing any open group.
	pub fn finish(&mut self) -> Result<(), C::Error> {
		self.finish_group()?;
		self.inner.finish()?;
		Ok(())
	}

	/// Create a consumer for this track.
	pub fn consume(&self) -> moq_net::TrackSubscriber {
		self.inner.subscribe(None)
	}
}

impl<C: Container> std::ops::Deref for Producer<C> {
	type Target = moq_net::TrackProducer;

	fn deref(&self) -> &Self::Target {
		&self.inner
	}
}

#[cfg(test)]
mod tests {
	use bytes::Bytes;

	use super::*;
	use crate::catalog::hang::Container;
	use moq_net::Timestamp;

	fn frame(timestamp_us: u64, keyframe: bool) -> Frame {
		Frame {
			timestamp: Timestamp::from_micros(timestamp_us).unwrap(),
			payload: Bytes::from_static(&[0xDE, 0xAD]),
			keyframe,
			duration: None,
		}
	}

	/// Drain all groups from a finished track, returning their frame counts.
	async fn collect_groups(mut consumer: moq_net::TrackSubscriber) -> Vec<usize> {
		let mut groups = Vec::new();
		while let Some(mut group) = consumer.recv_group().await.unwrap() {
			let mut count = 0;
			while group.next_frame().await.unwrap().is_some() {
				count += 1;
			}
			groups.push(count);
		}
		groups
	}

	/// Explicit keyframe closes the current group and starts a new one.
	#[tokio::test]
	async fn keyframe_closes_group_immediately() {
		let track = moq_net::TrackProducer::new(
			"test",
			moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
		);
		let consumer = track.subscribe(None);
		let mut producer = Producer::new(track, Container::Legacy);

		producer.write(frame(0, true)).unwrap(); // first frame must be a keyframe
		producer.write(frame(10_000, false)).unwrap();
		producer.write(frame(20_000, true)).unwrap(); // keyframe → new group
		producer.write(frame(30_000, false)).unwrap();
		producer.finish().unwrap();

		assert_eq!(collect_groups(consumer).await, vec![2, 2]);
	}

	/// `finish_group()` flushes the current group immediately; the next write must be a keyframe.
	#[tokio::test]
	async fn finish_group_closes_immediately() {
		let track = moq_net::TrackProducer::new(
			"test",
			moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
		);
		let consumer = track.subscribe(None);
		let mut producer = Producer::new(track, Container::Legacy);

		producer.write(frame(0, true)).unwrap();
		producer.write(frame(10_000, false)).unwrap();
		producer.finish_group().unwrap();
		producer.write(frame(20_000, true)).unwrap();
		producer.finish().unwrap();

		assert_eq!(collect_groups(consumer).await, vec![2, 1]);
	}

	/// Writing a non-keyframe with no open group returns MissingKeyframe.
	#[test]
	fn first_frame_must_be_keyframe() {
		let track = moq_net::TrackProducer::new(
			"test",
			moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
		);
		let mut producer = Producer::new(track, Container::Legacy);

		let err = producer.write(frame(0, false)).unwrap_err();
		assert!(matches!(err, crate::Error::MissingKeyframe(_)));
	}

	/// Drain all groups from a finished track, returning their sequence numbers.
	async fn collect_sequences(mut consumer: moq_net::TrackSubscriber) -> Vec<u64> {
		let mut sequences = Vec::new();
		while let Some(group) = consumer.recv_group().await.unwrap() {
			sequences.push(group.sequence);
		}
		sequences
	}

	/// `seek(n)` opens the next group at sequence `n`.
	#[tokio::test]
	async fn seek_uses_explicit_sequence() {
		let track = moq_net::TrackProducer::new(
			"test",
			moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
		);
		let consumer = track.subscribe(None);
		let mut producer = Producer::new(track, Container::Legacy);

		producer.write(frame(0, true)).unwrap(); // seq 0
		producer.seek(42).unwrap();
		producer.write(frame(10_000, true)).unwrap(); // seq 42
		producer.finish().unwrap();

		assert_eq!(collect_sequences(consumer).await, vec![0, 42]);
	}

	/// `seek` is consumed on the next group creation; subsequent groups auto-increment from there.
	#[tokio::test]
	async fn seek_clears_pending_after_use() {
		let track = moq_net::TrackProducer::new(
			"test",
			moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
		);
		let consumer = track.subscribe(None);
		let mut producer = Producer::new(track, Container::Legacy);

		producer.seek(5).unwrap();
		producer.write(frame(0, true)).unwrap(); // seq 5
		producer.write(frame(10_000, true)).unwrap(); // seq 6 (auto-incremented)
		producer.finish().unwrap();

		assert_eq!(collect_sequences(consumer).await, vec![5, 6]);
	}

	/// Records the frames handed to each `write`, so tests can inspect the
	/// durations the producer backfilled. Write-only.
	#[derive(Clone, Default)]
	struct Recording(std::rc::Rc<std::cell::RefCell<Vec<Vec<Frame>>>>);

	impl super::Container for Recording {
		type Error = crate::Error;

		fn write(&self, _group: &mut moq_net::GroupProducer, frames: &[Frame]) -> Result<(), Self::Error> {
			self.0.borrow_mut().push(frames.to_vec());
			Ok(())
		}

		fn poll_read(
			&self,
			_group: &mut moq_net::GroupConsumer,
			_waiter: &kio::Waiter,
		) -> std::task::Poll<Result<Option<Vec<Frame>>, Self::Error>> {
			unreachable!("Recording is write-only")
		}
	}

	/// The keyframe that rolls a group over backfills the duration of the previous
	/// group's last frame, without buffering an extra frame.
	#[tokio::test]
	async fn keyframe_backfills_last_frame_duration() {
		let track = moq_net::TrackProducer::new(
			"test",
			moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
		);
		let recording = Recording::default();
		let mut producer = Producer::new(track, recording.clone()).with_latency(std::time::Duration::from_secs(10));

		producer.write(frame(0, true)).unwrap(); // group 0 opens
		producer.write(frame(33_000, false)).unwrap(); // buffered
		producer.write(frame(66_000, true)).unwrap(); // rolls group 0 over -> flush with next = 66ms
		producer.finish().unwrap();

		let writes = recording.0.borrow();
		let group0 = &writes[0];
		assert_eq!(group0.len(), 2);
		// Last frame's duration backfilled from the next keyframe: 66ms - 33ms.
		assert_eq!(group0[1].duration, Some(Timestamp::from_micros(33_000).unwrap()));
		// The earlier frame keeps None; only the trailing sample needs the boundary.
		assert_eq!(group0[0].duration, None);
	}
}
