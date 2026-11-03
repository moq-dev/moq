//! Control a capture-backed audio publication.

use std::fmt;
use std::sync::Arc;
use std::task::Poll;

use moq_net::time::Instant;

use moq_mux::catalog::hang::CatalogExt;

use super::{Input, Options, Producer};
use crate::capture;
use crate::resample::{Remix, Resampler};
use crate::{Error, Frame, Layout as PcmLayout, Level};

/// The capture publication's current lifecycle state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Status {
	/// Encoding was explicitly stopped; the shared capture is independent.
	Stopped,
	/// Raw audio is retained until a subscriber needs encoding.
	Waiting,
	/// The publication driver has not started yet.
	Starting,
	/// Post-processing samples are being encoded and published.
	Live,
	/// The capture consumer failed and the driver has ended.
	Failed,
	/// The MoQ track ended, so the driver has stopped.
	Ended,
}

/// A snapshot of a capture-backed publication's lifecycle.
///
/// Levels are deliberately not here: they change every buffer, so they would
/// drown out the transitions [`Control::changed`] exists to report. Read
/// them with [`Control::level`] instead.
#[derive(Clone, Debug)]
pub struct State {
	status: Status,
	source: capture::Source,
	device: Option<capture::Device>,
	failure: Option<Error>,
}

impl State {
	/// The publication's current lifecycle state.
	pub fn status(&self) -> Status {
		self.status
	}

	/// The selected source, including an unresolved default microphone.
	pub fn source(&self) -> &capture::Source {
		&self.source
	}

	/// The concrete microphone in use, or the last one that failed while live.
	///
	/// This is `None` for system-audio capture.
	pub fn device(&self) -> Option<&capture::Device> {
		self.device.as_ref()
	}

	/// The most recent input failure while [`Status::Failed`].
	pub fn failure(&self) -> Option<&Error> {
		self.failure.as_ref()
	}
}

/// Capture and encode settings for [`Control::new`] and [`publish_capture`].
///
/// Samples are stamped on the catalog's [`clock`](moq_mux::catalog::Producer::clock), the one
/// its consumers are told about, so a concurrent video capture on the same catalog stays aligned.
/// A microphone buffer is placed at the capture instant of its first sample, mapped through the
/// catalog clock's own correlation. A host that reports no usable capture time, and system audio,
/// are stamped once by the capture producer before fanout.
///
/// `#[non_exhaustive]`: construct via [`Capture::new`] and set
/// fields, so new publication settings can be added without changing
/// [`Control::new`].
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Capture {
	/// An independent reader from the shared capture producer.
	pub capture: capture::Consumer,
	/// The track's stable codec and encode settings.
	pub encode: Options,
	/// Encode only while subscribed, retaining raw audio up to the track's `max_age`.
	///
	/// On by default. Capture stays open, and a subscriber encodes the retained past before
	/// continuing live. Tracks without `max_age` keep 30 seconds, matching hang's media retention.
	/// Set false to encode continuously instead.
	pub on_demand: bool,
}

impl Capture {
	/// Publish samples from this independent capture consumer.
	pub fn new(capture: capture::Consumer) -> Self {
		Self {
			capture,
			encode: Options::default(),
			on_demand: true,
		}
	}
}

#[derive(Clone, Debug)]
struct Desired {
	capture: capture::Consumer,
	enabled: bool,
	revision: u64,
}

#[derive(Clone, Debug)]
struct PublishedState {
	state: State,
	revision: u64,
}

/// A handle for controlling a capture-backed audio publication.
///
/// Clones control the same MoQ track. Starting, stopping, or replacing this
/// reader leaves the shared device and other consumers alone. Dropping the
/// final clone stops encoding and releases the publication.
pub struct Control {
	desired: kio::Producer<Desired>,
	state: kio::Consumer<PublishedState>,
	level: kio::Consumer<Level>,
	observed: u64,
	track_name: Arc<str>,
}

impl Clone for Control {
	fn clone(&self) -> Self {
		Self {
			desired: self.desired.clone(),
			state: self.state.clone(),
			level: self.level.clone(),
			observed: self.state.read().revision,
			track_name: self.track_name.clone(),
		}
	}
}

impl fmt::Debug for Control {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Control")
			.field("track_name", &self.track_name)
			.field("state", &self.state.read().state)
			.finish_non_exhaustive()
	}
}

impl Control {
	/// Register a track from an already-open capture and return its controls and driver.
	pub fn new<E: CatalogExt>(
		mut broadcast: moq_net::broadcast::Producer,
		catalog: moq_mux::catalog::Producer<E>,
		options: Capture,
	) -> Result<(Self, Driver), Error> {
		let clock = catalog.clock();
		let input = options.capture.input().clone();
		let producer = Producer::new(&mut broadcast, catalog, input.clone(), &options.encode)?;
		let track_name: Arc<str> = producer.track_name().into();
		let reservation = options.encode.bandwidth.reserve(&producer.demand(), producer.bitrate());
		let initial = PublishedState {
			state: State {
				status: Status::Starting,
				source: options.capture.source().clone(),
				device: options.capture.device().cloned(),
				failure: None,
			},
			revision: 0,
		};
		let desired = kio::Producer::new(Desired {
			capture: options.capture,
			enabled: true,
			revision: 0,
		});
		let state = kio::Producer::new(initial);
		let level = kio::Producer::new(Level::default());
		let control = Self {
			desired: desired.clone(),
			state: state.consume(),
			level: level.consume(),
			observed: 0,
			track_name,
		};
		let driver = Driver {
			_broadcast: broadcast,
			_reservation: reservation,
			producer: Some(producer),
			input,
			on_demand: options.on_demand,
			clock,
			desired: desired.consume(),
			state,
			level,
		};
		Ok((control, driver))
	}

	/// Resume encoding the selected consumer from live capture.
	pub fn start(&self) {
		let Ok(mut desired) = self.desired.write() else { return };
		if desired.enabled {
			return;
		}
		desired.enabled = true;
		desired.revision = desired.revision.wrapping_add(1);
	}

	/// Stop this encoder while retaining the MoQ track and shared device.
	pub fn stop(&self) {
		let Ok(mut desired) = self.desired.write() else { return };
		if !desired.enabled {
			return;
		}
		desired.enabled = false;
		desired.revision = desired.revision.wrapping_add(1);
	}

	/// Replace the selected input without changing the MoQ track identity.
	///
	/// The new reader starts at live capture on the next [`start`](Self::start).
	/// A capture failure ends the driver; create a new publication to recover.
	pub fn replace(&self, capture: capture::Consumer) {
		let Ok(mut desired) = self.desired.write() else { return };
		desired.capture = capture;
		desired.revision = desired.revision.wrapping_add(1);
	}

	/// The stable track name registered for this publication.
	pub fn track_name(&self) -> &str {
		&self.track_name
	}

	/// Return the latest state without waiting.
	pub fn state(&self) -> State {
		self.state.read().state.clone()
	}

	/// The post-processing level of the most recent capture buffer.
	///
	/// Measured after the capture processing chain, so it is what a local meter
	/// or an active-speaker check wants. Zero while the input is closed.
	pub fn level(&self) -> Level {
		*self.level.read()
	}

	/// Wait for a lifecycle change, or return `None` after the driver exits.
	///
	/// Only [`State`] transitions wake this; a level change does not.
	pub async fn changed(&mut self) -> Option<State> {
		let observed = self.observed;
		let published = self
			.state
			.wait(move |published| {
				if published.revision != observed {
					Poll::Ready((**published).clone())
				} else {
					Poll::Pending
				}
			})
			.await
			.ok()?;
		self.observed = published.revision;
		Some(published.state)
	}

	/// Whether the publication driver has exited.
	pub fn is_finished(&self) -> bool {
		self.state.is_closed()
	}
}

/// The task that encodes one consumer without owning its capture device.
pub struct Driver {
	_broadcast: moq_net::broadcast::Producer,
	_reservation: moq_net::bandwidth::Reservation,
	producer: Option<Producer>,
	input: Input,
	on_demand: bool,
	clock: moq_mux::Clock,
	desired: kio::Consumer<Desired>,
	state: kio::Producer<PublishedState>,
	level: kio::Producer<Level>,
}

impl fmt::Debug for Driver {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Driver").finish_non_exhaustive()
	}
}

impl Driver {
	/// Encode until capture fails, the final control drops, or the track ends.
	pub async fn run(mut self) -> Result<(), Error> {
		let result = self.drive().await;
		let mut producer = self.producer.take().unwrap();
		let result = result.and_then(|()| {
			producer.set_active(false, self.clock.now())?;
			producer.finish()
		});
		match &result {
			Ok(()) => {
				self.update(Status::Ended, None);
			}
			Err(error) => {
				self.update(Status::Failed, Some(error.clone()));
				producer.abort(moq_net::Error::Transport(error.to_string()));
			}
		}
		if let Ok(mut level) = self.level.write() {
			*level = Level::default();
		}
		let _ = self.state.close();
		result
	}

	async fn drive(&mut self) -> Result<(), Error> {
		let track = self.producer.as_ref().unwrap().track().clone();
		loop {
			let desired = self.desired.read().clone();
			if !desired.enabled {
				self.update(Status::Stopped, None);
				if let Ok(mut level) = self.level.write() {
					*level = Level::default();
				}
				tokio::select! {
					biased;
					changed = desired_changed(&self.desired, desired.revision) => {
						if changed.is_none() { return Ok(()); }
					}
					_ = track.closed() => return Ok(()),
					error = kio::wait(|waiter| desired.capture.poll_failed(waiter)) => return Err(error),
				}
				continue;
			}
			let capture = desired.capture;
			let mut converter = Converter::new(
				capture::Layout {
					sample_rate: capture.input().sample_rate,
					channels: capture.input().layout.channels(),
				},
				capture::Layout {
					sample_rate: self.input.sample_rate,
					channels: self.input.layout.channels(),
				},
			)?;
			let mut output = EncoderOutput {
				producer: self.producer.as_mut().unwrap(),
				demand: track.demand(),
				on_demand: self.on_demand,
				clock: &self.clock,
				source: capture.source(),
				device: capture.device().cloned(),
				state: &self.state,
				level: &self.level,
				live: true,
			};
			// Capture is immutable metadata plus a cursor; read through an independent clone.
			let mut reader = capture.clone();
			if desired.revision != 0 {
				reader.skip_to_live();
			}
			output.producer.reanchor();
			output.refresh()?;
			loop {
				tokio::select! {
					biased;
					changed = desired_changed(&self.desired, desired.revision) => {
						if changed.is_none() { return Ok(()); }
						break;
					}
					_ = track.closed() => return Ok(()),
					samples = reader.read() => {
						let samples = samples?;
						if samples.gap { output.producer.reanchor(); }
						let timestamp = output.stamp(Some(samples.captured))?;
						if let Some((samples, timestamp)) = converter.process(samples, timestamp)? {
							output.write(samples, timestamp)?;
						}
					}
					changed = output.changed() => changed?,
				}
			}
		}
	}

	fn update(&self, status: Status, failure: Option<Error>) {
		let desired = self.desired.read();
		publish_state(
			&self.state,
			desired.capture.source(),
			status,
			desired.capture.device().cloned(),
			failure,
		);
	}
}

async fn desired_changed(desired: &kio::Consumer<Desired>, revision: u64) -> Option<Desired> {
	desired
		.wait(move |desired| {
			if desired.revision != revision {
				Poll::Ready((**desired).clone())
			} else {
				Poll::Pending
			}
		})
		.await
		.ok()
}

/// Publish a lifecycle transition, skipping one that changes nothing.
///
/// Levels and repeated capture states must not wake lifecycle-only observers.
fn publish_state(
	state: &kio::Producer<PublishedState>,
	source: &capture::Source,
	status: Status,
	device: Option<capture::Device>,
	failure: Option<Error>,
) {
	let Ok(mut published) = state.write() else { return };
	// `Error` is not `PartialEq`, and a failure publish is rare enough that
	// rendering both to compare them costs nothing worth avoiding.
	let same_failure = match (&published.state.failure, &failure) {
		(None, None) => true,
		(Some(before), Some(now)) => before.to_string() == now.to_string(),
		_ => false,
	};
	if same_failure
		&& published.state.status == status
		&& published.state.source == *source
		&& published.state.device == device
	{
		return;
	}
	published.state = State {
		status,
		source: source.clone(),
		device,
		failure,
	};
	published.revision = published.revision.wrapping_add(1);
}

/// Capture audio and publish it as an encoded MoQ track.
///
/// This convenience function runs a [`Control`] from
/// `options` without handing back the handle. Use [`Control::new`] directly to
/// retain controls.
pub async fn publish_capture<E: CatalogExt>(
	broadcast: moq_net::broadcast::Producer,
	catalog: moq_mux::catalog::Producer<E>,
	options: Capture,
) -> Result<(), Error> {
	let (_control, driver) = Control::new(broadcast, catalog, options)?;
	driver.run().await
}

/// Off macOS, [`publish_capture`]'s future must stay `Send` so a server can
/// `tokio::spawn` it. This is never called; it exists only to fail compilation
/// if the future ever regains a `!Send` component. macOS is exempt: the TCC
/// prompt and ScreenCaptureKit both hold ObjC handles across an await.
#[cfg(not(target_os = "macos"))]
#[allow(dead_code, reason = "a compile-time `Send` assertion, never called")]
fn assert_publish_capture_send(
	broadcast: moq_net::broadcast::Producer,
	catalog: moq_mux::catalog::Producer,
	options: Capture,
) {
	fn is_send<T: Send>(_: &T) {}
	is_send(&publish_capture(broadcast, catalog, options));
}

struct EncoderOutput<'a> {
	producer: &'a mut Producer,
	demand: moq_net::track::Demand,
	on_demand: bool,
	clock: &'a moq_mux::Clock,
	/// The source this reader belongs to, so a published state can
	/// never name an input this stream isn't actually reading.
	source: &'a capture::Source,
	state: &'a kio::Producer<PublishedState>,
	level: &'a kio::Producer<Level>,
	device: Option<capture::Device>,
	live: bool,
}

impl EncoderOutput<'_> {
	fn stamp(&self, captured: Option<Instant>) -> Result<u64, Error> {
		let timestamp = match captured {
			// Clamp to the nearest instant the clock can name instead of ending the publication:
			// a host that under-reports its delay can stamp a chunk after now, and a `Clock::at`
			// epoch taken just after the first sample leaves it before PTS zero.
			Some(at) => match self.clock.capture(at) {
				Err(moq_mux::Error::InvalidCapture) => {
					let now = self.clock.now();
					let ago = Instant::now().saturating_duration_since(at).as_micros();
					if ago > now.as_micros() {
						moq_net::Timestamp::ZERO
					} else {
						now
					}
				}
				timestamp => timestamp?,
			},
			None => self.clock.now(),
		};
		Ok(timestamp.as_micros() as u64)
	}

	fn write(&mut self, samples: capture::Buffer, timestamp_us: u64) -> Result<(), Error> {
		self.publish_level(Level::measure(&samples.data));
		self.refresh()?;
		self.producer.write(&frame(&samples.data, timestamp_us)?)
	}

	async fn changed(&mut self) -> Result<(), Error> {
		if self.producer.backlog_ready() {
			// Keep the microphone queue moving between codec frames during a long catch-up.
			tokio::task::yield_now().await;
			return self.refresh();
		}
		let delay = self
			.producer
			.backlog_deadline()
			.map(|at| at.saturating_sub(self.clock.now().into()));
		tokio::select! {
			changed = self.demand.wanted_changed() => { changed?; }
			_ = async {
				match delay { Some(delay) => kio::clock::sleep(delay).await, None => std::future::pending().await }
			} => {}
		}
		self.refresh()
	}

	fn refresh(&mut self) -> Result<(), Error> {
		let active = !self.on_demand || self.demand.wanted().is_some();
		self.producer.set_active(active, self.clock.now())?;
		if self.live {
			publish_state(
				self.state,
				self.source,
				if active { Status::Live } else { Status::Waiting },
				self.device.clone(),
				None,
			);
		}
		Ok(())
	}

	/// Levels ride their own channel: they change every buffer, so folding them
	/// into [`State`] would wake every [`Control::changed`] waiter at the
	/// capture rate and rebuild the whole snapshot to do it.
	fn publish_level(&self, level: Level) {
		let Ok(mut published) = self.level.write() else { return };
		if *published != level {
			*published = level;
		}
	}
}

/// The layout of a capture device, which reports only a channel count. That names
/// speakers for mono and stereo alone: a six-channel microphone array is not a
/// 5.1 speaker layout, so it stays discrete rather than being remixed as one.
fn device_layout(channels: u32) -> Result<PcmLayout, Error> {
	match channels {
		0..=2 => PcmLayout::from_channels(channels),
		channels => Ok(PcmLayout::Discrete(channels)),
	}
}

/// Converts a replacement consumer into the track's fixed PCM layout.
struct Converter {
	remix: Option<Remix>,
	resampler: Option<Resampler>,
	anchor_us: Option<u64>,
}

impl Converter {
	fn new(input: capture::Layout, output: capture::Layout) -> Result<Self, Error> {
		let remix = (input.channels != output.channels)
			.then(|| Remix::new(device_layout(input.channels)?, device_layout(output.channels)?))
			.transpose()?;

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
			remix,
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
	/// into them. The timestamp preserves the anchor when resampling spans reads.
	fn process(
		&mut self,
		mut samples: capture::Buffer,
		timestamp_us: u64,
	) -> Result<Option<(capture::Buffer, u64)>, Error> {
		if samples.gap {
			self.reset();
		}
		if self.anchor_us.is_none() && !samples.data.is_empty() {
			self.anchor_us = Some(timestamp_us);
		}

		if let Some(resampler) = self.resampler.as_mut() {
			let data = resampler.process(&samples.data, moq_net::Timestamp::from_micros(timestamp_us)?)?;
			samples.data = data.into();
		}
		if let Some(remix) = &self.remix {
			let data = remix.process(&samples.data);
			samples.data = data.into();
		}
		if samples.data.is_empty() {
			return Ok(None);
		}

		Ok(Some((samples, self.anchor_us.take().unwrap_or(timestamp_us))))
	}
}

/// Pack interleaved `f32` samples into a timestamped [`Frame`] of little-endian
/// bytes (i.e. [`crate::Format::F32`]).
fn frame(samples: &[f32], timestamp_us: u64) -> Result<Frame, Error> {
	let mut bytes = Vec::with_capacity(std::mem::size_of_val(samples));
	for sample in samples {
		bytes.extend_from_slice(&sample.to_le_bytes());
	}
	Ok(Frame::new(bytes.into(), moq_net::Timestamp::from_micros(timestamp_us)?))
}

#[cfg(all(test, feature = "bus"))]
mod tests {
	use super::*;
	use crate::bus::{Bus, Config as BusConfig, Strip};
	use crate::capture::Samples;
	use crate::encode::Codec;
	use moq_mux::container::{Consumer as Track, Kind, Read, legacy::Wire};
	use std::time::Duration;

	struct Fixture {
		capture: capture::Producer,
		samples: kio::Queue<Result<Samples, Error>>,
		control: Control,
		driver: Driver,
		broadcast: moq_net::broadcast::Consumer,
		catalog: moq_mux::catalog::Producer,
		epoch: Instant,
	}

	fn fixture(on_demand: bool) -> Fixture {
		fixture_codec(on_demand, Codec::Pcm)
	}

	fn fixture_codec(on_demand: bool, codec: Codec) -> Fixture {
		let (capture, samples) = capture::test_source();
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let epoch = Instant::now() - Duration::from_secs(1);
		let clock = moq_mux::Clock::at(epoch, kio::clock::wall()).unwrap();
		let catalog = moq_mux::catalog::Producer::new(
			&mut broadcast,
			moq_mux::catalog::Config::default()
				.with_clock(clock)
				.with_max_age(Duration::from_secs(30)),
		)
		.unwrap();
		let mut options = Capture::new(capture.consume());
		options.encode.track = Some("audio".into());
		options.encode.config.codec = codec;
		options.on_demand = on_demand;
		let (control, driver) = Control::new(broadcast, catalog.clone(), options).unwrap();
		Fixture {
			capture,
			samples,
			control,
			driver,
			broadcast: consumer,
			catalog,
			epoch,
		}
	}

	async fn subscribe(broadcast: &moq_net::broadcast::Consumer) -> Track<Wire> {
		let subscriber = broadcast.track("audio").unwrap().subscribe(None).await.unwrap();
		Track::new(subscriber, Wire(Kind::Audio))
	}

	async fn packet(track: &mut Track<Wire>) -> moq_mux::container::Frame {
		loop {
			if let Some(Read::Frame { frame, .. }) = track.read().await.unwrap() {
				return frame;
			}
		}
	}

	async fn status(control: &mut Control, expected: Status) {
		while control.state().status() != expected {
			control.changed().await.expect("driver still alive");
		}
	}

	#[tokio::test(start_paused = true)]
	async fn one_capture_feeds_a_bus_and_encoder_and_losing_the_bus_does_not_stop_encoding() {
		tokio::task::LocalSet::new()
			.run_until(async {
				let Fixture {
					capture,
					samples,
					control,
					driver,
					broadcast,
					catalog: _catalog,
					epoch,
				} = fixture(false);
				let mut observer = capture.consume();
				let bus = Bus::new(BusConfig::default()).unwrap();
				let mut bus_run = Box::pin(bus.capture(capture.consume(), Strip::default()).unwrap().run());
				// Register the bus channel and park its reader before the first mix read.
				assert!(
					std::future::poll_fn(|cx| Poll::Ready(std::future::Future::poll(bus_run.as_mut(), cx)))
						.await
						.is_pending()
				);
				let mut mixed = vec![0.0; 1920];
				let mut bus_pcm = Vec::new();
				let mut expected = Vec::new();
				bus.read(&mut mixed);
				let capture_task = tokio::task::spawn_local(capture.run());
				let encode_task = tokio::spawn(driver.run());
				let mut track = subscribe(&broadcast).await;
				for (index, gap) in [false, true, false].into_iter().enumerate() {
					let captured = epoch + Duration::from_millis([100, 200, 220][index]);
					let value = (index + 1) as f32 / 8.0;
					let mut block = Samples::plain(vec![value; 1920], gap);
					block.captured = Some(captured);
					samples.try_push(Ok(block)).unwrap();
					let observed = observer.read().await.unwrap();
					assert_eq!(observed.captured, captured);
					assert_eq!(observed.gap, gap);
					assert!(
						std::future::poll_fn(|cx| Poll::Ready(std::future::Future::poll(bus_run.as_mut(), cx)))
							.await
							.is_pending()
					);
					let encoded = packet(&mut track).await;
					assert_eq!(
						encoded.timestamp.as_micros(),
						captured.duration_since(epoch).as_micros()
					);
					assert_eq!(
						crate::Format::F32
							.as_interleaved_f32(&encoded.payload, 2)
							.unwrap()
							.as_ref(),
						&*observed.data
					);
					bus.read(&mut mixed);
					bus_pcm.extend_from_slice(&mixed);
					expected.extend_from_slice(&observed.data);
				}
				// The bus begins with 50 ms of silence to absorb jitter; drain that
				// fixed latency and compare every input sample in order.
				let mut tail = vec![0.0; 4800];
				bus.read(&mut tail);
				bus_pcm.extend_from_slice(&tail);
				assert_eq!(&bus_pcm[4800..], expected);
				drop(bus_run);
				let captured = epoch + Duration::from_millis(240);
				let mut block = Samples::plain(vec![0.625; 1920], false);
				block.captured = Some(captured);
				samples.try_push(Ok(block)).unwrap();
				observer.read().await.unwrap();
				assert_eq!(packet(&mut track).await.timestamp.as_micros(), 240_000);
				drop(control);
				encode_task.await.unwrap().unwrap();
				// The capture and independent observer remain live after the encoder goes away too.
				samples.try_push(Ok(Samples::plain(vec![0.75; 1920], false))).unwrap();
				assert_eq!(observer.read().await.unwrap().data[0], 0.75);
				capture_task.abort();
				assert!(capture_task.await.unwrap_err().is_cancelled());
				assert!(observer.read().await.is_err());
			})
			.await;
	}

	#[tokio::test(start_paused = true)]
	async fn source_failure_reaches_all_consumers_and_the_publication_status() {
		tokio::task::LocalSet::new()
			.run_until(async {
				let Fixture {
					capture,
					samples,
					control,
					driver,
					broadcast: _,
					catalog: _catalog,
					..
				} = fixture(false);
				let mut observer = capture.consume();
				let bus = Bus::new(BusConfig::default()).unwrap();
				let bus_task = tokio::spawn(bus.capture(capture.consume(), Strip::default()).unwrap().run());
				let capture_task = tokio::task::spawn_local(capture.run());
				let encode_task = tokio::spawn(driver.run());
				samples.try_push(Err(Error::Capture("device lost".into()))).unwrap();
				for result in [
					capture_task.await.unwrap(),
					encode_task.await.unwrap(),
					bus_task.await.unwrap(),
					observer.read().await.map(|_| ()),
				] {
					assert!(matches!(result, Err(Error::Capture(message)) if message == "device lost"));
				}
				assert_eq!(control.state().status(), Status::Failed);
				assert!(matches!(control.state().failure(), Some(Error::Capture(message)) if message == "device lost"));
				assert!(control.is_finished());
			})
			.await;
	}

	#[tokio::test(start_paused = true)]
	async fn subscriber_demand_retains_history_and_stop_only_stops_this_encoder() {
		tokio::task::LocalSet::new()
			.run_until(async {
				let Fixture {
					capture,
					samples,
					mut control,
					driver,
					broadcast,
					catalog: _catalog,
					epoch,
				} = fixture(true);
				let mut observer = capture.consume();
				let capture_task = tokio::task::spawn_local(capture.run());
				let encode_task = tokio::spawn(driver.run());
				status(&mut control, Status::Waiting).await;
				let captured = epoch + Duration::from_millis(100);
				let mut block = Samples::plain(vec![0.25; 1920], false);
				block.captured = Some(captured);
				samples.try_push(Ok(block)).unwrap();
				observer.read().await.unwrap();
				tokio::task::yield_now().await;
				let mut track = subscribe(&broadcast).await;
				let encoded = packet(&mut track).await;
				assert_eq!(encoded.timestamp.as_micros(), 100_000);
				control.stop();
				status(&mut control, Status::Stopped).await;
				samples.try_push(Ok(Samples::plain(vec![0.5; 1920], false))).unwrap();
				assert_eq!(observer.read().await.unwrap().data[0], 0.5);
				assert_eq!(control.level(), Level::default());
				control.start();
				status(&mut control, Status::Live).await;
				tokio::time::advance(Duration::from_millis(40)).await;
				samples.try_push(Ok(Samples::plain(vec![0.75; 1920], false))).unwrap();
				observer.read().await.unwrap();
				assert_eq!(packet(&mut track).await.timestamp.as_micros(), 1_040_000);
				drop(control);
				encode_task.await.unwrap().unwrap();
				capture_task.abort();
				assert!(capture_task.await.unwrap_err().is_cancelled());
			})
			.await;
	}
	#[tokio::test(start_paused = true)]
	async fn opus_uses_the_shared_capture_clock_including_gaps_and_clamping() {
		tokio::task::LocalSet::new()
			.run_until(async {
				for placement in [0, 1, 2, 3] {
					let Fixture {
						capture,
						samples,
						control,
						driver,
						broadcast,
						catalog: _catalog,
						epoch,
					} = fixture_codec(false, Codec::Opus);
					let mut observer = capture.consume();
					let capture_task = tokio::task::spawn_local(capture.run());
					let encode_task = tokio::spawn(driver.run());
					let mut track = subscribe(&broadcast).await;
					let now = Instant::now();
					let (captured, expected) = match placement {
						0 => (Some(now - Duration::from_millis(40)), 960_000),
						1 => (Some(epoch - Duration::from_millis(10)), 0),
						2 => (Some(now + Duration::from_secs(1)), 1_000_000),
						_ => (None, 1_000_000),
					};
					let mut block = Samples::plain(vec![0.25; 1920], false);
					block.captured = captured;
					samples.try_push(Ok(block)).unwrap();
					let observed = observer.read().await.unwrap();
					assert_eq!(observed.captured, captured.unwrap_or(now));
					assert_eq!(packet(&mut track).await.timestamp.as_micros(), expected);
					let mut block = Samples::plain(vec![0.5; 1920], true);
					block.captured = Some(now);
					samples.try_push(Ok(block)).unwrap();
					assert!(observer.read().await.unwrap().gap);
					let next = packet(&mut track).await;
					assert_eq!(next.timestamp.as_micros(), 1_000_000);
					drop(control);
					encode_task.await.unwrap().unwrap();
					capture_task.abort();
					assert!(capture_task.await.unwrap_err().is_cancelled());
				}
			})
			.await;
	}
}
