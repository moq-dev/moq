//! The relay over QUIC on kio's simulator: each relay accepts on a moq-tokio
//! [`Server`](moq_tokio::Server) bound on a simulated host, its peers dial over moq-tokio
//! [`Client`](moq_tokio::Client)s bound there too, and every datagram crosses a seeded,
//! impaired path. No sockets and no host clock.

use std::{
	net::{IpAddr, Ipv4Addr, SocketAddr},
	time::Duration,
};

use moq_relay::{Connection, auth, cluster};
use moq_tokio::quic::driver::sim::{Link, Network, Path};

use crate::memory;

const PORT: u16 = 443;
/// Where each client binds, on its own host
const CLIENT_PORT: u16 = 4433;
const PUBLISHER: IpAddr = ip(2);
const SUBSCRIBER: IpAddr = ip(3);

/// The groups the publisher writes, one every [`INTERVAL`]
const GROUPS: u64 = 20;
const INTERVAL: Duration = Duration::from_millis(100);
const FRAME: usize = 4_000;

/// How long a run may take before it counts as stalled, in simulated time
const TIMEOUT: Duration = Duration::from_secs(60);

const fn ip(last: u8) -> IpAddr {
	IpAddr::V4(Ipv4Addr::new(10, 0, 0, last))
}

/// A publisher and a subscriber on either side of one relay, across lossy paths.
#[test]
fn one_relay() {
	memory::run(|| async {
		for seed in 0..2 {
			let network = Network::new(seed);
			let relay = SocketAddr::new(ip(1), PORT);
			let auth = memory::public(&["**"], &[], &[]);
			let cluster = memory::start(memory::cluster(), moq_tokio::connect::Config::default()).await;
			let relay = serve(&network, relay, &cluster, &auth).await;
			for client in [PUBLISHER, SUBSCRIBER] {
				lossy(&network, SocketAddr::new(client, CLIENT_PORT), relay.addr);
			}

			let groups = kio::clock::timeout(TIMEOUT, deliver(&network, &relay, &relay))
				.await
				.unwrap_or_else(|_| panic!("seed {seed}: the track never finished"));
			assert_eq!(groups, (0..GROUPS).collect::<Vec<_>>(), "seed {seed}");
		}
	});
}

/// A publisher on relay A and a subscriber on relay B, which dials A as its cluster peer over
/// QUIC, every hop across a lossy path.
#[test]
fn two_relays() {
	memory::run(|| async {
		let network = Network::new(1);
		let auth = memory::public(&["**"], &[], &[]);

		let cluster_a = memory::start(memory::cluster(), moq_tokio::connect::Config::default()).await;
		let a = serve(&network, SocketAddr::new(ip(1), PORT), &cluster_a, &auth).await;

		let b = SocketAddr::new(ip(4), PORT);
		let mut config = cluster::Config::default();
		config.connect = vec![cluster::Peer::new(format!("moqt://{}/", a.addr))];
		let mut cache = moq_relay::cache::Config::default();
		// Past what the pool counts in milliseconds, so it never expires idle groups.
		cache.duration = Some(Duration::MAX.into());
		let mut options = cluster::Options::new(config).with_cache(cache.init().expect("cache"));
		options.sim = Some(network.host(b.ip()));
		// A fixed port, which the static peer dials share, so the test can impair that path.
		let peer = SocketAddr::new(b.ip(), CLIENT_PORT);
		let mut connect = moq_tokio::connect::Config::default();
		connect.bind = Some(peer);
		connect.tls.fingerprint = a.fingerprints.clone();
		let cluster_b = memory::start(memory::spawn_cluster(options), connect).await;
		let b = serve(&network, b, &cluster_b, &auth).await;

		lossy(&network, peer, a.addr);
		lossy(&network, SocketAddr::new(PUBLISHER, CLIENT_PORT), a.addr);
		lossy(&network, SocketAddr::new(SUBSCRIBER, CLIENT_PORT), b.addr);

		let groups = kio::clock::timeout(TIMEOUT, deliver(&network, &a, &b))
			.await
			.expect("the track never finished");
		assert_eq!(groups, (0..GROUPS).collect::<Vec<_>>());
	});
}

/// A relay serving on a simulated host
struct Relay {
	addr: SocketAddr,
	fingerprints: Vec<String>,
}

/// Serve `cluster` on `addr` through `auth`: each session the server accepts runs a relay
/// [`Connection`] on its own simulator task.
async fn serve(network: &Network, addr: SocketAddr, cluster: &cluster::Cluster, auth: &auth::Auth) -> Relay {
	let _ = rind_rustls::default_provider().unwrap().install_default();
	let mut config = moq_tokio::server::Config::default();
	config.listen.bind = Some(moq_tokio::listen::Bind::Addr(addr));
	config.listen.tls.generate = vec!["localhost".into()];
	config.sim = Some(network.host(addr.ip()));
	let mut listener = config.init().expect("init server").listen().await.expect("listen");
	let fingerprints = listener.certificates().fingerprints();
	let (cluster, auth) = (cluster.clone(), auth.clone());
	drop(kio::clock::sim::spawn(async move {
		while let Ok(request) = listener.accept().await {
			let (cluster, auth) = (cluster.clone(), auth.clone());
			drop(kio::clock::sim::spawn(Connection::new(request, cluster, auth).run()));
		}
	}));
	Relay { addr, fingerprints }
}

/// Publish [`GROUPS`] groups to `publish` from [`PUBLISHER`], subscribe from `subscribe` on
/// [`SUBSCRIBER`], and return the sequence of every group that arrived whole.
async fn deliver(network: &Network, publish: &Relay, subscribe: &Relay) -> Vec<u64> {
	let origin = moq_tokio::origin::spawn();
	let broadcast = origin
		.route("live", moq_net::origin::RouteOptions::default())
		.expect("route");
	let track = broadcast
		.broadcast()
		.create_track(
			"video",
			moq_net::track::Info::default()
				.with_timescale(moq_net::Timescale::MILLI)
				.with_max_age(None),
		)
		.expect("track");
	broadcast.announce().expect("announce");
	let _publisher = client(network, PUBLISHER, publish)
		.with_publisher(&origin)
		.connect(url(publish));

	let viewer = moq_tokio::origin::spawn();
	let _subscriber = client(network, SUBSCRIBER, subscribe)
		.with_subscriber(viewer.clone())
		.connect(url(subscribe));

	let write = async {
		let mut source = track.clone();
		while source.subscription_changed().await.unwrap().is_none() {}
		for sequence in 0..GROUPS {
			let mut group = source.create_group(moq_net::group::Info { sequence }).unwrap();
			let timestamp = moq_net::Timestamp::from_millis(sequence * INTERVAL.as_millis() as u64).unwrap();
			group.write_frame(timestamp, vec![sequence as u8; FRAME]).unwrap();
			group.finish().unwrap();
			kio::clock::sleep(INTERVAL).await;
		}
		let _source = source.finish().unwrap();
	};
	let read = async {
		let remote = loop {
			match viewer.consume().request("live", None).await {
				Ok(remote) => break remote,
				// Not announced across the relays yet.
				Err(_) => kio::clock::sleep(Duration::from_millis(50)).await,
			}
		};
		let mut reader = remote
			.track("video")
			.unwrap()
			// A budget spanning the run, so recovery has to deliver every group, however late.
			.subscribe(
				moq_net::track::Subscription::default().with_delay(moq_net::track::Delay::default().with_max(TIMEOUT)),
			)
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
	};
	let ((), groups) = futures::join!(write, read);
	drop(broadcast);
	groups
}

/// A client on the host at `ip`, trusting `relay`'s certificate
fn client(network: &Network, ip: IpAddr, relay: &Relay) -> moq_tokio::Client {
	let mut connect = moq_tokio::connect::Config::default();
	connect.bind = Some(SocketAddr::new(ip, CLIENT_PORT));
	connect.tls.fingerprint = relay.fingerprints.clone();
	let mut config = moq_tokio::client::Config::default();
	config.connect = connect;
	config.sim = Some(network.host(ip));
	config.init().expect("init client")
}

fn url(relay: &Relay) -> url::Url {
	format!("moqt://{}/", relay.addr).parse().unwrap()
}

/// Make both directions between `a` and `b` lossy, with latency
fn lossy(network: &Network, a: SocketAddr, b: SocketAddr) {
	let mut link = Link::default();
	link.loss = 0.05;
	link.duplicate = 0.01;
	link.jitter = Duration::from_millis(5);
	link.reorder = 0.02;
	link.reorder_delay = Duration::from_millis(20);
	let path = Path {
		latency: Duration::from_millis(20),
		link,
	};
	network.path(a, b, |p| *p = path.clone());
	network.path(b, a, |p| *p = path);
}
