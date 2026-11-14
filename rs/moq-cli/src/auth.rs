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
	///
	/// `cli` holds the settings the command line gave, which `serve` layers over its file.
	pub async fn run(self, cli: &usage_rs::config::CliLayer) -> anyhow::Result<()> {
		match self.command {
			// A container runs `moq` as PID 1, where a signal without a handler is
			// dropped. Only `serve` races one: `sign` and `verify` block on stdin, so a
			// handler would swallow the signal that used to end them. The handlers are
			// installed before `serve` logs that it is listening.
			Command::Serve(serve) => {
				let mut signals = crate::Signals::new()?;
				tokio::select! {
					() = signals.recv() => {}
					res = serve.run(cli) => res?,
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
				ttl,
				unbounded,
				issued,
			} => {
				let key = read_key(&key)?;
				let expires = match (unbounded, expires) {
					(true, _) => None,
					(false, Some(expires)) => Some(expires.into()),
					(false, None) => Some(
						kio::clock::wall()
							.checked_add(ttl.into())
							.context("--ttl reaches past the system clock's range")?,
					),
				};

				let payload = moq_auth::Claims::default()
					.with_root(root)
					.with_publish(publish)
					.with_subscribe(subscribe)
					.with_expires(expires)
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

		/// Expiration time as a unix timestamp, instead of `--ttl` from now.
		#[usage(long, conflicts("--ttl", "--unbounded"))]
		expires: Option<UnixTimestamp>,

		/// How long the token lasts from now, written as its `exp`.
		#[usage(long, default = "1h", conflicts = "--unbounded")]
		ttl: moq_tokio::cli::Duration,

		/// Write no `exp`, so the token never expires.
		#[usage(long)]
		unbounded: bool,

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
///
/// Every flag also reads from the TOML `file` under its dotted setting key, such as
/// `mtls.publish` for `--mtls-publish`; a flag given on the command line wins. Only
/// the file can scope particular certificates, under `[mtls.certificates.<sha256>]`.
#[derive(usage_rs::Args, Clone, Debug, serde::Serialize, serde::Deserialize)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde(deny_unknown_fields)]
pub struct Serve {
	/// A TOML file of settings, watched so a policy edit applies without a restart.
	#[usage(value_hint = usage_rs::ValueHint::FilePath, extensions("toml"))]
	#[serde(skip)]
	file: Option<PathBuf>,

	/// Where to answer: a socket address, or `unix:<path>` for a unix socket.
	#[usage(long, default = "127.0.0.1:4440", setting = "listen")]
	listen: String,

	/// Allow a bind on a non-loopback address. Needs `--listen-secret`.
	#[usage(long, setting = "listen_public")]
	#[serde(default)]
	listen_public: bool,

	/// A file holding the secret every request must carry as its HTTP Basic password;
	/// relays send it from `--auth-url https://relay:<secret>@host/`.
	#[usage(long, value_hint = usage_rs::ValueHint::FilePath, setting = "listen_secret")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	listen_secret: Option<PathBuf>,

	/// The key file a `jwt` is verified against.
	#[usage(long, conflicts("--key-dir", "--key-set"), value_hint = usage_rs::ValueHint::FilePath, extensions("jwk", "json"), setting = "key")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	key: Option<PathBuf>,

	/// A directory of `{kid}.jwk` files, selected by the token's `kid`.
	#[usage(long, conflicts("--key", "--key-set"), value_hint = usage_rs::ValueHint::DirPath, setting = "key_dir")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	key_dir: Option<PathBuf>,

	/// A JWK Set file, selecting one verification key by the token's `kid`.
	#[usage(long, conflicts("--key", "--key-dir"), value_hint = usage_rs::ValueHint::FilePath, extensions("json", "jwks"), setting = "key_set")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	key_set: Option<PathBuf>,

	#[usage(flatten)]
	#[serde(default)]
	public: PublicRules,

	#[usage(flatten)]
	#[serde(default)]
	mtls: MtlsRules,

	/// The tier label stamped on every grant, handed to the relay's stats: `/`-joined
	/// segments of ASCII letters, digits, `.`, `_`, and `-`.
	#[usage(long, setting = "tier")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	tier: Option<String>,

	/// How often the relay re-checks each grant. Off by default; needs `--expires`.
	#[usage(long, setting = "revalidate")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	revalidate: Option<moq_tokio::cli::Duration>,

	/// How long a grant with no bound of its own lasts: anonymous sessions, tokens without `exp`, certificates without one. Off by default.
	#[usage(long, setting = "expires")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	expires: Option<moq_tokio::cli::Duration>,

	#[usage(flatten)]
	#[serde(default)]
	limit: SessionLimits,

	#[usage(flatten)]
	#[serde(default)]
	mesh: MeshServe,
}

/// What an anonymous session may do.
#[derive(usage_rs::Args, Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde(deny_unknown_fields, default)]
struct PublicRules {
	/// Patterns an anonymous session may publish, rooted at `/` (repeatable); `foo/**` for a subtree.
	#[usage(name = "public-publish", long = "public-publish", setting = "public.publish")]
	publish: Vec<Pattern>,

	/// Patterns an anonymous session may subscribe to, rooted at `/` (repeatable).
	#[usage(name = "public-subscribe", long = "public-subscribe", setting = "public.subscribe")]
	subscribe: Vec<Pattern>,
}

/// What a session presenting a verified certificate may do.
#[derive(usage_rs::Args, Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde(deny_unknown_fields, default)]
struct MtlsRules {
	/// Patterns a session with a verified certificate may publish, rooted at `/` (repeatable). Empty refuses certificates no rule names.
	#[usage(name = "mtls-publish", long = "mtls-publish", setting = "mtls.publish")]
	publish: Vec<Pattern>,

	/// Patterns a session with a verified certificate may subscribe to, rooted at `/` (repeatable).
	#[usage(name = "mtls-subscribe", long = "mtls-subscribe", setting = "mtls.subscribe")]
	subscribe: Vec<Pattern>,

	/// The role verified certificates have on cluster links: peer or upstream.
	#[usage(
		name = "mtls-link",
		long = "mtls-link",
		choices("peer", "upstream"),
		setting = "mtls.link"
	)]
	#[serde(skip_serializing_if = "Option::is_none")]
	link: Option<moq_auth::serve::Link>,

	/// Rules for particular certificates, keyed by the SHA-256 of a leaf or of a
	/// certificate it was issued under.
	#[usage(skip)]
	#[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
	certificates: std::collections::BTreeMap<moq_auth::serve::Fingerprint, moq_auth::serve::Rule>,
}

/// Caps on live sessions.
#[derive(usage_rs::Args, Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde(deny_unknown_fields, default)]
struct SessionLimits {
	/// The most live sessions presenting one token. Needs `--revalidate`.
	#[usage(name = "limit-token", long = "limit-token", setting = "limit.token")]
	#[serde(skip_serializing_if = "Option::is_none")]
	token: Option<usize>,

	/// The most live sessions from one remote address. Needs `--revalidate`.
	#[usage(name = "limit-remote", long = "limit-remote", setting = "limit.remote")]
	#[serde(skip_serializing_if = "Option::is_none")]
	remote: Option<usize>,
}

/// The mesh coordinator served beside the auth server.
#[derive(usage_rs::Args, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde(deny_unknown_fields)]
struct MeshServe {
	/// Coordinate a mesh: the public base URL devices enroll at, which every enrollment
	/// proof is bound to. Serve it behind TLS; devices refuse plain `http://` beyond loopback.
	#[usage(name = "mesh-url", long = "mesh-url", setting = "mesh.url")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	url: Option<url::Url>,

	/// Where to answer devices at `--mesh-url`, usually behind a TLS-terminating proxy.
	#[usage(
		name = "mesh-listen",
		long = "mesh-listen",
		default = "127.0.0.1:4441",
		setting = "mesh.listen"
	)]
	listen: SocketAddr,

	/// The private key that signs mesh tokens; the relays verify it like any other.
	#[usage(name = "mesh-key", long = "mesh-key", value_hint = usage_rs::ValueHint::FilePath, extensions("jwk", "json"), setting = "mesh.key")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	key: Option<PathBuf>,

	/// A relay URL enrolled devices dial (repeatable).
	#[usage(name = "mesh-relay", long = "mesh-relay", setting = "mesh.relay")]
	#[serde(default)]
	relay: Vec<url::Url>,

	/// The JSON file enrollment keys and devices persist to. Without one, a restart forgets them.
	#[usage(name = "mesh-state", long = "mesh-state", value_hint = usage_rs::ValueHint::FilePath, setting = "mesh.state")]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	state: Option<PathBuf>,

	/// How long a mesh token lasts, which bounds how long a revoked device keeps access;
	/// devices refresh it two thirds of the way through.
	#[usage(
		name = "mesh-token-ttl",
		long = "mesh-token-ttl",
		default = "15m",
		setting = "mesh.token_ttl"
	)]
	token_ttl: moq_tokio::cli::Duration,
}

impl Default for MeshServe {
	fn default() -> Self {
		Self {
			url: None,
			listen: SocketAddr::from(([127, 0, 0, 1], 4441)),
			key: None,
			relay: Vec::new(),
			state: None,
			token_ttl: std::time::Duration::from_secs(900).into(),
		}
	}
}

/// Every setting `moq auth serve` merges from its file, declared for the merge's registry.
#[allow(
	dead_code,
	reason = "declares the settings for the usage registry; nothing reads the fields"
)]
#[derive(usage_rs::Config)]
struct ServeSettings {
	#[usage(cli("--listen"))]
	listen: Option<String>,
	#[usage(cli("--listen-public"))]
	listen_public: Option<bool>,
	#[usage(cli("--listen-secret"))]
	listen_secret: Option<String>,
	#[usage(cli("--key"))]
	key: Option<String>,
	#[usage(cli("--key-dir"))]
	key_dir: Option<String>,
	#[usage(cli("--key-set"))]
	key_set: Option<String>,
	#[usage(flatten)]
	public: PublicSettings,
	#[usage(flatten)]
	mtls: MtlsSettings,
	#[usage(cli("--tier"))]
	tier: Option<String>,
	#[usage(cli("--revalidate"))]
	revalidate: Option<String>,
	#[usage(cli("--expires"))]
	expires: Option<String>,
	#[usage(flatten)]
	limit: LimitSettings,
	#[usage(flatten)]
	mesh: MeshSettings,
}

#[allow(
	dead_code,
	reason = "declares the settings for the usage registry; nothing reads the fields"
)]
#[derive(usage_rs::Config)]
#[usage(prefix = "public")]
struct PublicSettings {
	#[usage(cli("--public-publish"))]
	publish: Option<Vec<String>>,
	#[usage(cli("--public-subscribe"))]
	subscribe: Option<Vec<String>>,
}

#[allow(
	dead_code,
	reason = "declares the settings for the usage registry; nothing reads the fields"
)]
#[derive(usage_rs::Config)]
#[usage(prefix = "mtls")]
struct MtlsSettings {
	#[usage(cli("--mtls-publish"))]
	publish: Option<Vec<String>>,
	#[usage(cli("--mtls-subscribe"))]
	subscribe: Option<Vec<String>>,
	#[usage(cli("--mtls-link"))]
	link: Option<String>,
}

#[allow(
	dead_code,
	reason = "declares the settings for the usage registry; nothing reads the fields"
)]
#[derive(usage_rs::Config)]
#[usage(prefix = "limit")]
struct LimitSettings {
	#[usage(cli("--limit-token"))]
	token: Option<u64>,
	#[usage(cli("--limit-remote"))]
	remote: Option<u64>,
}

#[allow(
	dead_code,
	reason = "declares the settings for the usage registry; nothing reads the fields"
)]
#[derive(usage_rs::Config)]
#[usage(prefix = "mesh")]
struct MeshSettings {
	#[usage(cli("--mesh-url"))]
	url: Option<String>,
	#[usage(cli("--mesh-listen"))]
	listen: Option<String>,
	#[usage(cli("--mesh-key"))]
	key: Option<String>,
	#[usage(cli("--mesh-relay"))]
	relay: Option<Vec<String>>,
	#[usage(cli("--mesh-state"))]
	state: Option<String>,
	#[usage(cli("--mesh-token-ttl"))]
	token_ttl: Option<String>,
}

impl Serve {
	/// These flags over the settings in [`file`](Self::file), read from disk now.
	///
	/// A mesh setting without a mesh URL is refused here rather than by the parser, so
	/// the URL can come from the file and the rest from the command line.
	fn merged(&self, cli: &usage_rs::config::CliLayer, env: &usage_rs::config::EnvLayer) -> anyhow::Result<Self> {
		let value = self
			.file
			.as_ref()
			.map(|path| {
				let text =
					std::fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
				toml::from_str::<toml::Value>(&text).with_context(|| format!("failed to parse {}", path.display()))
			})
			.transpose()?;
		let file = self
			.file
			.as_deref()
			.zip(value.as_ref())
			.map(|(path, value)| moq_tokio::cli::FileSource { path, value });
		let (mut merged, resolved) = moq_tokio::cli::Merge {
			registry: ServeSettings::SETTINGS_REGISTRY,
			cli,
			env,
			file,
		}
		.apply(self.clone())
		.map_err(|err| anyhow::anyhow!("{err}"))?;
		merged.file = self.file.clone();

		if merged.mesh.url.is_none() {
			for key in ["mesh.listen", "mesh.key", "mesh.relay", "mesh.state", "mesh.token_ttl"] {
				if let Some(origin) = resolved.origin_key(key) {
					anyhow::bail!("{} needs --mesh-url (mesh.url)", origin.describe());
				}
			}
		}
		Ok(merged)
	}

	/// The policy these settings describe, refusing a cadence the relay would spin on.
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
		if (self.limit.token.is_some() || self.limit.remote.is_some()) && revalidate.is_none() {
			anyhow::bail!(
				"--limit-token and --limit-remote need --revalidate, which ages out the slots of dead relays"
			);
		}
		let mtls = &self.mtls;
		if mtls.link.is_some() && mtls.publish.is_empty() && mtls.subscribe.is_empty() && mtls.certificates.is_empty() {
			anyhow::bail!("--mtls-link needs --mtls-publish or --mtls-subscribe, or every certificate is refused");
		}
		// The parser refuses two key sources on the command line; a file can add another.
		let sources = [&self.key, &self.key_dir, &self.key_set];
		if sources.iter().filter(|source| source.is_some()).count() > 1 {
			anyhow::bail!("--key, --key-dir, and --key-set conflict; pick one");
		}

		// 0.14 read `anon` as the prefix `anon/`, and a pattern reads it as exactly the
		// broadcast `anon`, so either silent reading would mislead someone upgrading.
		let mut flags = vec![
			(
				"--public-publish".to_string(),
				self.public.publish.iter().collect::<Vec<_>>(),
			),
			("--public-subscribe".to_string(), self.public.subscribe.iter().collect()),
			("--mtls-publish".to_string(), mtls.publish.iter().collect()),
			("--mtls-subscribe".to_string(), mtls.subscribe.iter().collect()),
		];
		for (hash, rule) in &mtls.certificates {
			for (direction, patterns) in [("publish", &rule.publish), ("subscribe", &rule.subscribe)] {
				let patterns = patterns.iter().flat_map(|patterns| patterns.iter()).collect();
				flags.push((format!("mtls.certificates.{hash}.{direction}"), patterns));
			}
		}
		for (flag, patterns) in flags {
			for pattern in patterns.into_iter().filter(|pattern| pattern.is_literal()) {
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
		policy.public = rules(&self.public.publish, &self.public.subscribe);
		policy.mtls = rules(&mtls.publish, &mtls.subscribe);
		policy.mtls_link = mtls.link;
		policy.certificates = mtls.certificates.clone();
		policy.tier = self.tier.clone();
		policy.revalidate = revalidate;
		policy.expires = expires;
		policy.limits.token = self.limit.token;
		policy.limits.remote = self.limit.remote;
		Ok(policy)
	}

	/// Where to listen, refusing a non-loopback address without `--listen-public`, and
	/// that without `--listen-secret`.
	fn listener(&self) -> anyhow::Result<Listen> {
		if self.listen_public && self.listen_secret.is_none() {
			anyhow::bail!(
				"--listen-public needs --listen-secret, so only relays can record sessions and fill the session limits"
			);
		}
		if let Some(path) = self.listen.strip_prefix("unix:") {
			// A unix URL carries no userinfo for a client to send the secret from.
			if self.listen_secret.is_some() {
				anyhow::bail!(
					"--listen-secret guards a TCP listener; a unix socket is guarded by its file permissions"
				);
			}
			return Ok(Listen::Unix(path.into()));
		}
		let addr: SocketAddr = self
			.listen
			.parse()
			.with_context(|| format!("--listen expects a socket address or unix:<path>, got {}", self.listen))?;
		if !addr.ip().is_loopback() && !self.listen_public {
			anyhow::bail!(
				"--listen {addr} is not a loopback address; pass --listen-public with --listen-secret to expose it on purpose"
			);
		}
		Ok(Listen::Tcp(addr))
	}

	/// The mesh coordinator these settings describe, if `--mesh-url` asks for one.
	fn coordinator(&self) -> anyhow::Result<Option<moq_auth::mesh::Coordinator>> {
		let Some(url) = &self.mesh.url else {
			return Ok(None);
		};
		if self.listen_public {
			anyhow::bail!(
				"--mesh-url serves the mesh owner's API on --listen, which every relay holding --listen-secret could reach, so it cannot be combined with --listen-public"
			);
		}
		let path = self
			.mesh
			.key
			.as_ref()
			.context("--mesh-url needs --mesh-key to sign tokens")?;
		let key = moq_auth::Key::from_file(path).with_context(|| format!("failed to read {}", path.display()))?;
		let ttl = std::time::Duration::from(self.mesh.token_ttl);
		if ttl.is_zero() {
			anyhow::bail!("--mesh-token-ttl must be longer than 0s");
		}
		let mut config = moq_auth::mesh::Config::new(url.clone(), key);
		config.relays = self.mesh.relay.clone();
		config.state = self.mesh.state.clone();
		config.token_ttl = ttl;
		Ok(Some(moq_auth::mesh::Coordinator::new(config)?))
	}

	/// Re-read the file and answer with its policy, leaving `server` as it was when
	/// either is refused.
	fn apply(
		&self,
		server: &moq_auth::serve::Server,
		cli: &usage_rs::config::CliLayer,
		env: &usage_rs::config::EnvLayer,
	) -> anyhow::Result<Self> {
		let serve = self.merged(cli, env)?;
		server.update(serve.policy()?)?;
		Ok(serve)
	}

	/// The settings only a restart applies: the listeners, their secret, and the mesh.
	fn restart_only(&self) -> (&str, bool, Option<&PathBuf>, &MeshServe) {
		(
			&self.listen,
			self.listen_public,
			self.listen_secret.as_ref(),
			&self.mesh,
		)
	}

	/// The secret in [`listen_secret`](Self::listen_secret), without its trailing newline.
	fn secret(&self) -> anyhow::Result<Option<String>> {
		let Some(path) = &self.listen_secret else {
			return Ok(None);
		};
		let text = std::fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
		let secret = text.trim_end_matches(['\r', '\n']);
		if secret.is_empty() {
			anyhow::bail!("--listen-secret {} is empty", path.display());
		}
		Ok(Some(secret.to_string()))
	}

	/// Reload `server`'s policy each time the file changes, keeping the last valid one
	/// when an edit does not parse or is refused. Never returns without a file.
	async fn reload(
		&self,
		running: &Self,
		server: &moq_auth::serve::Server,
		cli: &usage_rs::config::CliLayer,
	) -> anyhow::Result<()> {
		let Some(path) = &self.file else {
			return std::future::pending().await;
		};
		let mut watcher = moq_tokio::watch::Files::new(std::slice::from_ref(path))
			.with_context(|| format!("failed to watch {}", path.display()))?;
		let reload = |quiet: bool| match self.apply(server, cli, &usage_rs::config::EnvLayer::from_process()) {
			Ok(serve) if serve.restart_only() != running.restart_only() => tracing::error!(
				file = %path.display(),
				"reloaded the policy; listener and mesh changes take effect on restart"
			),
			Ok(_) if quiet => {}
			Ok(_) => tracing::info!(file = %path.display(), "reloaded the policy"),
			Err(err) => tracing::error!(file = %path.display(), "keeping the last valid policy: {err:#}"),
		};
		// An edit between the first read and the watch produced no event, so read once
		// more now that nothing further can be missed. Quiet on success, since it almost
		// always rereads what was just loaded.
		reload(true);
		loop {
			watcher.changed().await;
			reload(false);
		}
	}

	async fn run(self, cli: &usage_rs::config::CliLayer) -> anyhow::Result<()> {
		let serve = self.merged(cli, &usage_rs::config::EnvLayer::from_process())?;
		let listen = serve.listener()?;
		let mut server = moq_auth::serve::Server::new(serve.policy()?)?;
		if let Some(secret) = serve.secret()? {
			server = server.with_secret(secret);
		}
		// The device-facing contract runs beside the auth server; either stopping ends both.
		let mut mesh = None;
		if let Some(coordinator) = serve.coordinator()? {
			let addr = serve.mesh.listen;
			let listener = tokio::net::TcpListener::bind(addr)
				.await
				.with_context(|| format!("failed to bind {addr}"))?;
			let addr = listener.local_addr().context("failed to read the bound address")?;
			tracing::info!(%addr, url = ?serve.mesh.url, "mesh coordinator listening");
			mesh = Some((coordinator.clone(), listener));
			server = server.with_mesh(coordinator);
		}
		let reload = self.reload(&serve, &server, cli);
		let mesh = async move {
			match mesh {
				Some((coordinator, listener)) => coordinator.serve(listener).await,
				None => std::future::pending().await,
			}
		};
		let auth = async {
			match listen {
				Listen::Tcp(addr) => {
					let listener = tokio::net::TcpListener::bind(addr)
						.await
						.with_context(|| format!("failed to bind {addr}"))?;
					// Log the bound address, so `--listen 127.0.0.1:0` reports the port the OS picked.
					let addr = listener.local_addr().context("failed to read the bound address")?;
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
			res = reload => res,
		}
	}
}

/// `moq auth mesh`: the mesh owner's commands, sent to the auth server's own listener.
#[derive(usage_rs::Args, Clone, Debug)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Mesh {
	/// The auth server's listener, which serves the owner's API alongside `POST /`:
	/// `http://` for a TCP `--listen`, or `unix:///path` for a unix socket.
	/// With `--listen-secret`, put the secret in the URL as its password: `http://owner:SECRET@127.0.0.1:4440`.
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

		/// How many devices it may enroll.
		#[usage(long, default = "1")]
		uses: std::num::NonZeroU32,
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
			MeshCommand::CreateKey { tag, expires, uses } => {
				let mut new = moq_auth::mesh::NewKey::default();
				new.tags = tag.clone();
				new.expires_in = expires.map(std::time::Duration::from);
				new.uses = *uses;
				client.post(url("/mesh/keys", &[])).json(&new)
			}
			MeshCommand::Keys => client.get(url("/mesh/keys", &[])),
			MeshCommand::RevokeKey { id } => client.delete(url("/mesh/keys", &[id])),
			MeshCommand::Devices => client.get(url("/mesh/devices", &[])),
			MeshCommand::Revoke { endpoint } => client.delete(url("/mesh/devices", &[endpoint])),
		};
		let response = request.send().await.with_context(|| {
			let mut shown = self.url.clone();
			let _ = shown.set_password(None);
			format!("failed to reach {shown}")
		})?;
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
struct UnixTimestamp(kio::clock::Wall);

impl From<UnixTimestamp> for kio::clock::Wall {
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
			kio::clock::Wall::UNIX_EPOCH.saturating_add(std::time::Duration::from_secs(timestamp)),
		))
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	/// Drive the same Usage grammar the binary exposes, rather than building
	/// `Command` directly, so the flags stay part of what's under test.
	async fn run(args: &[&str]) -> anyhow::Result<()> {
		// `main` installs rind's provider before any subcommand, and reqwest reads it.
		moq_tokio::crypto::install_default().unwrap();
		let mut cli =
			crate::args::Invocation::try_parse_from(args.iter().copied()).map_err(|err| anyhow::anyhow!("{err}"))?;
		match cli.stages.remove(0) {
			crate::args::Command::Auth(auth) => auth.run(&cli.settings).await,
			other => anyhow::bail!("parsed something other than `moq auth`: {}", other.name()),
		}
	}

	/// `moq auth serve` with `args`, merged over its file the way `run` merges it, without
	/// the process environment.
	fn merged(args: &[&str]) -> anyhow::Result<(Serve, usage_rs::config::CliLayer)> {
		let (serve, settings) = parsed(args);
		let merged = serve.merged(&settings, &usage_rs::config::EnvLayer::new([]))?;
		Ok((merged, settings))
	}

	/// `moq auth serve` with `args` as the command line gave them, and the settings it gave.
	fn parsed(args: &[&str]) -> (Serve, usage_rs::config::CliLayer) {
		let mut cli = crate::args::Invocation::try_parse_from(args.iter().copied()).unwrap();
		let crate::args::Command::Auth(Args {
			command: Command::Serve(serve),
		}) = cli.stages.remove(0)
		else {
			panic!("expected serve");
		};
		(serve, cli.settings)
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
	fn listen_secret_reads_one_line() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("secret");
		let secret = |text: &str| {
			std::fs::write(&path, text).unwrap();
			serve(&["moq", "auth", "serve", "--listen-secret", path.to_str().unwrap()]).secret()
		};
		assert_eq!(secret("hunter2\n").unwrap().as_deref(), Some("hunter2"));
		assert!(secret("\n").unwrap_err().to_string().contains("empty"));
		assert_eq!(serve(&["moq", "auth", "serve"]).secret().unwrap(), None);
	}

	#[test]
	fn sign_writes_an_hour_of_exp_by_default() {
		let Command::Sign { ttl, unbounded, .. } = parse(&["moq", "auth", "sign", "--key", "k.jwk"]).unwrap().command
		else {
			panic!("expected sign");
		};
		assert_eq!(std::time::Duration::from(ttl), std::time::Duration::from_secs(3600));
		assert!(!unbounded);
		for args in [
			&["--unbounded", "--ttl", "1d"][..],
			&["--unbounded", "--expires", "1"],
			&["--ttl", "1d", "--expires", "1"],
		] {
			let argv: Vec<&str> = ["moq", "auth", "sign", "--key", "k.jwk"]
				.iter()
				.chain(args)
				.copied()
				.collect();
			assert!(parse(&argv).is_err(), "{args:?} must conflict");
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

	#[cfg(unix)]
	#[test]
	fn a_non_utf8_path_merges_without_a_file() {
		use std::os::unix::ffi::OsStrExt;
		let (mut serve, settings) = parsed(&["moq", "auth", "serve", "--key", "unused"]);
		let key = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(b"/keys/\xff.jwk"));
		serve.key = Some(key.clone());
		let merged = serve.merged(&settings, &usage_rs::config::EnvLayer::new([])).unwrap();
		assert_eq!(merged.key, Some(key));
	}

	#[test]
	fn mesh_flags_need_a_mesh_url_and_key() {
		let err = merged(&["moq", "auth", "serve", "--mesh-relay", "https://relay.example"])
			.err()
			.unwrap();
		assert!(err.to_string().contains("--mesh-relay needs --mesh-url"), "{err}");

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
		assert_eq!(flags.mesh.relay.len(), 2);
		assert!(flags.mesh.listen.ip().is_loopback());
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
				"--uses",
				"20",
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

		// Public, anyone could record sessions under a victim's address.
		let err = serve(&["moq", "auth", "serve", "--listen", "0.0.0.0:4440", "--listen-public"])
			.listener()
			.unwrap_err();
		assert!(err.to_string().contains("--listen-secret"), "{err}");
		assert!(matches!(
			serve(&[
				"moq",
				"auth",
				"serve",
				"--listen",
				"0.0.0.0:4440",
				"--listen-public",
				"--listen-secret",
				"secret.txt"
			])
			.listener(),
			Ok(Listen::Tcp(_))
		));
		assert!(
			matches!(serve(&["moq", "auth", "serve"]).listener(), Ok(Listen::Tcp(addr)) if addr.ip().is_loopback())
		);
		assert!(matches!(
			serve(&["moq", "auth", "serve", "--listen", "unix:/run/moq-auth.sock"]).listener(),
			Ok(Listen::Unix(path)) if path == std::path::Path::new("/run/moq-auth.sock")
		));
		let err = serve(&[
			"moq",
			"auth",
			"serve",
			"--listen",
			"unix:/run/moq-auth.sock",
			"--listen-secret",
			"secret.txt",
		])
		.listener()
		.unwrap_err();
		assert!(err.to_string().contains("unix socket"), "{err}");
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
		] {
			let argv: Vec<&str> = ["moq", "auth", "serve"].iter().chain(args).copied().collect();
			let err = serve(&argv).policy().unwrap_err().to_string();
			assert!(err.contains(needs), "{args:?}: {err}");
		}
		let expiring = serve(&["moq", "auth", "serve", "--expires", "1h"]).policy().unwrap();
		assert_eq!(expiring.revalidate, None);
	}

	#[test]
	fn the_settings_registry_matches_the_flags() {
		let drift =
			ServeSettings::SETTINGS_REGISTRY.drift(<Serve as usage_rs::argv::spec::CommandArgs>::SETTINGS_BINDINGS);
		assert!(drift.is_empty(), "{drift:#?}");
	}

	/// The file holds every setting and the certificate rules; a flag beats the file,
	/// and a file key the server does not know is refused.
	#[test]
	fn the_file_merges_under_the_flags() {
		let dir = tempfile::tempdir().unwrap();
		let file = dir.path().join("auth.toml");
		let leaf = "ab".repeat(32);
		std::fs::write(
			&file,
			format!(
				r#"
listen = "0.0.0.0:4440"
listen_public = true
listen_secret = "/etc/moq/auth.secret"
tier = "from-file"
revalidate = "30s"
expires = "2h"

[mtls]
subscribe = ["**"]
link = "peer"

[mtls.certificates.{leaf}]
publish = ["alice/**"]
link = "none"
publish_rate = 1000000

[mesh]
relay = ["https://relay.example"]
"#
			),
		)
		.unwrap();
		let path = file.to_str().unwrap();

		let err = merged(&["moq", "auth", "serve", path]).err().unwrap();
		assert!(err.to_string().contains("mesh.relay needs --mesh-url"), "{err}");

		let (serve, _) = merged(&[
			"moq",
			"auth",
			"serve",
			path,
			"--tier",
			"from-flag",
			"--mesh-url",
			"https://mesh.example",
		])
		.unwrap();
		assert!(matches!(serve.listener(), Ok(Listen::Tcp(addr)) if !addr.ip().is_loopback()));
		assert_eq!(
			serve.listen_secret.as_deref(),
			Some(std::path::Path::new("/etc/moq/auth.secret"))
		);
		assert_eq!(serve.mesh.relay.len(), 1);
		let policy = serve.policy().unwrap();
		assert_eq!(policy.tier.as_deref(), Some("from-flag"));
		assert_eq!(policy.revalidate, Some(std::time::Duration::from_secs(30)));
		assert_eq!(policy.mtls.subscribe, ["**".parse().unwrap()].into_iter().collect());
		assert_eq!(policy.mtls_link, Some(moq_auth::serve::Link::Peer));
		let rule = &policy.certificates[&leaf.parse().unwrap()];
		assert_eq!(rule.publish, Some(["alice/**".parse().unwrap()].into_iter().collect()));
		assert_eq!(rule.subscribe, None);
		assert_eq!(rule.link, Some(moq_auth::serve::RuleLink::None));
		assert_eq!(rule.publish_rate, moq_auth::Rate::from_bps(1_000_000).ok());

		// A flag replaces the file's list rather than adding to it.
		let (serve, _) = merged(&[
			"moq",
			"auth",
			"serve",
			path,
			"--mtls-subscribe",
			"room/**",
			"--mesh-url",
			"https://mesh.example",
		])
		.unwrap();
		assert_eq!(serve.mtls.subscribe, vec!["room/**".parse::<Pattern>().unwrap()]);

		for (body, needs) in [
			("lisen = \"127.0.0.1:1\"", "lisen"),
			("[mtls.certificates.nothex]\npublish = [\"**\"]", "SHA-256"),
			(&format!("[mtls.certificates.{leaf}]\npublsh = [\"**\"]"), "publsh"),
			(&format!("[mtls.certificates.{leaf}]\nlink = \"sideways\""), "sideways"),
		] {
			std::fs::write(&file, body).unwrap();
			let err = format!("{:#}", merged(&["moq", "auth", "serve", path]).err().unwrap());
			assert!(err.contains(needs), "{body}: {err}");
		}

		std::fs::write(
			&file,
			format!("key_set = \"/keys.jwks\"\n[mtls.certificates.{leaf}]\npublish = [\"alice\"]"),
		)
		.unwrap();
		let (serve, _) = merged(&["moq", "auth", "serve", path, "--key", "/key.jwk"]).unwrap();
		let err = serve.policy().unwrap_err().to_string();
		assert!(err.contains("conflict"), "{err}");
		let (serve, _) = merged(&["moq", "auth", "serve", path]).unwrap();
		let err = serve.policy().unwrap_err().to_string();
		assert!(
			err.contains(&format!("mtls.certificates.{leaf}.publish `alice`")),
			"{err}"
		);
	}

	/// A file edit narrows or revokes at the next re-check, and an edit that does not
	/// load leaves the last valid policy answering.
	#[tokio::test]
	async fn an_edit_reloads_the_policy_and_a_bad_one_keeps_the_last() {
		let dir = tempfile::tempdir().unwrap();
		let file = dir.path().join("auth.toml");
		let leaf = "ab".repeat(32);
		let write = |body: &str| std::fs::write(&file, body).unwrap();
		write(&format!("[mtls.certificates.{leaf}]\npublish = [\"**\"]"));
		// Reloads start from the command line, never from a merge that already holds the file.
		let (serve, cli) = parsed(&["moq", "auth", "serve", file.to_str().unwrap()]);
		let env = usage_rs::config::EnvLayer::new([]);
		let server = moq_auth::serve::Server::new(serve.merged(&cli, &env).unwrap().policy().unwrap()).unwrap();

		let mut session = moq_auth::Request::new("relay", moq_auth::Transport::Quic, "/");
		session.tls = Some(moq_auth::Peer {
			name: "alice".into(),
			fingerprint: leaf.clone(),
			chain: vec![leaf.clone()],
			expires: None,
			issuer: "CN=affiliates".into(),
		});
		let publish = |grant: Option<moq_auth::Grant>| grant.unwrap().publish;
		assert_eq!(
			publish(server.answer(&session).await.unwrap()),
			["**".parse().unwrap()].into_iter().collect()
		);

		session.event = moq_auth::Event::Revalidate;
		write(&format!("[mtls.certificates.{leaf}]\npublish = [\"alice/**\"]"));
		serve.apply(&server, &cli, &env).unwrap();
		let narrowed = ["alice/**".parse().unwrap()].into_iter().collect();
		assert_eq!(publish(server.answer(&session).await.unwrap()), narrowed);

		for broken in [
			format!("[mtls.certificates.{leaf}]\npublish = [\"alice/**\""),
			format!("[mtls.certificates.{leaf}]\npublish = [\"alice/**\"]\ntier = \"not a tier\""),
			"revalidate = \"1m\"".to_string(),
		] {
			write(&broken);
			assert!(serve.apply(&server, &cli, &env).is_err(), "{broken}");
			assert_eq!(publish(server.answer(&session).await.unwrap()), narrowed, "{broken}");
		}

		write("");
		serve.apply(&server, &cli, &env).unwrap();
		assert!(server.answer(&session).await.is_err(), "deleting the rule revokes it");
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
		let internal = moq_relay::internal::Internal::new(internal_config, moq_net::stats::Registry::disabled())
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
		let last = kio::clock::Wall::UNIX_EPOCH.saturating_add(std::time::Duration::MAX);
		let parse = |text: &str| kio::clock::Wall::from(text.parse::<UnixTimestamp>().unwrap());
		assert_eq!(parse(&i64::MAX.to_string()), last);
		assert!(parse("18446744073") < last);
	}
}
