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
	#[arg(long = "latency-max", default_value = "500ms", value_parser = humantime::parse_duration)]
	pub max_age: Duration,

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
		use crate::subscribe::{AudioCodecArg, VideoCodecArg};

		anyhow::ensure!(
			!matches!(self.select.video_codec, Some(VideoCodecArg::Vp8 | VideoCodecArg::Vp9)),
			"`play` cannot decode vp8 or vp9; pass --video-codec h264, h265, or av1"
		);
		anyhow::ensure!(
			!matches!(self.select.audio_codec, Some(AudioCodecArg::Aac)),
			"`play` cannot decode aac; pass --audio-codec opus or pcm"
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
		let mut video_started = false;
		let mut audio_started = false;
		// Set once the catalog track ends, which disarms the branch below: a stream
		// that has returned `None` returns it forever, so polling it again spins.
		let mut catalog_ended = false;

		loop {
			// Audio and video end independently, so one track finishing is not the end
			// of playback. Stop once every track that started has ended.
			if tasks.is_empty() && (video_started || audio_started) {
				return Ok(());
			}

			tokio::select! {
				result = tasks.join_next(), if !tasks.is_empty() => {
					joined(result.expect("guarded by is_empty"))?;
				}
				snapshot = catalogs.next(), if !catalog_ended && (!video_started || !audio_started) => {
					let Some(snapshot) = snapshot.context("failed to read the catalog")? else {
						anyhow::ensure!(video_started || audio_started, "the catalog contains no playable audio or video renditions");
						catalog_ended = true;
						continue;
					};

					// Why nothing started, so a catalog this build can't play reports the
					// reason instead of leaving a blank window up forever. The decoders are
					// gated by platform and cargo feature (no AV1 without nvdec, say), so
					// this covers gaps the codec flags can't be validated against up front.
					let mut rejected = Vec::new();

					if !video_started {
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
							decode.max_age = self.args.max_age;
							match moq_video::decode::Consumer::new(&rendition, &config, &name, decode).await {
								Ok(consumer) => {
									tracing::info!(track = name, decoder = consumer.name(), "playing video rendition");
									tasks.spawn(play_video(
										consumer,
										self.video.clone(),
										self.drained.clone(),
										self.proxy.clone(),
									));
									video_started = true;
									break;
								}
								Err(err) => {
									tracing::warn!(track = name, %err, "cannot play video rendition");
									rejected.push(format!("video `{name}`: {err}"));
								}
							}
						}
					}

					if !audio_started {
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
							decode.max_age = self.args.max_age;
							// The sink and the frame-duration math below both assume f32,
							// so ask for it rather than inheriting the decoder default.
							decode.format = moq_audio::Format::F32;
							match moq_audio::decode::Consumer::new(&rendition, &config, &name, decode).await {
								Ok(consumer) => {
									tracing::info!(track = name, "playing audio rendition");
									tasks.spawn(play_audio(consumer, self.audio_clock.clone(), self.proxy.clone()));
									audio_started = true;
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
	}
}

fn joined(result: Result<anyhow::Result<()>, tokio::task::JoinError>) -> anyhow::Result<()> {
	match result {
		Ok(result) => result,
		Err(err) if err.is_cancelled() => Ok(()),
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
	let mut sink = engine.sink(moq_audio::playback::Input {
		format: moq_audio::Format::F32,
		sample_rate,
		channels,
	})?;

	// One sample across every channel, the unit a write has to stay aligned to.
	let stride = channels as usize * size_of::<f32>();
	// A second per write, so a frame longer than the sink can hold is paced in
	// rather than handed over whole and truncated. An Opus packet caps at 120 ms,
	// but a PCM one is only required to be sample-aligned, so it can be any length.
	// Paired with the wait below this keeps the sink under two seconds, inside its
	// own ceiling.
	let chunk = (sample_rate as usize * stride).max(stride);

	while let Some(frame) = consumer.read().await? {
		let samples = frame.data.len() / size_of::<f32>() / channels as usize;
		let end =
			timestamp(frame.timestamp).saturating_add(Duration::from_secs_f64(samples as f64 / sample_rate as f64));

		for part in frame.data.chunks(chunk) {
			// Let the speaker catch up before handing it more than it can hold.
			if let Some(excess) = sink.buffered().checked_sub(AUDIO_BUFFER_MAX) {
				tokio::time::sleep(excess).await;
			}
			sink.write(part)?;
		}

		let previous = clock.lock().unwrap().replace(Clock {
			media: end.saturating_sub(sink.buffered()),
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

		let origin = moq_tokio::origin::spawn(moq_net::Origin::random());
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
}
