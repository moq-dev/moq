use std::{
	future::Future,
	net,
	path::PathBuf,
	pin::Pin,
	sync::{Arc, atomic::AtomicU64},
	task::{Context, Poll, ready},
};

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

use crate::{Auth, AuthParams, AuthToken, Cluster};

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

/// HTTPS listener configuration with TLS certificate and key.
#[derive(clap::Args, Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct HttpsConfig {
	/// Socket address to bind the HTTPS listener to.
	#[arg(long = "web-https-listen", id = "web-https-listen", env = "MOQ_WEB_HTTPS_LISTEN", requires_all = ["web-https-cert", "web-https-key"])]
	pub listen: Option<net::SocketAddr>,

	/// Load the given certificate from disk.
	#[arg(long = "web-https-cert", id = "web-https-cert", env = "MOQ_WEB_HTTPS_CERT")]
	pub cert: Option<PathBuf>,

	/// Load the given key from disk.
	#[arg(long = "web-https-key", id = "web-https-key", env = "MOQ_WEB_HTTPS_KEY")]
	pub key: Option<PathBuf>,

	/// PEM file(s) of root CAs for validating optional client certificates (mTLS).
	///
	/// When set, clients *may* present a certificate during the TLS handshake.
	/// A verified peer is granted an unrestricted [`AuthToken`] without a JWT,
	/// mirroring the QUIC server's `--server-tls-root` behavior. Clients that
	/// don't present a cert continue through the normal JWT path.
	#[arg(
		long = "web-https-root",
		id = "web-https-root",
		value_delimiter = ',',
		env = "MOQ_WEB_HTTPS_ROOT"
	)]
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub root: Vec<PathBuf>,
}

/// Shared state passed to all web handler routes.
pub struct WebState {
	/// The authenticator for verifying incoming requests.
	pub auth: Auth,
	/// The cluster state for resolving origins.
	pub cluster: Cluster,
	/// TLS certificate information served at `/certificate.sha256`.
	pub tls_info: Arc<std::sync::RwLock<moq_native::ServerTlsInfo>>,
	/// Monotonically increasing connection counter for WebSocket sessions.
	pub conn_id: AtomicU64,
}

/// Run a HTTP server using Axum
pub struct Web {
	state: WebState,
	config: WebConfig,
}

impl Web {
	pub fn new(state: WebState, config: WebConfig) -> Self {
		Self { state, config }
	}

	/// Runs the HTTP and/or HTTPS listeners until they shut down.
	pub async fn run(self) -> anyhow::Result<()> {
		let app = Router::new()
			.route("/certificate.sha256", get(serve_fingerprint))
			.route("/announced", get(serve_announced))
			.route("/announced/{*prefix}", get(serve_announced))
			.route("/fetch/{*path}", get(serve_fetch));

		// If WebSocket is enabled, add the WebSocket route.
		#[cfg(feature = "websocket")]
		let app = match self.config.ws {
			true => app.route("/{*path}", axum::routing::any(crate::websocket::serve_ws)),
			false => app,
		};

		let app = app
			.fallback(serve_landing)
			.layer(CorsLayer::new().allow_origin(Any).allow_methods([Method::GET]))
			.with_state(Arc::new(self.state))
			.into_make_service();

		let http = if let Some(listen) = self.config.http.listen {
			let server = axum_server::bind(listen);
			Some(server.serve(app.clone()))
		} else {
			None
		};

		let https = if let Some(listen) = self.config.https.listen {
			let cert = self.config.https.cert.expect("missing https.cert");
			let key = self.config.https.key.expect("missing https.key");
			let root = self.config.https.root.clone();

			let config = build_https_config(&cert, &key, &root).await?;
			let rustls_config = RustlsConfig::from_config(Arc::new(config));

			#[cfg(unix)]
			tokio::spawn(reload_https_config(rustls_config.clone(), cert, key, root));

			// MtlsAcceptor surfaces a verified peer cert as a request extension.
			// When no client CA is configured, the inner verifier is `NoClientAuth`
			// and `peer_certificates()` always returns None — the wrapper is then
			// a near-no-op, but keeping a single path simplifies reload + serve.
			let acceptor = MtlsAcceptor {
				inner: RustlsAcceptor::new(rustls_config),
			};
			let server = axum_server::bind(listen).acceptor(acceptor);
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
}

/// Build a [`rustls::ServerConfig`] for the HTTPS listener.
///
/// When `root` is non-empty, installs a [`WebPkiClientVerifier`] with
/// `.allow_unauthenticated()` so JWT-only callers still complete the
/// handshake without presenting a cert. When empty, falls back to
/// [`with_no_client_auth`]. ALPN is set to `h2`, `http/1.1` to match
/// axum-server's defaults.
///
/// TLS version is left at the rustls default (1.2 + 1.3) so older clients
/// can still hit the HTTPS API; the QUIC server separately forces 1.3.
async fn build_https_config(
	cert: &std::path::Path,
	key: &std::path::Path,
	root: &[PathBuf],
) -> anyhow::Result<rustls::ServerConfig> {
	use anyhow::Context;
	use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
	use rustls::server::WebPkiClientVerifier;

	let cert_chain: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert)
		.context("failed to open https cert")?
		.collect::<Result<_, _>>()
		.context("failed to parse https cert")?;
	let key_der = PrivateKeyDer::from_pem_file(key).context("failed to parse https key")?;

	let provider = rustls::crypto::CryptoProvider::get_default()
		.cloned()
		.expect("no default crypto provider installed");

	let builder =
		rustls::ServerConfig::builder_with_provider(provider.clone()).with_safe_default_protocol_versions()?;

	let mut config = if root.is_empty() {
		builder
			.with_no_client_auth()
			.with_single_cert(cert_chain, key_der)
			.context("invalid https cert/key pair")?
	} else {
		// Build the CA root store inline; `moq_native::ServerTlsConfig` is
		// `non_exhaustive`, so we can't construct one to call its `load_roots`.
		let mut root_store = rustls::RootCertStore::empty();
		for path in root {
			let mut found = false;
			for cert in CertificateDer::pem_file_iter(path).context("failed to open mTLS client CA")? {
				let cert = cert.context("failed to parse mTLS client CA PEM")?;
				root_store.add(cert).context("failed to add mTLS client CA")?;
				found = true;
			}
			anyhow::ensure!(found, "no certificates found in mTLS client CA: {}", path.display());
		}
		let verifier = WebPkiClientVerifier::builder_with_provider(Arc::new(root_store), provider)
			.allow_unauthenticated()
			.build()
			.context("failed to build https client cert verifier")?;

		builder
			.with_client_cert_verifier(verifier)
			.with_single_cert(cert_chain, key_der)
			.context("invalid https cert/key pair")?
	};

	config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
	Ok(config)
}

/// Reload the HTTPS cert/key on SIGUSR1.
///
/// `RustlsConfig::reload_from_pem_file` would rebuild with `with_no_client_auth`
/// — silently stripping mTLS when configured — so we always rebuild via the
/// full [`build_https_config`] path.
#[cfg(unix)]
async fn reload_https_config(config: RustlsConfig, cert: PathBuf, key: PathBuf, root: Vec<PathBuf>) {
	use tokio::signal::unix::{SignalKind, signal};

	let mut listener = signal(SignalKind::user_defined1()).expect("failed to listen for signals");

	while listener.recv().await.is_some() {
		tracing::info!("reloading web certificate");

		match build_https_config(&cert, &key, &root).await {
			Ok(new) => config.reload_from_config(Arc::new(new)),
			Err(err) => tracing::warn!(%err, "failed to reload web certificate"),
		}
	}
}

/// Marker inserted as a request extension when rustls verified a client cert
/// against the configured mTLS CA. We don't carry the cert bytes — "verified
/// by our CA" is the entire signal we need (mirrors `PeerIdentity` on the QUIC
/// side).
#[derive(Clone, Debug)]
pub(crate) struct MtlsPeer;

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
		AuthToken::unrestricted()
	} else {
		state.auth.verify(&params).await?
	};
	let Some(mut origin) = state.cluster.subscriber(&token) else {
		return Err(StatusCode::UNAUTHORIZED.into());
	};

	let mut broadcasts = Vec::new();

	while let Some((suffix, active)) = origin.try_announced() {
		if active.is_some() {
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

	let broadcast = path.join("/");
	let auth = AuthParams {
		path: broadcast.clone(),
		jwt: params.auth.jwt,
	};
	let token = if mtls.is_some() {
		AuthToken::unrestricted()
	} else {
		state.auth.verify(&auth).await?
	};

	let Some(origin) = state.cluster.subscriber(&token) else {
		return Err(StatusCode::UNAUTHORIZED.into());
	};

	tracing::info!(%broadcast, %track, "fetching track");

	let track = moq_lite::Track {
		name: track,
		priority: 0,
	};

	// NOTE: The auth token is already scoped to the broadcast.
	// TODO: switch to `announced_broadcast` (bounded by the fetch deadline) so freshly-connected
	// subscribers don't get a spurious 404 before the broadcast has gossiped.
	#[allow(deprecated)]
	let broadcast = origin.consume_broadcast("").ok_or(StatusCode::NOT_FOUND)?;
	let mut track = broadcast.subscribe_track(&track).map_err(|err| match err {
		moq_lite::Error::NotFound => StatusCode::NOT_FOUND,
		_ => StatusCode::INTERNAL_SERVER_ERROR,
	})?;

	let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(30);

	let result = tokio::time::timeout_at(deadline, async {
		let group = match params.group {
			FetchGroup::Latest => match track.latest() {
				Some(sequence) => track.get_group(sequence).await,
				None => track.recv_group().await,
			},
			FetchGroup::Num(sequence) => track.get_group(sequence).await,
		};

		let group = match group {
			Ok(Some(group)) => group,
			Ok(None) => return Err(StatusCode::NOT_FOUND),
			Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
		};

		tracing::info!(track = %track.name, group = %group.sequence, "serving group");

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
	group: Option<moq_lite::GroupConsumer>,
	frame: Option<moq_lite::FrameConsumer>,
	deadline: tokio::time::Instant,
}

impl ServeGroup {
	async fn next(&mut self) -> moq_lite::Result<Option<Bytes>> {
		while self.group.is_some() || self.frame.is_some() {
			if let Some(frame) = self.frame.as_mut() {
				let data = tokio::time::timeout_at(self.deadline, frame.read_all())
					.await
					.map_err(|_| moq_lite::Error::Timeout)?;
				self.frame.take();
				return Ok(Some(data?));
			}

			if let Some(group) = self.group.as_mut() {
				self.frame = tokio::time::timeout_at(self.deadline, group.next_frame())
					.await
					.map_err(|_| moq_lite::Error::Timeout)??;
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
struct ServeGroupError(moq_lite::Error);

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

	/// Generate a CA + server cert/key on disk and return the temp paths.
	/// Modeled after `auth.rs::mtls_fixture`.
	fn make_certs(dir: &TempDir) -> (PathBuf, PathBuf, PathBuf) {
		let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

		let ca_kp = KeyPair::generate().unwrap();
		let mut ca_params = CertificateParams::new(vec![]).unwrap();
		ca_params.distinguished_name.push(rcgen::DnType::CommonName, "Test CA");
		ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
		let ca_cert = ca_params.self_signed(&ca_kp).unwrap();

		let server_kp = KeyPair::generate().unwrap();
		let mut server_params = CertificateParams::new(vec!["localhost".to_string()]).unwrap();
		server_params
			.distinguished_name
			.push(rcgen::DnType::CommonName, "test-server");
		let server_cert = server_params.signed_by(&server_kp, &ca_cert, &ca_kp).unwrap();

		let ca_path = dir.path().join("ca.pem");
		let cert_path = dir.path().join("server.cert.pem");
		let key_path = dir.path().join("server.key.pem");
		std::fs::write(&ca_path, ca_cert.pem()).unwrap();
		std::fs::write(&cert_path, server_cert.pem()).unwrap();
		std::fs::write(&key_path, server_kp.serialize_pem()).unwrap();

		(ca_path, cert_path, key_path)
	}

	#[tokio::test]
	async fn build_https_config_round_trips() {
		let dir = TempDir::new().unwrap();
		let (ca_path, cert_path, key_path) = make_certs(&dir);

		let config = build_https_config(&cert_path, &key_path, &[ca_path])
			.await
			.expect("build_https_config should succeed");

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
		let config = build_https_config(&cert_path, &key_path, &[])
			.await
			.expect("no-CA path should still build a usable config");

		assert_eq!(config.alpn_protocols, vec![b"h2".to_vec(), b"http/1.1".to_vec()],);
	}

	#[tokio::test]
	async fn build_https_config_rejects_missing_ca() {
		let dir = TempDir::new().unwrap();
		let (_ca_path, cert_path, key_path) = make_certs(&dir);

		let bogus = dir.path().join("does-not-exist.pem");
		let res = build_https_config(&cert_path, &key_path, &[bogus]).await;
		assert!(res.is_err(), "missing CA file should be a hard error");
	}

	#[tokio::test]
	async fn build_https_config_rejects_empty_pem() {
		let dir = TempDir::new().unwrap();
		let (_ca_path, cert_path, key_path) = make_certs(&dir);

		let empty = dir.path().join("empty.pem");
		let mut f = std::fs::File::create(&empty).unwrap();
		writeln!(f, "# no certs here").unwrap();

		let res = build_https_config(&cert_path, &key_path, &[empty]).await;
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
