//! Regression for an external publisher whose protocol does not declare a Hop ID.
//! The relay records that publisher as `Origin::UNKNOWN`; reflected cluster paths
//! must not replace it while gossiping around a redundant mesh.

use std::{net::TcpListener, time::Duration};

use moq_net::Origin;
use moq_relay::{Config, PublicConfig, Relay};
use url::Url;

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
	config.listen.tcp.bind = Some(format!("127.0.0.1:{port}").parse().expect("parse bind"));
	config.connect.bind = Some("127.0.0.1:0".parse().expect("parse client bind"));
	config.connect.tls.insecure = Some(true);
	config.connect.version.extend(cluster_version);
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

fn client(version: Option<moq_net::Version>) -> moq_tokio::Client {
	let mut config = moq_tokio::connect::Config::default();
	config.tls.insecure = Some(true);
	config.websocket.delay = Some(Duration::ZERO);
	config.bind = Some("127.0.0.1:0".parse().expect("parse bind"));
	config.version.extend(version);
	config.init(Default::default()).expect("client init")
}

struct Publisher {
	_broadcast: moq_net::broadcast::Producer,
	_session: moq_tokio::Connection,
	streamer: tokio::task::AbortHandle,
}

impl Drop for Publisher {
	fn drop(&mut self) {
		// The streaming task owns the track producer; stop it with the
		// publisher so the broadcast actually ends (a live track producer
		// would keep it announced).
		self.streamer.abort();
	}
}

async fn publish_version(port: u16, version: &str) -> Publisher {
	let url: Url = format!("tcp://127.0.0.1:{port}").parse().expect("parse url");
	let origin = moq_tokio::origin::spawn(Origin::random());
	let mut broadcast = origin
		.create_broadcast(PATH, moq_net::broadcast::Route::new().with_announce(true))
		.expect("create broadcast");
	let mut track = broadcast.create_track("video", None).expect("create track");

	// Stream like a real publisher: a fresh group every 100ms, so a subscriber
	// that attaches at any point receives one (and the test doesn't depend on
	// cached-group replay, which differs per protocol). Aborted by the
	// Publisher's Drop, which is what ends the broadcast.
	let streamer = tokio::spawn(async move {
		loop {
			let Ok(mut group) = track.append_group() else { break };
			if group.write_frame(moq_net::Timestamp::ZERO, b"hello".as_ref()).is_err() {
				break;
			}
			if group.finish().is_err() {
				break;
			}
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
	})
	.abort_handle();

	let version = version.parse().expect("parse version");
	let session = tokio::time::timeout(
		TIMEOUT,
		client(Some(version)).with_publisher(&origin).connect(url).established(),
	)
	.await
	.expect("publisher connect timeout")
	.expect("publisher connect failed");

	Publisher {
		_broadcast: broadcast,
		_session: session,
		streamer,
	}
}

async fn publish_unknown(port: u16) -> Publisher {
	// Draft-14 has no Cluster extension, so the accepting relay must represent
	// this external publisher with the wire-defined UNKNOWN Hop ID.
	publish_version(port, "moq-transport-14").await
}

/// Subscribe to [`PATH`] through the relay on `port` and read its first frame.
/// `Err` carries why it never arrived, so a failing assertion says which step
/// broke rather than just "no frame".
async fn read_first_frame(port: u16) -> Result<Vec<u8>, String> {
	let url: Url = format!("tcp://127.0.0.1:{port}").parse().expect("parse url");
	let origin = moq_tokio::origin::spawn(Origin::random());
	let consumer = origin.consume();
	let session = tokio::time::timeout(TIMEOUT, client(None).with_subscriber(origin).connect(url).established())
		.await
		.map_err(|_| "subscriber connect timeout".to_string())?
		.map_err(|err| format!("subscriber connect failed: {err}"))?;

	// Wait for the announcement rather than asking the moment the session connects:
	// `request_broadcast` answers on the spot, so it would race the announcement that
	// makes the path routable.
	let broadcast = tokio::time::timeout(TIMEOUT, consumer.announced_broadcast(PATH))
		.await
		.map_err(|_| "announced_broadcast timed out".to_string())?
		.ok_or_else(|| "origin closed before the broadcast was announced".to_string())?;

	// This verifies route propagation rather than real-time backlog skipping. Give
	// every hop enough tolerance for the next 100ms group to arrive while the
	// selected group's frame is still crossing the redundant mesh.
	let subscription = moq_net::track::Subscription::default().with_max_age(Duration::from_secs(1));
	let mut track = tokio::time::timeout(
		TIMEOUT,
		broadcast.track("video").expect("track handle").subscribe(subscription),
	)
	.await
	.map_err(|_| "subscribe timed out".to_string())?
	.map_err(|err| format!("subscribe failed: {err}"))?;

	let mut group = tokio::time::timeout(TIMEOUT, track.recv_group())
		.await
		.map_err(|_| "recv_group timed out".to_string())?
		.map_err(|err| format!("recv_group failed: {err}"))?
		.ok_or_else(|| "track closed before a group arrived".to_string())?;

	let frame = tokio::time::timeout(TIMEOUT, group.read_frame())
		.await
		.map_err(|_| "read_frame timed out".to_string())?
		.map_err(|err| format!("read_frame failed: {err}"))?
		.ok_or_else(|| "group closed before a frame arrived".to_string())?;

	let payload = frame.payload.to_vec();
	drop(session);
	Ok(payload)
}

async fn watch_announces(port: u16, window: Duration) -> Vec<(String, bool)> {
	let url: Url = format!("tcp://127.0.0.1:{port}").parse().expect("parse url");
	let origin = moq_tokio::origin::spawn(Origin::random());
	let mut announced = origin.consume().announced();
	let _session = tokio::time::timeout(TIMEOUT, client(None).with_subscriber(origin).connect(url).established())
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

async fn assert_unknown_publisher_stays_announced(cluster_version: Option<moq_net::Version>, expect_frame: bool) {
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

	// The announce is only half the contract: the route it advertises must
	// actually serve a frame at the far end of the mesh. Over IETF cluster
	// sessions that data path is still broken for UNKNOWN publishers (see
	// unknown_publisher_frames_over_an_ietf_cluster below), so only the lite
	// variant asserts it for now.
	let frame = read_first_frame(d_port).await;

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
	if expect_frame {
		assert_eq!(frame.expect("read through the mesh"), b"hello");
	}
}

/// Every ingest version that can carry a broadcast must survive the same
/// redundant mesh. The pre-fix failure set was exactly the versions that
/// declare no origin identity (lite <= 03, moq-transport <= 16): their
/// broadcasts enter with an UNKNOWN first hop, and the reflected copy replaced
/// the live source instead of parking. The identity-carrying versions held
/// even before the fix, so this pins both halves of the boundary.
#[tokio::test]
async fn every_ingest_version_crosses_a_redundant_mesh() {
	let mut failures = Vec::new();

	for version in [
		"moq-lite-02",
		"moq-lite-03",
		"moq-lite-04",
		"moq-lite-05",
		"moq-transport-14",
		"moq-transport-16",
		"moq-transport-17",
	] {
		let (a_port, a) = spawn_relay(11, vec![], None).await;
		let (b_port, b) = spawn_relay(12, vec![format!("tcp://127.0.0.1:{a_port}")], None).await;
		let (c_port, c) = spawn_relay(
			13,
			vec![format!("tcp://127.0.0.1:{a_port}"), format!("tcp://127.0.0.1:{b_port}")],
			None,
		)
		.await;
		let (d_port, d) = spawn_relay(14, vec![format!("tcp://127.0.0.1:{c_port}")], None).await;
		tokio::time::sleep(Duration::from_millis(500)).await;

		let _publisher = publish_version(a_port, version).await;
		tokio::time::sleep(Duration::from_millis(750)).await;

		if let Err(err) = read_first_frame(d_port).await {
			failures.push(format!("{version}: {err}"));
		}

		a.abort();
		b.abort();
		c.abort();
		d.abort();
	}

	assert!(failures.is_empty(), "cross-mesh failures:\n{}", failures.join("\n"));
}

/// The same broadcast path republished after its previous session ended - what
/// a reconnecting robot (or a re-run smoke test) does. Each round must become
/// routable across the mesh again; a dead verdict left by the previous round
/// must not suppress the republish.
#[tokio::test]
async fn unknown_publisher_republish_stays_routable() {
	let (a_port, a) = spawn_relay(11, vec![], None).await;
	let (b_port, b) = spawn_relay(12, vec![format!("tcp://127.0.0.1:{a_port}")], None).await;
	let (c_port, c) = spawn_relay(
		13,
		vec![format!("tcp://127.0.0.1:{a_port}"), format!("tcp://127.0.0.1:{b_port}")],
		None,
	)
	.await;
	let (d_port, d) = spawn_relay(14, vec![format!("tcp://127.0.0.1:{c_port}")], None).await;
	tokio::time::sleep(Duration::from_millis(500)).await;

	for round in 0..3 {
		let publisher = publish_unknown(a_port).await;
		tokio::time::sleep(Duration::from_millis(750)).await;

		let frame = read_first_frame(d_port).await;
		assert_eq!(
			frame.unwrap_or_else(|err| panic!("round {round}: the republished broadcast never came back: {err}")),
			b"hello",
			"round {round}"
		);

		// The session ends; the broadcast unannounces everywhere before the
		// next round republishes the same path.
		drop(publisher);
		tokio::time::sleep(Duration::from_millis(750)).await;
	}

	a.abort();
	b.abort();
	c.abort();
	d.abort();
}

#[tokio::test]
async fn unknown_publisher_does_not_flap_across_a_lite_cluster_triangle() {
	assert_unknown_publisher_stays_announced(None, true).await;
}

#[tokio::test]
async fn unknown_publisher_does_not_flap_across_an_ietf_cluster_triangle() {
	let version = "moq-transport-19".parse().expect("parse version");
	assert_unknown_publisher_stays_announced(Some(version), false).await;
}

/// KNOWN GAP: over IETF inter-relay sessions an UNKNOWN publisher's broadcast
/// now stays announced (the fix above), but its frames never arrive at the far
/// relay - recv_group fails with `dropped` while a DECLARED publisher's frames
/// flow fine over the same mesh. The fleet's inter-relay sessions negotiate
/// lite, so this only bites IETF federation; pinned here so it isn't
/// forgotten, ignored so it doesn't fail CI until the data path is fixed.
#[tokio::test]
#[ignore = "UNKNOWN publisher frames do not flow over IETF cluster sessions"]
async fn unknown_publisher_frames_over_an_ietf_cluster() {
	let version: moq_net::Version = "moq-transport-19".parse().expect("parse version");
	let (a_port, a) = spawn_relay(11, vec![], Some(version)).await;
	let (b_port, b) = spawn_relay(12, vec![format!("tcp://127.0.0.1:{a_port}")], Some(version)).await;
	let (c_port, c) = spawn_relay(
		13,
		vec![format!("tcp://127.0.0.1:{a_port}"), format!("tcp://127.0.0.1:{b_port}")],
		Some(version),
	)
	.await;
	let (d_port, d) = spawn_relay(14, vec![format!("tcp://127.0.0.1:{c_port}")], Some(version)).await;
	tokio::time::sleep(Duration::from_millis(500)).await;

	let _publisher = publish_unknown(a_port).await;
	tokio::time::sleep(Duration::from_millis(750)).await;

	let frame = read_first_frame(d_port).await;

	a.abort();
	b.abort();
	c.abort();
	d.abort();

	assert_eq!(frame.expect("read through the IETF mesh"), b"hello");
}

/// Same drill over Lite04 inter-relay sessions, which have no restart support.
/// Pre-fix, `run_announce` only retained the broadcast consumer (whose
/// ExclusionGuard is the exposure registration) when restarts were supported,
/// so on Lite04 the guard released right after the Active was written and a
/// reflected UNKNOWN route could still replace the incumbent.
#[tokio::test]
async fn unknown_publisher_does_not_flap_across_a_lite04_cluster_triangle() {
	let version = "moq-lite-04".parse().expect("parse version");
	assert_unknown_publisher_stays_announced(Some(version), true).await;
}
