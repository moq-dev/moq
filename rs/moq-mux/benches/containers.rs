//! Complete fMP4/CMAF and Annex-B imports and exports, with setup outside timing.
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_mux::catalog::{CatalogFormat, Stream};
use moq_mux::codec::h264;
use moq_mux::container::{Frame, Kind};
use moq_net::Timestamp;

// A real baseline SPS, also exercised by the H.264 import regression.
const SPS: &[u8] = &[
	0x67, 0x42, 0xc0, 0x1f, 0xda, 0x01, 0x40, 0x16, 0xe9, 0xb8, 0x08, 0x08, 0x0a, 0x00, 0x00, 0x07, 0xd0, 0x00, 0x01,
	0xd4, 0xc0, 0x80,
];
const PPS: &[u8] = &[0x68, 0xce, 0x3c, 0x80];

fn payload(size: usize) -> Bytes {
	let mut payload = BytesMut::new();
	for nal in [SPS, PPS, &[0x65, 0x88, 0x84, 0x21]] {
		payload.extend_from_slice(&[0, 0, 0, 1]);
		payload.extend_from_slice(nal);
	}
	// Nonzero padding cannot accidentally add an Annex-B start code.
	payload.resize(size, 0xff);
	payload.freeze()
}

fn timestamp(frame: usize) -> Timestamp {
	Timestamp::from_millis(frame as u64 * 20).unwrap()
}

struct Fixture {
	_published: Box<dyn std::any::Any>,
	_origin: moq_net::origin::Producer,
	driver: tokio::task::JoinHandle<moq_net::Error>,
	requests: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
	fn drop(&mut self) {
		self.driver.abort();
		self.requests.abort();
	}
}

async fn prepare(tracks: usize, frames: usize, payload: &Bytes) -> (moq_mux::Source, impl Stream, Fixture) {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let mut publishers = Vec::new();
	for index in 0..tracks {
		let track = broadcast
			.create_track(
				format!("video{index}"),
				catalog.track_info(hang::catalog::PRIORITY.video),
			)
			.unwrap();
		let config = h264::config(payload).unwrap();
		let mut publisher = catalog
			.video(track, moq_mux::catalog::hang::Container::Legacy(Kind::Video), config)
			.unwrap();
		for index in 0..frames {
			publisher
				.write(Frame {
					timestamp: timestamp(index),
					duration: Some(Timestamp::from_millis(20).unwrap()),
					payload: payload.clone(),
					keyframe: true,
				})
				.unwrap();
		}
		publisher.finish().unwrap();
		publishers.push(publisher);
	}
	catalog.finish().unwrap();
	let (origin, driver) = moq_net::origin::Producer::new(Default::default());
	let driver = tokio::spawn(moq_net::time::run(driver));
	let dynamic = origin.dynamic("", Default::default()).unwrap();
	let requests = tokio::spawn(async move {
		while let Ok(request) = dynamic.requested_broadcast().await {
			request.accept(consumer.clone());
		}
	});
	let resolved = origin.consume().request_broadcast("bench", None).await.unwrap();
	let source = moq_mux::Source::new(origin.consume(), resolved);
	let stream = source.catalog::<()>(CatalogFormat::Hang).await.unwrap();
	let fixture = Fixture {
		_published: Box::new((broadcast, catalog, publishers)),
		_origin: origin,
		driver,
		requests,
	};
	(source, stream, fixture)
}

async fn export(format: &str, source: moq_mux::Source, stream: impl Stream, collect: bool) -> Bytes {
	let mut data = BytesMut::new();
	if format == "cmaf" {
		let mut export = moq_mux::container::fmp4::Export::new(source, stream)
			.with_max_delay(Duration::from_secs(30))
			.with_fragment_duration(Duration::ZERO);
		while let Some(chunk) = export.next().await.unwrap() {
			if collect {
				data.extend_from_slice(&chunk);
			} else {
				std::hint::black_box(chunk);
			}
		}
	} else {
		let mut export = h264::Export::new(source, stream).with_max_delay(Duration::from_secs(30));
		while let Some(chunk) = export.next().await.unwrap() {
			if collect {
				data.extend_from_slice(&chunk);
			} else {
				std::hint::black_box(chunk);
			}
		}
	}
	data.freeze()
}

fn cmaf_import() -> (moq_mux::catalog::Producer, moq_mux::container::fmp4::Import) {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let import = moq_mux::container::fmp4::Import::new(broadcast, catalog.reserve());
	(catalog, import)
}

fn import_cmaf(
	(catalog, mut import): (moq_mux::catalog::Producer, moq_mux::container::fmp4::Import),
	data: &Bytes,
) -> (moq_mux::catalog::Producer, moq_mux::container::fmp4::Import) {
	import.decode(data).unwrap();
	import.finish().unwrap();
	(catalog, import)
}

struct Annexb {
	_broadcast: moq_net::broadcast::Producer,
	catalog: moq_mux::catalog::Producer,
	import: h264::Import,
	split: h264::Split,
}

impl Annexb {
	fn new() -> Self {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let track = broadcast
			.create_track("video", catalog.track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let import = h264::Import::new(track, catalog.reserve(), Default::default()).unwrap();
		Self {
			_broadcast: broadcast,
			catalog,
			import,
			split: h264::Split::new(),
		}
	}

	fn decode(&mut self, payload: &Bytes, frames: usize) {
		for index in 0..frames {
			let pts = timestamp(index);
			let mut complete = self.split.decode(payload, pts).unwrap();
			complete.extend(self.split.flush(pts).unwrap());
			self.import.decode(complete).unwrap();
		}
		self.import.finish().unwrap();
	}
}

fn containers(c: &mut Criterion) {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	for format in ["cmaf", "annexb"] {
		for tracks in if format == "cmaf" { &[1, 4][..] } else { &[1][..] } {
			for frames in [1, 16, 64] {
				for size in [256, 4096] {
					let payload = payload(size);
					let (source, stream, fixture) = runtime.block_on(prepare(*tracks, frames, &payload));
					let data = runtime.block_on(export(format, source, stream, true));
					drop(fixture);
					if format == "cmaf" {
						let (catalog, mut import) = cmaf_import();
						import.decode(&data).unwrap();
						import.finish().unwrap();
						assert_eq!(catalog.snapshot().video.renditions.len(), *tracks);
						use mp4_atom::DecodeMaybe;
						let mut cursor = std::io::Cursor::new(data.as_ref());
						let mut fragments = 0;
						while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor).unwrap() {
							if matches!(atom, mp4_atom::Any::Moof(_)) {
								fragments += 1;
							}
						}
						assert_eq!(fragments, tracks * frames);
					} else {
						let mut import = Annexb::new();
						import.decode(&payload, frames);
						assert_eq!(import.catalog.snapshot().video.renditions.len(), 1);
					}
					assert!(!data.is_empty());
					if format == "annexb" {
						assert_eq!(data.len(), size * frames);
					}
					for unit in ["frames", "bytes"] {
						let throughput = if unit == "frames" {
							Throughput::Elements((*tracks * frames) as u64)
						} else {
							Throughput::Bytes(data.len() as u64)
						};
						for direction in ["import", "export"] {
							let mut group = c.benchmark_group(format!("{format}-{direction}-{unit}"));
							group.throughput(throughput.clone());
							group.bench_function(
								BenchmarkId::new(format!("{tracks}tracks-{frames}frames"), size),
								|b| {
									if direction == "import" {
										if format == "cmaf" {
											b.iter_batched(
												cmaf_import,
												|import| import_cmaf(import, std::hint::black_box(&data)),
												BatchSize::PerIteration,
											);
										} else {
											b.iter_batched(
												Annexb::new,
												|mut import| {
													import.decode(std::hint::black_box(&payload), frames);
													import
												},
												BatchSize::PerIteration,
											);
										}
									} else {
										b.iter_batched(
											|| runtime.block_on(prepare(*tracks, frames, &payload)),
											|(source, stream, fixture)| {
												std::hint::black_box(
													runtime.block_on(export(format, source, stream, false)),
												);
												fixture
											},
											BatchSize::PerIteration,
										);
									}
								},
							);
							group.finish();
						}
					}
				}
			}
		}
	}
}

criterion_group!(benches, containers);
criterion_main!(benches);
