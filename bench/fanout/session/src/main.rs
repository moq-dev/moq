//! Chat fan-out through a relay over the in-memory transport: one publisher session into
//! a relay origin and one viewer session out of it per subscriber, all on one thread. Each
//! cycle writes one 64-byte group that every viewer receives and reads.
//!
//! Usage: `fanout-session VIEWERS CYCLES [VERSION]`. Every session runs on this thread's
//! runtime and the memory transport has no sockets or clock waits, so the retired
//! instructions, less a run of zero cycles, are the per-group cost whatever the host does.
use std::hint::black_box;

use bytes::Bytes;
use moq_net::{Client, Hop, Server, Timestamp, Version, origin, transport::memory};

fn origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	tokio::spawn(moq_net::time::run(driver));
	producer
}

/// Connect `client` to `server` over one in-memory pair, returning both sessions.
async fn link(version: Version, client: Client, server: Server) -> (moq_net::Session, moq_net::Session) {
	let mut config = memory::Config::default();
	config.protocol = Some(version.alpn().to_string());
	let (near, far) = memory::pair(config);
	// Each driver runs as soon as its handshake resolves: a newer draft's accept waits on
	// the client's SETUP, which only its driver sends.
	futures::join!(
		async {
			let (session, driver) = client.connect(kio::clock::now(), near).await.expect("client handshake");
			tokio::spawn(moq_net::time::run(driver));
			session
		},
		async {
			let (session, driver) = server.accept(kio::clock::now(), far).await.expect("server handshake");
			tokio::spawn(moq_net::time::run(driver));
			session
		}
	)
}

fn main() {
	let mut args = std::env::args().skip(1);
	let viewers: u64 = args.next().unwrap().parse().unwrap();
	let cycles: usize = args.next().unwrap().parse().unwrap();
	let version: Version = args.next().as_deref().unwrap_or("moq-lite-06").parse().unwrap();
	let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
	runtime.block_on(async move {
		let relay = origin(1);
		let publisher = origin(2);
		let ingest = link(
			version,
			Client::new().with_versions(version.into()).with_publisher(publisher.consume()),
			Server::new().with_versions(version.into()).with_subscriber(relay.clone()),
		)
		.await;
		let broadcast = publisher.create_broadcast("room").unwrap();
		let info = moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI);
		let mut track = broadcast.create_track("chat", info).unwrap();
		broadcast.announce(Default::default()).unwrap();

		let mut audience = Vec::new();
		for viewer in 0..viewers {
			let local = origin(3 + viewer);
			let sessions = link(
				version,
				Client::new().with_versions(version.into()).with_subscriber(local.clone()),
				Server::new().with_versions(version.into()).with_publisher(relay.consume()),
			)
			.await;
			let remote = local.consume().follow("room", None).unwrap().available().await.expect("broadcast resolves");
			let subscriber = remote.track("chat").unwrap().subscribe(None).await.expect("subscribe");
			audience.push((sessions, local, subscriber));
		}
		while track.subscription_changed().await.expect("track open").is_none() {}

		let payload = Bytes::from(vec![0; 64]);
		// Past the first groups' one-time work, so the zero-cycle run shares it.
		for _ in 0..(16 + cycles) {
			let mut group = track.append_group().unwrap();
			group.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
			group.finish().unwrap();
			for (_, _, subscriber) in &mut audience {
				let mut group = subscriber.recv_group().await.unwrap().expect("a group");
				black_box(group.read_frame().await.unwrap().expect("a frame"));
			}
		}
		black_box((ingest, audience, broadcast));
	});
}
