#![cfg(feature = "async")]

//! Sockets through a relay: a dialer and a listener on their own origins, each with a
//! session to a relay, over moq-lite-07.

use std::time::Duration;

use moq_net::socket::{self, Closed, Listener, Socket};
use moq_net::{Hop, Instance, Version, origin, track};

use crate::support::harness::{MockConnectOptions, MockPair, connect_mock, spawn};

const TIMEOUT: Duration = Duration::from_secs(10);

/// Long enough for every hop to settle, in simulated time.
const SETTLE: Duration = Duration::from_secs(1);

fn origin(hop: u64) -> origin::Producer {
	let (origin, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	spawn(driver);
	origin
}

/// A client publishing and subscribing through one origin, connected to `relay`.
async fn client(origin: &origin::Producer, relay: &origin::Producer) -> MockPair {
	let mut options = MockConnectOptions::new("moq-lite-07-wip".parse::<Version>().unwrap());
	options.client_publish = Some(origin.consume());
	options.client_subscribe = Some(origin.clone());
	options.server_publish = Some(relay.consume());
	options.server_subscribe = Some(relay.clone());
	// Discovery follows the socket's own watches, as it does for an application.
	options.client_discovers_all = false;
	connect_mock(options).await
}

/// A relay with a listener client and a dialer client.
struct Fixture {
	relay: origin::Producer,
	dialer: origin::Producer,
	listener: origin::Producer,
	sessions: Vec<MockPair>,
}

impl Fixture {
	async fn new() -> Self {
		let relay = origin(1);
		let dialer = origin(2);
		let listener = origin(3);
		let sessions = vec![client(&dialer, &relay).await, client(&listener, &relay).await];
		Self {
			relay,
			dialer,
			listener,
			sessions,
		}
	}

	/// Another dialer client of the same relay.
	async fn dialer(&mut self, hop: u64) -> origin::Producer {
		let dialer = origin(hop);
		self.sessions.push(client(&dialer, &self.relay).await);
		dialer
	}
}

/// Dial `id` from `origin` while `listener` accepts, returning both ends.
async fn connect(origin: &origin::Producer, listener: &mut Listener, id: &str) -> (Socket, Socket) {
	let dial = socket::dial(origin, "room", id).unwrap();
	let (dialed, accepted) = kio::clock::timeout(TIMEOUT, async { futures::join!(dial, listener.accept()) })
		.await
		.expect("connect timeout");
	(dialed.expect("dial"), accepted.expect("accept"))
}

/// Write `payload` as one group of `track`.
fn send(track: &track::Producer, payload: &'static [u8]) {
	let mut group = track.append_group().unwrap();
	group.write_frame(None, payload).unwrap();
	group.finish().unwrap();
}

/// Subscribe to `track` and read its first frame.
async fn recv(track: &track::Consumer) -> Vec<u8> {
	kio::clock::timeout(TIMEOUT, async {
		let mut subscriber = track.subscribe(None).await.expect("subscribe");
		let mut group = subscriber.recv_group().await.expect("group").expect("a group");
		group
			.read_frame()
			.await
			.expect("frame")
			.expect("a frame")
			.payload
			.to_vec()
	})
	.await
	.expect("read timeout")
}

/// Uni and bi streams each way, through the relay.
#[kio::test]
async fn streams_flow_both_ways() {
	let fixture = Fixture::new().await;
	let mut listener = Listener::new(&fixture.listener, "room").unwrap();
	let (dialer, accepted) = connect(&fixture.dialer, &mut listener, "alice").await;
	assert_eq!(dialer.id(), "alice");
	assert_eq!(accepted.id(), "alice");

	// A uni stream each way.
	let up = dialer.open_uni("up", None).unwrap();
	send(&up, b"from the dialer");
	let uni = kio::clock::timeout(TIMEOUT, accepted.accept_uni())
		.await
		.expect("accept timeout")
		.unwrap()
		.expect("a stream");
	assert_eq!(&*uni.name, "up");
	assert_eq!(recv(&uni.consumer).await, b"from the dialer");

	let down = accepted.open_uni("down", None).unwrap();
	send(&down, b"from the listener");
	let uni = kio::clock::timeout(TIMEOUT, dialer.accept_uni())
		.await
		.expect("accept timeout")
		.unwrap()
		.expect("a stream");
	assert_eq!(&*uni.name, "down");
	assert_eq!(recv(&uni.consumer).await, b"from the listener");

	// A bi stream each way, under the same name: the opener's role keeps them apart.
	for (opener, acceptor) in [(&dialer, &accepted), (&accepted, &dialer)] {
		let info = track::Info::default().with_priority(3);
		let open = opener.open_bi("chat", info).unwrap();
		let (opened, answered) = kio::clock::timeout(TIMEOUT, async { futures::join!(open, acceptor.accept_bi()) })
			.await
			.expect("bi timeout");
		let opened = opened.expect("open");
		let answered = answered.expect("accept").expect("a stream");
		assert_eq!(&*answered.name, "chat");
		assert_eq!(
			answered.producer.info().priority,
			3,
			"the answer mirrors the opener's info"
		);

		send(&opened.producer, b"ping");
		assert_eq!(recv(&answered.consumer).await, b"ping");
		send(&answered.producer, b"pong");
		assert_eq!(recv(&opened.consumer).await, b"pong");
	}

	// Neither side's own streams come back as the peer's.
	assert!(
		kio::clock::timeout(SETTLE, dialer.accept_bi()).await.is_err(),
		"the dialer accepted a stream it opened"
	);
	assert!(
		kio::clock::timeout(SETTLE, dialer.accept_uni()).await.is_err(),
		"the dialer accepted a stream twice"
	);
}

/// A dialer that dials again with the same id, as a restarted one does, ends the socket
/// it replaces, and the listener accepts the new one.
#[kio::test]
async fn a_new_dial_replaces_the_old_socket() {
	let mut fixture = Fixture::new().await;
	let mut listener = Listener::new(&fixture.listener, "room").unwrap();
	let (old_dialer, old) = connect(&fixture.dialer, &mut listener, "alice").await;

	let restarted = fixture.dialer(4).await;
	let (new_dialer, new) = connect(&restarted, &mut listener, "alice").await;

	assert_eq!(
		kio::clock::timeout(TIMEOUT, old.closed())
			.await
			.expect("old socket open"),
		Closed::Replaced
	);
	assert_eq!(
		kio::clock::timeout(TIMEOUT, old_dialer.closed())
			.await
			.expect("old dialer open"),
		Closed::Remote
	);

	// The new socket is the new dialer's.
	let up = new_dialer.open_uni("up", None).unwrap();
	send(&up, b"again");
	let uni = kio::clock::timeout(TIMEOUT, new.accept_uni())
		.await
		.expect("accept timeout")
		.unwrap()
		.expect("a stream");
	assert_eq!(recv(&uni.consumer).await, b"again");
	assert!(
		kio::clock::timeout(SETTLE, new.closed()).await.is_err(),
		"the new socket ended"
	);
}

/// A dialer the listener has not accepted is not read: its tracks see no demand until
/// the application accepts it.
#[kio::test]
async fn an_unaccepted_dialer_holds_no_subscription() {
	let fixture = Fixture::new().await;
	let mut listener = Listener::new(&fixture.listener, "room").unwrap();

	// A dialer by hand, so the test can watch its broadcast.
	let dial = fixture.dialer.create_broadcast("room/.dial/bob").unwrap();
	let probe = dial.create_track("probe", None).unwrap();
	dial.announce(origin::Route::default().with_instance(Instance::random(), 0))
		.unwrap();

	let mut demand = probe.demand();
	assert!(demand.wanted_changed().await.unwrap().is_none());
	assert!(
		kio::clock::timeout(SETTLE, demand.wanted_changed()).await.is_err(),
		"an unaccepted dialer was read"
	);

	let accepted = kio::clock::timeout(TIMEOUT, listener.accept())
		.await
		.expect("accept timeout")
		.unwrap();
	assert_eq!(accepted.id(), "bob");
	let _subscriber = kio::clock::timeout(TIMEOUT, accepted.remote().track("probe").unwrap().subscribe(None))
		.await
		.expect("subscribe timeout")
		.unwrap();
	assert!(demand.wanted().is_some(), "the accepted dialer is read");
}

/// A listener going away ends the sockets it accepted.
#[kio::test]
async fn a_listener_restart_ends_its_sockets() {
	let mut fixture = Fixture::new().await;
	let mut listener = Listener::new(&fixture.listener, "room").unwrap();
	let (dialer, _accepted) = connect(&fixture.dialer, &mut listener, "alice").await;

	// The listener's session drops, as when its process restarts.
	drop(fixture.sessions.remove(1));
	assert_eq!(
		kio::clock::timeout(TIMEOUT, dialer.closed())
			.await
			.expect("socket open"),
		Closed::Remote
	);
}

/// A dial resolves only on the answer to it: neither an answer carrying another instance
/// at its path nor another id's answer is taken for it.
#[kio::test]
async fn a_dial_takes_only_its_own_answer() {
	let mut fixture = Fixture::new().await;

	// An answer at alice's path for some other dial of alice, such as a stale one.
	let stale = fixture
		.listener
		.publish(
			"room/.accept/alice",
			origin::Route::default().with_instance(Instance::random(), 0),
		)
		.unwrap();
	let dial = socket::dial(&fixture.dialer, "room", "alice").unwrap();
	let mut dial = std::pin::pin!(dial);
	assert!(
		kio::clock::timeout(SETTLE, dial.as_mut()).await.is_err(),
		"the dial took another dial's answer"
	);
	drop(stale);

	let mut listener = Listener::new(&fixture.listener, "room").unwrap();
	let alice = kio::clock::timeout(TIMEOUT, listener.accept())
		.await
		.expect("accept timeout")
		.unwrap();
	assert_eq!(alice.id(), "alice");
	let dialer = kio::clock::timeout(TIMEOUT, dial).await.expect("dial timeout").unwrap();
	assert_eq!(dialer.id(), "alice");
	assert_eq!(
		dialer.remote().info().instance,
		alice.remote().info().instance,
		"the answer carries the dial's instance"
	);

	// Another id is its own socket, never alice's.
	let bob = fixture.dialer(4).await;
	let (bob_dialer, bob_accepted) = connect(&bob, &mut listener, "bob").await;
	assert_eq!(bob_dialer.id(), "bob");
	assert_eq!(bob_accepted.id(), "bob");
	assert_ne!(bob_accepted.remote().info().instance, alice.remote().info().instance);
}
