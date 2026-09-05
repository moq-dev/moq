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
use super::timeline::{AudioTimeline, Clock, timestamp};
use super::window::Event;

/// Decoded frames held for presentation, and the point at which the decoder is
/// made to wait. About a second at 30fps: enough to absorb a burst, few enough
/// that raw frames can't run away with memory.
const MAX_VIDEO_FRAMES: usize = 30;

/// How far ahead of the speaker the decoder may run before we make it wait.
/// `Sink::write` never blocks and drops whatever won't fit, so a burst (a
/// completed group arriving all at once) would otherwise lose its tail. Well
/// under the sink's own ceiling, and well over the ~50 ms it settles at.
const AUDIO_BUFFER_MAX: Duration = Duration::from_secs(1);

/// Give up waiting on the speaker to drain this long after the last sample.
/// A device that never opens reports its queue as full forever, and a truncated
/// tail beats hanging on the way out.
const AUDIO_DRAIN_MAX: Duration = Duration::from_secs(4);

/// Everything the media task needs to fill the window and the speaker.
pub(super) struct Media {
	pub(super) origin: moq_net::origin::Consumer,
	pub(super) broadcast: String,
	pub(super) args: Args,
	pub(super) video: Arc<Mutex<VecDeque<moq_video::Frame>>>,
	pub(super) audio_clock: Arc<Mutex<Option<Clock>>>,
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
						playback.ended(joined(result.expect("guarded by is_empty"))?);
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
					decode.latency_max = Some(self.args.latency_max);
					match moq_video::decode::Consumer::new(&rendition, &config, &name, decode).await {
						Ok(consumer) => {
							tracing::info!(track = name, decoder = consumer.name(), "playing video rendition");
							let video = self.video.clone();
							let drained = self.drained.clone();
							let proxy = self.proxy.clone();
							tasks
								.spawn(async move { (Kind::Video, play_video(consumer, video, drained, proxy).await) });
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
					let mut decode = moq_audio::decode::Config::new();
					decode.latency_max = Some(self.args.latency_max);
					// The sink and the frame-duration math below both assume f32,
					// so ask for it rather than inheriting the decoder default.
					decode.format = moq_audio::Format::F32;
					match moq_audio::decode::Consumer::new(&rendition, &config, &name, decode).await {
						Ok(consumer) => {
							tracing::info!(track = name, "playing audio rendition");
							let clock = self.audio_clock.clone();
							let proxy = self.proxy.clone();
							tasks.spawn(async move { (Kind::Audio, play_audio(consumer, clock, proxy).await) });
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
	video: Arc<Mutex<VecDeque<moq_video::Frame>>>,
	drained: Arc<tokio::sync::Notify>,
	proxy: EventLoopProxy<Event>,
) -> anyhow::Result<()> {
	while let Some(frame) = consumer.read().await? {
		// Wait for room rather than dropping the oldest. Audio is paced to real
		// time, so during a catch-up burst the frames at the front are still ahead
		// of the clock, and dropping them would blank the window until the clock
		// reached whatever survived. The presentation clock is anchored to the wall
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
	clock: Arc<Mutex<Option<Clock>>>,
	proxy: EventLoopProxy<Event>,
) -> anyhow::Result<()> {
	let sample_rate = consumer.sample_rate();
	let channels = consumer.channels();
	let engine = moq_audio::playback::Engine::open(Default::default()).await?;
	let input = moq_audio::playback::Input {
		format: moq_audio::Format::F32,
		sample_rate,
		channels,
	};
	let mut sink = engine.sink(input.clone())?;

	// One sample across every channel, the unit a write has to stay aligned to.
	let stride = channels as usize * size_of::<f32>();
	// A second per write, so a frame longer than the sink can hold is paced in
	// rather than handed over whole and truncated. An Opus packet caps at 120 ms,
	// but a PCM one is only required to be sample-aligned, so it can be any length.
	// Paired with the wait below this keeps the sink under two seconds, inside its
	// own ceiling.
	let chunk = (sample_rate as usize * stride).max(stride);

	// The longest hole worth playing through, in samples. A hole this player would
	// rather sit through is one it is already willing to buffer, which is what the
	// decoder's latency budget says: anything longer is what that budget chose to
	// skip, so playing it as silence would hand back the delay the skip avoided.
	// Past it the sink skips the hole and the clock re-anchors, as it does today.
	let fill_max = (consumer.latency_max().as_secs_f64() * sample_rate as f64) as u64;
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
			*clock.lock().unwrap() = None;
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
				if let Some(excess) = sink.buffered().checked_sub(AUDIO_BUFFER_MAX) {
					tokio::time::sleep(excess).await;
				}
				let part = remaining.min(silence.len());
				sink.write(&silence[..part])?;
				remaining -= part;
			}
		}

		for part in frame.data.chunks(chunk) {
			// Let the speaker catch up before handing it more than it can hold.
			if let Some(excess) = sink.buffered().checked_sub(AUDIO_BUFFER_MAX) {
				tokio::time::sleep(excess).await;
			}
			sink.write(part)?;
		}

		let previous = clock.lock().unwrap().replace(Clock {
			media: timing.end.saturating_sub(sink.buffered()),
			wall: Instant::now(),
		});
		// Only the very first sample needs a wake, to hand the render loop a clock
		// to schedule against. After that the clock extrapolates from its wall
		// anchor, so waking per 20 ms frame would just redraw the same picture.
		if previous.is_none() {
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
	let _ = tokio::time::timeout(AUDIO_DRAIN_MAX, drain).await;

	Ok(())
}
