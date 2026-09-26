//! Control a capture-backed audio publication.

use std::fmt;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use rand::RngExt;

use moq_mux::catalog::hang::CatalogExt;

use super::producer::Reserved;
use super::{Input, Options, Producer};
use crate::capture;
use crate::resample::{Resampler, remix, validate_remix};
use crate::{Error, Format, Frame, Layout as PcmLayout};

/// Backoff bounds for reopening a capture source. The quick first retry covers
/// a USB device re-enumerating; the ceiling keeps a missing device from spinning.
const RETRY_MIN: Duration = Duration::from_millis(500);
const RETRY_MAX: Duration = Duration::from_secs(4);

/// The capture publication's current lifecycle state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Status {
	/// Capture was explicitly stopped.
	Stopped,
	/// Capture is enabled and waiting for a subscriber.
	Waiting,
	/// The selected input is being probed or opened.
	Starting,
	/// Post-processing samples are being encoded and published.
	Live,
	/// The input is unavailable and will be retried automatically when possible.
	Failed,
	/// The MoQ track ended, so the driver has stopped.
	Ended,
}

/// The post-processing level of the most recently captured buffer.
///
/// Zero while the input is closed. Read it with [`Control::level`] at
/// whatever rate the meter draws at.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Level {
	rms: f32,
	peak: f32,
}

impl Level {
	/// Root mean square amplitude on a linear `0.0..=1.0` scale.
	pub fn rms(&self) -> f32 {
		self.rms
	}

	/// Peak absolute amplitude on a linear `0.0..=1.0` scale.
	pub fn peak(&self) -> f32 {
		self.peak
	}

	fn measure(samples: &[f32]) -> Self {
		if samples.is_empty() {
			return Self::default();
		}

		let mut squares = 0.0f64;
		let mut peak = 0.0f32;
		for &sample in samples {
			let sample = sample.clamp(-1.0, 1.0);
			squares += f64::from(sample) * f64::from(sample);
			peak = peak.max(sample.abs());
		}

		Self {
			rms: (squares / samples.len() as f64).sqrt() as f32,
			peak,
		}
	}
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
	/// This is `None` before a microphone opens and for system-audio capture.
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
/// `#[non_exhaustive]`: construct via [`CaptureOptions::default`] and set
/// fields, so new publication settings can be added without changing
/// [`Control::new`].
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct CaptureOptions {
	/// The initial input and its capture processing.
	pub capture: capture::Config,
	/// The track's stable codec and encode settings.
	pub encode: Options,
	/// The shared clock used to align this track with concurrent media.
	pub clock: moq_mux::Clock,
}

#[derive(Clone, Debug)]
struct Desired {
	config: capture::Config,
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
/// Clones control the same MoQ track. Stopping or replacing the input releases
/// the device but leaves the track and catalog rendition intact, so restarting
/// does not change the broadcast identity. Dropping the final clone stops the
/// driver and releases the publication.
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
	/// Register one stable audio track and return its control handle and driver.
	///
	/// The initial source is enabled, but opens only while the track has a
	/// subscriber. On macOS the driver's future is `!Send`, because the permission
	/// prompt and ScreenCaptureKit hold ObjC handles across an await, so await
	/// [`Driver::run`] on a local task there; elsewhere it can be spawned.
	/// [`Control`] itself is `Send + Sync`, so the controls can live anywhere.
	///
	/// The track is registered here, but its catalog rendition describes the
	/// source's PCM layout, so the driver probes for that first and registers the
	/// rendition on the first success. Until then the broadcast advertises no
	/// audio, which [`Status::Starting`] reports. A source that never appears is
	/// retried with capped backoff rather than blocking the controls.
	pub fn new<E: CatalogExt>(
		broadcast: moq_net::broadcast::Producer,
		catalog: moq_mux::catalog::Producer<E>,
		options: CaptureOptions,
	) -> Result<(Self, Driver<E>), Error> {
		Self::build(broadcast, catalog, options, Supervisor::default())
	}

	fn build<E: CatalogExt>(
		mut broadcast: moq_net::broadcast::Producer,
		catalog: moq_mux::catalog::Producer<E>,
		options: CaptureOptions,
		supervisor: Supervisor,
	) -> Result<(Self, Driver<E>), Error> {
		let reserved = Reserved::new(&mut broadcast, catalog, &options.encode)?;
		let track_name: Arc<str> = reserved.name().into();
		let desired = Desired {
			config: options.capture,
			enabled: true,
			revision: 0,
		};
		let initial = PublishedState {
			state: State {
				status: Status::Starting,
				source: desired.config.source.clone(),
				device: None,
				failure: None,
			},
			revision: 0,
		};
		let desired_tx = kio::Producer::new(desired);
		let state_tx = kio::Producer::new(initial);
		let level_tx = kio::Producer::new(Level::default());
		let control = Self {
			desired: desired_tx.clone(),
			state: state_tx.consume(),
			level: level_tx.consume(),
			observed: 0,
			track_name,
		};
		let driver = Driver {
			_broadcast: broadcast,
			_reservation: None,
			track: Some(Track::Reserved(reserved)),
			encode: options.encode,
			clock: options.clock,
			supervisor,
			desired: desired_tx.consume(),
			state: state_tx,
			level: level_tx,
			park_on_failure: true,
		};
		Ok((control, driver))
	}

	/// Enable capture using the selected source.
	///
	/// Calling this while live is a no-op. Calling it after a terminal input
	/// failure retries immediately instead of waiting for the source to change.
	pub fn start(&self) {
		let failed = self.state.read().state.status == Status::Failed;
		let Ok(mut desired) = self.desired.write() else { return };
		if desired.enabled && !failed {
			return;
		}
		desired.enabled = true;
		desired.revision = desired.revision.wrapping_add(1);
	}

	/// Release the input while retaining the MoQ track and catalog rendition.
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
	/// If capture is stopped, the new input opens on the next [`start`](Self::start).
	pub fn replace(&self, source: capture::Source) {
		let Ok(mut desired) = self.desired.write() else { return };
		if desired.config.source == source {
			return;
		}
		desired.config.source = source;
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

	/// The post-AEC, post-processing level of the most recent capture buffer.
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

/// The task that opens the selected input and publishes its samples.
///
/// The driver owns the broadcast producer so its identity remains alive through
/// stop, failure, replacement, and restart. Dropping the final [`Control`]
/// ends the driver and releases that identity.
pub struct Driver<E: CatalogExt = ()> {
	_broadcast: moq_net::broadcast::Producer,
	// Held for the driver lifetime once the layout reveals the encoded rate.
	_reservation: Option<moq_net::bandwidth::Reservation>,
	track: Option<Track<E>>,
	/// The codec settings the rendition is built from once the layout is known.
	encode: Options,
	clock: moq_mux::Clock,
	supervisor: Supervisor,
	desired: kio::Consumer<Desired>,
	state: kio::Producer<PublishedState>,
	level: kio::Producer<Level>,
	/// Whether a terminal input failure parks the driver waiting for a control
	/// action. False for [`publish_capture`], whose caller never receives the
	/// controls, so parking there would hang instead of returning the error.
	park_on_failure: bool,
}

impl<E: CatalogExt> fmt::Debug for Driver<E> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Driver").finish_non_exhaustive()
	}
}

impl<E: CatalogExt> Driver<E> {
	/// Run capture until the final control handle drops or the MoQ track ends.
	pub async fn run(self) -> Result<(), Error> {
		self.run_with(DeviceSource).await
	}

	async fn run_with<S: CaptureSource>(mut self, mut source: S) -> Result<(), Error> {
		let result = self.drive(&mut source).await;
		let mut track = self.track.take().expect("driver always owns its track");

		match &result {
			Ok(()) => {
				self.update(Status::Ended, None, None);
				if let Err(err) = track.finish() {
					tracing::debug!(error = %err, "audio track finish after capture ended");
				}
			}
			Err(err) => {
				self.fail(err);
				track.abort(moq_net::Error::Transport(err.to_string()));
			}
		}
		self.silence();
		let _ = self.state.close();
		result
	}

	async fn drive<S: CaptureSource>(&mut self, source: &mut S) -> Result<(), Error> {
		let track = self
			.track
			.as_ref()
			.expect("driver always owns its track")
			.track()
			.clone();

		if let Some(result) = self.discover(source, &track).await {
			return result;
		}
		self.capture(source, &track).await
	}

	/// Probe the selected input for the PCM layout the catalog rendition
	/// describes, registering the rendition on the first success.
	///
	/// Returns `Some` when the driver should exit before that happens: the
	/// controls were dropped, the track ended, or a probe failed terminally with
	/// nobody left to retry it.
	async fn discover<S: CaptureSource>(
		&mut self,
		source: &mut S,
		track: &moq_net::track::Producer,
	) -> Option<Result<(), Error>> {
		loop {
			let desired = self.desired.read().clone();
			if !desired.enabled {
				if !self.idle(track, desired.revision).await {
					return Some(Ok(()));
				}
				continue;
			}

			self.update(Status::Starting, None, None);
			let discovered = tokio::select! {
				biased;
				changed = desired_changed(&self.desired, desired.revision) => {
					if changed.is_none() {
						return Some(Ok(()));
					}
					// A replaced source deserves an immediate probe, not the backoff
					// the one it replaced had climbed to.
					self.supervisor.reset();
					continue;
				}
				closed = track.closed() => {
					log_track_ended(closed);
					return Some(Ok(()));
				}
				layout = self.supervisor.discover(source, &desired.config) => layout,
			};

			let layout = match discovered {
				Ok(layout) => layout,
				Err(err) => match self.failed(err, track, desired.revision).await {
					Some(result) => return Some(result),
					None => continue,
				},
			};
			let pcm_layout = match PcmLayout::from_channels(layout.channels) {
				Ok(layout) => layout,
				Err(err) => match self.failed(err, track, desired.revision).await {
					Some(result) => return Some(result),
					None => continue,
				},
			};

			let Some(Track::Reserved(mut reserved)) = self.track.take() else {
				unreachable!("the track stays reserved until discovery succeeds")
			};
			let input = Input {
				format: Format::F32,
				sample_rate: layout.sample_rate,
				layout: pcm_layout,
			};
			let registered = match reserved.register(input, &self.encode) {
				Ok(registered) => registered,
				// A layout the codec rejects parks like any other terminal failure,
				// so `replace` can still hand the same track a compatible input.
				Err(err) => {
					self.track = Some(Track::Reserved(reserved));
					match self.failed(err, track, desired.revision).await {
						Some(result) => return Some(result),
						None => continue,
					}
				}
			};

			let producer = reserved.encode(registered);
			self._reservation = Some(self.encode.bandwidth.reserve(&producer.demand(), producer.bitrate()));
			self.track = Some(Track::Encoding(producer));
			return None;
		}
	}

	async fn capture<S: CaptureSource>(
		&mut self,
		source: &mut S,
		track: &moq_net::track::Producer,
	) -> Result<(), Error> {
		loop {
			let desired = self.desired.read().clone();
			if !desired.enabled {
				if !self.idle(track, desired.revision).await {
					return Ok(());
				}
				continue;
			}

			let event = {
				// Borrow the field, not all of `self`: the select below needs the rest.
				let producer = self
					.track
					.as_mut()
					.and_then(Track::producer)
					.expect("the layout is discovered before capture runs");
				let mut demand = TrackDemand { track };
				let mut output = EncoderOutput {
					producer,
					clock: &self.clock,
					source: &desired.config.source,
					state: &self.state,
					level: &self.level,
					device: None,
				};
				tokio::select! {
					biased;
					changed = desired_changed(&self.desired, desired.revision) => DriveEvent::Control(changed),
					result = self.supervisor.run(source, &desired.config, &mut demand, &mut output) => {
						DriveEvent::Capture(result)
					}
				}
			};

			// The supervisor is gone either way, so the input is closed.
			self.silence();

			match event {
				DriveEvent::Control(None) => return Ok(()),
				DriveEvent::Control(Some(_)) => {
					self.producer_mut().reset_epoch();
				}
				DriveEvent::Capture(Ok(())) => return Ok(()),
				DriveEvent::Capture(Err(err)) => {
					if let Some(result) = self.failed(err, track, desired.revision).await {
						return result;
					}
				}
			}
		}
	}

	/// Release the input and wait for a control action, returning false when the
	/// driver should exit.
	async fn idle(&mut self, track: &moq_net::track::Producer, revision: u64) -> bool {
		self.silence();
		self.update(Status::Stopped, None, None);
		tokio::select! {
			biased;
			changed = desired_changed(&self.desired, revision) => changed.is_some(),
			err = track.closed() => {
				log_track_ended(err);
				false
			}
		}
	}

	/// Publish a terminal input failure and park until a control action retries
	/// it, returning `Some` with the result when the driver should exit instead.
	async fn failed(
		&mut self,
		err: Error,
		track: &moq_net::track::Producer,
		revision: u64,
	) -> Option<Result<(), Error>> {
		self.fail(&err);
		if !self.park_on_failure || track.is_closed() {
			return Some(Err(err));
		}
		tokio::select! {
			biased;
			changed = desired_changed(&self.desired, revision) => {
				changed.is_none().then_some(Ok(()))
			}
			closed = track.closed() => {
				log_track_ended(closed);
				Some(Err(err))
			}
		}
	}

	fn producer_mut(&mut self) -> &mut Producer<E> {
		self.track
			.as_mut()
			.and_then(Track::producer)
			.expect("the layout is discovered before capture runs")
	}

	fn update(&self, status: Status, device: Option<capture::Device>, failure: Option<Error>) {
		let source = self.desired.read().config.source.clone();
		publish_state(&self.state, &source, status, device, failure);
	}

	/// Publish a failure the supervisor did not report itself (or re-publish one
	/// it did), keeping whichever device is already on record so a microphone
	/// that died while live stays identifiable.
	fn fail(&self, err: &Error) {
		let device = self.state.read().state.device.clone();
		self.update(Status::Failed, device, Some(err.clone()));
	}

	/// The meter reads zero whenever no supervisor is delivering samples.
	fn silence(&self) {
		if let Ok(mut level) = self.level.write()
			&& *level != Level::default()
		{
			*level = Level::default();
		}
	}
}

enum DriveEvent {
	Control(Option<Desired>),
	Capture(Result<(), Error>),
}

/// The publication's track, before and after the input's layout is known.
///
/// One per publication, moved twice in its life, so the size difference between
/// the variants never costs a copy worth an allocation to avoid.
#[allow(clippy::large_enum_variant)]
enum Track<E: CatalogExt = ()> {
	/// Registered in the broadcast but not the catalog, because the rendition
	/// describes a PCM layout no probe has returned yet.
	Reserved(Reserved<E>),
	/// Probed, so the rendition is registered and samples can be encoded.
	Encoding(Producer<E>),
}

impl<E: CatalogExt> Track<E> {
	fn track(&self) -> &moq_net::track::Producer {
		match self {
			Self::Reserved(reserved) => reserved.track(),
			Self::Encoding(producer) => producer.track(),
		}
	}

	fn producer(&mut self) -> Option<&mut Producer<E>> {
		match self {
			Self::Reserved(_) => None,
			Self::Encoding(producer) => Some(producer),
		}
	}

	fn finish(&mut self) -> Result<(), Error> {
		match self {
			Self::Reserved(reserved) => reserved.finish(),
			Self::Encoding(producer) => producer.finish(),
		}
	}

	fn abort(self, err: moq_net::Error) {
		match self {
			Self::Reserved(reserved) => reserved.abort(err),
			Self::Encoding(producer) => producer.abort(err),
		}
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
/// The supervisor re-announces `Waiting` and `Starting` on every demand cycle,
/// so without this a subscriber would wake on transitions it cannot see.
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

/// Capture audio on demand and publish it as an encoded MoQ track.
///
/// This convenience function runs a [`Control`] from
/// `options` without handing back the handle. Use [`Control::new`] directly to
/// retain controls.
pub async fn publish_capture<E: CatalogExt>(
	broadcast: moq_net::broadcast::Producer,
	catalog: moq_mux::catalog::Producer<E>,
	options: CaptureOptions,
) -> Result<(), Error> {
	// Held, not dropped: the driver ends as soon as the last control handle goes.
	let (_control, driver) = Control::new(broadcast, catalog, options)?;
	Driver {
		park_on_failure: false,
		..driver
	}
	.run()
	.await
}

/// Off macOS, [`publish_capture`]'s future must stay `Send` so a server can
/// `tokio::spawn` it. This is never called; it exists only to fail compilation
/// if the future ever regains a `!Send` component. macOS is exempt: the TCC
/// prompt and ScreenCaptureKit both hold ObjC handles across an await.
#[cfg(not(target_os = "macos"))]
#[allow(dead_code)]
fn assert_publish_capture_send(
	broadcast: moq_net::broadcast::Producer,
	catalog: moq_mux::catalog::Producer,
	options: CaptureOptions,
) {
	fn is_send<T: Send>(_: &T) {}
	is_send(&publish_capture(broadcast, catalog, options));
}

/// A capture backend as the supervisor sees it. Kept separate from cpal so the
/// retry and cancellation lifecycle can be tested without audio hardware.
trait CaptureSource {
	type Stream;

	async fn format(&mut self, config: &capture::Config) -> Result<capture::Layout, capture::Failure>;
	async fn open(&mut self, config: &capture::Config) -> Result<Self::Stream, capture::Failure>;
	fn layout(&self, stream: &Self::Stream) -> capture::Layout;
	fn device(&self, _stream: &Self::Stream) -> Option<capture::Device> {
		None
	}
	async fn read(&mut self, stream: &mut Self::Stream) -> Result<Option<capture::Samples>, capture::Failure>;
}

struct DeviceSource;

impl CaptureSource for DeviceSource {
	type Stream = capture::Stream;

	async fn format(&mut self, config: &capture::Config) -> Result<capture::Layout, capture::Failure> {
		capture::format(config).await
	}

	async fn open(&mut self, config: &capture::Config) -> Result<Self::Stream, capture::Failure> {
		capture::open(config).await
	}

	fn layout(&self, stream: &Self::Stream) -> capture::Layout {
		stream.layout()
	}

	fn device(&self, stream: &Self::Stream) -> Option<capture::Device> {
		stream.device().cloned()
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
	fn waiting(&mut self) {}
	fn starting(&mut self) {}
	fn live(&mut self, _device: Option<capture::Device>) {}
	fn failed(&mut self, _error: &Error) {}
	fn reset_epoch(&mut self);
	fn now(&self) -> u64;
	fn write(&mut self, samples: capture::Samples, timestamp_us: u64) -> Result<(), Error>;
}

struct EncoderOutput<'a, E: CatalogExt> {
	producer: &'a mut Producer<E>,
	clock: &'a moq_mux::Clock,
	/// The source the supervisor was started with, so a published state can
	/// never name an input this stream isn't actually reading.
	source: &'a capture::Source,
	state: &'a kio::Producer<PublishedState>,
	level: &'a kio::Producer<Level>,
	device: Option<capture::Device>,
}

impl<E: CatalogExt> Output for EncoderOutput<'_, E> {
	fn waiting(&mut self) {
		self.device = None;
		self.silence();
		publish_state(self.state, self.source, Status::Waiting, None, None);
	}

	fn starting(&mut self) {
		self.device = None;
		self.silence();
		publish_state(self.state, self.source, Status::Starting, None, None);
	}

	fn live(&mut self, device: Option<capture::Device>) {
		self.device = device.clone();
		publish_state(self.state, self.source, Status::Live, device, None);
	}

	fn failed(&mut self, error: &Error) {
		self.silence();
		publish_state(
			self.state,
			self.source,
			Status::Failed,
			self.device.clone(),
			Some(error.clone()),
		);
	}

	fn reset_epoch(&mut self) {
		self.producer.reset_epoch();
	}

	fn now(&self) -> u64 {
		self.clock.now().as_micros() as u64
	}

	fn write(&mut self, samples: capture::Samples, timestamp_us: u64) -> Result<(), Error> {
		self.publish_level(Level::measure(&samples.data));
		self.producer.write(&frame(&samples.data, timestamp_us)?)
	}
}

impl<E: CatalogExt> EncoderOutput<'_, E> {
	/// Levels ride their own channel: they change every buffer, so folding them
	/// into [`State`] would wake every [`Control::changed`] waiter at the
	/// capture rate and rebuild the whole snapshot to do it.
	fn publish_level(&self, level: Level) {
		let Ok(mut published) = self.level.write() else { return };
		if *published != level {
			*published = level;
		}
	}

	/// Drop the meter to zero whenever the input is not delivering samples.
	fn silence(&self) {
		self.publish_level(Level::default());
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
			validate_remix(
				PcmLayout::from_channels(input.channels)?,
				PcmLayout::from_channels(output.channels)?,
			)?;
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
			let data = resampler.process(&samples.data, moq_net::Timestamp::from_micros(timestamp_us)?)?;
			samples.replace(data);
		}
		if self.input.channels != self.output.channels {
			let input = PcmLayout::from_channels(self.input.channels)?;
			let output = PcmLayout::from_channels(self.output.channels)?;
			let data = remix(&samples.data, input, output)?;
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
	async fn discover<S: CaptureSource>(
		&mut self,
		source: &mut S,
		config: &capture::Config,
	) -> Result<capture::Layout, Error> {
		loop {
			let failure = match source.format(config).await {
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
	async fn run<S, D, O>(
		&mut self,
		source: &mut S,
		config: &capture::Config,
		demand: &mut D,
		output: &mut O,
	) -> Result<(), Error>
	where
		S: CaptureSource,
		D: Demand,
		O: Output,
	{
		let output_layout = self.layout.expect("capture format must be discovered before running");
		'demand: loop {
			// Idle until a listener subscribes; the track ending is a clean exit.
			output.waiting();
			if !demand.used().await {
				return Ok(());
			}

			let mut last_error = None;
			self.reset();

			loop {
				// Opening waits for the first buffer, so race it against demand too. A
				// cancelled open drops the half-built stream and its callback closures.
				output.starting();
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
					opened = source.open(config) => opened,
				};

				let failure = match opened {
					Ok(mut input) => {
						output.live(source.device(&input));
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
				let retryable = failure.is_retryable();
				let failure = failure.into_error();
				output.failed(&failure);
				if !retryable {
					return Err(failure);
				}

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
	Ok(Frame::new(bytes.into(), moq_net::Timestamp::from_micros(timestamp_us)?))
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
		device: Option<capture::Device>,
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

		async fn format(&mut self, _config: &capture::Config) -> Result<capture::Layout, capture::Failure> {
			self.format_attempts.fetch_add(1, Ordering::SeqCst);
			match self.formats.pop_front() {
				Some(Discovery::Error(message)) => Err(capture::Failure::retry(Error::Capture(message.into()))),
				Some(Discovery::Fatal(message)) => Err(capture::Failure::fatal(Error::Capture(message.into()))),
				Some(Discovery::Format(sample_rate, channels)) => Ok(capture::Layout { sample_rate, channels }),
				None => std::future::pending().await,
			}
		}

		async fn open(&mut self, _config: &capture::Config) -> Result<Self::Stream, capture::Failure> {
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

		fn device(&self, stream: &Self::Stream) -> Option<capture::Device> {
			stream.device.clone()
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

	fn config() -> capture::Config {
		capture::Config::default()
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
		let config = config();
		let future = supervisor.discover(&mut source, &config);
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
		let config = config();

		let err = Supervisor::exact()
			.discover(&mut source, &config)
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
			device: Some(device("mock")),
		};
		(events, stream)
	}

	fn with_layout(mut stream: MockStream, sample_rate: u32, channels: u32) -> MockStream {
		stream.layout = capture::Layout { sample_rate, channels };
		stream
	}

	fn with_device(mut stream: MockStream, name: &str) -> MockStream {
		stream.device = Some(device(name));
		stream
	}

	fn device(name: &str) -> capture::Device {
		capture::Device {
			id: name.into(),
			name: name.into(),
			default: false,
			host: "test".into(),
		}
	}

	async fn setup_publication(
		opens: impl IntoIterator<Item = Open>,
	) -> (
		Control,
		Driver,
		MockSource,
		moq_net::track::Subscriber,
		moq_mux::catalog::Producer,
	) {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let mut options = CaptureOptions::default();
		options.capture.source = capture::Source::Microphone(Some("first".into()));
		options.encode.track = Some("audio".into());
		let (control, driver) = Control::build(broadcast, catalog.clone(), options, Supervisor::exact()).unwrap();
		let subscription = consumer.track("audio").unwrap().subscribe(None).await.unwrap();
		(control, driver, source(opens, false), subscription, catalog)
	}

	/// Same, with the caller's encode options, which `setup_publication` fixes.
	async fn setup_encoding(
		channels: u32,
		opens: impl IntoIterator<Item = Open>,
	) -> (
		Control,
		Driver,
		MockSource,
		moq_net::track::Subscriber,
		moq_mux::catalog::Producer,
	) {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let mut options = CaptureOptions::default();
		options.capture.source = capture::Source::Microphone(Some("first".into()));
		options.encode.track = Some("audio".into());
		options.encode.settings.layout = PcmLayout::from_channels(channels).unwrap();
		let (control, driver) = Control::build(broadcast, catalog.clone(), options, Supervisor::exact()).unwrap();
		let subscription = consumer.track("audio").unwrap().subscribe(None).await.unwrap();
		(control, driver, source(opens, false), subscription, catalog)
	}

	async fn wait_for(control: &mut Control, status: Status) -> State {
		tokio::time::timeout(Duration::from_secs(1), async {
			loop {
				let state = control.state();
				if state.status() == status {
					return state;
				}
				control.changed().await.expect("driver still running");
			}
		})
		.await
		.expect("state transition")
	}

	#[tokio::test]
	async fn failed_publication_retries_but_duplicate_start_is_idempotent() {
		let (_events, recovered) = stream(None);
		let recovered = with_device(recovered, "allowed");
		let (mut control, driver, source, _subscription, _catalog) =
			setup_publication([Open::Fatal("permission denied"), Open::Stream(recovered)]).await;
		let attempts = source.attempts.clone();
		let task = tokio::spawn(driver.run_with(source));

		let failed = wait_for(&mut control, Status::Failed).await;
		assert!(matches!(failed.failure(), Some(Error::Capture(message)) if message == "permission denied"));
		assert_eq!(attempts.load(Ordering::SeqCst), 1);

		control.start();
		let live = wait_for(&mut control, Status::Live).await;
		assert_eq!(live.device().map(|device| device.id.as_str()), Some("allowed"));
		assert_eq!(attempts.load(Ordering::SeqCst), 2);

		control.start();
		tokio::task::yield_now().await;
		assert_eq!(attempts.load(Ordering::SeqCst), 2);

		drop(control);
		task.await.unwrap().unwrap();
	}

	#[tokio::test]
	async fn stop_and_replace_release_the_old_input_without_replacing_the_track() {
		let first_drops = Arc::new(AtomicUsize::new(0));
		let (_first_events, first) = stream(Some(first_drops.clone()));
		let first = with_device(first, "first");
		let (_second_events, second) = stream(None);
		let second = with_device(second, "second");
		let (mut control, driver, source, _subscription, _catalog) =
			setup_publication([Open::Stream(first), Open::Stream(second)]).await;
		let task = tokio::spawn(driver.run_with(source));

		wait_for(&mut control, Status::Live).await;
		control.stop();
		wait_for(&mut control, Status::Stopped).await;
		assert_eq!(first_drops.load(Ordering::SeqCst), 1);
		let track = control.track_name().to_string();

		control.replace(capture::Source::Microphone(Some("second".into())));
		control.start();
		let live = wait_for(&mut control, Status::Live).await;
		assert_eq!(live.device().map(|device| device.id.as_str()), Some("second"));
		assert_eq!(control.track_name(), track);

		drop(control);
		task.await.unwrap().unwrap();
	}

	#[tokio::test]
	async fn reports_post_processing_level() {
		let (events, input) = stream(None);
		let (mut control, driver, source, _subscription, _catalog) = setup_publication([Open::Stream(input)]).await;
		let task = tokio::spawn(driver.run_with(source));
		wait_for(&mut control, Status::Live).await;
		assert_eq!(control.level(), Level::default());

		events
			.try_push(Ok(capture::Samples::plain(vec![0.25, -0.5, 1.0, -1.0], false)))
			.unwrap();
		let level = tokio::time::timeout(Duration::from_secs(1), async {
			loop {
				let level = control.level();
				if level != Level::default() {
					return level;
				}
				tokio::task::yield_now().await;
			}
		})
		.await
		.unwrap();
		assert!((level.rms() - 0.760_345_34).abs() < 0.000_001);
		assert_eq!(level.peak(), 1.0);

		drop(control);
		task.await.unwrap().unwrap();
	}

	/// A level is not a lifecycle event, so it must not wake a `changed` waiter.
	#[tokio::test]
	async fn level_updates_do_not_wake_state_waiters() {
		let (events, input) = stream(None);
		let (mut control, driver, source, _subscription, _catalog) = setup_publication([Open::Stream(input)]).await;
		let task = tokio::spawn(driver.run_with(source));
		wait_for(&mut control, Status::Live).await;

		for _ in 0..4 {
			events
				.try_push(Ok(capture::Samples::plain(vec![0.25, -0.5, 1.0, -1.0], false)))
				.unwrap();
		}
		tokio::time::timeout(Duration::from_millis(100), control.changed())
			.await
			.expect_err("a level change woke a state waiter");

		drop(control);
		task.await.unwrap().unwrap();
	}

	/// `publish_capture` hands its caller no controls, so parking on a terminal
	/// failure would hang `moq import capture` instead of reporting the denial.
	#[tokio::test]
	async fn a_publication_without_controls_returns_its_terminal_failure() {
		let (mut control, driver, source, _subscription, _catalog) =
			setup_publication([Open::Fatal("permission denied")]).await;
		let driver = Driver {
			park_on_failure: false,
			..driver
		};

		let err = tokio::time::timeout(Duration::from_secs(1), driver.run_with(source))
			.await
			.expect("a terminal failure parked instead of returning")
			.expect_err("the terminal failure was swallowed");

		assert!(matches!(&err, Error::Capture(message) if message == "permission denied"));
		assert!(control.is_finished());
		assert_eq!(control.changed().await.unwrap().status(), Status::Failed);
		assert!(control.changed().await.is_none());
	}

	/// A retained publication keeps the track and waits to be told what to do.
	#[tokio::test]
	async fn a_terminal_failure_parks_a_retained_publication() {
		let (mut control, driver, source, _subscription, _catalog) =
			setup_publication([Open::Fatal("permission denied")]).await;
		let task = tokio::spawn(driver.run_with(source));

		wait_for(&mut control, Status::Failed).await;
		assert!(!control.is_finished());

		drop(control);
		task.await.unwrap().unwrap();
	}

	/// The meter must not stay pinned at the last buffer after the input closes.
	#[tokio::test]
	async fn stopping_zeroes_the_level() {
		let (events, input) = stream(None);
		let (mut control, driver, source, _subscription, _catalog) = setup_publication([Open::Stream(input)]).await;
		let task = tokio::spawn(driver.run_with(source));
		wait_for(&mut control, Status::Live).await;

		events
			.try_push(Ok(capture::Samples::plain(vec![0.5, -0.5], false)))
			.unwrap();
		tokio::time::timeout(Duration::from_secs(1), async {
			while control.level() == Level::default() {
				tokio::task::yield_now().await;
			}
		})
		.await
		.expect("no level was measured");

		control.stop();
		wait_for(&mut control, Status::Stopped).await;
		assert_eq!(control.level(), Level::default());

		drop(control);
		task.await.unwrap().unwrap();
	}

	/// `State::device` promises the last microphone that failed while live, so
	/// the terminal republish must not erase what the supervisor reported.
	#[tokio::test]
	async fn a_terminal_live_failure_keeps_the_device_that_failed() {
		let (events, input) = stream(None);
		let input = with_device(input, "wired");
		let (mut control, driver, source, _subscription, _catalog) = setup_publication([Open::Stream(input)]).await;
		let task = tokio::spawn(driver.run_with(source));
		wait_for(&mut control, Status::Live).await;

		events
			.try_push(Err(capture::Failure::fatal(Error::Capture("device vanished".into()))))
			.unwrap();

		let failed = wait_for(&mut control, Status::Failed).await;
		assert_eq!(failed.device().map(|device| device.id.as_str()), Some("wired"));
		assert!(matches!(failed.failure(), Some(Error::Capture(message)) if message == "device vanished"));

		// Retained controls park rather than end, so dropping them is the clean exit.
		drop(control);
		task.await.unwrap().unwrap();
	}

	/// The whole point of a retained handle is to outlive a missing device, so
	/// construction must not wait on one and the controls must work meanwhile.
	#[tokio::test]
	async fn controls_are_live_before_the_input_is_discovered() {
		let (mut control, driver, mut source, _subscription, catalog) = setup_publication([]).await;
		// Nothing ever answers a probe, which is a machine with no input device.
		source.formats.clear();
		let attempts = source.format_attempts.clone();
		let task = tokio::spawn(driver.run_with(source));

		assert_eq!(control.track_name(), "audio");
		tokio::time::timeout(Duration::from_secs(1), async {
			while attempts.load(Ordering::SeqCst) == 0 {
				tokio::task::yield_now().await;
			}
		})
		.await
		.expect("the driver never probed the input");

		assert_eq!(control.state().status(), Status::Starting);
		// The layout is unknown, so there is no rendition to advertise yet.
		assert!(catalog.snapshot().audio.renditions.is_empty());

		// A probe nothing answers is still cancellable by the controls.
		control.stop();
		wait_for(&mut control, Status::Stopped).await;
		assert_eq!(attempts.load(Ordering::SeqCst), 1);

		drop(control);
		task.await.unwrap().unwrap();
	}

	/// The rendition the catalog advertises describes the discovered layout, so
	/// it appears only once a probe succeeds.
	#[tokio::test]
	async fn discovery_registers_the_rendition() {
		let (_events, input) = stream(None);
		let (mut control, driver, mut source, _subscription, catalog) = setup_publication([Open::Stream(input)]).await;
		source.formats = [Discovery::Fatal("permission denied"), Discovery::Format(48_000, 2)]
			.into_iter()
			.collect();
		let task = tokio::spawn(driver.run_with(source));

		let failed = wait_for(&mut control, Status::Failed).await;
		assert!(matches!(failed.failure(), Some(Error::Capture(message)) if message == "permission denied"));
		assert!(catalog.snapshot().audio.renditions.is_empty());

		// A terminal probe failure parks like a terminal open failure, so `start`
		// retries it rather than the driver ending with no track.
		control.start();
		wait_for(&mut control, Status::Live).await;
		let renditions = catalog.snapshot().audio.renditions;
		let config = renditions.get("audio").expect("the rendition was never registered");
		assert_eq!(config.sample_rate, 48_000);
		assert_eq!(config.channel_count, 2);

		drop(control);
		task.await.unwrap().unwrap();
	}

	/// A discovered layout the codec rejects must not take the track down with
	/// it: the whole promise of the retained handle is that `replace` can still
	/// point the same track at an input that works.
	#[tokio::test]
	async fn a_rejected_layout_parks_the_publication() {
		let (_events, input) = stream(None);
		// A discrete source cannot be assigned stereo speaker positions implicitly.
		let (mut control, driver, mut source, _subscription, catalog) = setup_encoding(2, [Open::Stream(input)]).await;
		source.formats = [Discovery::Format(48_000, 6), Discovery::Format(48_000, 2)]
			.into_iter()
			.collect();
		let task = tokio::spawn(driver.run_with(source));

		let failed = wait_for(&mut control, Status::Failed).await;
		assert!(failed.failure().is_some());
		assert!(!control.is_finished());
		assert!(catalog.snapshot().audio.renditions.is_empty());

		control.replace(capture::Source::Microphone(Some("second".into())));
		wait_for(&mut control, Status::Live).await;
		assert_eq!(catalog.snapshot().audio.renditions.len(), 1);

		drop(control);
		task.await.unwrap().unwrap();
	}

	/// The controls are meant to live away from the capture driver.
	#[test]
	fn publication_controls_cross_threads() {
		fn assert_send_sync<T: Send + Sync>() {}
		assert_send_sync::<Control>();
		assert_send_sync::<State>();
		assert_send_sync::<Level>();
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
		let config = config();
		let future = supervisor.run(&mut source, &config, &mut demand, &mut output);
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
		let config = config();

		let err = Supervisor::exact()
			.run(&mut source, &config, &mut demand, &mut output)
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
		let config = config();
		let future = supervisor.run(&mut source, &config, &mut demand, &mut output);
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
		let config = config();
		let future = supervisor.run(&mut source, &config, &mut demand, &mut output);
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
		let config = config();
		{
			let future = supervisor.run(&mut source, &config, &mut demand, &mut output);
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
		let config = config();

		{
			let future = supervisor.run(&mut source, &config, &mut demand, &mut output);
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
		let config = config();
		let future = supervisor.run(&mut source, &config, &mut demand, &mut output);
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
		let config = config();

		{
			let future = supervisor.run(&mut source, &config, &mut demand, &mut output);
			tokio::pin!(future);
			poll_pending(future.as_mut()).await;
			assert_eq!(drops.load(Ordering::SeqCst), 0);
		}

		assert_eq!(drops.load(Ordering::SeqCst), 1);
	}

	/// Clock fixtures: the real publication driver, fed by synthetic microphones against a
	/// pinned broadcast clock, graded on the timestamps a subscriber reads back.
	///
	/// Capture stamps a buffer when the driver reads it, so each expectation is bracketed by
	/// the broadcast clock just before the fixture delivers the buffer and just after the
	/// published packet is read back.
	mod clock {
		use std::time::{Duration, Instant, SystemTime};

		use super::*;

		/// Retain every fixture group, so a slow runner never evicts one before it is read.
		const RETAIN: Duration = Duration::from_secs(600);

		type Samples = kio::Queue<Result<capture::Samples, capture::Failure>>;
		type Track = moq_mux::container::Consumer<moq_mux::container::legacy::Wire>;

		struct Fixture {
			epoch: Instant,
			clock: moq_mux::Clock,
			catalog: moq_mux::catalog::Producer,
			consumer: moq_net::broadcast::Consumer,
			publication: Control,
			task: tokio::task::JoinHandle<Result<(), Error>>,
		}

		impl Fixture {
			/// Run a publication over `opens` on a broadcast whose clock began `behind` ago, at `wall`.
			fn start(behind: Duration, wall: SystemTime, opens: impl IntoIterator<Item = Open>) -> Self {
				let epoch = Instant::now()
					.checked_sub(behind)
					.expect("a monotonic clock that far back");
				let clock = moq_mux::Clock::at(epoch, wall).unwrap();
				let mut broadcast = moq_net::broadcast::Info::new().produce();
				let consumer = broadcast.consume();
				let config = moq_mux::catalog::Config::default()
					.with_clock(clock)
					.with_max_age(RETAIN);
				let catalog = moq_mux::catalog::Producer::new(&mut broadcast, config).unwrap();
				let mut options = CaptureOptions::default();
				options.encode.track = Some("audio".into());
				// The broadcast's own clock, as `moq import capture` hands it.
				options.clock = catalog.clock();
				let (publication, driver) =
					Control::build(broadcast, catalog.clone(), options, Supervisor::exact()).unwrap();
				let task = tokio::spawn(driver.run_with(source(opens, false)));
				Self {
					epoch,
					clock,
					catalog,
					consumer,
					publication,
					task,
				}
			}

			/// Subscribe to the audio track, which is what opens the microphone.
			async fn subscribe(&mut self) -> Track {
				let track = self
					.consumer
					.track("audio")
					.unwrap()
					.subscribe(moq_net::track::Subscription::default().with_max_age(RETAIN))
					.await
					.unwrap();
				wait_for(&mut self.publication, Status::Live).await;
				moq_mux::container::Consumer::new(
					track,
					moq_mux::container::legacy::Wire(moq_mux::container::Kind::Audio),
				)
			}

			/// `instant` on the broadcast clock, in microseconds.
			fn at(&self, instant: Instant) -> u64 {
				u64::try_from(instant.duration_since(self.epoch).as_micros()).unwrap()
			}

			/// Deliver one Opus frame of stereo audio and read back the first packet not in
			/// `seen`, asserting it is stamped while the driver held the buffer.
			async fn deliver(&self, samples: &Samples, track: &mut Track, seen: &[u64]) -> u64 {
				let pushed = self.at(Instant::now());
				samples
					.try_push(Ok(capture::Samples::plain(vec![0.1; 1920], false)))
					.unwrap();
				let published = loop {
					let packet = track.read().await.unwrap().expect("a published packet");
					let timestamp = u64::try_from(packet.timestamp.as_micros()).unwrap();
					if !seen.contains(&timestamp) {
						break timestamp;
					}
				};
				let read = self.at(Instant::now());
				assert!(
					(pushed..=read).contains(&published),
					"published {published}us, delivered within {pushed}..={read}us on the broadcast clock"
				);
				published
			}

			/// Stop the publication, as dropping its last control does.
			async fn finish(self) -> (moq_mux::catalog::Producer, moq_net::broadcast::Consumer) {
				drop(self.publication);
				self.task.await.unwrap().unwrap();
				(self.catalog, self.consumer)
			}
		}

		/// A microphone whose first buffer arrives long after the broadcast began stamps it
		/// then, rather than restarting the broadcast at zero.
		#[tokio::test]
		async fn a_late_first_buffer_publishes_its_arrival() {
			let (samples, input) = stream(None);
			let mut fixture = Fixture::start(Duration::from_secs(5), SystemTime::now(), [Open::Stream(input)]);
			let mut track = fixture.subscribe().await;

			let published = fixture.deliver(&samples, &mut track, &[]).await;
			assert!(published >= 5_000_000, "{published}us restarted the broadcast at zero");
			fixture.finish().await;
		}

		/// A device that fails and reopens counts its samples from zero again. The broadcast
		/// continues forward from the reopen instead of rewinding to the old epoch.
		#[tokio::test]
		async fn a_device_restart_continues_forward() {
			let (first, failing) = stream(None);
			let (second, reopened) = stream(None);
			let mut fixture = Fixture::start(
				Duration::from_secs(1),
				SystemTime::now(),
				[Open::Stream(failing), Open::Stream(reopened)],
			);
			let mut track = fixture.subscribe().await;

			let before = fixture.deliver(&first, &mut track, &[]).await;
			first
				.try_push(Err(capture::Failure::retry(Error::Capture("unplugged".into()))))
				.unwrap();
			wait_for(&mut fixture.publication, Status::Failed).await;
			// The supervisor backs off before reopening, so the restart lands at least that late.
			let after = fixture.deliver(&second, &mut track, &[before]).await;
			let backoff = u64::try_from(RETRY_MIN.as_micros()).unwrap();
			assert!(
				after >= before + backoff,
				"{after}us rewound or collapsed the {backoff}us backoff"
			);
			fixture.finish().await;
		}

		/// Releasing the microphone while nobody listens keeps the broadcast clock running:
		/// the buffer after a resume lands after the real idle gap.
		#[tokio::test]
		async fn a_restart_after_idle_keeps_the_gap() {
			let idle = Duration::from_millis(300);
			let (first, input) = stream(None);
			let (second, resumed) = stream(None);
			let mut fixture = Fixture::start(
				Duration::from_secs(1),
				SystemTime::now(),
				[Open::Stream(input), Open::Stream(resumed)],
			);

			let mut track = fixture.subscribe().await;
			let before = fixture.deliver(&first, &mut track, &[]).await;
			drop(track);
			wait_for(&mut fixture.publication, Status::Waiting).await;

			tokio::time::sleep(idle).await;

			let mut track = fixture.subscribe().await;
			let after = fixture.deliver(&second, &mut track, &[before]).await;
			assert!(
				after - before >= u64::try_from(idle.as_micros()).unwrap(),
				"the {idle:?} idle gap collapsed to {}us",
				after - before
			);
			fixture.finish().await;
		}

		/// The wall mapping is pinned when the broadcast clock is built. A system clock stepped
		/// an hour since then retimes neither the published timestamps nor the advertised mapping.
		#[tokio::test]
		async fn a_system_wall_adjustment_retimes_nothing() {
			// Whole seconds, so the advertised mapping holds it exactly.
			let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap();
			let wall = SystemTime::UNIX_EPOCH + Duration::from_secs(now.as_secs() - 3600);
			let (samples, input) = stream(None);
			let mut fixture = Fixture::start(Duration::from_secs(1), wall, [Open::Stream(input)]);
			let advertised = fixture.catalog.snapshot().clock;
			assert_eq!(advertised, Some(fixture.clock.wall()));

			let mut track = fixture.subscribe().await;
			let published = fixture.deliver(&samples, &mut track, &[]).await;

			// The catalog maps to walls at millisecond precision.
			let mapped = advertised
				.unwrap()
				.wall_clock(moq_net::Timestamp::from_micros(published).unwrap())
				.unwrap();
			assert_eq!(mapped, wall + Duration::from_millis(published / 1000));
			assert_eq!(fixture.catalog.snapshot().clock, advertised);
			fixture.finish().await;
		}

		/// A recording replays what the live edge published: the archive's segment records
		/// carry the live timestamps across an idle restart, with the idle gap left in.
		#[tokio::test]
		async fn retained_archive_playback_keeps_the_live_timestamps() {
			let (first, input) = stream(None);
			let (second, resumed) = stream(None);
			let mut fixture = Fixture::start(
				Duration::from_secs(1),
				SystemTime::now(),
				[Open::Stream(input), Open::Stream(resumed)],
			);
			let mut live = Vec::new();
			let mut timeline = None;

			for samples in [&first, &second] {
				let mut track = fixture.subscribe().await;
				live.push(fixture.deliver(samples, &mut track, &live).await);
				// The rendition, and with it the archive, registers once the input is discovered.
				if timeline.is_none() {
					let section = fixture
						.catalog
						.snapshot()
						.archive
						.expect("the audio track enrolls an archive");
					timeline = Some(
						moq_mux::timeline::Consumer::<()>::subscribe(&fixture.consumer, &section)
							.await
							.unwrap(),
					);
				}
				drop(track);
				wait_for(&mut fixture.publication, Status::Waiting).await;
				// Idle past the minimum segment, so each run is archived as its own segment.
				tokio::time::sleep(moq_mux::timeline::DEFAULT_DURATION_MIN + Duration::from_millis(100)).await;
			}

			let (catalog, _consumer) = fixture.finish().await;
			catalog.timeline().finish().unwrap();
			let mut timeline = timeline.unwrap();
			let mut archived = Vec::new();
			while let Some(event) = timeline.next().await.unwrap() {
				match event {
					moq_mux::timeline::Event::Push { entry, .. } => archived.push(entry),
					other => panic!("unexpected timeline event {other:?}"),
				}
			}

			assert_eq!(archived.len(), live.len(), "one segment per capture run: {archived:?}");
			for (entry, live) in archived.iter().zip(&live) {
				// The archive keeps millisecond precision.
				assert_eq!(entry.pts.as_micros() / 1000, u128::from(*live / 1000), "{archived:?}");
				assert!(entry.tracks.contains_key("audio"), "{archived:?}");
			}
			let first = &archived[0];
			assert!(
				archived[1].pts.as_micros() >= first.pts.as_micros() + first.duration.as_micros(),
				"the resumed segment overlaps the one before it: {archived:?}"
			);
		}
	}
}
