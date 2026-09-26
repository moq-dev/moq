//! A deviceless [`Output`] for tests: it records what the speaker would have
//! played and when, on tokio's clock, so a paused runtime measures playback
//! without waiting on it.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use moq_audio::playback::Input;
use tokio::time::Instant;

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
	sinks: usize,
	events: Vec<Event>,
}

impl Recorder {
	/// Everything the speaker played, in write order.
	pub fn played(&self) -> Vec<Played> {
		self.state.lock().unwrap().played.clone()
	}

	/// Take the events the window has been sent so far.
	pub fn events(&self) -> Vec<Event> {
		std::mem::take(&mut self.state.lock().unwrap().events)
	}
}

impl Output for Recorder {
	type Speaker = Self;

	async fn speaker(&self) -> anyhow::Result<Self> {
		Ok(self.clone())
	}

	fn send(&self, event: Event) {
		self.state.lock().unwrap().events.push(event);
	}
}

impl Speaker for Recorder {
	type Sink = FakeSink;

	fn sink(&self, input: Input) -> anyhow::Result<FakeSink> {
		anyhow::ensure!(input.format == moq_audio::Format::F32, "the fake sink only reads f32");
		let id = {
			let mut state = self.state.lock().unwrap();
			state.sinks += 1;
			state.sinks - 1
		};
		Ok(FakeSink {
			state: self.state.clone(),
			id,
			stride: input.layout.channels() as usize * size_of::<f32>(),
			sample_rate: input.sample_rate,
			latency: input.latency,
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
	/// When everything written so far has played.
	end: Instant,
}

impl Sink for FakeSink {
	fn write(&mut self, samples: &[u8]) -> anyhow::Result<()> {
		anyhow::ensure!(samples.len().is_multiple_of(self.stride), "misaligned write");
		let now = Instant::now();
		// The real ring pads an underflow back up to its latency in silence once it
		// runs down to a quarter of it, rather than playing each late write the
		// instant it lands.
		if self.buffered() <= self.latency / 4 {
			self.end = now + self.latency;
		}

		let frames = (samples.len() / self.stride) as u64;
		let duration = Duration::from_nanos(frames * 1_000_000_000 / self.sample_rate as u64);
		let sample = samples
			.first_chunk::<4>()
			.map(|bytes| f32::from_le_bytes(*bytes))
			.unwrap_or_default();
		self.state.lock().unwrap().played.push(Played {
			sink: self.id,
			sample,
			from: self.end,
			to: self.end + duration,
		});
		self.end += duration;
		Ok(())
	}

	fn buffered(&self) -> Duration {
		self.end.saturating_duration_since(Instant::now())
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
