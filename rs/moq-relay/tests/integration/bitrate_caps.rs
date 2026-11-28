//! A capped grant over QUIC is admitted and holds its session to its caps, and every
//! session holds only the SETUP window until it is admitted, on kio's QUIC simulator and
//! the io_uring workers alike.
//!
//! The simulator runs the relay's `Connection` behind a moq-tokio server, not `Relay`'s own
//! listener, so a transfer's time is exact and asserted within a window: a cap that
//! throttles far below its rate fails too. The io_uring workers serve real sockets, so
//! there the caps are measured on the wall clock as lower bounds on how long a transfer
//! takes: uncapped, the same transfer takes milliseconds.

use std::{net::SocketAddr, time::Duration};

use moq_relay::auth::Admissions;
use moq_tokio::quic::driver;

const TIMEOUT: Duration = Duration::from_secs(10);

/// One megabyte a second, so a fresh cap allows a one-megabyte burst.
const RATE: u64 = 8_000_000;
/// Past the burst and the SETUP window, so the rest waits on the rate.
const FRAME: usize = 2 << 20;
/// The least time [`FRAME`] can take under [`RATE`]: what is left after a full burst and
/// the SETUP window, at the rate, with some slack for the burst a cap starts with.
const LEAST: Duration = Duration::from_millis(700);

/// A grant of everything, with `publish` and `subscribe` caps.
fn grant(publish: Option<u64>, subscribe: Option<u64>) -> moq_auth::Grant {
	let all = moq_auth::Prefixes::all();
	let mut grant = moq_auth::Grant::new(all.clone(), all);
	grant.publish.rate = publish.map(|bps| moq_auth::Rate::from_bps(bps).unwrap());
	grant.subscribe.rate = subscribe.map(|bps| moq_auth::Rate::from_bps(bps).unwrap());
	grant
}

/// Where a relay's QUIC listener is, and the clients and sockets that reach it.
enum Dial {
	/// Real sockets on loopback.
	#[cfg(uring)]
	Real(u16),
	/// A simulated host serving at `addr`, trusting `fingerprints`.
	#[cfg(feature = "test-support")]
	Sim {
		network: driver::sim::Network,
		addr: SocketAddr,
		fingerprints: Vec<String>,
	},
}

impl Dial {
	/// A client on the simulated host at `host`; a real one ignores it.
	fn client(&self, host: std::net::IpAddr) -> moq_tokio::Client {
		let _ = host;
		match self {
			#[cfg(uring)]
			Dial::Real(_) => {
				let mut config = moq_tokio::connect::Config::default();
				config.tls.insecure = Some(true);
				config.bind = Some("127.0.0.1:0".parse().expect("parse bind"));
				config
					.init(Default::default())
					.expect("client init")
					.with_reconnect(false)
			}
			#[cfg(feature = "test-support")]
			Dial::Sim {
				network, fingerprints, ..
			} => {
				let mut connect = moq_tokio::connect::Config::default();
				connect.bind = Some(SocketAddr::new(host, 0));
				connect.tls.fingerprint = fingerprints.clone();
				let mut config = moq_tokio::client::Config::default();
				config.connect = connect;
				config.sim = Some(network.host(host));
				config.init().expect("init client").with_reconnect(false)
			}
		}
	}

	fn addr(&self) -> SocketAddr {
		match self {
			#[cfg(uring)]
			Dial::Real(port) => ([127, 0, 0, 1], *port).into(),
			#[cfg(feature = "test-support")]
			Dial::Sim { addr, .. } => *addr,
		}
	}

	fn url(&self) -> url::Url {
		match self {
			#[cfg(uring)]
			Dial::Real(port) => format!("https://localhost:{port}/room").parse().unwrap(),
			#[cfg(feature = "test-support")]
			Dial::Sim { addr, .. } => format!("moqt://{addr}/room").parse().unwrap(),
		}
	}

	/// A raw QUIC endpoint on the simulated host at `host`, which a real one ignores.
	fn endpoint(&self, host: std::net::IpAddr) -> driver::endpoint::Endpoint {
		let _ = host;
		let socket: driver::endpoint::Socket = match self {
			#[cfg(uring)]
			Dial::Real(_) => std::net::UdpSocket::bind("127.0.0.1:0").unwrap().into(),
			#[cfg(feature = "test-support")]
			Dial::Sim { network, .. } => (Box::new(network.bind(SocketAddr::new(host, 4433)))
				as Box<dyn driver::endpoint::AsyncUdpSocket>)
				.into(),
		};
		driver::endpoint::Endpoint::new(Default::default(), socket).expect("endpoint")
	}
}

/// Connect `client`, admitted on `grant`.
async fn connect(
	dial: &Dial,
	client: moq_tokio::Client,
	admissions: &mut Admissions,
	grant: moq_auth::Grant,
) -> moq_tokio::Connection {
	let connecting = client.connect(dial.url()).established();
	let admitting = async {
		let admission = admissions.next().await.expect("admission");
		admission.grant(moq_auth::lease::Consumer::fixed(grant));
	};
	let (connection, ()) = kio::clock::timeout(TIMEOUT, async { tokio::join!(connecting, admitting) })
		.await
		.expect("connect timeout");
	connection.expect("connect failed")
}

/// Publish one [`FRAME`] on `test/video` from the host at `publisher`, and how long a
/// subscriber on the host at `subscriber` takes to read it.
async fn transfer(
	dial: &Dial,
	admissions: &mut Admissions,
	hosts: [std::net::IpAddr; 2],
	publisher: moq_auth::Grant,
	subscriber: moq_auth::Grant,
) -> Duration {
	let origin = moq_tokio::origin::spawn();
	let broadcast = origin
		.route("test", moq_net::origin::RouteOptions::default())
		.expect("create broadcast");
	let track = broadcast
		.broadcast()
		.create_track(
			"video",
			moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI),
		)
		.expect("create track");
	let client = dial.client(hosts[0]).with_publisher(&origin);
	let _publishing = connect(dial, client, admissions, publisher).await;

	let subscribing = moq_tokio::origin::spawn();
	let consumer = subscribing.consume();
	let client = dial.client(hosts[1]).with_subscriber(subscribing);
	let _subscribed = connect(dial, client, admissions, subscriber).await;
	let broadcast_consumer = kio::clock::timeout(TIMEOUT, consumer.request("test", None))
		.await
		.expect("request timeout")
		.expect("announced broadcast resolves");
	let mut subscription = broadcast_consumer
		.track("video")
		.unwrap()
		.subscribe(None)
		.await
		.expect("subscribe");

	let start = kio::clock::now();
	let group = track
		.append_group(
			moq_net::Timestamp::from_millis(track.largest().map_or(0, |largest| largest.group + 1)).unwrap(),
			vec![7u8; FRAME],
		)
		.expect("append group");
	group.finish().expect("finish group");
	let mut group = kio::clock::timeout(TIMEOUT, subscription.recv_group())
		.await
		.expect("recv_group timeout")
		.expect("recv_group failed")
		.expect("track closed");
	let frame = kio::clock::timeout(TIMEOUT, group.read_frame())
		.await
		.expect("read_frame timeout")
		.expect("read_frame failed")
		.expect("group closed");
	assert_eq!(frame.payload.len(), FRAME);
	kio::clock::now() - start
}

/// How long a capped publisher and then a capped subscriber each take to move [`FRAME`],
/// between the simulated hosts `hosts`.
async fn held_to_its_caps(dial: &Dial, admissions: &mut Admissions, hosts: [std::net::IpAddr; 2]) -> [Duration; 2] {
	let published = transfer(dial, admissions, hosts, grant(Some(RATE), None), grant(None, None)).await;
	let subscribed = transfer(dial, admissions, hosts, grant(None, None), grant(None, Some(RATE))).await;
	[published, subscribed]
}

/// Before admission the relay credits only the SETUP window, so a client cannot bank
/// the full one; once admitted, the window opens.
async fn window_opens_once_admitted(dial: &Dial, admissions: &mut Admissions, host: std::net::IpAddr) {
	let mut connect = moq_tokio::connect::Config::default();
	connect.tls.insecure = Some(true);
	let mut tls = connect.tls.build().expect("tls config");
	tls.alpn_protocols = vec![b"moq-lite-06".to_vec()];
	let endpoint = dial.endpoint(host);
	let conn = endpoint
		.connect(
			&driver::client::Config::new(std::sync::Arc::new(tls)),
			dial.addr(),
			"localhost",
		)
		.expect("connect")
		.await
		.expect("handshake");

	// SETUP type, then a one-byte message holding zero parameters.
	let mut setup = conn.open_uni(Default::default()).await.expect("open");
	setup.write_all(&[0x01, 0x01, 0x00][..]).await.expect("write");
	setup.finish();
	let admission = kio::clock::timeout(TIMEOUT, admissions.next())
		.await
		.expect("admission timeout")
		.expect("admission");

	// Unread until the session runs, so it holds its credit, and the write waits for more.
	// The session later stops this stream, so how the write ends is not reported: it is
	// polled alongside each wait below and never completes.
	let banked = 1 << 20;
	let mut stream = conn.open_uni(Default::default()).await.expect("open");
	let writing = async {
		let _ = stream.write_all(&vec![0u8; banked][..]).await;
		std::future::pending::<()>().await
	};
	tokio::pin!(writing);
	let blocked = async {
		while conn.stats().frame_tx.data_blocked == 0 {
			kio::clock::sleep(Duration::from_millis(10)).await;
		}
	};
	tokio::select! {
		biased;
		() = &mut writing => unreachable!("a write that never completes"),
		blocked = kio::clock::timeout(TIMEOUT, blocked) => {
			blocked.expect("the client was never blocked on the SETUP window");
		}
	}
	let stats = conn.stats();
	assert!(
		stats.udp_tx.bytes < moq_net::transport::SETUP_RECEIVE_WINDOW + (64 << 10),
		"sent {} bytes before admission",
		stats.udp_tx.bytes
	);
	// Reading SETUP returns a little credit, so the window may hold the writer more than once.
	assert!(stats.send_credit.stalls > 0, "the SETUP window held the writer");

	// Nothing reads the blocked stream before admission, so only opening the window
	// credits more. The session then stops that stream, so watch the credit, not the data.
	let credited = stats.frame_rx.max_data;
	admission.grant(moq_auth::lease::Consumer::fixed(grant(None, None)));
	let opened = async {
		while conn.stats().frame_rx.max_data == credited {
			kio::clock::sleep(Duration::from_millis(10)).await;
		}
	};
	tokio::select! {
		biased;
		() = &mut writing => unreachable!("a write that never completes"),
		opened = kio::clock::timeout(TIMEOUT, opened) => {
			opened.expect("the window never opened once admitted");
		}
	}
	let waited = conn.stats().send_credit.stalled;
	assert!(waited > Duration::ZERO, "the wait for admission counts, not {waited:?}");

	conn.close(driver::VarInt::from_u32(0), "done");
	endpoint.close(driver::VarInt::from_u32(0), "done").await;
}

/// The relay on kio's QUIC simulator, serving through an embedder that answers each admission.
#[cfg(feature = "test-support")]
mod sim {
	use std::{net::SocketAddr, time::Duration};

	use moq_relay::auth;
	use moq_tokio::quic::driver::sim::Network;

	use super::{Admissions, Dial, FRAME, LEAST, RATE};
	use crate::{
		memory,
		quic_sim::{PORT, PUBLISHER, SUBSCRIBER, ip, serve},
	};

	/// A relay at [`PORT`] on a fresh network.
	async fn relay() -> (Dial, Admissions) {
		let network = Network::new(0);
		let (auth, admissions) = auth::Auth::embedded("test");
		let cluster = memory::start(memory::cluster(), moq_tokio::connect::Config::default()).await;
		let addr = SocketAddr::new(ip(1), PORT);
		let relay = serve(&network, addr, &cluster, &auth).await;
		let dial = Dial::Sim {
			network,
			addr,
			fingerprints: relay.fingerprints,
		};
		(dial, admissions)
	}

	/// A capped session moves [`FRAME`] no faster than its cap, and no slower than the cap
	/// alone allows: the burst a fresh cap starts with is only ever extra.
	#[test]
	fn session_is_held_to_its_caps() {
		memory::run(|| async {
			let (dial, mut admissions) = relay().await;
			let most = Duration::from_secs_f64(FRAME as f64 * 8.0 / RATE as f64);
			for elapsed in super::held_to_its_caps(&dial, &mut admissions, [PUBLISHER, SUBSCRIBER]).await {
				assert!(
					(LEAST..=most).contains(&elapsed),
					"{FRAME} bytes took {elapsed:?}, not within {LEAST:?}..={most:?}"
				);
			}
		});
	}

	#[test]
	fn window_opens_once_admitted() {
		memory::run(|| async {
			let (dial, mut admissions) = relay().await;
			super::window_opens_once_admitted(&dial, &mut admissions, PUBLISHER).await;
		});
	}
}

#[cfg(uring)]
mod uring {
	use std::net::{IpAddr, Ipv4Addr};

	use moq_relay::{Config, Relay};

	use super::{Dial, FRAME, LEAST};

	/// Real sockets have no simulated hosts to name.
	const HOST: IpAddr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);

	async fn relay() -> Option<(Relay, u16, tempfile::TempDir)> {
		kio::clock::host_time(); // a real relay on real sockets, paced in real time
		let _ = rind_rustls::default_provider().unwrap().install_default();
		match moq_uring::Worker::new(Default::default()) {
			Ok(_) => {}
			Err(moq_uring::Error::Unsupported(reason)) if std::env::var_os("CI_STRICT").is_none() => {
				eprintln!("skipping io_uring relay test: {reason}");
				return None;
			}
			Err(err) => panic!("io_uring worker setup failed: {err}"),
		}
		let dir = tempfile::tempdir().expect("tempdir");
		let mut config: Config = crate::peer_limits::config(dir.path());
		config.quic.max_streams = None;
		config.runtime.workers = moq_relay::runtime::Workers::Count(std::num::NonZeroU16::new(1).unwrap());
		config.runtime.pin = false;
		config.runtime.io_uring = true;
		let relay = Relay::load(config).await.expect("load relay").with_signals(false);
		let port = relay.addr().expect("workers bound an address").port();
		Some((relay, port, dir))
	}

	/// A capped session takes at least as long as its cap allows.
	#[tokio::test]
	async fn session_is_held_to_its_caps() {
		let Some((mut relay, port, _dir)) = relay().await else {
			return;
		};
		let mut admissions = relay.admissions().expect("embedded auth");
		let running = tokio::spawn(relay.run());
		for elapsed in super::held_to_its_caps(&Dial::Real(port), &mut admissions, [HOST; 2]).await {
			assert!(elapsed >= LEAST, "{FRAME} bytes took only {elapsed:?}");
		}
		running.abort();
		let _ = running.await;
	}

	#[tokio::test]
	async fn window_opens_once_admitted() {
		let Some((mut relay, port, _dir)) = relay().await else {
			return;
		};
		let mut admissions = relay.admissions().expect("embedded auth");
		let running = tokio::spawn(relay.run());
		super::window_opens_once_admitted(&Dial::Real(port), &mut admissions, HOST).await;
		running.abort();
		let _ = running.await;
	}
}
