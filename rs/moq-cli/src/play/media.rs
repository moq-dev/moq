//! The media task: subscribe, pick renditions, and decode into the window and
//! the speaker.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use hang::moq_net;
use moq_mux::catalog::{self, Stream};
use winit::event_loop::EventLoopProxy;

use super::args::Args;
use super::playback::{Kind, Playback, joined};
use super::source::subscribe;
use super::timeline::{AudioTimeline, Presentation, timestamp};
use super::window::Event;

/// Decoded frames held for presentation, and the point at which the decoder is
/// made to wait. About a second at 30fps: enough to absorb a burst, few enough
/// that raw frames can't run away with memory.
const MAX_VIDEO_FRAMES: usize = 30;

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
pub(super) struct Media {
	pub(super) origin: moq_net::origin::Consumer,
	pub(super) broadcast: String,
	pub(super) args: Args,
	pub(super) video: Arc<Mutex<VecDeque<moq_video::Frame>>>,
	pub(super) presentation: Arc<Mutex<Presentation>>,
	pub(super) drained: Arc<tokio::sync::Notify>,
	pub(super) proxy: EventLoopProxy<Event>,
}

impl Media {
	pub(super) async fn run(self) {
		let proxy = self.proxy.clone();
		let event = match self.play().await {
			Ok(()) => Event::Ended,
			Err(err) => Event::Failed(format!("{err:#}")),
		};
		let _ = proxy.send_event(event);
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
		let mut tasks = tokio::task::JoinSet::new();
		let mut playback = Playback::default();

		loop {
			if playback.done() {
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
						let ended = joined(result.expect("guarded by is_empty"))?;
						if ended == Some(Kind::Audio) {
							// Nothing holds playback to the speaker's cadence any more, so
							// video takes the playout anchor back.
							self.presentation.lock().unwrap().stopped();
						}
						playback.ended(ended);
					}
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
					let mut decode = moq_video::decode::Config::new();
					// Nothing older than the playhead is worth presenting, so the delay
					// doubles as the staleness budget on the wire.
					decode.max_age = self.args.delay.into_std();
					match moq_video::decode::Consumer::new(&rendition, &config, &name, decode).await {
						Ok(consumer) => {
							tracing::info!(track = name, decoder = consumer.name(), "playing video rendition");
							let presentation = self.presentation.clone();
							let video = self.video.clone();
							let drained = self.drained.clone();
							let proxy = self.proxy.clone();
							tasks.spawn(async move {
								(
									Kind::Video,
									play_video(consumer, presentation, video, drained, proxy).await,
								)
							});
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
					// The floored depth, not the raw delay: the speaker holds at least
					// AUDIO_BUFFER_MIN whatever was asked for, so a smaller budget would
					// skip a group the playhead could still have reached, and would size
					// the hole fill below to a playhead that does not exist.
					let depth = self.args.delay.into_std().max(AUDIO_BUFFER_MIN);
					let mut decode = moq_audio::decode::Config::new();
					decode.max_age = depth;
					// The sink and the frame-duration math below both assume f32,
					// so ask for it rather than inheriting the decoder default.
					decode.format = moq_audio::Format::F32;
					match moq_audio::decode::Consumer::new(&rendition, &config, &name, decode).await {
						Ok(consumer) => {
							tracing::info!(track = name, "playing audio rendition");
							let presentation = self.presentation.clone();
							let proxy = self.proxy.clone();
							tasks.spawn(async move {
								(Kind::Audio, play_audio(consumer, presentation, depth, proxy).await)
							});
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

async fn play_video(
	mut consumer: moq_video::decode::Consumer,
	presentation: Arc<Mutex<Presentation>>,
	video: Arc<Mutex<VecDeque<moq_video::Frame>>>,
	drained: Arc<tokio::sync::Notify>,
	proxy: EventLoopProxy<Event>,
) -> anyhow::Result<()> {
	while let Some(frame) = consumer.read().await? {
		// Fold the arrival into the playout clock before queueing it, so the window
		// always has a deadline for whatever it finds in the queue. A move has to
		// wake it before the wait below, not after: the window is asleep on the old
		// anchor's deadline, and it is the only thing that drains the queue this
		// task is about to block on.
		if presentation.lock().unwrap().video(frame.timestamp, Instant::now()) {
			let _ = proxy.send_event(Event::Wake);
		}

		// Wait for room rather than dropping the oldest. Audio is paced to real
		// time, so during a catch-up burst the frames at the front are still ahead
		// of the clock, and dropping them would blank the window until the clock
		// reached whatever survived. The playout clock is anchored to the wall
		// clock, so the queue always drains and this always clears.
		while video.lock().unwrap().len() >= MAX_VIDEO_FRAMES {
			drained.notified().await;
		}

		video.lock().unwrap().push_back(frame);
		let _ = proxy.send_event(Event::Wake);
	}
	Ok(())
}

async fn play_audio(
	mut consumer: moq_audio::decode::Consumer,
	presentation: Arc<Mutex<Presentation>>,
	depth: Duration,
	proxy: EventLoopProxy<Event>,
) -> anyhow::Result<()> {
	// `depth` is how much the speaker holds: the playout delay, floored, and the
	// same value the decoder's age budget was built from. The delay lives in the
	// sink rather than in the throttle, since a sample handed over now sounds
	// that much later and waiting for the delayed instant before writing would
	// take it twice. The window schedules video against where the speaker
	// actually is, which keeps the two together.
	let sample_rate = consumer.sample_rate();
	let channels = consumer.channels();
	let engine = moq_audio::playback::Engine::open(Default::default()).await?;
	let mut input = moq_audio::playback::Input::default();
	input.format = moq_audio::Format::F32;
	input.sample_rate = sample_rate;
	input.channels = channels;
	input.latency = depth;
	let mut sink = engine.sink(input.clone())?;

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
			sink = engine.sink(input.clone())?;
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
			.audio(timing.end, sink.buffered(), Instant::now());
		if moved {
			let _ = proxy.send_event(Event::Wake);
		}
	}

	// The track ended, but the speaker is still a buffer behind. Play it out
	// instead of cutting the tail off by dropping the sink.
	let drain = async {
		// A partial period is left to the device: waiting on the last few
		// milliseconds costs a wakeup per iteration and can never fully settle.
		while let Some(remaining) = sink.buffered().checked_sub(Duration::from_millis(10)) {
			tokio::time::sleep(remaining.max(Duration::from_millis(10))).await;
		}
	};
	// A write tops the ring up to `depth` and then adds a chunk, so that sum is
	// the deepest it can be when the track ends, and draining it takes exactly
	// that long in real time.
	let _ = tokio::time::timeout(depth + AUDIO_CHUNK + AUDIO_DRAIN_GRACE, drain).await;

	Ok(())
}
