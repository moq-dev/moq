#![cfg(feature = "async")]

//! Sockets through a relay: endpoints on their own origins, each with a session to a
//! relay, dialing and accepting `tcp` and `track` sessions over moq-lite-07 and
//! moq-transport.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use moq_net::broadcast::TracksEvent;
use moq_net::socket::{Endpoint, TcpStream, TrackStream};
use moq_net::{Error, Hop, Version, frame, group, origin, track};

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

const MB: usize = 1 << 20;

/// Dial `acceptor` from `dialer` in a `tcp` session while `acceptor` accepts, returning
/// both ends.
async fn connect_tcp(dialer: &Endpoint, acceptor: &Endpoint) -> (TcpStream, TcpStream) {
	let dial = dialer.dial_tcp(acceptor.id()).unwrap();
	let accept = async {
		let incoming = acceptor.accept_tcp().await?;
		assert_eq!(incoming.peer(), dialer.id());
		incoming.accept().await
	};
	let (dialed, accepted) = kio::clock::timeout(TIMEOUT, async { futures::join!(dial, accept) })
		.await
		.expect("connect timeout");
	(dialed.expect("dial"), accepted.expect("accept"))
}

/// Dial `acceptor` from `dialer` in a `track` session; see [`connect_tcp`].
async fn connect_track(dialer: &Endpoint, acceptor: &Endpoint) -> (TrackStream, TrackStream) {
	let dial = dialer.dial_track(acceptor.id(), None).unwrap();
	let accept = async {
		let incoming = acceptor.accept_track().await?;
		assert_eq!(incoming.peer(), dialer.id());
		incoming.accept(None).await
	};
	let (dialed, accepted) = kio::clock::timeout(TIMEOUT, async { futures::join!(dial, accept) })
		.await
		.expect("connect timeout");
	(dialed.expect("dial"), accepted.expect("accept"))
}

/// Read the next write from a `tcp` session, or `None` once the peer finished.
async fn read(reader: &mut group::Consumer) -> Result<Option<Vec<u8>>, Error> {
	kio::clock::timeout(TIMEOUT, reader.read_frame())
		.await
		.expect("read timeout")
		.map(|frame| frame.map(|frame| frame.payload.to_vec()))
}

/// Write `payload` as one message of a `track` session: one group.
fn send(track: &track::Producer, payload: &'static [u8]) {
	let mut group = track.append_group().unwrap();
	group.write_frame(None, payload).unwrap();
	group.finish().unwrap();
}

/// Read the next message from a `track` session, or `None` once the peer finished.
async fn recv(reader: &mut track::Ordered) -> Result<Option<Vec<u8>>, Error> {
	kio::clock::timeout(TIMEOUT, async {
		let Some(mut group) = reader.next_group().await? else {
			return Ok(None);
		};
		let frame = group.read_frame().await?.expect("a frame");
		Ok(Some(frame.payload.to_vec()))
	})
	.await
	.expect("read timeout")
}

/// Writes flow both ways, in order, over every pairing of moq-lite-07 and moq-transport,
/// and finishing a half is FIN: the handshake uses neither instances nor track listings.
#[kio::test]
async fn tcp_sessions_carry_writes_both_ways() {
	for (dialer, acceptor) in [(LITE, LITE), (LITE, IETF), (IETF, LITE), (IETF, IETF)] {
		let mut fixture = Fixture::new();
		let alice = fixture.endpoint(dialer, "alice").await;
		let bob = fixture.endpoint(acceptor, "bob").await;
		let (mut dialed, mut accepted) = connect_tcp(&alice, &bob).await;
		assert_eq!(dialed.peer(), "bob");
		assert_eq!(accepted.peer(), "alice");
		assert_eq!(dialed.session(), accepted.session());

		for write in [b"one", b"two", b"six"] {
			dialed.producer.write_frame(None, &write[..]).unwrap();
		}
		for write in [b"one", b"two", b"six"] {
			assert_eq!(
				read(&mut accepted.consumer).await.unwrap().unwrap(),
				write,
				"{dialer} to {acceptor}"
			);
		}
		accepted.producer.write_frame(None, &b"pong"[..]).unwrap();
		assert_eq!(read(&mut dialed.consumer).await.unwrap().unwrap(), b"pong");

		// FIN, each way.
		dialed.producer.finish().unwrap();
		assert_eq!(
			read(&mut accepted.consumer).await.unwrap(),
			None,
			"{dialer} to {acceptor}"
		);
		accepted.producer.finish().unwrap();
		assert_eq!(
			read(&mut dialed.consumer).await.unwrap(),
			None,
			"{dialer} to {acceptor}"
		);
	}
}

/// A long session is one group of many writes, read in order.
#[kio::test]
async fn a_long_tcp_session_stays_in_order() {
	const WRITES: u32 = 2000;
	let mut fixture = Fixture::new();
	let alice = fixture.endpoint(LITE, "alice").await;
	let bob = fixture.endpoint(LITE, "bob").await;
	let (mut dialed, mut accepted) = connect_tcp(&alice, &bob).await;

	let write = async {
		for i in 0..WRITES {
			let payload = Bytes::copy_from_slice(&i.to_be_bytes());
			dialed
				.producer
				.ready(&frame::Info {
					size: 4,
					timestamp: None,
				})
				.await;
			dialed.producer.write_frame(None, payload).unwrap();
		}
		dialed.producer.finish().unwrap();
	};
	let read_all = async {
		for i in 0..WRITES {
			assert_eq!(read(&mut accepted.consumer).await.unwrap().unwrap(), i.to_be_bytes());
		}
		assert_eq!(read(&mut accepted.consumer).await.unwrap(), None);
	};
	futures::join!(write, read_all);
}

/// Aborting a half is RST: the peer's read fails instead of ending.
#[kio::test]
async fn a_reset_tcp_half_fails_the_read() {
	for version in [LITE, IETF] {
		let mut fixture = Fixture::new();
		let alice = fixture.endpoint(version, "alice").await;
		let bob = fixture.endpoint(version, "bob").await;
		let (dialed, mut accepted) = connect_tcp(&alice, &bob).await;
		let mut producer = dialed.producer;
		producer.write_frame(None, &b"partial"[..]).unwrap();
		assert_eq!(read(&mut accepted.consumer).await.unwrap().unwrap(), b"partial");
		producer.abort(Error::Cancel).unwrap();
		assert!(
			read(&mut accepted.consumer).await.is_err(),
			"{version}: a reset read as FIN"
		);
	}
}

/// A slow reader paces the writer through every hop, and a session longer than a group's
/// live tail loses nothing: the writer waits instead of running ahead.
#[kio::test]
async fn a_slow_tcp_reader_paces_the_writer() {
	// Well past the 32 MiB a group copy holds, and the window every hop may buffer.
	const WRITES: usize = 192;
	let mut fixture = Fixture::new();
	let alice = fixture.endpoint(LITE, "alice").await;
	let bob = fixture.endpoint(LITE, "bob").await;
	let (mut dialed, mut accepted) = connect_tcp(&alice, &bob).await;

	let written = Arc::new(AtomicUsize::new(0));
	let write = {
		let written = written.clone();
		async move {
			for i in 0..WRITES {
				let payload = Bytes::from(vec![i as u8; MB]);
				let info = frame::Info {
					size: MB as u64,
					timestamp: None,
				};
				dialed.producer.ready(&info).await;
				dialed.producer.write_frame(None, payload).unwrap();
				written.fetch_add(1, Ordering::SeqCst);
			}
			dialed.producer.finish().unwrap();
		}
	};
	let read_all = async {
		// Not reading for a while: the writer stalls well short of the end.
		kio::clock::sleep(Duration::from_secs(5)).await;
		let ahead = written.load(Ordering::SeqCst);
		assert!(ahead < WRITES, "the writer ran {ahead} writes ahead of an idle reader");
		for i in 0..WRITES {
			let frame = read(&mut accepted.consumer).await.unwrap().expect("a write");
			assert_eq!(frame.len(), MB);
			assert!(frame.iter().all(|byte| *byte == i as u8), "write {i} out of order");
			// Slowly.
			kio::clock::sleep(Duration::from_millis(10)).await;
		}
		assert_eq!(read(&mut accepted.consumer).await.unwrap(), None);
	};
	kio::clock::timeout(Duration::from_secs(120), async { futures::join!(write, read_all) })
		.await
		.expect("session timeout");
}

/// A `tcp` session carol dialed by hand, as a nonconforming implementation would.
struct ByHand {
	/// Bob's end.
	accepted: TcpStream,
	/// Carol's half, which she writes groups to as she likes.
	half: track::Producer,
	_dialed: track::Subscriber,
	_endpoint: origin::Publication,
	_carol: origin::Producer,
}

/// Dial bob by hand from carol in a `tcp` session.
async fn dial_by_hand(fixture: &mut Fixture, bob: &Endpoint) -> ByHand {
	let carol = fixture.client(LITE).await;
	let endpoint = carol.create_broadcast("room/.socket/carol").unwrap();
	endpoint.announce(Default::default()).unwrap();
	let mut requests = endpoint.requests();
	let name = "tcp/carol/bob/0123456789abcdef0123456789abcdef";
	let broadcast = kio::clock::timeout(TIMEOUT, carol.consume().request("room/.socket/bob", None))
		.await
		.expect("request timeout")
		.expect("bob's endpoint");
	let dial = broadcast.track(name).unwrap().subscribe(None);

	let accept = async {
		let incoming = bob.accept_tcp().await.unwrap();
		incoming.accept().await
	};
	let answer = async {
		let request = kio::wait(|waiter| requests.poll_requested_track(waiter)).await.unwrap();
		assert_eq!(request.name(), name);
		request.accept(None)
	};
	let (accepted, half, dialed) = kio::clock::timeout(TIMEOUT, async { futures::join!(accept, answer, dial) })
		.await
		.expect("connect timeout");
	ByHand {
		accepted: accepted.expect("accept"),
		half,
		_dialed: dialed.expect("bob's half"),
		_endpoint: endpoint,
		_carol: carol,
	}
}

/// Write `sequence` as a finished group of one write.
fn write_group(half: &track::Producer, sequence: u64) {
	let mut group = half.create_group(group::Info { sequence }).unwrap();
	group.write_frame(None, &b"data"[..]).unwrap();
	group.finish().unwrap();
}

/// A peer that writes a second group in a `tcp` session breaks the protocol: the session
/// is reset, both halves.
#[kio::test]
async fn a_second_tcp_group_resets_the_session() {
	let mut fixture = Fixture::new();
	let bob = fixture.endpoint(LITE, "bob").await;
	// Bound whole, so carol's endpoint and read stay open to the end.
	let mut session = dial_by_hand(&mut fixture, &bob).await;
	let (accepted, half) = (&mut session.accepted, &session.half);

	write_group(half, 0);
	write_group(half, 1);
	// Bob reads at most the first group's write, then the reset.
	let mut reads = 0;
	loop {
		match read(&mut accepted.consumer).await {
			Ok(Some(_)) => reads += 1,
			Ok(None) => panic!("a second group read as FIN"),
			Err(_) => break,
		}
		assert!(reads <= 1, "read past the first group");
	}
	// And bob's own half was reset with it.
	kio::clock::sleep(SETTLE).await;
	assert!(accepted.producer.write_frame(None, &b"late"[..]).is_err());
}

/// A second group after the first one finished, and was read to its end, still resets
/// the half that remains open.
#[kio::test]
async fn a_second_tcp_group_after_fin_resets_the_session() {
	let mut fixture = Fixture::new();
	let bob = fixture.endpoint(LITE, "bob").await;
	// Bound whole, so carol's endpoint and read stay open to the end.
	let mut session = dial_by_hand(&mut fixture, &bob).await;
	let (accepted, half) = (&mut session.accepted, &session.half);

	write_group(half, 0);
	assert_eq!(read(&mut accepted.consumer).await.unwrap().unwrap(), b"data");
	assert_eq!(read(&mut accepted.consumer).await.unwrap(), None);
	accepted.producer.write_frame(None, &b"still open"[..]).unwrap();

	write_group(half, 1);
	kio::clock::sleep(SETTLE).await;
	assert!(accepted.producer.write_frame(None, &b"late"[..]).is_err());
}

/// The peer resetting its half after its group finished, and was read to its end, resets
/// the half that remains open, as a TCP RST after FIN does. Finishing it instead leaves
/// that half open, as a half-close does.
#[kio::test]
async fn a_peer_reset_after_fin_resets_the_session() {
	for reset in [true, false] {
		let mut fixture = Fixture::new();
		let bob = fixture.endpoint(LITE, "bob").await;
		// Bound whole, so carol's endpoint and read stay open to the end.
		let session = dial_by_hand(&mut fixture, &bob).await;
		let ByHand { mut accepted, half, .. } = session;

		write_group(&half, 0);
		assert_eq!(read(&mut accepted.consumer).await.unwrap().unwrap(), b"data");
		assert_eq!(read(&mut accepted.consumer).await.unwrap(), None);
		accepted.producer.write_frame(None, &b"still open"[..]).unwrap();

		if reset {
			half.abort(Error::Cancel).unwrap();
		} else {
			half.finish().unwrap();
		}
		kio::clock::sleep(SETTLE).await;
		let late = accepted.producer.write_frame(None, &b"late"[..]);
		assert_eq!(late.is_err(), reset, "reset: {reset}");
	}
}

/// Messages flow both ways, in order, over every pairing of moq-lite-07 and
/// moq-transport.
#[kio::test]
async fn track_sessions_carry_messages_both_ways() {
	for (dialer, acceptor) in [(LITE, LITE), (LITE, IETF), (IETF, LITE), (IETF, IETF)] {
		let mut fixture = Fixture::new();
		let alice = fixture.endpoint(dialer, "alice").await;
		let bob = fixture.endpoint(acceptor, "bob").await;
		let (dialed, accepted) = connect_track(&alice, &bob).await;
		assert_eq!(dialed.peer(), "bob");
		assert_eq!(dialed.session(), accepted.session());
		let mut reader = accepted.consumer.ordered();
		let mut replies = dialed.consumer.ordered();

		for message in [b"one", b"two", b"six"] {
			send(&dialed.producer, message);
		}
		for message in [b"one", b"two", b"six"] {
			assert_eq!(
				recv(&mut reader).await.unwrap().unwrap(),
				message,
				"{dialer} to {acceptor}"
			);
		}
		send(&accepted.producer, b"pong");
		assert_eq!(recv(&mut replies).await.unwrap().unwrap(), b"pong");

		// Finishing a half closes the pipe that way.
		dialed.producer.finish().unwrap();
		assert_eq!(recv(&mut reader).await.unwrap(), None);
	}
}

/// Messages are read in sequence even when they are written, and so arrive, out of order.
#[kio::test]
async fn track_messages_are_read_in_sequence() {
	let mut fixture = Fixture::new();
	let alice = fixture.endpoint(LITE, "alice").await;
	let bob = fixture.endpoint(LITE, "bob").await;
	let (dialed, accepted) = connect_track(&alice, &bob).await;
	let mut reader = accepted.consumer.ordered();

	let write = |sequence: u64, payload: &'static [u8]| {
		let mut group = dialed.producer.create_group(group::Info { sequence }).unwrap();
		group.write_frame(None, payload).unwrap();
		group.finish().unwrap();
	};
	write(1, b"second");
	// Let the later message arrive first.
	assert!(kio::clock::timeout(SETTLE, reader.next_group()).await.is_err());
	write(0, b"first");
	assert_eq!(recv(&mut reader).await.unwrap().unwrap(), b"first");
	assert_eq!(recv(&mut reader).await.unwrap().unwrap(), b"second");
}

/// With a finite delay budget, a message that never arrives stops blocking the ones after
/// it once the budget runs out.
#[kio::test]
async fn a_lost_track_message_does_not_block_later_ones() {
	let mut fixture = Fixture::new();
	let alice = fixture.endpoint(LITE, "alice").await;
	let bob = fixture.endpoint(LITE, "bob").await;
	let (dialed, accepted) = connect_track(&alice, &bob).await;
	let lossy = accepted
		.consumer
		.subscription()
		.with_delay(track::Delay::default().with_max(Duration::from_millis(500)));
	accepted.consumer.control().update(lossy).unwrap();
	let mut reader = accepted.consumer.ordered();

	let write = |sequence: u64, payload: &'static [u8]| {
		let mut group = dialed.producer.create_group(group::Info { sequence }).unwrap();
		group.write_frame(None, payload).unwrap();
		group.finish().unwrap();
	};
	// Message 0 is never written: message 1 waits out the budget, then is read.
	write(1, b"one");
	assert_eq!(recv(&mut reader).await.unwrap().unwrap(), b"one");
	write(2, b"two");
	assert_eq!(recv(&mut reader).await.unwrap().unwrap(), b"two");
}

/// A pair of endpoints holds several sessions at once in either protocol, each its own
/// pipe.
#[kio::test]
async fn several_sessions_share_a_pair() {
	let mut fixture = Fixture::new();
	let alice = fixture.endpoint(LITE, "alice").await;
	let bob = fixture.endpoint(LITE, "bob").await;
	let (mut first, mut first_accepted) = connect_tcp(&alice, &bob).await;
	let (mut second, mut second_accepted) = connect_tcp(&alice, &bob).await;
	// Either side may dial, in either protocol.
	let (third, third_accepted) = connect_track(&bob, &alice).await;
	assert_ne!(first.session(), second.session());

	second.producer.write_frame(None, &b"second"[..]).unwrap();
	first.producer.write_frame(None, &b"first"[..]).unwrap();
	send(&third_accepted.producer, b"third");
	assert_eq!(read(&mut first_accepted.consumer).await.unwrap().unwrap(), b"first");
	assert_eq!(read(&mut second_accepted.consumer).await.unwrap().unwrap(), b"second");
	assert_eq!(recv(&mut third.consumer.ordered()).await.unwrap().unwrap(), b"third");
	first.producer.finish().unwrap();
	second.producer.finish().unwrap();
}

/// Rejecting a dial resets it.
#[kio::test]
async fn a_rejected_dial_fails() {
	let mut fixture = Fixture::new();
	let alice = fixture.endpoint(LITE, "alice").await;
	let bob = fixture.endpoint(LITE, "bob").await;
	let dial = alice.dial_tcp("bob").unwrap();
	let reject = async {
		bob.accept_tcp().await.unwrap().reject();
	};
	let (dialed, ()) = kio::clock::timeout(TIMEOUT, async { futures::join!(dial, reject) })
		.await
		.expect("reject timeout");
	assert!(dialed.is_err(), "a rejected dial connected");
}

/// A dial in a protocol the peer does not accept is reset, while the peer still accepts
/// the protocol it listens on.
#[kio::test]
async fn a_protocol_nobody_accepts_is_reset() {
	let mut fixture = Fixture::new();
	let alice = fixture.endpoint(LITE, "alice").await;
	let bob = fixture.endpoint(LITE, "bob").await;
	let dial = alice.dial_track("bob", None).unwrap();
	let accept = async { kio::clock::timeout(SETTLE, bob.accept_tcp()).await };
	let (dialed, accepted) = kio::clock::timeout(TIMEOUT, async { futures::join!(dial, accept) })
		.await
		.expect("dial timeout");
	assert!(dialed.is_err(), "a track dial reached a tcp listener");
	assert!(accepted.is_err(), "a track dial surfaced as a tcp dial");

	// An unknown protocol is reset too.
	let mallory = fixture.client(LITE).await;
	let broadcast = kio::clock::timeout(TIMEOUT, mallory.consume().request("room/.socket/bob", None))
		.await
		.expect("request timeout")
		.expect("bob's endpoint");
	let unknown = broadcast
		.track("quic/mallory/bob/0123456789abcdef0123456789abcdef")
		.unwrap()
		.subscribe(None);
	let unknown = async { kio::clock::timeout(TIMEOUT, unknown).await };
	let accept = async { kio::clock::timeout(SETTLE, bob.accept_tcp()).await };
	let (unknown, accepted) = futures::join!(unknown, accept);
	assert!(unknown.expect("unknown timeout").is_err());
	assert!(accepted.is_err(), "an unknown protocol surfaced as a dial");
}

/// A dial waits for its peer's endpoint to be announced, and a dial to itself fails.
#[kio::test]
async fn a_dial_waits_for_its_peer() {
	let mut fixture = Fixture::new();
	let alice = fixture.endpoint(LITE, "alice").await;
	assert!(matches!(alice.dial_tcp("alice"), Err(Error::Unsupported)));
	let mut dial = alice.dial_tcp("bob").unwrap();
	assert!(
		kio::clock::timeout(SETTLE, &mut dial).await.is_err(),
		"a dial to nobody resolved"
	);

	let bob = fixture.endpoint(LITE, "bob").await;
	let accept = async { bob.accept_tcp().await?.accept().await };
	let (dialed, accepted) = kio::clock::timeout(TIMEOUT, async { futures::join!(dial, accept) })
		.await
		.expect("connect timeout");
	let (mut dialed, accepted) = (dialed.unwrap(), accepted.unwrap());
	assert_eq!(dialed.session(), accepted.session());
	dialed.producer.finish().unwrap();
}

/// An accept waits for the dialer's endpoint while the dial stays open, since a relay can
/// deliver a dial before the dialer's announcement: carol dials bob before she announces
/// her endpoint, and bob's accept connects once she does.
#[kio::test]
async fn an_accept_waits_for_its_dialer() {
	let mut fixture = Fixture::new();
	let bob = fixture.endpoint(LITE, "bob").await;
	let carol = fixture.client(LITE).await;
	let name = "tcp/carol/bob/0123456789abcdef0123456789abcdef";
	let broadcast = kio::clock::timeout(TIMEOUT, carol.consume().request("room/.socket/bob", None))
		.await
		.expect("request timeout")
		.expect("bob's endpoint");
	let dial = broadcast.track(name).unwrap().subscribe(None);

	let incoming = kio::clock::timeout(TIMEOUT, bob.accept_tcp())
		.await
		.expect("accept timeout")
		.unwrap();
	assert_eq!(incoming.peer(), "carol");
	let mut accept = incoming.accept();
	assert!(
		kio::clock::timeout(SETTLE, &mut accept).await.is_err(),
		"bob's accept ended before carol announced"
	);

	let endpoint = carol.create_broadcast("room/.socket/carol").unwrap();
	let mut requests = endpoint.requests();
	endpoint.announce(Default::default()).unwrap();
	let answer = async {
		let request = kio::wait(|waiter| requests.poll_requested_track(waiter)).await.unwrap();
		assert_eq!(request.name(), name);
		request.accept(None)
	};
	let (accepted, _half, dialed) = kio::clock::timeout(TIMEOUT, async { futures::join!(accept, answer, dial) })
		.await
		.expect("connect timeout");
	assert_eq!(accepted.expect("accept").peer(), "carol");
	dialed.expect("bob's half");
}

/// An accept of a dial claiming an endpoint nobody announces waits only while the dial
/// stays open: the forger dropping it fails the accept.
#[kio::test]
async fn an_accept_ends_with_its_dial() {
	let mut fixture = Fixture::new();
	let bob = fixture.endpoint(LITE, "bob").await;
	let mallory = fixture.client(LITE).await;
	let broadcast = kio::clock::timeout(TIMEOUT, mallory.consume().request("room/.socket/bob", None))
		.await
		.expect("request timeout")
		.expect("bob's endpoint");
	let forged = broadcast
		.track("tcp/carol/bob/0123456789abcdef0123456789abcdef")
		.unwrap()
		.subscribe(None);
	let incoming = kio::clock::timeout(TIMEOUT, bob.accept_tcp())
		.await
		.expect("accept timeout")
		.unwrap();
	assert_eq!(incoming.peer(), "carol");
	let mut accept = incoming.accept();
	assert!(
		kio::clock::timeout(SETTLE, &mut accept).await.is_err(),
		"bob's accept ended while the dial was open"
	);

	drop(forged);
	let accepted = kio::clock::timeout(TIMEOUT, accept).await.expect("accept timeout");
	assert!(accepted.is_err(), "bob connected to nobody");
}

/// A session is bound to both its ends: an endpoint alice dialed knows the session's id,
/// but replaying it to carol connects neither under bob's name nor under carol's.
#[kio::test]
async fn a_replayed_session_never_connects() {
	let mut fixture = Fixture::new();
	let alice = fixture.endpoint(LITE, "alice").await;
	let bob = fixture.endpoint(LITE, "bob").await;
	let carol = fixture.endpoint(LITE, "carol").await;
	let (mut dialed, mut accepted) = connect_tcp(&alice, &bob).await;
	let session = accepted.session().to_string();

	// Replayed from bob's side, which knows the session.
	let mallory = fixture.client(LITE).await;
	let broadcast = kio::clock::timeout(TIMEOUT, mallory.consume().request("room/.socket/carol", None))
		.await
		.expect("request timeout")
		.expect("carol's endpoint");

	// Under the name alice dialed: carol is not its acceptor, so she resets it unheard.
	let replayed = broadcast
		.track(&format!("tcp/alice/bob/{session}"))
		.unwrap()
		.subscribe(None);
	// Carol accepts first, so the replay reaches her listening.
	let accept = async { kio::clock::timeout(SETTLE, carol.accept_tcp()).await };
	let (accepted_by_carol, replayed) = futures::join!(accept, async { kio::clock::timeout(TIMEOUT, replayed).await });
	assert!(
		replayed.expect("replay timeout").is_err(),
		"carol served a replayed session"
	);
	assert!(accepted_by_carol.is_err(), "a session for bob surfaced at carol");

	// Renamed for carol: alice never dialed it, so she resets carol's accept.
	let renamed = broadcast
		.track(&format!("tcp/alice/carol/{session}"))
		.unwrap()
		.subscribe(None);
	let incoming = kio::clock::timeout(TIMEOUT, carol.accept_tcp())
		.await
		.expect("accept timeout")
		.unwrap();
	assert_eq!(incoming.peer(), "alice");
	let (accepted_by_carol, renamed) =
		kio::clock::timeout(TIMEOUT, async { futures::join!(incoming.accept(), renamed) })
			.await
			.expect("connect timeout");
	assert!(accepted_by_carol.is_err(), "carol connected to a replayed session");
	assert!(renamed.is_err(), "the replayer read carol's half");

	// The real session is untouched.
	dialed.producer.write_frame(None, &b"still here"[..]).unwrap();
	assert_eq!(read(&mut accepted.consumer).await.unwrap().unwrap(), b"still here");
}

/// A third party asking bob for a session in alice's name surfaces at bob as a dial from
/// alice, but alice never dialed it, so she resets bob's accept, and the forger reads
/// nothing. The same holds in either protocol.
#[kio::test]
async fn a_forged_dial_never_connects() {
	for protocol in ["tcp", "track"] {
		let mut fixture = Fixture::new();
		let alice = fixture.endpoint(LITE, "alice").await;
		let bob = fixture.endpoint(LITE, "bob").await;
		let mallory = fixture.client(LITE).await;

		let broadcast = kio::clock::timeout(TIMEOUT, mallory.consume().request("room/.socket/bob", None))
			.await
			.expect("request timeout")
			.expect("bob's endpoint");
		let forged = broadcast
			.track(&format!("{protocol}/alice/bob/0123456789abcdef0123456789abcdef"))
			.unwrap()
			.subscribe(None);

		let (incoming_peer, accepted) = match protocol {
			"tcp" => {
				let incoming = kio::clock::timeout(TIMEOUT, bob.accept_tcp())
					.await
					.expect("accept timeout")
					.unwrap();
				let peer = incoming.peer().to_string();
				let accepted = kio::clock::timeout(TIMEOUT, incoming.accept()).await;
				(peer, accepted.expect("accept timeout").map(drop))
			}
			_ => {
				let incoming = kio::clock::timeout(TIMEOUT, bob.accept_track())
					.await
					.expect("accept timeout")
					.unwrap();
				let peer = incoming.peer().to_string();
				let accepted = kio::clock::timeout(TIMEOUT, incoming.accept(None)).await;
				(peer, accepted.expect("accept timeout").map(drop))
			}
		};
		assert_eq!(incoming_peer, "alice");
		assert!(accepted.is_err(), "bob connected to a forged {protocol} dial");
		let forged = kio::clock::timeout(TIMEOUT, forged).await.expect("forged timeout");
		assert!(forged.is_err(), "the forger read bob's {protocol} half");

		// Alice never heard of it as a dial either.
		assert!(kio::clock::timeout(SETTLE, alice.accept_tcp()).await.is_err());
		assert!(kio::clock::timeout(SETTLE, alice.accept_track()).await.is_err());
	}
}

/// A dial dropped before the peer accepts resets the accept.
#[kio::test]
async fn a_dropped_dial_resets_the_accept() {
	let mut fixture = Fixture::new();
	let alice = fixture.endpoint(LITE, "alice").await;
	let bob = fixture.endpoint(LITE, "bob").await;
	let mut dial = alice.dial_tcp("bob").unwrap();
	// Polled until bob hears of it, then dropped.
	let incoming = kio::clock::timeout(TIMEOUT, async {
		match futures::future::select(std::pin::pin!(bob.accept_tcp()), &mut dial).await {
			futures::future::Either::Left((incoming, _)) => incoming,
			futures::future::Either::Right(_) => panic!("the dial resolved unaccepted"),
		}
	})
	.await
	.expect("accept timeout")
	.unwrap();
	drop(dial);
	let accepted = kio::clock::timeout(TIMEOUT, incoming.accept())
		.await
		.expect("accept timeout");
	assert!(accepted.is_err(), "bob connected to a dropped dial");
}

/// Closing an endpoint resets its sessions for the peer, and fails its pending work.
#[kio::test]
async fn closing_an_endpoint_ends_its_sessions() {
	let mut fixture = Fixture::new();
	let alice = fixture.endpoint(LITE, "alice").await;
	let bob = fixture.endpoint(LITE, "bob").await;
	let (mut tcp_dialed, mut tcp_accepted) = connect_tcp(&alice, &bob).await;
	let (track_dialed, track_accepted) = connect_track(&alice, &bob).await;

	bob.close();
	assert!(
		read(&mut tcp_dialed.consumer).await.is_err(),
		"bob's tcp half outlived his endpoint"
	);
	// Both halves of bob's own tcp session end.
	assert!(read(&mut tcp_accepted.consumer).await.is_err());
	assert!(tcp_accepted.producer.write_frame(None, &b"late"[..]).is_err());
	assert!(
		recv(&mut track_dialed.consumer.ordered()).await.is_err(),
		"bob's track half outlived his endpoint"
	);
	assert!(track_accepted.producer.append_group().is_err());
	assert!(matches!(bob.accept_tcp().await, Err(Error::Closed)));
	assert!(matches!(bob.accept_track().await, Err(Error::Closed)));
	assert!(matches!(bob.dial_tcp("alice"), Err(Error::Closed)));
	// Alice saw bob's half reset, so she reset hers: the session is gone both ways.
	assert!(tcp_dialed.producer.finish().is_err());
}

/// Session tracks are the secret that keeps third parties out, so no listing names them:
/// a reader allowed everything lists each endpoint's broadcast while a session runs and
/// finds no track.
#[kio::test]
async fn session_tracks_are_never_listed() {
	let mut fixture = Fixture::new();
	let alice = fixture.endpoint(LITE, "alice").await;
	let bob = fixture.endpoint(LITE, "bob").await;
	let (mut dialed, mut accepted) = connect_tcp(&alice, &bob).await;
	let (track_dialed, track_accepted) = connect_track(&alice, &bob).await;
	dialed.producer.write_frame(None, &b"secret"[..]).unwrap();
	assert_eq!(read(&mut accepted.consumer).await.unwrap().unwrap(), b"secret");
	send(&track_dialed.producer, b"secret");
	assert_eq!(
		recv(&mut track_accepted.consumer.ordered()).await.unwrap().unwrap(),
		b"secret"
	);

	let eve = fixture.client(LITE).await;
	for id in ["alice", "bob"] {
		let broadcast = kio::clock::timeout(TIMEOUT, eve.consume().request(format!("room/.socket/{id}"), None))
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
	dialed.producer.finish().unwrap();
}

/// An endpoint takes dials only from its first accept on, like a TCP listener: before it,
/// a dial is reset rather than left waiting.
#[kio::test]
async fn an_endpoint_not_accepting_resets_dials() {
	let mut fixture = Fixture::new();
	let alice = fixture.endpoint(LITE, "alice").await;
	let _bob = fixture.endpoint(LITE, "bob").await;
	let dialed = kio::clock::timeout(TIMEOUT, alice.dial_tcp("bob").unwrap())
		.await
		.expect("dial timeout");
	assert!(dialed.is_err(), "a dial to an endpoint not accepting connected");
}

/// An acceptor can finish its initial burst before the dialer gets the answer.
#[kio::test]
async fn the_first_burst_survives_a_late_subscribe_answer() {
	for version in [LITE, IETF, "moq-transport-20", "moq-transport-22"] {
		let mut fixture = Fixture::new();
		let alice = fixture.endpoint(version, "alice").await;
		let bob = fixture.endpoint(version, "bob").await;
		let dial = alice.dial_track("bob", None).unwrap();
		let accept = async {
			let stream = bob.accept_track().await.unwrap().accept(None).await.unwrap();
			for payload in [b"one", b"two", b"six"] {
				send(&stream.producer, payload);
			}
			stream.producer.finish().unwrap();
			stream
		};
		let (dialed, accepted) = futures::join!(dial, accept);
		let dialed = dialed.unwrap();
		dialed.producer.finish().unwrap();
		let mut reader = dialed.consumer.ordered();
		for payload in [b"one", b"two", b"six"] {
			assert_eq!(recv(&mut reader).await.unwrap().unwrap(), payload, "{version}");
		}
		assert_eq!(recv(&mut reader).await.unwrap(), None);
		assert_eq!(recv(&mut accepted.consumer.ordered()).await.unwrap(), None);
	}
}

/// TCP must also recover every frame when FIN overtakes the subscription answer.
#[kio::test]
async fn the_first_tcp_writes_survive_a_late_subscribe_answer() {
	for version in [LITE, IETF, "moq-transport-20", "moq-transport-22"] {
		let mut fixture = Fixture::new();
		let alice = fixture.endpoint(version, "alice").await;
		let bob = fixture.endpoint(version, "bob").await;
		let dial = alice.dial_tcp("bob").unwrap();
		let accept = async {
			let mut stream = bob.accept_tcp().await.unwrap().accept().await.unwrap();
			for payload in [b"first".as_slice(), b"", b"last"] {
				stream.producer.write_frame(None, payload).unwrap();
			}
			stream.producer.finish().unwrap();
			stream
		};
		let (dialed, accepted) = futures::join!(dial, accept);
		let mut dialed = dialed.unwrap();
		let mut accepted = accepted;
		for payload in [b"first".as_slice(), b"", b"last"] {
			assert_eq!(read(&mut dialed.consumer).await.unwrap().unwrap(), payload, "{version}");
		}
		assert_eq!(read(&mut dialed.consumer).await.unwrap(), None);
		dialed.producer.finish().unwrap();
		assert_eq!(read(&mut accepted.consumer).await.unwrap(), None);
	}
}
