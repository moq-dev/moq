//! Native playback for a MoQ broadcast.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::Args as ClapArgs;
use hang::moq_net;
use moq_mux::catalog::{self, CatalogFormat, Stream};
use moq_video::render::wgpu;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use crate::subscribe::{CatalogFormatArg, SelectArgs};

/// Decoded frames held for presentation, and the point at which the decoder is
/// made to wait. About a second at 30fps: enough to absorb a burst, few enough
/// that raw frames can't run away with memory.
const MAX_VIDEO_FRAMES: usize = 30;

/// How early a frame may be shown rather than waiting another wakeup for it.
/// Under a display's frame interval, so it can't be seen, but enough that timer
/// slop doesn't push every frame a whole refresh late.
const VIDEO_EARLY_TOLERANCE: Duration = Duration::from_millis(2);

/// How far ahead of the speaker the decoder may run before we make it wait.
/// `Sink::write` never blocks and drops whatever won't fit, so a burst (a
/// completed group arriving all at once) would otherwise lose its tail. Well
/// under the sink's own ceiling, and well over the ~50 ms it settles at.
const AUDIO_BUFFER_MAX: Duration = Duration::from_secs(1);

/// Consecutive surface rebuilds before a frame is written off. A display change
/// or a resume costs one; without a ceiling, a surface that can never present
/// would redraw forever, since each retry is what schedules the next.
const MAX_PRESENT_RETRIES: u32 = 8;

/// Give up waiting on the speaker to drain this long after the last sample.
/// A device that never opens reports its queue as full forever, and a truncated
/// tail beats hanging on the way out.
const AUDIO_DRAIN_MAX: Duration = Duration::from_secs(4);

/// Play one MoQ broadcast through a native window and speaker.
#[derive(ClapArgs, Clone)]
pub struct Args {
	/// Catalog format, detected from the broadcast suffix when omitted.
	#[arg(long)]
	pub catalog_format: Option<CatalogFormatArg>,

	/// Maximum media buffering before skipping a stalled group.
	#[arg(long, default_value = "500ms", value_parser = humantime::parse_duration)]
	pub latency_max: Duration,

	/// Rendition selection by track name or codec.
	#[command(flatten)]
	pub select: SelectArgs,
}

impl Args {
	fn catalog_format(&self, broadcast: &str) -> CatalogFormat {
		self.catalog_format
			.map(Into::into)
			.or_else(|| CatalogFormat::detect(broadcast))
			.unwrap_or_default()
	}

	/// Reject a codec the local decoders can't open.
	///
	/// The selection flags are shared with the stdout exports, which pass bytes
	/// through and so accept every codec the catalog can name. Asking for one of
	/// those here would filter the catalog down to a rendition that then fails to
	/// decode, leaving a blank window rather than an error.
	pub fn validate(&self) -> anyhow::Result<()> {
		use crate::subscribe::VideoCodecArg;

		anyhow::ensure!(
			!matches!(self.select.video_codec, Some(VideoCodecArg::Vp8 | VideoCodecArg::Vp9)),
			"`play` cannot decode vp8 or vp9; pass --video-codec h264, h265, or av1"
		);
		Ok(())
	}
}

#[derive(Clone, Copy)]
struct Clock {
	media: Duration,
	wall: Instant,
}

impl Clock {
	fn now(self) -> Duration {
		self.media.saturating_add(self.wall.elapsed())
	}
}

enum Event {
	Wake,
	/// Stop now: Ctrl-C, or the transport is gone and there is nothing more coming.
	Finished,
	/// Every track reached its end. Present what is still queued, then stop.
	Ended,
	Failed(String),
}

/// Run native playback on the calling thread until the window closes.
///
/// Blocking is deliberate: winit only builds an event loop on the process main
/// thread, so this has to stay on the `#[tokio::main]` future rather than being
/// spawned. Media and transport run on tasks and talk to it through the proxy.
pub fn run(
	origin: moq_net::origin::Consumer,
	broadcast: String,
	args: Args,
	network: tokio::task::JoinSet<anyhow::Result<()>>,
) -> anyhow::Result<()> {
	let event_loop = EventLoop::<Event>::with_user_event()
		.build()
		.context("failed to create the playback event loop")?;
	let proxy = event_loop.create_proxy();
	let video = Arc::new(Mutex::new(VecDeque::new()));
	let audio_clock = Arc::new(Mutex::new(None));
	// Signals the decoder that the presenter took a frame, so it can hand over
	// the next one instead of dropping it.
	let drained = Arc::new(tokio::sync::Notify::new());

	let media = tokio::spawn(
		Media {
			origin,
			broadcast: broadcast.clone(),
			args,
			video: video.clone(),
			audio_clock: audio_clock.clone(),
			drained: drained.clone(),
			proxy: proxy.clone(),
		}
		.run(),
	);
	let network = tokio::spawn(watch_network(network, proxy.clone()));
	let signal = tokio::spawn({
		let proxy = proxy.clone();
		async move {
			if tokio::signal::ctrl_c().await.is_ok() {
				let _ = proxy.send_event(Event::Finished);
			}
		}
	});

	let title = if broadcast.is_empty() {
		"moq play".to_string()
	} else {
		format!("moq play: {broadcast}")
	};
	let mut app = App::new(title, video, audio_clock, drained);
	let result = event_loop.run_app(&mut app).context("playback event loop failed");
	media.abort();
	network.abort();
	signal.abort();
	result?;

	match app.error {
		Some(err) => anyhow::bail!(err),
		None => Ok(()),
	}
}

/// Wait for `broadcast` to be announced on `origin`, then subscribe to it.
///
/// The wait is the whole point. Subscribing goes through
/// `origin::Consumer::request_broadcast`, which resolves `Unroutable` on the
/// spot when no session has registered a handler yet rather than waiting for
/// one, and the media task starts well before the first handshake lands. The
/// window is already up, so this shows as a black frame rather than as a hang.
async fn subscribe(origin: moq_net::origin::Consumer, broadcast: &str) -> anyhow::Result<moq_mux::Source> {
	origin
		.announced_broadcast(broadcast)
		.await
		.with_context(|| format!("origin closed before broadcast `{broadcast}` was announced"))?;

	Ok(moq_mux::Source::new(origin, broadcast))
}

async fn watch_network(mut tasks: tokio::task::JoinSet<anyhow::Result<()>>, proxy: EventLoopProxy<Event>) {
	while let Some(result) = tasks.join_next().await {
		let event = match result {
			Ok(Ok(())) => Event::Finished,
			Ok(Err(err)) => Event::Failed(format!("MoQ transport failed: {err:#}")),
			Err(err) if err.is_cancelled() => continue,
			Err(err) => Event::Failed(format!("MoQ transport task failed: {err}")),
		};
		let _ = proxy.send_event(event);
		return;
	}
}

/// Everything the media task needs to fill the window and the speaker.
struct Media {
	origin: moq_net::origin::Consumer,
	broadcast: String,
	args: Args,
	video: Arc<Mutex<VecDeque<moq_video::Frame>>>,
	audio_clock: Arc<Mutex<Option<Clock>>>,
	drained: Arc<tokio::sync::Notify>,
	proxy: EventLoopProxy<Event>,
}

impl Media {
	async fn run(self) {
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

/// Which half of the pipeline a playback task drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
	Video,
	Audio,
}

/// One half of the pipeline: whether it is playing, and which catalog snapshot
/// it last picked a rendition from.
#[derive(Default)]
struct Half {
	playing: bool,
	/// The snapshot this half last read. A half reads a snapshot once, so a
	/// track that ends with nothing newer on offer stays stopped rather than
	/// resubscribing to the rendition it just finished, and doing it again every
	/// time that resubscription ends.
	read: Option<u64>,
}

/// What is playing, and whether anything else still can.
///
/// A track ending is not the end of playback. Audio and video end
/// independently, and either can end while the broadcast plays on: a publisher
/// retires a rendition (a transcode ladder resizing under a source that changed
/// resolution) by naming the replacement in a catalog snapshot and only then
/// finishing the retired track. That snapshot therefore lands while the doomed
/// track is still playing, so it is held onto and read again once the track
/// ends. Playback stops only once the catalog itself is over.
#[derive(Default)]
struct Playback {
	video: Half,
	audio: Half,
	/// The newest snapshot, held until every half has read it.
	latest: Option<catalog::hang::Catalog>,
	/// How many snapshots have arrived. A half records this rather than the
	/// catalog itself, so "newer than the one I read" costs a comparison and
	/// survives intermediate snapshots being dropped.
	snapshots: u64,
	/// Whether anything ever played, so a catalog with nothing playable in it
	/// reports why rather than exiting as a success.
	played: bool,
	/// Set once the catalog track ends, which disarms its branch (a stream that
	/// has returned `None` returns it forever, so polling it again spins) and
	/// means no replacement rendition can arrive.
	catalog_ended: bool,
}

impl Playback {
	fn half(&self, kind: Kind) -> &Half {
		match kind {
			Kind::Video => &self.video,
			Kind::Audio => &self.audio,
		}
	}

	fn half_mut(&mut self, kind: Kind) -> &mut Half {
		match kind {
			Kind::Video => &mut self.video,
			Kind::Audio => &mut self.audio,
		}
	}

	/// Hold onto a snapshot, which may not be read until a track ends.
	fn received(&mut self, snapshot: catalog::hang::Catalog) {
		self.snapshots += 1;
		self.latest = Some(snapshot);
	}

	fn started(&mut self, kind: Kind) {
		self.played = true;
		self.half_mut(kind).playing = true;
	}

	/// Record a task ending, re-arming selection for that half.
	fn ended(&mut self, kind: Option<Kind>) {
		if let Some(kind) = kind {
			self.half_mut(kind).playing = false;
		}
	}

	/// Whether this half needs a rendition and hasn't already looked for one in
	/// the snapshot on hand.
	fn wants(&self, kind: Kind) -> bool {
		let half = self.half(kind);
		!half.playing && half.read != Some(self.snapshots)
	}

	/// Record that this half read the snapshot on hand, whether or not it found
	/// anything playable in it.
	fn read(&mut self, kind: Kind) {
		let snapshots = self.snapshots;
		self.half_mut(kind).read = Some(snapshots);
	}

	/// The snapshot to pick renditions from, if either half still needs one.
	fn pending(&self) -> Option<&catalog::hang::Catalog> {
		let snapshot = self.latest.as_ref()?;
		(self.wants(Kind::Video) || self.wants(Kind::Audio)).then_some(snapshot)
	}

	/// Whether the catalog is still worth reading.
	///
	/// Deliberately blind to what is playing: the snapshot that retires a
	/// rendition arrives while both halves are still running, and it is the only
	/// warning we get.
	fn following(&self) -> bool {
		!self.catalog_ended
	}

	/// True once nothing is playing and nothing more can start.
	///
	/// A snapshot no half has read yet still can: the last thing a catalog says
	/// before it ends may be the rendition that replaces the one just retired.
	fn done(&self) -> bool {
		self.catalog_ended && !self.video.playing && !self.audio.playing && self.pending().is_none()
	}
}

/// The half a finished task was driving, or `None` if it was cancelled (on the
/// way out, where which half it was no longer matters).
fn joined(result: Result<(Kind, anyhow::Result<()>), tokio::task::JoinError>) -> anyhow::Result<Option<Kind>> {
	match result {
		Ok((kind, result)) => result.map(|()| Some(kind)),
		Err(err) if err.is_cancelled() => Ok(None),
		Err(err) => Err(err.into()),
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

fn timestamp(timestamp: hang::moq_net::Timestamp) -> Duration {
	Duration::from_micros(timestamp.as_micros().min(u64::MAX as u128) as u64)
}

#[derive(Default)]
struct AudioTimeline {
	origin: Option<Duration>,
	end: Option<Duration>,
	written: u64,
}

struct AudioTiming {
	end: Duration,
	silence: u64,
	reset_sink: bool,
}

impl AudioTimeline {
	fn push(&mut self, start: Duration, samples: usize, sample_rate: u32, fill_max: u64) -> AudioTiming {
		let duration = Duration::from_secs_f64(samples as f64 / sample_rate as f64);
		let end = start.saturating_add(duration);
		// Millisecond-stamped input can put adjacent frames on either side of their
		// exact boundary. Two output samples cover the conversions on top of that.
		let tolerance = Duration::from_millis(1).saturating_add(Duration::from_secs_f64(2.0 / sample_rate as f64));
		let rewound = self
			.end
			.is_some_and(|previous| start.saturating_add(tolerance) < previous);
		if rewound {
			self.origin = None;
			self.written = 0;
		}

		// Measure every hole from the track origin so timestamp rounding cannot
		// accumulate into drift. Advancing to `expected` even when the fill is capped
		// makes a longer hole a timeline skip rather than refilling the cap forever.
		let origin = *self.origin.get_or_insert(start);
		let expected = (start.saturating_sub(origin).as_secs_f64() * sample_rate as f64).round() as u64;
		let hole = expected.saturating_sub(self.written);
		let silence = hole.min(fill_max);
		let reset_sink = rewound || hole > fill_max;
		self.written = self
			.written
			.max(expected)
			.saturating_add(u64::try_from(samples).unwrap_or(u64::MAX));
		self.end = Some(end);

		AudioTiming {
			end,
			silence,
			reset_sink,
		}
	}
}

struct App {
	title: String,
	video: Arc<Mutex<VecDeque<moq_video::Frame>>>,
	audio_clock: Arc<Mutex<Option<Clock>>>,
	drained: Arc<tokio::sync::Notify>,
	video_clock: Option<Clock>,
	display: Option<Display>,
	next_redraw: Option<Instant>,
	/// The media tasks are done. Keep presenting whatever they left queued, then
	/// stop; exiting the moment the decoder hits EOF would cut the tail off.
	ending: bool,
	/// Consecutive presents that rebuilt the surface instead of showing a frame.
	retries: u32,
	error: Option<String>,
}

impl App {
	fn new(
		title: String,
		video: Arc<Mutex<VecDeque<moq_video::Frame>>>,
		audio_clock: Arc<Mutex<Option<Clock>>>,
		drained: Arc<tokio::sync::Notify>,
	) -> Self {
		Self {
			title,
			video,
			audio_clock,
			drained,
			video_clock: None,
			display: None,
			next_redraw: None,
			ending: false,
			retries: 0,
			error: None,
		}
	}

	fn redraw(&mut self) -> anyhow::Result<()> {
		let Some(display) = self.display.as_mut() else {
			return Ok(());
		};
		let mut video = self.video.lock().unwrap();
		let audio_clock = *self.audio_clock.lock().unwrap();

		if self.video_clock.is_none()
			&& audio_clock.is_none()
			&& let Some(frame) = video.front()
		{
			self.video_clock = Some(Clock {
				media: timestamp(frame.timestamp),
				wall: Instant::now(),
			});
		}
		let clock = audio_clock.or(self.video_clock);
		let now = clock.map(Clock::now);
		let mut due = None;
		while video.front().is_some_and(|frame| {
			now.is_none_or(|now| timestamp(frame.timestamp) <= now.saturating_add(VIDEO_EARLY_TOLERANCE))
		}) {
			due = video.pop_front();
		}
		let next_timestamp = video.front().map(|frame| timestamp(frame.timestamp));
		let popped = due.is_some();
		drop(video);

		// Room in the queue, so the decoder can hand over whatever it held back.
		if popped {
			self.drained.notify_one();
		}

		if let Some(frame) = due {
			display.render(&frame)?;
		}
		let presented = display.present()?;

		// `checked_add`: the wait comes from a wire timestamp, and adding a bogus
		// one to an `Instant` panics rather than saturating. No deadline just means
		// the next frame waits for a media wakeup instead.
		self.next_redraw = match (clock, next_timestamp) {
			(Some(clock), Some(next)) => Instant::now().checked_add(next.saturating_sub(clock.now())),
			_ => None,
		};

		// A rebuilt surface still owes us the frame we just drew, and nothing else
		// will ask for it: a stalled live stream has no next frame to trigger one,
		// and an ending stream would exit first. Bounded, so a surface that can
		// never present fails instead of looping.
		match presented {
			Presented::Shown => self.retries = 0,
			// Giving up is a dropped frame, not a failure: the next one redraws.
			// Retrying without a budget would spin, since each retry asks for the
			// redraw that produces the next one.
			Presented::Retry if self.retries >= MAX_PRESENT_RETRIES => {
				tracing::warn!("gave up re-presenting after rebuilding the graphics surface");
				self.retries = 0;
			}
			Presented::Retry => {
				self.retries += 1;
				display.window.request_redraw();
			}
		}
		Ok(())
	}

	fn fail(&mut self, event_loop: &ActiveEventLoop, err: impl ToString) {
		self.error = Some(err.to_string());
		event_loop.exit();
	}
}

impl ApplicationHandler<Event> for App {
	fn resumed(&mut self, event_loop: &ActiveEventLoop) {
		if self.display.is_some() {
			return;
		}
		match Display::new(event_loop, &self.title) {
			// Draw once up front so the window is black while the broadcast is
			// still resolving, rather than showing whatever was in the surface.
			Ok(display) => {
				display.window.request_redraw();
				self.display = Some(display);
			}
			Err(err) => self.fail(event_loop, err),
		}
	}

	fn user_event(&mut self, event_loop: &ActiveEventLoop, event: Event) {
		match event {
			Event::Wake => {
				if let Some(display) = &self.display {
					display.window.request_redraw();
				}
			}
			Event::Finished => event_loop.exit(),
			Event::Ended => {
				self.ending = true;
				if let Some(display) = &self.display {
					display.window.request_redraw();
				}
			}
			Event::Failed(err) => self.fail(event_loop, err),
		}
	}

	fn window_event(&mut self, event_loop: &ActiveEventLoop, window_id: WindowId, event: WindowEvent) {
		if self
			.display
			.as_ref()
			.is_none_or(|display| display.window.id() != window_id)
		{
			return;
		}
		match event {
			WindowEvent::CloseRequested
			| WindowEvent::KeyboardInput {
				event:
					KeyEvent {
						logical_key: Key::Named(NamedKey::Escape),
						state: ElementState::Pressed,
						..
					},
				..
			} => event_loop.exit(),
			WindowEvent::Resized(size) => {
				if let Some(display) = self.display.as_mut() {
					display.resize(size.width, size.height);
					display.window.request_redraw();
				}
			}
			WindowEvent::RedrawRequested => {
				if let Err(err) = self.redraw() {
					self.fail(event_loop, err);
				}
			}
			_ => {}
		}
	}

	fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
		// Nothing left to decode and nothing left to show. No window means nothing
		// can drain the queue, so don't wait on it.
		let drained = self.display.is_none() || self.video.lock().unwrap().is_empty();
		if self.ending && self.retries == 0 && self.next_redraw.is_none() && drained {
			event_loop.exit();
			return;
		}

		match self.next_redraw {
			Some(deadline) if deadline <= Instant::now() => {
				self.next_redraw = None;
				if let Some(display) = &self.display {
					display.window.request_redraw();
				}
			}
			Some(deadline) => event_loop.set_control_flow(ControlFlow::WaitUntil(deadline)),
			None => event_loop.set_control_flow(ControlFlow::Wait),
		}
	}
}

struct Display {
	window: Arc<Window>,
	/// Kept past setup so a lost surface can be rebuilt from it.
	instance: wgpu::Instance,
	surface: wgpu::Surface<'static>,
	device: wgpu::Device,
	queue: wgpu::Queue,
	config: wgpu::SurfaceConfiguration,
	renderer: moq_video::render::Renderer,
	presenter: Presenter,
	texture: Option<(wgpu::Texture, moq_video::Size)>,
}

impl Display {
	fn new(event_loop: &ActiveEventLoop, title: &str) -> anyhow::Result<Self> {
		let window = Arc::new(
			event_loop.create_window(
				Window::default_attributes()
					.with_title(title)
					.with_inner_size(LogicalSize::new(960, 540)),
			)?,
		);
		let size = window.inner_size();
		let instance = wgpu::Instance::default();
		let surface = instance.create_surface(window.clone())?;
		let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
			power_preference: wgpu::PowerPreference::HighPerformance,
			force_fallback_adapter: false,
			compatible_surface: Some(&surface),
			apply_limit_buckets: false,
		}))?;
		let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))?;
		let mut config = surface
			.get_default_config(&adapter, size.width.max(1), size.height.max(1))
			.context("the graphics adapter cannot present to this window")?;
		let caps = surface.get_capabilities(&adapter);
		if let Some(format) = caps.formats.iter().copied().find(|format| !format.is_srgb()) {
			config.format = format;
		}
		config.desired_maximum_frame_latency = 1;
		surface.configure(&device, &config);
		let renderer = moq_video::render::Renderer::new(&device, &queue, Default::default())?;
		let presenter = Presenter::new(&device, config.format);

		Ok(Self {
			window,
			instance,
			surface,
			device,
			queue,
			config,
			renderer,
			presenter,
			texture: None,
		})
	}

	fn resize(&mut self, width: u32, height: u32) {
		if width == 0 || height == 0 {
			return;
		}
		self.config.width = width;
		self.config.height = height;
		self.surface.configure(&self.device, &self.config);
	}

	fn render(&mut self, frame: &moq_video::Frame) -> anyhow::Result<()> {
		self.texture = Some((self.renderer.render(frame)?, frame.size()));
		Ok(())
	}

	fn present(&mut self) -> anyhow::Result<Presented> {
		use wgpu::CurrentSurfaceTexture;

		let (output, reconfigure) = match self.surface.get_current_texture() {
			CurrentSurfaceTexture::Success(output) => (output, false),
			CurrentSurfaceTexture::Suboptimal(output) => (output, true),
			// The swapchain was busy, not broken. Ask again rather than waiting on a
			// next frame that a stalled or ending stream may never produce.
			CurrentSurfaceTexture::Timeout => return Ok(Presented::Retry),
			// Nobody is looking, so there is nothing to retry for. Being shown again
			// is itself a redraw.
			CurrentSurfaceTexture::Occluded => return Ok(Presented::Shown),
			CurrentSurfaceTexture::Outdated => {
				self.surface.configure(&self.device, &self.config);
				return Ok(Presented::Retry);
			}
			// Not just a stale configuration like `Outdated`: the surface itself is
			// gone and has to be rebuilt before it can be configured again. A
			// display change or a resume can do this, so it isn't fatal.
			CurrentSurfaceTexture::Lost => {
				self.surface = self
					.instance
					.create_surface(self.window.clone())
					.context("failed to rebuild the playback window's graphics surface")?;
				self.surface.configure(&self.device, &self.config);
				return Ok(Presented::Retry);
			}
			CurrentSurfaceTexture::Validation => anyhow::bail!("failed to acquire the playback window's next frame"),
		};
		let target = output.texture.create_view(&Default::default());
		let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
			label: Some("moq play present"),
		});
		let viewport = self
			.texture
			.as_ref()
			.map(|(_, size)| fit((self.config.width, self.config.height), (size.width, size.height)));
		self.presenter.draw(
			&self.device,
			&mut encoder,
			self.texture.as_ref().map(|(texture, _)| texture),
			&target,
			viewport,
		);
		self.queue.submit([encoder.finish()]);
		self.queue.present(output);
		if reconfigure {
			self.surface.configure(&self.device, &self.config);
		}
		Ok(Presented::Shown)
	}
}

/// Whether a present reached the screen, or recovered the surface and still owes
/// the caller a redraw.
#[derive(Clone, Copy)]
enum Presented {
	Shown,
	Retry,
}

struct Presenter {
	pipeline: wgpu::RenderPipeline,
	layout: wgpu::BindGroupLayout,
	sampler: wgpu::Sampler,
}

impl Presenter {
	fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
		let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
			label: Some("moq play texture layout"),
			entries: &[
				wgpu::BindGroupLayoutEntry {
					binding: 0,
					visibility: wgpu::ShaderStages::FRAGMENT,
					ty: wgpu::BindingType::Texture {
						sample_type: wgpu::TextureSampleType::Float { filterable: true },
						view_dimension: wgpu::TextureViewDimension::D2,
						multisampled: false,
					},
					count: None,
				},
				wgpu::BindGroupLayoutEntry {
					binding: 1,
					visibility: wgpu::ShaderStages::FRAGMENT,
					ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
					count: None,
				},
			],
		});
		let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
			label: Some("moq play pipeline layout"),
			bind_group_layouts: &[Some(&layout)],
			immediate_size: 0,
		});
		let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
			label: Some("moq play shader"),
			source: wgpu::ShaderSource::Wgsl(
				"struct VertexOutput {\n\
				   @builtin(position) position: vec4f,\n\
				   @location(0) tex_coords: vec2f,\n\
				 }\n\
				 @group(0) @binding(0) var image: texture_2d<f32>;\n\
				 @group(0) @binding(1) var image_sampler: sampler;\n\
				 @vertex fn vs_main(@builtin(vertex_index) i: u32) -> VertexOutput {\n\
				   var out: VertexOutput;\n\
				   out.tex_coords = vec2f(f32((i << 1u) & 2u), f32(i & 2u));\n\
				   out.position = vec4f(out.tex_coords * 2.0 - 1.0, 0.0, 1.0);\n\
				   out.tex_coords.y = 1.0 - out.tex_coords.y;\n\
				   return out;\n\
				 }\n\
				 @fragment fn fs_main(in: VertexOutput) -> @location(0) vec4f {\n\
				   return textureSample(image, image_sampler, in.tex_coords);\n\
				 }"
				.into(),
			),
		});
		let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
			label: Some("moq play pipeline"),
			layout: Some(&pipeline_layout),
			vertex: wgpu::VertexState {
				module: &shader,
				entry_point: Some("vs_main"),
				compilation_options: Default::default(),
				buffers: &[],
			},
			primitive: Default::default(),
			depth_stencil: None,
			multisample: Default::default(),
			fragment: Some(wgpu::FragmentState {
				module: &shader,
				entry_point: Some("fs_main"),
				compilation_options: Default::default(),
				targets: &[Some(wgpu::ColorTargetState {
					format,
					blend: None,
					write_mask: wgpu::ColorWrites::ALL,
				})],
			}),
			multiview_mask: None,
			cache: None,
		});
		let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
			label: Some("moq play sampler"),
			mag_filter: wgpu::FilterMode::Linear,
			min_filter: wgpu::FilterMode::Linear,
			..Default::default()
		});
		Self {
			pipeline,
			layout,
			sampler,
		}
	}

	fn draw(
		&self,
		device: &wgpu::Device,
		encoder: &mut wgpu::CommandEncoder,
		source: Option<&wgpu::Texture>,
		target: &wgpu::TextureView,
		viewport: Option<(f32, f32, f32, f32)>,
	) {
		let bind = source.map(|texture| {
			let view = texture.create_view(&Default::default());
			device.create_bind_group(&wgpu::BindGroupDescriptor {
				label: Some("moq play texture"),
				layout: &self.layout,
				entries: &[
					wgpu::BindGroupEntry {
						binding: 0,
						resource: wgpu::BindingResource::TextureView(&view),
					},
					wgpu::BindGroupEntry {
						binding: 1,
						resource: wgpu::BindingResource::Sampler(&self.sampler),
					},
				],
			})
		});
		let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
			label: Some("moq play present"),
			color_attachments: &[Some(wgpu::RenderPassColorAttachment {
				view: target,
				depth_slice: None,
				resolve_target: None,
				ops: wgpu::Operations {
					load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
					store: wgpu::StoreOp::Store,
				},
			})],
			depth_stencil_attachment: None,
			timestamp_writes: None,
			occlusion_query_set: None,
			multiview_mask: None,
		});
		if let (Some(bind), Some((x, y, width, height))) = (bind.as_ref(), viewport) {
			pass.set_viewport(x, y, width, height, 0.0, 1.0);
			pass.set_pipeline(&self.pipeline);
			pass.set_bind_group(0, bind, &[]);
			pass.draw(0..3, 0..1);
		}
	}
}

fn fit(window: (u32, u32), video: (u32, u32)) -> (f32, f32, f32, f32) {
	let scale = (window.0 as f32 / video.0 as f32).min(window.1 as f32 / video.1 as f32);
	let width = video.0 as f32 * scale;
	let height = video.1 as f32 * scale;
	(
		(window.0 as f32 - width) / 2.0,
		(window.1 as f32 - height) / 2.0,
		width,
		height,
	)
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Subscribing before the announcement lands doesn't wait, it fails: with no
	/// session yet there is no handler registered on the origin, and
	/// `request_broadcast` resolves `Unroutable` immediately. The media task is
	/// spawned right after the reconnect loop starts, so it gets there first.
	#[tokio::test]
	async fn subscribe_waits_for_the_announcement() {
		tokio::time::pause();

		let origin = moq_net::Origin::random().produce();
		let consumer = origin.consume();

		// Resolving straight away, which is what the media task used to do.
		let unannounced = moq_mux::Source::new(consumer.clone(), "room.hang").broadcast().await;
		assert!(unannounced.is_err(), "expected an unroutable broadcast");

		// Waiting first parks instead, for as long as it takes.
		let mut waiting = std::pin::pin!(subscribe(consumer, "room.hang"));
		let parked = tokio::time::timeout(Duration::from_secs(60), &mut waiting).await;
		assert!(parked.is_err(), "expected to still be waiting on the announcement");

		let _broadcast = origin
			.create_broadcast("room.hang", moq_net::broadcast::Route::new().with_announce(true))
			.unwrap();
		waiting.await.unwrap();
	}

	/// A publisher retiring a rendition (a transcode ladder resizing under a
	/// source that changed resolution) names the replacement in a catalog
	/// snapshot and only then finishes the retired track, at the end of the group
	/// it was mid-way through. So the snapshot lands while both halves are still
	/// playing and has to be kept: it is the only warning the player gets, and
	/// the half whose track ends afterwards reads it then.
	#[test]
	fn a_finished_track_re_arms_its_half() {
		let mut playback = Playback::default();
		playback.received(Default::default());
		playback.started(Kind::Video);
		playback.read(Kind::Video);
		playback.started(Kind::Audio);
		playback.read(Kind::Audio);

		// The snapshot naming the replacement, while the retired track plays on.
		playback.received(Default::default());
		assert!(playback.pending().is_none(), "nothing to start while both halves play");

		playback.ended(Some(Kind::Video));
		assert!(!playback.done(), "playback ended on a retired rendition");
		assert!(playback.audio.playing, "audio ended with the video rendition");
		assert!(playback.pending().is_some(), "the retirement snapshot was dropped");
		assert!(playback.wants(Kind::Video), "the replacement was never looked for");
		assert!(!playback.wants(Kind::Audio), "audio is still playing its own rendition");

		// The replacement lands and playback carries on.
		playback.read(Kind::Video);
		playback.started(Kind::Video);
		assert!(playback.pending().is_none());
		assert!(!playback.done());
	}

	/// A track that ends with nothing newer on offer stays stopped. Reading the
	/// snapshot it was selected from again would resubscribe to the rendition
	/// that just finished, and do it again the moment that ended too.
	#[test]
	fn a_read_snapshot_is_not_read_twice() {
		let mut playback = Playback::default();
		playback.received(Default::default());
		// A video-only catalog: both halves read the snapshot, only one found
		// something in it.
		playback.read(Kind::Video);
		playback.started(Kind::Video);
		playback.read(Kind::Audio);

		playback.ended(Some(Kind::Video));
		assert!(playback.pending().is_none(), "the player would resubscribe in a loop");

		playback.received(Default::default());
		assert!(playback.pending().is_some(), "a fresh snapshot must be read");
	}

	/// The catalog track ending is what ends playback, since it is the only thing
	/// that rules out a replacement rendition. Tracks ending before it just stop
	/// their own half.
	#[test]
	fn playback_ends_with_the_catalog() {
		let mut playback = Playback::default();
		playback.started(Kind::Video);

		playback.catalog_ended = true;
		assert!(!playback.done(), "playback ended while video was still playing");

		playback.ended(Some(Kind::Video));
		assert!(playback.done());
	}

	/// The catalog's last word can be the replacement for the rendition it
	/// retires, and the retired track outlives the catalog by the group it was
	/// mid-way through. So a half that has not read the final snapshot yet is
	/// still a half that can start something, however finished everything else
	/// looks.
	#[test]
	fn a_final_snapshot_outlives_the_catalog() {
		let mut playback = Playback::default();
		playback.received(Default::default());
		playback.started(Kind::Video);
		playback.read(Kind::Video);

		// The last snapshot names the replacement, then the catalog ends, then the
		// retired track does.
		playback.received(Default::default());
		playback.catalog_ended = true;
		playback.ended(Some(Kind::Video));

		assert!(playback.pending().is_some(), "the final snapshot was never offered");
		assert!(!playback.done(), "playback ended with a replacement still unread");

		// Read it for both halves, the way the selection pass does, find nothing
		// playable in it, and only then stop.
		playback.read(Kind::Video);
		playback.read(Kind::Audio);
		assert!(playback.done());
	}

	#[test]
	fn letterboxes_without_changing_aspect_ratio() {
		let assert_near = |actual: (f32, f32, f32, f32), expected: (f32, f32, f32, f32)| {
			for (actual, expected) in [actual.0, actual.1, actual.2, actual.3]
				.into_iter()
				.zip([expected.0, expected.1, expected.2, expected.3])
			{
				assert!((actual - expected).abs() < 0.01, "{actual} != {expected}");
			}
		};
		assert_near(fit((1000, 1000), (1920, 1080)), (0.0, 218.75, 1000.0, 562.5));
		assert_near(fit((1920, 1080), (1000, 1000)), (420.0, 0.0, 1080.0, 1080.0));
	}

	#[test]
	fn clock_advances_from_its_media_anchor() {
		let clock = Clock {
			media: Duration::from_secs(10),
			wall: Instant::now() - Duration::from_millis(20),
		};
		assert!(clock.now() >= Duration::from_millis(10_020));
	}

	#[test]
	fn audio_timeline_restarts_when_media_time_rewinds() {
		let mut timeline = AudioTimeline::default();
		let first = timeline.push(Duration::from_secs(10), 960, 48_000, 24_000);
		assert!(!first.reset_sink);

		let rewound = timeline.push(Duration::from_secs(5), 960, 48_000, 24_000);
		assert!(rewound.reset_sink);
		assert_eq!(rewound.silence, 0);

		let next = timeline.push(Duration::from_millis(5_020), 960, 48_000, 24_000);
		assert!(!next.reset_sink);
		assert_eq!(next.silence, 0);
	}

	#[test]
	fn audio_timeline_tolerates_millisecond_stamp_rounding() {
		let mut timeline = AudioTimeline::default();
		let first = timeline.push(Duration::ZERO, 1024, 44_100, 22_050);
		assert!(!first.reset_sink);

		// 1024 frames end at 23.22 ms, but an FLV timestamp carries 23 ms.
		let rounded = timeline.push(Duration::from_millis(23), 1024, 44_100, 22_050);
		assert!(!rounded.reset_sink);
	}

	#[test]
	fn audio_timeline_resets_sink_when_forward_hole_exceeds_fill_cap() {
		let mut timeline = AudioTimeline::default();
		timeline.push(Duration::ZERO, 960, 48_000, 4_800);

		let filled = timeline.push(Duration::from_millis(100), 960, 48_000, 4_800);
		assert!(!filled.reset_sink);
		assert_eq!(filled.silence, 3_840);

		let skipped = timeline.push(Duration::from_secs(1), 960, 48_000, 4_800);
		assert!(skipped.reset_sink);
		assert_eq!(skipped.silence, 4_800);
	}
}
