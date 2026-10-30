//! MoQ sessions over QUIC on moq-clock's simulator: no sockets and no host clock, with the
//! path's latency, bandwidth, loss, and reordering drawn from a seed.
//!
//! A publisher serves a track of one-frame groups to a subscriber across the simulated path.
//! Random loss and reordering check that recovery still delivers every group whole. The
//! bottleneck runs below the track's bitrate behind a deep but finite queue, and the run checks
//! that the queue overflowed, so the congestion loss came from the sender filling it.

#![cfg(feature = "quic")]

use std::{net::SocketAddr, sync::Arc, time::Duration};

use moq_clock::Instant;
use moq_tokio::moq_net::{self, Timescale, Timestamp, Version, origin, track};
use moq_tokio::quic::driver::{
	self,
	sim::{Bottleneck, Link, Network, Path},
};
use moq_tokio::quic::web;

const VERSIONS: [&str; 2] = ["moq-lite-06", "moq-transport-22"];

const SERVER: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 1)), 443);
const CLIENT: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 2)), 4433);

/// The groups the publisher writes, one every [`INTERVAL`]
const GROUPS: u64 = 40;
const INTERVAL: Duration = Duration::from_millis(100);
/// Each group's single frame: 8 KB every 100 ms, about 640 kbps
const FRAME: usize = 8_000;

/// How long a run may take before it counts as stalled, in simulated time
const TIMEOUT: Duration = Duration::from_secs(60);

/// When each group arrived whole, by sequence, as the subscriber saw it
type Arrivals = Vec<(u64, Instant)>;

/// A clean path each way.
#[test]
fn clean() {
	for version in VERSIONS {
		let arrivals = run(version, 1, Path::default(), Path::default()).0;
		assert_eq!(sequences(&arrivals), (0..GROUPS).collect::<Vec<_>>(), "{version}");
	}
}

/// Random loss, duplication, jitter, and reordering both ways, which recovery has to repair.
#[test]
fn lossy() {
	for version in VERSIONS {
		for seed in 0..4 {
			let path = || Path {
				latency: Duration::from_millis(40),
				link: lossy_link(),
			};
			let arrivals = run(version, seed, path(), path()).0;
			assert_eq!(
				sequences(&arrivals),
				(0..GROUPS).collect::<Vec<_>>(),
				"{version} seed {seed}"
			);
		}
	}
}

/// A bottleneck below the track's bitrate behind a deep but finite queue: the queue overflows,
/// and every group still arrives whole, late.
#[test]
fn congested() {
	for version in VERSIONS {
		let mut link = Link::default();
		// 500 kbps, under the track's 640 kbps, behind a 16 KB queue: 256 ms, six round trips.
		link.bottleneck = Some(Bottleneck::new(62_500, 16_000, false));
		let down = Path {
			latency: Duration::from_millis(20),
			link,
		};
		let up = Path {
			latency: Duration::from_millis(20),
			..Path::default()
		};
		let (arrivals, network) = run(version, 7, up, down);
		assert_eq!(sequences(&arrivals), (0..GROUPS).collect::<Vec<_>>(), "{version}");
		let stats = network.path(SERVER, CLIENT, |path| {
			path.link.bottleneck.as_ref().unwrap().stats.clone()
		});
		assert!(
			stats.dropped > 0,
			"{version}: the sender never filled the queue: {stats:?}"
		);
	}
}

/// The same seed replays the run exactly, down to when each group arrived.
#[test]
fn replays() {
	let path = || Path {
		latency: Duration::from_millis(25),
		link: lossy_link(),
	};
	let first = run(VERSIONS[0], 3, path(), path()).0;
	let second = run(VERSIONS[0], 3, path(), path()).0;
	assert_eq!(first, second);
}

fn lossy_link() -> Link {
	let mut link = Link::default();
	link.loss = 0.05;
	link.duplicate = 0.01;
	link.jitter = Duration::from_millis(5);
	link.reorder = 0.02;
	link.reorder_delay = Duration::from_millis(20);
	link
}

fn sequences(arrivals: &Arrivals) -> Vec<u64> {
	arrivals.iter().map(|&(sequence, _)| sequence).collect()
}

/// Serve [`GROUPS`] groups from [`SERVER`] to a subscriber at [`CLIENT`], with `up` carrying
/// the client's datagrams and `down` the server's, on a fresh simulator seeded with `seed`.
fn run(version: &str, seed: u64, up: Path, down: Path) -> (Arrivals, Network) {
	let version: Version = version.parse().unwrap();
	moq_clock::sim::run(async move {
		let network = Network::new(seed);
		network.path(CLIENT, SERVER, |path| *path = up);
		network.path(SERVER, CLIENT, |path| *path = down);
		let arrivals = moq_clock::timeout(TIMEOUT, serve(&network, version, seed))
			.await
			.unwrap_or_else(|_| panic!("{version} seed {seed}: the track never finished"));
		(arrivals, network)
	})
}

async fn serve(network: &Network, version: Version, seed: u64) -> Arrivals {
	let (mut server_config, mut client_config) = tls(version);
	server_config.transport = transport(seed);
	client_config.transport = transport(seed);
	let server =
		driver::endpoint::Endpoint::new(endpoint(seed, 1).with_server(server_config), network.bind(SERVER)).unwrap();
	let client = driver::endpoint::Endpoint::new(endpoint(seed, 2), network.bind(CLIENT)).unwrap();

	let publisher = origin_producer(1);
	let viewer = origin_producer(2);
	let broadcast = publisher.create_broadcast("live").unwrap();
	let source = broadcast
		.create_track("video", track::Info::default().with_timescale(Timescale::MILLI))
		.unwrap();
	broadcast.announce(Default::default()).unwrap();

	let accept = async {
		let conn = server.accept().await.unwrap().accept().unwrap().await.unwrap();
		let session = moq_tokio::transport::Session::new(web::Session::raw(conn), moq_net::transport::Address::Setup);
		let (session, driver) = moq_net::Server::new()
			.with_versions(version.into())
			.with_publisher(&publisher.consume())
			.accept(moq_clock::now(), session)
			.await
			.unwrap();
		spawn(driver);
		session
	};
	let connect = async {
		let conn = client
			.connect(&client_config, SERVER, "localhost")
			.unwrap()
			.await
			.unwrap();
		let session = moq_tokio::transport::Session::new(web::Session::raw(conn), moq_net::transport::Address::Setup);
		let (session, driver) = moq_net::Client::new()
			.with_versions(version.into())
			.with_subscriber(viewer.clone())
			.connect(moq_clock::now(), session)
			.await
			.unwrap();
		spawn(driver);
		session
	};
	let (_server_session, _client_session) = futures::join!(accept, connect);

	let consumer = viewer.consume();
	let remote = consumer.request("live", None).await.unwrap();
	let mut reader = remote
		.track("video")
		.unwrap()
		// A budget spanning the run, so recovery has to deliver every group, however late.
		.subscribe(track::Subscription::default().with_max_delay(TIMEOUT))
		.await
		.unwrap();

	let publish = async {
		let mut source = source.clone();
		while source.subscription_changed().await.unwrap().is_none() {}
		for sequence in 0..GROUPS {
			let mut group = source.create_group(moq_net::group::Info { sequence }).unwrap();
			let timestamp = Timestamp::from_millis(sequence * INTERVAL.as_millis() as u64).unwrap();
			group.write_frame(timestamp, vec![sequence as u8; FRAME]).unwrap();
			group.finish().unwrap();
			moq_clock::sleep(INTERVAL).await;
		}
		source.finish().unwrap();
	};
	let read = async {
		let mut arrivals = Vec::new();
		while let Some(mut group) = reader.recv_group().await.unwrap() {
			let sequence = group.sequence;
			let frame = group.read_frame().await.unwrap().expect("a group is cut short");
			assert_eq!(frame.payload.len(), FRAME, "group {sequence} is cut short");
			arrivals.push((sequence, moq_clock::now()));
		}
		arrivals
	};
	let ((), mut arrivals) = futures::join!(publish, read);
	arrivals.sort_by_key(|&(sequence, _)| sequence);
	arrivals
}

/// An endpoint whose keys, connection IDs, and packet-number skips `seed` and `host` replay
fn endpoint(seed: u64, host: u8) -> driver::endpoint::Config {
	let mut rng_seed = [host; 32];
	rng_seed[..8].copy_from_slice(&seed.to_le_bytes());
	let mut config = driver::endpoint::Config::default();
	config.rng_seed = Some(rng_seed);
	config
}

/// The default transport, with BBR3's probe phases drawn from `seed`
fn transport(seed: u64) -> driver::transport::Config {
	let mut bbr = moq_congestion::Bbr3Config::default();
	let mut probe = [0; 16];
	probe[..8].copy_from_slice(&seed.to_le_bytes());
	bbr.probe_rng_seed(probe);
	let mut transport = driver::transport::Config::default();
	transport.congestion = Arc::new(bbr);
	transport
}

/// A self-signed certificate for `localhost`, and the configs that serve and trust it with
/// `version`'s ALPN
fn tls(version: Version) -> (driver::server::Config, driver::client::Config) {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let rcgen::CertifiedKey { cert, signing_key } =
		rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
	let key = rustls::pki_types::PrivateKeyDer::Pkcs8(signing_key.serialize_der().into());
	let alpn = vec![version.alpn().as_bytes().to_vec()];

	let mut server = rustls::ServerConfig::builder()
		.with_no_client_auth()
		.with_single_cert(vec![cert.der().clone()], key)
		.unwrap();
	server.alpn_protocols = alpn.clone();
	let server = driver::crypto::rustls::QuicServerConfig::try_from(server).unwrap();

	let mut roots = rustls::RootCertStore::empty();
	roots.add(cert.der().clone()).unwrap();
	let mut client = rustls::ClientConfig::builder()
		.with_root_certificates(roots)
		.with_no_client_auth();
	client.alpn_protocols = alpn;
	let client = driver::crypto::rustls::QuicClientConfig::try_from(client).unwrap();

	(
		driver::server::Config::new(Arc::new(server)),
		driver::client::Config::new(Arc::new(client)),
	)
}

fn origin_producer(hop: u64) -> origin::Producer {
	let (origin, driver) = origin::Producer::new(origin::Config::new(moq_net::Hop::new(hop).unwrap()));
	spawn(driver);
	origin
}

/// Run a protocol driver on the simulator for as long as the run lasts
fn spawn<D: moq_net::time::Driver + Unpin + 'static>(mut driver: D) {
	drop(moq_clock::sim::spawn(moq_clock::drive(move |now, waiter| {
		moq_net::time::Driver::poll(&mut driver, now, waiter)
	})));
}
