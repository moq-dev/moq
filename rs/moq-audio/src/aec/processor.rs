use std::time::Duration;

use sonora::{AudioProcessing, StreamConfig};

use super::Config;
use crate::{Error, Layout};

/// Callback headroom retained from the built-in device path.
const MAX_CALLBACK_FRAMES: usize = 4096;

/// Format of one interleaved PCM stream processed by echo cancellation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Stream {
	/// Samples per channel per second: a multiple of 100 Hz from 8 to 384 kHz,
	/// so every 10 ms processing frame is a whole number of samples.
	pub sample_rate: u32,
	/// Mono or stereo channel layout.
	pub layout: Layout,
}

impl Stream {
	/// Describe an interleaved PCM stream.
	pub fn new(sample_rate: u32, layout: Layout) -> Self {
		Self { sample_rate, layout }
	}

	fn validate(self, role: &str) -> Result<StreamConfig, Error> {
		if !(8_000..=384_000).contains(&self.sample_rate) {
			return Err(Error::Unsupported(format!(
				"echo cancellation needs a {role} sample rate between 8 and 384 kHz (got {})",
				self.sample_rate
			)));
		}
		// Sonora consumes `rate / 100` samples as each 10 ms frame. At a rate such as
		// 22,050 Hz that frame is short, so the stream would drift against the other.
		if !self.sample_rate.is_multiple_of(100) {
			return Err(Error::Unsupported(format!(
				"echo cancellation needs a {role} sample rate that is a multiple of 100 Hz (got {})",
				self.sample_rate
			)));
		}

		let channels = self.layout.channels();
		if channels == 0 || channels > 2 {
			return Err(Error::Unsupported(format!(
				"echo cancellation accepts mono or stereo {role} PCM (got {channels} channels)"
			)));
		}

		Ok(StreamConfig::new(self.sample_rate, channels as u16))
	}
}

/// Echo-cancellation measurements, with `None` for unavailable estimates.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Stats {
	/// Echo return loss in decibels.
	pub echo_return_loss: Option<f64>,
	/// Additional echo return loss from processing, in decibels.
	pub echo_return_loss_enhancement: Option<f64>,
	/// Fraction of the latest aggregation window with a divergent filter.
	pub divergent_filter_fraction: Option<f64>,
	/// Median estimated echo-path delay.
	pub delay_median: Option<Duration>,
	/// Standard deviation of the estimated echo-path delay.
	pub delay_standard_deviation: Option<Duration>,
	/// Current estimated echo-path delay.
	pub delay: Option<Duration>,
	/// Current residual echo likelihood from zero to one.
	pub residual_echo_likelihood: Option<f64>,
	/// Maximum recent residual echo likelihood from zero to one.
	pub residual_echo_likelihood_recent_max: Option<f64>,
}

impl Stats {
	fn from_sonora(stats: &sonora::AudioProcessingStats) -> Self {
		Self {
			echo_return_loss: stats.echo_return_loss,
			echo_return_loss_enhancement: stats.echo_return_loss_enhancement,
			divergent_filter_fraction: stats.divergent_filter_fraction,
			delay_median: duration(stats.delay_median_ms),
			delay_standard_deviation: duration(stats.delay_standard_deviation_ms),
			delay: duration(stats.delay_ms),
			residual_echo_likelihood: stats.residual_echo_likelihood,
			residual_echo_likelihood_recent_max: stats.residual_echo_likelihood_recent_max,
		}
	}
}

/// Device-independent acoustic echo processor for interleaved `f32` PCM.
///
/// Give one owner every playback buffer through [`Processor::process_render`]
/// before the microphone buffer that can contain its echo. Buffers may be any
/// channel-aligned size. Samples outside `[-1.0, 1.0]` are clipped and
/// non-finite samples are rejected.
pub struct Processor {
	config: Config,
	render: Stream,
	capture: Stream,
	inner: AudioProcessing,
	stream_delay: Duration,
	render_pending: Vec<f32>,
	capture_pending: Vec<f32>,
	capture_processed: Vec<f32>,
	render_in: Vec<f32>,
	render_out: Vec<f32>,
	capture_in: Vec<f32>,
	capture_out: Vec<f32>,
}

impl Processor {
	/// Build a processor for fixed playback and microphone formats.
	pub fn new(config: Config, render: Stream, capture: Stream) -> Result<Self, Error> {
		let render_config = render.validate("playback")?;
		let capture_config = capture.validate("microphone")?;
		let inner = build(&config, render_config, capture_config);

		let render_samples = render_config.num_samples();
		let capture_samples = capture_config.num_samples();
		Ok(Self {
			config,
			render,
			capture,
			inner,
			stream_delay: Duration::ZERO,
			render_pending: Vec::with_capacity(render_samples),
			capture_pending: Vec::with_capacity(
				capture_samples + MAX_CALLBACK_FRAMES * capture.layout.channels() as usize,
			),
			capture_processed: Vec::with_capacity(
				capture_samples + MAX_CALLBACK_FRAMES * capture.layout.channels() as usize,
			),
			render_in: vec![0.0; render_samples],
			render_out: vec![0.0; render_samples],
			capture_in: vec![0.0; capture_samples],
			capture_out: vec![0.0; capture_samples],
		})
	}

	/// Feed chronological playback PCM into the echo model before capture.
	pub fn process_render(&mut self, samples: &[f32]) -> Result<(), Error> {
		validate_samples(samples, self.render.layout.channels() as usize)?;

		let Self {
			inner,
			render,
			render_pending,
			render_in,
			render_out,
			..
		} = self;
		let config = stream_config(*render);
		let stride = config.num_samples();
		let mut remaining = samples;

		if !render_pending.is_empty() {
			let needed = stride - render_pending.len();
			let take = needed.min(remaining.len());
			render_pending.extend_from_slice(&remaining[..take]);
			remaining = &remaining[take..];
			if render_pending.len() == stride {
				process_frame(inner, &config, render_pending, render_in, render_out, true);
				render_pending.clear();
			}
		}

		let mut frames = remaining.chunks_exact(stride);
		for frame in &mut frames {
			process_frame(inner, &config, frame, render_in, render_out, true);
		}
		render_pending.extend_from_slice(frames.remainder());
		Ok(())
	}

	/// Replace microphone PCM with the same span after echo processing.
	///
	/// Buffers may contain any whole number of interleaved samples. A partial
	/// 10 ms processing frame is retained, so the first irregular buffer can end
	/// with silence and later calls recover the same number of samples.
	pub fn process_capture(&mut self, samples: &mut [f32]) -> Result<(), Error> {
		validate_samples(samples, self.capture.layout.channels() as usize)?;

		let Self {
			inner,
			capture,
			capture_pending,
			capture_processed,
			capture_in,
			capture_out,
			..
		} = self;
		let config = stream_config(*capture);
		let stride = config.num_samples();
		capture_pending.extend_from_slice(samples);
		let complete = capture_pending.len() / stride * stride;

		for frame in capture_pending[..complete].chunks_exact(stride) {
			process_frame(inner, &config, frame, capture_in, capture_out, false);
			interleave(capture_out, config.num_channels() as usize, capture_processed);
		}
		capture_pending.drain(..complete);

		let ready = capture_processed.len().min(samples.len());
		samples[..ready].copy_from_slice(&capture_processed[..ready]);
		samples[ready..].fill(0.0);
		capture_processed.drain(..ready);
		Ok(())
	}

	/// Set the render-to-capture delay with millisecond precision.
	pub fn set_stream_delay(&mut self, delay: Duration) -> Result<(), Error> {
		if delay > Duration::from_millis(500) || !delay.as_nanos().is_multiple_of(1_000_000) {
			return Err(Error::Unsupported(format!(
				"echo cancellation delay must be a whole number of milliseconds from 0 through 500 (got {delay:?})"
			)));
		}

		self.inner
			.set_stream_delay_ms(delay.as_millis() as i32)
			.expect("the delay is within the supported range");
		self.stream_delay = delay;
		Ok(())
	}

	/// Return the configured render-to-capture delay.
	pub fn stream_delay(&self) -> Duration {
		self.stream_delay
	}

	#[cfg(feature = "aec")]
	pub(super) fn capture(&self) -> Stream {
		self.capture
	}

	/// Return the latest echo-cancellation measurements.
	pub fn statistics(&self) -> Stats {
		Stats::from_sonora(self.inner.statistics())
	}

	/// Forget adaptive and partial-frame state after a stream discontinuity.
	pub fn reset(&mut self) {
		let render = stream_config(self.render);
		let capture = stream_config(self.capture);
		self.inner = build(&self.config, render, capture);
		self.inner
			.set_stream_delay_ms(self.stream_delay.as_millis() as i32)
			.expect("a previously accepted delay remains valid");
		self.clear_buffers();
	}

	pub(super) fn clear_buffers(&mut self) {
		self.render_pending.clear();
		self.capture_pending.clear();
		self.capture_processed.clear();
	}

	#[cfg(all(test, feature = "aec"))]
	pub(super) fn pending_samples(&self) -> usize {
		self.capture_pending.len()
	}

	#[cfg(all(test, feature = "aec"))]
	pub(super) fn processed_samples(&self) -> usize {
		self.capture_processed.len()
	}
}

impl std::fmt::Debug for Processor {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Processor")
			.field("render", &self.render)
			.field("capture", &self.capture)
			.field("stream_delay", &self.stream_delay)
			.finish_non_exhaustive()
	}
}

fn build(config: &Config, render: StreamConfig, capture: StreamConfig) -> AudioProcessing {
	AudioProcessing::builder()
		.capture_config(capture)
		.render_config(render)
		.config(config.build())
		.echo_detector(config.residual_echo_detection)
		.build()
}

fn stream_config(stream: Stream) -> StreamConfig {
	StreamConfig::new(stream.sample_rate, stream.layout.channels() as u16)
}

fn validate_samples(samples: &[f32], channels: usize) -> Result<(), Error> {
	if !samples.len().is_multiple_of(channels) {
		return Err(Error::Misaligned {
			got: samples.len(),
			expected: samples.len().next_multiple_of(channels),
		});
	}
	if samples.iter().any(|sample| !sample.is_finite()) {
		return Err(Error::Unsupported(
			"echo cancellation PCM samples must be finite".into(),
		));
	}
	Ok(())
}

fn process_frame(
	processor: &mut AudioProcessing,
	config: &StreamConfig,
	input: &[f32],
	planar_in: &mut [f32],
	planar_out: &mut [f32],
	render: bool,
) {
	let channels = config.num_channels() as usize;
	deinterleave(input, planar_in, channels);
	let result = match channels {
		1 if render => processor.process_render_f32_with_config(&[planar_in], config, config, &mut [planar_out]),
		1 => processor.process_capture_f32_with_config(&[planar_in], config, config, &mut [planar_out]),
		_ => {
			let frame = config.num_frames();
			let (left, right) = planar_in.split_at(frame);
			let (left_out, right_out) = planar_out.split_at_mut(frame);
			if render {
				processor.process_render_f32_with_config(&[left, right], config, config, &mut [left_out, right_out])
			} else {
				processor.process_capture_f32_with_config(&[left, right], config, config, &mut [left_out, right_out])
			}
		}
	};
	result.expect("stream formats were validated at construction");
}

fn duration(milliseconds: Option<i32>) -> Option<Duration> {
	milliseconds.and_then(|value| u64::try_from(value).ok().map(Duration::from_millis))
}

fn deinterleave(src: &[f32], dest: &mut [f32], channels: usize) {
	let frames = src.len() / channels;
	for (channel, samples) in dest.chunks_exact_mut(frames).enumerate() {
		for (frame, sample) in samples.iter_mut().enumerate() {
			*sample = src[frame * channels + channel].clamp(-1.0, 1.0);
		}
	}
}

fn interleave(src: &[f32], channels: usize, dest: &mut Vec<f32>) {
	let frames = src.len() / channels;
	for frame in 0..frames {
		for channel in 0..channels {
			dest.push(src[channel * frames + frame]);
		}
	}
}

#[cfg(test)]
mod tests {
	use std::collections::VecDeque;

	use super::*;

	fn processor() -> Processor {
		Processor::new(
			Config {
				noise_suppression: false,
				auto_gain: false,
				..Default::default()
			},
			Stream::new(48_000, Layout::Stereo),
			Stream::new(48_000, Layout::Mono),
		)
		.unwrap()
	}

	#[test]
	fn accepts_irregular_pcm_buffers() {
		let mut processor = processor();
		for frames in [64, 480, 511, 4096] {
			processor.process_render(&vec![0.0; frames * 2]).unwrap();
			let mut capture = vec![0.25; frames];
			processor.process_capture(&mut capture).unwrap();
			assert_eq!(capture.len(), frames);
		}
	}

	#[test]
	fn rejects_invalid_formats_and_samples() {
		assert!(
			Processor::new(
				Config::default(),
				Stream::new(4_000, Layout::Mono),
				Stream::new(48_000, Layout::Mono),
			)
			.is_err()
		);
		assert!(
			Processor::new(
				Config::default(),
				Stream::new(48_000, Layout::FivePointOne),
				Stream::new(48_000, Layout::Mono),
			)
			.is_err()
		);
		assert!(
			Processor::new(
				Config::default(),
				Stream::new(22_050, Layout::Mono),
				Stream::new(48_000, Layout::Mono),
			)
			.is_err()
		);
		assert!(
			Processor::new(
				Config::default(),
				Stream::new(48_000, Layout::Stereo),
				Stream::new(22_050, Layout::Mono),
			)
			.is_err()
		);

		let mut processor = processor();
		assert!(processor.process_render(&[0.0, 0.0, 0.0]).is_err());
		assert!(processor.process_render(&[f32::NAN, 0.0]).is_err());
		let mut capture = [f32::INFINITY];
		assert!(processor.process_capture(&mut capture).is_err());
		let mut clipped = [1.5; 480];
		processor.process_capture(&mut clipped).unwrap();
		assert!(clipped.iter().all(|sample| *sample <= 1.0));
	}

	#[test]
	fn delay_is_typed_and_bounded() {
		let mut processor = processor();
		processor.set_stream_delay(Duration::from_millis(140)).unwrap();
		assert_eq!(processor.stream_delay(), Duration::from_millis(140));
		assert!(processor.set_stream_delay(Duration::from_micros(1)).is_err());
		assert!(processor.set_stream_delay(Duration::from_millis(501)).is_err());
		processor.reset();
		assert_eq!(processor.stream_delay(), Duration::from_millis(140));
	}

	#[test]
	fn cancels_caller_supplied_echo() {
		const DELAY: usize = 2;
		const ATTENUATION: f32 = 0.5;
		let mut processor = processor();
		let mut noise = Noise::default();
		let mut history = VecDeque::new();
		let mut heard = 0.0;
		let mut left = 0.0;

		for round in 0..200 {
			let played: Vec<f32> = (0..480).map(|_| noise.next()).collect();
			let render: Vec<f32> = played.iter().flat_map(|sample| [*sample, *sample]).collect();
			processor.process_render(&render).unwrap();
			history.push_back(played);
			if history.len() <= DELAY {
				continue;
			}

			let mut echo: Vec<f32> = history
				.pop_front()
				.unwrap()
				.into_iter()
				.map(|sample| sample * ATTENUATION)
				.collect();
			let before = energy(&echo);
			processor.process_capture(&mut echo).unwrap();
			if round >= 150 {
				heard += before;
				left += energy(&echo);
			}
		}

		let attenuation = 10.0 * (heard / left.max(f64::MIN_POSITIVE)).log10();
		assert!(attenuation > 20.0, "echo was only attenuated by {attenuation:.1} dB");
	}

	fn energy(samples: &[f32]) -> f64 {
		samples.iter().map(|sample| (*sample as f64).powi(2)).sum()
	}

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
			self.0 as f32 / u32::MAX as f32 - 0.5
		}
	}
}
