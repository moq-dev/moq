//! Complete container and Annex-B imports and exports, with setup outside the measurement.
//!
//! Run with `cargo bench -p moq-mux --bench containers`.
use std::fmt;
use std::sync::LazyLock;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use moq_mux::catalog::{CatalogFormat, Stream};
use moq_mux::codec::h264;
use moq_mux::container::{Frame, Kind};
use moq_net::Timestamp;
use swole::bench;

/// The runtime that publishes and exports, shared by setup and the benchmarks.
static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
	tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap()
});

/// What each call consumes, rebuilt from `from` before the next one.
struct Fresh<F, T> {
	from: F,
	make: fn(&F) -> T,
	next: Option<T>,
}

impl<F, T> Fresh<F, T> {
	fn new(from: F, make: fn(&F) -> T) -> Self {
		Self { from, make, next: None }
	}

	fn take(&mut self) -> (&F, T) {
		(&self.from, self.next.take().expect("refilled before each call"))
	}
}

fn refill<F, T>(fresh: &mut Fresh<F, T>) {
	fresh.next = Some((fresh.make)(&fresh.from));
}

/// A transport chunk size, or the whole input at once.
#[derive(Clone, Copy)]
enum Chunk {
	Bytes(usize),
	Whole,
}

impl Chunk {
	fn size(self, len: usize) -> usize {
		match self {
			Self::Bytes(size) => size,
			Self::Whole => len,
		}
	}
}

impl fmt::Display for Chunk {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		match self {
			Self::Bytes(size) => write!(f, "{size}"),
			Self::Whole => write!(f, "whole"),
		}
	}
}

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

#[derive(Clone, Copy)]
struct Video {
	tracks: usize,
	frames: usize,
	frames_per_group: usize,
}

/// A published broadcast's source and catalog, and what keeps it served.
type Prepared = (moq_mux::Source, moq_mux::catalog::Consumer, Fixture);

async fn prepare(video: Video, payload: &Bytes) -> Prepared {
	let Video {
		tracks,
		frames,
		frames_per_group,
	} = video;
	let (origin, driver) = moq_net::origin::Producer::new(Default::default());
	let driver = tokio::spawn(moq_net::time::run(driver));
	let broadcast = origin.route("bench", moq_net::origin::RouteOptions::hidden()).unwrap();
	let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default()).unwrap();
	let mut publishers = Vec::new();
	for index in 0..tracks {
		let track = broadcast
			.broadcast()
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
		publishers.push(publisher.finish().unwrap());
	}
	catalog.finish().unwrap();
	broadcast.announce().unwrap();
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
			.with_delay(moq_mux::container::Delay::default().with_max(Duration::from_secs(30)))
			.with_fragment_duration(Duration::ZERO);
		while let Some(chunk) = export.next().await.unwrap() {
			if collect {
				data.extend_from_slice(&chunk);
			} else {
				std::hint::black_box(chunk);
			}
		}
	} else {
		let mut export = h264::Export::new(source, stream)
			.with_delay(moq_mux::container::Delay::default().with_max(Duration::from_secs(30)));
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

type CmafImporter = (moq_mux::catalog::Producer, moq_mux::container::fmp4::Import);

/// An fMP4 importer, republishing fragments whole when `passthrough` and unwrapping their samples
/// otherwise.
fn cmaf_importer(passthrough: bool) -> CmafImporter {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let import = moq_mux::container::fmp4::Import::new(broadcast, catalog.reserve()).with_cmaf(passthrough);
	(catalog, import)
}

fn import_cmaf(import: CmafImporter, data: &Bytes) -> (moq_mux::catalog::Producer, moq_mux::container::fmp4::Finished) {
	import_cmaf_chunks(import, data, data.len())
}

fn import_cmaf_chunks(
	(catalog, mut import): CmafImporter,
	data: &Bytes,
	chunk: usize,
) -> (moq_mux::catalog::Producer, moq_mux::container::fmp4::Finished) {
	for chunk in data.chunks(chunk) {
		import.decode(chunk).unwrap();
	}
	(catalog, import.finish().unwrap())
}

struct Annexb {
	broadcast: moq_net::broadcast::Producer,
	catalog: moq_mux::catalog::Producer,
	import: h264::Import,
	split: h264::Split,
}

/// What a finished [`Annexb`] import keeps published, returned so the measured call drops none of it.
struct Imported {
	_broadcast: moq_net::broadcast::Producer,
	catalog: moq_mux::catalog::Producer,
	_track: moq_mux::container::Finished<moq_mux::catalog::hang::Container, hang::catalog::VideoConfig>,
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
			broadcast,
			catalog,
			import,
			split: h264::Split::new(),
		}
	}

	fn decode(mut self, payload: &Bytes, frames: usize) -> Imported {
		for index in 0..frames {
			let pts = timestamp(index);
			let mut complete = self.split.decode(payload, pts).unwrap();
			complete.extend(self.split.flush(pts).unwrap());
			self.import.decode(complete).unwrap();
		}
		Imported {
			_broadcast: self.broadcast,
			catalog: self.catalog,
			_track: self.import.finish().unwrap(),
		}
	}
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
							samples: vec![mp4_atom::TrunEntry {
								duration: Some(trak.mdia.mdhd.timescale / 50),
								size: Some(payload.len() as u32),
								flags: Some(if run == 0 { 0 } else { 0x0001_0000 }),
								cts: None,
							}]
							.into(),
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

/// `frames` frames of `size` bytes on each of `tracks` tracks, named `<tracks>tracks-<frames>frames/<size>`.
#[derive(Clone, Copy)]
struct Shape {
	tracks: usize,
	frames: usize,
	size: usize,
}

impl Shape {
	fn video(self) -> Video {
		Video {
			tracks: self.tracks,
			frames: self.frames,
			frames_per_group: 1,
		}
	}

	fn items(self) -> usize {
		self.tracks * self.frames
	}
}

impl fmt::Display for Shape {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}tracks-{}frames/{}", self.tracks, self.frames, self.size)
	}
}

fn shapes(tracks: &[usize]) -> Vec<Shape> {
	let mut shapes = Vec::new();
	for &tracks in tracks {
		for frames in [1, 16, 64] {
			for size in [256, 4096] {
				shapes.push(Shape { tracks, frames, size });
			}
		}
	}
	shapes
}

fn cmaf_shapes() -> Vec<Shape> {
	shapes(&[1, 4])
}

fn annexb_shapes() -> Vec<Shape> {
	shapes(&[1])
}

/// Publish `shape` and export it whole as `format`.
fn exported(format: &str, video: Video, payload: &Bytes) -> Bytes {
	let (source, stream, fixture) = RUNTIME.block_on(prepare(video, payload));
	let data = RUNTIME.block_on(export(format, source, stream, true));
	drop(fixture);
	assert!(!data.is_empty());
	data
}

/// `shape` exported as fMP4, checked to import both ways and to hold a fragment per frame.
fn cmaf_data(shape: Shape) -> Bytes {
	let data = exported("cmaf", shape.video(), &payload(shape.size));
	for passthrough in [false, true] {
		let (catalog, _finished) = import_cmaf(cmaf_importer(passthrough), &data);
		assert_eq!(catalog.snapshot().video.renditions.len(), shape.tracks);
	}
	use mp4_atom::DecodeMaybe;
	let mut cursor = std::io::Cursor::new(data.as_ref());
	let mut fragments = 0;
	while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor).unwrap() {
		if matches!(atom, mp4_atom::Any::Moof(_)) {
			fragments += 1;
		}
	}
	assert_eq!(fragments, shape.items());
	data
}

/// `shape` exported as Annex-B, checked to import and to hold every frame's bytes.
fn annexb_data(shape: Shape) -> Bytes {
	let payload = payload(shape.size);
	let data = exported("annexb", shape.video(), &payload);
	assert_eq!(data.len(), shape.size * shape.frames);
	let import = Annexb::new().decode(&payload, shape.frames);
	assert_eq!(import.catalog.snapshot().video.renditions.len(), 1);
	data
}

/// A video to publish afresh for each call to export.
type Exporting = Fresh<(Video, Bytes), Prepared>;

fn exporting(shape: Shape) -> Exporting {
	Fresh::new((shape.video(), payload(shape.size)), |(video, payload)| {
		RUNTIME.block_on(prepare(*video, payload))
	})
}

/// Exported fragments, and an fMP4 importer for each call.
type CmafImport = Fresh<Bytes, CmafImporter>;

fn cmaf_passthrough(shape: Shape) -> CmafImport {
	Fresh::new(cmaf_data(shape), |_| cmaf_importer(true))
}

fn cmaf_unwrapping(shape: Shape) -> CmafImport {
	Fresh::new(cmaf_data(shape), |_| cmaf_importer(false))
}

#[bench(args = cmaf_shapes(), setup = cmaf_passthrough, before = refill, items = Shape::items)]
fn cmaf_import(fresh: &mut CmafImport) -> (moq_mux::catalog::Producer, moq_mux::container::fmp4::Finished) {
	let (data, import) = fresh.take();
	import_cmaf(import, data)
}

/// Imports the same fragments one frame per sample.
#[bench(args = cmaf_shapes(), setup = cmaf_unwrapping, before = refill, items = Shape::items)]
fn cmaf_unwrap(fresh: &mut CmafImport) -> (moq_mux::catalog::Producer, moq_mux::container::fmp4::Finished) {
	let (data, import) = fresh.take();
	import_cmaf(import, data)
}

fn cmaf_exporting(shape: Shape) -> Exporting {
	cmaf_data(shape);
	exporting(shape)
}

#[bench(args = cmaf_shapes(), setup = cmaf_exporting, before = refill, items = Shape::items)]
fn cmaf_export(fresh: &mut Exporting) -> (Bytes, Fixture) {
	let (_, (source, stream, fixture)) = fresh.take();
	(RUNTIME.block_on(export("cmaf", source, stream, false)), fixture)
}

/// A payload, its frame count, and an Annex-B importer for each call.
type AnnexbImport = Fresh<(Bytes, usize), Annexb>;

fn annexb_importing(shape: Shape) -> AnnexbImport {
	annexb_data(shape);
	Fresh::new((payload(shape.size), shape.frames), |_| Annexb::new())
}

#[bench(args = annexb_shapes(), setup = annexb_importing, before = refill, items = Shape::items)]
fn annexb_import(fresh: &mut AnnexbImport) -> Imported {
	let ((payload, frames), import) = fresh.take();
	import.decode(payload, *frames)
}

fn annexb_exporting(shape: Shape) -> Exporting {
	annexb_data(shape);
	exporting(shape)
}

#[bench(args = annexb_shapes(), setup = annexb_exporting, before = refill, items = Shape::items)]
fn annexb_export(fresh: &mut Exporting) -> (Bytes, Fixture) {
	let (_, (source, stream, fixture)) = fresh.take();
	(RUNTIME.block_on(export("annexb", source, stream, false)), fixture)
}

/// Queued groups and frames in each, named `<groups>/<frames_per_group>`.
#[derive(Clone, Copy)]
struct Queued {
	groups: usize,
	frames_per_group: usize,
}

impl Queued {
	fn video(self) -> Video {
		Video {
			tracks: 1,
			frames: self.frames(),
			frames_per_group: self.frames_per_group,
		}
	}

	fn frames(self) -> usize {
		self.groups * self.frames_per_group
	}
}

impl fmt::Display for Queued {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}/{}", self.groups, self.frames_per_group)
	}
}

fn queued() -> Vec<Queued> {
	[1, 16, 64]
		.into_iter()
		.flat_map(|groups| {
			[1, 16].map(|frames_per_group| Queued {
				groups,
				frames_per_group,
			})
		})
		.collect()
}

fn queued_groups(case: Queued) -> Exporting {
	let payload = payload(256);
	let data = exported("annexb", case.video(), &payload);
	assert_eq!(data.len(), 256 * case.frames());
	Fresh::new((case.video(), payload), |(video, payload)| {
		RUNTIME.block_on(prepare(*video, payload))
	})
}

// Keep both dimensions independent: per-frame scans over queued groups grow with
// frames per group as well as group count, unlike one constant cost per group.
#[bench(args = queued(), setup = queued_groups, before = refill, items = Queued::frames)]
fn annexb_export_queued_groups(fresh: &mut Exporting) -> (Bytes, Fixture) {
	let (_, (source, stream, fixture)) = fresh.take();
	(RUNTIME.block_on(export("annexb", source, stream, false)), fixture)
}

/// Interleaved fragments, named `<tracks>tracks/<runs>runs-<size>bytes-<chunk>chunk`.
#[derive(Clone, Copy)]
struct Interleave {
	tracks: usize,
	runs: usize,
	size: usize,
	chunk: Chunk,
}

impl fmt::Display for Interleave {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(
			f,
			"{}tracks/{}runs-{}bytes-{}chunk",
			self.tracks, self.runs, self.size, self.chunk
		)
	}
}

fn interleaves() -> Vec<Interleave> {
	let mut cases = Vec::new();
	for tracks in [1, 4] {
		for size in [1, 4096] {
			for runs in [1, 16] {
				for chunk in [Chunk::Bytes(1), Chunk::Bytes(1024), Chunk::Whole] {
					cases.push(Interleave {
						tracks,
						runs,
						size,
						chunk,
					});
				}
			}
		}
	}
	cases
}

/// 16 interleaved fragments after an exported init, checked to import both ways.
fn interleaved_data(case: Interleave) -> Bytes {
	let payload = payload(4096);
	let video = Video {
		tracks: case.tracks,
		frames: 1,
		frames_per_group: 1,
	};
	let init = exported("cmaf", video, &payload);
	let data = interleaved(&init, case.tracks, case.runs, 16, &payload.slice(..case.size));
	for passthrough in [false, true] {
		let (catalog, _finished) = import_cmaf(cmaf_importer(passthrough), &data);
		assert_eq!(catalog.snapshot().video.renditions.len(), case.tracks);
	}
	data
}

fn interleaved_len(case: Interleave) -> usize {
	interleaved_data(case).len()
}

/// Interleaved fragments, the chunk size to import them in, and an fMP4 importer for each call.
type InterleavedImport = Fresh<(Bytes, usize), CmafImporter>;

fn interleaved_passthrough(case: Interleave) -> InterleavedImport {
	let data = interleaved_data(case);
	let chunk = case.chunk.size(data.len());
	Fresh::new((data, chunk), |_| cmaf_importer(true))
}

fn interleaved_unwrapping(case: Interleave) -> InterleavedImport {
	let data = interleaved_data(case);
	let chunk = case.chunk.size(data.len());
	Fresh::new((data, chunk), |_| cmaf_importer(false))
}

/// fMP4 passthrough of fragments whose tracks interleave their runs in one mdat, across track
/// and run count: each track copies only its own samples.
#[bench(args = interleaves(), setup = interleaved_passthrough, before = refill, bytes = interleaved_len)]
fn cmaf_interleaved_import(
	fresh: &mut InterleavedImport,
) -> (moq_mux::catalog::Producer, moq_mux::container::fmp4::Finished) {
	let ((data, chunk), import) = fresh.take();
	import_cmaf_chunks(import, data, *chunk)
}

/// The same fragments unwrapped into one frame per sample.
#[bench(args = interleaves(), setup = interleaved_unwrapping, before = refill, bytes = interleaved_len)]
fn cmaf_interleaved_unwrap(
	fresh: &mut InterleavedImport,
) -> (moq_mux::catalog::Producer, moq_mux::container::fmp4::Finished) {
	let ((data, chunk), import) = fresh.take();
	import_cmaf_chunks(import, data, *chunk)
}

/// Receive-side init and sample parsing, across metadata counts and media payload sizes.
mod cmaf_receive {
	use std::fmt;

	use bytes::Bytes;
	use moq_mux::container::Container;
	use moq_mux::container::fmp4::Wire;
	use mp4_atom::{Decode, Encode};
	use swole::bench;

	/// The init of the fixture's audio track alone, and that track's id.
	fn audio() -> (Vec<u8>, u32) {
		let mut source = include_bytes!("../src/container/fmp4/test_data/bbb.mp4").as_slice();
		mp4_atom::Ftyp::decode(&mut source).unwrap();
		let mut moov = mp4_atom::Moov::decode(&mut source).unwrap();
		let trak = moov
			.trak
			.iter()
			.find(|trak| trak.mdia.hdlr.handler.as_ref() == b"soun")
			.unwrap()
			.clone();
		let id = trak.tkhd.track_id;
		moov.trak = vec![trak];
		let mut init = Vec::new();
		moov.encode(&mut init).unwrap();
		(init, id)
	}

	fn audio_init() -> Vec<u8> {
		audio().0
	}

	#[bench(setup = audio_init)]
	fn init(init: &[u8]) -> Wire {
		Wire::from_init(init).unwrap()
	}

	/// Samples per fragment and bytes per sample, named `<samples>/<size>`.
	#[derive(Clone, Copy)]
	struct Samples {
		samples: usize,
		size: u32,
	}

	impl fmt::Display for Samples {
		fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
			write!(f, "{}/{}", self.samples, self.size)
		}
	}

	fn samples() -> Vec<Samples> {
		[1, 32, 1024]
			.into_iter()
			.flat_map(|samples| [1, 4096].map(|size| Samples { samples, size }))
			.collect()
	}

	/// The audio track's wire and one fragment of `case` samples.
	fn fragment(case: Samples) -> (Wire, moq_net::frame::Frame) {
		let Samples { samples, size } = case;
		let (init, id) = audio();
		let wire = Wire::from_init(&init).unwrap();
		let mut moof = mp4_atom::Moof {
			mfhd: mp4_atom::Mfhd { sequence_number: 1 },
			traf: vec![mp4_atom::Traf {
				tfhd: mp4_atom::Tfhd {
					track_id: id,
					..Default::default()
				},
				tfdt: Some(mp4_atom::Tfdt {
					base_media_decode_time: 0,
				}),
				trun: vec![mp4_atom::Trun {
					data_offset: Some(0),
					samples: vec![
						mp4_atom::TrunEntry {
							size: Some(size),
							duration: Some(1024),
							flags: Some(0),
							cts: None
						};
						samples
					]
					.into(),
				}],
				..Default::default()
			}],
		};
		let mut data = Vec::new();
		moof.encode(&mut data).unwrap();
		moof.traf[0].trun[0].data_offset = Some((data.len() + 8) as i32);
		data.clear();
		moof.encode(&mut data).unwrap();
		mp4_atom::Mdat {
			data: vec![1; size as usize * samples],
		}
		.encode(&mut data)
		.unwrap();
		let frame = moq_net::frame::Frame::new(moq_net::Timestamp::unscaled(0), Bytes::from(data));
		assert_eq!(wire.decode(frame.clone()).unwrap().len(), samples);
		(wire, frame)
	}

	#[bench(args = samples(), setup = fragment, items = |case: Samples| case.samples)]
	fn decode((wire, frame): &(Wire, moq_net::frame::Frame)) -> Vec<moq_mux::container::Frame> {
		wire.decode(frame.clone()).unwrap()
	}
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
		// The first renewal advertises the SI tracks in the catalog, a one-time publish that would
		// otherwise land in every timed iteration, since each starts from a fresh importer.
		this.tick();
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

/// Retained state and SI arrivals each call touches, named `<retained>/<touches>`.
#[derive(Clone, Copy)]
struct Retained {
	retained: u16,
	touches: u16,
}

impl fmt::Display for Retained {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}/{}", self.retained, self.touches)
	}
}

fn retained(sizes: [u16; 3]) -> Vec<Retained> {
	sizes
		.into_iter()
		.flat_map(|retained| [1, 16, 64].map(|touches| Retained { retained, touches }))
		.collect()
}

/// An importer holding `retained` state, and the SI arrival each touch delivers.
struct Si {
	import: SiImport,
	case: Retained,
	touch: fn(u16, u16) -> (usize, u8, u16),
}

impl Si {
	/// One accepted PCR interval plus `touches` SI arrivals against the retained state, returned
	/// so its drop stays outside the measurement.
	fn sweep(mut self) -> Self {
		let Retained { retained, touches } = self.case;
		let touch = self.touch;
		self.import.tick();
		self.import.repeat((0..touches).map(|key| touch(retained, key)));
		self
	}
}

fn renewal(case: Retained) -> Si {
	Si {
		import: SiImport::identities(case.retained),
		case,
		touch: |identities, id| (2, 0x80, id % identities),
	}
}

/// SI renewal across retained sub-table count and touched identities. Accepted
/// PCR advances the deadlines during the call without expiring the retained set.
#[bench(args = retained([1, 64, 4096]), setup = renewal, items = |case: Retained| case.touches)]
fn ts_si_renewal_pcr(si: Si) -> Si {
	si.sweep()
}

fn pending(case: Retained) -> Si {
	Si {
		import: SiImport::pending(case.retained),
		case,
		touch: |identities, id| (1, 0x42, id % identities),
	}
}

/// Pending SDT generations retain only their first section; the selected service
/// prevents a repeated partial set from committing as a sparse complete cycle.
#[bench(args = retained([1, 64, 4096]), setup = pending, items = |case: Retained| case.touches)]
fn ts_si_pending_pcr(si: Si) -> Si {
	si.sweep()
}

fn history(case: Retained) -> Si {
	Si {
		import: SiImport::history(case.retained, case.touches),
		case,
		touch: |history, key| SiImport::table_key(key % history),
	}
}

/// Distinct expired PID/table-key history versus current touched entries, using
/// only the four SI PIDs the importer actually captures and the full table-ID space.
#[bench(args = retained([1, 64, 1024]), setup = history, items = |case: Retained| case.touches)]
fn ts_si_table_history_pcr(si: Si) -> Si {
	si.sweep()
}

type MkvImporter = (moq_mux::catalog::Producer, moq_mux::container::mkv::Import);

fn mkv_importer() -> MkvImporter {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let import = moq_mux::container::mkv::Import::new(broadcast, catalog.reserve());
	(catalog, import)
}

fn import_mkv(
	(catalog, mut import): MkvImporter,
	data: &[u8],
	chunk: usize,
) -> (moq_mux::catalog::Producer, moq_mux::container::mkv::Finished) {
	for bytes in data.chunks(chunk) {
		import.decode(bytes).unwrap();
	}
	(catalog, import.finish().unwrap())
}

const OPUS: &[u8] = include_bytes!("../src/container/mkv/test_data/opus.webm");

/// The chunk size, and an MKV importer for each call.
type MkvChunks = Fresh<usize, MkvImporter>;

fn opus(chunk: Chunk) -> MkvChunks {
	Fresh::new(chunk.size(OPUS.len()), |_| mkv_importer())
}

/// Live WebM framing across transport chunk sizes, including byte-at-a-time metadata.
#[bench(
	args = [Chunk::Bytes(1), Chunk::Bytes(7), Chunk::Bytes(64), Chunk::Whole],
	setup = opus,
	before = refill,
	bytes = |_| OPUS.len(),
)]
fn mkv_chunks(fresh: &mut MkvChunks) -> (moq_mux::catalog::Producer, moq_mux::container::mkv::Finished) {
	let (chunk, import) = fresh.take();
	import_mkv(import, OPUS, *chunk)
}

/// A one-track VP9 WebM of `total` bytes of keyframe SimpleBlocks, each carrying `payload` bytes.
fn mkv_payload_file(payload: usize, total: usize) -> Vec<u8> {
	use webm_iterable::WebmWriter;
	use webm_iterable::matroska_spec::{Master, MatroskaSpec, SimpleBlock};

	let frame = vec![0x5a; payload];
	let blocks: Vec<MatroskaSpec> = (0..total / payload)
		.map(|index| SimpleBlock::new_uncheked(&frame, 1, (index % 1024) as i16, false, None, false, true).into())
		.collect();
	let mut data = Vec::new();
	let mut writer = WebmWriter::new(&mut data);
	writer
		.write(&MatroskaSpec::Ebml(Master::Full(vec![MatroskaSpec::DocType(
			"webm".into(),
		)])))
		.unwrap();
	writer.write(&MatroskaSpec::Segment(Master::Start)).unwrap();
	writer
		.write(&MatroskaSpec::Tracks(Master::Full(vec![MatroskaSpec::TrackEntry(
			Master::Full(vec![
				MatroskaSpec::TrackNumber(1),
				MatroskaSpec::TrackType(1),
				MatroskaSpec::CodecID("V_VP9".into()),
			]),
		)])))
		.unwrap();
	// Relative block timestamps are 16-bit, so each cluster holds a bounded run of them.
	for (cluster, blocks) in blocks.chunks(1024).enumerate() {
		let mut children = vec![MatroskaSpec::Timestamp(cluster as u64 * 1024)];
		children.extend_from_slice(blocks);
		writer.write(&MatroskaSpec::Cluster(Master::Full(children))).unwrap();
	}
	writer.write(&MatroskaSpec::Segment(Master::End)).unwrap();
	writer.flush().unwrap();
	drop(writer);
	data
}

/// Bytes per block and per transport chunk, named `payload-<payload>/chunk-<chunk>`.
#[derive(Clone, Copy)]
struct Payloads {
	payload: usize,
	chunk: Chunk,
}

impl fmt::Display for Payloads {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "payload-{}/chunk-{}", self.payload, self.chunk)
	}
}

fn payloads() -> Vec<Payloads> {
	[1024, 64 * 1024, 1024 * 1024]
		.into_iter()
		.flat_map(|payload| {
			[Chunk::Bytes(1500), Chunk::Bytes(64 * 1024), Chunk::Whole].map(|chunk| Payloads { payload, chunk })
		})
		.collect()
}

fn payload_file(case: Payloads) -> Vec<u8> {
	mkv_payload_file(case.payload, 4 * 1024 * 1024)
}

/// The file, its chunk size, and an MKV importer for each call.
type MkvPayloads = Fresh<(Vec<u8>, usize), MkvImporter>;

fn mkv_payload(case: Payloads) -> MkvPayloads {
	let data = payload_file(case);
	let chunk = case.chunk.size(data.len());
	Fresh::new((data, chunk), |_| mkv_importer())
}

/// MKV block payloads across payload and transport chunk sizes.
#[bench(args = payloads(), setup = mkv_payload, before = refill, bytes = |case| payload_file(case).len())]
fn mkv_payloads(fresh: &mut MkvPayloads) -> (moq_mux::catalog::Producer, moq_mux::container::mkv::Finished) {
	let ((data, chunk), import) = fresh.take();
	import_mkv(import, data, *chunk)
}

fn main() {
	swole::main();
}
