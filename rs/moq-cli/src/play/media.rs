//! The media task: subscribe, pick renditions, and decode into the window and
//! the speaker.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use hang::moq_net;
use moq_mux::catalog::{self, Stream};
use moq_mux::follow::{Event as FollowEvent, Instances};
// The ambient clock, which follows tokio's paused clock when a test drives the
// playout clock itself.
use moq_clock::Instant;

use super::args::Args;
use super::output::{Output, Sink, Speaker};
use super::playback::{Kind, Playback, Track, joined};
use super::timeline::{AudioTimeline, Presentation, Role, Transition, fit, timestamp};
use super::video::Video;
use super::window::Event;

/// What a frame that starts a new decoded timeline crossed.
enum Crossing {
	/// A declared end, whose transition the frame resumes.
	Resume(Transition),
	/// Lost media: no end was declared.
	Loss,
}

/// The floor on the speaker's buffer, whatever delay was asked for. The device
/// pulls on a fixed clock, so a ring shallower than this drops out on ordinary
/// network jitter, and one with no depth at all can never be read from.
const AUDIO_BUFFER_MIN: Duration = Duration::from_millis(50);

/// How far the speaker may hold more than its target, ahead of a write, before
/// the write skips back onto it.
///
/// The level moves by a device period each time the speaker pulls, so a margin
/// under that would skip on ordinary cadence. Anything over it is latency nobody
/// asked for, since a live stream arrives as fast as it plays and never drains
/// the excess on its own.
const AUDIO_SLACK: Duration = Duration::from_millis(20);

/// Maximum sample frames per silence write, independent of the catalog rate.
const AUDIO_SILENCE: usize = 960;

/// How much longer than the speaker holds to wait for it to drain. A device that
/// never opens reports its queue as full forever, and a truncated tail beats
/// hanging on the way out.
const AUDIO_DRAIN_GRACE: Duration = Duration::from_secs(1);

/// Everything the media task needs to fill the window and the speaker.
pub(super) struct Media<O: Output> {
	pub(super) origin: moq_net::origin::Consumer,
	pub(super) broadcast: String,
	pub(super) args: Args,
	pub(super) video: Arc<Mutex<VecDeque<moq_video::Frame>>>,
	pub(super) presentation: Arc<Mutex<Presentation>>,
	pub(super) drained: Arc<tokio::sync::Notify>,
	pub(super) output: O,
}

impl<O: Output> Media<O> {
	pub(super) async fn run(self) {
		let output = self.output.clone();
		let event = match self.play().await {
			Ok(()) => Event::Ended,
			Err(err) => Event::Failed(format!("{err:#}")),
		};
		output.send(event);
	}

	/// Follow the path's announcements for as long as the origin lasts: a start
	/// plays the broadcast, a restart drops it for the instance that replaced it,
	/// and an end lets what is playing finish while waiting for the next start. An
	/// update is the same instance, which its subscriptions already ride out.
	async fn play(self) -> anyhow::Result<()> {
		let mut follow = Instances::new(&self.origin, &self.broadcast, None)?;
		// The device belongs to the player, so replacing a broadcast never waits
		// for it to close and reopen. Each sink adapts its own rate and layout.
		let device = tokio::sync::OnceCell::new();
		let mut playing = None;

		loop {
			tokio::select! {
				event = follow.next() => {
					let broadcast = match event {
						Ok(FollowEvent::Start(broadcast)) => broadcast,
						Ok(FollowEvent::End) => {
							tracing::info!(broadcast = %self.broadcast, "offline, waiting for it to return");
							continue;
						}
						Err(moq_mux::Error::Moq(moq_net::Error::Closed)) => return Ok(()),
						Err(err) => return Err(err).context("failed to follow the broadcast"),
					};
					tracing::info!(
						broadcast = %self.broadcast,
						epoch = broadcast.info().epoch.as_ref().map(tracing::field::display),
						"online"
					);

					// Another instance's timeline is its own, so nothing of the old one carries over.
					drop(playing.take());
					*self.presentation.lock().unwrap() = Presentation::new(self.args.video_delay());
					self.video.lock().unwrap().clear();
					self.drained.notify_one();
					self.output.send(Event::Wake);
					playing = Some(Box::pin(self.play_broadcast(broadcast, &device)));
				}
				result = async { playing.as_mut().expect("guarded").await }, if playing.is_some() => {
					playing = None;
					match result {
						Ok(()) => tracing::info!(broadcast = %self.broadcast, "broadcast ended"),
						Err(err) if err.is::<Unplayable>() => return Err(err),
						Err(err) => tracing::warn!(broadcast = %self.broadcast, err = format!("{err:#}"), "broadcast ended"),
					}
				}
			}
		}
	}

	/// Play the instance handed out by the follower until its catalog and tracks end.
	async fn play_broadcast(
		&self,
		broadcast: moq_net::broadcast::Consumer,
		device: &tokio::sync::OnceCell<O::Speaker>,
	) -> anyhow::Result<()> {
		let source = moq_mux::Source::new(self.origin.clone(), broadcast.clone());
		let mut catalogs = catalog::Consumer::<()>::new(&broadcast, self.args.catalog_format(&self.broadcast))
			.await
			.context("failed to subscribe to the catalog")?;
		// Applied by hand rather than with `select`, so each snapshot holds the renditions that are
		// actually playing, and so the player can tell a disabled rendition it lets go, which stops
		// now, from one that leaves the catalog, which plays out its tail.
		let selection = self.args.select.selection(None);
		let mut tasks = tokio::task::JoinSet::new();
		let mut playback = Playback::default();
		let mut video_demand: Option<Demand> = None;
		let mut audio_demand: Option<Demand> = None;
		// Shared by an audio rendition and the retired tails still playing beside
		// it, so their sinks mix on one stream: a second stream on an exclusive
		// device would fail to open. Released once none of them is left to hand
		// the presentation anchor back to video; the player keeps the device.
		let mut speaker = None;
		// Retired audio sinks still playing out what they hold.
		let mut tails = tokio::task::JoinSet::new();

		loop {
			if playback.done() {
				tails.join_all().await;
				return Ok(());
			}

			// Only wait when there is nothing on hand to act on. The snapshot that
			// retires a rendition arrives while that rendition is still playing, so
			// the half it stops reads it after the fact, by which time the catalog
			// may have ended and the task set emptied: both branches disarmed, with
			// a replacement still on offer.
			if playback.pending().is_none() {
				tokio::select! {
					result = tasks.join_next(), if !tasks.is_empty() => {
						let ended = joined(result.expect("guarded by is_empty"))?.map(|(kind, (finished, sink))| {
							if kind == Kind::Audio {
								// The tail still sounds, so the speaker keeps the anchor, but a
								// replacement is a track boundary whose timestamps need not
								// continue this one: its first frame re-pins.
								self.presentation.lock().unwrap().restarted();
							}
							// The retired sink still holds a delay of audio, and a replacement
							// holds its own before its first sample sounds. Played one after
							// the other, a rendition switch costs that delay in silence, so the
							// tail plays out while the replacement fills.
							if let Some(sink) = sink {
								tails.spawn(drain(sink));
							}
							(kind, finished)
						});
						if let Some((kind, finished)) = ended {
							playback.ended(kind, finished);
						}
					}
					_ = tails.join_next(), if !tails.is_empty() => {}
					// Followed for as long as it lasts, not just until something is
					// playing: a publisher retires renditions (a transcode ladder
					// resizing under a source that changed resolution) by naming the
					// replacement in a snapshot and only then finishing the track it
					// replaces, so the snapshot that matters lands while both halves
					// are still running.
					snapshot = catalogs.next(), if playback.following() => {
						match snapshot.context("failed to read the catalog")? {
							Some(mut snapshot) => {
								// A playing rendition its publisher disabled is held while nothing else
								// of its kind is enabled, so an unmute plays without a resubscribe.
								let video_disabled = disabled(&video_demand, &snapshot.video.renditions, |config| config.enabled);
								let audio_disabled = disabled(&audio_demand, &snapshot.audio.renditions, |config| config.enabled);
								let playing = moq_mux::select::Selected {
									video: video_demand.iter().map(|demand| demand.name.clone()).collect(),
									audio: audio_demand.iter().map(|demand| demand.name.clone()).collect(),
								};
								selection.retain(&mut snapshot, &playing);
								stop_released(&mut video_demand, video_disabled, &snapshot.video.renditions);
								stop_released(&mut audio_demand, audio_disabled, &snapshot.audio.renditions);
								if let Some(demand) = &mut video_demand
									&& demand.retire_moved(&source, snapshot.video.renditions.get(&demand.name))
								{
									playback.retire(Kind::Video);
								}
								if let Some(demand) = &mut audio_demand
									&& demand.retire_moved(&source, snapshot.audio.renditions.get(&demand.name))
								{
									playback.retire(Kind::Audio);
								}
								playback.received(snapshot);
							},
							None => {
								if !playback.played {
									return Err(Unplayable("the catalog contains no playable audio or video renditions".into()).into());
								}
								playback.catalog_ended = true;
							}
						}
					}
				}
			}

			// The last sink went quiet: nothing holds playback to the speaker's
			// cadence any more, and video takes the anchor back.
			if !playback.playing(Kind::Audio) && tails.is_empty() && speaker.take().is_some() {
				self.presentation.lock().unwrap().stopped();
				self.drained.notify_one();
			}

			// Start whatever isn't playing from the newest snapshot, which is not
			// necessarily the one that just arrived: the half that a retirement
			// stopped reads the snapshot naming its replacement afterwards.
			let Some(snapshot) = playback.pending().cloned() else {
				continue;
			};

			// Why nothing started, so a catalog this build can't play reports the
			// reason instead of leaving a blank window up forever. The decoders are
			// gated by platform and cargo feature (no AV1 without `nvidia`, say), so
			// this covers gaps the codec flags can't be validated against up front.
			let mut rejected = Vec::new();

			if playback.wants(Kind::Video) {
				playback.read(Kind::Video);
				for (name, config) in snapshot.video.enabled() {
					let reference = Track::of(&source, config, name);
					// A finished track has only its newest group left, which already played.
					if playback.finished(&reference) {
						continue;
					}
					// A rendition pointing at a broadcast we can't reach is that
					// rendition's problem, not the catalog's: fall through to the
					// next one like an unsupported codec does.
					let rendition = match source.resolve(config.broadcast.as_ref(), config.epoch.as_ref()).await {
						Ok(rendition) => rendition,
						Err(err) => {
							tracing::warn!(track = name, %err, "cannot resolve video rendition");
							rejected.push(format!("video `{name}`: {err}"));
							continue;
						}
					};
					// Nothing older than the playhead is worth presenting, so the delay
					// doubles as the staleness budget on the wire.
					let max_delay = self.args.video_max_delay(&snapshot.audio);
					let opened = async {
						// The window's renderer refuses a GPU surface it cannot import, so
						// the decoder downloads those instead, all of them on a device
						// that imports none.
						let mut decode = moq_video::decode::Config::new();
						decode.output = self.output.render().await;
						let decoder = moq_video::decode::Sink::open(config, &decode).await?;
						let track = rendition.track(hang::catalog::Reference::track(config, name))?;
						let mut subscriber = track
							.subscribe(
								moq_net::track::Subscription::default()
									.with_priority(hang::catalog::PRIORITY.video)
									.with_max_delay(max_delay),
							)
							.await?;
						// Start at the live edge without asking the shared publisher
						// subscription to rewind to a cached sequence. The edge is known only
						// once the feed is live, so wait for it rather than read the backlog of
						// a warm copy still waiting on its source; the wait ends with the track.
						if let Some(largest) = subscriber.largest().await {
							subscriber = subscriber.with_range(largest.group..);
						}
						let format = catalog::hang::Container::try_from(config)?;
						Ok::<_, anyhow::Error>((moq_mux::container::Consumer::new(subscriber, format), decoder))
					}
					.await;
					match opened {
						Ok((track, decoder)) => {
							tracing::info!(track = name, decoder = decoder.name(), "playing video rendition");
							let video = Video {
								presentation: self.presentation.clone(),
								frames: self.video.clone(),
								changed: self.drained.clone(),
								output: self.output.clone(),
								max_delay,
							};
							let (stop, stopped) = tokio::sync::oneshot::channel();
							let (retire, retired) = tokio::sync::oneshot::channel();
							video_demand = Some(Demand {
								name: name.clone(),
								track: reference.clone(),
								stop,
								retire: Some(retire),
							});
							tasks.spawn(async move {
								// Video holds no tail beyond the frames already queued for the window,
								// so a retirement stops it too.
								let stopped = async {
									tokio::select! {
										_ = stopped => {}
										Ok(()) = retired => {}
									}
								};
								let result = until_stopped(video.run(track, decoder), stopped).await;
								(Kind::Video, result.map(|(finished, _)| (finished, None)))
							});
							playback.started(Kind::Video, reference);
							break;
						}
						Err(err) => {
							tracing::warn!(track = name, %err, "cannot play video rendition");
							rejected.push(format!("video `{name}`: {err}"));
						}
					}
				}
			}

			if playback.wants(Kind::Audio) {
				playback.read(Kind::Audio);
				for (name, config) in snapshot.audio.enabled() {
					let reference = Track::of(&source, config, name);
					// A finished track has only its newest group left, which already played.
					if playback.finished(&reference) {
						continue;
					}
					let rendition = match source.resolve(config.broadcast.as_ref(), config.epoch.as_ref()).await {
						Ok(rendition) => rendition,
						Err(err) => {
							tracing::warn!(track = name, %err, "cannot resolve audio rendition");
							rejected.push(format!("audio `{name}`: {err}"));
							continue;
						}
					};
					let mut decode = moq_audio::decode::Options::new();
					decode.start = moq_audio::decode::Start::Latest;
					// Floored: the speaker holds at least AUDIO_BUFFER_MIN whatever was
					// asked for, so a smaller budget would skip a group the playhead could
					// still have reached, and would size the hole fill below to a playhead
					// that does not exist.
					decode.max_delay = self.args.max_delay().max(AUDIO_BUFFER_MIN);
					decode.delay = self.args.fixed_delay();
					// The sink and the frame-duration math below both assume f32,
					// so ask for it rather than inheriting the decoder default.
					decode.output.format = moq_audio::Format::F32;
					match moq_audio::decode::Consumer::new(&rendition, config, name, decode).await {
						Ok(consumer) => {
							tracing::info!(track = name, decoder = consumer.name(), "playing audio rendition");
							if speaker.is_none() {
								speaker = Some(device.get_or_try_init(|| self.output.speaker()).await?.clone());
							}
							let audio = AudioPlayback {
								speaker: speaker.clone().expect("opened above"),
								presentation: self.presentation.clone(),
								latency: self.args.fixed_delay().unwrap_or_default().max(AUDIO_BUFFER_MIN),
								changed: self.drained.clone(),
								output: self.output.clone(),
							};
							let (stop, stopped) = tokio::sync::oneshot::channel();
							let (retire, retired) = tokio::sync::oneshot::channel();
							audio_demand = Some(Demand {
								name: name.clone(),
								track: reference.clone(),
								stop,
								retire: Some(retire),
							});
							tasks.spawn(async move {
								// A dropped sender retires nothing.
								let retired = async {
									if retired.await.is_err() {
										std::future::pending().await
									}
								};
								(
									Kind::Audio,
									until_stopped(play_audio(consumer, audio, retired), stopped).await,
								)
							});
							playback.started(Kind::Audio, reference);
							break;
						}
						Err(err) => {
							tracing::warn!(track = name, %err, "cannot play audio rendition");
							rejected.push(format!("audio `{name}`: {err}"));
						}
					}
				}
			}

			// Renditions on offer and not one of them playable, with nothing
			// already running to fall back on.
			if tasks.is_empty() && !rejected.is_empty() {
				return Err(
					Unplayable(format!("no playable rendition in the catalog: {}", rejected.join("; "))).into(),
				);
			}
		}
	}
}

/// Drive `play` until it ends or `stopped` fires, reporting whether it ended on its
/// own. Decided by whichever branch won, not by who holds the stop handle: a
/// rendition can be disabled after its track ended but before the end is joined.
async fn until_stopped<T>(
	play: impl Future<Output = anyhow::Result<T>>,
	stopped: impl Future,
) -> anyhow::Result<(bool, Option<T>)> {
	tokio::select! {
		result = play => result.map(|left| (true, Some(left))),
		_ = stopped => Ok((false, None)),
	}
}

/// The rendition a half is playing, and what ends it. Dropping it stops the rendition.
struct Demand {
	name: String,
	track: Track,
	stop: tokio::sync::oneshot::Sender<()>,
	/// Ends the rendition as though its track finished, taken once sent.
	retire: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Demand {
	/// Retire the rendition if `config`, its entry in a newer snapshot, moved it to another
	/// track, returning whether it did. The half then picks again from that snapshot.
	fn retire_moved(&mut self, source: &moq_mux::Source, config: Option<&impl hang::catalog::Reference>) -> bool {
		let moved = config.is_some_and(|config| Track::of(source, config, &self.name) != self.track);
		let Some(retire) = self.retire.take_if(|_| moved) else {
			return false;
		};
		let _ = retire.send(());
		true
	}
}

/// Whether the catalog still lists the rendition `playing` names, disabled.
fn disabled<C>(playing: &Option<Demand>, renditions: &BTreeMap<String, C>, enabled: impl Fn(&C) -> bool) -> bool {
	playing
		.as_ref()
		.and_then(|playing| renditions.get(&playing.name))
		.is_some_and(|config| !enabled(config))
}

/// Stop the rendition `playing` if it was `disabled` and the selection let it go, because
/// another of its kind is enabled. Cancelled through the task's own result so it still reports
/// which half ended.
fn stop_released<C>(playing: &mut Option<Demand>, disabled: bool, selected: &BTreeMap<String, C>) {
	if disabled && let Some(demand) = playing.take_if(|playing| !selected.contains_key(&playing.name)) {
		let _ = demand.stop.send(());
	}
}

/// A broadcast this build cannot play, which ends the player. A broadcast that
/// ends or fails on the wire does not: the next announcement plays again.
#[derive(Debug)]
struct Unplayable(String);

impl std::fmt::Display for Unplayable {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(&self.0)
	}
}

impl std::error::Error for Unplayable {}

struct AudioPlayback<O: Output> {
	changed: Arc<tokio::sync::Notify>,
	speaker: O::Speaker,
	presentation: Arc<Mutex<Presentation>>,
	/// The depth the sink opens on, which also sizes its ring.
	latency: Duration,
	output: O,
}

/// Play a track until it ends, or until `retired` resolves, which ends it where
/// it stands. Either way it hands back the sink with the delay it still holds.
async fn play_audio<O: Output>(
	mut consumer: moq_audio::decode::Consumer,
	playback: AudioPlayback<O>,
	retired: impl Future<Output = ()>,
) -> anyhow::Result<<O::Speaker as Speaker>::Sink> {
	let AudioPlayback {
		changed,
		speaker,
		presentation,
		latency,
		output,
	} = playback;

	// The playout delay is the consumer's to size, from how unevenly packets arrive,
	// and the sink is where it lives: a sample handed over now sounds that much
	// later. That estimate times each packet when `read` hands it over, so this
	// receiver never waits on the speaker between reads. Live playback lets
	// `fit` hold the sink on the target; declared ends queue their replacements
	// until the old sink drains. The window
	// schedules video against where the speaker actually is, which keeps the two
	// together whatever the target does.
	let role = Role::new(presentation.clone(), Kind::Audio);
	let received = Mutex::new(VecDeque::new());
	let arrived = tokio::sync::Notify::new();
	let popped = tokio::sync::Notify::new();
	let max_delay = consumer.max_delay();
	let sample_rate = consumer.sample_rate();
	let layout = consumer.layout();
	let channels = layout.channels();
	let mut input = moq_audio::playback::Input::default();
	input.format = moq_audio::Format::F32;
	input.sample_rate = sample_rate;
	input.layout = layout;
	// The floor the sink opens on and pads an underflow back to, and what sizes
	// its ring, so a fixed delay has to be it or the ring could not hold it. An
	// estimated target moves, so it gets the floor, and `fit` steers the sink
	// onto the target on every write.
	input.latency = latency;
	let mut sink = speaker.sink(input.clone())?;
	let mut dry = true;

	// One sample across every channel, the unit a write has to stay aligned to.
	let stride = channels as usize * size_of::<f32>();
	let samples = |duration: Duration| (duration.as_secs_f64() * sample_rate as f64).round() as u64;
	let slack = samples(AUDIO_SLACK);
	let mut silence = Vec::new();

	// The longest hole worth playing through, in samples. A hole this player would
	// rather sit through is one it is already willing to buffer, which is what the
	// decoder's latency budget says: anything longer is what that budget chose to
	// skip, so playing it as silence would hand back the delay the skip avoided.
	// Past it the sink skips the hole and the clock re-anchors, as it does today.
	let fill_max = samples(max_delay);

	let mut timeline = AudioTimeline::default();

	// How far ahead of its due time a frame is written: the playout target less
	// the frame itself, never under the sink's floor.
	let lead = |delay: Duration, frame: &moq_audio::Frame| {
		let duration = Duration::from_secs_f64((frame.data.len() / stride) as f64 / sample_rate as f64);
		latency.max(delay.min(max_delay).max(AUDIO_BUFFER_MIN).saturating_sub(duration))
	};

	// Tracks whether the last read failed, so a stream the decoder can't read at
	// all logs once rather than once per packet.
	let mut dropping = false;

	// Resolved in delivery order, ahead of playback, so each end's transition is
	// declared before the replacement media that resumes it is planned. A short
	// timeline can declare its end before its first PCM, so ends queue until a
	// frame crosses them.
	let receive = async {
		let mut retired = std::pin::pin!(retired);
		// Media went missing since the last frame, so the next one does not continue it.
		let mut lost = false;
		let mut ends = VecDeque::new();
		loop {
			// Stop reading while play is held behind a resume with more than the age
			// budget queued, so the subscription evicts the excess instead of this
			// queue growing without bound.
			while received
				.lock()
				.unwrap()
				.front()
				.is_some_and(|(.., queued): &(_, _, _, Instant)| queued.elapsed() > max_delay)
			{
				popped.notified().await;
			}
			let event = tokio::select! {
				event = consumer.read() => event,
				() = &mut retired => Ok(None),
			};
			let event = match event {
				Err(err) if !matches!(err, moq_audio::Error::Decode(_)) => return Err(err.into()),
				event => event,
			};
			let ended = matches!(event, Ok(None));
			let delay = consumer.delay();
			let crossing = match &event {
				Ok(Some(moq_audio::decode::Read::End { end })) => {
					ends.push_back((Duration::from(*end), role.declare()));
					None
				}
				// Playback restarts at the frame after it, so that is what carries it.
				Ok(Some(moq_audio::decode::Read::Gap { .. })) => {
					lost = true;
					continue;
				}
				Ok(Some(moq_audio::decode::Read::Frame(frame))) => {
					let lost = std::mem::take(&mut lost);
					// A frame at or past an end comes from the encoder that replaced it; one
					// before it is the stopped encoder's drain. Only the last end crossed has
					// replacement media; the ones before it ended empty timelines.
					let mut crossed = None;
					while let Some(&(end, transition)) = ends.front()
						&& Duration::from(frame.timestamp) >= end
					{
						ends.pop_front();
						if let Some(skipped) = crossed.replace(transition) {
							role.abandon(skipped);
						}
					}
					match crossed {
						Some(transition) => {
							role.plan(transition, frame.timestamp, Some(lead(delay, frame)));
							Some(Crossing::Resume(transition))
						}
						None if lost => Some(Crossing::Loss),
						None => None,
					}
				}
				_ => None,
			};
			received
				.lock()
				.unwrap()
				.push_back((event, delay, crossing, Instant::now()));
			arrived.notify_one();
			if ended {
				return Ok::<_, anyhow::Error>(());
			}
		}
	};
	let play = async {
		// Whether a timeline is playing and its end is already declared, and an end
		// declared past that one, for a later timeline that can only be mapped once it
		// resumes.
		let mut playing = false;
		let mut ended = false;
		let mut deferred = None;
		let mut sequential = false;
		loop {
			let (event, delay, crossing, _) = loop {
				if let Some(event) = received.lock().unwrap().pop_front() {
					popped.notify_one();
					break event;
				}
				arrived.notified().await;
			};
			let frame = match event {
				Ok(Some(moq_audio::decode::Read::Frame(frame))) => {
					playing = true;
					match crossing {
						Some(Crossing::Loss) => {
							deferred = None;
							ended = false;
							role.gap();
							sequential = false;
						}
						Some(Crossing::Resume(transition)) => {
							// A declared boundary preserves the old sink, unlike an undeclared hole.
							drain(sink).await;
							let first = lead(delay, &frame);
							let mapping = role.resume(transition, frame.timestamp, Some(first)).await?;
							role.apply(transition, mapping);
							let at = presentation
								.lock()
								.unwrap()
								.due(role.map(frame.timestamp)?)
								.and_then(|at| at.checked_sub(first));
							if let Some(at) = at {
								moq_clock::sleep_until(at).await;
							}
							ended = false;
							// An end this frame did not cross ends the timeline it resumes.
							if let Some(end) = deferred.take()
								&& Duration::from(frame.timestamp) < Duration::from(end)
							{
								role.end(end)?;
								ended = true;
							}
							sequential = true;
							timeline = AudioTimeline::default();
							sink = speaker.sink(input.clone())?;
							dry = true;
						}
						None => {}
					}
					frame
				}
				Ok(Some(moq_audio::decode::Read::End { end })) => {
					if playing && ended {
						deferred = Some(end);
					} else {
						role.end(end)?;
						ended = true;
					}
					continue;
				}
				// Never queued: receive folds it into the next frame's crossing.
				Ok(Some(moq_audio::decode::Read::Gap { .. })) => continue,
				Ok(None) => break,
				// One bad packet is that packet's problem: the decoder stays usable, so
				// skip it rather than ending playback and taking the video window down
				// with it.
				Err(err @ moq_audio::Error::Decode(_)) => {
					if dropping {
						tracing::debug!(%err, "dropping an audio frame");
					} else {
						tracing::warn!(%err, "dropping an audio frame");
						dropping = true;
					}
					continue;
				}
				Err(err) => return Err(err.into()),
			};
			dropping = false;

			let length = frame.data.len() / stride;
			let mapped = role.map(frame.timestamp)?;
			let start = timestamp(mapped);
			let timing = timeline.push(start, length, sample_rate, fill_max);

			// A rewind or a hole too large to fill starts a new playback sink. The old
			// sink has no media clock, so its buffered audio cannot be carried across a
			// timeline region the player skipped.
			if timing.reset_sink {
				sequential = false;
				drop(sink);
				presentation.lock().unwrap().restarted();
				sink = speaker.sink(input.clone())?;
				dry = true;
			}

			// Resumed media was received while the old tail played. Keep its
			// spacing instead of fitting that queued burst as if it arrived live.
			// The lead follows the current target so jitter can still deepen the sink.
			if sequential {
				let at = presentation
					.lock()
					.unwrap()
					.due(mapped)
					.and_then(|at| at.checked_sub(lead(delay, &frame)));
				if let Some(at) = at {
					moq_clock::sleep_until(at).await;
				}
			}

			// A hole in the media is a hole in the audio, not a splice. Handing the next
			// frame straight to the speaker shortens the track by the missing duration,
			// which leaves it running ahead of media time until the clock below
			// re-anchors, taking the video with it. So the hole goes in as silence ahead
			// of the frame, and the pair is fitted to the target as one write.
			let buffered = samples(sink.buffered());
			dry |= buffered == 0;
			// Capped at the age budget: audio older than it is skipped rather than held,
			// so a deeper target could never fill. The advertised floor needs the cap,
			// being a number the publisher declared about itself, unbounded. The budget
			// is also what the sink's ring was sized to hold.
			let held = delay.min(max_delay).max(AUDIO_BUFFER_MIN);
			let target = samples(held);
			let fit = fit(dry, buffered, target, slack, timing.silence, length as u64);
			dry = false;

			// The packet pays for the channel count; the catalog cannot allocate
			// a duration's worth of silence before any samples arrive.
			silence.resize(length.min(AUDIO_SILENCE) * stride, 0);
			let mut quiet = fit.pad + timing.silence.saturating_sub(fit.skip);
			while quiet > 0 && !silence.is_empty() {
				let frames = quiet.min((silence.len() / stride) as u64) as usize;
				let written = sink.write(&silence[..frames * stride])?;
				// Once full, padding is dropped just like live media. A catalog's
				// rate must not make us walk seconds of refused input samples.
				if written.dropped_sample_frames != 0 {
					break;
				}
				quiet -= frames as u64;
			}
			let skip = fit.skip.saturating_sub(timing.silence) as usize * stride;
			let _ = sink.write(&frame.data[skip.min(frame.data.len())..])?;

			// Anchor the playout clock on where the speaker has actually reached, which
			// is the only half of the pipeline that cannot skip ahead. A move has to
			// wake the window: it is asleep on a deadline computed from the old anchor,
			// and every queued frame is due earlier now.
			let moved = {
				let mut presentation = presentation.lock().unwrap();
				presentation.hold(held);
				presentation.audio(timing.end, sink.buffered(), Instant::now())
			};
			if moved {
				changed.notify_one();
				output.send(Event::Wake);
			}
		}

		Ok::<_, anyhow::Error>(sink)
	};
	let (_, sink) = tokio::try_join!(receive, play)?;
	role.retire(Instant::now().checked_add(sink.buffered()));
	Ok(sink)
}

/// Play out what a retired sink still holds, instead of cutting the tail off
/// by dropping it.
async fn drain(sink: impl Sink) {
	// The estimated target moves, so the depth the sink was opened with says
	// nothing about what it holds now: read it at retirement instead.
	let _ = moq_clock::timeout(sink.buffered() + AUDIO_DRAIN_GRACE, sink.finish()).await;
}

#[cfg(test)]
#[expect(clippy::disallowed_methods, reason = "tests read the host clock directly")]
mod tests {
	use bytes::Bytes;
	use hang::catalog::{AudioCodec, AudioConfig};
	use moq_mux::catalog::hang::Container;

	use super::*;
	use crate::play::args::Delay;
	use crate::play::fake::Recorder;

	const SAMPLE_RATE: u32 = 48_000;
	/// Samples per packet.
	const PACKET: u64 = 960;
	const PACKET_DURATION: Duration = Duration::from_millis(20);

	#[tokio::test(start_paused = true)]
	async fn an_unfed_catalog_needs_no_silence_buffer() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let _track = broadcast
			.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		let consumer = moq_audio::decode::Consumer::new(
			&broadcast.consume(),
			&AudioConfig::new(AudioCodec::Pcm, 4_000_000_000, 8),
			"audio",
			moq_audio::decode::Options::new(),
		)
		.await
		.unwrap();
		let recorder = Recorder::default();
		let playback = AudioPlayback {
			changed: Default::default(),
			speaker: recorder.clone(),
			presentation: Arc::new(Mutex::new(Presentation::new(AUDIO_BUFFER_MIN))),
			latency: AUDIO_BUFFER_MIN,
			output: recorder,
		};
		let _sink = play_audio(consumer, playback, std::future::ready(())).await.unwrap();
	}

	#[tokio::test]
	async fn a_quick_audio_resume_preserves_the_old_speaker_tail() {
		tokio::time::pause();
		let delay = Duration::from_millis(200);
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		let mut producer = moq_mux::container::Producer::new(track, Container::Legacy(moq_mux::container::Kind::Audio));
		let mut options = moq_audio::decode::Options::new();
		options.delay = Some(delay);
		options.max_delay = delay;
		options.output.format = moq_audio::Format::F32;
		let consumer = moq_audio::decode::Consumer::new(
			&broadcast.consume(),
			&AudioConfig::new(AudioCodec::Pcm, SAMPLE_RATE, 1),
			"audio",
			options,
		)
		.await
		.unwrap();
		let recorder = Recorder::default();
		let playback = AudioPlayback {
			changed: Default::default(),
			speaker: recorder.clone(),
			presentation: Arc::new(Mutex::new(Presentation::new(delay))),
			latency: delay,
			output: recorder.clone(),
		};
		let task = tokio::spawn(async move {
			play_audio(consumer, playback, std::future::pending())
				.await
				.unwrap()
				.finish()
				.await;
		});
		producer.write(packet(0, 0.25)).unwrap();
		tokio::task::yield_now().await;
		producer.discontinuity().unwrap();
		producer.write(packet(0, 0.5)).unwrap();
		producer.finish().unwrap();
		task.await.unwrap();
		let played = recorder.played();
		let old: Vec<_> = played.iter().filter(|part| part.sample == 0.25).collect();
		let new: Vec<_> = played.iter().filter(|part| part.sample == 0.5).collect();
		assert_eq!(
			old.iter().map(|part| part.to - part.from).sum::<Duration>(),
			PACKET_DURATION
		);
		assert_eq!(
			new.iter().map(|part| part.to - part.from).sum::<Duration>(),
			PACKET_DURATION
		);
		assert!(old.last().unwrap().to <= new.first().unwrap().from);
	}

	#[tokio::test]
	async fn paired_quick_resumes_keep_audio_and_reordered_video_together() {
		quick_resumes(true, true, Duration::from_millis(200)).await;
	}

	#[tokio::test]
	async fn an_audio_resume_aligns_with_continuous_video() {
		quick_resumes(true, false, Duration::from_millis(200)).await;
	}

	#[tokio::test]
	async fn a_video_resume_aligns_with_continuous_audio() {
		quick_resumes(false, true, Duration::from_millis(200)).await;
	}

	#[tokio::test]
	async fn paired_resumes_follow_the_speaker_when_delays_differ() {
		quick_resumes(true, true, Duration::from_millis(80)).await;
	}

	async fn quick_resumes(audio_end: bool, video_end: bool, audio_latency: Duration) {
		struct Buffered(Vec<moq_video::Frame>);
		impl super::super::video::Decoder for Buffered {
			async fn decode(&mut self, frame: moq_mux::container::Frame) -> anyhow::Result<Vec<moq_video::Frame>> {
				self.0.push(moq_video::Frame::new(
					moq_video::Surface::rgba(&[128; 16 * 16 * 4], moq_video::Size::new(16, 16))?,
					frame.timestamp,
				));
				self.0.sort_by_key(|frame| frame.timestamp);
				let ready = self.0.len().saturating_sub(2);
				Ok(self.0.drain(..ready).collect())
			}
			async fn flush(&mut self) -> anyhow::Result<Vec<moq_video::Frame>> {
				Ok(std::mem::take(&mut self.0))
			}
		}
		tokio::time::pause();
		let playback_start = Instant::now();
		let delay = Duration::from_millis(200);
		let recorder = Recorder::default();
		let origin = moq_tokio::origin::spawn();
		let media = media(&origin, delay, recorder.clone());
		let broadcast = moq_net::broadcast::Info::new().produce();
		let audio = broadcast
			.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		let video = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let subscriber = video
			.consume()
			.subscribe(moq_net::track::Subscription::default().with_max_delay(delay))
			.await
			.unwrap();
		let mut audio = moq_mux::container::Producer::new(audio, Container::Legacy(moq_mux::container::Kind::Audio));
		let mut video = moq_mux::container::Producer::new(video, Container::Legacy(moq_mux::container::Kind::Video));
		let mut options = moq_audio::decode::Options::new();
		options.delay = Some(delay);
		options.max_delay = delay;
		options.output.format = moq_audio::Format::F32;
		let consumer = moq_audio::decode::Consumer::new(
			&broadcast.consume(),
			&AudioConfig::new(AudioCodec::Pcm, SAMPLE_RATE, 1),
			"audio",
			options,
		)
		.await
		.unwrap();
		let audio_play = AudioPlayback {
			changed: media.drained.clone(),
			speaker: recorder.clone(),
			presentation: media.presentation.clone(),
			latency: audio_latency,
			output: recorder.clone(),
		};
		let video_play = Video {
			presentation: media.presentation.clone(),
			frames: media.video.clone(),
			changed: media.drained.clone(),
			output: recorder.clone(),
			max_delay: delay,
		};
		let audio_task = tokio::spawn(async move {
			play_audio(consumer, audio_play, std::future::pending())
				.await
				.unwrap()
				.finish()
				.await;
		});
		let video_task = tokio::spawn(video_play.run(
			moq_mux::container::Consumer::new(subscriber, Container::Legacy(moq_mux::container::Kind::Video)),
			Buffered(Vec::new()),
		));
		let picture = |index: u64, keyframe: bool| moq_mux::container::Frame {
			timestamp: moq_net::Timestamp::from_millis(index * 20).unwrap(),
			duration: Some(moq_net::Timestamp::from_millis(20).unwrap()),
			keyframe,
			payload: Bytes::from_static(b"picture"),
		};
		for index in 0..3 {
			let mut pcm = packet(index, 0.25);
			pcm.keyframe = index == 0;
			audio.write(pcm).unwrap();
			video.write(picture(index, index == 0)).unwrap();
			tokio::time::sleep(PACKET_DURATION).await;
		}
		if audio_end {
			audio.discontinuity().unwrap();
		}
		if video_end {
			video.discontinuity().unwrap();
		}
		for index in 0..3 {
			if audio_end || index == 0 {
				let mut pcm = packet(index + 3, 0.5);
				pcm.keyframe = index == 0;
				audio.write(pcm).unwrap();
			}
			if video_end || index == 0 {
				video.write(picture(index + 3, video_end && index == 0)).unwrap();
			}
		}
		if audio_end {
			audio.finish().unwrap();
		}
		if video_end {
			video.finish().unwrap();
		}
		let mut shown = Vec::new();
		for step in 0..800 {
			if step >= 20 && step % 20 == 0 {
				let index = 3 + step / 20;
				if !audio_end {
					audio.write(packet(index, 0.5)).unwrap();
				}
				if !video_end {
					video.write(picture(index, false)).unwrap();
				}
			}
			tokio::task::yield_now().await;
			if let Some(timestamp) = recorder.present(&media) {
				shown.push((timestamp, Instant::now()));
			}
			tokio::time::advance(Duration::from_millis(1)).await;
		}
		if !audio_end {
			audio.finish().unwrap();
		}
		if !video_end {
			video.finish().unwrap();
		}
		audio_task.await.unwrap();
		video_task.await.unwrap().unwrap();
		if video_end {
			assert_eq!(shown.len(), 6, "both decoder tails must be shown");
		}
		assert_eq!(
			shown[..3]
				.iter()
				.map(|(timestamp, _)| timestamp.as_millis())
				.collect::<Vec<_>>(),
			[0, 20, 40]
		);
		let played = recorder.played();
		let old: Vec<_> = played.iter().filter(|part| part.sample == 0.25).collect();
		let new: Vec<_> = played.iter().filter(|part| part.sample == 0.5).collect();
		assert_eq!(
			old.iter().map(|part| part.to - part.from).sum::<Duration>(),
			PACKET_DURATION * 3
		);
		if audio_end {
			assert_eq!(
				new.iter().map(|part| part.to - part.from).sum::<Duration>(),
				PACKET_DURATION * 3
			);
		}
		assert!(old.last().unwrap().to <= new[0].from);
		let (picture, sample) = if audio_end && video_end {
			(shown[3].1, new[0].from)
		} else if audio_end {
			let timestamp = new[0].from.duration_since(playback_start + delay);
			let shown = shown
				.iter()
				.find(|(shown, _)| {
					let start = super::timestamp(*shown);
					start <= timestamp && timestamp < start + PACKET_DURATION
				})
				.expect("continuous video reaches resumed audio");
			(shown.1 + (timestamp - super::timestamp(shown.0)), new[0].from)
		} else {
			let timestamp = super::timestamp(shown[3].0);
			let index = (shown[3].0.as_millis() / 20) as usize;
			let offset = timestamp - PACKET_DURATION * index as u32;
			(shown[3].1, new[index - 3].from + offset)
		};
		assert!(
			picture.saturating_duration_since(sample) <= Duration::from_millis(1)
				&& sample.saturating_duration_since(picture) <= Duration::from_millis(1),
			"new A/V clocks diverged: picture={picture:?} sample={sample:?}"
		);
	}

	#[tokio::test]
	async fn an_end_before_its_first_pcm_still_resumes_that_timeline() {
		tokio::time::pause();
		let delay = Duration::from_millis(200);
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		let mut producer = moq_mux::container::Producer::new(track, Container::Legacy(moq_mux::container::Kind::Audio));
		let mut options = moq_audio::decode::Options::new();
		options.delay = Some(delay);
		options.max_delay = delay;
		options.output.format = moq_audio::Format::F32;
		options.output.sample_rate = Some(SAMPLE_RATE);
		let consumer = moq_audio::decode::Consumer::new(
			&broadcast.consume(),
			&AudioConfig::new(AudioCodec::Pcm, 44_100, 1),
			"audio",
			options,
		)
		.await
		.unwrap();
		let recorder = Recorder::default();
		let playback = AudioPlayback {
			changed: Default::default(),
			speaker: recorder.clone(),
			presentation: Arc::new(Mutex::new(Presentation::new(delay))),
			latency: delay,
			output: recorder.clone(),
		};
		let task = tokio::spawn(async move {
			play_audio(consumer, playback, std::future::pending())
				.await
				.unwrap()
				.finish()
				.await;
		});
		let pcm = |micros: u64, samples: usize, sample: f32| moq_mux::container::Frame {
			timestamp: moq_net::Timestamp::from_micros(micros).unwrap(),
			duration: Some(moq_net::Timestamp::from_scale(samples as u64, 44_100).unwrap()),
			keyframe: true,
			payload: std::iter::repeat_n(sample.to_le_bytes(), samples)
				.flatten()
				.collect::<Vec<_>>()
				.into(),
		};
		for index in 0..4 {
			producer.write(pcm(index * 23_220, 1024, 0.25)).unwrap();
		}
		producer.discontinuity().unwrap();
		// Too short for the resampler to emit anything before this encoder ends too.
		producer.write(pcm(100_000, 64, 0.5)).unwrap();
		producer.discontinuity().unwrap();
		for index in 0..4 {
			producer.write(pcm(200_000 + index * 23_220, 1024, 0.75)).unwrap();
		}
		producer.finish().unwrap();
		task.await.unwrap();
		let played = recorder.played();
		let sinks = |range: std::ops::Range<f32>| {
			played
				.iter()
				.filter(|part| range.contains(&part.sample))
				.map(|part| part.sink)
				.collect::<std::collections::BTreeSet<_>>()
		};
		let (old, short, new) = (sinks(0.2..0.3), sinks(0.4..0.6), sinks(0.7..0.8));
		assert!(!short.is_empty(), "the short timeline played: {played:?}");
		assert!(
			old.is_disjoint(&short),
			"the short timeline resumed on its own sink: {played:?}"
		);
		assert!(
			short.is_disjoint(&new),
			"the next timeline resumed on its own sink: {played:?}"
		);
	}

	#[tokio::test]
	async fn joining_a_paused_track_resumes_on_its_first_frame() {
		tokio::time::pause();
		let delay = Duration::from_millis(200);
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		let mut producer = moq_mux::container::Producer::new(track, Container::Legacy(moq_mux::container::Kind::Audio));
		for index in 0..3 {
			producer.write(packet(index, 0.25)).unwrap();
		}
		producer.discontinuity().unwrap();
		// Joins at the end marker: the stopped encoder's media is already behind it.
		let mut options = moq_audio::decode::Options::new();
		options.delay = Some(delay);
		options.max_delay = delay;
		options.start = moq_audio::decode::Start::Latest;
		options.output.format = moq_audio::Format::F32;
		let consumer = moq_audio::decode::Consumer::new(
			&broadcast.consume(),
			&AudioConfig::new(AudioCodec::Pcm, SAMPLE_RATE, 1),
			"audio",
			options,
		)
		.await
		.unwrap();
		let recorder = Recorder::default();
		let playback = AudioPlayback {
			changed: Default::default(),
			speaker: recorder.clone(),
			presentation: Arc::new(Mutex::new(Presentation::new(delay))),
			latency: delay,
			output: recorder.clone(),
		};
		let task = tokio::spawn(async move {
			play_audio(consumer, playback, std::future::pending())
				.await
				.unwrap()
				.finish()
				.await;
		});
		tokio::task::yield_now().await;
		for index in 3..6 {
			producer.write(packet(index, 0.5)).unwrap();
		}
		producer.finish().unwrap();
		task.await.unwrap();
		let played = recorder.played();
		let resumed: Vec<_> = played.iter().filter(|part| part.sample == 0.5).collect();
		assert!(!resumed.is_empty(), "{played:?}");
		assert!(
			resumed.iter().all(|part| part.sink > 0),
			"the replacement resumed its declared transition: {played:?}"
		);
	}

	#[tokio::test]
	async fn a_held_audio_resume_leaves_the_excess_to_the_subscription() {
		tokio::time::pause();
		let delay = Duration::from_millis(200);
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("audio", hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		let mut producer = moq_mux::container::Producer::new(track, Container::Legacy(moq_mux::container::Kind::Audio));
		let mut options = moq_audio::decode::Options::new();
		options.delay = Some(delay);
		options.max_delay = delay;
		options.output.format = moq_audio::Format::F32;
		let consumer = moq_audio::decode::Consumer::new(
			&broadcast.consume(),
			&AudioConfig::new(AudioCodec::Pcm, SAMPLE_RATE, 1),
			"audio",
			options,
		)
		.await
		.unwrap();
		let recorder = Recorder::default();
		let presentation = Arc::new(Mutex::new(Presentation::new(delay)));
		let changed = Arc::new(tokio::sync::Notify::new());
		let playback = AudioPlayback {
			changed: changed.clone(),
			speaker: recorder.clone(),
			presentation: presentation.clone(),
			latency: delay,
			output: recorder.clone(),
		};
		let task = tokio::spawn(async move {
			play_audio(consumer, playback, std::future::pending())
				.await
				.unwrap()
				.finish()
				.await;
		});
		for index in 0..5 {
			producer.write(packet(index, 0.25)).unwrap();
			tokio::time::sleep(PACKET_DURATION).await;
		}
		// Video declares an end two seconds out, holding the audio replacement back.
		let video = Role::new(presentation.clone(), Kind::Video);
		video.declare();
		video.end(moq_net::Timestamp::from_millis(2_000).unwrap()).unwrap();
		producer.discontinuity().unwrap();
		const REPLACEMENT: u64 = 100;
		for index in 0..REPLACEMENT {
			producer.write(packet(5 + index, 0.5)).unwrap();
			tokio::time::sleep(PACKET_DURATION).await;
		}
		producer.finish().unwrap();
		task.await.unwrap();
		drop(video);
		let (resumed, _) = played(&recorder, 0.5);
		assert!(!resumed.is_zero(), "the replacement played");
		assert!(
			resumed < PACKET_DURATION * REPLACEMENT as u32 / 2,
			"media held behind the resume past the age budget was queued, not evicted: {resumed:?}"
		);
	}

	#[tokio::test]
	async fn an_evicted_replacement_gop_gives_way_to_the_next_replacement() {
		struct Passthrough;
		impl super::super::video::Decoder for Passthrough {
			async fn decode(&mut self, frame: moq_mux::container::Frame) -> anyhow::Result<Vec<moq_video::Frame>> {
				Ok(vec![moq_video::Frame::new(
					moq_video::Surface::rgba(&[128; 16 * 16 * 4], moq_video::Size::new(16, 16))?,
					frame.timestamp,
				)])
			}
			async fn flush(&mut self) -> anyhow::Result<Vec<moq_video::Frame>> {
				Ok(Vec::new())
			}
		}
		tokio::time::pause();
		let delay = Duration::from_millis(200);
		let recorder = Recorder::default();
		let origin = moq_tokio::origin::spawn();
		let media = media(&origin, delay, recorder.clone());
		let broadcast = moq_net::broadcast::Info::new().produce();
		let video = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		// The network keeps every group; only the player's age budget evicts.
		let subscriber = video
			.consume()
			.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(10)))
			.await
			.unwrap();
		let mut video = moq_mux::container::Producer::new(video, Container::Legacy(moq_mux::container::Kind::Video));
		let video_play = Video {
			presentation: media.presentation.clone(),
			frames: media.video.clone(),
			changed: media.drained.clone(),
			output: recorder.clone(),
			max_delay: delay,
		};
		let video_task = tokio::spawn(video_play.run(
			moq_mux::container::Consumer::new(subscriber, Container::Legacy(moq_mux::container::Kind::Video)),
			Passthrough,
		));
		let picture = |millis: u64, keyframe: bool| moq_mux::container::Frame {
			timestamp: moq_net::Timestamp::from_millis(millis).unwrap(),
			duration: Some(moq_net::Timestamp::from_millis(20).unwrap()),
			keyframe,
			payload: Bytes::from_static(b"picture"),
		};
		for index in 0..3 {
			video.write(picture(index * 20, index == 0)).unwrap();
			tokio::time::sleep(PACKET_DURATION).await;
		}
		video.discontinuity().unwrap();
		// A burst longer than the age budget evicts this replacement's only keyframe.
		for index in 0..15 {
			video.write(picture(1_000 + index * 20, index == 0)).unwrap();
		}
		video.discontinuity().unwrap();
		for index in 0..3 {
			video.write(picture(5_000 + index * 20, index == 0)).unwrap();
		}
		video.finish().unwrap();
		let mut shown = Vec::new();
		for _ in 0..2_000 {
			tokio::task::yield_now().await;
			if let Some(timestamp) = recorder.present(&media) {
				shown.push(timestamp.as_millis());
			}
			tokio::time::advance(Duration::from_millis(1)).await;
		}
		video_task.await.unwrap().unwrap();
		assert_eq!(shown[..3], [0, 20, 40], "{shown:?}");
		assert_eq!(shown.len(), 6, "the next replacement plays: {shown:?}");
	}

	/// A mono PCM rendition, published and named in the catalog until dropped.
	fn rendition(
		broadcast: &moq_net::broadcast::Producer,
		catalog: &catalog::Producer,
		name: &str,
	) -> moq_mux::container::Producer<Container, AudioConfig> {
		let track = broadcast
			.create_track(name, hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		catalog
			.audio(
				track,
				Container::Legacy(moq_mux::container::Kind::Audio),
				AudioConfig::new(AudioCodec::Pcm, SAMPLE_RATE, 1),
			)
			.unwrap()
	}

	/// The `index`th packet of the broadcast, every sample set to `sample` so the
	/// recorder can tell which rendition played it.
	fn packet(index: u64, sample: f32) -> moq_mux::container::Frame {
		let payload: Vec<u8> = std::iter::repeat_n(sample.to_le_bytes(), PACKET as usize)
			.flatten()
			.collect();
		moq_mux::container::Frame {
			timestamp: moq_net::Timestamp::from_scale(index * PACKET, SAMPLE_RATE as u64).unwrap(),
			duration: None,
			payload: Bytes::from(payload),
			keyframe: true,
		}
	}

	fn media(origin: &moq_net::origin::Producer, delay: Duration, output: Recorder) -> Media<Recorder> {
		Media {
			origin: origin.consume(),
			broadcast: "room".to_string(),
			args: Args {
				catalog_format: None,
				metrics: None,
				delay: Delay::Fixed(delay),
				select: Default::default(),
			},
			video: Default::default(),
			presentation: Arc::new(Mutex::new(Presentation::new(delay))),
			drained: Default::default(),
			output,
		}
	}

	#[tokio::test]
	async fn disabling_a_live_high_rendition_plays_the_lower_one() {
		tokio::time::pause();
		let origin = moq_tokio::origin::spawn();
		let broadcast = origin.create_broadcast("room").unwrap();
		broadcast.announce(Default::default()).unwrap();
		let mut catalog = catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default()).unwrap();
		let mut high = rendition(&broadcast, &catalog, "high");
		let recorder = Recorder::default();
		let player = tokio::spawn(media(&origin, Duration::from_millis(50), recorder.clone()).run());
		write(&mut high, 10, 0.25).await;
		let mut low = rendition(&broadcast, &catalog, "low");
		catalog
			.mutate(|catalog| {
				let high = catalog.audio.renditions.get_mut("high").unwrap();
				high.bitrate = Some(u64::from(SAMPLE_RATE) * 32);
				high.enabled = false;
				// An unknown bitrate ranks below a known one without falsifying PCM metadata.
				catalog.audio.renditions.get_mut("low").unwrap().bitrate = None;
			})
			.unwrap();
		for index in 10..40 {
			high.write(packet(index, 0.25)).unwrap();
			low.write(packet(index, 0.5)).unwrap();
			tokio::time::sleep(PACKET_DURATION).await;
		}
		low.finish().unwrap();
		catalog.finish().unwrap();
		// The high rendition remains open: only the disabled catalog can stop it.
		settle(player, &recorder).await;
		assert!(recorder.played().iter().any(|part| part.sample == 0.25));
		assert!(
			recorder.played().iter().any(|part| part.sample == 0.5),
			"the live disabled track prevented fallback"
		);
	}

	/// Muting the only audio rendition holds it, so the unmute plays on the subscription it
	/// already had rather than a new one.
	#[tokio::test]
	async fn a_muted_rendition_is_held_through_the_unmute() {
		tokio::time::pause();
		let origin = moq_tokio::origin::spawn();
		let broadcast = origin.create_broadcast("room").unwrap();
		broadcast.announce(Default::default()).unwrap();
		let mut catalog = catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default()).unwrap();
		let mut audio = rendition(&broadcast, &catalog, "audio");
		let recorder = Recorder::default();
		let player = tokio::spawn(media(&origin, Duration::from_millis(50), recorder.clone()).run());
		let mut enable = |enabled: bool| {
			catalog
				.mutate(|catalog| catalog.audio.renditions.get_mut("audio").unwrap().enabled = enabled)
				.unwrap()
		};

		write(&mut audio, 10, 0.25).await;
		enable(false);
		// Half a second muted, with the timeline running on through it.
		let demand = audio.demand();
		for _ in 0..25 {
			tokio::time::sleep(PACKET_DURATION).await;
			assert!(demand.wanted().is_some(), "the mute unsubscribed");
		}
		enable(true);
		for index in 35..60 {
			audio.write(packet(index, 0.5)).unwrap();
			tokio::time::sleep(PACKET_DURATION).await;
		}

		settle(player, &recorder).await;
		assert!(played(&recorder, 0.25).0 > Duration::ZERO);
		assert!(played(&recorder, 0.5).0 > Duration::ZERO, "the unmute did not play");
	}

	/// A publisher retires an audio rendition by naming its replacement and then
	/// finishing the old track. The retired sink still holds a delay of audio, and
	/// the replacement's sink holds its own before its first sample sounds, so
	/// played one after the other the switch costs a delay of silence (#3966).
	#[tokio::test]
	async fn an_audio_rendition_switch_leaves_no_gap() {
		tokio::time::pause();

		const OLD: f32 = 0.25;
		const NEW: f32 = 0.5;
		let delay = Duration::from_millis(500);

		let origin = moq_tokio::origin::spawn();
		let broadcast = origin.create_broadcast("room").unwrap();
		broadcast.announce(Default::default()).unwrap();
		let mut catalog = catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default()).unwrap();

		let recorder = Recorder::default();
		let player = tokio::spawn(media(&origin, delay, recorder.clone()).run());

		// A second of the old rendition, published in real time.
		// Paced against absolute deadlines: tokio rounds each sleep up to the next
		// millisecond, which relative sleeps would accumulate into a publisher
		// falling behind the speaker.
		let mut old = rendition(&broadcast, &catalog, "old");
		let start = Instant::now();
		let mut index = 0;
		while index < 50 {
			old.write(packet(index, OLD)).unwrap();
			index += 1;
			moq_clock::sleep_until(start + PACKET_DURATION * index as u32).await;
		}

		// The replacement joins the catalog, then the old track finishes.
		let mut new = rendition(&broadcast, &catalog, "new");
		new.write(packet(index, NEW)).unwrap();
		index += 1;
		old.finish().unwrap();
		drop(old);

		while index < 100 {
			moq_clock::sleep_until(start + PACKET_DURATION * index as u32).await;
			new.write(packet(index, NEW)).unwrap();
			index += 1;
		}
		new.finish().unwrap();
		drop(new);
		catalog.finish().unwrap();

		settle(player, &recorder).await;

		let played = recorder.played();
		let old_end = played.iter().filter(|p| p.sample == OLD).map(|p| p.to).max().unwrap();
		let new_start = played.iter().filter(|p| p.sample == NEW).map(|p| p.from).min().unwrap();
		// Tokio rounds the replacement's pacing sleep to the next millisecond.
		// The exact sample count below rules out any truncation within that tick.
		let gap = new_start.saturating_duration_since(old_end);
		assert!(gap <= Duration::from_millis(1), "the switch went silent for {gap:?}");
		let old_duration: Duration = played.iter().filter(|p| p.sample == OLD).map(|p| p.to - p.from).sum();
		assert_eq!(old_duration, Duration::from_secs(1));
	}

	/// A catalog moves the rendition to another track while the old one still
	/// sends, then back. Each move plays out the retired sink's tail while the new
	/// track fills, as a finished track's does, and nothing the track it left sent
	/// after the move plays. A track left this way did not finish, so moving back
	/// plays it again.
	#[tokio::test]
	async fn a_moved_audio_rendition_plays_its_new_track() {
		tokio::time::pause();

		const OLD: f32 = 0.25;
		const LEFT: f32 = 0.125;
		const NEW: f32 = 0.5;
		const BACK: f32 = 0.75;
		let delay = Duration::from_millis(500);

		let origin = moq_tokio::origin::spawn();
		let broadcast = origin.create_broadcast("room").unwrap();
		broadcast.announce(Default::default()).unwrap();
		let mut catalog = catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default()).unwrap();
		let track = |name: &str| {
			let track = broadcast
				.create_track(name, hang::container::track_info(hang::catalog::PRIORITY.audio))
				.unwrap();
			moq_mux::container::Producer::new(track, Container::Legacy(moq_mux::container::Kind::Audio))
		};
		let (mut a, mut b) = (track("a"), track("b"));
		let point = |catalog: &mut catalog::Producer, track: &str| {
			let mut config = AudioConfig::new(AudioCodec::Pcm, SAMPLE_RATE, 1);
			config.container = hang::catalog::Container::Legacy;
			config.track = Some(track.to_string());
			catalog
				.mutate(|catalog| {
					catalog.audio.renditions.insert("main".to_string(), config);
				})
				.unwrap();
		};
		point(&mut catalog, "a");

		let recorder = Recorder::default();
		let player = tokio::spawn(media(&origin, delay, recorder.clone()).run());

		// Paced against absolute deadlines, as in the switch test above. Each move
		// lands with only the new track's packet out, so the player takes it before
		// the track it left sends again.
		let start = Instant::now();
		let mut index = 0;
		while index < 150 {
			match index {
				0..50 => a.write(packet(index, OLD)).unwrap(),
				50 => {
					b.write(packet(index, NEW)).unwrap();
					point(&mut catalog, "b");
				}
				51..100 => {
					a.write(packet(index, LEFT)).unwrap();
					b.write(packet(index, NEW)).unwrap();
				}
				100 => {
					a.write(packet(index, BACK)).unwrap();
					point(&mut catalog, "a");
				}
				_ => a.write(packet(index, BACK)).unwrap(),
			}
			index += 1;
			moq_clock::sleep_until(start + PACKET_DURATION * index as u32).await;
		}

		settle(player, &recorder).await;

		let played = recorder.played();
		assert!(
			!played.iter().any(|p| p.sample == LEFT),
			"the track it left kept playing"
		);
		let span = |sample: f32| {
			let parts = played.iter().filter(|p| p.sample == sample);
			let from = parts.clone().map(|p| p.from).min().unwrap();
			let to = parts.clone().map(|p| p.to).max().unwrap();
			let duration: Duration = parts.map(|p| p.to - p.from).sum();
			(from, to, duration)
		};
		let (_, old_end, old) = span(OLD);
		let (new_start, new_end, new) = span(NEW);
		let (back_start, _, back) = span(BACK);
		assert_eq!(old, Duration::from_secs(1), "the retired tail was cut");
		assert_eq!(new, Duration::from_secs(1), "the retired tail was cut");
		assert_eq!(
			back,
			Duration::from_secs(1),
			"the track moved back to did not play in full"
		);
		// Tokio rounds the pacing sleep to the next millisecond.
		for (end, start) in [(old_end, new_start), (new_end, back_start)] {
			let gap = start.saturating_duration_since(end);
			assert!(gap <= Duration::from_millis(1), "the move went silent for {gap:?}");
		}
	}

	/// Publish an audio-only broadcast at `room` under `route`, returning what keeps
	/// it up and its one rendition.
	fn publish(
		origin: &moq_net::origin::Producer,
		route: moq_net::origin::Route,
	) -> (
		moq_net::origin::Publication,
		catalog::Producer,
		moq_mux::container::Producer<Container, AudioConfig>,
	) {
		let broadcast = origin.create_broadcast("room").unwrap();
		let catalog = catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default()).unwrap();
		let audio = rendition(&broadcast, &catalog, "audio");
		broadcast.announce(route).unwrap();
		(broadcast, catalog, audio)
	}

	/// Answer the next request through `claim` with one `audio` rendition, under `route`'s epoch.
	async fn answer(
		claim: &moq_net::origin::Requests,
		route: &moq_net::origin::Route,
	) -> (
		moq_net::origin::Publication,
		catalog::Producer,
		moq_mux::container::Producer<Container, AudioConfig>,
	) {
		let request = tokio::time::timeout(Duration::from_secs(5), claim.requested_broadcast())
			.await
			.expect("the claim is asked")
			.unwrap();
		let prepared = request.prepare(route.epoch.clone().expect("an epoch")).unwrap();
		let mut broadcast = prepared.broadcast().clone();
		let catalog = catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let audio = rendition(&broadcast, &catalog, "audio");
		(prepared.publish().unwrap(), catalog, audio)
	}

	/// Write `count` packets of `sample` in real time, its timeline starting at 0.
	async fn write(audio: &mut moq_mux::container::Producer<Container, AudioConfig>, count: u64, sample: f32) {
		for index in 0..count {
			audio.write(packet(index, sample)).unwrap();
			tokio::time::sleep(PACKET_DURATION).await;
		}
	}

	/// How long `sample` played for, and through how many sinks.
	fn played(recorder: &Recorder, sample: f32) -> (Duration, usize) {
		let played: Vec<_> = recorder.played().into_iter().filter(|p| p.sample == sample).collect();
		let mut sinks: Vec<_> = played.iter().map(|p| p.sink).collect();
		sinks.dedup();
		(played.iter().map(|p| p.to - p.from).sum(), sinks.len())
	}

	/// The player follows the path for as long as the origin lasts, so it never
	/// ends on its own: give it long enough to play out what it was handed, then
	/// stop it.
	async fn settle(player: tokio::task::JoinHandle<()>, recorder: &Recorder) {
		tokio::time::sleep(Duration::from_secs(5)).await;
		for event in recorder.events() {
			match event {
				Event::Failed(err) => panic!("playback failed: {err}"),
				Event::Ended | Event::Finished => panic!("the player stopped following the path"),
				Event::Wake => {}
			}
		}
		assert!(!player.is_finished(), "the player stopped following the path");
		player.abort();
	}

	/// A restarted publisher announces a fresh epoch while its old run still
	/// stands. The restart drops the old broadcast and plays the new one, whose
	/// timeline starts over, instead of waiting for it to catch up.
	#[tokio::test]
	async fn a_republish_plays_the_new_broadcast() {
		tokio::time::pause();

		const OLD: f32 = 0.25;
		const NEW: f32 = 0.5;
		let origin = moq_tokio::origin::spawn();
		let epoch = || moq_net::origin::Route::default().with_epoch(moq_net::Epoch::mint());

		let (_old_broadcast, _old_catalog, mut old) = publish(&origin, epoch());
		let recorder = Recorder::default();
		let player = tokio::spawn(media(&origin, Duration::from_millis(50), recorder.clone()).run());
		write(&mut old, 10, OLD).await;

		let (_new_broadcast, _new_catalog, mut new) = publish(&origin, epoch());
		write(&mut new, 10, NEW).await;

		settle(player, &recorder).await;
		assert_eq!(recorder.speakers(), 1, "the replacement reopened the speaker");
		assert!(
			played(&recorder, OLD).0 > Duration::ZERO,
			"the old broadcast never played"
		);
		assert_eq!(
			played(&recorder, NEW).0,
			PACKET_DURATION * 10,
			"the new broadcast did not play in full"
		);
	}

	/// A selected handler's explicit refusal is terminal, rather than an offline gap that a
	/// later announcement might fix. Waiting forever hides a wrong path or denied request.
	#[tokio::test(start_paused = true)]
	async fn a_refused_broadcast_ends_the_player() {
		let origin = moq_tokio::origin::spawn();
		let handler = origin.requests("").unwrap();
		let recorder = Recorder::default();
		let player = tokio::spawn(media(&origin, Duration::from_millis(50), recorder.clone()).run());
		let request = handler.requested_broadcast().await.unwrap();
		request.reject(moq_net::Error::NotFound);
		tokio::time::timeout(Duration::from_secs(1), player)
			.await
			.expect("a refusal held the player")
			.unwrap();
		assert!(recorder.events().iter().any(|event| matches!(event, Event::Failed(_))));
	}

	/// Without epochs (moq-lite 06), a restart that overlaps the old route still
	/// replaces it: the newest announcement wins the path.
	#[tokio::test]
	async fn an_epochless_republish_plays_the_new_broadcast() {
		tokio::time::pause();

		const OLD: f32 = 0.25;
		const NEW: f32 = 0.5;
		let origin = moq_tokio::origin::spawn();

		let (_old_broadcast, _old_catalog, mut old) = publish(&origin, Default::default());
		let recorder = Recorder::default();
		let player = tokio::spawn(media(&origin, Duration::from_millis(50), recorder.clone()).run());
		write(&mut old, 10, OLD).await;

		let (_new_broadcast, _new_catalog, mut new) = publish(&origin, Default::default());
		// A restart without an epoch waits out the origin's update hold, since a
		// withdrawal wave's stale route looks just like one, and the player joins the
		// new broadcast at its live edge.
		tokio::time::sleep(moq_net::origin::Config::default().update_hold).await;
		write(&mut new, 10, NEW).await;

		settle(player, &recorder).await;
		assert_eq!(recorder.speakers(), 1, "the replacement reopened the speaker");
		assert!(
			played(&recorder, OLD).0 > Duration::ZERO,
			"the old broadcast never played"
		);
		assert_eq!(
			played(&recorder, NEW).0,
			PACKET_DURATION * 10,
			"the new broadcast did not play in full"
		);
	}

	/// A publisher that stops cleanly ends the path, and its restart starts it
	/// afresh. The player waits through the gap and plays the restarted broadcast.
	#[tokio::test]
	async fn a_broadcast_that_returns_plays_again() {
		tokio::time::pause();

		const OLD: f32 = 0.25;
		const NEW: f32 = 0.5;
		let origin = moq_tokio::origin::spawn();

		let (old_broadcast, mut old_catalog, mut old) = publish(&origin, Default::default());
		let recorder = Recorder::default();
		let player = tokio::spawn(media(&origin, Duration::from_millis(50), recorder.clone()).run());
		write(&mut old, 10, OLD).await;
		old.finish().unwrap();
		old_catalog.finish().unwrap();
		drop((old_broadcast, old_catalog, old));

		// Offline for longer than any timeout the player might give up on.
		tokio::time::sleep(Duration::from_secs(60)).await;
		assert!(!player.is_finished(), "the player stopped following the path");

		let (_new_broadcast, _new_catalog, mut new) = publish(&origin, Default::default());
		write(&mut new, 10, NEW).await;

		settle(player, &recorder).await;
		assert_eq!(recorder.speakers(), 1, "the returning broadcast reopened the speaker");
		assert_eq!(
			played(&recorder, OLD).0,
			PACKET_DURATION * 10,
			"the old broadcast lost its tail"
		);
		assert_eq!(
			played(&recorder, NEW).0,
			PACKET_DURATION * 10,
			"the new broadcast did not play in full"
		);
	}

	/// A catalog update lands while a track plays, then the track ends. The
	/// player reads the newer snapshot, which still names the finished track, and
	/// must not resubscribe to it and replay its newest group.
	#[tokio::test]
	async fn a_finished_track_is_not_replayed_after_a_catalog_update() {
		tokio::time::pause();

		let origin = moq_tokio::origin::spawn();
		let (_broadcast, mut catalog, mut audio) = publish(&origin, Default::default());
		let recorder = Recorder::default();
		let player = tokio::spawn(media(&origin, Duration::from_millis(50), recorder.clone()).run());
		write(&mut audio, 10, 0.25).await;
		catalog
			.mutate(|catalog| {
				catalog.audio.renditions.get_mut("audio").unwrap().bitrate = Some(u64::from(SAMPLE_RATE) * 32);
			})
			.unwrap();
		audio.finish().unwrap();

		// The catalog stays open, so the player keeps following it.
		settle(player, &recorder).await;
		assert_eq!(
			played(&recorder, 0.25).0,
			PACKET_DURATION * 10,
			"the finished track replayed"
		);
	}

	/// A catalog can disable a rendition after its track ended but before the
	/// player joins that end. The stop then lands on nothing, and the end still
	/// counts as finished, so re-enabling the rendition does not replay it.
	#[tokio::test]
	async fn a_stop_after_the_track_ended_still_reports_it_finished() {
		let (stop, stopped) = tokio::sync::oneshot::channel();
		let task = tokio::spawn(until_stopped(async { anyhow::Ok(()) }, stopped));
		while !task.is_finished() {
			tokio::task::yield_now().await;
		}
		assert!(stop.send(()).is_err(), "the stop raced a running task");
		let (finished, _) = task.await.unwrap().unwrap();
		assert!(finished, "a track that ended was reported as stopped");
	}

	/// Equivalent `broadcast` references name one track, so a snapshot that only
	/// respells the reference does not make a finished track look new.
	#[tokio::test]
	async fn equivalent_references_name_one_track() {
		let origin = moq_tokio::origin::spawn();
		let (broadcast, _catalog, _audio) = publish(&origin, Default::default());
		let source = moq_mux::Source::new(origin.consume(), broadcast.consume());
		let mut config = AudioConfig::new(AudioCodec::Pcm, SAMPLE_RATE, 1);
		let absent = Track::of(&source, &config, "audio");
		config.broadcast = Some(moq_net::path::Relative::new("").unwrap());
		assert_eq!(Track::of(&source, &config, "audio"), absent);
	}

	/// A covering route of the same epoch that arrives as the exact route goes, both
	/// before the player looks, is the path served again after a gap. A player with
	/// nothing playing plays it.
	#[tokio::test]
	async fn a_covering_route_after_a_gap_plays_while_idle() {
		tokio::time::pause();

		const OLD: f32 = 0.25;
		const NEW: f32 = 0.5;
		let origin = moq_tokio::origin::spawn();
		let route = moq_net::origin::Route::default().with_epoch(moq_net::Epoch::mint());

		let (exact, mut exact_catalog, mut old) = publish(&origin, route.clone());
		let recorder = Recorder::default();
		let player = tokio::spawn(media(&origin, Duration::from_millis(50), recorder.clone()).run());
		write(&mut old, 10, OLD).await;
		old.finish().unwrap();
		exact_catalog.finish().unwrap();
		drop((exact_catalog, old));
		// The broadcast ends while its route stands, so the player goes idle.
		tokio::time::sleep(Duration::from_secs(60)).await;

		let pool = origin.requests("").unwrap();
		drop(exact);
		let (served, served_catalog, mut new) = answer(&pool, &route).await;
		write(&mut new, 10, NEW).await;

		settle(player, &recorder).await;
		assert_eq!(
			played(&recorder, NEW).0,
			PACKET_DURATION * 10,
			"the idle player never played the covering route"
		);
		drop((served, served_catalog, pool));
	}

	/// The exact route goes while the old run still holds its delay, and a covering
	/// route of the same epoch takes over. Playback reaches the covering route, whether
	/// the announcements show the gap (a restart) or the origin rides it out (the same
	/// run carrying on).
	#[tokio::test]
	async fn a_handoff_to_a_covering_route_plays_on_while_draining() {
		tokio::time::pause();

		const OLD: f32 = 0.25;
		const NEW: f32 = 0.5;
		let origin = moq_tokio::origin::spawn();
		let route = moq_net::origin::Route::default().with_epoch(moq_net::Epoch::mint());

		// One instance, served by the exact route and then answered again behind a covering one.
		let (exact, exact_catalog, mut old) = publish(&origin, route.clone());
		let recorder = Recorder::default();
		let player = tokio::spawn(media(&origin, Duration::from_millis(500), recorder.clone()).run());
		for index in 0..50 {
			old.write(packet(index, OLD)).unwrap();
			tokio::time::sleep(PACKET_DURATION).await;
		}

		exact.unannounce();
		let pool = origin.requests("").unwrap();
		let (mirror, mirror_catalog, mut copy) = answer(&pool, &route).await;
		for index in 50..60 {
			copy.write(packet(index, NEW)).unwrap();
			tokio::time::sleep(PACKET_DURATION).await;
		}

		settle(player, &recorder).await;
		assert!(
			played(&recorder, NEW).0 > Duration::ZERO,
			"the player never reached the covering route"
		);
		drop((mirror, mirror_catalog, copy, pool, exact, exact_catalog, old));
	}

	/// Re-pricing the same instance is an update, which playback rides through on
	/// one sink rather than starting over.
	#[tokio::test]
	async fn a_reprice_keeps_playing() {
		tokio::time::pause();

		const SAMPLE: f32 = 0.25;
		let origin = moq_tokio::origin::spawn();
		let route = moq_net::origin::Route::default().with_epoch(moq_net::Epoch::mint());

		let (broadcast, _catalog, mut audio) = publish(&origin, route.clone());
		let recorder = Recorder::default();
		let player = tokio::spawn(media(&origin, Duration::from_millis(50), recorder.clone()).run());
		for index in 0..20 {
			if index == 10 {
				broadcast.announce(route.clone().with_cost(5)).unwrap();
			}
			audio.write(packet(index, SAMPLE)).unwrap();
			tokio::time::sleep(PACKET_DURATION).await;
		}

		settle(player, &recorder).await;
		assert_eq!(
			played(&recorder, SAMPLE),
			(PACKET_DURATION * 20, 1),
			"the update restarted playback"
		);
	}

	#[tokio::test]
	async fn a_finite_audio_track_plays_its_final_samples() {
		tokio::time::pause();
		let origin = moq_tokio::origin::spawn();
		let broadcast = origin.create_broadcast("room").unwrap();
		broadcast.announce(Default::default()).unwrap();
		let mut catalog = catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default()).unwrap();
		let recorder = Recorder::default();
		let player = tokio::spawn(media(&origin, Duration::from_millis(50), recorder.clone()).run());
		let mut audio = rendition(&broadcast, &catalog, "audio");
		audio.write(packet(0, 0.25)).unwrap();
		tokio::time::sleep(PACKET_DURATION).await;
		audio.finish().unwrap();
		drop(audio);
		catalog.finish().unwrap();
		settle(player, &recorder).await;
		let played: Duration = recorder.played().iter().map(|p| p.to - p.from).sum();
		assert_eq!(played, PACKET_DURATION, "the finished track lost its tail");
	}

	/// An encoder fed 1024-sample blocks emits each 960-sample packet once the
	/// block holding its end arrives: a little later each time, then two at once,
	/// so arrivals swing by a packet. That is cadence, and a fixed delay plays
	/// every sample through it.
	#[tokio::test]
	async fn arrivals_swinging_by_a_packet_play_every_sample() {
		tokio::time::pause();
		const BLOCK: u64 = 1024;
		const COUNT: u64 = 100;
		let origin = moq_tokio::origin::spawn();
		let (_broadcast, _catalog, mut audio) = publish(&origin, Default::default());
		let recorder = Recorder::default();
		let player = tokio::spawn(media(&origin, Duration::from_millis(50), recorder.clone()).run());
		let start = Instant::now();
		for index in 0..COUNT {
			let ready = ((index + 1) * PACKET).div_ceil(BLOCK) * BLOCK;
			moq_clock::sleep_until(start + Duration::from_secs_f64(ready as f64 / SAMPLE_RATE as f64)).await;
			audio.write(packet(index, 0.25)).unwrap();
		}
		settle(player, &recorder).await;
		assert_eq!(
			played(&recorder, 0.25).0,
			PACKET_DURATION * COUNT as u32,
			"a packet was skipped"
		);
	}

	/// The 61-frame tune-in burst from #3946 must reach the clock before the
	/// window drains its first picture, regardless of the raw queue's capacity.
	#[tokio::test]
	async fn a_wide_delay_observes_the_whole_tune_in_burst() {
		tokio::time::pause();
		let delay = Duration::from_secs(2);
		let origin = moq_tokio::origin::spawn();
		let broadcast = origin.create_broadcast("room").unwrap();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let mut producer = moq_mux::container::Producer::new(track, Container::Legacy(moq_mux::container::Kind::Data));
		let mut config = moq_video::encode::Config::new(64, 64, moq_video::FrameRate::new(30, 1).unwrap());
		config.kind = moq_video::encode::Kind::Software;
		config.gop = moq_video::encode::Gop::Keyframe { interval: 120 };
		let mut encoder = moq_video::encode::Encoder::new(&config).unwrap();
		for index in 0..=60 {
			let surface = moq_video::Surface::rgba(&vec![128; 64 * 64 * 4], moq_video::Size::new(64, 64)).unwrap();
			let frame = moq_video::Frame::new(surface, moq_net::Timestamp::from_millis(index * 33).unwrap());
			for encoded in encoder.encode(&frame).unwrap() {
				producer
					.write(moq_mux::container::Frame {
						timestamp: encoded.timestamp,
						duration: None,
						payload: encoded.payload,
						keyframe: index == 0,
					})
					.unwrap();
			}
		}
		producer.finish().unwrap();
		let catalog = hang::catalog::VideoConfig::new(hang::catalog::H264 {
			inline: true,
			profile: 0x42,
			constraints: 0,
			level: 30,
		});
		let mut options = moq_video::decode::Options::new();
		options.decoder.kind = moq_video::decode::Kind::Software;
		options.max_delay = delay;
		let decoder = moq_video::decode::Sink::open(&catalog, &options.decoder).await.unwrap();
		let subscriber = broadcast
			.consume()
			.track("video")
			.unwrap()
			.subscribe(moq_net::track::Subscription::default().with_max_delay(delay))
			.await
			.unwrap();
		let track = moq_mux::container::Consumer::new(subscriber, Container::try_from(&catalog).unwrap());
		let recorder = Recorder::default();
		let media = media(&origin, delay, recorder.clone());
		let now = Instant::now();
		let last = moq_net::Timestamp::from_millis(60 * 33).unwrap();
		let task = tokio::spawn(
			Video {
				presentation: media.presentation.clone(),
				frames: media.video.clone(),
				changed: media.drained.clone(),
				output: recorder.clone(),
				max_delay: delay,
			}
			.run(track, decoder),
		);
		loop {
			tokio::task::yield_now().await;
			if media.video.lock().unwrap().len() == 30
				|| media.presentation.lock().unwrap().due(last) == Some(now + delay)
			{
				break;
			}
		}
		let due = media.presentation.lock().unwrap().due(last);
		task.abort();
		let _ = task.await;
		assert_eq!(due, Some(now + delay), "the decoder did not observe the live edge");
		assert!(recorder.present(&media).is_none(), "nothing is due before its delay");
	}
}
