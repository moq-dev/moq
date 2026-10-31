//! The receive-ts spike's real-socket half: does the forward delay that receive timestamps
//! expose detect uplink queue growth earlier and with less noise than the RTT, over the
//! cellular moq-shaper presets?
//!
//! A media sender paces a constant 80% of the preset's starting uplink rate through the
//! shaper, and the preset's first step drops the rate below it, so the queue grows from then
//! on. The sender asks the receiver for receive timestamps, which it stamps from the kernel.
//! Both signals come from the same ACKs, read by the same trendline detector as the
//! deterministic comparison in moq-quic's `tests::receive_ts`. `near-zero` measures the noise
//! floor: what the shaper's userspace timing and this host's scheduling add on their own.
//!
//! It runs on the wall clock for about two minutes, so it is ignored: `just rs quic-sim` runs
//! it nightly. A busy host widens every number here, so read it beside the noise floor.

use std::{
	net::{SocketAddr, UdpSocket},
	sync::{Arc, Mutex},
	time::Duration,
};

use bytes::Bytes;
use moq_clock::Instant;
use moq_congestion::{self as congestion, Controller};
use moq_tokio::quic::driver;

/// How long the sender runs past the last step, so the shaper sees every phase
const AFTER: Duration = Duration::from_secs(2);
/// Samples before this are the connection settling, and count for nothing
const WARMUP: Duration = Duration::from_secs(3);
/// The span of send times each trendline fits
const WINDOW: Duration = Duration::from_millis(100);
/// A fixed detector threshold: the queue growing by 50ms each second
const THRESHOLD: f64 = 0.05;

/// One acknowledged packet, as the sender's controller saw it
#[derive(Debug, Clone, Copy)]
struct Acked {
	sent: Instant,
	acked: Instant,
	received: Option<Duration>,
	newest: bool,
}

/// Paces at a fixed rate with a window that never binds, recording every acknowledged packet
#[derive(Clone)]
struct Recorder {
	log: Arc<Mutex<Vec<Acked>>>,
	/// Bytes per second
	rate: u64,
	next: Option<Instant>,
}

impl Controller for Recorder {
	fn on_sent(&mut self, sent: congestion::Sent) -> congestion::Packet {
		let start = self.next.map_or(sent.now, |next| next.max(sent.now));
		self.next = Some(start + Duration::from_secs_f64(f64::from(sent.bytes) / self.rate as f64));
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
		self.next.filter(|&next| next > now)
	}

	fn clone_box(&self) -> Box<dyn Controller> {
		Box::new(self.clone())
	}

	fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
		self
	}
}

impl congestion::ControllerFactory for Recorder {
	fn build(self: Arc<Self>, _init: congestion::Init) -> Box<dyn Controller> {
		Box::new((*self).clone())
	}
}

/// One preset's run
struct Run {
	rtt: Detection,
	forward: Detection,
	/// How many acknowledged packets carried a receive time, of how many
	stamped: (usize, usize),
}

async fn run(preset: &str) -> Run {
	let preset = moq_shaper::Preset::load(preset).expect("preset");
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
	tokio::spawn(async move {
		let conn = server.accept().await.unwrap().accept().unwrap().await.unwrap();
		let mut stream = conn.accept_uni().await.unwrap();
		while let Ok(Some(_)) = stream.read_chunk(usize::MAX).await {}
	});

	let setup = preset.setup("127.0.0.1:0".parse().unwrap(), server_addr);
	// The media runs below the uplink until its first step, or at 1.6 Mbit/s on a path with no
	// rate
	let uplink = setup
		.config
		.up
		.rate
		.as_ref()
		.map_or(2_000_000, |rate| rate.bits_per_second);
	let step = setup.up.steps.first().map(|step| step.at);
	let duration = setup
		.up
		.steps
		.last()
		.map_or(Duration::from_secs(10), |last| last.at + AFTER);
	let start = Instant::now();
	let shaper = moq_shaper::Shaper::bind(setup).await.expect("shaper");
	let dial: SocketAddr = shaper.addr();

	let recorder = Recorder {
		log: Default::default(),
		rate: uplink * 8 / 10 / 8,
		next: None,
	};
	let tls = rustls::ClientConfig::builder()
		.dangerous()
		.with_custom_certificate_verifier(Arc::new(crate::receive_credit::AnyCert))
		.with_no_client_auth();
	let quic = driver::crypto::rustls::QuicClientConfig::try_from(tls).expect("quic config");
	let mut client = driver::client::Config::new(Arc::new(quic));
	client.transport.congestion = Arc::new(recorder.clone());
	client.transport.receive_timestamps = std::num::NonZeroU32::new(32);
	let endpoint =
		driver::endpoint::Endpoint::new(Default::default(), UdpSocket::bind("127.0.0.1:0").unwrap()).expect("client");
	let conn = endpoint
		.connect(&client, dial, "localhost")
		.expect("connect")
		.await
		.expect("handshake");
	let mut send = conn.open_uni(Default::default()).await.unwrap();
	let chunk = Bytes::from(vec![0; 64 * 1024]);
	let _ = tokio::time::timeout_at(
		(std::time::Instant::now() + duration.saturating_sub(start.elapsed())).into(),
		async {
			loop {
				send.write_all(chunk.clone()).await.unwrap();
			}
		},
	)
	.await;
	conn.close(0u32.into(), "done");
	shaper.verify().expect("the shaper acted");

	let log = recorder.log.lock().unwrap();
	let acked: Vec<&Acked> = log.iter().filter(|a| a.sent >= start).collect();
	let stamped = acked.iter().filter(|a| a.received.is_some()).count();
	let step = step.unwrap_or(duration);
	Run {
		rtt: detect(&rtt(&log, start), step),
		forward: detect(&forward(&log, start), step),
		stamped: (stamped, acked.len()),
	}
}

/// A delay reading, in milliseconds, at the send time it measures and the arrival of the ACK
/// that carried it
#[derive(Debug, Clone, Copy)]
struct Reading {
	sent: f64,
	acked: Duration,
	delay: f64,
}

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

#[derive(Debug, Default, Clone, Copy)]
struct Detection {
	rate: f64,
	noise: f64,
	ceiling: f64,
	clean: Option<Duration>,
	fixed: Option<Duration>,
	false_alarms: u32,
}

fn detect(readings: &[Reading], step: Duration) -> Detection {
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

	let steady = |r: &Reading| r.acked >= WARMUP && r.acked < step;
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
			.find(|(r, t)| r.acked >= step && t.is_some_and(|t| t > threshold))
			.map(|(r, _)| r.acked - step)
	};
	Detection {
		rate: before.len() as f64 / step.saturating_sub(WARMUP).as_secs_f64(),
		noise,
		ceiling,
		clean: first_above(ceiling),
		fixed: first_above(THRESHOLD),
		false_alarms,
	}
}

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

/// Runs `preset` and prints its rows of the table, under the same header as the others
async fn report(preset: &str) {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let run = run(preset).await;
	println!(
		"| preset | signal | readings/s | noise σ | ceiling | detect @ceiling | detect @{THRESHOLD} | false alarms @{THRESHOLD} |"
	);
	for (signal, d) in [("RTT", run.rtt), ("forward", run.forward)] {
		let show = |d: Option<Duration>| d.map_or("missed".into(), |d| format!("{:.0}ms", ms(d)));
		println!(
			"| {preset} | {signal} | {:.0} | {:.2}ms | {:.3} | {} | {} | {} |",
			d.rate,
			d.noise,
			d.ceiling,
			show(d.clean),
			show(d.fixed),
			d.false_alarms,
		);
	}
	println!(
		"| {preset} | stamped | {} of {} acknowledged packets | | | | | |",
		run.stamped.0, run.stamped.1
	);
	assert!(run.stamped.0 * 2 > run.stamped.1, "few packets carried a receive time");
}

/// The noise floor: the shaper in the path, treating nothing
#[tokio::test(flavor = "multi_thread")]
#[ignore = "the receive-ts spike's real-socket measurement, run nightly by `just rs quic-sim`"]
async fn near_zero() {
	report("near-zero").await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "the receive-ts spike's real-socket measurement, run nightly by `just rs quic-sim`"]
async fn cellular_step() {
	report("cellular-step").await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "the receive-ts spike's real-socket measurement, run nightly by `just rs quic-sim`"]
async fn cellular_bursty() {
	report("cellular-bursty").await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "the receive-ts spike's real-socket measurement, run nightly by `just rs quic-sim`"]
async fn cellular_deep() {
	report("cellular-deep").await;
}
