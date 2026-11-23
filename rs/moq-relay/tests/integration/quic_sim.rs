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

pub(crate) const PORT: u16 = 443;
/// Where each client binds, on its own host
const CLIENT_PORT: u16 = 4433;
pub(crate) const PUBLISHER: IpAddr = ip(2);
pub(crate) const SUBSCRIBER: IpAddr = ip(3);

/// The groups the publisher writes, one every [`INTERVAL`]
const GROUPS: u64 = 20;
const INTERVAL: Duration = Duration::from_millis(100);
const FRAME: usize = 4_000;

/// How long a run may take before it counts as stalled, in simulated time
const TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) const fn ip(last: u8) -> IpAddr {
	IpAddr::V4(Ipv4Addr::new(10, 0, 0, last))
}

/// A publisher and a subscriber on either side of one relay, across lossy paths.
#[test]
fn one_relay() {
	memory::run(|| async {
		for seed in 0..2 {
			let network = Network::new(seed);
			let relay = SocketAddr::new(ip(1), PORT);
			let auth = memory::public(&[""], &[], &[]);
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
		let auth = memory::public(&[""], &[], &[]);

		let cluster_a = memory::start(memory::cluster(), moq_tokio::connect::Config::default()).await;
		let a = serve(&network, SocketAddr::new(ip(1), PORT), &cluster_a, &auth).await;

		let b = SocketAddr::new(ip(4), PORT);
		let mut config = cluster::Config::default();
		config.connect = vec![cluster::Peer::new(format!("moqt://{}/", a.addr))];
		let mut cache = moq_relay::cache::Config::default();
		// Past what the pool counts in milliseconds, so it never expires idle groups.
		cache.duration = Some(Duration::MAX.into());
		// And no governor, whose timer would never let the simulation settle.
		cache.headroom = Some("0".to_string());
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
pub(crate) struct Relay {
	pub(crate) addr: SocketAddr,
	pub(crate) fingerprints: Vec<String>,
}

/// Serve `cluster` on `addr` through `auth`: each session the server accepts runs a relay
/// [`Connection`] on its own simulator task.
pub(crate) async fn serve(network: &Network, addr: SocketAddr, cluster: &cluster::Cluster, auth: &auth::Auth) -> Relay {
	let (mut listener, relay) = listen(network, addr).await;
	let (cluster, auth) = (cluster.clone(), auth.clone());
	drop(kio::clock::sim::spawn(async move {
		while let Ok(request) = listener.accept().await {
			let (cluster, auth) = (cluster.clone(), auth.clone());
			drop(kio::clock::sim::spawn(Connection::new(request, cluster, auth).run()));
		}
	}));
	relay
}

/// A QUIC listener on `addr`, with a generated certificate
async fn listen(network: &Network, addr: SocketAddr) -> (moq_tokio::Listener, Relay) {
	let _ = rind_rustls::default_provider().unwrap().install_default();
	let mut config = moq_tokio::server::Config::default();
	config.listen.bind = Some(moq_tokio::listen::Bind::Addr(addr));
	config.listen.tls.generate = vec!["localhost".into()];
	config.sim = Some(network.host(addr.ip()));
	let listener = config.init().expect("init server").listen().await.expect("listen");
	let fingerprints = listener.certificates().fingerprints();
	(listener, Relay { addr, fingerprints })
}

/// A drain deadline does not wait for a WebTransport peer that vanished to acknowledge
/// the close capsule, which it never will, until the idle timeout. The listener closes
/// after the drain, as `Relay::run`'s does.
#[test]
fn a_drain_waits_on_no_vanished_peer() {
	memory::run(|| async {
		let network = Network::new(0);
		let addr = SocketAddr::new(ip(1), PORT);
		let auth = memory::public(&[""], &[], &[]);
		let cluster = memory::start(memory::cluster(), moq_tokio::connect::Config::default()).await;
		let (trigger, observer) = moq_relay::shutdown::Observer::new(Duration::ZERO);
		let (mut listener, relay) = listen(&network, addr).await;
		let served = kio::clock::sim::spawn(async move {
			let accept = async {
				while let Ok(request) = listener.accept().await {
					let connection =
						Connection::new(request, cluster.clone(), auth.clone()).with_shutdown(observer.clone());
					drop(kio::clock::sim::spawn(connection.run()));
				}
			};
			futures::future::select(std::pin::pin!(accept), std::pin::pin!(observer.drained())).await;
			listener.close().await;
		});

		let url: url::Url = format!("https://{addr}/").parse().unwrap();
		let _vanished = client(&network, PUBLISHER, &relay)
			.with_reconnect(false)
			.connect(url)
			.established()
			.await
			.expect("connect");
		let gone = SocketAddr::new(PUBLISHER, CLIENT_PORT);
		for (from, to) in [(gone, addr), (addr, gone)] {
			network.path(from, to, |path| path.link.loss = 1.0);
		}

		trigger.start();
		kio::clock::timeout(Duration::from_secs(1), served)
			.await
			.expect("the drain waited on the vanished peer")
			.unwrap();
	});
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
			let mut group = source.create_group(moq_net::group::Info { start: sequence }).unwrap();
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
			let sequence = group.start;
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

/// The bytes the bulk transfer carries in one group: past the 32 MB live tail, so a hop
/// that sheds rather than paces loses some
const BULK: usize = 40 * 1024 * 1024;
/// The size of each frame of [`BULK`]
const CHUNK: usize = 8 * 1024;

/// A group carries a bulk transfer through a relay without loss: the writer waits on `ready`
/// before each frame, and the reader drains at its own pace, slower than the writer, so
/// every hop has to pace its ingest to that reader rather than shed.
///
/// A paused stream must hold no more than the credit its session reserves for it, or the
/// connection window fills with bytes nobody takes and the transport evicts the stream.
#[test]
fn a_group_carries_a_bulk_transfer_losslessly() {
	memory::run(|| bulk(1, CHUNK, BULK));
}

/// Many groups on one connection each carry a bulk transfer without loss, every one read
/// slower than it is written, so every hop has all of them paused at once. A group that
/// could not pause would read on and shed past the live tail before its reader got far.
#[test]
fn concurrent_groups_carry_bulk_transfers_losslessly() {
	memory::run(|| bulk(SESSIONS, 64 * 1024, 1024 * 1024));
}

/// [`concurrent_groups_carry_bulk_transfers_losslessly`] at [`MANY`] sessions, past what any
/// fixed credit per paused stream fits in half the connection window. Every one still
/// pauses, since a paused stream holds a share of that half set by its slot, not by how many
/// pause.
///
/// Their read-ahead together passes the relay session's live-edge budget, which sheds the
/// lowest priorities first: only the [`LOSSLESS`] highest must arrive whole.
#[test]
#[ignore = "a hundred bulk transfers, about a minute in debug: run in release by `just rs quic-sim`"]
fn many_groups_carry_their_highest_priorities_losslessly() {
	memory::run(|| bulk(MANY, 64 * 1024, 1024 * 1024));
}

/// How many bulk transfers share one connection in
/// [`concurrent_groups_carry_bulk_transfers_losslessly`]: well past the two that pause when
/// each paused stream holds its whole stream window.
const SESSIONS: usize = 12;

/// How many share one in [`many_groups_carry_their_highest_priorities_losslessly`]. Past a
/// couple of hundred, the frames in flight at once each hold credit until they seal, and that
/// fills the connection window whatever the paused streams hold.
const MANY: usize = 100;

/// How many of the highest-priority tracks [`bulk`] reads whole. A session past its 256 MiB
/// live-edge budget sheds every frame readers took first, and each group's ingest runs at most
/// half the 32 MiB live tail ahead of its reader, so this many fit with room to spare however
/// the rest are shed. Measured, a group holds about 8 MiB ahead, so about 30 survive.
const LOSSLESS: usize = 12;

/// Write `tracks` tracks of one group each, [`BULK`] bytes in `chunk`-byte frames, through
/// one relay, and read the first `read` bytes of each slower than they are written. Track `i`
/// has publisher priority `i - 128`, ordered for up to 256 tracks: the [`LOSSLESS`] highest must arrive whole, and a lower one may
/// end early with an error but never delivers a wrong frame.
async fn bulk(tracks: usize, chunk: usize, read: usize) {
	let network = Network::new(0);
	let auth = memory::public(&[""], &[], &[]);
	let cluster = memory::start(memory::cluster(), moq_tokio::connect::Config::default()).await;
	let relay = serve(&network, SocketAddr::new(ip(1), PORT), &cluster, &auth).await;

	let origin = moq_tokio::origin::spawn();
	let broadcast = origin
		.route("bulk", moq_net::origin::RouteOptions::default())
		.expect("route");
	let sources: Vec<_> = (0..tracks)
		.map(|index| {
			broadcast
				.broadcast()
				.create_track(
					format!("data{index}"),
					moq_net::track::Info::default()
						.with_priority(i8::try_from(index as i64 - 128).expect("at most 256 tracks")),
				)
				.expect("track")
		})
		.collect();
	broadcast.announce().expect("announce");
	let _publisher = client(&network, PUBLISHER, &relay)
		.with_publisher(&origin)
		.connect(url(&relay));

	let viewer = moq_tokio::origin::spawn();
	let _subscriber = client(&network, SUBSCRIBER, &relay)
		.with_subscriber(viewer.clone())
		.connect(url(&relay));

	let write = |mut track: moq_net::track::Producer| async move {
		while track.subscription_changed().await.unwrap().is_none() {}
		let mut group = track.append_group(moq_net::Timestamp::unscaled(0)).unwrap();
		let info = moq_net::frame::Info {
			size: chunk as u64,
			timestamp: moq_net::Timestamp::unscaled(0),
		};
		for index in 0..BULK / chunk {
			group.ready(&info).await;
			group
				.write_frame(moq_net::Timestamp::unscaled(0), vec![index as u8; chunk])
				.unwrap();
			// Other tasks run between frames, as on a real runtime.
			kio::clock::sim::yield_now().await;
		}
		group.finish().unwrap();
		// Held until the reads end: dropping a track unfinished aborts it.
		futures::future::pending::<()>().await;
	};
	let read = |index: usize| {
		let viewer = viewer.clone();
		let name = format!("data{index}");
		let lossless = index + LOSSLESS >= tracks;
		async move {
			let remote = loop {
				match viewer.consume().request("bulk", None).await {
					Ok(remote) => break remote,
					// Not announced through the relay yet.
					Err(_) => kio::clock::sleep(Duration::from_millis(50)).await,
				}
			};
			let consumer = remote
				.track(&name)
				.unwrap()
				.subscribe(
					moq_net::track::Subscription::default()
						.with_delay(moq_net::track::Delay::default().with_max(Duration::from_secs(600))),
				)
				.await
				.unwrap();
			let mut ordered = consumer.ordered();
			let mut group = ordered.next_group().await.unwrap().expect("a group");
			for index in 0..read / chunk {
				let frame = match group.read_frame().await {
					Ok(frame) => frame.unwrap_or_else(|| panic!("{name} ended at frame {index}")),
					// Shed by the budget, which a lower priority may be.
					Err(_) if !lossless => return,
					Err(err) => panic!("{name} frame {index}: {err}"),
				};
				assert_eq!(frame.payload.len(), chunk, "{name} frame {index} is cut short");
				assert!(
					frame.payload.iter().all(|&byte| byte == index as u8),
					"{name} frame {index} is corrupted"
				);
				// A consumer draining at about 64 MB/s, slower than the writer.
				kio::clock::sleep(Duration::from_micros(chunk as u64 / 64)).await;
			}
			// A group read whole ends cleanly right after, never with more or a failure.
			if read == BULK {
				let end = group
					.read_frame()
					.await
					.unwrap_or_else(|err| panic!("{name} end: {err}"));
				assert!(end.is_none(), "{name} carries more than {BULK} bytes");
			}
		}
	};
	let writes = futures::future::join_all(sources.into_iter().map(write));
	let reads = futures::future::join_all((0..tracks).map(read));
	kio::clock::timeout(Duration::from_secs(600), async {
		futures::pin_mut!(writes, reads);
		futures::future::select(writes, reads).await;
	})
	.await
	.expect("the transfer stalled");
}
