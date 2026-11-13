//! Sockets through a relay that admits with `moq-auth` tokens, verified by the reference
//! auth server, granting only the prefixes `doc/concept/socket.md` lists.
//!
//! Every token is rooted at the socket prefix `room`. A server's token publishes its own
//! endpoint, `.server/<id>`, and subscribes to every client, `.client/**`; a client's token
//! publishes `.client/<id>` and subscribes to every server, `.server/**`. So a client can't
//! pose as a server or read another client, and the session track names are what keep
//! third parties out of a server's sessions: a forged dial never connects, and no listing
//! gives a session's name away.

use std::time::Duration;

use moq_net::broadcast::TracksEvent;
use moq_net::socket::{Endpoint, TcpStream, TrackStream};
use moq_net::transport::memory;
use moq_relay::{Accepted, Connection};

const TIMEOUT: Duration = Duration::from_secs(10);

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
			cluster: crate::spawn_cluster(Default::default()),
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
		let session = self.connect(client, version).await.expect("connect");
		(origin, session)
	}

	/// Dial the relay over a fresh in-memory link, as `client` speaking `version`.
	///
	/// On Tokio rather than kio's simulator, which `crate::memory` uses, since the
	/// reference auth server is a real HTTP server.
	async fn connect(
		&self,
		client: moq_net::Client,
		version: moq_net::Version,
	) -> Result<moq_net::Session, moq_net::Error> {
		let mut config = memory::Config::default();
		config.protocol = Some(version.alpn().to_string());
		let (client_link, relay_link) = memory::pair(config);

		let (cluster, auth) = (self.cluster.clone(), self.auth.clone());
		tokio::spawn(async move {
			let server = moq_net::Server::new().with_versions(version.into());
			let Ok(handshake) = server.accept_request(moq_net::time::Instant::now(), relay_link).await else {
				return;
			};
			let request = Accepted::new(handshake, moq_auth::Transport::Quic);
			Connection::new(request, cluster, auth).run().await;
		});

		let (session, driver) = client
			.with_versions(version.into())
			.connect(moq_net::time::Instant::now(), client_link)
			.await?;
		tokio::spawn(moq_net::time::run(driver));
		Ok(session)
	}

	/// The endpoint `id` in `role`, holding the grants `doc/concept/socket.md` lists for it:
	/// publish its own endpoint, subscribe to every endpoint of the other role.
	async fn endpoint(
		&self,
		role: Role,
		id: &str,
		version: &str,
	) -> (Endpoint, moq_net::origin::Producer, moq_net::Session) {
		let (own, peers) = match role {
			Role::Server => (".server", ".client"),
			Role::Client => (".client", ".server"),
		};
		let (origin, session) = self
			.client(&self.token(&format!("{own}/{id}"), &format!("{peers}/**")), version)
			.await;
		// The session is rooted at the prefix, so the endpoint's prefix is the root.
		let endpoint = Endpoint::new(&origin, "", id).expect("endpoint");
		(endpoint, origin, session)
	}
}

/// Which grants an endpoint's token holds.
#[derive(Clone, Copy)]
enum Role {
	Server,
	Client,
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
	let mut group = track
		.append_group(track.largest().map_or(0, |largest| largest.group + 1))
		.expect("group");
	group.write_frame(None, payload).expect("frame");
	group.close().expect("finish");
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

/// Wait until `id`, a hidden endpoint, is announced to `origin`, so a request for it
/// never syncs ahead of the announce and reports it unroutable.
async fn announced(origin: &moq_net::origin::Producer, id: &str) {
	let mut announced = origin.consume().with_hidden(true).announced();
	let wait = async {
		while let Some(event) = announced.next().await {
			if let moq_net::announce::Event::Present(announce) = event
				&& announce.prefix.as_str() == id
			{
				return;
			}
		}
		panic!("{id}: origin closed");
	};
	kio::clock::timeout(TIMEOUT, wait)
		.await
		.unwrap_or_else(|_| panic!("{id} was never announced"));
}

/// The prefix grants admit a dial and its accept in either protocol, and the session
/// carries data, over moq-lite-07 and across a moq-transport hop.
#[tokio::test]
async fn prefix_grants_admit_a_session() {
	for (dialer, acceptor) in [(LITE, LITE), (LITE, IETF), (IETF, LITE)] {
		let fixture = Fixture::new().await;
		let (alice, _alice_origin, _alice_session) = fixture.endpoint(Role::Client, "alice", dialer).await;
		let (bob, _bob_origin, _bob_session) = fixture.endpoint(Role::Server, "bob", acceptor).await;
		let (mut dialed, mut accepted) = connect_tcp(&alice, &bob).await;
		dialed.producer.write_frame(None, &b"ping"[..]).expect("write");
		assert_eq!(read(&mut accepted.consumer).await, b"ping", "{dialer} to {acceptor}");
		accepted.producer.write_frame(None, &b"pong"[..]).expect("write");
		assert_eq!(read(&mut dialed.consumer).await, b"pong", "{dialer} to {acceptor}");
		dialed.producer.close().expect("finish");
		accepted.producer.close().expect("finish");

		let (dialed, accepted) = connect_track(&alice, &bob).await;
		send(&dialed.producer, b"ping");
		assert_eq!(
			recv(&mut accepted.consumer.ordered()).await,
			b"ping",
			"{dialer} to {acceptor}"
		);
	}
}

/// Mallory, a client, may dial every server, so she can ask bob for a session in alice's
/// name. Bob sees a dial from alice, but alice never dialed it and resets his accept, so it
/// never connects and mallory reads nothing.
#[tokio::test]
async fn a_forged_dial_never_connects() {
	let fixture = Fixture::new().await;
	let (alice, _alice_origin, _alice_session) = fixture.endpoint(Role::Client, "alice", LITE).await;
	let (bob, _bob_origin, _bob_session) = fixture.endpoint(Role::Server, "bob", LITE).await;
	let (_mallory, mallory_origin, _mallory_session) = fixture.endpoint(Role::Client, "mallory", LITE).await;
	// Alice dials elsewhere, so her client endpoint is there to reset the forged accept.
	let _elsewhere = alice.dial_tcp("carol").expect("dial");

	// Bob's first accept announces his server endpoint.
	assert!(bob.poll_accept_tcp(&kio::Waiter::noop()).is_pending());
	announced(&mallory_origin, ".server/bob").await;

	let forge = async {
		let broadcast = mallory_origin
			.consume()
			.request(".server/bob", None)
			.await
			.expect("bob's endpoint");
		let forged = broadcast
			.track("tcp/alice/bob/0123456789abcdef0123456789abcdef")
			.expect("track")
			.subscribe(None);
		let incoming = bob.accept_tcp().await.expect("accept");
		assert_eq!(incoming.peer(), "alice");
		let (accepted, forged) = futures::join!(incoming.accept(), forged);
		assert!(accepted.is_err(), "bob connected to a forged dial");
		assert!(forged.is_err(), "mallory read bob's half");
	};
	kio::clock::timeout(TIMEOUT, forge).await.expect("forge timeout");
}

/// A client's grant keeps it a client: an accept announces a server endpoint the relay
/// never carries, so no dial reaches it, and another client's endpoint is out of reach.
///
/// Each absence is read by a session that connects once the relay holds everything it
/// could have carried, so its first sync with the relay would have listed it.
#[tokio::test]
async fn a_client_grant_neither_serves_nor_reads_clients() {
	let fixture = Fixture::new().await;
	let (alice, _alice_origin, _alice_session) = fixture.endpoint(Role::Client, "alice", LITE).await;
	let (_bob, bob_origin, _bob_session) = fixture.endpoint(Role::Server, "bob", LITE).await;
	// Bob, a server, reads a client endpoint once the relay carries it.
	let carried = |id: &'static str| {
		let origin = bob_origin.clone();
		async move {
			announced(&origin, id).await;
			kio::clock::timeout(TIMEOUT, origin.consume().request(id, None))
				.await
				.expect("request timeout")
				.unwrap_or_else(|err| panic!("{id}: {err}"));
		}
	};

	// Alice dials, which announces her client endpoint.
	let _alice_dial = alice.dial_tcp("carol").expect("dial");
	carried(".client/alice").await;

	// Mallory, a client, can't read it.
	let (mallory, mallory_origin, _mallory_session) = fixture.endpoint(Role::Client, "mallory", LITE).await;
	let read = kio::clock::timeout(TIMEOUT, mallory_origin.consume().request(".client/alice", None))
		.await
		.expect("request timeout");
	assert!(
		matches!(read, Err(moq_net::Error::Unroutable)),
		"mallory read alice's endpoint: {:?}",
		read.err()
	);

	// Mallory accepts as a server would, then dials, announcing her client endpoint after
	// her server one on the same session: once it is carried, the relay decided both.
	assert!(mallory.poll_accept_tcp(&kio::Waiter::noop()).is_pending());
	let _mallory_dial = mallory.dial_tcp("bob").expect("dial");
	carried(".client/mallory").await;

	// So no dial can reach her.
	let (_dave, dave_origin, _dave_session) = fixture.endpoint(Role::Client, "dave", LITE).await;
	let served = kio::clock::timeout(TIMEOUT, dave_origin.consume().request(".server/mallory", None))
		.await
		.expect("request timeout");
	assert!(
		matches!(served, Err(moq_net::Error::Unroutable)),
		"the relay carried mallory's server endpoint: {:?}",
		served.err()
	);
}

/// A reader allowed every endpoint lists both ends of a running session and finds no
/// track, so it cannot learn a session's name to read or forge it.
#[tokio::test]
async fn a_reader_of_everything_lists_no_session() {
	let fixture = Fixture::new().await;
	let (alice, _alice_origin, _alice_session) = fixture.endpoint(Role::Client, "alice", LITE).await;
	let (bob, _bob_origin, _bob_session) = fixture.endpoint(Role::Server, "bob", LITE).await;
	let (mut dialed, mut accepted) = connect_tcp(&alice, &bob).await;
	dialed.producer.write_frame(None, &b"secret"[..]).expect("write");
	assert_eq!(read(&mut accepted.consumer).await, b"secret");
	let (track_dialed, track_accepted) = connect_track(&alice, &bob).await;
	send(&track_dialed.producer, b"secret");
	assert_eq!(recv(&mut track_accepted.consumer.ordered()).await, b"secret");

	let (eve_origin, _eve_session) = fixture.client(&fixture.token(".client/eve", "**"), LITE).await;
	for id in [".client/alice", ".server/bob"] {
		let broadcast = kio::clock::timeout(TIMEOUT, eve_origin.consume().request(id, None))
			.await
			.expect("request timeout")
			.expect("the endpoint");
		let mut tracks = broadcast.tracks().expect("tracks");
		let event = kio::clock::timeout(TIMEOUT, tracks.next())
			.await
			.expect("listing timeout")
			.expect("listing");
		// The session tracks existed before this listing began, so a `Synced` first
		// means it listed none of them.
		assert!(matches!(event, Some(TracksEvent::Synced)), "{id} listed {event:?}");
	}
}
