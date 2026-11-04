//! A deviceless [`Output`] for tests: it records what the speaker would have
//! played and when, on the ambient clock, so a paused runtime measures playback
//! without waiting on it.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use kio::clock::Instant;
use moq_audio::playback::Input;

use super::output::{Output, Sink, Speaker};
use super::window::Event;

/// One write, as the speaker plays it.
#[derive(Clone, Copy, Debug)]
pub(super) struct Played {
	/// Which sink it went through, in the order they were opened.
	pub sink: usize,
	/// Its first sample, which tells a test's renditions apart.
	pub sample: f32,
	/// When it starts sounding.
	pub from: Instant,
	/// When it stops sounding, which is earlier than its length if the sink was
	/// dropped first.
	pub to: Instant,
}

/// The window and speaker in one, recording instead of presenting.
#[derive(Clone, Default)]
pub(super) struct Recorder {
	state: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
	played: Vec<Played>,
	latencies: Vec<Duration>,
	speakers: usize,
	sinks: usize,
	events: Vec<Event>,
}

impl Recorder {
	/// How many times playback opened the speaker.
	pub fn speakers(&self) -> usize {
		self.state.lock().unwrap().speakers
	}

	/// Present the newest due frame, just as the window does on a redraw.
	pub fn present(&self, media: &super::media::Media<Self>) -> Option<hang::moq_net::Timestamp> {
		self.present_frame(media).map(|frame| frame.timestamp)
	}

	/// [`present`](Self::present), returning the picture itself.
	pub fn present_frame(&self, media: &super::media::Media<Self>) -> Option<moq_video::Frame> {
		let mut video = media.video.lock().unwrap();
		let presentation = media.presentation.consume();
		let mut shown = None;
		if let Some(index) = presentation.present(video.iter().map(|frame| frame.timestamp), kio::clock::now()) {
			for _ in 0..=index {
				shown = video.pop_front();
			}
		}
		if shown.is_some() {
			media.drained.notify_one();
		}
		shown
	}

	/// Everything the speaker played, in write order.
	pub fn played(&self) -> Vec<Played> {
		self.state.lock().unwrap().played.clone()
	}

	pub fn latencies(&self) -> Vec<Duration> {
		self.state.lock().unwrap().latencies.clone()
	}

	/// Take the events the window has been sent so far.
	pub fn events(&self) -> Vec<Event> {
		std::mem::take(&mut self.state.lock().unwrap().events)
	}
}

impl Output for Recorder {
	type Speaker = Self;

	async fn speaker(&self) -> anyhow::Result<Self> {
		self.state.lock().unwrap().speakers += 1;
		Ok(self.clone())
	}

	fn send(&self, event: Event) {
		self.state.lock().unwrap().events.push(event);
	}

	async fn render(&self) -> moq_video::Output {
		moq_video::Output::Cpu
	}
}

impl Speaker for Recorder {
	type Sink = FakeSink;

	fn sink(&self, input: Input) -> anyhow::Result<FakeSink> {
		anyhow::ensure!(input.format == moq_audio::Format::F32, "the fake sink only reads f32");
		let id = {
			let mut state = self.state.lock().unwrap();
			state.sinks += 1;
			state.latencies.push(input.latency);
			state.sinks - 1
		};
		Ok(FakeSink {
			state: self.state.clone(),
			id,
			stride: input.layout.channels() as usize * size_of::<f32>(),
			sample_rate: input.sample_rate,
			latency: input.latency,
			refill: false,
			prime: Some(input.latency),
			pending: Vec::new(),
			queued: Duration::ZERO,
			// A real sink's ring starts out holding its latency in silence, which is
			// why the first sample written sounds that much later.
			end: Instant::now() + input.latency,
		})
	}
}

/// A speaker draining in real time from the moment the sink opens.
pub(super) struct FakeSink {
	state: Arc<Mutex<State>>,
	id: usize,
	stride: usize,
	sample_rate: u32,
	latency: Duration,
	refill: bool,
	prime: Option<Duration>,
	pending: Vec<(f32, Duration)>,
	queued: Duration,
	/// When everything written so far has played.
	end: Instant,
}

impl Sink for FakeSink {
	fn set_latency(&mut self, latency: Duration) -> anyhow::Result<()> {
		self.latency = latency;
		Ok(())
	}

	async fn finish(mut self) {
		// A finite short tail drains even when it cannot refill the target.
		if self.prime.take().is_some() {
			self.end = Instant::now();
		}
		if self.refill {
			self.resume();
		}
		kio::clock::sleep_until(self.end).await;
	}

	fn write(&mut self, samples: &[u8]) -> anyhow::Result<moq_audio::playback::Write> {
		anyhow::ensure!(samples.len().is_multiple_of(self.stride), "misaligned write");
		let now = Instant::now();
		// DSP and clock drift are tested in moq-audio's real ring. This fake
		// models its emergency recovery: no synthetic padding, and no PCM
		// heard until fresh writes refill the target after a genuine gap.
		if let Some(prime) = self.prime.take() {
			self.end = now + prime;
		} else if self.end < now {
			self.refill = true;
		}

		let frames = (samples.len() / self.stride) as u64;
		let duration = Duration::from_nanos(frames * 1_000_000_000 / self.sample_rate as u64);
		let sample = samples
			.first_chunk::<4>()
			.map(|bytes| f32::from_le_bytes(*bytes))
			.unwrap_or_default();
		if self.refill {
			self.pending.push((sample, duration));
			self.queued += duration;
			if self.queued >= self.latency {
				self.resume();
			}
		} else {
			self.play(sample, duration);
		}
		let mut written = moq_audio::playback::Write::default();
		written.accepted_sample_frames = frames as usize;
		Ok(written)
	}

	fn buffered(&self) -> Duration {
		if let Some(prime) = self.prime {
			prime
		} else if self.refill {
			self.queued
		} else {
			self.end.saturating_duration_since(Instant::now())
		}
	}
}

impl FakeSink {
	fn play(&mut self, sample: f32, duration: Duration) {
		self.state.lock().unwrap().played.push(Played {
			sink: self.id,
			sample,
			from: self.end,
			to: self.end + duration,
		});
		self.end += duration;
	}

	fn resume(&mut self) {
		self.end = Instant::now();
		let pending = std::mem::take(&mut self.pending);
		for (sample, duration) in pending {
			self.play(sample, duration);
		}
		self.queued = Duration::ZERO;
		self.refill = false;
	}
}

impl Drop for FakeSink {
	/// A dropped sink leaves the mix, cutting off whatever it still held.
	fn drop(&mut self) {
		let now = Instant::now();
		let mut state = self.state.lock().unwrap();
		state.played.retain_mut(|played| {
			if played.sink == self.id {
				played.to = played.to.min(now);
			}
			played.from < played.to
		});
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn underrun_waits_for_received_audio_at_the_new_target() {
		tokio::time::pause();
		let recorder = Recorder::default();
		let mut sink = recorder.sink(Input::default()).unwrap();
		let pcm = [0u8; 960 * 2 * 4];
		assert_eq!(sink.write(&pcm).unwrap().accepted_sample_frames, 960);
		tokio::time::advance(Duration::from_millis(100)).await;
		sink.set_latency(Duration::from_millis(80)).unwrap();
		let heard = recorder.played().len();
		for frames in 1..4 {
			assert_eq!(sink.write(&pcm).unwrap().accepted_sample_frames, 960);
			assert_eq!(sink.buffered(), Duration::from_millis(frames * 20));
			assert_eq!(recorder.played().len(), heard);
		}
		assert_eq!(sink.write(&pcm).unwrap().accepted_sample_frames, 960);
		let played = recorder.played();
		assert_eq!(played.len(), heard + 4);
		assert_eq!(played[heard].from, Instant::now());
		assert_eq!(sink.buffered(), Duration::from_millis(80));
	}

	#[tokio::test]
	async fn finish_releases_a_short_refill_without_padding() {
		tokio::time::pause();
		let recorder = Recorder::default();
		let mut sink = recorder.sink(Input::default()).unwrap();
		assert_eq!(sink.write(&[0; 960 * 2 * 4]).unwrap().accepted_sample_frames, 960);
		tokio::time::advance(Duration::from_millis(100)).await;
		assert_eq!(sink.write(&[0; 960 * 2 * 4]).unwrap().accepted_sample_frames, 960);
		assert_eq!(recorder.played().len(), 1);
		let now = Instant::now();
		sink.finish().await;
		let played = recorder.played();
		assert_eq!(played[1].from, now);
		assert_eq!(played[1].to, now + Duration::from_millis(20));
		assert!(Instant::now() >= played[1].to);
	}
}
