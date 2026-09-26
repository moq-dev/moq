//! The media task: subscribe, pick renditions, and decode into the window and
//! the speaker.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use hang::moq_net;
use moq_mux::catalog::{self, Stream};
// tokio's clock, which is the wall clock unless a test pauses it to drive the
// playout clock itself.
use tokio::time::Instant;

use super::args::Args;
use super::output::{Output, Sink, Speaker};
use super::playback::{Kind, Playback, joined};
use super::source::subscribe;
use super::timeline::{AudioTimeline, Presentation, timestamp};
use super::video::Video;
use super::window::Event;

/// The floor on the speaker's buffer, whatever delay was asked for. The device
/// pulls on a fixed clock, so a ring shallower than this drops out on ordinary
/// network jitter, and one with no depth at all can never be read from.
const AUDIO_BUFFER_MIN: Duration = Duration::from_millis(50);

/// How much audio is handed to the speaker per write.
///
/// The playout delay lives in the sink, so every byte written past that depth
/// overshoots it, and writing in slices keeps the overshoot under one slice
/// however long a PCM frame is (an Opus packet caps at 120 ms, but a PCM one is
/// only required to be sample-aligned). Pacing between the slices is also what
/// stops `Sink::write`, which never blocks and drops whatever won't fit, from
/// losing the tail of a burst.
const AUDIO_CHUNK: Duration = Duration::from_millis(20);

/// How much longer than the speaker could possibly hold to wait for it to
/// drain. A device that never opens reports its queue as full forever, and a
/// truncated tail beats hanging on the way out, but the budget has to cover the
/// ring the delay asked for or every finite track loses its last `delay`.
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

	async fn play(self) -> anyhow::Result<()> {
		let source = subscribe(self.origin.clone(), &self.broadcast).await?;
		let broadcast = source
			.broadcast()
			.await
			.context("failed to subscribe to the broadcast")?;
		let catalog = catalog::Consumer::<()>::new(&broadcast, self.args.catalog_format(&self.broadcast))
			.await
			.context("failed to subscribe to the catalog")?;
		let mut catalogs = catalog.select(self.args.select.selection(None));
		// The floored depth, not the raw delay: the speaker holds at least
		// AUDIO_BUFFER_MIN whatever was asked for, so a smaller budget would skip a
		// group the playhead could still have reached, and would size the hole
		// fill in `play_audio` to a playhead that does not exist.
		let depth = self.args.delay.into_std().max(AUDIO_BUFFER_MIN);
		let mut tasks = tokio::task::JoinSet::new();
		let mut playback = Playback::default();
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
						let ended = joined(result.expect("guarded by is_empty"))?.map(|(kind, sink)| {
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
								tails.spawn(drain(sink, depth));
							}
							kind
						});
						playback.ended(ended);
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
							Some(snapshot) => playback.received(snapshot),
							None => {
								anyhow::ensure!(playback.played, "the catalog contains no playable audio or video renditions");
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
				for (name, config) in snapshot.video.renditions {
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
					let max_age = self.args.delay.into_std();
					let opened = async {
						let decoder = moq_video::decode::Sink::open(&config, &Default::default()).await?;
						let track = rendition.track(&name)?;
						let mut subscriber = track
							.subscribe(
								moq_net::track::Subscription::default()
									.with_priority(hang::catalog::PRIORITY.video)
									.with_max_age(max_age),
							)
							.await?;
						// Start at the local live edge without asking the shared publisher
						// subscription to rewind to a cached sequence.
						if let Some(latest) = track.latest() {
							subscriber.set_groups(latest..);
						}
						let format = catalog::hang::Container::try_from(&config)?;
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
								max_age,
							};
							tasks.spawn(async move { (Kind::Video, video.run(track, decoder).await.map(|()| None)) });
							playback.started(Kind::Video);
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
				for (name, config) in snapshot.audio.renditions {
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
					decode.max_age = depth;
					// The sink and the frame-duration math below both assume f32,
					// so ask for it rather than inheriting the decoder default.
					decode.output.format = moq_audio::Format::F32;
					match moq_audio::decode::Consumer::new(&rendition, &config, &name, decode).await {
						Ok(consumer) => {
							tracing::info!(track = name, "playing audio rendition");
							if speaker.is_none() {
								speaker = Some(self.output.speaker().await?);
							}
							let audio = AudioPlayback {
								speaker: speaker.clone().expect("opened above"),
								presentation: self.presentation.clone(),
								depth,
								changed: self.drained.clone(),
								output: self.output.clone(),
							};
							tasks.spawn(async move { (Kind::Audio, play_audio(consumer, audio).await.map(Some)) });
							playback.started(Kind::Audio);
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
			anyhow::ensure!(
				!tasks.is_empty() || rejected.is_empty(),
				"no playable rendition in the catalog: {}",
				rejected.join("; ")
			);
		}
	}
}

struct AudioPlayback<O: Output> {
	changed: Arc<tokio::sync::Notify>,
	speaker: O::Speaker,
	presentation: Arc<Mutex<Presentation>>,
	depth: Duration,
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
		depth,
		output,
	} = playback;

	// `depth` is how much the speaker holds: the playout delay, floored, and the
	// same value the decoder's age budget was built from. The delay lives in the
	// sink rather than in the throttle, since a sample handed over now sounds
	// that much later and waiting for the delayed instant before writing would
	// take it twice. The window schedules video against where the speaker
	// actually is, which keeps the two together.
	let sample_rate = consumer.sample_rate();
	let layout = consumer.layout();
	let channels = layout.channels();
	let mut input = moq_audio::playback::Input::default();
	input.format = moq_audio::Format::F32;
	input.sample_rate = sample_rate;
	input.layout = layout;
	input.latency = depth;
	let mut sink = speaker.sink(input.clone())?;

	// One sample across every channel, the unit a write has to stay aligned to.
	let stride = channels as usize * size_of::<f32>();
	let chunk = ((AUDIO_CHUNK.as_secs_f64() * sample_rate as f64) as usize * stride).max(stride);

	// The longest hole worth playing through, in samples. A hole this player would
	// rather sit through is one it is already willing to buffer, which is what the
	// decoder's latency budget says: anything longer is what that budget chose to
	// skip, so playing it as silence would hand back the delay the skip avoided.
	// Past it the sink skips the hole and the clock re-anchors, as it does today.
	let fill_max = (consumer.max_age().as_secs_f64() * sample_rate as f64) as u64;
	let silence = vec![0u8; chunk];

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

		let samples = frame.data.len() / size_of::<f32>() / channels as usize;
		let start = timestamp(frame.timestamp);
		let timing = timeline.push(start, samples, sample_rate, fill_max);

		// A rewind or a hole too large to fill starts a new playback sink. The old
		// sink has no media clock, so its buffered audio cannot be carried across a
		// timeline region the player skipped.
		if timing.reset_sink {
			drop(sink);
			presentation.lock().unwrap().restarted();
			sink = speaker.sink(input.clone())?;
		}

		// A hole in the media is a hole in the audio, not a splice. Handing the next
		// frame straight to the speaker shortens the track by the missing duration,
		// which leaves it running ahead of media time until the clock below
		// re-anchors, taking the video with it. Play the hole instead.
		if timing.silence > 0 {
			let mut remaining = usize::try_from(timing.silence)
				.unwrap_or(usize::MAX / stride)
				.saturating_mul(stride);
			while remaining > 0 {
				if let Some(excess) = sink.buffered().checked_sub(depth) {
					tokio::time::sleep(excess).await;
				}
				let part = remaining.min(silence.len());
				sink.write(&silence[..part])?;
				remaining -= part;
			}
		}

		for part in frame.data.chunks(chunk) {
			// Let the speaker drain back to the target depth before topping it up.
			// This is what paces the whole task: the device drains in real time, so
			// the writes end up on the media clock and the sink holds the delay.
			if let Some(excess) = sink.buffered().checked_sub(depth) {
				tokio::time::sleep(excess).await;
			}
			sink.write(part)?;
		}

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
/// by dropping it. `latency` is the depth the sink was opened with.
async fn drain(sink: impl Sink, latency: Duration) {
	let drain = async {
		// A partial period is left to the device: waiting on the last few
		// milliseconds costs a wakeup per iteration and can never fully settle.
		while let Some(remaining) = sink.buffered().checked_sub(Duration::from_millis(10)) {
			tokio::time::sleep(remaining.max(Duration::from_millis(10))).await;
		}
	};
	// A write tops the ring up to its latency and then adds a chunk, so that sum
	// is the deepest it can be when the track ends, and draining it takes exactly
	// that long in real time.
	let _ = tokio::time::timeout(latency + AUDIO_CHUNK + AUDIO_DRAIN_GRACE, drain).await;
}

#[cfg(test)]
mod tests {
	use bytes::Bytes;
	use hang::catalog::{AudioCodec, AudioConfig};
	use moq_mux::catalog::hang::Container;

	use super::*;
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
				delay: delay.into(),
				select: Default::default(),
			},
			video: Default::default(),
			presentation: Arc::new(Mutex::new(Presentation::new(delay))),
			drained: Default::default(),
			output,
		}
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

		player.await.unwrap();
		match recorder.events().pop() {
			Some(Event::Ended) => {}
			Some(Event::Failed(err)) => panic!("playback failed: {err}"),
			_ => panic!("playback never ended"),
		}

		let played = recorder.played();
		let old_end = played.iter().filter(|p| p.sample == OLD).map(|p| p.to).max().unwrap();
		let new_start = played.iter().filter(|p| p.sample == NEW).map(|p| p.from).min().unwrap();
		// The tail plays out while the replacement fills, so the two meet. What is
		// left is the partial period `drain` leaves to the device, which dropping
		// the sink cuts.
		let gap = new_start.saturating_duration_since(old_end);
		assert!(gap < AUDIO_CHUNK, "the switch went silent for {gap:?}");
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
		options.max_age = delay;
		let decoder = moq_video::decode::Sink::open(&catalog, &options.decoder).await.unwrap();
		let subscriber = broadcast
			.consume()
			.track("video")
			.unwrap()
			.subscribe(moq_net::track::Subscription::default().with_max_age(delay))
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
				max_age: delay,
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
