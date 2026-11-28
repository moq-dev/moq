//! Container export cost swept over independently varied tracks and queued frames, and the
//! cost of a live export's poll that nothing new woke.
//!
//! Run with `cargo bench -p moq-mux --bench export`.
use std::fmt;
use std::sync::LazyLock;
use std::task::Poll;
use std::time::Duration;

use bytes::Bytes;
use hang::catalog::{AAC, AudioConfig};
use moq_mux::catalog::hang::Container;
use moq_mux::container::{Frame, Kind};
use moq_net::Timestamp;

/// What keeps a prepared broadcast served; dropping it stops the origin's tasks.
struct Fixture {
	/// The broadcast, its catalog and its tracks, which a reader needs kept open.
	_published: Box<dyn std::any::Any>,
	_origin: moq_net::origin::Producer,
	driver: tokio::task::JoinHandle<moq_net::Error>,
}

impl Drop for Fixture {
	fn drop(&mut self) {
		self.driver.abort();
	}
}

/// A broadcast of `tracks` audio tracks with `frames` queued frames each, finished unless `live`.
async fn prepare(tracks: usize, frames: usize, live: bool) -> (moq_mux::Source, moq_mux::catalog::Consumer, Fixture) {
	let (origin, driver) = moq_net::origin::Producer::new(Default::default());
	let driver = tokio::spawn(moq_net::time::run(driver));
	let broadcast = origin.route("bench", moq_net::origin::RouteOptions::hidden()).unwrap();
	let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default()).unwrap();
	let stream = moq_mux::catalog::Consumer::from(catalog.consume().unwrap());
	let mut publishers = Vec::new();
	for index in 0..tracks {
		let track = broadcast
			.broadcast()
			.create_track(
				format!("audio{index}"),
				catalog.track_info(hang::catalog::PRIORITY.audio),
			)
			.unwrap();
		let mut config = AudioConfig::new(AAC { profile: 2 }, 44100, 2);
		config.description = Some(Bytes::from_static(&[0x12, 0x10]));
		publishers.push(catalog.audio(track, Container::Legacy(Kind::Audio), config).unwrap());
	}
	for track in &mut publishers {
		for index in 0..frames {
			track
				.write(Frame {
					timestamp: Timestamp::from_millis(index as u64 * 20).unwrap(),
					decode: None,
					duration: None,
					payload: Bytes::from_static(b"audio-frame"),
					keyframe: index == 0,
				})
				.unwrap();
		}
	}
	let tracks: Box<dyn std::any::Any> = match live {
		true => Box::new(publishers),
		false => {
			let finished: Vec<_> = publishers.into_iter().map(|track| track.finish().unwrap()).collect();
			catalog.finish().unwrap();
			Box::new(finished)
		}
	};
	broadcast.announce().unwrap();
	let resolved = origin.consume().request("bench", None).await.unwrap();
	let source = moq_mux::Source::new(origin.consume(), resolved);
	let fixture = Fixture {
		_published: Box::new((broadcast, catalog, tracks)),
		_origin: origin,
		driver,
	};
	(source, stream, fixture)
}

/// Opened during setup so the measured drain has the same boundary across API changes.
enum Export {
	Flv(Box<moq_mux::container::flv::Export<moq_mux::catalog::Consumer>>),
	Mkv(Box<moq_mux::container::mkv::Export<moq_mux::catalog::Consumer>>),
	Ts(Box<moq_mux::container::ts::Export<moq_mux::catalog::Consumer>>),
}

impl Export {
	/// Poll for the next chunk, ready with whether one came out rather than the export ending.
	fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<bool> {
		match self {
			Export::Flv(export) => export.poll_next(waiter).map(|res| res.unwrap().is_some()),
			Export::Mkv(export) => export.poll_next(waiter).map(|res| res.unwrap().is_some()),
			Export::Ts(export) => export.poll_next(waiter).map(|res| res.unwrap().is_some()),
		}
	}
}

fn open(format: &str, source: moq_mux::Source, stream: moq_mux::catalog::Consumer) -> Export {
	match format {
		"flv" => Export::Flv(Box::new(
			moq_mux::container::flv::Export::new(source, stream).with_multitrack(true),
		)),
		"ts" => Export::Ts(Box::new(moq_mux::container::ts::Export::new(
			source,
			stream,
			std::time::Duration::ZERO,
		))),
		_ => Export::Mkv(Box::new(
			moq_mux::container::mkv::Export::new(source, stream).with_fragment_duration(Duration::ZERO),
		)),
	}
}

async fn export(export: Export) -> usize {
	let mut chunks = 0;
	match export {
		Export::Flv(mut export) => {
			while let Some(chunk) = export.next().await.unwrap() {
				std::hint::black_box(chunk);
				chunks += 1;
			}
		}
		Export::Ts(mut export) => {
			while let Some(chunk) = export.next().await.unwrap() {
				std::hint::black_box(chunk);
				chunks += 1;
			}
		}
		Export::Mkv(mut export) => {
			while let Some(chunk) = export.next().await.unwrap() {
				std::hint::black_box(chunk);
				chunks += 1;
			}
		}
	}
	chunks
}

/// A runtime whose clock stands still between setup and the measured drain. A finished track's
/// last group ages out of a live-edge export once its span passes on the clock, so a host slow
/// enough to spend that long between the two would export nothing. Static, since each call takes
/// its broadcast by value.
static PAUSED: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
	tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.start_paused(true)
		.build()
		.unwrap()
});

/// Audio tracks and queued frames on each, named `<tracks>x<frames>`.
#[derive(Clone, Copy)]
struct Queue {
	tracks: usize,
	frames: usize,
}

impl Queue {
	fn items(self) -> usize {
		self.tracks * self.frames
	}
}

impl fmt::Display for Queue {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}x{}", self.tracks, self.frames)
	}
}

fn queues() -> Vec<Queue> {
	[1, 4, 16]
		.into_iter()
		.flat_map(|tracks| [1, 16, 64].map(|frames| Queue { tracks, frames }))
		.collect()
}

/// A finished broadcast's export, opened; publishing and opening are setup, so only draining it
/// is measured.
mod container_export {
	use swole::bench;

	use super::{Export, Fixture, PAUSED, Queue, open, prepare, queues};

	/// An opened export, the case it exports, and what keeps its broadcast served.
	struct Opened {
		export: Export,
		queue: Queue,
		fixture: Fixture,
	}

	fn opened(format: &str, queue: Queue) -> Opened {
		let (source, stream, fixture) = PAUSED.block_on(prepare(queue.tracks, queue.frames, false));
		Opened {
			export: open(format, source, stream),
			queue,
			fixture,
		}
	}

	/// Drain the export, returning its broadcast to drop after the measurement.
	fn drain(opened: Opened) -> (usize, Fixture) {
		(PAUSED.block_on(super::export(opened.export)), opened.fixture)
	}

	fn flv_opened(queue: Queue) -> Opened {
		opened("flv", queue)
	}

	#[bench(args = queues(), setup = flv_opened, items = Queue::items)]
	fn flv(opened: Opened) -> Fixture {
		let items = opened.queue.items();
		let (chunks, fixture) = drain(opened);
		assert_eq!(chunks, 1 + items);
		fixture
	}

	fn mkv_opened(queue: Queue) -> Opened {
		opened("mkv", queue)
	}

	#[bench(args = queues(), setup = mkv_opened, items = Queue::items)]
	fn mkv(opened: Opened) -> Fixture {
		let items = opened.queue.items();
		let (chunks, fixture) = drain(opened);
		assert_eq!(chunks, 1 + items);
		fixture
	}

	fn ts_opened(queue: Queue) -> Opened {
		opened("ts", queue)
	}

	#[bench(args = queues(), setup = ts_opened, items = Queue::items)]
	fn ts(opened: Opened) -> Fixture {
		let (chunks, fixture) = drain(opened);
		assert!(chunks > 0);
		fixture
	}
}

/// A live export polled again from the same task with nothing new: the poll a task pays for
/// whenever something else wakes it. Swept over tracks, a slope is work repeated per track.
mod container_export_idle {
	use std::task::{Context, Waker};
	use std::time::Duration;

	use swole::bench;

	use super::{Export, Fixture, open, prepare};

	/// A live export that has taken all it can, the park its polls hold their waiter in, and the
	/// runtime its broadcast runs on.
	struct Idle {
		opened: Export,
		park: kio::Park,
		_fixture: Fixture,
		/// Last, so the broadcast drops while its driver task still exists.
		runtime: tokio::runtime::Runtime,
	}

	fn idle(format: &str, tracks: usize) -> Idle {
		let runtime = tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.unwrap();
		let (opened, fixture) = runtime.block_on(async {
			let (source, stream, fixture) = prepare(tracks, 4, true).await;
			let mut opened = open(format, source, stream);
			// Take what the export lets go now, giving the origin's tasks a turn between polls.
			while let Ok(chunk) =
				kio::clock::timeout(Duration::from_millis(10), kio::wait(|waiter| opened.poll_next(waiter))).await
			{
				assert!(chunk, "a live export ended");
			}
			(opened, fixture)
		});
		Idle {
			opened,
			park: kio::Park::default(),
			_fixture: fixture,
			runtime,
		}
	}

	/// Poll once, holding its waiter as `kio::wait` does, on the runtime the export belongs to.
	fn poll(idle: &mut Idle) {
		let _runtime = idle.runtime.enter();
		let cx = Context::from_waker(Waker::noop());
		assert!(idle.opened.poll_next(idle.park.hold(&cx)).is_pending());
	}

	const TRACKS: [usize; 3] = [1, 4, 16];

	fn flv_idle(tracks: usize) -> Idle {
		idle("flv", tracks)
	}

	#[bench(args = TRACKS, setup = flv_idle, items = |tracks| tracks)]
	fn flv(idle: &mut Idle) {
		poll(idle);
	}

	fn mkv_idle(tracks: usize) -> Idle {
		idle("mkv", tracks)
	}

	#[bench(args = TRACKS, setup = mkv_idle, items = |tracks| tracks)]
	fn mkv(idle: &mut Idle) {
		poll(idle);
	}

	fn ts_idle(tracks: usize) -> Idle {
		idle("ts", tracks)
	}

	#[bench(args = TRACKS, setup = ts_idle, items = |tracks| tracks)]
	fn ts(idle: &mut Idle) {
		poll(idle);
	}
}

/// A progressive AVC SPS declaring a reorder depth of two (25 fps), as the catalog does.
const REORDER_SPS: &[u8] = &[
	0x67, 0x64, 0x00, 0x0a, 0xac, 0xd9, 0x44, 0x26, 0xc0, 0x44, 0x00, 0x00, 0x03, 0x00, 0x04, 0x00, 0x00, 0x03, 0x00,
	0xca, 0x3c, 0x48, 0x96, 0x58,
];

async fn prepare_video(
	count: usize,
	size: usize,
	pattern: &str,
) -> (moq_mux::Source, moq_mux::catalog::Consumer, Fixture) {
	let (origin, driver) = moq_net::origin::Producer::new(Default::default());
	let driver = tokio::spawn(moq_net::time::run(driver));
	let broadcast = origin.route("bench", moq_net::origin::RouteOptions::hidden()).unwrap();
	let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default()).unwrap();
	let stream = moq_mux::catalog::Consumer::from(catalog.consume().unwrap());
	let track = broadcast
		.broadcast()
		.create_track("video", catalog.track_info(hang::catalog::PRIORITY.video))
		.unwrap();
	let mut config = hang::catalog::VideoConfig::new(hang::catalog::H264 {
		profile: 100,
		constraints: 0,
		level: 10,
		inline: false,
	});
	config.coded_width = Some(64);
	config.coded_height = Some(64);
	config.reorder = Some(2);
	let mut description = vec![1, 100, 0, 10, 0xff, 0xe1];
	description.extend_from_slice(&(REORDER_SPS.len() as u16).to_be_bytes());
	description.extend_from_slice(REORDER_SPS);
	description.extend_from_slice(&[1, 0, 4, 0x68, 0xce, 0x3c, 0x80]);
	config.description = Some(description.into());
	config.container = hang::catalog::Container::Legacy;
	let mut video = catalog.video(track, Container::Legacy(Kind::Video), config).unwrap();
	for index in 0..count {
		let pts = match pattern {
			"pyramid" | "step-back" => {
				let pts = ((index / 9 * 9 + [0, 4, 2, 1, 3, 8, 6, 5, 7][index % 9]) * 3_600) as u64;
				if pattern == "step-back" && index >= count / 2 {
					// Keep the 300 ms rewind in the one group every subscription reads.
					pts - 300 * 90
				} else {
					pts
				}
			}
			"flat" => 90_000,
			"backward" => 90_000 - index as u64,
			_ => unreachable!(),
		};
		let mut payload = vec![0; size];
		payload[..4].copy_from_slice(&((size - 4) as u32).to_be_bytes());
		payload[4] = if index == 0 { 0x65 } else { 0x41 };
		video
			.write(Frame {
				timestamp: Timestamp::from_scale(pts, 90_000).unwrap(),
				decode: None,
				duration: None,
				payload: payload.into(),
				keyframe: index == 0,
			})
			.unwrap();
	}
	let video = video.finish().unwrap();
	catalog.finish().unwrap();
	broadcast.announce().unwrap();
	let resolved = origin.consume().request("bench", None).await.unwrap();
	let source = moq_mux::Source::new(origin.consume(), resolved);
	(
		source,
		stream,
		Fixture {
			_published: Box::new((broadcast, catalog, video)),
			_origin: origin,
			driver,
		},
	)
}

/// Frames of a size in a PTS pattern, named `<pattern>/<count>x<size>`.
#[derive(Clone, Copy)]
struct Reorder {
	pattern: &'static str,
	count: usize,
	size: usize,
}

impl fmt::Display for Reorder {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}/{}x{}", self.pattern, self.count, self.size)
	}
}

fn reorders(patterns: &[&'static str]) -> Vec<Reorder> {
	let mut cases = Vec::new();
	for &pattern in patterns {
		for count in [64, 256, 1024] {
			for size in [256, 16_384] {
				cases.push(Reorder { pattern, count, size });
			}
		}
	}
	cases
}

/// A finished video broadcast in `case`'s pattern, prepared for each call.
struct Reordered {
	source: moq_mux::Source,
	stream: moq_mux::catalog::Consumer,
	fixture: Fixture,
	pattern: &'static str,
}

fn reordered(case: Reorder) -> Reordered {
	let (source, stream, fixture) = PAUSED.block_on(prepare_video(case.count, case.size, case.pattern));
	Reordered {
		source,
		stream,
		fixture,
		pattern: case.pattern,
	}
}

/// Exporting video whose PTS run as the pattern says; MKV refuses any but the pyramid.
mod video_reorder {
	use std::time::Duration;

	use swole::bench;

	use super::{Fixture, PAUSED, Reordered, reordered, reorders};

	fn mkv_reorders() -> Vec<super::Reorder> {
		reorders(&["pyramid", "flat", "backward"])
	}

	#[bench(args = mkv_reorders(), setup = reordered)]
	fn mkv(reordered: Reordered) -> Fixture {
		let Reordered {
			source,
			stream,
			fixture,
			pattern,
		} = reordered;
		let refused = PAUSED.block_on(async {
			let mut export =
				moq_mux::container::mkv::Export::new(source, stream).with_fragment_duration(Duration::ZERO);
			loop {
				match export.next().await {
					Ok(Some(frame)) => {
						std::hint::black_box(frame);
					}
					Ok(None) => break false,
					Err(_) => break true,
				}
			}
		});
		assert_eq!(refused, pattern != "pyramid");
		fixture
	}

	fn ts_reorders() -> Vec<super::Reorder> {
		reorders(&["pyramid", "step-back"])
	}

	#[bench(args = ts_reorders(), setup = reordered)]
	fn ts(reordered: Reordered) -> Fixture {
		let Reordered {
			source,
			stream,
			fixture,
			pattern,
		} = reordered;
		PAUSED.block_on(async {
			let mut opened = moq_mux::container::ts::Export::new(source, stream, Duration::ZERO);
			let mut chunks = 0;
			while let Some(frame) = opened.next().await.unwrap() {
				std::hint::black_box(frame);
				chunks += 1;
			}
			assert!(chunks > 0);
			assert_eq!(opened.discontinuity(), u64::from(pattern == "step-back"));
		});
		fixture
	}
}

fn main() {
	swole::main();
}
