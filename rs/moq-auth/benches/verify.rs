//! What one token costs `moq auth serve` to verify, split into the work it does.
//!
//! `decide` is [`Policy::decide`] end to end, as the server answers a `connect`: it
//! rereads the configured key source, imports the key, and verifies the token. The
//! `stage` cases time each part alone: reading the file, parsing a key or set, decoding
//! the header, looking up a `kid`, and verifying with a freshly imported (`cold`) or an
//! already used (`warm`) key. `set` sweeps a key set's size against where the `kid`
//! sits, and `refuse` times the refusals a client can provoke.
//!
//! `clients` runs concurrent requests on a multi-threaded runtime, including a burst of
//! refusals and a key replaced and removed while they run, and prints each case's
//! per-request latency percentiles after the run. `sessions` times a `connect` and its
//! `end` against a server whose session limits count other live sessions. `http` is one
//! `connect` as a relay sends it, a `POST /` over loopback TCP, for scale.
//!
//! Validity checks read a [`Manual`](kio::clock::Manual) clock, so every run decides the
//! same way. Keys and tokens are generated once into `VERIFY_FIXTURE` (a fresh temporary
//! directory by default); point repeated runs at one directory so they verify identical
//! signatures. With `VERIFY_ITERATIONS=n`, the bench skips Criterion and runs the case
//! named by `VERIFY_CASE` exactly `n` times, printing its allocations, so an external
//! instruction counter divides by a known count:
//!
//! ```bash
//! cargo bench --manifest-path rs/moq-auth/Cargo.toml --bench verify --no-run
//! VERIFY_FIXTURE=fixture VERIFY_CASE=verify/decide/ES256/file VERIFY_ITERATIONS=1000 \
//!     perf stat -e instructions:u target/release/deps/verify-*
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::VecDeque;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use criterion::Criterion;
use moq_auth::serve::{Denial, Keys, Policy, Server};
use moq_auth::{Algorithm, Bytes, Claims, Event, Grant, Key, KeyId, KeySet, Request, Transport, lease};

/// Counts every allocation on every thread, including tokio's blocking pool that reads files,
/// while [`COUNTING`] is set. Only the fixed-iteration mode sets it, so timed runs share no
/// counter between threads.
struct Counter;

static COUNTING: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED: AtomicU64 = AtomicU64::new(0);

fn count(size: usize) {
	if COUNTING.load(Ordering::Relaxed) {
		ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
		ALLOCATED.fetch_add(size as u64, Ordering::Relaxed);
	}
}

#[global_allocator]
static ALLOCATOR: Counter = Counter;

unsafe impl GlobalAlloc for Counter {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		count(layout.size());
		unsafe { System.alloc(layout) }
	}

	unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
		count(layout.size());
		unsafe { System.alloc_zeroed(layout) }
	}

	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
		count(new_size);
		unsafe { System.realloc(ptr, layout, new_size) }
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		unsafe { System.dealloc(ptr, layout) }
	}
}

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

/// Concurrent requests per iteration of a `clients` case.
const CLIENTS: [usize; 3] = [1, 16, 64];

/// Live sessions in the server's table, for the `sessions` cases.
const SESSIONS: [usize; 3] = [0, 1024, 16384];

/// Per-request latencies kept for each concurrent case's percentiles: the most recent ones.
const SAMPLES: usize = 1 << 16;

/// Every token expires a year after the manual clock's start, or expired a second before it.
fn claims(expires: kio::clock::Wall) -> Claims {
	Claims::default()
		.with_root("demo")
		.with_publish(["**".parse().unwrap()])
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
	key.to_public().unwrap_or_else(|_| key.clone())
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

/// A benchmark case: a name and the work one iteration does.
type Case = (String, Box<dyn FnMut()>);

/// The most recent per-request latencies of one concurrent case.
#[derive(Clone, Default)]
struct Latencies(Arc<Mutex<VecDeque<Duration>>>);

impl Latencies {
	fn record(&self, elapsed: Duration) {
		let mut samples = self.0.lock().unwrap();
		if samples.len() == SAMPLES {
			samples.pop_front();
		}
		samples.push_back(elapsed);
	}

	fn summary(&self) -> Option<String> {
		let mut samples: Vec<Duration> = self.0.lock().unwrap().iter().copied().collect();
		if samples.is_empty() {
			return None;
		}
		samples.sort_unstable();
		let at = |quantile: f64| samples[((samples.len() - 1) as f64 * quantile).round() as usize];
		Some(format!(
			"{} requests: p50 {:?}, p95 {:?}, p99 {:?}, max {:?}",
			samples.len(),
			at(0.5),
			at(0.95),
			at(0.99),
			at(1.0),
		))
	}
}

/// Which outcomes a concurrent case accepts from each request.
type Expect = fn(&Result<Grant, Denial>) -> bool;

struct Bench {
	root: PathBuf,
	runtime: Arc<tokio::runtime::Runtime>,
	/// A multi-threaded runtime for the concurrent cases.
	pool: Arc<tokio::runtime::Runtime>,
	cases: Vec<Case>,
	latencies: Vec<(String, Latencies)>,
}

impl Bench {
	fn read(&self, path: impl AsRef<Path>) -> String {
		std::fs::read_to_string(self.root.join(path)).unwrap()
	}

	fn add(&mut self, name: String, run: impl FnMut() + 'static) {
		self.cases.push((name, Box::new(run)));
	}

	/// Time `Policy::decide` for `token`, asserting it is admitted or refused as expected.
	fn decide(&mut self, name: String, keys: Keys, token: String, admit: bool) {
		let mut policy = Policy::default();
		policy.keys = Some(keys);
		let mut request = Request::new("bench", Transport::Quic, "/demo");
		request.query = Some(format!("jwt={token}"));
		let runtime = self.runtime.clone();
		let decided = runtime.block_on(policy.decide(&request));
		assert_eq!(decided.is_ok(), admit, "{name}: {decided:?}");
		self.add(name, move || {
			black_box(runtime.block_on(policy.decide(black_box(&request))).is_ok());
		});
	}

	/// Time `clients` concurrent `Policy::decide` calls, presenting `tokens` in turn. With
	/// `rotate`, each iteration also replaces or removes that directory's only key while
	/// the requests run.
	fn clients(&mut self, name: String, keys: Keys, tokens: &[String], rotate: Option<PathBuf>, expect: Expect) {
		let clients: usize = name.split('/').nth(2).unwrap().parse().unwrap();
		let mut policy = Policy::default();
		policy.keys = Some(keys);
		let policy = Arc::new(policy);
		let requests: Vec<Arc<Request>> = (0..clients)
			.map(|client| {
				let mut request = Request::new("bench", Transport::Quic, "/demo");
				request.query = Some(format!("jwt={}", tokens[client % tokens.len()]));
				Arc::new(request)
			})
			.collect();
		let latencies = Latencies::default();
		self.latencies.push((name.clone(), latencies.clone()));
		let pool = self.pool.clone();
		let mut version = 0;
		self.add(name, move || {
			version += 1;
			pool.block_on(async {
				let rotation = rotate
					.clone()
					.map(|dir| tokio::task::spawn_blocking(move || rotate_key(&dir, version)));
				let tasks: Vec<_> = requests
					.iter()
					.map(|request| {
						let (policy, request) = (policy.clone(), request.clone());
						tokio::spawn(async move { timed(policy.decide(&request)).await })
					})
					.collect();
				if let Some(rotation) = rotation {
					rotation.await.unwrap();
				}
				for task in tasks {
					let (decided, elapsed) = task.await.unwrap();
					assert!(expect(&decided), "{decided:?}");
					latencies.record(elapsed);
				}
			});
		});
	}

	/// Time one `connect` as a relay asks it: a `POST /` to the server over loopback TCP,
	/// on a kept-alive connection, reading and decoding the grant it answers with.
	fn http(&mut self, name: String, keys: Keys, token: String) {
		let mut policy = Policy::default();
		policy.keys = Some(keys);
		let server = Server::new(policy).unwrap();
		let runtime = self.runtime.clone();
		let listener = runtime.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
		let url = format!("http://{}/", listener.local_addr().unwrap());
		runtime.spawn(async move { server.serve(listener).await });
		let mut request = Request::new("bench", Transport::Quic, "/demo");
		request.query = Some(format!("jwt={token}"));
		let client = reqwest::Client::new();
		let post = move || {
			let response = client.post(&url).json(&request).send();
			async move {
				let response = response.await.unwrap().error_for_status().unwrap();
				response.json::<Grant>().await.unwrap()
			}
		};
		let grant = runtime.block_on(post());
		assert_eq!(grant.publish, ["**".parse().unwrap()].into_iter().collect(), "{name}");
		self.add(name, move || {
			black_box(runtime.block_on(post()));
		});
	}

	/// Time one `connect` and its `end` against a server whose session limits count `live`
	/// other sessions, each from its own address with its own token.
	fn sessions(&mut self, live: usize) {
		let path = self.root.join("HS256").join("key.jwk");
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
			let claims = valid().with_publish([format!("s{index}/**").parse().unwrap()]);
			request.query = Some(format!("jwt={}", key.sign(&claims).unwrap()));
			request
		};
		for index in 0..live {
			self.runtime.block_on(server.answer(&request(index))).unwrap();
		}
		let connect = request(live);
		let mut end = connect.clone();
		end.event = Event::End {
			reason: lease::Reason::Shutdown,
			duration: Duration::ZERO,
			bytes: Bytes::default(),
		};
		let runtime = self.runtime.clone();
		self.add(format!("verify/sessions/{live}"), move || {
			runtime.block_on(async {
				black_box(server.answer(black_box(&connect)).await.unwrap());
				black_box(server.answer(black_box(&end)).await.unwrap());
			});
		});
	}

	fn cases(&mut self) {
		for algorithm in ALGORITHMS {
			let name = format!("{algorithm:?}");
			let dir = self.root.join(&name);
			let token = self.read(dir.join("token"));
			self.decide(
				format!("verify/decide/{name}/file"),
				Keys::File(dir.join("key.jwk")),
				token.clone(),
				true,
			);
			self.decide(
				format!("verify/decide/{name}/dir"),
				Keys::Dir(dir.join("dir")),
				token.clone(),
				true,
			);
			self.decide(
				format!("verify/decide/{name}/set"),
				Keys::Set(dir.join("set.jwks")),
				token.clone(),
				true,
			);

			let text = self.read(dir.join("key.jwk"));
			self.add(format!("verify/stage/{name}/parse_key"), move || {
				black_box(black_box(text.as_str()).parse::<Key>().unwrap());
			});

			// A fresh import derives the crypto material on first use; a used key has it cached.
			let key = Key::from_file(dir.join("key.jwk")).unwrap();
			key.verify(&token).unwrap();
			let jwk = key.export();
			let cold = token.clone();
			self.add(format!("verify/stage/{name}/verify_cold"), move || {
				let key = black_box(jwk.clone()).import().unwrap();
				black_box(key.verify(black_box(&cold)).unwrap());
			});
			let jwk = key.export();
			self.add(format!("verify/stage/{name}/import"), move || {
				black_box(black_box(jwk.clone()).import().unwrap());
			});
			self.add(format!("verify/stage/{name}/verify_warm"), move || {
				black_box(key.verify(black_box(&token)).unwrap());
			});
		}

		let path = self.root.join("ES256").join("key.jwk");
		let runtime = self.runtime.clone();
		let tokio_path = path.clone();
		self.add("verify/stage/read_tokio".into(), move || {
			black_box(runtime.block_on(tokio::fs::read_to_string(&tokio_path)).unwrap());
		});
		self.add("verify/stage/read_std".into(), move || {
			black_box(std::fs::read_to_string(&path).unwrap());
		});
		let token = self.read("ES256/token");
		self.add("verify/stage/header".into(), move || {
			black_box(jsonwebtoken::decode_header(black_box(&token)).unwrap());
		});

		let first = self.read("set/first");
		let missing = self.read("set/missing");
		for size in SET_SIZES {
			let path = self.root.join("set").join(format!("{size}.jwks"));
			let last = self.read(format!("set/{size}-last"));
			for (kid, token, admit) in [
				("first", &first, true),
				("last", &last, true),
				("missing", &missing, false),
			] {
				self.decide(
					format!("verify/set/{size}/{kid}"),
					Keys::Set(path.clone()),
					token.clone(),
					admit,
				);
			}

			let text = self.read(&path);
			let set: KeySet = text.parse().unwrap();
			self.add(format!("verify/stage/parse_set/{size}"), move || {
				black_box(black_box(text.as_str()).parse::<KeySet>().unwrap());
			});
			let kid = format!("k{}", size - 1);
			self.add(format!("verify/stage/lookup/{size}"), move || {
				black_box(set.find_key(black_box(&kid)).unwrap());
			});
		}

		let dir = self.root.join("ES256").join("dir");
		let refusals = ["expired", "forged", "unknown_kid", "missing_kid", "malformed"];
		for refusal in refusals {
			let token = self.read(format!("refuse/{refusal}"));
			self.decide(format!("verify/refuse/{refusal}"), Keys::Dir(dir.clone()), token, false);
		}

		let es256 = [self.read("ES256/token")];
		let spread: Vec<String> = (0..SPREAD).map(|index| self.read(format!("tokens/k{index}"))).collect();
		for clients in CLIENTS {
			let file = Keys::File(self.root.join("ES256").join("key.jwk"));
			self.clients(
				format!("verify/clients/{clients}/file"),
				file,
				&es256,
				None,
				Result::is_ok,
			);
			let spread_dir = Keys::Dir(self.root.join("spread"));
			let name = format!("verify/clients/{clients}/dir{SPREAD}");
			self.clients(name, spread_dir, &spread, None, Result::is_ok);
			let set = Keys::Set(self.root.join("set").join(format!("{SPREAD}.jwks")));
			let name = format!("verify/clients/{clients}/set{SPREAD}");
			self.clients(name, set, &spread, None, Result::is_ok);
		}

		// Tokens from both versions while the key is replaced and removed underneath them.
		let live = self.root.join("rotate").join("live");
		rotate_key(&live, 0);
		let versions = [self.read("rotate/a.jwt"), self.read("rotate/b.jwt")];
		self.clients(
			"verify/clients/16/rotate".into(),
			Keys::Dir(live.clone()),
			&versions,
			Some(live),
			|decided| matches!(decided, Ok(_) | Err(Denial::InvalidToken(_) | Denial::UnknownKey)),
		);
		let refusals: Vec<String> = refusals
			.iter()
			.map(|refusal| self.read(format!("refuse/{refusal}")))
			.collect();
		self.clients(
			"verify/clients/64/refuse".into(),
			Keys::Dir(dir),
			&refusals,
			None,
			Result::is_err,
		);

		for live in SESSIONS {
			self.sessions(live);
		}

		for algorithm in [Algorithm::HS256, Algorithm::ES256] {
			let name = format!("{algorithm:?}");
			let keys = Keys::File(self.root.join(&name).join("key.jwk"));
			let token = self.read(format!("{name}/token"));
			self.http(format!("verify/http/{name}"), keys, token);
		}
	}
}

/// Run `future`, returning its output and the host time it took.
#[expect(
	clippy::disallowed_methods,
	clippy::disallowed_types,
	reason = "request latency is host time; the manual clock only decides validity"
)]
async fn timed<T>(future: impl Future<Output = T>) -> (T, Duration) {
	let start = std::time::Instant::now();
	let output = future.await;
	(output, start.elapsed())
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
	let fixture = std::env::var_os("VERIFY_FIXTURE");
	let temporary = tempfile::tempdir().unwrap();
	let root = fixture.map(PathBuf::from).unwrap_or_else(|| temporary.path().into());
	generate(&root);
	let root = root.canonicalize().unwrap();

	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	let _clock = kio::clock::Manual::new().install(runtime.handle());
	let pool = tokio::runtime::Builder::new_multi_thread()
		.worker_threads(4)
		.enable_all()
		.build()
		.unwrap();
	let _pool_clock = kio::clock::Manual::new().install(pool.handle());
	// Hold the runtime context so validity checks outside `block_on` read the manual clock too.
	let _enter = runtime.enter();
	let mut bench = Bench {
		root,
		runtime: Arc::new(runtime),
		pool: Arc::new(pool),
		cases: Vec::new(),
		latencies: Vec::new(),
	};
	bench.cases();

	if let Some(iterations) = std::env::var_os("VERIFY_ITERATIONS") {
		let iterations: u64 = iterations.to_str().unwrap().parse().unwrap();
		let name = std::env::var("VERIFY_CASE").expect("VERIFY_CASE names the case to run");
		let (_, run) = bench
			.cases
			.iter_mut()
			.find(|(case, _)| *case == name)
			.unwrap_or_else(|| panic!("no case {name}"));
		// One untimed run first, so lazily initialized statics are not counted.
		run();
		COUNTING.store(true, Ordering::Relaxed);
		for _ in 0..iterations {
			run();
		}
		COUNTING.store(false, Ordering::Relaxed);
		let per = |total: &AtomicU64| total.load(Ordering::Relaxed) as f64 / iterations.max(1) as f64;
		println!(
			"{name}: {:.1} allocations, {:.0} bytes allocated per iteration",
			per(&ALLOCATIONS),
			per(&ALLOCATED),
		);
	} else {
		let mut criterion = Criterion::default().configure_from_args();
		for (name, run) in &mut bench.cases {
			criterion.bench_function(name, |b| b.iter(&mut *run));
		}
		criterion.final_summary();
	}

	for (name, latencies) in &bench.latencies {
		if let Some(summary) = latencies.summary() {
			println!("{name}: {summary}");
		}
	}
}
