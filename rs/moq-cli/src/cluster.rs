//! LAN clustering: find MoQ peers with mDNS and mesh with them.
//!
//! [`Lan`] advertises this process's `--listen` listener over mDNS, dials
//! every peer it discovers, and attaches each session to the shared origin in
//! both directions, so the whole network converges on one set of broadcasts
//! with no relay involved. Loop prevention comes from each broadcast's route,
//! like a relay cluster.
//!
//! Discovery and the membership proofs live in [`moq_native::mdns`] and the
//! dials are plain [`moq_native::Connection`] loops, so this module is only the
//! policy: who to dial, and who is allowed in.

use std::collections::HashMap;

use anyhow::Context;
use hang::moq_net;
use moq_native::mdns;
use url::Url;

/// The request path prefix a mesh dial presents, marking it as a peer rather
/// than an ordinary publisher or viewer on the same listener.
const MESH_PATH: &str = "/.cluster";

/// Whether a protocol version carries the request path used to mark a mesh dial.
fn carries_request_path(version: &moq_net::Version) -> bool {
	!version.is_lite() || version.code() >= 0xff0dad05
}

/// Reject version restrictions that leave the mesh with no request path.
pub(crate) fn validate_versions(
	client: &moq_native::connect::Config,
	server: &moq_native::listen::Config,
) -> anyhow::Result<()> {
	let client = client.versions();
	let server = server.versions();
	anyhow::ensure!(
		client
			.iter()
			.any(|version| carries_request_path(version) && server.contains(version)),
		"--cluster-lan needs --connect-version and --listen-version to share a version that carries a request path (moq-lite-05 or any moq-transport version)"
	);
	Ok(())
}

/// The LAN mesh policy: dial every discovered peer, accept the ones that prove
/// membership.
///
/// Cheap to clone; the inbound accept path and the outbound dial loop share one.
#[derive(Clone)]
pub struct Lan {
	/// Published to and ingested from every peer, so all of them converge.
	origin: moq_net::origin::Producer,

	/// The per-advertisement credential an inbound peer must present.
	/// Without a secret this is public and prevents path collisions, not access.
	credential: String,

	/// The dial template, per-peer fingerprint applied on top.
	client: moq_native::connect::Config,
}

/// The discovered details needed to open one LAN dial.
struct DialTarget {
	urls: Vec<Url>,
	has_node: bool,
	fingerprint: Option<String>,
	credential: String,
}

impl From<&mdns::Peer> for DialTarget {
	fn from(peer: &mdns::Peer) -> Self {
		Self {
			urls: peer.urls(),
			has_node: peer.node.is_some(),
			fingerprint: peer.fingerprint.clone(),
			credential: peer.credential.clone(),
		}
	}
}

impl Lan {
	/// Advertise `server` on the LAN and start browsing for peers.
	///
	/// Binds nothing itself: the listener is the one `--listen` already
	/// configured, so peers and ordinary clients share a port and a certificate.
	/// Returns the live [`mdns::Discovery`] alongside, so a bind or mDNS failure
	/// surfaces before readiness is signaled and [`run`](Self::run) can consume it.
	pub async fn start(
		args: &Args,
		origin: moq_net::origin::Producer,
		server: &moq_native::Server,
		mut client: moq_native::connect::Config,
	) -> anyhow::Result<(Self, mdns::Discovery)> {
		let port = server
			.local_addr()
			.context("--cluster-lan needs a QUIC listener")?
			.port();

		let mut config = mdns::Config::new(port);
		// Peers pin this instead of validating against a certificate authority,
		// which is what lets a generated certificate work with no setup at all.
		if let Some(fingerprint) = server.certificates().fingerprints().into_iter().next() {
			config = config.with_fingerprint(fingerprint);
		}
		if let Some(secret) = args.secret()? {
			config = config.with_secret(secret);
		}

		// A peer that is still advertising is still wanted, however long it has
		// been unreachable; mDNS expiry is what ends a dial, not a retry budget.
		client.backoff.timeout = Some(std::time::Duration::ZERO);
		// Whatever `--connect-once` means for the relay dial, a mesh dial has to keep
		// redialing: the map is keyed on the advertisement, so a one-shot handle that
		// stopped after the first session would sit there dead until mDNS expired the
		// peer and re-reported it.
		client.once = Some(false);
		// Each peer gets its own client, so they cannot all share one fixed port.
		// Keep the configured interface and let the OS pick the port, otherwise a
		// nonzero `--connect-bind` means the second peer (or the first, when
		// `--connect` already owns it) fails with EADDRINUSE.
		let mut bind = client.resolved_bind();
		bind.set_port(0);
		client.bind = Some(bind);
		// The membership proof rides in the request path. Legacy moq-lite versions
		// ignore that path, so they cannot be offered by a mesh dial even when the
		// same client config also serves an ordinary relay connection.
		client.version = client
			.versions()
			.iter()
			.filter(|version| carries_request_path(version))
			.copied()
			.collect();

		let discovery = config.advertise().await?;
		let credential = discovery.credential().to_string();
		Ok((
			Self {
				origin,
				credential,
				client,
			},
			discovery,
		))
	}

	/// Whether `request` is a mesh dial rather than an ordinary client.
	pub fn is_peer(&self, request: &moq_native::Request) -> bool {
		self.authorized(request.path())
	}

	/// Authorize an inbound mesh dial and attach it to the origin in both
	/// directions, returning once the session closes.
	pub async fn accept(&self, request: moq_native::Request) -> anyhow::Result<()> {
		if !self.authorized(request.path()) {
			request.close(403).await.ok();
			anyhow::bail!("LAN peer did not present this listener's membership proof");
		}

		let session = request
			.with_publisher(&self.origin)
			.with_subscriber(self.origin.clone())
			.ok()
			.await?;
		tracing::info!("accepted LAN peer");
		Err(session.closed().await.into())
	}

	/// Whether an inbound dial presented this advertisement's credential.
	fn authorized(&self, path: &str) -> bool {
		match split_credential(path) {
			(MESH_PATH, Some(presented)) => mdns::ct_eq(&self.credential, presented),
			_ => false,
		}
	}

	/// Discover peers and keep one session alive to each until discovery stops.
	///
	/// Both sides of a pair see each other and a MoQ session is bidirectional, so
	/// only the lower identity dials ([`mdns::Discovery::should_dial`]); the other
	/// waits in [`accept`](Self::accept).
	pub async fn run(self, mut discovery: mdns::Discovery) -> anyhow::Result<()> {
		// Each entry is a reconnect loop; dropping it closes the session and stops
		// retrying, so the map is the whole lifecycle.
		let mut dials: HashMap<String, Dial> = HashMap::new();

		while let Some(event) = discovery.recv().await {
			match event {
				mdns::Event::Found(peer) => {
					if !discovery.should_dial(&peer.id) {
						continue;
					}
					match dials.get(&peer.id) {
						// A periodic re-resolve with the same details; the dial stands.
						Some(dial) if dial.peer == peer => continue,
						// New addresses, port, or certificate. The old loop would retry
						// stale details forever, so replace it.
						Some(_) => tracing::info!(peer = %peer.id, "LAN peer re-advertised; redialing"),
						None => tracing::info!(peer = %peer.id, "discovered LAN peer; dialing"),
					}
					match self.dial(&peer) {
						Ok(connection) => {
							dials.insert(peer.id.clone(), Dial { connection, peer });
						}
						Err(err) => {
							// The advertisement the old loop was started from is gone, so
							// leaving it in place would retry a stale address forever.
							dials.remove(&peer.id);
							tracing::warn!(%err, peer = %peer.id, "failed to dial LAN peer");
						}
					}
				}
				mdns::Event::Lost(id) => {
					if dials.remove(&id).is_some() {
						tracing::info!(peer = %id, "LAN peer expired; dropping dial");
					}
				}
				_ => continue,
			}
		}

		// Discovery only ends if the daemon or its channel died, which is a failure
		// rather than a finished job: `drive` returns on the first task to report
		// `Ok(())`, so returning success here would exit an otherwise healthy
		// process with status zero and take the media tasks down with it.
		anyhow::bail!("LAN discovery stopped unexpectedly");
	}

	/// Start a reconnecting session to one peer.
	fn dial(&self, peer: &mdns::Peer) -> anyhow::Result<moq_native::Connection> {
		self.dial_target(&peer.into())
	}

	/// Start a reconnecting session from the discovered details needed by a dial.
	fn dial_target(&self, peer: &DialTarget) -> anyhow::Result<moq_native::Connection> {
		// Every advertised address is a candidate: only the peer's own routing table
		// knows which of them reaches it from here. A peer that advertised nothing
		// reachable has nothing to dial, which `Addrs` makes us handle here rather
		// than inside a connection that would retry an empty list.
		let addrs = moq_native::Addrs::collect(self.dial_urls(peer)).context("peer advertised no reachable address")?;

		let mut config = self.client.clone();
		// A peer that advertised a fingerprint serves a generated certificate, so
		// the relay's CA roots and client certificate say nothing about it. Pinning
		// the advertised fingerprint is the whole trust decision, and combining it
		// with roots is rejected outright, so start from a clean TLS config.
		if let Some(fingerprint) = &peer.fingerprint {
			config.tls = moq_native::tls::Connect::default();
			config.tls.fingerprint = vec![fingerprint.clone()];
		}

		let client = config
			.init(Default::default())?
			.with_publisher(&self.origin)
			.with_subscriber(self.origin.clone());
		Ok(client.connect(addrs))
	}

	/// Every URL worth trying for `peer`, each carrying the membership proof.
	///
	/// The credential is [`mdns::Peer::credential`], which discovery derived from
	/// that peer's own advertisement. With a secret it authenticates to that
	/// listener and nowhere else; without one it is only a collision-proof marker.
	fn dial_urls(&self, peer: &DialTarget) -> Vec<Url> {
		peer.urls
			.iter()
			.cloned()
			.map(|mut url| {
				// A peer that advertised a canonical node URL is reachable by name
				// and brings its own path; only a bare LAN socket gets the marker.
				if !peer.has_node {
					url.set_path(&format!("{MESH_PATH}/{}", peer.credential));
				}
				url
			})
			.collect()
	}
}

/// Split a mesh request path into its marker prefix and the credential it carries.
fn split_credential(request: &str) -> (&str, Option<&str>) {
	let Some(rest) = request.strip_prefix(MESH_PATH) else {
		return (request, None);
	};
	match rest {
		"" => (MESH_PATH, None),
		// Only a path segment boundary counts, so `/.clusterish` is not the marker.
		_ => match rest.strip_prefix('/') {
			Some("") | None => (request, None),
			Some(credential) => (MESH_PATH, Some(credential)),
		},
	}
}

/// Accept inbound sessions, splitting mesh peers off from ordinary clients.
///
/// Both share one listener, so a peer and a viewer reach this process the same
/// way; only the request path tells them apart. Without a mesh,
/// [`moq_native::Server::serve_publish`] and its sibling already do the ordinary
/// half, so this exists only to interleave the two.
pub async fn serve(
	server: moq_native::Server,
	lan: Lan,
	origin: moq_net::origin::Producer,
	direction: crate::Direction,
	public: bool,
) -> anyhow::Result<()> {
	let mut server = server.listen().await.context("failed to bind listeners")?;
	let mut tasks = tokio::task::JoinSet::new();

	while let Some(request) = server.accept().await {
		// Reap before dispatching, so a listener that only ever sees mesh peers
		// still drains: their branch returns early.
		while tasks.try_join_next().is_some() {}

		if lan.is_peer(&request) {
			let lan = lan.clone();
			tasks.spawn(async move {
				if let Err(err) = lan.accept(request).await {
					tracing::warn!(%err, "LAN peer session ended");
				}
			});
			continue;
		}

		// The mesh's own listener is not a public one. `--cluster-lan` fills in
		// `--listen` when it is unset, so a user who asked only to mesh never
		// asked to serve viewers, and the membership proof gates the mesh path
		// alone. Attaching an unauthenticated stranger to the origin here would
		// hand them exactly what the secret is meant to withhold.
		if !public {
			tracing::debug!(path = %request.path(), "refusing a non-peer request on the LAN mesh listener");
			request.close(404).await.ok();
			continue;
		}

		let request = match direction {
			crate::Direction::Import => request.with_publisher(origin.consume()),
			crate::Direction::Export => request.with_subscriber(origin.clone()),
		};
		tasks.spawn(async move {
			let err = match request.ok().await {
				Ok(session) => session.closed().await.into(),
				Err(err) => err,
			};
			tracing::warn!(%err, "session ended with error");
		});
	}

	// Same reasoning as `Lan::run`: the accept loop only ends if the listener is
	// gone, and reporting that as success would exit the process cleanly. A
	// shutdown never gets here, since `drive` returns on the signal task instead.
	anyhow::bail!("the MoQ listener stopped accepting");
}

/// One peer's reconnect loop, plus the advertisement it was started from so a
/// refreshed advertisement can be told apart from a periodic re-resolve.
struct Dial {
	/// Dropped when the peer is lost or re-advertises, which closes the session.
	#[allow(dead_code, reason = "held to keep the reconnect loop alive")]
	connection: moq_native::Connection,
	peer: mdns::Peer,
}

/// The `--cluster-lan` flags.
#[derive(clap::Args, Clone, Default)]
pub struct Args {
	/// Discover and mesh with every other participating MoQ process on the LAN
	/// via mDNS: no relay, internet, or certificate setup needed. Reuses the
	/// --listen listener, defaulting it to an ephemeral port with a
	/// generated certificate. Composes with --connect, e.g. mesh locally
	/// while a relay serves external viewers.
	#[arg(
		id = "cluster-lan",
		long = "cluster-lan",
		env = "MOQ_CLUSTER_LAN",
		help_heading = "Cluster",
		default_missing_value = "true",
		num_args = 0..=1,
		require_equals = true,
	)]
	pub enabled: Option<bool>,

	/// Restrict the LAN mesh to peers holding this key, as 64 hexadecimal
	/// characters or a path containing them. Every peer needs the same value, and
	/// peers without it are mutually invisible.
	///
	/// Without it, anyone who can reach the listener joins, so leave it unset only
	/// on networks you trust. A missing file is an error, never generated.
	#[arg(
		id = "cluster-lan-secret",
		long = "cluster-lan-secret",
		env = "MOQ_CLUSTER_LAN_SECRET",
		help_heading = "Cluster",
		requires = "cluster-lan",
		value_name = "HEX_OR_PATH"
	)]
	pub secret: Option<String>,
}

impl Args {
	/// Whether `--cluster-lan` asked for the mesh.
	pub fn enabled(&self) -> bool {
		self.enabled.unwrap_or(false)
	}

	/// Reject a secret configured without the mesh that would read it.
	pub fn validate(&self) -> anyhow::Result<()> {
		anyhow::ensure!(
			self.secret.is_none() || self.enabled(),
			"--cluster-lan-secret requires --cluster-lan=true"
		);
		Ok(())
	}

	/// The configured key, read from the value itself or the file it names.
	pub fn secret(&self) -> anyhow::Result<Option<mdns::Secret>> {
		self.secret
			.as_deref()
			.map(mdns::Secret::load)
			.transpose()
			.context("invalid --cluster-lan-secret")
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
	const KEY: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

	#[test]
	fn split_credential_separates_the_marker_from_the_proof() {
		assert_eq!(split_credential("/.cluster"), ("/.cluster", None));
		assert_eq!(split_credential("/.cluster/"), ("/.cluster/", None));
		assert_eq!(split_credential("/.cluster/abc"), ("/.cluster", Some("abc")));
		assert_eq!(split_credential("/room"), ("/room", None));
		// A path that merely starts with the marker's letters is not the marker.
		assert_eq!(split_credential("/.clusterish/abc"), ("/.clusterish/abc", None));
	}

	fn lan(credential: &str) -> Lan {
		Lan {
			origin: moq_net::Origin::random().produce(),
			credential: credential.to_string(),
			client: moq_native::connect::Config::default(),
		}
	}

	/// Only this listener's per-advertisement credential gets in.
	#[test]
	fn authorized_requires_the_advertised_credential() {
		let closed = lan("expected-proof");
		assert!(closed.authorized("/.cluster/expected-proof"));
		assert!(!closed.authorized(MESH_PATH), "a missing proof must be rejected");
		assert!(!closed.authorized("/.cluster/other-proof"));
		assert!(!closed.authorized("/.cluster/expected-proof-plus"));
		assert!(!closed.authorized("/room"));
	}

	/// The dial presents the proof discovery derived from that peer's own
	/// advertisement, at every address the peer offered.
	#[test]
	fn dial_urls_carry_the_proof_to_every_address() {
		let lan = lan("ours");

		let socket = DialTarget {
			urls: ["moqt://192.168.1.5:4443", "moqt://127.0.0.1:4443"]
				.into_iter()
				.map(|url| url.parse().expect("valid url"))
				.collect(),
			has_node: false,
			fingerprint: Some("abcd".into()),
			credential: "theirs".into(),
		};
		let urls: Vec<String> = lan.dial_urls(&socket).into_iter().map(String::from).collect();
		assert_eq!(
			urls,
			[
				"moqt://192.168.1.5:4443/.cluster/theirs",
				"moqt://127.0.0.1:4443/.cluster/theirs",
			],
			"the peer's own proof, at each candidate, loopback last"
		);

		// A node URL is dialed exactly as advertised.
		let node = DialTarget {
			urls: vec!["https://relay.example.com/anon".parse().expect("valid url")],
			has_node: true,
			fingerprint: None,
			credential: "theirs".into(),
		};
		assert_eq!(lan.dial_urls(&node)[0].path(), "/anon");
	}

	/// A pinned peer ignores the CA roots configured for `--connect`:
	/// combining the two is rejected outright, and they say nothing about a
	/// generated certificate anyway.
	#[tokio::test]
	async fn pinning_a_peer_drops_the_relay_roots() {
		let mut client = moq_native::connect::Config::default();
		client.tls.root = vec!["ca.pem".into()];
		let mut lan = lan("ours");
		lan.client = client;

		let peer = DialTarget {
			urls: vec!["moqt://127.0.0.1:4443".parse().expect("valid url")],
			has_node: false,
			fingerprint: Some("00".repeat(32)),
			credential: "theirs".into(),
		};
		let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
		// Held rather than dropped: the dial is what's under test, and dropping the
		// connection would abort it before the assertion means anything.
		let _connection = lan
			.dial_target(&peer)
			.expect("a pinned dial must not inherit the CA roots");
	}

	#[test]
	fn secret_reads_hex_or_a_file() {
		let args = Args {
			enabled: Some(true),
			secret: Some(KEY.to_string()),
		};
		assert!(args.secret().expect("valid hex").is_some());
		assert!(args.validate().is_ok());

		let file = tempfile::NamedTempFile::new().expect("temp file");
		std::fs::write(file.path(), format!("{KEY}\n")).expect("write");
		let args = Args {
			enabled: Some(true),
			secret: Some(file.path().to_str().expect("utf-8").to_string()),
		};
		assert!(args.secret().expect("valid file").is_some());

		let args = Args {
			enabled: Some(true),
			secret: Some("definitely-missing-key".to_string()),
		};
		// The message has to cover both readings: a mistyped inline key looks
		// exactly like a path that isn't there.
		let err = format!("{:#}", args.secret().unwrap_err());
		assert!(err.contains("64 hexadecimal characters"), "{err}");
		assert!(err.contains("definitely-missing-key"), "{err}");

		// A secret nothing would read is an error, not a silently ignored flag.
		let args = Args {
			enabled: Some(false),
			secret: Some(KEY.to_string()),
		};
		assert!(args.validate().unwrap_err().to_string().contains("--cluster-lan=true"));
	}

	/// Bind a listener the way `--cluster-lan` does, returning it plus the peer
	/// record mDNS would have produced for it.
	fn listener() -> (moq_native::Server, DialTarget) {
		let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

		let mut config = moq_native::listen::Config::default();
		config.bind = Some("127.0.0.1:0".to_string());
		config.tls.generate = vec!["moq-cluster-lan".to_string()];
		let server = config.init(Default::default()).expect("failed to bind listener");

		let port = server.local_addr().expect("no local addr").port();
		let fingerprint = server
			.certificates()
			.fingerprints()
			.into_iter()
			.next()
			.expect("no fingerprint");
		let peer = DialTarget {
			urls: vec![format!("moqt://127.0.0.1:{port}").parse().expect("valid url")],
			has_node: false,
			fingerprint: Some(fingerprint),
			credential: "listener-proof".into(),
		};
		(server, peer)
	}

	/// One mesh session carries both directions: a broadcast published on either
	/// side's origin is announced on the other. Wires `serve` to `dial` directly,
	/// so the test needs no multicast and stays CI-safe.
	#[tokio::test]
	async fn session_shares_origin_bidirectionally() {
		let origin_a = moq_net::Origin::random().produce();
		let origin_b = moq_net::Origin::random().produce();

		// Published before the session exists; announcements flow once it connects.
		let _from_a = origin_a
			.create_broadcast("from-a", moq_net::broadcast::Route::new().with_announce(true))
			.expect("failed to create broadcast");

		let (server, peer) = listener();
		let mut accept = lan("listener-proof");
		accept.origin = origin_b.clone();
		// The mesh shares the listener with ordinary clients, so go through the
		// same dispatch `--cluster-lan` installs rather than calling `accept`.
		tokio::spawn(serve(server, accept, origin_b.clone(), crate::Direction::Import, false));

		let mut dialer = lan("dialer-proof");
		dialer.origin = origin_a.clone();
		let _dial = dialer.dial_target(&peer).expect("dial");

		let mut announced_on_b = origin_b.consume().announced();
		let update = tokio::time::timeout(TIMEOUT, announced_on_b.next())
			.await
			.expect("timed out waiting for announcement")
			.expect("origin closed");
		assert_eq!(update.path.as_str(), "from-a");

		// And the reverse direction over the same session. This stream replays a's
		// own "from-a" first, so read until the remote broadcast shows up.
		let _from_b = origin_b
			.create_broadcast("from-b", moq_net::broadcast::Route::new().with_announce(true))
			.expect("failed to create broadcast");
		let mut announced_on_a = origin_a.consume().announced();
		loop {
			let update = tokio::time::timeout(TIMEOUT, announced_on_a.next())
				.await
				.expect("timed out waiting for announcement")
				.expect("origin closed");
			if update.path.as_str() == "from-b" {
				break;
			}
		}
	}

	/// A dial that cannot produce this listener's proof is rejected before the
	/// origin is attached: reaching the listener is not membership.
	#[tokio::test]
	async fn mesh_rejects_a_dial_without_the_proof() {
		let origin_a = moq_net::Origin::random().produce();
		let origin_b = moq_net::Origin::random().produce();
		let _from_b = origin_b
			.create_broadcast("from-b", moq_net::broadcast::Route::new().with_announce(true))
			.expect("failed to create broadcast");

		let (server, peer) = listener();
		let mut accept = lan("the-real-proof");
		accept.origin = origin_b.clone();
		tokio::spawn(serve(server, accept, origin_b.clone(), crate::Direction::Import, false));

		// A peer that reached the port but never saw a verifiable advertisement.
		let mut dialer = lan("dialer-proof");
		dialer.origin = origin_a.clone();
		let _dial = dialer.dial_target(&peer).expect("dial");

		// Nothing is ever announced, because the session is closed at authorization.
		let mut announced_on_a = origin_a.consume().announced();
		let update = tokio::time::timeout(std::time::Duration::from_secs(2), announced_on_a.next()).await;
		assert!(update.is_err(), "an unauthorized peer must not share broadcasts");
	}

	/// A listener `--cluster-lan` invented is for the mesh, not for viewers.
	///
	/// The membership proof only gates the `/.cluster` path, so an ordinary path
	/// used to skip authorization entirely and be attached to the origin: someone
	/// who found the advertised ephemeral port could read an `import` or publish
	/// into an `export` without the secret, which is what the secret is for. A
	/// user who passed `--listen` did ask to serve, so that case still does.
	#[tokio::test]
	async fn a_mesh_only_listener_refuses_ordinary_clients() {
		let origin = moq_net::Origin::random().produce();
		let _published = origin
			.create_broadcast("secret-stream", moq_net::broadcast::Route::new().with_announce(true))
			.expect("failed to create broadcast");

		let (server, peer) = listener();
		let mut accept = lan("the-real-proof");
		accept.origin = origin.clone();
		// `public: false` is what `--cluster-lan` passes with no `--listen`.
		tokio::spawn(serve(server, accept, origin.clone(), crate::Direction::Import, false));

		// An ordinary client: no mesh marker, no proof, just the advertised port.
		let mut config = moq_native::connect::Config::default();
		config.tls = moq_native::tls::Connect::default();
		config.tls.fingerprint = vec![peer.fingerprint.clone().expect("fingerprint")];
		config.once = Some(true);
		let stolen = moq_net::Origin::random().produce();
		let url = peer.urls.into_iter().next().expect("an address");
		let connection = config
			.init(Default::default())
			.expect("client")
			.with_subscriber(stolen.clone())
			.connect(url);

		// The broadcast must never reach them.
		let mut announced = stolen.consume().announced();
		let update = tokio::time::timeout(std::time::Duration::from_secs(2), announced.next()).await;
		assert!(
			update.is_err(),
			"a mesh-only listener must not serve broadcasts to an unauthenticated client"
		);
		drop(connection);
	}
}
