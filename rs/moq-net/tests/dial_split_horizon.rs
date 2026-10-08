//! A dialed moq-transport session whose peer declares no hop must not be offered
//! a route learned from that peer, and a SUBSCRIBE that arrives for that route
//! must not be sent back to the peer that announced it.
//!
//! Draft 16 has no cluster extension, so neither side declares a hop. That is the
//! anonymous dial: the accepting side already stamps one, and the dialing side
//! has to as well.

mod support;

use std::time::Duration;

use moq_net::{Client, Hop, Server, Session, Version, origin, stats};
use support::{
	harness::{now, spawn},
	mock::create_mock_session_pair,
};

/// Long enough, in virtual time, for an announce or subscribe to cross the mock link.
const SETTLE: Duration = Duration::from_secs(1);

fn version() -> Version {
	"moq-transport-16".parse().unwrap()
}

fn produce(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	spawn(driver);
	producer
}

/// One dial from `dialer` to `edge`, held open for the test.
struct Link {
	_client: Session,
	_server: Session,
	/// Stats on the accepted session, so an announce or subscribe that arrives
	/// there is visible without reading the wire.
	registry: stats::Registry,
}

async fn dial(dialer: &origin::Producer, edge: &origin::Producer) -> Link {
	let version = version();
	let registry = stats::Registry::new(stats::Config::new());
	let session_stats = registry.tier(stats::Tier::default()).session("");
	let (client_transport, server_transport) = create_mock_session_pair(Some(version.alpn()));

	// Cluster dials publish and subscribe through a peer handle, which is what
	// records the announcing session on the route.
	let dialer_origin = dialer.clone().peer();
	let edge_origin = edge.clone().peer();
	let client = Client::new()
		.with_versions(version.into())
		.with_publisher(dialer_origin.consume().with_hidden(true))
		.with_subscriber(dialer_origin);
	let server = Server::new()
		.with_versions(version.into())
		.with_stats(session_stats)
		.with_publisher(edge_origin.consume().with_hidden(true))
		.with_subscriber(edge_origin);

	let client = async {
		let (session, driver) = client
			.connect(now(), client_transport)
			.await
			.expect("client handshake failed");
		spawn(driver);
		session
	};
	let server = async {
		let (session, driver) = server
			.accept(now(), server_transport)
			.await
			.expect("server handshake failed");
		spawn(driver);
		session
	};
	let (client, server) = futures::join!(client, server);
	Link {
		_client: client,
		_server: server,
		registry,
	}
}

fn traffic(registry: &stats::Registry, role: stats::Role) -> stats::Traffic {
	registry
		.snapshot()
		.traffic()
		.into_iter()
		.find(|(_, row, _)| *row == role)
		.map(|(_, _, traffic)| traffic)
		.unwrap_or_default()
}

/// The hop the dialer attributed `path` to, once that announce has arrived.
async fn learned_hop(dialer: &origin::Producer, path: &str) -> Hop {
	let mut announced = dialer.consume().announced();
	let event = moq_net_sim::timeout(SETTLE, async {
		loop {
			let event = announced.next().await.expect("announce cursor closed");
			let announce = match event {
				moq_net::announce::Event::Start(announce) | moq_net::announce::Event::Update(announce) => announce,
				moq_net::announce::Event::End(_) => continue,
			};
			if announce.prefix.as_str() == path {
				return announce;
			}
		}
	})
	.await
	.unwrap_or_else(|_| panic!("{path}: the dialer never learned the namespace"));

	match event.route.source() {
		origin::Source::Peer(hop) => hop,
		origin::Source::Local => panic!("{path}: a route learned from the peer looks local"),
	}
}

/// Draft-16 edges dialed by one origin each get their own hop. The edge that
/// announced a namespace is not told about it, and the other edge is. A
/// subscribe from that other edge is fetched from the announcer and is not
/// sent back to the subscriber.
#[moq_net_sim::test]
async fn dialed_anonymous_session_does_not_echo_or_route_subscribe_back() {
	let dialer = produce(1);
	let publisher = produce(2);
	let subscriber = produce(3);
	let to_publisher = dial(&dialer, &publisher).await;
	let to_subscriber = dial(&dialer, &subscriber).await;

	let broadcast = publisher.create_broadcast("room").unwrap();
	let track = broadcast.create_track("video", None).unwrap();
	broadcast.announce(origin::Route::default()).unwrap();

	let publisher_hop = learned_hop(&dialer, "room").await;
	assert_ne!(
		publisher_hop,
		Hop::UNKNOWN,
		"an anonymous dial still needs a hop, or its routes echo"
	);

	// The other edge learns the namespace, attributed to a different hop, so the
	// two dials are not one endpoint.
	let mut subscriber_announced = subscriber.consume().announced();
	let subscriber_saw = moq_net_sim::timeout(SETTLE, subscriber_announced.next())
		.await
		.expect("the other edge never learned the namespace")
		.expect("announce cursor closed");
	let moq_net::announce::Event::Start(subscriber_saw) = subscriber_saw else {
		panic!("the other edge's first event was not the namespace: {subscriber_saw:?}");
	};
	assert_eq!(subscriber_saw.prefix.as_str(), "room");
	let subscriber_hop = match subscriber_saw.route.source() {
		origin::Source::Peer(hop) => hop,
		origin::Source::Local => panic!("the forwarded route looks local"),
	};
	assert_ne!(subscriber_hop, publisher_hop, "each dial gets its own hop");
	assert_ne!(subscriber_hop, Hop::UNKNOWN);

	moq_net_sim::sleep(SETTLE).await;

	let echoed = traffic(&to_publisher.registry, stats::Role::Subscriber);
	assert_eq!(
		echoed.announces_started, 0,
		"the announcing edge was offered its own namespace back"
	);
	let forwarded = traffic(&to_subscriber.registry, stats::Role::Subscriber);
	assert!(
		forwarded.announces_started >= 1,
		"the other edge was not offered the namespace"
	);

	// The subscribe arrives on the dialer's session with the other edge. It has
	// to be fetched from the announcer, and not turned into a subscribe on the
	// session it arrived on.
	let viewed = subscriber
		.consume()
		.request_broadcast("room", None)
		.await
		.expect("subscriber resolves the namespace");
	let subscription = moq_net::track::Subscription::default();
	let _reader = viewed
		.track("video")
		.unwrap()
		.subscribe(subscription)
		.await
		.expect("subscriber subscribes");
	moq_net_sim::timeout(SETTLE, track.demand().used())
		.await
		.expect("the subscribe never reached the announcer")
		.unwrap();

	moq_net_sim::sleep(SETTLE).await;

	let from_announcer = traffic(&to_publisher.registry, stats::Role::Publisher);
	assert_eq!(
		from_announcer.subscriptions_started, 1,
		"the subscribe was not routed to the edge that announced it"
	);
	let back_to_subscriber = traffic(&to_subscriber.registry, stats::Role::Publisher);
	assert_eq!(
		back_to_subscriber.subscriptions_started, 0,
		"the subscribe was routed back to the edge that sent it"
	);
}
