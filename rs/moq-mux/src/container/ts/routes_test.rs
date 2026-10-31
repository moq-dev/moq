//! Every PID of an imported program takes one declared route: decoded, whole PES, section,
//! captured PSI/SI, or not carried and counted.

use std::time::Duration;

use super::catalog::Ext;
use super::fixture::{Flags, MS, Mux};
use super::stats::{Class, Stream};
use super::{Import, Programs, psi};

const PMT: u16 = 0x20;
const CLOCK: u16 = 0x1ff;
const VIDEO: u16 = 0x100;
const DATA: u16 = 0x101;
const UNLISTED: u16 = 0x300;
/// The PCR leads each picture's PTS by this many 90 kHz ticks.
const LEAD: u64 = 9_000;
/// `registration_descriptor` naming SMPTE RP 217 KLV ('KLVA').
const KLVA: &[u8] = &[0x05, 0x04, b'K', b'L', b'V', b'A'];

/// A 33-bit PTS field behind the '0010' prefix.
fn pts_field(ticks: u64) -> [u8; 5] {
	[
		0x21 | ((ticks >> 29) & 0x0e) as u8,
		(ticks >> 22) as u8,
		((ticks >> 14) & 0xfe) as u8 | 1,
		(ticks >> 7) as u8,
		((ticks << 1) & 0xfe) as u8 | 1,
	]
}

/// An MPEG-2 video PES presenting at `pts`, its length left unbounded as video may: a sequence
/// header start code when `sequence`, a picture start code, then `len` bytes of `fill`.
fn picture(pts: u64, sequence: bool, fill: u8, len: usize) -> Vec<u8> {
	let mut pes = vec![0, 0, 1, 0xe0, 0, 0, 0x80, 0x80, 5];
	pes.extend_from_slice(&pts_field(pts));
	if sequence {
		pes.extend_from_slice(&[0, 0, 1, 0xb3]);
	}
	pes.extend_from_slice(&[0, 0, 1, 0x00]);
	pes.resize(pes.len() + len, fill);
	pes
}

/// An asynchronous KLV PES (private_stream_1, no PTS) of `len` bytes of `fill`.
fn klv(fill: u8, len: usize) -> Vec<u8> {
	let mut pes = vec![0, 0, 1, 0xbd];
	pes.extend_from_slice(&((3 + len) as u16).to_be_bytes());
	pes.extend_from_slice(&[0x84, 0x00, 0x00]);
	pes.resize(pes.len() + len, fill);
	pes
}

/// A bounded audio PES presenting at `pts`, carrying `payload`.
fn audio(pts: u64, payload: &[u8]) -> Vec<u8> {
	let mut pes = vec![0, 0, 1, 0xc0];
	pes.extend_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
	pes.extend_from_slice(&[0x84, 0x80, 5]);
	pes.extend_from_slice(&pts_field(pts));
	pes.extend_from_slice(payload);
	pes
}

/// A `pid` packet whose payload is TS-scrambled.
fn scrambled(mux: &mut Mux, pid: u16) {
	let at = mux.out.len();
	mux.fill(pid, 1);
	mux.out[at + 3] |= 0x80;
}

/// An importer of one program, onto a catalog with the `mpegts` section.
struct Imported {
	consumer: moq_net::broadcast::Consumer,
	catalog: crate::catalog::Producer<Ext>,
	import: Import<Ext>,
}

impl Imported {
	fn new() -> Self {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(
			&mut broadcast,
			crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
		)
		.unwrap();
		let import = Import::new(broadcast, catalog.reserve());
		Self {
			consumer,
			catalog,
			import,
		}
	}

	/// Import all of `data`.
	fn all(data: &[u8]) -> Self {
		let mut imported = Self::new();
		imported.import.decode(data).unwrap();
		imported.import.finish().unwrap();
		imported
	}

	/// The `mpegts` catalog entry for `pid`: its track name and record.
	fn entry(&self, pid: u16) -> Option<(String, super::catalog::Track)> {
		let mpegts = self.catalog.snapshot().ext.mpegts;
		mpegts.tracks.into_iter().find(|(_, track)| track.pid == pid)
	}

	/// The stats row for `pid`.
	fn row(&self, pid: u16) -> Stream {
		self.import.stats().streams.get(&pid).cloned().unwrap_or_default()
	}

	/// Every frame on the track carrying `pid`.
	async fn frames(&self, pid: u16) -> Vec<crate::container::Frame> {
		let (name, _) = self.entry(pid).expect("a track on the PID");
		let subscription = moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(3600));
		let track = self
			.consumer
			.track(&name)
			.unwrap()
			.subscribe(subscription)
			.await
			.unwrap();
		let mut reader = crate::container::Consumer::new(
			track,
			crate::catalog::hang::Container::Legacy(crate::container::Kind::Data),
		);
		let mut frames = Vec::new();
		while let Ok(Ok(Some(frame))) = tokio::time::timeout(Duration::from_millis(50), reader.read_frame()).await {
			frames.push(frame);
		}
		frames
	}
}

/// MPEG-2 video rides whole while its PTS still paces the program, and a TS round trip keeps
/// every PES byte of it. A sequence header opens each group, so the picture joined mid-GOP is
/// dropped, and so is the picture after one lost to scrambling, until the next sequence header.
#[tokio::test(start_paused = true)]
async fn mpeg2_video_rides_whole_and_survives_a_round_trip() {
	let mut mux = Mux::default();
	mux.pat(&[(1, PMT)]).pmt(PMT, 1, CLOCK, &[(0x02, VIDEO)], 0);
	let start = 2_000 * MS;
	let mut pictures = Vec::new();
	for i in 0..9u64 {
		let ticks = start + i * 40 * MS;
		mux.packet(CLOCK, Flags::pcr(ticks));
		// Picture 6 is still open, its length unbounded, when the scrambled packet loses it.
		if i == 7 {
			scrambled(&mut mux, VIDEO);
		}
		let pes = picture(
			ticks / 300 + LEAD,
			matches!(i, 1 | 5 | 8),
			i as u8,
			300 + i as usize * 50,
		);
		mux.whole(VIDEO, &pes);
		pictures.push(pes);
	}
	mux.packet(CLOCK, Flags::pcr(start + 9 * 40 * MS));
	// What a subscriber gets, and which pictures open a group.
	let carried: Vec<_> = [1, 2, 3, 4, 5, 8].map(|i| pictures[i].clone()).into();
	let opens = [true, false, false, false, true, true];

	let imported = Imported::all(&mux.bytes());
	let (_, entry) = imported.entry(VIDEO).expect("the video is in the catalog");
	assert_eq!(entry.pes_packet.as_ref().map(|pes| pes.stream_type), Some(0x02));
	let snapshot = imported.catalog.snapshot();
	assert!(
		snapshot.video.renditions.is_empty(),
		"no rendition a player cannot decode"
	);
	let frames = imported.frames(VIDEO).await;
	let payloads: Vec<_> = frames.iter().map(|frame| frame.payload.to_vec()).collect();
	assert_eq!(payloads, carried);
	let keyframes: Vec<_> = frames.iter().map(|frame| frame.keyframe).collect();
	assert_eq!(keyframes, opens);
	assert!(frames.windows(2).all(|pair| pair[0].timestamp < pair[1].timestamp));
	let row = imported.row(VIDEO);
	assert_eq!((row.track.as_str(), row.class), (".ts", Class::Video));
	assert_eq!((row.units, row.refused, row.damaged), (6, 1, 0));

	let ts = super::export_test::drain_with(
		super::export_test::open_ts(
			crate::source::announced(&imported.consumer).await,
			crate::catalog::CatalogFormat::Hang,
		)
		.await
		.unwrap(),
	)
	.await;
	let again = Imported::all(&ts);
	let (_, entry) = again.entry(VIDEO).expect("the export lists the video");
	assert_eq!(entry.pes_packet.as_ref().map(|pes| pes.stream_type), Some(0x02));
	let frames = again.frames(VIDEO).await;
	let payloads: Vec<_> = frames.iter().map(|frame| frame.payload.to_vec()).collect();
	assert_eq!(payloads, carried, "the round trip keeps every PES byte");
	let keyframes: Vec<_> = frames.iter().map(|frame| frame.keyframe).collect();
	assert_eq!(keyframes, opens, "and its groups");
}

/// A sequence header split across two video PES, at each of its three inner boundaries, opens
/// its group at the PES it begins in, while a picture start code split the same way opens none.
/// A whole PES ending in part of a start code with no whole PES after it on its clock still goes
/// out, at a clock break and at the end of input.
#[tokio::test(start_paused = true)]
async fn mpeg2_sequence_header_split_across_pes_opens_a_group() {
	let mut mux = Mux::default();
	mux.pat(&[(1, PMT)]).pmt(PMT, 1, CLOCK, &[(0x02, VIDEO)], 0);
	let start = 2_000 * MS;
	// Each PES's payload after the picture start code it opens with, its last bytes completing
	// in the next PES: a sequence header at `00 00 | 01 b3`, `00 | 00 01 b3`, and `00 00 01 | b3`,
	// and a picture start code at `00 00 | 01 00`. A clock break cuts the sixth, its length
	// unbounded, so the fifth is the last whole PES on the old clock. The seventh, carrying a
	// whole sequence header, ends the input.
	let tails: [(&[u8], &[u8]); 7] = [
		(&[], &[0, 0]),
		(&[1, 0xb3], &[0]),
		(&[0, 1, 0xb3], &[]),
		(&[], &[0, 0, 1]),
		(&[0xb3], &[0, 0]),
		(&[1, 0], &[0]),
		(&[0, 0, 1, 0xb3], &[0, 0]),
	];
	let restart = 9_000 * MS;
	let mut pictures = Vec::new();
	for (i, (head, tail)) in tails.into_iter().enumerate() {
		let ticks = match i {
			6 => restart,
			_ => start + i as u64 * 40 * MS,
		};
		match i {
			6 => mux.packet(CLOCK, Flags::pcr(ticks).discontinuity()),
			_ => mux.packet(CLOCK, Flags::pcr(ticks)),
		};
		let mut pes = vec![0, 0, 1, 0xe0, 0, 0, 0x80, 0x80, 5];
		pes.extend_from_slice(&pts_field(ticks / 300 + LEAD));
		pes.extend_from_slice(head);
		pes.extend_from_slice(&[0, 0, 1, 0x00]);
		pes.resize(pes.len() + 200, 0x55);
		pes.extend_from_slice(tail);
		mux.whole(VIDEO, &pes);
		pictures.push(pes);
	}
	mux.packet(CLOCK, Flags::pcr(restart + 40 * MS));

	let imported = Imported::all(&mux.bytes());
	let frames = imported.frames(VIDEO).await;
	// The PES on the new clock has its PTS rebased, so compare around it.
	let unstamped = |pes: &[u8]| [&pes[..9], &pes[14..]].concat();
	let payloads: Vec<_> = frames.iter().map(|frame| unstamped(&frame.payload)).collect();
	pictures.remove(5);
	let pictures: Vec<_> = pictures.iter().map(|pes| unstamped(pes)).collect();
	assert_eq!(
		payloads, pictures,
		"every whole PES goes out, at the clock break and the end of input"
	);
	let keyframes: Vec<_> = frames.iter().map(|frame| frame.keyframe).collect();
	assert_eq!(keyframes, [true, true, false, true, false, true]);
}

/// Each ISO 13818-6 DSM-CC type rides DSM-CC sections, so each is reassembled as sections
/// rather than failing a PES start code.
#[tokio::test(start_paused = true)]
async fn dsmcc_types_ride_as_sections() {
	let types = [
		(0x0a, 0x110, 0x3a),
		(0x0b, 0x111, 0x3b),
		(0x0c, 0x112, 0x3d),
		(0x0d, 0x113, 0x3e),
	];
	let mut mux = Mux::default();
	let streams: Vec<_> = types.iter().map(|&(stream_type, pid, _)| (stream_type, pid)).collect();
	mux.pat(&[(1, PMT)]).pmt(PMT, 1, CLOCK, &streams, 0);
	mux.packet(CLOCK, Flags::pcr(0));
	let mut sections = Vec::new();
	for &(_, pid, table_id) in &types {
		let section = psi::section(table_id, pid, 0, 0, 0, &[pid as u8; 40]);
		mux.section(pid, &section);
		sections.push(section);
	}
	mux.packet(CLOCK, Flags::pcr(40 * MS));

	let imported = Imported::all(&mux.bytes());
	for (&(stream_type, pid, _), section) in types.iter().zip(sections) {
		let (_, entry) = imported.entry(pid).expect("a section track");
		assert_eq!(entry.verbatim.as_ref().map(|v| v.stream_type), Some(stream_type));
		let frames = imported.frames(pid).await;
		assert_eq!(frames.len(), 1, "{stream_type:#04x}");
		assert_eq!(frames[0].payload.as_ref(), section);
		let row = imported.row(pid);
		assert_eq!((row.units, row.damaged, row.refused), (1, 0, 0), "{stream_type:#04x}");
	}
}

/// Scrambled packets lose the section they interrupt, even when 16 of them wrap the continuity
/// counter so the clear packet after them looks contiguous with the partial.
#[tokio::test(start_paused = true)]
async fn scrambled_packets_lose_a_partial_section() {
	const DSMCC: u16 = 0x110;
	let mut mux = Mux::default();
	mux.pat(&[(1, PMT)]).pmt(PMT, 1, CLOCK, &[(0x0b, DSMCC)], 0);
	mux.packet(CLOCK, Flags::pcr(0));
	let at = mux.out.len();
	mux.section(DSMCC, &psi::section(0x3b, 1, 0, 0, 0, &[0x11; 300]));
	let tail = mux.out.split_off(at + 188);
	for cc in 1..=16u8 {
		let mut pkt = vec![0x47, (DSMCC >> 8) as u8, DSMCC as u8, 0x90 | (cc & 0x0f)];
		pkt.resize(188, 0xee);
		mux.out.extend_from_slice(&pkt);
	}
	mux.out.extend_from_slice(&tail);
	let clear = psi::section(0x3b, 2, 0, 0, 0, &[0x22; 40]);
	mux.section(DSMCC, &clear);
	mux.packet(CLOCK, Flags::pcr(40 * MS));

	let imported = Imported::all(&mux.bytes());
	let frames = imported.frames(DSMCC).await;
	let payloads: Vec<_> = frames.iter().map(|frame| frame.payload.to_vec()).collect();
	assert_eq!(payloads, [clear]);
	let row = imported.row(DSMCC);
	assert_eq!((row.units, row.refused, row.damaged), (1, 16, 0));
}

/// A 0x86 stream without a CUEI registration is refused once and never PES-parsed, while LATM
/// AAC, which nothing here decodes, rides whole.
#[tokio::test(start_paused = true)]
async fn ambiguous_streams_are_refused_and_latm_rides_whole() {
	const DTS: u16 = 0x120;
	const LATM: u16 = 0x121;
	let mut mux = Mux::default();
	mux.pat(&[(1, PMT)]).pmt(PMT, 1, CLOCK, &[(0x86, DTS), (0x11, LATM)], 0);
	mux.packet(CLOCK, Flags::pcr(0));
	let latm = audio(LEAD, &[0x56, 0xe0, 0x10, 0xaa, 0xbb]);
	mux.whole(LATM, &latm);
	mux.whole(DTS, &audio(LEAD, &[0x7f, 0xfe, 0x80, 0x01]));
	mux.packet(CLOCK, Flags::pcr(40 * MS));

	let imported = Imported::all(&mux.bytes());
	assert!(imported.entry(DTS).is_none());
	let row = imported.row(DTS);
	assert_eq!((row.track.as_str(), row.class), ("", Class::Data));
	assert_eq!((row.refused, row.damaged, row.units), (1, 0, 0));

	let (_, entry) = imported.entry(LATM).expect("LATM is carried");
	assert_eq!(entry.pes_packet.as_ref().map(|pes| pes.stream_type), Some(0x11));
	let frames = imported.frames(LATM).await;
	assert_eq!(frames.len(), 1);
	assert_eq!(frames[0].payload.as_ref(), latm);
}

/// Once the program's PMT is in, a packet on a PID nothing claims counts in its own row: the CAT,
/// an ATSC PSIP base PID, and a PID no table lists. Packets before the PAT or the PMT are
/// acquisition, and the null PID, the captured SI and the tables themselves are never counted.
/// A TS-scrambled packet on a carried stream is refused rather than damaged.
#[tokio::test(start_paused = true)]
async fn unclaimed_pids_count_once_the_pmt_is_in() {
	const CAT: u16 = 0x0001;
	const PSIP: u16 = 0x1ffb;
	let mut mux = Mux::default();
	// A mid-stream join: media and strays before the PAT, then before the PMT.
	mux.fill(DATA, 2).fill(UNLISTED, 2);
	mux.pat(&[(1, PMT)]);
	mux.fill(DATA, 1).fill(UNLISTED, 1);
	mux.pmt_with(PMT, 1, CLOCK, &[(0x06, DATA, KLVA)], 0);
	mux.packet(CLOCK, Flags::pcr(0));
	mux.section(CAT, &psi::section(0x01, 0xffff, 0, 0, 0, &[]));
	mux.fill(PSIP, 2).fill(UNLISTED, 3).fill(0x1fff, 2).fill(0x11, 1);
	mux.pat(&[(1, PMT)]).pmt_with(PMT, 1, CLOCK, &[(0x06, DATA, KLVA)], 0);
	scrambled(&mut mux, DATA);
	let pes = klv(0x5a, 64);
	mux.whole(DATA, &pes);
	mux.packet(CLOCK, Flags::pcr(40 * MS));

	let imported = Imported::all(&mux.bytes());
	let stats = imported.import.stats();
	let rows: Vec<u16> = stats.streams.keys().copied().collect();
	assert_eq!(rows, [CAT, DATA, UNLISTED, PSIP]);
	assert_eq!(
		stats.streams[&UNLISTED],
		Stream {
			class: Class::Data,
			refused: 3,
			..Default::default()
		}
	);
	for (pid, refused) in [(CAT, 1), (PSIP, 2)] {
		let row = &stats.streams[&pid];
		assert_eq!((row.track.as_str(), row.class, row.refused), ("", Class::Data, refused));
	}
	let data = &stats.streams[&DATA];
	assert_eq!(
		(data.track.as_str(), data.units, data.refused, data.damaged),
		(".ts", 1, 1, 0)
	);
	assert_eq!(imported.frames(DATA).await[0].payload.as_ref(), pes);
}

/// Two programs, the second's media and a stray PID ahead of its PMT. Under `--program all`
/// nothing counts until both PMTs are in, and a stray PID counts once for the multiplex. A
/// `--program 1` import cannot tell program 2's PIDs from strays, so it counts neither.
#[tokio::test(start_paused = true)]
async fn an_mpts_counts_strays_once_both_pmts_are_in() {
	const PMT2: u16 = 0x30;
	const DATA2: u16 = 0x201;
	let mut mux = Mux::default();
	mux.pat(&[(1, PMT), (2, PMT2)]);
	mux.pmt_with(PMT, 1, 0x1f1, &[(0x06, DATA, KLVA)], 0);
	mux.packet(0x1f1, Flags::pcr(0)).packet(0x1f2, Flags::pcr(0));
	mux.whole(DATA2, &klv(2, 32));
	mux.fill(UNLISTED, 2);
	mux.pmt_with(PMT2, 2, 0x1f2, &[(0x06, DATA2, KLVA)], 0);
	mux.whole(DATA, &klv(1, 32));
	mux.whole(DATA2, &klv(2, 32));
	mux.fill(UNLISTED, 4);
	mux.packet(0x1f1, Flags::pcr(40 * MS))
		.packet(0x1f2, Flags::pcr(40 * MS));
	let data = mux.bytes();

	let origin = crate::source::produce_origin();
	let mut programs = Programs::new(
		origin,
		moq_net::Path::from_static("event.hang"),
		crate::catalog::Config::default(),
	);
	programs.decode(&data).unwrap();
	let stats = programs.stats();
	assert_eq!(
		stats.streams[&UNLISTED],
		Stream {
			class: Class::Data,
			refused: 4,
			..Default::default()
		}
	);
	for pid in [DATA, DATA2] {
		assert_eq!(stats.streams[&pid].refused, 0, "{pid:#x}");
		assert_eq!(stats.streams[&pid].track, ".ts", "{pid:#x}");
	}

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(
		&mut broadcast,
		crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
	)
	.unwrap();
	let mut one = Import::new(broadcast, catalog.reserve()).with_program(1);
	one.decode(&data).unwrap();
	let rows: Vec<u16> = one.stats().streams.keys().copied().collect();
	assert_eq!(rows, [DATA], "only program 1's own PIDs");
}

/// A PMT section on another program's PMT PID claims nothing: program 2's PMT arriving on
/// program 1's PID never takes program 2's claim on a stray.
#[tokio::test(start_paused = true)]
async fn a_pmt_on_another_programs_pid_claims_nothing() {
	const PMT2: u16 = 0x30;
	const DATA2: u16 = 0x201;
	let mut mux = Mux::default();
	mux.pat(&[(1, PMT), (2, PMT2)]);
	mux.pmt_with(PMT, 1, CLOCK, &[(0x06, DATA, KLVA)], 0);
	mux.pmt_with(PMT2, 2, CLOCK, &[(0x06, DATA2, KLVA)], 0);
	mux.packet(CLOCK, Flags::pcr(0));
	mux.fill(UNLISTED, 2);
	mux.pmt_at(1, PMT, 2, CLOCK, &[(0x06, UNLISTED, KLVA)], 0);
	mux.fill(UNLISTED, 3);

	let origin = crate::source::produce_origin();
	let mut programs = Programs::new(
		origin,
		moq_net::Path::from_static("event.hang"),
		crate::catalog::Config::default(),
	);
	programs.decode(&mux.bytes()).unwrap();
	assert_eq!(programs.stats().streams[&UNLISTED].refused, 5);
}

/// One PID through its whole life, packets flowing throughout: unlisted, added by a PMT
/// revision, removed by the next, then added again. It keeps one row, whose counters never go
/// back, whichever owner holds it: the import while carried, the dropped-PID count otherwise,
/// which `Programs` holds for `--program all`.
#[tokio::test(start_paused = true)]
async fn a_pid_keeps_one_row_through_its_lifecycle() {
	let mut steps = Vec::new();
	let mut mux = Mux::default();
	mux.pat(&[(1, PMT)]).pmt_at(0, PMT, 1, CLOCK, &[(0x06, DATA, KLVA)], 0);
	mux.packet(CLOCK, Flags::pcr(0));
	mux.fill(UNLISTED, 3);
	steps.push(mux.out.len());
	mux.pmt_at(1, PMT, 1, CLOCK, &[(0x06, DATA, KLVA), (0x06, UNLISTED, KLVA)], 0);
	mux.packet(CLOCK, Flags::pcr(40 * MS));
	mux.whole(UNLISTED, &klv(1, 32));
	mux.whole(UNLISTED, &klv(2, 32));
	// A later PCR places them.
	mux.packet(CLOCK, Flags::pcr(60 * MS));
	steps.push(mux.out.len());
	mux.pmt_at(2, PMT, 1, CLOCK, &[(0x06, DATA, KLVA)], 0);
	mux.fill(UNLISTED, 4);
	steps.push(mux.out.len());
	mux.pmt_at(3, PMT, 1, CLOCK, &[(0x06, DATA, KLVA), (0x06, UNLISTED, KLVA)], 0);
	mux.packet(CLOCK, Flags::pcr(80 * MS));
	mux.whole(UNLISTED, &klv(3, 32));
	mux.packet(CLOCK, Flags::pcr(100 * MS));
	steps.push(mux.out.len());
	let data = mux.bytes();
	// (track, units, refused) after each step.
	let expected = [("", 0, 3), (".ts", 2, 3), ("", 2, 7), (".ts", 3, 7)];

	let mut one = Imported::new();
	let mut programs = Programs::new(
		crate::source::produce_origin(),
		moq_net::Path::from_static("event.hang"),
		crate::catalog::Config::default(),
	);
	let mut at = 0;
	for (end, (track, units, refused)) in steps.into_iter().zip(expected) {
		one.import.decode(&data[at..end]).unwrap();
		programs.decode(&data[at..end]).unwrap();
		at = end;
		for (owner, row) in [
			("import", one.row(UNLISTED)),
			("programs", programs.stats().streams[&UNLISTED].clone()),
		] {
			let class = row.class;
			assert_eq!(
				(row.track.as_str(), row.units, row.refused, row.damaged),
				(track, units, refused, 0),
				"{owner} at {end}"
			);
			assert_eq!(class, Class::Data, "{owner} at {end}");
		}
	}
}
