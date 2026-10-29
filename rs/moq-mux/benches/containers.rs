//! Complete container and Annex-B imports and exports, with setup outside timing.
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
}

impl Drop for Fixture {
	fn drop(&mut self) {
		self.driver.abort();
	}
}

struct Video {
	tracks: usize,
	frames: usize,
	frames_per_group: usize,
}

async fn prepare(video: Video, payload: &Bytes) -> (moq_mux::Source, impl Stream, Fixture) {
	let Video {
		tracks,
		frames,
		frames_per_group,
	} = video;
	let (origin, driver) = moq_net::origin::Producer::new(Default::default());
	let driver = tokio::spawn(moq_net::time::run(driver));
	let broadcast = origin.create_broadcast("bench").unwrap();
	let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default()).unwrap();
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
					keyframe: index % frames_per_group == 0,
				})
				.unwrap();
		}
		publisher.finish().unwrap();
		publishers.push(publisher);
	}
	catalog.finish().unwrap();
	broadcast.announce(Default::default()).unwrap();
	let resolved = origin.consume().request("bench", None).await.unwrap();
	let source = moq_mux::Source::new(origin.consume(), resolved);
	let stream = source.catalog::<()>(CatalogFormat::Hang).await.unwrap();
	let fixture = Fixture {
		_published: Box::new((broadcast, catalog, publishers)),
		_origin: origin,
		driver,
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

/// An fMP4 importer, republishing fragments whole when `passthrough` and unwrapping their samples
/// otherwise.
fn cmaf_import(passthrough: bool) -> (moq_mux::catalog::Producer, moq_mux::container::fmp4::Import) {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let import = moq_mux::container::fmp4::Import::new(broadcast, catalog.reserve()).with_passthrough(passthrough);
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

// Collect only timed import work, excluding the output validation in setup.
#[inline(never)]
fn profiled_import(mut import: Annexb, payload: &Bytes, frames: usize) -> Annexb {
	import.decode(payload, frames);
	import
}

/// `fragments` moofs whose `tracks` trafs each split their samples into `runs` one-sample runs,
/// interleaved in the mdat run by run, after the init of an exported `tracks`-track broadcast.
fn interleaved(init: &[u8], tracks: usize, runs: usize, fragments: usize, payload: &Bytes) -> Bytes {
	use mp4_atom::{Decode, Encode};
	let mut cursor = std::io::Cursor::new(init);
	mp4_atom::Ftyp::decode(&mut cursor).unwrap();
	let moov = mp4_atom::Moov::decode(&mut cursor).unwrap();
	let mut out = BytesMut::from(&init[..cursor.position() as usize]);
	for fragment in 0..fragments {
		let mut moof = mp4_atom::Moof {
			mfhd: mp4_atom::Mfhd {
				sequence_number: fragment as u32 + 1,
			},
			traf: moov.trak[..tracks]
				.iter()
				.map(|trak| mp4_atom::Traf {
					tfhd: mp4_atom::Tfhd {
						track_id: trak.tkhd.track_id,
						..Default::default()
					},
					tfdt: Some(mp4_atom::Tfdt {
						base_media_decode_time: (fragment * runs * 20) as u64 * trak.mdia.mdhd.timescale as u64 / 1000,
					}),
					trun: (0..runs)
						.map(|run| mp4_atom::Trun {
							data_offset: Some(0),
							entries: vec![mp4_atom::TrunEntry {
								duration: Some(trak.mdia.mdhd.timescale / 50),
								size: Some(payload.len() as u32),
								flags: Some(if run == 0 { 0 } else { 0x0001_0000 }),
								cts: None,
							}],
						})
						.collect(),
					..Default::default()
				})
				.collect(),
		};
		let mut buf = Vec::new();
		moof.encode(&mut buf).unwrap();
		let data = buf.len() + 8;
		for (track, traf) in moof.traf.iter_mut().enumerate() {
			for (run, trun) in traf.trun.iter_mut().enumerate() {
				trun.data_offset = Some((data + (run * tracks + track) * payload.len()) as i32);
			}
		}
		buf.clear();
		moof.encode(&mut buf).unwrap();
		mp4_atom::Mdat {
			data: payload.repeat(tracks * runs),
		}
		.encode(&mut buf)
		.unwrap();
		out.extend_from_slice(&buf);
	}
	out.freeze()
}

/// fMP4 passthrough and unwrapping of fragments whose tracks interleave their runs in one mdat,
/// across track and run count: each track copies only its own samples.
fn interleaved_runs(c: &mut Criterion) {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	let payload = payload(4096);
	let frames = 16;
	for tracks in [1, 4] {
		let (source, stream, fixture) = runtime.block_on(prepare(
			Video {
				tracks,
				frames: 1,
				frames_per_group: 1,
			},
			&payload,
		));
		let init = runtime.block_on(export("cmaf", source, stream, true));
		drop(fixture);
		for size in [1, payload.len()] {
			let sample = payload.slice(..size);
			for runs in [1, 16] {
				let data = interleaved(&init, tracks, runs, frames, &sample);
				for passthrough in [false, true] {
					let (catalog, import) = import_cmaf(cmaf_import(passthrough), &data);
					assert_eq!(catalog.snapshot().video.renditions.len(), tracks);
					drop(import);
				}
				for (direction, passthrough) in [("import", true), ("unwrap", false)] {
					let mut group = c.benchmark_group(format!("cmaf-interleaved-{direction}"));
					group.throughput(Throughput::Bytes(data.len() as u64));
					group.bench_function(
						BenchmarkId::new(format!("{tracks}tracks"), format!("{runs}runs-{size}bytes")),
						|b| {
							b.iter_batched(
								|| cmaf_import(passthrough),
								|import| import_cmaf(import, std::hint::black_box(&data)),
								BatchSize::PerIteration,
							);
						},
					);
					group.finish();
				}
			}
		}
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
					let (source, stream, fixture) = runtime.block_on(prepare(
						Video {
							tracks: *tracks,
							frames,
							frames_per_group: 1,
						},
						&payload,
					));
					let data = runtime.block_on(export(format, source, stream, true));
					drop(fixture);
					if format == "cmaf" {
						for passthrough in [false, true] {
							let (catalog, mut import) = cmaf_import(passthrough);
							import.decode(&data).unwrap();
							import.finish().unwrap();
							assert_eq!(catalog.snapshot().video.renditions.len(), *tracks);
						}
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
						// `unwrap` imports the same fragments one frame per sample; `import` passes them through.
						let directions: &[&str] = match format {
							"cmaf" => &["import", "unwrap", "export"],
							_ => &["import", "export"],
						};
						for &direction in directions {
							let mut group = c.benchmark_group(format!("{format}-{direction}-{unit}"));
							group.throughput(throughput.clone());
							group.bench_function(
								BenchmarkId::new(format!("{tracks}tracks-{frames}frames"), size),
								|b| {
									if direction != "export" {
										if format == "cmaf" {
											let passthrough = direction == "import";
											b.iter_batched(
												|| cmaf_import(passthrough),
												|import| import_cmaf(import, std::hint::black_box(&data)),
												BatchSize::PerIteration,
											);
										} else {
											b.iter_batched(
												Annexb::new,
												|import| {
													profiled_import(import, std::hint::black_box(&payload), frames)
												},
												BatchSize::PerIteration,
											);
										}
									} else {
										b.iter_batched(
											|| {
												runtime.block_on(prepare(
													Video {
														tracks: *tracks,
														frames,
														frames_per_group: 1,
													},
													&payload,
												))
											},
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

// A stable profiling boundary excludes publishing and benchmark setup from counts.
#[inline(never)]
fn export_queued_groups(runtime: &tokio::runtime::Runtime, source: moq_mux::Source, stream: impl Stream) {
	std::hint::black_box(runtime.block_on(export("annexb", source, stream, false)));
}

// Keep both dimensions independent: per-frame scans over queued groups grow with
// frames per group as well as group count, unlike one constant cost per group.
fn queued_groups(c: &mut Criterion) {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	let payload = payload(256);
	let mut group = c.benchmark_group("annexb-export-queued-groups");
	for groups in [1, 16, 64] {
		for frames_per_group in [1, 16] {
			let frames = groups * frames_per_group;
			let (source, stream, fixture) = runtime.block_on(prepare(
				Video {
					tracks: 1,
					frames,
					frames_per_group,
				},
				&payload,
			));
			let data = runtime.block_on(export("annexb", source, stream, true));
			assert_eq!(data.len(), 256 * frames);
			drop(fixture);
			group.throughput(Throughput::Elements(frames as u64));
			group.bench_function(BenchmarkId::new(groups.to_string(), frames_per_group), |b| {
				b.iter_batched(
					|| {
						runtime.block_on(prepare(
							Video {
								tracks: 1,
								frames,
								frames_per_group,
							},
							&payload,
						))
					},
					|(source, stream, fixture)| {
						export_queued_groups(&runtime, source, stream);
						fixture
					},
					BatchSize::PerIteration,
				);
			});
		}
	}
	group.finish();
}

/// An SI-only TS program with accepted PCR progress. The four capture PIDs and
/// eight-bit table IDs bound table-key history to 1,024 keys in the real importer.
struct SiImport {
	inner: moq_mux::container::ts::Import<moq_mux::container::ts::Ext>,
	cc: [u8; 4],
	clock: u64,
	pending: bool,
}

impl SiImport {
	const PIDS: [u16; 4] = [0x10, 0x11, 0x12, 0x14];

	fn new() -> Self {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(
			&mut broadcast,
			moq_mux::catalog::Config::default()
				.with_catalog(moq_mux::catalog::hang::Catalog::<moq_mux::container::ts::Ext>::default()),
		)
		.unwrap();
		let mut this = Self {
			inner: moq_mux::container::ts::Import::new(broadcast, catalog.reserve()),
			cc: [0; 4],
			clock: 0,
			pending: false,
		};
		// PAT and an empty PMT declaring a dedicated PCR PID. Together they also
		// provide the framer's initial second packet for confirming its sync byte.
		let program = [
			Self::packet(0, 0, this.section(0, 1, &[0, 1, 0xe0, 0x20])),
			Self::packet(0x20, 0, this.section(2, 1, &[0xe1, 0xff, 0xf0, 0])),
		]
		.concat();
		this.inner.decode(&program).unwrap();
		this.tick();
		this
	}

	fn section(&self, table: u8, id: u16, body: &[u8]) -> Vec<u8> {
		let length = 9 + body.len();
		let mut section = vec![
			table,
			0xb0,
			length as u8,
			(id >> 8) as u8,
			id as u8,
			0xc1,
			0,
			u8::from(self.pending && table == 0x42),
		];
		section.extend_from_slice(body);
		let crc = crc::Crc::<u32>::new(&crc::CRC_32_MPEG_2).checksum(&section);
		section.extend_from_slice(&crc.to_be_bytes());
		section
	}

	fn packet(pid: u16, cc: u8, section: Vec<u8>) -> Vec<u8> {
		let mut packet = vec![0x47, 0x40 | (pid >> 8) as u8, pid as u8, 0x10 | cc, 0];
		packet.extend(section);
		packet.resize(188, 0xff);
		packet
	}

	/// A 40 ms PCR interval, safely below the importer's accepted jump bound.
	fn tick(&mut self) {
		let base = self.clock / 300;
		let mut packet = vec![0x47, 0x01, 0xff, 0x20, 183, 0x10];
		packet.extend_from_slice(&[
			(base >> 25) as u8,
			(base >> 17) as u8,
			(base >> 9) as u8,
			(base >> 1) as u8,
			((base & 1) << 7) as u8 | 0x7e,
			0,
		]);
		packet.resize(188, 0xff);
		self.inner.decode(&packet).unwrap();
		self.clock += 40 * 27_000;
	}

	fn repeat(&mut self, entries: impl IntoIterator<Item = (usize, u8, u16)>) {
		let mut packets = Vec::new();
		for (pid, table, id) in entries {
			let cc = self.cc[pid];
			self.cc[pid] = (cc + 1) & 15;
			packets.extend(Self::packet(Self::PIDS[pid], cc, self.section(table, id, &[0; 6])));
		}
		self.inner.decode(&packets).unwrap();
	}

	fn identities(count: u16) -> Self {
		let mut this = Self::new();
		this.repeat((0..count).map(|id| (2, 0x80, id)));
		this
	}

	fn pending(count: u16) -> Self {
		let mut this = Self::new();
		this.pending = true;
		this.inner = this.inner.with_program(1);
		this.repeat((0..count).map(|id| (1, 0x42, id)));
		this
	}

	fn table_key(index: u16) -> (usize, u8, u16) {
		(usize::from(index / 256), index as u8, 1)
	}

	fn history(count: u16, touches: u16) -> Self {
		let mut this = Self::new();
		this.repeat((0..count).map(Self::table_key));
		// All table generations expire. The existing continuity policy retains
		// their Entry/track owners; re-activate just the entries the timed call touches.
		for _ in 0..=775 {
			this.tick();
		}
		this.repeat((0..touches).map(|key| Self::table_key(key % count)));
		this
	}
}

/// Time one accepted PCR interval plus `touches` SI arrivals against `retained` state,
/// swept over both axes so a cost that grows with the retained set shows up as a slope.
fn ts_si_sweep(
	c: &mut Criterion,
	name: &str,
	retained: [u16; 3],
	setup: impl Fn(u16, u16) -> SiImport,
	touch: impl Fn(u16, u16) -> (usize, u8, u16),
) {
	let mut group = c.benchmark_group(name);
	group.measurement_time(Duration::from_secs(1));
	group.warm_up_time(Duration::from_millis(200));
	group.sample_size(10);
	for retained in retained {
		for touches in [1, 16, 64] {
			group.throughput(Throughput::Elements(u64::from(touches)));
			group.bench_with_input(
				BenchmarkId::new(retained.to_string(), touches),
				&touches,
				|b, &touches| {
					b.iter_batched_ref(
						|| setup(retained, touches),
						|import| {
							import.tick();
							import.repeat((0..touches).map(|key| touch(retained, key)));
						},
						BatchSize::SmallInput,
					);
				},
			);
		}
	}
	group.finish();
}

/// SI renewal across retained sub-table count and touched identities. Accepted
/// PCR advances the deadlines during timing without expiring the retained set.
fn ts_si_renewal(c: &mut Criterion) {
	ts_si_sweep(
		c,
		"ts_si_renewal_pcr",
		[1, 64, 4096],
		|identities, _| SiImport::identities(identities),
		|identities, id| (2, 0x80, id % identities),
	);
}

/// Pending SDT generations retain only their first section; the selected service
/// prevents a repeated partial set from committing as a sparse complete cycle.
fn ts_si_pending(c: &mut Criterion) {
	ts_si_sweep(
		c,
		"ts_si_pending_pcr",
		[1, 64, 4096],
		|identities, _| SiImport::pending(identities),
		|identities, id| (1, 0x42, id % identities),
	);
}

/// Distinct expired PID/table-key history versus current touched entries, using
/// only the four SI PIDs the importer actually captures and the full table-ID space.
fn ts_si_table_history(c: &mut Criterion) {
	ts_si_sweep(
		c,
		"ts_si_table_history_pcr",
		[1, 64, 1024],
		SiImport::history,
		|history, key| SiImport::table_key(key % history),
	);
}

/// Live WebM framing across transport chunk sizes, including byte-at-a-time metadata.
fn mkv_chunks(c: &mut Criterion) {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	let _enter = runtime.enter();
	let data = include_bytes!("../src/container/mkv/test_data/opus.webm");
	let mut group = c.benchmark_group("mkv-chunks");
	group.throughput(Throughput::Bytes(data.len() as u64));
	for chunk in [1, 7, 64, data.len()] {
		group.bench_function(BenchmarkId::from_parameter(chunk), |b| {
			b.iter_batched(
				|| {
					let mut broadcast = moq_net::broadcast::Info::new().produce();
					let catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
					let import = moq_mux::container::mkv::Import::new(broadcast, catalog.reserve());
					(catalog, import)
				},
				|(catalog, mut import)| {
					for bytes in std::hint::black_box(data).chunks(chunk) {
						import.decode(bytes).unwrap();
					}
					import.finish().unwrap();
					(catalog, import)
				},
				BatchSize::PerIteration,
			);
		});
	}
	group.finish();
}

criterion_group!(
	benches,
	containers,
	mkv_chunks,
	interleaved_runs,
	queued_groups,
	ts_si_renewal,
	ts_si_pending,
	ts_si_table_history
);
criterion_main!(benches);
