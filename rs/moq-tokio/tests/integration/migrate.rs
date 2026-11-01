//! A GOAWAY migration closes the old session once nothing flows on it, rather than holding
//! it to the handover cap.

#![cfg(feature = "tcp")]

use std::time::Duration;

use moq_tokio::moq_net;

/// The only version whose routes carry the epoch that lets a subscription resume across
/// the move.
const LITE_07: &str = "moq-lite-07-wip";

/// The client's handover cap, far past [`TIMEOUT`]: the GOAWAY names no deadline, so only
/// the early close can end the old session within a test.
const HANDOVER: Duration = Duration::from_secs(3600);

const TIMEOUT: Duration = Duration::from_secs(10);

/// Groups each viewer reads between two steps.
const GROUPS: usize = 10;

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

/// A relay: one origin every accepted session publishes from and subscribes into,
/// speaking `versions`, or the defaults when empty. Returns the port it listens on and
/// each session it accepts.
async fn relay(
	origin: &moq_net::origin::Producer,
	versions: Vec<moq_net::Version>,
) -> (u16, tokio::sync::mpsc::UnboundedReceiver<moq_net::Session>) {
	let mut listen = moq_tokio::listen::Config::default();
	listen.tcp.bind = Some("127.0.0.1:0".parse().unwrap());
	if !versions.is_empty() {
		listen.version = versions;
	}
	let mut server = listen
		.init(Default::default())
		.expect("init server")
		.listen()
		.await
		.expect("bind tcp listener");
	let port = server.tcp_local_addr().expect("tcp listener bound").port();
	let (accepted, sessions) = tokio::sync::mpsc::unbounded_channel();
	let origin = origin.clone();
	tokio::spawn(async move {
		while let Some(request) = server.accept().await {
			let request = request.with_publisher(&origin).with_subscriber(origin.clone());
			if let Ok(session) = request.ok().await {
				let _ = accepted.send(session);
			}
		}
	});
	(port, sessions)
}

/// A client sharing `origin` with the relay on `port`, speaking `versions`, or the
/// defaults when empty.
fn connect(origin: &moq_net::origin::Producer, port: u16, versions: Vec<moq_net::Version>) -> moq_tokio::Connection {
	let mut config = moq_tokio::connect::Config::default();
	if !versions.is_empty() {
		config.version = versions;
	}
	config.goaway.handover = Some(HANDOVER.into());
	// Healthy well before the GOAWAY, so the replacement dials without a backoff.
	config.backoff.healthy = Some(Duration::from_millis(100).into());
	config
		.init(Default::default())
		.expect("init client")
		.with_origin(origin.clone())
		.connect(format!("tcp://127.0.0.1:{port}/").parse::<url::Url>().unwrap())
}

/// Announce `name` on `origin` as `route`, and publish a group carrying its own sequence
/// number every 20ms for the rest of the test.
fn publish(origin: &moq_net::origin::Producer, name: &str, route: moq_net::origin::Route) {
	let broadcast = origin.create_broadcast(name).expect("create broadcast");
	broadcast.announce(route).expect("announce");
	let track = broadcast.create_track("video", timed()).expect("create track");
	tokio::spawn(async move {
		let _broadcast = broadcast;
		loop {
			let mut group = track.append_group().expect("append group");
			let payload = group.sequence.to_string().into_bytes();
			group
				.write_frame(moq_net::Timestamp::ZERO, payload)
				.expect("write frame");
			group.finish().expect("finish group");
			kio::clock::sleep(Duration::from_millis(20)).await;
		}
	});
}

/// Read `name`'s track through `origin`, sending each group's sequence once its payload
/// checks out. Dropping the receiver ends the subscription.
fn view(origin: &moq_net::origin::Producer, name: &str) -> tokio::sync::mpsc::UnboundedReceiver<u64> {
	let (seen, sequences) = tokio::sync::mpsc::unbounded_channel();
	let consumer = origin.consume();
	let name = name.to_string();
	tokio::spawn(async move {
		let broadcast = consumer
			.follow(name.as_str(), None)
			.expect("follow")
			.available()
			.await
			.expect("the broadcast resolves");
		// A budget, so a group the move delays is late rather than skipped.
		let subscription = moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(5));
		let mut track = broadcast
			.track("video")
			.expect("track")
			.subscribe(subscription)
			.await
			.expect("subscribe");
		while let Some(mut group) = track.recv_group().await.expect("the subscription failed") {
			let frame = group.read_frame().await.expect("read frame").expect("an empty group");
			assert_eq!(frame.payload[..], group.sequence.to_string().into_bytes()[..]);
			if seen.send(group.sequence).is_err() {
				return;
			}
		}
		panic!("the subscription ended");
	});
	sequences
}

/// Receive `count` sequences, after any already queued.
async fn read(sequences: &mut tokio::sync::mpsc::UnboundedReceiver<u64>, count: usize, into: &mut Vec<u64>) {
	for _ in 0..count {
		let sequence = kio::clock::timeout(TIMEOUT, sequences.recv())
			.await
			.expect("no group arrived")
			.expect("the viewer stopped");
		into.push(sequence);
	}
}

/// Every group from the first read through the last arrived exactly once.
fn assert_contiguous(who: &str, sequences: &[u64]) {
	for pair in sequences.windows(2) {
		assert_eq!(pair[1], pair[0] + 1, "{who} skipped or repeated a group: {sequences:?}");
	}
}

/// Wait for `old` to close, and check it closed cleanly rather than at a cap.
async fn assert_closes(old: &moq_net::Session) {
	let closed = kio::clock::timeout(TIMEOUT, old.closed())
		.await
		.expect("the old session waited for the handover cap");
	assert!(
		matches!(closed, moq_net::Error::Cancel),
		"the old session ended with {closed} rather than closing cleanly"
	);
}

/// A client publishing one track and reading another through a relay migrates on the
/// relay's GOAWAY. With an epoch, both subscriptions resume on the replacement at a group
/// boundary, which leaves the old session carrying nothing, so it closes long before the
/// handover cap.
#[tokio::test]
async fn a_migrated_session_closes_once_nothing_flows() {
	let versions = vec![LITE_07.parse().unwrap()];
	let origin = moq_tokio::origin::spawn();
	let (port, mut sessions) = relay(&origin, versions.clone()).await;
	let local = moq_tokio::origin::spawn();
	let _connection = connect(&local, port, versions);
	let old = kio::clock::timeout(TIMEOUT, sessions.recv())
		.await
		.expect("the client never connected")
		.expect("the relay stopped");

	let epoch = || moq_net::origin::Route::default().with_epoch(moq_net::Epoch::mint());
	publish(&local, "up", epoch());
	publish(&origin, "down", epoch());
	let mut upstream = view(&origin, "up");
	let mut downstream = view(&local, "down");
	let (mut up, mut down) = (Vec::new(), Vec::new());
	read(&mut upstream, GROUPS, &mut up).await;
	read(&mut downstream, GROUPS, &mut down).await;

	old.drain().send(moq_net::goaway::Goaway::new()).expect("send goaway");
	// Held, since dropping the last handle closes the replacement.
	let _replacement = kio::clock::timeout(TIMEOUT, sessions.recv())
		.await
		.expect("the client never redialed")
		.expect("the relay stopped");
	assert_closes(&old).await;

	// Delivery carries on through the replacement without a hole or a repeat.
	read(&mut upstream, GROUPS, &mut up).await;
	read(&mut downstream, GROUPS, &mut down).await;
	assert_contiguous("the relay", &up);
	assert_contiguous("the client", &down);
	assert!(sessions.try_recv().is_err(), "the client dialed past its replacement");
}

/// Without an epoch, as with default clients, a subscription stays on the instance it
/// started on, so the old session keeps serving it after the replacement is up. It closes
/// once the application lets that subscription go, not at the handover cap.
#[tokio::test]
async fn a_predecessor_serves_until_its_last_subscription_leaves() {
	let origin = moq_tokio::origin::spawn();
	let (port, mut sessions) = relay(&origin, Vec::new()).await;
	let local = moq_tokio::origin::spawn();
	let _connection = connect(&local, port, Vec::new());
	let old = kio::clock::timeout(TIMEOUT, sessions.recv())
		.await
		.expect("the client never connected")
		.expect("the relay stopped");

	publish(&origin, "down", moq_net::origin::Route::default());
	let mut downstream = view(&local, "down");
	let mut down = Vec::new();
	read(&mut downstream, GROUPS, &mut down).await;

	old.drain().send(moq_net::goaway::Goaway::new()).expect("send goaway");
	// Held, since dropping the last handle closes the replacement.
	let _replacement = kio::clock::timeout(TIMEOUT, sessions.recv())
		.await
		.expect("the client never redialed")
		.expect("the relay stopped");

	// These groups can only come through the old session, which the subscription stays on.
	read(&mut downstream, GROUPS, &mut down).await;
	assert_contiguous("the client", &down);
	assert!(
		futures::FutureExt::now_or_never(old.closed()).is_none(),
		"the old session closed while it still served a subscription"
	);

	drop(downstream);
	assert_closes(&old).await;
	assert!(sessions.try_recv().is_err(), "the client dialed past its replacement");
}
