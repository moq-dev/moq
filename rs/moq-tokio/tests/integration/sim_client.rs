//! moq-tokio's [`Client`](moq_tokio::Client) and [`Server`](moq_tokio::Server) over QUIC on kio's
//! simulator: each binds its endpoint on a simulated host instead of a UDP socket, so the whole
//! dial, from the Happy Eyeballs race through the reconnect loop, runs with no sockets and no
//! host clock.

#![cfg(feature = "quic")]

use std::{
	net::{IpAddr, Ipv4Addr, SocketAddr},
	time::Duration,
};

use kio::clock::Instant;
use moq_net::{Timescale, Timestamp, track};
use moq_tokio::quic::driver::sim::{Link, Network, Path};

const CLIENT: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));
const PORT: u16 = 443;

/// The groups the publisher writes, one every [`INTERVAL`]
const GROUPS: u64 = 20;
const INTERVAL: Duration = Duration::from_millis(100);
const FRAME: usize = 4_000;

/// How long a run may take before it counts as stalled, in simulated time
const TIMEOUT: Duration = Duration::from_secs(60);

fn server_addr(last: u8) -> SocketAddr {
	SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, last)), PORT)
}

/// A publisher dials a server over raw QUIC and over WebTransport, across a lossy path, and the
/// server receives every group whole.
#[test]
fn transports() {
	for (scheme, transport) in [
		("moqt", moq_tokio::Transport::Quic),
		("https", moq_tokio::Transport::WebTransport),
	] {
		for seed in 0..2 {
			kio::clock::sim::run(async move {
				let network = Network::new(seed);
				let server = server_addr(1);
				let mut server = Server::start(&network, server).await;
				let (connection, publisher) =
					publish(&network, &[server.addr], scheme, &server.fingerprints, |client| {
						let lossy = Path {
							latency: Duration::from_millis(30),
							link: lossy_link(),
						};
						network.path(client, server.addr, |path| *path = lossy.clone());
						network.path(server.addr, client, |path| *path = lossy);
					});
				let groups = kio::clock::timeout(TIMEOUT, async {
					let session = server.sessions.recv().await.expect("the server stopped");
					let ((), groups) = futures::join!(publisher.run(), read(&server.origin));
					drop(session);
					groups
				})
				.await
				.unwrap_or_else(|_| panic!("{scheme} seed {seed}: the track never finished"));
				assert_eq!(groups, (0..GROUPS).collect::<Vec<_>>(), "{scheme} seed {seed}");
				assert_eq!(connection.transport(), Some(transport), "{scheme} seed {seed}");
			});
		}
	}
}

/// Happy Eyeballs across two addresses: the first answers over a path whose handshake takes
/// seconds, so the client dials the second after the race delay, and that one wins.
#[test]
fn happy_eyeballs() {
	kio::clock::sim::run(async {
		let network = Network::new(1);
		let (slow, fast) = (server_addr(1), server_addr(3));
		let mut slow_server = Server::start(&network, slow).await;
		let mut fast_server = Server::start(&network, fast).await;
		let mut fingerprints = slow_server.fingerprints.clone();
		fingerprints.extend(fast_server.fingerprints.clone());

		let start = Instant::now();
		let (connection, _publisher) = publish(&network, &[slow, fast], "moqt", &fingerprints, |client| {
			for server in [slow, fast] {
				let latency = match server == slow {
					true => Duration::from_secs(2),
					false => Duration::from_millis(20),
				};
				let path = Path {
					latency,
					link: Link::default(),
				};
				network.path(client, server, |p| *p = path.clone());
				network.path(server, client, |p| *p = path);
			}
		});
		let connection = kio::clock::timeout(TIMEOUT, connection.established())
			.await
			.expect("never connected")
			.expect("the dial failed");
		let elapsed = Instant::now() - start;
		// The second address is dialed only once the race delay lapses, then needs a handshake.
		assert!(
			elapsed >= Duration::from_millis(250) && elapsed < Duration::from_secs(2),
			"connected after {elapsed:?}"
		);
		let _session = fast_server.sessions.recv().await.expect("the fast server stopped");
		// The slow handshake never finished before the winner's cancelled it.
		kio::clock::sleep(Duration::from_secs(10)).await;
		assert!(
			slow_server.sessions.try_recv().is_err(),
			"the slow address also connected"
		);
		assert_eq!(connection.connects(), 1);
	});
}

/// The server restarts on the same address: the client's reconnect loop redials it over QUIC
/// and the broadcast reaches the new server.
#[test]
fn reconnect() {
	kio::clock::sim::run(async {
		let network = Network::new(1);
		let addr = server_addr(1);
		// One identity across the restart, so the pinned fingerprint still matches.
		let tls = identity();
		let mut server = Server::with_identity(&network, addr, tls.clone()).await;
		let (connection, publisher) = publish(&network, &[addr], "moqt", &server.fingerprints.clone(), |_| {});
		let _publisher = publisher;
		let session = kio::clock::timeout(TIMEOUT, server.sessions.recv())
			.await
			.expect("never connected")
			.expect("the server stopped");
		announced(&server.origin).await;
		assert_eq!(connection.connects(), 1);

		server.stop().await;
		drop(session);
		let mut server = Server::with_identity(&network, addr, tls).await;
		let _session = kio::clock::timeout(TIMEOUT, server.sessions.recv())
			.await
			.unwrap_or_else(|_| panic!("never reconnected"))
			.expect("the server stopped");
		announced(&server.origin).await;
		assert_eq!(connection.connects(), 2);
	});
}

/// A server accepting on a simulated host, collecting what its sessions publish
struct Server {
	addr: SocketAddr,
	fingerprints: Vec<String>,
	origin: moq_net::origin::Producer,
	sessions: futures::channel::mpsc::UnboundedReceiver<moq_net::Session>,
	stop: Option<futures::channel::oneshot::Sender<()>>,
	stopped: Option<futures::channel::oneshot::Receiver<()>>,
}

/// A fresh served identity for `localhost`
fn identity() -> moq_tokio::tls::Serve {
	let _ = rind_rustls::default_provider().unwrap().install_default();
	let mut listen = moq_tokio::tls::Listen::default();
	listen.generate = vec!["localhost".into()];
	listen.load().expect("generate identity")
}

impl Server {
	/// A server with a fresh identity
	async fn start(network: &Network, addr: SocketAddr) -> Self {
		Self::with_identity(network, addr, identity()).await
	}

	async fn with_identity(network: &Network, addr: SocketAddr, tls: moq_tokio::tls::Serve) -> Self {
		let mut config = moq_tokio::server::Config::default();
		config.listen.bind = Some(moq_tokio::listen::Bind::Addr(addr));
		config.tls = Some(tls);
		config.sim = Some(network.host(addr.ip()));
		let mut listener = config.init().expect("init server").listen().await.expect("listen");
		assert_eq!(listener.local_addr().expect("bound"), addr);
		let fingerprints = listener.certificates().fingerprints();

		let origin = moq_tokio::origin::spawn();
		let (accepted, sessions) = futures::channel::mpsc::unbounded();
		let (stop, mut stopping) = futures::channel::oneshot::channel();
		let (done, stopped) = futures::channel::oneshot::channel();
		let subscriber = origin.clone();
		drop(kio::clock::sim::spawn(async move {
			loop {
				futures::select_biased! {
					_ = stopping => break,
					request = futures::FutureExt::fuse(listener.accept()) => {
						let Ok(request) = request else { break };
						if let Ok(session) = request.with_subscriber(subscriber.clone()).ok().await {
							let _ = accepted.unbounded_send(session);
						}
					}
				}
			}
			listener.close().await;
			let _ = done.send(());
		}));
		Self {
			addr,
			fingerprints,
			origin,
			sessions,
			stop: Some(stop),
			stopped: Some(stopped),
		}
	}

	/// Close every connection and release the address.
	async fn stop(&mut self) {
		let _ = self.stop.take().expect("running").send(());
		let _ = self.stopped.take().expect("running").await;
	}
}

/// A publisher on [`CLIENT`] dialing `servers` in order over `scheme`, trusting `fingerprints`.
///
/// The client binds a fixed address, which `paths` is handed to set the paths from and to it.
fn publish(
	network: &Network,
	servers: &[SocketAddr],
	scheme: &str,
	fingerprints: &[String],
	paths: impl FnOnce(SocketAddr),
) -> (moq_tokio::Connection, Publisher) {
	let mut connect = moq_tokio::connect::Config::default();
	connect.bind = Some(SocketAddr::new(CLIENT, 4433));
	connect.tls.fingerprint = fingerprints.to_vec();
	let mut config = moq_tokio::client::Config::default();
	config.connect = connect;
	config.sim = Some(network.host(CLIENT));
	paths(SocketAddr::new(CLIENT, 4433));

	let origin = moq_tokio::origin::spawn();
	let broadcast = origin
		.route("live", moq_net::origin::RouteOptions::default())
		.expect("route");
	let track = broadcast
		.broadcast()
		.create_track(
			"video",
			track::Info::default()
				.with_timescale(Timescale::MILLI)
				.with_max_age(None),
		)
		.expect("track");
	broadcast.announce().expect("announce");

	let url: url::Url = format!("{scheme}://localhost:{PORT}/").parse().unwrap();
	let addr = moq_tokio::connect::Addr::pinned(url, servers.iter().copied()).expect("pinned");
	let client = config.init().expect("init client").with_publisher(&origin);
	let connection = client.connect(addr);
	(
		connection,
		Publisher {
			_broadcast: broadcast,
			track,
		},
	)
}

struct Publisher {
	_broadcast: moq_net::origin::Route,
	track: moq_net::track::Producer,
}

impl Publisher {
	/// Write [`GROUPS`] one-frame groups once someone subscribes, then finish the track.
	async fn run(&self) {
		let mut source = self.track.clone();
		while source.subscription_changed().await.unwrap().is_none() {}
		for sequence in 0..GROUPS {
			let mut group = source.create_group(moq_net::group::Info { sequence }).unwrap();
			let timestamp = Timestamp::from_millis(sequence * INTERVAL.as_millis() as u64).unwrap();
			group.write_frame(timestamp, vec![sequence as u8; FRAME]).unwrap();
			group.finish().unwrap();
			kio::clock::sleep(INTERVAL).await;
		}
		let _source = source.finish().unwrap();
	}
}

/// Wait until `origin` holds the publisher's broadcast.
async fn announced(origin: &moq_net::origin::Producer) {
	let mut announced = origin.consume().announced();
	let present = async {
		while let Some(event) = announced.next().await {
			if let moq_net::announce::Event::Present(route) = event
				&& route.prefix.as_str() == "live"
			{
				return;
			}
		}
		panic!("the origin closed");
	};
	kio::clock::timeout(TIMEOUT, present).await.expect("never announced");
}

/// Subscribe to the publisher's track through `origin` and return each whole group's sequence.
async fn read(origin: &moq_net::origin::Producer) -> Vec<u64> {
	let remote = origin.consume().request("live", None).await.unwrap();
	let mut reader = remote
		.track("video")
		.unwrap()
		// A budget spanning the run, so recovery has to deliver every group, however late.
		.subscribe(track::Subscription::default().with_delay(track::Delay::default().with_max(TIMEOUT)))
		.await
		.unwrap();
	let mut groups = Vec::new();
	while let Some(mut group) = reader.recv_group().await.unwrap() {
		let sequence = group.sequence;
		let frame = group.read_frame().await.unwrap().expect("a group is cut short");
		assert_eq!(frame.payload.len(), FRAME, "group {sequence} is cut short");
		assert!(
			frame.payload.iter().all(|&byte| byte == sequence as u8),
			"group {sequence} is corrupted"
		);
		groups.push(sequence);
	}
	groups.sort();
	groups
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
