//! [`Sink`]: one stream of PCM on its way to the speaker.

use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fixed_resample::{PushStatus, ResamplingChannelConfig, ResamplingCons, ResamplingProd, resampling_channel};

use super::driver::Shared;
use super::mixer::{self, Gain};
use crate::resample::Remix;
use crate::{Error, Format, Layout};

/// Default for [`Input::latency`]: audio buffered between [`Sink::write`] and
/// the speaker.
///
/// This is the price of surviving jitter: the device pulls on a fixed clock, so
/// a late write is a dropout. 50 ms rides out a stalled network read without
/// being audible as delay.
const LATENCY: Duration = Duration::from_millis(50);

/// Headroom above [`Input::latency`]. A writer that runs ahead (a decoder
/// catching up after a pause) parks samples here instead of losing them.
const HEADROOM: f64 = 3.0;

/// The PCM layout a [`Sink`] accepts.
///
/// The playback counterpart to [`encode::Input`](crate::encode::Input): it
/// describes the buffers you hand in, not the device, which is free to run at
/// its own rate and layout.
///
/// `#[non_exhaustive]`: construct via [`Input::default`] and set fields, so new
/// options can be added without breaking callers.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Input {
	/// How samples are packed in each buffer.
	pub format: Format,
	/// Samples per second per channel. Resampled to the device rate if they
	/// differ.
	pub sample_rate: u32,
	/// Speaker meaning and channel order. Remixed to the device's layout, so it
	/// must name speaker positions rather than be [`Layout::Discrete`].
	pub layout: Layout,

	/// How much audio to hold between [`Sink::write`] and the speaker (default:
	/// 50 ms).
	///
	/// The device pulls on its own clock, so this is the jitter a late write may
	/// absorb without a dropout, and it is also delay: the sample handed over now
	/// sounds this much later. A player that presents video against a playout
	/// delay sets this to match, so the sink is where that delay lives rather
	/// than something the picture has to be held back for separately. Must be
	/// non-zero: a ring with no depth can never be read from.
	pub latency: Duration,
}

impl Default for Input {
	fn default() -> Self {
		Self {
			format: Format::F32,
			sample_rate: 48_000,
			layout: Layout::Stereo,
			latency: LATENCY,
		}
	}
}

impl Input {
	/// The longest [`latency`](Self::latency) a sink accepts. Well past any
	/// playout delay worth presenting, and it bounds the ring the device thread
	/// walks. Public so a caller taking the depth from its own configuration can
	/// refuse an impossible one before it opens a device.
	pub const LATENCY_MAX: Duration = Duration::from_secs(10);

	fn validate(&self) -> Result<(), Error> {
		if self.sample_rate == 0 {
			return Err(Error::Unsupported("sample rate must be > 0".into()));
		}
		if self.layout.speakers().is_none() {
			return Err(Error::Unsupported(format!(
				"playback needs speaker positions to remix (got {:?})",
				self.layout
			)));
		}
		if self.latency.is_zero() || self.latency > Self::LATENCY_MAX {
			return Err(Error::Unsupported(format!(
				"playback latency must be non-zero and at most {:?} (got {:?})",
				Self::LATENCY_MAX,
				self.latency
			)));
		}
		Ok(())
	}
}

/// The sample frames accepted and dropped by one [`Sink::write`].
///
/// A sample frame is one instant across all input channels, so one stereo
/// frame counts once rather than twice.
#[must_use = "inspect dropped_sample_frames; retrying dropped live audio adds latency"]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Write {
	/// Input sample frames accepted for playback.
	pub accepted_sample_frames: usize,
	/// Input sample frames dropped to keep playback live.
	pub dropped_sample_frames: usize,
}

impl Write {
	fn from_accepted(requested_sample_frames: usize, accepted_sample_frames: usize) -> Self {
		Self {
			accepted_sample_frames,
			dropped_sample_frames: requested_sample_frames - accepted_sample_frames,
		}
	}
}

/// One stream of PCM being played, mixed with every other sink on the device.
///
/// Write decoded samples with [`write`](Self::write) and drop the sink to stop.
/// Writes are cheap and never block on the device: they remix to the device's
/// layout and hand samples to a ring buffer that the audio thread drains on its
/// own clock, resampling to the device rate on the way.
pub struct Sink {
	id: u64,
	input: Input,
	channel: Arc<Mutex<Channel>>,
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
	/// The returned [`Write`] counts input sample frames accepted and dropped;
	/// dropped live audio should be observed for telemetry, not retried.
	pub fn write(&mut self, samples: &[u8]) -> Result<Write, Error> {
		let channels = self.input.layout.channels();
		let pcm = self.input.format.as_interleaved_f32(samples, channels)?;
		let requested_sample_frames = pcm.len() / channels as usize;

		let mut channel = self.channel.lock().unwrap();
		let mixed;
		let pcm = match &channel.remix {
			Some(remix) => {
				mixed = remix.process(&pcm);
				&mixed
			}
			None => pcm.as_ref(),
		};

		let accepted_sample_frames = match channel.prod.push_interleaved(pcm) {
			// OutputNotReady means the device has not read yet, so these samples
			// are dropped rather than queued to play late.
			PushStatus::Ok => {
				self.overflowing = false;
				requested_sample_frames
			}
			PushStatus::OutputNotReady => {
				self.overflowing = false;
				0
			}
			PushStatus::OverflowOccurred { num_frames_pushed } => {
				// Once per spell, not once per write: a writer that stays ahead
				// of the device would otherwise warn every frame for as long as
				// it lasts.
				if !self.overflowing {
					tracing::warn!(num_frames_pushed, "audio playback overflow, dropping samples");
					self.overflowing = true;
				}
				num_frames_pushed
			}
			PushStatus::UnderflowCorrected { num_zero_frames_pushed } => {
				self.overflowing = false;
				tracing::debug!(num_zero_frames_pushed, "audio playback underflow, padded with silence");
				requested_sample_frames
			}
		};

		Ok(Write::from_accepted(requested_sample_frames, accepted_sample_frames))
	}

	/// How much audio is queued between the last [`write`](Self::write) and the
	/// speaker.
	///
	/// The pacing signal for A/V sync: the sample playing right now was written
	/// at roughly `last_timestamp - buffered()`, so a video clock can steer
	/// against it. It settles near [`Input::latency`] once playback is running,
	/// climbs when the writer runs ahead, and falls toward zero when it falls
	/// behind.
	pub fn buffered(&self) -> Duration {
		Duration::from_secs_f64(self.channel.lock().unwrap().prod.occupied_seconds().max(0.0))
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

/// The ring into the mixer and the remix that fills it, swapped together when
/// the device changes rate or layout.
struct Channel {
	prod: ResamplingProd<f32>,
	/// Converts the sink's layout to the device's, when they differ.
	remix: Option<Remix>,
}

/// A sink as the driver sees it: enough to rebuild its channel when the device
/// changes underneath it.
pub(super) struct Registration {
	pub(super) id: u64,
	/// The caller's rate, layout, and latency: the input side of the channel.
	input: Input,
	channel: Arc<Mutex<Channel>>,
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

	/// Re-create the channel for a device now running at `rate` in `bus`,
	/// swapping the producer the caller's [`Sink`] writes into.
	pub(super) fn rebuild(&mut self, rate: u32, bus: Layout) {
		let (channel, cons) = channel(&self.input, rate, bus);
		*self.channel.lock().unwrap() = channel;
		self.pending = Some(cons);
	}
}

/// Build a sink and its registration. The device may not be open yet, in which
/// case `rate` and `bus` are placeholders the driver replaces on the next
/// rebuild.
pub(super) fn new(
	id: u64,
	rate: u32,
	bus: Layout,
	input: Input,
	shared: Arc<Shared>,
	engine: Arc<super::Handle>,
) -> Result<(Sink, Registration), Error> {
	input.validate()?;

	let (channel, cons) = self::channel(&input, rate, bus);
	let channel = Arc::new(Mutex::new(channel));
	let gain = Arc::new(Gain::new());

	let sink = Sink {
		id,
		input,
		channel: channel.clone(),
		control: Control { gain: gain.clone() },
		overflowing: false,
		shared,
		engine,
	};

	let registration = Registration {
		id,
		input: sink.input.clone(),
		channel,
		gain,
		pending: Some(cons),
	};

	Ok((sink, registration))
}

/// The ring buffer between a writer and the audio thread, remixing the caller's
/// layout to the device's and resampling its rate to the device's.
fn channel(input: &Input, rate: u32, bus: Layout) -> (Channel, ResamplingCons<f32>) {
	let remix =
		(input.layout != bus).then(|| Remix::new(input.layout, bus).expect("sink and bus layouts name their speakers"));

	let latency = input.latency.as_secs_f64();
	let (prod, cons) = resampling_channel::<f32>(
		bus.channels() as usize,
		input.sample_rate,
		rate,
		// We only ever push interleaved, which lets the channel skip its planar
		// staging buffer.
		true,
		ResamplingChannelConfig {
			latency_seconds: latency,
			capacity_seconds: latency + HEADROOM,
			// Correct drift by resampling rather than by jumping, so a clock
			// that is slightly off doesn't tick audibly.
			underflow_autocorrect_percent_threshold: Some(25.0),
			overflow_autocorrect_percent_threshold: Some(75.0),
			..Default::default()
		},
	);

	(Channel { prod, remix }, cons)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn sink(input: Input, output_rate: u32) -> (Sink, ResamplingCons<f32>) {
		sink_into(input, output_rate, Layout::Stereo)
	}

	fn sink_into(input: Input, output_rate: u32, bus: Layout) -> (Sink, ResamplingCons<f32>) {
		let shared = Arc::new(Shared::default());
		let engine = Arc::new(super::super::Handle {
			commands: super::super::driver::Commands::default(),
		});
		let (sink, mut registration) = new(0, output_rate, bus, input, shared, engine).unwrap();
		(sink, registration.pending.take().unwrap())
	}

	/// Write `frame` repeated as `input` and read back what the bus got.
	fn mix(input: Layout, bus: Layout, frame: &[f32]) -> Vec<f32> {
		let input = Input {
			layout: input,
			..Default::default()
		};
		let (mut sink, mut cons) = sink_into(input, 48_000, bus);
		let channels = bus.channels() as usize;
		cons.read_interleaved(&mut vec![0.0; channels], false);

		let pcm: Vec<u8> = frame.repeat(4800).iter().flat_map(|s| s.to_le_bytes()).collect();
		assert_eq!(sink.write(&pcm).unwrap().dropped_sample_frames, 0);

		let mut out = vec![0.0; 4800 * channels];
		cons.read_interleaved(&mut out, false);
		out[out.len() - channels..].to_vec()
	}

	fn close(got: &[f32], want: &[f32]) {
		assert_eq!(got.len(), want.len(), "{got:?} vs {want:?}");
		for (g, w) in got.iter().zip(want) {
			assert!((g - w).abs() < 1e-4, "{got:?} vs {want:?}");
		}
	}

	/// A 5.1 track on a stereo device plays its downmix, center and surrounds
	/// folded into the front pair at -3 dB.
	#[test]
	fn a_surround_sink_downmixes_into_a_stereo_bus() {
		let h = std::f32::consts::FRAC_1_SQRT_2;
		let got = mix(Layout::FivePointOne, Layout::Stereo, &[0.1, 0.2, 0.3, 0.4, 0.05, 0.06]);
		close(&got, &[0.1 + h * 0.3 + h * 0.05, 0.2 + h * 0.3 + h * 0.06]);
	}

	/// A stereo track on a 5.1 device plays from the front pair alone.
	#[test]
	fn a_stereo_sink_fills_the_front_of_a_surround_bus() {
		let got = mix(Layout::Stereo, Layout::FivePointOne, &[0.25, 0.75]);
		close(&got, &[0.25, 0.75, 0.0, 0.0, 0.0, 0.0]);
	}

	fn s16(frames: usize, channels: usize) -> Vec<u8> {
		vec![0; frames * channels * 2]
	}

	fn ready(cons: &mut ResamplingCons<f32>) {
		cons.read_interleaved(&mut [0.0; 2], false);
	}

	#[test]
	fn reports_output_not_ready_as_dropped() {
		let input = Input {
			format: Format::S16,
			..Default::default()
		};
		let (mut sink, _cons) = sink(input, 48_000);

		assert_eq!(
			sink.write(&s16(10, 2)).unwrap(),
			Write {
				accepted_sample_frames: 0,
				dropped_sample_frames: 10,
			}
		);
	}

	#[test]
	fn reports_output_that_stops_as_dropped() {
		let input = Input {
			format: Format::S16,
			..Default::default()
		};
		let (mut sink, mut cons) = sink(input, 48_000);
		ready(&mut cons);
		cons.set_output_stream_ready(false);

		assert_eq!(
			sink.write(&s16(10, 2)).unwrap(),
			Write {
				accepted_sample_frames: 0,
				dropped_sample_frames: 10,
			}
		);
	}

	#[test]
	fn reports_input_frame_units_through_conversion_and_resampling() {
		let input = Input {
			format: Format::S16,
			sample_rate: 44_100,
			layout: Layout::Mono,
			..Default::default()
		};
		let (mut sink, mut cons) = sink(input, 48_000);
		ready(&mut cons);

		assert_eq!(
			sink.write(&s16(441, 1)).unwrap(),
			Write {
				accepted_sample_frames: 441,
				dropped_sample_frames: 0,
			}
		);
	}

	#[test]
	fn reports_partial_acceptance_on_overflow() {
		let input = Input {
			format: Format::S16,
			latency: Duration::from_millis(1),
			..Default::default()
		};
		let (mut sink, mut cons) = sink(input, 48_000);
		ready(&mut cons);
		let requested = 4 * 48_000;

		let write = sink.write(&s16(requested, 2)).unwrap();
		assert!(write.accepted_sample_frames > 0);
		assert!(write.dropped_sample_frames > 0);
		assert_eq!(write.accepted_sample_frames + write.dropped_sample_frames, requested);
	}

	#[test]
	fn keeps_invalid_input_distinct_from_dropping() {
		let input = Input {
			format: Format::S16,
			..Default::default()
		};
		let (mut sink, _cons) = sink(input, 48_000);

		assert!(matches!(sink.write(&[0]), Err(Error::Misaligned { .. })));
	}

	#[test]
	fn rejects_layouts_it_cannot_mix() {
		for layout in [Layout::Discrete(0), Layout::Discrete(2), Layout::Discrete(6)] {
			let input = Input {
				layout,
				..Default::default()
			};
			assert!(matches!(input.validate(), Err(Error::Unsupported(_))), "{layout:?}");
		}

		let input = Input {
			sample_rate: 0,
			..Default::default()
		};
		assert!(matches!(input.validate(), Err(Error::Unsupported(_))));
	}

	#[test]
	fn accepts_named_layouts() {
		for layout in [
			Layout::Mono,
			Layout::Stereo,
			Layout::FivePointOne,
			Layout::SevenPointOne,
		] {
			let input = Input {
				layout,
				..Default::default()
			};
			input.validate().unwrap();
		}
	}

	/// A ring with no depth can never be read from, and one deeper than the
	/// device could ever drain is a delay nobody asked for.
	#[test]
	fn rejects_a_latency_it_cannot_buffer() {
		for latency in [Duration::ZERO, Input::LATENCY_MAX + Duration::from_secs(1)] {
			let input = Input {
				latency,
				..Default::default()
			};
			assert!(matches!(input.validate(), Err(Error::Unsupported(_))), "{latency:?}");
		}

		Input {
			latency: Input::LATENCY_MAX,
			..Default::default()
		}
		.validate()
		.unwrap();
	}
}
