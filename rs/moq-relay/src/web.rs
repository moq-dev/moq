use std::{
	future::Future,
	net,
	path::PathBuf,
	pin::Pin,
	sync::{Arc, atomic::AtomicU64},
	task::{Context, Poll, ready},
};

use anyhow::Context as _;
use axum::{
	Router,
	body::Body,
	extract::{Extension, Path, Query, State},
	http::{self, Method, StatusCode},
	response::{Html, IntoResponse, Response},
	routing::get,
};
use axum_server::{
	accept::{Accept, DefaultAcceptor},
	tls_rustls::{RustlsAcceptor, RustlsConfig},
};
use bytes::Bytes;
use clap::Parser;
use futures::{FutureExt, future::BoxFuture};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::server::TlsStream;
use tower_http::cors::{Any, CorsLayer};
use tower_service::Service;

use crate::{Auth, AuthParams, Cluster};

/// Configuration for the HTTP/HTTPS web server.
#[derive(Parser, Clone, Debug, serde::Deserialize, serde::Serialize, Default)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct WebConfig {
	/// Plain HTTP listener settings.
	#[command(flatten)]
	#[serde(default)]
	pub http: HttpConfig,

	/// HTTPS listener settings with TLS.
	#[command(flatten)]
	#[serde(default)]
	pub https: HttpsConfig,

	/// If true (default), expose a WebTransport compatible WebSocket polyfill.
	#[arg(long = "web-ws", env = "MOQ_WEB_WS", default_value = "true")]
	#[serde(default = "default_true")]
	pub ws: bool,
}

/// Plain HTTP listener configuration.
#[derive(clap::Args, Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct HttpConfig {
	/// Socket address to bind the HTTP listener to.
	#[arg(long = "web-http-listen", id = "http-listen", env = "MOQ_WEB_HTTP_LISTEN")]
	pub listen: Option<net::SocketAddr>,
}

/// HTTPS listener configuration with TLS certificates and keys.
#[serde_with::serde_as]
#[derive(clap::Args, Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct HttpsConfig {
	/// Socket address to bind the HTTPS listener to.
	#[arg(long = "web-https-listen", id = "web-https-listen", env = "MOQ_WEB_HTTPS_LISTEN", requires_all = ["web-https-cert", "web-https-key"])]
	pub listen: Option<net::SocketAddr>,

	/// Load the given certificate chain files from disk.
	///
	/// In config files, accepts either a single string or a TOML array.
	#[arg(
		long = "web-https-cert",
		id = "web-https-cert",
		value_delimiter = ',',
		env = "MOQ_WEB_HTTPS_CERT"
	)]
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	#[serde_as(as = "serde_with::OneOrMany<_>")]
	pub cert: Vec<PathBuf>,

	/// Load the given private key files from disk.
	///
	/// Each key is paired with the certificate chain at the same index.
	/// In config files, accepts either a single string or a TOML array.
	#[arg(
		long = "web-https-key",
		id = "web-https-key",
		value_delimiter = ',',
		env = "MOQ_WEB_HTTPS_KEY"
	)]
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	#[serde_as(as = "serde_with::OneOrMany<_>")]
	pub key: Vec<PathBuf>,

	/// PEM file(s) of root CAs for validating optional client certificates (mTLS).
	///
	/// When set, clients *may* present a certificate during the TLS handshake.
	/// A verified peer is granted full publish/subscribe access scoped to the
	/// URL path without a JWT, mirroring the QUIC server's `--server-tls-root`
	/// behavior. Clients that don't present a cert continue through the normal
	/// JWT path.
	///
	/// In config files, accepts either a single string or a TOML array.
	#[arg(
		long = "web-https-root",
		id = "web-https-root",
		value_delimiter = ',',
		env = "MOQ_WEB_HTTPS_ROOT"
	)]
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	#[serde_as(as = "serde_with::OneOrMany<_>")]
	pub root: Vec<PathBuf>,
}

/// Shared state passed to all web handler routes. An internal detail: callers
/// build a [`Web`] from its parts via [`Web::new`] rather than constructing this.
pub(crate) struct WebState {
	/// The authenticator for verifying incoming requests.
	pub(crate) auth: Auth,
	/// The cluster state for resolving origins.
	pub(crate) cluster: Cluster,
	/// TLS certificate information served at `/certificate.sha256`.
	pub(crate) tls_info: Arc<std::sync::RwLock<moq_native::tls::Info>>,
	/// Monotonically increasing connection counter for WebSocket sessions.
	#[cfg_attr(not(feature = "websocket"), allow(dead_code))]
	pub(crate) conn_id: AtomicU64,
}

/// Run a HTTP server using Axum
pub struct Web {
	state: Arc<WebState>,
	config: WebConfig,
}

impl Web {
	/// Build a web server from its parts. `tls_info` is the relay's TLS
	/// certificate info (e.g. `server.tls_info()`), served at
	/// `/certificate.sha256`.
	pub fn new(
		auth: Auth,
		cluster: Cluster,
		tls_info: Arc<std::sync::RwLock<moq_native::tls::Info>>,
		config: WebConfig,
	) -> Self {
		let state = Arc::new(WebState {
			auth,
			cluster,
			tls_info,
			conn_id: AtomicU64::new(0),
		});
		Self { state, config }
	}

	/// Build the default web router with `state` applied, returning a
	/// state-erased [`Router`] an embedder can extend (`merge`/`nest` extra
	/// routes) before handing it to [`serve`](Self::serve).
	///
	/// Includes the WebSocket polyfill catch-all (`/{*path}`, when
	/// `config.ws`) and CORS scoped to its own GET routes, but NOT the
	/// landing-page fallback (that is global, so [`serve`](Self::serve) sets it
	/// once across the merged router). Extra routes a caller merges in keep their
	/// own layers and bring their own CORS as needed (e.g. a WHIP POST endpoint).
	pub fn routes(&self) -> Router {
		let app = Router::new()
			.route("/health", get(serve_health))
			.route("/certificate.sha256", get(serve_fingerprint))
			.route("/announced", get(serve_announced))
			.route("/announced/{*prefix}", get(serve_announced))
			.route("/fetch/{*path}", get(serve_fetch));

		// If WebSocket is enabled, add the WebSocket route. Both `/` and
		// `/{*path}` map to the same handler so a client that dials a bare
		// `host:port` with no path (e.g. `moqsink url="https://host:4443"`)
		// still gets a WebSocket upgrade at the empty (root) auth scope. Without
		// the root route, axum's wildcard never matches `/`, the request falls
		// through to the landing page, and the client's WS fallback is silently
		// dead.
		#[cfg(feature = "websocket")]
		let app = match self.config.ws {
			true => app
				.route("/", axum::routing::any(crate::websocket::serve_ws))
				.route("/{*path}", axum::routing::any(crate::websocket::serve_ws)),
			false => app,
		};

		app.layer(CorsLayer::new().allow_origin(Any).allow_methods([Method::GET]))
			.with_state(self.state.clone())
	}

	/// Serve `app` on the configured HTTP/HTTPS listeners until they shut down.
	///
	/// Applies the landing-page fallback (so an unmatched route renders the
	/// informational page rather than a bare 404) and owns the listener +
	/// TLS machinery: optional mTLS client-cert extraction and hot cert
	/// reload. The caller builds `app` from [`routes`](Self::routes) plus any
	/// extra routes it merged in.
	pub async fn serve(self, app: Router) -> anyhow::Result<()> {
		let config = self.config;
		let app = app.fallback(serve_landing).into_make_service();

		let http = if let Some(listen) = config.http.listen {
			// Dual-stack so the cert endpoint + WebSocket fallback answer over IPv4
			// too, even on Windows where `[::]` is IPv6-only by default.
			let listener = moq_native::bind::tcp(listen).context("failed to bind HTTP listener")?;
			let server = axum_server::from_tcp(listener)?;
			Some(server.serve(app.clone()))
		} else {
			None
		};

		let https = if let Some(listen) = config.https.listen {
			let cert = config.https.cert.clone();
			let key = config.https.key.clone();
			let root = config.https.root.clone();

			let rustls = build_https_config(&cert, &key, &root)?;
			let rustls_config = RustlsConfig::from_config(rustls);

			tokio::spawn(reload_https_config(rustls_config.clone(), cert, key, root));

			// MtlsAcceptor surfaces a verified peer cert as a request extension.
			// When no client CA is configured, the inner verifier is `NoClientAuth`
			// and `peer_certificates()` always returns None — the wrapper is then
			// a near-no-op, but keeping a single path simplifies reload + serve.
			let acceptor = MtlsAcceptor {
				inner: RustlsAcceptor::new(rustls_config),
			};
			let listener = moq_native::bind::tcp(listen).context("failed to bind HTTPS listener")?;
			let server = axum_server::from_tcp(listener)?.acceptor(acceptor);
			Some(server.serve(app))
		} else {
			None
		};

		tokio::select! {
			Some(res) = async move { Some(http?.await) } => res?,
			Some(res) = async move { Some(https?.await) } => res?,
			else => {},
		};

		Ok(())
	}

	/// Runs the default router on the configured listeners until they shut
	/// down. Convenience for the standalone binary; equivalent to
	/// `web.serve(web.routes())`.
	pub async fn run(self) -> anyhow::Result<()> {
		let app = self.routes();
		self.serve(app).await
	}
}

/// Build a [`rustls::ServerConfig`] for the HTTPS listener.
///
/// TLS version is left at the rustls default (1.2 + 1.3) so older clients
/// can still hit the HTTPS API; the QUIC server separately forces 1.3.
fn build_https_config(
	cert: &[PathBuf],
	key: &[PathBuf],
	root: &[PathBuf],
) -> anyhow::Result<Arc<rustls::ServerConfig>> {
	anyhow::ensure!(
		!cert.is_empty(),
		"web.https.cert must include at least one certificate when web.https.listen is configured"
	);
	anyhow::ensure!(
		cert.len() == key.len(),
		"web.https.cert and web.https.key must have the same number of entries"
	);

	let mut tls = moq_native::tls::Server::default();
	tls.cert = cert.to_vec();
	tls.key = key.to_vec();
	tls.root = root.to_vec();

	tls.server_config(vec![b"h2".to_vec(), b"http/1.1".to_vec()])
		.context("failed to build https TLS config")
}

/// Reload the HTTPS cert/key/root whenever they change on disk.
///
/// `RustlsConfig::reload_from_pem_file` would rebuild with `with_no_client_auth`
/// (silently stripping mTLS when configured), so we always rebuild via the full
/// [`build_https_config`] path.
async fn reload_https_config(config: RustlsConfig, cert: Vec<PathBuf>, key: Vec<PathBuf>, root: Vec<PathBuf>) {
	let paths: Vec<PathBuf> = cert
		.iter()
		.cloned()
		.chain(key.iter().cloned())
		.chain(root.iter().cloned())
		.collect();

	let mut watcher = match moq_native::watch::FileWatcher::new(&paths) {
		Ok(watcher) => watcher,
		Err(err) => {
			tracing::error!(%err, "failed to watch web certificate files; hot reload disabled");
			return;
		}
	};

	loop {
		watcher.changed().await;
		tracing::info!("reloading web certificate");

		match build_https_config(&cert, &key, &root) {
			Ok(new) => config.reload_from_config(new),
			Err(err) => tracing::warn!(%err, "failed to reload web certificate"),
		}
	}
}

/// Marker inserted as a request extension after HTTPS mTLS verifies a client certificate.
///
/// Embedded routes can extract `Option<Extension<MtlsPeer>>` to mirror the
/// built-in relay handlers, then call [`Auth::verify_mtls`] with their route
/// path when the marker is present.
#[derive(Clone, Debug)]
pub struct MtlsPeer;

/// Wraps [`RustlsAcceptor`] so that, after the TLS handshake, we extract the
/// peer cert presence from rustls's `ServerConnection` and attach it to every
/// request on this connection as `Extension<Option<MtlsPeer>>`.
#[derive(Clone)]
struct MtlsAcceptor {
	inner: RustlsAcceptor<DefaultAcceptor>,
}

impl<I, S> Accept<I, S> for MtlsAcceptor
where
	I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
	S: Send + 'static,
{
	type Stream = TlsStream<I>;
	type Service = SetMtlsExtension<S>;
	type Future = BoxFuture<'static, std::io::Result<(Self::Stream, Self::Service)>>;

	fn accept(&self, stream: I, service: S) -> Self::Future {
		let inner = self.inner.accept(stream, service);
		async move {
			let (tls, service) = inner.await?;
			let peer = tls
				.get_ref()
				.1
				.peer_certificates()
				.filter(|certs| !certs.is_empty())
				.map(|_| MtlsPeer);
			Ok((tls, SetMtlsExtension { inner: service, peer }))
		}
		.boxed()
	}
}

/// Per-connection tower service that injects `Extension<Option<MtlsPeer>>` on
/// every request before forwarding to the inner service.
#[derive(Clone)]
struct SetMtlsExtension<S> {
	inner: S,
	peer: Option<MtlsPeer>,
}

impl<S, B> Service<http::Request<B>> for SetMtlsExtension<S>
where
	S: Service<http::Request<B>>,
{
	type Response = S::Response;
	type Error = S::Error;
	type Future = S::Future;

	fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		self.inner.poll_ready(cx)
	}

	fn call(&mut self, mut req: http::Request<B>) -> Self::Future {
		// Only insert when a cert was presented; handlers extract via
		// `Option<Extension<MtlsPeer>>` so absence is the no-cert case.
		if let Some(peer) = self.peer.clone() {
			req.extensions_mut().insert(peer);
		}
		self.inner.call(req)
	}
}

/// HTML landing page served when a plain browser hits the relay directly.
///
/// MoQ clients speak WebTransport or WebSocket, so a GET request from a
/// regular browser isn't something we can service. Rather than exposing an
/// internal error (e.g. the "Request method must be `CONNECT`" rejection
/// from axum's WebSocket extractor), we render a short informational page.
pub(crate) const LANDING_PAGE: &str = "<!doctype html>
<html lang=\"en\">
<head><meta charset=\"utf-8\"><title>moq-relay</title></head>
<body>
<h1>moq-relay</h1>
<p>This is a moq-relay instance, and you're not a MoQ client.</p>
<p>See <a href=\"https://moq.dev\">https://moq.dev</a> for more info.</p>
</body>
</html>
";

pub(crate) fn landing_response() -> Response {
	(StatusCode::NOT_FOUND, Html(LANDING_PAGE)).into_response()
}

/// Axum fallback handler for any unmatched route.
async fn serve_landing() -> Response {
	landing_response()
}

/// Liveness probe. Always returns `200 ok`. Unauthenticated so load-balancer
/// probes don't need a JWT. Host overload monitoring belongs in a separate
/// process, not the relay.
async fn serve_health() -> Response {
	(StatusCode::OK, "ok\n").into_response()
}

async fn serve_fingerprint(State(state): State<Arc<WebState>>) -> String {
	// Get the first certificate's fingerprint.
	// TODO serve all of them so we can support multiple signature algorithms.
	state
		.tls_info
		.read()
		.expect("tls_info lock poisoned")
		.fingerprints
		.first()
		.expect("missing certificate")
		.clone()
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct AuthQuery {
	pub(crate) jwt: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct FetchParams {
	#[serde(flatten)]
	auth: AuthQuery,

	#[serde(default)]
	group: FetchGroup,

	#[serde(default)]
	frame: FetchFrame,
}

#[derive(Debug, Default)]
enum FetchGroup {
	// Return the group at the given sequence number.
	Num(u64),

	// Return the latest group.
	#[default]
	Latest,
}

impl<'de> serde::Deserialize<'de> for FetchGroup {
	fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		let s = String::deserialize(deserializer)?;
		if let Ok(num) = s.parse::<u64>() {
			Ok(FetchGroup::Num(num))
		} else if s == "latest" {
			Ok(FetchGroup::Latest)
		} else {
			Err(serde::de::Error::custom(format!("invalid group value: {s}")))
		}
	}
}

#[derive(Debug, Default)]
enum FetchFrame {
	Num(usize),
	#[default]
	Chunked,
}

impl<'de> serde::Deserialize<'de> for FetchFrame {
	fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		let s = String::deserialize(deserializer)?;
		if let Ok(num) = s.parse::<usize>() {
			Ok(FetchFrame::Num(num))
		} else if s == "chunked" {
			Ok(FetchFrame::Chunked)
		} else {
			Err(serde::de::Error::custom(format!("invalid frame value: {s}")))
		}
	}
}

/// Serve the announced broadcasts for a given prefix.
async fn serve_announced(
	path: Option<Path<String>>,
	Query(query): Query<AuthQuery>,
	mtls: Option<Extension<MtlsPeer>>,
	State(state): State<Arc<WebState>>,
) -> axum::response::Result<String> {
	let prefix = match path {
		Some(Path(prefix)) => prefix,
		None => String::new(),
	};

	let params = AuthParams {
		path: prefix,
		jwt: query.jwt,
	};
	let token = if mtls.is_some() {
		// mTLS peers: the API returns the canonical root and the billing tier.
		state.auth.verify_mtls(&params.path).await?
	} else {
		state.auth.verify(&params).await?
	};
	let Some(origin) = state.cluster.subscriber(&token) else {
		return Err(StatusCode::UNAUTHORIZED.into());
	};

	let mut announced = origin.consume().announced();
	let mut broadcasts = Vec::new();

	while let Some((suffix, event)) = announced.try_next() {
		if event.broadcast().is_some() {
			broadcasts.push(suffix);
		}
	}

	Ok(broadcasts.iter().map(|p| p.to_string()).collect::<Vec<_>>().join("\n"))
}

/// Serve the given group for a given track
async fn serve_fetch(
	Path(path): Path<String>,
	Query(params): Query<FetchParams>,
	mtls: Option<Extension<MtlsPeer>>,
	State(state): State<Arc<WebState>>,
) -> axum::response::Result<ServeGroup> {
	// The path containts a broadcast/track
	let mut path: Vec<&str> = path.split("/").collect();
	let track = path.pop().unwrap().to_string();

	// We need at least a broadcast and a track.
	if path.is_empty() {
		return Err(StatusCode::BAD_REQUEST.into());
	}

	let auth = AuthParams {
		path: path.join("/"),
		jwt: params.auth.jwt,
	};
	let token = if mtls.is_some() {
		// mTLS peers: the API returns the canonical root and the billing tier.
		state.auth.verify_mtls(&auth.path).await?
	} else {
		state.auth.verify(&auth).await?
	};
	// The token's root is the canonical (alias-resolved) broadcast path.
	let broadcast = token.root.to_string();

	let Some(origin) = state.cluster.subscriber(&token) else {
		return Err(StatusCode::UNAUTHORIZED.into());
	};

	tracing::info!(%broadcast, %track, "fetching track");

	let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(30);

	let result = tokio::time::timeout_at(deadline, async {
		// NOTE: The auth token is already scoped to the broadcast.
		// Block until the broadcast has been announced (within the fetch deadline) so
		// freshly-connected subscribers don't get a spurious 404 before gossip arrives.
		let broadcast = origin
			.consume()
			.announced_broadcast("")
			.await
			.ok_or(StatusCode::NOT_FOUND)?;
		let group = match params.group {
			// "latest" needs a live subscription to learn the newest sequence;
			// fetch only retrieves a specified past group.
			FetchGroup::Latest => match async { broadcast.track(&track)?.subscribe(None).await }.await {
				Ok(mut sub) => match sub.latest() {
					Some(sequence) => sub.get_group(sequence).await,
					None => sub.recv_group().await,
				},
				Err(err) => Err(err),
			},
			// A one-shot fetch, no subscription required.
			FetchGroup::Num(sequence) => async { broadcast.track(&track)?.fetch_group(sequence, None).await }
				.await
				.map(Some),
		};

		let group = match group {
			Ok(Some(group)) => group,
			Ok(None) | Err(moq_net::Error::NotFound) => return Err(StatusCode::NOT_FOUND),
			Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
		};

		tracing::info!(%track, group = %group.sequence, "serving group");

		match params.frame {
			FetchFrame::Num(index) => match group.get_frame(index).await {
				Ok(Some(frame)) => Ok(ServeGroup {
					group: None,
					frame: Some(frame),
					deadline,
				}),
				Ok(None) => Err(StatusCode::NOT_FOUND),
				Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
			},
			FetchFrame::Chunked => Ok(ServeGroup {
				group: Some(group),
				frame: None,
				deadline,
			}),
		}
	})
	.await;

	match result {
		Ok(Ok(serve)) => Ok(serve),
		Ok(Err(status)) => Err(status.into()),
		Err(_) => Err(StatusCode::GATEWAY_TIMEOUT.into()),
	}
}

struct ServeGroup {
	group: Option<moq_net::GroupConsumer>,
	frame: Option<moq_net::FrameConsumer>,
	deadline: tokio::time::Instant,
}

impl ServeGroup {
	async fn next(&mut self) -> moq_net::Result<Option<Bytes>> {
		while self.group.is_some() || self.frame.is_some() {
			if let Some(frame) = self.frame.as_mut() {
				let data = tokio::time::timeout_at(self.deadline, frame.read_all())
					.await
					.map_err(|_| moq_net::Error::Timeout)?;
				self.frame.take();
				return Ok(Some(data?));
			}

			if let Some(group) = self.group.as_mut() {
				self.frame = tokio::time::timeout_at(self.deadline, group.next_frame())
					.await
					.map_err(|_| moq_net::Error::Timeout)??;
				if self.frame.is_none() {
					self.group.take();
				}
			}
		}

		Ok(None)
	}
}

impl IntoResponse for ServeGroup {
	fn into_response(self) -> Response {
		Response::new(Body::new(self))
	}
}

impl http_body::Body for ServeGroup {
	type Data = Bytes;
	type Error = ServeGroupError;

	fn poll_frame(
		self: Pin<&mut Self>,
		cx: &mut Context<'_>,
	) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
		let this = self.get_mut();

		// Use `poll_fn` to turn the async function into a Future
		let future = this.next();
		tokio::pin!(future);

		match ready!(future.poll(cx)) {
			Ok(Some(data)) => {
				let frame = http_body::Frame::data(data);
				Poll::Ready(Some(Ok(frame)))
			}
			Ok(None) => Poll::Ready(None),
			Err(e) => Poll::Ready(Some(Err(ServeGroupError(e)))),
		}
	}
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
struct ServeGroupError(moq_net::Error);

impl IntoResponse for ServeGroupError {
	fn into_response(self) -> Response {
		(StatusCode::INTERNAL_SERVER_ERROR, self.0.to_string()).into_response()
	}
}

fn default_true() -> bool {
	true
}

#[cfg(test)]
mod tests {
	use super::*;
	use rcgen::{CertificateParams, KeyPair};
	use std::io::Write;
	use tempfile::TempDir;

	fn make_certs(dir: &TempDir) -> (PathBuf, PathBuf, PathBuf) {
		make_named_certs(dir, "server", "localhost")
	}

	/// Generate a CA + server cert/key on disk and return the temp paths.
	/// Modeled after `auth.rs::mtls_fixture`.
	fn make_named_certs(dir: &TempDir, name: &str, hostname: &str) -> (PathBuf, PathBuf, PathBuf) {
		let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

		let ca_kp = KeyPair::generate().unwrap();
		let mut ca_params = CertificateParams::new(vec![]).unwrap();
		ca_params.distinguished_name.push(rcgen::DnType::CommonName, "Test CA");
		ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
		let ca_cert = ca_params.self_signed(&ca_kp).unwrap();
		let ca_issuer = rcgen::Issuer::from_params(&ca_params, &ca_kp);

		let server_kp = KeyPair::generate().unwrap();
		let mut server_params = CertificateParams::new(vec![hostname.to_string()]).unwrap();
		server_params
			.distinguished_name
			.push(rcgen::DnType::CommonName, format!("test-{name}"));
		let server_cert = server_params.signed_by(&server_kp, &ca_issuer).unwrap();

		let ca_path = dir.path().join(format!("{name}.ca.pem"));
		let cert_path = dir.path().join(format!("{name}.cert.pem"));
		let key_path = dir.path().join(format!("{name}.key.pem"));
		std::fs::write(&ca_path, ca_cert.pem()).unwrap();
		std::fs::write(&cert_path, server_cert.pem()).unwrap();
		std::fs::write(&key_path, server_kp.serialize_pem()).unwrap();

		(ca_path, cert_path, key_path)
	}

	#[tokio::test]
	async fn build_https_config_round_trips() {
		let dir = TempDir::new().unwrap();
		let (ca_path, cert_path, key_path) = make_certs(&dir);

		let config =
			build_https_config(&[cert_path], &[key_path], &[ca_path]).expect("build_https_config should succeed");

		// ALPN must include h2 + http/1.1; otherwise reqwest's h2 attempt
		// would silently downgrade or fail. Mirrors axum_server's default.
		assert_eq!(
			config.alpn_protocols,
			vec![b"h2".to_vec(), b"http/1.1".to_vec()],
			"ALPN must advertise h2 and http/1.1",
		);
	}

	#[tokio::test]
	async fn build_https_config_no_client_auth_when_ca_empty() {
		let dir = TempDir::new().unwrap();
		let (_ca_path, cert_path, key_path) = make_certs(&dir);

		// Empty root is the JWT-only path; should still produce a valid
		// config with ALPN set so axum-server's hyper layer can negotiate h2.
		let config =
			build_https_config(&[cert_path], &[key_path], &[]).expect("no-CA path should still build a usable config");

		assert_eq!(config.alpn_protocols, vec![b"h2".to_vec(), b"http/1.1".to_vec()],);
	}

	#[tokio::test]
	async fn build_https_config_accepts_multiple_cert_pairs() {
		let dir = TempDir::new().unwrap();
		let (_ca_a, cert_a, key_a) = make_named_certs(&dir, "cdn", "cdn.moq.dev");
		let (_ca_b, cert_b, key_b) = make_named_certs(&dir, "pro", "moq.pro");

		let config = build_https_config(&[cert_a, cert_b], &[key_a, key_b], &[])
			.expect("multiple HTTPS cert/key pairs should build");

		assert_eq!(config.alpn_protocols, vec![b"h2".to_vec(), b"http/1.1".to_vec()]);
	}

	#[tokio::test]
	async fn build_https_config_rejects_missing_ca() {
		let dir = TempDir::new().unwrap();
		let (_ca_path, cert_path, key_path) = make_certs(&dir);

		let bogus = dir.path().join("does-not-exist.pem");
		let res = build_https_config(&[cert_path], &[key_path], &[bogus]);
		assert!(res.is_err(), "missing CA file should be a hard error");
	}

	#[tokio::test]
	async fn build_https_config_rejects_empty_cert_list() {
		let res = build_https_config(&[], &[], &[]);
		assert!(res.is_err(), "HTTPS must require at least one cert/key pair");
	}

	#[tokio::test]
	async fn build_https_config_rejects_mismatched_cert_key_lists() {
		let dir = TempDir::new().unwrap();
		let (_ca_path, cert_path, _key_path) = make_certs(&dir);

		let res = build_https_config(&[cert_path], &[], &[]);
		assert!(res.is_err(), "HTTPS cert/key lists must be paired");
	}

	#[tokio::test]
	async fn build_https_config_rejects_empty_pem() {
		let dir = TempDir::new().unwrap();
		let (_ca_path, cert_path, key_path) = make_certs(&dir);

		let empty = dir.path().join("empty.pem");
		let mut f = std::fs::File::create(&empty).unwrap();
		writeln!(f, "# no certs here").unwrap();

		let res = build_https_config(&[cert_path], &[key_path], &[empty]);
		assert!(
			res.is_err(),
			"empty PEM must be rejected to avoid a silently disabled verifier"
		);
	}

	/// Confirm `SetMtlsExtension` injects the marker into request extensions
	/// when a peer cert was presented, and leaves them untouched otherwise.
	#[tokio::test]
	async fn set_mtls_extension_injects_marker() {
		use axum::http::Request;
		use std::convert::Infallible;

		// Inner service that just echoes back whether the extension was present.
		#[derive(Clone)]
		struct EchoExt;
		impl Service<Request<()>> for EchoExt {
			type Response = bool;
			type Error = Infallible;
			type Future = std::pin::Pin<Box<dyn Future<Output = Result<bool, Infallible>> + Send>>;
			fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
				Poll::Ready(Ok(()))
			}
			fn call(&mut self, req: Request<()>) -> Self::Future {
				let has = req.extensions().get::<MtlsPeer>().is_some();
				Box::pin(async move { Ok(has) })
			}
		}

		let mut with_peer = SetMtlsExtension {
			inner: EchoExt,
			peer: Some(MtlsPeer),
		};
		let mut no_peer = SetMtlsExtension {
			inner: EchoExt,
			peer: None,
		};

		let req = Request::builder().body(()).unwrap();
		assert!(
			with_peer.call(req).await.unwrap(),
			"SetMtlsExtension(Some) must surface MtlsPeer"
		);

		let req = Request::builder().body(()).unwrap();
		assert!(
			!no_peer.call(req).await.unwrap(),
			"SetMtlsExtension(None) must NOT surface MtlsPeer"
		);
	}
}
