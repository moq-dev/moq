//! Datagram delivery over native QUIC through a bufferbloated bottleneck.
//!
//! One stock server publishes a datagram track to two viewers: one on loopback and one
//! behind a rate limit below the offered bitrate, with a deep queue that drops what overflows
//! it. Datagrams are best-effort, so this records what each viewer got rather than timing
//! the work: the in-memory `session_delivery_datagram` benchmark owns protocol CPU, which
//! here would mix with QUIC, the shaper and the kernel.

#![cfg(feature = "quic")]

use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use moq_tokio::moq_net::{self, Timestamp};
use moq_tokio::quic::{driver, web};

use crate::receive_credit::{AnyCert, rand_seed};

const TIMEOUT: Duration = Duration::from_secs(10);

/// How long a viewer waits for another datagram once the publisher has stopped.
const QUIET: Duration = Duration::from_secs(1);

/// The slow viewer's bottleneck, both ways: half the overloaded cell's offered rate.
const BOTTLENECK: u64 = 2_000_000;

/// Bytes per datagram payload: a typical full datagram once headers are added.
const PAYLOAD: usize = 1000;

/// How long each cell publishes.
const RUN: Duration = Duration::from_secs(8);

/// A bottleneck below the offered bitrate, behind a deep but finite queue that drops what
/// overflows it. No random loss: every drop is congestion.
fn bufferbloat() -> moq_shaper::Profile {
	moq_shaper::Profile {
		delay: Duration::from_millis(10),
		rate: Some(moq_shaper::Rate {
			bits_per_second: BOTTLENECK,
			// One datagram, so a flight is paced rather than let through whole.
			burst: 1500,
			queue: Duration::from_millis(250),
		}),
		..Default::default()
	}
}

fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

/// One offered load.
#[derive(Clone, Copy)]
struct Cell {
	name: &'static str,
	/// Time between datagrams.
	interval: Duration,
}

/// A datagram's payload: its index, then a fill derived from it, so a crossed, truncated
/// or corrupted delivery fails.
fn payload(index: u64) -> Bytes {
	let mut bytes = vec![index as u8; PAYLOAD];
	bytes[..8].copy_from_slice(&index.to_le_bytes());
	Bytes::from(bytes)
}

/// The index `payload` encoded, checking the rest of it.
fn decode(payload: &[u8]) -> u64 {
	assert_eq!(payload.len(), PAYLOAD, "datagram truncated");
	let index = u64::from_le_bytes(payload[..8].try_into().unwrap());
	assert!(
		payload[8..].iter().all(|byte| *byte == index as u8),
		"datagram corrupted"
	);
	index
}

/// A viewer's session to the server, `through` a shaper or direct.
struct Viewer {
	name: &'static str,
	track: moq_net::track::Subscriber,
	conn: driver::connection::Connection,
	_session: moq_net::Session,
	_endpoint: driver::endpoint::Endpoint,
}

impl Viewer {
	async fn connect(name: &'static str, version: moq_net::Version, addr: std::net::SocketAddr) -> Self {
		let mut tls = rustls::ClientConfig::builder()
			.dangerous()
			.with_custom_certificate_verifier(Arc::new(AnyCert))
			.with_no_client_auth();
		tls.alpn_protocols = vec![version.alpn().as_bytes().to_vec()];
		let quic = driver::crypto::rustls::QuicClientConfig::try_from(tls).expect("quic config");
		let client = driver::client::Config::new(Arc::new(quic));
		let endpoint =
			driver::endpoint::Endpoint::new(Default::default(), std::net::UdpSocket::bind("127.0.0.1:0").unwrap())
				.expect("endpoint");
		let conn = endpoint
			.connect(&client, addr, "localhost")
			.expect("connect")
			.await
			.expect("handshake");

		let origin = moq_tokio::origin::spawn();
		let transport =
			moq_tokio::transport::Session::new(web::Session::raw(conn.clone()), moq_net::transport::Address::Setup);
		let (session, driver) = moq_net::Client::new()
			.with_versions(version.into())
			.with_subscriber(origin.clone())
			.connect(moq_net::time::Instant::now(), transport)
			.await
			.expect("client handshake");
		tokio::spawn(moq_net::time::run(driver));

		let consumer = origin.consume();
		let broadcast = kio::clock::timeout(TIMEOUT, consumer.follow("room", None).unwrap().available())
			.await
			.expect("resolve timeout")
			.expect("broadcast resolves");
		let track = kio::clock::timeout(TIMEOUT, broadcast.track("audio").unwrap().subscribe(None))
			.await
			.expect("subscribe timeout")
			.expect("subscribe");
		Self {
			name,
			track,
			conn,
			_session: session,
			_endpoint: endpoint,
		}
	}

	/// Read the priming group, which binds a moq-transport subscription's alias before any
	/// datagram needs it.
	async fn primed(&mut self) {
		let mut group = kio::clock::timeout(TIMEOUT, self.track.recv_group())
			.await
			.expect("group timed out")
			.expect("group")
			.expect("track ended");
		kio::clock::timeout(TIMEOUT, group.read_frame())
			.await
			.expect("frame timed out")
			.expect("frame")
			.expect("group ended");
	}

	/// Every datagram until [`QUIET`] passes without one after `done` is set: each one's
	/// index and arrival time.
	async fn receive(mut self, done: Arc<tokio::sync::Notify>) -> (Self, Vec<(u64, kio::clock::Instant)>) {
		let mut arrivals = Vec::new();
		let finished = done.notified();
		tokio::pin!(finished);
		let mut stopping = false;
		loop {
			let next = self.track.recv_datagram();
			let datagram = if stopping {
				match kio::clock::timeout(QUIET, next).await {
					Ok(datagram) => datagram,
					Err(_) => break,
				}
			} else {
				tokio::select! {
					datagram = next => datagram,
					_ = &mut finished => {
						stopping = true;
						continue;
					}
				}
			};
			let datagram = datagram.expect("datagram").expect("track ended");
			arrivals.push((decode(&datagram.payload), kio::clock::now()));
		}
		(self, arrivals)
	}
}

/// What one viewer got of `offered` datagrams sent at `sent`.
struct Report {
	received: usize,
	/// Arrivals with a lower index than one already received.
	reordered: usize,
	/// QUIC DATAGRAM frames the viewer's connection received.
	frames: u64,
	/// From the publisher's append to the viewer's receive, sorted.
	latency: Vec<Duration>,
}

impl Report {
	fn new(viewer: &Viewer, arrivals: &[(u64, kio::clock::Instant)], sent: &[kio::clock::Instant]) -> Self {
		let mut seen = vec![false; sent.len()];
		let mut newest = None;
		let mut reordered = 0;
		let mut latency = Vec::with_capacity(arrivals.len());
		for &(index, arrived) in arrivals {
			let slot = seen.get_mut(index as usize).expect("an index never offered");
			assert!(!*slot, "{}: datagram {index} delivered twice", viewer.name);
			*slot = true;
			if newest.is_some_and(|newest| index < newest) {
				reordered += 1;
			}
			newest = newest.max(Some(index));
			latency.push(arrived.saturating_duration_since(sent[index as usize]));
		}
		latency.sort();
		Self {
			received: arrivals.len(),
			reordered,
			frames: viewer.conn.stats().frame_rx.datagram,
			latency,
		}
	}

	fn percentile(&self, p: usize) -> f64 {
		self.latency
			.get(self.latency.len().saturating_sub(1) * p / 100)
			.map_or(f64::NAN, |latency| latency.as_secs_f64() * 1e3)
	}
}

/// Publish datagrams at `cell`'s rate to a loopback viewer and a bufferbloated one.
async fn measure(version: &str, cell: Cell) {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let version: moq_net::Version = version.parse().expect("version");

	let publisher = moq_tokio::origin::spawn();
	let broadcast = publisher.create_broadcast("room").expect("broadcast");
	let mut track = broadcast.create_track("audio", timed()).unwrap();
	broadcast.announce(Default::default()).expect("announce");

	let mut config = moq_tokio::listen::Config::default();
	config.bind = Some("127.0.0.1:0".parse().unwrap());
	config.tls.generate = vec!["localhost".into()];
	config.version = vec![version];
	let mut server = config
		.init(Default::default())
		.expect("init server")
		.listen()
		.await
		.expect("listen");
	let server_addr: std::net::SocketAddr = ([127, 0, 0, 1], server.local_addr().expect("addr").port()).into();
	let shaper = moq_shaper::Shaper::bind(moq_shaper::Config {
		bind: "127.0.0.1:0".parse().unwrap(),
		target: server_addr,
		seed: rand_seed(),
		up: bufferbloat(),
		down: bufferbloat(),
	})
	.await
	.expect("start the shaper");
	let accept = tokio::spawn(async move {
		loop {
			let request = server.accept().await.expect("accept");
			let session = request.with_publisher(&publisher).ok().await.expect("session");
			tokio::spawn(async move {
				let _ = session.closed().await;
			});
		}
	});

	let mut viewers = [
		Viewer::connect("loopback", version, server_addr).await,
		Viewer::connect("bufferbloat", version, shaper.addr()).await,
	];
	track
		.write_frame(Timestamp::ZERO, &b"prime"[..])
		.expect("priming group");
	for viewer in &mut viewers {
		viewer.primed().await;
	}

	let done = Arc::new(tokio::sync::Notify::new());
	let receivers: Vec<_> = viewers
		.into_iter()
		.map(|viewer| tokio::spawn(viewer.receive(done.clone())))
		.collect();
	let offered = (RUN.as_nanos() / cell.interval.as_nanos()) as u64;
	let mut sent = Vec::with_capacity(offered as usize);
	let mut ticks = kio::clock::interval(cell.interval);
	ticks.set_missed_tick_behavior(kio::clock::MissedTickBehavior::Burst);
	for index in 0..offered {
		ticks.tick().await;
		sent.push(kio::clock::now());
		track
			.append_datagram(Timestamp::from_millis(index).unwrap(), payload(index))
			.expect("append datagram");
	}
	done.notify_waiters();

	let shaped = shaper.verify().unwrap_or_else(|err| panic!("shaper: {err:#}"));
	for receiver in receivers {
		let (viewer, arrivals) = receiver.await.unwrap();
		let report = Report::new(&viewer, &arrivals, &sent);
		let overflowed = if viewer.name == "bufferbloat" {
			shaped.down.overflowed
		} else {
			0
		};
		println!(
			"{version},{},{},{offered},{},{},{overflowed},{},{},{:.1},{:.1},{:.1}",
			cell.name,
			viewer.name,
			report.received,
			offered as usize - report.received,
			report.frames,
			report.reordered,
			report.percentile(50),
			report.percentile(99),
			report.percentile(100),
		);
		if viewer.name == "loopback" {
			// The slow viewer's queue and drops must not reach the other session.
			assert_eq!(
				report.received as u64, offered,
				"{version}: the loopback viewer lost datagrams"
			);
		}
	}
	accept.abort();
}

/// Datagram delivery to a loopback viewer and a bufferbloated one, below and above the
/// bottleneck's rate. Reports per viewer the datagrams offered, received, lost, those the
/// bottleneck's full queue dropped (UDP packets, so approximate), QUIC DATAGRAM frames
/// received, reordered arrivals, and append-to-receive latency percentiles. Nothing on the
/// path expires a datagram by age: what the sender's QUIC datagram buffer holds waits
/// there until it is sent or displaced. Fails if a datagram arrives twice or corrupted, or
/// if the loopback viewer loses any. Run by nightly.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "release benchmark, run by nightly"]
async fn benchmark_datagram_delivery() {
	let cells = [
		// 1 Mbit/s of payload: under the bottleneck.
		Cell {
			name: "under",
			interval: Duration::from_millis(8),
		},
		// 4 Mbit/s of payload: twice the bottleneck.
		Cell {
			name: "over",
			interval: Duration::from_millis(2),
		},
	];
	println!("version,cell,viewer,offered,received,lost,overflowed,quic_frames,reordered,p50_ms,p99_ms,max_ms");
	for version in ["moq-lite-06", "moq-lite-07-wip", "moq-transport-22"] {
		for cell in cells {
			measure(version, cell).await;
		}
	}
}
