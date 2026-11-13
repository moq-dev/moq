//! Publish a capture consumer as an encoded audio track.

use std::fmt;
use std::sync::Arc;
use std::task::Poll;

use moq_net::time::Instant;

use moq_mux::catalog::hang::CatalogExt;

use super::{Options, Producer};
use crate::capture;
use crate::{Error, Frame, Level};

/// Capture and encode settings for [`publish_capture`].
///
/// Samples are stamped on the catalog's [`clock`](moq_mux::catalog::Producer::clock), the one
/// its consumers are told about, so a concurrent video capture on the same catalog stays aligned.
/// A microphone buffer is placed at the capture instant of its first sample, mapped through the
/// catalog clock's own correlation. A host that reports no usable capture time, and system audio,
/// are stamped once by the capture producer before fanout.
///
/// `#[non_exhaustive]`: construct via [`Capture::new`] and set
/// fields, so new publication settings can be added without changing
/// [`publish_capture`].
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Capture {
	/// An independent reader from the shared capture producer.
	pub capture: capture::Consumer,
	/// The track's stable codec and encode settings.
	pub encode: Options,
	/// Retain raw audio up to the track's `max_age` and encode only while subscribed.
	///
	/// On by default. Capture stays open, and a subscriber encodes the retained past before
	/// continuing live. Tracks with an unlimited `max_age` keep 30 seconds, the default media max age.
	/// Set false to encode continuously instead.
	pub retain: bool,
}

impl Capture {
	/// Publish samples from this independent capture consumer.
	pub fn new(capture: capture::Consumer) -> Self {
		Self {
			capture,
			encode: Options::default(),
			retain: true,
		}
	}
}

/// The capture publication's current lifecycle state.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Status {
	/// Raw audio is retained until a subscriber needs encoding.
	Waiting,
	/// The [`Driver`] has not started yet.
	Starting,
	/// Post-processing samples are being encoded and published.
	Live,
	/// The source failed, or the driver ended with a conversion, encoding, or
	/// publishing error.
	///
	/// After a source failure the driver keeps serving retained history on demand
	/// until it is served or expires, then returns the failure.
	Failed(Error),
	/// The MoQ track ended, so the driver has stopped.
	Ended,
	/// The [`Driver`] or its [`run`](Driver::run) future was dropped before it
	/// finished, cancelling the track.
	Cancelled,
}

/// A snapshot of a capture publication's lifecycle.
///
/// Levels are deliberately not here: they change every buffer, so they would
/// drown out the transitions [`Publication::changed`] exists to report. Read
/// them with [`Publication::level`] instead.
#[derive(Clone, Debug)]
pub struct State {
	status: Status,
	source: capture::Source,
	device: Option<capture::Device>,
}

impl State {
	/// The publication's current lifecycle state.
	pub fn status(&self) -> &Status {
		&self.status
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
}

#[derive(Clone, Debug)]
struct Published {
	state: State,
	revision: u64,
}

/// Capture audio and publish it as an encoded MoQ track.
///
/// Registers the rendition before any samples are read, and returns a handle
/// that observes the publication plus the [`Driver`] that encodes it.
pub fn publish_capture<E: CatalogExt>(
	mut broadcast: moq_net::broadcast::Producer,
	catalog: moq_mux::catalog::Producer<E>,
	options: Capture,
) -> Result<(Publication, Driver), Error> {
	let clock = catalog.clock();
	let input = options.capture.input().clone();
	let producer = Producer::new(&mut broadcast, catalog, input, &options.encode)?;
	let track_name: Arc<str> = producer.track_name().into();
	let reservation = options.encode.bandwidth.reserve(&producer.demand(), producer.bitrate());
	let state = kio::Producer::new(Published {
		state: State {
			status: Status::Starting,
			source: options.capture.source().clone(),
			device: options.capture.device().cloned(),
		},
		revision: 0,
	});
	let level = kio::Producer::new(Level::default());
	let publication = Publication {
		state: state.consume(),
		level: level.consume(),
		observed: 0,
		track_name,
	};
	let driver = Driver {
		_broadcast: broadcast,
		_reservation: reservation,
		producer: Some(producer),
		capture: options.capture,
		retain: options.retain,
		clock,
		state,
		level,
	};
	Ok((publication, driver))
}

/// A handle that observes a capture publication.
///
/// Clones observe the same MoQ track. Dropping it changes nothing: the
/// [`Driver`] owns the publication.
pub struct Publication {
	state: kio::Consumer<Published>,
	level: kio::Consumer<Level>,
	observed: u64,
	track_name: Arc<str>,
}

impl Clone for Publication {
	fn clone(&self) -> Self {
		Self {
			state: self.state.clone(),
			level: self.level.clone(),
			observed: self.state.read().revision,
			track_name: self.track_name.clone(),
		}
	}
}

impl fmt::Debug for Publication {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Publication")
			.field("track_name", &self.track_name)
			.field("state", &self.state.read().state)
			.finish_non_exhaustive()
	}
}

impl Publication {
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

	/// Whether the driver has exited.
	pub fn is_finished(&self) -> bool {
		self.state.is_closed()
	}
}

/// Publishing a consumer can be spawned independently of the device's platform affinity.
#[allow(dead_code, reason = "a compile-time `Send` assertion, never called")]
fn assert_driver_send(driver: Driver) {
	fn is_send<T: Send>(_: &T) {}
	is_send(&driver.run());
}

/// The task that encodes one consumer without owning its capture device.
///
/// Dropping it, or its [`run`](Self::run) future, cancels the track and leaves the
/// shared capture and its other consumers running.
pub struct Driver {
	_broadcast: moq_net::broadcast::Producer,
	_reservation: moq_net::bandwidth::Reservation,
	producer: Option<Producer>,
	capture: capture::Consumer,
	retain: bool,
	clock: moq_mux::Clock,
	state: kio::Producer<Published>,
	level: kio::Producer<Level>,
}

impl fmt::Debug for Driver {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Driver").finish_non_exhaustive()
	}
}

impl Drop for Driver {
	fn drop(&mut self) {
		// Dropped before `run` finished, e.g. a sibling task failed: say so rather than `Dropped`.
		if let Some(producer) = self.producer.take() {
			producer.abort(moq_net::Error::Cancel);
		}
		// A no-op once `run` closed the state on its own terminal status.
		publish_state(&self.state, Status::Cancelled);
		publish_level(&self.level, Level::default());
	}
}

/// Why [`Driver::drive`] stopped without an encoding or publishing error.
enum End {
	/// The track closed.
	Closed,
	/// The source failed, and every retained frame since was served or expired.
	Failed(Error),
}

impl Driver {
	/// Encode until the track closes or the source fails, then end the track.
	///
	/// A source failure is terminal, yet retained history stays reachable: subscribers
	/// still encode it on demand until it is served or expires. The driver then ends the
	/// track with the failure, keeping what it published, and returns it.
	pub async fn run(mut self) -> Result<(), Error> {
		let end = self.drive().await;
		let mut producer = self.producer.take().unwrap();
		let now = self.clock.now();
		// Held until the state closes, so the track outlives its terminal status.
		let mut finished = None;
		let result = match end {
			Ok(End::Failed(failure)) => {
				// Retention already reports the failure. It stays the cause even when the track
				// can no longer end cleanly, such as one aborted while it still retained audio.
				let cause = moq_net::Error::Transport(failure.to_string());
				match producer.set_active(false, now) {
					Ok(()) => finished = producer.fail(cause).ok(),
					Err(_) => producer.abort(cause),
				}
				Err(failure)
			}
			// The track closed, or encoding or publishing failed.
			end => {
				let ended = match end.and_then(|_| producer.set_active(false, now)) {
					// A finish that fails ends the track with an error itself.
					Ok(()) => producer.finish().map(|track| finished = Some(track)),
					Err(err) => {
						producer.abort(moq_net::Error::Transport(err.to_string()));
						Err(err)
					}
				};
				match ended {
					Ok(()) => {
						self.update(Status::Ended);
						Ok(())
					}
					Err(err) => {
						self.update(Status::Failed(err.clone()));
						Err(err)
					}
				}
			}
		};
		self.publish_level(Level::default());
		let _ = self.state.close();
		drop(finished);
		result
	}

	async fn drive(&mut self) -> Result<End, Error> {
		let producer = self.producer.as_mut().unwrap();
		let track = producer.track().clone();
		let mut retention = Retention {
			demand: track.demand(),
			retain: self.retain,
			clock: self.clock.clone(),
			state: &self.state,
			level: &self.level,
			failed: false,
		};
		let reader = &mut self.capture;
		retention.refresh(producer)?;
		let failure = loop {
			tokio::select! {
				biased;
				_ = track.closed() => return Ok(End::Closed),
				samples = reader.read() => {
					let samples = match samples {
						Ok(samples) => samples,
						Err(error) => break error,
					};
					if samples.gap { producer.reanchor(); }
					if samples.data.is_empty() { continue; }
					let timestamp = stamp(&retention.clock, samples.captured)?;
					retention.level(Level::measure(&samples.data));
					retention.refresh(producer)?;
					producer.write(&frame(&samples.data, timestamp)?)?;
				}
				changed = retention.changed(producer) => changed?,
			}
		};

		// The source failed, yet retained history stays reachable: subscribers still encode
		// it on demand until none is left or nothing can want it any more.
		retention.failed(&failure);
		retention.refresh(producer)?;
		while producer.retains() {
			tokio::select! {
				biased;
				_ = track.closed() => break,
				changed = retention.changed(producer) => changed?,
			}
		}
		Ok(End::Failed(failure))
	}

	fn update(&self, status: Status) {
		publish_state(&self.state, status);
	}

	fn publish_level(&self, level: Level) {
		publish_level(&self.level, level);
	}
}

/// Publish a lifecycle transition, skipping one that changes nothing.
///
/// Levels and repeated capture states must not wake lifecycle-only observers.
fn publish_state(state: &kio::Producer<Published>, status: Status) {
	let Ok(mut published) = state.write() else { return };
	// A repeated status changes nothing an observer can see; a failure always carries news.
	if !matches!(status, Status::Failed(_))
		&& std::mem::discriminant(&published.state.status) == std::mem::discriminant(&status)
	{
		return;
	}
	published.state.status = status;
	published.revision = published.revision.wrapping_add(1);
}

/// Levels ride their own channel: they change every buffer, so folding them
/// into [`State`] would wake every [`Publication::changed`] waiter at the
/// capture rate and rebuild the whole snapshot to do it.
fn publish_level(level: &kio::Producer<Level>, now: Level) {
	let Ok(mut published) = level.write() else { return };
	if *published != now {
		*published = now;
	}
}

/// The capture instant `captured` on `clock`, in microseconds.
fn stamp(clock: &moq_mux::Clock, captured: Instant) -> Result<u64, Error> {
	// Clamp to the nearest instant the clock can name: a host can stamp a
	// chunk after now, or a newly created clock can begin after capture.
	let timestamp = match clock.capture(captured) {
		Err(moq_mux::Error::InvalidCapture) => {
			let now = clock.now();
			let ago = Instant::now().saturating_duration_since(captured).as_micros();
			if ago > now.as_micros() {
				moq_net::Timestamp::ZERO
			} else {
				now
			}
		}
		timestamp => timestamp?,
	};
	Ok(timestamp.as_micros() as u64)
}

/// Encodes retained history while a subscriber wants it, and expires it otherwise.
///
/// Runs whether or not the source is still capturing, so history stays reachable
/// after a source failure. Publishes whether the track is live or waiting, until
/// the source fails.
struct Retention<'a> {
	demand: moq_net::track::Demand,
	retain: bool,
	clock: moq_mux::Clock,
	state: &'a kio::Producer<Published>,
	level: &'a kio::Producer<Level>,
	/// The source failed, so the status stays failed.
	failed: bool,
}

impl Retention<'_> {
	/// Expire retained history, then encode one retained frame if wanted.
	fn refresh(&self, producer: &mut Producer) -> Result<(), Error> {
		let active = !self.retain || self.demand.wanted().is_some();
		producer.set_active(active, self.clock.now())?;
		if !self.failed {
			publish_state(self.state, if active { Status::Live } else { Status::Waiting });
		}
		Ok(())
	}

	/// Wait for a change in demand, the next expiry, or the next retained frame to encode,
	/// then [`refresh`](Self::refresh).
	async fn changed(&mut self, producer: &mut Producer) -> Result<(), Error> {
		if producer.backlog_ready() {
			// Keep the microphone queue moving between codec frames during a long catch-up.
			tokio::task::yield_now().await;
			return self.refresh(producer);
		}
		let delay = producer
			.backlog_deadline()
			.map(|at| at.saturating_sub(self.clock.now().into()));
		tokio::select! {
			changed = self.demand.wanted_changed() => { changed?; }
			_ = async {
				match delay { Some(delay) => kio::clock::sleep(delay).await, None => std::future::pending().await }
			} => {}
		}
		self.refresh(producer)
	}

	fn level(&self, level: Level) {
		publish_level(self.level, level);
	}

	/// The source failed: the level goes quiet and the status stays failed.
	fn failed(&mut self, failure: &Error) {
		publish_level(self.level, Level::default());
		publish_state(self.state, Status::Failed(failure.clone()));
		self.failed = true;
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
		publication: Publication,
		driver: Driver,
		broadcast: moq_net::broadcast::Consumer,
		catalog: moq_mux::catalog::Producer,
		epoch: Instant,
	}

	fn fixture(retain: bool) -> Fixture {
		fixture_codec(retain, Codec::Pcm)
	}

	fn fixture_codec(retain: bool, codec: Codec) -> Fixture {
		let (capture, samples) = capture::test_source();
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let epoch = Instant::now().saturating_sub(Duration::from_secs(1));
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
		options.retain = retain;
		let (publication, driver) = publish_capture(broadcast, catalog.clone(), options).unwrap();
		Fixture {
			capture,
			samples,
			publication,
			driver,
			broadcast: consumer,
			catalog,
			epoch,
		}
	}

	/// Subscribe with a `max` delay, which bounds how much retained history the encoder replays.
	async fn subscribe(broadcast: &moq_net::broadcast::Consumer, max: Duration) -> Track<Wire> {
		let subscription =
			moq_net::track::Subscription::default().with_delay(moq_net::track::Delay::default().with_max(max));
		let subscriber = broadcast.track("audio").unwrap().subscribe(subscription).await.unwrap();
		Track::new(subscriber, Wire(Kind::Audio))
	}

	async fn packet(track: &mut Track<Wire>) -> moq_mux::container::Frame {
		loop {
			if let Some(Read::Frame { frame, .. }) = track.read().await.unwrap() {
				return frame;
			}
		}
	}

	async fn status(publication: &mut Publication, reached: impl Fn(&Status) -> bool) {
		while !reached(publication.state().status()) {
			publication.changed().await.expect("driver still alive");
		}
	}

	/// Wait until the driver has read and retained a buffer, which publishes its level.
	async fn retained(publication: &Publication) {
		while publication.level() == Level::default() {
			tokio::task::yield_now().await;
		}
	}

	/// An HLS-only viewer of an on-demand capture, as a default `moq import capture` publishes,
	/// starts it and gets a playlist listing both renditions. The export holds every media track
	/// wanted while the catalog declares no join interval, so the capture produces the media the
	/// interval is measured from, and the video, captured only while wanted too, follows.
	#[tokio::test(start_paused = true)]
	async fn an_hls_viewer_starts_an_on_demand_capture() {
		tokio::task::LocalSet::new()
			.run_until(async {
				let (capture, samples) = capture::test_source();
				let (origin, origin_driver) = moq_net::origin::Producer::new(Default::default());
				tokio::spawn(moq_net::time::run(origin_driver));
				let publication = origin.route("live", moq_net::origin::RouteOptions::hidden()).unwrap();
				publication.announce().unwrap();
				let mut broadcast = publication.broadcast().clone();
				let catalog =
					moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();

				// Video captured only while wanted, its GOP declared as the encoder does.
				let net = broadcast
					.create_track("video", catalog.track_info(hang::catalog::PRIORITY.video))
					.unwrap();
				let mut config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
				config.coded_width = Some(320);
				config.coded_height = Some(240);
				let mut video = catalog
					.reserve()
					.with_interval(Duration::from_secs(2))
					.video(
						net,
						moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
						config,
					)
					.unwrap();
				let wanted = video.demand();

				// Audio retains its PCM until wanted, the capture default.
				let mut options = Capture::new(capture.consume());
				options.encode.track = Some("audio".into());
				assert!(options.retain, "retained by default");
				let (_publication, driver) = publish_capture(broadcast.clone(), catalog.clone(), options).unwrap();
				let capture_task = tokio::task::spawn_local(capture.run());
				let encode_task = tokio::spawn(driver.run());

				let remote = origin
					.consume()
					.follow("live", None)
					.unwrap()
					.available()
					.await
					.unwrap()
					.unwrap();
				let source = moq_mux::Source::new(origin.consume(), remote);
				let (broadcaster, export) =
					moq_hls::export::Broadcaster::new(source, moq_hls::export::Config::default()).unwrap();
				tokio::spawn(export.run());

				// The capture runs on, writing video only while the export wants it.
				let listed = |kind, name| {
					broadcaster.rendition(kind, name).is_some_and(|rendition| {
						rendition.media_playlist(None).is_ok_and(|playlist| playlist.is_some())
					})
				};
				let mut frame = 0u64;
				while !(listed(moq_hls::export::Kind::Video, "video") && listed(moq_hls::export::Kind::Audio, "audio"))
				{
					assert!(frame < 1_000, "the viewer never got both renditions listed");
					samples.try_push(Ok(Samples::plain(vec![0.25; 1920], false))).unwrap();
					if frame.is_multiple_of(5) && wanted.wanted().is_some() {
						// A 2s GOP, opening with a keyframe once the capture starts.
						let keyframe = video.needs_keyframe() || frame.is_multiple_of(100);
						let payload: &[u8] = match keyframe {
							true => &[0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0x40, 0x01, 0xf0, 0x00],
							false => &[0x31, 0x00, 0x00],
						};
						video
							.write(moq_mux::container::Frame {
								timestamp: catalog.timestamp(None).unwrap(),
								payload: bytes::Bytes::copy_from_slice(payload),
								keyframe,
								duration: None,
							})
							.unwrap();
					}
					kio::clock::sleep(Duration::from_millis(20)).await;
					frame += 1;
				}
				assert!(
					catalog.snapshot().media.join.is_some(),
					"the capture declared its interval"
				);

				drop(broadcaster);
				encode_task.abort();
				capture_task.abort();
			})
			.await;
	}

	#[tokio::test(start_paused = true)]
	async fn one_capture_feeds_a_bus_and_encoder_and_losing_the_bus_does_not_stop_encoding() {
		tokio::task::LocalSet::new()
			.run_until(async {
				let Fixture {
					capture,
					samples,
					driver,
					broadcast,
					catalog: _catalog,
					epoch,
					..
				} = fixture(false);
				let mut observer = capture.consume();
				let (bus, mut bus_driver) = Bus::new(BusConfig::default()).unwrap();
				let strip = Strip::default();
				let program = bus.mix();
				program.route(&strip);
				let mut program = program.reader();
				let mut mix = |frames: usize| {
					bus_driver.advance(frames);
					match program.poll_read(&kio::Waiter::noop()) {
						Poll::Ready(Some(block)) => block.data,
						_ => panic!("the program mix is live"),
					}
				};
				let mut bus_run = Box::pin(bus.capture(capture.consume(), strip).unwrap().run());
				// Register the bus channel and park its reader before the first mix read.
				assert!(
					std::future::poll_fn(|cx| Poll::Ready(std::future::Future::poll(bus_run.as_mut(), cx)))
						.await
						.is_pending()
				);
				let mut bus_pcm = Vec::new();
				let mut expected = Vec::new();
				mix(960);
				let capture_task = tokio::task::spawn_local(capture.run());
				let encode_task = tokio::spawn(driver.run());
				let mut track = subscribe(&broadcast, Duration::ZERO).await;
				for (index, gap) in [false, true, false].into_iter().enumerate() {
					let captured = epoch.saturating_add(Duration::from_millis([100, 200, 220][index]));
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
					bus_pcm.extend_from_slice(&mix(960));
					expected.extend_from_slice(&observed.data);
				}
				// The bus begins with 50 ms of silence to absorb jitter; drain that
				// fixed latency and compare every input sample in order.
				bus_pcm.extend_from_slice(&mix(2400));
				assert_eq!(&bus_pcm[4800..], expected);
				drop(bus_run);
				let captured = epoch.saturating_add(Duration::from_millis(240));
				let mut block = Samples::plain(vec![0.625; 1920], false);
				block.captured = Some(captured);
				samples.try_push(Ok(block)).unwrap();
				observer.read().await.unwrap();
				assert_eq!(packet(&mut track).await.timestamp.as_micros(), 240_000);
				encode_task.abort();
				assert!(encode_task.await.unwrap_err().is_cancelled());
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
					publication,
					driver,
					broadcast: _,
					catalog: _catalog,
					..
				} = fixture(false);
				let mut observer = capture.consume();
				let (bus, _bus_driver) = Bus::new(BusConfig::default()).unwrap();
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
				assert!(
					matches!(publication.state().status(), Status::Failed(error) if matches!(error, Error::Capture(message) if message == "device lost"))
				);
				assert!(publication.is_finished());
			})
			.await;
	}

	#[tokio::test(start_paused = true)]
	async fn retained_history_is_encoded_on_demand_or_expires() {
		tokio::task::LocalSet::new()
			.run_until(async {
				for expire in [false, true] {
					let Fixture {
						capture,
						samples,
						mut publication,
						driver,
						broadcast,
						catalog: _catalog,
						epoch,
					} = fixture(true);
					let mut observer = capture.consume();
					let capture_task = tokio::task::spawn_local(capture.run());
					let encode_task = tokio::spawn(driver.run());
					status(&mut publication, |status| matches!(status, Status::Waiting)).await;
					let mut block = Samples::plain(vec![0.25; 1920], false);
					block.captured = Some(epoch.saturating_add(Duration::from_millis(100)));
					samples.try_push(Ok(block)).unwrap();
					observer.read().await.unwrap();
					retained(&publication).await;
					if expire {
						// Past the track's 30 s max_age.
						tokio::time::advance(Duration::from_secs(31)).await;
					}
					let mut track = subscribe(&broadcast, Duration::from_secs(30)).await;
					if expire {
						// Only fresh capture, resuming after the discontinuity, reaches the subscriber.
						samples.try_push(Ok(Samples::plain(vec![0.5; 1920], true))).unwrap();
						observer.read().await.unwrap();
						assert!(packet(&mut track).await.timestamp.as_micros() > 30_000_000);
					} else {
						assert_eq!(packet(&mut track).await.timestamp.as_micros(), 100_000);
						assert!(matches!(publication.state().status(), Status::Live));
					}
					encode_task.abort();
					assert!(encode_task.await.unwrap_err().is_cancelled());
					capture_task.abort();
					assert!(capture_task.await.unwrap_err().is_cancelled());
				}
			})
			.await;
	}

	#[tokio::test(start_paused = true)]
	async fn a_failed_source_serves_retained_history_then_ends_with_the_failure() {
		tokio::task::LocalSet::new()
			.run_until(async {
				for subscribe_late in [true, false] {
					let Fixture {
						capture,
						samples,
						mut publication,
						driver,
						broadcast,
						catalog: _catalog,
						epoch,
					} = fixture(true);
					let mut observer = capture.consume();
					let capture_task = tokio::task::spawn_local(capture.run());
					let encode_task = tokio::spawn(driver.run());
					status(&mut publication, |status| matches!(status, Status::Waiting)).await;
					let mut block = Samples::plain(vec![0.25; 1920], false);
					block.captured = Some(epoch.saturating_add(Duration::from_millis(100)));
					samples.try_push(Ok(block)).unwrap();
					observer.read().await.unwrap();
					retained(&publication).await;
					samples.try_push(Err(Error::Capture("device lost".into()))).unwrap();
					status(&mut publication, |status| matches!(status, Status::Failed(_))).await;
					assert!(
						matches!(publication.state().status(), Status::Failed(error) if matches!(error, Error::Capture(message) if message == "device lost"))
					);
					// Failure is terminal, but the publication still serves what it retained.
					assert!(!publication.is_finished());
					let track = if subscribe_late {
						let mut track = subscribe(&broadcast, Duration::from_secs(30)).await;
						assert_eq!(packet(&mut track).await.timestamp.as_micros(), 100_000);
						Some(track)
					} else {
						None
					};
					// Once served, or expired unwanted, the history is gone and the driver reports the failure.
					let result = encode_task.await.unwrap();
					assert!(matches!(result, Err(Error::Capture(message)) if message == "device lost"));
					assert!(publication.is_finished());
					assert!(matches!(publication.state().status(), Status::Failed(_)));
					if let Some(mut track) = track {
						// The track keeps what it published, then ends with the failure rather than cleanly.
						let error = loop {
							match track.read().await {
								Ok(Some(_)) => {}
								Ok(None) => panic!("the failed track ended cleanly"),
								Err(error) => break error,
							}
						};
						assert!(error.to_string().contains("device lost"), "{error}");
					}
					assert!(capture_task.await.unwrap().is_err());
				}
			})
			.await;
	}

	#[tokio::test(start_paused = true)]
	async fn dropping_an_unfinished_driver_cancels_its_track() {
		let Fixture {
			driver,
			broadcast,
			mut publication,
			..
		} = fixture(false);
		let mut track = subscribe(&broadcast, Duration::ZERO).await;
		drop(driver);
		let error = track.read().await.unwrap_err();
		assert!(format!("{error:?}").contains("Cancel"), "{error:?}");
		let state = publication.changed().await.expect("the cancellation is observed");
		assert!(matches!(state.status(), Status::Cancelled));
		assert!(publication.changed().await.is_none());
		assert!(publication.is_finished());
	}

	/// A gap resumes on the shared capture clock, unless the block before it ran past that: a
	/// timeline only moves forward, so it then resumes where that block's packet ends.
	#[tokio::test(start_paused = true)]
	async fn opus_uses_the_shared_capture_clock_including_gaps_and_clamping() {
		tokio::task::LocalSet::new()
			.run_until(async {
				for placement in [0, 1, 2, 3] {
					let Fixture {
						capture,
						samples,
						driver,
						broadcast,
						catalog: _catalog,
						epoch,
						..
					} = fixture_codec(false, Codec::Opus);
					let mut observer = capture.consume();
					let capture_task = tokio::task::spawn_local(capture.run());
					let encode_task = tokio::spawn(driver.run());
					let mut track = subscribe(&broadcast, Duration::ZERO).await;
					let now = Instant::now();
					let (captured, expected) = match placement {
						0 => (Some(now.saturating_sub(Duration::from_millis(40))), 960_000),
						1 => (Some(epoch.saturating_sub(Duration::from_millis(10))), 0),
						2 => (Some(now.saturating_add(Duration::from_secs(1))), 1_000_000),
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
					// The first block is one 20 ms packet.
					assert_eq!(next.timestamp.as_micros(), (expected + 20_000).max(1_000_000));
					encode_task.abort();
					assert!(encode_task.await.unwrap_err().is_cancelled());
					capture_task.abort();
					assert!(capture_task.await.unwrap_err().is_cancelled());
				}
			})
			.await;
	}
}
