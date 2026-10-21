//! Recovery from MoQ content that never arrives, or arrives too late, after the output began.
//!
//! Each arm imports a clip FFmpeg generates in a declared software profile, records every group
//! and frame the import publishes, and republishes them on a paused clock under a fault schedule
//! at the MoQ content boundary: a group omitted or cut short, frames held past their deadline, a
//! track stalled. Loss here is content the path never delivers, not IP packets QUIC would
//! retransmit. The schedule is the ledger of what was delivered and when, and the export of the
//! republished broadcast is graded against it rather than against what the export reports:
//!
//! - Nothing goes out that was not delivered, or that the schedule made late. Every picture that
//!   does keeps its dependency chain: a group's pictures are a decode-order prefix from its
//!   keyframe, and its leading pictures go out only after the group before went out whole.
//! - What the schedule left alone goes out. A track the fault missed keeps running, and each
//!   track resumes at the first group whose chain survives.
//! - Every delivered frame missing from the output is counted: `trimmed` before its track's
//!   first frame went out, `dropped` after, and each skip upstream is one of `gaps`.
//! - Every unit arrives before it decodes, and the continuity counters run unbroken.
//! - FFmpeg decodes the output without an error from each point a receiver acquires at, and
//!   presents every picture that went out, once, as the continuous source decode's picture at
//!   the same presentation time.
//!
//! The raw lane's equivalents are in [`release_test`](super::release_test).

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

/// How long the export holds each frame past its decode time: `export ts --delay`'s default.
const DELAY: Duration = Duration::from_secs(1);
/// How long after its nominal time a frame arrives on a clean path, about a frame's transit.
const LAG: Duration = Duration::from_millis(40);
/// How far a frame's lateness may sit from a bound before the ledger calls it either way: the
/// export deadlines each picture on the decode time it derives, a reorder depth before it
/// presents, and anchors its clock on the track sent latest.
const MARGIN: Duration = Duration::from_millis(160);
/// How long a drain waits for the next frame before taking the export to have stalled.
const STALL: Duration = Duration::from_secs(10);

/// A software profile FFmpeg generates: 6 s of 320x240 H.264 High at 25 fps, a group every
/// twelve pictures, beside AAC-LC.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Profile {
	/// Closed GOP with B pictures, the profile `test/ts/decode.py` grades.
	Closed,
	/// The same, starting 2.7 s before the 33-bit PTS and PCR wrap.
	Wrap,
	/// Open GOP: after the first IDR every group opens on a recovery point, behind three
	/// leading pictures that reference the group before.
	///
	/// Resuming at a recovery point after a hole leaves a `frame_num` gap, which H.264 takes
	/// for unintentional loss (8.2.5.2), so a decoder that ran through the hole conceals around
	/// it. A receiver acquiring at the recovery point decodes it cleanly.
	Open,
}

impl Profile {
	/// The clip, generated once per process.
	fn clip(self) -> &'static [u8] {
		static CLIPS: [std::sync::OnceLock<Vec<u8>>; 3] = [const { std::sync::OnceLock::new() }; 3];
		CLIPS[self as usize].get_or_init(|| {
			let x264 = match self {
				Self::Closed | Self::Wrap => "keyint=12:min-keyint=12:scenecut=0",
				Self::Open => "keyint=12:min-keyint=12:scenecut=0:open-gop=1:bframes=3:b-adapt=0",
			};
			let mut args = vec![
				"-nostdin",
				"-hide_banner",
				"-v",
				"error",
				"-f",
				"lavfi",
				"-i",
				"testsrc=size=320x240:rate=25",
				"-f",
				"lavfi",
				"-i",
				"sine=frequency=1000:sample_rate=48000",
				"-t",
				"6",
				"-c:v",
				"libx264",
				"-profile:v",
				"high",
				"-preset",
				"veryfast",
				"-threads",
				"1",
				"-pix_fmt",
				"yuv420p",
				"-x264-params",
				x264,
				"-b:v",
				"400k",
				"-c:a",
				"aac",
				"-b:a",
				"96k",
			];
			if self == Self::Wrap {
				// 2^33 ticks is 95,443.7 s.
				args.extend(["-output_ts_offset", "95441"]);
			}
			args.extend(["-pcr_period", "20", "-f", "mpegts", "pipe:1"]);
			let output = std::process::Command::new("ffmpeg")
				.args(&args)
				.output()
				.expect("ffmpeg is in the dev shell");
			let stderr = String::from_utf8_lossy(&output.stderr);
			assert!(output.status.success() && stderr.is_empty(), "ffmpeg: {stderr}");
			output.stdout
		})
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
	catalog: Catalog,
	tracks: Vec<Recorded>,
}

/// Import `ts` whole and read back every frame of every track.
async fn record(ts: &[u8]) -> Recording {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let consumer = broadcast.consume();
	let mut import = super::Import::new(broadcast.clone(), catalog.reserve());
	import.decode(ts).unwrap();
	import.finish().unwrap();
	let snapshot = catalog.snapshot();

	let names = (snapshot.video.renditions.keys().map(|name| (name.clone(), true)))
		.chain(snapshot.audio.renditions.keys().map(|name| (name.clone(), false)));
	let mut tracks = Vec::new();
	for (name, video) in names {
		let mut track = consumer
			.track(&name)
			.unwrap()
			.subscribe(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(30)))
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
	Recording {
		catalog: snapshot,
		tracks,
	}
}

/// The presentation time of a published frame, in microseconds on the catalog clock.
fn micros(frame: &moq_net::frame::Frame) -> u64 {
	hang::container::Frame::decode(frame.payload.clone())
		.unwrap()
		.timestamp
		.as_micros() as u64
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

/// What the schedule decided for one recorded frame, and what the export made of it.
#[derive(Clone, Debug)]
struct Unit {
	group: usize,
	/// 90 kHz presentation time, wrapped as the output carries it.
	pts: u64,
	/// Presented before its group's keyframe, so it references the group before.
	leading: bool,
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
	units: Vec<Unit>,
	/// Skips the schedule made upstream: each run of omitted groups, and each cut with the run
	/// after it.
	gaps: u64,
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

/// Republish `recording` under `schedule`, start an export `join` in, and match its output to
/// the ledger.
async fn run(recording: &Recording, schedule: Schedule, join: Duration) -> Run {
	let origin = recording
		.tracks
		.iter()
		.filter_map(|track| track.groups.first())
		.map(|(_, frames)| micros(&frames[0]))
		.min()
		.unwrap();

	// Every frame's delivery, and each group's end: (at, track, group, frame or the end).
	let mut events = Vec::new();
	let mut ledgers = Vec::new();
	for (t, track) in recording.tracks.iter().enumerate() {
		let mut units = Vec::new();
		let mut gaps = 0;
		let mut skipping = false;
		// The group a subscription made at the join starts at: the newest begun by then.
		let mut joined = 0;
		for (g, (_, frames)) in track.groups.iter().enumerate() {
			let keyframe = micros(&frames[0]);
			let mut kept = 0;
			for (f, frame) in frames.iter().enumerate() {
				let at = micros(frame);
				let nominal = match track.video {
					// A group's pictures go out in decode order, a frame apart.
					true => Duration::from_micros(keyframe - origin) + FRAME * f as u32,
					false => Duration::from_micros(at - origin),
				};
				let delivery = schedule(Slot {
					video: track.video,
					group: g,
					frame: f,
					nominal,
				});
				let on_time = match delivery {
					Delivery::At(arrival) => {
						let late = arrival.saturating_sub(nominal) + LAG;
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
					if f == 0 && arrival + LAG <= join {
						joined = g;
					}
					kept += 1;
					events.push((arrival + LAG, t, g, Some(f)));
				}
				units.push(Unit {
					group: g,
					pts: ticks(at),
					leading: track.video && at < keyframe,
					delivered: delivery != Delivery::Omitted,
					on_time,
					output: false,
				});
			}
			let whole = kept == frames.len();
			if whole {
				let last = events.last().map(|(at, ..)| *at).unwrap();
				events.push((last, t, g, None));
			} else if kept > 0 {
				let next = Duration::from_micros(keyframe - origin) + FRAME * kept as u32;
				events.push((next + LAG, t, g, None));
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
		// What came before the join was never offered to the export.
		for unit in units.iter_mut().filter(|unit| unit.group < joined) {
			unit.delivered = false;
		}
		ledgers.push(Ledger {
			video: track.video,
			units,
			gaps,
		});
	}
	// Stable, so a group's end follows its last frame.
	events.sort_by_key(|&(at, ..)| at);

	// The republisher: the same catalog and tracks on a new broadcast, each frame at its time.
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let snapshot = recording.catalog.clone();
	catalog
		.mutate(|catalog| {
			catalog.video = snapshot.video;
			catalog.audio = snapshot.audio;
		})
		.unwrap();
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
		let _keep = (&broadcast, &catalog);
		let mut open: Vec<Option<moq_net::group::Producer>> = producers.iter().map(|_| None).collect();
		let mut kept = vec![0; producers.len()];
		for (at, t, g, step) in events {
			tokio::time::sleep_until(start + at).await;
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
			group
				.write_frame(frames[f].timestamp, frames[f].payload.clone())
				.unwrap();
			kept[t] += 1;
		}
		assert!(open.iter().all(Option::is_none), "every group ends");
		for producer in &producers {
			producer.finish().unwrap();
		}
		std::future::pending::<()>().await;
	});

	tokio::time::sleep_until(start + join).await;
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
			let matched: Vec<usize> = (0..ledger.units.len())
				.filter(|&i| ledger.units[i].pts.abs_diff(pts) <= 1)
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
		for (i, unit) in units.iter().enumerate() {
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
		while i < units.len() && units[i].group == group {
			let unit = &units[i];
			if unit.leading && !whole {
				if unit.output {
					return Err(format!("video unit {i} went out without the group before it: {unit:?}"));
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
		for (i, unit) in units.iter().enumerate() {
			if unit.output && (!unit.delivered || unit.on_time == Some(false)) {
				return Err(format!("{name} unit {i} went out, though {unit:?}"));
			}
		}
		let chained = chain(ledger)?;

		// The export starts on the video's first group, with the audio presented from its
		// keyframe on; audio straddling the keyframe may be trimmed with what precedes it.
		for (i, unit) in units.iter().enumerate() {
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
		for (i, unit) in units.iter().enumerate() {
			if unit.delivered && !unit.output {
				*if i < first { &mut before } else { &mut after } += 1;
				late += u64::from(unit.on_time != Some(true));
			}
		}
		gaps += ledger.gaps;
	}

	// The source skips frames that came older than its age budget, each run one more gap, and
	// they are neither trimmed nor dropped.
	let skips = stats
		.gaps
		.checked_sub(gaps)
		.ok_or(format!("{} gaps reported for the {gaps} the schedule made", stats.gaps))?;
	let counted = stats.dropped + stats.trimmed;
	let skipped = (before + after).checked_sub(counted).ok_or(format!(
		"{counted} frames counted dropped or trimmed, but only {} are missing",
		before + after
	))?;
	let exact = (stats.dropped, stats.trimmed) == (after, before);
	if !(exact || (skipped > 0 && stats.dropped <= after && stats.trimmed <= before))
		|| (skips > 0) != (skipped > 0)
		|| skips > skipped
		|| skipped > late
	{
		return Err(format!(
			"(dropped, trimmed, gaps) reported {:?}; {before} missing before each track started, \
			 {after} after, {late} of them late, and {gaps} gaps scheduled",
			(stats.dropped, stats.trimmed, stats.gaps)
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
/// logs.
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
		let source = decode(profile.clip(), false).unwrap();
		// The import moved the source onto the catalog clock; the first pictures give the shift.
		let earliest = |pts: &mut dyn Iterator<Item = u64>| pts.min_by_key(|pts| (pts + WRAP / 2) % WRAP).unwrap();
		let shift = earliest(&mut source.pictures.iter().map(|(pts, _)| *pts)) + WRAP
			- earliest(&mut video.units.iter().map(|unit| unit.pts));
		let pictures = source.pictures.into_iter();
		Self(
			pictures
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

/// The video from each point a receiver acquires at, as a fresh decoder's input: its first
/// keyframe out, and each one it resumes at after a group did not go out whole. Each segment is
/// the output's leading tables and its packets up to the next, without the audio, beside the
/// pictures it carries.
fn segments(run: &Run) -> Vec<(Vec<u8>, Vec<&Unit>)> {
	let ts = run.ts();
	let packets = ts.as_chunks::<188>().0;
	let (video, audio) = (run.pid(Class::Video), run.pid(Class::Audio));
	let pid = |packet: &[u8; 188]| u16::from_be_bytes([packet[1] & 0x1f, packet[2]]);
	// The packet each picture's PES starts at, by its PTS.
	let starts: Vec<(usize, u64)> = (packets.iter().enumerate())
		.filter(|(_, packet)| pid(packet) == video && packet[1] & 0x40 != 0)
		.map(|(at, packet)| {
			let adaptation = match packet[3] & 0x20 {
				0 => 0,
				_ => usize::from(packet[4]) + 1,
			};
			let pes = &packet[4 + adaptation..];
			let pts = u64::from((pes[9] >> 1) & 7) << 30
				| u64::from(pes[10]) << 22
				| u64::from(pes[11] >> 1) << 15
				| u64::from(pes[12]) << 7
				| u64::from(pes[13] >> 1);
			(at, pts)
		})
		.collect();

	// Where each segment starts, and its pictures.
	let mut cuts: Vec<(usize, Vec<&Unit>)> = Vec::new();
	let mut whole = false;
	for group in run.ledger(true).units.chunk_by(|a, b| a.group == b.group) {
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
		whole = group.iter().all(|unit| unit.output || (unit.leading && !whole));
	}

	let tables = packets[..cuts[0].0].iter().filter(|packet| pid(packet) != audio);
	let tables: Vec<u8> = tables.flatten().copied().collect();
	let ends: Vec<usize> = cuts.iter().skip(1).map(|(at, _)| *at).chain([packets.len()]).collect();
	(cuts.into_iter().zip(ends))
		.map(|((at, units), end)| {
			let mut bytes = tables.clone();
			bytes.extend(packets[at..end].iter().filter(|packet| pid(packet) != audio).flatten());
			(bytes, units)
		})
		.collect()
}

/// Republish `profile`'s clip under `schedule`, export it from `join` on, and grade the export
/// and its decode: from each point a receiver acquires at, and as a whole when the profile's
/// resumptions are decodable straight through.
async fn recover(profile: Profile, schedule: Schedule, join: Duration) -> Run {
	let recording = record(profile.clip()).await;
	let run = run(&recording, schedule, join).await;
	grade(&run.ledgers, &run.stats).unwrap();
	assert!(assert_on_time(&run.frames) > 200, "too little output to judge");
	assert_continuous(&run.ts());
	assert_eq!(count_discontinuity(&run.frames), 0, "a loss is not a new program clock");

	let reference = Reference::new(profile, run.ledger(true));
	for (i, (ts, emitted)) in segments(&run).iter().enumerate() {
		let output = decode(ts, true).unwrap_or_else(|err| panic!("segment {i}: {err}"));
		(reference.compare(&output, emitted)).unwrap_or_else(|err| panic!("segment {i}: {err}"));
	}
	if profile != Profile::Open {
		let output = decode(&run.ts(), false).unwrap();
		let emitted: Vec<&Unit> = run.ledger(true).units.iter().filter(|unit| unit.output).collect();
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

#[tokio::test(start_paused = true)]
async fn a_clean_path_loses_nothing() {
	let run = recover(Profile::Closed, clean, Duration::ZERO).await;
	let video = run.ledger(true);
	assert_eq!(out(video), video.units.len(), "every picture goes out");
	assert_eq!((run.stats.dropped, run.stats.gaps), (0, 0));
}

/// A group the path never delivers is skipped whole: the group before keeps its tail, though
/// nothing after it settles their decode times until the hole is past, and the export resumes
/// on the next keyframe.
#[tokio::test(start_paused = true)]
async fn a_skipped_group_resumes_at_the_next() {
	let run = recover(
		Profile::Closed,
		|slot| match slot.video && slot.group == 5 {
			true => Delivery::Omitted,
			false => clean(slot),
		},
		Duration::ZERO,
	)
	.await;
	assert_eq!((run.stats.dropped, run.stats.gaps), (0, 1));
}

/// A group cut short keeps the pictures it delivered, though they are fewer than it takes to
/// settle their decode times, and loses only its tail.
#[tokio::test(start_paused = true)]
async fn a_cut_group_keeps_what_it_delivered() {
	let run = recover(
		Profile::Closed,
		|slot| match slot.video && slot.group == 5 && slot.frame >= 4 {
			true => Delivery::Omitted,
			false => clean(slot),
		},
		Duration::ZERO,
	)
	.await;
	let video = run.ledger(true);
	let kept = video.units.iter().filter(|unit| unit.group == 5 && unit.output).count();
	assert_eq!(kept, 4, "the cut group's delivered pictures go out");
	assert_eq!((run.stats.dropped, run.stats.gaps), (0, 1));
}

/// The video stalls for 2.5 s while the audio runs on, then delivers what it held at once: the
/// source skips the groups it held past the age budget as one gap, nothing late goes out, and the
/// video resumes on the first keyframe delivered in time, on the same clock.
#[tokio::test(start_paused = true)]
async fn a_stall_drops_what_it_made_late() {
	let run = recover(Profile::Closed, |slot| hold(slot.video, slot), Duration::ZERO).await;
	let (video, audio) = (run.ledger(true), run.ledger(false));
	assert!(
		video.units.iter().any(|unit| unit.on_time == Some(false)),
		"the schedule made pictures late"
	);
	assert_eq!((run.stats.dropped, run.stats.gaps), (0, 1));
	let first = audio.units.iter().position(|unit| unit.output).unwrap();
	assert!(
		audio.units[first..].iter().all(|unit| unit.output),
		"the audio runs on through the stall"
	);
}

/// The whole path stalls, so nothing wakes the export until it delivers what it held. The export
/// judges a frame as arriving when its source was last found empty, which is when the export last
/// ran, so what the stall held behind the deadline still goes out, late.
#[tokio::test(start_paused = true)]
#[ignore = "an export nothing wakes judges a frame on when it last ran (quest/m1/ts-reliability/recovery.md)"]
async fn a_path_stall_drops_what_it_made_late() {
	recover(Profile::Closed, |slot| hold(true, slot), Duration::ZERO).await;
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
	const FROM: Duration = Duration::from_millis(2000);
	const UNTIL: Duration = Duration::from_millis(3500);
	let run = recover(
		Profile::Closed,
		|slot| match !slot.video && (FROM..UNTIL).contains(&slot.nominal) {
			true => Delivery::Omitted,
			false => clean(slot),
		},
		Duration::ZERO,
	)
	.await;
	let video = run.ledger(true);
	assert_eq!(out(video), video.units.len(), "every picture goes out");
	assert_eq!((run.stats.dropped, run.stats.gaps), (0, 1));

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

/// Audio sent 200 ms behind the video, within the delay, survives a skipped video group whole.
#[tokio::test(start_paused = true)]
async fn skewed_audio_survives_a_video_skip() {
	let run = recover(
		Profile::Closed,
		|slot| match (slot.video, slot.group) {
			(true, 5) => Delivery::Omitted,
			(true, _) => clean(slot),
			(false, _) => Delivery::At(slot.nominal + Duration::from_millis(200)),
		},
		Duration::ZERO,
	)
	.await;
	assert_eq!((run.stats.dropped, run.stats.gaps), (0, 1));
}

/// A skipped group around the 33-bit wrap keeps the clock, and the pictures either side decode.
#[tokio::test(start_paused = true)]
async fn a_skip_across_the_wrap_keeps_the_clock() {
	let run = recover(
		Profile::Wrap,
		|slot| match slot.video && slot.group == 5 {
			true => Delivery::Omitted,
			false => clean(slot),
		},
		Duration::ZERO,
	)
	.await;
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
	assert_eq!((run.stats.dropped, run.stats.gaps), (0, 1));
}

/// Open GOP: a skipped group and a cut one each orphan the next group's leading pictures, which
/// are trimmed, while every group after a whole one keeps its own.
#[tokio::test(start_paused = true)]
async fn open_gop_trims_only_the_orphans() {
	let run = recover(
		Profile::Open,
		|slot| match slot.video && (slot.group == 5 || (slot.group == 8 && slot.frame >= 6)) {
			true => Delivery::Omitted,
			false => clean(slot),
		},
		Duration::ZERO,
	)
	.await;
	let video = run.ledger(true);
	let leading = |group: usize| {
		let units: Vec<&Unit> = video
			.units
			.iter()
			.filter(|unit| unit.group == group && unit.leading)
			.collect();
		assert!(!units.is_empty(), "group {group} has leading pictures");
		units.iter().filter(|unit| unit.output).count()
	};
	assert_eq!((leading(6), leading(9)), (0, 0), "the orphans are trimmed");
	assert!(leading(7) > 0 && leading(10) > 0, "a continuous group keeps them");
	assert_eq!(run.stats.gaps, 2);
}

/// A subscriber joining 2.3 s in starts on a whole group and loses nothing after it.
#[tokio::test(start_paused = true)]
async fn a_join_starts_on_a_whole_group() {
	let run = recover(Profile::Closed, clean, Duration::from_millis(2300)).await;
	let video = run.ledger(true);
	let first = video.units.iter().find(|unit| unit.output).unwrap().group;
	assert!(first >= 4, "the export starts at the live edge, group {first}");
	assert_eq!((run.stats.dropped, run.stats.gaps), (0, 0));
}

/// The graders fail what they should: a picture that went out after its chain broke, a group
/// that survived but did not go out, a loss the counters do not show, and a picture FFmpeg
/// cannot decode as the source's.
#[tokio::test(start_paused = true)]
async fn the_graders_fail_what_they_should() {
	let recording = record(Profile::Closed.clip()).await;
	let run = run(
		&recording,
		|slot| match slot.video && slot.group == 5 {
			true => Delivery::Omitted,
			false => clean(slot),
		},
		Duration::ZERO,
	)
	.await;
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
		stats.dropped += 1;
	});
	assert!(err.contains("chain broke"), "{err}");
	let err = edited(&|ledgers, stats| {
		for frame in 0..12 {
			ledgers[video].units[unit(7, frame)].output = false;
		}
		stats.dropped += 12;
	});
	assert!(err.contains("did not go out"), "{err}");
	let err = edited(&|_, stats| stats.dropped += 1);
	assert!(err.contains("dropped"), "{err}");
	let err = edited(&|_, stats| stats.gaps = 0);
	assert!(err.contains("gaps reported"), "{err}");

	// The output less one P picture, with the ledger agreeing it never went out: FFmpeg must
	// refuse what follows it, or decode it as other than the source's.
	let reference = Reference::new(Profile::Closed, run.ledger(true));
	let pid = run.pid(Class::Video);
	let target = unit(3, 1);
	let ts = run.ts();
	let mut starts = ts
		.as_chunks::<188>()
		.0
		.iter()
		.enumerate()
		.filter(|(_, packet)| u16::from_be_bytes([packet[1] & 0x1f, packet[2]]) == pid && packet[1] & 0x40 != 0)
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
