//! What one token costs `moq auth serve` to verify, split into the work it does.
//!
//! `decide` is [`Policy::decide`] end to end, as the server answers a `connect`: it
//! rereads the configured key source, imports the key, and verifies the token. The
//! `stage` benchmarks time each part alone: reading the file, parsing a key or set,
//! decoding the header, looking up a `kid`, and verifying with a freshly imported
//! (`verify_cold`) or an already used (`verify_warm`) key. `set` sweeps a key set's size
//! against where the `kid` sits, and `refuse` times the refusals a client can provoke.
//!
//! `clients` runs concurrent requests on a multi-threaded runtime, including a burst of
//! refusals and a key replaced and removed while they run. `sessions` times a `connect`
//! and its `end` against a server whose session limits count other live sessions. `http`
//! is one `connect` as a relay sends it, a `POST /` over loopback TCP, for scale.
//!
//! Validity checks read a [`Manual`](kio::clock::Manual) clock, so every run decides the
//! same way. Keys and tokens are generated once per process into a temporary directory, or
//! into `VERIFY_FIXTURE` when set; point repeated runs at one directory so they verify
//! identical signatures.
//!
//! Run with `cargo bench --manifest-path rs/moq-auth/Cargo.toml --bench verify`.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, OnceLock};
use std::time::Duration;

use moq_auth::serve::{Denial, Keys, Policy, Server};
use moq_auth::{Algorithm, Claims, Event, Grant, Key, KeyId, KeySet, Request, Transport, lease};

const ALGORITHMS: [Algorithm; 12] = [
	Algorithm::HS256,
	Algorithm::HS384,
	Algorithm::HS512,
	Algorithm::ES256,
	Algorithm::ES384,
	Algorithm::RS256,
	Algorithm::RS384,
	Algorithm::RS512,
	Algorithm::PS256,
	Algorithm::PS384,
	Algorithm::PS512,
	Algorithm::EdDSA,
];

/// Key set sizes, all ES256.
const SET_SIZES: [usize; 3] = [1, 16, 256];

/// Keys in the directory and set that concurrent clients spread their tokens over.
const SPREAD: usize = 16;

/// Concurrent requests per call of a `clients` case.
const CLIENTS: [usize; 3] = [1, 16, 64];

/// Live sessions in the server's table, for the `sessions` cases.
const SESSIONS: [usize; 3] = [0, 1024, 16384];

/// Every token expires a year after the manual clock's start, or expired a second before it.
fn claims(expires: kio::clock::Wall) -> Claims {
	Claims::default()
		.with_root("demo")
		.with_publish([moq_auth::Prefix::all()])
		.with_expires(expires)
}

fn valid() -> Claims {
	claims(kio::clock::Manual::WALL_START + Duration::from_secs(365 * 24 * 60 * 60))
}

fn kid(name: &str) -> Option<KeyId> {
	Some(KeyId::decode(name).unwrap())
}

/// The half a server verifies with: the public key, or the shared secret.
fn verifier(key: &Key) -> Key {
	key.public().unwrap_or_else(|_| key.clone())
}

fn write(path: &Path, contents: &str) {
	std::fs::create_dir_all(path.parent().unwrap()).unwrap();
	std::fs::write(path, contents).unwrap();
}

/// Generate the keys and tokens, unless an earlier run left them in `root`.
fn generate(root: &Path) {
	if root.join("ready").exists() {
		return;
	}
	for algorithm in ALGORITHMS {
		let name = format!("{algorithm:?}");
		let id = name.to_lowercase();
		let key = Key::generate(algorithm, kid(&id)).unwrap();
		let public = verifier(&key);
		let dir = root.join(&name);
		std::fs::create_dir_all(dir.join("dir")).unwrap();
		public.to_file(dir.join("key.jwk")).unwrap();
		public.to_file(dir.join("dir").join(format!("{id}.jwk"))).unwrap();
		KeySet {
			keys: vec![Arc::new(public)],
		}
		.to_file(dir.join("set.jwks"))
		.unwrap();
		write(&dir.join("token"), &key.sign(&valid()).unwrap());
		if algorithm == Algorithm::ES256 {
			let expired = claims(kio::clock::Manual::WALL_START - Duration::from_secs(1));
			write(&root.join("refuse").join("expired"), &key.sign(&expired).unwrap());
		}
	}

	let keys: Vec<Key> = (0..*SET_SIZES.last().unwrap())
		.map(|index| Key::generate(Algorithm::ES256, kid(&format!("k{index}"))).unwrap())
		.collect();
	let dir = root.join("set");
	std::fs::create_dir_all(&dir).unwrap();
	for size in SET_SIZES {
		KeySet {
			keys: keys[..size].iter().map(|key| Arc::new(verifier(key))).collect(),
		}
		.to_file(dir.join(format!("{size}.jwks")))
		.unwrap();
		write(
			&dir.join(format!("{size}-last")),
			&keys[size - 1].sign(&valid()).unwrap(),
		);
	}
	write(&root.join("set").join("first"), &keys[0].sign(&valid()).unwrap());
	// A directory of the first keys, each with a token, so concurrent clients spread over kids.
	let spread = root.join("spread");
	std::fs::create_dir_all(&spread).unwrap();
	for (index, key) in keys[..SPREAD].iter().enumerate() {
		verifier(key).to_file(spread.join(format!("k{index}.jwk"))).unwrap();
		write(
			&root.join("tokens").join(format!("k{index}")),
			&key.sign(&valid()).unwrap(),
		);
	}
	// Two versions of one kid, swapped in and out while clients present tokens from both.
	std::fs::create_dir_all(root.join("rotate")).unwrap();
	for version in ["a", "b"] {
		let key = Key::generate(Algorithm::ES256, kid("rotate")).unwrap();
		verifier(&key)
			.to_file(root.join("rotate").join(format!("{version}.jwk")))
			.unwrap();
		write(
			&root.join("rotate").join(format!("{version}.jwt")),
			&key.sign(&valid()).unwrap(),
		);
	}
	let stranger = Key::generate(Algorithm::ES256, kid("missing")).unwrap();
	write(&root.join("set").join("missing"), &stranger.sign(&valid()).unwrap());

	// The other refusals, against the ES256 directory.
	let refuse = root.join("refuse");
	// The directory's kid, another key: the signature fails.
	let forger = Key::generate(Algorithm::ES256, kid("es256")).unwrap();
	write(&refuse.join("forged"), &forger.sign(&valid()).unwrap());
	write(&refuse.join("unknown_kid"), &keys[0].sign(&valid()).unwrap());
	let bare = Key::generate(Algorithm::ES256, None).unwrap();
	write(&refuse.join("missing_kid"), &bare.sign(&valid()).unwrap());
	write(&refuse.join("malformed"), "not-a-jwt");

	write(&root.join("ready"), "");
}

/// A runtime whose clock is the manual one, so validity checks decide alike every run.
struct Runtime {
	runtime: tokio::runtime::Runtime,
	_clock: kio::clock::Installed,
}

impl Runtime {
	fn new(builder: &mut tokio::runtime::Builder) -> Self {
		let runtime = builder.enable_all().build().unwrap();
		let _clock = kio::clock::Manual::new().install(runtime.handle());
		Self { runtime, _clock }
	}
}

/// The runtime every case but `clients` runs on, entered by `main` so validity checks
/// outside `block_on` read the manual clock too.
static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| Runtime::new(&mut tokio::runtime::Builder::new_current_thread()));

/// A multi-threaded runtime for the concurrent cases, started only by them.
static POOL: LazyLock<Runtime> =
	LazyLock::new(|| Runtime::new(tokio::runtime::Builder::new_multi_thread().worker_threads(4)));

fn block_on<F: Future>(future: F) -> F::Output {
	RUNTIME.runtime.block_on(future)
}

/// Where `main` puts the fixture unless `VERIFY_FIXTURE` names a directory.
static TEMPORARY: OnceLock<PathBuf> = OnceLock::new();

/// The generated keys and tokens, built on first use.
fn fixture() -> &'static Path {
	static ROOT: LazyLock<PathBuf> = LazyLock::new(|| match std::env::var_os("VERIFY_FIXTURE") {
		Some(root) => {
			let root = PathBuf::from(root);
			if !root.join("ready").exists() {
				// Concurrent benchmark processes each generate their own and the first rename
				// wins, so none reads a half-written fixture.
				std::fs::create_dir_all(root.parent().unwrap_or(Path::new("."))).unwrap();
				let staged = tempfile::tempdir_in(root.parent().unwrap_or(Path::new("."))).unwrap();
				generate(staged.path());
				let _ = std::fs::rename(staged.path(), &root);
			}
			assert!(
				root.join("ready").exists(),
				"{} holds no complete fixture",
				root.display()
			);
			root.canonicalize().unwrap()
		}
		None => {
			let root = TEMPORARY.get().expect("main sets the temporary directory");
			generate(root);
			root.canonicalize().unwrap()
		}
	});
	&ROOT
}

fn read(path: impl AsRef<Path>) -> String {
	std::fs::read_to_string(fixture().join(path)).unwrap()
}

/// A request presenting `token`.
fn request(token: &str) -> Request {
	let mut request = Request::new("bench", Transport::Quic, "/demo");
	request.query = Some(format!("jwt={token}"));
	request
}

/// A policy verifying with `keys`, and a request it answers.
struct Decide {
	policy: Policy,
	request: Request,
}

/// Asserts `token` is admitted or refused as expected before any call.
fn decider(keys: Keys, token: String, admit: bool) -> Decide {
	let mut policy = Policy::default();
	policy.keys = Some(keys);
	let request = request(&token);
	let decided = block_on(policy.decide(&request));
	assert_eq!(decided.is_ok(), admit, "{decided:?}");
	Decide { policy, request }
}

/// One algorithm's key, in one of the three places a server reads keys from.
#[derive(Clone, Copy)]
struct Source {
	algorithm: Algorithm,
	keys: &'static str,
}

impl fmt::Display for Source {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{:?}/{}", self.algorithm, self.keys)
	}
}

fn sources() -> Vec<Source> {
	ALGORITHMS
		.into_iter()
		.flat_map(|algorithm| ["file", "dir", "set"].map(|keys| Source { algorithm, keys }))
		.collect()
}

fn source(source: Source) -> Decide {
	let dir = fixture().join(format!("{:?}", source.algorithm));
	let keys = match source.keys {
		"file" => Keys::File(dir.join("key.jwk")),
		"dir" => Keys::Dir(dir.join("dir")),
		_ => Keys::Set(dir.join("set.jwks")),
	};
	decider(keys, read(dir.join("token")), true)
}

#[swole::bench(args = sources(), setup = source)]
fn decide(input: &Decide) -> Result<Grant, Denial> {
	block_on(input.policy.decide(&input.request))
}

mod stage {
	use moq_auth::{Algorithm, Jwk, Key, KeySet};
	use swole::bench;

	use super::{ALGORITHMS, SET_SIZES, block_on, fixture, read};

	fn key_text(algorithm: Algorithm) -> String {
		read(format!("{algorithm:?}/key.jwk"))
	}

	#[bench(args = ALGORITHMS, setup = key_text)]
	fn parse_key(text: &str) -> Key {
		text.parse().unwrap()
	}

	/// A used key, so it holds its crypto material, and a token it verifies.
	fn used(algorithm: Algorithm) -> (Key, String) {
		let key = Key::from_file(fixture().join(format!("{algorithm:?}/key.jwk"))).unwrap();
		let token = read(format!("{algorithm:?}/token"));
		key.verify(&token).unwrap();
		(key, token)
	}

	fn exported(algorithm: Algorithm) -> (Jwk, String) {
		let (key, token) = used(algorithm);
		(key.export(), token)
	}

	/// A fresh import derives the crypto material on first use.
	#[bench(args = ALGORITHMS, setup = exported)]
	fn verify_cold((jwk, token): &(Jwk, String)) -> moq_auth::Claims {
		let key = jwk.clone().import().unwrap();
		key.verify(token).unwrap()
	}

	#[bench(args = ALGORITHMS, setup = exported)]
	fn import((jwk, _): &(Jwk, String)) -> Key {
		jwk.clone().import().unwrap()
	}

	#[bench(args = ALGORITHMS, setup = used)]
	fn verify_warm((key, token): &(Key, String)) -> moq_auth::Claims {
		key.verify(token).unwrap()
	}

	fn es256_key() -> std::path::PathBuf {
		fixture().join("ES256").join("key.jwk")
	}

	#[bench(setup = es256_key)]
	fn read_tokio(path: &std::path::Path) -> String {
		block_on(tokio::fs::read_to_string(path)).unwrap()
	}

	#[bench(setup = es256_key)]
	fn read_std(path: &std::path::Path) -> String {
		std::fs::read_to_string(path).unwrap()
	}

	fn es256_token() -> String {
		read("ES256/token")
	}

	#[bench(setup = es256_token)]
	fn header(token: &str) -> jsonwebtoken::Header {
		jsonwebtoken::decode_header(token).unwrap()
	}

	fn set_text(size: usize) -> String {
		read(format!("set/{size}.jwks"))
	}

	#[bench(args = SET_SIZES, setup = set_text)]
	fn parse_set(text: &str) -> KeySet {
		text.parse().unwrap()
	}

	/// A set and the `kid` of its last key.
	fn set(size: usize) -> (KeySet, String) {
		(set_text(size).parse().unwrap(), format!("k{}", size - 1))
	}

	#[bench(args = SET_SIZES, setup = set)]
	fn lookup((set, kid): &(KeySet, String)) -> std::sync::Arc<Key> {
		set.find(kid).unwrap()
	}
}

/// A set's size and which `kid` the token carries.
#[derive(Clone, Copy)]
struct Lookup {
	size: usize,
	kid: &'static str,
}

impl fmt::Display for Lookup {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}/{}", self.size, self.kid)
	}
}

fn lookups() -> Vec<Lookup> {
	SET_SIZES
		.into_iter()
		.flat_map(|size| ["first", "last", "missing"].map(|kid| Lookup { size, kid }))
		.collect()
}

fn lookup(lookup: Lookup) -> Decide {
	let token = match lookup.kid {
		"last" => read(format!("set/{}-last", lookup.size)),
		kid => read(format!("set/{kid}")),
	};
	let keys = Keys::Set(fixture().join("set").join(format!("{}.jwks", lookup.size)));
	decider(keys, token, lookup.kid != "missing")
}

#[swole::bench(args = lookups(), setup = lookup)]
fn set(input: &Decide) -> Result<Grant, Denial> {
	block_on(input.policy.decide(&input.request))
}

const REFUSALS: [&str; 5] = ["expired", "forged", "unknown_kid", "missing_kid", "malformed"];

fn refusal(refusal: &str) -> Decide {
	let keys = Keys::Dir(fixture().join("ES256").join("dir"));
	decider(keys, read(format!("refuse/{refusal}")), false)
}

#[swole::bench(args = REFUSALS, setup = refusal)]
fn refuse(input: &Decide) -> Result<Grant, Denial> {
	block_on(input.policy.decide(&input.request))
}

/// How many concurrent requests, against which keys.
#[derive(Clone, Copy)]
struct Crowd {
	clients: usize,
	keys: &'static str,
}

impl fmt::Display for Crowd {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}/{}", self.clients, self.keys)
	}
}

/// Which outcomes a concurrent case accepts from each request.
type Expect = fn(&Result<Grant, Denial>) -> bool;

/// A shared policy and the requests one call sends it at once. With `rotate`, each call also
/// replaces or removes that directory's only key while the requests run.
struct Clients {
	policy: Arc<Policy>,
	requests: Vec<Arc<Request>>,
	rotate: Option<PathBuf>,
	expect: Expect,
	version: usize,
}

fn crowds() -> Vec<Crowd> {
	let mut crowds: Vec<Crowd> = CLIENTS
		.into_iter()
		.flat_map(|clients| ["file", "dir16", "set16"].map(|keys| Crowd { clients, keys }))
		.collect();
	crowds.push(Crowd {
		clients: 16,
		keys: "rotate",
	});
	crowds.push(Crowd {
		clients: 64,
		keys: "refuse",
	});
	crowds
}

fn crowd(crowd: Crowd) -> Clients {
	let root = fixture();
	let spread = || (0..SPREAD).map(|index| read(format!("tokens/k{index}"))).collect();
	let (keys, tokens, rotate, expect): (Keys, Vec<String>, Option<PathBuf>, Expect) = match crowd.keys {
		"file" => (
			Keys::File(root.join("ES256").join("key.jwk")),
			vec![read("ES256/token")],
			None,
			Result::is_ok,
		),
		"dir16" => (Keys::Dir(root.join("spread")), spread(), None, Result::is_ok),
		"set16" => (
			Keys::Set(root.join("set").join(format!("{SPREAD}.jwks"))),
			spread(),
			None,
			Result::is_ok,
		),
		// Tokens from both versions while the key is replaced and removed underneath them.
		"rotate" => {
			let live = root.join("rotate").join("live");
			rotate_key(&live, 0);
			(
				Keys::Dir(live.clone()),
				vec![read("rotate/a.jwt"), read("rotate/b.jwt")],
				Some(live),
				|decided| matches!(decided, Ok(_) | Err(Denial::InvalidToken(_) | Denial::UnknownKey)),
			)
		}
		_ => (
			Keys::Dir(root.join("ES256").join("dir")),
			REFUSALS
				.iter()
				.map(|refusal| read(format!("refuse/{refusal}")))
				.collect(),
			None,
			Result::is_err,
		),
	};
	let mut policy = Policy::default();
	policy.keys = Some(keys);
	let requests = (0..crowd.clients)
		.map(|client| Arc::new(request(&tokens[client % tokens.len()])))
		.collect();
	Clients {
		policy: Arc::new(policy),
		requests,
		rotate,
		expect,
		version: 0,
	}
}

#[swole::bench(args = crowds(), setup = crowd)]
fn clients(input: &mut Clients) {
	input.version += 1;
	let version = input.version;
	POOL.runtime.block_on(async {
		let rotation = input
			.rotate
			.clone()
			.map(|dir| tokio::task::spawn_blocking(move || rotate_key(&dir, version)));
		let tasks: Vec<_> = input
			.requests
			.iter()
			.map(|request| {
				let (policy, request) = (input.policy.clone(), request.clone());
				tokio::spawn(async move { policy.decide(&request).await })
			})
			.collect();
		if let Some(rotation) = rotation {
			rotation.await.unwrap();
		}
		for task in tasks {
			let decided = task.await.unwrap();
			assert!((input.expect)(&decided), "{decided:?}");
		}
	});
}

/// A server whose session limits count other live sessions, and one more session's `connect`
/// and `end`.
struct Sessions {
	server: Server,
	connect: Request,
	end: Request,
}

/// A server already holding `live` sessions, each from its own address with its own token.
fn live(live: usize) -> Sessions {
	let path = fixture().join("HS256").join("key.jwk");
	let key = Key::from_file(&path).unwrap();
	let mut policy = Policy::default();
	policy.keys = Some(Keys::File(path));
	policy.limits.token = Some(usize::MAX);
	policy.limits.remote = Some(usize::MAX);
	policy.revalidate = Some(Duration::from_secs(60));
	policy.expires = Some(Duration::from_secs(3600));
	let server = Server::new(policy).unwrap();
	let request = |index: usize| {
		let [_, a, b, c] = (index as u32).to_be_bytes();
		let mut request = Request::new("bench", Transport::Quic, "/demo");
		request.remote = Some(([10, a, b, c], 4433).into());
		let claims = valid().with_publish([format!("s{index}").parse().unwrap()]);
		request.query = Some(format!("jwt={}", key.sign(&claims).unwrap()));
		request
	};
	for index in 0..live {
		block_on(server.answer(&request(index))).unwrap();
	}
	let connect = request(live);
	let mut end = connect.clone();
	end.event = Event::End {
		reason: lease::Reason::Shutdown,
		duration: Duration::ZERO,
		bytes: moq_auth::Bytes::default(),
	};
	Sessions { server, connect, end }
}

/// One `connect` and its `end`.
#[swole::bench(args = SESSIONS, setup = live)]
fn sessions(input: &Sessions) -> (Option<Grant>, Option<Grant>) {
	block_on(async {
		let connect = input.server.answer(&input.connect).await.unwrap();
		let end = input.server.answer(&input.end).await.unwrap();
		(connect, end)
	})
}

/// A kept-alive client of a server on loopback TCP, and the `connect` it posts.
struct Http {
	client: reqwest::Client,
	url: String,
	request: Request,
}

impl Http {
	async fn post(&self) -> Grant {
		let response = self.client.post(&self.url).json(&self.request).send().await.unwrap();
		response.error_for_status().unwrap().json::<Grant>().await.unwrap()
	}
}

fn server(algorithm: Algorithm) -> Http {
	let name = format!("{algorithm:?}");
	let mut policy = Policy::default();
	policy.keys = Some(Keys::File(fixture().join(&name).join("key.jwk")));
	let server = Server::new(policy).unwrap();
	let listener = block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
	let url = format!("http://{}/", listener.local_addr().unwrap());
	RUNTIME.runtime.spawn(async move { server.serve(listener).await });
	// reqwest reads rustls's process default, which a binary's main installs.
	moq_crypto::provider::install_default().unwrap();
	let http = Http {
		client: reqwest::Client::new(),
		url,
		request: request(&read(format!("{name}/token"))),
	};
	let grant = block_on(http.post());
	assert_eq!(grant.publish.path, [moq_auth::Prefix::all()].into_iter().collect());
	http
}

/// One `connect` as a relay asks it: a `POST /`, reading and decoding the grant it answers.
#[swole::bench(args = [Algorithm::HS256, Algorithm::ES256], setup = server)]
fn http(input: &Http) -> Grant {
	block_on(input.post())
}

/// Install version `a` or `b` of the rotating key in `dir` by an atomic rename, or remove it,
/// in turn.
fn rotate_key(dir: &Path, version: usize) {
	std::fs::create_dir_all(dir).unwrap();
	let live = dir.join("rotate.jwk");
	let source = match version % 3 {
		0 => "a.jwk",
		1 => "b.jwk",
		_ => {
			let _ = std::fs::remove_file(&live);
			return;
		}
	};
	let staged = dir.join(format!("{version}.tmp"));
	std::fs::copy(dir.parent().unwrap().join(source), &staged).unwrap();
	std::fs::rename(&staged, &live).unwrap();
}

fn main() {
	let temporary = tempfile::tempdir().unwrap();
	TEMPORARY.set(temporary.path().into()).unwrap();
	let _enter = RUNTIME.runtime.enter();
	swole::main();
}
