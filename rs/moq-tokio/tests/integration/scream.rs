//! SCReAMv2 ([`congestion::Scream`]) over real sockets through the cellular moq-shaper presets.
//!
//! A media sender encodes 30 frames a second at the controller's estimate, a keyframe every two
//! seconds, each group on its own stream, up through the shaper's uplink. Each frame carries when
//! it was written, so the receiver times it to its last byte. The sender asks for receive
//! timestamps, and every acknowledged packet's forward delay above the run's lowest is its
//! queueing delay: sender and receiver share this host's clock. `near-zero` measures the noise
//! floor that delay carries on its own, from the shaper's userspace timing and this host's
//! scheduling.
//!
//! It runs on the wall clock for about a minute and a half, so it is ignored: `just rs quic-sim`
//! runs it nightly. moq-quic's `tests::scream` runs the same measurement on the seeded sim, beside
//! BBR3 on the same media. A busy host widens every number here, so read it beside the noise
//! floor.

use std::{
	net::UdpSocket,
	sync::{Arc, Mutex},
	time::Duration,
};

use bytes::{Buf, BufMut, Bytes, BytesMut};
use kio::clock::Instant;
use moq_congestion::{self as congestion, Controller, ControllerFactory};
use moq_tokio::quic::driver;

const FPS: u64 = 30;
/// Frames per group: a keyframe every two seconds
const GOP: u64 = 60;
/// A keyframe's size, in delta frames
const KEYFRAME: u64 = 5;
/// The encoder's bitrate range, 100 kbit/s to 10 Mbit/s, in bytes per second
const MIN_RATE: u64 = 12_500;
const MAX_RATE: u64 = 1_250_000;
/// Each frame's header: when it was written, in microseconds since the run began, and its length
const HEADER: usize = 12;
/// Samples before this are the connection starting up, and count for nothing
const WARMUP: Duration = Duration::from_secs(3);
/// How long the sender runs past the last step
const AFTER: Duration = Duration::from_secs(2);
/// The queue delay a step's reaction drains back under: half SCReAMv2's default target
const DRAINED: Duration = Duration::from_millis(30);

/// Delegates to a controller, recording when each acknowledged packet was sent and received
struct Recording {
	inner: Box<dyn Controller>,
	log: Arc<Mutex<Vec<(Instant, Duration)>>>,
}

impl Controller for Recording {
	fn on_sent(&mut self, sent: congestion::Sent) -> congestion::Packet {
		self.inner.on_sent(sent)
	}

	fn on_cwnd_limited(&mut self) {
		self.inner.on_cwnd_limited()
	}

	fn on_app_limited(&mut self, in_flight: u64) {
		self.inner.on_app_limited(in_flight)
	}

	fn on_probe_filled(&mut self, in_flight: u64) {
		self.inner.on_probe_filled(in_flight)
	}

	fn on_ack(&mut self, ack: congestion::Ack<'_>) {
		let mut log = self.log.lock().unwrap();
		log.extend(ack.packets.iter().filter_map(|p| Some((p.sent, p.received?))));
		drop(log);
		self.inner.on_ack(ack)
	}

	fn on_loss(&mut self, loss: congestion::Loss<'_>) {
		self.inner.on_loss(loss)
	}

	fn on_ce(&mut self, ce: congestion::Ce) {
		self.inner.on_ce(ce)
	}

	fn on_spurious_loss(&mut self) {
		self.inner.on_spurious_loss()
	}

	fn on_mtu_update(&mut self, new_mtu: u16) {
		self.inner.on_mtu_update(new_mtu)
	}

	fn on_ack_frequency_update(&mut self, threshold: u64, max_ack_delay: Duration) {
		self.inner.on_ack_frequency_update(threshold, max_ack_delay)
	}

	fn window(&self) -> u64 {
		self.inner.window()
	}

	fn pace(&mut self, now: Instant, bytes: u64) -> Option<Instant> {
		self.inner.pace(now, bytes)
	}

	fn burst(&self) -> Option<u64> {
		self.inner.burst()
	}

	fn metrics(&self) -> congestion::ControllerMetrics {
		self.inner.metrics()
	}

	fn probes(&self) -> bool {
		self.inner.probes()
	}

	fn clone_box(&self) -> Box<dyn Controller> {
		Box::new(Self {
			inner: self.inner.clone_box(),
			log: self.log.clone(),
		})
	}

	fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
		self
	}
}

struct Recorder {
	inner: Arc<dyn ControllerFactory + Send + Sync>,
	log: Arc<Mutex<Vec<(Instant, Duration)>>>,
}

impl ControllerFactory for Recorder {
	fn build(self: Arc<Self>, init: congestion::Init) -> Box<dyn Controller> {
		Box::new(Recording {
			inner: self.inner.clone().build(init),
			log: self.log.clone(),
		})
	}
}

/// What one run measured
struct Run {
	/// Each acknowledged packet's queueing delay, by when it was sent
	queue: Vec<(Duration, Duration)>,
	/// Each delivered frame's delay from its write to its last byte read
	frames: Vec<Duration>,
	/// The bandwidth estimate every 10ms
	estimates: Vec<(Duration, u64)>,
	/// Media bytes read, by when
	read: Arc<Mutex<Vec<(Duration, u64)>>>,
}

async fn run(preset: &moq_shaper::Preset) -> Run {
	let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
	let key = rustls::pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());
	let server_config =
		driver::server::Config::with_single_cert(vec![cert.cert.into()], key.into()).expect("server config");
	let server = driver::endpoint::Endpoint::new(
		driver::endpoint::Config::default().with_server(server_config),
		UdpSocket::bind("127.0.0.1:0").unwrap(),
	)
	.expect("server");
	let server_addr = server.local_addr();

	let setup = preset.setup("127.0.0.1:0".parse().unwrap(), server_addr);
	let duration = setup
		.up
		.steps
		.last()
		.map_or(Duration::from_secs(10), |last| last.at + AFTER);
	let start = Instant::now();
	let shaper = moq_shaper::Shaper::bind(setup).await.expect("shaper");

	let frames = Arc::new(Mutex::new(Vec::new()));
	let read = Arc::new(Mutex::new(Vec::new()));
	tokio::spawn({
		let frames = frames.clone();
		let read = read.clone();
		async move {
			let conn = server.accept().await.unwrap().accept().unwrap().await.unwrap();
			let total = Arc::new(Mutex::new(0u64));
			while let Ok(mut stream) = conn.accept_uni().await {
				let (frames, read, total) = (frames.clone(), read.clone(), total.clone());
				tokio::spawn(async move {
					let mut buf = BytesMut::new();
					while let Ok(Some(chunk)) = stream.read_chunk(usize::MAX).await {
						let now = start.elapsed();
						let mut total = total.lock().unwrap();
						*total += chunk.len() as u64;
						read.lock().unwrap().push((now, *total));
						buf.extend_from_slice(&chunk);
						while buf.len() >= HEADER {
							let len = u32::from_be_bytes(buf[8..12].try_into().unwrap()) as usize;
							if buf.len() < HEADER + len {
								break;
							}
							let written = Duration::from_micros(buf.get_u64());
							buf.advance(4 + len);
							frames.lock().unwrap().push(now - written);
						}
					}
				});
			}
		}
	});

	let log = Arc::new(Mutex::new(Vec::new()));
	let tls = rustls::ClientConfig::builder()
		.dangerous()
		.with_custom_certificate_verifier(Arc::new(crate::receive_credit::AnyCert))
		.with_no_client_auth();
	let quic = driver::crypto::rustls::QuicClientConfig::try_from(tls).expect("quic config");
	let mut client = driver::client::Config::new(Arc::new(quic));
	client.transport.congestion = Arc::new(Recorder {
		inner: Arc::new(congestion::ScreamConfig::default()),
		log: log.clone(),
	});
	client.transport.receive_timestamps = std::num::NonZeroU32::new(32);
	let endpoint =
		driver::endpoint::Endpoint::new(Default::default(), UdpSocket::bind("127.0.0.1:0").unwrap()).expect("client");
	let conn = endpoint
		.connect(&client, shaper.addr(), "localhost")
		.expect("connect")
		.await
		.expect("handshake");

	let estimates = Arc::new(Mutex::new(Vec::new()));
	let sampler = tokio::spawn({
		let conn = conn.clone();
		let estimates = estimates.clone();
		async move {
			let mut tick = kio::clock::interval(Duration::from_millis(10));
			loop {
				tick.tick().await;
				let estimate = conn.stats().path.bandwidth_estimate.unwrap_or(0);
				estimates.lock().unwrap().push((start.elapsed(), estimate));
			}
		}
	});

	let mut tick = kio::clock::interval(Duration::from_nanos(1_000_000_000 / FPS));
	let mut stream = None;
	let mut finished = Vec::new();
	for n in 0.. {
		tick.tick().await;
		let now = start.elapsed();
		if now >= duration {
			break;
		}
		let rate = conn
			.stats()
			.path
			.bandwidth_estimate
			.unwrap_or(MIN_RATE)
			.clamp(MIN_RATE, MAX_RATE);
		let delta = rate * GOP / FPS / (GOP - 1 + KEYFRAME);
		let size = if n % GOP == 0 { delta * KEYFRAME } else { delta } as usize;
		if n % GOP == 0 {
			if let Some(done) = stream.take() {
				finished.push(driver::send::Stream::finish(done));
			}
			stream = Some(conn.open_uni(Default::default()).await.unwrap());
		}
		let mut frame = BytesMut::with_capacity(HEADER + size);
		frame.put_u64(now.as_micros() as u64);
		frame.put_u32(size as u32);
		frame.put_bytes(0, size);
		stream.as_mut().unwrap().write_all(Bytes::from(frame)).await.unwrap();
	}
	sampler.abort();
	conn.close(0u32.into(), "done");
	shaper.verify().expect("the shaper acted");

	let log = log.lock().unwrap();
	let forward: Vec<(Duration, i128)> = log
		.iter()
		.filter(|(sent, _)| *sent >= start)
		.map(|&(sent, received)| {
			let sent = sent - start;
			(sent, received.as_nanos() as i128 - sent.as_nanos() as i128)
		})
		.collect();
	let base = forward.iter().map(|(_, delay)| *delay).min().unwrap_or(0);
	let queue = forward
		.into_iter()
		.map(|(sent, delay)| (sent, Duration::from_nanos((delay - base) as u64)))
		.collect();
	let mut frames = frames.lock().unwrap().clone();
	frames.sort();
	let estimates = estimates.lock().unwrap().clone();
	Run {
		queue,
		frames,
		estimates,
		read,
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

fn triple(sorted: &[Duration]) -> String {
	let [p50, p95, p99] = [0.5, 0.95, 0.99].map(|p| ms(percentile(sorted, p)));
	format!("{p50:.0}/{p95:.0}/{p99:.0}ms")
}

/// Runs `preset` and prints its row of the table, under the same header as the others
async fn report(name: &str) {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let preset = moq_shaper::Preset::load(name).expect("preset");
	let setup = preset.setup("127.0.0.1:0".parse().unwrap(), "127.0.0.1:1".parse().unwrap());
	// The uplink's rate over each phase, from its start; none on a path with no rate limit
	let mut phases = Vec::new();
	if let Some(rate) = &setup.config.up.rate {
		phases.push((Duration::ZERO, rate.bits_per_second / 8));
		for step in &setup.up.steps {
			if let Some(bps) = step.bits_per_second {
				phases.push((step.at, bps / 8));
			}
		}
	}
	println!(
		"| preset | queue p50/p95/p99 | frame p50/p95/p99 | utilization per phase | estimate fall | queue drain | estimate rise |"
	);
	let run = run(&preset).await;
	let end = run.estimates.last().map_or(Duration::ZERO, |(at, _)| *at);
	let mut queue: Vec<Duration> = run
		.queue
		.iter()
		.filter(|(sent, _)| *sent >= WARMUP)
		.map(|(_, q)| *q)
		.collect();
	queue.sort();

	let read = run.read.lock().unwrap();
	let read_at = |at: Duration| read.iter().take_while(|(t, _)| *t <= at).last().map_or(0, |r| r.1);
	let mut utilization = Vec::new();
	let (mut fall, mut drain, mut rise) = (Vec::new(), Vec::new(), Vec::new());
	let show = |d: Option<Duration>| d.map_or("never".to_string(), |d| format!("{:.0}ms", ms(d)));
	for (i, &(from, rate)) in phases.iter().enumerate() {
		let until = phases.get(i + 1).map_or(end, |&(at, _)| at);
		let settled = from.max(WARMUP) + Duration::from_secs(1);
		let carried = read_at(until).saturating_sub(read_at(settled));
		utilization.push(format!(
			"{:.2}",
			carried as f64 / (until - settled).as_secs_f64() / rate as f64
		));
		let Some(&(_, before)) = i.checked_sub(1).map(|j| &phases[j]) else {
			continue;
		};
		let estimates = run.estimates.iter().filter(|(at, _)| *at >= from && *at < until);
		if rate < before {
			fall.push(show(
				estimates.clone().find(|(_, e)| *e <= rate).map(|(at, _)| *at - from),
			));
			let phase: Vec<&(Duration, Duration)> = run
				.queue
				.iter()
				.filter(|(sent, _)| *sent >= from && *sent < until)
				.collect();
			let built = phase.iter().position(|(_, q)| *q >= DRAINED).unwrap_or(0);
			drain.push(show(
				phase[built..]
					.iter()
					.find(|(_, q)| *q < DRAINED)
					.map(|(sent, _)| *sent - from),
			));
		} else {
			rise.push(show(
				estimates
					.clone()
					.find(|(_, e)| *e >= rate * 4 / 5)
					.map(|(at, _)| *at - from),
			));
		}
	}
	println!(
		"| {name} | {} | {} | {} | {} | {} | {} |",
		triple(&queue),
		triple(&run.frames),
		utilization.join(" "),
		fall.join(" "),
		drain.join(" "),
		rise.join(" "),
	);
	assert!(!run.queue.is_empty(), "no acknowledged packet carried a receive time");
}

/// The noise floor: the shaper in the path, treating nothing
#[tokio::test(flavor = "multi_thread")]
#[ignore = "SCReAMv2's real-socket measurement, run nightly by `just rs quic-sim`"]
async fn near_zero() {
	report("near-zero").await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "SCReAMv2's real-socket measurement, run nightly by `just rs quic-sim`"]
async fn cellular_step() {
	report("cellular-step").await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "SCReAMv2's real-socket measurement, run nightly by `just rs quic-sim`"]
async fn cellular_bursty() {
	report("cellular-bursty").await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "SCReAMv2's real-socket measurement, run nightly by `just rs quic-sim`"]
async fn cellular_deep() {
	report("cellular-deep").await;
}
