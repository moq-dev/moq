//! Receive encoded video independently of the paced decoder and window.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hang::moq_net;
use moq_mux::container::Frame;
use tokio::time::Instant;

use super::buffer::Buffer;
use super::output::Output;
use super::timeline::Presentation;
use super::window::Event;

/// A few surfaces, independent of the requested playout delay.
pub(super) const MAX_FRAMES: usize = 3;
const DECODE_AHEAD: Duration = Duration::from_millis(100);

type Track = moq_mux::container::Consumer<moq_mux::catalog::hang::Container>;

/// The window's shared state and the subscription's encoded retention budget.
pub(super) struct Video<O> {
	pub presentation: Arc<Mutex<Presentation>>,
	pub frames: Arc<Mutex<VecDeque<moq_video::Frame>>>,
	pub changed: Arc<tokio::sync::Notify>,
	pub output: O,
	pub max_age: Duration,
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
					buffer.push(frame, self.max_age);
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
			let mut generation = None;
			loop {
				let next = {
					let buffer = buffer.lock().unwrap();
					buffer.frames.front().map(|frame| frame.timestamp)
				};
				let Some(timestamp) = next else {
					if buffer.lock().unwrap().ended {
						break;
					}
					self.changed.notified().await;
					continue;
				};
				let at = {
					let frames = self.frames.lock().unwrap();
					let presentation = self.presentation.lock().unwrap();
					let mut at = presentation.due(timestamp).and_then(|at| at.checked_sub(DECODE_AHEAD));
					// A full window may lag, but a future picture still deserves its
					// slot. Once due, evict it rather than blocking the live reader.
					if frames.len() >= MAX_FRAMES {
						at = at.max(frames.front().and_then(|frame| presentation.due(frame.timestamp)));
					}
					at
				};
				if let Some(at) = at.filter(|at| *at > Instant::now().into_std()) {
					tokio::select! {
						_ = tokio::time::sleep_until(at.into()) => {},
						_ = self.changed.notified() => {},
					}
					continue;
				}
				let (frame, current) = {
					let mut buffer = buffer.lock().unwrap();
					(
						buffer.pop().expect("no await since inspecting the front"),
						buffer.generation,
					)
				};
				// Never race a codec call: cancelling Sink::decode poisons it. The
				// joined receiver continues observing arrivals during this await.
				let frames = decoder.decode(frame).await?;
				generation = Some(current);
				if buffer.lock().unwrap().generation == current {
					self.decoded(frames, buffer.lock().unwrap().floor);
				}
			}
			let tail = decoder.flush().await?;
			if generation == Some(buffer.lock().unwrap().generation) {
				self.decoded(tail, buffer.lock().unwrap().floor);
			}
			Ok::<_, anyhow::Error>(())
		};
		tokio::try_join!(receive, decode)?;
		Ok(())
	}

	fn decoded(&self, frames: Vec<moq_video::Frame>, floor: Option<moq_net::Timestamp>) {
		let mut queue = self.frames.lock().unwrap();
		for frame in frames {
			// A codec may return pictures it held across the keyframe that
			// resumed a skipped timeline. Those pictures no longer have a slot.
			if floor.is_some_and(|floor| frame.timestamp.as_micros() < floor.as_micros()) {
				continue;
			}
			let index = queue.partition_point(|queued| queued.timestamp <= frame.timestamp);
			queue.insert(index, frame);
			while queue.len() > MAX_FRAMES {
				queue.pop_front();
			}
		}
		self.output.send(Event::Wake);
	}
}

#[cfg(test)]
mod tests {
	use super::super::{args::Args, fake::Recorder, media::Media};
	use super::*;

	#[derive(Default)]
	struct Buffered {
		pending: Option<moq_video::Frame>,
		decoded: Arc<Mutex<Vec<(moq_net::Timestamp, Instant)>>>,
		flushed: Arc<Mutex<usize>>,
	}

	impl Decoder for Buffered {
		async fn decode(&mut self, frame: Frame) -> anyhow::Result<Vec<moq_video::Frame>> {
			self.decoded.lock().unwrap().push((frame.timestamp, Instant::now()));
			let surface = moq_video::Surface::rgba(&[128; 16 * 16 * 4], moq_video::Size::new(16, 16))?;
			Ok(self
				.pending
				.replace(moq_video::Frame::new(surface, frame.timestamp))
				.into_iter()
				.collect())
		}

		async fn flush(&mut self) -> anyhow::Result<Vec<moq_video::Frame>> {
			*self.flushed.lock().unwrap() += 1;
			Ok(self.pending.take().into_iter().collect())
		}
	}

	fn media(delay: Duration) -> Media<Recorder> {
		Media {
			origin: moq_tokio::origin::spawn().consume(),
			broadcast: "room".into(),
			args: Args {
				delay: delay.into(),
				catalog_format: None,
				select: Default::default(),
			},
			video: Default::default(),
			presentation: Arc::new(Mutex::new(Presentation::new(delay))),
			drained: Default::default(),
			output: Recorder::default(),
		}
	}

	fn playback(media: &Media<Recorder>, max_age: Duration) -> Video<Recorder> {
		Video {
			presentation: media.presentation.clone(),
			frames: media.video.clone(),
			changed: media.drained.clone(),
			output: media.output.clone(),
			max_age,
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
			.subscribe(moq_net::track::Subscription::default().with_max_age(delay))
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
		assert_eq!(
			shown,
			[33, 66, 99],
			"the bounded queue evicts the oldest, then presents reordered output"
		);
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
			.subscribe(moq_net::track::Subscription::default().with_max_age(delay))
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
}
