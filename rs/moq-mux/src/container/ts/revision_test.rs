//! A program whose PMT revises mid-stream, imported and exported again: the export announces
//! each revision with a PMT at the next `version_number`, its PAT unchanged.

use std::time::Duration;

use super::catalog::Ext;
use super::fixture::{Flags, MS, Mux};
use crate::container::Frame;

const PMT_PID: u16 = 0x20;
/// The PMT PID the export gives a program that recorded none.
const OWN_PMT_PID: u16 = 0x1000;
const CLOCK_PID: u16 = 0x1ff;
const AUDIO_PID: u16 = 0x101;
const DATA_PID: u16 = 0x102;
const OTHER_AUDIO_PID: u16 = 0x103;

const AUDIO: u8 = 0x0f;
const PRIVATE: u8 = 0x06;
const METADATA: u8 = 0x15;

/// ISO 639 language descriptors for English and French.
const ENG: &[u8] = &[0x0a, 0x04, b'e', b'n', b'g', 0x00];
const FRA: &[u8] = &[0x0a, 0x04, b'f', b'r', b'a', 0x00];
/// `metadata_descriptor` for asynchronous KLV (ISO 13818-1 2.6.60).
const KLV: &[u8] = &[0x26, 0x09, 0x01, 0x00, 0xff, b'K', b'L', b'V', b'A', 0x0f, 0x00];

/// How long the export holds each frame.
const DELAY: Duration = Duration::from_secs(2);

/// A PMT the export wrote: its `version_number`, PCR PID, program descriptors, and each
/// stream as `(stream_type, PID, descriptor bytes)`.
#[derive(Debug, PartialEq)]
struct Table {
	version: u8,
	pcr_pid: u16,
	info: Vec<u8>,
	streams: Vec<(u8, u16, Vec<u8>)>,
}

/// One span of the source: the streams its PMT lists, and how many bytes of program
/// descriptors it carries.
struct Span<'a> {
	streams: &'a [(u8, u16, &'a [u8])],
	info: usize,
}

/// Import `spans` one by one, each opened by a PAT and a PMT at the next version and running
/// for `intervals` PCRs 40 ms apart (the first for three seconds), with an AAC frame on each
/// audio stream and a PES on each private one every interval. The export polls after each,
/// as a live one keeps up with the catalog, and runs to its end after the last. Returns
/// every PMT it wrote, each run of repeats once.
async fn reexport(spans: &[Span<'_>], intervals: u64) -> Vec<Table> {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(
		&mut broadcast,
		crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
	)
	.unwrap();
	let mut import = super::Import::new(broadcast, catalog.reserve());

	let mut ticks = 1_000 * MS;
	let mut export = None;
	let mut frames = Vec::new();
	for (version, span) in spans.iter().enumerate() {
		let mut mux = Mux::default();
		mux.pat(&[(1, PMT_PID)])
			.pmt_at(version as u8 % 32, PMT_PID, 1, CLOCK_PID, span.streams, span.info);
		// The import publishes its catalog once it has measured the multiplex rate.
		let intervals = if version == 0 { 75 } else { intervals };
		for _ in 0..intervals {
			mux.packet(CLOCK_PID, Flags::pcr(ticks));
			for &(stream_type, pid, _) in span.streams {
				let payload = match stream_type {
					AUDIO => aac(ticks / 300 + 9_000, pid as u8),
					PRIVATE => pes(0xbd, pid as u8),
					_ => pes(0xfc, pid as u8),
				};
				mux.whole(pid, &payload);
			}
			ticks += 40 * MS;
		}
		import.decode(&mux.bytes()).unwrap();
		let export = match &mut export {
			Some(export) => export,
			None => {
				let source = crate::source::announced(&consumer).await;
				let opened = super::export_test::open_ts(source, crate::catalog::CatalogFormat::Hang)
					.await
					.unwrap();
				let export = export.insert(opened.with_delay(DELAY).with_replay());
				// The first frame out carries the tables built from this span alone.
				let first = tokio::time::timeout(DELAY * 2, export.next()).await;
				frames.push(first.expect("a first frame").unwrap().unwrap());
				export
			}
		};
		settle(export, &mut frames).await;
	}
	let mut mux = Mux::default();
	mux.packet(CLOCK_PID, Flags::pcr(ticks));
	import.decode(&mux.bytes()).unwrap();
	import.finish().unwrap();
	catalog.finish().unwrap();

	drain(&mut export.unwrap(), &mut frames).await;
	tables(&frames, PMT_PID)
}

/// Run `export` until it waits on its source, as a live one keeps up with each catalog
/// snapshot as it comes.
async fn settle<S: crate::catalog::Stream>(export: &mut super::Export<S>, frames: &mut Vec<Frame>)
where
	S::Ext: super::catalog::Catalog,
{
	while let Ok(frame) = tokio::time::timeout(Duration::from_millis(1), export.next()).await {
		frames.push(frame.unwrap().expect("the export runs on"));
	}
}

/// Run `export` to its end.
async fn drain<S: crate::catalog::Stream>(export: &mut super::Export<S>, frames: &mut Vec<Frame>)
where
	S::Ext: super::catalog::Catalog,
{
	loop {
		match tokio::time::timeout(DELAY * 10, export.next())
			.await
			.expect("the export ends")
		{
			Ok(Some(frame)) => frames.push(frame),
			Ok(None) => break,
			Err(err) => panic!("the export failed: {err}"),
		}
	}
}

/// An ADTS AAC frame presenting at `pts`, its payload filled with `fill`.
fn aac(pts: u64, fill: u8) -> Vec<u8> {
	let mut frame = super::adts::write_header(2, 48_000, 2, 64).unwrap().to_vec();
	frame.resize(7 + 64, fill);
	let field = [
		0x21 | ((pts >> 29) & 0x0e) as u8,
		(pts >> 22) as u8,
		1 | ((pts >> 14) & 0xfe) as u8,
		(pts >> 7) as u8,
		1 | ((pts << 1) & 0xfe) as u8,
	];
	let mut pes = vec![0, 0, 1, 0xc0];
	pes.extend_from_slice(&((3 + field.len() + frame.len()) as u16).to_be_bytes());
	pes.extend_from_slice(&[0x80, 0x80, field.len() as u8]);
	pes.extend_from_slice(&field);
	pes.extend_from_slice(&frame);
	pes
}

/// A PES of `stream_id` without a PTS, its payload filled with `fill`.
fn pes(stream_id: u8, fill: u8) -> Vec<u8> {
	let payload = [fill; 24];
	let mut pes = vec![0, 0, 1, stream_id];
	pes.extend_from_slice(&((3 + payload.len()) as u16).to_be_bytes());
	pes.extend_from_slice(&[0x84, 0x00, 0x00]);
	pes.extend_from_slice(&payload);
	pes
}

/// Every packet in `frames`.
fn packets(frames: &[Frame]) -> impl Iterator<Item = &[u8; 188]> {
	frames
		.iter()
		.flat_map(|frame| frame.payload.as_chunks::<188>().0.iter())
}

/// The PID of `packet`.
fn pid(packet: &[u8; 188]) -> u16 {
	u16::from(packet[1] & 0x1f) << 8 | u16::from(packet[2])
}

/// Every PMT on `pmt_pid` in `frames`, each run of repeats once.
fn tables(frames: &[Frame], pmt_pid: u16) -> Vec<Table> {
	let mut out: Vec<Table> = Vec::new();
	for packet in packets(frames) {
		if pid(packet) != pmt_pid || packet[1] & 0x40 == 0 {
			continue;
		}
		let mut at = 4;
		if packet[3] & 0x20 != 0 {
			at += 1 + usize::from(packet[4]);
		}
		let section = &packet[at + 1 + usize::from(packet[at])..];
		let section = &section[..3 + (usize::from(section[1] & 0x0f) << 8 | usize::from(section[2]))];
		let pmt = super::psi::Pmt::parse(section).expect("a whole PMT");
		let bytes = |descriptors: &[super::catalog::Descriptor]| {
			descriptors
				.iter()
				.flat_map(|d| [vec![d.tag, d.data.len() as u8], d.data.to_vec()].concat())
				.collect::<Vec<u8>>()
		};
		let table = Table {
			version: (section[5] >> 1) & 0x1f,
			pcr_pid: pmt.pcr_pid.map_or(0x1fff, |pid| pid.as_u16()),
			info: bytes(&pmt.program_info),
			streams: pmt
				.streams
				.iter()
				.map(|es| (es.stream_type, es.pid.as_u16(), bytes(&es.descriptors)))
				.collect(),
		};
		if out.last() != Some(&table) {
			out.push(table);
		}
	}
	out
}

/// Each table's version and streams as `(stream_type, PID)`.
fn layouts(tables: &[Table]) -> Vec<(u8, Vec<(u8, u16)>)> {
	tables
		.iter()
		.map(|table| {
			let streams = table
				.streams
				.iter()
				.map(|&(stream_type, pid, _)| (stream_type, pid))
				.collect();
			(table.version, streams)
		})
		.collect()
}

#[tokio::test(start_paused = true)]
async fn a_stream_added_mid_stream_revises_the_pmt() {
	let tables = reexport(
		&[
			Span {
				streams: &[(AUDIO, AUDIO_PID, &[])],
				info: 0,
			},
			Span {
				streams: &[(AUDIO, AUDIO_PID, &[]), (PRIVATE, DATA_PID, &[])],
				info: 0,
			},
		],
		25,
	)
	.await;
	assert_eq!(
		layouts(&tables),
		vec![
			(0, vec![(AUDIO, AUDIO_PID)]),
			(1, vec![(AUDIO, AUDIO_PID), (PRIVATE, DATA_PID)]),
		]
	);
	assert!(
		tables.iter().all(|table| table.pcr_pid == AUDIO_PID),
		"the clock stays put"
	);
}

#[tokio::test(start_paused = true)]
async fn a_stream_removed_mid_stream_leaves_the_pmt_once_it_ends() {
	let tables = reexport(
		&[
			Span {
				streams: &[(AUDIO, AUDIO_PID, &[]), (PRIVATE, DATA_PID, &[])],
				info: 0,
			},
			Span {
				streams: &[(AUDIO, AUDIO_PID, &[])],
				info: 0,
			},
		],
		25,
	)
	.await;
	assert_eq!(
		layouts(&tables),
		vec![
			(0, vec![(AUDIO, AUDIO_PID), (PRIVATE, DATA_PID)]),
			(1, vec![(AUDIO, AUDIO_PID)]),
		]
	);
}

/// A changed `stream_type` keeps the PID, and the PMT names the new type at the next version.
#[tokio::test(start_paused = true)]
async fn a_retyped_stream_revises_its_entry_in_place() {
	let tables = reexport(
		&[
			Span {
				streams: &[(AUDIO, AUDIO_PID, &[]), (PRIVATE, DATA_PID, &[])],
				info: 0,
			},
			Span {
				streams: &[(AUDIO, AUDIO_PID, &[]), (METADATA, DATA_PID, KLV)],
				info: 0,
			},
		],
		25,
	)
	.await;
	assert_eq!(
		layouts(&tables),
		vec![
			(0, vec![(AUDIO, AUDIO_PID), (PRIVATE, DATA_PID)]),
			(1, vec![(AUDIO, AUDIO_PID), (METADATA, DATA_PID)]),
		]
	);
	assert_eq!(tables[1].streams[1].2, KLV);
}

/// An audio language retag reopens the rendition, which takes the PID and the clock over.
#[tokio::test(start_paused = true)]
async fn a_descriptor_change_revises_the_entry_and_keeps_the_clock() {
	let tables = reexport(
		&[
			Span {
				streams: &[(AUDIO, AUDIO_PID, ENG)],
				info: 0,
			},
			Span {
				streams: &[(AUDIO, AUDIO_PID, FRA)],
				info: 0,
			},
		],
		25,
	)
	.await;
	assert_eq!(
		tables,
		vec![
			Table {
				version: 0,
				pcr_pid: AUDIO_PID,
				info: Vec::new(),
				streams: vec![(AUDIO, AUDIO_PID, ENG.to_vec())],
			},
			Table {
				version: 1,
				pcr_pid: AUDIO_PID,
				info: Vec::new(),
				streams: vec![(AUDIO, AUDIO_PID, FRA.to_vec())],
			},
		]
	);
}

/// The PCR leaves a PID only with the track it rode on, for the next audio track.
#[tokio::test(start_paused = true)]
async fn the_clock_moves_only_when_its_track_retires() {
	let tables = reexport(
		&[
			Span {
				streams: &[(AUDIO, AUDIO_PID, ENG), (AUDIO, OTHER_AUDIO_PID, FRA)],
				info: 0,
			},
			Span {
				streams: &[(AUDIO, OTHER_AUDIO_PID, FRA)],
				info: 0,
			},
		],
		25,
	)
	.await;
	let clocks: Vec<(u8, u16)> = tables.iter().map(|table| (table.version, table.pcr_pid)).collect();
	assert_eq!(clocks, vec![(0, AUDIO_PID), (1, OTHER_AUDIO_PID)]);
	assert_eq!(tables[1].streams, vec![(AUDIO, OTHER_AUDIO_PID, FRA.to_vec())]);
}

/// A PID the PMT dropped can come back as another stream.
#[tokio::test(start_paused = true)]
async fn a_pid_removed_can_return_as_another_stream() {
	let tables = reexport(
		&[
			Span {
				streams: &[(AUDIO, AUDIO_PID, &[]), (PRIVATE, DATA_PID, &[])],
				info: 0,
			},
			Span {
				streams: &[(AUDIO, AUDIO_PID, &[])],
				info: 0,
			},
			Span {
				streams: &[(AUDIO, AUDIO_PID, &[]), (METADATA, DATA_PID, KLV)],
				info: 0,
			},
		],
		25,
	)
	.await;
	assert_eq!(
		layouts(&tables),
		vec![
			(0, vec![(AUDIO, AUDIO_PID), (PRIVATE, DATA_PID)]),
			(1, vec![(AUDIO, AUDIO_PID)]),
			(2, vec![(AUDIO, AUDIO_PID), (METADATA, DATA_PID)]),
		]
	);
}

/// A changed `program_info` is a revision of its own.
#[tokio::test(start_paused = true)]
async fn a_program_descriptor_change_revises_the_pmt() {
	let tables = reexport(
		&[
			Span {
				streams: &[(AUDIO, AUDIO_PID, &[])],
				info: 0,
			},
			Span {
				streams: &[(AUDIO, AUDIO_PID, &[])],
				info: 8,
			},
		],
		25,
	)
	.await;
	let versions: Vec<(u8, Vec<u8>)> = tables.iter().map(|table| (table.version, table.info.clone())).collect();
	assert_eq!(
		versions,
		vec![(0, Vec::new()), (1, vec![0x80, 6, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa])]
	);
}

/// Layouts that change faster than the export lags behind its source still reach the wire
/// as a run of versions, each one up from the last and differing from it: a table copied into
/// a frame queued behind a revision never goes out after it, and a revision overtaken before
/// any frame carried it takes no version.
#[tokio::test(start_paused = true)]
async fn each_version_out_follows_the_last_by_one_change() {
	let with: &[(u8, u16, &[u8])] = &[(AUDIO, AUDIO_PID, &[]), (PRIVATE, DATA_PID, &[])];
	let without: &[(u8, u16, &[u8])] = &[(AUDIO, AUDIO_PID, &[])];
	let spans: Vec<Span> = (0..12)
		.map(|i| Span {
			streams: if i % 2 == 0 { with } else { without },
			info: 0,
		})
		.collect();
	let tables = reexport(&spans, 10).await;
	assert!(tables.len() > 2, "{tables:?}");
	for pair in tables.windows(2) {
		assert_eq!(pair[1].version, (pair[0].version + 1) % 32, "{tables:?}");
		assert_ne!(pair[1].streams, pair[0].streams, "{tables:?}");
	}
}

/// The `version_number` runs on from 31 to 0.
#[tokio::test(start_paused = true)]
async fn the_version_wraps_after_31() {
	let with: &[(u8, u16, &[u8])] = &[(AUDIO, AUDIO_PID, &[]), (PRIVATE, DATA_PID, &[])];
	let without: &[(u8, u16, &[u8])] = &[(AUDIO, AUDIO_PID, &[])];
	let spans: Vec<Span> = (0..34)
		.map(|i| Span {
			streams: if i % 2 == 0 { with } else { without },
			info: 0,
		})
		.collect();
	// Each span outlasts the export's lag behind its source: a PID dropped for less is
	// listed again before the table dropping it goes out.
	let tables = reexport(&spans, 40).await;
	let versions: Vec<u8> = tables.iter().map(|table| table.version).collect();
	let expected: Vec<u8> = (0..34).map(|i| i % 32).collect();
	assert_eq!(versions, expected);
	for (i, table) in tables.iter().enumerate() {
		assert_eq!(table.streams.len(), if i % 2 == 0 { 2 } else { 1 }, "revision {i}");
	}
}

/// A track's writer, its frames in the legacy container.
type Writer = crate::container::Producer<crate::catalog::hang::Container>;

/// Open an AAC track on `broadcast` and list it in `catalog`, with `pid` recorded if given.
fn aac_track(
	broadcast: &mut moq_net::broadcast::Producer,
	catalog: &mut crate::catalog::Producer<Ext>,
	pid: Option<u16>,
) -> (String, Writer) {
	let track = broadcast
		.create_track(
			broadcast.unique_name(".aac"),
			hang::container::track_info(hang::catalog::PRIORITY.audio),
		)
		.unwrap();
	let name = track.name().to_string();
	let mut config = hang::catalog::AudioConfig::new(hang::catalog::AAC { profile: 2 }, 48_000, 2);
	config.container = hang::catalog::Container::Legacy;
	let mut guard = catalog.modify().unwrap();
	guard.audio.renditions.insert(name.clone(), config);
	if let Some(pid) = pid {
		guard
			.ext
			.mpegts
			.tracks
			.insert(name.clone(), super::catalog::Track::new(pid));
	}
	drop(guard);
	let container = crate::catalog::hang::Container::Legacy(crate::container::Kind::Audio);
	(name, Writer::new(track, container))
}

/// Write an AAC frame at `millis` to each of `tracks`.
fn write_aac(tracks: &mut [&mut Writer], millis: u64) {
	for track in tracks {
		write_marked(track, millis, 0x21);
	}
}

/// Write an AAC frame at `millis` to `track`, its payload filled with `mark`.
fn write_marked(track: &mut Writer, millis: u64, mark: u8) {
	let mut payload = vec![mark; 16];
	payload[0] = 0x21;
	track
		.write(Frame {
			timestamp: moq_net::Timestamp::from_micros(millis * 1_000).unwrap(),
			duration: None,
			payload: bytes::Bytes::from(payload),
			keyframe: true,
		})
		.unwrap();
	track.cut(None).unwrap();
}

/// The marks of the frames ([`write_marked`]) that went out on `pid`, in order.
fn marks(frames: &[Frame], pid: u16, marks: &[u8]) -> Vec<u8> {
	let mut payload = Vec::new();
	for packet in packets(frames).filter(|packet| self::pid(packet) == pid && packet[3] & 0x10 != 0) {
		let at = if packet[3] & 0x20 != 0 {
			5 + usize::from(packet[4])
		} else {
			4
		};
		payload.extend_from_slice(&packet[at..]);
	}
	let mut out = Vec::new();
	for (at, run) in payload.windows(15).enumerate() {
		let mark = run[0];
		if marks.contains(&mark) && run.iter().all(|&byte| byte == mark) && (at == 0 || payload[at - 1] != mark) {
			out.push(mark);
		}
	}
	out
}

/// The export of `consumer`.
async fn export_of(consumer: &moq_net::broadcast::Consumer) -> super::Export<crate::catalog::Consumer<Ext>> {
	let source = crate::source::announced(consumer).await;
	let opened = super::export_test::open_ts(source, crate::catalog::CatalogFormat::Hang)
		.await
		.unwrap();
	opened.with_delay(DELAY).with_replay()
}

/// Open the export of `consumer` once its first frame is out.
async fn open(
	consumer: &moq_net::broadcast::Consumer,
	frames: &mut Vec<Frame>,
) -> super::Export<crate::catalog::Consumer<Ext>> {
	let mut export = export_of(consumer).await;
	let out = tokio::time::timeout(DELAY * 2, export.next()).await;
	frames.push(out.expect("the tables went out").unwrap().unwrap());
	settle(&mut export, frames).await;
	export
}

/// Take `name` out of the catalog.
fn remove(catalog: &mut crate::catalog::Producer<Ext>, name: &str) {
	let mut guard = catalog.modify().unwrap();
	guard.audio.renditions.remove(name);
	guard.ext.mpegts.tracks.remove(name);
}

/// Under sync the tables stay as laid, so a track added after them fails the export.
#[tokio::test(start_paused = true)]
async fn a_track_added_under_sync_fails_the_export() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(
		&mut broadcast,
		crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
	)
	.unwrap();
	let (_, mut first) = aac_track(&mut broadcast, &mut catalog, Some(AUDIO_PID));
	for millis in (0..3_000).step_by(20) {
		write_aac(&mut [&mut first], millis);
	}
	let source = crate::source::announced(&consumer).await;
	let opened = super::export_test::open_ts(source, crate::catalog::CatalogFormat::Hang)
		.await
		.unwrap();
	let mut export = opened.with_delay(DELAY).with_mux_rate(2_000_000).with_sync();
	let out = tokio::time::timeout(DELAY * 2, export.next()).await;
	out.expect("the tables went out").unwrap().unwrap();

	let _second = aac_track(&mut broadcast, &mut catalog, Some(OTHER_AUDIO_PID));
	let err = tokio::time::timeout(DELAY * 2, async {
		loop {
			if let Err(err) = export.next().await {
				break err;
			}
		}
	})
	.await
	.expect("the export reads the catalog update");
	assert!(err.to_string().contains("under --sync"), "{err}");
}

/// A track the catalog lists after the tables went out, but before its PID is recorded,
/// takes the frames it holds along to the recorded PID when that arrives.
#[tokio::test(start_paused = true)]
async fn a_pid_recorded_after_its_track_takes_the_held_frames_along() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(
		&mut broadcast,
		crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
	)
	.unwrap();
	let (_, mut first) = aac_track(&mut broadcast, &mut catalog, Some(AUDIO_PID));
	for millis in (0..3_000).step_by(20) {
		write_aac(&mut [&mut first], millis);
	}
	let source = crate::source::announced(&consumer).await;
	let opened = super::export_test::open_ts(source, crate::catalog::CatalogFormat::Hang)
		.await
		.unwrap();
	let mut export = opened.with_delay(DELAY).with_replay();
	let out = tokio::time::timeout(DELAY * 2, export.next()).await;
	let mut frames = vec![out.expect("the tables went out").unwrap().unwrap()];
	settle(&mut export, &mut frames).await;

	let (name, mut second) = aac_track(&mut broadcast, &mut catalog, None);
	settle(&mut export, &mut frames).await;
	for millis in (3_000..3_500).step_by(20) {
		write_aac(&mut [&mut first, &mut second], millis);
	}
	settle(&mut export, &mut frames).await;
	catalog
		.modify()
		.unwrap()
		.ext
		.mpegts
		.tracks
		.insert(name, super::catalog::Track::new(OTHER_AUDIO_PID));
	settle(&mut export, &mut frames).await;
	for millis in (3_500..6_000).step_by(20) {
		write_aac(&mut [&mut first, &mut second], millis);
	}
	first.finish().unwrap();
	second.finish().unwrap();
	catalog.finish().unwrap();
	drain(&mut export, &mut frames).await;

	let tables = tables(&frames, OWN_PMT_PID);
	assert_eq!(
		tables.last().expect("a PMT").streams,
		vec![(AUDIO, AUDIO_PID, Vec::new()), (AUDIO, OTHER_AUDIO_PID, Vec::new())]
	);
	let carried: std::collections::BTreeSet<u16> = packets(&frames)
		.filter(|packet| packet[1] & 0x40 != 0 && packet[3] & 0x10 != 0)
		.filter(|packet| {
			let at = if packet[3] & 0x20 != 0 {
				5 + usize::from(packet[4])
			} else {
				4
			};
			packet[at..].starts_with(&[0, 0, 1])
		})
		.map(pid)
		.collect();
	assert_eq!(
		carried,
		[AUDIO_PID, OTHER_AUDIO_PID].into(),
		"no PES left on another PID"
	);
}

/// The next frame `export` writes.
async fn next<S: crate::catalog::Stream>(export: &mut super::Export<S>) -> Frame
where
	S::Ext: super::catalog::Catalog,
{
	let out = tokio::time::timeout(DELAY * 4, export.next()).await;
	out.expect("a frame").unwrap().expect("the export runs on")
}

/// A track not listed yet, whose first frame is held behind the old generation's tail, takes
/// that frame along to the PID recorded meanwhile.
#[tokio::test(start_paused = true)]
async fn a_pid_recorded_while_its_frame_is_held_takes_the_frame_along() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(
		&mut broadcast,
		crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
	)
	.unwrap();
	let (_, mut first) = aac_track(&mut broadcast, &mut catalog, Some(AUDIO_PID));
	for millis in (0..3_000).step_by(20) {
		write_aac(&mut [&mut first], millis);
	}
	let mut frames = Vec::new();
	let mut export = open(&consumer, &mut frames).await;
	while export.buffered() > 0 {
		frames.push(next(&mut export).await);
	}
	let (name, mut second) = aac_track(&mut broadcast, &mut catalog, None);
	settle(&mut export, &mut frames).await;
	write_aac(&mut [&mut second], 3_000);
	for millis in (3_020..3_500).step_by(20) {
		write_aac(&mut [&mut first, &mut second], millis);
	}
	settle(&mut export, &mut frames).await;
	tokio::time::advance(DELAY * 2).await;
	let held = export.hold_next().expect("a frame due");
	assert_ne!(held, AUDIO_PID, "the second track's frame is held");
	catalog
		.modify()
		.unwrap()
		.ext
		.mpegts
		.tracks
		.insert(name, super::catalog::Track::new(OTHER_AUDIO_PID));
	first.finish().unwrap();
	second.finish().unwrap();
	catalog.finish().unwrap();
	drain(&mut export, &mut frames).await;

	assert_eq!(
		tables(&frames, OWN_PMT_PID).last().expect("a PMT").streams,
		vec![(AUDIO, AUDIO_PID, Vec::new()), (AUDIO, OTHER_AUDIO_PID, Vec::new())]
	);
}

/// A track taking a PID over goes out behind everything the track before it there sent, even a
/// tail sent further ahead of its decode time than the new track's first frame: none of the
/// old stream goes out under the PMT revised for the new one.
#[tokio::test(start_paused = true)]
async fn a_track_taking_a_pid_over_goes_out_behind_the_tail_before_it() {
	const OLD: u8 = 0xa5;
	const NEW: u8 = 0x5a;
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(
		&mut broadcast,
		crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
	)
	.unwrap();
	let (name, mut old) = aac_track(&mut broadcast, &mut catalog, Some(AUDIO_PID));
	let (_, mut other) = aac_track(&mut broadcast, &mut catalog, Some(OTHER_AUDIO_PID));
	let mut frames = Vec::new();
	let start = tokio::time::Instant::now();
	let mut export = export_of(&consumer).await;
	live(
		&mut export,
		&mut frames,
		start,
		&mut [(&mut old, OLD), (&mut other, 0x21)],
		0,
		3_000,
	)
	.await;

	// The old stream's tail goes out ahead of its time, running on past where the new one starts.
	for millis in (3_000..3_800).step_by(20) {
		write_marked(&mut old, millis, OLD);
	}
	old.finish().unwrap();
	remove(&mut catalog, &name);
	settle(&mut export, &mut frames).await;
	let (_, mut new) = aac_track(&mut broadcast, &mut catalog, Some(AUDIO_PID));
	settle(&mut export, &mut frames).await;
	live(
		&mut export,
		&mut frames,
		start,
		&mut [(&mut new, NEW), (&mut other, 0x21)],
		3_000,
		5_000,
	)
	.await;
	new.finish().unwrap();
	other.finish().unwrap();
	catalog.finish().unwrap();
	drain(&mut export, &mut frames).await;

	let marks = marks(&frames, AUDIO_PID, &[OLD, NEW]);
	let first = marks
		.iter()
		.position(|&mark| mark == NEW)
		.expect("the new stream went out");
	assert!(marks[..first].contains(&OLD), "the old stream went out first");
	assert!(
		marks[first..].iter().all(|&mark| mark == NEW),
		"the old stream's tail went out after the new one began: {marks:x?}"
	);
}

/// A PID the PMT stops listing is gone rather than quiet, so a stream returning on it counts
/// its silence from its own listing, not from the last unit of the stream before it.
#[tokio::test(start_paused = true)]
async fn a_pid_that_leaves_the_pmt_is_gone_until_it_returns() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(
		&mut broadcast,
		crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
	)
	.unwrap();
	let (name, mut old) = aac_track(&mut broadcast, &mut catalog, Some(AUDIO_PID));
	let (_, mut other) = aac_track(&mut broadcast, &mut catalog, Some(OTHER_AUDIO_PID));
	let mut frames = Vec::new();
	let start = tokio::time::Instant::now();
	let mut export = export_of(&consumer).await;
	live(
		&mut export,
		&mut frames,
		start,
		&mut [(&mut old, 0x21), (&mut other, 0x21)],
		0,
		2_000,
	)
	.await;
	old.finish().unwrap();
	remove(&mut catalog, &name);
	live(&mut export, &mut frames, start, &mut [(&mut other, 0x21)], 2_000, 5_000).await;
	assert!(!export.stats().streams.contains_key(&AUDIO_PID), "the PID left the PMT");
	assert_eq!(
		export.liveness(AUDIO_PID).1,
		None,
		"a PID the PMT left is gone, not quiet"
	);

	let (_, mut new) = aac_track(&mut broadcast, &mut catalog, Some(AUDIO_PID));
	settle(&mut export, &mut frames).await;
	live(
		&mut export,
		&mut frames,
		start,
		&mut [(&mut new, 0x21), (&mut other, 0x21)],
		5_000,
		7_500,
	)
	.await;
	let (_, quiet) = export.liveness(AUDIO_PID);
	assert!(
		quiet.is_some_and(|quiet| quiet < Duration::from_secs(1)),
		"the returned stream is quiet for {quiet:?}"
	);
}

/// A track whose first frame is held behind a generation boundary, then moved onto a PID taken
/// over, goes out behind the tail there all the same: the held frame drops.
#[tokio::test(start_paused = true)]
async fn a_held_frame_moved_onto_a_pid_taken_over_waits_for_the_tail() {
	const OLD: u8 = 0xa5;
	const NEW: u8 = 0x5a;
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(
		&mut broadcast,
		crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
	)
	.unwrap();
	let (old_name, mut old) = aac_track(&mut broadcast, &mut catalog, Some(AUDIO_PID));
	for millis in (0..3_000).step_by(20) {
		write_marked(&mut old, millis, OLD);
	}
	let mut frames = Vec::new();
	let mut export = open(&consumer, &mut frames).await;
	while export.buffered() > 0 {
		frames.push(next(&mut export).await);
	}
	let (name, mut new) = aac_track(&mut broadcast, &mut catalog, None);
	settle(&mut export, &mut frames).await;
	write_marked(&mut new, 3_000, NEW);
	for millis in (3_020..3_500).step_by(20) {
		write_marked(&mut old, millis, OLD);
		write_marked(&mut new, millis, NEW);
	}
	settle(&mut export, &mut frames).await;
	tokio::time::advance(DELAY * 2).await;
	let held = export.hold_next().expect("a frame due");
	assert_ne!(held, AUDIO_PID, "the new track's frame is held");
	{
		let mut guard = catalog.modify().unwrap();
		guard.audio.renditions.remove(&old_name);
		guard.ext.mpegts.tracks.remove(&old_name);
		guard
			.ext
			.mpegts
			.tracks
			.insert(name, super::catalog::Track::new(AUDIO_PID));
	}
	for millis in (3_500..4_000).step_by(20) {
		write_marked(&mut old, millis, OLD);
		write_marked(&mut new, millis, NEW);
	}
	old.finish().unwrap();
	new.finish().unwrap();
	catalog.finish().unwrap();
	drain(&mut export, &mut frames).await;

	let marks = marks(&frames, AUDIO_PID, &[OLD, NEW]);
	let first = marks
		.iter()
		.position(|&mark| mark == NEW)
		.expect("the new stream went out");
	assert!(
		marks[first..].iter().all(|&mark| mark == NEW),
		"the old stream's tail went out after the new one began: {marks:x?}"
	);
}

/// A track that left the catalog while waiting to take a PID over goes once a later one takes
/// its place there, so such tracks do not pile up behind a stream that never ends.
#[tokio::test(start_paused = true)]
async fn a_track_waiting_to_take_a_pid_over_goes_once_superseded() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(
		&mut broadcast,
		crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
	)
	.unwrap();
	let (name, mut old) = aac_track(&mut broadcast, &mut catalog, Some(AUDIO_PID));
	let mut frames = Vec::new();
	let start = tokio::time::Instant::now();
	let mut export = export_of(&consumer).await;
	live(&mut export, &mut frames, start, &mut [(&mut old, 0x21)], 0, 2_000).await;
	remove(&mut catalog, &name);
	let mut unended = Vec::new();
	for round in 0..4 {
		let (name, mut successor) = aac_track(&mut broadcast, &mut catalog, Some(AUDIO_PID));
		let from = 2_000 + round * 500;
		live(
			&mut export,
			&mut frames,
			start,
			&mut [(&mut old, 0x21), (&mut successor, 0x21)],
			from,
			from + 500,
		)
		.await;
		remove(&mut catalog, &name);
		unended.push(successor);
	}
	settle(&mut export, &mut frames).await;
	assert_eq!(
		export.tracks(),
		2,
		"the stream that never ends, and the last track waiting behind it"
	);
}

/// A track that leaves the catalog drains under the program descriptors it was listed with: the
/// PMT carries them beside the revision's own until it ends.
#[tokio::test(start_paused = true)]
async fn a_track_leaving_drains_under_the_program_descriptors_it_was_listed_with() {
	let cuei = super::catalog::Descriptor {
		tag: 0x05,
		data: bytes::Bytes::from_static(b"CUEI"),
	};
	let other = super::catalog::Descriptor {
		tag: 0x80,
		data: bytes::Bytes::from_static(&[0xaa, 0xaa]),
	};
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(
		&mut broadcast,
		crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
	)
	.unwrap();
	catalog.modify().unwrap().ext.mpegts.program_descriptors = vec![cuei];
	let (_, mut first) = aac_track(&mut broadcast, &mut catalog, Some(AUDIO_PID));
	let (name, mut leaving) = aac_track(&mut broadcast, &mut catalog, Some(OTHER_AUDIO_PID));
	let mut frames = Vec::new();
	let start = tokio::time::Instant::now();
	let mut export = export_of(&consumer).await;
	live(
		&mut export,
		&mut frames,
		start,
		&mut [(&mut first, 0x21), (&mut leaving, 0x21)],
		0,
		3_000,
	)
	.await;
	{
		let mut guard = catalog.modify().unwrap();
		guard.audio.renditions.remove(&name);
		guard.ext.mpegts.tracks.remove(&name);
		guard.ext.mpegts.program_descriptors = vec![other];
	}
	leaving.finish().unwrap();
	live(&mut export, &mut frames, start, &mut [(&mut first, 0x21)], 3_000, 6_000).await;
	first.finish().unwrap();
	catalog.finish().unwrap();
	drain(&mut export, &mut frames).await;

	let versions: Vec<(u8, Vec<u8>, Vec<u16>)> = tables(&frames, OWN_PMT_PID)
		.into_iter()
		.map(|table| (table.version, table.info, table.streams.iter().map(|es| es.1).collect()))
		.collect();
	let (cuei, other) = (b"\x05\x04CUEI".to_vec(), vec![0x80, 2, 0xaa, 0xaa]);
	assert_eq!(
		versions,
		vec![
			(0, cuei.clone(), vec![AUDIO_PID, OTHER_AUDIO_PID]),
			(1, [other.clone(), cuei].concat(), vec![AUDIO_PID, OTHER_AUDIO_PID]),
			(2, other, vec![AUDIO_PID]),
		]
	);
}

/// Write a frame stamped `millis`, filled with its mark, to each of `tracks` for each 20 ms
/// from `from` to `to`, `millis` after `start`, as a live source does, the export keeping up.
async fn live<S: crate::catalog::Stream>(
	export: &mut super::Export<S>,
	frames: &mut Vec<Frame>,
	start: tokio::time::Instant,
	tracks: &mut [(&mut Writer, u8)],
	from: u64,
	to: u64,
) where
	S::Ext: super::catalog::Catalog,
{
	for millis in (from..to).step_by(20) {
		tokio::time::sleep_until(start + Duration::from_millis(millis)).await;
		for (track, mark) in tracks.iter_mut() {
			write_marked(track, millis, *mark);
		}
		settle(export, frames).await;
	}
}
