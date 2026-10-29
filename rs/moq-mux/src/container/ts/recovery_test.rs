//! Recovery from MoQ content that never arrives, or arrives too late, after the output began.
//!
//! Each arm imports a clip FFmpeg generates in a declared software profile, records every group
//! and frame the import publishes, and republishes them on a paused clock under a fault schedule
//! at the MoQ content boundary: a group omitted or cut short, frames held past their deadline, a
//! track stalled, the catalog late, a bottleneck whose queue overflows. Loss here is content the
//! path never delivers, not IP packets QUIC would retransmit. The schedule is the ledger of what
//! was delivered and when, and the export of the republished broadcast is graded against it
//! rather than against what the export reports:
//!
//! - Nothing goes out that was not delivered, or that the schedule made late. Every picture that
//!   does keeps its dependency chain: a group's pictures are a decode-order prefix from its
//!   keyframe, and its leading pictures that reference the group before go out only after that
//!   group went out whole.
//! - What the schedule left alone goes out. A track the fault missed keeps running, and each
//!   track resumes at the first group whose chain survives.
//! - Every delivered frame missing from the output is counted: `trimmed` before its track's
//!   first frame went out, `dropped` after, and each skip upstream is one of `gaps`.
//! - Every unit arrives before it decodes, the continuity counters run unbroken, and the PCR
//!   signals a discontinuity exactly where the source declared one.
//! - Each point a receiver acquires at carries its own PAT and PMT, and FFmpeg decodes the output
//!   without an error from there, presenting every picture that went out, once, as the
//!   continuous source decode's picture at the same presentation time.
//!
//! The raw lane's equivalents are in [`release_test`](super::release_test).

use std::ops::Range;
use std::time::Duration;

use mpeg2ts::pes::{PesPacketReader, ReadPesPacket};
use mpeg2ts::ts::TsPacketReader;
use tokio::time::Instant;

use super::export_test::{assert_on_time, count_discontinuity, open_export};
use super::stats::{self, Class};
use crate::catalog::hang::Catalog;
use crate::container::Frame;

/// 25 fps.
const FRAME: Duration = Duration::from_millis(40);
/// AAC-LC at 48 kHz: 1024 samples, exactly 1920 ticks of the 90 kHz clock.
const AUDIO_TICKS: u64 = 1920;
const WRAP: u64 = 1 << 33;

/// How long the export holds each frame past its decode time: half `export ts --delay`'s
/// default, so a fault costs more of it.
const DELAY: Duration = Duration::from_millis(500);
/// How long after its nominal time a frame arrives on a clean path, about a frame's transit.
const LAG: Duration = Duration::from_millis(40);
/// How far a frame's lateness may sit from a bound before the ledger calls it either way: the
/// export deadlines each picture on the decode time it derives, a reorder depth before it
/// presents, and anchors its clock on the track sent latest.
const MARGIN: Duration = Duration::from_millis(160);
/// How long a drain waits for the next frame before taking the export to have stalled.
const STALL: Duration = Duration::from_secs(10);

/// Where a spliced profile changes over from its first encode to its second: the keyframe that
/// opens the seventh group.
const SPLICE: Duration = Duration::from_millis(2880);
/// How far the clock jumps at a declared discontinuity.
const JUMP: Duration = Duration::from_secs(20);

/// A software profile FFmpeg generates: 320x240 at 25 fps, a group every twelve pictures, beside
/// AAC-LC. Six seconds long unless it says otherwise.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Profile {
	/// H.264 High, closed GOP with B pictures, the profile `test/ts/decode.py` grades.
	Closed,
	/// The same, starting 2.7 s before the 33-bit PTS and PCR wrap.
	Wrap,
	/// H.264 open GOP: after the first IDR every group opens on a recovery point, behind three
	/// leading pictures that reference the group before.
	///
	/// Resuming at a recovery point after a hole leaves a `frame_num` gap, which H.264 takes
	/// for unintentional loss (8.2.5.2), so a decoder that ran through the hole conceals around
	/// it. A receiver acquiring at the recovery point decodes it cleanly.
	Open,
	/// HEVC open GOP: after the first IDR every group opens on a CRA, behind three RASL
	/// pictures that reference the group before.
	Rasl,
	/// HEVC closed GOP: after the first IDR every group opens on an IDR_W_RADL, behind two RADL
	/// pictures that lead it but reference only it.
	Radl,
	/// H.264 closed GOP that drops from 320x240 to 176x144 at [`SPLICE`], the new SPS and PPS in
	/// band.
	Resized,
	/// H.264 closed GOP whose PCR declares a discontinuity at [`SPLICE`], where the clock jumps
	/// about [`JUMP`] ahead.
	Spliced,
	/// [`Closed`](Self::Closed) for two minutes.
	Long,
}

/// One FFmpeg encode.
struct Encode {
	size: &'static str,
	seconds: &'static str,
	/// The x264 or x265 parameters.
	x26x: &'static str,
	hevc: bool,
	/// Seconds the timestamps start late by.
	offset: u64,
}

impl Encode {
	fn run(&self) -> Vec<u8> {
		let source = format!("testsrc=size={}:rate=25", self.size);
		let offset = self.offset.to_string();
		let x26x = match self.hevc {
			true => format!("{}:log-level=error", self.x26x),
			false => self.x26x.to_string(),
		};
		let mut args = vec![
			"-nostdin",
			"-hide_banner",
			"-v",
			"error",
			"-f",
			"lavfi",
			"-i",
			&source,
			"-f",
			"lavfi",
			"-i",
			"sine=frequency=1000:sample_rate=48000",
			"-t",
			self.seconds,
		];
		args.extend(match self.hevc {
			true => ["-c:v", "libx265", "-x265-params"],
			false => ["-c:v", "libx264", "-x264-params"],
		});
		args.push(&x26x);
		if !self.hevc {
			args.extend(["-profile:v", "high"]);
		}
		args.extend([
			"-preset",
			"veryfast",
			"-threads",
			"1",
			"-pix_fmt",
			"yuv420p",
			"-b:v",
			"400k",
			"-c:a",
			"aac",
			"-b:a",
			"96k",
			"-output_ts_offset",
			&offset,
			// One audio frame a PES, so a splice cuts no PES but the one it starts at.
			"-pes_payload_size",
			"0",
			"-pcr_period",
			"20",
			"-f",
			"mpegts",
			"pipe:1",
		]);
		let output = std::process::Command::new("ffmpeg")
			.args(&args)
			.output()
			.expect("ffmpeg is in the dev shell");
		let stderr = String::from_utf8_lossy(&output.stderr);
		assert!(output.status.success() && stderr.is_empty(), "ffmpeg: {stderr}");
		output.stdout
	}
}

/// A generated clip, and the encodes its pictures come from.
struct Clip {
	ts: Vec<u8>,
	/// Each encode, and the 90 kHz presentation times the clip takes from it.
	parts: Vec<(Vec<u8>, Range<u64>)>,
}

impl Profile {
	/// The clip, generated once per process.
	fn clip(self) -> &'static Clip {
		static CLIPS: [std::sync::OnceLock<Clip>; 8] = [const { std::sync::OnceLock::new() }; 8];
		CLIPS[self as usize].get_or_init(|| {
			const CLOSED: &str = "keyint=12:min-keyint=12:scenecut=0";
			let encode = |size, x26x, hevc, offset| Encode {
				size,
				seconds: "6",
				x26x,
				hevc,
				offset,
			};
			let whole = |encode: Encode| {
				let ts = encode.run();
				let parts = vec![(ts.clone(), 0..u64::MAX)];
				Clip { ts, parts }
			};
			match self {
				Self::Closed => whole(encode("320x240", CLOSED, false, 0)),
				// 2^33 ticks is 95,443.7 s.
				Self::Wrap => whole(encode("320x240", CLOSED, false, 95441)),
				Self::Open => whole(encode(
					"320x240",
					"keyint=12:min-keyint=12:scenecut=0:open-gop=1:bframes=3:b-adapt=0",
					false,
					0,
				)),
				Self::Rasl => whole(encode(
					"320x240",
					"keyint=12:min-keyint=12:scenecut=0:open-gop=1:bframes=3:b-adapt=0",
					true,
					0,
				)),
				Self::Radl => whole(encode(
					"320x240",
					"keyint=12:min-keyint=12:scenecut=0:open-gop=0:radl=2:bframes=3:b-adapt=0",
					true,
					0,
				)),
				Self::Resized => splice(
					encode("320x240", CLOSED, false, 0).run(),
					encode("176x144", CLOSED, false, 0).run(),
					false,
				),
				Self::Spliced => splice(
					encode("320x240", CLOSED, false, 0).run(),
					encode("320x240", CLOSED, false, JUMP.as_secs()).run(),
					true,
				),
				Self::Long => whole(Encode {
					seconds: "120",
					..encode("320x240", CLOSED, false, 0)
				}),
			}
		})
	}

	/// How long the clip runs.
	fn seconds(self) -> usize {
		match self {
			Self::Long => 120,
			_ => 6,
		}
	}

	/// Whether its leading pictures reference the group before.
	fn orphans(self) -> bool {
		matches!(self, Self::Open | Self::Rasl)
	}

	/// The PCR discontinuities the source declares.
	fn breaks(self) -> usize {
		usize::from(self == Self::Spliced)
	}
}

/// The PID, payload and payload unit start of a packet.
fn payload(packet: &[u8; 188]) -> (u16, &[u8], bool) {
	let pid = u16::from_be_bytes([packet[1] & 0x1f, packet[2]]);
	let start = match packet[3] & 0x30 {
		0x10 => 4,
		0x30 => (5 + usize::from(packet[4])).min(188),
		_ => 188,
	};
	(pid, &packet[start..], packet[1] & 0x40 != 0)
}

/// The PTS a packet's PES starts with, if it starts one.
fn pes_pts(packet: &[u8; 188]) -> Option<u64> {
	let (_, pes, start) = payload(packet);
	if !start || pes.len() < 14 || pes[..3] != [0, 0, 1] || pes[7] & 0x80 == 0 {
		return None;
	}
	Some(
		u64::from((pes[9] >> 1) & 7) << 30
			| u64::from(pes[10]) << 22
			| u64::from(pes[11] >> 1) << 15
			| u64::from(pes[12]) << 7
			| u64::from(pes[13] >> 1),
	)
}

/// Whether a packet starts a video PES.
fn starts_video(packet: &[u8; 188]) -> bool {
	let (_, pes, start) = payload(packet);
	start && pes.len() > 3 && pes[..3] == [0, 0, 1] && (0xe0..=0xef).contains(&pes[3])
}

/// `a` up to its keyframe at [`SPLICE`], then `b` from its own, declaring a PCR discontinuity
/// there if `declare`. Every other PID resumes in `b` at its first PES presented after `a`'s last
/// whole one, and every continuity counter carries on across the join.
fn splice(a: Vec<u8>, b: Vec<u8>, declare: bool) -> Clip {
	let (a_packets, b_packets) = (a.as_chunks::<188>().0, b.as_chunks::<188>().0);
	// The keyframe `SPLICE` after each encode's first picture.
	let at = |packets: &[[u8; 188]]| {
		let first = packets.iter().filter(|p| starts_video(p)).find_map(pes_pts).unwrap();
		let pts = first + SPLICE.as_micros() as u64 * 9 / 100;
		let index = packets.iter().position(|p| starts_video(p) && pes_pts(p) == Some(pts));
		(index.expect("a keyframe at the splice"), pts)
	};
	let ((from, from_pts), (to, to_pts)) = (at(a_packets), at(b_packets));

	// Each PID's last whole PES in `a`: its PTS, and where an unfinished one started.
	let mut last = std::collections::HashMap::<u16, (Option<u64>, usize, usize, usize)>::new();
	for (index, packet) in a_packets[..from].iter().enumerate() {
		let (pid, data, start) = payload(packet);
		let entry = last.entry(pid).or_insert((None, index, 0, 0));
		if start && data.len() >= 6 && data[..3] == [0, 0, 1] {
			if entry.3 == 0 || entry.2 >= entry.3 {
				entry.0 = entry.0.max(pes_pts(packet));
			}
			*entry = (
				entry.0,
				index,
				0,
				match u16::from_be_bytes([data[4], data[5]]) {
					0 => 0,
					len => usize::from(len) + 6,
				},
			);
		}
		entry.2 += data.len();
	}
	let mut out: Vec<[u8; 188]> = Vec::new();
	for (index, packet) in a_packets[..from].iter().enumerate() {
		let (pid, ..) = payload(packet);
		// A PES `a` leaves unfinished goes too.
		match last.get(&pid) {
			Some(&(_, started, have, want)) if want > have && index >= started => {}
			_ => out.push(*packet),
		}
	}
	if declare {
		// Receive an AU boundary ahead of the discontinuity. The preceding
		// unbounded video PES is now provably complete; the delimiter's own
		// unfinished AU is discarded when the clock breaks.
		let pid = payload(&a_packets[from]).0;
		let cc = out
			.iter()
			.rev()
			.find(|packet| payload(packet).0 == pid && packet[3] & 0x10 != 0)
			.unwrap()[3]
			.wrapping_add(1)
			& 15;
		let pes = [0, 0, 1, 0xe0, 0, 8, 0x80, 0, 0, 0, 0, 1, 9, 0xf0];
		let mut packet = [0xff; 188];
		packet[..6].copy_from_slice(&[0x47, 0x40 | (pid >> 8) as u8, pid as u8, 0x30 | cc, 169, 0]);
		packet[174..].copy_from_slice(&pes);
		out.push(packet);
	}

	let mut counters = std::collections::HashMap::new();
	for packet in &out {
		if packet[3] & 0x10 != 0 {
			counters.insert(payload(packet).0, packet[3] & 0x0f);
		}
	}
	let mut resumed = std::collections::HashSet::new();
	for (index, packet) in b_packets[to..].iter().enumerate() {
		let (pid, data, start) = payload(packet);
		if !resumed.contains(&pid) {
			let pes = data.len() >= 3 && data[..3] == [0, 0, 1];
			let after = match (pes_pts(packet), last.get(&pid)) {
				(Some(pts), Some(&(Some(kept), ..))) => pts > kept,
				_ => true,
			};
			if !(start && (!pes || after) || index == 0) {
				continue;
			}
			resumed.insert(pid);
		}
		let mut packet = *packet;
		if packet[3] & 0x10 != 0
			&& let Some(counter) = counters.get_mut(&pid)
		{
			*counter = (*counter + 1) & 0x0f;
			packet[3] = (packet[3] & 0xf0) | *counter;
		}
		if declare && index == 0 {
			assert!(
				packet[3] & 0x20 != 0 && packet[4] > 0 && packet[5] & 0x10 != 0,
				"the keyframe carries the PCR"
			);
			packet[5] |= 0x80;
		}
		out.push(packet);
	}
	Clip {
		ts: out.into_flattened(),
		parts: vec![(a, 0..from_pts), (b, to_pts..u64::MAX)],
	}
}

/// One track as the import published it: each group's sequence and frames, in decode order.
struct Recorded {
	name: String,
	video: bool,
	groups: Vec<(u64, Vec<moq_net::frame::Frame>)>,
}

/// What the import of a clip published.
struct Recording {
	profile: Profile,
	catalog: Catalog,
	tracks: Vec<Recorded>,
}

/// Import `profile`'s clip whole and read back every frame of every track.
async fn record(profile: Profile) -> Recording {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	// Kept whole, however long the clip.
	let config = crate::catalog::Config::default().with_max_age(Duration::from_secs(300));
	let catalog = crate::catalog::Producer::new(&mut broadcast, config).unwrap();
	let consumer = broadcast.consume();
	let mut import = super::Import::new(broadcast.clone(), catalog.reserve());
	import.decode(&profile.clip().ts).unwrap();
	import.finish().unwrap();
	let snapshot = catalog.snapshot();

	let names = (snapshot.video.renditions.keys().map(|name| (name.clone(), true)))
		.chain(snapshot.audio.renditions.keys().map(|name| (name.clone(), false)));
	let mut tracks = Vec::new();
	for (name, video) in names {
		let mut track = consumer
			.track(&name)
			.unwrap()
			.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(300)))
			.await
			.unwrap();
		let mut groups = Vec::new();
		while let Some(mut group) = tokio::time::timeout(Duration::from_secs(1), track.recv_group())
			.await
			.unwrap()
			.unwrap()
		{
			let mut frames = Vec::new();
			while let Some(frame) = group.read_frame().await.unwrap() {
				frames.push(frame);
			}
			groups.push((group.sequence, frames));
		}
		tracks.push(Recorded { name, video, groups });
	}
	assert_eq!(
		tracks.iter().map(|track| track.video).collect::<Vec<_>>(),
		[true, false],
		"the clip publishes a video and an audio track"
	);
	let pictures = (tracks[0].groups.iter().flat_map(|(_, frames)| frames))
		.filter(|frame| !decoded(frame).1)
		.count();
	assert_eq!(pictures, profile.seconds() * 25, "the import publishes every picture");
	Recording {
		profile,
		catalog: snapshot,
		tracks,
	}
}

/// The presentation time of a published frame, in microseconds on the catalog clock, and
/// whether it is the marker of a declared discontinuity.
fn decoded(frame: &moq_net::frame::Frame) -> (u64, bool) {
	let frame = hang::container::Frame::decode(frame.payload.clone()).unwrap();
	(frame.timestamp.as_micros() as u64, frame.payload.is_empty())
}

/// The presentation time of a published frame, in microseconds on the catalog clock.
fn micros(frame: &moq_net::frame::Frame) -> u64 {
	decoded(frame).0
}

/// The 90 kHz presentation time a microsecond timestamp goes out at, wrapped.
fn ticks(micros: u64) -> u64 {
	(micros * 9).div_ceil(100) % WRAP
}

/// A frame's place in a recording, and when a clean path delivers it after the first frame.
#[derive(Clone, Copy, Debug)]
struct Slot {
	video: bool,
	/// Index into the track's recorded groups.
	group: usize,
	/// Index into the group's frames.
	frame: usize,
	nominal: Duration,
	/// The marker of a declared discontinuity, not media.
	marker: bool,
	/// Its payload's size in bytes.
	size: usize,
}

/// Where a schedule puts a frame.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Delivery {
	At(Duration),
	Omitted,
}

/// A fault schedule: where each frame goes, from its slot. A group delivers a prefix of its
/// frames; one that loses the rest is evicted once its next frame would have arrived.
type Schedule = fn(Slot) -> Delivery;

fn clean(slot: Slot) -> Delivery {
	Delivery::At(slot.nominal)
}

/// A bottleneck every track shares after the schedule's faults: while it is congested, frames
/// leave at `rate` in the order they arrive, behind a FIFO `queue` deep that tail-drops what
/// overflows it. A group that loses a frame there loses the rest of it too.
#[derive(Clone, Debug)]
struct Bottleneck {
	/// Bits per second.
	rate: u64,
	queue: Duration,
	/// When the path is congested; outside it, the link carries whatever reaches it at once.
	congested: fn(Duration) -> bool,
}

impl Bottleneck {
	/// Pass every delivery through the link, in arrival order.
	fn carry(&self, slots: &[(usize, Slot)], deliveries: &mut [Delivery]) {
		let mut order: Vec<usize> = (0..slots.len())
			.filter(|&i| deliveries[i] != Delivery::Omitted)
			.collect();
		order.sort_by_key(|&i| match deliveries[i] {
			Delivery::At(at) => at,
			Delivery::Omitted => unreachable!(),
		});
		let mut free = Duration::ZERO;
		let mut cut = std::collections::HashSet::new();
		for i in order {
			let (track, slot) = slots[i];
			let Delivery::At(at) = deliveries[i] else {
				unreachable!()
			};
			if cut.contains(&(track, slot.group)) {
				deliveries[i] = Delivery::Omitted;
				continue;
			}
			if !(self.congested)(at) {
				free = free.max(at);
				deliveries[i] = Delivery::At(free);
				continue;
			}
			// The frame is admitted only if all of it fits: the backlog ahead plus its own bytes.
			let leaves = free.max(at) + Duration::from_nanos(slot.size as u64 * 8 * 1_000_000_000 / self.rate);
			if leaves - at > self.queue {
				cut.insert((track, slot.group));
				deliveries[i] = Delivery::Omitted;
				continue;
			}
			free = leaves;
			deliveries[i] = Delivery::At(free);
		}
	}
}

/// What one arm republishes, and how.
#[derive(Clone)]
struct Arm {
	profile: Profile,
	schedule: Schedule,
	/// A bottleneck after the faults.
	path: Option<Bottleneck>,
	/// When the export starts.
	join: Duration,
	/// When the catalog names the tracks.
	catalog: Duration,
}

/// `profile` under `schedule`, exported from the start.
fn arm(profile: Profile, schedule: Schedule) -> Arm {
	Arm {
		profile,
		schedule,
		path: None,
		join: Duration::ZERO,
		catalog: Duration::ZERO,
	}
}

/// What the schedule decided for one recorded frame, and what the export made of it.
#[derive(Clone, Debug)]
struct Unit {
	group: usize,
	/// 90 kHz presentation time, wrapped as the output carries it.
	pts: u64,
	nominal: Duration,
	/// Presented before its group's keyframe.
	leading: bool,
	/// A declared discontinuity's marker, which carries no media.
	marker: bool,
	delivered: bool,
	/// Whether it arrived inside both the source's age budget and its deadline, `None` when
	/// too close to call.
	on_time: Option<bool>,
	output: bool,
}

/// One track's ledger, in decode order.
#[derive(Clone, Debug)]
struct Ledger {
	video: bool,
	/// Whether its leading pictures reference the group before.
	orphans: bool,
	units: Vec<Unit>,
	/// Skips the schedule made upstream: each run of omitted groups, and each cut with the run
	/// after it.
	gaps: u64,
}

impl Ledger {
	/// The units that carry media.
	fn media(&self) -> impl Iterator<Item = (usize, &Unit)> {
		self.units.iter().enumerate().filter(|(_, unit)| !unit.marker)
	}
}

/// What an export of a republished recording wrote.
struct Run {
	ledgers: Vec<Ledger>,
	frames: Vec<Frame>,
	stats: stats::Export,
	/// The stats after each output frame, and when, from the republisher's start.
	samples: Vec<(Duration, stats::Export)>,
}

impl Run {
	fn ts(&self) -> Vec<u8> {
		self.frames
			.iter()
			.flat_map(|frame| frame.payload.iter().copied())
			.collect()
	}

	fn ledger(&self, video: bool) -> &Ledger {
		self.ledgers.iter().find(|ledger| ledger.video == video).unwrap()
	}

	/// The PID of the output's audio or video stream.
	fn pid(&self, class: Class) -> u16 {
		let [pid] = self
			.stats
			.streams
			.iter()
			.filter(|(_, row)| row.class == class)
			.map(|(pid, _)| *pid)
			.collect::<Vec<_>>()[..]
		else {
			panic!("one {class:?} row: {:?}", self.stats.streams);
		};
		pid
	}
}

/// Republish `recording` as `arm` says, start an export, and match its output to the ledger.
async fn run(recording: &Recording, arm: &Arm) -> Run {
	let origin = recording
		.tracks
		.iter()
		.filter_map(|track| track.groups.first())
		.map(|(_, frames)| micros(&frames[0]))
		.min()
		.unwrap();
	// How far a declared break moved the clock, from the video's last picture before it to its
	// first after, a frame on; and where the frames after it start.
	let video = &recording.tracks[0].groups;
	let (jumped, jump) = match video.iter().position(|(_, frames)| decoded(&frames[0]).1) {
		Some(marker) => {
			let before = micros(&video[marker].1[0]) + FRAME.as_micros() as u64;
			let jump = micros(&video[marker + 1].1[0]) - before;
			(before + jump / 2, Duration::from_micros(jump))
		}
		None => (u64::MAX, Duration::ZERO),
	};

	// Every frame's slot, in recorded order, and where the schedule puts it.
	let mut slots: Vec<(usize, Slot)> = Vec::new();
	for (t, track) in recording.tracks.iter().enumerate() {
		let mut last = Duration::ZERO;
		for (g, (_, frames)) in track.groups.iter().enumerate() {
			let keyframe = micros(&frames[0]);
			for (f, frame) in frames.iter().enumerate() {
				let (at, marker) = decoded(frame);
				let since = |micros: u64| match micros >= jumped {
					true => Duration::from_micros(micros - origin) - jump,
					false => Duration::from_micros(micros - origin),
				};
				let nominal = match (marker, track.video) {
					// Written at the break, right behind what came before it.
					(true, _) => last,
					// A group's pictures go out in decode order, a frame apart.
					(false, true) => since(keyframe) + FRAME * f as u32,
					(false, false) => since(at),
				};
				last = nominal;
				let size = frame.payload.len();
				let video = track.video;
				let slot = Slot {
					video,
					group: g,
					frame: f,
					nominal,
					marker,
					size,
				};
				slots.push((t, slot));
			}
		}
	}
	let mut deliveries: Vec<Delivery> = slots.iter().map(|(_, slot)| (arm.schedule)(*slot)).collect();
	if let Some(path) = &arm.path {
		path.carry(&slots, &mut deliveries);
	}

	// Every frame's delivery, and each group's end: (at, track, group, frame or the end).
	let mut events = Vec::new();
	let mut ledgers = Vec::new();
	let opened = arm.join.max(arm.catalog);
	let mut next = 0;
	for (t, track) in recording.tracks.iter().enumerate() {
		let mut units = Vec::new();
		let mut gaps = 0;
		let mut skipping = false;
		// The group a subscription made when the export opens starts at: the newest begun by then.
		let mut joined = 0;
		for (g, (_, frames)) in track.groups.iter().enumerate() {
			let keyframe = micros(&frames[0]);
			let mut kept = 0;
			for (f, frame) in frames.iter().enumerate() {
				let (_, slot) = slots[next];
				let delivery = deliveries[next];
				next += 1;
				let at = micros(frame);
				let on_time = match delivery {
					Delivery::At(arrival) => {
						let late = arrival.saturating_sub(slot.nominal) + LAG;
						// The source skips a group older than half the delay behind the newest. The
						// clock anchors on the video, which the export sends up to a delay ahead of
						// its decode time, so audio, sent barely ahead, has nearly a delay more.
						let slack = if track.video { DELAY } else { 2 * DELAY };
						match (late + MARGIN < DELAY / 2, late > slack + MARGIN) {
							(true, _) => Some(true),
							(_, true) => Some(false),
							_ => None,
						}
					}
					Delivery::Omitted => None,
				};
				if let Delivery::At(arrival) = delivery {
					assert_eq!(kept, f, "{} group {g} resumes after a hole", track.name);
					if f == 0 && arrival + LAG <= opened {
						joined = g;
					}
					kept += 1;
					events.push((arrival + LAG, Some((t, g, Some(f)))));
				}
				units.push(Unit {
					group: g,
					pts: ticks(at),
					nominal: slot.nominal,
					leading: track.video && at < keyframe,
					marker: slot.marker,
					delivered: delivery != Delivery::Omitted,
					on_time,
					output: false,
				});
			}
			let whole = kept == frames.len();
			if whole {
				let last = events.last().map(|(at, ..)| *at).unwrap();
				events.push((last, Some((t, g, None))));
			} else if kept > 0 {
				// Evicted once its next frame would have arrived, after what it delivered.
				let next = units[units.len() - frames.len() + kept].nominal + LAG;
				let last = events.last().map(|(at, ..)| *at).unwrap();
				events.push((next.max(last), Some((t, g, None))));
			}
			if kept > 0 && skipping {
				gaps += 1;
			}
			skipping = match kept {
				0 => skipping || units.iter().any(|unit| unit.delivered),
				_ => !whole,
			};
		}
		assert!(!skipping, "{}: a schedule ends a track on a hole", track.name);
		// What came before the export opened was never offered to it.
		for unit in units.iter_mut().filter(|unit| unit.group < joined) {
			unit.delivered = false;
		}
		ledgers.push(Ledger {
			video: track.video,
			orphans: recording.profile.orphans(),
			units,
			gaps,
		});
	}
	// The catalog first among its peers; stable, so a group's end follows its last frame.
	events.push((arm.catalog, None));
	events.sort_by_key(|&(at, event)| (at, event.is_some()));

	// The republisher: the same catalog and tracks on a new broadcast, each frame at its time.
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let snapshot = recording.catalog.clone();
	let producers: Vec<_> = recording
		.tracks
		.iter()
		.map(|track| {
			let priority = match track.video {
				true => hang::catalog::PRIORITY.video,
				false => hang::catalog::PRIORITY.audio,
			};
			broadcast
				.create_track(track.name.clone(), catalog.track_info(priority))
				.unwrap()
		})
		.collect();
	let consumer = broadcast.consume();
	let groups: Vec<Vec<(u64, Vec<moq_net::frame::Frame>)>> =
		recording.tracks.iter().map(|track| track.groups.clone()).collect();
	let start = Instant::now();
	let publisher = tokio::spawn(async move {
		let _keep = &broadcast;
		let mut open: Vec<Option<moq_net::group::Producer>> = producers.iter().map(|_| None).collect();
		let mut kept = vec![0; producers.len()];
		for (at, event) in events {
			tokio::time::sleep_until(start + at).await;
			let Some((t, g, step)) = event else {
				let snapshot = snapshot.clone();
				catalog
					.mutate(|catalog| {
						catalog.video = snapshot.video;
						catalog.audio = snapshot.audio;
					})
					.unwrap();
				continue;
			};
			let (sequence, frames) = &groups[t][g];
			let Some(f) = step else {
				let mut group = open[t].take().unwrap();
				match kept[t] == frames.len() {
					true => group.finish().unwrap(),
					false => group.abort(moq_net::Error::Old).unwrap(),
				}
				continue;
			};
			let group = open[t].get_or_insert_with(|| {
				kept[t] = 0;
				let info = moq_net::group::Info { sequence: *sequence };
				producers[t].create_group(info).unwrap()
			});
			assert_eq!(group.sequence, *sequence, "a schedule interleaves groups of track {t}");
			group
				.write_frame(frames[f].timestamp, frames[f].payload.clone())
				.unwrap();
			kept[t] += 1;
		}
		assert!(open.iter().all(Option::is_none), "every group ends");
		for producer in &producers {
			producer.finish().unwrap();
		}
		let _keep = catalog;
		std::future::pending::<()>().await;
	});

	tokio::time::sleep_until(start + arm.join).await;
	let source = crate::source::announced(&consumer).await;
	let mut export = open_export(source).await.unwrap().with_delay(DELAY);
	let mut frames = Vec::new();
	let mut samples = Vec::new();
	while let Ok(next) = tokio::time::timeout(STALL, export.next()).await {
		let Some(frame) = next.expect("export error") else {
			break;
		};
		frames.push(frame);
		samples.push((start.elapsed(), export.stats()));
	}
	publisher.abort();
	let mut run = Run {
		ledgers,
		frames,
		stats: export.stats(),
		samples,
	};

	// Match each PES, and each ADTS frame of an audio PES, to the unit it carries.
	let mut reader = PesPacketReader::new(TsPacketReader::new(std::io::Cursor::new(run.ts())));
	while let Some(pes) = reader.read_pes_packet().unwrap() {
		let video = pes.header.stream_id.is_video();
		assert!(video || pes.header.stream_id.is_audio(), "only audio and video go out");
		let pts = pes.header.pts.expect("every PES carries a PTS").as_u64();
		let count = match video {
			true => 1,
			false => adts_frames(&pes.data),
		};
		let ledger = run.ledgers.iter_mut().find(|ledger| ledger.video == video).unwrap();
		for k in 0..count as u64 {
			let pts = (pts + k * AUDIO_TICKS) % WRAP;
			let matched: Vec<usize> = (ledger.media())
				.filter(|(_, unit)| unit.pts.abs_diff(pts) <= 1)
				.map(|(i, _)| i)
				.collect();
			let [i] = matched[..] else {
				panic!("output PTS {pts} matches units {matched:?} of the video={video} ledger");
			};
			assert!(!ledger.units[i].output, "unit {i} went out twice");
			ledger.units[i].output = true;
		}
	}
	run
}

/// How many ADTS frames an AAC PES carries.
fn adts_frames(mut data: &[u8]) -> usize {
	let mut count = 0;
	while data.len() > 7 {
		assert!(data[0] == 0xff && data[1] & 0xf0 == 0xf0, "an ADTS sync word");
		let len = usize::from(data[3] & 3) << 11 | usize::from(data[4]) << 3 | usize::from(data[5] >> 5);
		data = &data[len.clamp(7, data.len())..];
		count += 1;
	}
	count
}

/// Whether each unit's chain survived the schedule, so it must go out, failing on a picture that
/// went out without its own.
fn chain(ledger: &Ledger) -> Result<Vec<bool>, String> {
	let units = &ledger.units;
	let mut chained = vec![false; units.len()];
	if !ledger.video {
		for (i, unit) in ledger.media() {
			chained[i] = unit.delivered && unit.on_time == Some(true);
		}
		return Ok(chained);
	}
	// Whether every picture of the group before went out, but the orphans it left out.
	let mut whole = false;
	let mut i = 0;
	while i < units.len() {
		let group = units[i].group;
		let (mut alive, mut broken) = (true, false);
		let mut complete = true;
		if units[i].marker {
			i += 1;
			continue;
		}
		while i < units.len() && units[i].group == group {
			let unit = &units[i];
			if unit.leading && ledger.orphans && !whole {
				if unit.output {
					return Err(format!("video unit {i} went out without the group before it: {unit:?}"));
				}
				i += 1;
				continue;
			}
			// Nothing after a leading picture that needs only its own keyframe references it.
			if unit.leading && !ledger.orphans {
				chained[i] = alive && unit.delivered && unit.on_time == Some(true);
				if unit.output && broken {
					return Err(format!("video unit {i} went out after its chain broke: {unit:?}"));
				}
				i += 1;
				continue;
			}
			alive &= unit.delivered && unit.on_time == Some(true);
			chained[i] = alive;
			match unit.output {
				true if broken => return Err(format!("video unit {i} went out after its chain broke: {unit:?}")),
				true => {}
				false => (broken, complete) = (true, false),
			}
			i += 1;
		}
		whole = complete;
	}
	Ok(chained)
}

/// Grade an export against its ledger and the counters it reported.
fn grade(ledgers: &[Ledger], stats: &stats::Export) -> Result<(), String> {
	let video = ledgers.iter().find(|ledger| ledger.video).unwrap();
	let Some(start) = video.units.iter().find(|unit| unit.output) else {
		return Err("no video went out".into());
	};
	let keyframe = video.units.iter().find(|unit| unit.group == start.group).unwrap();
	if keyframe.pts != start.pts {
		return Err(format!("the video starts mid-group, at {start:?}"));
	}
	// Missing frames before and after their track's first went out, those the schedule made late,
	// and the skips it made.
	let (mut before, mut after, mut late, mut gaps) = (0u64, 0u64, 0u64, 0u64);
	for ledger in ledgers {
		let name = if ledger.video { "video" } else { "audio" };
		let units = &ledger.units;
		for (i, unit) in ledger.media() {
			if unit.output && (!unit.delivered || unit.on_time == Some(false)) {
				return Err(format!("{name} unit {i} went out, though {unit:?}"));
			}
		}
		let chained = chain(ledger)?;

		// The export starts on the video's first group, with the audio presented from its
		// keyframe on; audio straddling the keyframe may be trimmed with what precedes it.
		for (i, unit) in ledger.media() {
			let started = match ledger.video {
				true => unit.group >= start.group,
				false => (AUDIO_TICKS..WRAP / 2).contains(&((unit.pts + WRAP - keyframe.pts) % WRAP)),
			};
			if started && chained[i] && !unit.output {
				return Err(format!(
					"{name} unit {i} survived the schedule but did not go out: {unit:?}"
				));
			}
		}

		let first = units.iter().position(|unit| unit.output).unwrap_or(units.len());
		for (i, unit) in ledger.media() {
			if unit.delivered && !unit.output {
				*if i < first { &mut before } else { &mut after } += 1;
				late += u64::from(unit.on_time != Some(true));
			}
		}
		gaps += ledger.gaps;
	}

	// The source skips frames that came older than its age budget, each run one more gap, and
	// they are neither trimmed nor dropped.
	let skips = stats.release.gaps.checked_sub(gaps).ok_or(format!(
		"{} gaps reported for the {gaps} the schedule made",
		stats.release.gaps
	))?;
	let counted = stats.release.dropped + stats.release.trimmed;
	let skipped = (before + after).checked_sub(counted).ok_or(format!(
		"{counted} frames counted dropped or trimmed, but only {} are missing",
		before + after
	))?;
	let exact = (stats.release.dropped, stats.release.trimmed) == (after, before);
	if !(exact || (skipped > 0 && stats.release.dropped <= after && stats.release.trimmed <= before))
		|| (skips > 0) != (skipped > 0)
		|| skips > skipped
		|| skipped > late
	{
		return Err(format!(
			"(dropped, trimmed, gaps) reported {:?}; {before} missing before each track started, \
			 {after} after, {late} of them late, and {gaps} gaps scheduled",
			(stats.release.dropped, stats.release.trimmed, stats.release.gaps)
		));
	}
	Ok(())
}

/// Every PID's continuity counter steps by one on each packet with a payload.
fn assert_continuous(ts: &[u8]) {
	let mut last = std::collections::HashMap::new();
	for (at, packet) in ts.as_chunks::<188>().0.iter().enumerate() {
		let pid = u16::from_be_bytes([packet[1] & 0x1f, packet[2]]);
		if pid == 0x1fff || packet[3] & 0x10 == 0 {
			continue;
		}
		let counter = packet[3] & 0x0f;
		if let Some(previous) = last.insert(pid, counter) {
			assert_eq!(
				counter,
				(previous + 1) & 0x0f,
				"PID {pid:#x} skips its counter at packet {at}"
			);
		}
	}
}

/// What FFmpeg decoded: each picture's 90 kHz presentation time and hash, and the audio's
/// sample count.
struct Decoded {
	pictures: Vec<(u64, String)>,
	samples: u64,
}

/// Decode `ts` with software FFmpeg, its audio too unless `video_only`, failing on any error it
/// logs. Pictures keep their own size, so a resolution change hashes each at its own.
fn decode(ts: &[u8], video_only: bool) -> Result<Decoded, String> {
	use std::io::Write as _;
	let mut args = vec![
		"-nostdin",
		"-hide_banner",
		"-v",
		"error",
		"-xerror",
		"-err_detect",
		"explode",
		"-copyts",
		"-threads",
		"1",
		"-i",
		"pipe:0",
		"-map",
		"0:v:0",
		"-noautoscale",
		"-c:v",
		"rawvideo",
		"-fps_mode",
		"passthrough",
		"-enc_time_base:v",
		"1:90000",
	];
	if !video_only {
		args.extend(["-map", "0:a:0", "-c:a", "pcm_s16le"]);
	}
	args.extend(["-f", "framemd5", "pipe:1"]);
	let mut child = std::process::Command::new("ffmpeg")
		.args(&args)
		.stdin(std::process::Stdio::piped())
		.stdout(std::process::Stdio::piped())
		.stderr(std::process::Stdio::piped())
		.spawn()
		.expect("ffmpeg is in the dev shell");
	let mut stdin = child.stdin.take().unwrap();
	let input = ts.to_vec();
	let feed = std::thread::spawn(move || stdin.write_all(&input));
	let output = child.wait_with_output().unwrap();
	// FFmpeg may stop reading at its first error, which the status reports.
	let _ = feed.join().unwrap();
	let stderr = String::from_utf8_lossy(&output.stderr);
	if !output.status.success() || !stderr.is_empty() {
		return Err(format!("FFmpeg failed ({}): {stderr}", output.status));
	}
	let mut decoded = Decoded {
		pictures: Vec::new(),
		samples: 0,
	};
	for line in String::from_utf8(output.stdout).unwrap().lines() {
		if line.starts_with("#tb 0:") {
			assert_eq!(line, "#tb 0: 1/90000", "pictures are stamped at 90 kHz");
		}
		if line.starts_with('#') || line.is_empty() {
			continue;
		}
		let fields: Vec<&str> = line.split(',').map(str::trim).collect();
		let [index, _dts, pts, duration, _size, hash] = fields[..] else {
			panic!("framemd5 line {line}");
		};
		match index {
			"0" => {
				let pts: i64 = pts.parse().unwrap();
				decoded
					.pictures
					.push((pts.rem_euclid(WRAP as i64) as u64, hash.to_string()));
			}
			_ => decoded.samples += duration.parse::<u64>().unwrap(),
		}
	}
	if decoded.pictures.is_empty() || (!video_only && decoded.samples == 0) {
		return Err("FFmpeg decoded no pictures or no audio".into());
	}
	Ok(decoded)
}

/// The continuous source decode's pictures, by presentation time on the catalog clock.
struct Reference(std::collections::HashMap<u64, String>);

impl Reference {
	fn new(profile: Profile, video: &Ledger) -> Self {
		// Each encode decoded on its own, from its start, keeping the pictures the clip took.
		let source: Vec<(u64, String)> = (profile.clip().parts.iter())
			.flat_map(|(ts, range)| {
				let pictures = decode(ts, false).unwrap().pictures;
				pictures.into_iter().filter(|(pts, _)| range.contains(pts))
			})
			.collect();
		// The import moved the source onto the catalog clock; the first pictures give the shift.
		let earliest = |pts: &mut dyn Iterator<Item = u64>| pts.min_by_key(|pts| (pts + WRAP / 2) % WRAP).unwrap();
		let shift = earliest(&mut source.iter().map(|(pts, _)| *pts)) + WRAP
			- earliest(&mut video.media().map(|(_, unit)| unit.pts));
		Self(
			source
				.into_iter()
				.map(|(pts, hash)| ((pts + WRAP - shift % WRAP) % WRAP, hash))
				.collect(),
		)
	}

	/// FFmpeg presented each `emitted` picture exactly once, as the source's, and nothing else.
	fn compare(&self, decoded: &Decoded, emitted: &[&Unit]) -> Result<(), String> {
		let mut presented = vec![false; emitted.len()];
		for (pts, hash) in &decoded.pictures {
			let Some(i) = emitted.iter().position(|unit| unit.pts.abs_diff(*pts) <= 1) else {
				return Err(format!("FFmpeg presented a picture at {pts} that never went out"));
			};
			if std::mem::replace(&mut presented[i], true) {
				return Err(format!("FFmpeg presented the picture at {pts} twice"));
			}
			let source = (pts.saturating_sub(1)..=pts + 1).find_map(|pts| self.0.get(&pts));
			if source != Some(hash) {
				return Err(format!("the picture at {pts} differs from the source's"));
			}
		}
		match presented.iter().filter(|presented| !**presented).count() {
			0 => Ok(()),
			withheld => Err(format!(
				"FFmpeg withheld {withheld} of the {} pictures that went out",
				emitted.len()
			)),
		}
	}
}

/// A fresh decoder's input, and the pictures it carries.
type Segment<'a> = (Vec<u8>, Vec<&'a Unit>);

/// The video from each point a receiver acquires at, as a fresh decoder's input: its first
/// keyframe out, and each one it resumes at after a group did not go out whole. Each segment
/// starts at the last PAT before its keyframe, which must follow the picture before it so a
/// receiver acquiring there finds the tables the export sent for it, and runs to the next
/// segment, without the audio. Each comes beside the pictures it carries.
fn segments<'a>(ts: &[u8], video: &'a Ledger, pids: (u16, u16)) -> Result<Vec<Segment<'a>>, String> {
	let packets = ts.as_chunks::<188>().0;
	let pid = |packet: &[u8; 188]| payload(packet).0;
	let (video_pid, audio_pid) = pids;
	// The packet each picture's PES starts at, by its PTS.
	let starts: Vec<(usize, u64)> = (packets.iter().enumerate())
		.filter(|(_, packet)| pid(packet) == video_pid && packet[1] & 0x40 != 0)
		.map(|(at, packet)| (at, pes_pts(packet).unwrap()))
		.collect();

	// Where each segment's keyframe starts, and its pictures.
	let mut cuts: Vec<(usize, Vec<&Unit>)> = Vec::new();
	let mut whole = false;
	for group in video.units.chunk_by(|a, b| a.group == b.group) {
		if group[0].marker {
			continue;
		}
		let emitted: Vec<&Unit> = group.iter().filter(|unit| unit.output).collect();
		match emitted.first() {
			Some(first) if !whole => {
				let (at, _) = starts.iter().find(|(_, pts)| pts.abs_diff(first.pts) <= 1).unwrap();
				cuts.push((*at, emitted));
			}
			_ => cuts
				.last_mut()
				.into_iter()
				.for_each(|(_, units)| units.extend(&emitted)),
		}
		// The group before must have gone out whole for its leading pictures to.
		whole = group
			.iter()
			.all(|unit| unit.output || (unit.leading && video.orphans && !whole));
	}

	// Each segment from the PAT ahead of its keyframe, after the picture before it.
	let mut froms = Vec::new();
	for (at, _) in &cuts {
		let before = starts
			.iter()
			.rev()
			.find(|(start, _)| start < at)
			.map_or(0, |(start, _)| *start);
		let Some(pat) = (before..*at).rev().find(|&i| pid(&packets[i]) == 0) else {
			return Err(format!(
				"no PAT between the picture at packet {before} and the keyframe at {at}"
			));
		};
		froms.push(pat);
	}
	let ends: Vec<usize> = froms.iter().skip(1).copied().chain([packets.len()]).collect();
	Ok((froms.into_iter().zip(ends).zip(cuts))
		.map(|((from, end), (_, units))| {
			let bytes = packets[from..end].iter().filter(|packet| pid(packet) != audio_pid);
			(bytes.flatten().copied().collect(), units)
		})
		.collect())
}

/// Where an arm leaves its evidence, kept when it fails or `HARNESS_KEEP` is set: under
/// `$HARNESS_RUNS/ts-recovery/`, by test name.
struct Evidence(Option<std::path::PathBuf>);

impl Evidence {
	fn save(run: &Run) -> Self {
		let Some(root) = std::env::var_os("HARNESS_RUNS") else {
			return Self(None);
		};
		// libtest names each test's thread after it; a test run on the main thread has its process.
		let name = match std::thread::current().name() {
			Some(name) if name != "main" => name.replace("::", "-"),
			_ => format!("arm-{}", std::process::id()),
		};
		let dir = std::path::Path::new(&root).join("ts-recovery").join(name);
		std::fs::create_dir_all(&dir).unwrap();
		std::fs::write(dir.join("export.ts"), run.ts()).unwrap();
		std::fs::write(dir.join("ledgers.txt"), format!("{:#?}", run.ledgers)).unwrap();
		std::fs::write(dir.join("stats.txt"), format!("{:#?}", run.stats)).unwrap();
		Self(Some(dir))
	}
}

impl Drop for Evidence {
	fn drop(&mut self) {
		if let Some(dir) = &self.0
			&& !std::thread::panicking()
			&& std::env::var_os("HARNESS_KEEP").is_none()
		{
			let _ = std::fs::remove_dir_all(dir);
		}
	}
}

/// Republish `arm`'s clip under its faults, export it, and grade the export and its decode:
/// from each point a receiver acquires at, and as a whole when the profile's resumptions are
/// decodable straight through.
async fn recover(arm: Arm) -> Run {
	let profile = arm.profile;
	let recording = record(profile).await;
	let run = run(&recording, &arm).await;
	let _evidence = Evidence::save(&run);
	grade(&run.ledgers, &run.stats).unwrap();
	assert_eq!(
		run.stats.release.out_of_tolerance, 0,
		"exact source clock: {:?}",
		run.stats.release
	);
	assert!(assert_on_time(&run.frames) > 200, "too little output to judge");
	assert_continuous(&run.ts());
	assert_eq!(
		count_discontinuity(&run.frames),
		profile.breaks(),
		"a PCR discontinuity where the source declared one, and no other"
	);

	let reference = Reference::new(profile, run.ledger(true));
	let pids = (run.pid(Class::Video), run.pid(Class::Audio));
	for (i, (ts, emitted)) in segments(&run.ts(), run.ledger(true), pids).unwrap().iter().enumerate() {
		let output = decode(ts, true).unwrap_or_else(|err| panic!("segment {i}: {err}"));
		(reference.compare(&output, emitted)).unwrap_or_else(|err| panic!("segment {i}: {err}"));
	}
	if profile != Profile::Open {
		let output = decode(&run.ts(), false).unwrap();
		let video = run.ledger(true);
		let emitted: Vec<&Unit> = video.units.iter().filter(|unit| unit.output).collect();
		reference.compare(&output, &emitted).unwrap();
		let frames = out(run.ledger(false)) as u64;
		assert_eq!(output.samples, frames * 1024, "every audio frame that went out decodes");
	}
	run
}

/// How many of a ledger's units went out.
fn out(ledger: &Ledger) -> usize {
	ledger.units.iter().filter(|unit| unit.output).count()
}

/// How long after its nominal time each picture that went out did, in output order.
fn lags(run: &Run) -> Vec<Duration> {
	let pid = run.pid(Class::Video);
	let emitted: Vec<&Unit> = run.ledger(true).units.iter().filter(|unit| unit.output).collect();
	assert_eq!(
		run.stats.streams[&pid].units,
		emitted.len() as u64,
		"a PES for each picture"
	);
	let mut lags = Vec::new();
	for (at, stats) in &run.samples {
		while (lags.len() as u64) < stats.streams.get(&pid).map_or(0, |row| row.units) {
			lags.push(at.saturating_sub(emitted[lags.len()].nominal));
		}
	}
	lags
}

#[tokio::test(start_paused = true)]
async fn a_clean_path_loses_nothing() {
	let run = recover(arm(Profile::Closed, clean)).await;
	let video = run.ledger(true);
	assert_eq!(out(video), video.units.len(), "every picture goes out");
	assert_eq!((run.stats.release.dropped, run.stats.release.gaps), (0, 0));
}

/// A group the path never delivers is skipped whole: the group before keeps its tail, though
/// nothing after it settles their decode times until the hole is past, and the export resumes
/// on the next keyframe.
#[tokio::test(start_paused = true)]
async fn a_skipped_group_resumes_at_the_next() {
	let run = recover(arm(Profile::Closed, |slot| match slot.video && slot.group == 5 {
		true => Delivery::Omitted,
		false => clean(slot),
	}))
	.await;
	assert_eq!((run.stats.release.dropped, run.stats.release.gaps), (0, 1));
}

/// A group cut short keeps the pictures it delivered, though they are fewer than it takes to
/// settle their decode times, and loses only its tail.
#[tokio::test(start_paused = true)]
async fn a_cut_group_keeps_what_it_delivered() {
	let run = recover(arm(Profile::Closed, |slot| {
		match slot.video && slot.group == 5 && slot.frame >= 4 {
			true => Delivery::Omitted,
			false => clean(slot),
		}
	}))
	.await;
	let video = run.ledger(true);
	let kept = video.units.iter().filter(|unit| unit.group == 5 && unit.output).count();
	assert_eq!(kept, 4, "the cut group's delivered pictures go out");
	assert_eq!((run.stats.release.dropped, run.stats.release.gaps), (0, 1));
}

/// The video stalls for 2.5 s while the audio runs on, then delivers what it held at once: the
/// source skips the groups it held past the age budget as one gap, the newest whole group it held,
/// within that budget but past its deadline, is dropped, nothing late goes out, and the video
/// resumes on the first keyframe delivered in time, on the same clock.
#[tokio::test(start_paused = true)]
async fn a_stall_drops_what_it_made_late() {
	let run = recover(arm(Profile::Closed, |slot| hold(slot.video, slot))).await;
	let (video, audio) = (run.ledger(true), run.ledger(false));
	assert!(
		video.units.iter().any(|unit| unit.on_time == Some(false)),
		"the schedule made pictures late"
	);
	assert_eq!((run.stats.release.dropped, run.stats.release.gaps), (12, 1));
	let first = audio.units.iter().position(|unit| unit.output).unwrap();
	assert!(
		audio.units[first..].iter().all(|unit| unit.output),
		"the audio runs on through the stall"
	);
}

/// The whole path stalls, so nothing wakes the export until it delivers what it held. Each frame
/// is judged on when it landed, not on when the export next ran, so what the stall held behind the
/// deadline is dropped rather than sent late.
#[tokio::test(start_paused = true)]
async fn a_path_stall_drops_what_it_made_late() {
	recover(arm(Profile::Closed, |slot| hold(true, slot))).await;
}

/// Hold what is `held` and due in [1.92 s, 4.4 s) until the end, delivering the rest cleanly.
fn hold(held: bool, slot: Slot) -> Delivery {
	let (from, until) = (Duration::from_millis(1920), Duration::from_millis(4400));
	match held && (from..until).contains(&slot.nominal) {
		true => Delivery::At(until),
		false => clean(slot),
	}
}

/// The audio stalls for 1.5 s while the video runs on: every picture still goes out, the
/// audio's row stops counting and its quiet grows on the output's clock, and the audio resumes.
#[tokio::test(start_paused = true)]
async fn a_stalled_track_leaves_the_others_running() {
	let run = recover(arm(Profile::Closed, stall_audio)).await;
	let video = run.ledger(true);
	assert_eq!(out(video), video.units.len(), "every picture goes out");
	assert_eq!((run.stats.release.dropped, run.stats.release.gaps), (0, 1));

	let (audio, pictures) = (run.pid(Class::Audio), run.pid(Class::Video));
	// The longest stretch of output the audio row sat still through.
	let during = (run
		.samples
		.chunk_by(|(_, a), (_, b)| a.streams[&audio].units == b.streams[&audio].units))
	.max_by_key(|run| run.last().unwrap().0 - run[0].0)
	.unwrap();
	let ((from, first), (until, last)) = (&during[0], during.last().unwrap());
	assert!(
		*until - *from > Duration::from_millis(1200),
		"the audio row stops counting, for only {:?}",
		*until - *from
	);
	assert!(
		last.streams[&pictures].units > first.streams[&pictures].units,
		"the video row keeps counting"
	);
	let quiet = last.streams[&audio].quiet.expect("the output carries a PCR");
	assert!(quiet > Duration::from_millis(1200), "audio quiet for only {quiet:?}");
	assert!(
		run.stats.streams[&audio].units > last.streams[&audio].units,
		"the audio resumes"
	);
}

/// Omit the audio due in [2 s, 3.5 s).
fn stall_audio(slot: Slot) -> Delivery {
	let (from, until) = (Duration::from_millis(2000), Duration::from_millis(3500));
	match !slot.video && (from..until).contains(&slot.nominal) {
		true => Delivery::Omitted,
		false => clean(slot),
	}
}

/// Audio sent 200 ms behind the video, within the delay, survives a skipped video group whole.
#[tokio::test(start_paused = true)]
async fn skewed_audio_survives_a_video_skip() {
	let run = recover(arm(Profile::Closed, |slot| match (slot.video, slot.group) {
		(true, 5) => Delivery::Omitted,
		(true, _) => clean(slot),
		(false, _) => Delivery::At(slot.nominal + Duration::from_millis(200)),
	}))
	.await;
	assert_eq!((run.stats.release.dropped, run.stats.release.gaps), (0, 1));
}

/// A skipped group around the 33-bit wrap keeps the clock, and the pictures either side decode.
#[tokio::test(start_paused = true)]
async fn a_skip_across_the_wrap_keeps_the_clock() {
	let run = recover(arm(Profile::Wrap, skip_group_5)).await;
	let video = run.ledger(true);
	let pts: Vec<u64> = video
		.units
		.iter()
		.filter(|unit| unit.output)
		.map(|unit| unit.pts)
		.collect();
	assert!(
		pts.windows(2).any(|pair| pair[1] < pair[0] / 2),
		"the output crosses the wrap"
	);
	assert_eq!((run.stats.release.dropped, run.stats.release.gaps), (0, 1));
}

fn skip_group_5(slot: Slot) -> Delivery {
	match slot.video && slot.group == 5 {
		true => Delivery::Omitted,
		false => clean(slot),
	}
}

/// Skip video group 5 and cut group 8 after its sixth picture.
fn skip_and_cut(slot: Slot) -> Delivery {
	match slot.video && (slot.group == 5 || (slot.group == 8 && slot.frame >= 6)) {
		true => Delivery::Omitted,
		false => clean(slot),
	}
}

/// How many of `group`'s leading pictures went out, failing if it has none.
fn leading_out(video: &Ledger, group: usize) -> usize {
	let units: Vec<&Unit> = video
		.units
		.iter()
		.filter(|unit| unit.group == group && unit.leading)
		.collect();
	assert!(!units.is_empty(), "group {group} has leading pictures");
	units.iter().filter(|unit| unit.output).count()
}

/// Open GOP: a skipped group and a cut one each orphan the next group's leading pictures, which
/// are trimmed, while every group after a whole one keeps its own.
#[tokio::test(start_paused = true)]
async fn open_gop_trims_only_the_orphans() {
	let run = recover(arm(Profile::Open, skip_and_cut)).await;
	let video = run.ledger(true);
	assert_eq!(
		(leading_out(video, 6), leading_out(video, 9)),
		(0, 0),
		"the orphans are trimmed"
	);
	assert!(
		leading_out(video, 7) > 0 && leading_out(video, 10) > 0,
		"a continuous group keeps them"
	);
	assert_eq!(run.stats.release.gaps, 2);
}

/// HEVC open GOP: the RASL pictures a skip or a cut orphans are trimmed, a continuous CRA keeps
/// its own, and the output decodes straight through the CRA it resumes at.
#[tokio::test(start_paused = true)]
async fn hevc_trims_only_orphaned_rasl() {
	let run = recover(arm(Profile::Rasl, skip_and_cut)).await;
	let video = run.ledger(true);
	assert_eq!(
		(leading_out(video, 6), leading_out(video, 9)),
		(0, 0),
		"the orphans are trimmed"
	);
	assert!(
		leading_out(video, 7) > 0 && leading_out(video, 10) > 0,
		"a continuous group keeps them"
	);
	assert_eq!(run.stats.release.gaps, 2);
}

/// HEVC RADL pictures reference only their own IDR, so after a skip or a cut they go out with it.
#[tokio::test(start_paused = true)]
async fn hevc_keeps_radl_after_a_hole() {
	let run = recover(arm(Profile::Radl, skip_and_cut)).await;
	let video = run.ledger(true);
	assert_eq!(
		(leading_out(video, 6), leading_out(video, 9)),
		(2, 2),
		"RADL survives a hole"
	);
	assert_eq!(run.stats.release.gaps, 2);
}

/// A subscriber joining 2.3 s in starts on a whole group and loses nothing after it.
#[tokio::test(start_paused = true)]
async fn a_join_starts_on_a_whole_group() {
	let run = recover(Arm {
		join: Duration::from_millis(2300),
		..arm(Profile::Closed, clean)
	})
	.await;
	let video = run.ledger(true);
	let first = video.units.iter().find(|unit| unit.output).unwrap().group;
	assert!(first >= 4, "the export starts at the live edge, group {first}");
	assert_eq!((run.stats.release.dropped, run.stats.release.gaps), (0, 0));
}

/// The catalog names the tracks only 2.3 s in, while the group in flight is cut short: the export
/// starts on what arrived of it, from its keyframe with the tables ahead of it, and loses
/// nothing after the cut.
#[tokio::test(start_paused = true)]
async fn a_late_catalog_starts_on_the_group_in_flight() {
	let run = recover(Arm {
		catalog: Duration::from_millis(2300),
		..arm(Profile::Closed, |slot| {
			match slot.video && slot.group == 4 && slot.frame >= 10 {
				true => Delivery::Omitted,
				false => clean(slot),
			}
		})
	})
	.await;
	let video = run.ledger(true);
	let first = video.units.iter().find(|unit| unit.output).unwrap().group;
	assert_eq!(first, 4, "the export starts on the group in flight");
	assert_eq!(run.stats.release.dropped, 0);
}

/// The picture size changes at a keyframe the path never delivers: the export resumes at the next
/// one with its new parameter sets, which a receiver acquiring there and one that ran through the
/// hole both decode at the new size.
#[tokio::test(start_paused = true)]
async fn a_skip_across_a_resolution_change_takes_the_new_parameters() {
	let run = recover(arm(Profile::Resized, |slot| {
		match slot.video && (slot.group == 6 || (slot.group == 5 && slot.frame >= 4)) {
			true => Delivery::Omitted,
			false => clean(slot),
		}
	}))
	.await;
	assert_eq!((run.stats.release.dropped, run.stats.release.gaps), (0, 1));
}

/// The clock jumps 20 s at a declared discontinuity, and the path loses the group before the
/// break and the one after it: the output signals the break once, on the PCR, and resumes on the
/// keyframe after.
#[tokio::test(start_paused = true)]
async fn a_declared_discontinuity_survives_loss_either_side() {
	let run = recover(arm(Profile::Spliced, |slot| {
		match slot.video && (slot.group == 7 || (slot.group == 5 && slot.frame >= 4)) {
			true => Delivery::Omitted,
			false => clean(slot),
		}
	}))
	.await;
	assert_eq!(run.stats.release.dropped, 0);
}

/// The path loses the marker that declares the break, and every frame on either side arrives:
/// the export still finds the break where the clock jumps and signals it once.
#[tokio::test(start_paused = true)]
async fn a_lost_discontinuity_marker_still_breaks_the_clock() {
	let run = recover(arm(Profile::Spliced, |slot| match slot.marker {
		true => Delivery::Omitted,
		false => clean(slot),
	}))
	.await;
	assert_eq!(run.stats.release.dropped, 0);
}

/// A path narrower than the media, behind a queue twice the delay deep, for two seconds: frames
/// queue past their deadline and the queue overflows. Nothing late goes out, every group the
/// export resumes at is whole from its keyframe, and its latency recovers once the path does.
#[tokio::test(start_paused = true)]
async fn a_congested_path_sheds_what_it_made_late() {
	let run = recover(Arm {
		path: Some(Bottleneck {
			rate: 150_000,
			queue: 2 * DELAY,
			congested: |at| (Duration::from_millis(1500)..Duration::from_millis(3500)).contains(&at),
		}),
		..arm(Profile::Closed, clean)
	})
	.await;
	let video = run.ledger(true);
	assert!(video.units.iter().any(|unit| !unit.delivered), "the queue overflowed");
	assert!(
		video.units.iter().any(|unit| unit.on_time == Some(false)),
		"the queue made pictures late"
	);
	let lags = lags(&run);
	// A group's leading pictures decode just before its keyframe is presented, so they leave
	// later than the rest: compare the least lag of a whole group at each end.
	let group = 12;
	let (first, last) = (
		lags[..group].iter().min().unwrap(),
		lags[lags.len() - group..].iter().min().unwrap(),
	);
	assert!(
		last.abs_diff(*first) <= FRAME,
		"the latency recovers: {first:?} at first, {last:?} at last"
	);
}

/// The bottleneck tail-drops a frame whose own bytes don't fit behind the backlog.
#[test]
fn the_bottleneck_admits_only_what_fits() {
	// At 8 kbit/s, 1000 bytes take the whole one-second queue.
	let link = Bottleneck {
		rate: 8_000,
		queue: Duration::from_secs(1),
		congested: |_| true,
	};
	let slot = |group, size| Slot {
		video: true,
		group,
		frame: 0,
		nominal: Duration::ZERO,
		marker: false,
		size,
	};
	let slots = [(0, slot(0, 1000)), (0, slot(1, 1))];
	let mut deliveries = [Delivery::At(Duration::ZERO); 2];
	link.carry(&slots, &mut deliveries);
	assert_eq!(
		deliveries,
		[Delivery::At(Duration::from_secs(1)), Delivery::Omitted],
		"a full queue has no room for one more byte"
	);
}

/// One of each fault every twenty seconds, for the first five cycles: a skipped video group at 2 s, one cut
/// short at 6 s, the video held from 9 s to 11.5 s, and the audio lost from 13 s to 14.5 s.
fn faults(slot: Slot) -> Delivery {
	const CYCLE: u128 = 20_000_000;
	// Where the frame's group starts: a video group's keyframe, or the audio frame itself.
	let start = match slot.video {
		true => slot.nominal - FRAME * slot.frame as u32,
		false => slot.nominal,
	};
	if start >= Duration::from_secs(100) {
		return clean(slot);
	}
	let cycled = |at: Duration| Duration::from_micros((at.as_micros() % CYCLE) as u64);
	let within =
		|from: u64, until: u64, at: Duration| (Duration::from_millis(from)..Duration::from_millis(until)).contains(&at);
	let (since, group) = (cycled(slot.nominal), cycled(start));
	match slot.video {
		true if within(2000, 2480, group) => Delivery::Omitted,
		true if within(6000, 6480, group) && slot.frame >= 4 => Delivery::Omitted,
		true if within(9000, 11500, group) => Delivery::At(slot.nominal - since + Duration::from_millis(11500)),
		false if within(13000, 14500, since) => Delivery::Omitted,
		_ => clean(slot),
	}
}

/// Two minutes of faults, and a congested path between them: every recovery is graded as in the
/// short arms, and the latency never grows past where it started.
#[tokio::test(start_paused = true)]
#[ignore = "long: run nightly by `just rs ts-recovery`"]
async fn long_faults_hold_their_latency() {
	let run = recover(Arm {
		path: Some(Bottleneck {
			rate: 150_000,
			queue: 2 * DELAY,
			// Two seconds of every twenty.
			congested: |at| (17..19).contains(&(at.as_secs() % 20)) && at < Duration::from_secs(100),
		}),
		..arm(Profile::Long, faults)
	})
	.await;
	assert!(run.stats.release.gaps >= 20, "every fault skipped: {:?}", run.stats);
	let lags = lags(&run);
	let (least, most) = (lags.iter().min().unwrap(), lags.iter().max().unwrap());
	assert!(
		*most - *least <= MARGIN,
		"the latency ranged from {least:?} to {most:?}"
	);
	// The first and last ten seconds of pictures, each on average.
	let mean = |lags: &[Duration]| lags.iter().sum::<Duration>() / lags.len() as u32;
	let (first, last) = (mean(&lags[..250]), mean(&lags[lags.len() - 250..]));
	assert!(
		first.abs_diff(last) <= FRAME,
		"the latency drifted from {first:?} to {last:?}"
	);
}

/// The graders fail what they should: a picture that went out after its chain broke, a group
/// that survived but did not go out, a loss the counters do not show, a resumption without its
/// own tables, and a picture FFmpeg cannot decode as the source's.
#[tokio::test(start_paused = true)]
async fn the_graders_fail_what_they_should() {
	let recording = record(Profile::Closed).await;
	let run = run(&recording, &arm(Profile::Closed, skip_group_5)).await;
	grade(&run.ledgers, &run.stats).unwrap();
	let video = run.ledgers.iter().position(|ledger| ledger.video).unwrap();
	let unit = |group: usize, frame: usize| {
		run.ledgers[video]
			.units
			.iter()
			.position(|unit| unit.group == group)
			.unwrap() + frame
	};
	let edited = |edit: &dyn Fn(&mut Vec<Ledger>, &mut stats::Export)| {
		let (mut ledgers, mut stats) = (run.ledgers.clone(), run.stats.clone());
		edit(&mut ledgers, &mut stats);
		grade(&ledgers, &stats).unwrap_err()
	};
	let err = edited(&|ledgers, _| ledgers[video].units[unit(5, 0)].output = true);
	assert!(err.contains("went out, though"), "{err}");
	let err = edited(&|ledgers, stats| {
		ledgers[video].units[unit(6, 3)].output = false;
		stats.release.dropped += 1;
	});
	assert!(err.contains("chain broke"), "{err}");
	let err = edited(&|ledgers, stats| {
		for frame in 0..12 {
			ledgers[video].units[unit(7, frame)].output = false;
		}
		stats.release.dropped += 12;
	});
	assert!(err.contains("did not go out"), "{err}");
	let err = edited(&|_, stats| stats.release.dropped += 1);
	assert!(err.contains("dropped"), "{err}");
	let err = edited(&|_, stats| stats.release.gaps = 0);
	assert!(err.contains("gaps reported"), "{err}");

	// The output less the tables ahead of the keyframe it resumes at.
	let ts = run.ts();
	let pid = run.pid(Class::Video);
	let pids = (pid, run.pid(Class::Audio));
	let packets = ts.as_chunks::<188>().0;
	let resume = (packets.iter())
		.position(|packet| {
			payload(packet).0 == pid && pes_pts(packet) == Some(run.ledgers[video].units[unit(6, 0)].pts)
		})
		.unwrap();
	let tables = (packets[..resume].iter()).rposition(starts_video).unwrap();
	let stripped: Vec<u8> = (packets.iter().enumerate())
		.filter(|&(at, packet)| !((tables..resume).contains(&at) && payload(packet).0 == 0))
		.flat_map(|(_, packet)| packet.iter().copied())
		.collect();
	segments(&ts, &run.ledgers[video], pids).unwrap();
	let err = segments(&stripped, &run.ledgers[video], pids).unwrap_err();
	assert!(err.contains("no PAT"), "{err}");

	// The output less one P picture, with the ledger agreeing it never went out: FFmpeg must
	// refuse what follows it, or decode it as other than the source's.
	let reference = Reference::new(Profile::Closed, run.ledger(true));
	let target = unit(3, 1);
	let mut starts = (packets.iter().enumerate())
		.filter(|(_, packet)| payload(packet).0 == pid && packet[1] & 0x40 != 0)
		.map(|(at, _)| at);
	let from = starts.nth(target).unwrap();
	let to = starts.next().unwrap();
	let mut damaged = ts[..from * 188].to_vec();
	damaged.extend_from_slice(&ts[to * 188..]);
	let mut emitted: Vec<&Unit> = run.ledgers[video].units.iter().filter(|unit| unit.output).collect();
	emitted.retain(|unit| unit.pts != run.ledgers[video].units[target].pts);
	let verdict = decode(&damaged, true).and_then(|output| reference.compare(&output, &emitted));
	assert!(verdict.is_err(), "a lost reference decoded as the source");
}
