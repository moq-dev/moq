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
/// attempt, and recovery stops as soon as the track becomes unused.
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
	let (sample_rate, channels) = capture::format(&capture).await?;
	let input = Input {
		format: Format::F32,
		sample_rate,
		channels,
	};

	// The producer's input layout is fixed in the catalog. Re-resolve the device
	// itself on every open, but keep asking replacements for that same layout.
	let mut capture = capture;
	capture.sample_rate = Some(sample_rate);
	capture.channels = Some(channels);

	let mut producer = Producer::new(&mut broadcast, catalog, input, &encode)?;
	let track = producer.track().clone();

	let mut source = DeviceSource { config: &capture };
	let mut demand = TrackDemand { track: &track };
	let mut output = EncoderOutput {
		producer: &mut producer,
		clock: &clock,
	};
	let result = Supervisor::default().run(&mut source, &mut demand, &mut output).await;

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

	async fn open(&mut self) -> Result<Self::Stream, Error>;
	async fn read(&mut self, stream: &mut Self::Stream) -> Result<Option<capture::Samples>, Error>;
}

struct DeviceSource<'a> {
	config: &'a capture::Config,
}

impl CaptureSource for DeviceSource<'_> {
	type Stream = capture::Stream;

	async fn open(&mut self) -> Result<Self::Stream, Error> {
		capture::open(self.config).await
	}

	async fn read(&mut self, stream: &mut Self::Stream) -> Result<Option<capture::Samples>, Error> {
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
	fn write(&mut self, samples: capture::Samples) -> Result<(), Error>;
}

struct EncoderOutput<'a> {
	producer: &'a mut Producer,
	clock: &'a moq_mux::Clock,
}

impl Output for EncoderOutput<'_> {
	fn reset_epoch(&mut self) {
		self.producer.reset_epoch();
	}

	fn write(&mut self, samples: capture::Samples) -> Result<(), Error> {
		self.producer.write(&frame(samples.data, self.clock.micros())?)
	}
}

struct Supervisor {
	next: Duration,
	jitter: fn(Duration) -> Duration,
}

impl Default for Supervisor {
	fn default() -> Self {
		Self {
			next: RETRY_MIN,
			jitter: |delay| delay.mul_f64(0.5 + rand::rng().random::<f64>() / 2.0),
		}
	}
}

impl Supervisor {
	#[cfg(test)]
	fn exact() -> Self {
		Self {
			next: RETRY_MIN,
			jitter: std::convert::identity,
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
						let recovered = last_error.take().is_some();
						self.reset();
						if recovered {
							tracing::info!("audio capture recovered");
						}

						loop {
							// Demand wins over a simultaneous buffer or error, so an unused
							// track releases the device without starting a retry sequence.
							let samples = tokio::select! {
								biased;
								unused = demand.unused() => {
									drop(input);
									output.reset_epoch();
									if !unused {
										return Ok(());
									}
									tracing::info!("no listeners: released audio capture");
									continue 'demand;
								}
								samples = source.read(&mut input) => samples,
							};

							match samples {
								Ok(Some(samples)) => {
									// A bounded-queue drop is a real hole in the timeline.
									if samples.gap {
										output.reset_epoch();
									}
									output.write(samples)?;
								}
								Ok(None) => break Error::Capture("audio capture stream stopped".into()),
								Err(err) => break err,
							}
						}
					}
					Err(err) if retryable(&err) => err,
					Err(err) => return Err(err),
				};

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

fn retryable(err: &Error) -> bool {
	matches!(err, Error::Capture(_) | Error::Device(_))
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
fn frame(samples: Vec<f32>, timestamp_us: u64) -> Result<Frame, Error> {
	let mut bytes = Vec::with_capacity(samples.len() * size_of::<f32>());
	for sample in &samples {
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

	use tokio::sync::{mpsc, watch};

	use super::*;

	struct MockStream {
		events: mpsc::UnboundedReceiver<Result<capture::Samples, Error>>,
		drops: Option<Arc<AtomicUsize>>,
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
		Stream(MockStream),
	}

	struct MockSource {
		opens: VecDeque<Open>,
		attempts: Arc<AtomicUsize>,
		fallback_error: bool,
	}

	impl CaptureSource for MockSource {
		type Stream = MockStream;

		async fn open(&mut self) -> Result<Self::Stream, Error> {
			self.attempts.fetch_add(1, Ordering::SeqCst);
			match self.opens.pop_front() {
				Some(Open::Error(message)) => Err(Error::Capture(message.into())),
				Some(Open::Stream(stream)) => Ok(stream),
				None if self.fallback_error => Err(Error::Capture("still unavailable".into())),
				None => std::future::pending().await,
			}
		}

		async fn read(&mut self, stream: &mut Self::Stream) -> Result<Option<capture::Samples>, Error> {
			match stream.events.recv().await {
				Some(Ok(samples)) => Ok(Some(samples)),
				Some(Err(err)) => Err(err),
				None => Ok(None),
			}
		}
	}

	struct MockDemand {
		rx: watch::Receiver<bool>,
	}

	impl MockDemand {
		async fn wait(&mut self, value: bool) -> bool {
			loop {
				if *self.rx.borrow() == value {
					return true;
				}
				if self.rx.changed().await.is_err() {
					return false;
				}
			}
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

		fn write(&mut self, samples: capture::Samples) -> Result<(), Error> {
			self.events
				.push(OutputEvent::Write(samples.data.into_iter().map(f32::to_bits).collect()));
			Ok(())
		}
	}

	fn source(opens: impl IntoIterator<Item = Open>, fallback_error: bool) -> MockSource {
		MockSource {
			opens: opens.into_iter().collect(),
			attempts: Arc::new(AtomicUsize::new(0)),
			fallback_error,
		}
	}

	fn stream(drops: Option<Arc<AtomicUsize>>) -> (mpsc::UnboundedSender<Result<capture::Samples, Error>>, MockStream) {
		let (events, rx) = mpsc::unbounded_channel();
		(events, MockStream { events: rx, drops })
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
		let (demand_tx, demand_rx) = watch::channel(true);
		let mut demand = MockDemand { rx: demand_rx };
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

	#[tokio::test(start_paused = true)]
	async fn successful_reopen_resumes_the_same_output_after_an_epoch_reset() {
		let (failed_tx, failed) = stream(None);
		failed_tx.send(Err(Error::Capture("lost".into()))).unwrap();
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
		let (demand_tx, demand_rx) = watch::channel(true);
		let mut demand = MockDemand { rx: demand_rx };
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
				.send(Ok(capture::Samples {
					data: vec![0.25],
					gap: false,
				}))
				.unwrap();
			poll_pending(future.as_mut()).await;

			demand_tx.send(false).unwrap();
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
	async fn demand_loss_stops_a_pending_retry() {
		let mut source = source([Open::Error("lost")], true);
		let attempts = source.attempts.clone();
		let (demand_tx, demand_rx) = watch::channel(true);
		let mut demand = MockDemand { rx: demand_rx };
		let mut output = MockOutput::default();
		let mut supervisor = Supervisor::exact();
		let future = supervisor.run(&mut source, &mut demand, &mut output);
		tokio::pin!(future);

		poll_pending(future.as_mut()).await;
		assert_eq!(attempts.load(Ordering::SeqCst), 1);
		demand_tx.send(false).unwrap();
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
		let (_demand_tx, demand_rx) = watch::channel(true);
		let mut demand = MockDemand { rx: demand_rx };
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
