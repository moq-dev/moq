//! Receive encoded video independently of the paced decoder and window.

use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use moq_mux::container::Frame;
use tokio::time::Instant;

use super::buffer::Buffer;
use super::output::Output;
use super::timeline::Presentation;
use super::window::Event;

/// A few surfaces, independent of the requested playout delay. A codec batch
/// that overflows it waits beside the queue rather than pushing out pictures
/// the window has yet to show.
pub(super) const MAX_FRAMES: usize = 3;
/// How long before the earliest owed picture is due to feed the codec.
const DECODE_AHEAD: Duration = Duration::from_millis(100);

/// What the decode loop does next.
enum Step {
	/// Sleep until the instant, or until the buffer or the window changes.
	Wait(Option<std::time::Instant>),
	Decode,
	/// The track ended: drain what the codec still holds.
	Flush,
}

/// When the window's queue has room for another picture, or `None` if it does
/// now.
///
/// The window presents the newest due picture, so a full queue's oldest one
/// is skipped anyway once the next falls due. Making room then keeps a
/// stalled window from stalling decode without dropping a picture a live one
/// would show.
fn vacancy(queue: &VecDeque<moq_video::Frame>, presentation: &Presentation) -> Option<std::time::Instant> {
	if queue.len() < MAX_FRAMES {
		return None;
	}
	queue.get(1).and_then(|frame| presentation.due(frame.timestamp))
}

type Track = moq_mux::container::Consumer<moq_mux::catalog::hang::Container>;

/// The window's shared state and the subscription's encoded retention budget.
pub(super) struct Video<O> {
	pub presentation: Arc<Mutex<Presentation>>,
	pub frames: Arc<Mutex<VecDeque<moq_video::Frame>>>,
	pub changed: Arc<tokio::sync::Notify>,
	pub output: O,
	pub max_delay: Duration,
}

/// The production sink and the deterministic buffered decoder used by tests.
pub(super) trait Decoder {
	async fn decode(&mut self, frame: Frame) -> anyhow::Result<Vec<moq_video::Frame>>;
	async fn flush(&mut self) -> anyhow::Result<Vec<moq_video::Frame>>;
}

impl Decoder for moq_video::decode::Sink {
	async fn decode(&mut self, frame: Frame) -> anyhow::Result<Vec<moq_video::Frame>> {
		Ok(self.decode(frame.payload, frame.timestamp, frame.keyframe).await?)
	}

	async fn flush(&mut self) -> anyhow::Result<Vec<moq_video::Frame>> {
		Ok(self.flush().await?)
	}
}

impl<O: Output> Video<O> {
	pub async fn run(self, mut track: Track, mut decoder: impl Decoder) -> anyhow::Result<()> {
		let buffer = Mutex::new(Buffer::default());
		let receive = async {
			let mut discontinuity = track.discontinuity();
			loop {
				let frame = track.read().await?;
				let mut buffer = buffer.lock().unwrap();
				let before = buffer.generation;
				if track.discontinuity() != discontinuity {
					discontinuity = track.discontinuity();
					buffer.clear();
					self.presentation.lock().unwrap().video_restarted();
				}
				let ended = frame.is_none();
				if let Some(frame) = frame {
					if self
						.presentation
						.lock()
						.unwrap()
						.video(frame.timestamp, Instant::now().into_std())
					{
						self.output.send(Event::Wake);
					}
					buffer.push(frame, self.max_delay);
				} else {
					buffer.ended = true;
				}
				if buffer.generation != before {
					self.frames.lock().unwrap().clear();
					self.output.send(Event::Wake);
				}
				tracing::trace!(
					bytes = buffer.bytes,
					frames = buffer.frames.len(),
					"buffered encoded video"
				);
				self.changed.notify_one();
				if ended {
					break;
				}
			}
			Ok::<_, anyhow::Error>(())
		};
		let decode = async {
			// The generation the decoder's output belongs to, while it holds any.
			let mut generation = None;
			// Presentation times fed to the decoder that it has not returned yet.
			let mut held = BTreeSet::new();
			// Decoded pictures waiting for room in the window's queue.
			let mut pending = VecDeque::new();
			loop {
				let (current, oldest, ended) = {
					let buffer = buffer.lock().unwrap();
					(buffer.generation, buffer.oldest(), buffer.ended)
				};
				if generation.is_some_and(|generation| generation != current) {
					// Whatever the codec still holds sits below the new floor, so it is
					// filtered on the way out and owes the window nothing.
					generation = None;
					held.clear();
					pending.clear();
				}
				let step = {
					let mut queue = self.frames.lock().unwrap();
					let presentation = self.presentation.lock().unwrap();
					let now = Instant::now().into_std();
					let mut placed = false;
					while !pending.is_empty() && vacancy(&queue, &presentation).is_none_or(|at| at <= now) {
						if queue.len() >= MAX_FRAMES {
							queue.pop_front();
						}
						let frame: moq_video::Frame = pending.pop_front().expect("checked by the loop");
						let index = queue.partition_point(|queued| queued.timestamp <= frame.timestamp);
						queue.insert(index, frame);
						placed = true;
					}
					if placed {
						self.output.send(Event::Wake);
					}
					if !pending.is_empty() {
						Step::Wait(vacancy(&queue, &presentation))
					} else if let Some(oldest) = oldest {
						// Every access unit up to the earliest picture still owed must be
						// decoded before that picture is due, however deep the stream
						// reorders or the codec holds pictures back.
						let owed = held.first().map_or(oldest, |held| oldest.min(*held));
						let at = presentation
							.due(owed)
							.and_then(|at| at.checked_sub(DECODE_AHEAD))
							.max(vacancy(&queue, &presentation));
						match at.filter(|at| *at > now) {
							Some(at) => Step::Wait(Some(at)),
							None => Step::Decode,
						}
					} else if !ended {
						Step::Wait(None)
					} else if generation.is_some() {
						Step::Flush
					} else {
						break;
					}
				};
				let frames = match step {
					Step::Wait(Some(at)) => {
						tokio::select! {
							_ = tokio::time::sleep_until(at.into()) => {},
							_ = self.changed.notified() => {},
						}
						continue;
					}
					Step::Wait(None) => {
						self.changed.notified().await;
						continue;
					}
					Step::Decode => {
						let frame = buffer
							.lock()
							.unwrap()
							.pop()
							.expect("no await since inspecting the buffer");
						generation = Some(current);
						held.insert(frame.timestamp);
						// Never race a codec call: cancelling Sink::decode poisons it. The
						// joined receiver continues observing arrivals during this await.
						decoder.decode(frame).await?
					}
					Step::Flush => {
						generation = None;
						held.clear();
						decoder.flush().await?
					}
				};
				// A codec returns display order, so a picture held before the newest
				// one it returned was dropped rather than delayed.
				if let Some(last) = frames.iter().map(|frame| frame.timestamp).max() {
					held.retain(|held| *held > last);
				}
				// A codec may return pictures it held across the keyframe that
				// resumed a skipped timeline. Those pictures no longer have a slot.
				let floor = buffer.lock().unwrap().floor;
				pending.extend(
					frames
						.into_iter()
						.filter(|frame| floor.is_none_or(|floor| frame.timestamp.as_micros() >= floor.as_micros())),
				);
			}
			Ok::<_, anyhow::Error>(())
		};
		tokio::try_join!(receive, decode)?;
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::super::{
		args::{Args, Delay},
		fake::Recorder,
		media::Media,
	};
	use super::*;
	use hang::moq_net;

	/// A codec whose reorder buffer holds `depth` pictures and bumps the
	/// earliest one out, the way a real decoder returns display order.
	struct Buffered {
		depth: usize,
		held: Vec<moq_video::Frame>,
		decoded: Arc<Mutex<Vec<(moq_net::Timestamp, Instant)>>>,
		flushed: Arc<Mutex<usize>>,
	}

	impl Default for Buffered {
		fn default() -> Self {
			Self::new(1)
		}
	}

	impl Buffered {
		fn new(depth: usize) -> Self {
			Self {
				depth,
				held: Vec::new(),
				decoded: Default::default(),
				flushed: Default::default(),
			}
		}
	}

	impl Decoder for Buffered {
		async fn decode(&mut self, frame: Frame) -> anyhow::Result<Vec<moq_video::Frame>> {
			self.decoded.lock().unwrap().push((frame.timestamp, Instant::now()));
			let surface = moq_video::Surface::rgba(&[128; 16 * 16 * 4], moq_video::Size::new(16, 16))?;
			self.held.push(moq_video::Frame::new(surface, frame.timestamp));
			self.held.sort_by_key(|frame| frame.timestamp);
			let bumped = self.held.len().saturating_sub(self.depth);
			Ok(self.held.drain(..bumped).collect())
		}

		async fn flush(&mut self) -> anyhow::Result<Vec<moq_video::Frame>> {
			*self.flushed.lock().unwrap() += 1;
			Ok(std::mem::take(&mut self.held))
		}
	}

	fn media(delay: Duration) -> Media<Recorder> {
		Media {
			origin: moq_tokio::origin::spawn().consume(),
			broadcast: "room".into(),
			args: Args {
				delay: Delay::Fixed(delay),
				catalog_format: None,
				select: Default::default(),
			},
			video: Default::default(),
			presentation: Arc::new(Mutex::new(Presentation::new(delay))),
			drained: Default::default(),
			output: Recorder::default(),
		}
	}

	fn playback(media: &Media<Recorder>, max_delay: Duration) -> Video<Recorder> {
		Video {
			presentation: media.presentation.clone(),
			frames: media.video.clone(),
			changed: media.drained.clone(),
			output: media.output.clone(),
			max_delay,
		}
	}

	fn frame(ms: u64, keyframe: bool) -> Frame {
		Frame {
			timestamp: moq_net::Timestamp::from_millis(ms).unwrap(),
			duration: None,
			keyframe,
			payload: bytes::Bytes::from_static(b"encoded"),
		}
	}

	async fn track(frames: impl IntoIterator<Item = Frame>, delay: Duration) -> Track {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscriber = track
			.consume()
			.subscribe(moq_net::track::Subscription::default().with_max_delay(delay))
			.await
			.unwrap();
		let format = moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data);
		let mut producer = moq_mux::container::Producer::new(
			track,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
		);
		for frame in frames {
			producer.write(frame).unwrap();
		}
		producer.finish().unwrap();
		moq_mux::container::Consumer::new(subscriber, format)
	}

	async fn burst(speaker: bool, paused_window: bool) {
		tokio::time::pause();
		let delay = Duration::from_secs(2);
		let media = media(delay);
		let start = Instant::now();
		let last = moq_net::Timestamp::from_millis(1_980).unwrap();
		if speaker {
			media
				.presentation
				.lock()
				.unwrap()
				.audio(Duration::ZERO, delay, start.into_std());
		}
		let decoder = Buffered::default();
		let decoded = decoder.decoded.clone();
		let flushed = decoder.flushed.clone();
		let track = track((0..=60).map(|index| frame(index * 33, index == 0)), delay).await;
		let task = tokio::spawn(playback(&media, delay).run(track, decoder));
		tokio::task::yield_now().await;
		let last_due = start
			+ delay + if speaker {
			Duration::from_millis(1_980)
		} else {
			Duration::ZERO
		};
		assert_eq!(media.presentation.lock().unwrap().due(last), Some(last_due.into_std()));
		assert!(media.video.lock().unwrap().len() <= MAX_FRAMES);
		if speaker {
			assert!(
				decoded.lock().unwrap().is_empty(),
				"the delay was buffered as raw pictures"
			);
		}
		let mut shown = Vec::new();
		while Instant::now() <= last_due + Duration::from_millis(10) {
			if (!paused_window || Instant::now() >= start + Duration::from_secs(1))
				&& let Some(timestamp) = media.output.present(&media)
			{
				shown.push((timestamp, Instant::now()));
			}
			assert!(media.video.lock().unwrap().len() <= MAX_FRAMES, "unbounded raw queue");
			tokio::time::advance(Duration::from_millis(1)).await;
			tokio::task::yield_now().await;
		}
		task.await.unwrap().unwrap();
		assert_eq!(*flushed.lock().unwrap(), 1, "decoder tail was not flushed exactly once");
		let &(timestamp, at) = shown.last().expect("presented video");
		assert_eq!(timestamp.as_micros(), last.as_micros(), "decoder tail was lost");
		assert!(
			at.max(last_due).duration_since(at.min(last_due)) <= Duration::from_millis(1),
			"live edge remained late: {at:?}, expected {last_due:?}"
		);
		assert!(shown.windows(2).all(|pair| pair[0].0 < pair[1].0));
		assert_eq!(decoded.lock().unwrap().len(), 61);
	}

	#[tokio::test]
	async fn video_only_burst_reaches_live_and_flushes_its_tail() {
		burst(false, false).await;
	}

	#[tokio::test]
	async fn a_stalled_presenter_does_not_stall_arrivals_or_decode() {
		burst(false, true).await;
	}

	#[tokio::test]
	async fn a_video_burst_follows_the_speakers_clock() {
		burst(true, false).await;
	}

	#[tokio::test]
	async fn reordered_decoder_output_is_presented_in_timestamp_order() {
		tokio::time::pause();
		let delay = Duration::from_millis(100);
		let media = media(delay);
		let track = track(
			[frame(0, true), frame(99, false), frame(33, false), frame(66, false)],
			delay,
		)
		.await;
		let task = tokio::spawn(playback(&media, delay).run(track, Buffered::default()));
		let mut shown = Vec::new();
		for _ in 0..110 {
			tokio::task::yield_now().await;
			if let Some(frame) = media.output.present(&media) {
				shown.push(frame.as_millis());
			}
			tokio::time::advance(Duration::from_millis(1)).await;
		}
		task.await.unwrap().unwrap();
		assert_eq!(shown, [0, 33, 66, 99], "reordered output was not presented in order");
	}
	#[tokio::test]
	async fn a_discontinuity_discards_old_pictures_and_restarts_the_delay() {
		tokio::time::pause();
		let delay = Duration::from_secs(2);
		let media = media(delay);
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscriber = track
			.consume()
			.subscribe(moq_net::track::Subscription::default().with_max_delay(delay))
			.await
			.unwrap();
		let mut producer = moq_mux::container::Producer::new(
			track,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
		);
		let track = moq_mux::container::Consumer::new(
			subscriber,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
		);
		producer.write(frame(0, true)).unwrap();
		let decoder = Buffered::default();
		let decoded = decoder.decoded.clone();
		let task = tokio::spawn(playback(&media, delay).run(track, decoder));
		tokio::task::yield_now().await;
		tokio::time::advance(delay).await;
		tokio::task::yield_now().await;
		assert_eq!(decoded.lock().unwrap().len(), 1, "old picture held inside decoder");
		producer.discontinuity().unwrap();
		producer.write(frame(500, true)).unwrap();
		producer.finish().unwrap();
		let restarted = Instant::now();
		tokio::task::yield_now().await;
		assert_eq!(
			media
				.presentation
				.lock()
				.unwrap()
				.due(moq_net::Timestamp::from_millis(500).unwrap()),
			Some((restarted + delay).into_std())
		);
		assert!(media.output.present(&media).is_none());
		tokio::time::advance(delay + Duration::from_millis(1)).await;
		task.await.unwrap().unwrap();
		let queued = media
			.video
			.lock()
			.unwrap()
			.iter()
			.map(|frame| frame.timestamp.as_millis())
			.collect::<Vec<_>>();
		assert_eq!(queued, [500], "old decoder output crossed the discontinuity");
		assert_eq!(media.output.present(&media).unwrap().as_millis(), 500);
	}

	/// Play one burst of 33 ms pictures, given by slot in decode order, through a
	/// codec holding `depth` pictures, and check the window shows every one on
	/// time.
	async fn schedule(order: &[u64], depth: usize) {
		tokio::time::pause();
		let delay = Duration::from_secs(2);
		let media = media(delay);
		let track = track(order.iter().map(|&slot| frame(slot * 33, slot == 0)), delay).await;
		let task = tokio::spawn(playback(&media, delay).run(track, Buffered::new(depth)));
		tokio::task::yield_now().await;
		let last = moq_net::Timestamp::from_millis(order.iter().max().unwrap() * 33).unwrap();
		let end = media.presentation.lock().unwrap().due(last).unwrap();
		let mut shown = Vec::new();
		while Instant::now().into_std() <= end + Duration::from_millis(10) {
			if let Some(timestamp) = media.output.present(&media) {
				shown.push((timestamp, Instant::now().into_std()));
			}
			tokio::time::advance(Duration::from_millis(1)).await;
			tokio::task::yield_now().await;
		}
		task.await.unwrap().unwrap();
		let mut expected = order.iter().map(|slot| (slot * 33) as u128).collect::<Vec<_>>();
		expected.sort();
		assert_eq!(
			shown
				.iter()
				.map(|(timestamp, _)| timestamp.as_millis())
				.collect::<Vec<_>>(),
			expected,
			"a picture was dropped"
		);
		let presentation = media.presentation.lock().unwrap();
		for (timestamp, at) in shown {
			let due = presentation.due(timestamp).unwrap();
			assert!(
				at.saturating_duration_since(due) <= Duration::from_millis(1),
				"{timestamp:?} was shown {:?} late",
				at - due
			);
		}
	}

	/// A hierarchical GOP: the reference coded second is presented 264 ms after
	/// the first picture, so the B-pictures coded after it need it decoded far
	/// more than 100 ms before its own deadline.
	#[tokio::test]
	async fn reordering_deeper_than_the_decode_lead_stays_on_time() {
		schedule(&[0, 8, 4, 2, 1, 3, 6, 5, 7], 3).await;
	}

	/// A codec holding more pictures than the window's queue returns a tail
	/// larger than the queue when it is flushed.
	#[tokio::test]
	async fn a_flush_larger_than_the_queue_keeps_its_tail() {
		schedule(&(0..10).collect::<Vec<_>>(), 5).await;
	}
}
