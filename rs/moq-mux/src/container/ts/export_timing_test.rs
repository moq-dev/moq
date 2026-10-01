//! Timing of a delayed [`Export`] on a live clock: what a receiver renders when it joins a
//! broadcast already running, when one track loses frames, when the source's clock runs off the
//! receiver's, and when two receivers render one broadcast for a 1+1 (SMPTE ST 2022-7) pair.
//!
//! Every case runs on the paused clock, with each frame written at the instant a live source
//! would send it. A receiver's view is measured from its own output: the lead of each PES (its
//! decode time less the PCR of the slot carrying it) and the instant each slot went out.

use std::io::Cursor;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use hang::catalog::{AAC, AudioConfig, Container, H264, VideoConfig};
use moq_net::Timestamp;
use mpeg2ts::ts::{ReadTsPacket, TsPacketReader, TsPayload};
use tokio::time::Instant;

use crate::catalog::hang::Container as HangContainer;
use crate::container::ts::Export;
use crate::container::ts::export::PCR_INTERVAL;
use crate::container::ts::export::Stats;
use crate::container::{Container as _, Frame, Producer};

const SPS: &[u8] = &[0x67, 0x42, 0xc0, 0x1f, 0xde];
const PPS: &[u8] = &[0x68, 0xce, 0x3c, 0x80];

/// 25 fps video.
const VIDEO_US: u64 = 40_000;
/// 48 kHz AAC, 1024 samples per frame.
const AUDIO_US: u64 = 21_333;
/// Video frames per group: 600 ms.
const GOP: u64 = 15;
/// Audio frames per group, roughly matching the video group duration.
const AUDIO_GROUP: u64 = 28;

const DELAY: Duration = Duration::from_millis(500);
/// 34 packets in every 25 ms slot: room for the first slot's keyframe and tables, which have no
/// earlier slot to spread into, and a whole number of packets per slot, so that only
/// [`two_legs_pad_the_same_slots_at_a_fractional_rate`] depends on how a slot's share of a
/// fractional packet is carried.
const MUX_RATE: u64 = 34 * 188 * 8 * 40;

/// The tick a late receiver joins at: eight frames into a group, so its subscription starts
/// with 320 ms of that group already published and delivered at once.
const JOIN: u64 = 3 * GOP + 8;

const VIDEO_PID: u16 = 0x1001;
const AUDIO_PID: u16 = 0x1002;

fn length_prefixed(nals: &[&[u8]]) -> Bytes {
	let mut out = BytesMut::new();
	for nal in nals {
		out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
		out.extend_from_slice(nal);
	}
	out.freeze()
}

/// A live H.264 + AAC broadcast whose frames are written on the source's clock.
struct Live {
	_broadcast: moq_net::broadcast::Producer,
	_catalog: crate::catalog::Producer,
	/// Written group by group, so a group can be left open: see [`Self::run`].
	video: moq_net::track::Producer,
	group: Option<moq_net::group::Producer>,
	/// Whether the open group has stalled, and the stalled groups, kept open.
	stalling: bool,
	stalled: Vec<moq_net::group::Producer>,
	audio: Producer<HangContainer>,
	source: crate::Source,
	/// Media time per unit of the receivers' clock: above 1 the source runs fast.
	scale: f64,
	/// The multiplex rate the receivers pad to.
	rate: u64,
	/// How much later than its video the source sends audio of the same media time, in µs;
	/// negative sends the video later. A TS source sends video well ahead of its decode time
	/// and audio just in time, so its audio lags.
	audio_lag: i64,
	start: Instant,
	tick: u64,
	audio_index: u64,
}

impl Live {
	fn new(scale: f64, audio_lag: i64) -> Self {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let mut catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();

		let track = broadcast
			.create_track(
				broadcast.unique_name(".avc1"),
				hang::container::track_info(hang::catalog::PRIORITY.video),
			)
			.unwrap();
		let mut cfg = VideoConfig::new(H264 {
			profile: 0x42,
			constraints: 0xc0,
			level: 0x1f,
			inline: false,
		});
		cfg.container = Container::Legacy;
		cfg.description =
			Some(crate::codec::h264::build_avcc(&[Bytes::from_static(SPS)], &[Bytes::from_static(PPS)]).unwrap());
		catalog
			.modify()
			.unwrap()
			.video
			.renditions
			.insert(track.name().to_string(), cfg);
		let video = track;

		let track = broadcast
			.create_track(
				broadcast.unique_name(".aac"),
				hang::container::track_info(hang::catalog::PRIORITY.audio),
			)
			.unwrap();
		let mut cfg = AudioConfig::new(AAC { profile: 2 }, 48_000, 2);
		cfg.container = Container::Legacy;
		catalog
			.modify()
			.unwrap()
			.audio
			.renditions
			.insert(track.name().to_string(), cfg);
		let audio = Producer::new(track, HangContainer::Legacy(crate::container::Kind::Audio));

		Self {
			source: crate::source::announced(&consumer),
			_broadcast: broadcast,
			_catalog: catalog,
			video,
			group: None,
			stalling: false,
			stalled: Vec::new(),
			audio,
			scale,
			rate: MUX_RATE,
			audio_lag,
			start: Instant::now(),
			tick: 0,
			audio_index: 0,
		}
	}

	/// The receivers' instant at which the source's clock reads media time `micros`.
	fn sent(&self, micros: u64) -> Instant {
		self.start + Duration::from_secs_f64(micros as f64 / 1e6 / self.scale)
	}

	/// When the source sends the video frame of media time `video` and the audio frame of
	/// `audio`: the track that lags is sent late, the other on the source's clock.
	fn send_times(&self, video: u64, audio: u64) -> (Instant, Instant) {
		let lag = Duration::from_micros(self.audio_lag.unsigned_abs());
		let (v, a) = match self.audio_lag >= 0 {
			true => (Duration::ZERO, lag),
			false => (lag, Duration::ZERO),
		};
		(self.sent(video) + v, self.sent(audio) + a)
	}

	/// A receiver joining now.
	async fn join(&self, every: Duration) -> Leg {
		let export = Export::new(self.source.clone())
			.await
			.unwrap()
			.with_delay(DELAY)
			.with_mux_rate(self.rate);
		Leg {
			export,
			every,
			next_poll: Instant::now(),
			out: Vec::new(),
		}
	}

	/// Send every frame before video tick `until`, each at its instant on the source's clock,
	/// polling the legs as they would poll on their own.
	///
	/// A video tick `stall` accepts is never sent, and its group is left open: its tail is
	/// still on its way when the next group starts, as when the relay's backlog for a joiner
	/// arrives behind the live edge. The subscription then skips it as slow once the next
	/// group is the delay newer, and counts a discontinuity.
	async fn run(&mut self, until: u64, legs: &mut [&mut Leg], stall: impl Fn(u64) -> bool) {
		while self.tick < until {
			let video = self.tick * VIDEO_US;
			let audio = self.audio_index * AUDIO_US;
			let (video_at, audio_at) = self.send_times(video, audio);
			advance(video_at.min(audio_at), legs).await;
			if audio_at < video_at {
				let index = self.audio_index;
				self.audio
					.write(Frame {
						timestamp: Timestamp::from_micros(audio).unwrap(),
						duration: None,
						payload: Bytes::from_iter((0..180u16).map(|i| (i ^ index as u16) as u8)),
						keyframe: index.is_multiple_of(AUDIO_GROUP),
					})
					.unwrap();
				self.audio_index += 1;
			} else {
				let tick = self.tick;
				let keyframe = tick.is_multiple_of(GOP);
				if keyframe && let Some(group) = self.group.take() {
					match std::mem::take(&mut self.stalling) {
						true => self.stalled.push(group),
						false => group.finish().unwrap(),
					}
				}
				if keyframe {
					self.group = Some(self.video.append_group().unwrap());
				}
				if stall(tick) {
					self.stalling = true;
				} else if !self.stalling {
					let slice = if keyframe {
						vec![0x65u8; 3_000]
					} else {
						vec![0x41u8; 400]
					};
					let group = self.group.as_mut().expect("the first tick is a keyframe");
					HangContainer::Legacy(crate::container::Kind::Video)
						.write(
							group,
							&[Frame {
								timestamp: Timestamp::from_micros(video).unwrap(),
								duration: None,
								payload: length_prefixed(&[&slice]),
								keyframe,
							}],
						)
						.unwrap();
				}
				self.tick += 1;
			}
			for leg in legs.iter_mut() {
				leg.poll_now();
			}
		}
	}

	/// End the broadcast and let every leg render what it holds.
	async fn finish(&mut self, legs: &mut [&mut Leg]) {
		if let Some(group) = self.group.take() {
			group.finish().unwrap();
		}
		self.stalled.clear();
		self.video.finish().unwrap();
		self.audio.finish().unwrap();
		for leg in legs.iter_mut() {
			leg.drain().await;
		}
	}
}

/// One receiver: an exporter and what it rendered, with the instant each slot went out.
struct Leg {
	export: Export,
	/// How often it reads its subscription. Zero reads after every frame the source sends; a
	/// coarser cadence stands for a receiver behind a burstier network or scheduler.
	every: Duration,
	next_poll: Instant,
	out: Vec<(Instant, Frame)>,
}

impl Leg {
	/// When this leg next reads on its own.
	fn wake(&self) -> Option<Instant> {
		match self.every.is_zero() {
			true => self.export.next_due(),
			false => Some(self.next_poll),
		}
	}

	fn poll_now(&mut self) {
		if !self.every.is_zero() {
			return;
		}
		self.poll();
	}

	fn poll(&mut self) {
		let waiter = kio::Waiter::noop();
		let now = Instant::now();
		while let std::task::Poll::Ready(frame) = self.export.poll_next(&waiter) {
			match frame.expect("exporter error") {
				Some(frame) => self.out.push((now, frame)),
				None => break,
			}
		}
	}

	/// Read to the end of the broadcast.
	async fn drain(&mut self) {
		loop {
			self.poll();
			if let Some(deadline) = self.export.next_due() {
				tokio::time::sleep_until(deadline).await;
				continue;
			}
			match tokio::time::timeout(Duration::from_millis(10), self.export.next()).await {
				Ok(Ok(Some(frame))) => self.out.push((Instant::now(), frame)),
				Ok(Ok(None)) => break,
				Err(_) if self.export.next_due().is_none() => break,
				Err(_) => {}
				Ok(Err(err)) => panic!("exporter error: {err}"),
			}
		}
	}

	/// The PES timing this leg rendered: one entry per PES start.
	fn units(&self) -> Vec<Unit> {
		units(&self.out)
	}
}

/// Run the clock to `at`, letting every leg read whenever it would have on its own.
async fn advance(at: Instant, legs: &mut [&mut Leg]) {
	while let Some(wake) = legs
		.iter()
		.filter_map(|leg| leg.wake())
		.filter(|wake| *wake <= at)
		.min()
	{
		tokio::time::sleep_until(wake).await;
		let now = Instant::now();
		for leg in legs.iter_mut() {
			if leg.wake().is_some_and(|wake| wake <= now) {
				leg.poll();
				if !leg.every.is_zero() {
					while leg.next_poll <= now {
						leg.next_poll += leg.every;
					}
				}
			}
		}
	}
	tokio::time::sleep_until(at).await;
	// Read up to the instant the next frame arrives, so the arrival is timed exactly.
	for leg in legs.iter_mut() {
		leg.poll_now();
	}
}

/// A PES start as a receiver sees it.
#[derive(Debug, Clone, Copy)]
struct Unit {
	pid: u16,
	/// Decode time (DTS, else PTS), 90 kHz, as carried.
	decode: u64,
	/// Decode time less the PCR of the slot carrying the PES start, in milliseconds: how long
	/// the unit waits in the receiver's buffer after its first byte arrives.
	lead: f64,
	/// When the slot carrying it went out.
	sent: Instant,
}

const WRAP: i64 = 1 << 33;

/// Signed difference of two 33-bit 90 kHz values.
fn diff90(a: u64, b: u64) -> i64 {
	let d = (a as i64 - b as i64).rem_euclid(WRAP);
	if d >= WRAP / 2 { d - WRAP } else { d }
}

fn units(out: &[(Instant, Frame)]) -> Vec<Unit> {
	// One reader across the slots, since it needs the tables an earlier slot carried.
	let bytes: Vec<u8> = out
		.iter()
		.flat_map(|(_, frame)| frame.payload.iter().copied())
		.collect();
	let slot_of: Vec<Instant> = out
		.iter()
		.flat_map(|(sent, frame)| std::iter::repeat_n(*sent, frame.payload.len() / 188))
		.collect();
	let mut reader = TsPacketReader::new(Cursor::new(bytes));
	let mut units = Vec::new();
	let mut pcr: Option<u64> = None;
	for sent in &slot_of {
		let packet = reader.read_ts_packet().unwrap().expect("a packet per slot entry");
		if let Some(clock) = packet.adaptation_field.as_ref().and_then(|af| af.pcr) {
			pcr = Some(clock.as_u64() / 300);
		}
		if let Some(TsPayload::PesStart(pes)) = &packet.payload
			&& let Some(pts) = pes.header.pts
			&& let Some(pcr) = pcr
		{
			let decode = pes.header.dts.unwrap_or(pts).as_u64();
			units.push(Unit {
				pid: packet.header.pid.as_u16(),
				decode,
				lead: diff90(decode, pcr) as f64 / 90.0,
				sent: *sent,
			});
		}
	}
	units
}

fn median(mut values: Vec<f64>) -> f64 {
	assert!(!values.is_empty(), "no units to take a median of");
	values.sort_by(f64::total_cmp);
	values[values.len() / 2]
}

/// The median lead of one PID's units decoding at or after `from` (90 kHz).
fn lead(units: &[Unit], pid: u16, from: u64) -> f64 {
	median(
		units
			.iter()
			.filter(|unit| unit.pid == pid && diff90(unit.decode, from) >= 0)
			.map(|unit| unit.lead)
			.collect(),
	)
}

/// The median of (slot sent − decode time) over one PID's units decoding at or after `from`
/// (90 kHz), in milliseconds against an arbitrary origin: when the receiver releases a unit
/// against when it decodes.
fn release(units: &[Unit], pid: u16, from: u64) -> f64 {
	let origin = units[0].sent;
	median(
		units
			.iter()
			.filter(|unit| unit.pid == pid && diff90(unit.decode, from) >= 0)
			.map(|unit| unit.sent.duration_since(origin).as_secs_f64() * 1e3 - unit.decode as f64 / 90.0)
			.collect(),
	)
}

/// How much later than the audio the receiver releases video of the same decode time, in
/// milliseconds, over units decoding at or after `from` (90 kHz). Zero, to a slot, on one clock.
fn video_behind_audio(units: &[Unit], from: u64) -> f64 {
	release(units, VIDEO_PID, from) - release(units, AUDIO_PID, from)
}

/// The decode time `micros` after the leg's first video unit, 90 kHz.
fn after_first_video(units: &[Unit], micros: u64) -> u64 {
	let first = units.iter().find(|unit| unit.pid == VIDEO_PID).expect("a video unit");
	(first.decode + at90(micros)) % WRAP as u64
}

/// `micros` in 90 kHz units.
fn at90(micros: u64) -> u64 {
	micros * 9 / 100
}

const SLOT_MS: f64 = PCR_INTERVAL.as_millis() as f64;

// A receiver that joins mid-group is handed that group's published frames at once, so its clock
// starts behind the live edge. When its subscription then skips a slow group on one track, that
// track must stay on the others' clock. In the field it did not: the skipped video opened a
// generation of its own and ran 200 ms behind, a whole delay behind or 1.2 s ahead of the audio
// depending on the join, fixed for the run, and in the last two the decoder buffers of whichever
// track ran behind underflowed.
//
// The split needs the tracks to be sent out of step, as a TS source's are (video well ahead of
// its decode time, audio just in time); sent in step, the new generation's anchor is clamped onto
// the old clock and nothing shows. So each case runs with each track lagging in turn.

/// Send lags, in µs, each case runs with: audio late, then video late.
const LAGS: [i64; 2] = [300_000, -300_000];

/// How far apart, on one clock, two runs' video-against-audio release may sit: a slot either way
/// for the grid, so a rewind that lands the video one slot over still passes.
const CLOCK_SLACK: f64 = 2.0 * SLOT_MS;

/// Join mid-group with `lag`, render 7.2 s with the video ticks `stall` accepts stalled, and
/// return the leg's units.
async fn join(lag: i64, stall: impl Fn(u64) -> bool) -> Vec<Unit> {
	let mut live = Live::new(1.0, lag);
	live.run(JOIN, &mut [], |_| false).await;
	let mut leg = live.join(Duration::ZERO).await;
	live.run(24 * GOP, &mut [&mut leg], stall).await;
	assert_eq!(leg.export.dropped(), 0, "every frame arrived inside its deadline");
	leg.units()
}

/// Assert a skip left the video where it is without one, against the audio, from 4 s after the
/// leg's first video unit.
async fn assert_one_clock(stall: impl Fn(u64) -> bool + Copy) {
	let mut moved = Vec::new();
	for lag in LAGS {
		let (clean, skipped) = (join(lag, |_| false).await, join(lag, stall).await);
		let from = after_first_video(&clean, 4_000_000);
		let (want, got) = (video_behind_audio(&clean, from), video_behind_audio(&skipped, from));
		if (got - want).abs() > CLOCK_SLACK {
			moved.push(format!(
				"{} sent {} ms late: video released {:+.1} ms against audio of the same decode time, \
				 {:+.1} ms without the skip (PCR lead video {:.1} ms, audio {:.1} ms)",
				if lag > 0 { "audio" } else { "video" },
				lag.abs() / 1_000,
				got,
				want,
				lead(&skipped, VIDEO_PID, from),
				lead(&skipped, AUDIO_PID, from),
			));
		}
	}
	assert!(
		moved.is_empty(),
		"the skip put the video on its own clock:\n{}",
		moved.join("\n")
	);
}

/// The joined group stalls straight after the join, before anything has been released: its
/// tail never arrives, and the subscription skips to the next group.
#[tokio::test(start_paused = true)]
async fn a_skip_at_the_join_keeps_one_clock() {
	assert_one_clock(|tick| (JOIN..4 * GOP).contains(&tick)).await;
}

/// The same skip well after the first release: half a group of video stalls mid-stream.
#[tokio::test(start_paused = true)]
async fn a_skip_while_running_keeps_one_clock() {
	assert_one_clock(|tick| (6 * GOP + 7..7 * GOP).contains(&tick)).await;
}

// A source's clock is never the receiver's. A transport stream's 27 MHz may be off by 30 ppm,
// which walks a fixed anchor 108 ms an hour: a source running slow makes every frame late in
// turn once the walk passes the delay, and one running fast makes the buffer grow without bound.
// The output's clock may follow it only as fast as 13818-1 lets a system clock change, so
// reaching a source 30 ppm off takes hours (the jitter buffer's own tests run a day); these
// cases run five minutes of a source at the limit, long enough to measure its rate.

/// How far the instant each slot goes out may wander against the source over a run: what
/// reaching a source at the limit costs under the slew limit (30 ppm squared over twice
/// 0.075 Hz/s of 27 MHz), and a slot.
const DRIFT_SLACK: Duration = Duration::from_millis(162 + 25);

/// Five minutes of media.
const DRIFT_TICKS: u64 = 300_000_000 / VIDEO_US;

/// The spread of (slot sent − source's clock at the slot) after the first 10 s over `ticks`
/// of media, the drops, and the export's stats at the end.
async fn on_a_scaled_clock(scale: f64, ticks: u64) -> (Duration, u64, Stats) {
	let mut live = Live::new(scale, 0);
	let mut leg = live.join(Duration::ZERO).await;
	live.run(ticks, &mut [&mut leg], |_| false).await;

	let first = leg.out.first().expect("output").1.timestamp.as_micros() as u64;
	let held: Vec<Duration> = leg
		.out
		.iter()
		.filter(|(_, frame)| frame.timestamp.as_micros() as u64 >= first + 10_000_000)
		.map(|(sent, frame)| sent.saturating_duration_since(live.sent(frame.timestamp.as_micros() as u64)))
		.collect();
	assert!(held.len() > 1_000, "too little output to judge: {}", held.len());
	let spread = *held.iter().max().unwrap() - *held.iter().min().unwrap();
	(spread, leg.export.dropped(), leg.export.stats())
}

#[tokio::test(start_paused = true)]
async fn a_slow_source_clock_keeps_the_delay() {
	let (spread, dropped, stats) = on_a_scaled_clock(1.0 - 30e-6, DRIFT_TICKS).await;
	assert_eq!(dropped, 0, "frames went late as the source fell behind the anchor");
	assert!(spread <= DRIFT_SLACK, "the delay walked by {spread:?}");
	let drift = stats.drift.expect("a measurement");
	assert!((drift + 30.0).abs() < 0.5, "measured {drift} ppm");
}

#[tokio::test(start_paused = true)]
async fn a_fast_source_clock_keeps_the_delay() {
	let (spread, dropped, stats) = on_a_scaled_clock(1.0 + 30e-6, DRIFT_TICKS).await;
	assert_eq!(dropped, 0);
	assert!(spread <= DRIFT_SLACK, "the delay walked by {spread:?}");
	let drift = stats.drift.expect("a measurement");
	assert!((drift - 30.0).abs() < 0.5, "measured {drift} ppm");
}

/// A source 400 ppm off, past what the output's clock may follow, is measured and counted.
#[tokio::test(start_paused = true)]
async fn a_source_past_the_limit_is_counted() {
	let (_, _, stats) = on_a_scaled_clock(1.0004, 300_000_000 / VIDEO_US).await;
	let drift = stats.drift.expect("a measurement");
	assert!((drift - 400.0).abs() < 1.0, "measured {drift} ppm");
	assert!(stats.out_of_tolerance > 0, "not counted");
}

// A 1+1 pair is two receivers of one broadcast whose outputs a seamless switch compares packet
// for packet. They join at different times and read with different cadences, and from the moment
// they overlap they must render the same bytes. The continuity counters are the known exception
// (they number what each process sent); every other byte has to match.

/// Assert two legs rendered the same slots from media time `from` (µs, the slots' own
/// timestamps) to the end, continuity counters aside.
fn assert_same_packets(a: &Leg, b: &Leg, from: u64) {
	let pick = |leg: &Leg| -> Vec<Frame> {
		leg.out
			.iter()
			.map(|(_, frame)| frame.clone())
			.filter(|frame| frame.timestamp.as_micros() as u64 >= from)
			.collect()
	};
	let (a, b) = (pick(a), pick(b));
	assert!(b.len() > 40, "not enough overlap to be worth comparing: {}", b.len());
	assert_eq!(a.len(), b.len(), "the legs rendered a different number of slots");
	for (a, b) in a.iter().zip(&b) {
		assert_eq!(a.timestamp, b.timestamp, "compared slots must be the same slot");
		assert_eq!(
			a.payload.len(),
			b.payload.len(),
			"slot at {:?} rendered to a different size",
			a.timestamp
		);
		for (offset, (x, y)) in a.payload.iter().zip(b.payload.iter()).enumerate() {
			// Only the low nibble of byte 3 is the counter.
			assert!(
				x == y || (offset % 188 == 3 && (x ^ y) & 0xf0 == 0),
				"slot at {:?} differs outside the continuity counter: offset {offset}, {x:#04x} vs {y:#04x}",
				a.timestamp,
			);
		}
	}
}

/// Two legs of one broadcast on `scale` with `lag`: one there from the start and reading after
/// every frame, the other joining mid-group and reading every 100 ms. Both see the same `stall`.
/// Returns them with the slot time 2 s after the second joined, from which they must agree.
async fn pair(scale: f64, lag: i64, ticks: u64, stall: impl Fn(u64) -> bool + Copy) -> (Leg, Leg, u64) {
	pair_at(MUX_RATE, scale, lag, ticks, stall).await
}

async fn pair_at(rate: u64, scale: f64, lag: i64, ticks: u64, stall: impl Fn(u64) -> bool + Copy) -> (Leg, Leg, u64) {
	let mut live = Live::new(scale, lag);
	live.rate = rate;
	let mut a = live.join(Duration::ZERO).await;
	live.run(JOIN, &mut [&mut a], stall).await;
	let mut b = live.join(Duration::from_millis(100)).await;
	live.run(ticks, &mut [&mut a, &mut b], stall).await;
	live.finish(&mut [&mut a, &mut b]).await;
	let joined = b.out.first().expect("the joiner rendered").1.timestamp.as_micros() as u64;
	(a, b, joined + 2_000_000)
}

#[tokio::test(start_paused = true)]
async fn two_legs_render_the_same_packets() {
	for lag in LAGS {
		let (a, b, from) = pair(1.0, lag, 12 * GOP, |_| false).await;
		assert_same_packets(&a, &b, from);
	}
}

/// Most multiplex rates are not a whole number of packets per slot (10 Mb/s is 166.2), so each
/// slot carries 166 or 167. Which slots carry the extra packet has to follow the media grid, not
/// when the leg started.
#[tokio::test(start_paused = true)]
async fn two_legs_pad_the_same_slots_at_a_fractional_rate() {
	let (a, b, from) = pair_at(2_000_000, 1.0, 0, 12 * GOP, |_| false).await;
	assert_same_packets(&a, &b, from);
}

/// A slow group reaches both legs. Both have to skip it the same way.
#[tokio::test(start_paused = true)]
#[ignore = "1+1 (ST 2022-7) packet identity across a skip is deferred to an m2 follow-up"]
async fn two_legs_render_a_skip_the_same_way() {
	for lag in LAGS {
		let (a, b, from) = pair(1.0, lag, 12 * GOP, |tick| (6 * GOP + 7..7 * GOP).contains(&tick)).await;
		assert_same_packets(&a, &b, from);
	}
}

/// Whatever follows the source's clock has to follow it identically on both legs, though they
/// joined at different points on it.
#[tokio::test(start_paused = true)]
async fn two_legs_render_a_drifting_source_the_same_way() {
	for scale in [0.9996, 1.0004] {
		let (a, b, from) = pair(scale, 0, 180_000_000 / VIDEO_US, |_| false).await;
		assert_same_packets(&a, &b, from);
	}
}

// A receiver locks its decoder's clock to the PCR, so the output's system clock is held to
// ISO 13818-1 2.4.2.1: 27 MHz within 810 Hz (30 ppm), changing by at most 0.075 Hz/s. Steering it,
// to follow the source or to wear away a lead, has to stay inside both.

/// A receiver that joins a running broadcast mid-group keeps its output's system clock within
/// 30 ppm of the source's and slews it no faster than 0.075 Hz/s, over 30 s windows of slots.
#[tokio::test(start_paused = true)]
async fn a_joiner_keeps_the_system_clock_in_tolerance() {
	const WINDOW: u64 = 30_000_000;
	let mut live = Live::new(1.0, 0);
	live.run(JOIN, &mut [], |_| false).await;
	let mut leg = live.join(Duration::ZERO).await;
	live.run(JOIN + 300_000_000 / VIDEO_US, &mut [&mut leg], |_| false)
		.await;

	// The instant each slot first went out, by its PCR time.
	let mut first = std::collections::BTreeMap::new();
	for (sent, frame) in &leg.out {
		first.entry(frame.timestamp.as_micros() as u64).or_insert(*sent);
	}
	let slots: Vec<(u64, Instant)> = first.into_iter().collect();
	// (window end in seconds of PCR time, the system clock's offset in ppm)
	let mut rates = Vec::new();
	let mut from = 0;
	for to in 0..slots.len() {
		if slots[to].0 < slots[from].0 + WINDOW {
			continue;
		}
		let pcr = (slots[to].0 - slots[from].0) as f64 / 1e6;
		let wall = (slots[to].1 - slots[from].1).as_secs_f64();
		rates.push((slots[to].0 as f64 / 1e6, (pcr / wall - 1.0) * 1e6));
		from = to;
	}
	assert!(rates.len() >= 8, "too little output to judge: {} windows", rates.len());
	for (at, ppm) in &rates {
		assert!(
			ppm.abs() <= 30.0,
			"the system clock ran {ppm:+.1} ppm off over the 30 s to {at:.0} s"
		);
	}
	for pair in rates.windows(2) {
		let slew = (pair[1].1 - pair[0].1).abs() * 27.0 / (pair[1].0 - pair[0].0);
		assert!(
			slew <= 0.075,
			"the system clock slewed {slew:.3} Hz/s at {:.0} s",
			pair[1].0
		);
	}
}

/// A receiver that joins mid-group runs at the delay from its first output: every slot goes
/// out a fixed lag after the source sent its media (the delay before release, and the delay
/// the schedule sends ahead of the decode time), rather than that plus how old the group it
/// joined on was.
#[tokio::test(start_paused = true)]
async fn a_joiner_runs_at_the_delay_from_its_first_output() {
	let mut live = Live::new(1.0, 0);
	live.run(JOIN, &mut [], |_| false).await;
	let mut leg = live.join(Duration::ZERO).await;
	live.run(JOIN + 10_000_000 / VIDEO_US, &mut [&mut leg], |_| false).await;
	assert_eq!(leg.export.dropped(), 0, "every frame arrived inside its deadline");

	let held: Vec<Duration> = leg
		.out
		.iter()
		.map(|(sent, frame)| sent.saturating_duration_since(live.sent(frame.timestamp.as_micros() as u64)))
		.collect();
	assert!(held.len() > 100, "too little output to judge: {}", held.len());
	let slot = Duration::from_millis(SLOT_MS as u64);
	for (i, held) in held.iter().enumerate() {
		assert!(
			*held >= 2 * DELAY && *held <= 2 * DELAY + 2 * slot,
			"slot {i} went out {held:?} after the source sent it, against a {DELAY:?} delay"
		);
	}
}
