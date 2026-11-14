//! Regression for an external publisher whose protocol does not declare a Hop ID.
//! The relay records that publisher as `Hop::UNKNOWN`; reflected cluster paths
//! must not replace it while propagating around a redundant mesh.
//!
//! The relays run on kio's simulator, linked in memory, so [`kio::clock::sim::idle`]
//! proves the mesh has settled: nothing is left in flight that could still flap.

use std::time::Duration;

use moq_relay::{auth, cluster, shutdown};

use crate::memory::{self, Net};

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

/// Long enough that only a hang reaches it; simulated time skips straight there.
const TIMEOUT: Duration = Duration::from_secs(10);
const PATH: &str = "robot/cell-clumsy-octopus/cameras/left.hang";

/// The redundant mesh: B dials A, C dials A and B, and D dials C, so a broadcast
/// published at A reaches D over two paths that reflect it back toward A.
struct Mesh {
	auth: auth::Auth,
	a: cluster::Cluster,
	d: cluster::Cluster,
}

impl Mesh {
	/// Start every relay and let every dial land, so a publish crosses the whole
	/// mesh rather than racing its links coming up. `cluster_version` pins the
	/// version the relays dial each other with.
	async fn start(cluster_version: Option<moq_net::Version>) -> Self {
		let net = Net::new();
		let auth = memory::public(&["**"], &[], &[]);
		let mut relays = Vec::new();
		for (name, peers) in [("a", &[][..]), ("b", &["a"]), ("c", &["a", "b"]), ("d", &["c"])] {
			let cluster = memory::spawn_cluster(net.options(peers));
			net.relay(name, &cluster, &auth, &shutdown::Observer::disabled());
			let mut connect = moq_tokio::connect::Config::default();
			connect.version.extend(cluster_version);
			relays.push(memory::start(cluster, connect).await);
		}
		kio::clock::sim::idle().await;
		for (name, dialed) in [("a", 2), ("b", 1), ("c", 1), ("d", 0)] {
			assert_eq!(net.accepted(name), dialed, "{name} accepted the wrong number of peers");
		}
		// Take D first: removing A first would move D into A's place.
		let d = relays.pop().expect("d is up");
		let a = relays.swap_remove(0);
		Self { auth, a, d }
	}
}

/// A broadcast at [`PATH`] streaming a fresh group every 100ms until [`stop`](Self::stop).
struct Publisher {
	_session: moq_net::Session,
	_broadcast: moq_net::origin::Route,
	streaming: std::sync::Arc<std::sync::atomic::AtomicBool>,
	streamer: kio::clock::sim::JoinHandle<()>,
}

impl Drop for Publisher {
	fn drop(&mut self) {
		// The streaming task owns the track producer; stop it with the
		// publisher so the track ends with the route.
		self.streamer.abort();
	}
}

impl Publisher {
	/// Stop writing groups, keeping the broadcast and its track up.
	fn stop(&self) {
		self.streaming.store(false, std::sync::atomic::Ordering::Relaxed);
	}
}

async fn publish_version(relay: &cluster::Cluster, auth: &auth::Auth, version: &str) -> Publisher {
	let origin = memory::origin();
	let broadcast = origin
		.route(PATH, moq_net::origin::RouteOptions::default())
		.expect("create broadcast");
	let track = broadcast
		.broadcast()
		.create_track("video", timed())
		.expect("create track");

	// Stream like a real publisher: a fresh group every 100ms, so a subscriber
	// that attaches at any point receives one (and the test doesn't depend on
	// cached-group replay, which differs per protocol). Once stopped, the task
	// parks holding the track, so the broadcast stays whole.
	let streaming = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
	let running = streaming.clone();
	let streamer = kio::clock::sim::spawn(async move {
		while running.load(std::sync::atomic::Ordering::Relaxed) {
			let mut group = track
				.append_group(track.largest().map_or(0, |largest| largest.group + 1))
				.expect("append group");
			group
				.write_frame(moq_net::Timestamp::from_millis(group.start).unwrap(), b"hello".as_ref())
				.expect("write frame");
			group.close().expect("finish group");
			kio::clock::sleep(Duration::from_millis(100)).await;
		}
		let _track = track;
		std::future::pending::<()>().await;
	});

	let version = version.parse().expect("parse version");
	let session = kio::clock::timeout(
		TIMEOUT,
		memory::connect(
			moq_net::Client::new().with_publisher(origin.consume()),
			version,
			relay,
			auth,
		),
	)
	.await
	.expect("publisher connect timeout")
	.expect("publisher connect failed");

	Publisher {
		_session: session,
		_broadcast: broadcast,
		streaming,
		streamer,
	}
}

async fn publish_unknown(relay: &cluster::Cluster, auth: &auth::Auth) -> Publisher {
	// Lite-03 declares no origin identity, so the accepting relay must represent
	// this external publisher with the wire-defined UNKNOWN Hop ID.
	publish_version(relay, auth, "moq-lite-03").await
}

/// A viewer of `relay` speaking the newest version, and the origin it reads into.
async fn viewer(relay: &cluster::Cluster, auth: &auth::Auth) -> (moq_net::Session, moq_net::origin::Producer) {
	let origin = memory::origin();
	let session = kio::clock::timeout(
		TIMEOUT,
		memory::connect(
			moq_net::Client::new().with_subscriber(origin.clone()),
			memory::newest(),
			relay,
			auth,
		),
	)
	.await
	.map_err(|_| "viewer connect timeout")
	.unwrap()
	.expect("viewer connect failed");
	(session, origin)
}

/// Subscribe to [`PATH`] through `relay` and read its first frame.
/// `Err` carries why it never arrived, so a failing assertion says which step
/// broke rather than just "no frame".
async fn read_first_frame(relay: &cluster::Cluster, auth: &auth::Auth) -> Result<Vec<u8>, String> {
	let (_session, origin) = viewer(relay, auth).await;

	// Wait for the announcement rather than asking the moment the session connects:
	// `request` answers on the spot, so it would race the announcement that
	// makes the path routable.
	let broadcast = kio::clock::timeout(TIMEOUT, origin.consume().follow(PATH, None).unwrap().available())
		.await
		.map_err(|_| "never announced".to_string())?
		.map_err(|err| format!("broadcast unroutable: {err}"))?
		.ok_or("the first change is an instance")?;

	// This verifies route propagation rather than real-time backlog skipping. Give
	// every hop enough tolerance for the next 100ms group to arrive while the
	// selected group's frame is still crossing the redundant mesh.
	let subscription = moq_net::track::Subscription::default()
		.with_delay(moq_net::track::Delay::default().with_max(Duration::from_secs(1)));
	let mut track = kio::clock::timeout(
		TIMEOUT,
		broadcast.track("video").expect("track handle").subscribe(subscription),
	)
	.await
	.map_err(|_| "subscribe timed out".to_string())?
	.map_err(|err| format!("subscribe failed: {err}"))?;

	let mut group = kio::clock::timeout(TIMEOUT, track.recv_group())
		.await
		.map_err(|_| "recv_group timed out".to_string())?
		.map_err(|err| format!("recv_group failed: {err}"))?
		.ok_or_else(|| "track closed before a group arrived".to_string())?;

	let frame = kio::clock::timeout(TIMEOUT, group.read_frame())
		.await
		.map_err(|_| "read_frame timed out".to_string())?
		.map_err(|err| format!("read_frame failed: {err}"))?
		.ok_or_else(|| "group closed before a frame arrived".to_string())?;

	Ok(frame.payload.to_vec())
}

async fn assert_unknown_publisher_stays_announced(cluster_version: Option<moq_net::Version>) {
	let mesh = Mesh::start(cluster_version).await;

	// Watched from before the publish, so nothing it announces can slip past.
	let (_viewer, viewed) = viewer(&mesh.d, &mesh.auth).await;
	let recorded = memory::record(viewed.consume().routes());
	let publisher = publish_unknown(&mesh.a, &mesh.auth).await;

	// The announce is only half the contract: the route it advertises must
	// actually serve a frame at the far end of the mesh.
	let frame = read_first_frame(&mesh.d, &mesh.auth).await;
	// Once the publisher stops writing, the mesh settles; a reflected route that
	// could still replace the live one would have done so by then.
	publisher.stop();
	kio::clock::sim::idle().await;
	let updates = recorded.lock().unwrap().clone();

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
#[test]
fn every_ingest_version_crosses_a_redundant_mesh() {
	memory::run(|| async {
		let mut failures = Vec::new();

		for version in [
			"moq-lite-02",
			"moq-lite-03",
			"moq-lite-04",
			"moq-lite-05",
			"moq-transport-18",
		] {
			let mesh = Mesh::start(None).await;
			let _publisher = publish_version(&mesh.a, &mesh.auth, version).await;
			if let Err(err) = read_first_frame(&mesh.d, &mesh.auth).await {
				failures.push(format!("{version}: {err}"));
			}
		}

		assert!(failures.is_empty(), "cross-mesh failures:\n{}", failures.join("\n"));
	});
}

/// The same broadcast path republished after its previous session ended - what
/// a reconnecting robot (or a re-run smoke test) does. Each round must become
/// routable across the mesh again; a dead verdict left by the previous round
/// must not suppress the republish.
#[test]
fn unknown_publisher_republish_stays_routable() {
	memory::run(|| async {
		let mesh = Mesh::start(None).await;
		let (_viewer, viewed) = viewer(&mesh.d, &mesh.auth).await;
		let mut announced = viewed.consume().routes();

		for round in 0..3 {
			let publisher = publish_unknown(&mesh.a, &mesh.auth).await;

			let frame = read_first_frame(&mesh.d, &mesh.auth).await;
			assert_eq!(
				frame.unwrap_or_else(|err| panic!("round {round}: the republished broadcast never came back: {err}")),
				b"hello",
				"round {round}"
			);

			// The session ends; the broadcast unannounces everywhere before the
			// next round republishes the same path.
			drop(publisher);
			kio::clock::timeout(TIMEOUT, async {
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
	});
}

#[test]
fn unknown_publisher_does_not_flap_across_a_lite_cluster_triangle() {
	memory::run(|| assert_unknown_publisher_stays_announced(None));
}

#[test]
fn unknown_publisher_does_not_flap_across_an_ietf_cluster_triangle() {
	memory::run(|| assert_unknown_publisher_stays_announced(Some("moq-transport-22".parse().unwrap())));
}

/// Same drill over Lite04 inter-relay sessions, which have no restart support.
/// Pre-fix, `run_announce` only retained the broadcast consumer (whose
/// ExclusionGuard is the exposure registration) when restarts were supported,
/// so on Lite04 the guard released right after the Active was written and a
/// reflected UNKNOWN route could still replace the incumbent.
#[test]
fn unknown_publisher_does_not_flap_across_a_lite04_cluster_triangle() {
	memory::run(|| assert_unknown_publisher_stays_announced(Some("moq-lite-04".parse().unwrap())));
}

/// The next route and whether it is active.
async fn next_update(announced: &mut moq_net::origin::Routes) -> Option<(moq_net::announce::Announce, bool)> {
	match announced.next().await? {
		moq_net::origin::RouteEvent {
			kind:
				moq_net::origin::RouteKind::Start | moq_net::origin::RouteKind::Update | moq_net::origin::RouteKind::Restart,
			announce: route,
		} => Some((route, true)),
		moq_net::origin::RouteEvent {
			kind: moq_net::origin::RouteKind::End | moq_net::origin::RouteKind::Offline,
			announce: route,
		} => Some((route, false)),
	}
}
