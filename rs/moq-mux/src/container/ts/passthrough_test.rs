//! Tests for [`Passthrough`](super::Passthrough).
//!
//! The Kyrion captures are a hardware encoder's broadcast output: a dedicated PCR PID (33) beside
//! the PMT (32) and video (256), so they exercise clock discovery on real bytes. Everything the
//! captures don't carry (scrambling, several programs, a wrap, a flagged jump, oversized groups)
//! is built by [`Mux`].

use std::collections::HashMap;
use std::time::Duration;

use bytes::Bytes;

use super::passthrough::{
	GROUP_BYTES_MAX, GROUP_OBJECTS_MAX, PacketSizeError, Passthrough, PcrGapError, PcrRestartError, PcrRewindError,
};
use super::psi;

/// A delay budget no test timeline comes close to, so the reader sees every group.
const RECORDING_MAX_DELAY: Duration = Duration::from_secs(30);

const KYRION: &[u8] = include_bytes!("test_data/kyrion_mpeg2av_ac3.ts");
const KYRION_H264: &[u8] = include_bytes!("test_data/scte35/kyrion_dirtystart.ts");

/// One object as a subscriber reads it.
#[derive(Clone, Debug, PartialEq)]
struct Object {
	group: u64,
	micros: u64,
	payload: Bytes,
}

struct Imported {
	catalog: hang::catalog::Catalog,
	objects: Vec<Object>,
}

impl Imported {
	/// Every object's packets, concatenated in order.
	fn bytes(&self) -> Vec<u8> {
		self.objects
			.iter()
			.flat_map(|object| object.payload.iter().copied())
			.collect()
	}

	fn section(&self) -> &hang::catalog::M2ts {
		self.catalog.m2ts.as_ref().expect("an m2ts section")
	}

	/// Each group's objects.
	fn groups(&self) -> Vec<Vec<&Object>> {
		let mut groups: Vec<Vec<&Object>> = Vec::new();
		for object in &self.objects {
			match groups.last_mut() {
				Some(group) if group[0].group == object.group => group.push(object),
				_ => groups.push(vec![object]),
			}
		}
		groups
	}
}

/// Import `data` in chunks of `chunk` bytes and read back every object.
async fn import(data: &[u8], chunk: usize, pcr_pid: Option<u16>) -> anyhow::Result<Imported> {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default())?;
	let mut passthrough = Passthrough::new(broadcast.clone(), catalog.reserve())?;
	if let Some(pid) = pcr_pid {
		passthrough = passthrough.with_pcr_pid(pid);
	}
	let name = passthrough.track().to_string();
	for chunk in data.chunks(chunk) {
		passthrough.decode(chunk)?;
	}
	passthrough.finish()?;

	let mut track = consumer
		.track(&name)?
		.subscribe(moq_net::track::Subscription::default().with_max_delay(RECORDING_MAX_DELAY))
		.await?;
	let mut objects = Vec::new();
	while let Some(mut group) = tokio::time::timeout(Duration::from_secs(1), track.recv_group()).await?? {
		while let Some(frame) = group.read_frame().await? {
			let frame = hang::container::Frame::decode(frame.payload)?;
			objects.push(Object {
				group: group.sequence,
				micros: frame.timestamp.as_micros() as u64,
				payload: frame.payload,
			});
		}
	}
	Ok(Imported {
		catalog: catalog.snapshot(),
		objects,
	})
}

/// What a packet's header and adaptation field say.
#[derive(Debug, Default)]
struct Header {
	pid: u16,
	pcr: Option<u64>,
	random_access: bool,
	discontinuity: bool,
}

fn header(pkt: &[u8]) -> Header {
	let pid = u16::from_be_bytes([pkt[1] & 0x1f, pkt[2]]);
	let mut header = Header {
		pid,
		..Default::default()
	};
	if pkt[3] & 0x20 != 0 && pkt[4] > 0 {
		let flags = pkt[5];
		header.discontinuity = flags & 0x80 != 0;
		header.random_access = flags & 0x40 != 0;
		if flags & 0x10 != 0 && pkt[4] >= 7 {
			let base = (u64::from(pkt[6]) << 25)
				| (u64::from(pkt[7]) << 17)
				| (u64::from(pkt[8]) << 9)
				| (u64::from(pkt[9]) << 1)
				| (u64::from(pkt[10]) >> 7);
			header.pcr = Some(base * 300 + ((u64::from(pkt[10] & 1) << 8) | u64::from(pkt[11])));
		}
	}
	header
}

/// Builds a transport stream packet by packet.
#[derive(Default)]
struct Mux {
	out: Vec<u8>,
	cc: HashMap<u16, u8>,
}

#[derive(Clone, Copy, Default)]
struct Flags {
	pcr: Option<u64>,
	random_access: bool,
	discontinuity: bool,
}

impl Flags {
	fn pcr(ticks: u64) -> Self {
		Self {
			pcr: Some(ticks),
			..Default::default()
		}
	}

	fn random_access(mut self) -> Self {
		self.random_access = true;
		self
	}

	fn discontinuity(mut self) -> Self {
		self.discontinuity = true;
		self
	}
}

const ACCESS: Flags = Flags {
	pcr: None,
	random_access: true,
	discontinuity: false,
};

impl Mux {
	fn cc(&mut self, pid: u16) -> u8 {
		let cc = self.cc.entry(pid).or_default();
		let out = *cc;
		*cc = (*cc + 1) & 0x0f;
		out
	}

	/// One packet on `pid`, its payload filled with the PID's low byte.
	fn packet(&mut self, pid: u16, flags: Flags) -> &mut Self {
		let cc = self.cc(pid);
		let mut pkt = vec![0x47, (pid >> 8) as u8 & 0x1f, pid as u8];
		let marked = flags.pcr.is_some() || flags.random_access || flags.discontinuity;
		if marked {
			pkt.push(0x30 | cc);
			let mut field = vec![
				u8::from(flags.discontinuity) << 7
					| u8::from(flags.random_access) << 6
					| u8::from(flags.pcr.is_some()) << 4,
			];
			if let Some(pcr) = flags.pcr {
				let (base, ext) = (pcr / 300, pcr % 300);
				field.extend_from_slice(&[
					(base >> 25) as u8,
					(base >> 17) as u8,
					(base >> 9) as u8,
					(base >> 1) as u8,
					((base & 1) << 7) as u8 | 0x7e | (ext >> 8) as u8,
					ext as u8,
				]);
			}
			pkt.push(field.len() as u8);
			pkt.extend_from_slice(&field);
		} else {
			pkt.push(0x10 | cc);
		}
		pkt.resize(188, pid as u8);
		self.out.extend_from_slice(&pkt);
		self
	}

	/// `n` plain packets on `pid`.
	fn fill(&mut self, pid: u16, n: usize) -> &mut Self {
		for _ in 0..n {
			self.packet(pid, Flags::default());
		}
		self
	}

	/// A section on `pid`, split across as many packets as it needs.
	fn section(&mut self, pid: u16, section: &[u8]) -> &mut Self {
		let mut payload = vec![0u8];
		payload.extend_from_slice(section);
		for (i, chunk) in payload.chunks(184).enumerate() {
			let cc = self.cc(pid);
			let pusi = if i == 0 { 0x40 } else { 0 };
			let mut pkt = vec![0x47, pusi | (pid >> 8) as u8 & 0x1f, pid as u8, 0x10 | cc];
			pkt.extend_from_slice(chunk);
			pkt.resize(188, 0xff);
			self.out.extend_from_slice(&pkt);
		}
		self
	}

	fn pat(&mut self, programs: &[(u16, u16)]) -> &mut Self {
		let body: Vec<u8> = programs
			.iter()
			.flat_map(|&(number, pmt)| [(number >> 8) as u8, number as u8, 0xe0 | (pmt >> 8) as u8, pmt as u8])
			.collect();
		self.section(0, &psi::section(psi::PAT_TABLE_ID, 1, 0, 0, 0, &body))
	}

	/// A PMT whose `program_info` holds `info` bytes of private descriptors, so a large one
	/// spans packets.
	fn pmt(&mut self, pid: u16, program: u16, pcr_pid: u16, streams: &[(u8, u16)], info: usize) -> &mut Self {
		let mut descriptors = Vec::new();
		while descriptors.len() < info {
			let len = (info - descriptors.len()).clamp(2, 200) - 2;
			descriptors.push(0x80);
			descriptors.push(len as u8);
			descriptors.extend(std::iter::repeat_n(0xaa, len));
		}
		let mut body = vec![
			0xe0 | (pcr_pid >> 8) as u8,
			pcr_pid as u8,
			0xf0 | (descriptors.len() >> 8) as u8,
			descriptors.len() as u8,
		];
		body.extend_from_slice(&descriptors);
		for &(stream_type, es) in streams {
			body.extend_from_slice(&[stream_type, 0xe0 | (es >> 8) as u8, es as u8, 0xf0, 0x00]);
		}
		self.section(pid, &psi::section(psi::PMT_TABLE_ID, program, 0, 0, 0, &body))
	}

	fn bytes(&self) -> Vec<u8> {
		self.out.clone()
	}
}

/// 27 MHz ticks per millisecond.
const MS: u64 = 27_000;

/// A single program: PMT on 0x20, H.264 video on 0x100, audio on 0x101, the clock on `pcr_pid`.
fn program(mux: &mut Mux, pcr_pid: u16) {
	mux.pat(&[(1, 0x20)])
		.pmt(0x20, 1, pcr_pid, &[(0x1b, 0x100), (0x0f, 0x101)], 0);
}

/// `seconds` of a stream paced 40 ms per PCR on `pcr_pid`, starting at `start` ticks, with a
/// video random access point every `gop` PCRs (none when zero). Each PCR interval is the PCR
/// packet then `fill` packets alternating video and audio.
fn paced(mux: &mut Mux, pcr_pid: u16, start: u64, intervals: u64, gop: u64, fill: usize) {
	for i in 0..intervals {
		mux.packet(pcr_pid, Flags::pcr(start + i * 40 * MS));
		if gop != 0 && i % gop == 0 {
			mux.packet(0x100, ACCESS);
		}
		for j in 0..fill {
			mux.fill(if j % 2 == 0 { 0x100 } else { 0x101 }, 1);
		}
	}
}

/// Where in `data` the packet starting the first object sits, checking every packet from there on
/// arrived verbatim.
fn assert_suffix(data: &[u8], imported: &Imported) -> usize {
	let bytes = imported.bytes();
	assert!(!bytes.is_empty(), "nothing was published");
	assert_eq!(bytes.len() % 188, 0, "objects are whole packets");
	let start = data.len() - bytes.len();
	assert_eq!(start % 188, 0, "the first object starts on the packet grid");
	assert!(
		data[start..] == bytes[..],
		"the output is the input from the first group on"
	);
	start
}

#[tokio::test(start_paused = true)]
async fn a_broadcast_capture_is_byte_identical_from_the_first_group() {
	for data in [KYRION, KYRION_H264] {
		let imported = import(data, data.len(), None).await.unwrap();
		let start = assert_suffix(data, &imported);

		// Groups start at the video PID's random access points, not on the dedicated clock PID.
		let groups = imported.groups();
		assert!(groups.len() >= 2, "{} groups", groups.len());
		let mut at = start;
		for group in &groups {
			let first = header(&group[0].payload);
			assert_eq!(first.pid, 256, "a group starts on the video PID");
			assert!(first.random_access, "a group starts at a random access point");
			for object in &group[1..] {
				let first = header(&object.payload);
				assert_eq!(first.pid, 33, "an object inside a group starts at a PCR packet");
				assert!(first.pcr.is_some());
			}
			at += group.iter().map(|object| object.payload.len()).sum::<usize>();
		}
		assert_eq!(at, data.len());
		assert!(
			imported.section().random_access,
			"every group starts at a random access point"
		);
		assert!(imported.section().track.ends_with(".m2ts"));
		assert!(imported.catalog.video.renditions.is_empty(), "nothing is demultiplexed");

		// The bytes before the first group are the ones dropped, and the first group is the first
		// random access point once a PCR has been seen and a PMT has followed a PAT. Both captures
		// open on a PMT the importer cannot place yet, so it waits for the next.
		let packets: Vec<(usize, Header, bool)> = (0..data.len() / 188)
			.map(|i| (i, header(&data[i * 188..]), data[i * 188 + 1] & 0x40 != 0))
			.collect();
		let pat = packets.iter().find(|(_, h, pusi)| h.pid == 0 && *pusi).unwrap().0;
		let pmt = packets[pat..]
			.iter()
			.find(|(_, h, pusi)| h.pid == 32 && *pusi)
			.unwrap()
			.0;
		let pcr = packets
			.iter()
			.find(|(_, h, _)| h.pid == 33 && h.pcr.is_some())
			.unwrap()
			.0;
		let first_rai = packets[pmt.max(pcr)..]
			.iter()
			.find(|(_, h, _)| h.pid == 256 && h.random_access)
			.unwrap()
			.0;
		assert_eq!(start, first_rai * 188);
	}
}

#[tokio::test(start_paused = true)]
async fn an_object_at_a_pcr_is_stamped_just_before_it() {
	let imported = import(KYRION, KYRION.len(), None).await.unwrap();
	let mut previous = 0;
	for object in &imported.objects {
		assert!(object.micros >= previous, "timestamps rise through one timebase");
		previous = object.micros;
		if let Some(pcr) = header(&object.payload).pcr {
			let pcr = pcr / 27;
			// Ten bytes before the PCR's own byte: microseconds at broadcast rates.
			assert!(
				object.micros <= pcr && pcr - object.micros < 100,
				"{} vs {pcr}",
				object.micros
			);
		}
	}
}

#[tokio::test(start_paused = true)]
async fn objects_do_not_depend_on_how_the_input_is_chunked() {
	let whole = import(KYRION_H264, KYRION_H264.len(), None).await.unwrap();
	for chunk in [7, 188, 1316, 65_536] {
		let chunked = import(KYRION_H264, chunk, None).await.unwrap();
		assert_eq!(chunked.objects, whole.objects, "chunks of {chunk}");
		assert_eq!(chunked.catalog.m2ts, whole.catalog.m2ts, "chunks of {chunk}");
	}
}

/// Junk as long as a 204-byte packet, a lone sync byte in it, then a clean stream: the grid is
/// found behind it whether the input arrives at once or in pieces.
#[tokio::test(start_paused = true)]
async fn leading_junk_does_not_hide_the_grid() {
	let mut data: Vec<u8> = (0..204).map(|i| if i == 60 { 0x47 } else { 0 }).collect();
	data.extend_from_slice(KYRION_H264);
	let clean = import(KYRION_H264, KYRION_H264.len(), None).await.unwrap();
	for chunk in [7, 188, 1316, 65_536, data.len()] {
		let imported = import(&data, chunk, None).await.unwrap();
		assert_eq!(imported.objects, clean.objects, "chunks of {chunk}");
		assert_eq!(imported.catalog.m2ts, clean.catalog.m2ts, "chunks of {chunk}");
	}
}

/// Three packets, too few for a full run of sync bytes, still settle the grid at the end of the
/// input and publish.
#[tokio::test(start_paused = true)]
async fn a_stream_shorter_than_a_sync_run_still_publishes() {
	let mut mux = Mux::default();
	program(&mut mux, 0x100);
	mux.packet(0x100, Flags::pcr(0).random_access());
	let data = mux.bytes();
	for chunk in [7, data.len()] {
		let imported = import(&data, chunk, None).await.unwrap();
		assert_eq!(assert_suffix(&data, &imported), 2 * 188, "chunks of {chunk}");
	}
}

/// TS scrambling leaves the header and adaptation field in clear, which is all grouping reads.
#[tokio::test(start_paused = true)]
async fn a_scrambled_twin_groups_and_paces_the_same() {
	let mut scrambled = KYRION.to_vec();
	for pkt in scrambled.chunks_mut(188) {
		let pid = u16::from_be_bytes([pkt[1] & 0x1f, pkt[2]]);
		if ![256, 257, 258].contains(&pid) || pkt[3] & 0x10 == 0 {
			continue;
		}
		pkt[3] |= 0x80;
		let start = if pkt[3] & 0x20 != 0 { 5 + usize::from(pkt[4]) } else { 4 };
		for byte in &mut pkt[start.min(188)..] {
			*byte ^= 0x5a;
		}
	}
	let clear = import(KYRION, KYRION.len(), None).await.unwrap();
	let twin = import(&scrambled, scrambled.len(), None).await.unwrap();
	assert_suffix(&scrambled, &twin);

	let shape = |imported: &Imported| {
		imported
			.objects
			.iter()
			.map(|object| (object.group, object.micros, object.payload.len()))
			.collect::<Vec<_>>()
	};
	assert_eq!(shape(&twin), shape(&clear));
	assert_eq!(twin.catalog.m2ts, clear.catalog.m2ts);
}

#[tokio::test(start_paused = true)]
async fn two_programs_on_one_clock_publish_no_random_access() {
	let mut mux = Mux::default();
	mux.pat(&[(1, 0x20), (2, 0x30)])
		.pmt(0x20, 1, 0x100, &[(0x1b, 0x100)], 0)
		.pmt(0x30, 2, 0x100, &[(0x1b, 0x200)], 0);
	for i in 0..100 {
		mux.packet(0x100, Flags::pcr(i * 40 * MS));
		// Staggered GOPs: program 2's random access points fall between program 1's.
		if i % 10 == 0 {
			mux.packet(0x100, ACCESS);
		}
		if i % 10 == 5 {
			mux.packet(0x200, ACCESS);
		}
		mux.fill(0x200, 3);
	}
	let data = mux.bytes();
	let imported = import(&data, data.len(), None).await.unwrap();
	assert!(!imported.section().random_access);
	for group in imported.groups() {
		let first = header(&group[0].payload);
		assert_eq!(
			(first.pid, first.random_access),
			(0x100, true),
			"groups follow program 1's video"
		);
	}
}

#[tokio::test(start_paused = true)]
async fn two_video_streams_in_one_program_publish_no_random_access() {
	let mut mux = Mux::default();
	mux.pat(&[(1, 0x20)])
		.pmt(0x20, 1, 0x100, &[(0x1b, 0x100), (0x24, 0x102)], 0);
	for i in 0..100 {
		mux.packet(0x100, Flags::pcr(i * 40 * MS));
		if i % 10 == 0 {
			mux.packet(0x100, ACCESS);
		}
		if i % 10 == 5 {
			mux.packet(0x102, ACCESS);
		}
		mux.fill(0x102, 3);
	}
	let data = mux.bytes();
	let imported = import(&data, data.len(), None).await.unwrap();
	assert!(!imported.section().random_access);
	assert_eq!(imported.groups().len(), 10);
}

#[tokio::test(start_paused = true)]
async fn a_clock_on_the_audio_pid_still_groups_at_video_random_access() {
	let mut mux = Mux::default();
	program(&mut mux, 0x101);
	for i in 0..100 {
		// Audio marks every frame random access; only video's mark starts a group.
		mux.packet(0x101, Flags::pcr(i * 40 * MS).random_access());
		if i % 12 == 0 {
			mux.packet(0x100, ACCESS);
		}
		mux.fill(0x100, 4);
	}
	let data = mux.bytes();
	let imported = import(&data, data.len(), None).await.unwrap();
	assert!(imported.section().random_access);
	let groups = imported.groups();
	assert_eq!(groups.len(), 9);
	for group in &groups {
		let first = header(&group[0].payload);
		assert_eq!((first.pid, first.random_access), (0x100, true));
		for object in &group[1..] {
			assert_eq!(header(&object.payload).pid, 0x101, "objects start at the clock's PCRs");
		}
	}
}

#[tokio::test(start_paused = true)]
async fn the_clock_is_found_behind_a_pmt_split_across_packets() {
	let mut mux = Mux::default();
	mux.pat(&[(1, 0x20)])
		.pmt(0x20, 1, 0x1ff, &[(0x02, 0x100), (0x81, 0x101)], 400);
	assert!(mux.out.len() >= 188 * 4, "the PMT spans packets");
	paced(&mut mux, 0x1ff, 0, 50, 10, 6);
	let data = mux.bytes();
	let imported = import(&data, data.len(), None).await.unwrap();
	assert!(imported.section().random_access);
	assert_eq!(imported.groups().len(), 5);
	assert_eq!(header(&imported.objects[1].payload).pid, 0x1ff);
}

#[tokio::test(start_paused = true)]
async fn a_source_without_random_access_groups_every_second() {
	let mut mux = Mux::default();
	program(&mut mux, 0x100);
	paced(&mut mux, 0x100, 0, 200, 0, 4);
	let data = mux.bytes();
	let imported = import(&data, data.len(), None).await.unwrap();
	assert!(!imported.section().random_access);
	let groups = imported.groups();
	// The first group waits a second for a random access point that never comes.
	assert_eq!(groups[0][0].micros, 1_000_000 - micros_before_pcr(&data, 1_000_000));
	for group in &groups {
		assert_eq!(group.len(), 25, "a second of 40 ms objects");
	}
}

/// How far before its PCR an object starting at that PCR's packet is stamped, in microseconds.
fn micros_before_pcr(data: &[u8], pcr_micros: u64) -> u64 {
	let at = (0..data.len() / 188)
		.find(|i| header(&data[i * 188..]).pcr == Some(pcr_micros * 27))
		.unwrap();
	let prev = (0..at).rev().find(|i| header(&data[i * 188..]).pcr.is_some()).unwrap();
	// Ten bytes at the rate of the 40 ms interval ending at this PCR.
	let span = ((at - prev) * 188) as u64;
	(10 * 40 * MS).div_ceil(span).div_ceil(27)
}

#[tokio::test(start_paused = true)]
async fn a_random_access_point_between_pcrs_is_interpolated() {
	let mut mux = Mux::default();
	program(&mut mux, 0x101);
	// One microsecond per byte: 1,880 bytes between PCRs, 1,880 * 27 ticks apart.
	let step = 1_880 * 27;
	for i in 0..4 {
		mux.packet(0x101, Flags::pcr(1_000 * 27 + i * step));
		mux.fill(0x100, 4);
		if i == 1 {
			mux.packet(0x100, ACCESS);
			mux.fill(0x100, 4);
		} else {
			mux.fill(0x100, 5);
		}
	}
	let data = mux.bytes();
	let imported = import(&data, data.len(), None).await.unwrap();
	let first = &imported.objects[0];
	assert!(header(&first.payload).random_access);
	// The random access point is five packets after the second PCR's packet, which is ten bytes
	// before that PCR.
	assert_eq!(first.micros, 1_000 + 1_880 + 5 * 188 - 10);
}

/// Objects after the last PCR are timed at the rate of the interval before it.
#[tokio::test(start_paused = true)]
async fn objects_after_the_last_pcr_extrapolate_its_rate() {
	let mut mux = Mux::default();
	program(&mut mux, 0x101);
	// One microsecond per byte, and no PCR after the random access point to bound it.
	let step = 1_880 * 27;
	for i in 0..4 {
		mux.packet(0x101, Flags::pcr(1_000 * 27 + i * step));
		if i == 3 {
			mux.fill(0x100, 4);
			mux.packet(0x100, ACCESS);
			mux.fill(0x100, 4);
		} else {
			mux.fill(0x100, 9);
		}
	}
	let data = mux.bytes();
	for chunk in [7, 188, data.len()] {
		let imported = import(&data, chunk, None).await.unwrap();
		assert_eq!(
			assert_suffix(&data, &imported),
			data.len() - 5 * 188,
			"chunks of {chunk}"
		);
		assert_eq!(imported.objects.len(), 1, "chunks of {chunk}");
		assert_eq!(
			imported.objects[0].micros,
			1_000 + 3 * 1_880 + 5 * 188 - 10,
			"chunks of {chunk}"
		);
	}
}

/// The section describes the track for as long as the importer lives, however the track ends.
#[tokio::test(start_paused = true)]
async fn the_section_goes_with_the_importer() {
	for abort in [false, true] {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let mut passthrough = Passthrough::new(broadcast.clone(), catalog.reserve()).unwrap();
		passthrough.decode(KYRION_H264).unwrap();
		assert!(catalog.snapshot().m2ts.is_some(), "abort {abort}");
		if abort {
			passthrough.abort(moq_net::Error::Cancel);
		} else {
			passthrough.finish().unwrap();
			assert!(catalog.snapshot().m2ts.is_some(), "a finished track is still described");
			drop(passthrough);
		}
		assert!(catalog.snapshot().m2ts.is_none(), "abort {abort}");
	}
}

#[tokio::test(start_paused = true)]
async fn the_pcr_wrap_continues_the_timeline() {
	const WRAP: u64 = (1 << 33) * 300;
	let mut mux = Mux::default();
	program(&mut mux, 0x100);
	let start = WRAP - 2_000 * MS;
	for i in 0..100 {
		mux.packet(0x100, Flags::pcr((start + i * 40 * MS) % WRAP));
		if i % 10 == 0 {
			mux.packet(0x100, ACCESS);
		}
		mux.fill(0x101, 3);
	}
	let data = mux.bytes();
	let imported = import(&data, data.len(), None).await.unwrap();
	let micros: Vec<u64> = imported.objects.iter().map(|object| object.micros).collect();
	assert!(micros.windows(2).all(|pair| pair[0] < pair[1]), "{micros:?}");
	let last = *micros.last().unwrap();
	assert!(last > WRAP / 27, "unwrapped past 2^33 * 300 ticks: {last}");
	assert!(imported.section().random_access, "a wrap is not a discontinuity");
}

/// A flagged jump forward starts a group behind a break marker, and the count carries on by the
/// jump, so the timeline never steps back.
#[tokio::test(start_paused = true)]
async fn a_flagged_jump_forward_starts_a_group_behind_a_break() {
	let mut mux = Mux::default();
	program(&mut mux, 0x100);
	paced(&mut mux, 0x100, 10_000 * MS, 50, 10, 4);
	// A splice forward to thirty seconds, flagged, with no random access point at the break.
	mux.packet(0x100, Flags::pcr(30_000 * MS).discontinuity());
	mux.fill(0x101, 4);
	paced(&mut mux, 0x100, 30_040 * MS, 50, 10, 4);
	let data = mux.bytes();
	let imported = import(&data, data.len(), None).await.unwrap();
	assert_suffix(&data, &imported);

	let micros: Vec<u64> = imported.objects.iter().map(|object| object.micros).collect();
	assert!(micros.windows(2).all(|pair| pair[0] <= pair[1]), "{micros:?}");
	let groups = imported.groups();
	let jump = groups
		.iter()
		.position(|group| header(&group[0].payload).discontinuity)
		.expect("the break starts a group");
	assert!(jump > 0);
	let before = groups[jump - 1].last().unwrap().micros;
	let after = groups[jump][0].micros;
	assert!((11_959_000..11_960_000).contains(&before), "{before}");
	assert!((29_999_000..30_000_000).contains(&after), "{after}");
	assert_eq!(
		groups[jump][0].group,
		groups[jump - 1][0].group + 2,
		"a skipped sequence marks the break"
	);
	assert!(
		!imported.section().random_access,
		"a group started by a break alone is not a random access point"
	);
}

/// A flagged step back is a restart: new content, which a timeline cannot step back into.
#[tokio::test(start_paused = true)]
async fn a_flagged_step_back_is_a_restart() {
	let mut mux = Mux::default();
	program(&mut mux, 0x100);
	paced(&mut mux, 0x100, 10_000 * MS, 30, 10, 2);
	mux.packet(0x100, Flags::pcr(2_000 * MS).discontinuity());
	let data = mux.bytes();
	let err = match import(&data, data.len(), None).await {
		Ok(_) => panic!("a restart was carried on one timeline"),
		Err(err) => err,
	};
	let restart = err.downcast_ref::<PcrRestartError>().expect("a typed refusal");
	assert_eq!((restart.pid, restart.to), (0x100, 2_000 * MS));
}

/// A break before any group has started is no group start, so the first group still waits for a
/// random access point.
#[tokio::test(start_paused = true)]
async fn a_break_before_the_first_group_does_not_start_it() {
	let mut mux = Mux::default();
	program(&mut mux, 0x100);
	paced(&mut mux, 0x100, 0, 5, 0, 2);
	mux.packet(0x100, Flags::pcr(5_000 * MS).discontinuity());
	mux.fill(0x101, 2);
	paced(&mut mux, 0x100, 5_040 * MS, 30, 10, 2);
	let data = mux.bytes();
	let imported = import(&data, data.len(), None).await.unwrap();
	assert!(header(&imported.objects[0].payload).random_access);
	assert!(imported.section().random_access);
}

#[tokio::test(start_paused = true)]
async fn an_unflagged_rewind_is_refused() {
	let mut mux = Mux::default();
	program(&mut mux, 0x100);
	paced(&mut mux, 0x100, 10_000 * MS, 30, 10, 2);
	mux.packet(0x100, Flags::pcr(9_000 * MS));
	let data = mux.bytes();
	let err = match import(&data, data.len(), None).await {
		Ok(_) => panic!("a rewind was accepted"),
		Err(err) => err,
	};
	let rewind = err.downcast_ref::<PcrRewindError>().expect("a typed refusal");
	assert_eq!((rewind.pid, rewind.to), (0x100, 9_000 * MS));
}

#[tokio::test(start_paused = true)]
async fn a_retransmitted_break_declares_one_jump() {
	let mut mux = Mux::default();
	program(&mut mux, 0x100);
	paced(&mut mux, 0x100, 10_000 * MS, 30, 10, 2);
	let flagged = Flags::pcr(30_000 * MS).discontinuity();
	mux.packet(0x100, flagged);
	mux.out.extend_from_within(mux.out.len() - 188..);
	paced(&mut mux, 0x100, 30_040 * MS, 30, 10, 2);
	let data = mux.bytes();
	let imported = import(&data, data.len(), None).await.unwrap();
	let breaks = imported
		.groups()
		.iter()
		.filter(|group| header(&group[0].payload).discontinuity)
		.count();
	assert_eq!(breaks, 1);
	assert_suffix(&data, &imported);
}

#[tokio::test(start_paused = true)]
async fn a_long_gop_splits_at_the_byte_ceiling_between_objects() {
	let mut mux = Mux::default();
	program(&mut mux, 0x100);
	mux.packet(0x100, Flags::pcr(0));
	mux.packet(0x100, ACCESS);
	// 100 packets per PCR, a microsecond apart: far past the ceiling in well under a second.
	let objects = GROUP_BYTES_MAX as usize * 5 / 2 / (100 * 188);
	for i in 1..=objects as u64 {
		mux.packet(0x100, Flags::pcr(i * 27));
		mux.fill(0x101, 99);
	}
	let data = mux.bytes();
	let imported = import(&data, 1 << 20, None).await.unwrap();
	assert_suffix(&data, &imported);
	let groups = imported.groups();
	assert_eq!(groups.len(), 3);
	for group in &groups {
		let bytes: usize = group.iter().map(|object| object.payload.len()).sum();
		assert!(bytes as u64 <= GROUP_BYTES_MAX, "{bytes}");
	}
	for group in &groups[1..] {
		assert!(header(&group[0].payload).pcr.is_some(), "the split falls on an object");
	}
	assert!(!imported.section().random_access);
}

#[tokio::test(start_paused = true)]
async fn a_clock_in_every_packet_splits_at_the_object_ceiling() {
	let mut mux = Mux::default();
	program(&mut mux, 0x100);
	mux.packet(0x100, Flags::pcr(0).random_access());
	for i in 1..(GROUP_OBJECTS_MAX as u64 * 2 + 10) {
		mux.packet(0x100, Flags::pcr(i * 27));
	}
	let data = mux.bytes();
	let imported = import(&data, data.len(), None).await.unwrap();
	let sizes: Vec<usize> = imported.groups().iter().map(Vec::len).collect();
	assert_eq!(sizes[..2], [GROUP_OBJECTS_MAX, GROUP_OBJECTS_MAX]);
	assert!(!imported.section().random_access);
}

#[tokio::test(start_paused = true)]
async fn a_clock_that_stops_is_refused_once_a_group_would_overflow() {
	let mut mux = Mux::default();
	program(&mut mux, 0x100);
	mux.packet(0x100, Flags::pcr(0));
	mux.packet(0x100, ACCESS);
	mux.fill(0x101, GROUP_BYTES_MAX as usize / 188 + 1);
	let data = mux.bytes();
	let err = match import(&data, 1 << 20, None).await {
		Ok(_) => panic!("an untimed run was accepted"),
		Err(err) => err,
	};
	assert_eq!(err.downcast_ref::<PcrGapError>().map(|gap| gap.pid), Some(0x100));
}

#[tokio::test(start_paused = true)]
async fn nothing_is_published_before_the_pmt_and_a_pcr() {
	let mut mux = Mux::default();
	// A random access point before any PSI, then one before any PCR.
	mux.packet(0x100, ACCESS);
	program(&mut mux, 0x100);
	mux.packet(0x100, ACCESS);
	mux.fill(0x101, 3);
	paced(&mut mux, 0x100, 0, 30, 10, 3);
	let data = mux.bytes();
	let imported = import(&data, data.len(), None).await.unwrap();
	let start = assert_suffix(&data, &imported);
	let first_pcr = (0..data.len() / 188)
		.find(|i| header(&data[i * 188..]).pcr.is_some())
		.unwrap();
	assert!(start > first_pcr * 188);
}

#[tokio::test(start_paused = true)]
async fn a_192_or_204_byte_source_is_refused() {
	for size in [192, 204] {
		let data: Vec<u8> = KYRION[..188 * 40]
			.chunks(188)
			.flat_map(|pkt| {
				let mut wide = if size == 192 { vec![0, 1, 2, 3] } else { Vec::new() };
				wide.extend_from_slice(pkt);
				wide.resize(size, 0);
				wide
			})
			.collect();
		let err = match import(&data, data.len(), None).await {
			Ok(_) => panic!("{size}-byte packets were accepted"),
			Err(err) => err,
		};
		assert_eq!(err.downcast_ref::<PacketSizeError>(), Some(&PacketSizeError { size }));
	}
}

#[tokio::test(start_paused = true)]
async fn a_cbr_source_records_its_mux_rate() {
	let data = include_bytes!("test_data/bbb_cbr.ts");
	let imported = import(data, data.len(), None).await.unwrap();
	let rate = imported.section().mux_rate.expect("a CBR source records its mux rate");
	assert!(rate.abs_diff(400_000) * 1000 <= 400_000, "{rate}");
	assert_suffix(data, &imported);
}

#[tokio::test(start_paused = true)]
async fn an_overriding_clock_paces_on_its_own_pcrs() {
	let mut mux = Mux::default();
	// The PMT names the audio PID's clock; the override paces on a PID no PMT names.
	program(&mut mux, 0x101);
	for i in 0..60 {
		mux.packet(0x101, Flags::pcr(i * 40 * MS));
		mux.fill(0x100, 2);
		mux.packet(0x1ee, Flags::pcr(i * 40 * MS + 20 * MS));
		if i % 10 == 0 {
			mux.packet(0x1ee, ACCESS);
		}
		mux.fill(0x100, 2);
	}
	let data = mux.bytes();
	let imported = import(&data, data.len(), Some(0x1ee)).await.unwrap();
	for group in imported.groups() {
		assert_eq!(header(&group[0].payload).pid, 0x1ee, "its own random access points");
		for object in &group[1..] {
			assert_eq!(header(&object.payload).pid, 0x1ee, "objects start at its PCRs");
		}
	}
	assert_eq!(imported.groups().len(), 6);
	assert!(
		!imported.section().random_access,
		"a clock no program names marks no program's decoding boundary"
	);
}
