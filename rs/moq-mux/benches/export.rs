//! Container export cost swept over independently varied tracks and queued frames.
use std::time::Duration;

use bytes::Bytes;
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
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

/// A finished broadcast of `tracks` audio tracks with `frames` queued frames each.
async fn prepare(tracks: usize, frames: usize) -> (moq_mux::Source, moq_mux::catalog::Consumer, Fixture) {
	let (origin, driver) = moq_net::origin::Producer::new(Default::default());
	let driver = tokio::spawn(moq_net::time::run(driver));
	let broadcast = origin.create_broadcast("bench").unwrap();
	let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default()).unwrap();
	let stream = moq_mux::catalog::Consumer::from(catalog.consume().unwrap());
	let mut publishers = Vec::new();
	for index in 0..tracks {
		let track = broadcast
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
					duration: None,
					payload: Bytes::from_static(b"audio-frame"),
					keyframe: index == 0,
				})
				.unwrap();
		}
		track.finish().unwrap();
	}
	catalog.finish().unwrap();
	broadcast.announce(Default::default()).unwrap();
	let resolved = origin.consume().request("bench", None).await.unwrap();
	let source = moq_mux::Source::new(origin.consume(), resolved);
	let fixture = Fixture {
		_published: Box::new((broadcast, catalog, publishers)),
		_origin: origin,
		driver,
	};
	(source, stream, fixture)
}

/// Opened during setup so the timed drain has the same boundary across API changes.
enum Export {
	Flv(Box<moq_mux::container::flv::Export<moq_mux::catalog::Consumer>>),
	Mkv(Box<moq_mux::container::mkv::Export<moq_mux::catalog::Consumer>>),
	Ts(Box<moq_mux::container::ts::Export<moq_mux::catalog::Consumer>>),
}

fn open(format: &str, source: moq_mux::Source, stream: moq_mux::catalog::Consumer) -> Export {
	match format {
		"flv" => Export::Flv(Box::new(
			moq_mux::container::flv::Export::new(source, stream).with_multitrack(true),
		)),
		"ts" => Export::Ts(Box::new(moq_mux::container::ts::Export::new(source, stream))),
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

// A stable profiling boundary excludes publishing and opening from instruction counts.
#[inline(never)]
fn profiled_export(runtime: &tokio::runtime::Runtime, opened: Export) -> usize {
	runtime.block_on(export(opened))
}

fn bench_export(c: &mut Criterion) {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	let mut group = c.benchmark_group("container-export");
	group.sample_size(10);
	group.measurement_time(Duration::from_secs(1));
	for format in ["flv", "mkv", "ts"] {
		for tracks in [1, 4, 16] {
			for frames in [1, 16, 64] {
				group.throughput(Throughput::Elements((tracks * frames) as u64));
				group.bench_function(BenchmarkId::new(format, format!("{tracks}x{frames}")), |b| {
					// Publishing and opening are setup; only draining the export is measured.
					b.iter_batched(
						|| {
							let (source, stream, fixture) = runtime.block_on(prepare(tracks, frames));
							(open(format, source, stream), fixture)
						},
						|(opened, fixture)| {
							let chunks = profiled_export(&runtime, opened);
							if format == "ts" {
								assert!(chunks > 0);
							} else {
								assert_eq!(chunks, 1 + tracks * frames);
							}
							fixture
						},
						BatchSize::PerIteration,
					);
				});
			}
		}
	}
	group.finish();
}

criterion_group!(benches, bench_export, video_reorder);
criterion_main!(benches);

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
	let broadcast = origin.create_broadcast("bench").unwrap();
	let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default()).unwrap();
	let stream = moq_mux::catalog::Consumer::from(catalog.consume().unwrap());
	let track = broadcast
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
			"pyramid" => ((index / 9 * 9 + [0, 4, 2, 1, 3, 8, 6, 5, 7][index % 9]) * 3_600) as u64,
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
				duration: None,
				payload: payload.into(),
				keyframe: index == 0,
			})
			.unwrap();
	}
	video.finish().unwrap();
	catalog.finish().unwrap();
	broadcast.announce(Default::default()).unwrap();
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

fn video_reorder(c: &mut Criterion) {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	let mut group = c.benchmark_group("video-reorder");
	group.sample_size(10);
	group.warm_up_time(Duration::from_millis(50));
	group.measurement_time(Duration::from_millis(200));
	for pattern in ["pyramid", "flat", "backward"] {
		for count in [64, 256, 1024] {
			for size in [256, 16_384] {
				group.bench_function(BenchmarkId::new(pattern, format!("{count}x{size}")), |b| {
					b.iter_batched(
						|| runtime.block_on(prepare_video(count, size, pattern)),
						|(source, stream, fixture)| {
							let refused = runtime.block_on(async {
								let mut export = moq_mux::container::mkv::Export::new(source, stream)
									.with_fragment_duration(Duration::ZERO);
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
						},
						BatchSize::PerIteration,
					);
				});
			}
		}
	}
	group.finish();
}
