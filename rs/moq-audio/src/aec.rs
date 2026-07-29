//! Acoustic echo cancellation: keep the speaker out of the microphone.
//!
//! Without this, anyone on a laptop without a headset sends the rest of the call
//! back to itself. A [`Canceller`] comes from the [`playback::Engine`] doing the
//! playing, because cancelling an echo means knowing what was played, and goes
//! into [`capture::Config`](crate::capture::Config) so the microphone it hears
//! is already clean:
//!
//! ```no_run
//! # async fn example() -> Result<(), moq_audio::Error> {
//! use moq_audio::{aec, capture, playback};
//!
//! let engine = playback::Engine::open(playback::Config::default()).await?;
//!
//! let mut capture = capture::Config::default();
//! capture.aec = Some(engine.canceller(aec::Config::default()));
//! # Ok(())
//! # }
//! ```
//!
//! One canceller belongs to one microphone: it holds the adaptive filter that
//! models the path from that speaker to that microphone. Clones share it, so
//! clone for a mute button on a UI thread, not to run a second capture.
//!
//! The work happens in the microphone callback, on 10 ms frames, which is what
//! adds up to 10 ms of latency to the capture path. Both the echo reference and
//! the microphone are processed there, in that order, which is the ordering the
//! echo model needs and the reason none of this is split across the two audio
//! callbacks.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use fixed_resample::{ResamplingChannelConfig, ResamplingCons, ResamplingProd, resampling_channel};
use sonora::config::{EchoCanceller, GainController2, NoiseSuppression, NoiseSuppressionLevel};
use sonora::{AudioProcessing, StreamConfig};

use crate::Error;
use crate::playback::{self, BUS_CHANNELS};

/// Sample rate the echo reference is resampled to on its way out of the mixer.
///
/// Fixed rather than the device's, so switching to a 44.1 kHz output doesn't
/// invalidate an adaptive filter that spent seconds converging. Devices run at
/// 48 kHz far more often than not, in which case this resamples nothing.
const REFERENCE_RATE: u32 = 48_000;

/// Slack between the output callback and the microphone callback, for the
/// jitter between two independently scheduled threads.
///
/// Not a target depth. The microphone callback drains this queue every pass, so
/// what it holds is only what the output callback has produced since, and the
/// reference keeps the full head start the output hardware's own buffering
/// gives it. A standing queue here would be subtracted from that head start,
/// and a device that buffers less than the queue holds would see its echo
/// arrive before the reference describing it, which is a reference the model
/// cannot use at all.
const REFERENCE_LATENCY: f64 = 0.01;

/// Ceiling on that queue, bounding what a closed microphone can pin.
const REFERENCE_CAPACITY: f64 = 0.2;

/// Samples per channel in one 10 ms reference frame.
const REFERENCE_FRAME: usize = REFERENCE_RATE as usize / 100;

/// Callback size the scratch buffers are sized for, in frames. Bigger callbacks
/// still work; they just grow the buffers once instead of never.
const MAX_CALLBACK: usize = 4096;

/// Echo cancellation settings.
///
/// `#[non_exhaustive]`: construct via [`Config::default`] and set fields, so new
/// options can be added without breaking callers.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// Also suppress steady background noise: fans, hum, street noise.
	///
	/// On by default, matching what a browser gives a call. Turn it off to
	/// capture music, or anything else where "steady" doesn't mean "unwanted".
	pub noise_suppression: bool,

	/// Also level the microphone toward a consistent loudness.
	///
	/// On by default, again matching the browser. Turn it off when the input
	/// level is already managed, e.g. by an audio interface.
	pub auto_gain: bool,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			noise_suppression: true,
			auto_gain: true,
		}
	}
}

impl Config {
	/// The settings sonora takes.
	fn build(&self) -> sonora::Config {
		sonora::Config {
			echo_canceller: Some(EchoCanceller::default()),
			noise_suppression: self.noise_suppression.then(|| NoiseSuppression {
				level: NoiseSuppressionLevel::High,
				..Default::default()
			}),
			gain_controller2: self.auto_gain.then(GainController2::default),
			..Default::default()
		}
	}
}

/// Removes the echo of a [`playback::Engine`] from a microphone.
///
/// Built by [`Engine::canceller`](playback::Engine::canceller) and handed to
/// [`capture::Config::aec`](crate::capture::Config::aec), which is the whole of
/// the usual surface: the rest here is a mute button you can hold on another
/// thread.
///
/// Cheap to clone, and every clone drives the same adaptive filter. The
/// reference tap on the engine goes away when the last clone drops.
#[derive(Clone)]
pub struct Canceller {
	inner: Arc<Inner>,
}

impl Canceller {
	/// Register a canceller against a running engine.
	///
	/// `pub(crate)`: [`Engine::canceller`](playback::Engine::canceller) is the
	/// entry point, so a canceller can't exist without the reference it needs.
	pub(crate) fn new(shared: Arc<playback::Shared>, config: Config) -> Self {
		static NEXT_ID: AtomicU64 = AtomicU64::new(0);

		let inner = Arc::new(Inner {
			id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
			enabled: AtomicBool::new(true),
			config,
			state: Arc::new(Mutex::new(State::default())),
			shared: shared.clone(),
		});

		shared.set_reference(Reference {
			id: inner.id,
			alive: Arc::downgrade(&inner),
			state: inner.state.clone(),
			pending: None,
		});

		Self { inner }
	}

	/// Turn cancellation on or off without reopening any device.
	///
	/// Off is a straight passthrough, so the 10 ms of latency goes with it. The
	/// adaptive filter stops adapting while off and needs a moment to
	/// re-converge once it comes back.
	pub fn set_enabled(&self, enabled: bool) {
		self.inner.enabled.store(enabled, Ordering::Relaxed);
	}

	/// Whether cancellation is running. Starts out `true`.
	pub fn enabled(&self) -> bool {
		self.inner.enabled.load(Ordering::Relaxed)
	}

	/// Point the canceller at a microphone with this format.
	///
	/// Called when `capture` opens a device, so the allocation lands there
	/// rather than in the first callback. Capture is demand-gated, so this runs
	/// again every time a listener comes back: the adaptive filter survives that
	/// as long as the format hasn't changed, since it is still the same room.
	pub(crate) fn open(&self, sample_rate: u32, channels: u32) -> Result<(), Error> {
		// sonora reports a bad format per call, by which point we are on the
		// audio thread and can only log it. Reject it while there is still a
		// caller to hand an error to.
		if !(8_000..=384_000).contains(&sample_rate) {
			return Err(Error::Unsupported(format!(
				"echo cancellation needs a microphone between 8 and 384 kHz (got {sample_rate})"
			)));
		}
		if channels == 0 || channels > BUS_CHANNELS as u32 {
			return Err(Error::Unsupported(format!(
				"echo cancellation accepts a mono or stereo microphone (got {channels} channels)"
			)));
		}

		let capture = StreamConfig::new(sample_rate, channels as u16);
		// Built outside the lock: this allocates, and the microphone callback
		// takes the same lock.
		let processor = AudioProcessing::builder()
			.capture_config(capture)
			.render_config(reference_config())
			.config(self.inner.config.build())
			.build();

		let mut state = self.inner.state.lock().unwrap();
		if state.processor.is_none() || state.capture != capture {
			state.processor = Some(processor);
			state.capture = capture;
			state.resize();
		} else {
			state.reset();
		}

		// Nothing drained the tap while the microphone was closed, so it holds
		// whatever was playing back then. Feeding that in as if it were current
		// would put the echo model seconds out.
		if let Some(reference) = &mut state.reference {
			reference.discard_frames(reference.available_frames());
		}

		Ok(())
	}

	/// Replace the microphone samples in `buf` with the same span, minus the
	/// echo.
	///
	/// Interleaved at the format passed to [`open`](Self::open). Runs on the
	/// microphone callback thread, so it locks rather than allocates: the
	/// buffers are sized in `open` for callbacks up to [`MAX_CALLBACK`] frames,
	/// and only a larger one grows them.
	pub(crate) fn process(&self, buf: &mut [f32]) {
		let enabled = self.enabled();
		self.inner.state.lock().unwrap().process(buf, enabled);
	}
}

impl fmt::Debug for Canceller {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Canceller")
			.field("enabled", &self.enabled())
			.finish_non_exhaustive()
	}
}

/// The shared half: what the microphone callback locks, and what the playback
/// driver swaps a new reference into.
struct Inner {
	/// Distinguishes this canceller from a later one built on the same engine,
	/// so dropping the first doesn't take the second's tap with it.
	id: u64,
	enabled: AtomicBool,
	config: Config,
	/// Behind its own `Arc` so the playback driver can reach it without holding
	/// a handle on this `Inner`. Dropping such a handle from inside the driver's
	/// own lock would re-enter it through [`Inner::drop`] below.
	state: Arc<Mutex<State>>,
	/// The engine's registry, so dropping the last clone removes the tap.
	shared: Arc<playback::Shared>,
}

impl Drop for Inner {
	fn drop(&mut self) {
		self.shared.clear_reference(self.id);
	}
}

/// Everything the microphone callback owns while it holds the lock.
///
/// Uncontended in practice: only that callback takes it, plus the playback
/// driver on a device switch.
struct State {
	/// `None` until a microphone opens and reports its format.
	processor: Option<AudioProcessing>,
	capture: StreamConfig,
	/// Reference from the output mixer, replaced whenever the output stream is
	/// rebuilt and `None` while none is running.
	reference: Option<ResamplingCons<f32>>,

	/// Whether the last callback cancelled, so a toggle can drop the buffers
	/// that no longer line up.
	running: bool,
	/// Microphone samples that didn't fill a 10 ms frame, interleaved.
	pending: Vec<f32>,
	/// Processed samples waiting to be handed back, interleaved.
	processed: Vec<f32>,

	/// One interleaved reference frame, read from [`State::reference`].
	reference_frame: Vec<f32>,
	/// Deinterleaved scratch, channel-major: the reference, and the output the
	/// processor insists on writing even though we discard it.
	render_in: Vec<f32>,
	render_out: Vec<f32>,
	/// Deinterleaved scratch for the microphone frame and its result.
	capture_in: Vec<f32>,
	capture_out: Vec<f32>,
}

impl Default for State {
	fn default() -> Self {
		Self {
			processor: None,
			// Replaced by `Canceller::open`; nothing is processed before then.
			capture: reference_config(),
			reference: None,
			running: false,
			pending: Vec::new(),
			processed: Vec::new(),
			reference_frame: Vec::new(),
			render_in: Vec::new(),
			render_out: Vec::new(),
			capture_in: Vec::new(),
			capture_out: Vec::new(),
		}
	}
}

impl State {
	/// Size every buffer for the microphone format in [`State::capture`].
	fn resize(&mut self) {
		let frame = self.capture.num_frames();
		let channels = self.capture.num_channels() as usize;
		let headroom = (frame + MAX_CALLBACK) * channels;

		self.pending = Vec::with_capacity(headroom);
		self.processed = Vec::with_capacity(headroom);

		self.reference_frame = vec![0.0; REFERENCE_FRAME * BUS_CHANNELS];
		self.render_in = vec![0.0; REFERENCE_FRAME * BUS_CHANNELS];
		self.render_out = vec![0.0; REFERENCE_FRAME * BUS_CHANNELS];
		self.capture_in = vec![0.0; frame * channels];
		self.capture_out = vec![0.0; frame * channels];

		self.reset();
	}

	/// Forget the samples in flight, because the stream they belong to is gone.
	fn reset(&mut self) {
		self.pending.clear();
		self.processed.clear();
		self.running = false;
	}

	fn process(&mut self, buf: &mut [f32], enabled: bool) {
		let Self {
			processor,
			capture,
			reference,
			running,
			pending,
			processed,
			reference_frame,
			render_in,
			render_out,
			capture_in,
			capture_out,
		} = self;

		// No microphone has been opened against this canceller, so there is
		// nothing to line the reference up against.
		let Some(processor) = processor else { return };

		let channels = capture.num_channels() as usize;
		let stride = capture.num_frames() * channels;
		if !buf.len().is_multiple_of(channels) {
			return;
		}

		if !enabled {
			// Passthrough. Drop the partial frames that would otherwise
			// resurface out of order when it is switched back on.
			if *running {
				pending.clear();
				processed.clear();
				*running = false;
			}
			// Keep the tap empty anyway, so a toggle seconds from now starts on
			// what is playing then rather than on what filled the queue while
			// nobody was reading it.
			if let Some(reference) = reference {
				reference.discard_frames(reference.available_frames());
			}
			return;
		}
		*running = true;

		// The reference first: the echo model has to be told what was played
		// before it is asked to find it in the microphone.
		//
		// Everything the mixer has produced goes in, rather than one frame per
		// microphone frame. Pacing it against capture would leave whatever the
		// queue starts out holding in it forever, as a fixed delay on top of the
		// hardware's, and a reference that trails its own echo is one the model
		// cannot subtract. Draining instead means the reference is never older
		// than the last output callback, so it leads the echo by however much
		// the output device buffers, however little that is.
		if let Some(reference) = reference {
			while reference.available_frames() >= REFERENCE_FRAME {
				reference.read_interleaved(reference_frame, false);
				deinterleave(reference_frame, render_in, BUS_CHANNELS);
				let _ = process_render(processor, render_in, render_out);
			}
		}

		pending.extend_from_slice(buf);

		while pending.len() >= stride {
			deinterleave(&pending[..stride], capture_in, channels);
			match process_capture(processor, capture, capture_in, capture_out) {
				Ok(()) => interleave(capture_out, channels, processed),
				// sonora writes a fallback of its own choosing when it rejects a
				// format, and it will keep rejecting it. Pass the microphone
				// through rather than publish whatever that fallback was.
				Err(_) => processed.extend_from_slice(&pending[..stride]),
			}

			pending.drain(..stride);
		}

		// Hand back exactly what came in. Short only while the first frame is
		// still accumulating, which is where the 10 ms of latency comes from.
		let ready = processed.len().min(buf.len());
		buf[..ready].copy_from_slice(&processed[..ready]);
		buf[ready..].fill(0.0);
		processed.drain(..ready);
	}
}

/// The format the mixer's reference tap arrives in.
fn reference_config() -> StreamConfig {
	StreamConfig::new(REFERENCE_RATE, BUS_CHANNELS as u16)
}

/// Feed one reference frame to the echo model.
///
/// sonora writes a processed copy we have no use for; the model is built from
/// what goes in.
fn process_render(processor: &mut AudioProcessing, input: &[f32], output: &mut [f32]) -> Result<(), sonora::Error> {
	let config = reference_config();
	let (left, right) = input.split_at(REFERENCE_FRAME);
	let (left_out, right_out) = output.split_at_mut(REFERENCE_FRAME);
	processor.process_render_f32_with_config(&[left, right], &config, &config, &mut [left_out, right_out])
}

/// Run one microphone frame through the processor, channel-major in and out.
///
/// Split by channel count rather than collected into a `Vec<&[f32]>` so the
/// microphone callback doesn't allocate. Mono and stereo are the only counts
/// [`Canceller::open`] accepts, which is also all Opus encodes.
fn process_capture(
	processor: &mut AudioProcessing,
	config: &StreamConfig,
	input: &[f32],
	output: &mut [f32],
) -> Result<(), sonora::Error> {
	let frame = config.num_frames();
	match config.num_channels() {
		1 => processor.process_capture_f32_with_config(&[input], config, config, &mut [output]),
		_ => {
			let (left, right) = input.split_at(frame);
			let (left_out, right_out) = output.split_at_mut(frame);
			processor.process_capture_f32_with_config(&[left, right], config, config, &mut [left_out, right_out])
		}
	}
}

/// Rewrite interleaved samples as channel-major.
fn deinterleave(src: &[f32], dest: &mut [f32], channels: usize) {
	let frames = src.len() / channels;
	for (channel, samples) in dest.chunks_exact_mut(frames).enumerate() {
		for (frame, sample) in samples.iter_mut().enumerate() {
			*sample = src[frame * channels + channel];
		}
	}
}

/// Append channel-major samples to `dest`, interleaved.
fn interleave(src: &[f32], channels: usize, dest: &mut Vec<f32>) {
	let frames = src.len() / channels;
	for frame in 0..frames {
		for channel in 0..channels {
			dest.push(src[channel * frames + frame]);
		}
	}
}

/// The echo reference as the playback driver sees it: enough to rebuild the tap
/// when the output device changes underneath it.
///
/// The mirror image of a sink's registration, rebuilt at the same points. The
/// producer goes to the mixer and the consumer to the canceller.
pub(crate) struct Reference {
	/// The canceller this belongs to, matched on removal.
	id: u64,
	/// Whether that canceller still exists. Never upgraded: this is held inside
	/// the driver's lock, and the last handle to an `Inner` dropping there would
	/// re-enter that lock.
	alive: Weak<Inner>,
	/// Where a rebuilt consumer is left for the microphone callback to pick up.
	state: Arc<Mutex<State>>,
	/// The producer waiting to be handed to a mixer, refilled by
	/// [`rebuild`](Self::rebuild).
	pending: Option<ResamplingProd<f32>>,
}

impl Reference {
	/// Whether this tap belongs to the canceller with this id.
	pub(crate) fn owned_by(&self, id: u64) -> bool {
		self.id == id
	}

	/// Whether the mixer has the producer, i.e. there is nothing left to retry.
	pub(crate) fn attached(&self) -> bool {
		self.pending.is_none()
	}

	/// The producer to hand to a running mixer, or `None` if one is already
	/// there.
	pub(crate) fn take(&mut self) -> Option<ResamplingProd<f32>> {
		self.pending.take()
	}

	/// Put back a producer the mixer refused, so the next attach retries.
	pub(crate) fn restore(&mut self, prod: ResamplingProd<f32>) {
		self.pending = Some(prod);
	}

	/// Re-create the tap for an output stream now running at `rate`, handing the
	/// canceller the new consumer.
	///
	/// Returns whether the canceller is still around; the driver drops the
	/// registration when it isn't.
	pub(crate) fn rebuild(&mut self, rate: u32) -> bool {
		if self.alive.strong_count() == 0 {
			return false;
		}

		let (prod, cons) = channel(rate);
		self.state.lock().unwrap().reference = Some(cons);
		self.pending = Some(prod);
		true
	}
}

/// The tap itself: the mixer pushes the device's mix in at `rate` and the
/// canceller reads it back at [`REFERENCE_RATE`].
fn channel(rate: u32) -> (ResamplingProd<f32>, ResamplingCons<f32>) {
	resampling_channel::<f32>(
		BUS_CHANNELS,
		rate,
		REFERENCE_RATE,
		true,
		ResamplingChannelConfig {
			latency_seconds: REFERENCE_LATENCY,
			capacity_seconds: REFERENCE_CAPACITY,
			// A near-empty queue is the normal state here, not an underflow to
			// correct: the microphone callback drains it every pass. Left on,
			// the default would pad it back up to the target depth with silence
			// the speaker never played, which is both a fabricated reference and
			// the standing delay this is shaped to avoid.
			underflow_autocorrect_percent_threshold: None,
			..Default::default()
		},
	)
}

#[cfg(test)]
mod tests {
	use std::collections::VecDeque;

	use super::*;

	/// Samples per channel in one 10 ms frame at `rate`.
	const fn frame(rate: u32) -> usize {
		rate as usize / 100
	}

	/// A canceller registered against an engine that never opened a device, so
	/// the frame arithmetic can be tested without hardware.
	fn detached() -> Canceller {
		Canceller::new(Arc::new(playback::Shared::default()), Config::default())
	}

	fn opened(sample_rate: u32, channels: u32) -> Canceller {
		let canceller = detached();
		canceller.open(sample_rate, channels).unwrap();
		canceller
	}

	/// Give `canceller` a tap and hand back the end the mixer would push into,
	/// built exactly as the driver builds it so the two can't drift.
	fn tap(canceller: &Canceller) -> ResamplingProd<f32> {
		let (prod, cons) = channel(REFERENCE_RATE);
		canceller.inner.state.lock().unwrap().reference = Some(cons);
		prod
	}

	/// Push one 10 ms stereo frame of `value` into the tap.
	fn play(prod: &mut ResamplingProd<f32>, value: f32) {
		prod.push_interleaved(&vec![value; REFERENCE_FRAME * BUS_CHANNELS]);
	}

	#[test]
	fn rejects_formats_the_processor_cannot_take() {
		let canceller = detached();
		assert!(matches!(canceller.open(4_000, 1), Err(Error::Unsupported(_))));
		assert!(matches!(canceller.open(48_000, 0), Err(Error::Unsupported(_))));
		assert!(matches!(canceller.open(48_000, 6), Err(Error::Unsupported(_))));
		canceller.open(48_000, 1).unwrap();
		canceller.open(16_000, 2).unwrap();
	}

	#[test]
	fn returns_exactly_what_it_was_given() {
		let canceller = opened(48_000, 2);
		for frames in [64, 480, 512, 4096] {
			let mut buf = vec![0.25f32; frames * 2];
			canceller.process(&mut buf);
			assert_eq!(buf.len(), frames * 2, "{frames} frames");
		}
	}

	/// A device whose callback is a whole number of 10 ms frames pays no
	/// latency: the frame is complete the moment it arrives.
	#[test]
	fn whole_frames_are_not_delayed() {
		let canceller = opened(48_000, 1);
		let frame = frame(48_000);

		// Nothing is playing, so there is no echo to remove and a steady tone
		// has to survive.
		let mut loud = false;
		for _ in 0..20 {
			let mut buf = vec![0.5f32; frame];
			canceller.process(&mut buf);
			loud |= buf.iter().any(|s| s.abs() > 0.01);
		}
		assert!(loud, "the microphone came back silent");
	}

	/// A callback that straddles a frame boundary is short by the leftover, once,
	/// which is where the latency comes from.
	#[test]
	fn partial_frames_cost_the_leftover_once() {
		let canceller = opened(48_000, 1);
		let frame = frame(48_000);

		let mut first = vec![0.5f32; frame * 3 / 2];
		canceller.process(&mut first);
		let silent = first.iter().rev().take_while(|s| **s == 0.0).count();
		assert_eq!(silent, frame / 2, "expected the leftover half-frame to be held back");

		let mut second = vec![0.5f32; frame * 3 / 2];
		canceller.process(&mut second);
		assert_ne!(second.last(), Some(&0.0), "the leftover was never made up");
	}

	/// Callback sizes that don't divide evenly into 10 ms must not lose samples:
	/// the residual carries into the next call rather than being dropped.
	#[test]
	fn keeps_up_with_callbacks_that_straddle_frames() {
		let canceller = opened(48_000, 1);
		let chunk = frame(48_000) * 3 / 2;

		let mut short = 0;
		for round in 0..20 {
			let mut buf = vec![0.5f32; chunk];
			canceller.process(&mut buf);
			// Past the first couple of rounds the pipeline is primed, so every
			// callback has to come back full.
			if round > 2 {
				short += buf.iter().rev().take_while(|s| **s == 0.0).count();
			}
		}

		assert_eq!(short, 0, "samples went missing after the pipeline filled");
	}

	#[test]
	fn disabled_passes_the_microphone_straight_through() {
		let canceller = opened(48_000, 2);
		canceller.set_enabled(false);
		assert!(!canceller.enabled());

		let mut buf = vec![0.5f32; 960 * 2];
		canceller.process(&mut buf);
		assert!(buf.iter().all(|s| *s == 0.5), "passthrough altered the samples");
	}

	/// Turning it off must drop the partial frame that was in flight, which would
	/// otherwise resurface seconds later in the middle of a live stream.
	#[test]
	fn toggling_off_drops_buffered_samples() {
		let canceller = opened(48_000, 1);
		let frame = frame(48_000);

		let mut buf = vec![0.5f32; frame * 3 / 2];
		canceller.process(&mut buf);
		assert!(!canceller.inner.state.lock().unwrap().pending.is_empty());

		canceller.set_enabled(false);
		let mut buf = vec![0.5f32; frame / 2];
		canceller.process(&mut buf);
		assert!(buf.iter().all(|s| *s == 0.5), "passthrough altered the samples");

		let state = canceller.inner.state.lock().unwrap();
		assert!(state.pending.is_empty(), "a partial frame survived the toggle");
		assert!(state.processed.is_empty(), "processed samples survived the toggle");
	}

	/// A second canceller takes the engine's one tap. The first must not take it
	/// away again on its way out.
	#[test]
	fn a_replaced_canceller_leaves_the_tap_alone() {
		let shared = Arc::new(playback::Shared::default());

		let first = Canceller::new(shared.clone(), Config::default());
		let second = Canceller::new(shared.clone(), Config::default());
		assert!(shared.has_reference());

		drop(first);
		assert!(shared.has_reference(), "the replacement lost its tap");

		drop(second);
		assert!(!shared.has_reference(), "the tap outlived every canceller");
	}

	#[test]
	fn a_canceller_without_a_microphone_leaves_the_buffer_alone() {
		let canceller = detached();
		let mut buf = vec![0.5f32; 480];
		canceller.process(&mut buf);
		assert!(buf.iter().all(|s| *s == 0.5));
	}

	/// Anything left queued in the tap is delay added on top of the hardware's,
	/// and once it exceeds what the output device buffers, the echo reaches the
	/// microphone before the reference describing it and cancellation is no
	/// longer possible at all. So a pass has to take everything, not one frame
	/// per microphone frame.
	#[test]
	fn a_pass_drains_the_whole_tap() {
		let canceller = opened(48_000, 1);
		let mut prod = tap(&canceller);
		let frame = frame(48_000);

		// The tap discards until the canceller has read once, so prime it.
		canceller.process(&mut vec![0.0f32; frame]);
		play(&mut prod, 0.5);
		canceller.process(&mut vec![0.0f32; frame]);

		// Ten frames of output against one frame of microphone, which is what a
		// device with a long period against a short one looks like.
		for _ in 0..10 {
			play(&mut prod, 0.5);
		}
		canceller.process(&mut vec![0.0f32; frame]);

		let queued = canceller
			.inner
			.state
			.lock()
			.unwrap()
			.reference
			.as_ref()
			.map_or(0, |r| r.available_frames());
		assert!(
			queued < REFERENCE_FRAME,
			"{queued} reference frames were left queued as standing delay"
		);
	}

	/// A canceller that never sees a reference still has to hand the microphone
	/// back, since the output device may simply not be open yet.
	#[test]
	fn survives_a_missing_reference() {
		let canceller = opened(48_000, 2);
		assert!(canceller.inner.state.lock().unwrap().reference.is_none());

		let mut buf = vec![0.5f32; frame(48_000) * 2 * 2];
		canceller.process(&mut buf);
		canceller.process(&mut buf);
		assert!(buf.iter().any(|s| s.abs() > 0.01), "audio was dropped");
	}

	/// A synthetic speaker-to-microphone path, since a real one needs hardware:
	/// the reference is noise and the microphone hears that same noise
	/// attenuated and delayed, with nobody talking over it.
	struct Room {
		canceller: Canceller,
		prod: ResamplingProd<f32>,
		noise: Noise,
		/// Frames played, newest last, so the delay can move mid-run.
		history: VecDeque<Vec<f32>>,
		delay: usize,
		frame: usize,
	}

	/// The most speaker-to-microphone delay [`Room`] can hold, in 10 ms frames.
	const MAX_DELAY: usize = 12;

	/// What a laptop lid does to its own speaker on the way to its own
	/// microphone.
	const ATTENUATION: f32 = 0.5;

	impl Room {
		/// The echo canceller on its own. Noise suppression would also attenuate
		/// noise, and gain control would move the level under the measurement.
		fn new(delay: usize) -> Self {
			let canceller = detached();
			let capture = StreamConfig::new(48_000, 1);
			{
				let mut state = canceller.inner.state.lock().unwrap();
				state.processor = Some(
					AudioProcessing::builder()
						.capture_config(capture)
						.render_config(reference_config())
						.config(sonora::Config {
							echo_canceller: Some(EchoCanceller::default()),
							..Default::default()
						})
						.build(),
				);
				state.capture = capture;
				state.resize();
			}

			let prod = tap(&canceller);
			Self {
				canceller,
				prod,
				noise: Noise::default(),
				history: VecDeque::with_capacity(MAX_DELAY + 1),
				delay,
				frame: frame(48_000),
			}
		}

		/// Play 10 ms of noise, hear its echo, and report the energy the
		/// microphone picked up against what came back out of the canceller.
		fn round(&mut self) -> (f64, f64) {
			let played: Vec<f32> = (0..self.frame).map(|_| self.noise.next()).collect();

			let mut reference = vec![0.0f32; self.frame * BUS_CHANNELS];
			for (i, sample) in played.iter().enumerate() {
				reference[i * BUS_CHANNELS] = *sample;
				reference[i * BUS_CHANNELS + 1] = *sample;
			}
			self.prod.push_interleaved(&reference);

			self.history.push_back(played);
			if self.history.len() > MAX_DELAY + 1 {
				self.history.pop_front();
			}

			let echo = self.history.len().saturating_sub(1 + self.delay);
			let mut heard: Vec<f32> = self.history[echo].iter().map(|s| s * ATTENUATION).collect();

			let before = energy(&heard);
			self.canceller.process(&mut heard);
			(before, energy(&heard))
		}
	}

	fn energy(samples: &[f32]) -> f64 {
		samples.iter().map(|s| (*s as f64).powi(2)).sum()
	}

	/// The point of the whole module: audio played out the speaker and picked up
	/// by the microphone has to come back quieter than it went in.
	#[test]
	fn cancels_the_echo_of_what_was_played() {
		let mut room = Room::new(2);
		let (mut heard, mut left, mut measured) = (0.0f64, 0.0f64, 0);

		// Two seconds: the adaptive filter needs a moment before its output means
		// anything, so only the last half second is measured.
		for round in 0..200 {
			let (before, after) = room.round();
			if round >= 150 {
				heard += before;
				left += after;
				measured += 1;
			}
		}

		assert!(measured > 0);
		let attenuation = 10.0 * (heard / left.max(f64::MIN_POSITIVE)).log10();
		assert!(attenuation > 20.0, "echo was only attenuated by {attenuation:.1} dB");
	}

	/// A call where the echo path changes length, which is what a device switch,
	/// an output stall, or somebody picking the laptop up looks like.
	///
	/// Reaching the end *is* the assertion. The adaptive filter grows to cover a
	/// long delay and has to shrink again when it shortens, and until sonora
	/// 0.2.0 that shrink indexed a slice backwards and panicked
	/// (dignifiedquire/sonora#14, `slice index starts at 13 but ends at 12`).
	/// Cargo builds test binaries with unwinding, so here that surfaces as a
	/// failure; a real binary takes the workspace's `panic = "abort"` and dies.
	#[test]
	fn survives_a_moving_echo_delay() {
		let mut room = Room::new(MAX_DELAY - 4);

		// Long enough for the filter to grow past the size it starts at.
		for _ in 0..400 {
			room.round();
		}

		// Then the path shortens, and keeps changing, so the estimate has to
		// keep moving rather than settling once.
		for delay in [1, MAX_DELAY - 4, 2, MAX_DELAY - 2, 1] {
			room.delay = delay;
			for _ in 0..200 {
				room.round();
			}
		}

		// Still cancelling, so the filter recovered rather than merely surviving.
		let (mut heard, mut left) = (0.0f64, 0.0f64);
		for _ in 0..100 {
			let (before, after) = room.round();
			heard += before;
			left += after;
		}
		let attenuation = 10.0 * (heard / left.max(f64::MIN_POSITIVE)).log10();
		assert!(attenuation > 10.0, "the filter never re-converged: {attenuation:.1} dB");
	}

	/// Deterministic white-ish noise, so the test above measures the same room
	/// every run. xorshift, because `rand` is not a dependency and this is not
	/// cryptography.
	#[derive(Default)]
	struct Noise(u32);

	impl Noise {
		fn next(&mut self) -> f32 {
			if self.0 == 0 {
				self.0 = 0x1234_5678;
			}
			self.0 ^= self.0 << 13;
			self.0 ^= self.0 >> 17;
			self.0 ^= self.0 << 5;
			// Half scale, leaving headroom for the attenuated echo.
			self.0 as f32 / u32::MAX as f32 - 0.5
		}
	}

	/// The half of this that only a real device can prove: what a
	/// [`playback::Engine`] plays reaches the tap.
	///
	/// Ignored by default, like the rest of the device round trips: CI has no
	/// sound card. Run it on a machine with speakers via
	/// `cargo test -p moq-audio --features aec -- --ignored`. Whether the
	/// microphone then stops hearing the speaker is an acoustic question about
	/// the room, not something a test can assert.
	#[tokio::test]
	#[ignore]
	async fn taps_a_real_output_device() {
		let engine = playback::Engine::open(playback::Config::default())
			.await
			.expect("an output device");
		let canceller = engine.canceller(Config::default());
		canceller.open(48_000, 1).expect("a mono microphone");

		let mut sink = engine
			.sink(playback::Input {
				sample_rate: 48_000,
				channels: 2,
				..Default::default()
			})
			.expect("a sink");

		// A 440 Hz tone at half scale, as the interleaved `f32` bytes a decoder
		// would hand over.
		let frames = 48_000 / 10;
		let mut tone = Vec::with_capacity(frames * 2 * 4);
		for frame in 0..frames {
			let value = (std::f32::consts::TAU * 440.0 * frame as f32 / 48_000.0).sin() * 0.5;
			for _ in 0..BUS_CHANNELS {
				tone.extend_from_slice(&value.to_le_bytes());
			}
		}

		let mut energy = 0.0f64;
		let mut buf = vec![0.0f32; REFERENCE_FRAME * BUS_CHANNELS];

		for _ in 0..20 {
			sink.write(&tone).expect("write");
			tokio::time::sleep(std::time::Duration::from_millis(100)).await;

			// Drain the tap the way the microphone callback would, and add up
			// what the speaker was given.
			let mut state = canceller.inner.state.lock().unwrap();
			while let Some(reference) = state.reference.as_mut()
				&& reference.available_frames() >= REFERENCE_FRAME
			{
				reference.read_interleaved(&mut buf, false);
				energy += buf.iter().map(|s| (*s as f64).powi(2)).sum::<f64>();
			}
		}

		assert!(energy > 1.0, "the mix never reached the echo reference");
	}

	#[test]
	fn interleaving_round_trips() {
		let interleaved = [1.0, -1.0, 2.0, -2.0, 3.0, -3.0];
		let mut planar = vec![0.0; 6];
		deinterleave(&interleaved, &mut planar, 2);
		assert_eq!(planar, vec![1.0, 2.0, 3.0, -1.0, -2.0, -3.0]);

		let mut round = Vec::new();
		interleave(&planar, 2, &mut round);
		assert_eq!(round, interleaved);
	}
}
