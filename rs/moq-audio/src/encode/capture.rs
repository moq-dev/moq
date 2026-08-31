//! Capture audio on demand and publish it as an encoded track.
//!
//! The turnkey entry point, mirroring `moq_video::encode::publish_capture`: the
//! capture-side settings come from [`capture::Config`](crate::capture::Config),
//! the encode-side settings from [`Options`], and the input PCM layout is read
//! off the source rather than declared by the caller.

use std::time::Duration;

use rand::RngExt;

use super::{Input, Options, Producer};
use crate::capture;
use crate::resample::{Resampler, remix, validate_channels};
use crate::{Error, Format, Frame};

/// Backoff bounds for reopening a capture source. The quick first retry covers
/// a USB device re-enumerating; the ceiling keeps a missing device from spinning.
const RETRY_MIN: Duration = Duration::from_millis(500);
const RETRY_MAX: Duration = Duration::from_secs(4);

/// Capture audio on demand and publish it as an encoded moq track.
///
/// The catalog rendition is registered up front from the source's reported
/// format (no capture needed), but the device only opens while a subscriber is
/// listening and is released when the last one leaves. On resume the timeline
/// re-anchors (via [`Producer::reset_epoch`]) so the idle gap lands in the PTS,
/// keeping audio aligned with a wall-clock video track. A buffer dropped because
/// the encoder fell behind re-anchors the same way, so the loss surfaces as a
/// skip rather than as drift.
///
/// If an active device fails, it is dropped and reopened behind the same track
/// with capped jittered backoff. A default microphone is resolved again on every
/// attempt, and each replacement's native layout is converted to the layout
/// already registered in the catalog. Recovery stops as soon as the track becomes unused.
/// Transient failures during initial format discovery use the same retry policy
/// before the catalog track is registered.
///
/// Frames are stamped from `clock`, so passing the same [`Clock`](moq_mux::Clock)
/// to a concurrent video publish keeps the two tracks aligned. Returns when the
/// broadcast is dropped or the capture loop fails.
pub async fn publish_capture(
	mut broadcast: moq_net::broadcast::Producer,
	catalog: moq_mux::catalog::Producer,
	capture: capture::Config,
	encode: Options,
	clock: moq_mux::Clock,
) -> Result<(), Error> {
	let mut supervisor = Supervisor::default();
	let layout = supervisor.discover(&mut DeviceSource { config: &capture }).await?;
	let input = Input {
		format: Format::F32,
		sample_rate: layout.sample_rate,
		channels: layout.channels,
	};

	let mut producer = Producer::new(&mut broadcast, catalog, input, &encode)?;
	let track = producer.track().clone();

	let mut source = DeviceSource { config: &capture };
	let mut demand = TrackDemand { track: &track };
	let mut output = EncoderOutput {
		producer: &mut producer,
		clock: &clock,
	};
	let result = supervisor.run(&mut source, &mut demand, &mut output).await;

	// Best-effort clean close: flush the trailing sub-frame and finalize the
	// track. Runs only when the loop ends on its own; a Ctrl+C cancels the future
	// before this point, since async `Drop` can't finalize the track.
	if let Err(err) = producer.finish() {
		tracing::debug!(error = %err, "audio track finish after capture ended");
	}
	result
}

/// A capture backend as the supervisor sees it. Kept separate from cpal so the
/// retry and cancellation lifecycle can be tested without audio hardware.
trait CaptureSource {
	type Stream;

	async fn format(&mut self) -> Result<capture::Layout, capture::Failure>;
	async fn open(&mut self) -> Result<Self::Stream, capture::Failure>;
	fn layout(&self, stream: &Self::Stream) -> capture::Layout;
	async fn read(&mut self, stream: &mut Self::Stream) -> Result<Option<capture::Samples>, capture::Failure>;
}

struct DeviceSource<'a> {
	config: &'a capture::Config,
}

impl CaptureSource for DeviceSource<'_> {
	type Stream = capture::Stream;

	async fn format(&mut self) -> Result<capture::Layout, capture::Failure> {
		capture::format(self.config).await
	}

	async fn open(&mut self) -> Result<Self::Stream, capture::Failure> {
		capture::open(self.config).await
	}

	fn layout(&self, stream: &Self::Stream) -> capture::Layout {
		stream.layout()
	}

	async fn read(&mut self, stream: &mut Self::Stream) -> Result<Option<capture::Samples>, capture::Failure> {
		stream.read().await
	}
}

/// Demand for the published track. `false` means the track itself ended.
trait Demand {
	async fn used(&mut self) -> bool;
	async fn unused(&mut self) -> bool;
}

struct TrackDemand<'a> {
	track: &'a moq_net::track::Producer,
}

impl Demand for TrackDemand<'_> {
	async fn used(&mut self) -> bool {
		match self.track.used().await {
			Ok(()) => true,
			Err(err) => {
				log_track_ended(err);
				false
			}
		}
	}

	async fn unused(&mut self) -> bool {
		match self.track.unused().await {
			Ok(()) => true,
			Err(err) => {
				log_track_ended(err);
				false
			}
		}
	}
}

/// The stable producer the supervisor writes through across device replacements.
trait Output {
	fn reset_epoch(&mut self);
	fn now(&self) -> u64;
	fn write(&mut self, samples: capture::Samples, timestamp_us: u64) -> Result<(), Error>;
}

struct EncoderOutput<'a> {
	producer: &'a mut Producer,
	clock: &'a moq_mux::Clock,
}

impl Output for EncoderOutput<'_> {
	fn reset_epoch(&mut self) {
		self.producer.reset_epoch();
	}

	fn now(&self) -> u64 {
		self.clock.micros()
	}

	fn write(&mut self, samples: capture::Samples, timestamp_us: u64) -> Result<(), Error> {
		self.producer.write(&frame(&samples.data, timestamp_us)?)
	}
}

/// Converts one opened stream's native layout into the producer's fixed input
/// layout. A new instance per open keeps filter state out of recovery gaps.
struct Converter {
	input: capture::Layout,
	output: capture::Layout,
	resampler: Option<Resampler>,
	anchor_us: Option<u64>,
}

impl Converter {
	fn new(input: capture::Layout, output: capture::Layout) -> Result<Self, Error> {
		if input.channels != output.channels {
			validate_channels(input.channels)?;
			validate_channels(output.channels)?;
		}

		let resampler = if input.sample_rate == output.sample_rate {
			None
		} else {
			// Ten milliseconds bounds recovery buffering while giving rubato a
			// useful window independent of the device callback size.
			let chunk_frames = (input.sample_rate as usize / 100).max(1);
			Some(Resampler::new(
				input.sample_rate,
				output.sample_rate,
				input.channels,
				chunk_frames,
			)?)
		};

		Ok(Self {
			input,
			output,
			resampler,
			anchor_us: None,
		})
	}

	fn reset(&mut self) {
		if let Some(resampler) = self.resampler.as_mut() {
			resampler.reset();
		}
		self.anchor_us = None;
	}

	/// Return converted samples plus the timestamp of the first input buffered
	/// into them. The timestamp preserves the epoch when resampling spans reads.
	fn process(
		&mut self,
		mut samples: capture::Samples,
		timestamp_us: u64,
	) -> Result<Option<(capture::Samples, u64)>, Error> {
		if samples.gap {
			self.reset();
		}
		if self.anchor_us.is_none() && !samples.data.is_empty() {
			self.anchor_us = Some(timestamp_us);
		}

		if let Some(resampler) = self.resampler.as_mut() {
			let data = resampler.process(&samples.data)?;
			samples.replace(data);
		}
		if self.input.channels != self.output.channels {
			let data = remix(&samples.data, self.input.channels, self.output.channels)?;
			samples.replace(data);
		}
		if samples.data.is_empty() {
			return Ok(None);
		}

		Ok(Some((samples, self.anchor_us.take().unwrap_or(timestamp_us))))
	}
}

struct Supervisor {
	next: Duration,
	jitter: fn(Duration) -> Duration,
	layout: Option<capture::Layout>,
}

impl Default for Supervisor {
	fn default() -> Self {
		Self {
			next: RETRY_MIN,
			jitter: |delay| delay.mul_f64(0.5 + rand::rng().random::<f64>() / 2.0),
			layout: None,
		}
	}
}

impl Supervisor {
	#[cfg(test)]
	fn exact() -> Self {
		Self {
			next: RETRY_MIN,
			jitter: std::convert::identity,
			layout: Some(capture::Layout {
				sample_rate: 48_000,
				channels: 2,
			}),
		}
	}

	fn reset(&mut self) {
		self.next = RETRY_MIN;
	}

	fn advance(&mut self) -> Duration {
		let wait = (self.jitter)(self.next);
		self.next = (self.next * 2).min(RETRY_MAX);
		wait
	}

	/// Discover the source format, retrying failures that can clear when the
	/// device or host state changes.
	async fn discover<S: CaptureSource>(&mut self, source: &mut S) -> Result<capture::Layout, Error> {
		loop {
			let failure = match source.format().await {
				Ok(format) => {
					self.reset();
					self.layout = Some(format);
					return Ok(format);
				}
				Err(failure) if failure.is_retryable() => failure.into_error(),
				Err(failure) => return Err(failure.into_error()),
			};

			tracing::warn!(error = %failure, "audio capture format unavailable");
			tokio::time::sleep(self.advance()).await;
		}
	}

	/// Open the source while a listener is subscribed, release it when the last
	/// one leaves, and rebuild a failed source behind the same producer.
	///
	/// Cancel safety: every wait is a real `.await` (a buffer read or a demand
	/// transition), so dropping this future (e.g. on Ctrl+C) drops the input and
	/// stops the underlying stream. No blocking thread is left behind.
	async fn run<S, D, O>(&mut self, source: &mut S, demand: &mut D, output: &mut O) -> Result<(), Error>
	where
		S: CaptureSource,
		D: Demand,
		O: Output,
	{
		let output_layout = self.layout.expect("capture format must be discovered before running");
		'demand: loop {
			// Idle until a listener subscribes; the track ending is a clean exit.
			if !demand.used().await {
				return Ok(());
			}

			let mut last_error = None;
			self.reset();

			loop {
				// Opening waits for the first buffer, so race it against demand too. A
				// cancelled open drops the half-built stream and its callback closures.
				let opened = tokio::select! {
					biased;
					unused = demand.unused() => {
						if !unused {
							return match last_error {
								Some(err) => Err(err),
								None => Ok(()),
							};
						}
						continue 'demand;
					}
					opened = source.open() => opened,
				};

				let failure = match opened {
					Ok(mut input) => {
						let mut converter = Converter::new(source.layout(&input), output_layout)?;
						loop {
							// Demand wins over a simultaneous buffer or error, so an unused
							// track releases the device without starting a retry sequence.
							let samples = tokio::select! {
								biased;
								unused = demand.unused() => {
									drop(input);
									output.reset_epoch();
									if !unused {
										return match last_error {
											Some(err) => Err(err),
											None => Ok(()),
										};
									}
									tracing::info!("no listeners: released audio capture");
									continue 'demand;
								}
								samples = source.read(&mut input) => samples,
							};

							match samples {
								Ok(Some(samples)) => {
									// An open is not a recovery until it actually delivers audio.
									// Otherwise a flapping device would reset its backoff after each
									// empty stream and retry at the minimum delay forever.
									if last_error.take().is_some() {
										self.reset();
										tracing::info!("audio capture recovered");
									}
									// A bounded-queue drop is a real hole in the timeline.
									if samples.gap {
										output.reset_epoch();
									}
									if let Some((samples, timestamp_us)) = converter.process(samples, output.now())? {
										output.write(samples, timestamp_us)?;
									}
								}
								Ok(None) => {
									break capture::Failure::retry(Error::Capture(
										"audio capture stream stopped".into(),
									));
								}
								Err(err) => break err,
							}
						}
					}
					Err(err) => err,
				};
				if !failure.is_retryable() {
					return Err(failure.into_error());
				}
				let failure = failure.into_error();

				// The failed stream was dropped by the match above. Reset before waiting
				// so a publication that ends during recovery cannot flush stale samples.
				output.reset_epoch();
				tracing::warn!(error = %failure, "audio capture unavailable");
				last_error = Some(failure);

				let wait = self.advance();
				tokio::select! {
					biased;
					unused = demand.unused() => {
						if !unused {
							return Err(last_error.expect("a failed capture has an error"));
						}
						continue 'demand;
					}
					_ = tokio::time::sleep(wait) => {}
				}
			}
		}
	}
}

/// A dropped or closed track is the normal end of a publish; any other cause is
/// a real abort (e.g. a transport reset) worth surfacing rather than treating as
/// a clean exit.
fn log_track_ended(err: moq_net::Error) {
	if matches!(err, moq_net::Error::Dropped | moq_net::Error::Closed) {
		tracing::debug!("audio track no longer announced; stopping capture");
	} else {
		tracing::warn!(error = %err, "audio track aborted; stopping capture");
	}
}

/// Pack interleaved `f32` samples into a timestamped [`Frame`] of little-endian
/// bytes (i.e. [`Format::F32`]).
fn frame(samples: &[f32], timestamp_us: u64) -> Result<Frame, Error> {
	let mut bytes = Vec::with_capacity(std::mem::size_of_val(samples));
	for sample in samples {
		bytes.extend_from_slice(&sample.to_le_bytes());
	}
	Ok(Frame {
		timestamp: moq_net::Timestamp::from_micros(timestamp_us)?,
		data: bytes.into(),
	})
}

#[cfg(test)]
mod tests {
	use std::collections::VecDeque;
	use std::future::Future;
	use std::pin::Pin;
	use std::sync::{
		Arc,
		atomic::{AtomicUsize, Ordering},
	};
	use std::task::Poll;

	use super::*;

	struct MockStream {
		events: kio::Queue<Result<capture::Samples, capture::Failure>>,
		drops: Option<Arc<AtomicUsize>>,
		layout: capture::Layout,
	}

	impl Drop for MockStream {
		fn drop(&mut self) {
			if let Some(drops) = &self.drops {
				drops.fetch_add(1, Ordering::SeqCst);
			}
		}
	}

	enum Open {
		Error(&'static str),
		Fatal(&'static str),
		Stream(MockStream),
	}

	enum Discovery {
		Error(&'static str),
		Fatal(&'static str),
		Format(u32, u32),
	}

	struct MockSource {
		formats: VecDeque<Discovery>,
		format_attempts: Arc<AtomicUsize>,
		opens: VecDeque<Open>,
		attempts: Arc<AtomicUsize>,
		fallback_error: bool,
	}

	impl CaptureSource for MockSource {
		type Stream = MockStream;

		async fn format(&mut self) -> Result<capture::Layout, capture::Failure> {
			self.format_attempts.fetch_add(1, Ordering::SeqCst);
			match self.formats.pop_front() {
				Some(Discovery::Error(message)) => Err(capture::Failure::retry(Error::Capture(message.into()))),
				Some(Discovery::Fatal(message)) => Err(capture::Failure::fatal(Error::Capture(message.into()))),
				Some(Discovery::Format(sample_rate, channels)) => Ok(capture::Layout { sample_rate, channels }),
				None => std::future::pending().await,
			}
		}

		async fn open(&mut self) -> Result<Self::Stream, capture::Failure> {
			self.attempts.fetch_add(1, Ordering::SeqCst);
			match self.opens.pop_front() {
				Some(Open::Error(message)) => Err(capture::Failure::retry(Error::Capture(message.into()))),
				Some(Open::Fatal(message)) => Err(capture::Failure::fatal(Error::Capture(message.into()))),
				Some(Open::Stream(stream)) => Ok(stream),
				None if self.fallback_error => Err(capture::Failure::retry(Error::Capture("still unavailable".into()))),
				None => std::future::pending().await,
			}
		}

		fn layout(&self, stream: &Self::Stream) -> capture::Layout {
			stream.layout
		}

		async fn read(&mut self, stream: &mut Self::Stream) -> Result<Option<capture::Samples>, capture::Failure> {
			match stream.events.pop().await {
				Ok(Ok(samples)) => Ok(Some(samples)),
				Ok(Err(err)) => Err(err),
				Err(_) => Ok(None),
			}
		}
	}

	struct MockDemand {
		state: kio::Consumer<bool>,
	}

	impl MockDemand {
		async fn wait(&mut self, value: bool) -> bool {
			self.state
				.wait(|state| {
					if **state == value {
						Poll::Ready(())
					} else {
						Poll::Pending
					}
				})
				.await
				.is_ok()
		}
	}

	impl Demand for MockDemand {
		async fn used(&mut self) -> bool {
			self.wait(true).await
		}

		async fn unused(&mut self) -> bool {
			self.wait(false).await
		}
	}

	#[derive(Debug, PartialEq, Eq)]
	enum OutputEvent {
		Reset,
		Write(Vec<u32>),
	}

	#[derive(Default)]
	struct MockOutput {
		events: Vec<OutputEvent>,
	}

	impl Output for MockOutput {
		fn reset_epoch(&mut self) {
			self.events.push(OutputEvent::Reset);
		}

		fn now(&self) -> u64 {
			0
		}

		fn write(&mut self, samples: capture::Samples, _timestamp_us: u64) -> Result<(), Error> {
			self.events.push(OutputEvent::Write(
				samples.data.iter().copied().map(f32::to_bits).collect(),
			));
			Ok(())
		}
	}

	fn source(opens: impl IntoIterator<Item = Open>, fallback_error: bool) -> MockSource {
		MockSource {
			formats: [Discovery::Format(48_000, 2)].into_iter().collect(),
			format_attempts: Arc::new(AtomicUsize::new(0)),
			opens: opens.into_iter().collect(),
			attempts: Arc::new(AtomicUsize::new(0)),
			fallback_error,
		}
	}

	#[tokio::test(start_paused = true)]
	async fn initial_discovery_retries_a_missing_device() {
		let mut source = source([], false);
		source.formats = [
			Discovery::Error("no default input device"),
			Discovery::Format(48_000, 2),
		]
		.into_iter()
		.collect();
		let attempts = source.format_attempts.clone();
		let mut supervisor = Supervisor::exact();
		let future = supervisor.discover(&mut source);
		tokio::pin!(future);

		poll_pending(future.as_mut()).await;
		assert_eq!(attempts.load(Ordering::SeqCst), 1);
		tokio::time::advance(Duration::from_millis(500)).await;

		assert_eq!(
			future.await.unwrap(),
			capture::Layout {
				sample_rate: 48_000,
				channels: 2,
			}
		);
		assert_eq!(attempts.load(Ordering::SeqCst), 2);
	}

	#[tokio::test]
	async fn initial_discovery_returns_a_permanent_failure() {
		let mut source = source([], false);
		source.formats = [Discovery::Fatal("permission denied")].into_iter().collect();
		let attempts = source.format_attempts.clone();

		let err = Supervisor::exact()
			.discover(&mut source)
			.await
			.expect_err("permanent discovery failure was ignored");

		assert_eq!(attempts.load(Ordering::SeqCst), 1);
		assert!(matches!(err, Error::Capture(message) if message == "permission denied"));
	}

	fn demand(value: bool) -> (kio::Producer<bool>, MockDemand) {
		let state = kio::Producer::new(value);
		let demand = MockDemand { state: state.consume() };
		(state, demand)
	}

	fn set_demand(state: &kio::Producer<bool>, value: bool) {
		let Ok(mut state) = state.write() else {
			panic!("demand state closed");
		};
		*state = value;
	}

	fn stream(drops: Option<Arc<AtomicUsize>>) -> (kio::Queue<Result<capture::Samples, capture::Failure>>, MockStream) {
		let events = kio::Queue::new();
		let stream = MockStream {
			events: events.clone(),
			drops,
			layout: capture::Layout {
				sample_rate: 48_000,
				channels: 2,
			},
		};
		(events, stream)
	}

	fn with_layout(mut stream: MockStream, sample_rate: u32, channels: u32) -> MockStream {
		stream.layout = capture::Layout { sample_rate, channels };
		stream
	}

	/// Poll `future` through all immediately-ready work until its next real wait.
	async fn poll_pending<F: Future>(future: Pin<&mut F>) {
		tokio::select! {
			biased;
			_ = future => panic!("capture supervisor ended unexpectedly"),
			_ = tokio::task::yield_now() => {}
		}
	}

	#[tokio::test(start_paused = true)]
	async fn failed_reopens_back_off_to_the_cap() {
		let mut source = source([], true);
		let attempts = source.attempts.clone();
		let (demand_tx, mut demand) = demand(true);
		let mut output = MockOutput::default();
		let mut supervisor = Supervisor::exact();
		let future = supervisor.run(&mut source, &mut demand, &mut output);
		tokio::pin!(future);

		poll_pending(future.as_mut()).await;
		assert_eq!(attempts.load(Ordering::SeqCst), 1);

		for (wait, expected) in [
			(Duration::from_millis(500), 2),
			(Duration::from_secs(1), 3),
			(Duration::from_secs(2), 4),
			(Duration::from_secs(4), 5),
			(Duration::from_secs(4), 6),
		] {
			tokio::time::advance(wait).await;
			poll_pending(future.as_mut()).await;
			assert_eq!(attempts.load(Ordering::SeqCst), expected);
		}

		drop(demand_tx);
		let err = future.await.expect_err("recovery ended without its device error");
		assert!(matches!(err, Error::Capture(message) if message == "still unavailable"));
	}

	#[tokio::test]
	async fn permanent_open_error_is_not_retried() {
		let mut source = source([Open::Fatal("permission denied")], true);
		let attempts = source.attempts.clone();
		let (_demand_tx, mut demand) = demand(true);
		let mut output = MockOutput::default();

		let err = Supervisor::exact()
			.run(&mut source, &mut demand, &mut output)
			.await
			.expect_err("permanent failure was ignored");

		assert_eq!(attempts.load(Ordering::SeqCst), 1);
		assert!(matches!(err, Error::Capture(message) if message == "permission denied"));
	}

	#[tokio::test(start_paused = true)]
	async fn empty_reopens_do_not_reset_the_backoff() {
		let (first_tx, first) = stream(None);
		first_tx
			.try_push(Err(capture::Failure::retry(Error::Capture("first lost".into()))))
			.unwrap();
		let (second_tx, second) = stream(None);
		second_tx
			.try_push(Err(capture::Failure::retry(Error::Capture("second lost".into()))))
			.unwrap();
		let mut source = source([Open::Stream(first), Open::Stream(second)], true);
		let attempts = source.attempts.clone();
		let (demand_tx, mut demand) = demand(true);
		let mut output = MockOutput::default();
		let mut supervisor = Supervisor::exact();
		let future = supervisor.run(&mut source, &mut demand, &mut output);
		tokio::pin!(future);

		poll_pending(future.as_mut()).await;
		assert_eq!(attempts.load(Ordering::SeqCst), 1);
		tokio::time::advance(Duration::from_millis(500)).await;
		poll_pending(future.as_mut()).await;
		assert_eq!(attempts.load(Ordering::SeqCst), 2);

		tokio::time::advance(Duration::from_millis(999)).await;
		poll_pending(future.as_mut()).await;
		assert_eq!(attempts.load(Ordering::SeqCst), 2);
		tokio::time::advance(Duration::from_millis(1)).await;
		poll_pending(future.as_mut()).await;
		assert_eq!(attempts.load(Ordering::SeqCst), 3);

		drop(demand_tx);
		let err = future.await.expect_err("recovery ended without its device error");
		assert!(matches!(err, Error::Capture(message) if message == "still unavailable"));
	}

	#[tokio::test(start_paused = true)]
	async fn track_end_after_empty_reopen_returns_the_last_error() {
		let (failed_tx, failed) = stream(None);
		failed_tx
			.try_push(Err(capture::Failure::retry(Error::Capture("lost".into()))))
			.unwrap();
		let (_recovered_tx, recovered) = stream(None);
		let mut source = source([Open::Stream(failed), Open::Stream(recovered)], false);
		let attempts = source.attempts.clone();
		let (demand_tx, mut demand) = demand(true);
		let mut output = MockOutput::default();
		let mut supervisor = Supervisor::exact();
		let future = supervisor.run(&mut source, &mut demand, &mut output);
		tokio::pin!(future);

		poll_pending(future.as_mut()).await;
		tokio::time::advance(Duration::from_millis(500)).await;
		poll_pending(future.as_mut()).await;
		assert_eq!(attempts.load(Ordering::SeqCst), 2);

		drop(demand_tx);
		let err = future.await.expect_err("track end hid the pending device error");
		assert!(matches!(err, Error::Capture(message) if message == "lost"));
	}

	#[tokio::test(start_paused = true)]
	async fn successful_reopen_resumes_the_same_output_after_an_epoch_reset() {
		let (failed_tx, failed) = stream(None);
		failed_tx
			.try_push(Err(capture::Failure::retry(Error::Capture("lost".into()))))
			.unwrap();
		let (recovered_tx, recovered) = stream(None);
		let mut source = source(
			[
				Open::Stream(failed),
				Open::Error("reopen failed"),
				Open::Stream(recovered),
			],
			false,
		);
		let attempts = source.attempts.clone();
		let (demand_tx, mut demand) = demand(true);
		let mut output = MockOutput::default();
		let mut supervisor = Supervisor::exact();
		{
			let future = supervisor.run(&mut source, &mut demand, &mut output);
			tokio::pin!(future);

			poll_pending(future.as_mut()).await;
			assert_eq!(attempts.load(Ordering::SeqCst), 1);
			tokio::time::advance(Duration::from_millis(500)).await;
			poll_pending(future.as_mut()).await;
			assert_eq!(attempts.load(Ordering::SeqCst), 2);
			tokio::time::advance(Duration::from_secs(1)).await;
			poll_pending(future.as_mut()).await;
			assert_eq!(attempts.load(Ordering::SeqCst), 3);

			recovered_tx
				.try_push(Ok(capture::Samples::plain(vec![0.25], false)))
				.unwrap();
			poll_pending(future.as_mut()).await;

			set_demand(&demand_tx, false);
			poll_pending(future.as_mut()).await;
			drop(demand_tx);
			future.await.unwrap();
		}
		assert_eq!(
			output.events,
			[
				OutputEvent::Reset,
				OutputEvent::Reset,
				OutputEvent::Write(vec![0.25f32.to_bits()]),
				OutputEvent::Reset,
			]
		);
	}

	#[tokio::test(start_paused = true)]
	async fn replacement_device_is_converted_to_the_catalog_layout() {
		let (failed_tx, failed) = stream(None);
		let failed = with_layout(failed, 48_000, 1);
		failed_tx
			.try_push(Err(capture::Failure::retry(Error::Capture("lost".into()))))
			.unwrap();
		let (recovered_tx, recovered) = stream(None);
		let recovered = with_layout(recovered, 48_000, 2);
		let mut source = source([Open::Stream(failed), Open::Stream(recovered)], false);
		let (demand_tx, mut demand) = demand(true);
		let mut output = MockOutput::default();
		let mut supervisor = Supervisor::exact();
		supervisor.layout = Some(capture::Layout {
			sample_rate: 48_000,
			channels: 1,
		});

		{
			let future = supervisor.run(&mut source, &mut demand, &mut output);
			tokio::pin!(future);

			poll_pending(future.as_mut()).await;
			tokio::time::advance(RETRY_MIN).await;
			poll_pending(future.as_mut()).await;

			recovered_tx
				.try_push(Ok(capture::Samples::plain(vec![1.0, 3.0, 2.0, 4.0], false)))
				.unwrap();
			poll_pending(future.as_mut()).await;

			set_demand(&demand_tx, false);
			poll_pending(future.as_mut()).await;
			drop(demand_tx);
			future.await.unwrap();
		}

		assert_eq!(
			output.events,
			[
				OutputEvent::Reset,
				OutputEvent::Write(vec![2.0f32.to_bits(), 3.0f32.to_bits()]),
				OutputEvent::Reset,
			]
		);
	}

	#[tokio::test(start_paused = true)]
	async fn demand_loss_stops_a_pending_retry() {
		let mut source = source([Open::Error("lost")], true);
		let attempts = source.attempts.clone();
		let (demand_tx, mut demand) = demand(true);
		let mut output = MockOutput::default();
		let mut supervisor = Supervisor::exact();
		let future = supervisor.run(&mut source, &mut demand, &mut output);
		tokio::pin!(future);

		poll_pending(future.as_mut()).await;
		assert_eq!(attempts.load(Ordering::SeqCst), 1);
		set_demand(&demand_tx, false);
		poll_pending(future.as_mut()).await;
		tokio::time::advance(Duration::from_secs(60)).await;
		poll_pending(future.as_mut()).await;
		assert_eq!(attempts.load(Ordering::SeqCst), 1);

		drop(demand_tx);
		future.await.unwrap();
	}

	#[tokio::test]
	async fn cancellation_drops_the_live_stream() {
		let drops = Arc::new(AtomicUsize::new(0));
		let (_events, live) = stream(Some(drops.clone()));
		let mut source = source([Open::Stream(live)], false);
		let (_demand_tx, mut demand) = demand(true);
		let mut output = MockOutput::default();
		let mut supervisor = Supervisor::exact();

		{
			let future = supervisor.run(&mut source, &mut demand, &mut output);
			tokio::pin!(future);
			poll_pending(future.as_mut()).await;
			assert_eq!(drops.load(Ordering::SeqCst), 0);
		}

		assert_eq!(drops.load(Ordering::SeqCst), 1);
	}
}
