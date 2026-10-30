//! ACME end to end against an in-process directory that really validates TLS-ALPN-01.
//!
//! These cross real sockets and rustls-acme's own wall-clock polling, so they
//! run on the wall clock and bound every wait with [`TIMEOUT`].

use std::{
	net::SocketAddr,
	sync::{
		Mutex,
		atomic::{AtomicUsize, Ordering},
	},
};

use axum::{
	Router,
	extract::{Path as Route, State},
	http::{StatusCode, header},
	response::{IntoResponse, Response},
	routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::*;
use crate::{Config as RelayConfig, Relay};

const DOMAIN: &str = "relay.test";
const TIMEOUT: Duration = Duration::from_secs(30);

/// A minimal ACME directory over plain HTTP.
///
/// It skips JWS verification, which is rustls-acme's half of the protocol, and
/// keeps the relay's half honest: every challenge is a real TLS-ALPN-01
/// handshake against the relay's HTTPS listener, checked for the
/// `acmeIdentifier` extension before the authorization turns valid.
struct Directory {
	url: Url,
	state: Arc<Server>,
	task: tokio::task::JoinHandle<()>,
}

struct Server {
	base: String,
	ca: rcgen::Issuer<'static, rcgen::KeyPair>,
	ca_der: CertificateDer<'static>,
	ca_pem: String,
	/// The relay's HTTPS listener, where every domain "resolves".
	target: Mutex<Option<SocketAddr>>,
	orders: Mutex<Vec<Order>>,
	/// Successful TLS-ALPN-01 validations.
	validated: AtomicUsize,
	/// Failed TLS-ALPN-01 validations.
	failed: AtomicUsize,
	/// Certificates handed out, in order.
	issued: tokio::sync::watch::Sender<Vec<CertificateDer<'static>>>,
	/// Orders refused once this many certificates were issued.
	refuse_after: AtomicUsize,
	refused: tokio::sync::watch::Sender<usize>,
	/// Issue the first certificate already due for renewal.
	due_first: bool,
}

struct Order {
	domains: Vec<String>,
	/// Each authorization: `None` pending, then whether validation succeeded.
	valid: Vec<Option<bool>>,
	cert: Option<String>,
}

impl Drop for Directory {
	fn drop(&mut self) {
		self.task.abort();
	}
}

impl Directory {
	async fn start(due_first: bool) -> Self {
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let base = format!("http://{}", listener.local_addr().unwrap());

		let ca_key = rcgen::KeyPair::generate().unwrap();
		let mut params = rcgen::CertificateParams::new(Vec::new()).unwrap();
		params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
		params
			.distinguished_name
			.push(rcgen::DnType::CommonName, "moq test ACME");
		let ca = params.self_signed(&ca_key).unwrap();

		let state = Arc::new(Server {
			base: base.clone(),
			ca: rcgen::Issuer::new(params, ca_key),
			ca_der: ca.der().clone(),
			ca_pem: ca.pem(),
			target: Mutex::new(None),
			orders: Mutex::new(Vec::new()),
			validated: AtomicUsize::new(0),
			failed: AtomicUsize::new(0),
			issued: tokio::sync::watch::channel(Vec::new()).0,
			refuse_after: AtomicUsize::new(usize::MAX),
			refused: tokio::sync::watch::channel(0).0,
			due_first,
		});
		let app = Router::new()
			.route("/directory", get(directory))
			.route("/nonce", get(nonce))
			.route("/account", post(account))
			.route("/order", post(new_order))
			.route("/order/{n}", post(order))
			.route("/authz/{n}/{i}", post(authz))
			.route("/challenge/{n}/{i}", post(challenge))
			.route("/finalize/{n}", post(finalize))
			.route("/cert/{n}", post(certificate))
			.with_state(state.clone());
		let task = tokio::spawn(async move {
			axum::serve(listener, app).await.unwrap();
		});
		Self {
			url: format!("{base}/directory").parse().unwrap(),
			state,
			task,
		}
	}

	fn issued(&self) -> Vec<CertificateDer<'static>> {
		self.state.issued.borrow().clone()
	}

	#[cfg(all(target_os = "linux", feature = "_quic"))]
	async fn wait_issued(&self, count: usize) -> Vec<CertificateDer<'static>> {
		let mut issued = self.state.issued.subscribe();
		moq_clock::timeout(TIMEOUT, issued.wait_for(|issued| issued.len() >= count))
			.await
			.expect("the directory never issued")
			.unwrap()
			.clone()
	}
}

impl Server {
	fn order_json(&self, n: usize, order: &Order) -> Value {
		let base = &self.base;
		let status = match &order.cert {
			Some(_) => "valid",
			None if order.valid.contains(&Some(false)) => "invalid",
			None if order.valid.iter().all(|valid| *valid == Some(true)) => "ready",
			None => "pending",
		};
		let mut json = json!({
			"status": status,
			"authorizations": (0..order.domains.len()).map(|i| format!("{base}/authz/{n}/{i}")).collect::<Vec<_>>(),
			"finalize": format!("{base}/finalize/{n}"),
		});
		if order.cert.is_some() {
			json["certificate"] = format!("{base}/cert/{n}").into();
		}
		json
	}

	/// Dial the relay as the domain and check it answers with a challenge certificate.
	async fn validate(&self, domain: &str) -> bool {
		let target = self.target.lock().unwrap().expect("no relay to validate against");
		let mut tls = rustls::ClientConfig::builder_with_provider(moq_tokio::crypto::provider())
			.with_safe_default_protocol_versions()
			.unwrap()
			.dangerous()
			.with_custom_certificate_verifier(Arc::new(Validator))
			.with_no_client_auth();
		tls.alpn_protocols = vec![rustls_acme::acme::ACME_TLS_ALPN_NAME.to_vec()];
		let Ok(stream) = tokio::net::TcpStream::connect(target).await else {
			return false;
		};
		let name = ServerName::try_from(domain.to_string()).unwrap();
		let Ok(stream) = tokio_rustls::TlsConnector::from(Arc::new(tls))
			.connect(name, stream)
			.await
		else {
			return false;
		};
		let (_, conn) = stream.get_ref();
		let Some(leaf) = conn.peer_certificates().and_then(|chain| chain.first()) else {
			return false;
		};
		let (_, cert) = x509_parser::parse_x509_certificate(leaf).unwrap();
		conn.alpn_protocol() == Some(rustls_acme::acme::ACME_TLS_ALPN_NAME)
			&& cert
				.extensions()
				.iter()
				.any(|ext| ext.oid.to_id_string() == "1.3.6.1.5.5.7.1.31")
	}
}

/// Accepts any certificate, since a challenge certificate is self-signed and
/// carries a critical extension webpki refuses; [`Server::validate`] checks it instead.
#[derive(Debug)]
struct Validator;

impl rustls::client::danger::ServerCertVerifier for Validator {
	fn verify_server_cert(
		&self,
		_: &CertificateDer<'_>,
		_: &[CertificateDer<'_>],
		_: &ServerName<'_>,
		_: &[u8],
		_: rustls::pki_types::UnixTime,
	) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
		Ok(rustls::client::danger::ServerCertVerified::assertion())
	}

	fn verify_tls12_signature(
		&self,
		_: &[u8],
		_: &CertificateDer<'_>,
		_: &rustls::DigitallySignedStruct,
	) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
		Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
	}

	fn verify_tls13_signature(
		&self,
		_: &[u8],
		_: &CertificateDer<'_>,
		_: &rustls::DigitallySignedStruct,
	) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
		Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
	}

	fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
		moq_tokio::crypto::provider()
			.signature_verification_algorithms
			.supported_schemes()
	}
}

/// The decoded JWS payload, `Null` for a POST-as-GET.
fn payload(body: &str) -> Value {
	#[serde_with::serde_as]
	#[derive(Deserialize)]
	struct Jws {
		#[serde_as(as = "serde_with::base64::Base64<serde_with::base64::UrlSafe, serde_with::formats::Unpadded>")]
		payload: Vec<u8>,
	}
	let jws: Jws = serde_json::from_str(body).unwrap();
	match jws.payload.is_empty() {
		true => Value::Null,
		false => serde_json::from_slice(&jws.payload).unwrap(),
	}
}

fn created(location: String, body: Value) -> Response {
	(StatusCode::CREATED, [(header::LOCATION, location)], axum::Json(body)).into_response()
}

async fn directory(State(state): State<Arc<Server>>) -> axum::Json<Value> {
	let base = &state.base;
	axum::Json(json!({
		"newNonce": format!("{base}/nonce"),
		"newAccount": format!("{base}/account"),
		"newOrder": format!("{base}/order"),
	}))
}

async fn nonce() -> impl IntoResponse {
	[("replay-nonce", "nonce")]
}

async fn account(State(state): State<Arc<Server>>) -> Response {
	created(format!("{}/account/1", state.base), json!({ "status": "valid" }))
}

async fn new_order(State(state): State<Arc<Server>>, body: String) -> Response {
	if state.issued.borrow().len() >= state.refuse_after.load(Ordering::Relaxed) {
		state.refused.send_modify(|refused| *refused += 1);
		return (StatusCode::SERVICE_UNAVAILABLE, "refusing orders").into_response();
	}
	let domains: Vec<String> = payload(&body)["identifiers"]
		.as_array()
		.unwrap()
		.iter()
		.map(|id| id["value"].as_str().unwrap().to_string())
		.collect();
	let mut orders = state.orders.lock().unwrap();
	let order = Order {
		valid: vec![None; domains.len()],
		domains,
		cert: None,
	};
	let n = orders.len();
	let json = state.order_json(n, &order);
	orders.push(order);
	created(format!("{}/order/{n}", state.base), json)
}

async fn order(State(state): State<Arc<Server>>, Route(n): Route<usize>) -> axum::Json<Value> {
	let orders = state.orders.lock().unwrap();
	axum::Json(state.order_json(n, &orders[n]))
}

async fn authz(State(state): State<Arc<Server>>, Route((n, i)): Route<(usize, usize)>) -> axum::Json<Value> {
	let orders = state.orders.lock().unwrap();
	let order = &orders[n];
	axum::Json(json!({
		"status": match order.valid[i] {
			None => "pending",
			Some(true) => "valid",
			Some(false) => "invalid",
		},
		"identifier": { "type": "dns", "value": order.domains[i] },
		"challenges": [{
			"type": "tls-alpn-01",
			"url": format!("{}/challenge/{n}/{i}", state.base),
			"token": format!("token-{n}-{i}"),
		}],
	}))
}

async fn challenge(State(state): State<Arc<Server>>, Route((n, i)): Route<(usize, usize)>) -> axum::Json<Value> {
	let domain = state.orders.lock().unwrap()[n].domains[i].clone();
	// Like a real directory, one failed attempt invalidates the authorization.
	let valid = state.validate(&domain).await;
	match valid {
		true => state.validated.fetch_add(1, Ordering::Relaxed),
		false => state.failed.fetch_add(1, Ordering::Relaxed),
	};
	state.orders.lock().unwrap()[n].valid[i] = Some(valid);
	axum::Json(json!({
		"type": "tls-alpn-01",
		"url": format!("{}/challenge/{n}/{i}", state.base),
		"token": format!("token-{n}-{i}"),
		"status": if valid { "valid" } else { "invalid" },
	}))
}

async fn finalize(State(state): State<Arc<Server>>, Route(n): Route<usize>, body: String) -> axum::Json<Value> {
	#[serde_with::serde_as]
	#[derive(Deserialize)]
	struct Finalize {
		#[serde_as(as = "serde_with::base64::Base64<serde_with::base64::UrlSafe, serde_with::formats::Unpadded>")]
		csr: Vec<u8>,
	}
	let request: Finalize = serde_json::from_value(payload(&body)).unwrap();
	let mut csr = rcgen::CertificateSigningRequestParams::from_der(&request.csr.into()).unwrap();
	let now = time::OffsetDateTime::now_utc();
	let first = state.issued.borrow().is_empty();
	// Already two thirds through its life, which rustls-acme renews at once.
	(csr.params.not_before, csr.params.not_after) = match first && state.due_first {
		true => (now - time::Duration::hours(1), now + time::Duration::minutes(10)),
		false => (now - time::Duration::hours(1), now + time::Duration::days(90)),
	};
	let leaf = csr.signed_by(&state.ca).unwrap();
	let chain = format!("{}{}", leaf.pem(), state.ca_pem);

	let mut orders = state.orders.lock().unwrap();
	orders[n].cert = Some(chain);
	state.issued.send_modify(|issued| issued.push(leaf.der().clone()));
	axum::Json(state.order_json(n, &orders[n]))
}

async fn certificate(State(state): State<Arc<Server>>, Route(n): Route<usize>) -> String {
	state.orders.lock().unwrap()[n].cert.clone().unwrap()
}

/// A relay that takes its certificate from `directory` for `domains`, keeping state in `dir`.
fn relay_config(directory: &Url, dir: &Path, domains: &[&str]) -> RelayConfig {
	let mut config = RelayConfig::default();
	config.drain_timeout = Some((Duration::ZERO).into());
	config.listen.bind = Some("127.0.0.1:0".parse().unwrap());
	config.web.https.listen = Some("127.0.0.1:0".parse().unwrap());
	config.auth.public = vec![moq_auth::Pattern::all()];
	config.acme.domains = domains.iter().map(|domain| domain.to_string()).collect();
	config.acme.cache = Some(dir.to_path_buf());
	config.acme.directory = Some(directory.clone());
	config
}

/// A relay serving after its first certificate deployed.
struct Running {
	https: SocketAddr,
	quic: SocketAddr,
	trigger: crate::shutdown::Trigger,
	task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Running {
	async fn start(config: RelayConfig, directory: &Directory) -> Self {
		let _ = moq_tokio::crypto::install_default();
		let relay = Relay::load(config).await.unwrap().with_signals(false);
		let https = relay.web_addrs().https.unwrap();
		let quic = relay.quic_addr().unwrap();
		*directory.state.target.lock().unwrap() = Some(https);
		let ready = relay.ready();
		let trigger = relay.shutdown_trigger().clone();
		let task = tokio::spawn(relay.run());
		moq_clock::timeout(TIMEOUT, ready.wait())
			.await
			.expect("the relay never became ready")
			.unwrap();
		Self {
			https,
			quic,
			trigger,
			task,
		}
	}

	async fn stop(self) {
		self.trigger.start();
		self.task.await.unwrap().unwrap();
	}

	/// The leaf a fresh HTTPS handshake serves, trusting only the test CA for `domain`.
	async fn served(&self, directory: &Directory, domain: &str) -> CertificateDer<'static> {
		let mut roots = rustls::RootCertStore::empty();
		roots.add(directory.state.ca_der.clone()).unwrap();
		let tls = rustls::ClientConfig::builder_with_provider(moq_tokio::crypto::provider())
			.with_safe_default_protocol_versions()
			.unwrap()
			.with_root_certificates(roots)
			.with_no_client_auth();
		let stream = tokio::net::TcpStream::connect(self.https).await.unwrap();
		let name = ServerName::try_from(domain.to_string()).unwrap();
		let stream = tokio_rustls::TlsConnector::from(Arc::new(tls))
			.connect(name, stream)
			.await
			.expect("the HTTPS listener must serve the issued certificate");
		stream.get_ref().1.peer_certificates().unwrap()[0].clone().into_owned()
	}

	/// Wait for HTTPS to serve `leaf`, which deploys just after the directory hands it out.
	#[cfg(all(target_os = "linux", feature = "_quic"))]
	async fn wait_served(&self, directory: &Directory, leaf: &CertificateDer<'static>) {
		moq_clock::timeout(TIMEOUT, async {
			while self.served(directory, DOMAIN).await != *leaf {
				moq_clock::sleep(Duration::from_millis(20)).await;
			}
		})
		.await
		.expect("the relay never served the renewed certificate");
	}

	/// Open a QUIC session pinned to `leaf`, which only succeeds if QUIC serves it.
	async fn quic_serves(&self, leaf: &CertificateDer<'static>) {
		let mut config = moq_tokio::connect::Config::default();
		config.tls.fingerprint = vec![fingerprint(leaf)];
		config.bind = Some("127.0.0.1:0".parse().unwrap());
		let client = config.init(Default::default()).unwrap().with_reconnect(false);
		let url: Url = format!("moql://127.0.0.1:{}/", self.quic.port()).parse().unwrap();
		let _session = moq_clock::timeout(TIMEOUT, client.with_reconnect(false).connect(url).established())
			.await
			.expect("QUIC connect timed out")
			.expect("QUIC must serve the same certificate as HTTPS");
	}
}

/// A port that refuses every connection for as long as the socket lives.
///
/// Bound but never listening, so a dial is refused at once. A released port
/// would instead be free for another test process to bind, and a listener
/// there that never answers stalls the dial past every budget.
fn refusing() -> (tokio::net::TcpSocket, SocketAddr) {
	let socket = tokio::net::TcpSocket::new_v4().unwrap();
	socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
	let addr = socket.local_addr().unwrap();
	assert!(
		std::net::TcpListener::bind(addr).is_err(),
		"no other listener can take the refusing port"
	);
	(socket, addr)
}

fn fingerprint(leaf: &CertificateDer<'_>) -> String {
	hex::encode(aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, leaf))
}

/// The first start issues over the HTTPS acceptor, and QUIC serves the same certificate.
#[tokio::test]
async fn first_start_issues_over_https() {
	let directory = Directory::start(false).await;
	let dir = tempfile::tempdir().unwrap();
	let relay = Running::start(relay_config(&directory.url, dir.path(), &[DOMAIN]), &directory).await;

	let issued = directory.issued();
	assert_eq!(issued.len(), 1);
	assert_eq!(directory.state.validated.load(Ordering::Relaxed), 1);
	assert_eq!(relay.served(&directory, DOMAIN).await, issued[0]);
	relay.quic_serves(&issued[0]).await;

	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt;
		let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
		assert_eq!(mode(dir.path()), 0o700);
		let files: Vec<_> = std::fs::read_dir(dir.path())
			.unwrap()
			.map(|entry| entry.unwrap().path())
			.collect();
		assert_eq!(files.len(), 2, "one account key and one certificate: {files:?}");
		for file in files {
			assert_eq!(mode(&file), 0o600, "{} must be owner-only", file.display());
		}
	}
	relay.stop().await;
}

/// A restart with a valid cache serves at once with no challenge, and a cache that
/// misses a domain or came from another directory reissues.
#[tokio::test]
async fn restart_reuses_only_a_matching_cache() {
	let directory = Directory::start(false).await;
	let dir = tempfile::tempdir().unwrap();
	let first = Running::start(relay_config(&directory.url, dir.path(), &[DOMAIN]), &directory).await;
	let leaf = directory.issued()[0].clone();
	first.stop().await;

	let restarted = Running::start(relay_config(&directory.url, dir.path(), &[DOMAIN]), &directory).await;
	assert_eq!(directory.issued().len(), 1, "a valid cache must not reissue");
	assert_eq!(directory.state.validated.load(Ordering::Relaxed), 1);
	assert_eq!(restarted.served(&directory, DOMAIN).await, leaf);
	restarted.stop().await;

	// The same names recased and repeated are one certificate, not a new order.
	let recased = Running::start(
		relay_config(&directory.url, dir.path(), &["RELAY.test", DOMAIN]),
		&directory,
	)
	.await;
	assert_eq!(directory.issued().len(), 1, "a recased domain set must reuse the cache");
	recased.stop().await;

	let widened = Running::start(
		relay_config(&directory.url, dir.path(), &[DOMAIN, "other.test"]),
		&directory,
	)
	.await;
	assert_eq!(directory.issued().len(), 2, "a cache missing a domain must reissue");
	assert_eq!(directory.state.validated.load(Ordering::Relaxed), 3);
	assert_eq!(widened.served(&directory, "other.test").await, directory.issued()[1]);
	widened.stop().await;

	let other = Directory::start(false).await;
	let moved = Running::start(relay_config(&other.url, dir.path(), &[DOMAIN]), &other).await;
	assert_eq!(other.issued().len(), 1, "a cache from another directory must reissue");
	assert_eq!(moved.served(&other, DOMAIN).await, other.issued()[0]);
	moved.stop().await;
}

/// An unreachable directory fails the first start inside the budget, naming the
/// domain and the HTTPS listener.
#[tokio::test]
async fn unreachable_directory_fails_inside_the_budget() {
	let (_refusing, closed) = refusing();
	let url: Url = format!("http://{closed}/directory").parse().unwrap();
	let dir = tempfile::tempdir().unwrap();
	let mut config = relay_config(&url, dir.path(), &[DOMAIN]);
	let budget = Duration::from_secs(2);
	config.acme.timeout = Some((budget).into());

	let relay = Relay::load(config).await.unwrap().with_signals(false);
	let https = relay.web_addrs().https.unwrap();
	let ready = relay.ready();
	let started = moq_clock::now();
	let err = moq_clock::timeout(TIMEOUT, relay.run())
		.await
		.expect("the budget must bound the first issuance")
		.expect_err("no certificate means no relay");
	assert!(started.elapsed() >= budget);
	let message = format!("{err:#}");
	assert!(message.contains(DOMAIN), "{message}");
	assert!(message.contains(&https.to_string()), "{message}");
	assert!(
		ready.wait().await.is_err(),
		"a relay without a certificate never reports ready"
	);
}

/// A stop signal during the first issuance ends startup, since a container's
/// PID 1 ignores one left at its default action until issuance gives up.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_signal_during_issuance_stops_startup() {
	let _signals = crate::shutdown::SIGNALS.lock().await;
	let (_refusing, closed) = refusing();
	let url: Url = format!("http://{closed}/directory").parse().unwrap();
	let dir = tempfile::tempdir().unwrap();
	let mut config = relay_config(&url, dir.path(), &[DOMAIN]);
	config.acme.timeout = Some((Duration::from_secs(60 * 60)).into());

	let relay = Relay::load(config).await.unwrap();
	let ready = relay.ready();
	let run = relay.run();
	tokio::pin!(run);
	// Nothing awaits before issuance, so one poll leaves `run` waiting on it.
	assert!(futures::FutureExt::now_or_never(run.as_mut()).is_none());
	crate::shutdown::raise(libc::SIGTERM).await;
	let started = moq_clock::now();
	run.await.expect("a stop during issuance is not a failure");
	assert_eq!(started.elapsed(), Duration::ZERO, "waited out the issuance");
	assert!(ready.wait().await.is_err(), "reported ready without a certificate");
}

/// Repeated failed validations stop the first issuance short of Let's Encrypt's
/// failed-validation limit, and a restart inside the hour does not try again.
#[tokio::test]
async fn failed_validations_stop_short_of_the_rate_limit() {
	let directory = Directory::start(false).await;
	// Every validation is refused, like an unforwarded port 443.
	let (_refusing, closed) = refusing();
	*directory.state.target.lock().unwrap() = Some(closed);
	let dir = tempfile::tempdir().unwrap();
	let start = || async {
		let mut config = relay_config(&directory.url, dir.path(), &[DOMAIN]);
		config.acme.timeout = Some((TIMEOUT).into());
		let relay = Relay::load(config).await.unwrap().with_signals(false);
		let err = moq_clock::timeout(TIMEOUT, relay.run())
			.await
			.expect("failed validations must end startup before the budget")
			.expect_err("no certificate means no relay");
		format!("{err:#}")
	};

	let message = start().await;
	assert_eq!(directory.state.failed.load(Ordering::Relaxed), FAILURE_LIMIT);
	assert!(
		message.contains("5 failed validations per hostname per hour"),
		"{message}"
	);
	assert!(message.contains(DOMAIN), "{message}");

	let orders = directory.state.orders.lock().unwrap().len();
	let message = start().await;
	assert_eq!(
		directory.state.orders.lock().unwrap().len(),
		orders,
		"a restart within the hour must not order again"
	);
	assert!(
		message.contains("5 failed validations per hostname per hour"),
		"{message}"
	);
}

/// A renewal reaches HTTPS and every QUIC worker through fresh handshakes, without a restart.
#[cfg(all(target_os = "linux", feature = "_quic"))]
#[tokio::test]
async fn renewal_reaches_every_worker() {
	let directory = Directory::start(true).await;
	let dir = tempfile::tempdir().unwrap();
	let mut config = relay_config(&directory.url, dir.path(), &[DOMAIN]);
	config.runtime.workers = Some(2);
	config.runtime.pin = false;
	let relay = Running::start(config, &directory).await;

	let issued = directory.wait_issued(2).await;
	assert_ne!(issued[0], issued[1]);
	relay.wait_served(&directory, &issued[1]).await;
	// Each dial takes a fresh source port, which the kernel spreads over the workers.
	for _ in 0..8 {
		relay.quic_serves(&issued[1]).await;
	}
	relay.stop().await;
}

/// A failed renewal leaves the previous certificate serving.
#[tokio::test]
async fn failed_renewal_keeps_serving() {
	let directory = Directory::start(true).await;
	directory.state.refuse_after.store(1, Ordering::Relaxed);
	let dir = tempfile::tempdir().unwrap();
	let relay = Running::start(relay_config(&directory.url, dir.path(), &[DOMAIN]), &directory).await;

	let mut refused = directory.state.refused.subscribe();
	moq_clock::timeout(TIMEOUT, refused.wait_for(|refused| *refused >= 1))
		.await
		.expect("the relay never tried to renew")
		.unwrap();
	let leaf = directory.issued()[0].clone();
	assert_eq!(relay.served(&directory, DOMAIN).await, leaf);
	relay.quic_serves(&leaf).await;
	relay.stop().await;
}

/// A key and self-signed leaf for [`DOMAIN`] in the cache's PEM layout, valid until `not_after`.
fn cached_pem(not_after: time::OffsetDateTime) -> String {
	let key = rcgen::KeyPair::generate().unwrap();
	let mut params = rcgen::CertificateParams::new(vec![DOMAIN.to_string()]).unwrap();
	params.not_before = not_after - time::Duration::days(90);
	params.not_after = not_after;
	format!("{}{}", key.serialize_pem(), params.self_signed(&key).unwrap().pem())
}

/// An expired cached certificate counts as missing rather than being served.
#[tokio::test]
async fn expired_cache_counts_as_missing() {
	use rustls_acme::CertCache;

	let dir = tempfile::tempdir().unwrap();
	let cache = Cache {
		dir: dir.path().to_path_buf(),
	};
	let domains = vec![DOMAIN.to_string()];
	let now = time::OffsetDateTime::now_utc();

	let expired = cached_pem(now - time::Duration::minutes(1));
	cache.store_cert(&domains, "dir", expired.as_bytes()).await.unwrap();
	assert_eq!(cache.load_cert(&domains, "dir").await.unwrap(), None);

	let current = cached_pem(now + time::Duration::days(1));
	cache.store_cert(&domains, "dir", current.as_bytes()).await.unwrap();
	assert_eq!(
		cache.load_cert(&domains, "dir").await.unwrap().as_deref(),
		Some(current.as_bytes())
	);
}

/// A client for [`DOMAIN`] keeping state in `dir`, whose directory is never contacted.
fn uncontacted(dir: &Path) -> Acme {
	let mut config = RelayConfig::default();
	config.web.https.listen = Some("127.0.0.1:0".parse().unwrap());
	config.acme.domains = vec![DOMAIN.into()];
	config.acme.cache = Some(dir.to_path_buf());
	config.acme.directory = Some("http://127.0.0.1:1/directory".parse().unwrap());
	config
		.acme
		.init(&config.listen.tls, &config.web.https)
		.unwrap()
		.unwrap()
}

/// What rustls-acme yields when the directory marks an authorization invalid.
fn bad_auth() -> rustls_acme::EventError<io::Error, io::Error> {
	rustls_acme::EventError::Order(rustls_acme::OrderError::BadAuth(rustls_acme::acme::Auth {
		status: rustls_acme::acme::AuthStatus::Invalid,
		identifier: rustls_acme::acme::Identifier::Dns(DOMAIN.into()),
		challenges: Vec::new(),
	}))
}

/// Failed renewal validations stop ordering at the startup budget, which a start
/// inside the window honors too, and renewal resumes once the oldest leaves it.
#[moq_clock::test]
async fn renewal_pauses_once_failed_validations_spend_the_budget() {
	let dir = tempfile::tempdir().unwrap();
	let mut acme = uncontacted(dir.path());
	let polled = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
	// rustls-acme retries a failed order within seconds; these come back at once.
	let events = futures::stream::iter([
		Err(bad_auth()),
		// The directory may still fail an authorization rustls-acme stopped waiting on.
		Err(rustls_acme::EventError::Order(
			rustls_acme::OrderError::TooManyAttemptsAuth(DOMAIN.into()),
		)),
		Err(bad_auth()),
		Err(bad_auth()),
		Ok(EventOk::DeployedNewCert),
	])
	.chain(futures::stream::pending())
	.inspect({
		let polled = polled.clone();
		move |_| polled.borrow_mut().push(moq_clock::wall())
	});
	let start = moq_clock::wall();
	let failures = Failures {
		path: acme.failures.path.clone(),
	};
	let renewal = moq_clock::sim::spawn(renew(events, failures, DOMAIN.into()));

	moq_clock::sleep(FAILURE_WINDOW - Duration::from_secs(1)).await;
	assert_eq!(
		*polled.borrow(),
		[start; FAILURE_LIMIT],
		"renewal ordered past the budget"
	);
	let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
	let err = acme
		.issue(&listener)
		.await
		.expect_err("a start without a cached certificate must not order inside the window");
	let message = format!("{err:#}");
	assert!(
		message.contains("5 failed validations per hostname per hour"),
		"{message}"
	);

	moq_clock::sleep(Duration::from_secs(1)).await;
	moq_clock::sim::idle().await;
	let resumed = start + FAILURE_WINDOW;
	assert_eq!(*polled.borrow(), [start, start, start, resumed, resumed]);
	assert!(
		acme.failures.recent().unwrap().is_empty(),
		"a renewal clears the failures"
	);
	renewal.abort();
}

/// A spent budget still starts on a valid cached certificate, and keeps its
/// failures so renewal stays paused instead of ordering again. One rustls-acme
/// cannot deploy refuses instead, since it would order a replacement.
#[tokio::test]
async fn a_spent_budget_still_serves_the_cached_certificate() {
	let dir = tempfile::tempdir().unwrap();
	let mut acme = uncontacted(dir.path());
	let pem = cached_pem(time::OffsetDateTime::now_utc() + time::Duration::days(30));
	Cache::write(&acme.cert, pem.as_bytes()).unwrap();
	for _ in 0..FAILURE_LIMIT {
		acme.failures.record().unwrap();
	}

	let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
	listener.set_nonblocking(true).unwrap();
	moq_clock::timeout(TIMEOUT, acme.issue(&listener))
		.await
		.expect("the cached certificate deploys at once")
		.expect("a valid cached certificate serves despite a spent budget");
	assert_eq!(acme.failures.recent().unwrap().len(), FAILURE_LIMIT);

	// A current leaf without its key, which rustls-acme fails to parse.
	let leaf = &pem[pem.find("-----BEGIN CERTIFICATE-----").unwrap()..];
	Cache::write(&acme.cert, leaf.as_bytes()).unwrap();
	let mut acme = uncontacted(dir.path());
	let err = moq_clock::timeout(TIMEOUT, acme.issue(&listener))
		.await
		.expect("an undeployable cached certificate must not order past the budget")
		.expect_err("an undeployable cached certificate is no certificate");
	let message = format!("{err:#}");
	assert!(
		message.contains("5 failed validations per hostname per hour"),
		"{message}"
	);
}

/// What the config refuses before anything binds.
#[test]
fn config_refuses_what_acme_cannot_serve() {
	let dir = tempfile::tempdir().unwrap();
	let valid = || {
		let mut config = RelayConfig::default();
		config.web.https.listen = Some("127.0.0.1:0".parse().unwrap());
		config.acme.domains = vec![DOMAIN.into()];
		config.acme.cache = Some(dir.path().to_path_buf());
		config
	};
	let init = |config: RelayConfig| {
		config
			.acme
			.init(&config.listen.tls, &config.web.https)
			.map(|acme| acme.is_some())
			.map_err(|err| err.to_string())
	};
	assert_eq!(init(valid()), Ok(true));
	assert_eq!(init(RelayConfig::default()), Ok(false));

	type Mutate = fn(&mut RelayConfig);
	let cases: [(&str, Mutate); 6] = [
		("listen.tls.cert", |config| {
			config.listen.tls.cert = vec!["cert.pem".into()]
		}),
		("web.https.listen", |config| config.web.https.listen = None),
		("web.https.cert", |config| {
			config.web.https.cert = vec!["cert.pem".into()]
		}),
		("wildcard", |config| config.acme.domains = vec!["*.relay.test".into()]),
		("acme.cache", |config| config.acme.cache = None),
		("require acme.domains", |config| config.acme.domains.clear()),
	];
	for (expected, mutate) in cases {
		let mut config = valid();
		mutate(&mut config);
		let err = init(config).expect_err(expected);
		assert!(err.contains(expected), "{expected}: {err}");
	}
}
