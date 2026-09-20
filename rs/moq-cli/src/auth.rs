//! `moq auth`: generate, sign, and verify the JWT tokens a relay authenticates with.
//!
//! The `moq-auth` library underneath stays free of CLI concerns.

use anyhow::Context;
use std::net::SocketAddr;
use std::{io, path::PathBuf};

use moq_auth::serve::{Keys, Policy};
use moq_auth::{Algorithm, Pattern, Permissions};

/// Generate, sign, and verify the JWT tokens a relay authenticates with.
#[derive(usage::Args, Clone, Debug)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Args {
	#[usage(subcommand)]
	command: Command,
}

impl Args {
	/// Run the requested command, writing the key, token, or payload to the chosen
	/// destination (stdout by default). `serve` runs until killed.
	pub async fn run(self) -> anyhow::Result<()> {
		match self.command {
			Command::Serve(serve) => serve.run().await?,
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

			Command::Sessions(filter) => filter.get("/sessions").await?,
			Command::Revalidate(filter) => filter.post("/sessions/revalidate").await?,
		}

		Ok(())
	}
}

#[derive(usage::Subcommands, Clone, Debug)]
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
		#[usage(long, value_hint = usage::ValueHint::FilePath, extensions("jwk", "json"))]
		out: Option<PathBuf>,

		/// Write the key to a directory as {kid}.jwk.
		#[usage(long, conflicts = "--out", value_hint = usage::ValueHint::DirPath)]
		out_dir: Option<PathBuf>,

		/// Write the public key to a file path (asymmetric algorithms only). Use `-` for stdout.
		#[usage(long, value_hint = usage::ValueHint::FilePath, extensions("jwk", "json"))]
		public: Option<PathBuf>,

		/// Write the public key to a directory as {kid}.jwk (asymmetric algorithms only).
		#[usage(long, conflicts = "--public", value_hint = usage::ValueHint::DirPath)]
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
		#[usage(long, value_hint = usage::ValueHint::FilePath, extensions("jwk", "json"))]
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

	/// List live sessions on a relay's internal listener.
	Sessions(SessionFilter),

	/// Ask a relay to re-check matching sessions now.
	Revalidate(SessionFilter),

	/// Verify a token, writing the payload to stdout.
	Verify {
		/// Path to the key file. Use `-` for stdin (requires `--in` to be a file).
		#[usage(long, value_hint = usage::ValueHint::FilePath, extensions("jwk", "json"))]
		key: PathBuf,

		/// Path to read the token from. Use `-` for stdin.
		#[usage(long = "in", default = "-", value_hint = usage::ValueHint::FilePath)]
		token: PathBuf,
	},
}

/// Filter flags shared by `moq auth sessions` and `moq auth revalidate`.
#[derive(usage::Args, Clone, Debug)]
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
#[derive(usage::Args, Clone, Debug)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Serve {
	/// Where to answer: a socket address, or `unix:<path>` for a unix socket.
	#[usage(long, default = "127.0.0.1:4440")]
	listen: String,

	/// Allow a bind on a non-loopback address. The server has no authentication of its own.
	#[usage(long)]
	listen_public: bool,

	/// The key file a `jwt` is verified against.
	#[usage(long, conflicts = "--key-dir", value_hint = usage::ValueHint::FilePath, extensions("jwk", "json"))]
	key: Option<PathBuf>,

	/// A directory of `{kid}.jwk` files, selected by the token's `kid`.
	#[usage(long, conflicts = "--key", value_hint = usage::ValueHint::DirPath)]
	key_dir: Option<PathBuf>,

	/// Patterns an anonymous session may publish (repeatable); `foo/**` for a subtree.
	#[usage(long)]
	public_publish: Vec<Pattern>,

	/// Patterns an anonymous session may subscribe to (repeatable).
	#[usage(long)]
	public_subscribe: Vec<Pattern>,

	/// Patterns a session with a verified certificate may publish (repeatable). Empty refuses certificates.
	#[usage(long)]
	mtls_publish: Vec<Pattern>,

	/// Patterns a session with a verified certificate may subscribe to (repeatable).
	#[usage(long)]
	mtls_subscribe: Vec<Pattern>,

	/// The tier label stamped on every grant, handed to the relay's stats.
	#[usage(long)]
	tier: Option<String>,

	/// How often the relay re-checks each grant.
	#[usage(long, default = "1m")]
	revalidate: crate::duration::Duration,

	/// How long a grant with no bound of its own lasts: anonymous sessions, tokens without `exp`, certificates without one.
	#[usage(long, default = "1d")]
	expires: crate::duration::Duration,

	/// The most live sessions presenting one token.
	#[usage(long)]
	limit_token: Option<usize>,

	/// The most live sessions from one remote address.
	#[usage(long)]
	limit_remote: Option<usize>,
}

impl Serve {
	/// The policy these flags describe, refusing a cadence the relay would spin on.
	fn policy(&self) -> anyhow::Result<Policy> {
		let revalidate = self.revalidate.into_std();
		if revalidate.is_zero() {
			anyhow::bail!("--revalidate must be longer than 0s; every client would re-check in a tight loop");
		}
		let rules = |publish: &[Pattern], subscribe: &[Pattern]| {
			Permissions::new(publish.iter().cloned().collect(), subscribe.iter().cloned().collect())
		};
		let mut policy = Policy::default();
		policy.keys = match (&self.key, &self.key_dir) {
			(Some(key), _) => Some(Keys::File(key.clone())),
			(None, Some(dir)) => Some(Keys::Dir(dir.clone())),
			(None, None) => None,
		};
		policy.public = rules(&self.public_publish, &self.public_subscribe);
		policy.mtls = rules(&self.mtls_publish, &self.mtls_subscribe);
		policy.tier = self.tier.clone();
		policy.revalidate = revalidate;
		policy.expires = self.expires.into_std();
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

	async fn run(self) -> anyhow::Result<()> {
		let listen = self.listener()?;
		let server = moq_auth::serve::Server::new(self.policy()?);
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
		Ok(())
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
		moq_auth::Key::from_str(contents.trim()).context("failed to parse key from stdin")
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
struct UnixTimestamp(std::time::SystemTime);

impl From<UnixTimestamp> for std::time::SystemTime {
	fn from(value: UnixTimestamp) -> Self {
		value.0
	}
}

impl std::str::FromStr for UnixTimestamp {
	type Err = anyhow::Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		let timestamp = s.parse::<i64>().context("expected unix timestamp")?;
		let timestamp = timestamp.try_into().context("timestamp out of range")?;
		// checked_add, because plain `+` panics on overflow and how far a SystemTime
		// reaches is platform-dependent: a timespec holds i64 seconds, while Windows
		// counts 100ns ticks and runs out far sooner.
		let value = std::time::SystemTime::UNIX_EPOCH
			.checked_add(std::time::Duration::from_secs(timestamp))
			.context("timestamp out of range")?;
		Ok(Self(value))
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
		assert_eq!(policy.tier.as_deref(), Some("internal"));
		assert_eq!(policy.revalidate, std::time::Duration::from_secs(30));
		assert_eq!(policy.expires, std::time::Duration::from_secs(7200));
		assert_eq!(policy.limits.token, Some(3));
		assert_eq!(policy.limits.remote, Some(8));

		// Nothing configured is a server that refuses everyone, on the defaults.
		let bare = serve(&["moq", "auth", "serve"]).policy().unwrap();
		assert!(bare.keys.is_none());
		assert!(bare.public.is_empty() && bare.mtls.is_empty());
		assert_eq!(bare.revalidate, std::time::Duration::from_secs(60));
		assert_eq!(bare.expires, std::time::Duration::from_secs(86400));

		// A zero cadence would have every client re-check in a tight loop.
		let err = serve(&["moq", "auth", "serve", "--revalidate", "0s"])
			.policy()
			.unwrap_err();
		assert!(err.to_string().contains("--revalidate"), "{err}");
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

		let listen = std::net::TcpListener::bind("127.0.0.1:0")
			.expect("probe bind")
			.local_addr()
			.expect("probe addr");
		let mut internal_config = moq_relay::internal::Config::default();
		internal_config.listen = Some(listen);
		let internal =
			moq_relay::internal::Internal::new(internal_config, moq_tokio::moq_net::stats::Registry::disabled())
				.with_sessions(sessions);
		tokio::spawn(async move {
			let _ = internal.run().await;
		});

		let url = format!("http://{listen}");
		let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
		while reqwest::Client::new()
			.get(format!("{url}/health"))
			.send()
			.await
			.is_err()
		{
			assert!(std::time::Instant::now() < deadline, "internal listener never came up");
			tokio::time::sleep(std::time::Duration::from_millis(25)).await;
		}

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

	// Whether the largest parseable timestamp is representable depends on the
	// platform's SystemTime, so assert only that it never panics.
	#[test]
	fn timestamp_at_the_maximum_does_not_panic() {
		let _ = i64::MAX.to_string().parse::<UnixTimestamp>();
	}
}
