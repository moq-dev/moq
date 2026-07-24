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
/// [`cut`](Self::cut) closes the current group early, ideally saying where its content
/// ends; the next write must be a keyframe. Reach for it when the following keyframe won't
/// supply that boundary in time, or for a stream without inherent keyframes (e.g. audio)
/// that marks every Nth frame as one but wants to close the current group now.
/// [`discontinuity`](Self::discontinuity) goes further and publishes an empty group, for
/// when the timeline is about to jump rather than merely continue.
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
	inner: moq_net::track::Producer,
	container: C,
	group: Option<moq_net::group::Producer>,
	buffer: Vec<Frame>,

	latency: std::time::Duration,

	/// Sequence to use for the next group opened by [`Self::write`].
	/// Set by [`Self::seek`] and consumed on the next group creation.
	pending_sequence: Option<u64>,

	/// Records each group open (sequence + keyframe timestamp) into this rendition's
	/// timeline track, when the producer was built with one.
	recorder: Option<crate::timeline::Recorder>,
}

impl<C: Container> Producer<C> {
	/// Create a Producer wrapping the given moq-lite producer, muxing into `container`.
	///
	/// A plain media track by default: no latency buffering, no timeline. Add buffering with
	/// [`with_latency`](Self::with_latency); the timeline recorder is wired by the catalog (see
	/// [`catalog::Producer::media_producer`](crate::catalog::Producer::media_producer)).
	pub fn new(track: moq_net::track::Producer, container: C) -> Self {
		Self {
			inner: track,
			container,
			group: None,
			buffer: Vec::new(),
			latency: std::time::Duration::ZERO,
			pending_sequence: None,
			recorder: None,
		}
	}

	/// Set the maximum buffering latency.
	///
	/// When non-zero, frames are buffered and flushed together when the buffered duration exceeds
	/// it, or a keyframe arrives, packing multiple samples into one container frame (e.g. a CMAF
	/// moof+mdat). Zero (the default) flushes each frame immediately.
	pub fn with_latency(mut self, latency: std::time::Duration) -> Self {
		self.latency = latency;
		self
	}

	/// Record each group open (sequence + keyframe timestamp) through `recorder`, so consumers can
	/// index the media without downloading it.
	///
	/// Mint the recorder from a [`timeline::Producer`](crate::timeline::Producer) (see
	/// [`catalog::Producer::timeline`](crate::catalog::Producer::timeline)). The record carries no
	/// track id, so wire one recorder per timeline; a set of aligned renditions shares a timeline by
	/// recording only the source and advertising the same section on the rest.
	/// [`media_producer`](crate::catalog::Producer::media_producer) wires the 1:1 default for you.
	pub fn with_recorder(mut self, recorder: crate::timeline::Recorder) -> Self {
		self.recorder = Some(recorder);
		self
	}

	/// The underlying moq-lite track producer. Read-only; mutating it directly
	/// would sidestep group/keyframe invariants.
	pub fn track(&self) -> &moq_net::track::Producer {
		&self.inner
	}

	/// Write a frame to the track.
	///
	/// A keyframe closes any open group and starts a new one. A non-keyframe extends
	/// the current group; if no group is open it returns [`MissingKeyframe`](super::MissingKeyframe),
	/// so a caller joining mid-stream can skip frames until the first keyframe.
	pub fn write(&mut self, frame: Frame) -> Result<(), C::Error> {
		// A keyframe cuts the previous group, using its timestamp as the boundary
		// where the previous group's content ends.
		if frame.keyframe {
			self.cut(Some(frame.timestamp))?;
		}

		// Start a new group if needed; the first frame of a group must be a keyframe.
		if self.group.is_none() {
			if !frame.keyframe {
				// No group yet and this delta can't anchor one. The caller (e.g. a
				// mid-stream join) decides whether to skip until the first keyframe.
				return Err(super::MissingKeyframe.into());
			}
			let group = match self.pending_sequence.take() {
				Some(sequence) => self.inner.create_group(moq_net::group::Info { sequence })?,
				None => self.inner.append_group()?,
			};

			// Index the group the moment it opens: its start is this keyframe's timestamp. The
			// timeline is an optional sidecar (consumers tolerate gaps by extrapolating), so a
			// recording failure must NOT abort the media write. Drop the recorder and carry on.
			let timeline_err = match self.recorder.as_mut() {
				Some(recorder) => recorder.record(group.sequence, frame.timestamp).err(),
				None => None,
			};
			if let Some(err) = timeline_err {
				tracing::warn!(?err, "timeline recording failed; dropping the timeline for this track");
				self.recorder = None;
			}

			self.group = Some(group);
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

	/// Cut the current group, flushing buffered frames and closing it.
	///
	/// `end` bounds the final buffered frame when the publisher knows where the
	/// group's content stops. The next [`write`](Self::write) must be a keyframe.
	pub fn cut(&mut self, end: Option<moq_net::Timestamp>) -> Result<(), C::Error> {
		self.flush(end)?;
		if let Some(mut group) = self.group.take() {
			group.finish()?;
		}
		Ok(())
	}

	#[doc(hidden)]
	#[deprecated(note = "use `cut`")]
	pub fn finish_group(&mut self) -> Result<(), C::Error> {
		self.cut(None)
	}

	/// Close the current group (if any) and open the next group at the given sequence.
	///
	/// The next [`write`](Self::write) must be a keyframe and will land in a group with
	/// `sequence`. Useful for joining mid-stream.
	pub fn seek(&mut self, sequence: u64) -> Result<(), C::Error> {
		self.cut(None)?;
		self.pending_sequence = Some(sequence);
		Ok(())
	}

	/// Publish an EMPTY group standing for a break in the timeline: content stopped, and
	/// whatever comes next does not continue it.
	///
	/// Call this whenever the timeline is about to jump -- pausing an encoder, switching
	/// source, resuming on a re-anchored clock. Without it a break is invisible: the next
	/// group looks exactly like the one that would have followed, and a consumer bounding a
	/// sample by the next group's first frame hands it the entire gap as its duration. That
	/// produced a 2405 second video sample out of a publisher that had been paused 40 minutes
	/// (moq-dev/moq.pro#814). Consecutive sequence numbers can't rule a pause out, so this
	/// marker is the only thing that can say one happened.
	///
	/// It also fixes what a subscriber joining mid-break sees. A subscription starts at the
	/// track's latest group, and creating this one advances that -- so a late joiner lands on
	/// the marker and waits for real media, instead of being served the group from *before*
	/// the break as though it were live.
	///
	/// Carries no timestamp on purpose: a break is a gap between two groups, so any single
	/// timestamp is ambiguous about which side it belongs to. To bound the closing group's
	/// final frame, [`cut(end)`](Self::cut) before calling this; the open group is closed
	/// either way (an unbounded [`cut`](Self::cut) here is a no-op after yours).
	///
	/// The marker group carries no frames at all. Ending the closing group with an empty
	/// frame at `end` is the eventual shape, once decoders are known to skip one.
	pub fn discontinuity(&mut self) -> Result<(), C::Error> {
		self.cut(None)?;
		let mut group = match self.pending_sequence.take() {
			Some(sequence) => self.inner.create_group(moq_net::group::Info { sequence })?,
			None => self.inner.append_group()?,
		};
		group.finish()?;
		Ok(())
	}

	/// Flush any buffered frames into the current group without closing it.
	///
	/// Backfills the per-sample duration the source didn't provide. A CMAF fragment
	/// reconstructs each sample's DTS by accumulating durations, so every non-final
	/// sample packed into one fragment needs one or the decoder collapses their
	/// timestamps. Frames are in decode order, so a sample's duration is the gap to the
	/// next buffered sample; the final sample borrows `next` (the timestamp of the
	/// keyframe that rolled the group over), which is already in hand so this adds no
	/// latency. Frames that already carry a duration (e.g. fMP4 passthrough) keep it,
	/// and a backwards gap (a B-frame whose successor presents earlier) is left unset.
	/// Containers that don't use per-frame durations (Legacy, LOC) ignore the field.
	fn flush(&mut self, next: Option<moq_net::Timestamp>) -> Result<(), C::Error> {
		if self.buffer.is_empty() {
			return Ok(());
		}

		for i in 0..self.buffer.len() {
			if self.buffer[i].duration.is_some() {
				continue;
			}
			let boundary = self.buffer.get(i + 1).map(|f| f.timestamp).or(next);
			if let Some(boundary) = boundary
				&& let Ok(duration) = boundary.checked_sub(self.buffer[i].timestamp)
			{
				self.buffer[i].duration = Some(duration);
			}
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
		self.cut(None)?;
		self.inner.finish()?;
		Ok(())
	}

	/// Abort the track and any open group with the given error.
	///
	/// The counterpart to [`Self::finish`] for a failed teardown: consumers observe
	/// `err` instead of the generic [`moq_net::Error::Dropped`] a bare drop surfaces,
	/// so the real cause (a disconnect, a decode failure) reaches them. Any buffered
	/// frames are discarded, not flushed. Consumes the producer.
	pub fn abort(mut self, err: moq_net::Error) {
		self.buffer.clear();
		if let Some(group) = self.group.take() {
			let _ = group.abort(err.clone());
		}
		let _ = self.inner.abort(err);
	}

	/// Create a consumer for this track.
	pub fn consume(&self) -> moq_net::track::Subscriber {
		self.inner.subscribe(None)
	}
}

impl<C: Container> std::ops::Deref for Producer<C> {
	type Target = moq_net::track::Producer;

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

	/// Mint a standalone track for tests via a throwaway broadcast, since tracks are
	/// born from their broadcast (no public `track::Producer::new`).
	fn track_producer(
		name: impl Into<std::sync::Arc<str>>,
		info: impl Into<Option<moq_net::track::Info>>,
	) -> moq_net::track::Producer {
		moq_net::broadcast::Info::new()
			.produce()
			.create_track(name, info)
			.unwrap()
	}

	fn frame(timestamp_us: u64, keyframe: bool) -> Frame {
		Frame {
			timestamp: Timestamp::from_micros(timestamp_us).unwrap(),
			payload: Bytes::from_static(&[0xDE, 0xAD]),
			keyframe,
			duration: None,
		}
	}

	/// Drain all groups from a finished track, returning their frame counts.
	async fn collect_groups(mut consumer: moq_net::track::Subscriber) -> Vec<usize> {
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

	/// A discontinuity lands as its own empty group between the content either side, so a
	/// consumer can see the break instead of inferring continuity from adjacent sequences.
	#[tokio::test]
	async fn discontinuity_publishes_an_empty_group() {
		let track = track_producer("test", hang::container::track_info());
		let consumer = track.subscribe(None);
		let mut producer = Producer::new(track, Container::Legacy);

		producer.write(frame(0, true)).unwrap();
		producer.write(frame(10_000, false)).unwrap();
		producer.discontinuity().unwrap();
		// Resumed on a re-anchored clock, 40 minutes later.
		producer.write(frame(2_405_070_000, true)).unwrap();
		producer.finish().unwrap();

		assert_eq!(collect_groups(consumer).await, vec![2, 0, 1]);
	}

	/// A subscription starts at the track's LATEST group, and the marker advances it even
	/// though it carries nothing. So a subscriber arriving mid-break waits for real media
	/// rather than being handed the pre-break group as if it were live -- which is how a
	/// 40-minute-stale frame reached a VOD recording in moq-dev/moq.pro#814.
	#[tokio::test]
	async fn discontinuity_moves_the_live_edge_off_stale_content() {
		let track = track_producer("test", hang::container::track_info());
		let mut producer = Producer::new(track, Container::Legacy);

		producer.write(frame(0, true)).unwrap();
		let stale = producer.track().latest();

		producer.discontinuity().unwrap();
		let edge = producer.track().latest();

		assert_ne!(edge, stale, "the empty group is the live edge now");
		assert_eq!(edge, stale.map(|s| s + 1));
	}

	/// Explicit keyframe closes the current group and starts a new one.
	#[tokio::test]
	async fn keyframe_closes_group_immediately() {
		let track = track_producer("test", hang::container::track_info());
		let consumer = track.subscribe(None);
		let mut producer = Producer::new(track, Container::Legacy);

		producer.write(frame(0, true)).unwrap(); // first frame must be a keyframe
		producer.write(frame(10_000, false)).unwrap();
		producer.write(frame(20_000, true)).unwrap(); // keyframe → new group
		producer.write(frame(30_000, false)).unwrap();
		producer.finish().unwrap();

		assert_eq!(collect_groups(consumer).await, vec![2, 2]);
	}

	/// `cut()` flushes the current group immediately; the next write must be a keyframe.
	#[tokio::test]
	async fn cut_closes_immediately() {
		let track = track_producer("test", hang::container::track_info());
		let consumer = track.subscribe(None);
		let mut producer = Producer::new(track, Container::Legacy);

		producer.write(frame(0, true)).unwrap();
		producer.write(frame(10_000, false)).unwrap();
		producer
			.cut(Some(moq_net::Timestamp::from_micros(15_000).unwrap()))
			.unwrap();
		producer.write(frame(20_000, true)).unwrap();
		producer.finish().unwrap();

		assert_eq!(collect_groups(consumer).await, vec![2, 1]);
	}

	#[tokio::test]
	#[allow(deprecated)]
	async fn deprecated_finish_group_still_closes() {
		let track = track_producer("test", hang::container::track_info());
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
		let track = track_producer("test", hang::container::track_info());
		let mut producer = Producer::new(track, Container::Legacy);

		let err = producer.write(frame(0, false)).unwrap_err();
		assert!(matches!(err, crate::Error::MissingKeyframe(_)));
	}

	/// Drain all groups from a finished track, returning their sequence numbers.
	async fn collect_sequences(mut consumer: moq_net::track::Subscriber) -> Vec<u64> {
		let mut sequences = Vec::new();
		while let Some(group) = consumer.recv_group().await.unwrap() {
			sequences.push(group.sequence);
		}
		sequences
	}

	/// `seek(n)` opens the next group at sequence `n`.
	#[tokio::test]
	async fn seek_uses_explicit_sequence() {
		let track = track_producer("test", hang::container::track_info());
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
		let track = track_producer("test", hang::container::track_info());
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

		fn write(&self, _group: &mut moq_net::group::Producer, frames: &[Frame]) -> Result<(), Self::Error> {
			self.0.borrow_mut().push(frames.to_vec());
			Ok(())
		}

		fn poll_read(
			&self,
			_group: &mut moq_net::group::Consumer,
			_waiter: &kio::Waiter,
		) -> std::task::Poll<Result<Option<Vec<Frame>>, Self::Error>> {
			unreachable!("Recording is write-only")
		}
	}

	/// The keyframe that rolls a group over backfills the duration of the previous
	/// group's last frame, without buffering an extra frame.
	#[tokio::test]
	async fn keyframe_backfills_batched_durations() {
		let track = track_producer("test", hang::container::track_info());
		let recording = Recording::default();
		let mut producer = Producer::new(track, recording.clone()).with_latency(std::time::Duration::from_secs(10));

		producer.write(frame(0, true)).unwrap(); // group 0 opens
		producer.write(frame(33_000, false)).unwrap(); // buffered
		producer.write(frame(66_000, true)).unwrap(); // rolls group 0 over -> flush with next = 66ms
		producer.finish().unwrap();

		let writes = recording.0.borrow();
		let group0 = &writes[0];
		assert_eq!(group0.len(), 2);
		// The first sample's duration is the gap to the next buffered sample: 33ms - 0.
		assert_eq!(group0[0].duration, Some(Timestamp::from_micros(33_000).unwrap()));
		// The last sample's duration is backfilled from the next keyframe: 66ms - 33ms.
		assert_eq!(group0[1].duration, Some(Timestamp::from_micros(33_000).unwrap()));
	}
}
