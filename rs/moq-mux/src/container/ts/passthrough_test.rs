//! Tests for [`Passthrough`](super::Passthrough).
//!
//! The Kyrion captures are a hardware encoder's broadcast output: a dedicated PCR PID (33) beside
//! the PMT (32) and video (256), so they exercise clock discovery on real bytes. Everything the
//! captures don't carry (scrambling, several programs, a wrap, a flagged jump, oversized groups)
//! is built by [`Mux`].

use std::time::Duration;

use bytes::Bytes;

use super::fixture::{ACCESS, Flags, MS, Mux, WRAP, paced, program};
use super::import::test::{WRAP_MICROS, epoch_clock, epoch_time};
use super::passthrough::{
	GROUP_BYTES_MAX, GROUP_OBJECTS_MAX, PacketSizeError, Passthrough, PcrGapError, PcrRestartError, PcrRewindError,
};

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
	/// The sequence of each break marker: a group of one object with no bytes.
	markers: Vec<u64>,
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
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default())?;
	import_onto(broadcast, catalog, data, chunk, pcr_pid).await
}

/// [`import`] onto an existing `catalog` of `broadcast`.
async fn import_onto(
	broadcast: moq_net::broadcast::Producer,
	catalog: crate::catalog::Producer,
	data: &[u8],
	chunk: usize,
	pcr_pid: Option<u16>,
) -> anyhow::Result<Imported> {
	let timebase = catalog.timebase();
	import_on(broadcast, catalog, timebase, data, chunk, pcr_pid).await
}

/// [`import_onto`] on `timebase`, one of `catalog`'s.
async fn import_on(
	broadcast: moq_net::broadcast::Producer,
	catalog: crate::catalog::Producer,
	timebase: crate::catalog::Timebase,
	data: &[u8],
	chunk: usize,
	pcr_pid: Option<u16>,
) -> anyhow::Result<Imported> {
	let consumer = broadcast.consume();
	let mut passthrough = Passthrough::new(broadcast.clone(), timebase.reserve())?;
	if let Some(pid) = pcr_pid {
		passthrough = passthrough.with_pcr_pid(pid);
	}
	let name = passthrough.track().to_string();
	for chunk in data.chunks(chunk) {
		passthrough.decode(chunk)?;
	}
	passthrough.finish()?;
	read_back(&consumer, &name, &catalog).await
}

/// Every object of the finished track `name`, and the catalog as it stands.
async fn read_back(
	consumer: &moq_net::broadcast::Consumer,
	name: &str,
	catalog: &crate::catalog::Producer,
) -> anyhow::Result<Imported> {
	let mut track = consumer
		.track(name)?
		.subscribe(moq_net::track::Subscription::default().with_max_delay(RECORDING_MAX_DELAY))
		.await?;
	let mut objects = Vec::new();
	let mut markers = Vec::new();
	while let Some(mut group) = tokio::time::timeout(Duration::from_secs(1), track.recv_group()).await?? {
		while let Some(frame) = group.read_frame().await? {
			if frame.payload.is_empty() {
				markers.push(group.sequence);
				continue;
			}
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
		markers,
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

/// The window a second program was on air in publishes no random access, and the first window
/// carried whole on one program wins it back.
#[tokio::test(start_paused = true)]
async fn random_access_returns_once_a_window_carries_one_program() {
	let mut mux = Mux::default();
	mux.pat(&[(1, 0x20), (2, 0x30)])
		.pmt(0x20, 1, 0x100, &[(0x1b, 0x100)], 0)
		.pmt(0x30, 2, 0x100, &[(0x1b, 0x200)], 0);
	paced(&mut mux, 0x100, 0, 30, 10, 4);
	program(&mut mux, 0x100);
	paced(&mut mux, 0x100, 1_200 * MS, 100, 10, 4);
	let data = mux.bytes();
	let cut = pcr_at(&data, 3_000 * MS);
	let early = import(&data[..cut], cut, None).await.unwrap();
	assert!(!early.section().random_access, "the window with two programs");
	let imported = import(&data, data.len(), None).await.unwrap();
	assert!(imported.groups().len() >= 12);
	assert!(imported.section().random_access, "the window after it");
}

/// A window holds the PCR intervals that start in it: a second program carried past a line, up to
/// the first PCR after it, counts in the window before the line, and the window from the line on
/// carries one program throughout.
#[tokio::test(start_paused = true)]
async fn a_layout_change_before_the_first_pcr_past_a_line_counts_before_it() {
	let mut mux = Mux::default();
	mux.pat(&[(1, 0x20), (2, 0x30)])
		.pmt(0x20, 1, 0x100, &[(0x1b, 0x100)], 0)
		.pmt(0x30, 2, 0x100, &[(0x1b, 0x200)], 0);
	// The last PCR before the 2 s line is at 1.98 s; most of the interval after it, program 2's
	// packets included, lies past the line, and the PAT drops program 2 before the 2.02 s PCR.
	paced(&mut mux, 0x100, 20 * MS, 50, 10, 4);
	mux.fill(0x200, 20);
	program(&mut mux, 0x100);
	paced(&mut mux, 0x100, 2_020 * MS, 60, 10, 4);
	let data = mux.bytes();
	let end = pcr_at(&data, 4_020 * MS);
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let mut passthrough = Passthrough::new(broadcast.clone(), catalog.reserve()).unwrap();
	passthrough.decode(&data[..end + 188]).unwrap();
	let section = catalog.snapshot().m2ts.expect("the window from 2 s has passed");
	assert!(section.random_access, "the window from the line carried one program");
}

/// The section waits for the first window that starts at or after the first group, so the catalog
/// first lists the track with a whole window behind it.
#[tokio::test(start_paused = true)]
async fn the_catalog_waits_for_a_whole_window() {
	let mut mux = Mux::default();
	program(&mut mux, 0x100);
	// The first group starts at 0.52 s, so the window from 2 s is the first after it.
	paced(&mut mux, 0x100, 520 * MS, 120, 10, 4);
	let data = mux.bytes();
	let end = pcr_at(&data, 4_000 * MS);
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let mut passthrough = Passthrough::new(broadcast.clone(), catalog.reserve()).unwrap();
	passthrough.decode(&data[..end]).unwrap();
	assert!(catalog.snapshot().m2ts.is_none(), "listed before a whole window");
	passthrough.decode(&data[end..end + 188]).unwrap();
	let section = catalog.snapshot().m2ts.expect("listed once the window passed");
	assert!(section.random_access);
	// 250 packets a window, and 5 random access points: 255 * 1504 bits over two seconds.
	assert_eq!(section.mux_rate, Some(192_000));
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
	// The first group waits for a random access point that never comes. The first PCR sits on a
	// line, so the first line watched from a PCR before its second is at 2 s.
	assert_eq!(groups[0][0].micros, 2_000_000 - micros_before_pcr(&data, 2_000_000));
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
	let mut mux = Mux::default();
	program(&mut mux, 0x100);
	paced(&mut mux, 0x100, 0, 125, 10, 4);
	let data = mux.bytes();
	for abort in [false, true] {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let mut passthrough = Passthrough::new(broadcast.clone(), catalog.reserve()).unwrap();
		passthrough.decode(&data).unwrap();
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
/// jump, so the timeline never steps back. The break is marked whether or not the splice is also a
/// random access point, and only a group started by the break alone gives up random access.
#[tokio::test(start_paused = true)]
async fn a_flagged_jump_forward_starts_a_group_behind_a_break() {
	for random_access in [false, true] {
		let mut mux = Mux::default();
		program(&mut mux, 0x100);
		paced(&mut mux, 0x100, 10_000 * MS, 50, 10, 4);
		// A splice forward to thirty seconds, flagged.
		let mut splice = Flags::pcr(30_000 * MS).discontinuity();
		splice.random_access = random_access;
		mux.packet(0x100, splice);
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
			imported.markers,
			[groups[jump - 1][0].group + 1],
			"a marker group between them declares the break, random access {random_access}"
		);
		assert_eq!(groups[jump][0].group, groups[jump - 1][0].group + 2);
		assert_eq!(imported.section().random_access, random_access);
	}
}

/// Where in `data` the packet carrying PCR `ticks` starts.
fn pcr_at(data: &[u8], ticks: u64) -> usize {
	let packet = data.chunks(188).position(|pkt| header(pkt).pcr == Some(ticks));
	packet.expect("a packet carrying that PCR") * 188
}

/// A passthrough importer whose clock a shared epoch at `epoch` placed: PCR zero one wrap before it.
struct Placed {
	broadcast: moq_net::broadcast::Consumer,
	catalog: crate::catalog::Producer,
	passthrough: Passthrough,
	name: String,
}

impl Placed {
	fn new(epoch: std::time::SystemTime) -> Self {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let timebase = catalog.timebase();
		timebase
			.place(moq_net::Timestamp::ZERO, epoch - Duration::from_micros(WRAP_MICROS))
			.unwrap();
		let passthrough = Passthrough::new(broadcast.clone(), timebase.reserve()).unwrap();
		Self {
			broadcast: broadcast.consume(),
			catalog,
			name: passthrough.track().to_string(),
			passthrough,
		}
	}

	async fn finish(mut self) -> Imported {
		self.passthrough.finish().unwrap();
		read_back(&self.broadcast, &self.name, &self.catalog).await.unwrap()
	}
}

/// Whether each object starts a group.
fn starts(imported: &Imported) -> Vec<bool> {
	let objects = &imported.objects;
	(0..objects.len())
		.map(|i| i == 0 || objects[i].group != objects[i - 1].group)
		.collect()
}

/// Import `data` placed by one epoch from its first byte, and again, on a host clock 7 ms ahead,
/// from the program and then the packet carrying PCR `join`, the two fed in step. From the second's
/// first group on, both must publish the same objects in the same groups, and from the second's
/// first section on, the same section after every PCR.
async fn import_twice_placed(data: &[u8], prefix: usize, join: u64) -> (Imported, Imported) {
	import_twice_placed_from(data, prefix, pcr_at(data, join)).await
}

/// [`import_twice_placed`], the second importer joining at byte `split`.
async fn import_twice_placed_from(data: &[u8], prefix: usize, split: usize) -> (Imported, Imported) {
	let epoch = epoch_time();
	let hosts = [epoch + Duration::from_secs(600), epoch + Duration::from_millis(600_007)];
	crate::clock::fake_host_wall(hosts[0]);
	let mut first = Placed::new(epoch);
	first.passthrough.decode(&data[..split]).unwrap();
	crate::clock::fake_host_wall(hosts[1]);
	let mut second = Placed::new(epoch);
	second.passthrough.decode(&data[..prefix]).unwrap();
	let mut described = false;
	for pkt in data[split..].chunks(188) {
		crate::clock::fake_host_wall(hosts[0]);
		first.passthrough.decode(pkt).unwrap();
		crate::clock::fake_host_wall(hosts[1]);
		second.passthrough.decode(pkt).unwrap();
		let Some(pcr) = header(pkt).pcr else { continue };
		let section = second.catalog.snapshot().m2ts;
		described |= section.is_some();
		if described {
			assert_eq!(section, first.catalog.snapshot().m2ts, "the sections after PCR {pcr}");
		}
	}
	assert!(described, "the joiner described no window");
	let (first, second) = (first.finish().await, second.finish().await);

	assert_eq!(first.catalog.clock, Some(epoch_clock(epoch)));
	assert_eq!(second.catalog.clock, first.catalog.clock);
	assert!(!second.objects.is_empty(), "the joiner published nothing");
	let skip = first.objects.len() - second.objects.len();
	for (index, (first, second)) in first.objects[skip..].iter().zip(&second.objects).enumerate() {
		assert_eq!(
			(first.micros, &first.payload),
			(second.micros, &second.payload),
			"object {index} of the importer that joined"
		);
	}
	assert_eq!(starts(&first)[skip..], starts(&second), "group starts");
	(first, second)
}

/// Importers placed by one epoch publish identical objects across a PCR wrap, the second joining
/// after it.
#[tokio::test(start_paused = true)]
async fn importers_sharing_an_epoch_publish_identical_objects_across_a_wrap() {
	const WRAP: u64 = (1 << 33) * 300;
	let mut mux = Mux::default();
	program(&mut mux, 0x100);
	let prefix = mux.out.len();
	let start = WRAP - 2_000 * MS;
	for i in 0..250 {
		mux.packet(0x100, Flags::pcr((start + i * 40 * MS) % WRAP));
		if i % 10 == 0 {
			mux.packet(0x100, ACCESS);
		}
		mux.fill(0x101, 3);
	}
	let data = mux.bytes();
	let (first, _) = import_twice_placed(&data, prefix, 1_000 * MS).await;
	let micros: Vec<u64> = first.objects.iter().map(|object| object.micros).collect();
	assert!(micros.windows(2).all(|pair| pair[0] < pair[1]), "{micros:?}");
}

/// Importers placed by one epoch agree across a flagged jump that carries the source over a wrap,
/// the second joining after it and never seeing the flag.
#[tokio::test(start_paused = true)]
async fn importers_sharing_an_epoch_agree_across_a_flagged_jump() {
	const WRAP: u64 = (1 << 33) * 300;
	let mut mux = Mux::default();
	program(&mut mux, 0x100);
	let prefix = mux.out.len();
	paced(&mut mux, 0x100, WRAP - 3_000 * MS, 50, 10, 4);
	// Flagged three seconds on from the PCR due a second short of the wrap, so the jump crosses it.
	mux.packet(0x100, Flags::pcr(2_000 * MS).discontinuity());
	mux.fill(0x101, 4);
	paced(&mut mux, 0x100, 2_040 * MS, 150, 10, 4);
	let data = mux.bytes();
	let (first, _) = import_twice_placed(&data, prefix, 3_000 * MS).await;
	let micros: Vec<u64> = first.objects.iter().map(|object| object.micros).collect();
	assert!(micros.windows(2).all(|pair| pair[0] <= pair[1]), "{micros:?}");
}

/// Importers placed by one epoch cut the same groups and describe them alike wherever the second
/// joined, as a restarted one does: on random access points rarer than the grid, on none, and on a
/// clock two programs share.
/// Every join falls after a cut on the grid, and the later ones after the first importer recorded a
/// rate.
#[tokio::test(start_paused = true)]
async fn importers_sharing_an_epoch_cut_the_same_groups() {
	// Random access every `gop` PCRs on program 1's video, and how many programs share the clock.
	for (gop, programs) in [(36, 1), (0, 1), (36, 2)] {
		let mut mux = Mux::default();
		match programs {
			1 => program(&mut mux, 0x100),
			_ => {
				mux.pat(&[(1, 0x20), (2, 0x30)])
					.pmt(0x20, 1, 0x100, &[(0x1b, 0x100)], 0)
					.pmt(0x30, 2, 0x100, &[(0x1b, 0x200)], 0);
			}
		}
		let prefix = mux.out.len();
		for i in 0..400 {
			mux.packet(0x100, Flags::pcr(i * 40 * MS));
			if gop != 0 && i % gop == 0 {
				mux.packet(0x100, ACCESS);
			}
			if programs == 2 && i % 36 == 18 {
				mux.packet(0x200, ACCESS);
			}
			mux.fill(0x101, 4);
		}
		let data = mux.bytes();
		for join in [2_520, 4_720, 7_360] {
			let (first, _) = import_twice_placed(&data, prefix, join * MS).await;
			let fallback = first
				.groups()
				.iter()
				.any(|group| !header(&group[0].payload).random_access);
			assert!(fallback, "gop {gop}: the grid cut");
			if gop == 0 || programs == 2 {
				assert!(!first.section().random_access, "gop {gop}, {programs} programs");
			}
			assert!(first.section().mux_rate.is_some(), "gop {gop}: a rate was recorded");
		}
	}
}

/// Importers placed by one epoch split groups at the object ceiling alike, the ceiling counted
/// from a group start both made.
#[tokio::test(start_paused = true)]
async fn importers_sharing_an_epoch_split_full_groups_alike() {
	let mut mux = Mux::default();
	program(&mut mux, 0x100);
	let prefix = mux.out.len();
	mux.packet(0x100, Flags::pcr(0).random_access());
	// A PCR in every packet, 200 µs apart: the ceiling fills before each line of the grid.
	for i in 1..60_000 {
		mux.packet(0x100, Flags::pcr(i * 5_400));
	}
	let data = mux.bytes();
	let (first, second) = import_twice_placed(&data, prefix, 3_300 * MS).await;
	let full = second
		.groups()
		.iter()
		.filter(|group| group.len() == GROUP_OBJECTS_MAX)
		.count();
	assert!(full >= 3, "{full} groups at the ceiling");
	assert!(!first.section().random_access);
}

/// A joiner whose first PCR lands exactly on a window's line misses the grid cut its partner made
/// there, so the first window it describes is the one after: describing that window would publish
/// random access the partner lost to the cut.
#[tokio::test(start_paused = true)]
async fn a_joiner_on_a_window_line_describes_the_window_after() {
	const WINDOW: u64 = 2_000 * MS;
	let mut mux = Mux::default();
	program(&mut mux, 0x100);
	let prefix = mux.out.len();
	// The placed epoch puts these PCRs one wrap on, so `offset` lands every fiftieth on a line.
	let offset = (WINDOW - WRAP % WINDOW) % WINDOW;
	for i in 0..400 {
		mux.packet(0x100, Flags::pcr(offset + i * 40 * MS));
		// No random access before 4 s, then one just after the PCR on each whole second.
		if i >= 100 && i % 25 == 0 {
			mux.packet(0x100, ACCESS);
		}
		mux.fill(0x101, 4);
	}
	let data = mux.bytes();
	let (first, second) = import_twice_placed(&data, prefix, offset + 4_000 * MS).await;
	assert!(
		first.section().random_access,
		"every group from 6 s on is a random access point"
	);
	assert_eq!(first.section(), second.section());
}

/// A joiner whose first PCR is a flagged jump on a line of the grid that it cannot see as one, the
/// retransmitted copy of the flagged PCR or a PCR whose flag came in the packet before, waits out
/// the spacing after it as its partner, which counted the jump there, does.
#[tokio::test(start_paused = true)]
async fn a_joiner_on_a_jump_it_cannot_see_waits_out_the_spacing() {
	const LINE: u64 = 1_000 * MS;
	for retransmitted in [true, false] {
		let mut mux = Mux::default();
		program(&mut mux, 0x100);
		let prefix = mux.out.len();
		// The placed epoch puts these PCRs one wrap on, so `offset` lands every 25th on a line.
		let offset = (LINE - WRAP % LINE) % LINE;
		paced(&mut mux, 0x100, offset, 100, 0, 4);
		let jump = offset + 11 * LINE;
		if retransmitted {
			mux.packet(0x100, Flags::pcr(jump).discontinuity());
			mux.out.extend_from_within(mux.out.len() - 188..);
		} else {
			mux.packet(0x100, Flags::default().discontinuity());
			mux.packet(0x100, Flags::pcr(jump));
		}
		mux.fill(0x101, 4);
		paced(&mut mux, 0x100, jump + 40 * MS, 250, 0, 4);
		let data = mux.bytes();
		let split = pcr_at(&data, jump) + if retransmitted { 188 } else { 0 };
		import_twice_placed_from(&data, prefix, split).await;
	}
}

/// The grid lies on the timeline the epoch places, so it runs on regularly across a PCR wrap that
/// would shift a grid of raw ticks by about 0.72 s, and an importer joining after the wrap cuts on
/// it too.
#[tokio::test(start_paused = true)]
async fn the_grid_runs_on_across_a_wrap() {
	const WRAP: u64 = (1 << 33) * 300;
	let mut mux = Mux::default();
	program(&mut mux, 0x100);
	let prefix = mux.out.len();
	let start = WRAP - 3_000 * MS;
	for i in 0..300 {
		mux.packet(0x100, Flags::pcr((start + i * 40 * MS) % WRAP));
		mux.fill(0x101, 4);
	}
	let data = mux.bytes();
	let (first, _) = import_twice_placed(&data, prefix, 2_000 * MS).await;
	let groups = first.groups();
	assert!(groups.len() >= 10, "{} groups", groups.len());
	for group in &groups {
		let pcr = header(&group[0].payload).pcr.expect("a cut on the grid falls on a PCR");
		let unwrapped = if pcr >= start { pcr } else { pcr + WRAP };
		let past = unwrapped % (1_000 * MS);
		assert!(past < 40 * MS, "a cut {} ms past the line", past / MS);
	}
}

/// With every PCR in the era nearest the clock and nothing clamping it, a flagged jump forward by
/// less than the object a random access point started already reached would start its group
/// before that one: the import ends rather than shift the timeline off the source's clock.
#[tokio::test(start_paused = true)]
async fn a_flagged_jump_short_of_a_group_start_ends_the_import() {
	let mut mux = Mux::default();
	program(&mut mux, 0x100);
	paced(&mut mux, 0x100, 10_000 * MS, 20, 10, 4);
	mux.packet(0x100, Flags::pcr(10_800 * MS));
	mux.packet(0x100, ACCESS);
	mux.fill(0x101, 4);
	// A millisecond on from the last PCR, short of the access point's group a few ms after it.
	mux.packet(0x100, Flags::pcr(10_801 * MS).discontinuity());
	mux.fill(0x101, 4);
	paced(&mut mux, 0x100, 10_841 * MS, 20, 10, 4);
	let data = mux.bytes();
	let err = match import(&data, data.len(), None).await {
		Ok(_) => panic!("the jump was clamped onto the group it falls short of"),
		Err(err) => err,
	};
	assert!(
		err.chain().any(|cause| matches!(
			cause.downcast_ref::<crate::Error>(),
			Some(crate::Error::TimestampRewind(_))
		)),
		"{err:?}"
	);
}

/// A capture that took the catalog's clock first keeps it: the import shifts every object by one
/// offset that lands its first PCR at now, so the timeline still never steps back and a flagged
/// jump still adds its step.
#[tokio::test(start_paused = true)]
async fn an_import_shifts_onto_a_clock_already_in_use() {
	let mut mux = Mux::default();
	program(&mut mux, 0x100);
	paced(&mut mux, 0x100, 3_600_000 * MS, 50, 10, 4);
	mux.packet(0x100, Flags::pcr(3_620_000 * MS).discontinuity());
	mux.fill(0x101, 4);
	paced(&mut mux, 0x100, 3_620_040 * MS, 50, 10, 4);
	let data = mux.bytes();
	let verbatim = import(&data, data.len(), None).await.unwrap();

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let clock = catalog.clock();
	let fixed = catalog.snapshot().clock;
	tokio::time::advance(Duration::from_secs(5)).await;
	let before = clock.now().as_micros() as u64;
	let shifted = import_onto(broadcast, catalog.clone(), &data, data.len(), None)
		.await
		.unwrap();
	let after = clock.now().as_micros() as u64;

	assert_eq!(catalog.snapshot().clock, fixed, "the import never moves the clock");
	let first = shifted.objects[0].micros;
	let lead = verbatim.objects[0].micros - 3_600_000_000;
	assert!(
		(before + lead..=after + lead).contains(&first),
		"the first PCR lands at now: {before} {first} {after}, {lead} µs ahead of it"
	);
	let offset = first as i64 - verbatim.objects[0].micros as i64;
	assert!(offset < 0, "an hour in shifts back onto the capture's five seconds");
	assert_eq!(shifted.objects.len(), verbatim.objects.len());
	for (shifted, verbatim) in shifted.objects.iter().zip(&verbatim.objects) {
		assert_eq!((shifted.group, &shifted.payload), (verbatim.group, &verbatim.payload));
		assert_eq!(
			shifted.micros as i64 - verbatim.micros as i64,
			offset,
			"one offset for every object"
		);
	}
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
	assert_eq!(imported.markers.len(), 1, "one marker for the one jump");
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
