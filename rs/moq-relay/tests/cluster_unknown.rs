//! Regression for an external publisher whose protocol does not declare a Hop ID.
//! The relay records that publisher as `Origin::UNKNOWN`; reflected cluster paths
//! must not replace it while gossiping around a redundant mesh.

use std::{net::TcpListener, time::Duration};

use moq_net::Origin;
use moq_relay::{Config, PublicConfig, Relay};

const TIMEOUT: Duration = Duration::from_secs(10);
const PATH: &str = "opalin/cell-clumsy-octopus/cameras/left.hang";

fn free_tcp_port() -> u16 {
	TcpListener::bind("127.0.0.1:0")
		.expect("bind probe")
		.local_addr()
		.expect("local addr")
		.port()
}

async fn spawn_relay(
	id: u64,
	connect: Vec<String>,
	cluster_version: Option<moq_net::Version>,
) -> (u16, tokio::task::JoinHandle<()>) {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
	let port = free_tcp_port();

	let mut config = Config::default();
	config.server.tcp.bind = Some(format!("127.0.0.1:{port}").parse().expect("parse bind"));
	config.client.bind = "127.0.0.1:0".parse().expect("parse client bind");
	config.client.tls.disable_verify = Some(true);
	config.client.version.extend(cluster_version);
	#[allow(deprecated)]
	{
		config.auth.public = Some(PublicConfig::Simple(vec![String::new()]));
	}
	config.cluster.id = Some(id);
	config.cluster.connect = connect;

	let relay = Relay::load(config).await.expect("relay load");
	let handle = tokio::spawn(async move {
		let _ = relay.run().await;
	});

	let deadline = std::time::Instant::now() + Duration::from_secs(5);
	loop {
		if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
			break;
		}
		assert!(std::time::Instant::now() < deadline, "relay {id} never became ready");
		tokio::time::sleep(Duration::from_millis(25)).await;
	}

	(port, handle)
}

fn client(version: Option<moq_net::Version>) -> moq_native::Client {
	let mut config = moq_native::ClientConfig::default();
	config.tls.disable_verify = Some(true);
	config.websocket.delay = None;
	config.bind = "127.0.0.1:0".parse().expect("parse bind");
	config.version.extend(version);
	config.init().expect("client init")
}

struct Publisher {
	_track: moq_net::track::Producer,
	_broadcast: moq_net::broadcast::Producer,
	_session: moq_net::Session,
}

async fn publish_unknown(port: u16) -> Publisher {
	let url = format!("tcp://127.0.0.1:{port}").parse().expect("parse url");
	let origin = Origin::random().produce();
	let mut broadcast = origin
		.create_broadcast(PATH, moq_net::broadcast::Route::new().with_announce(true))
		.expect("create broadcast");
	let mut track = broadcast.create_track("video", None).expect("create track");
	let mut group = track.append_group().expect("append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, b"hello".as_ref())
		.expect("write frame");
	group.finish().expect("finish group");

	// Draft-14 has no Cluster extension, so the accepting relay must represent
	// this external publisher with the wire-defined UNKNOWN Hop ID.
	let version = "moq-transport-14".parse().expect("parse version");
	let session = tokio::time::timeout(TIMEOUT, client(Some(version)).with_publisher(&origin).connect(url))
		.await
		.expect("publisher connect timeout")
		.expect("publisher connect failed");

	Publisher {
		_track: track,
		_broadcast: broadcast,
		_session: session,
	}
}

async fn watch_announces(port: u16, window: Duration) -> Vec<(String, bool)> {
	let url = format!("tcp://127.0.0.1:{port}").parse().expect("parse url");
	let origin = Origin::random().produce();
	let mut announced = origin.consume().announced();
	let _session = tokio::time::timeout(TIMEOUT, client(None).with_subscriber(origin).connect(url))
		.await
		.expect("viewer connect timeout")
		.expect("viewer connect failed");

	let mut updates = Vec::new();
	let deadline = tokio::time::Instant::now() + window;
	while let Ok(Some(update)) = tokio::time::timeout_at(deadline, announced.next()).await {
		updates.push((update.path.as_str().to_string(), update.broadcast.is_some()));
	}
	updates
}

async fn assert_unknown_publisher_stays_announced(cluster_version: Option<moq_net::Version>) {
	let (a_port, a) = spawn_relay(11, vec![], cluster_version).await;
	let (b_port, b) = spawn_relay(12, vec![format!("tcp://127.0.0.1:{a_port}")], cluster_version).await;
	let (c_port, c) = spawn_relay(
		13,
		vec![format!("tcp://127.0.0.1:{a_port}"), format!("tcp://127.0.0.1:{b_port}")],
		cluster_version,
	)
	.await;
	let (d_port, d) = spawn_relay(14, vec![format!("tcp://127.0.0.1:{c_port}")], cluster_version).await;

	tokio::time::sleep(Duration::from_millis(500)).await;
	let watcher = tokio::spawn(watch_announces(d_port, Duration::from_secs(8)));
	// Attach the viewer before publishing so a short-lived announce is observed.
	tokio::time::sleep(Duration::from_millis(250)).await;
	let publisher = publish_unknown(a_port).await;
	let updates = watcher.await.expect("viewer task");

	drop(publisher);
	a.abort();
	b.abort();
	c.abort();
	d.abort();

	let announces = updates.iter().filter(|(_, active)| *active).count();
	let unannounces = updates.iter().filter(|(_, active)| !*active).count();
	assert!(announces >= 1, "the broadcast never reached the viewer: {updates:?}");
	assert_eq!(
		unannounces, 0,
		"the broadcast flapped while its publisher stayed up: {updates:?}"
	);
}

#[tokio::test]
async fn unknown_publisher_does_not_flap_across_a_lite_cluster_triangle() {
	assert_unknown_publisher_stays_announced(None).await;
}

#[tokio::test]
async fn unknown_publisher_does_not_flap_across_an_ietf_cluster_triangle() {
	let version = "moq-transport-19".parse().expect("parse version");
	assert_unknown_publisher_stays_announced(Some(version)).await;
}
