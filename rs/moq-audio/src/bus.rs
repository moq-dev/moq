//! Mix PCM sources into one program feed, read on the caller's clock.
//!
//! A [`Bus`] sums its [`Channel`]s, each with its own [`Strip`] of gain, mute,
//! and level meter, into one interleaved `f32` stream at the bus's
//! [`Config`]. It is what a production switcher's program audio needs:
//! microphones, system audio, and decoded broadcasts mixed into the one feed
//! that gets encoded and published.
//!
//! Unlike `playback::Engine`, which a device callback drives, nothing here owns
//! a clock. Whoever owns the program clock calls [`Bus::read`] with as many
//! frames as that clock has advanced, so the mix stays in step with it exactly.
//! Each channel is resampled from its own rate to the bus's, and a source that
//! runs slightly fast or slow against that clock is corrected by the same
//! ring that absorbs its jitter, so it never drifts out of the mix.
//!
//! ```
//! # fn example() -> Result<(), moq_audio::Error> {
//! use moq_audio::{bus, encode};
//!
//! let bus = bus::Bus::new(bus::Config::default())?;
//! let strip = bus::Strip::default();
//! let mut mic = bus.channel(encode::Input::new(44_100, moq_audio::Layout::Mono), strip.clone())?;
//! strip.set_gain(0.5);
//!
//! // On the program clock: 10 ms of stereo at 48 kHz.
//! let mut program = vec![0.0f32; 480 * 2];
//! bus.read(&mut program);
//! # Ok(())
//! # }
//! ```
//!
//! Enabled by the default-on `bus` feature; `Bus::capture` also needs `capture`.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use fixed_resample::{ResamplerConfig, ResamplingChannelConfig, ResamplingCons, ResamplingProd, resampling_channel};

use crate::encode::Input;
use crate::resample::Remix;
use crate::{Error, Layout, Level};

/// Audio held between a [`Channel::write`] and the [`Bus::read`] that mixes it.
///
/// The jitter a late write absorbs without a dropout, and the delay every
/// source carries into the mix. Matches the playback sink's default, which
/// rides out a stalled network read without being audible as delay.
const LATENCY: Duration = Duration::from_millis(50);

/// Room above [`LATENCY`] before writes are dropped. A writer that runs ahead
/// (a decoder catching up after a stall) parks its samples here rather than
/// losing them, and the overflow correction then trims back to [`LATENCY`].
const HEADROOM: Duration = Duration::from_millis(150);

/// What the program feed is mixed to.
///
/// `#[non_exhaustive]`: construct via [`Config::default`] and set fields, so new
/// options can be added without breaking callers.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// Samples per second per channel of [`Bus::read`]'s output.
	pub sample_rate: u32,
	/// Speaker layout of [`Bus::read`]'s output. Must name speaker positions,
	/// since every channel is remixed to it.
	pub layout: Layout,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			sample_rate: 48_000,
			layout: Layout::Stereo,
		}
	}
}

/// One program mix that any number of [`Channel`]s feed.
///
/// Cheap to clone; every clone reads and feeds the same mix.
#[derive(Clone)]
pub struct Bus {
	inner: Arc<Inner>,
}

struct Inner {
	config: Config,
	mixer: Mutex<Mixer>,
	next: AtomicU64,
	level: Meter,
}

struct Mixer {
	entries: Vec<Entry>,
	/// One channel's share of the block being mixed, reused across reads.
	scratch: Vec<f32>,
}

/// A channel as the mixer sees it.
struct Entry {
	id: u64,
	cons: ResamplingCons<f32>,
	strip: Strip,
	/// Gain actually applied, ramping toward the strip's target across a block.
	applied: f32,
}

impl Bus {
	/// An empty mix producing PCM in `config`'s format.
	pub fn new(config: Config) -> Result<Self, Error> {
		if config.sample_rate == 0 {
			return Err(Error::Unsupported("bus sample rate must be > 0".into()));
		}
		if config.layout.speakers().is_none() {
			return Err(Error::Unsupported(format!(
				"a bus needs speaker positions to remix into (got {:?})",
				config.layout
			)));
		}

		Ok(Self {
			inner: Arc::new(Inner {
				config,
				mixer: Mutex::new(Mixer {
					entries: Vec::new(),
					scratch: Vec::new(),
				}),
				next: AtomicU64::new(0),
				level: Meter::default(),
			}),
		})
	}

	/// The format [`read`](Self::read) produces.
	pub fn config(&self) -> &Config {
		&self.inner.config
	}

	/// Add a source to the mix, taking PCM in the layout `input` describes and
	/// controlled by `strip`.
	///
	/// The strip is the source's identity to a UI: it outlives the channel, so a
	/// source that reopens with another format (a new decoder, a replugged
	/// device) keeps its gain, mute, and meter by passing the same strip again.
	/// The channel leaves the mix when dropped.
	pub fn channel(&self, input: Input, strip: Strip) -> Result<Channel, Error> {
		if input.sample_rate == 0 {
			return Err(Error::Unsupported("channel sample rate must be > 0".into()));
		}
		let output = self.inner.config.layout;
		let remix = if input.layout == output {
			None
		} else {
			Some(Remix::new(input.layout, output)?)
		};

		let (prod, cons) = resampling_channel::<f32>(
			output.channels() as usize,
			input.sample_rate,
			self.inner.config.sample_rate,
			// Only ever pushed interleaved, which skips the planar staging buffer.
			true,
			ResamplingChannelConfig {
				latency_seconds: LATENCY.as_secs_f64(),
				capacity_seconds: (LATENCY + HEADROOM).as_secs_f64(),
				// Correct drift against the program clock by trimming or padding
				// back to the target depth, not by letting the ring run dry or full.
				underflow_autocorrect_percent_threshold: Some(25.0),
				overflow_autocorrect_percent_threshold: Some(75.0),
				resampler_config: ResamplerConfig::default(),
				..Default::default()
			},
		);

		let id = self.inner.next.fetch_add(1, Ordering::Relaxed);
		self.inner.mixer.lock().unwrap().entries.push(Entry {
			id,
			cons,
			strip: strip.clone(),
			applied: strip.target(),
		});

		Ok(Channel {
			id,
			prod,
			remix,
			input,
			strip,
			bus: Arc::downgrade(&self.inner),
		})
	}

	/// Mix the next `output.len() / channels` frames into `output`, interleaved
	/// in the bus layout.
	///
	/// Call it as the program clock advances, with exactly that much time's worth
	/// of frames: the bus has no clock of its own, so this is what paces every
	/// channel. A channel with nothing buffered contributes silence. Any samples
	/// past the last whole frame are zeroed.
	pub fn read(&self, output: &mut [f32]) {
		output.fill(0.0);
		let channels = self.inner.config.layout.channels() as usize;
		let whole = output.len() - output.len() % channels;
		let output = &mut output[..whole];
		if output.is_empty() {
			return;
		}

		let mut mixer = self.inner.mixer.lock().unwrap();
		let Mixer { entries, scratch } = &mut *mixer;
		scratch.resize(output.len(), 0.0);
		let frames = output.len() / channels;

		for entry in entries.iter_mut() {
			entry.cons.read_interleaved(scratch, false);

			// Ramp across the block rather than stepping, so a fader move or a
			// mute never clicks.
			let target = entry.strip.target();
			let start = entry.applied;
			let step = (target - start) / frames as f32;
			for (index, frame) in scratch.chunks_exact_mut(channels).enumerate() {
				let gain = start + step * (index + 1) as f32;
				for sample in frame {
					*sample *= gain;
				}
			}
			entry.applied = target;

			entry.strip.shared.level.record(Level::measure(scratch));
			for (out, sample) in output.iter_mut().zip(scratch.iter()) {
				*out += *sample;
			}
		}
		drop(mixer);

		self.inner.level.record(Level::measure(output));
	}

	/// The loudest the mix has been since the previous call.
	pub fn level(&self) -> Level {
		self.inner.level.take()
	}

	/// Capture an input device into the mix, described by `config` and
	/// controlled by `strip`.
	///
	/// Nothing opens until [`Capture::run`] is awaited, and the device's format
	/// is whatever it offers: the channel adapts to it each time it opens, under
	/// the same strip.
	///
	/// Requires the `capture` feature.
	#[cfg(feature = "capture")]
	pub fn capture(&self, config: crate::capture::Config, strip: Strip) -> Capture {
		Capture {
			bus: self.clone(),
			config,
			strip,
		}
	}
}

/// One source's PCM on its way into a [`Bus`].
///
/// Dropping it takes the source out of the mix.
pub struct Channel {
	id: u64,
	prod: ResamplingProd<f32>,
	remix: Option<Remix>,
	input: Input,
	strip: Strip,
	bus: Weak<Inner>,
}

impl Channel {
	/// Push PCM in the format, rate, and layout this channel was created with.
	///
	/// Never blocks. Audio that arrives before the bus is first read, or that
	/// overruns the channel's buffer, is dropped to keep the mix live.
	pub fn write(&mut self, data: &[u8]) -> Result<(), Error> {
		let pcm = self
			.input
			.format
			.as_interleaved_f32(data, self.input.layout.channels())?;
		self.push(&pcm);
		Ok(())
	}

	fn push(&mut self, pcm: &[f32]) {
		match &self.remix {
			Some(remix) => {
				let _ = self.prod.push_interleaved(&remix.process(pcm));
			}
			None => {
				let _ = self.prod.push_interleaved(pcm);
			}
		}
	}

	/// This source's gain, mute, and meter.
	pub fn strip(&self) -> &Strip {
		&self.strip
	}
}

impl Drop for Channel {
	fn drop(&mut self) {
		if let Some(bus) = self.bus.upgrade() {
			bus.mixer.lock().unwrap().entries.retain(|entry| entry.id != self.id);
		}
	}
}

/// The gain, mute, and meter of one source in a [`Bus`].
///
/// Cheap to clone and `Send + Sync`: clones control the same source, so a UI
/// can hold one while a task owns the [`Channel`]. Changes apply from the next
/// [`Bus::read`], ramped across it.
#[derive(Clone, Debug, Default)]
pub struct Strip {
	shared: Arc<StripState>,
}

#[derive(Debug)]
struct StripState {
	/// Linear gain, as [`f32::to_bits`] since there is no atomic float.
	gain: AtomicU32,
	muted: AtomicBool,
	level: Meter,
}

impl Default for StripState {
	fn default() -> Self {
		Self {
			gain: AtomicU32::new(1.0f32.to_bits()),
			muted: AtomicBool::new(false),
			level: Meter::default(),
		}
	}
}

impl Strip {
	/// Set the linear gain: `1.0` is unity, `0.0` silent, above `1.0` boosts.
	///
	/// # Panics
	///
	/// If `gain` is negative or not finite.
	pub fn set_gain(&self, gain: f32) {
		assert!(gain.is_finite() && gain >= 0.0, "invalid gain {gain}");
		self.shared.gain.store(gain.to_bits(), Ordering::Relaxed);
	}

	/// The linear gain, independent of [`muted`](Self::muted).
	pub fn gain(&self) -> f32 {
		f32::from_bits(self.shared.gain.load(Ordering::Relaxed))
	}

	/// Silence this source without forgetting its gain.
	pub fn set_muted(&self, muted: bool) {
		self.shared.muted.store(muted, Ordering::Relaxed);
	}

	/// Whether this source is muted.
	pub fn muted(&self) -> bool {
		self.shared.muted.load(Ordering::Relaxed)
	}

	/// The loudest this source contributed to the mix since the previous call,
	/// after its gain and mute.
	pub fn level(&self) -> Level {
		self.shared.level.take()
	}

	fn target(&self) -> f32 {
		if self.muted() { 0.0 } else { self.gain() }
	}
}

/// The loudest [`Level`] since it was last taken.
#[derive(Debug, Default)]
struct Meter(Mutex<Level>);

impl Meter {
	fn record(&self, level: Level) {
		let mut held = self.0.lock().unwrap();
		*held = held.max(level);
	}

	fn take(&self) -> Level {
		std::mem::take(&mut *self.0.lock().unwrap())
	}
}

/// Backoff bounds for reopening a capture device, matching the capture
/// publication's: a quick first retry covers a USB device re-enumerating, and
/// the ceiling keeps a missing device from spinning.
#[cfg(feature = "capture")]
const RETRY_MIN: Duration = Duration::from_millis(500);
#[cfg(feature = "capture")]
const RETRY_MAX: Duration = Duration::from_secs(4);

/// An input device feeding a [`Bus`], built by [`Bus::capture`].
///
/// Requires the `capture` feature.
#[cfg(feature = "capture")]
pub struct Capture {
	bus: Bus,
	config: crate::capture::Config,
	strip: Strip,
}

#[cfg(feature = "capture")]
impl Capture {
	/// Capture into the mix until the device fails for good, or until the future
	/// is dropped, which releases the device.
	///
	/// A device that disappears or stops is reopened with backoff; one that can
	/// never work (a denied permission, an unsupported format, no such device)
	/// returns its error. On macOS the future is `!Send`, because the permission
	/// prompt and ScreenCaptureKit hold ObjC handles across an await, so run it
	/// on a local task there.
	pub async fn run(self) -> Result<(), Error> {
		let mut backoff = RETRY_MIN;
		loop {
			match crate::capture::open(&self.config).await {
				Ok(mut stream) => {
					let layout = stream.layout();
					let mut input = Input::new(layout.sample_rate, Layout::from_channels(layout.channels)?);
					input.format = crate::Format::F32;
					let mut channel = self.bus.channel(input, self.strip.clone())?;
					backoff = RETRY_MIN;

					loop {
						match stream.read().await {
							// A gap needs nothing here: the bus has no timeline of its
							// own to re-anchor, so the channel just plays on.
							Ok(Some(samples)) => channel.push(&samples.data),
							Ok(None) => break,
							Err(failure) if failure.is_retryable() => {
								tracing::warn!(err = %failure.into_error(), "audio capture stopped; reopening");
								break;
							}
							Err(failure) => return Err(failure.into_error()),
						}
					}
				}
				Err(failure) if failure.is_retryable() => {
					tracing::warn!(err = %failure.into_error(), ?backoff, "audio capture unavailable; retrying");
				}
				Err(failure) => return Err(failure.into_error()),
			}

			moq_clock::sleep(backoff).await;
			backoff = (backoff * 2).min(RETRY_MAX);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::Format;

	/// Interleaved bytes of `frames` frames of `channels` channels, every sample `value`.
	fn constant(value: f32, frames: usize, channels: usize) -> Vec<u8> {
		std::iter::repeat_n(value, frames * channels)
			.flat_map(f32::to_le_bytes)
			.collect()
	}

	fn input(sample_rate: u32, layout: Layout) -> Input {
		let mut input = Input::new(sample_rate, layout);
		input.format = Format::F32;
		input
	}

	/// Read blocks of `frames` until `blocks` have passed, returning the last.
	fn settle(bus: &Bus, frames: usize, blocks: usize) -> Vec<f32> {
		let mut out = vec![0.0; frames * bus.config().layout.channels() as usize];
		for _ in 0..blocks {
			bus.read(&mut out);
		}
		out
	}

	/// Writes that land before the first read are dropped, so the first read
	/// opens the channel and the test feeds it from there, one block per read the
	/// way a live source keeps pace with the program clock.
	fn feed(bus: &Bus, channel: &mut Channel, value: f32, blocks: usize) -> Vec<f32> {
		let frames = 480;
		let channels = channel.input.layout.channels() as usize;
		let in_frames = frames * channel.input.sample_rate as usize / bus.config().sample_rate as usize;
		let mut out = vec![0.0; frames * bus.config().layout.channels() as usize];
		bus.read(&mut out);
		for _ in 0..blocks {
			channel.write(&constant(value, in_frames, channels)).unwrap();
			bus.read(&mut out);
		}
		out
	}

	#[test]
	fn sums_channels_with_their_gain() {
		let bus = Bus::new(Config::default()).unwrap();
		let mut a = bus.channel(input(48_000, Layout::Stereo), Strip::default()).unwrap();
		let mut b = bus.channel(input(48_000, Layout::Stereo), Strip::default()).unwrap();
		b.strip().set_gain(0.5);

		let mut out = vec![0.0; 480 * 2];
		bus.read(&mut out);
		for _ in 0..20 {
			a.write(&constant(0.25, 480, 2)).unwrap();
			b.write(&constant(0.25, 480, 2)).unwrap();
			bus.read(&mut out);
		}

		// Past the latency fill and the gain ramp, every sample is 0.25 + 0.125.
		for sample in &out {
			assert!((sample - 0.375).abs() < 1e-4, "{sample}");
		}
	}

	#[test]
	fn mute_silences_and_keeps_the_gain() {
		let bus = Bus::new(Config::default()).unwrap();
		let mut channel = bus.channel(input(48_000, Layout::Stereo), Strip::default()).unwrap();
		channel.strip().set_gain(0.8);
		channel.strip().set_muted(true);

		let out = feed(&bus, &mut channel, 0.5, 20);
		assert!(out.iter().all(|sample| *sample == 0.0));
		assert_eq!(channel.strip().gain(), 0.8);
		assert_eq!(channel.strip().level(), Level::default());

		channel.strip().set_muted(false);
		let out = feed(&bus, &mut channel, 0.5, 20);
		assert!((out.last().unwrap() - 0.4).abs() < 1e-4);
	}

	/// A gain change ramps across one block instead of stepping, which would click.
	#[test]
	fn gain_changes_ramp() {
		let bus = Bus::new(Config::default()).unwrap();
		let mut channel = bus.channel(input(48_000, Layout::Stereo), Strip::default()).unwrap();
		feed(&bus, &mut channel, 0.5, 20);

		channel.strip().set_gain(0.0);
		channel.write(&constant(0.5, 480, 2)).unwrap();
		let mut out = vec![0.0; 480 * 2];
		bus.read(&mut out);

		let first = out[0];
		let last = out[out.len() - 1];
		assert!(first > 0.49, "the ramp starts at the old gain: {first}");
		assert!(last.abs() < 1e-6, "the ramp ends at the new gain: {last}");
		assert!(out.windows(2).all(|pair| pair[1] <= pair[0] + 1e-6), "monotonic");
	}

	/// A mono source plays from both speakers of a stereo bus, and a source at
	/// another rate is resampled to the bus's rather than played fast or slow.
	#[test]
	fn converts_layout_and_rate() {
		let bus = Bus::new(Config::default()).unwrap();
		let mut channel = bus.channel(input(16_000, Layout::Mono), Strip::default()).unwrap();
		let out = feed(&bus, &mut channel, 0.5, 40);

		for frame in out.as_chunks::<2>().0 {
			assert!((frame[0] - 0.5).abs() < 1e-2, "{frame:?}");
			assert!((frame[1] - 0.5).abs() < 1e-2, "{frame:?}");
		}
	}

	/// A source that stops writing leaves silence, not a repeat of its last block.
	#[test]
	fn a_stalled_source_is_silent() {
		let bus = Bus::new(Config::default()).unwrap();
		let mut channel = bus.channel(input(48_000, Layout::Stereo), Strip::default()).unwrap();
		feed(&bus, &mut channel, 0.5, 20);

		let out = settle(&bus, 480, 20);
		assert!(out.iter().all(|sample| *sample == 0.0));
	}

	#[test]
	fn dropping_a_channel_leaves_the_mix() {
		let bus = Bus::new(Config::default()).unwrap();
		let mut kept = bus.channel(input(48_000, Layout::Stereo), Strip::default()).unwrap();
		let mut dropped = bus.channel(input(48_000, Layout::Stereo), Strip::default()).unwrap();
		feed(&bus, &mut dropped, 0.5, 5);
		drop(dropped);

		let out = feed(&bus, &mut kept, 0.25, 20);
		assert!((out.last().unwrap() - 0.25).abs() < 1e-4);
	}

	/// The meter holds the loudest reading until it is read, so a UI polling
	/// slower than the program clock still sees a peak.
	#[test]
	fn levels_hold_until_read() {
		let bus = Bus::new(Config::default()).unwrap();
		let mut channel = bus.channel(input(48_000, Layout::Stereo), Strip::default()).unwrap();
		feed(&bus, &mut channel, 0.5, 20);
		settle(&bus, 480, 5);

		let level = channel.strip().level();
		assert!((level.peak() - 0.5).abs() < 1e-3, "{level:?}");
		assert_eq!(channel.strip().level(), Level::default(), "taken");
		assert!((bus.level().peak() - 0.5).abs() < 1e-3);
	}

	/// A source that reopens in another format keeps its controls by passing the
	/// same strip again, which is what a decoder restart or a replugged device does.
	#[test]
	fn a_strip_outlives_its_channel() {
		let bus = Bus::new(Config::default()).unwrap();
		let strip = Strip::default();
		strip.set_gain(0.5);
		let first = bus.channel(input(48_000, Layout::Stereo), strip.clone()).unwrap();
		drop(first);

		let mut second = bus.channel(input(16_000, Layout::Mono), strip.clone()).unwrap();
		let out = feed(&bus, &mut second, 0.5, 40);
		assert!((out.last().unwrap() - 0.25).abs() < 1e-2, "{}", out.last().unwrap());
		assert!(strip.level().peak() > 0.2);
	}

	#[test]
	fn refuses_a_layout_without_speakers() {
		let config = Config {
			layout: Layout::Discrete(2),
			..Default::default()
		};
		assert!(Bus::new(config).is_err());

		let bus = Bus::new(Config::default()).unwrap();
		assert!(
			bus.channel(input(48_000, Layout::Discrete(3)), Strip::default())
				.is_err()
		);
	}

	/// A partial trailing frame in the output is zeroed rather than left with
	/// whatever the caller had there.
	#[test]
	fn zeroes_a_partial_frame() {
		let bus = Bus::new(Config::default()).unwrap();
		let mut out = vec![1.0; 5];
		bus.read(&mut out);
		assert!(out.iter().all(|sample| *sample == 0.0));
	}
}
