//! SCReAMv2 ([`congestion::Scream`]) on the seeded sim: an encoder that follows the controller's
//! estimate sends up through a bottleneck shaped like each cellular moq-shaper preset, and the run
//! reports the queue it built, what it carried, and how fast it answered each step.
//!
//! The sim models each preset's rates, steps, delay, in-order jitter and queue, and the bursty
//! preset's scheduler grants as periodic slots. `rs/moq-tokio/tests/integration/scream.rs` runs the
//! presets themselves over real sockets. The merge gate runs one preset and the probe's recovery
//! on one seed; the `#[ignore]`d [`compare`] table runs nightly with `just rs quic-sim`, beside
//! BBR3 on the same media for reference.

use std::collections::HashMap;

use super::*;
use crate::congestion::{Bbr3Config, ScreamConfig};

const FPS: u64 = 30;
/// Frames per group, each group on its own stream: a keyframe every two seconds
const GOP: u64 = 60;
/// A keyframe's size, in delta frames
const KEYFRAME: u64 = 5;
/// The encoder's bitrate range, 100 kbit/s to 10 Mbit/s
const MIN_RATE: u64 = 12_500;
const MAX_RATE: u64 = 1_250_000;
/// The most receive timestamps the sender asks for per ACK, past any ACK threshold here
const TIMESTAMPS: u32 = 32;
/// How often the run samples the estimate and the bottleneck's queue
const SAMPLE: Duration = Duration::from_millis(10);
/// Samples before this are the connection starting up, and count for nothing
const WARMUP: Duration = Duration::from_secs(3);
/// The queue delay a step's reaction drains back under: half SCReAMv2's default target, where it
/// starts backing off
const DRAINED: Duration = Duration::from_millis(30);

/// The uplink a run sends through, after one of the cellular moq-shaper presets
#[derive(Debug, Clone)]
struct Uplink {
	name: &'static str,
	/// One-way propagation delay, each way
	latency: Duration,
	/// In-order jitter on the uplink
	jitter: Duration,
	/// The uplink scheduler's grant period
	grant: Option<Duration>,
	/// The uplink's buffer, in time at its first rate
	queue: Duration,
	/// The uplink's rate in bytes per second from each instant on, the first at the start
	rates: Vec<(Duration, u64)>,
	duration: Duration,
}

/// `cellular-step`: 2 Mbit/s behind a 500ms queue, down to 500 kbit/s at ten seconds and back at
/// twenty
fn cellular_step() -> Uplink {
	Uplink {
		name: "cellular-step",
		latency: Duration::from_millis(20),
		jitter: Duration::from_millis(3),
		grant: None,
		queue: Duration::from_millis(500),
		rates: vec![
			(Duration::ZERO, 250_000),
			(Duration::from_secs(10), 62_500),
			(Duration::from_secs(20), 250_000),
		],
		duration: Duration::from_secs(30),
	}
}

/// `cellular-bursty`: 2 Mbit/s arriving in 20ms grants, down to 1 Mbit/s at ten seconds and back
/// at twenty
fn cellular_bursty() -> Uplink {
	Uplink {
		name: "cellular-bursty",
		jitter: Duration::from_millis(5),
		grant: Some(Duration::from_millis(20)),
		rates: vec![
			(Duration::ZERO, 250_000),
			(Duration::from_secs(10), 125_000),
			(Duration::from_secs(20), 250_000),
		],
		..cellular_step()
	}
}

/// `cellular-deep`: 1 Mbit/s behind a 2s queue, down to 300 kbit/s at ten seconds and back at
/// twenty
fn cellular_deep() -> Uplink {
	Uplink {
		name: "cellular-deep",
		latency: Duration::from_millis(30),
		jitter: Duration::from_millis(5),
		grant: None,
		queue: Duration::from_secs(2),
		rates: vec![
			(Duration::ZERO, 125_000),
			(Duration::from_secs(10), 37_500),
			(Duration::from_secs(20), 125_000),
		],
		duration: Duration::from_secs(30),
	}
}

/// What the publisher sends
#[derive(Debug, Clone, Copy)]
enum Media {
	/// An encoder that retunes each frame to the controller's estimate
	Adaptive,
	/// One rung at a fixed bitrate in bytes per second, its upper rungs stopped
	Fixed(u64),
	/// The same rung at a constant bitrate, with no keyframes to burst
	Constant(u64),
}

/// One run's controller, media and path
#[derive(Clone)]
struct Scenario {
	controller: Arc<dyn ControllerFactory + Send + Sync>,
	media: Media,
	/// The rate the session probes up to; zero for none
	probe_target: u64,
	path: Uplink,
	seed: u64,
}

/// One sample of the sender and the bottleneck
#[derive(Debug, Clone, Copy)]
struct Snapshot {
	/// Since the media's start
	at: Duration,
	/// The bandwidth estimate, in bytes per second
	estimate: u64,
	/// The queueing delay a packet entering the bottleneck would meet
	queue: Duration,
}

/// What a run measured
#[derive(Debug, Default)]
struct Outcome {
	samples: Vec<Snapshot>,
	/// Each delivered frame's delay from its write to its last byte read, sorted
	frames: Vec<Duration>,
	/// Frames written that never arrived
	missing: u64,
	/// Media bytes read by the receiver, by when
	read: Vec<(Duration, u64)>,
	/// Media bytes written
	unique: u64,
	/// Bytes of probe packets
	redundant: u64,
	dropped: u64,
}

/// A group the sender has opened
struct Group {
	id: StreamId,
	/// When each frame was written, and the stream offset its last byte ends at
	frames: Vec<(Instant, u64)>,
	/// Bytes written to the group that the stream had no room for yet
	backlog: u64,
	ended: bool,
	finished: bool,
}

fn run(scenario: &Scenario) -> Outcome {
	let _guard = subscribe_at(tracing::Level::INFO);
	entropy::reseed(scenario.seed);
	let path = &scenario.path;
	let mut server = server_config();
	server.transport.limits.max_streams = NonZeroU64::new(1000);
	let mut pair = Pair::new(Default::default(), server);
	pair.latency = path.latency;
	let mut config = client_config();
	config.transport.congestion = scenario.controller.clone();
	config.transport.receive_timestamps = NonZeroU32::new(TIMESTAMPS);
	let (client_ch, server_ch) = pair.connect_with(config);

	let (_, first) = path.rates[0];
	let buffer = (first as f64 * path.queue.as_secs_f64()) as u32;
	pair.to_server.bottleneck = Some(Bottleneck::new(first, buffer, false));
	pair.to_server.jitter = path.jitter;
	pair.to_server.grant = path.grant;
	pair.client_conn_mut(client_ch).set_probe_target(scenario.probe_target);

	let start = pair.time;
	let end = start + path.duration;
	let frame_at = |n: u64| start + Duration::from_nanos(n * 1_000_000_000 / FPS);
	let mut outcome = Outcome::default();
	let mut groups: Vec<Group> = Vec::new();
	let mut reading: HashMap<StreamId, (u64, usize)> = HashMap::new();
	let mut read = 0;
	let mut next_frame = 0;
	let mut next_sample = start;
	let mut next_rate = 1;
	loop {
		let now = pair.time;
		if let Some(&(at, rate)) = path.rates.get(next_rate)
			&& now >= start + at
		{
			pair.to_server.bottleneck.as_mut().unwrap().set_rate(rate);
			next_rate += 1;
		}

		while frame_at(next_frame) <= now && frame_at(next_frame) < end {
			let rate = match scenario.media {
				Media::Adaptive => {
					let estimate = pair.client_conn_mut(client_ch).stats().path.bandwidth_estimate;
					estimate.unwrap_or(MIN_RATE).clamp(MIN_RATE, MAX_RATE)
				}
				Media::Fixed(rate) | Media::Constant(rate) => rate,
			};
			let keyframe = match scenario.media {
				Media::Constant(_) => 1,
				_ => KEYFRAME,
			};
			let index = next_frame % GOP;
			let delta = rate * GOP / FPS / (GOP - 1 + keyframe);
			let size = if index == 0 { delta * keyframe } else { delta };
			if index == 0 {
				if let Some(group) = groups.last_mut() {
					group.ended = true;
				}
				let id = pair
					.client_streams(client_ch)
					.open(
						Dir::Uni,
						send::Options {
							group: None,
							priority: groups.len() as i64,
						},
					)
					.expect("stream limit");
				groups.push(Group {
					id,
					frames: Vec::new(),
					backlog: 0,
					ended: false,
					finished: false,
				});
			}
			let group = groups.last_mut().unwrap();
			let offset = group.frames.last().map_or(0, |&(_, end)| end) + size;
			group.frames.push((now, offset));
			group.backlog += size;
			outcome.unique += size;
			next_frame += 1;
		}
		flush(&mut pair, client_ch, &mut groups);

		pair.drive_client();
		pair.drive_server();

		let conn = pair.server_conn_mut(server_ch);
		while let Some(id) = conn.streams().accept(Dir::Uni) {
			reading.insert(id, (0, 0));
		}
		for (id, (received, completed)) in reading.iter_mut() {
			let mut stream = conn.recv_stream(*id);
			let Ok(mut chunks) = stream.read() else { continue };
			while let Ok(Some(chunk)) = chunks.next(usize::MAX) {
				*received += chunk.bytes.len() as u64;
				read += chunk.bytes.len() as u64;
			}
			let _ = chunks.finalize();
			let frames = &groups[(id.index() - groups[0].id.index()) as usize].frames;
			while let Some(&(written, end)) = frames.get(*completed)
				&& end <= *received
			{
				outcome.frames.push(now - written);
				*completed += 1;
			}
		}

		if now >= next_sample {
			let estimate = pair.client_conn_mut(client_ch).stats().path.bandwidth_estimate;
			let queue = pair.to_server.bottleneck.as_ref().unwrap().delay(now);
			outcome.samples.push(Snapshot {
				at: now - start,
				estimate: estimate.unwrap_or(0),
				queue,
			});
			outcome.read.push((now - start, read));
			next_sample += SAMPLE;
		}
		if now >= end {
			break;
		}

		let wakeups = [
			pair.client.next_wakeup(),
			pair.server.next_wakeup(),
			Some(frame_at(next_frame)),
			Some(next_sample),
			path.rates.get(next_rate).map(|&(at, _)| start + at),
		];
		pair.time = pair.time.max(wakeups.into_iter().flatten().min().unwrap());
	}

	let written: u64 = groups.iter().map(|g| g.frames.len() as u64).sum();
	outcome.missing = written - outcome.frames.len() as u64;
	outcome.frames.sort();
	outcome.redundant = pair.client_conn_mut(client_ch).stats().path.sent_bandwidth_probe_bytes;
	outcome.dropped = pair.to_server.bottleneck.take().unwrap().stats.dropped;
	outcome
}

/// Write what credit allows of each group's backlog, finishing every ended group once written
fn flush(pair: &mut Pair, ch: ConnectionHandle, groups: &mut [Group]) {
	const ZEROS: [u8; 16_384] = [0; 16_384];
	for group in groups.iter_mut().filter(|g| !g.finished) {
		let mut stream = pair.client_send(ch, group.id);
		while group.backlog > 0 {
			let chunk = &ZEROS[..group.backlog.min(ZEROS.len() as u64) as usize];
			match stream.write(&mut &chunk[..]) {
				Ok(n) => group.backlog -= n as u64,
				Err(send::Error::Blocked) => return,
				Err(e) => panic!("write: {e}"),
			}
		}
		if group.ended {
			stream.finish().unwrap();
			group.finished = true;
		}
	}
}

/// What a run's numbers came to over each of its rate phases
#[derive(Debug, Default, Clone)]
struct Report {
	/// The bottleneck's queueing delay at the 50th, 95th and 99th percentile, past [`WARMUP`]
	queue: [Duration; 3],
	/// Frame delays at the 50th, 95th and 99th percentile
	frame: [Duration; 3],
	/// Each phase's goodput over its rate, from a second into it
	utilization: Vec<f64>,
	/// How soon after each step down the estimate fell to the new rate
	fall: Vec<Option<Duration>>,
	/// How soon after each step down the queue it built drained back under [`DRAINED`]
	drain: Vec<Option<Duration>>,
	/// How soon after each step up the estimate reached 80% of the new rate
	rise: Vec<Option<Duration>>,
}

impl Outcome {
	fn report(&self, path: &Uplink) -> Report {
		let mut queue: Vec<Duration> = self
			.samples
			.iter()
			.filter(|s| s.at >= WARMUP)
			.map(|s| s.queue)
			.collect();
		queue.sort();
		let mut report = Report {
			queue: [0.5, 0.95, 0.99].map(|p| percentile(&queue, p)),
			frame: [0.5, 0.95, 0.99].map(|p| percentile(&self.frames, p)),
			..Default::default()
		};
		for (i, &(from, rate)) in path.rates.iter().enumerate() {
			let until = path.rates.get(i + 1).map_or(path.duration, |&(at, _)| at);
			let read = |at: Duration| self.read.iter().take_while(|(t, _)| *t <= at).last().map_or(0, |r| r.1);
			let from_settled = from.max(WARMUP) + Duration::from_secs(1);
			let carried = read(until) - read(from_settled);
			report
				.utilization
				.push(carried as f64 / (until - from_settled).as_secs_f64() / rate as f64);
			let Some(&(_, before)) = i.checked_sub(1).map(|j| &path.rates[j]) else {
				continue;
			};
			let phase: Vec<&Snapshot> = self.samples.iter().filter(|s| s.at >= from && s.at < until).collect();
			if rate < before {
				report.fall.push(phase.iter().find(|s| s.estimate <= rate).map(|s| s.at - from));
				let built = phase.iter().position(|s| s.queue >= DRAINED).unwrap_or(0);
				report
					.drain
					.push(phase[built..].iter().find(|s| s.queue < DRAINED).map(|s| s.at - from));
			} else {
				report
					.rise
					.push(phase.iter().find(|s| s.estimate >= rate * 4 / 5).map(|s| s.at - from));
			}
		}
		report
	}
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
	sorted
		.get(((sorted.len() as f64 * p) as usize).min(sorted.len().saturating_sub(1)))
		.copied()
		.unwrap_or_default()
}

fn ms(d: Duration) -> f64 {
	d.as_secs_f64() * 1e3
}

fn scream() -> Arc<ScreamConfig> {
	Arc::new(ScreamConfig::default())
}

/// The cellular-step preset with media that follows the estimate: SCReAMv2 holds a smaller queue
/// than BBR3 does on the same media, and its estimate follows the drop where BBR3's max filter
/// holds on to the old rate
#[test]
fn smaller_queue_than_bbr3() {
	let _guard = subscribe();
	let path = cellular_step();
	let report = |controller| {
		let scenario = Scenario {
			controller,
			media: Media::Adaptive,
			probe_target: 0,
			path: path.clone(),
			seed: 1,
		};
		run(&scenario).report(&path)
	};
	let scream = report(scream());
	let bbr3 = report(Arc::new(Bbr3Config::default()));
	info!(?scream, ?bbr3, "cellular-step");
	for (i, p) in ["p50", "p95"].into_iter().enumerate() {
		assert!(
			scream.queue[i] < bbr3.queue[i],
			"queue {p} {:?}, {:?} under BBR3",
			scream.queue[i],
			bbr3.queue[i]
		);
	}
	assert!(scream.fall[0].is_some(), "the estimate never fell to the dropped rate");
}

/// A stopped rung recovers under native probing: a constant 500 kbit/s rung alone on a 4 Mbit/s
/// path raises the estimate to the 2.5 Mbit/s the stopped rungs need, where without a probe
/// target it stays near the rung's own rate. A rung with keyframes needs no probe: each keyframe's
/// burst raises the window on its own.
#[test]
fn probe_recovers_a_stopped_rung() {
	let _guard = subscribe();
	const TARGET: u64 = 312_500;
	let path = Uplink {
		name: "4 Mbit/s",
		rates: vec![(Duration::ZERO, 500_000)],
		duration: Duration::from_secs(15),
		..cellular_step()
	};
	let scenario = |probe_target| Scenario {
		controller: scream(),
		media: Media::Constant(62_500),
		probe_target,
		path: path.clone(),
		seed: 1,
	};
	let reached = |outcome: &Outcome| outcome.samples.iter().find(|s| s.estimate >= TARGET).map(|s| s.at);
	let baseline = run(&scenario(0));
	assert_eq!(reached(&baseline), None, "the estimate reached the target with no probe");
	let probing = run(&scenario(TARGET));
	let rise = reached(&probing).expect("the probe never raised the estimate to the target");
	info!(?rise, redundant = probing.redundant, unique = probing.unique, "probe");
}

/// The comparison table: each preset with adaptive media under SCReAMv2's variants and BBR3, and
/// the probe recovering a stopped rung on a path that opens up
#[test]
#[ignore = "SCReAMv2's measurement, run nightly by `just rs quic-sim`"]
fn compare() {
	const SEEDS: ops::RangeInclusive<u64> = 1..=4;
	let reduce_jitter = {
		let mut config = ScreamConfig::default();
		config.reduce_jitter(true);
		Arc::new(config)
	};
	let target_30ms = {
		let mut config = ScreamConfig::default();
		config.qdelay_target(Duration::from_millis(30));
		Arc::new(config)
	};
	let controllers: [(&str, Arc<dyn ControllerFactory + Send + Sync>); 4] = [
		("scream", scream()),
		("scream reduce_jitter", reduce_jitter),
		("scream target 30ms", target_30ms),
		("bbr3", Arc::new(Bbr3Config::default())),
	];

	println!(
		"| preset | controller | queue p50/p95/p99 | frame p50/p95/p99 | utilization per phase | estimate fall | queue drain | estimate rise |"
	);
	println!("|---|---|---|---|---|---|---|---|");
	for path in [cellular_step(), cellular_bursty(), cellular_deep()] {
		for (name, controller) in &controllers {
			let reports: Vec<Report> = SEEDS
				.map(|seed| {
					let scenario = Scenario {
						controller: controller.clone(),
						media: Media::Adaptive,
						probe_target: 0,
						path: path.clone(),
						seed,
					};
					run(&scenario).report(&path)
				})
				.collect();
			let triple = |pick: fn(&Report) -> [Duration; 3]| {
				let mean = |i: usize| reports.iter().map(|r| ms(pick(r)[i])).sum::<f64>() / reports.len() as f64;
				format!("{:.0}/{:.0}/{:.0}ms", mean(0), mean(1), mean(2))
			};
			let utilization = (0..path.rates.len())
				.map(|i| {
					let mean = reports.iter().map(|r| r.utilization[i]).sum::<f64>() / reports.len() as f64;
					format!("{mean:.2}")
				})
				.collect::<Vec<_>>()
				.join(" ");
			println!(
				"| {} | {name} | {} | {} | {utilization} | {} | {} | {} |",
				path.name,
				triple(|r| r.queue),
				triple(|r| r.frame),
				latency(reports.iter().flat_map(|r| r.fall.clone())),
				latency(reports.iter().flat_map(|r| r.drain.clone())),
				latency(reports.iter().flat_map(|r| r.rise.clone())),
			);
		}
	}

	// A stopped 500 kbit/s rung on a path too narrow for the stopped rungs' 2.5 Mbit/s until it
	// opens up to 4 Mbit/s at 15s
	const TARGET: u64 = 312_500;
	let path = Uplink {
		name: "1 then 4 Mbit/s",
		rates: vec![(Duration::ZERO, 125_000), (Duration::from_secs(15), 500_000)],
		duration: Duration::from_secs(30),
		..cellular_step()
	};
	println!();
	println!(
		"| media | probe | estimate before the path opened, mean | reached the target after it opened | overhead | queue p95 before/after |"
	);
	println!("|---|---|---|---|---|---|");
	let opened = path.rates[1].0;
	for (media, name) in [
		(Media::Fixed(62_500), "500 kbit/s rung"),
		(Media::Constant(62_500), "same, no keyframes"),
	] {
		for target in [TARGET, 0] {
			let outcomes: Vec<Outcome> = SEEDS
				.map(|seed| {
					run(&Scenario {
						controller: scream(),
						media,
						probe_target: target,
						path: path.clone(),
						seed,
					})
				})
				.collect();
			let mean = |f: &dyn Fn(&Outcome) -> f64| outcomes.iter().map(f).sum::<f64>() / outcomes.len() as f64;
			let before = mean(&|o| {
				let estimates: Vec<f64> = o
					.samples
					.iter()
					.filter(|s| s.at >= WARMUP && s.at < opened)
					.map(|s| s.estimate as f64)
					.collect();
				estimates.iter().sum::<f64>() / estimates.len() as f64
			});
			let reached = outcomes.iter().map(|o| {
				o.samples
					.iter()
					.find(|s| s.at >= opened && s.estimate >= TARGET)
					.map(|s| s.at - opened)
			});
			let queue = |o: &Outcome, range: ops::Range<Duration>| {
				let mut queue: Vec<Duration> =
					o.samples.iter().filter(|s| range.contains(&s.at)).map(|s| s.queue).collect();
				queue.sort();
				ms(percentile(&queue, 0.95))
			};
			println!(
				"| {name} | {} | {:.0} B/s | {} | {:.3} | {:.0}/{:.0}ms |",
				if target > 0 { "on" } else { "off" },
				before,
				latency(reached),
				mean(&|o| o.redundant as f64 / o.unique as f64),
				mean(&|o| queue(o, WARMUP..opened)),
				mean(&|o| queue(o, opened..path.duration)),
			);
		}
	}
}

/// The mean over the steps that happened, and how many never did
fn latency(values: impl Iterator<Item = Option<Duration>>) -> String {
	let values: Vec<Option<Duration>> = values.collect();
	let hits: Vec<f64> = values.iter().flatten().map(|d| ms(*d)).collect();
	let missed = values.len() - hits.len();
	let mean = hits.iter().sum::<f64>() / hits.len().max(1) as f64;
	match (hits.is_empty(), missed) {
		(true, _) => format!("never ({missed})"),
		(false, 0) => format!("{mean:.0}ms"),
		(false, _) => format!("{mean:.0}ms, never {missed}"),
	}
}
