//! Where the media task's output goes: the window and the speaker in `moq play`,
//! a recorder in tests.
//!
//! This seam is what lets the task logic in `media.rs` run without a device.

use std::time::Duration;

use kio::clock::Instant;

use moq_audio::playback::Input;

use super::window::Event;

/// The window the media task wakes, and the speaker it opens.
pub(super) trait Output: Clone + Send + Sync + 'static {
	type Speaker: Speaker;

	/// Open the speaker. Every sink opened from one mixes on the same device stream.
	fn speaker(&self) -> impl Future<Output = anyhow::Result<Self::Speaker>> + Send;

	/// Tell the window something changed. Dropped if the window is gone.
	fn send(&self, event: Event);

	/// What to decode video as for the window's renderer, once its device is open.
	fn render(&self) -> impl Future<Output = moq_video::Output> + Send;
}

/// An open speaker, closed once it and every sink from it are dropped.
pub(super) trait Speaker: Clone + Send + Sync + 'static {
	type Sink: Sink;

	/// Start a stream of PCM that holds `input.latency` ahead of the speaker.
	fn sink(&self, input: Input) -> anyhow::Result<Self::Sink>;
}

/// One stream of PCM on its way to the speaker, cut off when dropped.
pub(super) trait Sink: Send + Sync + 'static {
	/// Queue samples behind what is already buffered.
	fn write(&mut self, samples: &[u8]) -> anyhow::Result<moq_audio::playback::Write>;

	/// Update the callback's playout target without replacing the sink.
	fn set_latency(&mut self, latency: Duration) -> anyhow::Result<()>;

	/// The audio written so far is due to finish sounding at `at`; trailing that
	/// by more than `stretch` is skipped, and by as much as the clock's reference
	/// `jumped` earlier since the last call. A zero `stretch` turns the time
	/// stretch off.
	fn due(&mut self, at: Instant, stretch: Duration, jumped: Duration);

	/// How much audio is queued ahead of the speaker.
	fn buffered(&self) -> Duration;

	/// How long the device takes to sound what it reads, once it has reported it.
	fn output_latency(&self) -> Option<Duration>;

	/// Play every queued sample before releasing the sink.
	fn finish(self) -> impl Future<Output = ()> + Send;

	/// The audio written so far is followed by a declared silence.
	fn end(&mut self);

	/// The next write starts the next utterance after a declared silence.
	fn next(&mut self);
}

impl Speaker for moq_audio::playback::Engine {
	type Sink = moq_audio::playback::Sink;

	fn sink(&self, input: Input) -> anyhow::Result<Self::Sink> {
		Ok(moq_audio::playback::Engine::sink(self, input)?)
	}
}

impl Sink for moq_audio::playback::Sink {
	fn set_latency(&mut self, latency: Duration) -> anyhow::Result<()> {
		Ok(moq_audio::playback::Sink::set_latency(self, latency)?)
	}

	fn due(&mut self, at: Instant, stretch: Duration, jumped: Duration) {
		moq_audio::playback::Sink::due(self, at, stretch, jumped);
	}

	fn write(&mut self, samples: &[u8]) -> anyhow::Result<moq_audio::playback::Write> {
		Ok(moq_audio::playback::Sink::write(self, samples)?)
	}

	fn finish(self) -> impl Future<Output = ()> + Send {
		moq_audio::playback::Sink::finish(self)
	}

	fn buffered(&self) -> Duration {
		moq_audio::playback::Sink::buffered(self)
	}

	fn output_latency(&self) -> Option<Duration> {
		moq_audio::playback::Sink::output_latency(self)
	}

	fn end(&mut self) {
		moq_audio::playback::Sink::end(self);
	}

	fn next(&mut self) {
		moq_audio::playback::Sink::next(self);
	}
}
