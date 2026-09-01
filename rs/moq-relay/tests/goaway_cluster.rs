//! Upstream GOAWAY migration over real TCP transports.
//!
//! Two "sibling" upstream servers share one origin (the same live broadcast is
//! reachable through either). The relay's cluster dials sibling A; A sends a
//! GOAWAY redirecting to sibling B; the cluster reconnects to B and the origin
//! hands the live subscription over at a group boundary. A downstream consumer
//! of the cluster origin observes contiguous groups and no unannounce.

use std::collections::BTreeSet;
use std::net::TcpListener;
use std::time::Duration;

use moq_net::Hop;
use moq_relay::{AuthConfig, Cluster, ClusterConfig, Connection, PublicConfig};
use url::Url;

const TEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Bound `fut` by [`TEST_TIMEOUT`], panicking with `step` so a hang names the
/// exact stage that failed instead of a bare "test timed out".
async fn within<T>(step: &str, fut: impl std::future::Future<Output = T>) -> T {
	tokio::time::timeout(TEST_TIMEOUT, fut)
		.await
		.unwrap_or_else(|_| panic!("timed out: {step}"))
}

/// Run an integration-test future on a dedicated thread with a large stack.
///
/// Under `--all-features`, `moq-tokio` compiles every transport backend
/// (quinn, quiche, noq, iroh, websocket) into its `Session`/`Client` types.
/// These multi-relay tests hold several such values live across await points,
/// so the single test future's state machine is large, and in an unoptimized
/// build it overflows libtest's default 2 MiB per-test thread stack (a SIGABRT
/// "stack overflow", surfacing as SIGSEGV on some runners). Running the body on
/// a 32 MiB thread with a current-thread runtime keeps the same execution model
/// as `#[tokio::test]` while giving the future room.
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

/// Bind a stream-only moq server to a free loopback TCP port, retrying if the
/// port is claimed in the window between the free-port probe and the real bind.
/// Returns the chosen port and the initialized server. Avoids the spurious
/// `init()` panic that a probe/drop/bind race can cause under parallel tests.
fn bind_free_tcp_server() -> (u16, moq_tokio::Server) {
	for _ in 0..20 {
		let probe = TcpListener::bind("127.0.0.1:0").expect("bind probe");
		let port = probe.local_addr().expect("local addr").port();
		drop(probe);

		let mut config = moq_tokio::listen::Config::default();
		config.tcp.bind = Some(format!("127.0.0.1:{port}").parse().expect("parse addr"));
		if let Ok(server) = config.init(Default::default()) {
			return (port, server);
		}
	}
	panic!("could not bind a free TCP port after 20 attempts");
}

#[test]
fn cluster_migrates_on_upstream_goaway() {
	run_cluster_test(cluster_migrates_on_upstream_goaway_inner());
}

#[test]
fn cluster_diamond_goaway_seamless_failover() {
	run_cluster_test(cluster_diamond_goaway_seamless_failover_inner());
}

#[test]
fn drain_session_with_zero_timeout_closes_at_once() {
	run_cluster_test(drain_session_with_zero_timeout_closes_at_once_inner());
}

/// A zero drain window means no grace, so `drain_session` closes the session
/// rather than sending a GOAWAY the peer has no time to act on.
///
/// The regression it guards: `Goaway::with_timeout(ZERO)` means "no deadline" on
/// the wire, so passing the window straight through left the session with no
/// force-close at all and `drain_session` awaiting a peer that never leaves.
async fn drain_session_with_zero_timeout_closes_at_once_inner() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let origin = moq_tokio::origin::spawn(Hop::random());
	let (port, mut accepted, _handle) = spawn_upstream(origin);
	wait_listening(port).await;

	let mut client_config = moq_tokio::connect::Config::default();
	client_config.tls.insecure = Some(true);
	let client = client_config.init(Default::default()).expect("client init");
	let (_client_connection_client, client_connection) = within("client connects", async {
		connect_once(client, format!("tcp://127.0.0.1:{port}/").parse().expect("parse url")).await
	})
	.await
	.expect("connect");

	let server_session = within("upstream accepts", accepted.recv())
		.await
		.expect("accept channel closed");

	let (_trigger, shutdown) = moq_relay::Shutdown::new(Duration::ZERO);

	// The assertion is that this resolves at all: with the window passed through as
	// a wire timeout it would await a peer under no deadline to leave.
	within("drain_session returns", shutdown.drain_session(&server_session)).await;

	// The peer observes the close rather than being left connected.
	let _ = within("client observes the close", client_connection.closed()).await;
}

#[test]
fn cluster_reconnects_on_empty_uri_goaway() {
	run_cluster_test(cluster_reconnects_on_empty_uri_goaway_inner());
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
	let (port, server) = bind_free_tcp_server();

	let (accepted_tx, accepted_rx) = tokio::sync::mpsc::unbounded_channel();

	let handle = tokio::spawn(async move {
		let mut server = server.listen().await.expect("listen");
		while let Some(request) = server.accept().await {
			// Serve the shared origin bidirectionally, like a relay peer would.
			let scratch = moq_tokio::origin::spawn(Hop::random());
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

/// An upstream GOAWAY with a redirect migrates the cluster dial to the sibling.
/// The draining session keeps serving through the handover window, and the path
/// never retracts on the cluster origin (route re-pricing and the sibling's
/// announce surface as metadata updates, not churn).
async fn cluster_migrates_on_upstream_goaway_inner() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	tokio::time::timeout(TEST_TIMEOUT, async {
		// ── the shared "live" broadcast both siblings can serve ─────────
		let upstream_origin = moq_tokio::origin::spawn(Hop::random());
		let mut broadcast = upstream_origin.create_broadcast("cam").expect("create broadcast");
		let _announce_broadcast = upstream_origin
			.announce("cam", Default::default())
			.expect("create broadcast");
		let mut track = broadcast.create_track("video", None).expect("create track");

		let (port_a, mut accepted_a, _handle_a) = spawn_upstream(upstream_origin.clone());
		let (port_b, mut accepted_b, _handle_b) = spawn_upstream(upstream_origin.clone());
		wait_listening(port_a).await;
		wait_listening(port_b).await;

		// ── the relay cluster under test, dialing sibling A ─────────────
		let mut client_config = moq_tokio::connect::Config::default();
		client_config.tls.insecure = Some(true);
		// Short handover so the test observes the old session close quickly.
		client_config.goaway.handover = Duration::from_secs(2).into();
		let client = client_config.init(Default::default()).expect("client init");

		let mut cluster_config = ClusterConfig::default();
		cluster_config.connect = vec![format!("tcp://127.0.0.1:{port_a}/")];
		let cluster = Cluster::new(cluster_config).expect("cluster init").with_client(client);

		let started = cluster.clone().start().await.expect("cluster start");
		let cluster_run = tokio::spawn(started.run());

		// A dials in; hold its server-side session so we can drain it.
		let session_a = accepted_a.recv().await.expect("sibling A accepts the cluster dial");

		// ── downstream consumer on the cluster origin ───────────────────
		let consumer = cluster.origin.consume();
		consumer
			.routed("cam")
			.await
			.expect("broadcast announced through sibling A");
		let bc = consumer.request_broadcast("cam").await.expect("broadcast resolves");
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

		// Watch for retraction during the swap: the migration must never
		// unannounce the path (metadata updates are expected: the drain
		// re-prices the old route and the sibling announces its own).
		let mut announcements = cluster.origin.consume().announced();
		let first = announcements.next().await.expect("initial announce");
		assert_eq!(first.prefix.as_path().as_str(), "cam");

		// ── sibling A drains with a redirect to sibling B ────────────────
		session_a
			.drain()
			.send(moq_net::goaway::Goaway::redirect(format!("tcp://127.0.0.1:{port_b}/")))
			.expect("send goaway");

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
		// handover window at the latest).
		session_a.closed().await;

		// No retraction leaked to the origin during the whole swap. Metadata
		// updates (the drain re-pricing, the sibling's route) are expected and
		// harmless; an inactive event means the path flapped.
		loop {
			match tokio::time::timeout(Duration::from_millis(500), announcements.next()).await {
				Err(_) => break,
				Ok(Some(update)) if update.active => continue,
				Ok(event) => panic!("migration must not retract the path on the cluster origin: {event:?}"),
			}
		}

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

	let (port, server) = bind_free_tcp_server();
	let mut server = server.listen().await.expect("listen");

	// Fully public auth: any no-JWT stream client gets the whole root.
	#[allow(deprecated)]
	let public = PublicConfig::Simple(vec![String::new()]);
	let mut auth_config = AuthConfig::default();
	auth_config.public = Some(public);
	let auth = auth_config
		.init(&moq_tokio::tls::Connect::default())
		.await
		.expect("auth init");

	let mut cluster_config = ClusterConfig::default();
	cluster_config.connect = vec![upstream_url.to_string()];
	// Short drain so the test observes teardown quickly.

	let mut client_config = moq_tokio::connect::Config::default();
	client_config.tls.insecure = Some(true);
	// Short handover so the test observes the old session close quickly.
	client_config.goaway.handover = Duration::from_secs(2).into();
	let client = client_config.init(Default::default()).expect("client init");

	let cluster = Cluster::new(cluster_config).expect("cluster init").with_client(client);

	let started = cluster.clone().start().await.expect("cluster start");
	let handle = tokio::spawn(async move {
		tokio::spawn(async move {
			let _ = started.run().await;
		});

		let mut accept_notify = accept_notify;
		let mut id = 0;
		while let Some(request) = server.accept().await {
			if let Some(notify) = accept_notify.take() {
				let _ = notify.send(());
			}
			let conn = Connection::new(request, cluster.clone(), auth.clone())
				.with_id(id)
				.with_shutdown(moq_relay::Shutdown::disabled());
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
/// Proves the failover contract across a GOAWAY:
/// 1. Content flows TOP -> MID-A -> BOTTOM -> subscriber.
/// 2. On MID-A's GOAWAY naming MID-B, BOTTOM reconnects there (positively
///    gated: MID-B's first inbound connection can only be that reconnect).
/// 3. The draining MID-A leg keeps serving through the handover window, so
///    every group published across the swap arrives exactly once.
/// 4. Once the old leg closes, the subscription through it ends (failover is
///    a resubscribe, not a transparent splice) and a fresh subscription
///    through MID-B carries the rest.
/// 5. No GOAWAY leaks to the subscriber's own session, and the path never
///    retracts under the subscriber.
async fn cluster_diamond_goaway_seamless_failover_inner() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	// ── TOP: origin server serving the same broadcast to both mids ──────
	let top_origin = moq_tokio::origin::spawn(Hop::random());
	let mut broadcast = top_origin.create_broadcast("diamond").expect("create broadcast");
	let _announce_broadcast = top_origin
		.announce("diamond", Default::default())
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
	let mid_a_origin = moq_tokio::origin::spawn(Hop::random());
	let mut client_config = moq_tokio::connect::Config::default();
	client_config.tls.insecure = Some(true);
	// Short handover so the test observes the old session close quickly.
	client_config.goaway.handover = Duration::from_secs(2).into();
	let mid_a_client = client_config.init(Default::default()).expect("mid-a client init");
	let (_mid_a_upstream_client, mid_a_upstream) = within(
		"MID-A connects to TOP",
		connect_once(
			mid_a_client.with_subscriber(mid_a_origin.clone()),
			top_url.parse().expect("parse top url"),
		),
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
	let sub_origin = moq_tokio::origin::spawn(Hop::random());
	let mut sub_client_config = moq_tokio::connect::Config::default();
	sub_client_config.tls.insecure = Some(true);
	let sub_client = sub_client_config
		.init(Default::default())
		.expect("subscriber client init");
	let (_sub_connection_client, sub_connection) = within(
		"subscriber connects to BOTTOM",
		connect_once(
			sub_client.with_subscriber(sub_origin.clone()),
			format!("tcp://127.0.0.1:{bottom_port}/").parse().expect("parse url"),
		),
	)
	.await
	.expect("subscriber connect");

	// Watch announcements for the whole test: the failover must never
	// unannounce the broadcast under the subscriber.
	let mut announcements = sub_origin.consume().announced();
	let first = within("broadcast announced through the MID-A leg", announcements.next())
		.await
		.expect("origin closed before the announce");
	assert_eq!(first.prefix.as_path().as_str(), "diamond");

	let bc = within("broadcast resolves on the subscriber origin", async {
		let consumer = sub_origin.consume();
		consumer.routed("diamond").await?;
		consumer.request_broadcast("diamond").await.ok()
	})
	.await
	.expect("broadcast announced");
	// The age budget is what makes the completeness check below meaningful: this test
	// asserts every group arrives exactly once, which is more than the default budget
	// promises. A subscriber that wants completeness across a failover has to say how
	// far behind the live edge it is willing to sit (clamped to what the publisher
	// retains).
	//
	// Arrival order, not sequence order: a publisher transmits the newest group of a
	// track first, so a back-to-back burst legally arrives inverted. What the failover
	// must preserve is that every group arrives, exactly once, with its frames intact.
	let mut sub = within(
		"subscribe to the video track",
		bc.track("video")
			.expect("track handle")
			.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_secs(60))),
	)
	.await
	.expect("subscribe");

	// ── group 0 flows through the MID-A leg (all frames verified) ──
	const FRAMES_PER_GROUP: u64 = 3;
	let mut g = track.append_group().expect("append group");
	for f in 0..FRAMES_PER_GROUP {
		let payload = format!("diamond_g0_f{f}");
		g.write_frame(moq_net::Timestamp::ZERO, payload.as_bytes())
			.expect("write frame");
	}
	g.finish().expect("finish");

	let mut seen = BTreeSet::new();
	collect_group(&mut sub, &mut seen, FRAMES_PER_GROUP, "pre-failover (via MID-A)").await;
	assert_eq!(seen, BTreeSet::from([0]), "group 0 must arrive before the failover");

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
	session_bottom_on_a
		.drain()
		.send(moq_net::goaway::Goaway::redirect(mid_b_url.clone()).with_timeout(Duration::from_secs(5)))
		.expect("send goaway");

	// Positive gate: MID-B's first inbound connection can only be BOTTOM's
	// post-GOAWAY reconnect (the subscriber talks to BOTTOM, and MID-B's link
	// to TOP is outbound). Without this, the test could pass with a broken
	// reconnect because groups can still flow through the draining old leg.
	within("BOTTOM reconnects to MID-B", mid_b_accepted_rx)
		.await
		.expect("MID-B accept notify dropped");

	// ── completeness across the swap: every group exactly once, in order,
	// every frame intact, exact frame count (no loss, no duplicates) ──────
	for _ in 1..=LAST_GROUP {
		collect_group(&mut sub, &mut seen, FRAMES_PER_GROUP, "across the failover window").await;
	}
	assert_eq!(
		seen,
		(0..=LAST_GROUP).collect::<BTreeSet<_>>(),
		"every group must cross the failover exactly once"
	);

	let mut track = within("publisher task finishes", publisher)
		.await
		.expect("publisher task");

	// ── the old MID-A leg drains away, then is severed entirely ─────────
	within("old session drains after the swap", session_bottom_on_a.closed()).await;
	// Cut MID-A off from TOP so it can never receive (let alone forward) new
	// groups. Anything delivered from here on MUST have flowed TOP -> MID-B ->
	// BOTTOM, positively proving the new leg carries the resubscribe.
	drop(mid_a_upstream);

	// The subscription was served through the MID-A leg, so its close ends it:
	// failover is a resubscribe, not a transparent splice. Drain any groups
	// still in flight, then observe the end.
	within("old subscription ends with the drained leg", async {
		while let Ok(Some(_)) = sub.recv_group().await {}
	})
	.await;

	// Resubscribe on the same broadcast handle: the subscriber's session with
	// BOTTOM is intact, and BOTTOM now resolves the track through MID-B.
	drop(bc);
	let bc = within("broadcast re-resolves after the failover", async {
		let consumer = sub_origin.consume();
		consumer.request_broadcast("diamond").await.ok()
	})
	.await
	.expect("broadcast re-resolves");
	let mut sub = within(
		"resubscribe after the failover",
		bc.track("video")
			.expect("track handle")
			.subscribe(moq_net::track::Subscription::default().with_max_age(Duration::from_secs(60))),
	)
	.await
	.expect("resubscribe");

	// A lite-05 subscription starts at the live edge (its Max Age is a staleness
	// tolerance, not a replay request), so publish at a steady cadence and collect
	// what lands once the fresh chain establishes. MID-A is severed, so any
	// post-drain group can only have flowed TOP -> MID-B -> BOTTOM.
	const POST_DRAIN_LAST: u64 = LAST_GROUP + 40;
	let post_publisher = tokio::spawn(async move {
		for seq in (LAST_GROUP + 1)..=POST_DRAIN_LAST {
			let mut g = track.append_group().expect("append group");
			for f in 0..FRAMES_PER_GROUP {
				let payload = format!("diamond_g{seq}_f{f}");
				g.write_frame(moq_net::Timestamp::ZERO, payload.as_bytes())
					.expect("write frame");
			}
			g.finish().expect("finish");
			tokio::time::sleep(Duration::from_millis(50)).await;
		}
	});

	// Three distinct post-drain groups, every frame verified, proves the new leg
	// carries the subscription (the establish may still see the pre-swap edge).
	let mut post = BTreeSet::new();
	while post.iter().filter(|seq| **seq > LAST_GROUP).count() < 3 {
		collect_group(&mut sub, &mut post, FRAMES_PER_GROUP, "post-drain (MID-B leg only)").await;
	}
	post_publisher.abort();

	// ── no GOAWAY cascade to the downstream subscriber ───────────────────
	assert!(
		sub_connection.draining().expect("connected").peek().is_none(),
		"BOTTOM must not propagate the upstream GOAWAY downstream"
	);
	// The async observer must stay pending too (bounded probe: everything
	// above already synchronized, so 2s of silence is decisive).
	let leaked = tokio::time::timeout(
		Duration::from_secs(2),
		sub_connection.draining().expect("connected").recv(),
	)
	.await;
	assert!(
		leaked.is_err(),
		"downstream subscriber received a GOAWAY (the relay should absorb it): {leaked:?}"
	);

	// ── announcement stability: the path never retracted under the swap ──
	// Metadata updates (route re-pricing, the new leg's hops) are expected.
	loop {
		match tokio::time::timeout(Duration::from_millis(500), announcements.next()).await {
			Err(_) => break,
			Ok(Some(update)) if update.active => continue,
			Ok(event) => panic!("failover must not retract the path under the subscriber: {event:?}"),
		}
	}
}

/// Receive the next group, assert every frame's payload and the exact frame count
/// against the sequence it carries, and record that sequence in `seen`.
///
/// Sequence order is deliberately not asserted: a publisher transmits the newest group
/// of a track first, so a back-to-back burst legally arrives inverted. Completeness is
/// what the failover owes, and `seen` is what the caller checks it against.
/// `stage` names the failover phase for diagnostics.
async fn collect_group(sub: &mut moq_net::track::Subscriber, seen: &mut BTreeSet<u64>, frames: u64, stage: &str) {
	let mut group = within(&format!("recv a group {stage}"), sub.recv_group())
		.await
		.unwrap_or_else(|err| panic!("subscription errored {stage}: {err}"))
		.unwrap_or_else(|| panic!("track ended early {stage}"));
	let expected_seq = group.sequence;
	assert!(
		seen.insert(expected_seq),
		"group {expected_seq} arrived twice ({stage})"
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
/// endpoint. The subscription through the drained session ends with it, and a
/// resubscribe through the redialed session resumes delivery.
async fn cluster_reconnects_on_empty_uri_goaway_inner() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let upstream_origin = moq_tokio::origin::spawn(Hop::random());
	let mut broadcast = upstream_origin.create_broadcast("cam").expect("create broadcast");
	let _announce_broadcast = upstream_origin
		.announce("cam", Default::default())
		.expect("create broadcast");
	let mut track = broadcast.create_track("video", None).expect("create track");

	let (port, mut accepted, _handle) = spawn_upstream(upstream_origin.clone());
	wait_listening(port).await;

	let mut client_config = moq_tokio::connect::Config::default();
	client_config.tls.insecure = Some(true);
	// Short handover so the test observes the old session close quickly.
	client_config.goaway.handover = Duration::from_secs(2).into();
	let client = client_config.init(Default::default()).expect("client init");

	let mut cluster_config = ClusterConfig::default();
	cluster_config.connect = vec![format!("tcp://127.0.0.1:{port}/")];
	let cluster = Cluster::new(cluster_config).expect("cluster init").with_client(client);
	let started = cluster.clone().start().await.expect("cluster start");
	let cluster_run = tokio::spawn(started.run());

	let first_dial = within("upstream accepts the cluster dial", accepted.recv())
		.await
		.expect("accept channel closed");

	// Downstream consumer sees group 0 through the first session.
	let bc = within("broadcast announced", async {
		let consumer = cluster.origin.consume();
		consumer.routed("cam").await?;
		consumer.request_broadcast("cam").await.ok()
	})
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
	first_dial
		.drain()
		.send(moq_net::goaway::Goaway::new())
		.expect("send goaway");

	// Positive gate: the upstream accepts a SECOND session (the redial).
	let _second_dial = within("cluster redials the same endpoint", accepted.recv())
		.await
		.expect("accept channel closed");

	within("old session drains", first_dial.closed()).await;

	// The broadcast was materialized through the first session, so its close ends
	// the subscription: failover is a resubscribe, not a transparent splice.
	within("old subscription ends with the session", async {
		while let Ok(Some(_)) = sub.recv_group().await {}
	})
	.await;

	// Re-request through the redialed session's route and resubscribe.
	let bc = within("broadcast resolves through the redial", async {
		cluster.origin.consume().request_broadcast("cam").await.ok()
	})
	.await
	.expect("broadcast resolves");
	let mut sub = within("resubscribe", async {
		bc.track("video").expect("track handle").subscribe(None).await
	})
	.await
	.expect("resubscribe");

	let mut g = track.append_group().expect("append group");
	g.write_frame(moq_net::Timestamp::ZERO, b"empty_g1".as_ref())
		.expect("write frame");
	g.finish().expect("finish");
	// The fresh subscription may start at the retained group 0; skip up to g1.
	let mut g1 = loop {
		let g = within("recv g1 after the redial", sub.recv_group())
			.await
			.expect("recv")
			.expect("track ended early");
		if g.sequence >= 1 {
			break g;
		}
	};
	assert_eq!(g1.sequence, 1, "the redialed session must deliver the new group");
	assert_eq!(
		g1.read_frame().await.expect("read").expect("frame").payload[..],
		b"empty_g1"[..]
	);

	cluster_run.abort();
}

/// Dial once and hand back the client with its connection.
///
/// These tests want a single transport, so reconnecting is off: there is nothing
/// left to redial, and dropping the connection closes the transport because it
/// holds the last session clone.
///
/// The client comes back because it owns the transport endpoint (iroh's dies with
/// it), and the caller has to outlive the connection it just got.
async fn connect_once(
	client: moq_tokio::Client,
	url: url::Url,
) -> moq_tokio::Result<(moq_tokio::Client, moq_tokio::Connection)> {
	let connection = client.clone().with_reconnect(false).connect(url).established().await?;
	Ok((client, connection))
}

#[test]
fn goaway_handover_is_enforced_while_the_replacement_dial_hangs() {
	run_cluster_test(goaway_handover_is_enforced_while_the_replacement_dial_hangs_inner());
}

/// The handover deadline has to hold across the replacement dial, not just across
/// the sleeps around it.
///
/// A GOAWAY off a healthy session goes straight into the replacement dial with the
/// old one still draining. Redirect that dial at a listener that accepts TCP and
/// then says nothing, so the handshake hangs, and the predecessor must still be
/// closed on schedule rather than held for as long as the dial takes.
async fn goaway_handover_is_enforced_while_the_replacement_dial_hangs_inner() {
	let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

	let upstream_origin = moq_tokio::origin::spawn(Hop::random());
	let (port, mut accepted, _handle) = spawn_upstream(upstream_origin);
	wait_listening(port).await;

	// Accepts the connection, then never speaks MoQ: the dial hangs in the handshake.
	let black_hole = TcpListener::bind("127.0.0.1:0").expect("bind black hole");
	let black_hole_port = black_hole.local_addr().expect("local addr").port();
	let _black_hole = std::thread::spawn(move || {
		// Hold every accepted socket open and idle for the life of the test.
		let mut held = Vec::new();
		while let Ok((socket, _)) = black_hole.accept() {
			held.push(socket);
		}
	});

	let handover = Duration::from_millis(200);
	let mut client_config = moq_tokio::connect::Config::default();
	client_config.tls.insecure = Some(true);
	client_config.goaway.handover = handover.into();
	// The GOAWAY has to land on a *healthy* session, which is the path that goes
	// straight into the replacement dial. Below this bar it takes the immediate
	// redirect path instead, whose sleep polls the drain either way.
	client_config.backoff.initial = Duration::from_millis(50).into();
	let client = client_config.init(Default::default()).expect("client init");

	let url: Url = format!("tcp://127.0.0.1:{port}/").parse().expect("parse url");
	let connection = client.connect(url);
	let connection = within("client connects", connection.established())
		.await
		.expect("connect");

	let server_session = within("upstream accepts", accepted.recv())
		.await
		.expect("accept channel closed");

	// Clear the healthy bar, so the GOAWAY continues straight into the replacement
	// dial rather than sleeping first.
	tokio::time::sleep(Duration::from_millis(120)).await;

	// Send the client somewhere that will never finish handshaking. No wire timeout:
	// our own `--goaway-handover` is then the only thing bounding the old session.
	server_session
		.drain()
		.send(moq_net::goaway::Goaway::redirect(format!(
			"tcp://127.0.0.1:{black_hole_port}/"
		)))
		.expect("send goaway");

	// The predecessor must close on its own deadline, not whenever that dial gives
	// up. The bound is generous against a loaded runner but far below the seconds a
	// hanging handshake takes, which is the gap being measured.
	let sent = std::time::Instant::now();
	let closed = tokio::time::timeout(handover * 10, server_session.closed()).await;
	let elapsed = sent.elapsed();
	assert!(
		closed.is_ok() && elapsed < handover * 5,
		"the old session outlived its {handover:?} handover by {elapsed:?}: \
		 the replacement dial was never interrupted to enforce it"
	);

	drop(connection);
}
