//! The media task: subscribe, pick renditions, and decode into the window and
//! the speaker.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use hang::moq_net;
use moq_mux::catalog::{self, Stream};
use moq_net::announce::Event as AnnounceEvent;
// tokio's clock, which is the wall clock unless a test pauses it to drive the
// playout clock itself.
use tokio::time::Instant;

use super::args::Args;
use super::output::{Output, Sink, Speaker};
use super::playback::{Kind, Playback, Track, joined};
use super::timeline::{AudioTimeline, Presentation, fit, timestamp};
use super::video::Video;
use super::window::Event;

/// The floor on the speaker's buffer, whatever delay was asked for. The device
/// pulls on a fixed clock, so a ring shallower than this drops out on ordinary
/// network jitter, and one with no depth at all can never be read from.
const AUDIO_BUFFER_MIN: Duration = Duration::from_millis(50);

/// How far the speaker may run over its target before a write skips back onto
/// it.
///
/// The level moves by a device period each time the speaker pulls and by a
/// packet each time one lands, so a margin under that would skip on ordinary
/// cadence. Anything over it is latency nobody asked for, since a live stream
/// arrives as fast as it plays and never drains the excess on its own.
const AUDIO_SLACK: Duration = Duration::from_millis(20);

/// Silence written per slice when padding, so the buffer stays a fixed size.
const AUDIO_SILENCE: Duration = Duration::from_millis(20);

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
		let mut follow = self.origin.follow(&self.broadcast)?;
		let mut playing = None;

		loop {
			tokio::select! {
				event = follow.next() => {
					// The origin is gone, so nothing can announce the path again.
					let Some(event) = event else { return Ok(()) };
					let announce = match event {
						AnnounceEvent::Start(announce) | AnnounceEvent::Restart(announce) => announce,
						AnnounceEvent::Update(_) => continue,
						AnnounceEvent::End(_) => {
							tracing::info!(broadcast = %self.broadcast, "offline, waiting for it to return");
							continue;
						}
					};
					tracing::info!(
						broadcast = %self.broadcast,
						epoch = announce.route.epoch.as_ref().map(tracing::field::display),
						"online"
					);

					// Another instance's timeline is its own, so nothing of the old one carries over.
					drop(playing.take());
					*self.presentation.lock().unwrap() = Presentation::new(self.args.video_delay());
					self.video.lock().unwrap().clear();
					self.drained.notify_one();
					self.output.send(Event::Wake);
					playing = Some(Box::pin(self.play_broadcast()));
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

	/// Play the broadcast now at the path until its catalog and every track it
	/// started end.
	async fn play_broadcast(&self) -> anyhow::Result<()> {
		let broadcast = self
			.origin
			.request_broadcast(&self.broadcast, None)
			.await
			.context("failed to subscribe to the broadcast")?;
		let source = moq_mux::Source::new(self.origin.clone(), broadcast.clone());
		let mut catalogs = catalog::Consumer::<()>::new(&broadcast, self.args.catalog_format(&self.broadcast))
			.await
			.context("failed to subscribe to the catalog")?;
		// Applied by hand rather than with `select`, which drops disabled renditions: a disabled
		// rendition stops now, while one that leaves the catalog plays out its tail.
		let selection = self.args.select.selection(None);
		let mut tasks = tokio::task::JoinSet::new();
		let mut playback = Playback::default();
		let mut video_demand: Option<(String, tokio::sync::oneshot::Sender<()>)> = None;
		let mut audio_demand: Option<(String, tokio::sync::oneshot::Sender<()>)> = None;
		// Shared by an audio rendition and the retired tails still playing beside
		// it, so their sinks mix on one stream: a second stream on an exclusive
		// device would fail to open. Released once none of them is left, so an
		// idle `play` does not hold the device.
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
								stop_disabled(&mut video_demand, snapshot.video.renditions.iter().filter(|(_, config)| !config.enabled).map(|(name, _)| name));
								stop_disabled(&mut audio_demand, snapshot.audio.renditions.iter().filter(|(_, config)| !config.enabled).map(|(name, _)| name));
								selection.retain(&mut snapshot);
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

			// The speaker is only open while some audio is, so releasing it marks the
			// last of it going quiet: nothing holds playback to the speaker's cadence
			// any more, and video takes the anchor back.
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
					let rendition = match source.resolve(config.broadcast.as_ref()).await {
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
						let decoder = moq_video::decode::Sink::open(config, &Default::default()).await?;
						let track = rendition.track(hang::catalog::Reference::track(config, name))?;
						let mut subscriber = track
							.subscribe(
								moq_net::track::Subscription::default()
									.with_priority(hang::catalog::PRIORITY.video)
									.with_max_delay(max_delay),
							)
							.await?;
						// Start at the local live edge without asking the shared publisher
						// subscription to rewind to a cached sequence.
						if let Some(latest) = track.latest() {
							subscriber.set_groups(latest..);
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
							video_demand = Some((name.clone(), stop));
							tasks.spawn(async move {
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
					let rendition = match source.resolve(config.broadcast.as_ref()).await {
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
								speaker = Some(self.output.speaker().await?);
							}
							let audio = AudioPlayback {
								speaker: speaker.clone().expect("opened above"),
								presentation: self.presentation.clone(),
								latency: self.args.fixed_delay().unwrap_or_default().max(AUDIO_BUFFER_MIN),
								changed: self.drained.clone(),
								output: self.output.clone(),
							};
							let (stop, stopped) = tokio::sync::oneshot::channel();
							audio_demand = Some((name.clone(), stop));
							tasks.spawn(async move {
								(Kind::Audio, until_stopped(play_audio(consumer, audio), stopped).await)
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
	stopped: tokio::sync::oneshot::Receiver<()>,
) -> anyhow::Result<(bool, Option<T>)> {
	tokio::select! {
		result = play => result.map(|left| (true, Some(left))),
		_ = stopped => Ok((false, None)),
	}
}

// Cancel through the task's own result so it still reports which half ended.
fn stop_disabled<'a>(
	playing: &mut Option<(String, tokio::sync::oneshot::Sender<()>)>,
	mut disabled: impl Iterator<Item = &'a String>,
) {
	if playing
		.as_ref()
		.is_some_and(|(name, _)| disabled.any(|disabled| disabled == name))
	{
		let (_, stop) = playing.take().unwrap();
		let _ = stop.send(());
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

/// Play a track until it ends, handing back the sink with the delay it still
/// holds.
async fn play_audio<O: Output>(
	mut consumer: moq_audio::decode::Consumer,
	playback: AudioPlayback<O>,
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
	// loop never waits on the speaker between reads. It writes everything as it
	// comes and lets `fit` hold the sink on the target instead. The window
	// schedules video against where the speaker actually is, which keeps the two
	// together whatever the target does.
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
	let silence = vec![0u8; samples(AUDIO_SILENCE).max(1) as usize * stride];

	// The longest hole worth playing through, in samples. A hole this player would
	// rather sit through is one it is already willing to buffer, which is what the
	// decoder's latency budget says: anything longer is what that budget chose to
	// skip, so playing it as silence would hand back the delay the skip avoided.
	// Past it the sink skips the hole and the clock re-anchors, as it does today.
	let fill_max = samples(consumer.max_delay());

	let mut timeline = AudioTimeline::default();

	// Tracks whether the last read failed, so a stream the decoder can't read at
	// all logs once rather than once per packet.
	let mut dropping = false;

	loop {
		let frame = match consumer.read().await {
			Ok(Some(frame)) => frame,
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
		let start = timestamp(frame.timestamp);
		let timing = timeline.push(start, length, sample_rate, fill_max);

		// A rewind or a hole too large to fill starts a new playback sink. The old
		// sink has no media clock, so its buffered audio cannot be carried across a
		// timeline region the player skipped.
		if timing.reset_sink {
			drop(sink);
			presentation.lock().unwrap().restarted();
			sink = speaker.sink(input.clone())?;
			dry = true;
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
		let target = samples(consumer.delay().min(consumer.max_delay()).max(AUDIO_BUFFER_MIN));
		let fit = fit(dry, buffered, target, slack, timing.silence + length as u64);
		dry = false;

		// Playback drops stay on the live timeline; retrying them would add latency,
		// and the sink already reports them in its logs.
		let mut quiet = fit.pad + timing.silence.saturating_sub(fit.skip);
		while quiet > 0 {
			let part = (quiet as usize * stride).min(silence.len());
			sink.write(&silence[..part])?;
			quiet -= (part / stride) as u64;
		}
		let skip = fit.skip.saturating_sub(timing.silence) as usize * stride;
		sink.write(&frame.data[skip.min(frame.data.len())..])?;

		// Anchor the playout clock on where the speaker has actually reached, which
		// is the only half of the pipeline that cannot skip ahead. A move has to
		// wake the window: it is asleep on a deadline computed from the old anchor,
		// and every queued frame is due earlier now.
		let moved = presentation
			.lock()
			.unwrap()
			.audio(timing.end, sink.buffered(), Instant::now().into_std());
		if moved {
			changed.notify_one();
			output.send(Event::Wake);
		}
	}

	Ok(sink)
}

/// Play out what a retired sink still holds, instead of cutting the tail off
/// by dropping it.
async fn drain(sink: impl Sink) {
	// The estimated target moves, so the depth the sink was opened with says
	// nothing about what it holds now: read it at retirement instead.
	let _ = tokio::time::timeout(sink.buffered() + AUDIO_DRAIN_GRACE, sink.finish()).await;
}

#[cfg(test)]
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
		let mut broadcast = origin.create_broadcast("room").unwrap();
		broadcast.announce(Default::default()).unwrap();
		let mut catalog = catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
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
		let mut broadcast = origin.create_broadcast("room").unwrap();
		broadcast.announce(Default::default()).unwrap();
		let mut catalog = catalog::Producer::new(&mut broadcast, Default::default()).unwrap();

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
			tokio::time::sleep_until(start + PACKET_DURATION * index as u32).await;
		}

		// The replacement joins the catalog, then the old track finishes.
		let mut new = rendition(&broadcast, &catalog, "new");
		new.write(packet(index, NEW)).unwrap();
		index += 1;
		old.finish().unwrap();
		drop(old);

		while index < 100 {
			tokio::time::sleep_until(start + PACKET_DURATION * index as u32).await;
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

	/// Publish an audio-only broadcast at `room` under `route`, returning what keeps
	/// it up and its one rendition.
	fn publish(
		origin: &moq_net::origin::Producer,
		route: moq_net::origin::Route,
	) -> (
		moq_net::broadcast::Producer,
		catalog::Producer,
		moq_mux::container::Producer<Container, AudioConfig>,
	) {
		let mut broadcast = origin.create_broadcast("room").unwrap();
		let catalog = catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let audio = rendition(&broadcast, &catalog, "audio");
		broadcast.announce(route).unwrap();
		(broadcast, catalog, audio)
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
		tokio::time::sleep(moq_net::origin::DEFAULT_UPDATE_HOLD).await;
		write(&mut new, 10, NEW).await;

		settle(player, &recorder).await;
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
		config.broadcast = Some(moq_net::path::RelativeOwned::new(""));
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

		let mut served = moq_net::broadcast::Info::new().produce();
		let served_catalog = catalog::Producer::new(&mut served, Default::default()).unwrap();
		let mut new = rendition(&served, &served_catalog, "audio");
		let pool = origin.dynamic("", route).unwrap();
		drop(exact);
		let consumer = served.consume();
		tokio::spawn(async move {
			while let Ok(request) = pool.requested_broadcast().await {
				request.accept(consumer.clone());
			}
		});
		write(&mut new, 10, NEW).await;

		settle(player, &recorder).await;
		assert_eq!(
			played(&recorder, NEW).0,
			PACKET_DURATION * 10,
			"the idle player never played the covering route"
		);
		drop((served, served_catalog));
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

		// One instance, served by the exact route and mirrored behind a covering one.
		let (exact, exact_catalog, mut old) = publish(&origin, route.clone());
		let mut mirror = moq_net::broadcast::Info::new().produce();
		let mirror_catalog = catalog::Producer::new(&mut mirror, Default::default()).unwrap();
		let mut copy = rendition(&mirror, &mirror_catalog, "audio");
		let recorder = Recorder::default();
		let player = tokio::spawn(media(&origin, Duration::from_millis(500), recorder.clone()).run());
		for index in 0..50 {
			old.write(packet(index, OLD)).unwrap();
			copy.write(packet(index, OLD)).unwrap();
			tokio::time::sleep(PACKET_DURATION).await;
		}

		exact.unannounce();
		let pool = origin.dynamic("", route).unwrap();
		let consumer = mirror.consume();
		tokio::spawn(async move {
			while let Ok(request) = pool.requested_broadcast().await {
				request.accept(consumer.clone());
			}
		});
		for index in 50..60 {
			copy.write(packet(index, NEW)).unwrap();
			tokio::time::sleep(PACKET_DURATION).await;
		}

		settle(player, &recorder).await;
		assert!(
			played(&recorder, NEW).0 > Duration::ZERO,
			"the player never reached the covering route"
		);
		drop((mirror, mirror_catalog, copy, exact, exact_catalog, old));
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
		let mut broadcast = origin.create_broadcast("room").unwrap();
		broadcast.announce(Default::default()).unwrap();
		let mut catalog = catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
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
		let mut config = moq_video::encode::Config::new(64, 64, moq_video::Rate::new(30, 1).unwrap());
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
				|| media.presentation.lock().unwrap().due(last) == Some((now + delay).into_std())
			{
				break;
			}
		}
		let due = media.presentation.lock().unwrap().due(last);
		task.abort();
		let _ = task.await;
		assert_eq!(
			due,
			Some((now + delay).into_std()),
			"the decoder did not observe the live edge"
		);
		assert!(recorder.present(&media).is_none(), "nothing is due before its delay");
	}
}
