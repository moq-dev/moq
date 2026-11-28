//! The two costs a transcode ladder puts on the runtime that drives it.
//!
//! `fanout` feeds one decoded 1080p picture to a ladder of encoders, the way every rung of a source
//! takes the shared feed's frame. `caller` scales each rung's copy on the driving task before
//! handing it over, as rungs did before `encode::Config::resize`; `encoder` hands over the native
//! frame and lets each encoder scale it on its own thread. Each call times the whole fan-out,
//! encodes included; the driving thread's own CPU (the tokio worker a rung task occupies) is in
//! swole's per-call user and system CPU.
//!
//! `flapping` resizes a source back and forth between two pictures and times each edit until the
//! derived catalog is republished. Every edit retires and renames each rung, and the transcoder's
//! one loop resolves the new ladder before it does anything else, so an encoder probe per rung
//! per edit shows up here.
//!
//! Run with `cargo bench -p moq-transcode --bench ladder`.

use std::fmt;
use std::sync::Arc;

use moq_transcode::{Config, Ladder, Rung};

/// The decoded source picture every rung is fed.
const SOURCE: (u32, u32) = (1920, 1080);

/// Rung pictures, largest first; a case takes the first one or all of them.
const RUNGS: [(u32, u32); 3] = [(1280, 720), (640, 360), (320, 180)];

fn size((width, height): (u32, u32)) -> moq_video::Size {
	moq_video::Size::new(width, height)
}

/// Where a rung's copy of the picture is scaled.
#[derive(Clone, Copy)]
enum Scale {
	/// On the driving task, before the frame goes to the encoder.
	Caller,
	/// On the encoder's own thread, through `encode::Config::resize`.
	Encoder,
}

impl Scale {
	fn name(self) -> &'static str {
		match self {
			Self::Caller => "caller",
			Self::Encoder => "encoder",
		}
	}
}

/// One encoder per rung, each on its own thread.
async fn open(rungs: usize, scale: Scale) -> Vec<(moq_video::Size, moq_video::encode::Sink)> {
	let mut sinks = Vec::new();
	for &rung in &RUNGS[..rungs] {
		let rung = size(rung);
		let framerate = moq_video::FrameRate::new(30, 1).unwrap();
		let mut config = moq_video::encode::Config::new(rung.width, rung.height, framerate);
		config.kind = moq_video::encode::Kind::Software;
		config.bitrate = Some(moq_net::bandwidth::Rate::from_bps(1_000_000));
		// An unlabelled HD picture means BT.709, and its scaled copies keep that.
		config.color = Some(moq_video::Color::infer(size(SOURCE)));
		if let Scale::Encoder = scale {
			config.resize = Some(moq_video::resize::Config::default());
		}
		sinks.push((rung, moq_video::encode::Sink::open(&config).await.unwrap()));
	}
	sinks
}

/// A gradient rather than a flat picture, so the scaler has real samples to filter.
fn source_frame() -> Arc<moq_video::Frame> {
	let (width, height) = SOURCE;
	let mut data = Vec::with_capacity(size(SOURCE).pixels() as usize * 3 / 2);
	for y in 0..height {
		data.extend((0..width).map(|x| ((x + y) % 256) as u8));
	}
	data.resize(size(SOURCE).pixels() as usize * 3 / 2, 128);
	let i420 = moq_video::I420::new(size(SOURCE), data).unwrap();
	Arc::new(moq_video::Frame::new(
		moq_video::Surface::I420(i420),
		moq_net::Timestamp::ZERO,
	))
}

/// Feed `frame` to every rung at once, as each rung task does with the shared feed.
async fn feed(sinks: &mut [(moq_video::Size, moq_video::encode::Sink)], frame: &Arc<moq_video::Frame>, scale: Scale) {
	let resize = moq_video::resize::Config::default();
	futures::future::join_all(sinks.iter_mut().map(|(size, sink)| async {
		let frame = match scale {
			Scale::Caller => Arc::new(frame.resize(*size, &resize).unwrap()),
			Scale::Encoder => frame.clone(),
		};
		sink.encode(frame).await.unwrap();
		while sink.receive().unwrap().is_some() {}
	}))
	.await;
}

/// Where each rung's copy is scaled, and how many rungs.
#[derive(Clone, Copy)]
struct Case {
	scale: Scale,
	rungs: usize,
}

impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}/{}", self.scale.name(), self.rungs)
	}
}

fn cases() -> Vec<Case> {
	[1, RUNGS.len()]
		.into_iter()
		.flat_map(|rungs| [Scale::Caller, Scale::Encoder].map(|scale| Case { scale, rungs }))
		.collect()
}

/// Open encoders for one case, and the picture each call feeds them.
struct Fanout {
	sinks: Vec<(moq_video::Size, moq_video::encode::Sink)>,
	frame: Arc<moq_video::Frame>,
	scale: Scale,
	runtime: tokio::runtime::Runtime,
}

fn encoders(case: Case) -> Fanout {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	Fanout {
		sinks: runtime.block_on(open(case.rungs, case.scale)),
		frame: source_frame(),
		scale: case.scale,
		runtime,
	}
}

#[swole::bench(args = cases(), setup = encoders)]
fn fanout(fanout: &mut Fanout) {
	fanout
		.runtime
		.block_on(feed(&mut fanout.sinks, &fanout.frame, fanout.scale))
}

/// A source broadcast advertising one H.264 rendition, with its video track held open.
struct Source {
	broadcast: moq_net::broadcast::Producer,
	catalog: moq_mux::catalog::Producer,
	_track: moq_net::track::Producer,
}

impl Source {
	fn new(size: moq_video::Size) -> Self {
		let mut broadcast = moq_net::broadcast::Info::default().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let track = broadcast
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let mut source = Self {
			broadcast,
			catalog,
			_track: track,
		};
		source.publish(size);
		source
	}

	/// Republish the rendition at `size`, as an importer does when the picture changes.
	fn publish(&mut self, size: moq_video::Size) {
		let mut video = hang::catalog::VideoConfig::new(hang::catalog::H264 {
			inline: true,
			profile: 0x42,
			constraints: 0,
			level: 30,
		});
		video.coded_width = Some(size.width);
		video.coded_height = Some(size.height);
		video.bitrate = Some(1_000_000);
		video.framerate = Some(30.0);

		let mut guard = self.catalog.modify(self.catalog.next_timestamp()).unwrap();
		guard.video = hang::catalog::Video::default();
		guard.video.insert("video", video).unwrap();
	}
}

/// The rendition names in a derived catalog, which every edit replaces.
fn names(catalog: &moq_mux::catalog::hang::Catalog) -> Vec<String> {
	catalog.video.renditions.keys().cloned().collect()
}

/// A transcoder deriving a ladder from a source, and the derived catalog's renditions so far.
struct Flapping {
	source: Source,
	catalogs: moq_mux::catalog::hang::Consumer<()>,
	current: Vec<String>,
	edits: usize,
	runtime: tokio::runtime::Runtime,
}

/// Same height, different width: every rung below 720 lines changes shape on each edit.
const PICTURES: [(u32, u32); 2] = [(1280, 720), (960, 720)];

fn transcoder() -> Flapping {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();

	let source = Source::new(size(PICTURES[0]));
	let mut config = Config::default();
	let rung = |short: u32| Rung::new(short, moq_net::bandwidth::Rate::from_bps(u64::from(short) * 2_000));
	config.ladder = Ladder::new([480, 360, 240].map(rung)).unwrap();
	config.encoder = moq_video::encode::Kind::Software;
	config.decoder = moq_video::decode::Kind::Software;
	config.source = None;
	let output = moq_net::broadcast::Info::default().produce();
	let consumer = output.consume();
	runtime.spawn(moq_transcode::run(source.broadcast.consume(), output, config));

	let (catalogs, current) = runtime.block_on(async {
		let track = loop {
			match consumer.track(hang::Catalog::DEFAULT_NAME) {
				Ok(track) => break track,
				Err(moq_net::Error::NotFound) => tokio::task::yield_now().await,
				Err(err) => panic!("catalog track: {err}"),
			}
		};
		let mut catalogs = moq_mux::catalog::hang::Consumer::<()>::new(track);
		let current = loop {
			let snapshot = catalogs.next().await.unwrap().unwrap().value;
			if !snapshot.video.renditions.is_empty() {
				break names(&snapshot);
			}
		};
		(catalogs, current)
	});

	Flapping {
		source,
		catalogs,
		current,
		edits: 0,
		runtime,
	}
}

/// One edit, until the derived catalog's renditions change.
#[swole::bench(setup = transcoder)]
fn flapping(flapping: &mut Flapping) {
	flapping.edits += 1;
	flapping.source.publish(size(PICTURES[flapping.edits % PICTURES.len()]));
	flapping.runtime.block_on(async {
		loop {
			let snapshot = flapping.catalogs.next().await.unwrap().unwrap().value;
			let next = names(&snapshot);
			if next != flapping.current {
				flapping.current = next;
				break;
			}
		}
	})
}

fn main() {
	swole::main();
}
