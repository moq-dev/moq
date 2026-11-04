//! A mesh dial whose UDP flow is dropped recovers from a fresh source port.
//!
//! Live, a provider path dropped every packet of one flow to a peer for a day
//! while fresh source ports to the same peer handshook, and the relay kept
//! redialing from its one port. Here a proxy drops everything from the first
//! source port it sees, so the link only comes up once a redial leaves from
//! another.
#![cfg(feature = "quic")]

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use moq_relay::cluster::{self, Peer};
use tokio::net::UdpSocket;

/// Slack on top of the reconnect loop's budget for the link to come back: the
/// last dial timeout, the redial's first jittered wait, and a slow runner. A dial
/// that never changes port never connects at all.
const SLACK: Duration = Duration::from_secs(10);

/// Multi-transport client and session types make the test future large; give it a
/// big stack, as `cluster_idle.rs` does.
fn run_cluster_test<F>(fut: F)
where
	F: std::future::Future<Output = ()> + Send + 'static,
{
	std::thread::Builder::new()
		.stack_size(32 * 1024 * 1024)
		.spawn(move || {
			tokio::runtime::Builder::new_current_thread()
				.enable_all()
				.build()
				.expect("build test runtime")
				.block_on(fut);
		})
		.expect("spawn test thread")
		.join()
		.expect("test thread panicked");
}

/// A peer announcing one broadcast.
async fn spawn_peer() -> SocketAddr {
	let origin = moq_tokio::origin::spawn();
	let broadcast = origin.create_broadcast("peer").expect("create broadcast");
	broadcast.announce(Default::default()).expect("announce");

	let mut config = moq_tokio::listen::Config::default();
	config.bind = Some("127.0.0.1:0".parse().expect("parse bind"));
	config.tls.generate = vec!["localhost".into()];
	let mut listener = config
		.init(Default::default())
		.expect("peer init")
		.listen()
		.await
		.expect("peer listen");
	let addr = listener.local_addr().expect("peer addr");

	tokio::spawn(async move {
		let _broadcast = broadcast;
		while let Ok(request) = listener.accept().await {
			let origin = origin.clone();
			tokio::spawn(async move {
				if let Ok(session) = request.with_publisher(&origin).ok().await {
					let _ = session.closed().await;
				}
			});
		}
	});
	addr
}

/// A UDP relay to `upstream` that drops every datagram from the first source
/// address it hears, and forwards every other flow both ways through its own
/// upstream socket. Returns its address and every source it has heard.
async fn spawn_proxy(upstream: SocketAddr) -> (SocketAddr, Arc<Mutex<HashSet<SocketAddr>>>) {
	let front = Arc::new(UdpSocket::bind("127.0.0.1:0").await.expect("proxy bind"));
	let addr = front.local_addr().expect("proxy addr");
	let sources = Arc::new(Mutex::new(HashSet::new()));

	let heard = sources.clone();
	tokio::spawn(async move {
		let mut dropped = None;
		let mut flows: HashMap<SocketAddr, Arc<UdpSocket>> = HashMap::new();
		let mut buf = vec![0u8; 65536];
		loop {
			let Ok((len, source)) = front.recv_from(&mut buf).await else {
				return;
			};
			heard.lock().unwrap().insert(source);
			if *dropped.get_or_insert(source) == source {
				continue;
			}

			let back = match flows.get(&source) {
				Some(back) => back.clone(),
				None => {
					let back = Arc::new(UdpSocket::bind("127.0.0.1:0").await.expect("flow bind"));
					back.connect(upstream).await.expect("flow connect");
					// Replies from the peer go back to this flow's source.
					let (front, reply) = (front.clone(), back.clone());
					tokio::spawn(async move {
						let mut buf = vec![0u8; 65536];
						while let Ok(len) = reply.recv(&mut buf).await {
							let _ = front.send_to(&buf[..len], source).await;
						}
					});
					flows.insert(source, back.clone());
					back
				}
			};
			let _ = back.send(&buf[..len]).await;
		}
	});
	(addr, sources)
}

#[test]
fn a_dropped_flow_recovers_on_a_fresh_port() {
	let budget = Duration::from_secs(1);
	run_cluster_test(dropped_flow_recovers(budget, budget));
}

/// A zero budget would retry the dropped flow forever, so a mesh dial takes
/// moq-tokio's 10s default instead.
#[test]
fn an_unbounded_budget_still_leaves_the_dropped_port() {
	run_cluster_test(dropped_flow_recovers(Duration::ZERO, Duration::from_secs(10)));
}

/// Dial through a proxy that drops the first flow, with `budget` as the
/// configured backoff timeout and `effective` as the one a mesh dial runs.
async fn dropped_flow_recovers(budget: Duration, effective: Duration) {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let peer = spawn_peer().await;
	let (proxy, sources) = spawn_proxy(peer).await;

	let mut connect = moq_tokio::connect::Config::default();
	connect.bind = Some("127.0.0.1:0".parse().expect("parse bind"));
	connect.tls.insecure = Some(true);
	// A short dial timeout, so each attempt on the blackholed flow fails fast.
	connect.timeout = Some(Duration::from_millis(500).into());
	connect.backoff.initial = Some(Duration::from_millis(100).into());
	connect.backoff.timeout = Some(budget.into());
	#[cfg(feature = "websocket")]
	{
		connect.websocket.enabled = Some(false);
	}

	let mut config = cluster::Config::default();
	config.connect = vec![Peer::new(format!("https://{proxy}/"))];
	let cluster = crate::spawn_cluster(cluster::Options::new(config)).with_connect(connect, Default::default());
	let mut announced = cluster.origin.consume().announced();
	let run = tokio::spawn(cluster.clone().start().await.expect("cluster start").run());

	kio::clock::timeout(effective + SLACK, async {
		loop {
			let event = announced.next().await.expect("origin closed");
			if let moq_net::announce::Event::Start(update) | moq_net::announce::Event::Update(update) = event
				&& update.prefix.as_str() == "peer"
			{
				break;
			}
		}
	})
	.await
	.expect("the link never came back on another source port");

	let heard = sources.lock().unwrap().len();
	assert!(
		heard >= 2,
		"the link came up without leaving the dropped port ({heard} source)"
	);

	run.abort();
}
