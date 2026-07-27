//! Play decoded PCM out a speaker, via [`cpal`] (CoreAudio / WASAPI / ALSA).
//!
//! The playback counterpart to [`capture`](crate::capture), and where a
//! [`decode::Consumer`](crate::decode::Consumer) ends up:
//!
//! ```no_run
//! # async fn example(mut audio: moq_audio::decode::Consumer) -> Result<(), moq_audio::Error> {
//! use moq_audio::playback;
//!
//! let engine = playback::Engine::open(playback::Config::default()).await?;
//! let mut sink = engine.sink(playback::Input {
//!     sample_rate: audio.sample_rate(),
//!     channels: audio.channels(),
//!     ..Default::default()
//! })?;
//!
//! while let Some(frame) = audio.read().await? {
//!     sink.write(&frame.data)?;
//! }
//! # Ok(())
//! # }
//! ```
//!
//! [`Engine`] owns the output device and [`Sink`] is one stream playing through
//! it. That split is deliberate: a call with several participants mixes into one
//! device stream rather than opening one per speaker, which is what the device
//! wants and what echo cancellation will need a single mixed reference from.
//!
//! The device is not the caller's problem. Opening it, renegotiating its format,
//! resampling to its rate, and reopening it after it disappears all happen on a
//! driver thread behind [`Engine`]; a [`Sink`] keeps taking writes throughout,
//! and its samples land wherever the device currently is.

mod device;
mod driver;
mod mixer;
mod sink;

use std::sync::Arc;
use std::sync::mpsc::Sender;

pub use device::{Device, devices};
pub use sink::{Control, Input, Sink};

use crate::Error;

/// Playback configuration.
///
/// `#[non_exhaustive]`: construct via [`Config::default`] and set fields, so new
/// options can be added without breaking callers.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {
	/// Output device, by the id [`devices`] reports. `None` opens the system
	/// default.
	pub device: Option<String>,
}

/// An open output device, mixing up to 64 [`Sink`]s into it.
///
/// Cheap to clone; every clone drives the same device. The device closes once
/// the last clone and the last [`Sink`] are dropped.
#[derive(Clone)]
pub struct Engine {
	shared: Arc<driver::Shared>,
	handle: Arc<Handle>,
}

/// The driver thread's lifeline, shared by every [`Engine`] clone and every
/// [`Sink`]. Dropping the last one shuts the thread down and closes the device,
/// which is why a `Sink` outliving its `Engine` still plays.
pub(crate) struct Handle {
	commands: Sender<driver::Command>,
}

impl Handle {
	/// Nudge the driver to collect retired sinks and retry anything the mixer's
	/// command queue was too full to take.
	pub(crate) fn wake(&self) {
		let _ = self.commands.send(driver::Command::Sync);
	}
}

impl Drop for Handle {
	fn drop(&mut self) {
		let _ = self.commands.send(driver::Command::Shutdown);
	}
}

impl Engine {
	/// Open the output device described by `config`.
	///
	/// Fails if there is no such device, or none at all. Later failures are the
	/// driver's problem, not the caller's: it reopens the device with backoff
	/// and existing sinks pick up where they left off.
	pub async fn open(config: Config) -> Result<Self, Error> {
		let (commands, requests) = std::sync::mpsc::channel();
		let (opened, ready) = tokio::sync::oneshot::channel();

		let shared = Arc::new(driver::Shared::default());
		let engine = Self {
			handle: Arc::new(Handle {
				commands: commands.clone(),
			}),
			shared: shared.clone(),
		};

		// cpal streams are `!Send`, so the device has to live on the thread that
		// opens it.
		std::thread::Builder::new()
			.name("moq-audio-playback".into())
			.spawn(move || driver::run(requests, commands, shared, config.device, opened))
			.map_err(|err| Error::Playback(format!("cannot spawn the playback thread: {err}")))?;

		ready
			.await
			.map_err(|_| Error::Playback("the playback thread stopped".into()))??;

		Ok(engine)
	}

	/// Add a stream to the mix, taking PCM in the layout `input` describes.
	///
	/// Independent of the device: several sinks can play at different rates and
	/// channel counts, and each is resampled on its way to the mix. One device
	/// mixes up to 64 of them, past which this returns an error rather than
	/// handing back a sink that plays nothing.
	pub fn sink(&self, input: Input) -> Result<Sink, Error> {
		let sink = self
			.shared
			.add(|id, rate| sink::new(id, rate, input, self.shared.clone(), self.handle.clone()))?;

		// Covers the case where the mixer's command queue was momentarily full,
		// so a sink is never left silently unmixed.
		self.handle.wake();
		Ok(sink)
	}

	/// Move playback to the device `config` names, or back to the system default
	/// when [`Config::device`] is `None`.
	///
	/// Takes the same [`Config`] as [`open`](Self::open) rather than a bare
	/// device id, so a future playback option applies to a switch without
	/// changing this signature.
	///
	/// Sinks survive the move: they keep the same handles and resample to the
	/// new device's rate. Returns an error, and stays on no device, if the
	/// requested one can't be opened.
	pub async fn switch(&self, config: Config) -> Result<(), Error> {
		let (reply, response) = tokio::sync::oneshot::channel();
		self.handle
			.commands
			.send(driver::Command::Switch {
				device: config.device,
				reply,
			})
			.map_err(|_| Error::Playback("the playback thread stopped".into()))?;

		response
			.await
			.map_err(|_| Error::Playback("the playback thread stopped".into()))?
	}
}
