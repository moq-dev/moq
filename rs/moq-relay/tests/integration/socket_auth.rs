//! Sockets through a relay that admits with `moq-auth` tokens, verified by the reference
//! auth server, granting only the prefixes `doc/concept/socket.md` lists.
//!
//! Every token is rooted at the socket prefix `room`. An endpoint's token publishes its own
//! endpoint, `.socket/<id>`, and subscribes to every endpoint, `.socket/**`, so the
//! session track names are what keep third parties out: a forged dial never connects, and no
//! listing gives a session's name away.

use std::time::Duration;

use moq_net::broadcast::TracksEvent;
use moq_net::socket::{Endpoint, TcpStream, TrackStream};

use crate::memory;

const TIMEOUT: Duration = Duration::from_secs(10);

/// Long enough for a listing to have reported a track had it listed one. The relay links
/// are in memory, so this is many round trips.
const SETTLE: Duration = Duration::from_millis(500);

/// The socket prefix, and every token's root.
const ROOT: &str = "room";

const LITE: &str = "moq-lite-07-wip";
const IETF: &str = "moq-transport-18";

/// A key, the reference auth server verifying tokens it signs, and a relay asking it.
struct Fixture {
	key: moq_auth::Key,
	cluster: moq_relay::cluster::Cluster,
	auth: moq_relay::auth::Auth,
	_keys: tempfile::TempDir,
	_server: tokio::task::JoinHandle<()>,
}

impl Fixture {
	async fn new() -> Self {
		let keys = tempfile::tempdir().expect("tempdir");
		let key = moq_auth::Key::generate(moq_auth::Algorithm::HS256, None).expect("generate key");
		let key_path = keys.path().join("key.jwk");
		key.to_file(&key_path).expect("write key");

		let mut policy = moq_auth::serve::Policy::default();
		policy.keys = Some(moq_auth::serve::Keys::File(key_path));
		let server = moq_auth::serve::Server::new(policy).expect("auth server");
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind auth");
		let url = format!("http://{}/", listener.local_addr().unwrap()).parse().unwrap();
		let server = tokio::spawn(async move {
			let _ = server.serve(listener).await;
		});

		let mut config = moq_relay::auth::Config::default();
		config.url = Some(url);
		let auth = config
			.init("test", &moq_tokio::tls::Connect::default(), false)
			.expect("auth init");
		Self {
			key,
			cluster: memory::cluster(),
			auth,
			_keys: keys,
			_server: server,
		}
	}

	/// Sign a token rooted at [`ROOT`] with these grants.
	fn token(&self, publish: &str, subscribe: &str) -> String {
		let claims = moq_auth::Claims::default()
			.with_root(ROOT)
			.with_publish([publish.parse().expect("publish pattern")])
			.with_subscribe([subscribe.parse().expect("subscribe pattern")]);
		self.key.sign(&claims).expect("sign")
	}

	/// A client at [`ROOT`] presenting `token`, publishing and subscribing through one
	/// origin, over `version`.
	async fn client(&self, token: &str, version: &str) -> (moq_net::origin::Producer, moq_net::Session) {
		let origin = moq_tokio::origin::spawn();
		let url = format!("moql://relay.example.com/{ROOT}?jwt={token}").parse().unwrap();
		let client = moq_net::Client::new()
			.with_publisher(origin.consume())
			.with_subscriber(origin.clone())
			.with_url(url);
		let version = version.parse().expect("version");
		let session = memory::connect(client, version, &self.cluster, &self.auth)
			.await
			.expect("connect");
		(origin, session)
	}

	/// The endpoint `id`'s client, holding the grants `doc/concept/socket.md` lists: publish
	/// its own endpoint, subscribe to every endpoint.
	async fn endpoint(&self, id: &str, version: &str) -> (Endpoint, moq_net::origin::Producer, moq_net::Session) {
		let (origin, session) = self
			.client(&self.token(&format!(".socket/{id}"), ".socket/**"), version)
			.await;
		// The session is rooted at the prefix, so the endpoint's prefix is the root.
		let endpoint = Endpoint::new(&origin, "", id).expect("endpoint");
		(endpoint, origin, session)
	}
}

/// Dial `acceptor` from `dialer` in a `tcp` session while `acceptor` accepts, returning
/// both ends.
async fn connect_tcp(dialer: &Endpoint, acceptor: &Endpoint) -> (TcpStream, TcpStream) {
	let dial = dialer.dial_tcp(acceptor.id()).expect("dial");
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
	let dial = dialer.dial_track(acceptor.id(), None).expect("dial");
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

/// Read the next write from a `tcp` session.
async fn read(reader: &mut moq_net::group::Consumer) -> Vec<u8> {
	kio::clock::timeout(TIMEOUT, reader.read_frame())
		.await
		.expect("read timeout")
		.expect("frame")
		.expect("a frame")
		.payload
		.to_vec()
}

/// Write `payload` as one message of a `track` session: one group.
fn send(track: &moq_net::track::Producer, payload: &'static [u8]) {
	let mut group = track.append_group().expect("group");
	group.write_frame(None, payload).expect("frame");
	group.finish().expect("finish");
}

/// Read the next message of a `track` session.
async fn recv(reader: &mut moq_net::track::Ordered) -> Vec<u8> {
	kio::clock::timeout(TIMEOUT, async {
		let mut group = reader.next_group().await.expect("group").expect("a group");
		let frame = group.read_frame().await.expect("frame").expect("a frame");
		frame.payload.to_vec()
	})
	.await
	.expect("read timeout")
}

/// The prefix grants admit a dial and its accept in either protocol, and the session
/// carries data, over moq-lite-07 and across a moq-transport hop.
#[tokio::test]
async fn prefix_grants_admit_a_session() {
	for (dialer, acceptor) in [(LITE, LITE), (LITE, IETF), (IETF, LITE)] {
		let fixture = Fixture::new().await;
		let (alice, _alice_origin, _alice_session) = fixture.endpoint("alice", dialer).await;
		let (bob, _bob_origin, _bob_session) = fixture.endpoint("bob", acceptor).await;
		let (mut dialed, mut accepted) = connect_tcp(&alice, &bob).await;
		dialed.producer.write_frame(None, &b"ping"[..]).expect("write");
		assert_eq!(read(&mut accepted.consumer).await, b"ping", "{dialer} to {acceptor}");
		accepted.producer.write_frame(None, &b"pong"[..]).expect("write");
		assert_eq!(read(&mut dialed.consumer).await, b"pong", "{dialer} to {acceptor}");
		dialed.producer.finish().expect("finish");
		accepted.producer.finish().expect("finish");

		let (dialed, accepted) = connect_track(&alice, &bob).await;
		send(&dialed.producer, b"ping");
		assert_eq!(
			recv(&mut accepted.consumer.ordered()).await,
			b"ping",
			"{dialer} to {acceptor}"
		);
	}
}

/// Mallory may read every endpoint, so she can ask bob for a session in alice's name. Bob
/// sees a dial from alice, but alice never dialed it and resets his accept, so it never
/// connects and mallory reads nothing.
#[tokio::test]
async fn a_forged_dial_never_connects() {
	let fixture = Fixture::new().await;
	let (alice, _alice_origin, _alice_session) = fixture.endpoint("alice", LITE).await;
	let (bob, _bob_origin, _bob_session) = fixture.endpoint("bob", LITE).await;
	let (_mallory, mallory_origin, _mallory_session) = fixture.endpoint("mallory", LITE).await;
	// Alice keeps accepting, as a server would, which is how she resets the forged accept.
	let alice_accepts = async {
		let _ = alice.accept_tcp().await;
		panic!("alice heard of a dial");
	};

	let forge = async {
		let broadcast = mallory_origin
			.consume()
			.request(".socket/bob", None)
			.await
			.expect("bob's endpoint");
		let forged = broadcast
			.track("tcp/alice/0123456789abcdef0123456789abcdef")
			.expect("track")
			.subscribe(None);
		let incoming = bob.accept_tcp().await.expect("accept");
		assert_eq!(incoming.peer(), "alice");
		let (accepted, forged) = futures::join!(incoming.accept(), forged);
		assert!(accepted.is_err(), "bob connected to a forged dial");
		assert!(forged.is_err(), "mallory read bob's half");
	};
	kio::clock::timeout(TIMEOUT, async {
		futures::select_biased! {
			() = futures::FutureExt::fuse(Box::pin(forge)) => {}
			() = futures::FutureExt::fuse(Box::pin(alice_accepts)) => {}
		}
	})
	.await
	.expect("forge timeout");
}

/// A reader allowed every endpoint lists both ends of a running session and finds no
/// track, so it cannot learn a session's name to read or forge it.
#[tokio::test]
async fn a_reader_of_everything_lists_no_session() {
	let fixture = Fixture::new().await;
	let (alice, _alice_origin, _alice_session) = fixture.endpoint("alice", LITE).await;
	let (bob, _bob_origin, _bob_session) = fixture.endpoint("bob", LITE).await;
	let (mut dialed, mut accepted) = connect_tcp(&alice, &bob).await;
	dialed.producer.write_frame(None, &b"secret"[..]).expect("write");
	assert_eq!(read(&mut accepted.consumer).await, b"secret");
	let (track_dialed, track_accepted) = connect_track(&alice, &bob).await;
	send(&track_dialed.producer, b"secret");
	assert_eq!(recv(&mut track_accepted.consumer.ordered()).await, b"secret");

	let (eve_origin, _eve_session) = fixture.client(&fixture.token(".socket/eve", "**"), LITE).await;
	for id in ["alice", "bob"] {
		let broadcast = kio::clock::timeout(TIMEOUT, eve_origin.consume().request(format!(".socket/{id}"), None))
			.await
			.expect("request timeout")
			.expect("the endpoint");
		let mut tracks = broadcast.tracks().expect("tracks");
		let event = kio::clock::timeout(TIMEOUT, tracks.next())
			.await
			.expect("listing timeout")
			.expect("listing");
		assert!(matches!(event, Some(TracksEvent::Synced)), "{id} listed {event:?}");
		assert!(
			kio::clock::timeout(SETTLE, tracks.next()).await.is_err(),
			"{id} listed a track"
		);
	}
}
