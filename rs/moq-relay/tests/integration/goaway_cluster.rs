//! Upstream GOAWAY migration, on kio's simulator over in-memory links.
//!
//! Two "sibling" upstream servers share one origin (the same live broadcast is
//! reachable through either). The relay's cluster dials sibling A; A sends a
//! GOAWAY redirecting to sibling B; the cluster reconnects to B and the origin
//! hands the live subscription over at a group boundary. A downstream consumer
//! of the cluster origin observes contiguous groups and no unannounce.
//!
//! Every dial crosses a [`Net`], so [`kio::clock::sim::idle`] proves the mesh has
//! settled: nothing is left in flight that could still retract a route or send a
//! GOAWAY.

use std::collections::BTreeSet;
use std::time::Duration;

use futures::StreamExt;
use moq_relay::{cluster, shutdown};

use crate::memory::{self, Net};

/// A millisecond track with no publisher max age: the frames written here carry timestamps,
/// and superseded groups stay readable.
fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default()
		.with_timescale(moq_net::Timescale::MILLI)
		.with_max_age(None)
}

/// Long enough that only a hang reaches it; simulated time skips straight there.
const TEST_TIMEOUT: Duration = Duration::from_secs(15);

/// The cluster's first redial backoff. A GOAWAY this soon after connecting scores
/// the session as a failed attempt, so the redial waits this long first.
const BACKOFF: Duration = Duration::from_millis(300);

/// The cluster's `--connect-goaway-handover`: short, so a test observes the old session
/// close quickly, but past [`BACKOFF`] plus the redial, so the draining leg
/// serves until the replacement takes over.
const HANDOVER: Duration = Duration::from_secs(1);

/// Only lite-07 carries the instance that lets a failover resume the subscription.
const LITE_07: &str = "moq-lite-07-wip";

/// Bound `fut` by [`TEST_TIMEOUT`], panicking with `step` so a hang names the
/// exact stage that failed instead of a bare "test timed out".
async fn within<T>(step: &str, fut: impl std::future::Future<Output = T>) -> T {
	kio::clock::timeout(TEST_TIMEOUT, fut)
		.await
		.unwrap_or_else(|_| panic!("timed out: {step}"))
}

/// The dial template every cluster here uses: a quick redial and a short handover.
fn dialing() -> moq_tokio::connect::Config {
	let mut config = moq_tokio::connect::Config::default();
	config.goaway.handover = Some(HANDOVER.into());
	config.backoff.initial = Some(BACKOFF.into());
	config
}

/// A cluster dialing `peers` through `net`, its dials already started.
async fn clustered(net: &Net, peers: &[&str], connect: moq_tokio::connect::Config) -> cluster::Cluster {
	memory::start(memory::spawn_cluster(net.options(peers)), connect).await
}

/// The next session `accepted` hands over.
async fn accept(
	step: &str,
	accepted: &mut futures::channel::mpsc::UnboundedReceiver<moq_net::Session>,
) -> moq_net::Session {
	within(step, accepted.next())
		.await
		.expect("the upstream stopped accepting")
}

/// The routes `recorded` saw retract.
fn retractions(recorded: &std::sync::Mutex<Vec<(String, bool)>>) -> Vec<String> {
	recorded
		.lock()
		.unwrap()
		.iter()
		.filter(|(_, active)| !active)
		.map(|(prefix, _)| prefix.clone())
		.collect()
}

#[test]
fn cluster_migrates_on_upstream_goaway() {
	memory::run(cluster_migrates_on_upstream_goaway_inner);
}

#[test]
fn cluster_diamond_goaway_seamless_failover() {
	memory::run(cluster_diamond_goaway_seamless_failover_inner);
}

#[test]
fn drain_session_with_zero_timeout_closes_at_once() {
	memory::run(drain_session_with_zero_timeout_closes_at_once_inner);
}

/// A zero drain window means no grace, so `drain_session` closes the session
/// rather than sending a GOAWAY the peer has no time to act on.
///
/// The regression it guards: `Goaway::with_timeout(ZERO)` means "no deadline" on
/// the wire, so passing the window straight through left the session with no
/// force-close at all and `drain_session` awaiting a peer that never leaves.
async fn drain_session_with_zero_timeout_closes_at_once_inner() {
	let net = Net::new();
	let mut accepted = net.upstream("upstream", &memory::origin());

	let client_connection = within(
		"client connects",
		connect_once(net.client(Default::default()), net.url("upstream")),
	)
	.await
	.expect("connect");
	let server_session = accept("upstream accepts", &mut accepted).await;

	let (_trigger, shutdown) = shutdown::Observer::new(Duration::ZERO);

	// The assertion is that this resolves at all: with the window passed through as
	// a wire timeout it would await a peer under no deadline to leave.
	within("drain_session returns", shutdown.drain_session(&server_session)).await;

	// The peer observes the close rather than being left connected.
	let _ = within("client observes the close", client_connection.closed()).await;
}

#[test]
fn cluster_continues_a_group_split_by_goaway() {
	memory::run(cluster_continues_a_group_split_by_goaway_inner);
}

/// A GOAWAY that lands mid-group hands the rest of the group to the reader already
/// holding it, and the next group after it, never the split group a second time.
async fn cluster_continues_a_group_split_by_goaway_inner() {
	let net = Net::new();
	let upstream_origin = memory::origin();
	let broadcast = upstream_origin
		.route("cam", moq_net::origin::RouteOptions::default())
		.expect("create broadcast");
	let track = broadcast
		.broadcast()
		.create_track("video", timed())
		.expect("create track");

	let mut accepted_a = net.upstream("a", &upstream_origin);
	let mut accepted_b = net.upstream("b", &upstream_origin);
	let cluster = clustered(&net, &["a"], dialing()).await;

	let session_a = accept("A accepts", &mut accepted_a).await;
	let consumer = cluster.origin.consume();
	let bc = within("broadcast resolves", consumer.follow("cam", None).unwrap().available())
		.await
		.expect("broadcast resolves")
		.unwrap();
	let mut sub = within("subscribe", bc.track("video").expect("track").subscribe(None))
		.await
		.expect("subscribe");

	let write = |group: &mut moq_net::group::Producer, payload: &'static str| {
		group
			.write_frame(
				moq_net::Timestamp::from_millis(group.start).unwrap(),
				payload.as_bytes(),
			)
			.expect("write frame");
	};
	// A group's sequence and its next frame, or `None` at its end.
	async fn read(group: &mut moq_net::group::Consumer) -> (u64, Option<bytes::Bytes>) {
		let frame = within("read frame", group.read_frame()).await.expect("read");
		(group.start, frame.map(|frame| frame.payload))
	}

	// The first group is open, its first frame read, when A redirects to B.
	let mut split = track
		.append_group(track.largest().map_or(0, |largest| largest.group + 1))
		.expect("append group");
	write(&mut split, "a");
	let mut reading = within("recv the split group", sub.recv_group())
		.await
		.expect("recv")
		.expect("track ended");
	assert_eq!(read(&mut reading).await, (0, Some("a".into())));

	// The cursor replays A's route first. After the GOAWAY, A's ranks last, so the next
	// route under any other instance is B's.
	let mut announced = consumer.routes();
	let instance_a = match within("A's route", announced.next()).await.expect("announce update") {
		moq_net::origin::RouteEvent {
			kind: moq_net::origin::RouteKind::Start,
			announce: update,
		} => update.route.instance().clone(),
		other => panic!("expected A's route, got {other:?}"),
	};
	session_a
		.drain()
		.send(moq_net::goaway::Goaway::redirect(net.url("b")))
		.expect("send goaway");
	let _session_b = accept("B accepts", &mut accepted_b).await;
	within("routed via B", async {
		loop {
			let event = announced.next().await.expect("announce update");
			// Under another instance, B's route is another source: a restart.
			if let moq_net::origin::RouteEvent {
				kind:
					moq_net::origin::RouteKind::Start
					| moq_net::origin::RouteKind::Update
					| moq_net::origin::RouteKind::Restart,
				announce: update,
			} = event && *update.route.instance() != instance_a
			{
				break;
			}
		}
	})
	.await;

	write(&mut split, "b");
	split.close().expect("finish");
	assert_eq!(read(&mut reading).await, (0, Some("b".into())));
	assert_eq!(read(&mut reading).await, (0, None));

	let mut after = track
		.append_group(track.largest().map_or(0, |largest| largest.group + 1))
		.expect("append group");
	write(&mut after, "c");
	after.close().expect("finish");
	let mut next = within("recv the next group", sub.recv_group())
		.await
		.expect("recv")
		.expect("track ended");
	assert_eq!(
		read(&mut next).await,
		(1, Some("c".into())),
		"the split group came back"
	);
}

/// An upstream GOAWAY with a redirect migrates the cluster dial to the sibling.
/// The draining session keeps serving through the handover window, and the path
/// never retracts on the cluster origin (the sibling's announce surfaces as a
/// metadata update, not churn).
async fn cluster_migrates_on_upstream_goaway_inner() {
	// ── the shared "live" broadcast both siblings can serve ─────────
	let net = Net::new();
	let upstream_origin = memory::origin();
	let broadcast = upstream_origin
		.route("cam", moq_net::origin::RouteOptions::default())
		.expect("create broadcast");
	let track = broadcast
		.broadcast()
		.create_track("video", timed())
		.expect("create track");

	let mut accepted_a = net.upstream("a", &upstream_origin);
	let mut accepted_b = net.upstream("b", &upstream_origin);

	// ── the relay cluster under test, dialing sibling A ─────────────
	let cluster = clustered(&net, &["a"], dialing()).await;

	// A dials in; hold its server-side session so we can drain it.
	let session_a = accept("sibling A accepts the cluster dial", &mut accepted_a).await;

	// ── downstream consumer on the cluster origin ───────────────────
	let consumer = cluster.origin.consume();
	let bc = within("broadcast resolves", consumer.follow("cam", None).unwrap().available())
		.await
		.expect("broadcast resolves")
		.unwrap();
	let mut sub = within("subscribe", bc.track("video").expect("track handle").subscribe(None))
		.await
		.expect("subscribe");

	let mut group = track
		.append_group(track.largest().map_or(0, |largest| largest.group + 1))
		.expect("append group");
	group
		.write_frame(moq_net::Timestamp::from_millis(group.start).unwrap(), b"g0".as_ref())
		.expect("write frame");
	group.close().expect("finish");

	let mut g0 = within("recv g0", sub.recv_group())
		.await
		.expect("recv g0")
		.expect("track ended early");
	assert_eq!(
		within("read g0", g0.read_frame())
			.await
			.expect("read")
			.expect("frame")
			.payload[..],
		b"g0"[..]
	);

	// Watch for retraction during the swap: the migration must never
	// unannounce the path (metadata updates are expected: the sibling
	// announces its own route, which outranks the draining one).
	let recorded = memory::record(cluster.origin.consume().routes());

	// ── sibling A drains with a redirect to sibling B ────────────────
	session_a
		.drain()
		.send(moq_net::goaway::Goaway::redirect(net.url("b")))
		.expect("send goaway");

	// The cluster reconnects: sibling B accepts a session.
	let _session_b = accept("sibling B accepts the redirected dial", &mut accepted_b).await;

	// New content lands after the swap (both siblings serve the same
	// origin, so B has it).
	let mut group = track
		.append_group(track.largest().map_or(0, |largest| largest.group + 1))
		.expect("append group");
	group
		.write_frame(moq_net::Timestamp::from_millis(group.start).unwrap(), b"g1".as_ref())
		.expect("write frame");
	group.close().expect("finish");

	let mut g1 = within("recv g1", sub.recv_group())
		.await
		.expect("recv g1")
		.expect("track ended early");
	assert_eq!(
		g1.start, 1,
		"delivery must resume contiguously at the next group after the swap"
	);
	assert_eq!(
		within("read g1", g1.read_frame())
			.await
			.expect("read")
			.expect("frame")
			.payload[..],
		b"g1"[..]
	);

	// The old session drains away (the cluster force-closes it after the
	// handover window at the latest).
	within("A's session closes", session_a.closed()).await;

	// No retraction leaked to the origin during the whole swap, once the mesh
	// has settled. Metadata updates (the sibling's route winning) are expected
	// and harmless; an inactive event means the path flapped.
	kio::clock::sim::idle().await;
	let retracted = retractions(&recorded);
	assert!(
		retracted.is_empty(),
		"migration must not retract the path on the cluster origin: {retracted:?}"
	);
}

/// Diamond GOAWAY failover across relays:
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
/// 4. Once the old leg closes, the SAME subscription keeps flowing: both legs'
///    routes carry TOP's instance on lite-07, so BOTTOM's front re-splices onto
///    the MID-B leg instead of ending the subscription.
/// 5. No GOAWAY leaks to the subscriber's own session, and the path never
///    retracts under the subscriber.
async fn cluster_diamond_goaway_seamless_failover_inner() {
	let net = Net::new();
	let auth = memory::public(&["**"], &[], &[]);
	let mut lite07 = dialing();
	lite07.version = vec![LITE_07.parse().unwrap()];

	// ── TOP: origin server serving the same broadcast to both mids ──────
	let top_origin = memory::origin();
	let broadcast = top_origin
		.route("diamond", moq_net::origin::RouteOptions::default())
		.expect("create broadcast");
	let track = broadcast
		.broadcast()
		.create_track("video", timed())
		.expect("create track");
	let mut top_accepted = net.upstream("top", &top_origin);

	// ── MID-B: full relay clustered to TOP, up for the whole test ───────
	let mid_b = clustered(&net, &["top"], lite07.clone()).await;
	let mut mid_b_accepted = net.relay("mid-b", &mid_b, &auth, &shutdown::Observer::disabled());
	let _mid_b_session = accept("TOP accepts MID-B", &mut top_accepted).await;

	// ── MID-A: mini-relay consuming TOP, serving BOTTOM, drains later ───
	let mid_a_origin = memory::origin();
	// An upstream link: MID-A relays for BOTTOM, so it asks TOP for everything.
	let mid_a_upstream = within(
		"MID-A connects to TOP",
		connect_once(
			net.client(lite07.clone())
				.with_subscriber(mid_a_origin.clone().with_link(moq_net::origin::Link::Upstream)),
			net.url("top"),
		),
	)
	.await
	.expect("mid-a upstream connect");
	let _top_session_a = accept("TOP accepts MID-A", &mut top_accepted).await;
	let mut mid_a_accepted = net.upstream("mid-a", &mid_a_origin);

	// ── BOTTOM: full relay clustered to MID-A ───────────────────────────
	let bottom = clustered(&net, &["mid-a"], lite07).await;
	let _bottom_accepted = net.relay("bottom", &bottom, &auth, &shutdown::Observer::disabled());
	// MID-A's first inbound connection is BOTTOM's cluster dial: hold its
	// server-side session so we can drain it.
	let session_bottom_on_a = accept("MID-A accepts BOTTOM", &mut mid_a_accepted).await;

	// ── SUBSCRIBER: connects to BOTTOM ───────────────────────────────────
	let sub_origin = memory::origin();
	// Watch announcements for the whole test: the failover must never
	// unannounce the broadcast under the subscriber.
	let recorded = memory::record(sub_origin.consume().routes());
	let sub_connection = within(
		"subscriber connects to BOTTOM",
		connect_once(
			net.client(Default::default()).with_subscriber(sub_origin.clone()),
			net.url("bottom"),
		),
	)
	.await
	.expect("subscriber connect");

	let bc = within(
		"broadcast resolves on the subscriber origin",
		sub_origin.consume().follow("diamond", None).unwrap().available(),
	)
	.await
	.expect("broadcast announced")
	.expect("the first change is an instance");
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
		bc.track("video").expect("track handle").subscribe(
			moq_net::track::Subscription::default()
				.with_delay(moq_net::track::Delay::default().with_max(Duration::from_secs(60))),
		),
	)
	.await
	.expect("subscribe");

	// ── group 0 flows through the MID-A leg (all frames verified) ──
	const FRAMES_PER_GROUP: u64 = 3;
	let mut g = track
		.append_group(track.largest().map_or(0, |largest| largest.group + 1))
		.expect("append group");
	for f in 0..FRAMES_PER_GROUP {
		let payload = format!("diamond_g0_f{f}");
		g.write_frame(moq_net::Timestamp::from_millis(g.start).unwrap(), payload.as_bytes())
			.expect("write frame");
	}
	g.close().expect("finish");

	let mut seen = BTreeSet::new();
	collect_group(&mut sub, &mut seen, FRAMES_PER_GROUP, "pre-failover (via MID-A)").await;
	assert_eq!(seen, BTreeSet::from([0]), "group 0 must arrive before the failover");

	// ── continuous publishing THROUGH the failover window ────────────────
	// Groups 1..=LAST_GROUP stream at a steady cadence, with multiple frames
	// per group, while the GOAWAY, reconnect, and handover happen mid-stream.
	const LAST_GROUP: u64 = 20;
	let publisher = kio::clock::sim::spawn(async move {
		for seq in 1..=LAST_GROUP {
			let mut g = track
				.append_group(track.largest().map_or(0, |largest| largest.group + 1))
				.expect("append group");
			for f in 0..FRAMES_PER_GROUP {
				let payload = format!("diamond_g{seq}_f{f}");
				g.write_frame(moq_net::Timestamp::from_millis(g.start).unwrap(), payload.as_bytes())
					.expect("write frame");
			}
			g.close().expect("finish");
			kio::clock::sleep(Duration::from_millis(50)).await;
		}
		// Hand the track back so the post-drain phase can keep publishing.
		track
	});

	// ── TRIGGER: MID-A drains BOTTOM with a redirect to MID-B ───────────
	session_bottom_on_a
		.drain()
		.send(moq_net::goaway::Goaway::redirect(net.url("mid-b")).with_timeout(Duration::from_secs(5)))
		.expect("send goaway");

	// Positive gate: MID-B's first inbound connection can only be BOTTOM's
	// post-GOAWAY reconnect (the subscriber talks to BOTTOM, and MID-B's link
	// to TOP is outbound). Without this, the test could pass with a broken
	// reconnect because groups can still flow through the draining old leg.
	within("BOTTOM reconnects to MID-B", mid_b_accepted.next())
		.await
		.expect("MID-B stopped accepting");

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

	let track = within("publisher task finishes", publisher)
		.await
		.expect("publisher task");

	// ── the old MID-A leg drains away, then is severed entirely ─────────
	within("old session drains after the swap", session_bottom_on_a.closed()).await;
	// Cut MID-A off from TOP so it can never receive (let alone forward) new
	// groups. Anything delivered from here on MUST have flowed TOP -> MID-B ->
	// BOTTOM, positively proving the new leg carries the resubscribe.
	drop(mid_a_upstream);

	// The subscription was served through the MID-A leg, but its close does not
	// end it: both legs' routes carry TOP's instance, so BOTTOM's front re-splices
	// onto the MID-B leg and the subscription rides through. Keep publishing and keep reading the SAME subscription.
	const POST_DRAIN_LAST: u64 = LAST_GROUP + 40;
	let post_publisher = kio::clock::sim::spawn(async move {
		for seq in (LAST_GROUP + 1)..=POST_DRAIN_LAST {
			let mut g = track
				.append_group(track.largest().map_or(0, |largest| largest.group + 1))
				.expect("append group");
			for f in 0..FRAMES_PER_GROUP {
				let payload = format!("diamond_g{seq}_f{f}");
				g.write_frame(moq_net::Timestamp::from_millis(g.start).unwrap(), payload.as_bytes())
					.expect("write frame");
			}
			g.close().expect("finish");
			kio::clock::sleep(Duration::from_millis(50)).await;
		}
	});

	// Three distinct post-drain groups on the same subscription, every frame
	// verified, proves the new leg carries it: MID-A is severed, so they can
	// only have flowed TOP -> MID-B -> BOTTOM.
	let mut post = BTreeSet::new();
	while post.iter().filter(|seq| **seq > LAST_GROUP).count() < 3 {
		collect_group(&mut sub, &mut post, FRAMES_PER_GROUP, "post-drain (MID-B leg only)").await;
	}
	post_publisher.abort();
	// The splice must not re-deliver anything the old leg already served.
	assert!(
		post.iter().all(|seq| *seq > LAST_GROUP),
		"the splice re-delivered pre-failover groups: {post:?}"
	);

	// Once the mesh settles, nothing is left in flight that could still reach the
	// subscriber.
	kio::clock::sim::idle().await;

	// ── no GOAWAY cascade to the downstream subscriber ───────────────────
	assert!(
		sub_connection
			.draining()
			.expect("connected")
			.poll(&kio::Waiter::noop())
			.is_pending(),
		"BOTTOM must not propagate the upstream GOAWAY downstream"
	);

	// ── announcement stability: the path never retracted under the swap ──
	// Metadata updates (the new leg winning) are expected.
	let retracted = retractions(&recorded);
	assert!(
		retracted.is_empty(),
		"failover must not retract the path under the subscriber: {retracted:?}"
	);
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
	let expected_seq = group.start;
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

/// Dial `url` once with `client`, reconnecting off: these tests want a single
/// session, so there is nothing to redial.
async fn connect_once(client: moq_tokio::Client, url: String) -> moq_tokio::Result<moq_tokio::Connection> {
	client
		.with_reconnect(false)
		.connect(url::Url::parse(&url).unwrap())
		.established()
		.await
}

#[test]
fn goaway_handover_is_enforced_while_the_replacement_dial_hangs() {
	memory::run(goaway_handover_is_enforced_while_the_replacement_dial_hangs_inner);
}

/// The handover deadline has to hold across the replacement dial, not just across
/// the sleeps around it.
///
/// A GOAWAY off a healthy session goes straight into the replacement dial with the
/// old one still draining. Redirect that dial at a host that never answers, so the
/// handshake hangs, and the predecessor must still be
/// closed on schedule rather than held for as long as the dial takes.
async fn goaway_handover_is_enforced_while_the_replacement_dial_hangs_inner() {
	let net = Net::new();
	let mut accepted = net.upstream("upstream", &memory::origin());
	// Never answers: the dial hangs in the transport handshake.
	let mut black_hole = net.black_hole("black-hole");

	let handover = Duration::from_millis(200);
	let mut config = moq_tokio::connect::Config::default();
	config.goaway.handover = Some(handover.into());
	// The GOAWAY has to land on a *healthy* session, which is the path that goes
	// straight into the replacement dial. Below this bar it takes the immediate
	// redirect path instead, whose sleep polls the drain either way.
	let healthy = Duration::from_millis(100);
	config.backoff.healthy = Some(healthy.into());
	// Past the handover, so only the handover can end the old session.
	assert!(config.resolve().timeout.is_none_or(|timeout| timeout > handover));

	let connection = within(
		"client connects",
		net.client(config)
			.connect(url::Url::parse(&net.url("upstream")).unwrap())
			.established(),
	)
	.await
	.expect("connect");
	let server_session = accept("upstream accepts", &mut accepted).await;

	// Clear the healthy bar, so the GOAWAY continues straight into the replacement
	// dial rather than sleeping first.
	kio::clock::sleep(healthy).await;

	// Send the client somewhere that will never finish handshaking. No wire timeout:
	// our own `--connect-goaway-handover` is then the only thing bounding the old session.
	server_session
		.drain()
		.send(moq_net::goaway::Goaway::redirect(net.url("black-hole")))
		.expect("send goaway");
	let sent = kio::clock::now();

	// The handover deadline is set before the replacement dial starts, so once the
	// dial reaches the black hole, passing the handover must close the predecessor
	// while the dial still hangs.
	let _held = within("the replacement dial reaches the black hole", black_hole.next())
		.await
		.expect("the black hole stopped accepting");
	within("the old session closes", server_session.closed()).await;
	// The handover counts from the GOAWAY's arrival, one link after it was sent; the
	// close itself skips the latency.
	let bound = handover + memory::LATENCY;
	assert!(
		sent.elapsed() <= bound,
		"the old session outlived its {handover:?} handover by {:?}: the replacement dial was never interrupted to enforce it",
		sent.elapsed() - handover
	);

	drop(connection);
}
