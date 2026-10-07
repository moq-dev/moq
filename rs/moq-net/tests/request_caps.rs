//! A server's [`moq_net::session::Limits`] bound what one client can make it hold, without
//! ending the session: subscriptions past the cap are refused one at a time, and on
//! moq-transport drafts 14 to 16 the MAX_REQUEST_ID window they size is granted back as
//! requests close, so a client that keeps opening and closing requests never stalls.

mod support;

use std::time::Duration;

use moq_net::{Hop, Version, session::Limits};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const TIMEOUT: Duration = Duration::from_secs(10);

/// Drafts with MAX_REQUEST_ID, a draft without it, and the lite draft production runs.
const VERSIONS: [&str; 5] = [
	"moq-transport-14",
	"moq-transport-15",
	"moq-transport-16",
	"moq-transport-17",
	"moq-lite-06",
];

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// A server publishing `bcast` with `tracks`, and a client subscribed to its origin.
struct Setup {
	client: moq_net::origin::Producer,
	_server: moq_net::origin::Producer,
	_broadcast: moq_net::broadcast::Producer,
	tracks: Vec<moq_net::track::Producer>,
	_pair: MockPair,
}

async fn setup(version: &str, limits: Limits, tracks: usize) -> Setup {
	let version: Version = version.parse().unwrap();
	let server = produce_origin(1);
	let client = produce_origin(2);

	let broadcast = server.create_broadcast("bcast").unwrap();
	let tracks = (0..tracks)
		.map(|i| broadcast.create_track(format!("t{i}"), None).unwrap())
		.collect();
	broadcast.announce(Default::default()).unwrap();

	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(server.consume());
	options.client_subscribe = Some(client.clone());
	options.server_limits = Some(limits);
	let pair = connect_mock(options).await;

	Setup {
		client,
		_server: server,
		_broadcast: broadcast,
		tracks,
		_pair: pair,
	}
}

fn limits(announces: usize, subscriptions: usize) -> Limits {
	let mut limits = Limits::default();
	limits.announces = announces;
	limits.subscriptions = subscriptions;
	limits
}

/// Many more requests than the window admits at once, opened and closed in turn, all
/// succeed: each one that closes is granted back.
#[moq_net_sim::test]
async fn closed_requests_are_granted_back() {
	const ROUNDS: usize = 64;

	for version in VERSIONS {
		// A window of 8 requests, far fewer than the rounds below. The cap leaves room
		// for an UNSUBSCRIBE still in flight when the next SUBSCRIBE lands.
		let setup = setup(version, limits(0, 4), ROUNDS).await;
		let broadcast = moq_net_sim::timeout(TIMEOUT, setup.client.consume().routed_broadcast("bcast"))
			.await
			.expect("announce timeout")
			.unwrap();

		for round in 0..ROUNDS {
			let track = broadcast.track(&format!("t{round}")).unwrap();
			let subscriber = moq_net_sim::timeout(TIMEOUT, track.subscribe(None))
				.await
				.unwrap_or_else(|_| panic!("{version}: round {round} stalled"))
				.unwrap_or_else(|err| panic!("{version}: round {round} refused: {err}"));
			drop(subscriber);
			drop(track);

			// Wait for the server to see it end, so the next round is a fresh request.
			let published = &setup.tracks[round];
			moq_net_sim::timeout(TIMEOUT, published.demand().unused())
				.await
				.unwrap_or_else(|_| panic!("{version}: round {round} never ended"))
				.unwrap();
		}
	}
}

/// A subscription past the cap is refused on its own: the session and the subscription
/// already held carry on, and the slot frees once that one ends.
#[moq_net_sim::test]
async fn subscriptions_past_the_cap_are_refused() {
	for version in VERSIONS {
		let setup = setup(version, limits(1, 1), 2).await;
		let broadcast = moq_net_sim::timeout(TIMEOUT, setup.client.consume().routed_broadcast("bcast"))
			.await
			.expect("announce timeout")
			.unwrap();

		let held = moq_net_sim::timeout(TIMEOUT, broadcast.track("t0").unwrap().subscribe(None))
			.await
			.expect("subscribe timeout")
			.unwrap_or_else(|err| panic!("{version}: the first subscription was refused: {err}"));

		// moq-lite hands the subscriber over once TRACK_INFO answers, so its refusal
		// arrives on the first read instead.
		let refused = moq_net_sim::timeout(TIMEOUT, async {
			let mut subscriber = broadcast.track("t1").unwrap().subscribe(None).await?;
			subscriber.recv_group().await.map(|_| ())
		})
		.await
		.expect("subscribe timeout");
		assert!(refused.is_err(), "{version}: a second subscription was admitted");

		drop(held);
		moq_net_sim::timeout(TIMEOUT, setup.tracks[0].demand().unused())
			.await
			.expect("unsubscribe timeout")
			.unwrap();

		moq_net_sim::timeout(TIMEOUT, broadcast.track("t1").unwrap().subscribe(None))
			.await
			.expect("subscribe timeout")
			.unwrap_or_else(|err| panic!("{version}: the freed slot was not reused: {err}"));
	}
}
