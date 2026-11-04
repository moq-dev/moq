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
	let up = dialer.open_uni(None).unwrap();
	assert_eq!(up.name(), ".dial/uni/0");
	send(&up, b"from the dialer");
	let uni = kio::clock::timeout(TIMEOUT, accepted.accept_uni())
		.await
		.expect("accept timeout")
		.unwrap()
		.expect("a stream");
	assert_eq!(uni.name(), up.name());
	assert_eq!(recv(&uni).await, b"from the dialer");

	let down = accepted.open_uni(None).unwrap();
	assert_eq!(down.name(), ".accept/uni/0");
	send(&down, b"from the listener");
	let uni = kio::clock::timeout(TIMEOUT, dialer.accept_uni())
		.await
		.expect("accept timeout")
		.unwrap()
		.expect("a stream");
	assert_eq!(uni.name(), down.name());
	assert_eq!(recv(&uni).await, b"from the listener");

	// A bi stream each way, both the opener's second stream: the opener's role keeps
	// them apart.
	for (opener, acceptor) in [(&dialer, &accepted), (&accepted, &dialer)] {
		let info = track::Info::default().with_priority(3);
		let open = opener.open_bi(info).unwrap();
		let (opened, answered) = kio::clock::timeout(TIMEOUT, async { futures::join!(open, acceptor.accept_bi()) })
			.await
			.expect("bi timeout");
		let opened = opened.expect("open");
		let answered = answered.expect("accept").expect("a stream");
		assert!(opened.producer.name().ends_with("/bi/1"), "{}", opened.producer.name());
		assert_eq!(answered.producer.name(), opened.producer.name());
		assert_eq!(
			answered.producer.info().priority,
			3,
			"the answer mirrors the opener's info"
		);
		let info = kio::clock::timeout(TIMEOUT, opened.consumer.info())
			.await
			.expect("info timeout")
			.expect("the answer's info");
		assert_eq!(info.priority, 3, "the opener reads the mirrored info");

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
	let up = new_dialer.open_uni(None).unwrap();
	send(&up, b"again");
	let uni = kio::clock::timeout(TIMEOUT, new.accept_uni())
		.await
		.expect("accept timeout")
		.unwrap()
		.expect("a stream");
	assert_eq!(recv(&uni).await, b"again");
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

/// A stream is accepted only while its opener lists it: one whose producer the opener
/// dropped first, finished or not, is never handed out.
#[kio::test]
async fn a_stream_is_accepted_only_while_listed() {
	let fixture = Fixture::new().await;
	let mut listener = Listener::new(&fixture.listener, "room").unwrap();
	let (dialer, accepted) = connect(&fixture.dialer, &mut listener, "alice").await;

	let gone = dialer.open_uni(None).unwrap();
	let done = dialer.open_uni(None).unwrap();
	let live = dialer.open_uni(None).unwrap();
	// Let the listener's listing hear of all three before any ends.
	assert!(kio::clock::timeout(SETTLE, accepted.accept_bi()).await.is_err());
	drop(gone);
	done.finish().unwrap();
	drop(done);
	// And of the ends.
	assert!(kio::clock::timeout(SETTLE, accepted.accept_bi()).await.is_err());

	let uni = kio::clock::timeout(TIMEOUT, accepted.accept_uni())
		.await
		.expect("accept timeout")
		.unwrap()
		.expect("a stream");
	assert_eq!(uni.name(), live.name());
	assert!(
		kio::clock::timeout(SETTLE, accepted.accept_uni()).await.is_err(),
		"a stream that left the listing was accepted"
	);
	drop(live);
}

/// Two concurrent bi opens are two streams, each answered on its own, and dropping the
/// socket ends it for the peer even while an open is pending.
#[kio::test]
async fn a_pending_open_neither_merges_nor_outlives_its_socket() {
	let fixture = Fixture::new().await;
	let mut listener = Listener::new(&fixture.listener, "room").unwrap();
	let (dialer, accepted) = connect(&fixture.dialer, &mut listener, "alice").await;

	let first = dialer.open_bi(None).unwrap();
	let second = dialer.open_bi(None).unwrap();
	let (first, second, answers) = kio::clock::timeout(TIMEOUT, async {
		futures::join!(first, second, async {
			(accepted.accept_bi().await, accepted.accept_bi().await)
		})
	})
	.await
	.expect("bi timeout");
	let (first, second) = (first.expect("first open"), second.expect("second open"));
	let one = answers.0.expect("accept").expect("a stream");
	let two = answers.1.expect("accept").expect("a stream");
	assert_ne!(one.producer.name(), two.producer.name());
	send(&first.producer, b"one");
	send(&second.producer, b"two");
	assert_eq!(recv(&one.consumer).await, b"one");
	assert_eq!(recv(&two.consumer).await, b"two");
	send(&one.producer, b"one back");
	send(&two.producer, b"two back");
	assert_eq!(recv(&first.consumer).await, b"one back");
	assert_eq!(recv(&second.consumer).await, b"two back");

	let pending = dialer.open_bi(None).unwrap();
	drop(dialer);
	assert_eq!(
		kio::clock::timeout(TIMEOUT, accepted.closed())
			.await
			.expect("socket open"),
		Closed::Remote
	);
	drop(pending);
}

/// Closing a socket ends its pending bi opens, both one the peer never answers and one
/// answered too late, and refuses new streams.
#[kio::test]
async fn closing_a_socket_ends_its_opens() {
	let fixture = Fixture::new().await;
	let mut listener = Listener::new(&fixture.listener, "room").unwrap();
	let (dialer, accepted) = connect(&fixture.dialer, &mut listener, "alice").await;

	let late = dialer.open_bi(None).unwrap();
	let stalled = dialer.open_bi(None).unwrap();
	// The peer answers the first and never the second, and keeps its broadcast up: the
	// listener is not polled again, so nothing ends its socket.
	let answered = kio::clock::timeout(TIMEOUT, accepted.accept_bi())
		.await
		.expect("accept timeout")
		.unwrap()
		.expect("a stream");
	assert_eq!(answered.producer.name(), ".dial/bi/0", "the earlier open is answered");
	// Let the dialer's listing hear the answer before the close.
	assert!(kio::clock::timeout(SETTLE, dialer.accept_bi()).await.is_err());

	dialer.close();
	assert!(
		matches!(
			kio::clock::timeout(TIMEOUT, late).await.expect("late open"),
			Err(moq_net::Error::Closed)
		),
		"an answer after the close was taken"
	);
	assert!(
		matches!(
			kio::clock::timeout(TIMEOUT, stalled).await.expect("stalled open"),
			Err(moq_net::Error::Closed)
		),
		"a stalled open outlived the close"
	);

	assert!(matches!(dialer.open_uni(None), Err(moq_net::Error::Closed)));
	assert!(matches!(dialer.open_bi(None), Err(moq_net::Error::Closed)));
	assert!(matches!(
		kio::clock::timeout(TIMEOUT, dialer.accept_uni()).await.expect("accept"),
		Err(moq_net::Error::Closed)
	));
	drop(answered);
}

/// Each open is a new track, never an earlier one's: a held handle to an earlier stream
/// never reads a later one, and a bi open is answered while the peer still holds its
/// answer to an earlier one.
#[kio::test]
async fn each_open_is_a_new_track() {
	let fixture = Fixture::new().await;
	let mut listener = Listener::new(&fixture.listener, "room").unwrap();
	let (dialer, accepted) = connect(&fixture.dialer, &mut listener, "alice").await;

	// Accepted and held without subscribing, then the opener moves on to a new stream.
	let old = dialer.open_uni(None).unwrap();
	let held = kio::clock::timeout(TIMEOUT, accepted.accept_uni())
		.await
		.expect("accept timeout")
		.unwrap()
		.expect("a stream");
	drop(old);
	let new = dialer.open_uni(None).unwrap();
	send(&new, b"new");
	let fresh = kio::clock::timeout(TIMEOUT, accepted.accept_uni())
		.await
		.expect("accept timeout")
		.unwrap()
		.expect("a stream");
	assert_ne!(fresh.name(), held.name());
	assert_eq!(recv(&fresh).await, b"new");
	let stale = kio::clock::timeout(SETTLE, async {
		let mut subscriber = held.subscribe(None).await.ok()?;
		let mut group = subscriber.recv_group().await.ok()??;
		Some(group.read_frame().await.ok()??.payload.to_vec())
	})
	.await;
	assert!(
		!matches!(stale, Ok(Some(payload)) if payload == b"new"),
		"the earlier stream's handle read the new one"
	);

	// A bi answer the peer keeps does not stop the next bi open.
	let first = dialer.open_bi(None).unwrap();
	let (opened, answered) = kio::clock::timeout(TIMEOUT, async { futures::join!(first, accepted.accept_bi()) })
		.await
		.expect("bi timeout");
	let _kept = answered.expect("accept").expect("a stream");
	drop(opened.expect("open"));
	let again = dialer.open_bi(None).unwrap();
	let (opened, answered) = kio::clock::timeout(TIMEOUT, async { futures::join!(again, accepted.accept_bi()) })
		.await
		.expect("reopen timeout");
	let opened = opened.expect("open");
	let answered = answered.expect("accept").expect("a stream");
	send(&opened.producer, b"ping");
	assert_eq!(recv(&answered.consumer).await, b"ping");
	send(&answered.producer, b"pong");
	assert_eq!(recv(&opened.consumer).await, b"pong");
}

/// Open a bi stream from `dialer`, and have `accepted` accept it, write one group, and
/// finish its half before the open is polled, never reading the opener's half itself.
/// The opener still resolves and reads what the peer wrote.
async fn a_finished_answer_reaches_the_opener(dialer: &Socket, accepted: &Socket) {
	let open = dialer.open_bi(None).unwrap();
	let answered = kio::clock::timeout(TIMEOUT, accepted.accept_bi())
		.await
		.expect("accept timeout")
		.unwrap()
		.expect("a stream");
	send(&answered.producer, b"greeting");
	answered.producer.finish().unwrap();
	// Held, finished, as any stream is until the peer reads it.
	let answer = answered.producer;
	assert!(kio::clock::timeout(SETTLE, accepted.accept_bi()).await.is_err());

	let opened = kio::clock::timeout(TIMEOUT, open)
		.await
		.expect("open timeout")
		.expect("open");
	assert_eq!(recv(&opened.consumer).await, b"greeting");
	drop(answer);
}

/// A finished answer reaches the opener through a relay.
#[kio::test]
async fn a_finished_answer_reaches_the_opener_through_a_relay() {
	let fixture = Fixture::new().await;
	let mut listener = Listener::new(&fixture.listener, "room").unwrap();
	let (dialer, accepted) = connect(&fixture.dialer, &mut listener, "alice").await;
	a_finished_answer_reaches_the_opener(&dialer, &accepted).await;
}

/// A finished answer reaches the opener on one origin, with no session between them.
#[kio::test]
async fn a_finished_answer_reaches_the_opener_on_one_origin() {
	let shared = origin(1);
	let mut listener = Listener::new(&shared, "room").unwrap();
	let (dialer, accepted) = connect(&shared, &mut listener, "alice").await;
	a_finished_answer_reaches_the_opener(&dialer, &accepted).await;
}

/// A peer that accepts a bi stream and drops its half at once, only reading, still
/// answers the open: its half left the listing before the opener could see it, and it
/// has not subscribed to the opener's half yet either.
#[kio::test]
async fn a_dropped_answer_still_answers_the_open() {
	let fixture = Fixture::new().await;
	let mut listener = Listener::new(&fixture.listener, "room").unwrap();
	let (dialer, accepted) = connect(&fixture.dialer, &mut listener, "alice").await;

	let open = dialer.open_bi(None).unwrap();
	let answered = kio::clock::timeout(TIMEOUT, accepted.accept_bi())
		.await
		.expect("accept timeout")
		.unwrap()
		.expect("a stream");
	drop(answered.producer);
	let request = answered.consumer;
	assert!(kio::clock::timeout(SETTLE, accepted.accept_bi()).await.is_err());

	let opened = kio::clock::timeout(TIMEOUT, open)
		.await
		.expect("open timeout")
		.expect("open");
	send(&opened.producer, b"request");
	assert_eq!(recv(&request).await, b"request");
}

/// A fixed-name track sits beside the streams: accept skips it, and the peer reads it by
/// name from [`Socket::remote`].
#[kio::test]
async fn fixed_tracks_sit_beside_streams() {
	let fixture = Fixture::new().await;
	let mut listener = Listener::new(&fixture.listener, "room").unwrap();
	let (dialer, accepted) = connect(&fixture.dialer, &mut listener, "alice").await;

	let command = dialer.local().create_track("command", None).unwrap();
	send(&command, b"press a");
	let stream = dialer.open_uni(None).unwrap();
	send(&stream, b"stream");

	let uni = kio::clock::timeout(TIMEOUT, accepted.accept_uni())
		.await
		.expect("accept timeout")
		.unwrap()
		.expect("a stream");
	assert_eq!(uni.name(), stream.name(), "accept skips the fixed track");
	assert_eq!(recv(&uni).await, b"stream");
	assert_eq!(recv(&accepted.remote().track("command").unwrap()).await, b"press a");
	assert!(
		kio::clock::timeout(SETTLE, accepted.accept_uni()).await.is_err(),
		"the fixed track was accepted as a stream"
	);
}
