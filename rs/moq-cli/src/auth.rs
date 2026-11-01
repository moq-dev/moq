//! `moq auth`: generate, sign, and verify the JWT tokens a relay authenticates with.
//!
//! The `moq-auth` library underneath stays free of CLI concerns.

use anyhow::Context;
use std::net::SocketAddr;
use std::{io, path::PathBuf};

use moq_auth::serve::{Keys, Policy};
use moq_auth::{Algorithm, Pattern, Permissions};

/// Generate, sign, and verify the JWT tokens a relay authenticates with.
#[derive(usage_rs::Args, Clone, Debug)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Args {
	#[usage(subcommand)]
	command: Command,
}

impl Args {
	/// Run the requested command, writing the key, token, or payload to the chosen
	/// destination (stdout by default). `serve` runs until SIGINT or SIGTERM.
	pub async fn run(self) -> anyhow::Result<()> {
		match self.command {
			// A container runs `moq` as PID 1, where a signal without a handler is
			// dropped. Only `serve` races one: `sign` and `verify` block on stdin, so a
			// handler would swallow the signal that used to end them. The handlers are
			// installed before `serve` logs that it is listening.
			Command::Serve(serve) => {
				let mut signals = crate::Signals::new()?;
				tokio::select! {
					() = signals.recv() => {}
					res = serve.run() => res?,
				}
			}
			Command::Generate {
				algorithm,
				id,
				out,
				out_dir,
				public,
				public_dir,
				root,
				publish,
				subscribe,
			} => {
				let id = match id {
					Some(id) => moq_auth::KeyId::decode(&id)?,
					None => moq_auth::KeyId::random(),
				};

				let mut key = moq_auth::Key::generate(algorithm, Some(id.clone()))?;
				if !publish.is_empty() || !subscribe.is_empty() {
					key = key.with_scope(moq_auth::Scope {
						root,
						publish: publish.into_iter().collect(),
						subscribe: subscribe.into_iter().collect(),
					})?;
				}

				let public_to_stdout = public.as_deref().is_some_and(is_dash);
				let private_to_stdout = out_dir.is_none() && out.as_deref().is_none_or(is_dash);
				if public_to_stdout && private_to_stdout {
					anyhow::bail!(
						"cannot write both keys to stdout; use --out/--public with a file path, or --out-dir/--public-dir"
					);
				}

				if let Some(dir) = public_dir {
					let path = dir.join(format!("{id}.jwk"));
					write_key(&key.to_public()?, &path)?;
				} else if let Some(path) = public {
					write_key(&key.to_public()?, &path)?;
				}

				if let Some(dir) = out_dir {
					let path = dir.join(format!("{id}.jwk"));
					write_key(&key, &path)?;
				} else if let Some(path) = out {
					write_key(&key, &path)?;
				} else {
					let encoded = key.to_str()?;
					println!("{encoded}");
				}
			}

			Command::Sign {
				key,
				root,
				publish,
				subscribe,
				expires,
				issued,
			} => {
				let key = read_key(&key)?;

				let payload = moq_auth::Claims::default()
					.with_root(root)
					.with_publish(publish)
					.with_subscribe(subscribe)
					.with_expires(expires.map(Into::into))
					.with_issued(issued.map(Into::into));

				let token = key.sign(&payload)?;
				println!("{token}");
			}

			Command::Verify { key, token } => {
				if is_dash(&key) && is_dash(&token) {
					anyhow::bail!("--key and --in cannot both read from stdin");
				}
				let key = read_key(&key)?;
				let token = read_token(&token)?;
				let payload = key.verify(&token)?;

				println!("{payload:#?}");
			}

			Command::Mesh(mesh) => mesh.run().await?,
			Command::Sessions(filter) => filter.get("/sessions").await?,
			Command::Revalidate(filter) => filter.post("/sessions/revalidate").await?,
		}

		Ok(())
	}
}

#[derive(usage_rs::Subcommands, Clone, Debug)]
enum Command {
	/// Generate a new signing key.
	///
	/// A random key ID is assigned unless --id is specified.
	/// Output is base64url-encoded JSON.
	Generate {
		/// The algorithm to use.
		#[usage(long, default = "HS256")]
		algorithm: Algorithm,

		/// The key ID. Randomly generated if not provided.
		#[usage(long)]
		id: Option<String>,

		/// Write the key to a file path. Use `-` for stdout.
		#[usage(long, value_hint = usage_rs::ValueHint::FilePath, extensions("jwk", "json"))]
		out: Option<PathBuf>,

		/// Write the key to a directory as {kid}.jwk.
		#[usage(long, conflicts = "--out", value_hint = usage_rs::ValueHint::DirPath)]
		out_dir: Option<PathBuf>,

		/// Write the public key to a file path (asymmetric algorithms only). Use `-` for stdout.
		#[usage(long, value_hint = usage_rs::ValueHint::FilePath, extensions("jwk", "json"))]
		public: Option<PathBuf>,

		/// Write the public key to a directory as {kid}.jwk (asymmetric algorithms only).
		#[usage(long, conflicts = "--public", value_hint = usage_rs::ValueHint::DirPath)]
		public_dir: Option<PathBuf>,

		/// Root path for the optional key scope. Only applied alongside --publish or --subscribe.
		#[usage(long, default = "")]
		root: String,

		/// Publish patterns the key may grant (repeatable); `foo/**` for a subtree.
		#[usage(long)]
		publish: Vec<Pattern>,

		/// Subscribe patterns the key may grant (repeatable); `foo/**` for a subtree.
		#[usage(long)]
		subscribe: Vec<Pattern>,
	},

	/// Sign a token, writing it to stdout.
	Sign {
		/// Path to the signing key file. Use `-` for stdin.
		#[usage(long, value_hint = usage_rs::ValueHint::FilePath, extensions("jwk", "json"))]
		key: PathBuf,

		/// The root path for the token.
		#[usage(long, default = "")]
		root: String,

		/// Patterns the user can publish (repeatable); `foo/**` for a subtree.
		#[usage(long)]
		publish: Vec<Pattern>,

		/// Patterns the user can subscribe to (repeatable); `foo/**` for a subtree.
		#[usage(long)]
		subscribe: Vec<Pattern>,

		/// Expiration time as a unix timestamp.
		#[usage(long)]
		expires: Option<UnixTimestamp>,

		/// Issued-at time as a unix timestamp.
		#[usage(long)]
		issued: Option<UnixTimestamp>,
	},

	/// Answer a relay's auth requests with keys, public rules, an mTLS grant, tiers, and session limits.
	Serve(Serve),

	/// Manage a mesh served by `moq auth serve --mesh-url`: enrollment keys and devices.
	Mesh(Mesh),

	/// List live sessions on a relay's internal listener.
	Sessions(SessionFilter),

	/// Ask a relay to re-check matching sessions now.
	Revalidate(SessionFilter),

	/// Verify a token, writing the payload to stdout.
	Verify {
		/// Path to the key file. Use `-` for stdin (requires `--in` to be a file).
		#[usage(long, value_hint = usage_rs::ValueHint::FilePath, extensions("jwk", "json"))]
		key: PathBuf,

		/// Path to read the token from. Use `-` for stdin.
		#[usage(long = "in", default = "-", value_hint = usage_rs::ValueHint::FilePath)]
		token: PathBuf,
	},
}

/// Filter flags shared by `moq auth sessions` and `moq auth revalidate`.
#[derive(usage_rs::Args, Clone, Debug)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct SessionFilter {
	/// The relay's internal listener, e.g. `http://127.0.0.1:9101`.
	#[usage(long)]
	internal_url: url::Url,

	/// Session id.
	#[usage(long)]
	id: Option<String>,

	/// Pattern matched against the dialed path, e.g. `demo/**`.
	#[usage(long)]
	path: Option<String>,

	/// Peer IP or CIDR; the port is dropped.
	#[usage(long)]
	remote: Option<String>,

	/// How the session reached the relay: `quic`, `websocket`, `tcp`, ...
	#[usage(long)]
	transport: Option<String>,

	/// The relay's node name.
	#[usage(long)]
	node: Option<String>,

	/// The relay's local socket, exact including port.
	#[usage(long)]
	local: Option<String>,

	/// SNI or the host the client addressed.
	#[usage(long)]
	server_name: Option<String>,

	/// Negotiated application protocol.
	#[usage(long)]
	alpn: Option<String>,

	/// SETUP role: `publisher` or `subscriber`.
	#[usage(long)]
	role: Option<String>,

	/// Certificate SAN/CN/fingerprint.
	#[usage(long)]
	tls_name: Option<String>,

	/// Certificate SHA-256, hex.
	#[usage(long)]
	tls_fingerprint: Option<String>,

	/// Certificate issuer DN.
	#[usage(long)]
	tls_issuer: Option<String>,
}

impl SessionFilter {
	fn query(&self) -> Vec<(&'static str, &str)> {
		let mut query = Vec::new();
		if let Some(value) = &self.id {
			query.push(("id", value.as_str()));
		}
		if let Some(value) = &self.path {
			query.push(("path", value.as_str()));
		}
		if let Some(value) = &self.remote {
			query.push(("remote", value.as_str()));
		}
		if let Some(value) = &self.transport {
			query.push(("transport", value.as_str()));
		}
		if let Some(value) = &self.node {
			query.push(("node", value.as_str()));
		}
		if let Some(value) = &self.local {
			query.push(("local", value.as_str()));
		}
		if let Some(value) = &self.server_name {
			query.push(("server_name", value.as_str()));
		}
		if let Some(value) = &self.alpn {
			query.push(("alpn", value.as_str()));
		}
		if let Some(value) = &self.role {
			query.push(("role", value.as_str()));
		}
		if let Some(value) = &self.tls_name {
			query.push(("tls.name", value.as_str()));
		}
		if let Some(value) = &self.tls_fingerprint {
			query.push(("tls.fingerprint", value.as_str()));
		}
		if let Some(value) = &self.tls_issuer {
			query.push(("tls.issuer", value.as_str()));
		}
		query
	}

	fn url(&self, path: &str) -> url::Url {
		let mut url = self.internal_url.clone();
		let base = url.path().trim_end_matches('/');
		url.set_path(&format!("{base}{path}"));
		let query = self.query();
		if !query.is_empty() {
			url.set_query(Some(
				&url::form_urlencoded::Serializer::new(String::new())
					.extend_pairs(query)
					.finish(),
			));
		}
		url
	}

	async fn get(&self, path: &str) -> anyhow::Result<()> {
		self.send(reqwest::Method::GET, path).await
	}

	async fn post(&self, path: &str) -> anyhow::Result<()> {
		self.send(reqwest::Method::POST, path).await
	}

	async fn send(&self, method: reqwest::Method, path: &str) -> anyhow::Result<()> {
		let response = reqwest::Client::new()
			.request(method, self.url(path))
			.send()
			.await
			.with_context(|| format!("failed to reach {}", self.internal_url))?;
		let status = response.status();
		let body = response.text().await.context("failed to read the reply")?;
		if !status.is_success() {
			anyhow::bail!("{status}: {body}");
		}
		match serde_json::from_str::<serde_json::Value>(&body) {
			Ok(value) => println!("{}", serde_json::to_string_pretty(&value)?),
			Err(_) => println!("{body}"),
		}
		Ok(())
	}
}

/// The reference auth server: the policy a relay used to hold, behind `--auth-url`.
#[derive(usage_rs::Args, Clone, Debug)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Serve {
	/// Where to answer: a socket address, or `unix:<path>` for a unix socket.
	#[usage(long, default = "127.0.0.1:4440")]
	listen: String,

	/// Allow a bind on a non-loopback address. The server has no authentication of its own.
	#[usage(long)]
	listen_public: bool,

	/// The key file a `jwt` is verified against.
	#[usage(long, conflicts("--key-dir", "--key-set"), value_hint = usage_rs::ValueHint::FilePath, extensions("jwk", "json"))]
	key: Option<PathBuf>,

	/// A directory of `{kid}.jwk` files, selected by the token's `kid`.
	#[usage(long, conflicts("--key", "--key-set"), value_hint = usage_rs::ValueHint::DirPath)]
	key_dir: Option<PathBuf>,

	/// A JWK Set file, selecting one verification key by the token's `kid`.
	#[usage(long, conflicts("--key", "--key-dir"), value_hint = usage_rs::ValueHint::FilePath, extensions("json", "jwks"))]
	key_set: Option<PathBuf>,

	/// Patterns an anonymous session may publish, rooted at `/` (repeatable); `foo/**` for a subtree.
	#[usage(long)]
	public_publish: Vec<Pattern>,

	/// Patterns an anonymous session may subscribe to, rooted at `/` (repeatable).
	#[usage(long)]
	public_subscribe: Vec<Pattern>,

	/// Patterns a session with a verified certificate may publish, rooted at `/` (repeatable). Empty refuses certificates.
	#[usage(long)]
	mtls_publish: Vec<Pattern>,

	/// Patterns a session with a verified certificate may subscribe to, rooted at `/` (repeatable).
	#[usage(long)]
	mtls_subscribe: Vec<Pattern>,

	/// The role verified certificates have on cluster links: peer or upstream.
	#[usage(long, choices("peer", "upstream"))]
	mtls_link: Option<moq_auth::serve::Link>,

	#[usage(long = "mtls-peer", hide = true, default_missing = "true", num_args = 0..=1, require_equals = true)]
	mtls_peer: Option<bool>,
	#[usage(long = "mtls-upstream", hide = true, default_missing = "true", num_args = 0..=1, require_equals = true)]
	mtls_upstream: Option<bool>,

	/// The tier label stamped on every grant, handed to the relay's stats: `/`-joined
	/// segments of ASCII letters, digits, `.`, `_`, and `-`.
	#[usage(long)]
	tier: Option<String>,

	/// How often the relay re-checks each grant. Off by default; needs `--expires`.
	#[usage(long)]
	revalidate: Option<moq_tokio::cli::Duration>,

	/// How long a grant with no bound of its own lasts: anonymous sessions, tokens without `exp`, certificates without one. Off by default.
	#[usage(long)]
	expires: Option<moq_tokio::cli::Duration>,

	/// The most live sessions presenting one token. Needs `--revalidate`.
	#[usage(long)]
	limit_token: Option<usize>,

	/// The most live sessions from one remote address. Needs `--revalidate`.
	#[usage(long)]
	limit_remote: Option<usize>,

	/// Coordinate a mesh: the public base URL devices enroll at, which every enrollment
	/// proof is bound to. Serve it behind TLS; devices refuse plain `http://` beyond loopback.
	#[usage(long)]
	mesh_url: Option<url::Url>,

	/// Where to answer devices at `--mesh-url`, usually behind a TLS-terminating proxy.
	#[usage(long, default = "127.0.0.1:4441", requires = "--mesh-url")]
	mesh_listen: SocketAddr,

	/// The private key that signs mesh tokens; the relays verify it like any other.
	#[usage(long, requires = "--mesh-url", value_hint = usage_rs::ValueHint::FilePath, extensions("jwk", "json"))]
	mesh_key: Option<PathBuf>,

	/// A relay URL enrolled devices dial (repeatable).
	#[usage(long, requires = "--mesh-url")]
	mesh_relay: Vec<url::Url>,

	/// The JSON file enrollment keys and devices persist to. Without one, a restart forgets them.
	#[usage(long, requires = "--mesh-url", value_hint = usage_rs::ValueHint::FilePath)]
	mesh_state: Option<PathBuf>,

	/// How long a mesh token lasts; devices refresh it two thirds of the way through.
	#[usage(long, default = "1h", requires = "--mesh-url")]
	mesh_token_ttl: moq_tokio::cli::Duration,
}

impl Serve {
	/// The policy these flags describe, refusing a cadence the relay would spin on.
	fn policy(&self) -> anyhow::Result<Policy> {
		let revalidate = self.revalidate.map(std::time::Duration::from);
		let expires = self.expires.map(Into::into);
		if revalidate.is_some_and(|cadence| cadence.is_zero()) {
			anyhow::bail!("--revalidate must be longer than 0s; every client would re-check in a tight loop");
		}
		if revalidate.is_some() && expires.is_none() {
			anyhow::bail!("--revalidate needs --expires, so a grant re-checked through an outage still has a bound");
		}
		// Only a re-check tells the session table a slot is still live; without one,
		// a relay that died without an `end` would hold its slots forever.
		if (self.limit_token.is_some() || self.limit_remote.is_some()) && revalidate.is_none() {
			anyhow::bail!(
				"--limit-token and --limit-remote need --revalidate, which ages out the slots of dead relays"
			);
		}
		if self.mtls_peer.is_some() || self.mtls_upstream.is_some() {
			anyhow::bail!("--mtls-peer and --mtls-upstream are removed; use --mtls-link peer or --mtls-link upstream");
		}
		if self.mtls_link.is_some() && self.mtls_publish.is_empty() && self.mtls_subscribe.is_empty() {
			anyhow::bail!("--mtls-link needs --mtls-publish or --mtls-subscribe, or every certificate is refused");
		}
		// 0.14 read `anon` as the prefix `anon/`, and a pattern reads it as exactly the
		// broadcast `anon`, so either silent reading would mislead someone upgrading.
		let flags = [
			("--public-publish", &self.public_publish),
			("--public-subscribe", &self.public_subscribe),
			("--mtls-publish", &self.mtls_publish),
			("--mtls-subscribe", &self.mtls_subscribe),
		];
		for (flag, patterns) in flags {
			for pattern in patterns.iter().filter(|pattern| pattern.is_literal()) {
				// A literal at the maximum depth is already its own subtree.
				let subtree = Pattern::subtree(pattern.as_str())?;
				if subtree != *pattern {
					anyhow::bail!(
						"{flag} `{pattern}` has no wildcard, so it names exactly one broadcast; write `{subtree}` for the subtree"
					);
				}
			}
		}
		let rules = |publish: &[Pattern], subscribe: &[Pattern]| {
			Permissions::new(
				publish.iter().cloned().collect::<moq_auth::Patterns>(),
				subscribe.iter().cloned().collect::<moq_auth::Patterns>(),
			)
		};
		let mut policy = Policy::default();
		policy.keys = match (&self.key, &self.key_dir, &self.key_set) {
			(Some(key), _, _) => Some(Keys::File(key.clone())),
			(None, Some(dir), _) => Some(Keys::Dir(dir.clone())),
			(None, None, Some(set)) => Some(Keys::Set(set.clone())),
			(None, None, None) => None,
		};
		policy.public = rules(&self.public_publish, &self.public_subscribe);
		policy.mtls = rules(&self.mtls_publish, &self.mtls_subscribe);
		policy.mtls_link = self.mtls_link;
		policy.tier = self.tier.clone();
		policy.revalidate = revalidate;
		policy.expires = expires;
		policy.limits.token = self.limit_token;
		policy.limits.remote = self.limit_remote;
		Ok(policy)
	}

	/// Where to listen, refusing a non-loopback address without `--listen-public`.
	fn listener(&self) -> anyhow::Result<Listen> {
		if let Some(path) = self.listen.strip_prefix("unix:") {
			return Ok(Listen::Unix(path.into()));
		}
		let addr: SocketAddr = self
			.listen
			.parse()
			.with_context(|| format!("--listen expects a socket address or unix:<path>, got {}", self.listen))?;
		if !addr.ip().is_loopback() && !self.listen_public {
			anyhow::bail!(
				"--listen {addr} is not a loopback address; the server has no authentication of its own, so pass --listen-public to expose it on purpose"
			);
		}
		Ok(Listen::Tcp(addr))
	}

	/// The mesh coordinator these flags describe, if `--mesh-url` asks for one.
	fn coordinator(&self) -> anyhow::Result<Option<moq_auth::mesh::Coordinator>> {
		let Some(url) = &self.mesh_url else {
			return Ok(None);
		};
		if self.listen_public {
			anyhow::bail!(
				"--mesh-url serves the mesh owner's API on --listen, which has no authentication, so it cannot be combined with --listen-public"
			);
		}
		let path = self
			.mesh_key
			.as_ref()
			.context("--mesh-url needs --mesh-key to sign tokens")?;
		let key = moq_auth::Key::from_file(path).with_context(|| format!("failed to read {}", path.display()))?;
		let ttl = std::time::Duration::from(self.mesh_token_ttl);
		if ttl.is_zero() {
			anyhow::bail!("--mesh-token-ttl must be longer than 0s");
		}
		let mut config = moq_auth::mesh::Config::new(url.clone(), key);
		config.relays = self.mesh_relay.clone();
		config.state = self.mesh_state.clone();
		config.token_ttl = ttl;
		Ok(Some(moq_auth::mesh::Coordinator::new(config)?))
	}

	async fn run(self) -> anyhow::Result<()> {
		let listen = self.listener()?;
		let mut server = moq_auth::serve::Server::new(self.policy()?)?;
		// The device-facing contract runs beside the auth server; either stopping ends both.
		let mut mesh = None;
		if let Some(coordinator) = self.coordinator()? {
			let addr = self.mesh_listen;
			let listener = tokio::net::TcpListener::bind(addr)
				.await
				.with_context(|| format!("failed to bind {addr}"))?;
			tracing::info!(%addr, url = ?self.mesh_url, "mesh coordinator listening");
			mesh = Some((coordinator.clone(), listener));
			server = server.with_mesh(coordinator);
		}
		let mesh = async move {
			match mesh {
				Some((coordinator, listener)) => coordinator.serve(listener).await,
				None => std::future::pending().await,
			}
		};
		let auth = async move {
			match listen {
				Listen::Tcp(addr) => {
					let listener = tokio::net::TcpListener::bind(addr)
						.await
						.with_context(|| format!("failed to bind {addr}"))?;
					tracing::info!(%addr, "auth server listening");
					server.serve(listener).await?;
				}
				#[cfg(unix)]
				Listen::Unix(path) => {
					unlink_socket(&path)?;
					let listener = tokio::net::UnixListener::bind(&path)
						.with_context(|| format!("failed to bind {}", path.display()))?;
					tracing::info!(path = %path.display(), "auth server listening");
					server.serve_unix(listener).await?;
				}
				#[cfg(not(unix))]
				Listen::Unix(path) => anyhow::bail!("unix sockets are not supported here: {}", path.display()),
			}
			anyhow::Ok(())
		};
		tokio::select! {
			res = auth => res,
			res = mesh => res.context("the mesh coordinator stopped"),
		}
	}
}

/// `moq auth mesh`: the mesh owner's commands, sent to the auth server's own listener.
#[derive(usage_rs::Args, Clone, Debug)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Mesh {
	/// The auth server's listener, which serves the owner's API alongside `POST /`:
	/// `http://` for a TCP `--listen`, or `unix:///path` for a unix socket.
	#[usage(long, default = "http://127.0.0.1:4440")]
	url: url::Url,

	#[usage(subcommand)]
	command: MeshCommand,
}

#[derive(usage_rs::Subcommands, Clone, Debug)]
enum MeshCommand {
	/// Create an enrollment key, printing it once.
	CreateKey {
		/// A tag every device it enrolls carries (repeatable).
		#[usage(long)]
		tag: Vec<String>,

		/// How long until it stops enrolling. Never, by default.
		#[usage(long)]
		expires: Option<moq_tokio::cli::Duration>,

		/// Enroll any number of devices, instead of one.
		#[usage(long)]
		reusable: bool,
	},

	/// List enrollment keys, without their secrets.
	Keys,

	/// Revoke an enrollment key so it enrolls nothing new. Its devices stay.
	RevokeKey {
		/// The key's ID: the part before the dot.
		id: String,
	},

	/// List enrolled devices.
	Devices,

	/// Revoke a device: its next refresh fails, and it cannot enroll again.
	Revoke {
		/// The device's endpoint ID.
		endpoint: String,
	},
}

impl Mesh {
	async fn run(self) -> anyhow::Result<()> {
		let (client, base) = self.client()?;
		let url = |path: &str, segments: &[&str]| {
			let mut url = base.clone();
			let prefix = url.path().trim_end_matches('/').to_string();
			url.set_path(&format!("{prefix}{path}"));
			if let Ok(mut parts) = url.path_segments_mut() {
				parts.extend(segments);
			}
			url
		};
		let request = match &self.command {
			MeshCommand::CreateKey { tag, expires, reusable } => {
				let mut new = moq_auth::mesh::NewKey::default();
				new.tags = tag.clone();
				new.expires_in = expires.map(std::time::Duration::from);
				new.reusable = *reusable;
				client.post(url("/mesh/keys", &[])).json(&new)
			}
			MeshCommand::Keys => client.get(url("/mesh/keys", &[])),
			MeshCommand::RevokeKey { id } => client.delete(url("/mesh/keys", &[id])),
			MeshCommand::Devices => client.get(url("/mesh/devices", &[])),
			MeshCommand::Revoke { endpoint } => client.delete(url("/mesh/devices", &[endpoint])),
		};
		let response = request
			.send()
			.await
			.with_context(|| format!("failed to reach {}", self.url))?;
		let status = response.status();
		let body = response.text().await.context("failed to read the reply")?;
		if status == reqwest::StatusCode::NOT_FOUND {
			// The routes exist only with `--mesh-url`, so a miss there is the likelier cause.
			anyhow::bail!(match self.command {
				MeshCommand::RevokeKey { .. } if !body.is_empty() => "no enrollment key has that ID",
				MeshCommand::Revoke { .. } if !body.is_empty() => "no device has that endpoint ID",
				_ => "not found; is the server running with --mesh-url?",
			});
		}
		if !status.is_success() {
			anyhow::bail!("{status}: {body}");
		}
		match serde_json::from_str::<serde_json::Value>(&body) {
			Ok(value) => println!("{}", serde_json::to_string_pretty(&value)?),
			Err(_) if body.is_empty() => println!("ok"),
			Err(_) => println!("{body}"),
		}
		Ok(())
	}

	/// The HTTP client for `--url` and the URL its requests target: a `unix://` URL names
	/// the socket, and the requests go to the server's root over it.
	fn client(&self) -> anyhow::Result<(reqwest::Client, url::Url)> {
		let builder = reqwest::Client::builder();
		match self.url.scheme() {
			"http" | "https" => Ok((builder.build()?, self.url.clone())),
			#[cfg(unix)]
			"unix" => {
				let path = self
					.url
					.to_file_path()
					.map_err(|()| anyhow::anyhow!("--url {} names no socket path", self.url))?;
				let target = url::Url::parse("http://localhost/").expect("a constant URL parses");
				Ok((builder.unix_socket(path).build()?, target))
			}
			_ => anyhow::bail!("--url expects http://, https://, or unix://, got {}", self.url),
		}
	}
}

#[derive(Debug)]
enum Listen {
	Tcp(SocketAddr),
	Unix(PathBuf),
}

/// Remove the socket file a previous run left at `path`, which would refuse the bind.
/// Anything that is not a socket stays put: a typo must not delete a file.
#[cfg(unix)]
fn unlink_socket(path: &std::path::Path) -> anyhow::Result<()> {
	use std::os::unix::fs::FileTypeExt;
	match std::fs::symlink_metadata(path) {
		Ok(meta) if meta.file_type().is_socket() => {
			std::fs::remove_file(path).with_context(|| format!("failed to remove the stale socket {}", path.display()))
		}
		Ok(_) => anyhow::bail!("{} exists and is not a socket; refusing to replace it", path.display()),
		Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
		Err(err) => Err(err).with_context(|| format!("failed to inspect {}", path.display())),
	}
}

fn is_dash(path: &std::path::Path) -> bool {
	path == std::path::Path::new("-")
}

fn write_key(key: &moq_auth::Key, path: &std::path::Path) -> anyhow::Result<()> {
	if is_dash(path) {
		println!("{}", key.to_str()?);
		Ok(())
	} else {
		key.to_file(path)
			.with_context(|| format!("failed to write key to {}", path.display()))
	}
}

fn read_key(path: &std::path::Path) -> anyhow::Result<moq_auth::Key> {
	if is_dash(path) {
		let contents = io::read_to_string(io::stdin())?;
		contents.parse().context("failed to parse key from stdin")
	} else {
		moq_auth::Key::from_file(path).with_context(|| format!("failed to read key from {}", path.display()))
	}
}

fn read_token(path: &std::path::Path) -> anyhow::Result<String> {
	let raw = if is_dash(path) {
		io::read_to_string(io::stdin())?
	} else {
		std::fs::read_to_string(path).with_context(|| format!("failed to read token from {}", path.display()))?
	};
	Ok(raw.trim().to_string())
}

#[derive(Clone, Copy, Debug)]
struct UnixTimestamp(moq_clock::Wall);

impl From<UnixTimestamp> for moq_clock::Wall {
	fn from(value: UnixTimestamp) -> Self {
		value.0
	}
}

impl std::str::FromStr for UnixTimestamp {
	type Err = anyhow::Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		let timestamp = s.parse::<i64>().context("expected unix timestamp")?;
		let timestamp = timestamp.try_into().context("timestamp out of range")?;
		// Past 2554 saturates, like a decoded token time.
		Ok(Self(
			moq_clock::Wall::UNIX_EPOCH.saturating_add(std::time::Duration::from_secs(timestamp)),
		))
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	/// Drive the same Usage grammar the binary exposes, rather than building
	/// `Command` directly, so the flags stay part of what's under test.
	async fn run(args: &[&str]) -> anyhow::Result<()> {
		parse(args)?.run().await
	}

	fn parse(args: &[&str]) -> anyhow::Result<Args> {
		let mut cli =
			crate::args::Invocation::try_parse_from(args.iter().copied()).map_err(|err| anyhow::anyhow!("{err}"))?;
		match cli.stages.remove(0) {
			crate::args::Command::Auth(auth) => Ok(auth),
			other => anyhow::bail!("parsed something other than `moq auth`: {}", other.name()),
		}
	}

	fn serve(args: &[&str]) -> Serve {
		match parse(args).unwrap().command {
			Command::Serve(serve) => serve,
			other => panic!("expected serve, got {other:?}"),
		}
	}

	#[test]
	fn verification_key_sources_conflict_in_the_parser() {
		let flags = ["--key", "--key-dir", "--key-set"];
		for first in flags {
			assert!(parse(&["moq", "auth", "serve", first, "unused"]).is_ok());
			for second in flags {
				if first == second {
					continue;
				}
				let err = parse(&["moq", "auth", "serve", first, "unused", second, "unused"])
					.expect_err("key sources must conflict before loading files");
				assert!(
					err.to_string().contains(first) && err.to_string().contains(second),
					"{err}"
				);
			}
		}
	}

	#[test]
	fn mesh_flags_need_a_mesh_url_and_key() {
		let err = parse(&["moq", "auth", "serve", "--mesh-relay", "https://relay.example"]).unwrap_err();
		assert!(err.to_string().contains("--mesh-url"), "{err}");

		let flags = serve(&["moq", "auth", "serve", "--mesh-url", "https://mesh.example"]);
		let err = flags.coordinator().err().expect("no signing key");
		assert!(err.to_string().contains("--mesh-key"), "{err}");

		let dir = tempfile::tempdir().unwrap();
		let key = dir.path().join("mesh.jwk");
		moq_auth::Key::generate(Algorithm::HS256, None)
			.unwrap()
			.to_file(&key)
			.unwrap();
		let flags = serve(&[
			"moq",
			"auth",
			"serve",
			"--mesh-url",
			"https://mesh.example",
			"--mesh-key",
			key.to_str().unwrap(),
			"--mesh-relay",
			"https://a.example",
			"--mesh-relay",
			"https://b.example",
		]);
		assert_eq!(flags.mesh_relay.len(), 2);
		assert!(flags.mesh_listen.ip().is_loopback());
		assert!(flags.coordinator().unwrap().is_some());
		assert!(serve(&["moq", "auth", "serve"]).coordinator().unwrap().is_none());

		// The owner's API rides --listen, so a public one would hand out enrollment keys.
		let flags = serve(&[
			"moq",
			"auth",
			"serve",
			"--listen",
			"0.0.0.0:4440",
			"--listen-public",
			"--mesh-url",
			"https://mesh.example",
			"--mesh-key",
			key.to_str().unwrap(),
		]);
		let err = flags.coordinator().err().expect("a public owner API");
		assert!(err.to_string().contains("--listen-public"), "{err}");
	}

	#[cfg(unix)]
	#[tokio::test]
	async fn mesh_commands_reach_a_unix_listener() {
		let dir = tempfile::tempdir().unwrap();
		let key = moq_auth::Key::generate(Algorithm::HS256, None).unwrap();
		let config = moq_auth::mesh::Config::new("https://mesh.example".parse().unwrap(), key);
		let coordinator = moq_auth::mesh::Coordinator::new(config).unwrap();
		let server = moq_auth::serve::Server::new(Policy::default())
			.unwrap()
			.with_mesh(coordinator.clone());
		let path = dir.path().join("auth.sock");
		let listener = tokio::net::UnixListener::bind(&path).unwrap();
		tokio::spawn(async move { server.serve_unix(listener).await });

		let url = format!("unix://{}", path.display());
		run(&["moq", "auth", "mesh", "--url", &url, "create-key", "--expires", "1d"])
			.await
			.unwrap();
		let keys = coordinator.keys();
		assert_eq!(keys.len(), 1);
		assert!(keys[0].expires.is_some());
	}

	#[test]
	fn mesh_commands_parse() {
		for args in [
			&[
				"moq",
				"auth",
				"mesh",
				"create-key",
				"--tag",
				"robot",
				"--reusable",
				"--expires",
				"1d",
			][..],
			&["moq", "auth", "mesh", "devices"],
			&["moq", "auth", "mesh", "--url", "http://127.0.0.1:9000", "revoke", "ab"],
		] {
			assert!(matches!(parse(args).unwrap().command, Command::Mesh(_)), "{args:?}");
		}
	}

	#[test]
	fn serve_refuses_a_public_bind_without_saying_so() {
		let err = serve(&["moq", "auth", "serve", "--listen", "0.0.0.0:4440"])
			.listener()
			.unwrap_err();
		assert!(err.to_string().contains("--listen-public"), "{err}");

		assert!(matches!(
			serve(&["moq", "auth", "serve", "--listen", "0.0.0.0:4440", "--listen-public"]).listener(),
			Ok(Listen::Tcp(_))
		));
		assert!(
			matches!(serve(&["moq", "auth", "serve"]).listener(), Ok(Listen::Tcp(addr)) if addr.ip().is_loopback())
		);
		assert!(matches!(
			serve(&["moq", "auth", "serve", "--listen", "unix:/run/moq-auth.sock"]).listener(),
			Ok(Listen::Unix(path)) if path == std::path::Path::new("/run/moq-auth.sock")
		));
		assert!(
			serve(&["moq", "auth", "serve", "--listen", "nowhere"])
				.listener()
				.is_err()
		);
	}

	#[test]
	fn serve_flags_become_the_policy() {
		let policy = serve(&[
			"moq",
			"auth",
			"serve",
			"--key-dir",
			"/keys",
			"--public-subscribe",
			"anon/**",
			"--mtls-publish",
			"**",
			"--mtls-subscribe",
			"**",
			"--mtls-link",
			"upstream",
			"--tier",
			"internal",
			"--revalidate",
			"30s",
			"--expires",
			"2h",
			"--limit-token",
			"3",
			"--limit-remote",
			"8",
		])
		.policy()
		.unwrap();
		assert!(matches!(&policy.keys, Some(Keys::Dir(dir)) if dir == std::path::Path::new("/keys")));
		assert_eq!(
			policy.public.subscribe,
			["anon/**".parse().unwrap()].into_iter().collect()
		);
		assert!(policy.public.publish.is_empty());
		assert_eq!(policy.mtls.publish, ["**".parse().unwrap()].into_iter().collect());
		assert_eq!(policy.mtls_link, Some(moq_auth::serve::Link::Upstream));
		assert_eq!(policy.tier.as_deref(), Some("internal"));
		assert_eq!(policy.revalidate, Some(std::time::Duration::from_secs(30)));
		assert_eq!(policy.expires, Some(std::time::Duration::from_secs(7200)));
		assert_eq!(policy.limits.token, Some(3));
		assert_eq!(policy.limits.remote, Some(8));

		let set = serve(&["moq", "auth", "serve", "--key-set", "/keys.jwks"])
			.policy()
			.unwrap();
		assert!(matches!(&set.keys, Some(Keys::Set(path)) if path == std::path::Path::new("/keys.jwks")));
		assert!(parse(&["moq", "auth", "serve", "--key", "/key.jwk", "--key-set", "/keys.jwks"]).is_err());

		// Nothing configured is a server that refuses everyone, and like 0.14, never
		// re-checks or closes a session on its own.
		let bare = serve(&["moq", "auth", "serve"]).policy().unwrap();
		assert!(bare.keys.is_none());
		assert!(bare.public.is_empty() && bare.mtls.is_empty());
		assert_eq!(bare.mtls_link, None);
		assert_eq!(bare.revalidate, None);
		assert_eq!(bare.expires, None);

		for (args, needs) in [
			// A zero cadence would have every client re-check in a tight loop.
			(&["--revalidate", "0s", "--expires", "1h"][..], "longer than 0s"),
			// The contract refuses a cadence without a bound.
			(&["--revalidate", "1m"], "--revalidate needs --expires"),
			// Only a re-check keeps a slot alive.
			(&["--limit-token", "3"], "need --revalidate"),
			(&["--limit-remote", "3", "--expires", "1h"], "need --revalidate"),
			// A peer marking with nothing granted to certificates marks nobody.
			(&["--mtls-link", "peer"], "--mtls-link needs"),
			(&["--mtls-subscribe", "**", "--mtls-upstream"], "use --mtls-link"),
			(&["--mtls-peer=false"], "use --mtls-link"),
			(&["--mtls-upstream=false"], "use --mtls-link"),
		] {
			let argv: Vec<&str> = ["moq", "auth", "serve"].iter().chain(args).copied().collect();
			let err = serve(&argv).policy().unwrap_err().to_string();
			assert!(err.contains(needs), "{args:?}: {err}");
		}
		let expiring = serve(&["moq", "auth", "serve", "--expires", "1h"]).policy().unwrap();
		assert_eq!(expiring.revalidate, None);
	}

	/// 0.14 read `anon` as a prefix and a pattern reads it as one broadcast, so a
	/// wildcard-free rule refuses to start rather than pick silently.
	#[test]
	fn serve_refuses_a_rule_without_a_wildcard() {
		for (flag, rule, hint) in [
			("--public-publish", "anon", "anon/**"),
			("--public-subscribe", "event/cam1.hang", "event/cam1.hang/**"),
			("--mtls-publish", "", "**"),
			("--mtls-subscribe", "origin", "origin/**"),
		] {
			let err = serve(&["moq", "auth", "serve", flag, rule]).policy().unwrap_err();
			let err = err.to_string();
			assert!(err.contains(flag) && err.contains(&format!("`{hint}`")), "{err}");
		}
		assert!(
			serve(&[
				"moq",
				"auth",
				"serve",
				"--public-subscribe",
				"*/chat",
				"--mtls-publish",
				"origin/*"
			])
			.policy()
			.is_ok()
		);

		// Nothing sits beneath a literal at the maximum depth, so it is its own subtree.
		let deepest = vec!["a"; Pattern::MAX_SEGMENTS].join("/");
		assert!(
			serve(&["moq", "auth", "serve", "--public-subscribe", &deepest])
				.policy()
				.is_ok()
		);
	}

	#[cfg(unix)]
	#[test]
	fn unlink_socket_leaves_anything_but_a_socket_alone() {
		let dir = tempfile::tempdir().unwrap();

		let file = dir.path().join("config");
		std::fs::write(&file, "keep me").unwrap();
		let err = unlink_socket(&file).unwrap_err();
		assert!(err.to_string().contains("not a socket"), "{err}");
		assert_eq!(std::fs::read_to_string(&file).unwrap(), "keep me");

		unlink_socket(&dir.path().join("missing")).unwrap();

		let stale = dir.path().join("auth.sock");
		drop(std::os::unix::net::UnixListener::bind(&stale).unwrap());
		unlink_socket(&stale).unwrap();
		assert!(!stale.exists());
	}

	#[tokio::test]
	async fn generate_writes_a_usable_keypair() {
		let dir = tempfile::tempdir().unwrap();
		let private = dir.path().join("private.jwk");
		let public = dir.path().join("public.jwk");

		run(&[
			"moq",
			"auth",
			"generate",
			// ES256 rather than an RSA algorithm: keygen dominates this test's runtime.
			"--algorithm",
			"ES256",
			"--out",
			private.to_str().unwrap(),
			"--public",
			public.to_str().unwrap(),
		])
		.await
		.unwrap();

		// Sign through the CLI grammar rather than the library, so the value parsers
		// behind --expires / --issued are covered too.
		run(&[
			"moq",
			"auth",
			"sign",
			"--key",
			private.to_str().unwrap(),
			"--root",
			"demo",
			"--publish",
			"alice/**",
			"--issued",
			"1700000000",
			"--expires",
			"4102444800",
		])
		.await
		.unwrap();

		// What the relay actually does with these two files: the public half has to
		// verify what the private half signed. `sign` only prints to stdout, so the
		// token itself comes from the library.
		let token = moq_auth::Key::from_file(&private)
			.unwrap()
			.sign(
				&moq_auth::Claims::default()
					.with_root("demo")
					.with_publish(["alice/**".parse().unwrap()]),
			)
			.unwrap();
		let path = dir.path().join("alice.jwt");
		std::fs::write(&path, &token).unwrap();

		run(&[
			"moq",
			"auth",
			"verify",
			"--key",
			public.to_str().unwrap(),
			"--in",
			path.to_str().unwrap(),
		])
		.await
		.unwrap();
	}

	#[tokio::test]
	async fn generate_to_a_directory_names_the_file_after_the_kid() {
		let dir = tempfile::tempdir().unwrap();

		run(&["moq", "auth", "generate", "--out-dir", dir.path().to_str().unwrap()])
			.await
			.unwrap();

		let written: Vec<_> = std::fs::read_dir(dir.path())
			.unwrap()
			.map(|e| e.unwrap().path())
			.collect();
		assert_eq!(written.len(), 1, "expected exactly one key, got {written:?}");
		let key = moq_auth::Key::from_file(&written[0]).unwrap();
		let kid = key.kid.as_ref().expect("generate assigns a kid");
		assert_eq!(written[0].file_name().unwrap().to_str().unwrap(), format!("{kid}.jwk"));
	}

	// Both halves on stdout would interleave into one unparseable blob, so it's
	// rejected up front rather than written.
	#[tokio::test]
	async fn both_keys_to_stdout_is_rejected() {
		let err = run(&["moq", "auth", "generate", "--algorithm", "ES256", "--public", "-"])
			.await
			.unwrap_err();
		assert!(err.to_string().contains("cannot write both keys to stdout"), "{err}");
	}

	#[tokio::test]
	async fn both_inputs_from_stdin_is_rejected() {
		let err = run(&["moq", "auth", "verify", "--key", "-", "--in", "-"])
			.await
			.unwrap_err();
		assert!(err.to_string().contains("cannot both read from stdin"), "{err}");
	}

	#[test]
	fn revalidate_and_sessions_flags_parse() {
		let args = parse(&[
			"moq",
			"auth",
			"revalidate",
			"--internal-url",
			"http://127.0.0.1:9101",
			"--id",
			"abc",
			"--path",
			"demo/**",
			"--remote",
			"203.0.113.0/24",
			"--tls-name",
			"edge0",
		])
		.unwrap();
		match args.command {
			Command::Revalidate(filter) => {
				assert_eq!(filter.id.as_deref(), Some("abc"));
				assert_eq!(filter.path.as_deref(), Some("demo/**"));
				assert_eq!(filter.remote.as_deref(), Some("203.0.113.0/24"));
				assert_eq!(filter.tls_name.as_deref(), Some("edge0"));
			}
			other => panic!("expected revalidate, got {other:?}"),
		}

		assert!(matches!(
			parse(&["moq", "auth", "sessions", "--internal-url", "http://127.0.0.1:9101"])
				.unwrap()
				.command,
			Command::Sessions(_)
		));
	}

	#[tokio::test]
	async fn revalidate_posts_against_the_internal_listener() {
		let sessions = moq_relay::session::Registry::new();
		let mut request = moq_auth::Request::new("relay-1", moq_auth::Transport::Quic, "/demo/room");
		request.id = "abc".into();
		request.remote = Some("203.0.113.9:4433".parse().unwrap());
		request.query = Some("jwt=secret".into());
		let _reg = sessions.register(request);

		let mut internal_config = moq_relay::internal::Config::default();
		internal_config.listen = Some("127.0.0.1:0".parse().unwrap());
		let internal =
			moq_relay::internal::Internal::new(internal_config, moq_tokio::moq_net::stats::Registry::disabled())
				.with_sessions(sessions)
				.bind()
				.expect("bind internal listener");
		let listen = internal.addr().expect("internal listener is configured");
		tokio::spawn(async move {
			let _ = internal.run().await;
		});

		let url = format!("http://{listen}");

		run(&["moq", "auth", "revalidate", "--internal-url", &url, "--id", "abc"])
			.await
			.expect("revalidate");

		run(&["moq", "auth", "sessions", "--internal-url", &url, "--path", "demo/**"])
			.await
			.expect("sessions");
	}

	#[test]
	fn timestamp_before_the_epoch_is_rejected() {
		assert!("-1".parse::<UnixTimestamp>().is_err());
		assert!("not-a-number".parse::<UnixTimestamp>().is_err());
	}

	#[test]
	fn timestamp_past_the_wall_range_saturates() {
		let last = moq_clock::Wall::UNIX_EPOCH.saturating_add(std::time::Duration::MAX);
		let parse = |text: &str| moq_clock::Wall::from(text.parse::<UnixTimestamp>().unwrap());
		assert_eq!(parse(&i64::MAX.to_string()), last);
		assert!(parse("18446744073") < last);
	}
}
