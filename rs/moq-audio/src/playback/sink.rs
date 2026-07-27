//! [`Sink`]: one stream of PCM on its way to the speaker.

use std::borrow::Cow;
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fixed_resample::{PushStatus, ResamplingChannelConfig, ResamplingCons, ResamplingProd, resampling_channel};

use super::driver::Shared;
use super::mixer::{self, BUS_CHANNELS, Gain};
use crate::resample::remix;
use crate::{Error, Format};

/// Audio buffered between [`Sink::write`] and the speaker.
///
/// This is the price of surviving jitter: the device pulls on a fixed clock, so
/// a late write is a dropout. 50 ms rides out a stalled network read without
/// being audible as delay.
const LATENCY: f64 = 0.05;

/// Ceiling on that buffer. A writer that runs ahead (a decoder catching up after
/// a pause) parks samples here instead of losing them.
const CAPACITY: f64 = 3.0;

/// The PCM layout a [`Sink`] accepts.
///
/// The playback counterpart to [`encode::Input`](crate::encode::Input): it
/// describes the buffers you hand in, not the device, which is free to run at
/// its own rate and channel count.
#[derive(Clone, Debug)]
pub struct Input {
	/// How samples are packed in each buffer.
	pub format: Format,
	/// Samples per second per channel. Resampled to the device rate if they
	/// differ.
	pub sample_rate: u32,
	/// Channels per frame. Mono is duplicated and stereo passed through; more
	/// than two is rejected, since downmixing is not implemented.
	pub channels: u32,
}

impl Default for Input {
	fn default() -> Self {
		Self {
			format: Format::F32,
			sample_rate: 48_000,
			channels: 2,
		}
	}
}

impl Input {
	fn validate(&self) -> Result<(), Error> {
		if self.sample_rate == 0 {
			return Err(Error::Unsupported("sample rate must be > 0".into()));
		}
		if self.channels == 0 || self.channels > BUS_CHANNELS as u32 {
			return Err(Error::Unsupported(format!(
				"playback accepts mono or stereo input (got {} channels)",
				self.channels
			)));
		}
		Ok(())
	}
}

/// One stream of PCM being played, mixed with every other sink on the device.
///
/// Write decoded samples with [`write`](Self::write) and drop the sink to stop.
/// Writes are cheap and never block on the device: they hand samples to a ring
/// buffer that the audio thread drains on its own clock, resampling to the
/// device rate on the way.
pub struct Sink {
	id: u64,
	input: Input,
	prod: Arc<Mutex<ResamplingProd<f32>>>,
	control: Control,
	/// Whether the last write overflowed, so a writer that stays ahead of the
	/// device logs once rather than on every write.
	overflowing: bool,
	shared: Arc<Shared>,
	/// Keeps the driver thread running while this sink is alive, so dropping the
	/// [`Engine`](super::Engine) that made it doesn't cut playback short.
	engine: Arc<super::Handle>,
}

impl Sink {
	/// Play `samples`, in the layout this sink was built with.
	///
	/// Samples play back to back in write order. Nothing is scheduled against a
	/// clock here, so pace writes with [`buffered`](Self::buffered): it reports
	/// how far ahead of the speaker you are, which is the anchor an A/V sync
	/// clock steers video by.
	///
	/// Writing faster than the device consumes eventually overflows and drops
	/// the excess; writing slower underruns and plays silence. Both are logged
	/// and neither is an error, since a live stream recovers on the next write.
	pub fn write(&mut self, samples: &[u8]) -> Result<(), Error> {
		let pcm = self.input.format.as_interleaved_f32(samples, self.input.channels)?;
		let pcm = match self.input.channels as usize {
			BUS_CHANNELS => pcm,
			channels => Cow::Owned(remix(&pcm, channels as u32, BUS_CHANNELS as u32)?),
		};

		match self.prod.lock().unwrap().push_interleaved(&pcm) {
			// OutputNotReady means the device has not read yet, so these samples
			// are dropped rather than queued to play late.
			PushStatus::Ok | PushStatus::OutputNotReady => self.overflowing = false,
			PushStatus::OverflowOccurred { num_frames_pushed } => {
				// Once per spell, not once per write: a writer that stays ahead
				// of the device would otherwise warn every frame for as long as
				// it lasts.
				if !self.overflowing {
					tracing::warn!(num_frames_pushed, "audio playback overflow, dropping samples");
					self.overflowing = true;
				}
			}
			PushStatus::UnderflowCorrected { num_zero_frames_pushed } => {
				self.overflowing = false;
				tracing::debug!(num_zero_frames_pushed, "audio playback underflow, padded with silence");
			}
		}

		Ok(())
	}

	/// How much audio is queued between the last [`write`](Self::write) and the
	/// speaker.
	///
	/// The pacing signal for A/V sync: the sample playing right now was written
	/// at roughly `last_timestamp - buffered()`, so a video clock can steer
	/// against it. It settles near 50 ms once playback is running, climbs when
	/// the writer runs ahead, and falls toward zero when it falls behind.
	pub fn buffered(&self) -> Duration {
		Duration::from_secs_f64(self.prod.lock().unwrap().occupied_seconds().max(0.0))
	}

	/// The PCM layout this sink was built with.
	pub fn input(&self) -> &Input {
		&self.input
	}

	/// A handle for adjusting this sink from another thread, e.g. a volume
	/// slider on a UI thread while a decode task does the writing.
	pub fn control(&self) -> Control {
		self.control.clone()
	}

	/// Set the playback volume, `0.0` to `1.0`. See [`Control::set_volume`].
	pub fn set_volume(&self, volume: f32) {
		self.control.set_volume(volume);
	}

	/// The volume last set. Defaults to `1.0`.
	pub fn volume(&self) -> f32 {
		self.control.volume()
	}

	/// The loudest sample this sink contributed since the last call. See
	/// [`Control::peak`].
	pub fn peak(&self) -> f32 {
		self.control.peak()
	}
}

impl Drop for Sink {
	fn drop(&mut self) {
		self.shared.remove(self.id);
		// The mixer hands the retired sink back rather than dropping it on the
		// audio thread, so somebody has to come collect it.
		self.engine.wake();
	}
}

/// A cheap, clonable handle to one [`Sink`]'s volume and level.
///
/// Everything here is a lone atomic, so it is safe to poll from a UI frame loop
/// while the audio thread is mixing.
#[derive(Clone, Debug)]
pub struct Control {
	gain: Arc<Gain>,
}

impl Control {
	/// Set the playback volume, `0.0` (silent) to `1.0` (unchanged), clamped. A
	/// non-finite volume is ignored rather than clamped, since NaN would ride
	/// the ramp into every sample this sink contributes.
	///
	/// The change ramps in over a few milliseconds rather than landing on one
	/// sample, so muting mid-stream does not click. A muted sink keeps
	/// consuming its input: this is live audio, so it stays on the timeline
	/// instead of queueing up and jumping ahead when it unmutes.
	pub fn set_volume(&self, volume: f32) {
		self.gain.set_volume(volume);
	}

	/// The volume last set. Defaults to `1.0`.
	pub fn volume(&self) -> f32 {
		self.gain.volume()
	}

	/// The loudest sample this sink contributed since the previous call, `0.0`
	/// to `1.0`, after volume.
	///
	/// Reading resets it, so poll on the interval you want to display and do any
	/// smoothing or dB conversion yourself.
	pub fn peak(&self) -> f32 {
		self.gain.peak()
	}
}

/// A sink as the driver sees it: enough to rebuild its channel when the device
/// changes underneath it.
pub(super) struct Registration {
	pub(super) id: u64,
	/// The caller's rate, which is the input side of the channel.
	rate: u32,
	prod: Arc<Mutex<ResamplingProd<f32>>>,
	gain: Arc<Gain>,
	/// The consumer waiting to be handed to a mixer. Taken once it is attached,
	/// and refilled by [`rebuild`](Self::rebuild).
	pending: Option<ResamplingCons<f32>>,
}

impl Registration {
	/// Whether the mixer has taken this sink's consumer.
	pub(super) fn attached(&self) -> bool {
		self.pending.is_none()
	}

	/// Hand the consumer to a running mixer, if it hasn't been already.
	pub(super) fn attach(&mut self, mixer: &SyncSender<mixer::Command>) {
		let Some(cons) = self.pending.take() else { return };

		let command = mixer::Command::Add {
			id: self.id,
			cons,
			gain: self.gain.clone(),
		};

		if let Err(err) = mixer.try_send(command) {
			// The mixer is backed up or gone. Keep the consumer so the next
			// attach retries rather than leaving a silent sink forever.
			let (TrySendError::Full(rejected) | TrySendError::Disconnected(rejected)) = err;
			if let mixer::Command::Add { cons, .. } = rejected {
				self.pending = Some(cons);
			}
		}
	}

	/// Re-create the channel for a device now running at `rate`, swapping the
	/// producer the caller's [`Sink`] writes into.
	pub(super) fn rebuild(&mut self, rate: u32) {
		let (prod, cons) = channel(self.rate, rate);
		*self.prod.lock().unwrap() = prod;
		self.pending = Some(cons);
	}
}

/// Build a sink and its registration. The device may not be open yet, in which
/// case `rate` is a placeholder the driver replaces on the next rebuild.
pub(super) fn new(
	id: u64,
	rate: u32,
	input: Input,
	shared: Arc<Shared>,
	engine: Arc<super::Handle>,
) -> Result<(Sink, Registration), Error> {
	input.validate()?;

	let (prod, cons) = channel(input.sample_rate, rate);
	let prod = Arc::new(Mutex::new(prod));
	let gain = Arc::new(Gain::new());

	let sink = Sink {
		id,
		input,
		prod: prod.clone(),
		control: Control { gain: gain.clone() },
		overflowing: false,
		shared,
		engine,
	};

	let registration = Registration {
		id,
		rate: sink.input.sample_rate,
		prod,
		gain,
		pending: Some(cons),
	};

	Ok((sink, registration))
}

/// The ring buffer between a writer and the audio thread, resampling the
/// caller's rate to the device's.
fn channel(from: u32, to: u32) -> (ResamplingProd<f32>, ResamplingCons<f32>) {
	resampling_channel::<f32>(
		BUS_CHANNELS,
		from,
		to,
		// We only ever push interleaved, which lets the channel skip its planar
		// staging buffer.
		true,
		ResamplingChannelConfig {
			latency_seconds: LATENCY,
			capacity_seconds: CAPACITY,
			// Correct drift by resampling rather than by jumping, so a clock
			// that is slightly off doesn't tick audibly.
			underflow_autocorrect_percent_threshold: Some(25.0),
			overflow_autocorrect_percent_threshold: Some(75.0),
			..Default::default()
		},
	)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn rejects_layouts_it_cannot_mix() {
		for channels in [0, 6] {
			let input = Input {
				channels,
				..Default::default()
			};
			assert!(
				matches!(input.validate(), Err(Error::Unsupported(_))),
				"{channels} channels"
			);
		}

		let input = Input {
			sample_rate: 0,
			..Default::default()
		};
		assert!(matches!(input.validate(), Err(Error::Unsupported(_))));
	}

	#[test]
	fn accepts_mono_and_stereo() {
		for channels in [1, 2] {
			let input = Input {
				channels,
				..Default::default()
			};
			input.validate().unwrap();
		}
	}
}
