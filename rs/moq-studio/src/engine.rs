//! Everything behind the window: the GPU, the sources, the program clock that
//! draws the canvas and mixes the audio, and the broadcast it publishes.
//!
//! Threads:
//! - **program** draws preview and program once per canvas frame and hands the
//!   program to the encoder, stamped with the program clock.
//! - **audio** reads the program bus on the same clock and publishes it.
//! - **encode** reads the program back from the GPU and encodes it.
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
use moq_scene::{Collection, Compositor, Pending, Readback, wgpu};

/// The GPU everything shares: sources upload to it, the compositor draws on
/// it, the window presents from it, and the encoder reads back from it.
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
		// Lets a hardware-decoded or captured DMA-BUF reach the compositor
		// without a copy, where the driver can import one.
		let features = adapter.features() & wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF;
		let (device, queue) = adapter
			.request_device(&wgpu::DeviceDescriptor {
				label: Some("moq-studio"),
				required_features: features,
				..Default::default()
			})
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
	/// Program frames copied back to the CPU for the encoder.
	pub read_back: AtomicU64,
	/// Microseconds the last readback waited on the GPU and converted pixels.
	pub readback_us: AtomicU64,
	/// Program frames not encoded because the encoder still held every buffer.
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
	pub gpu: Gpu,
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
		let sources = Sources::new(source::Context::new(bus.clone(), subscribe.consume(), dir));
		let mut compositor = Compositor::new(&gpu.device, &gpu.queue, collection.canvas)?;
		compositor.set_preview(collection.scenes.first().map(|scene| scene.name.clone()));
		compositor.cut();
		let canvas = collection.canvas;

		let (output, origin, publish) = match connect {
			Some((client, url)) => {
				let (output, publish) = publish(client, url, subscribe, broadcast, canvas).await?;
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
			gpu,
			stats: Stats::default(),
			bus,
			output,
			_origin: origin,
		});

		let (video, audio) = match publish {
			Some(Publish { video, audio, config }) => {
				let (frames, pending) = mpsc::sync_channel(Readback::DEPTH);
				tokio::spawn(watch(video.demand(), engine.clone()));
				let encoder = engine.clone();
				std::thread::Builder::new()
					.name("encode".into())
					.spawn(move || encode(&encoder, pending, video, config))?;
				(Some(frames), Some(audio))
			}
			None => (None, None),
		};

		let start = Instant::now();
		let program = engine.clone();
		std::thread::Builder::new()
			.name("program".into())
			.spawn(move || clocked(program.run_program(start, video)))?;
		let mix = engine.clone();
		std::thread::Builder::new()
			.name("audio".into())
			.spawn(move || clocked(mix.run_audio(start, audio)))?;

		Ok(engine)
	}

	/// Draw a canvas frame per tick of the program clock, and hand the program
	/// to the encoder while anyone watches.
	async fn run_program(&self, start: Instant, frames: Option<mpsc::SyncSender<Pending>>) {
		let canvas = self.state.lock().unwrap().collection.canvas;
		let mut readback = Readback::new(&self.gpu.device, &self.gpu.queue, canvas);
		let period = Duration::from_secs(1) / canvas.framerate;
		let mut clock = moq_clock::interval_at(start, period);
		clock.set_missed_tick_behavior(moq_clock::MissedTickBehavior::Skip);

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

			let Some(frames) = &frames else { continue };
			if !self.stats.watched.load(Ordering::Relaxed) {
				continue;
			}
			let Ok(timestamp) = moq_net::Timestamp::try_from(tick.saturating_duration_since(start)) else {
				continue;
			};
			match readback.copy(&program, timestamp) {
				// The channel holds as many as the pool, so this never blocks.
				Some(pending) => {
					if frames.send(pending).is_err() {
						return;
					}
				}
				None => {
					self.stats.dropped.fetch_add(1, Ordering::Relaxed);
				}
			}
		}
	}

	/// Read the program bus as the program clock advances, and publish it.
	///
	/// Runs without a relay too, so the meters move and sources never back up.
	async fn run_audio(&self, start: Instant, mut producer: Option<moq_audio::encode::Producer>) {
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
			let timestamp = moq_net::Timestamp::from_micros((written * 1_000_000 / rate) as u64);
			written = due;

			let Some(publish) = producer.as_mut() else { continue };
			let Ok(timestamp) = timestamp else { continue };
			let data: Vec<u8> = samples.iter().flat_map(|sample| sample.to_le_bytes()).collect();
			if let Err(err) = publish.write(&moq_audio::Frame::new(Bytes::from(data), timestamp)) {
				self.stats.fail(format!("audio: {err}"));
				producer = None;
			}
		}
	}
}

/// The encoders behind an [`Output`], handed to their threads.
struct Publish {
	video: moq_video::encode::Producer,
	config: moq_video::encode::Config,
	audio: moq_audio::encode::Producer,
}

/// Announce the program broadcast and connect to the relay, which also
/// carries `subscribe`'s broadcast sources.
async fn publish(
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

	let config = moq_video::encode::Config::new(canvas.width, canvas.height, canvas.framerate());
	// Opens a throwaway encoder now, so a machine that cannot encode the
	// canvas fails at startup rather than when the first viewer arrives.
	let rendition = config.probe().await.context("cannot encode the canvas")?;
	let reserved = catalog
		.reserve()
		.with_timeline_hint(config.gop.duration(config.framerate));
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
	Ok((output, Publish { video, config, audio }))
}

/// Track whether anyone watches the video, so the program is only read back
/// and encoded while someone does.
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

/// Read back and encode program frames until the program clock stops.
fn encode(
	engine: &Engine,
	pending: mpsc::Receiver<Pending>,
	mut producer: moq_video::encode::Producer,
	config: moq_video::encode::Config,
) {
	let stats = &engine.stats;
	let mut encoder: Option<moq_video::encode::Encoder> = None;
	let mut watched = false;

	while let Ok(pending) = pending.recv() {
		let start = Instant::now();
		let frame = match pending.wait() {
			Ok(frame) => frame,
			Err(err) => return stats.fail(format!("readback: {err}")),
		};
		stats.readback_us.store(start.elapsed().as_micros() as u64, Ordering::Relaxed);
		stats.read_back.fetch_add(1, Ordering::Relaxed);

		let encoder = match &mut encoder {
			Some(encoder) => encoder,
			None => match moq_video::encode::Encoder::new(&config) {
				Ok(opened) => {
					*stats.encoder.lock().unwrap() = Some(opened.name().to_string());
					encoder.insert(opened)
				}
				Err(err) => return stats.fail(format!("video encoder: {err}")),
			},
		};

		// A viewer arriving after a break starts from a keyframe rather than
		// waiting out the rest of a group nobody received the start of.
		let now = stats.watched.load(Ordering::Relaxed);
		if now
			&& !watched
			&& let Err(err) = encoder.cut()
		{
			return stats.fail(format!("video encoder: {err}"));
		}
		watched = now;

		let start = Instant::now();
		let encoded = match encoder.encode(&frame) {
			Ok(encoded) => encoded,
			Err(err) => return stats.fail(format!("video encoder: {err}")),
		};
		if let Err(err) = producer.publish(&encoded) {
			return stats.fail(format!("video: {err}"));
		}
		stats.encode_us.store(start.elapsed().as_micros() as u64, Ordering::Relaxed);
		stats.encoded.fetch_add(1, Ordering::Relaxed);
	}
}

/// Run a source on its own thread until its feed is dropped.
fn spawn(driver: Driver) {
	let name = format!("source-{}", driver.name());
	let spawned = std::thread::Builder::new().name(name).spawn(move || clocked(driver.run()));
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
		let _engine = Engine::start(gpu, path, collection, connect, "studio")
			.await
			.expect("the studio starts");

		let broadcast = tokio::time::timeout(TIMEOUT, relay.routed_broadcast("studio"))
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
		let frame = tokio::time::timeout(TIMEOUT, async {
			loop {
				match decoder.read().await.expect("decode") {
					Some(moq_video::decode::Read::Frame(frame)) => return frame,
					Some(_) => continue,
					None => panic!("the video ended"),
				}
			}
		})
		.await
		.expect("a picture arrives");

		assert_eq!(frame.size(), moq_video::Size::new(320, 180));
		let rgba = frame.surface.to_rgba(&Default::default()).expect("rgba");
		let center = (90 * 320 + 160) * 4;
		let [r, g, b, _] = rgba.data()[center..center + 4] else {
			unreachable!()
		};
		// H.264 and its 4:2:0 round trip shift a primary a little.
		assert!(r > 200 && g < 60 && b < 60, "{:?}", [r, g, b]);

		let (audio, config) = catalog.audio.enabled().next().expect("an audio rendition");
		let mut options = moq_audio::decode::Options::new();
		options.start = moq_audio::decode::Start::Latest;
		let mut decoder = moq_audio::decode::Consumer::new(&broadcast, config, audio.clone(), options)
			.await
			.expect("audio decoder");
		tokio::time::timeout(TIMEOUT, async {
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
