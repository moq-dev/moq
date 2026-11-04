#![cfg(feature = "async")]

//! Sockets through a relay: endpoints on their own origins, each with a session to a
//! relay, dialing and accepting sessions over moq-lite-07 and moq-transport.

use std::time::Duration;

use moq_net::broadcast::TracksEvent;
use moq_net::socket::{Endpoint, Stream};
use moq_net::{Error, Hop, Version, origin, track};

use crate::support::harness::{MockConnectOptions, MockPair, connect_mock, spawn};

const TIMEOUT: Duration = Duration::from_secs(10);

/// Long enough for every hop to settle, in simulated time.
const SETTLE: Duration = Duration::from_secs(1);

const LITE: &str = "moq-lite-07-wip";
const IETF: &str = "moq-transport-18";

fn origin(hop: u64) -> origin::Producer {
	let (origin, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	spawn(driver);
	origin
}

/// A relay and the clients connected to it.
struct Fixture {
	relay: origin::Producer,
	next: u64,
	sessions: Vec<MockPair>,
}

impl Fixture {
	fn new() -> Self {
		Self {
			relay: origin(1),
			next: 2,
			sessions: Vec::new(),
		}
	}

	/// A client publishing and subscribing through one origin, connected to the relay over
	/// `version`.
	async fn client(&mut self, version: &str) -> origin::Producer {
		let client = origin(self.next);
		self.next += 1;
		let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
		options.client_publish = Some(client.consume());
		options.client_subscribe = Some(client.clone());
		options.server_publish = Some(self.relay.consume());
		options.server_subscribe = Some(self.relay.clone());
		// Discovery follows the endpoint's own requests, as it does for an application.
		options.client_discovers_all = false;
		self.sessions.push(connect_mock(options).await);
		client
	}

	/// An endpoint `id` on its own client of the relay.
	async fn endpoint(&mut self, version: &str, id: &str) -> Endpoint {
		let client = self.client(version).await;
		Endpoint::new(&client, "room", id).unwrap()
	}
}

/// Dial `acceptor` from `dialer` while `acceptor` accepts, returning both ends.
async fn connect(dialer: &Endpoint, acceptor: &Endpoint) -> (Stream, Stream) {
	let dial = dialer.dial(acceptor.id(), None).unwrap();
	let accept = async {
		let incoming = acceptor.accept().await?;
		assert_eq!(incoming.peer(), dialer.id());
		incoming.accept(None).await
	};
	let (dialed, accepted) = kio::clock::timeout(TIMEOUT, async { futures::join!(dial, accept) })
		.await
		.expect("connect timeout");
	(dialed.expect("dial"), accepted.expect("accept"))
}

/// Write `payload` as one message: one group of `track`.
fn send(track: &track::Producer, payload: &'static [u8]) {
	let mut group = track.append_group().unwrap();
	group.write_frame(None, payload).unwrap();
	group.finish().unwrap();
}

/// Read the next message from `subscriber`, or `None` once the peer finished.
async fn recv(subscriber: &mut track::Subscriber) -> Result<Option<Vec<u8>>, Error> {
	kio::clock::timeout(TIMEOUT, async {
		let Some(mut group) = subscriber.recv_group().await? else {
			return Ok(None);
		};
		let frame = group.read_frame().await?.expect("a frame");
		Ok(Some(frame.payload.to_vec()))
	})
	.await
	.expect("read timeout")
}

/// Messages flow both ways, in order, over every pairing of moq-lite-07 and
/// moq-transport: the handshake uses neither instances nor track listings.
#[kio::test]
async fn sessions_carry_messages_both_ways() {
	for (dialer, acceptor) in [(LITE, LITE), (LITE, IETF), (IETF, LITE), (IETF, IETF)] {
		let mut fixture = Fixture::new();
		let alice = fixture.endpoint(dialer, "alice").await;
		let bob = fixture.endpoint(acceptor, "bob").await;
		let (mut dialed, mut accepted) = connect(&alice, &bob).await;
		assert_eq!(dialed.peer(), "bob");
		assert_eq!(accepted.peer(), "alice");
		assert_eq!(dialed.session(), accepted.session());

		for message in [b"one", b"two", b"six"] {
			send(&dialed.producer, message);
		}
		for message in [b"one", b"two", b"six"] {
			assert_eq!(
				recv(&mut accepted.consumer).await.unwrap().unwrap(),
				message,
				"{dialer} to {acceptor}"
			);
		}
		send(&accepted.producer, b"pong");
		assert_eq!(recv(&mut dialed.consumer).await.unwrap().unwrap(), b"pong");

		// Finishing a half closes the pipe that way.
		dialed.producer.finish().unwrap();
		assert_eq!(recv(&mut accepted.consumer).await.unwrap(), None);
	}
}

/// A pair of endpoints holds several sessions at once, each its own pipe.
#[kio::test]
async fn several_sessions_share_a_pair() {
	let mut fixture = Fixture::new();
	let alice = fixture.endpoint(LITE, "alice").await;
	let bob = fixture.endpoint(LITE, "bob").await;
	let (first, mut first_accepted) = connect(&alice, &bob).await;
	let (second, mut second_accepted) = connect(&alice, &bob).await;
	// Either side may dial.
	let (mut third, third_accepted) = connect(&bob, &alice).await;
	assert_ne!(first.session(), second.session());

	send(&second.producer, b"second");
	send(&first.producer, b"first");
	send(&third_accepted.producer, b"third");
	assert_eq!(recv(&mut first_accepted.consumer).await.unwrap().unwrap(), b"first");
	assert_eq!(recv(&mut second_accepted.consumer).await.unwrap().unwrap(), b"second");
	assert_eq!(recv(&mut third.consumer).await.unwrap().unwrap(), b"third");
}

/// Rejecting a dial resets it.
#[kio::test]
async fn a_rejected_dial_fails() {
	let mut fixture = Fixture::new();
	let alice = fixture.endpoint(LITE, "alice").await;
	let bob = fixture.endpoint(LITE, "bob").await;
	let dial = alice.dial("bob", None).unwrap();
	let reject = async {
		bob.accept().await.unwrap().reject();
	};
	let (dialed, ()) = kio::clock::timeout(TIMEOUT, async { futures::join!(dial, reject) })
		.await
		.expect("reject timeout");
	assert!(dialed.is_err(), "a rejected dial connected");
}

/// A dial waits for its peer's endpoint to be announced, and a dial to itself fails.
#[kio::test]
async fn a_dial_waits_for_its_peer() {
	let mut fixture = Fixture::new();
	let alice = fixture.endpoint(LITE, "alice").await;
	assert!(matches!(alice.dial("alice", None), Err(Error::Unsupported)));
	let mut dial = alice.dial("bob", None).unwrap();
	assert!(
		kio::clock::timeout(SETTLE, &mut dial).await.is_err(),
		"a dial to nobody resolved"
	);

	let bob = fixture.endpoint(LITE, "bob").await;
	let accept = async { bob.accept().await?.accept(None).await };
	let (dialed, accepted) = kio::clock::timeout(TIMEOUT, async { futures::join!(dial, accept) })
		.await
		.expect("connect timeout");
	assert_eq!(dialed.unwrap().session(), accepted.unwrap().session());
}

/// An accept of a dial claiming an endpoint nobody announces fails.
#[kio::test]
async fn a_dial_from_nobody_fails() {
	let mut fixture = Fixture::new();
	let bob = fixture.endpoint(LITE, "bob").await;
	let mallory = fixture.client(LITE).await;
	let broadcast = kio::clock::timeout(TIMEOUT, mallory.consume().request("room/.tcp/bob", None))
		.await
		.expect("request timeout")
		.expect("bob's endpoint");
	let forged = broadcast
		.track("carol/0123456789abcdef0123456789abcdef")
		.unwrap()
		.subscribe(None);
	let incoming = kio::clock::timeout(TIMEOUT, bob.accept())
		.await
		.expect("accept timeout")
		.unwrap();
	assert_eq!(incoming.peer(), "carol");
	let accepted = kio::clock::timeout(TIMEOUT, incoming.accept(None))
		.await
		.expect("accept timeout");
	assert!(accepted.is_err(), "bob connected to nobody");
	assert!(
		kio::clock::timeout(TIMEOUT, forged)
			.await
			.expect("forged timeout")
			.is_err()
	);
}

/// A third party asking bob for a session in alice's name surfaces at bob as a dial from
/// alice, but alice never dialed it, so she resets bob's accept, and the forger reads
/// nothing.
#[kio::test]
async fn a_forged_dial_never_connects() {
	let mut fixture = Fixture::new();
	let alice = fixture.endpoint(LITE, "alice").await;
	let bob = fixture.endpoint(LITE, "bob").await;
	let mallory = fixture.client(LITE).await;

	let broadcast = kio::clock::timeout(TIMEOUT, mallory.consume().request("room/.tcp/bob", None))
		.await
		.expect("request timeout")
		.expect("bob's endpoint");
	let forged = broadcast
		.track("alice/0123456789abcdef0123456789abcdef")
		.unwrap()
		.subscribe(None);

	let incoming = kio::clock::timeout(TIMEOUT, bob.accept())
		.await
		.expect("accept timeout")
		.unwrap();
	assert_eq!(incoming.peer(), "alice");
	let accepted = kio::clock::timeout(TIMEOUT, incoming.accept(None))
		.await
		.expect("accept timeout");
	assert!(accepted.is_err(), "bob connected to a forged dial");
	let forged = kio::clock::timeout(TIMEOUT, forged).await.expect("forged timeout");
	assert!(forged.is_err(), "the forger read bob's half");

	// Alice never heard of it as a dial either.
	assert!(kio::clock::timeout(SETTLE, alice.accept()).await.is_err());
}

/// A dial dropped before the peer accepts resets the accept.
#[kio::test]
async fn a_dropped_dial_resets_the_accept() {
	let mut fixture = Fixture::new();
	let alice = fixture.endpoint(LITE, "alice").await;
	let bob = fixture.endpoint(LITE, "bob").await;
	let mut dial = alice.dial("bob", None).unwrap();
	// Polled until bob hears of it, then dropped.
	let incoming = kio::clock::timeout(TIMEOUT, async {
		match futures::future::select(std::pin::pin!(bob.accept()), &mut dial).await {
			futures::future::Either::Left((incoming, _)) => incoming,
			futures::future::Either::Right(_) => panic!("the dial resolved unaccepted"),
		}
	})
	.await
	.expect("accept timeout")
	.unwrap();
	drop(dial);
	let accepted = kio::clock::timeout(TIMEOUT, incoming.accept(None))
		.await
		.expect("accept timeout");
	assert!(accepted.is_err(), "bob connected to a dropped dial");
}

/// Closing an endpoint aborts its sessions for the peer, and fails its pending work.
#[kio::test]
async fn closing_an_endpoint_ends_its_sessions() {
	let mut fixture = Fixture::new();
	let alice = fixture.endpoint(LITE, "alice").await;
	let bob = fixture.endpoint(LITE, "bob").await;
	let (mut dialed, accepted) = connect(&alice, &bob).await;

	bob.close();
	assert!(
		recv(&mut dialed.consumer).await.is_err(),
		"bob's half outlived his endpoint"
	);
	assert!(accepted.producer.append_group().is_err());
	assert!(matches!(bob.accept().await, Err(Error::Closed)));
	assert!(matches!(bob.dial("alice", None), Err(Error::Closed)));
}

/// Session tracks are the secret that keeps third parties out, so no listing names them:
/// a reader allowed everything lists each endpoint's broadcast while a session runs and
/// finds no track.
#[kio::test]
async fn session_tracks_are_never_listed() {
	let mut fixture = Fixture::new();
	let alice = fixture.endpoint(LITE, "alice").await;
	let bob = fixture.endpoint(LITE, "bob").await;
	let (dialed, mut accepted) = connect(&alice, &bob).await;
	send(&dialed.producer, b"secret");
	assert_eq!(recv(&mut accepted.consumer).await.unwrap().unwrap(), b"secret");

	let eve = fixture.client(LITE).await;
	for id in ["alice", "bob"] {
		let broadcast = kio::clock::timeout(TIMEOUT, eve.consume().request(format!("room/.tcp/{id}"), None))
			.await
			.expect("request timeout")
			.expect("the endpoint");
		let mut tracks = broadcast.tracks().unwrap();
		let event = kio::clock::timeout(TIMEOUT, tracks.next())
			.await
			.expect("listing timeout")
			.unwrap();
		assert!(matches!(event, Some(TracksEvent::Synced)), "{id} listed {event:?}");
		assert!(
			kio::clock::timeout(SETTLE, tracks.next()).await.is_err(),
			"{id} listed a track"
		);
	}
}

/// An endpoint takes dials only from its first accept on, like a TCP listener: before it,
/// a dial is reset rather than left waiting.
#[kio::test]
async fn an_endpoint_not_accepting_resets_dials() {
	let mut fixture = Fixture::new();
	let alice = fixture.endpoint(LITE, "alice").await;
	let _bob = fixture.endpoint(LITE, "bob").await;
	let dialed = kio::clock::timeout(TIMEOUT, alice.dial("bob", None).unwrap())
		.await
		.expect("dial timeout");
	assert!(dialed.is_err(), "a dial to an endpoint not accepting connected");
}
