//! Everything behind the window: the GPU, the sources, the program clock that
//! draws the canvas and mixes the audio, and the broadcast it publishes.
//!
//! Threads:
//! - **program** draws preview and program once per canvas frame and hands the
//!   program to the encoder, stamped with the program clock.
//! - **audio** reads the program bus on the same clock and publishes it.
//! - **encode** waits for each program copy and encodes it.
//! - **source-***: one per running source, since a capture stream may not
//!   leave the thread that opened it.
//! - The UI runs on the main thread and edits [`State`] under its lock.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use anyhow::Context;
use bytes::Bytes;
use moq_clock::Instant;
use moq_scene::source::{self, Driver, Sources};
use moq_scene::{Collection, Compositor, wgpu};
use moq_video::render::export::{Exporter, Pending, Route};

/// The GPU everything shares: sources upload to it, the compositor draws on
/// it, the window presents from it, and the encoder reads the program from it.
#[derive(Clone)]
pub struct Gpu {
	pub instance: wgpu::Instance,
	pub adapter: wgpu::Adapter,
	pub device: wgpu::Device,
	pub queue: wgpu::Queue,
}

impl Gpu {
	pub async fn new() -> anyhow::Result<Self> {
		let instance = wgpu::Instance::default();
		let adapter = instance
			.request_adapter(&wgpu::RequestAdapterOptions {
				power_preference: wgpu::PowerPreference::HighPerformance,
				..Default::default()
			})
			.await
			.context("no GPU adapter")?;
		// Lets a hardware-decoded or captured frame reach the compositor, and the
		// program reach the encoder, without a copy where the driver allows.
		let descriptor = wgpu::DeviceDescriptor {
			label: Some("moq-studio"),
			..Default::default()
		};
		let (device, queue) = moq_video::render::request_device(&adapter, &descriptor)
			.await
			.context("cannot open the GPU")?;
		Ok(Self {
			instance,
			adapter,
			device,
			queue,
		})
	}
}

/// What the UI edits and the program clock draws.
pub struct State {
	/// The scene file, saved to and reloaded from.
	pub path: PathBuf,
	pub collection: Collection,
	pub sources: Sources,
	pub compositor: Compositor,
	/// Edited since the sources were last synced.
	stale: bool,
	/// Edited since the last save or load.
	unsaved: bool,
}

impl State {
	/// The collection, for an edit: the sources follow it on the next frame.
	pub fn edit(&mut self) -> &mut Collection {
		self.stale = true;
		self.unsaved = true;
		&mut self.collection
	}

	pub fn unsaved(&self) -> bool {
		self.unsaved
	}

	pub fn save(&mut self) -> anyhow::Result<()> {
		let text = self.collection.to_toml()?;
		std::fs::write(&self.path, text).with_context(|| format!("writing {}", self.path.display()))?;
		self.unsaved = false;
		Ok(())
	}

	/// Replace the collection with what the scene file holds now.
	pub fn reload(&mut self) -> anyhow::Result<()> {
		let collection = load(&self.path)?;
		anyhow::ensure!(
			collection.canvas == self.collection.canvas,
			"the canvas changed; restart the studio to use {}x{} at {} fps",
			collection.canvas.width,
			collection.canvas.height,
			collection.canvas.framerate
		);
		// A scene in preview or on program that the file no longer has draws
		// black until another is picked.
		self.collection = collection;
		self.stale = true;
		self.unsaved = false;
		Ok(())
	}
}

/// Read a scene file, or start a new collection if there is none yet.
pub fn load(path: &Path) -> anyhow::Result<Collection> {
	match std::fs::read_to_string(path) {
		Ok(text) => Collection::from_toml(&text).with_context(|| format!("loading {}", path.display())),
		Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Collection::new()),
		Err(err) => Err(err).with_context(|| format!("reading {}", path.display())),
	}
}

/// How the output is doing, for the stream panel.
#[derive(Default)]
pub struct Stats {
	/// Canvas frames drawn.
	pub rendered: AtomicU64,
	/// Program frames handed to the encoder.
	pub copied: AtomicU64,
	/// Microseconds the last one waited on the GPU, and on a readback converted
	/// pixels.
	pub copy_us: AtomicU64,
	/// Whether the encoder reads the program on the GPU, rather than each frame
	/// being read back to the CPU.
	pub zero_copy: AtomicBool,
	/// Program frames read back to the CPU for the encoder.
	pub read_back: AtomicU64,
	/// Program frames not encoded because the encoder still held every copy.
	pub dropped: AtomicU64,
	/// Program frames encoded.
	pub encoded: AtomicU64,
	/// Microseconds the last encode took.
	pub encode_us: AtomicU64,
	/// Whether any viewer is subscribed to the video.
	pub watched: AtomicBool,
	/// The encoder backend, once one opened.
	pub encoder: Mutex<Option<String>>,
	/// Why the output stopped, if it did.
	pub error: Mutex<Option<String>>,
}

impl Stats {
	fn fail(&self, err: impl std::fmt::Display) {
		tracing::error!(%err, "output stopped");
		*self.error.lock().unwrap() = Some(err.to_string());
	}
}

/// Where the program goes.
pub struct Output {
	/// The broadcast path on the relay.
	pub path: String,
	/// The relay connection, which reconnects on its own.
	pub connection: moq_tokio::Connection,
	publication: moq_net::origin::Publication,
}

impl Drop for Output {
	fn drop(&mut self) {
		self.publication.close();
	}
}

/// The running studio.
pub struct Engine {
	pub state: Mutex<State>,
	pub stats: Stats,
	pub bus: moq_audio::bus::Bus,
	/// `None` when running without a relay.
	pub output: Option<Output>,
	/// Keeps broadcast sources' origin open when no connection holds it.
	_origin: Option<moq_net::origin::Producer>,
}

/// The audio the program publishes.
const AUDIO_BITRATE: moq_net::bandwidth::Rate = moq_net::bandwidth::Rate::from_kbps(128);

/// How often the audio thread reads the bus. Short enough that a read is never
/// close to the bus's buffering, long enough to cost nothing.
const AUDIO_PERIOD: Duration = Duration::from_millis(10);

impl Engine {
	/// Open the collection at `path` on `gpu` and start the program clock.
	/// With `connect`, publish the program at `broadcast` on that relay.
	///
	/// Call inside the tokio runtime that serves the relay connection.
	pub async fn start(
		gpu: Gpu,
		path: PathBuf,
		collection: Collection,
		connect: Option<(moq_tokio::Client, url::Url)>,
		broadcast: &str,
	) -> anyhow::Result<Arc<Self>> {
		let bus = moq_audio::bus::Bus::new(Default::default())?;
		let subscribe = moq_tokio::origin::spawn();
		let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
		let sources = Sources::new(source::Context {
			bus: bus.clone(),
			origin: subscribe.consume(),
			dir,
			output: moq_video::render::output(&gpu.device),
		});
		let mut compositor = Compositor::new(&gpu.device, &gpu.queue, collection.canvas)?;
		compositor.set_preview(collection.scenes.first().map(|scene| scene.name.clone()));
		compositor.cut();
		let canvas = collection.canvas;

		let (output, origin, publish) = match connect {
			Some((client, url)) => {
				let (output, publish) = publish(&gpu, client, url, subscribe, broadcast, canvas).await?;
				(Some(output), None, Some(publish))
			}
			None => (None, Some(subscribe), None),
		};

		let engine = Arc::new(Self {
			state: Mutex::new(State {
				path,
				collection,
				sources,
				compositor,
				stale: true,
				unsaved: false,
			}),
			stats: Stats::default(),
			bus,
			output,
			_origin: origin,
		});

		let (video, audio, clock) = match publish {
			Some(Publish {
				video,
				audio,
				config,
				exporter,
				clock,
			}) => {
				let zero_copy = exporter.route() == Route::Gpu;
				engine.stats.zero_copy.store(zero_copy, Ordering::Relaxed);
				// One more than the exporter's pool, for a pause behind a full pool.
				let (frames, pending) = mpsc::sync_channel(Exporter::DEPTH + 1);
				tokio::spawn(watch(video.demand(), engine.clone()));
				let encoder = engine.clone();
				std::thread::Builder::new()
					.name("encode".into())
					.spawn(move || encode(&encoder, pending, video, config, zero_copy))?;
				(Some((frames, exporter)), Some(audio), Some(clock))
			}
			None => (None, None, None),
		};

		let start = Instant::now();
		// Where the program clock starts on the catalog's, so the wall time the
		// catalog advertises for a frame is when the program drew it.
		let epoch = match clock {
			Some(clock) => Duration::from(clock.capture(start)?),
			None => Duration::ZERO,
		};
		let program = engine.clone();
		std::thread::Builder::new()
			.name("program".into())
			.spawn(move || clocked(program.run_program(start, epoch, video)))?;
		let mix = engine.clone();
		std::thread::Builder::new()
			.name("audio".into())
			.spawn(move || clocked(mix.run_audio(start, epoch, audio)))?;

		Ok(engine)
	}

	/// Draw a canvas frame per tick of the program clock, and hand the program
	/// to the encoder while anyone watches, stamped `epoch` past `start`.
	///
	/// Drawing outlives the encoder: a failed output stops copying the program,
	/// and the preview and program keep moving.
	async fn run_program(
		&self,
		start: Instant,
		epoch: Duration,
		mut video: Option<(mpsc::SyncSender<Video>, Exporter)>,
	) {
		let canvas = self.state.lock().unwrap().collection.canvas;
		let period = Duration::from_secs(1) / canvas.framerate;
		let mut clock = moq_clock::interval_at(start, period);
		clock.set_missed_tick_behavior(moq_clock::MissedTickBehavior::Skip);
		let mut watched = false;

		loop {
			let tick = clock.tick().await;
			let program = {
				let mut guard = self.state.lock().unwrap();
				let state = &mut *guard;
				if std::mem::take(&mut state.stale) {
					for driver in state.sources.sync(&state.collection) {
						spawn(driver);
					}
				}
				if let Err(err) = state.compositor.render(&state.collection, &state.sources, tick) {
					tracing::error!(%err, "cannot draw the canvas");
					continue;
				}
				state.compositor.program().clone()
			};
			self.stats.rendered.fetch_add(1, Ordering::Relaxed);

			let Some((sender, exporter)) = &mut video else { continue };
			let was = std::mem::replace(&mut watched, self.stats.watched.load(Ordering::Relaxed));
			if !watched {
				// Behind every frame already sent, so the break lands after them.
				// A send fails only once the encoder stopped, having reported why.
				if was && sender.send(Video::Pause).is_err() {
					video = None;
				}
				continue;
			}
			let Ok(timestamp) = moq_net::Timestamp::try_from(epoch + tick.saturating_duration_since(start)) else {
				continue;
			};
			match exporter.copy(&program, timestamp) {
				// The pool bounds the frames in flight, so this waits only behind
				// a pause the encoder has not reached yet.
				Ok(Some(pending)) => {
					if sender.send(Video::Frame(pending)).is_err() {
						video = None;
					}
				}
				Ok(None) => {
					self.stats.dropped.fetch_add(1, Ordering::Relaxed);
				}
				// The canvas and the exporter share a size and format, so this is
				// the GPU failing: the video stops, and the canvas keeps drawing.
				Err(err) => {
					let _ = sender.send(Video::Failed(err));
					video = None;
				}
			}
		}
	}

	/// Read the program bus as the program clock advances, and publish it
	/// stamped `epoch` past `start`.
	///
	/// Runs without a relay too, so the meters move and sources never back up.
	async fn run_audio(&self, start: Instant, epoch: Duration, mut producer: Option<moq_audio::encode::Producer>) {
		let config = self.bus.config().clone();
		let rate = u128::from(config.sample_rate);
		let channels = config.layout.channels() as usize;
		let mut clock = moq_clock::interval_at(start, AUDIO_PERIOD);
		clock.set_missed_tick_behavior(moq_clock::MissedTickBehavior::Skip);
		let mut samples = Vec::new();
		let mut written: u128 = 0;

		loop {
			let tick = clock.tick().await;
			// Sample-exact against the program clock, so audio and video stay
			// aligned however the ticks jitter.
			let due = tick.saturating_duration_since(start).as_nanos() * rate / 1_000_000_000;
			let frames = (due - written) as usize;
			if frames == 0 {
				continue;
			}
			samples.resize(frames * channels, 0.0);
			self.bus.read(&mut samples);
			let timestamp = moq_net::Timestamp::from_micros((epoch.as_micros() + written * 1_000_000 / rate) as u64);
			written = due;

			let Some(publish) = producer.as_mut() else { continue };
			let Ok(timestamp) = timestamp else { continue };
			let data: Vec<u8> = samples.iter().flat_map(|sample| sample.to_le_bytes()).collect();
			if let Err(err) = publish.write(&moq_audio::Frame::new(Bytes::from(data), timestamp)) {
				self.stats.fail(format!("audio: {err}"));
				// Viewers see the audio fail rather than go away unfinished.
				if let Some(producer) = producer.take() {
					producer.abort(moq_net::StreamError::Internal.into());
				}
			}
		}
	}
}

/// The encoders behind an [`Output`], handed to their threads.
struct Publish {
	video: moq_video::encode::Producer,
	config: moq_video::encode::Config,
	exporter: Exporter,
	audio: moq_audio::encode::Producer,
	/// The catalog's clock, which the codec producers write timestamps
	/// against verbatim.
	clock: moq_mux::Clock,
}

/// Announce the program broadcast and connect to the relay, which also
/// carries `subscribe`'s broadcast sources.
async fn publish(
	gpu: &Gpu,
	client: moq_tokio::Client,
	url: url::Url,
	subscribe: moq_net::origin::Producer,
	path: &str,
	canvas: moq_scene::Canvas,
) -> anyhow::Result<(Output, Publish)> {
	let origin = moq_tokio::origin::spawn();
	let publication = origin.create_broadcast(path).context("cannot create the broadcast")?;
	let mut broadcast = publication.broadcast().clone();
	let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default())?;
	let clock = catalog.clock();

	let mut config = moq_video::encode::Config::new(canvas.width, canvas.height, canvas.framerate());
	// Opens a throwaway encoder now, so a machine that cannot encode the
	// canvas fails at startup rather than when the first viewer arrives. It
	// also settles whether that encoder reads the program on the GPU.
	let (exporter, rendition) = Exporter::new(&gpu.device, &gpu.queue, &mut config)
		.await
		.context("cannot encode the canvas")?;
	let reserved = catalog.reserve().with_hls_hint(config.gop.duration(config.framerate));
	let video = moq_video::encode::Producer::new(broadcast.clone(), reserved, rendition)?;

	let bus = moq_audio::bus::Config::default();
	let mut input = moq_audio::encode::Input::new(bus.sample_rate, bus.layout);
	input.format = moq_audio::Format::F32;
	let mut options = moq_audio::encode::Options::default();
	options.track = Some("audio".into());
	options.config = moq_audio::encode::Config::from_input(moq_audio::encode::Codec::Opus, &input);
	options.config.bitrate = Some(AUDIO_BITRATE);
	let audio = moq_audio::encode::Producer::new(&mut broadcast, catalog, input, &options)?;

	publication
		.announce(moq_net::origin::Route::default().with_epoch(moq_net::Epoch::mint()))
		.context("cannot announce the broadcast")?;
	tracing::info!(url = %moq_tokio::RedactedUrl::new(&url), broadcast = %path, "connecting");
	let connection = client.with_publisher(&origin).with_subscriber(subscribe).connect(url);

	let output = Output {
		path: path.to_string(),
		connection,
		publication,
	};
	Ok((
		output,
		Publish {
			video,
			config,
			exporter,
			audio,
			clock,
		},
	))
}

/// Track whether anyone watches the video, so the program is only copied and
/// encoded while someone does.
async fn watch(mut demand: moq_net::track::Demand, engine: Arc<Engine>) {
	loop {
		match demand.wanted_changed().await {
			Ok(wanted) => engine.stats.watched.store(wanted.is_some(), Ordering::Relaxed),
			Err(err) => {
				engine.stats.watched.store(false, Ordering::Relaxed);
				engine.stats.fail(format!("video: {err}"));
				return;
			}
		}
	}
}

/// What the program clock hands the encoder.
enum Video {
	/// A copy of the program on its way to the encoder.
	Frame(Pending),
	/// The last viewer left, so frames stop until one returns.
	Pause,
	/// The program could not be copied, so the video stops.
	Failed(moq_video::Error),
}

/// Encode program frames until the program clock stops.
///
/// A failure aborts the video track, so viewers see that it failed rather
/// than a track that went away unfinished.
fn encode(
	engine: &Engine,
	video: mpsc::Receiver<Video>,
	mut producer: moq_video::encode::Producer,
	config: moq_video::encode::Config,
	zero_copy: bool,
) {
	let result = encode_frames(&engine.stats, video, &mut producer, &config, zero_copy).and_then(|()| {
		producer.finish()?;
		Ok(())
	});
	if let Err(err) = result {
		engine.stats.fail(format!("{err:#}"));
		producer.abort(moq_net::StreamError::Internal.into());
	}
}

fn encode_frames(
	stats: &Stats,
	video: mpsc::Receiver<Video>,
	producer: &mut moq_video::encode::Producer,
	config: &moq_video::encode::Config,
	zero_copy: bool,
) -> anyhow::Result<()> {
	let mut encoder: Option<moq_video::encode::Encoder> = None;
	let mut paused = false;

	while let Ok(video) = video.recv() {
		let pending = match video {
			Video::Frame(pending) => pending,
			// Marks the gap, so a viewer arriving during it is not served the
			// last group before it as if it were live.
			Video::Pause => {
				producer.discontinuity().context("video")?;
				paused = true;
				continue;
			}
			Video::Failed(err) => return Err(err).context("copying the program"),
		};
		let start = Instant::now();
		let frame = pending.wait().context("copying the program")?;
		stats
			.copy_us
			.store(start.elapsed().as_micros() as u64, Ordering::Relaxed);
		stats.copied.fetch_add(1, Ordering::Relaxed);
		if !zero_copy {
			stats.read_back.fetch_add(1, Ordering::Relaxed);
		}

		let encoder = match &mut encoder {
			Some(encoder) => encoder,
			None => {
				let opened = moq_video::encode::Encoder::new(config).context("video encoder")?;
				*stats.encoder.lock().unwrap() = Some(opened.name().to_string());
				encoder.insert(opened)
			}
		};

		// A viewer arriving after a break starts from a keyframe rather than
		// waiting out the rest of a group nobody received the start of.
		if std::mem::take(&mut paused) {
			encoder.cut().context("video encoder")?;
		}

		let start = Instant::now();
		let encoded = encoder.encode(&frame).context("video encoder")?;
		producer.publish(&encoded).context("video")?;
		stats
			.encode_us
			.store(start.elapsed().as_micros() as u64, Ordering::Relaxed);
		stats.encoded.fetch_add(1, Ordering::Relaxed);
	}
	Ok(())
}

/// Run a source on its own thread until its feed is dropped.
fn spawn(driver: Driver) {
	let name = format!("source-{}", driver.name());
	let spawned = std::thread::Builder::new()
		.name(name)
		.spawn(move || clocked(driver.run()));
	if let Err(err) = spawned {
		tracing::error!(%err, "cannot start a source thread");
	}
}

/// Run `task` on the calling thread, with its own clock and IO.
fn clocked<F: Future<Output = ()>>(task: F) {
	match tokio::runtime::Builder::new_current_thread().enable_all().build() {
		Ok(runtime) => runtime.block_on(task),
		Err(err) => tracing::error!(%err, "cannot start a runtime"),
	}
}

#[cfg(test)]
mod tests {
	use moq_mux::catalog::Stream;

	use super::*;

	/// Bounds each step, so a stall fails rather than hangs.
	const TIMEOUT: Duration = Duration::from_secs(30);

	/// The program a studio publishes reaches a relay as the picture it composed,
	/// with its audio alongside.
	#[tokio::test(flavor = "multi_thread")]
	#[ignore = "needs a Vulkan device and an H.264 encoder; run by just rs render-sw"]
	async fn the_program_reaches_a_relay() {
		let _ = moq_tokio::crypto::install_default();
		let fixture = moq_relay::test_relay().await.expect("test relay");
		let ready = fixture.relay.ready();
		let relay = fixture.relay.cluster().origin.consume();
		tokio::spawn(fixture.relay.run());
		ready.wait().await.expect("relay ready");

		let mut client = moq_tokio::connect::Config::default();
		client.tls.fingerprint = vec![fixture.fingerprint.clone()];
		let client = client.init(Default::default()).expect("client");

		let dir = tempfile::tempdir().unwrap();
		let mut collection = Collection::new();
		collection.canvas = moq_scene::Canvas {
			width: 320,
			height: 180,
			framerate: 30,
		};
		collection.sources.insert(
			"red".into(),
			moq_scene::Source::Color {
				color: moq_scene::Rgba([255, 0, 0, 255]),
			},
		);
		collection.scenes[0].items.push(moq_scene::Item {
			source: "red".into(),
			rect: moq_scene::Rect::fill(&collection.canvas),
			visible: true,
		});

		let gpu = Gpu::new().await.expect("a GPU");
		let path = dir.path().join("show.toml");
		let connect = Some((client, fixture.url.clone()));
		let engine = Engine::start(gpu, path, collection, connect, "studio")
			.await
			.expect("the studio starts");

		let broadcast = moq_clock::timeout(TIMEOUT, relay.follow("studio", None).unwrap().available())
			.await
			.expect("the relay routes the program")
			.expect("broadcast");
		let format = moq_mux::catalog::CatalogFormat::default();
		let mut catalogs = moq_mux::catalog::Consumer::<()>::new(&broadcast, format)
			.await
			.expect("catalog");
		let catalog = catalogs.next().await.expect("catalog").expect("a catalog");
		let (video, config) = catalog.video.enabled().next().expect("a video rendition");
		let mut options = moq_video::decode::Options::new();
		options.start = moq_video::decode::Start::Latest;
		let mut decoder = moq_video::decode::Consumer::new(&broadcast, config, video.clone(), options)
			.await
			.expect("video decoder");
		// The canvas is black until the color source delivers its first
		// picture, so wait for the composed one rather than take the first.
		moq_clock::timeout(TIMEOUT, async {
			loop {
				let frame = match decoder.read().await.expect("decode") {
					Some(moq_video::decode::Read::Frame(frame)) => frame,
					Some(_) => continue,
					None => panic!("the video ended"),
				};
				assert_eq!(frame.size(), moq_video::Size::new(320, 180));
				let rgba = frame.surface.to_rgba(&Default::default()).expect("rgba");
				let center = (90 * 320 + 160) * 4;
				let [r, g, b, _] = rgba.data()[center..center + 4] else {
					unreachable!()
				};
				// H.264 and its 4:2:0 round trip shift a primary a little.
				if r > 200 && g < 60 && b < 60 {
					return;
				}
			}
		})
		.await
		.expect("the composed picture arrives");

		// The program reaches the encoder as a GPU surface when the encoder
		// reads one, and is read back frame by frame only when it does not.
		let stats = &engine.stats;
		assert!(stats.copied.load(Ordering::Relaxed) > 0);
		let read_back = stats.read_back.load(Ordering::Relaxed);
		if stats.zero_copy.load(Ordering::Relaxed) {
			assert_eq!(read_back, 0, "a zero-copy program was read back");
		} else {
			assert!(read_back > 0, "the program was neither exported nor read back");
		}

		let (audio, config) = catalog.audio.enabled().next().expect("an audio rendition");
		let mut options = moq_audio::decode::Options::new();
		options.start = moq_audio::decode::Start::Latest;
		let mut decoder = moq_audio::decode::Consumer::new(&broadcast, config, audio.clone(), options)
			.await
			.expect("audio decoder");
		moq_clock::timeout(TIMEOUT, async {
			loop {
				match decoder.read().await.expect("decode") {
					Some(moq_audio::decode::Read::Frame(_)) => return,
					Some(_) => continue,
					None => panic!("the audio ended"),
				}
			}
		})
		.await
		.expect("sound arrives");
	}
}
