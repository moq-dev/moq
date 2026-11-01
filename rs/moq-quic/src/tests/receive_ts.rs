//! Receive timestamps (draft-ietf-quic-receive-ts-03) between two moq-quic endpoints, and the
//! spike that asks whether the forward delay they expose detects uplink queue growth earlier
//! and with less noise than the RTT.
//!
//! A media sender paces a constant bitrate up through a bottleneck that steps its rate below
//! the media's, so the queue grows from the step on. Both signals come from the same ACKs, read
//! by the same trendline detector: the RTT of each ACK's newest packet, and each acknowledged
//! packet's forward delay, its peer receive time less its send time. The forward delay carries
//! an unknown constant offset, which a gradient ignores. The merge gate runs one path on one
//! seed; the `#[ignore]`d [`compare`] table runs nightly with `just rs quic-sim`.

use super::*;

/// The media's bitrate, 1.6 Mbit/s, paced and never reacting, so the queue is the path's alone
const MEDIA: u64 = 200_000;
/// The uplink's rate before the step, 2 Mbit/s
const UPLINK: u64 = 250_000;
/// The uplink's buffer, a second at [`UPLINK`]: deep, as a cellular uplink's is
const BUFFER: u32 = 250_000;
/// One-way propagation delay, each way
const LATENCY: Duration = Duration::from_millis(20);
/// Samples before this are the connection settling, and count for nothing
const WARMUP: Duration = Duration::from_secs(2);
/// When the uplink steps down
const STEP: Duration = Duration::from_secs(10);
/// How long the run goes on after the step
const AFTER: Duration = Duration::from_secs(2);
/// The span of send times each trendline fits, as GoogCC's window of twenty packet groups
/// spans at a few hundred packets per second
const WINDOW: Duration = Duration::from_millis(100);
/// A fixed detector threshold: the queue growing by 50ms each second
const THRESHOLD: f64 = 0.05;
/// The most receive timestamps the sender asks for per ACK, above any ACK threshold here
const TIMESTAMPS: u32 = 32;

/// One acknowledged packet, as the sender's controller saw it
#[derive(Debug, Clone, Copy)]
struct Acked {
	sent: Instant,
	/// When its ACK arrived
	acked: Instant,
	/// Its peer receive time, since the peer's basis
	received: Option<Duration>,
	/// Whether it was the newest packet its ACK acknowledged, which yields the RTT sample
	newest: bool,
}

/// Paces at a fixed rate with a window that never binds, recording every acknowledged packet
#[derive(Clone)]
struct Recorder {
	log: Arc<Mutex<Vec<Acked>>>,
	rate: u64,
	pacer: TestPacer,
}

impl Recorder {
	fn new(rate: u64) -> Self {
		Self {
			log: Default::default(),
			rate,
			pacer: TestPacer::default(),
		}
	}
}

impl Controller for Recorder {
	fn on_sent(&mut self, sent: congestion::Sent) -> congestion::Packet {
		self.pacer.on_sent(sent.now, sent.bytes, self.rate);
		Default::default()
	}

	fn on_ack(&mut self, ack: congestion::Ack<'_>) {
		let mut log = self.log.lock().unwrap();
		for (i, packet) in ack.packets.iter().enumerate() {
			log.push(Acked {
				sent: packet.sent,
				acked: ack.now,
				received: packet.received,
				newest: i + 1 == ack.packets.len(),
			});
		}
	}

	fn on_loss(&mut self, _loss: congestion::Loss<'_>) {}

	fn on_ce(&mut self, _ce: congestion::Ce) {}

	fn on_mtu_update(&mut self, _new_mtu: u16) {}

	fn window(&self) -> u64 {
		u64::from(u32::MAX)
	}

	fn pace(&mut self, now: Instant, _bytes: u64) -> Option<Instant> {
		self.pacer.pace(now)
	}

	fn clone_box(&self) -> Box<dyn Controller> {
		Box::new(self.clone())
	}

	fn into_any(self: Box<Self>) -> Box<dyn Any> {
		self
	}
}

impl ControllerFactory for Recorder {
	fn build(self: Arc<Self>, _init: congestion::Init) -> Box<dyn Controller> {
		Box::new((*self).clone())
	}
}

/// A pair whose client paces [`MEDIA`] through `recorder`, asking for `timestamps` per ACK
fn pair_with(
	recorder: &Recorder,
	timestamps: Option<u32>,
	ack_threshold: Option<u32>,
) -> (Pair, ConnectionHandle, ConnectionHandle) {
	let mut server = server_config();
	server.transport.limits.max_streams = NonZeroU64::new(10);
	let mut pair = Pair::new(Default::default(), server);
	pair.latency = LATENCY;
	let mut config = client_config();
	config.transport.congestion = Arc::new(recorder.clone());
	config.transport.receive_timestamps = timestamps.and_then(NonZeroU32::new);
	config.transport.ack_frequency = ack_threshold.map(|threshold| transport::ack_frequency::Config {
		ack_eliciting_threshold: threshold.into(),
		max_ack_delay: Some(Duration::from_millis(25)),
		..transport::ack_frequency::Config::default()
	});
	let (client_ch, server_ch) = pair.connect_with(config);
	(pair, client_ch, server_ch)
}

/// Sends the media until `end`, stepping the uplink to `step.1` at `step.0`, returning when it
/// started
fn send(
	pair: &mut Pair,
	client_ch: ConnectionHandle,
	server_ch: ConnectionHandle,
	end: Duration,
	mut step: Option<(Duration, u64)>,
) -> Instant {
	const CHUNK: [u8; 16_384] = [0; 16_384];
	let start = pair.time;
	let id = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	let mut recv = None;
	loop {
		let now = pair.time;
		if let Some((at, rate)) = step
			&& now >= start + at
		{
			pair.to_server.bottleneck.as_mut().unwrap().set_rate(rate);
			step = None;
		}
		while pair.client_send(client_ch, id).write(&mut &CHUNK[..]).is_ok() {}

		pair.drive_client();
		pair.drive_server();

		if recv.is_none() {
			recv = pair.server_streams(server_ch).accept(Dir::Uni);
		}
		if let Some(recv) = recv {
			let mut stream = pair.server_recv(server_ch, recv);
			let mut chunks = stream.read().unwrap();
			while let Ok(Some(_)) = chunks.next(usize::MAX) {}
			let _ = chunks.finalize();
		}

		if now >= start + end {
			return start;
		}
		let wakeups = [
			pair.client.next_wakeup(),
			pair.server.next_wakeup(),
			step.map(|(at, _)| start + at),
		];
		pair.time = pair.time.max(wakeups.into_iter().flatten().min().unwrap());
	}
}

/// A peer that asks for receive timestamps hears when each packet arrived: on a path that only
/// delays, every packet's receive time less its send time is the same, to the microsecond the
/// frame carries
#[test]
fn reports_arrival_times() {
	let _guard = subscribe();
	let recorder = Recorder::new(MEDIA);
	let (mut pair, client_ch, server_ch) = pair_with(&recorder, Some(TIMESTAMPS), None);
	let start = send(&mut pair, client_ch, server_ch, Duration::from_secs(1), None);

	let log = recorder.log.lock().unwrap();
	let acked: Vec<_> = log.iter().filter(|a| a.sent >= start).collect();
	let reported: Vec<i128> = acked
		.iter()
		.filter_map(|a| Some(a.received?.as_nanos() as i128 - (a.sent - start).as_nanos() as i128))
		.collect();
	assert!(
		reported.len() * 10 >= acked.len() * 9,
		"{} of {} acknowledged packets carried a receive time",
		reported.len(),
		acked.len()
	);
	let spread = reported.iter().max().unwrap() - reported.iter().min().unwrap();
	assert!(spread <= 2_000, "forward delay varied by {spread}ns on a fixed path");
}

/// Without asking, a peer sends none, and the controller sees no receive time
#[test]
fn none_unless_asked() {
	let _guard = subscribe();
	let recorder = Recorder::new(MEDIA);
	let (mut pair, client_ch, server_ch) = pair_with(&recorder, None, None);
	send(&mut pair, client_ch, server_ch, Duration::from_millis(500), None);
	let log = recorder.log.lock().unwrap();
	assert!(!log.is_empty());
	assert!(log.iter().all(|a| a.received.is_none()));
}

/// A delay signal: one reading per sample, in milliseconds, at the send time it measures and
/// the arrival of the ACK that carried it, when a detector could act on it
#[derive(Debug, Clone, Copy)]
struct Reading {
	sent: f64,
	acked: Duration,
	delay: f64,
}

/// The RTT of each ACK's newest packet
fn rtt(log: &[Acked], start: Instant) -> Vec<Reading> {
	log.iter()
		.filter(|a| a.newest && a.sent >= start)
		.map(|a| Reading {
			sent: ms(a.sent - start),
			acked: a.acked - start,
			delay: ms(a.acked - a.sent),
		})
		.collect()
}

/// Each acknowledged packet's forward delay, offset by the peer's unknown basis
fn forward(log: &[Acked], start: Instant) -> Vec<Reading> {
	log.iter()
		.filter(|a| a.sent >= start)
		.filter_map(|a| {
			Some(Reading {
				sent: ms(a.sent - start),
				acked: a.acked - start,
				delay: ms(a.received?) - ms(a.sent - start),
			})
		})
		.collect()
}

fn ms(d: Duration) -> f64 {
	d.as_secs_f64() * 1000.0
}

/// What a trendline detector made of one signal
#[derive(Debug, Default, Clone, Copy)]
struct Detection {
	/// Readings per second before the step
	rate: f64,
	/// The delay's standard deviation before the step, in milliseconds
	noise: f64,
	/// The steepest trend before the step: the lowest threshold that never fires falsely
	ceiling: f64,
	/// How long after the step a threshold at [`Self::ceiling`] first fires
	clean: Option<Duration>,
	/// How long after the step a threshold at [`THRESHOLD`] first fires
	fixed: Option<Duration>,
	/// The times a threshold at [`THRESHOLD`] began firing before the step
	false_alarms: u32,
}

/// Fits the trend of each reading's last [`WINDOW`] of send times, as a delay-based controller
/// would on each ACK, and judges it against the step at [`STEP`]
fn detect(readings: &[Reading]) -> Detection {
	let window = ms(WINDOW);
	let mut trends = Vec::with_capacity(readings.len());
	let mut first = 0;
	for (i, reading) in readings.iter().enumerate() {
		while readings[first].sent < reading.sent - window {
			first += 1;
		}
		let span = &readings[first..=i];
		trends.push((reading, (span.len() >= 4).then(|| slope(span)).flatten()));
	}

	let steady = |r: &Reading| r.acked >= WARMUP && r.acked < STEP;
	let before: Vec<f64> = readings.iter().filter(|r| steady(r)).map(|r| r.delay).collect();
	let mean = before.iter().sum::<f64>() / before.len() as f64;
	let noise = (before.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / before.len() as f64).sqrt();
	let ceiling = trends
		.iter()
		.filter(|(r, _)| steady(r))
		.filter_map(|(_, t)| *t)
		.fold(f64::MIN, f64::max);

	let mut false_alarms = 0;
	let mut firing = false;
	for (_, trend) in trends.iter().filter(|(r, _)| steady(r)) {
		let fires = trend.is_some_and(|t| t > THRESHOLD);
		false_alarms += (fires && !firing) as u32;
		firing = fires;
	}
	let first_above = |threshold: f64| {
		trends
			.iter()
			.find(|(r, t)| r.acked >= STEP && t.is_some_and(|t| t > threshold))
			.map(|(r, _)| r.acked - STEP)
	};
	Detection {
		rate: before.len() as f64 / (STEP - WARMUP).as_secs_f64(),
		noise,
		ceiling,
		clean: first_above(ceiling),
		fixed: first_above(THRESHOLD),
		false_alarms,
	}
}

/// The least-squares slope of delay over send time: milliseconds of queue per millisecond
fn slope(span: &[Reading]) -> Option<f64> {
	let n = span.len() as f64;
	let x = span.iter().map(|r| r.sent).sum::<f64>() / n;
	let y = span.iter().map(|r| r.delay).sum::<f64>() / n;
	let (mut num, mut den) = (0.0, 0.0);
	for r in span {
		num += (r.sent - x) * (r.delay - y);
		den += (r.sent - x).powi(2);
	}
	(den > 0.0).then(|| num / den)
}

/// The path a comparison runs over
#[derive(Debug, Clone, Copy)]
struct Path {
	name: &'static str,
	/// The uplink's rate from [`STEP`] on
	step: u64,
	/// In-order jitter on the uplink, which both signals see
	up_jitter: Duration,
	/// In-order jitter on the downlink, as a cellular downlink scheduler adds, which only the
	/// RTT sees
	down_jitter: Duration,
	/// The ACK frequency to ask of the receiver, or the default of every second packet
	ack_threshold: Option<u32>,
}

/// One seed's run over a path, with and without timestamps
#[derive(Debug)]
struct Run {
	rtt: Detection,
	forward: Detection,
	/// Downlink bytes per second with timestamps, then without
	downlink: (f64, f64),
	/// The queueing delay the bottleneck had built by the end
	queue: Duration,
}

fn run(path: &Path, seed: u64) -> Run {
	let mut downlink = [0.0; 2];
	let mut detections = None;
	let mut queue = Duration::ZERO;
	for (i, timestamps) in [Some(TIMESTAMPS), None].into_iter().enumerate() {
		entropy::reseed(seed);
		let recorder = Recorder::new(MEDIA);
		let (mut pair, client_ch, server_ch) = pair_with(&recorder, timestamps, path.ack_threshold);
		pair.to_server.bottleneck = Some(Bottleneck::new(UPLINK, BUFFER, false));
		pair.to_server.jitter = path.up_jitter;
		pair.to_client.jitter = path.down_jitter;
		let before = pair.server_conn_mut(server_ch).stats().udp_tx.bytes;
		let start = send(&mut pair, client_ch, server_ch, STEP + AFTER, Some((STEP, path.step)));
		let bytes = pair.server_conn_mut(server_ch).stats().udp_tx.bytes - before;
		downlink[i] = bytes as f64 / (STEP + AFTER).as_secs_f64();
		if timestamps.is_some() {
			let log = recorder.log.lock().unwrap();
			detections = Some((detect(&rtt(&log, start)), detect(&forward(&log, start))));
			queue = pair.to_server.bottleneck.as_ref().unwrap().stats.max_delay;
		}
	}
	let (rtt, forward) = detections.unwrap();
	Run {
		rtt,
		forward,
		downlink: (downlink[0], downlink[1]),
		queue,
	}
}

/// A cellular downlink's scheduling jitter, on a 1.2 Mbit/s step: the gate's one case
const GATE: Path = Path {
	name: "down jitter 10ms",
	step: 150_000,
	up_jitter: Duration::ZERO,
	down_jitter: Duration::from_millis(10),
	ack_threshold: None,
};

/// Jitter the ACK path adds is noise to the RTT and invisible to the forward delay, so the
/// forward delay catches the queue sooner at a threshold neither crosses before the step
#[test]
fn forward_delay_ignores_the_ack_path() {
	let _guard = subscribe();
	let run = run(&GATE, 1);
	let forward = run.forward.clean.expect("the forward delay never saw the queue");
	assert!(
		run.rtt.clean.is_none_or(|rtt| forward < rtt),
		"forward delay detected after {forward:?}, RTT after {:?}",
		run.rtt.clean
	);
}

/// The spike's table, over every path and [`SEEDS`]
#[test]
#[ignore = "the receive-ts spike's measurement, run nightly by `just rs quic-sim`"]
fn compare() {
	const SEEDS: ops::RangeInclusive<u64> = 1..=8;
	const SLOW: u64 = 190_000;
	let paths = [
		Path {
			name: "clean",
			step: 150_000,
			up_jitter: Duration::ZERO,
			down_jitter: Duration::ZERO,
			ack_threshold: None,
		},
		Path {
			name: "clean, slow growth",
			step: SLOW,
			..GATE
		}
		.without_jitter(),
		Path {
			name: "up jitter 5ms",
			up_jitter: Duration::from_millis(5),
			down_jitter: Duration::ZERO,
			..GATE
		},
		GATE,
		Path {
			name: "down jitter 10ms, slow growth",
			step: SLOW,
			..GATE
		},
		Path {
			name: "down jitter 30ms",
			down_jitter: Duration::from_millis(30),
			..GATE
		},
		Path {
			name: "both 5ms/10ms, ACK every 10",
			up_jitter: Duration::from_millis(5),
			ack_threshold: Some(10),
			..GATE
		},
	];

	println!(
		"| path | signal | readings/s | noise σ | ceiling | detect @ceiling | detect @{THRESHOLD} | false alarms @{THRESHOLD} |"
	);
	println!("|---|---|---|---|---|---|---|---|");
	for path in &paths {
		let runs: Vec<Run> = SEEDS.map(|seed| run(path, seed)).collect();
		for (signal, pick) in [
			("RTT", (|r: &Run| r.rtt) as fn(&Run) -> Detection),
			("forward", |r: &Run| r.forward),
		] {
			let d: Vec<Detection> = runs.iter().map(pick).collect();
			println!(
				"| {} | {signal} | {:.0} | {:.2}ms | {:.3} | {} | {} | {:.1} |",
				path.name,
				avg(d.iter().map(|d| d.rate)),
				avg(d.iter().map(|d| d.noise)),
				avg(d.iter().map(|d| d.ceiling)),
				latency(d.iter().map(|d| d.clean)),
				latency(d.iter().map(|d| d.fixed)),
				avg(d.iter().map(|d| f64::from(d.false_alarms))),
			);
		}
		let with = avg(runs.iter().map(|r| r.downlink.0));
		let without = avg(runs.iter().map(|r| r.downlink.1));
		println!(
			"| {} | downlink | {:.0} B/s with, {:.0} B/s without: +{:.0}% | queue at end {:?} | | | | |",
			path.name,
			with,
			without,
			(with / without - 1.0) * 100.0,
			runs[0].queue,
		);
	}
}

impl Path {
	const fn without_jitter(self) -> Self {
		Self {
			up_jitter: Duration::ZERO,
			down_jitter: Duration::ZERO,
			..self
		}
	}
}

fn avg(values: impl Iterator<Item = f64>) -> f64 {
	let values: Vec<f64> = values.collect();
	values.iter().sum::<f64>() / values.len() as f64
}

/// The mean latency over the seeds that detected, and how many missed
fn latency(values: impl Iterator<Item = Option<Duration>>) -> String {
	let values: Vec<Option<Duration>> = values.collect();
	let hits: Vec<f64> = values.iter().flatten().map(|d| ms(*d)).collect();
	let missed = values.len() - hits.len();
	match hits.is_empty() {
		true => format!("missed {missed}"),
		false if missed == 0 => format!("{:.0}ms", avg(hits.into_iter())),
		false => format!("{:.0}ms, missed {missed}", avg(hits.into_iter())),
	}
}
