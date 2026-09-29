//! Where the media task's output goes: the window and the speaker in `moq play`,
//! a recorder in tests.
//!
//! This seam is what lets the task logic in `media.rs` run without a device.

use std::time::Duration;

use moq_audio::playback::Input;

use super::window::Event;

/// The window the media task wakes, and the speaker it opens.
pub(super) trait Output: Clone + Send + Sync + 'static {
	type Speaker: Speaker;

	/// Open the speaker. Every sink opened from one mixes on the same device stream.
	fn speaker(&self) -> impl Future<Output = anyhow::Result<Self::Speaker>> + Send;

	/// Tell the window something changed. Dropped if the window is gone.
	fn send(&self, event: Event);
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
	fn write(&mut self, samples: &[u8]) -> anyhow::Result<()>;

	/// How much audio is queued ahead of the speaker.
	fn buffered(&self) -> Duration;
}

impl Speaker for moq_audio::playback::Engine {
	type Sink = moq_audio::playback::Sink;

	fn sink(&self, input: Input) -> anyhow::Result<Self::Sink> {
		Ok(moq_audio::playback::Engine::sink(self, input)?)
	}
}

impl Sink for moq_audio::playback::Sink {
	fn write(&mut self, samples: &[u8]) -> anyhow::Result<()> {
		// Playback drops stay on the live timeline; retrying them would add
		// latency, and the sink already reports them in its logs.
		let _ = moq_audio::playback::Sink::write(self, samples)?;
		Ok(())
	}

	fn buffered(&self) -> Duration {
		moq_audio::playback::Sink::buffered(self)
	}
}
