//! A relay [`Connection`](moq_relay::Connection) over the in-memory transport: admission,
//! the cluster origin, and stats, with no socket and on Tokio's paused clock.

use std::time::Duration;

use moq_net::stats;

use crate::memory::{self, connect, newest};

/// Long enough that only a hang reaches it; paused time skips straight there.
const TIMEOUT: Duration = Duration::from_secs(60);

/// The relay preserves every legal QUIC-varint sequence, including values beyond
/// JavaScript's exact number range, on each flavour used by the browser bindings.
#[tokio::test(start_paused = true)]
async fn largest_quic_sequence_reaches_the_subscriber() {
	for alpn in ["moq-lite-02", "moq-lite-06", "moq-transport-22"] {
		let version = alpn.parse().unwrap();
		let auth = memory::public(&["**"], &[], &[]);
		let cluster = memory::cluster();
		let publisher = moq_tokio::origin::spawn();
		let broadcast = publisher.create_broadcast("ladder").unwrap();
		let info = moq_net::track::Info::default().with_timescale(moq_net::Timescale::MICRO);
		let track = broadcast.create_track("data", info).unwrap();
		broadcast.announce(Default::default()).unwrap();
		let _publisher = connect(
			moq_net::Client::new().with_publisher(publisher.consume()),
			version,
			&cluster,
			&auth,
		)
		.await
		.unwrap();
		let subscriber = moq_tokio::origin::spawn();
		let _subscriber = connect(
			moq_net::Client::new().with_subscriber(subscriber.clone()),
			version,
			&cluster,
			&auth,
		)
		.await
		.unwrap();
		let remote = subscriber
			.consume()
			.follow("ladder", None)
			.unwrap()
			.available()
			.await
			.unwrap();
		let mut sub = remote.track("data").unwrap().subscribe(None).await.unwrap();
		for sequence in [5, 1 << 32, (1 << 53) - 1, (1 << 53) + 1, (1 << 62) - 1] {
			// Timestamps stop at 2^61 - 1, the most lite-05/06 can zigzag as a first frame.
			let timestamp = moq_net::Timestamp::from_micros(sequence.min((1 << 61) - 1)).unwrap();
			let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
			group.write_frame(timestamp, b"ladder".as_ref()).unwrap();
			group.finish().unwrap();
			let mut got = moq_clock::timeout(TIMEOUT, sub.recv_group())
				.await
				.unwrap_or_else(|_| panic!("{alpn} sequence={sequence} hung"))
				.unwrap()
				.unwrap();
			assert_eq!(got.sequence, sequence, "{alpn}");
			let frame = got.read_frame().await.unwrap().unwrap();
			assert_eq!(frame.payload, b"ladder".as_ref());
			// Lite-02 carries no timestamps, and a lite-06 subscriber drops them.
			let timed = alpn == "moq-transport-22";
			assert_eq!(frame.timestamp, timed.then_some(timestamp), "{alpn}");
		}
	}
}

/// A publisher of one broadcast holding one frame, kept alive by the returned handles.
fn publish(
	name: &str,
) -> (
	moq_net::origin::Producer,
	moq_net::origin::Publication,
	moq_net::track::Producer,
) {
	let origin = moq_tokio::origin::spawn();
	let broadcast = origin.create_broadcast(name).expect("create broadcast");
	broadcast.announce(Default::default()).expect("announce");
	let info = moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI);
	let track = broadcast.create_track("video", info).expect("create track");
	let mut group = track.append_group().expect("append group");
	group
		.write_frame(moq_net::Timestamp::ZERO, name.as_bytes())
		.expect("write frame");
	group.finish().expect("finish group");
	(origin, broadcast, track)
}

/// The next active route's prefix.
async fn next_active(announced: &mut moq_net::announce::Consumer) -> String {
	loop {
		match moq_clock::timeout(TIMEOUT, announced.next())
			.await
			.expect("announcement timeout")
			.expect("origin closed")
		{
			moq_net::announce::Event::Start(route)
			| moq_net::announce::Event::Update(route)
			| moq_net::announce::Event::Restart(route)
			| moq_net::announce::Event::Takeover { announce: route, .. } => return route.prefix.to_string(),
			moq_net::announce::Event::End(_) => {}
		}
	}
}

/// A session the relay refuses closes with the refusal, whether or not its SETUP
/// was answered first.
async fn assert_refused(result: Result<moq_net::Session, moq_net::Error>) {
	let err = match result {
		Ok(session) => moq_clock::timeout(TIMEOUT, session.closed())
			.await
			.expect("the relay must close a refused session, not leave it open"),
		Err(err) => err,
	};
	assert_eq!(moq_net::SessionError::from(&err), moq_net::SessionError::Unauthorized);
}

/// A publisher whose grant covers only subscribing is refused during the handshake,
/// instead of accepted to stream into a dropped session forever. Its SETUP's role
/// says it publishes.
#[tokio::test(start_paused = true)]
async fn subscribe_only_public_rejects_publisher_role() {
	let auth = memory::public(&[], &[], &["**"]);
	let (origin, _broadcast, _track) = publish("cam");
	let client = moq_net::Client::new().with_publisher(origin.consume());
	assert_refused(connect(client, newest(), &memory::cluster(), &auth).await).await;
}

/// The same grant keeps a subscriber: the role gate refuses only the mismatched direction.
#[tokio::test(start_paused = true)]
async fn subscribe_only_public_accepts_subscriber_role() {
	let auth = memory::public(&[], &[], &["**"]);
	let client = moq_net::Client::new().with_subscriber(moq_tokio::origin::spawn());
	let session = connect(client, newest(), &memory::cluster(), &auth)
		.await
		.expect("subscriber connects");
	assert!(
		moq_clock::timeout(TIMEOUT, session.closed()).await.is_err(),
		"subscribe-only relay should keep a subscriber session open"
	);
}

/// The mirror: a subscriber whose grant covers only publishing is refused.
#[tokio::test(start_paused = true)]
async fn publish_only_public_rejects_subscriber_role() {
	let auth = memory::public(&[], &["**"], &[]);
	let client = moq_net::Client::new().with_subscriber(moq_tokio::origin::spawn());
	assert_refused(connect(client, newest(), &memory::cluster(), &auth).await).await;
}

/// Two publish-only clients share one relay, and a subscriber sees both broadcasts:
/// neither tears the other down.
#[tokio::test(start_paused = true)]
async fn two_publish_only_clients_coexist() {
	let auth = memory::public(&["**"], &[], &[]);
	let cluster = memory::cluster();

	let (alpha, _alpha_broadcast, _alpha_track) = publish("alpha");
	let (beta, _beta_broadcast, _beta_track) = publish("beta");
	let _alpha = connect(
		moq_net::Client::new().with_publisher(alpha.consume()),
		newest(),
		&cluster,
		&auth,
	)
	.await
	.expect("alpha connects");
	let _beta = connect(
		moq_net::Client::new().with_publisher(beta.consume()),
		newest(),
		&cluster,
		&auth,
	)
	.await
	.expect("beta connects");

	let origin = moq_tokio::origin::spawn();
	let mut announced = origin.consume().announced();
	let _subscriber = connect(
		moq_net::Client::new().with_subscriber(origin),
		newest(),
		&cluster,
		&auth,
	)
	.await
	.expect("subscriber connects");

	let mut seen = [next_active(&mut announced).await, next_active(&mut announced).await];
	seen.sort();
	assert_eq!(seen, ["alpha", "beta"]);
}

/// The decider is asked about the path, query, and role the SETUP carried, and the
/// grant it answers is rooted at that path: a publisher's broadcast reaches a root
/// subscriber under it, frames and all.
#[tokio::test(start_paused = true)]
async fn the_setup_path_reaches_the_decider_and_roots_the_grant() {
	let (auth, mut admissions) = moq_relay::auth::Auth::embedded("test");
	let decider = tokio::spawn(async move {
		let mut seen = Vec::new();
		while let Some(admission) = admissions.next().await {
			seen.push(admission.request.clone());
			let all: moq_auth::Patterns = [moq_auth::Pattern::all()].into_iter().collect();
			admission.grant(moq_auth::lease::Consumer::fixed(moq_auth::Grant::new(all.clone(), all)));
			if seen.len() == 2 {
				return seen;
			}
		}
		seen
	});
	let cluster = memory::cluster();

	let (origin, _broadcast, _track) = publish("cam");
	let publisher = moq_net::Client::new()
		.with_publisher(origin.consume())
		.with_url("moql://relay.example.com/room?jwt=token".parse().unwrap());
	let _publisher = connect(publisher, newest(), &cluster, &auth)
		.await
		.expect("publisher connects");

	let origin = moq_tokio::origin::spawn();
	let consumer = origin.consume();
	let mut announced = consumer.announced();
	let _subscriber = connect(
		moq_net::Client::new().with_subscriber(origin),
		newest(),
		&cluster,
		&auth,
	)
	.await
	.expect("subscriber connects");
	assert_eq!(next_active(&mut announced).await, "room/cam");

	let broadcast = consumer
		.request("room/cam", None)
		.await
		.expect("announced broadcast resolves");
	let mut track = broadcast
		.track("video")
		.expect("track")
		.subscribe(None)
		.await
		.expect("subscribe");
	let mut group = moq_clock::timeout(TIMEOUT, track.recv_group())
		.await
		.expect("group timeout")
		.expect("recv group")
		.expect("track open");
	let frame = group.read_frame().await.expect("read frame").expect("a frame");
	assert_eq!(&frame.payload[..], b"cam");

	let seen = decider.await.expect("decider");
	let publisher = &seen[0];
	assert_eq!(publisher.transport, moq_auth::Transport::Quic);
	assert_eq!(publisher.path, "/room");
	assert_eq!(publisher.query.as_deref(), Some("jwt=token"));
	assert_eq!(publisher.role, Some(moq_auth::Role::Publisher));
	let subscriber = &seen[1];
	assert_eq!(subscriber.path, "/");
	assert_eq!(subscriber.query, None);
	assert_eq!(subscriber.role, Some(moq_auth::Role::Subscriber));
}

/// The relay's stats count an admitted session while it is open and a refused one
/// by its reason, under the default tier.
#[tokio::test(start_paused = true)]
async fn stats_count_admitted_and_refused_sessions() {
	let cluster = memory::cluster();
	let publisher = moq_stats::Producer::new(moq_stats::produce::Config::new().with_origin(cluster.origin.clone()));
	let registry = publisher.registry().clone();
	let cluster = cluster.with_stats(publisher);
	let presence = || {
		registry
			.snapshot()
			.sessions()
			.into_iter()
			.find(|(tier, _)| *tier == stats::Tier::default())
			.map_or(stats::Presence::default(), |(_, presence)| presence)
	};
	let auth = memory::public(&[], &[], &["**"]);

	let client = moq_net::Client::new().with_subscriber(moq_tokio::origin::spawn());
	let session = connect(client, newest(), &cluster, &auth)
		.await
		.expect("subscriber connects");
	moq_clock::timeout(TIMEOUT, async {
		while presence().active() != 1 {
			tokio::task::yield_now().await;
		}
	})
	.await
	.expect("the admitted session is counted");

	let (origin, _broadcast, _track) = publish("cam");
	let client = moq_net::Client::new().with_publisher(origin.consume());
	assert_refused(connect(client, newest(), &cluster, &auth).await).await;
	assert_eq!(presence().sessions_refused.forbidden, 1);

	drop(session);
	moq_clock::timeout(TIMEOUT, async {
		while presence().active() != 0 {
			tokio::task::yield_now().await;
		}
	})
	.await
	.expect("the closed session is no longer counted");
	let presence = presence();
	assert_eq!(presence.sessions_started, 1);
	assert_eq!(presence.sessions_ended, 1);
}
