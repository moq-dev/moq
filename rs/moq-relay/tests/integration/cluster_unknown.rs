//! Regression for an external publisher whose protocol does not declare a Hop ID.
//! The relay records that publisher as `Hop::UNKNOWN`; reflected cluster paths
//! must not replace it while propagating around a redundant mesh.

use std::time::Duration;

use moq_relay::{Config, Relay};
use url::Url;

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

const TIMEOUT: Duration = Duration::from_secs(10);
const PATH: &str = "robot/cell-clumsy-octopus/cameras/left.hang";

/// How long the far end watches for a flap after the broadcast first serves a
/// frame there. Propagation on loopback takes milliseconds, so this is many
/// times the window a reflected route has to replace the live one.
const SETTLE: Duration = Duration::from_secs(1);

/// A relay on loopback, with the internal listener up so [`Mesh::start`] can see
/// its dialed peers.
struct Node {
	port: u16,
	internal: u16,
	task: tokio::task::JoinHandle<()>,
}

async fn spawn_relay(id: u64, connect: Vec<String>, cluster_version: Option<moq_net::Version>) -> Node {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let mut config = Config::default();
	config.listen.tcp.bind = Some("127.0.0.1:0".parse().expect("parse bind"));
	config.internal.listen = Some("127.0.0.1:0".parse().expect("parse internal bind"));
	config.connect.bind = Some("127.0.0.1:0".parse().expect("parse client bind"));
	config.connect.tls.insecure = Some(true);
	config.connect.version.extend(cluster_version);
	config.auth.public = vec![moq_auth::Pattern::all()];
	config.cluster.id = Some(id);
	config.cluster.connect = connect.into_iter().map(moq_relay::cluster::Peer::new).collect();

	// `load` binds the TCP listener, so the port is ours before anyone dials it.
	let relay = Relay::load(config).await.expect("relay load").with_signals(false);
	let port = relay.tcp_addr().expect("TCP listener is configured").port();
	let internal = relay.internal().addr().expect("internal listener is configured").port();
	let task = tokio::spawn(async move {
		let _ = relay.run().await;
	});

	Node { port, internal, task }
}

/// Wait until the relay behind `internal` holds a session with each of the
/// `dialed` peers it connects to.
async fn wait_dialed(internal: u16, dialed: usize) {
	let url = format!("http://127.0.0.1:{internal}/nodes");
	let deadline = tokio::time::Instant::now() + TIMEOUT;
	loop {
		let body = reqwest::get(&url)
			.await
			.expect("fetch /nodes")
			.text()
			.await
			.expect("read /nodes");
		let nodes: serde_json::Value = serde_json::from_str(&body).expect("parse /nodes");
		if nodes["nodes"].as_array().expect("a node list").len() == dialed {
			return;
		}
		assert!(tokio::time::Instant::now() < deadline, "the mesh never formed: {nodes}");
		tokio::time::sleep(Duration::from_millis(10)).await;
	}
}

/// The redundant mesh: B dials A, C dials A and B, and D dials C, so a broadcast
/// published at A reaches D over two paths that reflect it back toward A.
struct Mesh {
	nodes: [Node; 4],
}

impl Mesh {
	/// Start every relay and wait for every dial to land, so a publish crosses the
	/// whole mesh rather than racing its links coming up.
	async fn start(cluster_version: Option<moq_net::Version>) -> Self {
		let a = spawn_relay(11, vec![], cluster_version).await;
		let b = spawn_relay(12, vec![format!("tcp://127.0.0.1:{}", a.port)], cluster_version).await;
		let c = spawn_relay(
			13,
			vec![
				format!("tcp://127.0.0.1:{}", a.port),
				format!("tcp://127.0.0.1:{}", b.port),
			],
			cluster_version,
		)
		.await;
		let d = spawn_relay(14, vec![format!("tcp://127.0.0.1:{}", c.port)], cluster_version).await;
		for (node, dialed) in [(&b, 1), (&c, 2), (&d, 1)] {
			wait_dialed(node.internal, dialed).await;
		}
		Self { nodes: [a, b, c, d] }
	}

	/// Where publishers ingest.
	fn a(&self) -> u16 {
		self.nodes[0].port
	}

	/// The far end, where viewers read.
	fn d(&self) -> u16 {
		self.nodes[3].port
	}
}

impl Drop for Mesh {
	fn drop(&mut self) {
		for node in &self.nodes {
			node.task.abort();
		}
	}
}

fn client(version: Option<moq_net::Version>) -> moq_tokio::Client {
	let mut config = moq_tokio::connect::Config::default();
	config.tls.insecure = Some(true);
	#[cfg(feature = "websocket")]
	{
		config.websocket.delay = Some((Duration::ZERO).into());
	}
	config.bind = Some("127.0.0.1:0".parse().expect("parse bind"));
	config.version.extend(version);
	config.init(Default::default()).expect("client init")
}

struct Publisher {
	_broadcast: moq_net::origin::Publication,
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
	let origin = moq_tokio::origin::spawn();
	let broadcast = origin.create_broadcast(PATH).expect("create broadcast");
	broadcast.announce(Default::default()).expect("create broadcast");
	let track = broadcast.create_track("video", timed()).expect("create track");

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
	// Lite-03 declares no origin identity, so the accepting relay must represent
	// this external publisher with the wire-defined UNKNOWN Hop ID.
	publish_version(port, "moq-lite-03").await
}

/// Subscribe to [`PATH`] through the relay on `port` and read its first frame.
/// `Err` carries why it never arrived, so a failing assertion says which step
/// broke rather than just "no frame".
async fn read_first_frame(port: u16) -> Result<Vec<u8>, String> {
	let url: Url = format!("tcp://127.0.0.1:{port}").parse().expect("parse url");
	let origin = moq_tokio::origin::spawn();
	let consumer = origin.consume();
	let session = tokio::time::timeout(TIMEOUT, client(None).with_subscriber(origin).connect(url).established())
		.await
		.map_err(|_| "subscriber connect timeout".to_string())?
		.map_err(|err| format!("subscriber connect failed: {err}"))?;

	// Wait for the announcement rather than asking the moment the session connects:
	// `request` answers on the spot, so it would race the announcement that
	// makes the path routable.
	let broadcast = consumer
		.follow(PATH, None)
		.unwrap()
		.available()
		.await
		.map_err(|err| format!("broadcast unroutable: {err}"))?;

	// This verifies route propagation rather than real-time backlog skipping. Give
	// every hop enough tolerance for the next 100ms group to arrive while the
	// selected group's frame is still crossing the redundant mesh.
	let subscription = moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(1));
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

/// Connect a viewer to `port` and record every route change it sees until
/// `stop` fires. Returns once the viewer is connected, so nothing published
/// afterward can slip past it.
async fn watch_announces(
	port: u16,
	stop: tokio::sync::oneshot::Receiver<()>,
) -> tokio::task::JoinHandle<Vec<(String, bool)>> {
	let url: Url = format!("tcp://127.0.0.1:{port}").parse().expect("parse url");
	let origin = moq_tokio::origin::spawn();
	let mut announced = origin.consume().announced();
	let session = tokio::time::timeout(TIMEOUT, client(None).with_subscriber(origin).connect(url).established())
		.await
		.expect("viewer connect timeout")
		.expect("viewer connect failed");

	tokio::spawn(async move {
		let _session = session;
		let mut updates = Vec::new();
		tokio::pin!(stop);
		loop {
			tokio::select! {
				_ = &mut stop => break,
				update = next_update(&mut announced) => match update {
					Some((update, active)) => updates.push((update.prefix.as_str().to_string(), active)),
					None => break,
				},
			}
		}
		updates
	})
}

async fn assert_unknown_publisher_stays_announced(cluster_version: Option<moq_net::Version>) {
	let mesh = Mesh::start(cluster_version).await;

	let (stop, stopped) = tokio::sync::oneshot::channel();
	let watcher = watch_announces(mesh.d(), stopped).await;
	let publisher = publish_unknown(mesh.a()).await;

	// The announce is only half the contract: the route it advertises must
	// actually serve a frame at the far end of the mesh.
	let frame = read_first_frame(mesh.d()).await;
	tokio::time::sleep(SETTLE).await;
	let _ = stop.send(());
	let updates = watcher.await.expect("viewer task");

	drop(publisher);
	drop(mesh);

	let announces = updates.iter().filter(|(_, active)| *active).count();
	let unannounces = updates.iter().filter(|(_, active)| !*active).count();
	assert!(announces >= 1, "the broadcast never reached the viewer: {updates:?}");
	assert_eq!(
		unannounces, 0,
		"the broadcast flapped while its publisher stayed up: {updates:?}"
	);
	assert_eq!(frame.expect("read through the mesh"), b"hello");
}

/// Every ingest version that can carry a broadcast must survive the same
/// redundant mesh. The pre-fix failure set was exactly the versions that
/// declare no origin identity (lite <= 03): their
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
		"moq-transport-18",
	] {
		let mesh = Mesh::start(None).await;
		let _publisher = publish_version(mesh.a(), version).await;
		if let Err(err) = read_first_frame(mesh.d()).await {
			failures.push(format!("{version}: {err}"));
		}
	}

	assert!(failures.is_empty(), "cross-mesh failures:\n{}", failures.join("\n"));
}

/// The same broadcast path republished after its previous session ended - what
/// a reconnecting robot (or a re-run smoke test) does. Each round must become
/// routable across the mesh again; a dead verdict left by the previous round
/// must not suppress the republish.
#[tokio::test]
async fn unknown_publisher_republish_stays_routable() {
	let mesh = Mesh::start(None).await;

	let url: Url = format!("tcp://127.0.0.1:{}", mesh.d()).parse().expect("parse url");
	let origin = moq_tokio::origin::spawn();
	let mut announced = origin.consume().announced();
	let _session = tokio::time::timeout(TIMEOUT, client(None).with_subscriber(origin).connect(url).established())
		.await
		.expect("viewer connect timeout")
		.expect("viewer connect failed");

	for round in 0..3 {
		let publisher = publish_unknown(mesh.a()).await;

		let frame = read_first_frame(mesh.d()).await;
		assert_eq!(
			frame.unwrap_or_else(|err| panic!("round {round}: the republished broadcast never came back: {err}")),
			b"hello",
			"round {round}"
		);

		// The session ends; the broadcast unannounces everywhere before the
		// next round republishes the same path.
		drop(publisher);
		tokio::time::timeout(TIMEOUT, async {
			while let Some((update, active)) = next_update(&mut announced).await {
				if !active && update.prefix.as_str() == PATH {
					return;
				}
			}
			panic!("round {round}: the viewer's origin closed");
		})
		.await
		.unwrap_or_else(|_| panic!("round {round}: the broadcast never unannounced at the far end"));
	}
}

#[tokio::test]
async fn unknown_publisher_does_not_flap_across_a_lite_cluster_triangle() {
	assert_unknown_publisher_stays_announced(None).await;
}

#[tokio::test]
async fn unknown_publisher_does_not_flap_across_an_ietf_cluster_triangle() {
	let version = "moq-transport-22".parse().expect("parse version");
	assert_unknown_publisher_stays_announced(Some(version)).await;
}

/// Same drill over Lite04 inter-relay sessions, which have no restart support.
/// Pre-fix, `run_announce` only retained the broadcast consumer (whose
/// ExclusionGuard is the exposure registration) when restarts were supported,
/// so on Lite04 the guard released right after the Active was written and a
/// reflected UNKNOWN route could still replace the incumbent.
#[tokio::test]
async fn unknown_publisher_does_not_flap_across_a_lite04_cluster_triangle() {
	let version = "moq-lite-04".parse().expect("parse version");
	assert_unknown_publisher_stays_announced(Some(version)).await;
}

/// The next route and whether it is active.
async fn next_update(announced: &mut moq_net::announce::Consumer) -> Option<(moq_net::announce::Announce, bool)> {
	match announced.next().await? {
		moq_net::announce::Event::Start(route)
		| moq_net::announce::Event::Update(route)
		| moq_net::announce::Event::Restart(route)
		| moq_net::announce::Event::Takeover { announce: route, .. } => Some((route, true)),
		moq_net::announce::Event::End(route) => Some((route, false)),
	}
}
