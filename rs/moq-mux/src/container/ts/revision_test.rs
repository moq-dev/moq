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

/// A PMT the export wrote: its program number, `version_number`, PCR PID, program descriptors,
/// and each stream as `(stream_type, PID, descriptor bytes)`.
#[derive(Debug, PartialEq)]
struct Table {
	program: u16,
	version: u8,
	pcr_pid: u16,
	info: Vec<u8>,
	streams: Vec<(u8, u16, Vec<u8>)>,
}

/// One span of the source: the streams its PMT lists.
struct Span<'a> {
	streams: &'a [(u8, u16, &'a [u8])],
}

/// Import `spans` one by one, each opened by a PAT and a PMT at the next version and running
/// for `intervals` PCRs 40 ms apart (the first for three seconds), with an AAC frame on each
/// audio stream and a PES on each private one every interval. The export polls after each,
/// as a live one keeps up with the catalog, and runs to its end after the last. Returns
/// every PMT it wrote, each run of repeats once.
async fn reexport(spans: &[Span<'_>], intervals: u64) -> Vec<Table> {
	let spans: Vec<_> = spans.iter().map(|span| (PMT_PID, span)).collect();
	tables(&reexport_frames(&spans, intervals).await, PMT_PID)
}

/// [`reexport`], each span's PAT naming its PMT on the PID beside it. Returns every frame
/// the export wrote.
async fn reexport_frames(spans: &[(u16, &Span<'_>)], intervals: u64) -> Vec<Frame> {
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
	for (version, &(pmt_pid, span)) in spans.iter().enumerate() {
		let mut mux = Mux::default();
		mux.pat(&[(1, pmt_pid)])
			.pmt_at(version as u8 % 32, pmt_pid, 1, CLOCK_PID, span.streams, 0);
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
				let first = moq_clock::timeout(DELAY * 2, export.next()).await;
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
	frames
}

/// Run `export` until it waits on its source, as a live one keeps up with each catalog
/// snapshot as it comes.
async fn settle<S: crate::catalog::Stream>(export: &mut super::Export<S>, frames: &mut Vec<Frame>)
where
	S::Ext: super::catalog::Catalog,
{
	while let Ok(frame) = moq_clock::timeout(Duration::from_millis(1), export.next()).await {
		frames.push(frame.unwrap().expect("the export runs on"));
	}
}

/// Run `export` to its end.
async fn drain<S: crate::catalog::Stream>(export: &mut super::Export<S>, frames: &mut Vec<Frame>)
where
	S::Ext: super::catalog::Catalog,
{
	loop {
		match moq_clock::timeout(DELAY * 10, export.next())
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
	for (_, table) in tables_at(frames, pmt_pid) {
		if out.last() != Some(&table) {
			out.push(table);
		}
	}
	out
}

/// Every PMT copy on `pmt_pid` in `frames`, with the index of the packet it ends in.
fn tables_at(frames: &[Frame], pmt_pid: u16) -> Vec<(usize, Table)> {
	let mut out = Vec::new();
	let mut section = Vec::new();
	for (index, packet) in packets(frames).enumerate() {
		if pid(packet) != pmt_pid {
			continue;
		}
		let mut at = 4;
		if packet[3] & 0x20 != 0 {
			at += 1 + usize::from(packet[4]);
		}
		if packet[1] & 0x40 != 0 {
			section = packet[at + 1 + usize::from(packet[at])..].to_vec();
		} else if !section.is_empty() {
			section.extend_from_slice(&packet[at..]);
		} else {
			continue;
		}
		let len = 3 + (usize::from(section[1] & 0x0f) << 8 | usize::from(section[2]));
		if section.len() < len {
			continue;
		}
		let whole = std::mem::take(&mut section);
		let section = &whole[..len];
		let pmt = super::psi::Pmt::parse(section).expect("a whole PMT");
		let bytes = |descriptors: &[super::catalog::Descriptor]| {
			descriptors
				.iter()
				.flat_map(|d| [vec![d.tag, d.data.len() as u8], d.data.to_vec()].concat())
				.collect::<Vec<u8>>()
		};
		let table = Table {
			program: pmt.program_number,
			version: (section[5] >> 1) & 0x1f,
			pcr_pid: pmt.pcr_pid.map_or(0x1fff, |pid| pid.as_u16()),
			info: bytes(&pmt.program_info),
			streams: pmt
				.streams
				.iter()
				.map(|es| (es.stream_type, es.pid.as_u16(), bytes(&es.descriptors)))
				.collect(),
		};
		out.push((index, table));
	}
	out
}

/// Every PAT in `frames` as its `version_number` and PMT PID, each run of repeats once.
fn pats(frames: &[Frame]) -> Vec<(u8, u16)> {
	let mut out = Vec::new();
	for pat in packets(frames).filter_map(pat) {
		if out.last() != Some(&pat) {
			out.push(pat);
		}
	}
	out
}

/// The `version_number` and PMT PID of the PAT `packet` starts, if it starts one.
fn pat(packet: &[u8; 188]) -> Option<(u8, u16)> {
	if pid(packet) != 0 || packet[1] & 0x40 == 0 {
		return None;
	}
	let mut at = 4;
	if packet[3] & 0x20 != 0 {
		at += 1 + usize::from(packet[4]);
	}
	let section = &packet[at + 1 + usize::from(packet[at])..];
	Some((
		(section[5] >> 1) & 0x1f,
		u16::from(section[10] & 0x1f) << 8 | u16::from(section[11]),
	))
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
			},
			Span {
				streams: &[(AUDIO, AUDIO_PID, &[]), (PRIVATE, DATA_PID, &[])],
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
			},
			Span {
				streams: &[(AUDIO, AUDIO_PID, &[])],
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
			},
			Span {
				streams: &[(AUDIO, AUDIO_PID, &[]), (METADATA, DATA_PID, KLV)],
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
			},
			Span {
				streams: &[(AUDIO, AUDIO_PID, FRA)],
			},
		],
		25,
	)
	.await;
	assert_eq!(
		tables,
		vec![
			Table {
				program: 1,
				version: 0,
				pcr_pid: AUDIO_PID,
				info: Vec::new(),
				streams: vec![(AUDIO, AUDIO_PID, ENG.to_vec())],
			},
			Table {
				program: 1,
				version: 1,
				pcr_pid: AUDIO_PID,
				info: Vec::new(),
				streams: vec![(AUDIO, AUDIO_PID, FRA.to_vec())],
			},
		]
	);
}

/// A listed track keeps the descriptors its packets went out under: a catalog revising them
/// under the same name revises nothing, as a revised stream comes under a new name.
#[tokio::test(start_paused = true)]
async fn a_listed_track_keeps_its_descriptors() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(
		&mut broadcast,
		crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
	)
	.unwrap();
	let (name, mut audio) = aac_track(&mut broadcast, &mut catalog, Some(AUDIO_PID));
	let language = |language: &[u8]| super::catalog::Descriptor {
		tag: language[0],
		data: bytes::Bytes::copy_from_slice(&language[2..]),
	};
	catalog
		.modify()
		.unwrap()
		.ext
		.mpegts
		.tracks
		.get_mut(&name)
		.unwrap()
		.descriptors = vec![language(ENG)];
	let mut frames = Vec::new();
	let start = moq_clock::now();
	let mut export = export_of(&consumer).await;
	live(&mut export, &mut frames, start, &mut [(&mut audio, 0x21)], 0, 3_000).await;
	catalog
		.modify()
		.unwrap()
		.ext
		.mpegts
		.tracks
		.get_mut(&name)
		.unwrap()
		.descriptors = vec![language(FRA)];
	live(&mut export, &mut frames, start, &mut [(&mut audio, 0x21)], 3_000, 6_000).await;
	audio.finish().unwrap();
	catalog.finish().unwrap();
	drain(&mut export, &mut frames).await;

	let tables = tables(&frames, OWN_PMT_PID);
	let streams: Vec<_> = tables.iter().map(|table| table.streams.clone()).collect();
	assert_eq!(streams, [vec![(AUDIO, AUDIO_PID, ENG.to_vec())]]);
}

/// The PCR leaves a PID only with the track it rode on, for the next audio track.
#[tokio::test(start_paused = true)]
async fn the_clock_moves_only_when_its_track_retires() {
	let tables = reexport(
		&[
			Span {
				streams: &[(AUDIO, AUDIO_PID, ENG), (AUDIO, OTHER_AUDIO_PID, FRA)],
			},
			Span {
				streams: &[(AUDIO, OTHER_AUDIO_PID, FRA)],
			},
		],
		25,
	)
	.await;
	let clocks: Vec<(u8, u16)> = tables.iter().map(|table| (table.version, table.pcr_pid)).collect();
	assert_eq!(clocks, vec![(0, AUDIO_PID), (1, OTHER_AUDIO_PID)]);
	assert_eq!(tables[1].streams, vec![(AUDIO, OTHER_AUDIO_PID, FRA.to_vec())]);
}

/// The PCR leaves the PID of a track that retires only behind a PMT naming the PID it moves
/// to: a receiver holding the PMT before finds the clock on the PID that one names.
#[tokio::test(start_paused = true)]
async fn the_clock_moves_behind_the_pmt_naming_its_pid() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(
		&mut broadcast,
		crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
	)
	.unwrap();
	let (name, mut clock) = aac_track(&mut broadcast, &mut catalog, Some(AUDIO_PID));
	let (_, mut other) = aac_track(&mut broadcast, &mut catalog, Some(OTHER_AUDIO_PID));
	let mut frames = Vec::new();
	let start = moq_clock::now();
	let mut export = export_of(&consumer).await;
	live(
		&mut export,
		&mut frames,
		start,
		&mut [(&mut clock, 0x21), (&mut other, 0x21)],
		0,
		3_000,
	)
	.await;
	remove(&mut catalog, &name);
	clock.finish().unwrap();
	live(&mut export, &mut frames, start, &mut [(&mut other, 0x21)], 3_000, 6_000).await;
	other.finish().unwrap();
	catalog.finish().unwrap();
	drain(&mut export, &mut frames).await;

	let mut named = None;
	let mut clocks = Vec::new();
	for frame in &frames {
		for packet in packets(std::slice::from_ref(frame)) {
			if packet[3] & 0x20 != 0 && packet[4] > 0 && packet[5] & 0x10 != 0 {
				if let Some(named) = named {
					assert_eq!(pid(packet), named, "a PCR on a PID the PMT out does not name");
				}
				clocks.push(pid(packet));
			}
		}
		if let Some(table) = tables(std::slice::from_ref(frame), OWN_PMT_PID).last() {
			named = Some(table.pcr_pid);
		}
	}
	clocks.dedup();
	assert_eq!(clocks, [AUDIO_PID, OTHER_AUDIO_PID]);
}

/// A PID the PMT dropped can come back as another stream.
#[tokio::test(start_paused = true)]
async fn a_pid_removed_can_return_as_another_stream() {
	let tables = reexport(
		&[
			Span {
				streams: &[(AUDIO, AUDIO_PID, &[]), (PRIVATE, DATA_PID, &[])],
			},
			Span {
				streams: &[(AUDIO, AUDIO_PID, &[])],
			},
			Span {
				streams: &[(AUDIO, AUDIO_PID, &[]), (METADATA, DATA_PID, KLV)],
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
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(
		&mut broadcast,
		crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
	)
	.unwrap();
	let (_, mut audio) = aac_track(&mut broadcast, &mut catalog, Some(AUDIO_PID));
	let mut frames = Vec::new();
	let start = moq_clock::now();
	let mut export = export_of(&consumer).await;
	live(&mut export, &mut frames, start, &mut [(&mut audio, 0x21)], 0, 3_000).await;
	catalog.modify().unwrap().ext.mpegts.program_descriptors = vec![super::catalog::Descriptor {
		tag: 0x80,
		data: bytes::Bytes::from_static(&[0xaa; 6]),
	}];
	live(&mut export, &mut frames, start, &mut [(&mut audio, 0x21)], 3_000, 6_000).await;
	audio.finish().unwrap();
	catalog.finish().unwrap();
	drain(&mut export, &mut frames).await;

	let versions: Vec<(u8, Vec<u8>)> = tables(&frames, OWN_PMT_PID)
		.into_iter()
		.map(|table| (table.version, table.info))
		.collect();
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
	let out = moq_clock::timeout(DELAY * 2, export.next()).await;
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
	let out = moq_clock::timeout(DELAY * 2, export.next()).await;
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
	let out = moq_clock::timeout(DELAY * 4, export.next()).await;
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

/// A track whose frame is held behind the old generation's tail stays until that frame goes,
/// even once it ends and leaves the catalog.
#[tokio::test(start_paused = true)]
async fn a_track_ending_while_its_frame_is_held_stays_for_the_frame() {
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
	let (name, mut second) = aac_track(&mut broadcast, &mut catalog, Some(OTHER_AUDIO_PID));
	settle(&mut export, &mut frames).await;
	write_marked(&mut second, 3_000, 0x31);
	for millis in (3_020..3_500).step_by(20) {
		write_aac(&mut [&mut first], millis);
	}
	settle(&mut export, &mut frames).await;
	tokio::time::advance(DELAY * 2).await;
	assert_eq!(
		export.hold_next(),
		Some(OTHER_AUDIO_PID),
		"the second track's frame is held"
	);
	second.finish().unwrap();
	remove(&mut catalog, &name);
	first.finish().unwrap();
	catalog.finish().unwrap();
	drain(&mut export, &mut frames).await;

	assert_eq!(
		marks(&frames, OTHER_AUDIO_PID, &[0x31]),
		[0x31],
		"the held frame went out"
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
	let start = moq_clock::now();
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
	let start = moq_clock::now();
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
/// A track recorded on the PID of a stream the catalog dropped that never ends cuts that stream:
/// what it already sent drains, then each track recorded there follows in turn.
#[tokio::test(start_paused = true)]
async fn a_pid_taken_from_a_stream_that_never_ends_cuts_it() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(
		&mut broadcast,
		crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
	)
	.unwrap();
	let (name, mut old) = aac_track(&mut broadcast, &mut catalog, Some(AUDIO_PID));
	let mut frames = Vec::new();
	let start = moq_clock::now();
	let mut export = export_of(&consumer).await;
	live(&mut export, &mut frames, start, &mut [(&mut old, 0x21)], 0, 2_000).await;
	remove(&mut catalog, &name);
	for round in 0..4 {
		let (name, mut successor) = aac_track(&mut broadcast, &mut catalog, Some(AUDIO_PID));
		let from = 2_000 + u64::from(round) * 500;
		live(
			&mut export,
			&mut frames,
			start,
			&mut [(&mut old, 0x21), (&mut successor, 0x31 + round)],
			from,
			from + 500,
		)
		.await;
		remove(&mut catalog, &name);
		successor.finish().unwrap();
	}
	catalog.finish().unwrap();
	drain(&mut export, &mut frames).await;
	let mut marks = marks(&frames, AUDIO_PID, &[0x21, 0x31, 0x32, 0x33, 0x34]);
	marks.dedup();
	assert_eq!(marks, [0x21, 0x31, 0x32, 0x33, 0x34]);
	drop(old);
}

/// A track the catalog records on the PMT's PID as it moves the PMT takes another, and the
/// PMT moves behind a new PAT, still the program the first named.
#[tokio::test(start_paused = true)]
async fn a_track_recorded_on_the_pmt_pid_after_the_catalog_moves_it_takes_another() {
	let program = |pmt_pid: u16, program_number: u16| super::catalog::Program {
		transport_stream_id: 1,
		program_number,
		pmt_pid,
	};
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(
		&mut broadcast,
		crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
	)
	.unwrap();
	catalog.modify().unwrap().ext.mpegts.program = Some(program(PMT_PID, 1));
	let (_, mut first) = aac_track(&mut broadcast, &mut catalog, Some(AUDIO_PID));
	let mut frames = Vec::new();
	let start = moq_clock::now();
	let mut export = export_of(&consumer).await;
	live(&mut export, &mut frames, start, &mut [(&mut first, 0x21)], 0, 3_000).await;
	catalog.modify().unwrap().ext.mpegts.program = Some(program(OWN_PMT_PID, 7));
	let (_, mut added) = aac_track(&mut broadcast, &mut catalog, Some(PMT_PID));
	live(
		&mut export,
		&mut frames,
		start,
		&mut [(&mut first, 0x21), (&mut added, 0x21)],
		3_000,
		6_000,
	)
	.await;
	first.finish().unwrap();
	added.finish().unwrap();
	catalog.finish().unwrap();
	drain(&mut export, &mut frames).await;

	assert_eq!(pats(&frames), [(0, PMT_PID), (1, OWN_PMT_PID)]);
	let tables = tables(&frames, OWN_PMT_PID);
	let pids: Vec<u16> = tables.last().unwrap().streams.iter().map(|es| es.1).collect();
	assert_eq!(pids.len(), 2, "{tables:?}");
	assert!(!pids.contains(&PMT_PID) && !pids.contains(&OWN_PMT_PID), "{tables:?}");
	assert!(tables.iter().all(|table| table.program == 1), "{tables:?}");
}

/// The program a catalog records, its PMT on `pmt_pid`.
fn program(pmt_pid: u16) -> super::catalog::Program {
	super::catalog::Program {
		transport_stream_id: 1,
		program_number: 1,
		pmt_pid,
	}
}

/// A catalog that moves the PMT to another PID is followed by a PAT at the next version
/// naming it, and from then on the PMT rides the new PID alone.
#[tokio::test(start_paused = true)]
async fn a_moved_pmt_moves_behind_a_new_pat() {
	const MOVED_PMT_PID: u16 = 0x30;
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(
		&mut broadcast,
		crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
	)
	.unwrap();
	catalog.modify().unwrap().ext.mpegts.program = Some(program(PMT_PID));
	let (_, mut audio) = aac_track(&mut broadcast, &mut catalog, Some(AUDIO_PID));
	let mut frames = Vec::new();
	let start = moq_clock::now();
	let mut export = export_of(&consumer).await;
	live(&mut export, &mut frames, start, &mut [(&mut audio, 0x21)], 0, 3_000).await;
	catalog.modify().unwrap().ext.mpegts.program = Some(program(MOVED_PMT_PID));
	live(&mut export, &mut frames, start, &mut [(&mut audio, 0x21)], 3_000, 6_000).await;
	audio.finish().unwrap();
	catalog.finish().unwrap();
	drain(&mut export, &mut frames).await;

	assert_eq!(pats(&frames), [(0, PMT_PID), (1, MOVED_PMT_PID)]);
	assert_eq!(
		layouts(&tables(&frames, MOVED_PMT_PID)),
		[(0, vec![(AUDIO, AUDIO_PID)])]
	);
	let moved = packets(&frames)
		.position(|packet| pat(packet) == Some((1, MOVED_PMT_PID)))
		.expect("the PAT naming the new PID");
	assert!(
		packets(&frames).take(moved).all(|packet| pid(packet) != MOVED_PMT_PID),
		"no PMT on the new PID before the PAT naming it"
	);
	assert!(
		packets(&frames).skip(moved).all(|packet| pid(packet) != PMT_PID),
		"no PMT on the old PID after the PAT that moved it"
	);
}

/// A catalog that moves the PMT onto the PID a stream it lists rides fails the export: the
/// PMT cannot follow it there.
#[tokio::test(start_paused = true)]
async fn a_pmt_moved_onto_a_stream_fails_the_export() {
	let (mut broadcast, consumer, mut catalog) = broadcast();
	catalog.modify().unwrap().ext.mpegts.program = Some(program(PMT_PID));
	let (_, mut audio) = aac_track(&mut broadcast, &mut catalog, Some(AUDIO_PID));
	let mut frames = Vec::new();
	let start = moq_clock::now();
	let mut export = export_of(&consumer).await;
	live(&mut export, &mut frames, start, &mut [(&mut audio, 0x21)], 0, 3_000).await;
	catalog.modify().unwrap().ext.mpegts.program = Some(program(AUDIO_PID));
	let err = moq_clock::timeout(DELAY * 2, async {
		loop {
			if let Err(err) = export.next().await {
				return err;
			}
		}
	})
	.await
	.expect("the export fails");
	assert!(err.to_string().contains("another stream rides"), "{err}");
	assert_eq!(pats(&frames), [(0, PMT_PID)]);
}

/// A catalog that moves the PMT onto the PID of a stream it dropped moves it once that stream
/// has drained: the stream's packets all go out ahead of the PAT naming its PID, and only the
/// PMT rides it after.
#[tokio::test(start_paused = true)]
async fn a_pmt_moved_onto_a_draining_stream_waits_for_it() {
	let (mut broadcast, consumer, mut catalog) = broadcast();
	catalog.modify().unwrap().ext.mpegts.program = Some(program(PMT_PID));
	let (_, mut audio) = aac_track(&mut broadcast, &mut catalog, Some(AUDIO_PID));
	let (name, mut leaving) = aac_track(&mut broadcast, &mut catalog, Some(OTHER_AUDIO_PID));
	let mut frames = Vec::new();
	let start = moq_clock::now();
	let mut export = export_of(&consumer).await;
	live(
		&mut export,
		&mut frames,
		start,
		&mut [(&mut audio, 0x21), (&mut leaving, 0x22)],
		0,
		3_000,
	)
	.await;
	remove(&mut catalog, &name);
	catalog.modify().unwrap().ext.mpegts.program = Some(program(OTHER_AUDIO_PID));
	live(
		&mut export,
		&mut frames,
		start,
		&mut [(&mut audio, 0x21), (&mut leaving, 0x22)],
		3_000,
		4_000,
	)
	.await;
	leaving.finish().unwrap();
	live(&mut export, &mut frames, start, &mut [(&mut audio, 0x21)], 4_000, 6_000).await;
	audio.finish().unwrap();
	catalog.finish().unwrap();
	drain(&mut export, &mut frames).await;

	assert_eq!(pats(&frames), [(0, PMT_PID), (1, OTHER_AUDIO_PID)]);
	let moved = packets(&frames)
		.position(|packet| pat(packet) == Some((1, OTHER_AUDIO_PID)))
		.expect("the PAT naming the new PID");
	// What each unit starting on the PID opens: a PES, or a section behind its pointer field.
	let starts = |packets: Vec<&[u8; 188]>| -> Vec<u8> {
		packets
			.into_iter()
			.filter(|packet| pid(packet) == OTHER_AUDIO_PID && packet[1] & 0x40 != 0)
			.map(|packet| {
				let at = match packet[3] & 0x20 != 0 {
					true => 5 + usize::from(packet[4]),
					false => 4,
				};
				match packet[at..at + 3] == [0, 0, 1] {
					true => 0xff,
					false => packet[at + 1 + usize::from(packet[at])],
				}
			})
			.collect()
	};
	let before = starts(packets(&frames).take(moved).collect());
	assert!(
		!before.is_empty() && before.iter().all(|&start| start == 0xff),
		"{before:?}"
	);
	let after = starts(packets(&frames).skip(moved).collect());
	assert!(
		!after.is_empty() && after.iter().all(|&start| start == 0x02),
		"{after:?}"
	);
}

/// A catalog that stops recording a PMT PID while a move waits for a draining stream cancels
/// the move: the PMT stays where it is once the stream has drained.
#[tokio::test(start_paused = true)]
async fn a_catalog_dropping_the_pmt_pid_cancels_a_waiting_move() {
	let (mut broadcast, consumer, mut catalog) = broadcast();
	catalog.modify().unwrap().ext.mpegts.program = Some(program(PMT_PID));
	let (_, mut audio) = aac_track(&mut broadcast, &mut catalog, Some(AUDIO_PID));
	let (name, mut leaving) = aac_track(&mut broadcast, &mut catalog, Some(OTHER_AUDIO_PID));
	let mut frames = Vec::new();
	let start = moq_clock::now();
	let mut export = export_of(&consumer).await;
	let mut tracks = [(&mut audio, 0x21), (&mut leaving, 0x22)];
	live(&mut export, &mut frames, start, &mut tracks, 0, 3_000).await;
	remove(&mut catalog, &name);
	catalog.modify().unwrap().ext.mpegts.program = Some(program(OTHER_AUDIO_PID));
	live(&mut export, &mut frames, start, &mut tracks, 3_000, 3_500).await;
	catalog.modify().unwrap().ext.mpegts.program = Some(program(0));
	live(&mut export, &mut frames, start, &mut tracks, 3_500, 4_000).await;
	leaving.finish().unwrap();
	live(&mut export, &mut frames, start, &mut [(&mut audio, 0x21)], 4_000, 6_000).await;
	audio.finish().unwrap();
	catalog.finish().unwrap();
	drain(&mut export, &mut frames).await;

	assert_eq!(pats(&frames), [(0, PMT_PID)]);
}

/// A catalog that gives a stream back the PID the PMT waits to move onto fails the export.
#[tokio::test(start_paused = true)]
async fn a_stream_returning_to_the_pid_the_pmt_waits_for_fails_the_export() {
	let (mut broadcast, consumer, mut catalog) = broadcast();
	catalog.modify().unwrap().ext.mpegts.program = Some(program(PMT_PID));
	let (_, mut audio) = aac_track(&mut broadcast, &mut catalog, Some(AUDIO_PID));
	let (name, mut leaving) = aac_track(&mut broadcast, &mut catalog, Some(OTHER_AUDIO_PID));
	let config = catalog.snapshot().audio.renditions[&name].clone();
	let mut frames = Vec::new();
	let start = moq_clock::now();
	let mut export = export_of(&consumer).await;
	let mut tracks = [(&mut audio, 0x21), (&mut leaving, 0x22)];
	live(&mut export, &mut frames, start, &mut tracks, 0, 3_000).await;
	remove(&mut catalog, &name);
	catalog.modify().unwrap().ext.mpegts.program = Some(program(OTHER_AUDIO_PID));
	live(&mut export, &mut frames, start, &mut tracks, 3_000, 3_500).await;
	{
		let mut guard = catalog.modify().unwrap();
		guard.audio.renditions.insert(name.clone(), config);
		guard
			.ext
			.mpegts
			.tracks
			.insert(name, super::catalog::Track::new(OTHER_AUDIO_PID));
	}
	let err = moq_clock::timeout(DELAY * 2, async {
		loop {
			if let Err(err) = export.next().await {
				return err;
			}
		}
	})
	.await
	.expect("the export fails");
	assert!(err.to_string().contains("another stream rides"), "{err}");
	assert_eq!(pats(&frames), [(0, PMT_PID)]);
}

/// A move landing while a copy of the tables is on its way waits for that copy: at a rate low
/// enough that a copy spans slots, every PMT packet rides the PID the PAT before it names, and
/// no two PATs at one version name different PIDs.
#[tokio::test(start_paused = true)]
async fn a_pmt_moved_while_its_tables_go_out_moves_behind_them() {
	const MOVED_PMT_PID: u16 = 0x30;
	let (mut broadcast, consumer, mut catalog) = broadcast();
	catalog.modify().unwrap().ext.mpegts.program = Some(program(PMT_PID));
	let (_, mut audio) = aac_track(&mut broadcast, &mut catalog, Some(AUDIO_PID));
	let mut frames = Vec::new();
	let start = moq_clock::now();
	let mut export = export_of(&consumer).await.with_mux_rate(rate(4));
	for millis in (0..6_000).step_by(50) {
		moq_clock::sleep_until(start + Duration::from_millis(millis)).await;
		match millis {
			// Three packets of PMT from here, in slots of four.
			2_500 => {
				catalog.modify().unwrap().ext.mpegts.program_descriptors = (0x80..0x82)
					.map(|tag| super::catalog::Descriptor {
						tag,
						data: bytes::Bytes::from(vec![tag; 250]),
					})
					.collect();
			}
			// Back and forth, landing at each point of the copies going out.
			3_000..4_000 => {
				let pmt_pid = match millis / 50 % 2 {
					0 => MOVED_PMT_PID,
					_ => PMT_PID,
				};
				catalog.modify().unwrap().ext.mpegts.program = Some(program(pmt_pid));
			}
			_ => {}
		}
		write_marked(&mut audio, millis, 0x21);
		settle(&mut export, &mut frames).await;
	}
	audio.finish().unwrap();
	catalog.finish().unwrap();
	drain(&mut export, &mut frames).await;

	let pats = pats(&frames);
	assert!(pats.len() > 2, "{pats:?}");
	let mut named = std::collections::HashMap::new();
	for &(version, pmt_pid) in &pats {
		assert_eq!(*named.entry(version).or_insert(pmt_pid), pmt_pid, "{pats:?}");
	}
	let mut pmt_pid = None;
	for packet in packets(&frames) {
		if let Some((_, pid)) = pat(packet) {
			pmt_pid = Some(pid);
		}
		if [PMT_PID, MOVED_PMT_PID].contains(&pid(packet)) {
			assert_eq!(Some(pid(packet)), pmt_pid, "a PMT packet off the PID its PAT names");
		}
	}
}

/// A PAT that moves the PMT before any table has gone out takes every copy queued with it:
/// the PAT names the new PID at the version the first had, and nothing goes out on the old.
#[tokio::test(start_paused = true)]
async fn a_pmt_moved_before_its_tables_go_out_moves_every_copy() {
	const MOVED_PMT_PID: u16 = 0x30;
	let span = Span {
		streams: &[(AUDIO, AUDIO_PID, &[])],
	};
	let frames = reexport_frames(&[(PMT_PID, &span), (MOVED_PMT_PID, &span)], 25).await;
	assert_eq!(pats(&frames), [(0, MOVED_PMT_PID)]);
	assert_eq!(
		layouts(&tables(&frames, MOVED_PMT_PID)),
		[(0, vec![(AUDIO, AUDIO_PID)])]
	);
	assert!(packets(&frames).all(|packet| pid(packet) != PMT_PID));
}

/// An SCTE-35 `splice_null` section.
const CUE: &[u8] = &[
	0xfc, 0x30, 0x1b, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xf0, 0x0a, 0x05, 0x00, 0x00, 0x2b, 0xb4, 0x7f,
	0xdf, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0xad, 0x25, 0xe8, 0x39,
];

/// Tables go out with the frame they were muxed with, and a placed frame waits for its
/// placement, so a revision muxed after another can go out first. The copies still queued
/// then carry the later table, under the version the earlier one took: the wire still
/// steps by one.
#[tokio::test(start_paused = true)]
async fn a_revision_out_before_the_one_it_follows_takes_its_version() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(
		&mut broadcast,
		crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
	)
	.unwrap();
	let (_, mut first) = aac_track(&mut broadcast, &mut catalog, Some(AUDIO_PID));
	let mut frames = Vec::new();
	let start = moq_clock::now();
	let mut export = export_of(&consumer).await;
	live(&mut export, &mut frames, start, &mut [(&mut first, 0x21)], 0, 3_000).await;

	// A cue and an AAC track start together, the cue muxed first, on the lower PID: its
	// revision waits with it for its placement, the AAC track's goes out now.
	let track = broadcast
		.unique_track(".scte35", hang::container::track_info(hang::catalog::PRIORITY.video))
		.unwrap();
	let entry = super::catalog::Track {
		pid: DATA_PID,
		descriptors: Vec::new(),
		verbatim: Some(super::catalog::Verbatim::new(0x86, super::catalog::Framing::Section)),
		pes_packet: None,
	};
	catalog
		.modify()
		.unwrap()
		.ext
		.mpegts
		.tracks
		.insert(track.name().to_string(), entry);
	let mut cue = Writer::new(
		track,
		crate::catalog::hang::Container::Legacy(crate::container::Kind::Data),
	);
	let (_, mut added) = aac_track(&mut broadcast, &mut catalog, Some(OTHER_AUDIO_PID));
	settle(&mut export, &mut frames).await;
	moq_clock::sleep_until(start + Duration::from_millis(3_000)).await;
	cue.write(Frame {
		timestamp: moq_net::Timestamp::from_millis(3_000).unwrap(),
		duration: None,
		payload: bytes::Bytes::from_static(CUE),
		keyframe: true,
	})
	.unwrap();
	cue.cut(None).unwrap();
	live(
		&mut export,
		&mut frames,
		start,
		&mut [(&mut first, 0x21), (&mut added, 0x21)],
		3_000,
		6_000,
	)
	.await;
	for track in [&mut first, &mut added, &mut cue] {
		track.finish().unwrap();
	}
	catalog.finish().unwrap();
	drain(&mut export, &mut frames).await;

	let tables = tables(&frames, OWN_PMT_PID);
	for pair in tables.windows(2) {
		assert_eq!(pair[1].version, (pair[0].version + 1) % 32, "{tables:?}");
	}
	let pids: Vec<u16> = tables.last().unwrap().streams.iter().map(|es| es.1).collect();
	assert_eq!(pids, [AUDIO_PID, DATA_PID, OTHER_AUDIO_PID], "{tables:?}");
}

/// A stream taking a PID over from a buffered one goes out on its own model, not the one
/// before it: a private section larger than the AAC buffer before it still goes out.
#[tokio::test(start_paused = true)]
async fn a_stream_taking_a_pid_over_leaves_the_buffer_before_it() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(
		&mut broadcast,
		crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
	)
	.unwrap();
	let (_, mut first) = aac_track(&mut broadcast, &mut catalog, Some(AUDIO_PID));
	let (name, mut before) = aac_track(&mut broadcast, &mut catalog, Some(DATA_PID));
	let mut frames = Vec::new();
	let start = moq_clock::now();
	let mut export = export_of(&consumer).await;
	live(
		&mut export,
		&mut frames,
		start,
		&mut [(&mut first, 0x21), (&mut before, 0x21)],
		0,
		3_000,
	)
	.await;
	remove(&mut catalog, &name);
	before.finish().unwrap();
	live(&mut export, &mut frames, start, &mut [(&mut first, 0x21)], 3_000, 6_000).await;

	let track = broadcast
		.unique_track(".private", hang::container::track_info(hang::catalog::PRIORITY.video))
		.unwrap();
	let entry = super::catalog::Track {
		pid: DATA_PID,
		descriptors: Vec::new(),
		verbatim: Some(super::catalog::Verbatim::new(0x05, super::catalog::Framing::Section)),
		pes_packet: None,
	};
	catalog
		.modify()
		.unwrap()
		.ext
		.mpegts
		.tracks
		.insert(track.name().to_string(), entry);
	let mut private = Writer::new(
		track,
		crate::catalog::hang::Container::Legacy(crate::container::Kind::Data),
	);
	settle(&mut export, &mut frames).await;
	moq_clock::sleep_until(start + Duration::from_millis(6_000)).await;
	let mut section = vec![0x80, 0x7f, 0xfd];
	section.resize(4_096, 0xab);
	private
		.write(Frame {
			timestamp: moq_net::Timestamp::from_millis(6_000).unwrap(),
			duration: None,
			payload: bytes::Bytes::from(section),
			keyframe: true,
		})
		.unwrap();
	private.cut(None).unwrap();
	live(&mut export, &mut frames, start, &mut [(&mut first, 0x21)], 6_000, 9_000).await;
	for track in [&mut first, &mut private] {
		track.finish().unwrap();
	}
	catalog.finish().unwrap();
	drain(&mut export, &mut frames).await;

	let layout = layouts(&tables(&frames, OWN_PMT_PID));
	assert_eq!(layout.last().unwrap().1, [(AUDIO, AUDIO_PID), (0x05, DATA_PID)]);
	let sent = packets(&frames).filter(|packet| pid(packet) == DATA_PID && packet[4..].iter().all(|&b| b == 0xab));
	assert_eq!(sent.count(), 4_096 / 184 - 1, "the section went out whole");
}
/// A stream added while one that left drains, the two too many for a section beside the rest,
/// fails the export: a PMT that cannot list its program is refused, not cut short.
#[tokio::test(start_paused = true)]
async fn a_stream_too_many_for_the_pmt_section_fails_the_export() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(
		&mut broadcast,
		crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
	)
	.unwrap();
	// A stream with 255 bytes of descriptors: three fit a section, four do not.
	fn track(
		broadcast: &mut moq_net::broadcast::Producer,
		catalog: &mut crate::catalog::Producer<Ext>,
		pid: u16,
	) -> (String, Writer) {
		let (name, writer) = aac_track(broadcast, catalog, Some(pid));
		let descriptor = super::catalog::Descriptor {
			tag: 0x80,
			data: bytes::Bytes::from(vec![pid as u8; 250]),
		};
		catalog
			.modify()
			.unwrap()
			.ext
			.mpegts
			.tracks
			.get_mut(&name)
			.unwrap()
			.descriptors = vec![descriptor];
		(name, writer)
	}
	let (_, mut first) = track(&mut broadcast, &mut catalog, AUDIO_PID);
	let (_, mut second) = track(&mut broadcast, &mut catalog, OTHER_AUDIO_PID);
	let (name, mut leaving) = track(&mut broadcast, &mut catalog, 0x105);
	let mut frames = Vec::new();
	let start = moq_clock::now();
	let mut export = export_of(&consumer).await;
	let mut tracks = [(&mut first, 0x21), (&mut second, 0x21), (&mut leaving, 0x21)];
	live(&mut export, &mut frames, start, &mut tracks, 0, 3_000).await;
	remove(&mut catalog, &name);
	let (_, mut added) = track(&mut broadcast, &mut catalog, 0x106);
	let mut tracks = [
		(&mut first, 0x21),
		(&mut second, 0x21),
		(&mut leaving, 0x21),
		(&mut added, 0x21),
	];
	let err = moq_clock::timeout(DELAY * 2, async {
		loop {
			for (track, mark) in tracks.iter_mut() {
				write_marked(track, start.elapsed().as_millis() as u64, *mark);
			}
			moq_clock::sleep(Duration::from_millis(20)).await;
			while let Ok(next) = moq_clock::timeout(Duration::from_millis(1), export.next()).await {
				if let Err(err) = next {
					return err;
				}
			}
		}
	})
	.await
	.expect("the export fails");
	assert!(err.to_string().contains("exceeds 1021 bytes"), "{err}");
}

/// Tracks the catalog takes back before sending anything, their sources left open, go with
/// it, each on a PID of its own as they are.
#[tokio::test(start_paused = true)]
async fn tracks_withdrawn_before_sending_anything_go() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(
		&mut broadcast,
		crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
	)
	.unwrap();
	let (_, mut first) = aac_track(&mut broadcast, &mut catalog, Some(AUDIO_PID));
	let mut frames = Vec::new();
	let start = moq_clock::now();
	let mut export = export_of(&consumer).await;
	live(&mut export, &mut frames, start, &mut [(&mut first, 0x21)], 0, 3_000).await;
	let mut open = Vec::new();
	for at in (3_000..8_000).step_by(1_000) {
		let (name, withdrawn) = aac_track(&mut broadcast, &mut catalog, None);
		live(&mut export, &mut frames, start, &mut [(&mut first, 0x21)], at, at + 500).await;
		remove(&mut catalog, &name);
		live(
			&mut export,
			&mut frames,
			start,
			&mut [(&mut first, 0x21)],
			at + 500,
			at + 1_000,
		)
		.await;
		open.push(withdrawn);
	}
	assert_eq!(export.tracks(), 1);
	first.finish().unwrap();
	catalog.finish().unwrap();
	drain(&mut export, &mut frames).await;
	drop(open);
}

/// Write a frame stamped `millis`, filled with its mark, to each of `tracks` for each 20 ms
/// from `from` to `to`, `millis` after `start`, as a live source does, the export keeping up.
async fn live<S: crate::catalog::Stream>(
	export: &mut super::Export<S>,
	frames: &mut Vec<Frame>,
	start: moq_clock::Instant,
	tracks: &mut [(&mut Writer, u8)],
	from: u64,
	to: u64,
) where
	S::Ext: super::catalog::Catalog,
{
	for millis in (from..to).step_by(20) {
		moq_clock::sleep_until(start + Duration::from_millis(millis)).await;
		for (track, mark) in tracks.iter_mut() {
			write_marked(track, millis, *mark);
		}
		settle(export, frames).await;
	}
}

/// A broadcast and its `mpegts` catalog.
fn broadcast() -> (
	moq_net::broadcast::Producer,
	moq_net::broadcast::Consumer,
	crate::catalog::Producer<Ext>,
) {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(
		&mut broadcast,
		crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
	)
	.unwrap();
	(broadcast, consumer, catalog)
}

/// The rate of `packets` a slot.
fn rate(packets: u64) -> u64 {
	packets * 188 * 8 * 40
}

/// The PCR moves only once the whole PMT naming its new PID is out: at a rate low enough
/// that the PMT spans slots, no PCR goes out there before the PMT's last packet.
#[tokio::test(start_paused = true)]
async fn the_clock_moves_behind_the_last_packet_of_the_pmt_naming_its_pid() {
	let (mut broadcast, consumer, mut catalog) = broadcast();
	let (name, mut clock) = aac_track(&mut broadcast, &mut catalog, Some(AUDIO_PID));
	let (_, mut other) = aac_track(&mut broadcast, &mut catalog, Some(OTHER_AUDIO_PID));
	let mut frames = Vec::new();
	let start = moq_clock::now();
	let mut export = export_of(&consumer).await.with_mux_rate(rate(4));
	// Frames 50 ms apart, for room beside the tables; the second track's from 500 ms, for room
	// beside the first tables.
	for millis in (0..6_000).step_by(50) {
		moq_clock::sleep_until(start + Duration::from_millis(millis)).await;
		match millis {
			..3_000 => write_marked(&mut clock, millis, 0x21),
			// The revision moving the clock lays three packets of PMT, in slots of four.
			3_000 => {
				let mut guard = catalog.modify().unwrap();
				guard.audio.renditions.remove(&name);
				guard.ext.mpegts.tracks.remove(&name);
				guard.ext.mpegts.program_descriptors = (0x80..0x82)
					.map(|tag| super::catalog::Descriptor {
						tag,
						data: bytes::Bytes::from(vec![tag; 250]),
					})
					.collect();
				drop(guard);
				clock.finish().unwrap();
			}
			_ => {}
		}
		if millis >= 500 {
			write_marked(&mut other, millis, 0x21);
		}
		settle(&mut export, &mut frames).await;
	}
	other.finish().unwrap();
	catalog.finish().unwrap();
	drain(&mut export, &mut frames).await;

	let ends: Vec<(usize, u16)> = tables_at(&frames, OWN_PMT_PID)
		.into_iter()
		.map(|(at, table)| (at, table.pcr_pid))
		.collect();
	let mut clocks = Vec::new();
	for (index, packet) in packets(&frames).enumerate() {
		if packet[3] & 0x20 != 0 && packet[4] > 0 && packet[5] & 0x10 != 0 {
			if let Some(&(_, named)) = ends.iter().take_while(|(at, _)| *at < index).last() {
				assert_eq!(pid(packet), named, "a PCR on a PID no whole PMT out names");
			}
			clocks.push(pid(packet));
		}
	}
	clocks.dedup();
	assert_eq!(clocks, [AUDIO_PID, OTHER_AUDIO_PID]);
}

/// A multiplex rate that changes with a revision applies behind it: the slots up to the one
/// the revised PMT starts out in are padded to the old rate, those after it to the new.
#[tokio::test(start_paused = true)]
async fn a_rate_change_applies_behind_the_revision_it_comes_with() {
	const OLD: u64 = 20;
	const NEW: u64 = 30;
	let (mut broadcast, consumer, mut catalog) = broadcast();
	catalog.modify().unwrap().ext.mpegts.mux_rate = Some(rate(OLD));
	let (_, mut audio) = aac_track(&mut broadcast, &mut catalog, Some(AUDIO_PID));
	let mut frames = Vec::new();
	let start = moq_clock::now();
	let mut export = export_of(&consumer).await;
	live(&mut export, &mut frames, start, &mut [(&mut audio, 0x21)], 0, 3_000).await;
	{
		let mut guard = catalog.modify().unwrap();
		guard.ext.mpegts.mux_rate = Some(rate(NEW));
		guard.ext.mpegts.program_descriptors = vec![super::catalog::Descriptor {
			tag: 0x80,
			data: bytes::Bytes::from_static(&[1, 2, 3, 4]),
		}];
	}
	live(&mut export, &mut frames, start, &mut [(&mut audio, 0x21)], 3_000, 6_000).await;
	audio.finish().unwrap();
	catalog.finish().unwrap();
	drain(&mut export, &mut frames).await;

	let revised = frames
		.iter()
		.position(|frame| {
			tables(std::slice::from_ref(frame), OWN_PMT_PID)
				.iter()
				.any(|table| table.version == 1)
		})
		.expect("a revised PMT");
	let counts: Vec<u64> = frames.iter().map(|frame| frame.payload.len() as u64 / 188).collect();
	assert!(
		counts[revised - 20..=revised].iter().all(|&n| n == OLD),
		"{revised}: {counts:?}"
	);
	assert!(
		counts[revised + 1..revised + 20].iter().all(|&n| n == NEW),
		"{revised}: {counts:?}"
	);
}

/// A track the catalog withdraws before the PMT lists it, while its first frames wait in the
/// jitter buffer, and lists again under the same name is the same stream: it is read on.
#[tokio::test(start_paused = true)]
async fn a_track_withdrawn_before_its_listing_is_read_on_when_it_returns() {
	withdraw_and_return(false).await;
}

/// As above, with the first frames it sent before leaving going out, which lists it, and the
/// rest still waiting when it returns.
#[tokio::test(start_paused = true)]
async fn a_track_withdrawn_before_its_listing_is_read_on_when_it_returns_listed() {
	withdraw_and_return(true).await;
}

/// Withdraw a track whose first frames wait in the jitter buffer, let some of them go out when
/// `listed`, and list it again: the frames it sends after returning go out.
async fn withdraw_and_return(listed: bool) {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(
		&mut broadcast,
		crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
	)
	.unwrap();
	let (_, mut first) = aac_track(&mut broadcast, &mut catalog, Some(AUDIO_PID));
	let mut frames = Vec::new();
	let start = moq_clock::now();
	let mut export = export_of(&consumer).await;
	live(&mut export, &mut frames, start, &mut [(&mut first, 0x21)], 0, 2_000).await;

	let (name, mut second) = aac_track(&mut broadcast, &mut catalog, Some(OTHER_AUDIO_PID));
	live(
		&mut export,
		&mut frames,
		start,
		&mut [(&mut first, 0x21), (&mut second, 0x31)],
		2_000,
		2_100,
	)
	.await;
	let config = catalog.modify().unwrap().audio.renditions[&name].clone();
	remove(&mut catalog, &name);
	settle(&mut export, &mut frames).await;
	let back = match listed {
		true => 2_050 + DELAY.as_millis() as u64,
		false => 2_100,
	};
	live(&mut export, &mut frames, start, &mut [(&mut first, 0x21)], 2_100, back).await;
	assert_eq!(
		marks(&frames, OTHER_AUDIO_PID, &[0x31]).is_empty(),
		!listed,
		"its frames went out before it returned"
	);
	let mut guard = catalog.modify().unwrap();
	guard.audio.renditions.insert(name.clone(), config);
	guard
		.ext
		.mpegts
		.tracks
		.insert(name, super::catalog::Track::new(OTHER_AUDIO_PID));
	drop(guard);
	live(
		&mut export,
		&mut frames,
		start,
		&mut [(&mut first, 0x21), (&mut second, 0x32)],
		back,
		back + 1_000,
	)
	.await;
	first.finish().unwrap();
	second.finish().unwrap();
	catalog.finish().unwrap();
	drain(&mut export, &mut frames).await;

	assert_eq!(
		marks(&frames, OTHER_AUDIO_PID, &[0x31, 0x32]).last(),
		Some(&0x32),
		"the returned track went out"
	);
}
