//! Sockets through a relay that admits with `moq-auth` tokens, verified by the reference
//! auth server, granting only the prefixes `doc/concept/socket.md` lists.
//!
//! The listener's token, rooted at the listener's path `room`, subscribes to `.dial/**`
//! and publishes `.accept/**`. A dialer's token publishes `.dial/<id>` and subscribes to
//! `.accept/<id>`, so it can neither dial as another id nor read another id's answer.

use std::time::Duration;

use moq_net::socket::{self, Closed, Listener, Socket};

use crate::memory;

const TIMEOUT: Duration = Duration::from_secs(10);

/// Long enough for a refused route to have arrived had it been allowed. The relay links
/// are in memory, so this is many round trips.
const SETTLE: Duration = Duration::from_millis(500);

/// The listener's path, and every token's root.
const ROOT: &str = "room";

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
	/// origin, over moq-lite-07.
	async fn client(&self, token: &str) -> (moq_net::origin::Producer, moq_net::Session) {
		let origin = moq_tokio::origin::spawn();
		let url = format!("moql://relay.example.com/{ROOT}?jwt={token}").parse().unwrap();
		let client = moq_net::Client::new()
			.with_publisher(origin.consume())
			.with_subscriber(origin.clone())
			.with_url(url);
		let version = "moq-lite-07-wip".parse().expect("lite-07");
		let session = memory::connect(client, version, &self.cluster, &self.auth)
			.await
			.expect("connect");
		(origin, session)
	}

	async fn listener(&self) -> (moq_net::origin::Producer, moq_net::Session) {
		self.client(&self.token(".accept/**", ".dial/**")).await
	}

	async fn dialer(&self, id: &str) -> (moq_net::origin::Producer, moq_net::Session) {
		let token = self.token(&format!(".dial/{id}"), &format!(".accept/{id}"));
		self.client(&token).await
	}
}

/// Dial as `id` from `origin` while `listener` accepts, returning both ends.
async fn connect(origin: &moq_net::origin::Producer, listener: &mut Listener, id: &str) -> (Socket, Socket) {
	// The session is rooted at the listener's path, so the socket's path is the root.
	let dial = socket::dial(origin, "", id).expect("dial");
	let (dialed, accepted) = kio::clock::timeout(TIMEOUT, async { futures::join!(dial, listener.accept()) })
		.await
		.expect("connect timeout");
	(dialed.expect("dial"), accepted.expect("accept"))
}

/// Write `payload` as one group of `track`.
fn send(track: &moq_net::track::Producer, payload: &'static [u8]) {
	let mut group = track.append_group().expect("group");
	group.write_frame(None, payload).expect("frame");
	group.finish().expect("finish");
}

/// Subscribe to `track` and read its first frame.
async fn recv(track: &moq_net::track::Consumer) -> Vec<u8> {
	kio::clock::timeout(TIMEOUT, async {
		let mut subscriber = track.subscribe(None).await.expect("subscribe");
		let mut group = subscriber.recv_group().await.expect("group").expect("a group");
		let frame = group.read_frame().await.expect("frame").expect("a frame");
		frame.payload.to_vec()
	})
	.await
	.expect("read timeout")
}

/// The prefix grants admit a dial and its accept, and a bi stream carries data both
/// ways. A dialer holding another id's token can neither dial as that id, which would
/// replace its socket, nor read that id's answer.
#[tokio::test]
async fn prefix_grants_scope_sockets_to_their_id() {
	let fixture = Fixture::new().await;
	let (listening, _listener_session) = fixture.listener().await;
	let mut listener = Listener::new(&listening, "").expect("listen");

	let (alice_origin, _alice_session) = fixture.dialer("alice").await;
	let (alice, accepted) = connect(&alice_origin, &mut listener, "alice").await;
	assert_eq!(accepted.id(), "alice");

	let open = alice.open_bi(None).expect("open");
	let (opened, answered) = kio::clock::timeout(TIMEOUT, async { futures::join!(open, accepted.accept_bi()) })
		.await
		.expect("bi timeout");
	let opened = opened.expect("open");
	let answered = answered.expect("accept").expect("a stream");
	send(&opened.producer, b"ping");
	assert_eq!(recv(&answered.consumer).await, b"ping");
	send(&answered.producer, b"pong");
	assert_eq!(recv(&opened.consumer).await, b"pong");

	// Mallory's token works for mallory's own socket.
	let (mallory_origin, _mallory_session) = fixture.dialer("mallory").await;
	let (_mallory, mallory_accepted) = connect(&mallory_origin, &mut listener, "mallory").await;
	assert_eq!(mallory_accepted.id(), "mallory");

	// It cannot read alice's answer.
	let mut following = mallory_origin
		.consume()
		.follow(".accept/alice", None)
		.expect("follow locally");
	let read = kio::clock::timeout(SETTLE, following.available()).await;
	assert!(!matches!(read, Ok(Ok(_))), "mallory read alice's answer");

	// Nor dial as alice. The dial is announced as it is made, so the listener would hear
	// of it without the dial being polled, and would end alice's socket as replaced.
	let dial = socket::dial(&mallory_origin, "", "alice").expect("dial locally");
	assert!(
		kio::clock::timeout(SETTLE, listener.accept()).await.is_err(),
		"the listener heard mallory dial as alice"
	);
	assert!(
		kio::clock::timeout(SETTLE, dial).await.is_err(),
		"mallory's dial as alice resolved"
	);
	for socket in [&alice, &accepted] {
		let closed: Option<Closed> = kio::clock::timeout(SETTLE, socket.closed()).await.ok();
		assert_eq!(closed, None, "alice's socket ended");
	}
}
