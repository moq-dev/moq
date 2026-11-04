//! A permanently dropped UDP flow must not strand a reconnecting client.
#![cfg(feature = "quic")]

use std::{
	collections::{HashMap, HashSet},
	net::SocketAddr,
	sync::{Arc, Mutex},
	time::Duration,
};

use moq_tokio::{Client, Connection, moq_net};
use tokio::{net::UdpSocket, sync::mpsc};

const DIAL: Duration = Duration::from_secs(1);

/// Resume only around real I/O; failed dials expire on the paused clock.
async fn socket_step<T>(future: impl std::future::Future<Output = T>) -> T {
	tokio::time::resume();
	let result = kio::clock::timeout(Duration::from_secs(10), future)
		.await
		.expect("socket operation stalled");
	tokio::time::pause();
	result
}

async fn peer() -> (SocketAddr, mpsc::UnboundedReceiver<(SocketAddr, moq_net::Session)>) {
	let mut config = moq_tokio::listen::Config::default();
	config.bind = Some("127.0.0.1:0".parse().unwrap());
	config.tls.generate = vec!["localhost".into()];
	let mut listener = config.init(Default::default()).unwrap().listen().await.unwrap();
	let addr = listener.local_addr().unwrap();
	let (send, recv) = mpsc::unbounded_channel();
	tokio::spawn(async move {
		while let Ok(request) = listener.accept().await {
			let remote = request.remote_addr().unwrap();
			let session = request.ok().await.unwrap();
			send.send((remote, session)).unwrap();
		}
	});
	(addr, recv)
}

/// The first source is always dropped; other sources get independent upstream sockets,
/// as in the relay's cluster_fresh_port test. Report each source only once.
struct Proxy {
	addr: SocketAddr,
	sources: mpsc::UnboundedReceiver<SocketAddr>,
	blocked: Arc<Mutex<HashSet<SocketAddr>>>,
}

async fn proxy(upstream: SocketAddr) -> Proxy {
	let front = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
	let addr = front.local_addr().unwrap();
	let (send, recv) = mpsc::unbounded_channel();
	let blocked = Arc::new(Mutex::new(HashSet::new()));
	let drop_sources = blocked.clone();
	tokio::spawn(async move {
		let mut heard = HashSet::new();
		let mut flows = HashMap::<SocketAddr, Arc<UdpSocket>>::new();
		let mut buf = vec![0; 65536];
		loop {
			let (len, source) = front.recv_from(&mut buf).await.unwrap();
			if heard.insert(source) {
				if heard.len() == 1 {
					drop_sources.lock().unwrap().insert(source);
				}
				send.send(source).unwrap();
			}
			if drop_sources.lock().unwrap().contains(&source) {
				continue;
			}
			let back = match flows.get(&source) {
				Some(back) => back.clone(),
				None => {
					let back = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
					back.connect(upstream).await.unwrap();
					let (front, reply) = (front.clone(), back.clone());
					tokio::spawn(async move {
						let mut buf = vec![0; 65536];
						loop {
							let len = reply.recv(&mut buf).await.unwrap();
							front.send_to(&buf[..len], source).await.unwrap();
						}
					});
					flows.insert(source, back.clone());
					back
				}
			};
			back.send(&buf[..len]).await.unwrap();
		}
	});
	Proxy {
		addr,
		sources: recv,
		blocked,
	}
}

fn client(bind: SocketAddr) -> Client {
	let mut config = moq_tokio::connect::Config::default();
	config.bind = Some(bind);
	config.tls.insecure = Some(true);
	config.timeout = Some(DIAL.into());
	config.backoff.initial = Some(Duration::ZERO.into());
	config.backoff.timeout = Some(Duration::ZERO.into());
	#[cfg(feature = "websocket")]
	{
		config.websocket.enabled = Some(false);
	}
	config.init(Default::default()).unwrap()
}

fn connect(client: &Client, addr: SocketAddr) -> Connection {
	client.connect(format!("moqt://{addr}/").parse::<url::Url>().unwrap())
}

/// Advance one failed dial and wait until the reconnect loop observes it.
async fn fail(connection: &Connection) {
	let monitor = connection.monitor();
	let before = monitor.state().revision;
	tokio::time::advance(DIAL).await;
	let state = monitor.state_changed(before).await.unwrap();
	assert!(state.failure.is_some());
}

#[tokio::test(start_paused = true)]
async fn a_dropped_flow_recovers_without_moving_a_healthy_connection() {
	let (peer, mut sessions) = peer().await;
	let client = client("127.0.0.1:0".parse().unwrap());
	let healthy = socket_step(connect(&client, peer).established()).await.unwrap();
	let (healthy_addr, healthy_session) = socket_step(sessions.recv()).await.unwrap();
	let mut proxy = proxy(peer).await;
	let reconnect = connect(&client, proxy.addr);
	let first = socket_step(proxy.sources.recv()).await.unwrap();
	assert_eq!(first, healthy_addr);

	fail(&reconnect).await;
	assert!(proxy.sources.try_recv().is_err(), "one failed dial changed the port");
	fail(&reconnect).await;
	let second = socket_step(proxy.sources.recv()).await.unwrap();
	assert_ne!(first.port(), second.port());
	let reconnect = socket_step(reconnect.established()).await.unwrap();
	let (_, recovered_session) = socket_step(sessions.recv()).await.unwrap();
	assert_eq!(healthy.connects(), 1);
	assert_eq!(healthy.monitor().state().phase, moq_tokio::connection::Phase::Connected);

	// The original client still owns and closes endpoints created by its reconnect loops.
	socket_step(client.close()).await;
	socket_step(healthy_session.closed()).await;
	socket_step(recovered_session.closed()).await;
	socket_step(healthy.closed())
		.await
		.expect_err("a closed endpoint redialed");
	socket_step(reconnect.closed())
		.await
		.expect_err("a fresh endpoint redialed after shutdown");
	drop((healthy, reconnect));
}

#[tokio::test(start_paused = true)]
async fn a_successful_dial_resets_the_failure_count() {
	let (peer, mut sessions) = peer().await;
	let client = client("127.0.0.1:0".parse().unwrap());
	let mut proxy = proxy(peer).await;
	let reconnect = connect(&client, proxy.addr);
	let first = socket_step(proxy.sources.recv()).await.unwrap();
	fail(&reconnect).await;
	proxy.blocked.lock().unwrap().remove(&first);
	let reconnect = socket_step(reconnect.established()).await.unwrap();
	let (_, session) = socket_step(sessions.recv()).await.unwrap();

	proxy.blocked.lock().unwrap().insert(first);
	drop(session);
	let monitor = reconnect.monitor();
	let revision = monitor.state().revision;
	socket_step(monitor.state_changed(revision)).await.unwrap();
	fail(&reconnect).await;
	proxy.blocked.lock().unwrap().remove(&first);
	let reconnect = socket_step(reconnect.established()).await.unwrap();
	let (_, _session) = socket_step(sessions.recv()).await.unwrap();
	assert!(
		proxy.sources.try_recv().is_err(),
		"success did not reset the failed-dial count"
	);
	assert_eq!(reconnect.connects(), 2);
	socket_step(client.close()).await;
}

#[tokio::test(start_paused = true)]
async fn a_fixed_bind_port_survives_consecutive_failed_dials() {
	let (peer, _sessions) = peer().await;
	let reserved = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
	let bind = reserved.local_addr().unwrap();
	drop(reserved);
	let client = client(bind);
	let mut proxy = proxy(peer).await;
	let reconnect = connect(&client, proxy.addr);
	assert_eq!(socket_step(proxy.sources.recv()).await.unwrap(), bind);
	for _ in 0..4 {
		fail(&reconnect).await;
	}
	assert!(proxy.sources.try_recv().is_err(), "a fixed bind port changed");
	assert_eq!(reconnect.connects(), 0);
	drop(reconnect);
	socket_step(client.close()).await;
}
