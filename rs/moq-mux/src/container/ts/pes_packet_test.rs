//! Import of undecoded PES streams as `pesPacket` tracks: each frame a whole PES, every byte as
//! received but the PTS and DTS moved onto the catalog clock, timestamped with the program clock
//! at its first byte.

use std::time::Duration;

use super::catalog::Ext;
use super::fixture::{Flags, MS, Mux, WRAP};

const PMT_PID: u16 = 0x20;
const VIDEO_PID: u16 = 0x100;
const DATA_PID: u16 = 0x104;
const CLOCK_PID: u16 = 0x1ff;
/// The PMT `PCR_PID` of a program with no PCR.
const NO_PCR: u16 = 0x1fff;

/// 2^33, where the PTS wraps.
const PTS_WRAP: u64 = 1 << 33;
/// The PCR leads each picture's PTS by this many 90 kHz ticks.
const LEAD: u64 = 9_000;

/// `registration_descriptor` naming SMPTE RP 217 KLV ('KLVA').
const KLVA: &[u8] = &[0x05, 0x04, b'K', b'L', b'V', b'A'];
/// `metadata_pointer`-free `metadata_descriptor` for synchronous KLV (ISO 13818-1 2.6.60).
const METADATA: &[u8] = &[0x26, 0x09, 0x01, 0x00, 0xff, b'K', b'L', b'V', b'A', 0x0f, 0x00];
/// DVB `AC-3_descriptor` with no optional fields.
const AC3: &[u8] = &[0x6a, 0x01, 0x00];

/// A transport stream under construction: one program clocked by PCRs on [`CLOCK_PID`], an
/// MPEG-2 video stream read only for its clock on [`VIDEO_PID`], and the private stream on
/// [`DATA_PID`]. It remembers where each PCR and PES went, to work out the expected placement.
struct Feed {
	mux: Mux,
	/// Each PCR's reference byte and its unwrapped 27 MHz ticks.
	pcrs: Vec<(u64, u64)>,
	/// Each private PES's start code offset and its bytes.
	pes: Vec<(u64, Vec<u8>)>,
}

impl Feed {
	fn new(stream_type: u8, descriptors: &[u8]) -> Self {
		Self::with_pcr(stream_type, descriptors, CLOCK_PID)
	}

	fn with_pcr(stream_type: u8, descriptors: &[u8], pcr_pid: u16) -> Self {
		let mut mux = Mux::default();
		mux.pat(&[(1, PMT_PID)]).pmt_with(
			PMT_PID,
			1,
			pcr_pid,
			&[(0x02, VIDEO_PID, &[]), (stream_type, DATA_PID, descriptors)],
			0,
		);
		Self {
			mux,
			pcrs: Vec::new(),
			pes: Vec::new(),
		}
	}

	/// A PCR of `ticks`, unwrapped: the packet carries them modulo the wrap.
	fn pcr(&mut self, ticks: u64) -> &mut Self {
		self.pcrs.push((self.mux.out.len() as u64 + 10, ticks));
		self.mux.packet(CLOCK_PID, Flags::pcr(ticks % WRAP));
		self
	}

	/// A picture presenting `LEAD` after the program clock `ticks`, unwrapped.
	fn picture(&mut self, ticks: u64) -> &mut Self {
		self.mux
			.pes(VIDEO_PID, Flags::default(), (ticks / 300 + LEAD) % PTS_WRAP);
		self.mux.fill(VIDEO_PID, 2);
		self
	}

	fn data(&mut self, pes: Vec<u8>) -> &mut Self {
		let at = self.mux.whole(DATA_PID, &pes);
		self.pes.push((at, pes));
		self
	}

	/// A PES whose packets straddle a PCR of `ticks`: its first `head` packets come before it.
	fn data_across(&mut self, pes: Vec<u8>, head: usize, ticks: u64) -> &mut Self {
		let (start, packets) = self.mux.split(DATA_PID, &pes);
		self.pes.push(((self.mux.out.len() + start) as u64, pes));
		self.mux.out.extend(packets[..head].concat());
		self.pcr(ticks);
		self.mux.out.extend(packets[head..].concat());
		self
	}

	/// The program clock at input offset `at` in unwrapped 27 MHz ticks: interpolated between
	/// the PCRs either side of it, extrapolated from the last two past the last. `None` before
	/// the first PCR or past a lone one.
	fn clock(&self, at: u64) -> Option<u64> {
		let before = self.pcrs.iter().rposition(|&(pcr, _)| pcr <= at)?;
		let (a, b) = match self.pcrs.get(before + 1) {
			Some(&after) => (self.pcrs[before], after),
			None if before > 0 => (self.pcrs[before - 1], self.pcrs[before]),
			None => return None,
		};
		Some(a.1 + (at - a.0) * (b.1 - a.1) / (b.0 - a.0))
	}

	fn bytes(&self) -> Vec<u8> {
		self.mux.bytes()
	}
}

/// A PES with an optional header: `marker` is its first flags byte, `flags` its second, `fields`
/// the optional fields those flags name, then `stuffing` bytes of 0xFF.
fn pes_with(stream_id: u8, marker: u8, flags: u8, fields: &[u8], stuffing: usize, payload: &[u8]) -> Vec<u8> {
	let header = fields.len() + stuffing;
	let mut pes = vec![0, 0, 1, stream_id];
	pes.extend_from_slice(&((3 + header + payload.len()) as u16).to_be_bytes());
	pes.extend_from_slice(&[marker, flags, header as u8]);
	pes.extend_from_slice(fields);
	pes.resize(pes.len() + stuffing, 0xff);
	pes.extend_from_slice(payload);
	pes
}

/// A PES of a `stream_id` with no optional header.
fn pes_bare(stream_id: u8, payload: &[u8]) -> Vec<u8> {
	let mut pes = vec![0, 0, 1, stream_id];
	pes.extend_from_slice(&(payload.len() as u16).to_be_bytes());
	pes.extend_from_slice(payload);
	pes
}

/// A 33-bit PTS or DTS field behind its 4-bit `prefix`.
fn clock_field(prefix: u8, ticks: u64) -> [u8; 5] {
	[
		prefix << 4 | ((ticks >> 29) & 0x0e) as u8 | 1,
		(ticks >> 22) as u8,
		((ticks >> 14) & 0xfe) as u8 | 1,
		(ticks >> 7) as u8,
		((ticks << 1) & 0xfe) as u8 | 1,
	]
}

/// The value of a PTS or DTS field.
fn clock_value(field: &[u8]) -> u64 {
	(u64::from(field[0] >> 1) & 0x07) << 30
		| u64::from(field[1]) << 22
		| u64::from(field[2] >> 1) << 15
		| u64::from(field[3]) << 7
		| u64::from(field[4] >> 1)
}

/// An asynchronous KLV PES (private_stream_1, no PTS) of `len` payload bytes, filled with `fill`.
fn klv(fill: u8, len: usize) -> Vec<u8> {
	// A SMPTE universal label, a BER length, then the value.
	let mut payload = vec![
		0x06, 0x0e, 0x2b, 0x34, 0x02, 0x0b, 0x01, 0x01, 0x0e, 0x01, 0x03, 0x01, 0x01, 0x00, 0x00, 0x00,
	];
	payload.extend_from_slice(&[0x82, (len >> 8) as u8, len as u8]);
	payload.resize(payload.len() + len, fill);
	pes_with(0xbd, 0x84, 0x00, &[], 0, &payload)
}

/// A synchronous KLV PES (metadata stream, stream_id 0xFC) presenting at `pts`.
fn sync_klv(fill: u8, pts: u64) -> Vec<u8> {
	// The metadata AU cell header (ISO 13818-1 2.12.4), then a KLV packet.
	let mut payload = vec![0x00, 0x00, 0xdf, 0x00, 0x14];
	payload.extend_from_slice(&[0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x01, 0x0e, 0x01, 0x03, 0x01]);
	payload.resize(payload.len() + 8, fill);
	pes_with(0xfc, 0x84, 0x80, &clock_field(0x2, pts), 0, &payload)
}

/// An imported feed: everything it published, and how it went.
struct Imported {
	consumer: moq_net::broadcast::Consumer,
	catalog: crate::catalog::Producer<Ext>,
	import: super::Import<Ext>,
	/// The catalog clock's shift of the source's timestamps, in microseconds.
	offset: i128,
}

/// Import `data`, onto a catalog clock already fixed when `captured` (so the timestamps shift),
/// or one the first PTS places (so they stay verbatim).
fn import(data: &[u8], captured: bool) -> anyhow::Result<Imported> {
	let mut import = start(captured);
	import.import.decode(data)?;
	import.import.finish()?;
	Ok(import)
}

fn start(captured: bool) -> Imported {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(
		&mut broadcast,
		crate::catalog::Config::default().with_catalog(crate::catalog::hang::Catalog::<Ext>::default()),
	)
	.unwrap();
	// Paused, the clock reads the same until the first PTS anchors on it.
	let offset = match captured {
		true => catalog.clock().now().as_micros() as i128,
		false => 0,
	};
	let import = super::Import::new(broadcast, catalog.reserve());
	Imported {
		consumer,
		catalog,
		import,
		offset,
	}
}

impl Imported {
	/// The catalog's `pesPacket` track on [`DATA_PID`]: its name and entry.
	fn entry(&self) -> Option<(String, super::catalog::Track)> {
		self.catalog
			.snapshot()
			.ext
			.mpegts
			.tracks
			.into_iter()
			.find(|(_, track)| track.pid == DATA_PID && track.pes_packet.is_some())
	}

	/// The [`DATA_PID`] row of the importer's stats.
	fn stats(&self) -> super::stats::Stream {
		self.import.stats().streams.get(&DATA_PID).cloned().unwrap_or_default()
	}
}

/// Every frame on track `name`, and the error that ended it if one did.
async fn read(consumer: &moq_net::broadcast::Consumer, name: &str) -> (Vec<crate::container::Frame>, Option<String>) {
	let (frames, end) = read_until(consumer, name).await;
	(frames, end.err())
}

/// Every frame on track `name`, and how it ended: `Ok(true)` finished, `Ok(false)` gone quiet
/// still open, or the error that ended it.
async fn read_until(
	consumer: &moq_net::broadcast::Consumer,
	name: &str,
) -> (Vec<crate::container::Frame>, Result<bool, String>) {
	let subscription = moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(3600));
	let track = consumer.track(name).unwrap().subscribe(subscription).await.unwrap();
	let mut reader = crate::container::Consumer::new(
		track,
		crate::catalog::hang::Container::Legacy(crate::container::Kind::Data),
	);
	let mut frames = Vec::new();
	loop {
		match kio::clock::timeout(Duration::from_millis(50), reader.read_frame()).await {
			Ok(Ok(Some(frame))) => frames.push(frame),
			Ok(Ok(None)) => return (frames, Ok(true)),
			Err(_) => return (frames, Ok(false)),
			Ok(Err(err)) => return (frames, Err(err.to_string())),
		}
	}
}

/// The microsecond timestamp the program clock `ticks` places a PES at, shifted by `offset`.
fn placed(ticks: u64, offset: i128) -> u64 {
	u64::try_from(i128::from(ticks / 27) + offset).unwrap()
}

/// Asynchronous KLV, with no PTS, takes the program clock at its first byte: the reproduction of
/// the PES that were stamped zero and then dropped by the export. It does so before the first
/// picture and between pictures, in a PES spanning packets and one straddling a PCR, and past the
/// last PCR by extrapolating from the last two. One before the first PCR has nothing to place it.
#[tokio::test(start_paused = true)]
async fn async_klv_takes_the_program_clock_at_its_first_byte() {
	let mut feed = Feed::new(0x06, KLVA);
	feed.data(klv(0, 8));
	let start = 2_000 * MS;
	feed.pcr(start).data(klv(1, 8));
	for i in 0..10 {
		let ticks = start + i * 40 * MS;
		feed.picture(ticks);
		match i {
			3 => feed.data(klv(2, 600)),
			5 => feed.data_across(klv(3, 400), 1, ticks + 20 * MS),
			_ => feed.data(klv(i as u8 + 0x10, 32)),
		};
		feed.mux.fill(VIDEO_PID, 3);
		feed.pcr(ticks + 40 * MS);
	}
	feed.picture(start + 400 * MS).data(klv(4, 8));

	let imported = import(&feed.bytes(), false).unwrap();
	let (name, entry) = imported.entry().expect("the KLV stream is in the catalog");
	assert_eq!(entry.pes_packet.as_ref().unwrap().stream_type, 0x06);
	assert_eq!(entry.descriptors.len(), 1);
	assert_eq!(entry.descriptors[0].tag, 0x05);
	assert_eq!(&entry.descriptors[0].data[..], b"KLVA");

	let (frames, err) = read(&imported.consumer, &name).await;
	assert!(err.is_none(), "{err:?}");
	let expected: Vec<_> = feed.pes[1..]
		.iter()
		.map(|(at, pes)| (placed(feed.clock(*at).unwrap(), 0), pes.clone()))
		.collect();
	let got: Vec<_> = frames
		.iter()
		.map(|frame| (frame.timestamp.as_micros() as u64, frame.payload.to_vec()))
		.collect();
	assert_eq!(got.len(), expected.len());
	for (k, (got, expected)) in got.iter().zip(&expected).enumerate() {
		assert_eq!(got.0, expected.0, "PES {k} timestamp");
		assert_eq!(got.1, expected.1, "PES {k} bytes");
		assert!(got.0 >= 2_000_000, "PES {k} is on the program clock");
	}
	assert!(frames.iter().all(|frame| frame.keyframe));
	let stats = imported.stats();
	assert_eq!(stats.unplaced, 1, "the PES before the first PCR");
	assert_eq!(stats.refused, 0);
	assert_eq!(stats.units, expected.len() as u64);
}

/// Synchronous KLV starting just before the 33-bit wrap, onto a catalog clock already running:
/// each PTS and DTS moves by the timebase's shift modulo 2^33, keeping its prefix and markers,
/// PTS zero stays a PTS while no PTS stays none, and every other byte is untouched.
#[tokio::test(start_paused = true)]
async fn sync_klv_rebases_its_pts_and_dts_onto_the_catalog_clock() {
	let mut feed = Feed::new(0x15, METADATA);
	// The first picture presents one second before the PTS wraps.
	let start = (PTS_WRAP - 90_000 - LEAD) * 300;
	feed.pcr(start);
	let mut wrapped = 0;
	for i in 0..60 {
		let ticks = start + i * 40 * MS;
		feed.picture(ticks);
		let pts = (ticks / 300 + LEAD) % PTS_WRAP;
		match i {
			// Its decode time a frame before its presentation.
			10 => {
				let mut fields = clock_field(0x3, pts).to_vec();
				fields.extend_from_slice(&clock_field(0x1, pts - 3_600));
				feed.data(pes_with(0xfc, 0x84, 0xc0, &fields, 0, b"distinct"));
			}
			// No PTS at all.
			20 => {
				feed.data(pes_with(0xfc, 0x84, 0x00, &[], 0, b"untimed"));
			}
			_ if pts < 3_600 => {
				// The picture after the wrap: a PTS of zero.
				wrapped += 1;
				feed.data(sync_klv(0xee, 0));
			}
			_ => {
				feed.data(sync_klv(i as u8, pts));
			}
		}
		feed.pcr(ticks + 40 * MS);
	}
	assert_eq!(wrapped, 1, "one picture lands on PTS zero");

	let imported = import(&feed.bytes(), true).unwrap();
	let (name, entry) = imported.entry().expect("the KLV stream is in the catalog");
	assert_eq!(entry.pes_packet.as_ref().unwrap().stream_type, 0x15);
	// The first PTS anchored the timebase at the captured reading.
	let first = (start / 300 + LEAD) * 1_000_000 / 90_000;
	let offset = imported.offset - first as i128;
	let shift = (offset * 9).div_euclid(100);
	assert!(shift < -(1 << 32), "the shift carries PTS past the wrap: {shift}");

	let (frames, err) = read(&imported.consumer, &name).await;
	assert!(err.is_none(), "{err:?}");
	assert_eq!(frames.len(), feed.pes.len());
	for (k, (frame, (at, pes))) in frames.iter().zip(&feed.pes).enumerate() {
		assert_eq!(
			frame.timestamp.as_micros() as u64,
			placed(feed.clock(*at).unwrap(), offset),
			"PES {k} timestamp"
		);
		let got = &frame.payload[..];
		assert_eq!(got.len(), pes.len(), "PES {k} length");
		let fields = match pes[7] >> 6 {
			0b10 => 1,
			0b11 => 2,
			_ => 0,
		};
		assert_eq!(got[..9], pes[..9], "PES {k} header");
		for field in 0..fields {
			let range = 9 + 5 * field..14 + 5 * field;
			let raw = clock_value(&pes[range.clone()]);
			let rebased = (i128::from(raw) + shift).rem_euclid(1 << 33) as u64;
			assert_eq!(
				got[range.clone()],
				clock_field(pes[range.start] >> 4, rebased),
				"PES {k} clock field {field}"
			);
		}
		assert_eq!(got[9 + 5 * fields..], pes[9 + 5 * fields..], "PES {k} after its clocks");
	}
	// A PTS of zero is still a PTS, and none is still none.
	let zero = feed.pes.iter().position(|(_, pes)| pes.ends_with(&[0xee; 8])).unwrap();
	assert_eq!(frames[zero].payload[7] >> 6, 0b10);
	assert_eq!(
		clock_value(&frames[zero].payload[9..14]),
		shift.rem_euclid(1 << 33) as u64
	);
	let untimed = feed.pes.iter().position(|(_, pes)| pes.ends_with(b"untimed")).unwrap();
	assert_eq!(frames[untimed].payload[7] >> 6, 0b00);
}

/// Every optional field a PES can carry without a clock of its own goes through as received,
/// as do `data_alignment_indicator` clear, stuffing, and the stream IDs with no optional header.
/// A PES CRC covers the previous PES's data bytes, so it still checks.
#[tokio::test(start_paused = true)]
async fn optional_fields_and_bare_stream_ids_go_through_untouched() {
	let mut feed = Feed::new(0x06, &[]);
	let start = 1_000 * MS;
	feed.pcr(start);

	let previous = b"the data bytes the next PES's CRC covers".to_vec();
	let mut fields = clock_field(0x3, 99_000).to_vec();
	fields.extend_from_slice(&clock_field(0x1, 95_400));
	// ES_rate, DSM trick mode, additional_copy_info, previous_PES_packet_CRC.
	fields.extend_from_slice(&[0x80, 0x10, 0x01]);
	fields.push(0x1f);
	fields.push(0x80 | 0x2a);
	fields.extend_from_slice(&crc16(&previous).to_be_bytes());
	// The extension: private data, a sequence counter, P-STD, and extension 2 naming a
	// stream_id_extension.
	fields.push(0x80 | 0x20 | 0x10 | 0x0e | 0x01);
	fields.extend_from_slice(&[0x5a; 16]);
	fields.extend_from_slice(&[0x80 | 0x05, 0x80 | 0x02]);
	fields.extend_from_slice(&[0x60, 0x20]);
	fields.extend_from_slice(&[0x80 | 0x03, 0x71, 0xaa, 0xbb]);
	let crc = pes_with(0xbd, 0x80, 0xc0 | 0x10 | 0x08 | 0x04 | 0x02 | 0x01, &fields, 3, b"crc");

	let pes = [
		pes_with(0xbd, 0x80, 0x00, &[], 0, &previous),
		crc,
		pes_bare(0xbf, b"private_stream_2"),
		pes_bare(0xbe, &[0xff; 20]),
		pes_bare(0xf0, b"ECM"),
		pes_bare(0xf1, b"EMM"),
		pes_bare(0xf2, b"DSM-CC"),
		pes_bare(0xf8, b"H.222.1 type E"),
		pes_bare(0xff, b"directory"),
		pes_with(0xbd, 0x80, 0x00, &[], 7, b"stuffed"),
	];
	for (i, pes) in pes.into_iter().enumerate() {
		let ticks = start + i as u64 * 40 * MS;
		feed.picture(ticks).data(pes).pcr(ticks + 40 * MS);
	}

	let imported = import(&feed.bytes(), true).unwrap();
	let (name, _) = imported.entry().unwrap();
	let (frames, err) = read(&imported.consumer, &name).await;
	assert!(err.is_none(), "{err:?}");
	assert_eq!(frames.len(), feed.pes.len());
	let first = (start / 300 + LEAD) * 1_000_000 / 90_000;
	let shift = ((imported.offset - first as i128) * 9).div_euclid(100);
	assert_ne!(shift, 0, "the timestamps shift");
	for (k, (frame, (_, pes))) in frames.iter().zip(&feed.pes).enumerate() {
		match k {
			// Only its PTS and DTS move.
			1 => {
				let mut expected = pes.clone();
				for field in [9..14, 14..19] {
					let raw = clock_value(&pes[field.clone()]);
					let rebased = (i128::from(raw) + shift).rem_euclid(1 << 33) as u64;
					expected[field.clone()].copy_from_slice(&clock_field(pes[field.start] >> 4, rebased));
				}
				assert_eq!(&frame.payload[..], &expected[..], "PES {k}");
			}
			_ => assert_eq!(&frame.payload[..], &pes[..], "PES {k}"),
		}
	}
	let carried = &frames[1].payload[..];
	let at = 9 + 10 + 3 + 1 + 1;
	assert_eq!(
		u16::from_be_bytes([carried[at], carried[at + 1]]),
		crc16(&frames[0].payload[9..]),
		"the CRC still covers the previous PES's data bytes"
	);
}

/// The CRC-16 a `previous_PES_packet_CRC` carries: polynomial 0x1021, registers preset to ones.
fn crc16(data: &[u8]) -> u16 {
	let mut crc = 0xffffu16;
	for &byte in data {
		crc ^= u16::from(byte) << 8;
		for _ in 0..8 {
			crc = if crc & 0x8000 != 0 { crc << 1 ^ 0x1021 } else { crc << 1 };
		}
	}
	crc
}

/// DVB AC-3 (private data with an AC-3 descriptor) is not decoded, so a PES of several sync
/// frames stays one frame, which the export writes back as one PES.
#[tokio::test(start_paused = true)]
async fn dvb_ac3_keeps_its_sync_frames_in_one_pes() {
	let mut sync = vec![0x0b, 0x77, 0x00, 0x00, 0x00, 0x40, 0x40];
	sync.resize(128, 0x00);
	let mut feed = Feed::new(0x06, AC3);
	let start = 1_000 * MS;
	feed.pcr(start);
	for i in 0..4 {
		let ticks = start + i * 96 * MS;
		let pts = ticks / 300 + LEAD;
		let payload: Vec<u8> = (0..3).flat_map(|_| sync.clone()).collect();
		feed.picture(ticks)
			.data(pes_with(0xbd, 0x84, 0x80, &clock_field(0x2, pts), 0, &payload))
			.pcr(ticks + 96 * MS);
	}

	let imported = import(&feed.bytes(), false).unwrap();
	let (name, entry) = imported.entry().unwrap();
	assert_eq!(entry.descriptors[0].tag, 0x6a);
	let (frames, _) = read(&imported.consumer, &name).await;
	let got: Vec<_> = frames.iter().map(|frame| frame.payload.to_vec()).collect();
	let expected: Vec<_> = feed.pes.iter().map(|(_, pes)| pes.clone()).collect();
	assert_eq!(got, expected);
}

/// A PES the track cannot carry unaltered, or whose header export refuses, refuses its stream:
/// what was already placed stays published, the track ends with an error and leaves the catalog,
/// and the refusal is counted.
#[tokio::test(start_paused = true)]
async fn a_pes_with_a_field_no_rebase_reaches_refuses_its_stream() {
	let cases: [(&str, Vec<u8>); 8] = [
		// PES_extension_flag set with no header byte left for the extension.
		("truncated extension", pes_with(0xbd, 0x80, 0x01, &[], 0, b"ext")),
		// ES_rate flagged with one of its three bytes inside PES_header_data_length.
		("overrunning ES_rate", pes_with(0xbd, 0x80, 0x10, &[0x80], 0, b"rate")),
		// No optional header to carry, but PES_packet_length 0 on a stream that is not video.
		("unbounded without an optional header", pes_bare(0xbf, b"")),
		(
			"ESCR",
			pes_with(0xbd, 0x80, 0x20, &[0x04, 0x00, 0x04, 0x00, 0x04, 0x01], 0, b"escr"),
		),
		(
			"pack_header_field",
			pes_with(0xbd, 0x80, 0x01, &[0x40, 0x01, 0xba], 0, b"pack"),
		),
		(
			"TREF",
			pes_with(
				0xbd,
				0x80,
				0x01,
				&[0x01, 0x80 | 0x06, 0xfe, 0x21, 0x00, 0x01, 0x00, 0x01],
				0,
				b"tref",
			),
		),
		("scrambling", pes_with(0xbd, 0x90, 0x00, &[], 0, b"scrambled")),
		("unbounded", {
			let mut pes = pes_with(0xbd, 0x80, 0x00, &[], 0, b"unbounded");
			pes[4..6].fill(0);
			pes
		}),
	];
	for (why, bad) in cases {
		let mut feed = Feed::new(0x06, &[]);
		let start = 1_000 * MS;
		feed.pcr(start).picture(start).data(klv(1, 8)).pcr(start + 40 * MS);
		let head = feed.bytes();
		feed.picture(start + 40 * MS)
			.data(bad)
			.pcr(start + 80 * MS)
			.picture(start + 80 * MS)
			.data(klv(2, 8))
			.pcr(start + 120 * MS);

		let mut imported = start_import(&head);
		let (name, _) = imported.entry().expect("the stream starts out carried");
		let reader = {
			let consumer = imported.consumer.clone();
			let name = name.clone();
			tokio::spawn(async move { read(&consumer, &name).await })
		};
		for _ in 0..10 {
			tokio::task::yield_now().await;
		}
		imported.import.decode(&feed.bytes()[head.len()..]).unwrap();
		imported.import.finish().unwrap();

		let (frames, err) = reader.await.unwrap();
		assert_eq!(frames.len(), 1, "{why}: the PES placed before it stays published");
		assert!(err.is_some(), "{why}: the track ends with an error");
		assert!(imported.entry().is_none(), "{why}: the track left the catalog");
		assert_eq!(imported.stats().refused, 1, "{why}");
	}
}

/// Begin importing `head` onto a clock the first PTS places.
fn start_import(head: &[u8]) -> Imported {
	let mut imported = start(false);
	imported.import.decode(head).unwrap();
	imported
}

/// A program with no PCR has nothing to place a private PES by, so the stream is refused before
/// any track is advertised.
#[tokio::test(start_paused = true)]
async fn a_program_without_a_pcr_refuses_private_pes() {
	let mut feed = Feed::with_pcr(0x06, KLVA, NO_PCR);
	for i in 0..5 {
		feed.picture(1_000 * MS + i * 40 * MS).data(klv(i as u8, 8));
	}
	let imported = import(&feed.bytes(), false).unwrap();
	assert!(imported.entry().is_none(), "no track is advertised");
	assert_eq!(imported.stats().refused, 1);
}

/// A PES after a lone PCR has no interval to extrapolate by, so it is dropped and counted.
#[tokio::test(start_paused = true)]
async fn a_pes_past_a_lone_pcr_is_unplaced() {
	let mut feed = Feed::new(0x06, KLVA);
	feed.pcr(1_000 * MS).picture(1_000 * MS).data(klv(0, 8));
	let imported = import(&feed.bytes(), false).unwrap();
	let (name, _) = imported.entry().unwrap();
	let (frames, _) = read(&imported.consumer, &name).await;
	assert!(frames.is_empty());
	assert_eq!(imported.stats().unplaced, 1);
}

/// A PMT declaring media still waits for its PTS before anchoring, so its
/// PES are refused at the end of the input rather than published on a guessed clock.
#[test]
fn a_program_with_missing_media_timestamps_is_refused() {
	let mut feed = Feed::new(0x06, KLVA);
	for i in 0..10 {
		feed.pcr(1_000 * MS + i * 40 * MS).data(klv(i as u8, 8));
	}
	let err = import(&feed.bytes(), false).err().expect("the import is refused");
	assert!(err.to_string().contains("without a media timestamp"), "{err:#}");
}

/// No media PTS is needed when the PMT declares only data: PCR carries the era too.
#[tokio::test(start_paused = true)]
async fn a_data_only_program_preserves_placements_across_the_pcr_wrap() {
	let mut mux = Mux::default();
	mux.pat(&[(1, PMT_PID)])
		.pmt_with(PMT_PID, 1, CLOCK_PID, &[(0x06, DATA_PID, KLVA)], 0);
	let mut feed = Feed {
		mux,
		pcrs: Vec::new(),
		pes: Vec::new(),
	};
	let start = WRAP - 100 * MS;
	for i in 0..10 {
		feed.pcr(start + i * 40 * MS).data(klv(i as u8, 8));
	}
	feed.pcr(start + 400 * MS);
	let imported = import(&feed.bytes(), false).unwrap();
	let (name, _) = imported.entry().unwrap();
	let (frames, err) = read(&imported.consumer, &name).await;
	assert!(err.is_none());
	assert_eq!(frames.len(), feed.pes.len());
	for (frame, (at, pes)) in frames.iter().zip(&feed.pes) {
		assert_eq!(frame.payload.as_ref(), pes);
		assert_eq!(
			frame.timestamp.as_micros(),
			u128::from(placed(feed.clock(*at).unwrap(), 0))
		);
	}
	assert!(frames.windows(2).all(|pair| pair[0].timestamp < pair[1].timestamp));
}

#[test]
fn a_data_only_program_without_pcr_refuses_sections_and_whole_pes() {
	let mut mux = Mux::default();
	mux.pat(&[(1, PMT_PID)])
		.pmt(PMT_PID, 1, NO_PCR, &[(0x06, DATA_PID), (0x05, DATA_PID + 1)], 0);
	mux.whole(DATA_PID, &klv(0, 8));
	mux.section(DATA_PID + 1, &[0xfc, 0x30, 0]);
	let imported = import(&mux.bytes(), false).unwrap();
	assert!(imported.catalog.snapshot().ext.mpegts.tracks.is_empty());
	let stats = imported.import.stats();
	for pid in [DATA_PID, DATA_PID + 1] {
		assert_eq!(stats.streams[&pid].refused, 1);
	}
}

/// A flagged clock break ends the placement timebase: the PES past its last PCR is extrapolated
/// on the old one, and those after the break are placed by the new one's PCRs.
#[tokio::test(start_paused = true)]
async fn a_clock_break_places_each_side_on_its_own_timebase() {
	let mut feed = Feed::new(0x06, KLVA);
	let start = 1_000 * MS;
	feed.pcr(start).picture(start).pcr(start + 40 * MS).data(klv(1, 8));
	let before = feed.clock(feed.pes[0].0).unwrap();
	let jump = 5_000 * MS;
	feed.pcrs.clear();
	feed.mux.packet(CLOCK_PID, Flags::pcr(jump).discontinuity());
	feed.pcrs.push((feed.mux.out.len() as u64 - 188 + 10, jump));
	feed.picture(jump).data(klv(2, 8)).pcr(jump + 40 * MS);

	let imported = import(&feed.bytes(), false).unwrap();
	let (name, _) = imported.entry().unwrap();
	let (frames, err) = read(&imported.consumer, &name).await;
	assert!(err.is_none(), "{err:?}");
	assert_eq!(frames.len(), 2);
	assert_eq!(frames[0].timestamp.as_micros() as u64, placed(before, 0));
	assert!(frames[1].timestamp > frames[0].timestamp);
	assert_eq!(&frames[1].payload[..], &feed.pes[1].1[..]);
}

/// A PMT moving the PCR to another PID ends the placement timebase too: the PES past the old
/// clock's last PCR is extrapolated on it, never interpolated toward the new clock's first PCR,
/// and those after the switch are placed by the new clock alone.
#[tokio::test(start_paused = true)]
async fn a_pcr_pid_switch_places_each_side_on_its_own_timebase() {
	const NEW_CLOCK_PID: u16 = 0x1fe;
	let mut feed = Feed::new(0x06, KLVA);
	let start = 1_000 * MS;
	feed.pcr(start).picture(start).pcr(start + 40 * MS).data(klv(1, 8));
	let before = feed.clock(feed.pes[0].0).unwrap();
	feed.mux.pmt_with(
		PMT_PID,
		1,
		NEW_CLOCK_PID,
		&[(0x02, VIDEO_PID, &[]), (0x06, DATA_PID, KLVA)],
		0,
	);
	// A clock far from the old one, so a PES placed across the two lands far from either.
	let other = 5_000 * MS;
	feed.pcrs.clear();
	for i in 0..3 {
		feed.pcrs.push((feed.mux.out.len() as u64 + 10, other + i * 40 * MS));
		feed.mux.packet(NEW_CLOCK_PID, Flags::pcr(other + i * 40 * MS));
		if i == 1 {
			feed.data(klv(2, 8));
		}
	}
	let after = feed.clock(feed.pes[1].0).unwrap();

	let imported = import(&feed.bytes(), false).unwrap();
	let (name, _) = imported.entry().unwrap();
	let (frames, err) = read(&imported.consumer, &name).await;
	assert!(err.is_none(), "{err:?}");
	let got: Vec<_> = frames.iter().map(|frame| frame.timestamp.as_micros() as u64).collect();
	assert_eq!(got, [placed(before, 0), placed(after, 0)]);
	assert_eq!(imported.stats().unplaced, 0);
}

/// Whole PES held for a timestamp count against the staging unit bound as sections do, so a
/// flood of tiny ones refuses the import long before their bookkeeping outgrows the byte bound.
#[test]
fn tiny_whole_pes_held_for_a_timestamp_are_bounded_by_count() {
	let units = super::passthrough::GROUP_OBJECTS_MAX;
	let mut feed = Feed::new(0x06, KLVA);
	feed.pcr(1_000 * MS);
	for _ in 0..=units {
		feed.data(pes_with(0xbd, 0x80, 0x00, &[], 0, b""));
	}
	let mut imported = start(false);
	let err = imported.import.decode(&feed.bytes()).expect_err("the flood is refused");
	assert!(
		err.to_string()
			.contains(&format!("TS staging exceeds {units} buffered units")),
		"{err:#}"
	);
}

/// A PES whose header spans packets is placed from its start code, not from where the header
/// completes: one begun before the first PCR stays unplaced though a PCR lands mid-header, and
/// one begun after a PCR is bracketed by the PCR that lands mid-header.
#[tokio::test(start_paused = true)]
async fn a_pes_header_split_by_a_pcr_is_placed_from_its_start_code() {
	let mut feed = Feed::new(0x06, KLVA);
	let start = 1_000 * MS;
	// 9 + 200 header bytes, so each header ends in its PES's second packet.
	let split = |fill: u8| pes_with(0xbd, 0x80, 0x00, &[], 200, &[fill; 8]);
	feed.data_across(split(1), 1, start);
	feed.picture(start).pcr(start + 40 * MS);
	feed.data_across(split(2), 1, start + 80 * MS);
	feed.pcr(start + 120 * MS);
	let expected = placed(feed.clock(feed.pes[1].0).unwrap(), 0);

	let imported = import(&feed.bytes(), false).unwrap();
	let (name, _) = imported.entry().unwrap();
	let (frames, err) = read(&imported.consumer, &name).await;
	assert!(err.is_none(), "{err:?}");
	assert_eq!(frames.len(), 1);
	assert_eq!(frames[0].timestamp.as_micros() as u64, expected);
	assert_eq!(&frames[0].payload[..], &feed.pes[1].1[..]);
	assert_eq!(imported.stats().unplaced, 1, "the PES begun before the first PCR");
}

/// A header held past more PCRs than the placement keeps is still bracketed by the first one past
/// its start code. The PCRs run at an uneven rate, so any later pair would place it elsewhere.
#[tokio::test(start_paused = true)]
async fn a_pes_header_held_past_several_pcrs_takes_the_first_after_it() {
	let mut feed = Feed::new(0x06, KLVA);
	let start = 1_000 * MS;
	feed.pcr(start).picture(start).pcr(start + 40 * MS);
	// 9 + 200 header bytes, so the header ends in the PES's second packet.
	let pes = pes_with(0xbd, 0x80, 0x00, &[], 200, &[1; 8]);
	let (head, packets) = feed.mux.split(DATA_PID, &pes);
	feed.pes.push(((feed.mux.out.len() + head) as u64, pes));
	feed.mux.out.extend_from_slice(&packets[0]);
	for ticks in [50, 300, 310] {
		feed.pcr(start + ticks * MS);
	}
	feed.mux.out.extend(packets[1..].concat());
	feed.pcr(start + 320 * MS);
	let expected = placed(feed.clock(feed.pes[0].0).unwrap(), 0);

	let imported = import(&feed.bytes(), false).unwrap();
	let (name, _) = imported.entry().unwrap();
	let (frames, err) = read(&imported.consumer, &name).await;
	assert!(err.is_none(), "{err:?}");
	assert_eq!(frames.len(), 1);
	assert_eq!(frames[0].timestamp.as_micros() as u64, expected);
}

/// A later PMT that names a PCR carries the private PES stream refused for want of one, and one
/// that drops the PCR again refuses the stream it carried.
#[tokio::test(start_paused = true)]
async fn a_pmt_naming_or_dropping_the_pcr_reroutes_private_pes() {
	let streams: &[(u8, u16, &[u8])] = &[(0x02, VIDEO_PID, &[]), (0x06, DATA_PID, KLVA)];
	let mut feed = Feed::with_pcr(0x06, KLVA, NO_PCR);
	let start = 1_000 * MS;
	feed.picture(start).data(klv(0, 8));
	feed.mux.pmt_with(PMT_PID, 1, CLOCK_PID, streams, 0);
	feed.pcr(start + 40 * MS)
		.picture(start + 40 * MS)
		.data(klv(1, 8))
		.pcr(start + 80 * MS);
	let head = feed.bytes();
	feed.mux.pmt_with(PMT_PID, 1, NO_PCR, streams, 0);
	feed.picture(start + 80 * MS).data(klv(2, 8));

	let mut imported = start_import(&head);
	let (name, _) = imported
		.entry()
		.expect("the stream is carried once the PMT names a PCR");
	let reader = {
		let consumer = imported.consumer.clone();
		tokio::spawn(async move { read(&consumer, &name).await })
	};
	for _ in 0..10 {
		tokio::task::yield_now().await;
	}
	imported.import.decode(&feed.bytes()[head.len()..]).unwrap();
	imported.import.finish().unwrap();

	let (frames, err) = reader.await.unwrap();
	assert_eq!(frames.len(), 1, "the PES placed while the PCR was named");
	assert_eq!(&frames[0].payload[..], &feed.pes[1].1[..]);
	assert!(err.is_some(), "the track ends with an error once the PCR goes");
	assert!(imported.entry().is_none(), "the track left the catalog");
	assert_eq!(imported.stats().refused, 2);
}

/// The `KLVA` registration with an `ISO_639_language_descriptor` for English after it.
const KLVA_ENG: &[u8] = &[0x05, 0x04, b'K', b'L', b'V', b'A', 0x0a, 0x04, b'e', b'n', b'g', 0x00];

/// The descriptors a catalog entry records, as `(tag, data)`.
fn recorded(track: &super::catalog::Track) -> Vec<(u8, Vec<u8>)> {
	track.descriptors.iter().map(|d| (d.tag, d.data.to_vec())).collect()
}

/// A raw descriptor loop as `(tag, data)`.
fn descriptor_loop(mut raw: &[u8]) -> Vec<(u8, Vec<u8>)> {
	let mut out = Vec::new();
	while let [tag, len, rest @ ..] = raw {
		let (data, tail) = rest.split_at(usize::from(*len));
		out.push((*tag, data.to_vec()));
		raw = tail;
	}
	out
}

/// Start reading track `name` before the input that ends it is imported.
async fn reader(
	imported: &Imported,
	name: &str,
) -> tokio::task::JoinHandle<(Vec<crate::container::Frame>, Result<bool, String>)> {
	let consumer = imported.consumer.clone();
	let name = name.to_string();
	let reader = tokio::spawn(async move { read_until(&consumer, &name).await });
	for _ in 0..10 {
		tokio::task::yield_now().await;
	}
	reader
}

/// A PMT revision that retypes the private stream, from 0x06 under `KLVA` to 0x15 under the
/// metadata descriptor, or changes its descriptors alone, finishes the old track and carries
/// what follows under a new name whose entry records the revision. The PES still waiting for a
/// PCR when the revision lands goes out on the old track, placed from the PCRs before it.
#[tokio::test(start_paused = true)]
async fn a_revised_private_stream_publishes_under_a_fresh_name() {
	for (stream_type, revised) in [(0x15, METADATA), (0x06, KLVA_ENG)] {
		let mut feed = Feed::new(0x06, KLVA);
		let start = 1_000 * MS;
		feed.pcr(start)
			.picture(start)
			.data(klv(1, 8))
			.pcr(start + 40 * MS)
			.data(klv(2, 8));
		let held = placed(feed.clock(feed.pes[1].0).unwrap(), 0);
		let head = feed.bytes();
		let streams: &[(u8, u16, &[u8])] = &[(0x02, VIDEO_PID, &[]), (stream_type, DATA_PID, revised)];
		feed.mux.pmt_with(PMT_PID, 1, CLOCK_PID, streams, 0);
		let at = start + 80 * MS;
		feed.pcr(at)
			.picture(at)
			.data(sync_klv(3, at / 300 + LEAD))
			.pcr(at + 40 * MS);

		let mut imported = start_import(&head);
		let (old, _) = imported.entry().unwrap();
		let old_reader = reader(&imported, &old).await;
		imported.import.decode(&feed.bytes()[head.len()..]).unwrap();

		let (frames, end) = old_reader.await.unwrap();
		assert_eq!(end, Ok(true), "the old track finishes");
		assert_eq!(frames.len(), 2);
		assert_eq!(frames[1].timestamp.as_micros() as u64, held);
		let tracks = imported.catalog.snapshot().ext.mpegts.tracks;
		let listed: Vec<_> = tracks.iter().filter(|(_, track)| track.pid == DATA_PID).collect();
		assert_eq!(listed.len(), 1, "one entry for the PID: {tracks:?}");
		let (name, entry) = listed[0];
		assert_ne!(*name, old, "the revised stream is carried under a new name");
		assert_eq!(entry.pes_packet.as_ref().unwrap().stream_type, stream_type);
		assert_eq!(recorded(entry), descriptor_loop(revised));

		let name = name.clone();
		imported.import.finish().unwrap();
		let (frames, err) = read(&imported.consumer, &name).await;
		assert!(err.is_none(), "{err:?}");
		assert_eq!(frames.len(), 1);
		assert_eq!(&frames[0].payload[..], &feed.pes[2].1[..]);
	}
}

/// A PMT that stops listing the private stream finishes its track and takes it out of the
/// catalog, its PID no longer counted as quiet; a later PMT listing the PID again opens a new
/// track rather than reviving the retired name.
#[tokio::test(start_paused = true)]
async fn a_dropped_private_stream_finishes_and_its_reused_pid_opens_a_new_track() {
	let mut feed = Feed::new(0x06, KLVA);
	let start = 1_000 * MS;
	feed.pcr(start).picture(start).data(klv(1, 8)).pcr(start + 40 * MS);
	let head = feed.bytes();
	feed.mux.pmt_with(PMT_PID, 1, CLOCK_PID, &[(0x02, VIDEO_PID, &[])], 0);
	feed.picture(start + 40 * MS).data(klv(2, 8)).pcr(start + 80 * MS);
	let dropped = feed.bytes();
	let streams: &[(u8, u16, &[u8])] = &[(0x02, VIDEO_PID, &[]), (0x06, DATA_PID, KLVA)];
	feed.mux.pmt_with(PMT_PID, 1, CLOCK_PID, streams, 0);
	feed.picture(start + 80 * MS).data(klv(3, 8)).pcr(start + 120 * MS);

	let mut imported = start_import(&head);
	let (old, _) = imported.entry().unwrap();
	let old_reader = reader(&imported, &old).await;
	imported.import.decode(&dropped[head.len()..]).unwrap();
	let (frames, end) = old_reader.await.unwrap();
	assert_eq!(end, Ok(true), "the dropped stream's track finishes");
	assert_eq!(frames.len(), 1, "nothing after the drop is carried");
	assert!(imported.entry().is_none(), "the track left the catalog");
	assert_eq!(imported.stats().units, 1);
	assert_eq!(imported.stats().quiet, None, "a PID the PMT dropped is not silent");

	imported.import.decode(&feed.bytes()[dropped.len()..]).unwrap();
	imported.import.finish().unwrap();
	let (name, _) = imported.entry().expect("the PID is carried again");
	assert_ne!(name, old, "a reused PID gets a new name");
	let (frames, err) = read(&imported.consumer, &name).await;
	assert!(err.is_none(), "{err:?}");
	assert_eq!(frames.len(), 1);
	assert_eq!(&frames[0].payload[..], &feed.pes[2].1[..]);
}

/// A PMT dropping a private stream before any media timestamp anchors the clock refuses the
/// import, as end of input does, rather than drop the PES it holds; aborting the import then
/// ends the stream's track with its cause.
#[tokio::test(start_paused = true)]
async fn a_private_stream_dropped_before_the_media_clock_refuses_the_import() {
	let mut feed = Feed::new(0x06, KLVA);
	let start = 1_000 * MS;
	feed.pcr(start).data(klv(1, 8)).pcr(start + 40 * MS);
	let head = feed.bytes();
	feed.mux.pmt_with(PMT_PID, 1, CLOCK_PID, &[(0x02, VIDEO_PID, &[])], 0);

	let mut imported = start_import(&head);
	let (name, _) = imported.entry().unwrap();
	let held = reader(&imported, &name).await;
	let err = imported
		.import
		.decode(&feed.bytes()[head.len()..])
		.expect_err("the held PES has no media timestamp to be placed by");
	assert!(err.to_string().contains("without a media timestamp"), "{err:#}");
	imported.import.abort(moq_net::Error::Unsupported);
	let (_, end) = held.await.unwrap();
	assert!(end.as_ref().is_err_and(|err| err.contains("unsupported")), "{end:?}");
}

/// A PMT dropping a private stream past a lone PCR ends it as end of input would: the PES no
/// PCR interval places is counted unplaced, and its track finishes.
#[tokio::test(start_paused = true)]
async fn a_private_stream_dropped_past_a_lone_pcr_counts_its_pes_unplaced() {
	let mut feed = Feed::new(0x06, KLVA);
	feed.pcr(1_000 * MS).picture(1_000 * MS).data(klv(0, 8));
	let head = feed.bytes();
	feed.mux.pmt_with(PMT_PID, 1, CLOCK_PID, &[(0x02, VIDEO_PID, &[])], 0);

	let mut imported = start_import(&head);
	let (name, _) = imported.entry().unwrap();
	let held = reader(&imported, &name).await;
	imported.import.decode(&feed.bytes()[head.len()..]).unwrap();
	let (frames, end) = held.await.unwrap();
	assert!(frames.is_empty());
	assert!(end.is_ok(), "{end:?}");
	assert_eq!(imported.stats().unplaced, 1);
}

/// Revisions apply by what they say, across the 5-bit `version_number` wrap: versions 30, 31,
/// then 0 each relabel the stream and open their own track, while a repetition of any of them
/// keeps the track it opened.
#[test]
fn pmt_revisions_apply_across_the_version_wrap() {
	let mut feed = Feed::new(0x06, KLVA);
	let mut imported = start(false);
	let mut names = Vec::new();
	let mut decoded = 0;
	let revisions: [(u8, u8, &[u8]); 4] = [
		(0, 0x06, KLVA),
		(30, 0x06, KLVA_ENG),
		(31, 0x15, METADATA),
		(0, 0x06, KLVA),
	];
	for (i, (version, stream_type, descriptors)) in revisions.into_iter().enumerate() {
		let streams: &[(u8, u16, &[u8])] = &[(0x02, VIDEO_PID, &[]), (stream_type, DATA_PID, descriptors)];
		if i > 0 {
			feed.mux.pmt_at(version, PMT_PID, 1, CLOCK_PID, streams, 0);
		}
		let bytes = feed.bytes();
		imported.import.decode(&bytes[decoded..]).unwrap();
		let (name, entry) = imported.entry().unwrap();
		assert_eq!(
			entry.pes_packet.as_ref().unwrap().stream_type,
			stream_type,
			"version {version}"
		);
		assert_eq!(recorded(&entry), descriptor_loop(descriptors), "version {version}");

		feed.mux.pmt_at(version, PMT_PID, 1, CLOCK_PID, streams, 0);
		let repeated = feed.bytes();
		imported.import.decode(&repeated[bytes.len()..]).unwrap();
		decoded = repeated.len();
		assert_eq!(imported.entry().unwrap().0, name, "a repetition of version {version}");
		names.push(name);
	}
	let distinct: std::collections::HashSet<_> = names.iter().collect();
	assert_eq!(distinct.len(), revisions.len(), "{names:?}");
}
