//! Realtime uplink congestion control: each candidate controller uploads as fast as it may
//! through sim replicas of moq-shaper's cellular uplink presets, and the bottleneck reports the
//! queue it kept, beside the throughput it reached and how fast it answered the uplink dropping.
//!
//! Each [`Preset`] copies its profile in `rs/moq-shaper/profiles/`: the delay each way, the
//! uplink's rate, queue, and steps, and its jitter and batches, with two changes the sim's
//! [`Link`] needs. The gaussian jitter becomes uniform in-order jitter of the same variance, and
//! a batch, which moq-shaper closes on a count or a window, becomes a scheduler grant releasing
//! whatever arrived within each window at its end.
//!
//! Every candidate runs each preset on the same seeds, alone and beside one CUBIC upload. The
//! merge gate runs one preset on one seed; the `#[ignore]`d [`compare`] table runs nightly with
//! `just rs quic-sim`.

use std::ops::Range;

use rand::RngExt;

use super::*;
use crate::congestion::Bbr3Config;

/// Packets reaching the bottleneck before this are Startup's, and count for no phase
const WARMUP: Duration = Duration::from_secs(2);
/// When every preset's uplink drops
const DROP: Duration = Duration::from_secs(10);
/// When every preset's uplink comes back
const RESTORE: Duration = Duration::from_secs(20);
/// How long each run goes on
const END: Duration = Duration::from_secs(30);
/// The span each throughput sample covers
const BIN: Duration = Duration::from_millis(100);
/// The most receive timestamps a candidate that reads them asks for per ACK
const TIMESTAMPS: u32 = 32;
/// The seeds the nightly table averages over
const SEEDS: ops::RangeInclusive<u64> = 1..=8;

/// A moq-shaper uplink preset, replicated on the sim
#[derive(Debug, Clone, Copy)]
struct Preset {
	name: &'static str,
	/// The one-way delay each way
	latency: Duration,
	/// The uplink's in-order jitter: uniform up to this, with the variance of the preset's
	/// gaussian
	jitter: Duration,
	/// The uplink's scheduler grants, for a preset that batches
	grant: Option<Duration>,
	/// The uplink's rate in bytes per second, before [`DROP`] and from [`RESTORE`]
	rate: u64,
	/// The uplink's rate in bytes per second, from [`DROP`] until [`RESTORE`]
	dropped: u64,
	/// The longest a datagram waits in the uplink queue before it is dropped
	queue: Duration,
}

/// Uniform jitter with the variance of a gaussian's `sigma`: `sigma` times the root of 12
const fn uniform(sigma_ms: u64) -> Duration {
	Duration::from_micros(sigma_ms * 3464)
}

/// 2 Mbit/s behind a 500ms queue, down to 500 kbit/s
const STEP: Preset = Preset {
	name: "cellular-step",
	latency: Duration::from_millis(20),
	jitter: uniform(3),
	grant: None,
	rate: 250_000,
	dropped: 62_500,
	queue: Duration::from_millis(500),
};

/// 2 Mbit/s in 20ms grants behind a 500ms queue, down to 1 Mbit/s
const BURSTY: Preset = Preset {
	name: "cellular-bursty",
	latency: Duration::from_millis(20),
	jitter: uniform(5),
	grant: Some(Duration::from_millis(20)),
	rate: 250_000,
	dropped: 125_000,
	queue: Duration::from_millis(500),
};

/// 1 Mbit/s behind a 2s queue, down to 300 kbit/s
const DEEP: Preset = Preset {
	name: "cellular-deep",
	latency: Duration::from_millis(30),
	jitter: uniform(5),
	grant: None,
	rate: 125_000,
	dropped: 37_500,
	queue: Duration::from_secs(2),
};

impl Preset {
	/// The bytes the uplink could carry in `range`
	fn capacity(&self, range: Range<Duration>) -> f64 {
		let overlap = |from: Duration, until: Duration| {
			let (from, until) = (from.max(range.start), until.min(range.end));
			until.saturating_sub(from).as_secs_f64()
		};
		let normal = overlap(Duration::ZERO, DROP) + overlap(RESTORE, Duration::MAX);
		normal * self.rate as f64 + overlap(DROP, RESTORE) * self.dropped as f64
	}
}

/// A controller under comparison
#[derive(Clone, Copy)]
struct Candidate {
	name: &'static str,
	/// The transport its connection runs, with the controller and any receive timestamps it
	/// reads
	transport: fn() -> transport::Config,
}

const BBR3: Candidate = Candidate {
	name: "BBR3",
	transport: transport::Config::default,
};

const DELAY_BOUND: Candidate = Candidate {
	name: "BBR3 delay bound",
	transport: || {
		let mut bbr = Bbr3Config::default();
		bbr.delay_bound(true).probe_rng_seed(entropy::rng().random());
		transport::Config {
			congestion: Arc::new(bbr),
			receive_timestamps: NonZeroU32::new(TIMESTAMPS),
			..transport::Config::default()
		}
	},
};

/// One bulk upload over the shared uplink
struct Flow {
	client: ConnectionHandle,
	server: ConnectionHandle,
	send: StreamId,
	recv: Option<StreamId>,
	/// Bytes the receiver read in each [`BIN`] from the start
	read: Vec<u64>,
}

/// What one run measured
#[derive(Debug, Default)]
struct Outcome {
	/// Each packet's arrival at the bottleneck, since the start, and its queueing delay there
	queue: Vec<(Duration, Duration)>,
	/// Packets the full queue dropped
	dropped: u64,
	/// How soon after the drop the candidate first paced at or below the dropped rate, if it did
	/// before the uplink came back
	reaction: Option<Duration>,
	/// The candidate's bytes the receiver read in each [`BIN`]
	read: Vec<u64>,
	/// The competing CUBIC upload's bytes the receiver read in each [`BIN`], if it ran
	competitor: Vec<u64>,
}

/// Uploads with `candidate` over `preset` on `seed`, beside a CUBIC upload if `competing`
fn run(preset: &Preset, candidate: &Candidate, seed: u64, competing: bool) -> Outcome {
	entropy::reseed(seed);
	let mut server = server_config();
	server.transport.limits.max_streams = NonZeroU64::new(10);
	let mut pair = Pair::new(Default::default(), server);
	pair.latency = preset.latency;

	let connect = |pair: &mut Pair, transport: transport::Config| {
		let mut config = client_config();
		config.transport = transport;
		let (client, server) = pair.connect_with(config);
		let send = pair.client_streams(client).open(Dir::Uni, Default::default()).unwrap();
		Flow {
			client,
			server,
			send,
			recv: None,
			read: Vec::new(),
		}
	};
	let mut flows = vec![connect(&mut pair, (candidate.transport)())];
	if competing {
		flows.push(connect(&mut pair, cubic_transport()));
	}

	let mut bottleneck = Bottleneck::timed(preset.rate, preset.queue);
	bottleneck.trace = Some(Vec::new());
	pair.to_server.bottleneck = Some(bottleneck);
	pair.to_server.jitter = preset.jitter;
	pair.to_server.grant = preset.grant;

	const CHUNK: [u8; 16_384] = [0; 16_384];
	let start = pair.time;
	let mut steps = vec![(RESTORE, preset.rate), (DROP, preset.dropped)];
	let mut reaction = None;
	loop {
		let now = pair.time;
		if let Some(&(at, rate)) = steps.last()
			&& now >= start + at
		{
			pair.to_server.bottleneck.as_mut().unwrap().set_rate(rate);
			steps.pop();
		}
		for flow in &flows {
			while pair.client_send(flow.client, flow.send).write(&mut &CHUNK[..]).is_ok() {}
		}

		pair.drive_client();
		pair.drive_server();

		let elapsed = now - start;
		if reaction.is_none() && (DROP..RESTORE).contains(&elapsed) {
			let metrics = pair.client_conn_mut(flows[0].client).congestion_state().metrics();
			if metrics.pacing_rate.is_some_and(|rate| rate <= preset.dropped) {
				reaction = Some(elapsed - DROP);
			}
		}

		let bin = (elapsed.as_nanos() / BIN.as_nanos()) as usize;
		for flow in &mut flows {
			if flow.recv.is_none() {
				flow.recv = pair.server_streams(flow.server).accept(Dir::Uni);
			}
			let Some(id) = flow.recv else { continue };
			if flow.read.len() <= bin {
				flow.read.resize(bin + 1, 0);
			}
			let mut stream = pair.server_recv(flow.server, id);
			let mut chunks = stream.read().unwrap();
			while let Ok(Some(chunk)) = chunks.next(usize::MAX) {
				flow.read[bin] += chunk.bytes.len() as u64;
			}
			let _ = chunks.finalize();
		}

		if now >= start + END {
			break;
		}
		let wakeups = [
			pair.client.next_wakeup(),
			pair.server.next_wakeup(),
			steps.last().map(|&(at, _)| start + at),
		];
		pair.time = pair.time.max(wakeups.into_iter().flatten().min().unwrap());
	}

	let bottleneck = pair.to_server.bottleneck.take().unwrap();
	let mut flows = flows.into_iter();
	Outcome {
		queue: bottleneck
			.trace
			.unwrap()
			.into_iter()
			.map(|(at, delay)| (at.saturating_duration_since(start), delay))
			.collect(),
		dropped: bottleneck.stats.dropped,
		reaction,
		read: flows.next().unwrap().read,
		competitor: flows.next().map(|flow| flow.read).unwrap_or_default(),
	}
}

impl Outcome {
	/// The queueing delays of packets reaching the bottleneck in `range`, sorted
	fn delays(&self, range: Range<Duration>) -> Vec<Duration> {
		let mut delays: Vec<Duration> = self
			.queue
			.iter()
			.filter(|(at, _)| range.contains(at))
			.map(|&(_, delay)| delay)
			.collect();
		delays.sort_unstable();
		delays
	}

	/// The `p` quantile of the queueing delay in `range`
	fn queue(&self, range: Range<Duration>, p: f64) -> Duration {
		let delays = self.delays(range);
		delays[((delays.len() - 1) as f64 * p) as usize]
	}

	/// The worst queueing delay a packet met while the uplink was down, and how soon after the
	/// drop it met it
	fn peak(&self) -> (Duration, Duration) {
		self.queue.iter().filter(|(at, _)| (DROP..RESTORE).contains(at)).fold(
			(Duration::ZERO, Duration::ZERO),
			|peak, &(at, delay)| match delay > peak.0 {
				true => (delay, at - DROP),
				false => peak,
			},
		)
	}

	/// How soon after the drop the queue fell back under a quarter of its peak, if it did
	/// before the uplink came back
	fn drained(&self) -> Option<Duration> {
		let (peak, reached) = self.peak();
		self.queue
			.iter()
			.find(|(at, delay)| *at > DROP + reached && *at < RESTORE && *delay < peak / 4)
			.map(|(at, _)| *at - DROP)
	}

	/// Bytes `read` in `range`, to the [`BIN`]
	fn bytes(read: &[u64], range: Range<Duration>) -> f64 {
		let bins = (range.start.as_nanos() / BIN.as_nanos()) as usize..(range.end.as_nanos() / BIN.as_nanos()) as usize;
		read.get(bins).unwrap_or_default().iter().sum::<u64>() as f64
	}

	/// The candidate's goodput in `range`, as a share of what the uplink could carry
	fn utilization(&self, preset: &Preset, range: Range<Duration>) -> f64 {
		Self::bytes(&self.read, range.clone()) / preset.capacity(range)
	}

	/// The candidate's share of the goodput beside the CUBIC upload, after Startup
	fn share(&self) -> f64 {
		let ours = Self::bytes(&self.read, WARMUP..END);
		ours / (ours + Self::bytes(&self.competitor, WARMUP..END))
	}
}

/// On the gate's seed, the delay bound keeps a shorter queue than BBR3 over cellular-step, whose
/// drop BBR3 answers only once its estimate ages
#[test]
fn delay_bound_keeps_a_shorter_queue() {
	let _guard = subscribe();
	let bbr3 = run(&STEP, &BBR3, 1, false);
	let bound = run(&STEP, &DELAY_BOUND, 1, false);
	let (bbr3, bound) = (bbr3.queue(WARMUP..END, 0.95), bound.queue(WARMUP..END, 0.95));
	assert!(
		bound < bbr3,
		"p95 queue {bound:?} with the delay bound, {bbr3:?} without"
	);
}

/// The comparison's table, over every preset, candidate, and [`SEEDS`], alone and beside CUBIC
#[test]
#[ignore = "the realtime comparison, run nightly by `just rs quic-sim`"]
fn compare() {
	let _guard = subscribe_at(tracing::Level::WARN);
	println!(
		"| preset | controller | queue p50 | p95 | p99 | p99 before the drop | utilization | after the drop | peak | reaction | drained | dropped | share beside CUBIC |"
	);
	println!("|---|---|---|---|---|---|---|---|---|---|---|---|---|");
	for preset in [STEP, BURSTY, DEEP] {
		for candidate in [BBR3, DELAY_BOUND] {
			let alone: Vec<Outcome> = SEEDS.map(|seed| run(&preset, &candidate, seed, false)).collect();
			let shared: Vec<Outcome> = SEEDS.map(|seed| run(&preset, &candidate, seed, true)).collect();
			let ms = |f: &dyn Fn(&Outcome) -> Duration| avg(alone.iter().map(|o| f(o).as_secs_f64() * 1e3));
			println!(
				"| {} | {} | {:.0}ms | {:.0}ms | {:.0}ms | {:.0}ms | {:.0}% | {:.0}% | {:.0}ms | {} | {} | {:.0} | {:.0}% |",
				preset.name,
				candidate.name,
				ms(&|o| o.queue(WARMUP..END, 0.5)),
				ms(&|o| o.queue(WARMUP..END, 0.95)),
				ms(&|o| o.queue(WARMUP..END, 0.99)),
				ms(&|o| o.queue(WARMUP..DROP, 0.99)),
				avg(alone.iter().map(|o| o.utilization(&preset, WARMUP..END))) * 100.0,
				avg(alone.iter().map(|o| o.utilization(&preset, DROP..RESTORE))) * 100.0,
				ms(&|o| o.peak().0),
				latency(alone.iter().map(|o| o.reaction)),
				latency(alone.iter().map(Outcome::drained)),
				avg(alone.iter().map(|o| o.dropped as f64)),
				avg(shared.iter().map(Outcome::share)) * 100.0,
			);
		}
	}
}

fn avg(values: impl Iterator<Item = f64>) -> f64 {
	let values: Vec<f64> = values.collect();
	values.iter().sum::<f64>() / values.len() as f64
}

/// The mean over the seeds that got there, and how many never did
fn latency(values: impl Iterator<Item = Option<Duration>>) -> String {
	let values: Vec<Option<Duration>> = values.collect();
	let hits: Vec<f64> = values.iter().flatten().map(|d| d.as_secs_f64() * 1e3).collect();
	let never = values.len() - hits.len();
	match hits.is_empty() {
		true => "never".to_string(),
		false if never == 0 => format!("{:.0}ms", avg(hits.into_iter())),
		false => format!("{:.0}ms, never {never}", avg(hits.into_iter())),
	}
}
