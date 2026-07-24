//! Upstream GOAWAY migration over real TCP transports.
//!
//! Two "sibling" upstream servers share one origin (the same live broadcast is
//! reachable through either). The relay's cluster dials sibling A; A sends a
//! GOAWAY redirecting to sibling B; the cluster reconnects to B and the origin
//! hands the live subscription over at a group boundary. A downstream consumer
//! of the cluster origin observes contiguous groups and no unannounce.

use std::net::TcpListener;
use std::time::Duration;

use moq_net::Origin;
use moq_relay::{AuthConfig, Cluster, ClusterConfig, Connection, PublicConfig};

const TEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Bound `fut` by [`TEST_TIMEOUT`], panicking with `step` so a hang names the
/// exact stage that failed instead of a bare "test timed out".
async fn within<T>(step: &str, fut: impl std::future::Future<Output = T>) -> T {
	tokio::time::timeout(TEST_TIMEOUT, fut)
		.await
		.unwrap_or_else(|_| panic!("timed out: {step}"))
}

/// A fake sibling upstream: a stream-only moq server publishing `origin`'s
/// broadcasts to whoever connects. Returns its port, a receiver yielding each
/// accepted [`moq_net::Session`] (so the test can drain it), and the task.
fn spawn_upstream(
	origin: moq_net::origin::Producer,
) -> (
	u16,
	tokio::sync::mpsc::UnboundedReceiver<moq_net::Session>,
	tokio::task::JoinHandle<()>,
) {
	// Pick a free TCP port, then drop the probe so the listener can bind it.
	let probe = TcpListener::bind("127.0.0.1:0").expect("bind probe");
	let port = probe.local_addr().expect("local addr").port();
	drop(probe);

	let mut config = moq_native::ServerConfig::default();
	config.tcp.bind = Some(format!("127.0.0.1:{port}").parse().expect("parse addr"));
	let mut server = config.init().expect("server init");

	let (accepted_tx, accepted_rx) = tokio::sync::mpsc::unbounded_channel();

	let handle = tokio::spawn(async move {
		while let Some(request) = server.accept().await {
			// Serve the shared origin bidirectionally, like a relay peer would.
			let scratch = Origin::random().produce();
			let session = match request.with_publisher(&origin).with_subscriber(scratch).ok().await {
				Ok(session) => session,
				Err(err) => {
					tracing::warn!(%err, "upstream accept failed");
					continue;
				}
			};
			let _ = accepted_tx.send(session);
		}
	});

	(port, accepted_rx, handle)
}

/// Wait for the upstream's TCP listener to come up.
async fn wait_listening(port: u16) {
	let deadline = std::time::Instant::now() + Duration::from_secs(5);
	loop {
		if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
			break;
		}
		assert!(
			std::time::Instant::now() < deadline,
			"upstream never became ready on port {port}"
		);
		tokio::time::sleep(Duration::from_millis(25)).await;
	}
}

/// An upstream GOAWAY with a redirect migrates the cluster dial to the sibling
/// with no gap and no unannounce visible on the cluster origin.
#[tokio::test]
async fn cluster_migrates_on_upstream_goaway() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	tokio::time::timeout(TEST_TIMEOUT, async {
		// ── the shared "live" broadcast both siblings can serve ─────────
		let upstream_origin = Origin::random().produce();
		let mut broadcast = upstream_origin
			.create_broadcast("cam", moq_net::broadcast::Route::new().with_announce(true))
			.expect("create broadcast");
		let mut track = broadcast.create_track("video", None).expect("create track");

		let (port_a, mut accepted_a, _handle_a) = spawn_upstream(upstream_origin.clone());
		let (port_b, mut accepted_b, _handle_b) = spawn_upstream(upstream_origin.clone());
		wait_listening(port_a).await;
		wait_listening(port_b).await;

		// ── the relay cluster under test, dialing sibling A ─────────────
		let mut client_config = moq_native::ClientConfig::default();
		client_config.tls.disable_verify = Some(true);
		let client = client_config.init().expect("client init");

		let mut cluster_config = ClusterConfig::default();
		cluster_config.connect = vec![format!("tcp://127.0.0.1:{port_a}/")];
		// Short drain so the test observes the old session force-close quickly.
		cluster_config.drain_timeout = Some(2);
		let cluster = Cluster::new(cluster_config).expect("cluster init").with_client(client);

		let cluster_run = tokio::spawn(cluster.clone().run());

		// A dials in; hold its server-side session so we can drain it.
		let session_a = accepted_a.recv().await.expect("sibling A accepts the cluster dial");

		// ── downstream consumer on the cluster origin ───────────────────
		let consumer = cluster.origin.consume();
		let bc = consumer
			.announced_broadcast("cam")
			.await
			.expect("broadcast announced through sibling A");
		let mut sub = bc
			.track("video")
			.expect("track handle")
			.subscribe(None)
			.await
			.expect("subscribe");

		let mut group = track.append_group().expect("append group");
		group
			.write_frame(moq_net::Timestamp::ZERO, b"g0".as_ref())
			.expect("write frame");
		group.finish().expect("finish");

		let mut g0 = sub.recv_group().await.expect("recv g0").expect("track ended early");
		assert_eq!(
			g0.read_frame().await.expect("read").expect("frame").payload[..],
			b"g0"[..]
		);

		// Watch for unannounce during the swap: seamless migration must never
		// unannounce the path.
		let mut announcements = cluster.origin.consume().announced();
		let first = announcements.next().await.expect("initial announce");
		assert_eq!(first.path.as_str(), "cam");

		// ── sibling A drains with a redirect to sibling B ────────────────
		let draining = session_a
			.drain()
			.expect("drain")
			.start(format!("tcp://127.0.0.1:{port_b}/"));

		// The cluster reconnects: sibling B accepts a session.
		let _session_b = accepted_b.recv().await.expect("sibling B accepts the redirected dial");

		// New content lands after the swap (both siblings serve the same
		// origin, so B has it).
		let mut group = track.append_group().expect("append group");
		group
			.write_frame(moq_net::Timestamp::ZERO, b"g1".as_ref())
			.expect("write frame");
		group.finish().expect("finish");

		let mut g1 = sub.recv_group().await.expect("recv g1").expect("track ended early");
		assert_eq!(
			g1.sequence, 1,
			"delivery must resume contiguously at the next group after the swap"
		);
		assert_eq!(
			g1.read_frame().await.expect("read").expect("frame").payload[..],
			b"g1"[..]
		);

		// The old session drains away (the cluster force-closes it after the
		// window at the latest).
		draining.complete().await;

		// No unannounce leaked to the origin during the whole swap: the next
		// announce event (with a generous bound) must never arrive.
		let churn = tokio::time::timeout(Duration::from_millis(500), announcements.next()).await;
		assert!(
			churn.is_err(),
			"migration must not churn announces on the cluster origin"
		);

		cluster_run.abort();
	})
	.await
	.expect("test timed out");
}

/// A full relay (server + cluster dial to `upstream_url`) on a free TCP port.
///
/// `accept_notify` fires on the relay's first inbound connection, so a test can
/// positively gate on a reconnect landing here. Returns the downstream port.
async fn spawn_relay_with_upstream(
	upstream_url: &str,
	accept_notify: Option<tokio::sync::oneshot::Sender<()>>,
) -> (u16, tokio::task::JoinHandle<()>) {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let probe = TcpListener::bind("127.0.0.1:0").expect("bind probe");
	let port = probe.local_addr().expect("local addr").port();
	drop(probe);

	let mut server_config = moq_native::ServerConfig::default();
	server_config.tcp.bind = Some(format!("127.0.0.1:{port}").parse().expect("parse addr"));
	let mut server = server_config.init().expect("relay server init");

	// Fully public auth: any no-JWT stream client gets the whole root.
	#[allow(deprecated)]
	let public = PublicConfig::Simple(vec![String::new()]);
	let mut auth_config = AuthConfig::default();
	auth_config.public = Some(public);
	let auth = auth_config
		.init(&moq_native::tls::Client::default())
		.await
		.expect("auth init");

	let mut cluster_config = ClusterConfig::default();
	cluster_config.connect = vec![upstream_url.to_string()];
	// Short drain so the test observes teardown quickly.
	cluster_config.drain_timeout = Some(2);

	let mut client_config = moq_native::ClientConfig::default();
	client_config.tls.disable_verify = Some(true);
	let client = client_config.init().expect("client init");

	let cluster = Cluster::new(cluster_config).expect("cluster init").with_client(client);

	let handle = tokio::spawn(async move {
		let cluster_run = cluster.clone();
		tokio::spawn(async move {
			let _ = cluster_run.run().await;
		});

		let mut accept_notify = accept_notify;
		let mut id = 0;
		while let Some(request) = server.accept().await {
			if let Some(notify) = accept_notify.take() {
				let _ = notify.send(());
			}
			let conn = Connection {
				id,
				request,
				cluster: cluster.clone(),
				auth: auth.clone(),
				shutdown: moq_relay::Shutdown::disabled(),
			};
			id += 1;
			tokio::spawn(async move {
				let _ = conn.run().await;
			});
		}
	});

	wait_listening(port).await;
	(port, handle)
}

/// Diamond GOAWAY failover across real relay instances:
///
/// ```text
///   TOP (origin server, accepts MID-A and MID-B)
///     ├── MID-A (mini-relay: consumes TOP, serves BOTTOM, sends the GOAWAY)
///     └── MID-B (full relay: cluster.connect = [TOP])
///           ↑
///   BOTTOM (full relay: cluster.connect = [MID-A]) ──reconnects-to──> MID-B
///     ↓
///   SUBSCRIBER
/// ```
///
/// Proves that with the route/resume machinery:
/// 1. Content flows TOP -> MID-A -> BOTTOM -> subscriber.
/// 2. On MID-A's GOAWAY naming MID-B, BOTTOM reconnects there (positively
///    gated: MID-B's first inbound connection can only be that reconnect).
/// 3. The subscriber sees contiguous, duplicate-free groups across the swap.
/// 4. No GOAWAY leaks to the subscriber's own session.
#[tokio::test]
async fn cluster_diamond_goaway_seamless_failover() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	// ── TOP: origin server serving the same broadcast to both mids ──────
	let top_origin = Origin::random().produce();
	let mut broadcast = top_origin
		.create_broadcast("diamond", moq_net::broadcast::Route::new().with_announce(true))
		.expect("create broadcast");
	let mut track = broadcast.create_track("video", None).expect("create track");

	let (top_port, mut top_accepted, _top_handle) = spawn_upstream(top_origin.clone());
	wait_listening(top_port).await;
	let top_url = format!("tcp://127.0.0.1:{top_port}/");

	// ── MID-B: full relay clustered to TOP, up for the whole test ───────
	let (mid_b_accepted_tx, mid_b_accepted_rx) = tokio::sync::oneshot::channel::<()>();
	let (mid_b_port, _mid_b_handle) = spawn_relay_with_upstream(&top_url, Some(mid_b_accepted_tx)).await;
	let mid_b_url = format!("tcp://127.0.0.1:{mid_b_port}/");
	let _mid_b_session = within("TOP accepts MID-B", top_accepted.recv())
		.await
		.expect("TOP accept channel closed");

	// ── MID-A: mini-relay consuming TOP, serving BOTTOM, drains later ───
	let mid_a_origin = Origin::random().produce();
	let mut client_config = moq_native::ClientConfig::default();
	client_config.tls.disable_verify = Some(true);
	let mid_a_client = client_config.init().expect("mid-a client init");
	let mid_a_upstream = within(
		"MID-A connects to TOP",
		mid_a_client
			.with_subscriber(mid_a_origin.clone())
			.connect(top_url.parse().expect("parse top url")),
	)
	.await
	.expect("mid-a upstream connect");
	let _top_session_a = within("TOP accepts MID-A", top_accepted.recv())
		.await
		.expect("TOP accept channel closed");

	let (mid_a_port, mut mid_a_accepted, _mid_a_handle) = spawn_upstream(mid_a_origin.clone());
	wait_listening(mid_a_port).await;
	let mid_a_url = format!("tcp://127.0.0.1:{mid_a_port}/");

	// ── BOTTOM: full relay clustered to MID-A ───────────────────────────
	let (bottom_port, _bottom_handle) = spawn_relay_with_upstream(&mid_a_url, None).await;
	// MID-A's first inbound connection is BOTTOM's cluster dial: hold its
	// server-side session so we can drain it.
	let session_bottom_on_a = within("MID-A accepts BOTTOM", mid_a_accepted.recv())
		.await
		.expect("MID-A accept channel closed");

	// ── SUBSCRIBER: connects to BOTTOM ───────────────────────────────────
	let sub_origin = Origin::random().produce();
	let mut sub_client_config = moq_native::ClientConfig::default();
	sub_client_config.tls.disable_verify = Some(true);
	let sub_client = sub_client_config.init().expect("subscriber client init");
	let sub_session = within(
		"subscriber connects to BOTTOM",
		sub_client
			.with_subscriber(sub_origin.clone())
			.connect(format!("tcp://127.0.0.1:{bottom_port}/").parse().expect("parse url")),
	)
	.await
	.expect("subscriber connect");

	// Watch announcements for the whole test: a seamless failover must never
	// unannounce the broadcast under the subscriber.
	let mut announcements = sub_origin.consume().announced();
	let first = within("broadcast announced through the MID-A leg", announcements.next())
		.await
		.expect("origin closed before the announce");
	assert_eq!(first.path.as_str(), "diamond");

	let bc = within(
		"broadcast resolves on the subscriber origin",
		sub_origin.consume().announced_broadcast("diamond"),
	)
	.await
	.expect("broadcast announced");
	let mut sub = within(
		"subscribe to the video track",
		bc.track("video").expect("track handle").subscribe(None),
	)
	.await
	.expect("subscribe");

	// ── group 0 flows through the MID-A leg (2 frames, fully verified) ──
	const FRAMES_PER_GROUP: u64 = 3;
	let mut g = track.append_group().expect("append group");
	for f in 0..FRAMES_PER_GROUP {
		let payload = format!("diamond_g0_f{f}");
		g.write_frame(moq_net::Timestamp::ZERO, payload.as_bytes())
			.expect("write frame");
	}
	g.finish().expect("finish");

	verify_group(&mut sub, 0, FRAMES_PER_GROUP, "pre-failover (via MID-A)").await;

	// ── continuous publishing THROUGH the failover window ────────────────
	// Groups 1..=LAST_GROUP stream at a steady cadence, with multiple frames
	// per group, while the GOAWAY, reconnect, and handover happen mid-stream.
	const LAST_GROUP: u64 = 20;
	let publisher = tokio::spawn(async move {
		for seq in 1..=LAST_GROUP {
			let mut g = track.append_group().expect("append group");
			for f in 0..FRAMES_PER_GROUP {
				let payload = format!("diamond_g{seq}_f{f}");
				g.write_frame(moq_net::Timestamp::ZERO, payload.as_bytes())
					.expect("write frame");
			}
			g.finish().expect("finish");
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
		// Hand the track back so the post-drain phase can keep publishing.
		track
	});

	// ── TRIGGER: MID-A drains BOTTOM with a redirect to MID-B ───────────
	let draining = session_bottom_on_a
		.drain()
		.expect("drain")
		.start_with_timeout(mid_b_url.clone(), Duration::from_secs(5));

	// Positive gate: MID-B's first inbound connection can only be BOTTOM's
	// post-GOAWAY reconnect (the subscriber talks to BOTTOM, and MID-B's link
	// to TOP is outbound). Without this, the test could pass with a broken
	// reconnect because groups can still flow through the draining old leg.
	within("BOTTOM reconnects to MID-B", mid_b_accepted_rx)
		.await
		.expect("MID-B accept notify dropped");

	// ── completeness across the swap: every group exactly once, in order,
	// every frame intact, exact frame count (no loss, no duplicates) ──────
	for seq in 1..=LAST_GROUP {
		verify_group(&mut sub, seq, FRAMES_PER_GROUP, "across the failover window").await;
	}

	let mut track = within("publisher task finishes", publisher)
		.await
		.expect("publisher task");

	// ── the old MID-A leg drains away, then is severed entirely ─────────
	within("old session drains after the swap", draining.complete()).await;
	// Cut MID-A off from TOP so it can never receive (let alone forward) new
	// groups. Anything delivered from here on MUST have flowed TOP -> MID-B ->
	// BOTTOM, positively proving the new leg carries the subscription.
	drop(mid_a_upstream);

	const POST_DRAIN_LAST: u64 = LAST_GROUP + 3;
	for seq in (LAST_GROUP + 1)..=POST_DRAIN_LAST {
		let mut g = track.append_group().expect("append group");
		for f in 0..FRAMES_PER_GROUP {
			let payload = format!("diamond_g{seq}_f{f}");
			g.write_frame(moq_net::Timestamp::ZERO, payload.as_bytes())
				.expect("write frame");
		}
		g.finish().expect("finish");
	}
	for seq in (LAST_GROUP + 1)..=POST_DRAIN_LAST {
		verify_group(&mut sub, seq, FRAMES_PER_GROUP, "post-drain (MID-B leg only)").await;
	}

	// ── no GOAWAY cascade to the downstream subscriber ───────────────────
	assert!(
		!sub_session.is_going_away(),
		"BOTTOM must not propagate the upstream GOAWAY downstream"
	);
	// The async observer must stay pending too (bounded probe: everything
	// above already synchronized, so 2s of silence is decisive).
	let leaked = tokio::time::timeout(Duration::from_secs(2), sub_session.goaway()).await;
	assert!(
		leaked.is_err(),
		"downstream subscriber received a GOAWAY (the relay should absorb it): {leaked:?}"
	);

	// ── announcement stability: the path never churned under the swap ────
	let churn = tokio::time::timeout(Duration::from_millis(500), announcements.next()).await;
	assert!(churn.is_err(), "failover must not churn announces under the subscriber");
}

/// Receive the next group and assert its sequence, every frame's payload, and
/// the exact frame count. `stage` names the failover phase for diagnostics.
async fn verify_group(sub: &mut moq_net::track::Subscriber, expected_seq: u64, frames: u64, stage: &str) {
	let mut group = within(&format!("recv group {expected_seq} {stage}"), sub.recv_group())
		.await
		.unwrap_or_else(|err| panic!("subscription errored at group {expected_seq} {stage}: {err}"))
		.unwrap_or_else(|| panic!("track ended early at group {expected_seq} {stage}"));
	assert_eq!(
		group.sequence, expected_seq,
		"groups must arrive exactly once, in order ({stage})"
	);
	for f in 0..frames {
		let frame = within(&format!("read frame {f} of group {expected_seq}"), group.read_frame())
			.await
			.unwrap_or_else(|err| panic!("frame read errored at group {expected_seq} frame {f} {stage}: {err}"))
			.unwrap_or_else(|| panic!("group {expected_seq} lost frame {f} {stage}"));
		let expected = format!("diamond_g{expected_seq}_f{f}");
		assert_eq!(
			frame.payload[..],
			expected.as_bytes()[..],
			"frame content must survive the failover intact ({stage})"
		);
	}
	let extra = within(&format!("group {expected_seq} frame-count check"), group.read_frame())
		.await
		.unwrap_or_else(|err| panic!("frame-count check errored at group {expected_seq} {stage}: {err}"));
	assert!(
		extra.is_none(),
		"group {expected_seq} must carry exactly {frames} frames, no duplicates ({stage})"
	);
}

/// An empty-URI GOAWAY ("reconnect to me") makes the cluster redial the same
/// endpoint. The upstream keeps its origin across the restart, so the rejoined
/// route resumes delivery.
#[tokio::test]
async fn cluster_reconnects_on_empty_uri_goaway() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let upstream_origin = Origin::random().produce();
	let mut broadcast = upstream_origin
		.create_broadcast("cam", moq_net::broadcast::Route::new().with_announce(true))
		.expect("create broadcast");
	let mut track = broadcast.create_track("video", None).expect("create track");

	let (port, mut accepted, _handle) = spawn_upstream(upstream_origin.clone());
	wait_listening(port).await;

	let mut client_config = moq_native::ClientConfig::default();
	client_config.tls.disable_verify = Some(true);
	let client = client_config.init().expect("client init");

	let mut cluster_config = ClusterConfig::default();
	cluster_config.connect = vec![format!("tcp://127.0.0.1:{port}/")];
	cluster_config.drain_timeout = Some(2);
	let cluster = Cluster::new(cluster_config).expect("cluster init").with_client(client);
	let cluster_run = tokio::spawn(cluster.clone().run());

	let first_dial = within("upstream accepts the cluster dial", accepted.recv())
		.await
		.expect("accept channel closed");

	// Downstream consumer sees group 0 through the first session.
	let bc = within(
		"broadcast announced",
		cluster.origin.consume().announced_broadcast("cam"),
	)
	.await
	.expect("broadcast announced");
	let mut sub = within("subscribe", async {
		bc.track("video").expect("track handle").subscribe(None).await
	})
	.await
	.expect("subscribe");

	let mut g = track.append_group().expect("append group");
	g.write_frame(moq_net::Timestamp::ZERO, b"empty_g0".as_ref())
		.expect("write frame");
	g.finish().expect("finish");
	let mut g0 = within("recv g0", sub.recv_group())
		.await
		.expect("recv")
		.expect("track ended early");
	assert_eq!(g0.sequence, 0);
	assert_eq!(
		g0.read_frame().await.expect("read").expect("frame").payload[..],
		b"empty_g0"[..]
	);

	// Drain with an EMPTY URI: the cluster must fall back to redialing the
	// originally configured endpoint.
	let draining = first_dial.drain().expect("drain").start("");

	// Positive gate: the upstream accepts a SECOND session (the redial).
	let _second_dial = within("cluster redials the same endpoint", accepted.recv())
		.await
		.expect("accept channel closed");

	within("old session drains", draining.complete()).await;

	// Delivery continues on the redialed session (same origin, same publisher
	// identity, so the rejoined route resumes at the boundary).
	let mut g = track.append_group().expect("append group");
	g.write_frame(moq_net::Timestamp::ZERO, b"empty_g1".as_ref())
		.expect("write frame");
	g.finish().expect("finish");
	let mut g1 = within("recv g1 after the redial", sub.recv_group())
		.await
		.expect("recv")
		.expect("track ended early");
	assert_eq!(g1.sequence, 1, "delivery must resume contiguously after the redial");
	assert_eq!(
		g1.read_frame().await.expect("read").expect("frame").payload[..],
		b"empty_g1"[..]
	);

	cluster_run.abort();
}
