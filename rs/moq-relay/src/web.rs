use std::{
	net,
	path::PathBuf,
	sync::Arc,
	task::{Context, Poll},
};

use anyhow::Context as _;
use axum::{
	Router,
	body::Body,
	extract::{ConnectInfo, Extension, Path, Query, State},
	http::{self, Method, StatusCode},
	response::{Html, IntoResponse, Response},
	routing::get,
};
use axum_server::{
	accept::{Accept, DefaultAcceptor},
	tls_rustls::{RustlsAcceptor, RustlsConfig},
};
use bytes::Bytes;
use futures::{FutureExt, future::BoxFuture};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::server::TlsStream;
use tower_service::Service;

use crate::{auth, cluster};

/// Configuration for the HTTP/HTTPS web server.
#[derive(usage_rs::Args, Clone, Debug, serde::Deserialize, serde::Serialize)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct Config {
	/// Plain HTTP listener settings.
	#[usage(flatten)]
	#[serde(default)]
	pub http: Http,

	/// HTTPS listener settings with TLS.
	#[usage(flatten)]
	#[serde(default)]
	pub https: Https,

	/// If true (default), expose a WebTransport compatible WebSocket polyfill.
	#[usage(
		long = "web-ws",
		env = "MOQ_WEB_WS",
		setting = "web.ws",
		default = "true",
		bool_value
	)]
	pub ws: bool,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			http: Http::default(),
			https: Https::default(),
			ws: true,
		}
	}
}

impl Config {
	/// Whether the WebSocket polyfill is served.
	pub fn resolved_ws(&self) -> bool {
		self.ws
	}
}

/// Plain HTTP listener configuration.
#[derive(usage_rs::Args, Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct Http {
	/// Socket address to bind the HTTP listener to.
	#[usage(
		long = "web-http-listen",
		name = "http-listen",
		env = "MOQ_WEB_HTTP_LISTEN",
		setting = "web.http.listen"
	)]
	pub listen: Option<net::SocketAddr>,
}

/// HTTPS listener configuration with TLS certificates and keys.
#[serde_with::serde_as]
#[derive(usage_rs::Args, Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct Https {
	/// Socket address to bind the HTTPS listener to, serving `cert`/`key` or the `[acme]` certificate.
	#[usage(
		long = "web-https-listen",
		name = "web-https-listen",
		env = "MOQ_WEB_HTTPS_LISTEN",
		setting = "web.https.listen"
	)]
	pub listen: Option<net::SocketAddr>,

	/// Load the given certificate chain files from disk.
	///
	/// In config files, accepts either a single string or a TOML array.
	#[usage(
		long = "web-https-cert",
		name = "web-https-cert",
		delimiter = ',',
		env = "MOQ_WEB_HTTPS_CERT",
		setting = "web.https.cert"
	)]
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	#[serde_as(as = "serde_with::OneOrMany<_>")]
	pub cert: Vec<PathBuf>,

	/// Load the given private key files from disk.
	///
	/// Each key is paired with the certificate chain at the same index.
	/// In config files, accepts either a single string or a TOML array.
	#[usage(
		long = "web-https-key",
		name = "web-https-key",
		delimiter = ',',
		env = "MOQ_WEB_HTTPS_KEY",
		setting = "web.https.key"
	)]
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	#[serde_as(as = "serde_with::OneOrMany<_>")]
	pub key: Vec<PathBuf>,

	/// PEM file(s) of root CAs for validating optional client certificates (mTLS).
	///
	/// When set, clients *may* present a certificate during the TLS handshake.
	/// A verified peer is reported to the auth source as [`MtlsPeer`], a fact for
	/// the server behind `--auth-url` to weigh; it grants nothing on its own, and
	/// under `--auth-public` the peer gets what any anonymous session gets. Same
	/// as the QUIC listener's `--listen-tls-root`.
	///
	/// In config files, accepts either a single string or a TOML array.
	#[usage(
		long = "web-https-root",
		name = "web-https-root",
		delimiter = ',',
		env = "MOQ_WEB_HTTPS_ROOT",
		setting = "web.https.root"
	)]
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	#[serde_as(as = "serde_with::OneOrMany<_>")]
	pub root: Vec<PathBuf>,
}

/// Shared state passed to all web handler routes. An internal detail: callers
/// build a [`Web`] from its parts via [`Web::new`] rather than constructing this.
pub(crate) struct WebState {
	/// The authenticator for verifying incoming requests.
	pub(crate) auth: auth::Auth,
	/// The cluster state for resolving origins.
	pub(crate) cluster: cluster::Cluster,
	/// TLS certificate information served at `/certificate.sha256`.
	pub(crate) certificates: moq_tokio::tls::Certificates,
	/// Relay-wide shutdown broadcast; WebSocket sessions drain with a GOAWAY
	/// when it fires. Defaults to a handle that never fires.
	#[cfg_attr(
		not(feature = "websocket"),
		allow(dead_code, reason = "only the `websocket` feature uses it")
	)]
	pub(crate) shutdown: crate::shutdown::Observer,
	/// Live sessions on this node, so a WebSocket session is listed and nudged
	/// like a QUIC one.
	#[cfg_attr(
		not(feature = "websocket"),
		allow(dead_code, reason = "only the `websocket` feature uses it")
	)]
	pub(crate) sessions: crate::session::Registry,
	/// How long a connection may sit with no complete request in flight (TLS
	/// handshake and partial headers included), and a WebSocket wait for its MoQ
	/// SETUP; `None` waits forever.
	pub(crate) timeout: Option<std::time::Duration>,
}

/// The bound addresses of the public web listeners.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct Addrs {
	/// The plain HTTP listener, if configured.
	pub http: Option<net::SocketAddr>,
	/// The HTTPS listener, if configured.
	pub https: Option<net::SocketAddr>,
}

/// The implementation WebSocket sessions advertise in SETUP, or `None` for the library's.
#[derive(Clone, Default)]
#[cfg_attr(not(feature = "websocket"), allow(dead_code, reason = "websocket-only"))]
pub(crate) struct Implementation(pub Option<String>);

/// Run a HTTP server using Axum
pub struct Web {
	state: Arc<WebState>,
	config: Config,
	versions: moq_net::Versions,
	extensions: moq_net::setup::Extensions,
	implementation: Implementation,
	health: moq_tokio::accept::Health,
	http_listener: Option<net::TcpListener>,
	https_listener: Option<net::TcpListener>,
	https_tls: Option<RustlsConfig>,
	/// The ACME certificate resolver HTTPS serves instead of `https.cert`/`https.key`.
	acme: Option<Arc<dyn rustls::server::ResolvesServerCert>>,
	addrs: Addrs,
}

impl Web {
	/// Build a web server from its parts. `certificates` is the relay's TLS
	/// certificate handle (e.g. `server.certificates()`), whose fingerprints are
	/// served at `/certificate.sha256`.
	pub fn new(
		auth: auth::Auth,
		cluster: cluster::Cluster,
		certificates: moq_tokio::tls::Certificates,
		config: Config,
	) -> Self {
		let state = Arc::new(WebState {
			auth,
			cluster,
			certificates,
			shutdown: crate::shutdown::Observer::disabled(),
			sessions: crate::session::Registry::new(),
			timeout: moq_tokio::listen::Config::default().resolved_timeout(),
		});
		Self {
			state,
			config,
			versions: moq_net::Versions::all(),
			extensions: Default::default(),
			implementation: Default::default(),
			health: moq_tokio::accept::Health::new("web"),
			http_listener: None,
			https_listener: None,
			https_tls: None,
			acme: None,
			addrs: Addrs::default(),
		}
	}

	/// Bind the configured listeners now, so [`addrs`](Self::addrs) reports ephemeral ports before serving.
	pub fn bind(mut self) -> anyhow::Result<Self> {
		let https = &self.config.https;
		if https.listen.is_none() {
			for (flag, set) in [
				("--web-https-cert", !https.cert.is_empty()),
				("--web-https-key", !https.key.is_empty()),
				("--web-https-root", !https.root.is_empty()),
			] {
				anyhow::ensure!(!set, "{flag} requires --web-https-listen");
			}
		}
		if self.https_tls.is_none() && self.config.https.listen.is_some() {
			let tls = match &self.acme {
				Some(resolver) => build_acme_config(resolver.clone(), &self.config.https.root)?,
				None => build_https_config(&self.config.https.cert, &self.config.https.key, &self.config.https.root)?,
			};
			self.https_tls = Some(RustlsConfig::from_config(tls));
		}
		if let Some(listen) = self.config.http.listen
			&& self.http_listener.is_none()
		{
			let listener = moq_tokio::bind::tcp(listen).context("failed to bind HTTP listener")?;
			let addr = listener.local_addr().context("failed to resolve HTTP bind address")?;
			self.addrs.http = Some(addr);
			tracing::info!(%addr, kind = "http", "listening");
			self.http_listener = Some(listener);
		}
		if let Some(listen) = self.config.https.listen
			&& self.https_listener.is_none()
		{
			let listener = moq_tokio::bind::tcp(listen).context("failed to bind HTTPS listener")?;
			let addr = listener.local_addr().context("failed to resolve HTTPS bind address")?;
			self.addrs.https = Some(addr);
			tracing::info!(%addr, kind = "https", "listening");
			self.https_listener = Some(listener);
		}
		Ok(self)
	}

	/// Serve HTTPS from the ACME resolver, which also answers TLS-ALPN-01 challenges.
	#[cfg(feature = "acme")]
	pub(crate) fn with_acme(mut self, resolver: Option<Arc<dyn rustls::server::ResolvesServerCert>>) -> Self {
		self.acme = resolver;
		self
	}

	/// The bound HTTPS socket, where ACME answers challenges before serving starts.
	#[cfg(feature = "acme")]
	pub(crate) fn https_listener(&self) -> Option<&net::TcpListener> {
		self.https_listener.as_ref()
	}

	/// The actual bound addresses after [`bind`](Self::bind) or [`crate::Relay::load`].
	pub fn addrs(&self) -> Addrs {
		self.addrs
	}

	/// Current served fingerprints, for a programmatic test fixture.
	#[cfg(feature = "test-support")]
	pub(crate) fn certificate_fingerprints(&self) -> Vec<String> {
		self.state.certificates.fingerprints()
	}

	/// Restrict which MoQ versions WebSocket sessions accept, in preference order.
	pub fn with_versions(mut self, versions: moq_net::Versions) -> Self {
		self.versions = versions;
		self
	}

	/// Choose which moq-transport extensions WebSocket sessions offer in SETUP.
	pub(crate) fn with_extensions(mut self, extensions: moq_net::setup::Extensions) -> Self {
		self.extensions = extensions;
		self
	}

	/// Set the implementation WebSocket sessions advertise in SETUP; empty advertises nothing.
	pub fn with_implementation(mut self, implementation: impl Into<String>) -> Self {
		self.implementation = Implementation(Some(implementation.into()));
		self
	}

	/// A live handle to the accept-loop health of the HTTP/HTTPS listeners, for an
	/// embedder that publishes it (see [`moq_tokio::accept`]).
	///
	/// This is the one signal that leaves the process when the public listener goes
	/// dark: [`serve`](Self::serve) never gives up, so a node can be unable to accept
	/// a WebSocket session, a WHIP offer, or an HLS request while every other metric
	/// looks healthy. Take it before `serve` consumes the server.
	///
	/// `None` when neither listener is configured, so a QUIC-only relay publishes no
	/// counters for a socket it never opens. A permanently-zero series for an absent
	/// listener reads as a watch that is passing when there is nothing there to watch.
	///
	/// When both are configured they share one handle. The failures worth escalating
	/// on are process- or host-wide (out of descriptors, out of kernel memory), so an
	/// HTTP accept that succeeds is real evidence that the HTTPS one is not stalled
	/// either.
	pub fn accept_health(&self) -> Option<moq_tokio::accept::Health> {
		let configured = self.config.http.listen.is_some() || self.config.https.listen.is_some();
		configured.then(|| self.health.clone())
	}

	/// Count accepted public connections in the relay drain and send WebSockets a GOAWAY.
	/// Without this coordination they are cut off on process exit.
	pub fn with_shutdown(mut self, shutdown: crate::shutdown::Observer) -> Self {
		let state = Arc::get_mut(&mut self.state).expect("with_shutdown called after routes were built");
		state.shutdown = shutdown;
		self
	}

	/// Close a connection that sits this long with no complete request in flight,
	/// including one stalled in its TLS handshake or headers or idle in keep-alive,
	/// bound an HTTP request's auth admission (answering 502 past it), and bound a
	/// WebSocket's MoQ SETUP after the upgrade; `None`, or a timeout past the clock's
	/// range, waits forever. Defaults to the
	/// [`moq_tokio::listen::Config::resolved_timeout`] default.
	pub fn with_timeout(mut self, timeout: Option<std::time::Duration>) -> Self {
		let state = Arc::get_mut(&mut self.state).expect("with_timeout called after routes were built");
		// hyper adds the header timeout to the clock unchecked, so an unreachable
		// one would panic on every connection.
		state.timeout = timeout.filter(|timeout| kio::clock::now().checked_add(*timeout).is_some());
		self
	}

	/// Register WebSocket sessions in the node's live table so they can be listed
	/// and nudged. Without it they are served but do not appear.
	pub fn with_sessions(mut self, sessions: crate::session::Registry) -> Self {
		let state = Arc::get_mut(&mut self.state).expect("with_sessions called after routes were built");
		state.sessions = sessions;
		self
	}

	/// Build the default web router with `state` applied, returning a
	/// state-erased [`Router`] an embedder can extend (`merge`/`nest` extra
	/// routes) before handing it to [`crate::Relay::with_web`] or
	/// [`serve`](Self::serve).
	///
	/// This is the public-facing router (customer media routes plus a liveness
	/// probe). `/metrics` is deliberately NOT here: node traffic counters ride
	/// the separate internal listener ([`internal::Internal`](crate::internal::Internal)) so they're
	/// never exposed on the public listener.
	///
	/// Includes the WebSocket polyfill catch-all (`/{*path}`, when
	/// `config.ws`), but NOT the
	/// landing-page fallback (that is global, so [`serve`](Self::serve) sets it
	/// once across the merged router). GET CORS is applied to the complete router by [`Self::serve`],
	/// including any routes the embedder merges in without their own policy.
	pub fn routes(&self) -> Router {
		// These responses depend on the caller's grant, including when auth or
		// extraction fails. Publisher bytes must never become active content.
		let granted = Router::new()
			.route("/announced", get(serve_announced))
			.route("/announced/{*prefix}", get(serve_announced))
			.route("/fetch/{*path}", get(serve_fetch))
			.layer(axum::middleware::map_response(async |mut response: Response| {
				let headers = response.headers_mut();
				headers.insert(
					http::header::CACHE_CONTROL,
					http::HeaderValue::from_static("private, no-store"),
				);
				headers.insert(
					http::header::X_CONTENT_TYPE_OPTIONS,
					http::HeaderValue::from_static("nosniff"),
				);
				response
			}));
		let app = Router::new()
			.route("/health", get(serve_health))
			.route("/certificate.sha256", get(serve_fingerprint))
			.merge(granted);

		// If WebSocket is enabled, add the WebSocket route. Both `/` and
		// `/{*path}` map to the same handler so a client that dials a bare
		// `host:port` with no path (e.g. `moqsink url="https://host:4443"`)
		// still gets a WebSocket upgrade at the empty (root) auth scope. Without
		// the root route, axum's wildcard never matches `/`, the request falls
		// through to the landing page, and the client's WS fallback is silently
		// dead.
		#[cfg(feature = "websocket")]
		let app = if self.config.resolved_ws() {
			app.route("/", axum::routing::any(crate::websocket::serve_ws))
				.route("/{*path}", axum::routing::any(crate::websocket::serve_ws))
		} else {
			app
		};

		app.layer(Extension(self.versions.clone()))
			.layer(Extension(self.extensions))
			.layer(Extension(self.implementation.clone()))
			.with_state(self.state.clone())
	}

	/// Serve `app` on the configured HTTP/HTTPS listeners until they shut down.
	///
	/// Applies the landing-page fallback (so an unmatched route renders the
	/// informational page rather than a bare 404) and owns the listener +
	/// TLS machinery: optional mTLS client-cert extraction and hot cert
	/// reload. The caller builds `app` from [`routes`](Self::routes) plus any
	/// extra routes it merged in. An embedder driving a [`crate::Relay`]
	/// passes that router to [`crate::Relay::with_web`] instead of calling this.
	/// Every route can extract the accepted peer as `ConnectInfo<SocketAddr>`.
	pub async fn serve(self, app: Router) -> anyhow::Result<()> {
		let Web {
			state,
			config,
			health,
			http_listener,
			https_listener,
			https_tls,
			acme,
			..
		} = self.bind()?;
		let app = app
			.fallback(serve_landing)
			.layer(axum::middleware::from_fn(get_cors))
			.into_make_service_with_connect_info::<net::SocketAddr>();
		let ws = config.resolved_ws();
		// Each request is admitted on its own, so a connection may have as many in flight
		// as a session may have credentials verifying; the client queues the rest.
		let streams = u32::try_from(state.auth.verifications()).unwrap_or(u32::MAX);

		let http = if let Some(listener) = http_listener {
			let mut server = crate::listener::server(
				listener,
				crate::listener::Config {
					health: health.clone(),
					timeout: state.timeout,
					activity: Some(state.shutdown.activity.clone()),
				},
				WebAcceptor::plain(ws),
			)?;
			server.http_builder().http2().max_concurrent_streams(streams);
			Some(server.serve(app.clone()))
		} else {
			None
		};

		let https = if let Some(listener) = https_listener {
			let rustls_config = https_tls.expect("HTTPS config was built before binding");
			// The ACME resolver renews in place, so there are no files to watch.
			if acme.is_none() {
				state.cluster.critical.spawn(
					"HTTPS certificate reload",
					reload_https_config(
						rustls_config.clone(),
						config.https.cert,
						config.https.key,
						config.https.root,
					),
				);
			}
			let mut server = crate::listener::server(
				listener,
				crate::listener::Config {
					health: health.clone(),
					timeout: state.timeout,
					activity: Some(state.shutdown.activity.clone()),
				},
				WebAcceptor::tls(rustls_config, ws),
			)?;
			server.http_builder().http2().max_concurrent_streams(streams);
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

// Public GET routes are readable cross-origin. Preserve an embedder's own
// response policy and leave every other method to its router.
async fn get_cors(request: axum::extract::Request, next: axum::middleware::Next) -> Response {
	let method = request.method().clone();
	if method == Method::OPTIONS
		&& request.headers().get(http::header::ACCESS_CONTROL_REQUEST_METHOD)
			== Some(&http::HeaderValue::from_static("GET"))
	{
		let mut response = StatusCode::OK.into_response();
		response.headers_mut().insert(
			http::header::ACCESS_CONTROL_ALLOW_ORIGIN,
			http::HeaderValue::from_static("*"),
		);
		response.headers_mut().insert(
			http::header::ACCESS_CONTROL_ALLOW_METHODS,
			http::HeaderValue::from_static("GET"),
		);
		if let Some(headers) = request.headers().get(http::header::ACCESS_CONTROL_REQUEST_HEADERS) {
			response
				.headers_mut()
				.insert(http::header::ACCESS_CONTROL_ALLOW_HEADERS, headers.clone());
		}
		return response;
	}

	let mut response = next.run(request).await;
	if method == Method::GET
		&& !response
			.headers()
			.contains_key(http::header::ACCESS_CONTROL_ALLOW_ORIGIN)
	{
		response.headers_mut().insert(
			http::header::ACCESS_CONTROL_ALLOW_ORIGIN,
			http::HeaderValue::from_static("*"),
		);
	}
	response
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

	let mut tls = moq_tokio::tls::Listen::default();
	tls.cert = cert.to_vec();
	tls.key = key.to_vec();
	tls.root = root.to_vec();

	tls.server_config(vec![b"h2".to_vec(), b"http/1.1".to_vec()])
		.context("failed to build https TLS config")
}

/// Build the HTTPS config around the ACME resolver, also offering the TLS-ALPN-01 protocol.
///
/// [`WebAcceptor`] closes a challenge connection once its handshake completes.
fn build_acme_config(
	resolver: Arc<dyn rustls::server::ResolvesServerCert>,
	root: &[PathBuf],
) -> anyhow::Result<Arc<rustls::ServerConfig>> {
	let mut tls = moq_tokio::tls::Listen::default();
	tls.root = root.to_vec();

	let alpn = vec![b"h2".to_vec(), b"http/1.1".to_vec(), b"acme-tls/1".to_vec()];
	moq_tokio::tls::Serve::resolver(resolver, &tls)?
		.config(alpn)
		.context("failed to build https TLS config")
}

fn https_watch_paths(cert: &[PathBuf], key: &[PathBuf], root: &[PathBuf]) -> Vec<PathBuf> {
	cert.iter()
		.cloned()
		.chain(key.iter().cloned())
		.chain(root.iter().cloned())
		.collect()
}

/// Reload the HTTPS certificate and key whenever they change on disk.
///
/// `RustlsConfig::reload_from_pem_file` would rebuild with `with_no_client_auth`
/// (silently stripping mTLS when configured), so we always rebuild via the full
/// [`build_https_config`] path. The client verifier watches root files itself,
/// while this watcher also uses them to retry a failed certificate/key rotation.
async fn reload_https_config(config: RustlsConfig, cert: Vec<PathBuf>, key: Vec<PathBuf>, root: Vec<PathBuf>) {
	let paths = https_watch_paths(&cert, &key, &root);

	let mut watcher = match moq_tokio::watch::Files::new(&paths) {
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

/// The kernel's view of the TCP connection a request arrived on.
///
/// An HTTP upgrade hands the WebSocket a type-erased stream, so qmux can't find the
/// socket underneath it and would otherwise report no RTT until its own `QX_PING`
/// completes a round trip. Capturing the descriptor at accept time, before TLS and
/// hyper take ownership, lets the MoQ session report latency from the first moment.
///
/// Absent on platforms with no readable TCP info, and on any socket the read fails
/// for; handlers extract `Option<Extension<SocketStats>>` accordingly.
#[cfg(feature = "websocket")]
#[derive(Clone)]
pub(crate) struct SocketStats(pub(crate) qmux::SharedSocketStats);

/// Without the `websocket` feature there is no qmux to hand a socket to, so this
/// carries nothing and is never constructed. Keeping the type present either way
/// lets the acceptor and its service stay free of feature gates.
#[cfg(not(feature = "websocket"))]
#[derive(Clone)]
pub(crate) struct SocketStats(std::convert::Infallible);

/// The client certificate HTTPS mTLS verified, inserted as a request extension.
///
/// Embedded routes can extract `Option<Extension<MtlsPeer>>` to mirror the
/// built-in relay handlers and report the identity in their auth request; it is a
/// fact for the auth server to weigh, never a grant on its own.
#[derive(Clone, Debug)]
pub struct MtlsPeer(pub moq_tokio::tls::PeerIdentity);

/// The SNI that selected the HTTPS certificate, inserted as a request extension.
///
/// Plain HTTP has no handshake to name a host, so it never carries one; the `Host`
/// header and `:authority` are client-chosen and never stand in for it.
#[derive(Clone, Debug)]
pub(crate) struct ServerName(pub(crate) String);

/// Accepts a connection on a public web listener.
///
/// Captures the socket, then hands the stream to `inner`: [`DefaultAcceptor`] for
/// `http://`, or [`RustlsAcceptor`] for `https://`, which also gives the connection
/// a peer certificate to report. Both listeners wrap this same acceptor, so the
/// capture cannot reach one and miss the other.
#[derive(Clone)]
struct WebAcceptor<A> {
	inner: A,
	/// Whether a WebSocket route exists to use the handle. The capture holds a
	/// duplicated descriptor for the life of the connection, so doing it when
	/// nothing can read it would cost every API, HLS, and health-check connection a
	/// second descriptor for nothing.
	ws: bool,
}

impl WebAcceptor<DefaultAcceptor> {
	/// Accepts plain HTTP, which has no handshake of its own to run.
	fn plain(ws: bool) -> Self {
		Self {
			inner: DefaultAcceptor::new(),
			ws,
		}
	}
}

impl WebAcceptor<RustlsAcceptor<DefaultAcceptor>> {
	/// Accepts HTTPS, running the TLS handshake and surfacing a verified peer
	/// certificate as [`MtlsPeer`].
	///
	/// When no client CA is configured the inner verifier is `NoClientAuth` and
	/// `peer_certificates()` always returns None, so the marker never appears.
	fn tls(config: RustlsConfig, ws: bool) -> Self {
		Self {
			inner: RustlsAcceptor::new(config),
			ws,
		}
	}
}

/// HTTP protocol and peer metadata available after a connection finishes TLS.
///
/// Which streams can present a client certificate is a property of the stream type,
/// not of the acceptor: only rustls has one to report, and a plain TCP connection
/// never does.
trait HttpStream {
	/// The marker for this connection's verified peer certificate.
	fn mtls_peer(&self) -> Option<MtlsPeer>;
	/// The SNI that selected the certificate, when the connection carried TLS.
	fn server_name(&self) -> Option<&str> {
		None
	}
	fn admission(&self) -> Option<crate::listener::Admission> {
		None
	}
	fn is_http2(&self) -> bool {
		false
	}
	/// Whether this connection is an ACME TLS-ALPN-01 validation, which carries no HTTP.
	fn acme_challenge(&self) -> bool {
		false
	}
}

impl HttpStream for tokio::net::TcpStream {
	fn mtls_peer(&self) -> Option<MtlsPeer> {
		None
	}
}

impl HttpStream for crate::listener::Accepted {
	fn mtls_peer(&self) -> Option<MtlsPeer> {
		None
	}
	fn admission(&self) -> Option<crate::listener::Admission> {
		self.admission.clone()
	}
}

impl<I: HttpStream> HttpStream for TlsStream<I> {
	fn admission(&self) -> Option<crate::listener::Admission> {
		self.get_ref().0.admission()
	}
	fn is_http2(&self) -> bool {
		self.get_ref().1.alpn_protocol() == Some(b"h2")
	}
	fn acme_challenge(&self) -> bool {
		self.get_ref().1.alpn_protocol() == Some(b"acme-tls/1")
	}
	fn server_name(&self) -> Option<&str> {
		self.get_ref().1.server_name()
	}
	fn mtls_peer(&self) -> Option<MtlsPeer> {
		self.get_ref()
			.1
			.peer_certificates()
			.filter(|certs| !certs.is_empty())
			.map(|certs| MtlsPeer(moq_tokio::tls::PeerIdentity::from_chain(certs.to_vec())))
	}
}

/// The connection [`WebAcceptor`] is handed: a byte stream, plus a descriptor to
/// read socket statistics from on platforms that expose them.
///
/// The stream owns the accepted TCP socket, so the extra bound narrows nothing;
/// it exists because a `cfg` can't be written on a `where` clause directly.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) trait AcceptStream: AsyncRead + AsyncWrite + Unpin + Send + std::os::fd::AsFd + 'static {}
#[cfg(any(target_os = "linux", target_os = "macos"))]
impl<T: AsyncRead + AsyncWrite + Unpin + Send + std::os::fd::AsFd + 'static> AcceptStream for T {}

/// As above, where kernel TCP statistics are unavailable.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) trait AcceptStream: AsyncRead + AsyncWrite + Unpin + Send + 'static {}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl<T: AsyncRead + AsyncWrite + Unpin + Send + 'static> AcceptStream for T {}

impl<I, S, A> Accept<I, S> for WebAcceptor<A>
where
	I: AcceptStream,
	S: Send + 'static,
	A: Accept<I, S>,
	A::Stream: HttpStream + Send + 'static,
	A::Service: Send + 'static,
	A::Future: Send + 'static,
{
	type Stream = A::Stream;
	type Service = SetConnectionExtensions<A::Service>;
	type Future = BoxFuture<'static, std::io::Result<(Self::Stream, Self::Service)>>;

	fn accept(&self, stream: I, service: S) -> Self::Future {
		// Read the socket here, while it is still a plain `TcpStream`. Once TLS and
		// then hyper's upgrade have wrapped it there is no descriptor left to find.
		let socket = self.ws.then(|| socket_stats(&stream)).flatten();
		let inner = self.inner.accept(stream, service);
		async move {
			let (stream, service) = inner.await?;
			if stream.acme_challenge() {
				// The validator checked the certificate during the handshake; RFC 8737 has the server close.
				return Err(std::io::Error::other("answered an ACME TLS-ALPN-01 challenge"));
			}
			let peer = stream.mtls_peer();
			let server_name = stream.server_name().map(|name| ServerName(name.to_owned()));
			let admission = stream.admission();
			if stream.is_http2() {
				// Extended CONNECT is not enabled, so h2 cannot become a WebSocket.
				drop(admission.as_ref().and_then(crate::listener::Admission::take));
			}
			Ok((
				stream,
				SetConnectionExtensions {
					inner: service,
					peer,
					server_name,
					socket,
					admission,
				},
			))
		}
		.boxed()
	}
}

/// The kernel's view of `stream`, when this platform exposes one and there is a
/// qmux session that could use it.
#[cfg(all(any(target_os = "linux", target_os = "macos"), feature = "websocket"))]
fn socket_stats<I: AcceptStream>(stream: &I) -> Option<SocketStats> {
	qmux::TcpStats::new(stream)
		.ok()
		.map(|s| SocketStats(std::sync::Arc::new(s)))
}

#[cfg(not(all(any(target_os = "linux", target_os = "macos"), feature = "websocket")))]
fn socket_stats<I: AcceptStream>(_stream: &I) -> Option<SocketStats> {
	None
}

/// Attaches connection facts to each request, handing the first upgrade its admission.
///
/// The mTLS marker, SNI, and captured socket are replayed on every request.
/// Admission is shared between service clones and consumed only once.
#[derive(Clone)]
struct SetConnectionExtensions<S> {
	inner: S,
	peer: Option<MtlsPeer>,
	server_name: Option<ServerName>,
	socket: Option<SocketStats>,
	admission: Option<crate::listener::Admission>,
}

impl<S, B> Service<http::Request<B>> for SetConnectionExtensions<S>
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
		// Taking from the shared slot classifies only the first request, even
		// when hyper cloned the service. Plain HTTP releases at these headers;
		// a later upgrade has no accept guard and enters from its handler.
		if let Some(active) = self.admission.as_ref().and_then(crate::listener::Admission::take) {
			let upgrade = req.version() != http::Version::HTTP_2
				&& req
					.headers()
					.get(http::header::UPGRADE)
					.is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"websocket"));
			if upgrade {
				req.extensions_mut().insert(crate::listener::Admission::new(active));
			}
		}

		// Only insert when a cert was presented; handlers extract via
		// `Option<Extension<MtlsPeer>>` so absence is the no-cert case.
		if let Some(peer) = self.peer.clone() {
			req.extensions_mut().insert(peer);
		}
		if let Some(server_name) = self.server_name.clone() {
			req.extensions_mut().insert(server_name);
		}
		if let Some(socket) = self.socket.clone() {
			req.extensions_mut().insert(socket);
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

async fn serve_fingerprint(State(state): State<Arc<WebState>>) -> Response {
	// The first certificate in configuration order, deliberately. The endpoint
	// exists so an `http://` client can pin a self-signed development
	// certificate, where there is exactly one. With several configured there is
	// no single answer: clients use `https://` so SNI selects the matching
	// certificate at the handshake.
	match state.certificates.fingerprints().into_iter().next() {
		Some(fingerprint) => fingerprint.into_response(),
		// A stream-only relay has no certificate to pin.
		None => (StatusCode::NOT_FOUND, "no certificate\n").into_response(),
	}
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

/// Admit a one-shot HTTP read: the auth server sees the path, the raw query, the
/// SNI on HTTPS, the peer, and any certificate. Only a grant that may
/// subscribe counts as a request; the returned origin meters reads under its token.
// The `Err` is axum's own `ErrorResponse`, so there is nothing here to box.
#[expect(clippy::result_large_err, reason = "the error type is axum's, not ours")]
async fn admit_http(
	state: &WebState,
	path: String,
	query: AuthQuery,
	uri: &http::Uri,
	server_name: Option<Extension<ServerName>>,
	remote: net::SocketAddr,
	mtls: Option<Extension<MtlsPeer>>,
) -> axum::response::Result<(auth::Lease, moq_net::origin::Consumer)> {
	// The public request API represents a missing or root path as empty; the
	// contract says what was dialed, and a URL always starts with `/`.
	let mut request = state
		.auth
		.request(moq_auth::Transport::Http, format!("/{}", path.trim_start_matches('/')));
	request.query = uri
		.query()
		.map(str::to_owned)
		.or_else(|| query.jwt.map(|jwt| format!("jwt={jwt}")));
	request.server_name = server_name.map(|Extension(ServerName(name))| name);
	request.remote = Some(remote);
	request.tls = mtls.and_then(|Extension(MtlsPeer(identity))| auth::peer(&identity));
	// The listener's timeout bounds admission as it bounds the request headers. An
	// HTTP read has no handshake deadline to inherit, so the bound starts here.
	let admit = state.auth.admit(request);
	let lease = match state.timeout {
		Some(timeout) => kio::clock::timeout(timeout, admit)
			.await
			.unwrap_or_else(|_| Err(auth::Error::Unavailable("the admission timed out".into()))),
		None => admit.await,
	}
	.inspect_err(|err| state.cluster.refusals.record_auth(err))?;
	let token = lease.token();
	let Some(origin) = state.cluster.subscriber(token) else {
		state.cluster.refusals.record(crate::refusals::RefusalKind::Forbidden);
		return Err(StatusCode::UNAUTHORIZED.into());
	};
	let stats = state.cluster.stats.tier(token.tier.clone()).request(&token.label);
	let origin = origin.consume().with_stats(stats.clone());
	Ok((lease.with_stats(stats), origin))
}

/// Serve the announced broadcasts for a given prefix.
// The `Err` is axum's own `ErrorResponse`, so there is nothing here to box.
#[expect(clippy::result_large_err, reason = "the error type is axum's, not ours")]
async fn serve_announced(
	path: Option<Path<String>>,
	Query(query): Query<AuthQuery>,
	mtls: Option<Extension<MtlsPeer>>,
	server_name: Option<Extension<ServerName>>,
	ConnectInfo(remote): ConnectInfo<net::SocketAddr>,
	uri: http::Uri,
	State(state): State<Arc<WebState>>,
) -> axum::response::Result<String> {
	let prefix = match path {
		Some(Path(prefix)) => prefix,
		None => String::new(),
	};

	let (lease, origin) = admit_http(&state, prefix, query, &uri, server_name, remote, mtls).await?;
	let mut announced = origin.announced();
	let mut broadcasts = Vec::new();

	while let Some(event) = announced.try_next() {
		if let moq_net::announce::Event::Start(update)
		| moq_net::announce::Event::Update(update)
		| moq_net::announce::Event::Restart(update)
		| moq_net::announce::Event::Takeover { announce: update, .. } = event
		{
			broadcasts.push(update.prefix);
		}
	}

	lease.close("done", moq_auth::Bytes::default());
	Ok(broadcasts
		.iter()
		.map(ToString::to_string)
		.collect::<Vec<_>>()
		.join("\n"))
}

/// Serve the given group for a given track
// The `Err` is axum's own `ErrorResponse`, so there is nothing here to box.
#[expect(clippy::result_large_err, reason = "the error type is axum's, not ours")]
async fn serve_fetch(
	Path(path): Path<String>,
	Query(params): Query<FetchParams>,
	mtls: Option<Extension<MtlsPeer>>,
	server_name: Option<Extension<ServerName>>,
	ConnectInfo(remote): ConnectInfo<net::SocketAddr>,
	uri: http::Uri,
	State(state): State<Arc<WebState>>,
) -> axum::response::Result<ServeGroup> {
	// The path containts a broadcast/track
	let mut path: Vec<&str> = path.split('/').collect();
	let track = path.pop().unwrap().to_string();

	// We need at least a broadcast and a track.
	if path.is_empty() {
		return Err(StatusCode::BAD_REQUEST.into());
	}

	let (lease, origin) = admit_http(&state, path.join("/"), params.auth, &uri, server_name, remote, mtls).await?;
	// The token's root is the canonical (alias-resolved) broadcast path.
	let broadcast = lease.token().root.to_string();

	tracing::info!(%broadcast, %track, "fetching track");

	let deadline = kio::clock::now() + tokio::time::Duration::from_secs(30);

	// The lease outlives this handler: the body streams after it returns, so the
	// response holds the lease and ends it when the last frame is served.
	let result = kio::clock::timeout_at(deadline, async {
		// NOTE: The auth token is already scoped to the broadcast.
		// Block until a route covers the broadcast (within the fetch deadline) so
		// freshly-connected subscribers don't get a spurious 404 before the announce arrives.
		let mut following = origin.follow("", None).map_err(|_| StatusCode::NOT_FOUND)?;
		let broadcast = following.available().await.map_err(|_| StatusCode::NOT_FOUND)?;
		let sequence = match params.group {
			FetchGroup::Num(sequence) => Some(sequence),
			FetchGroup::Latest => None,
		};
		let group = match async { crate::fetch_group(&broadcast.track(&track)?, sequence).await }.await {
			Ok(group) => group,
			// A miss upstream arrives as the stream reset that refused the FETCH. A datagram
			// group is never cached, and a group past the track's max age is gone, so both
			// are misses too.
			Err(
				moq_net::Error::NotFound
				| moq_net::Error::NotFetchable
				| moq_net::Error::Old
				| moq_net::Error::Stream(
					moq_net::StreamError::NotFound | moq_net::StreamError::NotFetchable | moq_net::StreamError::Old,
				),
			) => {
				return Err(StatusCode::NOT_FOUND);
			}
			Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
		};

		tracing::info!(%track, group = %group.sequence, "serving group");

		Ok(group)
	})
	.await;

	match result {
		Ok(Ok(group)) => Ok(ServeGroup {
			group,
			deadline,
			lease: Some(lease),
		}),
		Ok(Err(status)) => {
			lease.close(status.to_string(), moq_auth::Bytes::default());
			Err(status.into())
		}
		Err(_) => {
			lease.close("timeout", moq_auth::Bytes::default());
			Err(StatusCode::GATEWAY_TIMEOUT.into())
		}
	}
}

/// A group streamed as the response body, holding the fetch's lease until the
/// last frame is served so a refusal or expiry mid-transfer cuts it off and the
/// `end` event carries the outcome.
struct ServeGroup {
	group: moq_net::group::Consumer,
	deadline: kio::clock::Instant,
	/// Taken when the body ends, so the reason is reported once.
	lease: Option<auth::Lease>,
}

impl ServeGroup {
	async fn next(&mut self) -> moq_net::Result<Option<Bytes>> {
		let Some(lease) = self.lease.as_mut() else {
			return Ok(None);
		};
		tokio::select! {
			res = kio::clock::timeout_at(self.deadline, self.group.read_frame()) => match res {
				Ok(res) => Ok(res?.map(|frame| frame.payload)),
				Err(_) => Err(moq_net::Error::Timeout),
			},
			why = lease.ended() => {
				tracing::info!(%why, "lease ended, closing fetch");
				Err(crate::auth::error(&why))
			}
		}
	}

	/// End the lease with the body's outcome.
	fn end(&mut self, reason: &str) {
		if let Some(lease) = self.lease.take() {
			lease.close(reason, moq_auth::Bytes::default());
		}
	}
}

impl IntoResponse for ServeGroup {
	fn into_response(self) -> Response {
		// One stream owns the body for its whole life, so the waiters `next`
		// registers with the group and the lease survive between polls.
		let frames = futures::stream::unfold(Some(self), async |serve| {
			let mut serve = serve?;
			match serve.next().await {
				Ok(Some(data)) => Some((Ok(data), Some(serve))),
				Ok(None) => {
					serve.end("done");
					None
				}
				Err(err) => {
					serve.end(&err.to_string());
					Some((Err(err), None))
				}
			}
		});
		(
			[(http::header::CONTENT_TYPE, "application/octet-stream")],
			Body::from_stream(frames),
		)
			.into_response()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use rcgen::{CertificateParams, KeyPair};
	use std::io::Write;
	use tempfile::TempDir;

	#[tokio::test(start_paused = true)]
	async fn authenticated_http_responses_are_private_and_cannot_be_sniffed() {
		let dir = TempDir::new().unwrap();
		let (_, cert, _) = make_certs(&dir);
		let certificates = moq_tokio::tls::Certificates::from_pem(&std::fs::read(cert).unwrap()).unwrap();
		let auth = crate::auth::Config {
			public_subscribe: vec![moq_auth::Pattern::all()],
			..Default::default()
		}
		.init("test", &moq_tokio::tls::Connect::default(), false)
		.unwrap();
		let cluster = cluster::Cluster::new(crate::cluster::Options::default()).unwrap();
		let broadcast = cluster.origin.create_broadcast("room").unwrap();
		broadcast.announce(Default::default()).unwrap();
		let track = broadcast
			.create_track(
				"video",
				moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI),
			)
			.unwrap();
		let mut group = track.append_group().unwrap();
		const PAYLOAD: &[u8] = b"<!doctype html><script>alert(1)</script>";
		group.write_frame(moq_net::Timestamp::ZERO, PAYLOAD).unwrap();
		group.finish().unwrap();
		let mut refused = Web::new(
			auth::Auth::refuse("test"),
			cluster.clone(),
			certificates.clone(),
			Config::default(),
		)
		.routes();
		for path in ["/fetch/room/video", "/announced/room"] {
			let mut request = http::Request::builder().uri(path).body(Body::empty()).unwrap();
			request
				.extensions_mut()
				.insert(ConnectInfo("127.0.0.1:1234".parse::<net::SocketAddr>().unwrap()));
			let response = refused.call(request).await.unwrap();
			assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
			assert_eq!(response.headers()[http::header::CACHE_CONTROL], "private, no-store");
			assert_eq!(response.headers()[http::header::X_CONTENT_TYPE_OPTIONS], "nosniff");
		}
		let web = Web::new(auth, cluster, certificates, Config::default());
		let mut routes = web.routes();
		for (path, expected, content_type) in [
			(
				"/fetch/room/video?group=0",
				StatusCode::OK,
				Some("application/octet-stream"),
			),
			("/fetch/room/video?group=1", StatusCode::NOT_FOUND, None),
			("/fetch/room/video?group=invalid", StatusCode::BAD_REQUEST, None),
			("/announced/room", StatusCode::OK, Some("text/plain; charset=utf-8")),
			("/announced", StatusCode::OK, Some("text/plain; charset=utf-8")),
		] {
			let mut request = http::Request::builder().uri(path).body(Body::empty()).unwrap();
			request
				.extensions_mut()
				.insert(ConnectInfo("127.0.0.1:1234".parse::<net::SocketAddr>().unwrap()));
			let response = routes.call(request).await.unwrap();
			assert_eq!(response.status(), expected, "{path}");
			assert_eq!(
				response.headers().get(http::header::CACHE_CONTROL).unwrap(),
				"private, no-store",
				"{path}"
			);
			assert_eq!(
				response.headers().get(http::header::X_CONTENT_TYPE_OPTIONS).unwrap(),
				"nosniff",
				"{path}"
			);
			if let Some(content_type) = content_type {
				assert_eq!(
					response.headers().get(http::header::CONTENT_TYPE).unwrap(),
					content_type,
					"{path}"
				);
			}
			if path == "/fetch/room/video?group=0" {
				assert_eq!(
					axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap(),
					PAYLOAD
				);
			}
		}
	}

	#[tokio::test(start_paused = true)]
	async fn http_refusals_count_at_both_endpoints() {
		use crate::refusals::RefusalKind;
		let dir = TempDir::new().unwrap();
		let (_, cert, _) = make_certs(&dir);
		let certificates = moq_tokio::tls::Certificates::from_pem(&std::fs::read(cert).unwrap()).unwrap();
		for fetch in [false, true] {
			for publish_only in [false, true] {
				let auth = if publish_only {
					crate::auth::Config {
						public_publish: vec![moq_auth::Pattern::all()],
						..Default::default()
					}
					.init("test", &moq_tokio::tls::Connect::default(), false)
					.unwrap()
				} else {
					auth::Auth::refuse("test")
				};
				let cluster = cluster::Cluster::new(crate::cluster::Options::default()).unwrap();
				let web = Web::new(auth, cluster, certificates.clone(), Config::default());
				let state = web.state;
				let remote = ConnectInfo("127.0.0.1:1234".parse().unwrap());
				let status = if fetch {
					serve_fetch(
						Path("room/video".to_owned()),
						Query(FetchParams {
							auth: AuthQuery { jwt: None },
							group: FetchGroup::Latest,
						}),
						None,
						None,
						remote,
						"/fetch/room/video".parse().unwrap(),
						State(state.clone()),
					)
					.await
					.map(|_| ())
					.into_response()
					.status()
				} else {
					serve_announced(
						Some(Path("room".to_owned())),
						Query(AuthQuery { jwt: None }),
						None,
						None,
						remote,
						"/announced/room".parse().unwrap(),
						State(state.clone()),
					)
					.await
					.map(|_| ())
					.into_response()
					.status()
				};
				assert_eq!(status, StatusCode::UNAUTHORIZED);
				let reason = if publish_only {
					RefusalKind::Forbidden
				} else {
					RefusalKind::Refused
				};
				assert_eq!(
					state.cluster.refusals.count(reason),
					1,
					"fetch={fetch}, publish_only={publish_only}"
				);
			}
		}
	}

	#[tokio::test(start_paused = true)]
	async fn http_refusals_keep_the_deciders_root_and_tier() {
		use moq_net::stats::{Registry, Report};
		let dir = TempDir::new().unwrap();
		let (_, cert, _) = make_certs(&dir);
		let certificates = moq_tokio::tls::Certificates::from_pem(&std::fs::read(cert).unwrap()).unwrap();
		let (auth, mut admissions) = auth::Auth::embedded("test");
		let decider = tokio::spawn(async move {
			for _ in 0..2 {
				let mut refusal = moq_auth::Refusal::default();
				refusal.reason = moq_auth::refusal::Reason::Expired;
				refusal.root = Some("tenant".into());
				refusal.tier = Some("edge".into());
				admissions.next().await.unwrap().refuse(auth::Error::Refused(refusal));
			}
		});
		let registry = Registry::new(Default::default());
		let mut cluster = cluster::Cluster::new(crate::cluster::Options::default()).unwrap();
		cluster.refusals = crate::refusals::Refusals::new(&registry);
		let state = Web::new(auth, cluster, certificates, Config::default()).state;
		let fetched = serve_fetch(
			Path("room/video".to_owned()),
			Query(FetchParams {
				auth: AuthQuery { jwt: None },
				group: FetchGroup::Latest,
			}),
			None,
			None,
			ConnectInfo("127.0.0.1:1234".parse().unwrap()),
			"/fetch/room/video".parse().unwrap(),
			State(state.clone()),
		)
		.await;
		assert!(fetched.is_err(), "fetch was admitted");
		let announced = serve_announced(
			Some(Path("room".to_owned())),
			Query(AuthQuery { jwt: None }),
			None,
			None,
			ConnectInfo("127.0.0.1:1234".parse().unwrap()),
			"/announced/room".parse().unwrap(),
			State(state.clone()),
		)
		.await;
		assert!(announced.is_err(), "announced was admitted");
		decider.await.unwrap();

		let mut report = Report::default();
		registry.report(&mut report);
		let [entry] = report.sessions.as_slice() else {
			panic!("expected one attributed entry: {:?}", report.sessions);
		};
		assert_eq!(entry.label.as_str(), "tenant");
		assert_eq!(entry.tier.as_str(), "edge");
		assert_eq!(entry.presence.sessions_refused.expired, 2);
	}

	#[tokio::test(start_paused = true)]
	async fn http_fetch_meters_bytes_without_session_presence() {
		use moq_net::stats::{Registry, Report, Tier};
		let dir = TempDir::new().unwrap();
		let (_, cert, _) = make_certs(&dir);
		let certificates = moq_tokio::tls::Certificates::from_pem(&std::fs::read(cert).unwrap()).unwrap();
		let (auth, mut admissions) = auth::Auth::embedded("test");
		let decider = tokio::spawn(async move {
			let mut leases = Vec::new();
			for _ in 0..6 {
				let admission = admissions.next().await.unwrap();
				assert_eq!(admission.request.transport, moq_auth::Transport::Http);
				let mut grant = moq_auth::Grant::new(moq_auth::Patterns::new(), moq_auth::Pattern::all());
				grant.tier = Some(
					admission
						.request
						.query
						.as_deref()
						.unwrap()
						.strip_prefix("tier=")
						.unwrap()
						.to_owned(),
				);
				let (producer, consumer) = moq_auth::lease::Producer::new(grant);
				leases.push(producer);
				admission.grant(consumer);
			}
			leases
		});
		let mut cluster = cluster::Cluster::new(crate::cluster::Options::default()).unwrap();
		cluster.stats = Registry::new(Default::default());
		let broadcast = cluster.origin.create_broadcast("room").unwrap();
		broadcast.announce(Default::default()).unwrap();
		let track = broadcast
			.create_track(
				"video",
				moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI),
			)
			.unwrap();
		let mut group = track.append_group().unwrap();
		group.write_frame(moq_net::Timestamp::ZERO, b"hello".as_ref()).unwrap();
		group.finish().unwrap();
		let state = Web::new(auth, cluster, certificates, Config::default()).state;
		for tier in ["first", "second"] {
			for selector in [FetchGroup::Num(0), FetchGroup::Latest] {
				let mut served = serve_fetch(
					Path("room/video".into()),
					Query(FetchParams {
						auth: AuthQuery { jwt: None },
						group: selector,
					}),
					None,
					None,
					ConnectInfo("127.0.0.1:1234".parse().unwrap()),
					format!("/fetch/room/video?tier={tier}").parse().unwrap(),
					State(state.clone()),
				)
				.await
				.unwrap();
				assert_eq!(served.next().await.unwrap().unwrap(), "hello");
				assert!(served.next().await.unwrap().is_none());
				served.end("done");
			}
			let before = state.cluster.stats.snapshot();
			serve_announced(
				Some(Path("room".into())),
				Query(AuthQuery { jwt: None }),
				None,
				None,
				ConnectInfo("127.0.0.1:1234".parse().unwrap()),
				format!("/announced/room?tier={tier}").parse().unwrap(),
				State(state.clone()),
			)
			.await
			.unwrap();
			assert_eq!(
				before
					.traffic()
					.iter()
					.map(|(_, _, traffic)| traffic.bytes)
					.sum::<u64>(),
				state
					.cluster
					.stats
					.snapshot()
					.traffic()
					.iter()
					.map(|(_, _, traffic)| traffic.bytes)
					.sum::<u64>(),
				"announced serves no media"
			);
		}
		let _leases = decider.await.unwrap();
		let mut report = Report::default();
		state.cluster.stats.report(&mut report);
		assert_eq!(report.traffic.len(), 2);
		assert_eq!(report.sessions.len(), 2);
		for label in ["first", "second"] {
			let tier = Tier::new(moq_net::Path::from_static(label));
			let traffic = report.traffic.iter().find(|entry| entry.tier == tier).unwrap();
			assert_eq!(traffic.path.as_str(), "room");
			assert_eq!(
				traffic.publisher.bytes, 10,
				"numbered and latest payload on the token's tier"
			);
			assert_eq!(traffic.subscriber.bytes, 0);
			let presence = report.sessions.iter().find(|entry| entry.tier == tier).unwrap();
			assert_eq!(
				presence.label.as_str(),
				"",
				"an unlabeled request counts under the empty label"
			);
			assert_eq!(presence.presence.requests, 3);
			assert_eq!(
				(presence.presence.sessions_started, presence.presence.sessions_ended),
				(0, 0)
			);
		}
	}

	/// A group the track's max age has dropped is gone, which is a 404 rather than a 500.
	#[tokio::test(start_paused = true)]
	async fn http_fetch_of_an_expired_group_is_not_found() {
		let dir = TempDir::new().unwrap();
		let (_, cert, _) = make_certs(&dir);
		let certificates = moq_tokio::tls::Certificates::from_pem(&std::fs::read(cert).unwrap()).unwrap();
		let auth = crate::auth::Config {
			public_subscribe: vec![moq_auth::Pattern::all()],
			..Default::default()
		}
		.init("test", &moq_tokio::tls::Connect::default(), false)
		.unwrap();
		let cluster = cluster::Cluster::new(crate::cluster::Options::default()).unwrap();
		let broadcast = cluster.origin.create_broadcast("room").unwrap();
		broadcast.announce(Default::default()).unwrap();
		let track = broadcast
			.create_track(
				"video",
				moq_net::track::Info::default()
					.with_timescale(moq_net::Timescale::MILLI)
					.with_max_age(std::time::Duration::from_millis(500)),
			)
			.unwrap();
		// Group 0 can only present until group 1 starts at 1s, a full second behind the 2s
		// edge: past the 500ms max age, so it is no longer served.
		for millis in [0, 1000, 2000] {
			let mut group = track.append_group().unwrap();
			group
				.write_frame(moq_net::Timestamp::from_millis(millis).unwrap(), b"frame".as_ref())
				.unwrap();
			group.finish().unwrap();
		}
		let state = Web::new(auth, cluster, certificates, Config::default()).state;
		let status = serve_fetch(
			Path("room/video".into()),
			Query(FetchParams {
				auth: AuthQuery { jwt: None },
				group: FetchGroup::Num(0),
			}),
			None,
			None,
			ConnectInfo("127.0.0.1:1234".parse().unwrap()),
			"/fetch/room/video?group=0".parse().unwrap(),
			State(state),
		)
		.await
		.map(|_| ())
		.into_response()
		.status();
		assert_eq!(status, StatusCode::NOT_FOUND);
	}

	#[tokio::test(start_paused = true)]
	async fn http_refused_subscribe_counts_no_request() {
		let dir = TempDir::new().unwrap();
		let (_, cert, _) = make_certs(&dir);
		let certificates = moq_tokio::tls::Certificates::from_pem(&std::fs::read(cert).unwrap()).unwrap();
		let (auth, mut admissions) = auth::Auth::embedded("test");
		let decider = tokio::spawn(async move {
			let mut leases = Vec::new();
			for _ in 0..2 {
				let admission = admissions.next().await.unwrap();
				// Publish-only: admitted by auth, but forbidden from reading.
				let grant = moq_auth::Grant::new(moq_auth::Pattern::all(), moq_auth::Patterns::new());
				let (producer, consumer) = moq_auth::lease::Producer::new(grant);
				leases.push(producer);
				admission.grant(consumer);
			}
			leases
		});
		let mut cluster = cluster::Cluster::new(crate::cluster::Options::default()).unwrap();
		cluster.stats = moq_net::stats::Registry::new(Default::default());
		let state = Web::new(auth, cluster, certificates, Config::default()).state;
		let peer = || ConnectInfo("127.0.0.1:1234".parse().unwrap());
		let fetched = serve_fetch(
			Path("room/video".into()),
			Query(FetchParams {
				auth: AuthQuery { jwt: None },
				group: FetchGroup::Latest,
			}),
			None,
			None,
			peer(),
			"/fetch/room/video".parse().unwrap(),
			State(state.clone()),
		)
		.await;
		assert!(fetched.is_err());
		let announced = serve_announced(
			Some(Path("room".into())),
			Query(AuthQuery { jwt: None }),
			None,
			None,
			peer(),
			"/announced/room".parse().unwrap(),
			State(state.clone()),
		)
		.await;
		assert!(announced.is_err());
		let _leases = decider.await.unwrap();
		let requests: u64 = state
			.cluster
			.stats
			.snapshot()
			.sessions()
			.iter()
			.map(|(_, presence)| presence.requests)
			.sum();
		assert_eq!(requests, 0, "a refused read is not an admitted request");
	}

	fn make_certs(dir: &TempDir) -> (PathBuf, PathBuf, PathBuf) {
		make_named_certs(dir, "server", "localhost")
	}

	/// Generate a CA + server cert/key on disk and return the temp paths.
	/// Modeled after `auth.rs::mtls_fixture`.
	fn make_named_certs(dir: &TempDir, name: &str, hostname: &str) -> (PathBuf, PathBuf, PathBuf) {
		let _ = moq_tokio::crypto::install_default();

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

	#[test]
	fn https_watch_paths_include_roots() {
		let cert = PathBuf::from("cert.pem");
		let key = PathBuf::from("key.pem");
		let root = PathBuf::from("root.pem");
		assert_eq!(
			https_watch_paths(
				std::slice::from_ref(&cert),
				std::slice::from_ref(&key),
				std::slice::from_ref(&root)
			),
			vec![cert, key, root]
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

	/// A real accepted socket must reach the request as an extension.
	///
	/// Without a capture the handle is `None`, qmux gets no socket, and the session
	/// advertises no Probe capability -- silently, which is why this drives an actual
	/// `TcpStream` rather than constructing the service by hand.
	#[cfg(all(unix, feature = "websocket"))]
	#[tokio::test]
	async fn web_acceptor_surfaces_the_accepted_socket() {
		use axum::http::Request;
		use std::convert::Infallible;

		/// Reports whether the socket extension arrived on the request.
		#[derive(Clone)]
		struct EchoSocket;
		impl Service<Request<()>> for EchoSocket {
			type Response = bool;
			type Error = Infallible;
			type Future = std::pin::Pin<Box<dyn Future<Output = Result<bool, Infallible>> + Send>>;
			fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
				Poll::Ready(Ok(()))
			}
			fn call(&mut self, req: Request<()>) -> Self::Future {
				let has = req.extensions().get::<SocketStats>().is_some();
				Box::pin(async move { Ok(has) })
			}
		}

		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let addr = listener.local_addr().unwrap();
		let connecting = tokio::spawn(async move { tokio::net::TcpStream::connect(addr).await.unwrap() });
		let (accepted, _) = listener.accept().await.unwrap();
		let _client = connecting.await.unwrap();

		let (stream, mut service) = Accept::<_, EchoSocket>::accept(&WebAcceptor::plain(true), accepted, EchoSocket)
			.await
			.unwrap();

		assert!(
			service.socket.is_some(),
			"the acceptor must capture the descriptor of the socket it accepted"
		);

		let req = Request::builder().body(()).unwrap();
		assert!(
			service.call(req).await.unwrap(),
			"the captured socket must reach the request, or qmux never sees it"
		);

		// The kernel measured this during the handshake, so it is readable already --
		// which is what lets the Probe capability be decided at SETUP. Only where
		// qmux actually reads a TCP info struct: elsewhere on unix it supplies an
		// empty implementation and every metric is `None`, which is not a failure.
		#[cfg(any(target_os = "linux", target_os = "macos"))]
		{
			let stats = service.socket.as_ref().unwrap();
			assert!(
				qmux::SocketStats::rtt(&*stats.0).is_some(),
				"a connected TCP socket must report an RTT immediately"
			);
		}

		drop(stream);
	}

	/// With no WebSocket route to read them, the capture is skipped.
	///
	/// The handle holds a duplicated descriptor for the life of the connection, so
	/// capturing it for every API, HLS, and health-check connection would halve the
	/// descriptors a relay can spend on connections, for nothing.
	#[cfg(all(unix, feature = "websocket"))]
	#[tokio::test]
	async fn web_acceptor_skips_capture_without_websockets() {
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let addr = listener.local_addr().unwrap();
		let connecting = tokio::spawn(async move { tokio::net::TcpStream::connect(addr).await.unwrap() });
		let (accepted, _) = listener.accept().await.unwrap();
		let _client = connecting.await.unwrap();

		let (stream, service) = Accept::<_, ()>::accept(&WebAcceptor::plain(false), accepted, ())
			.await
			.unwrap();

		assert!(
			service.socket.is_none(),
			"no WebSocket route can use the handle, so it must not hold a descriptor"
		);

		drop(stream);
	}

	/// Reports whether the connection's socket reached the request.
	#[cfg(all(unix, feature = "websocket"))]
	async fn report_socket(socket: Option<Extension<SocketStats>>) -> &'static str {
		match socket {
			Some(_) => "captured",
			None => "missing",
		}
	}

	/// `GET /socket` over `io`, returning the body the handler produced.
	///
	/// Hand-rolled rather than reached through an HTTP client so the same request
	/// works over TLS and plain TCP. `Connection: close` is what ends the read.
	#[cfg(all(unix, feature = "websocket"))]
	async fn get_socket<S: AsyncRead + AsyncWrite + Unpin>(mut io: S) -> String {
		use tokio::io::{AsyncReadExt, AsyncWriteExt};

		io.write_all(b"GET /socket HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
			.await
			.unwrap();

		let mut response = String::new();
		// An unclean TLS shutdown is an error even after a complete response, so the
		// body decides whether the exchange worked, not the read.
		let _ = io.read_to_string(&mut response).await;
		response
			.split_once("\r\n\r\n")
			.unwrap_or_else(|| panic!("no HTTP response body in {response:?}"))
			.1
			.to_string()
	}

	/// A TLS client trusting only `ca`, so the handshake also proves the relay
	/// served the certificate it was configured with.
	#[cfg(all(unix, feature = "websocket"))]
	fn tls_connector(ca: &std::path::Path) -> tokio_rustls::TlsConnector {
		use rustls::pki_types::{CertificateDer, pem::PemObject};

		let pem = std::fs::read(ca).unwrap();
		let mut roots = rustls::RootCertStore::empty();
		for cert in CertificateDer::pem_slice_iter(&pem) {
			roots.add(cert.unwrap()).unwrap();
		}

		let mut config = rustls::ClientConfig::builder()
			.with_root_certificates(roots)
			.with_no_client_auth();
		// The listener advertises h2 first, and this speaks HTTP/1.1 by hand.
		config.alpn_protocols = vec![b"http/1.1".to_vec()];

		tokio_rustls::TlsConnector::from(Arc::new(config))
	}

	/// [`Web::serve`] must install the capturing acceptor on every listener it opens.
	///
	/// The tests above cover the acceptor; this covers its installation. A listener
	/// that skips it hands qmux a session with no descriptor, which stays on the
	/// fallback jitter buffer while the other listener still looks correct, so both
	/// run here and each is asked whether the capture reached the request.
	#[cfg(all(unix, feature = "websocket"))]
	#[tokio::test]
	async fn serve_captures_the_socket_on_every_listener() {
		let dir = TempDir::new().unwrap();
		let (ca, cert, key) = make_certs(&dir);
		let mut config = Config::default();
		config.http.listen = Some("127.0.0.1:0".parse().unwrap());
		config.https.listen = Some("127.0.0.1:0".parse().unwrap());
		config.https.cert = vec![cert.clone()];
		config.https.key = vec![key];

		// The probed route is the test's own, so auth never runs; it just has to be
		// configured with something for `Web` to build.
		let auth_config = crate::auth::Config {
			public_subscribe: vec![moq_auth::Pattern::all()],
			..Default::default()
		};
		let auth = auth_config
			.init("test", &moq_tokio::tls::Connect::default(), false)
			.unwrap();
		let cluster = cluster::Cluster::new(crate::cluster::Options::default()).unwrap();
		let certificates = moq_tokio::tls::Certificates::from_pem(&std::fs::read(&cert).unwrap()).unwrap();

		let web = Web::new(auth, cluster, certificates, config).bind().unwrap();
		let Addrs {
			http: Some(http),
			https: Some(https),
		} = web.addrs()
		else {
			panic!("both listeners are configured");
		};
		let serving = tokio::spawn(web.serve(Router::new().route("/socket", get(report_socket))));

		assert_eq!(
			get_socket(tokio::net::TcpStream::connect(http).await.unwrap()).await,
			"captured",
			"the HTTP listener must install the capturing acceptor"
		);

		let tcp = tokio::net::TcpStream::connect(https).await.unwrap();
		let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
		let tls = tls_connector(&ca).connect(name, tcp).await.expect("TLS handshake");
		assert_eq!(
			get_socket(tls).await,
			"captured",
			"the HTTPS listener must install the capturing acceptor"
		);

		serving.abort();
	}

	/// `server_name` is the SNI that selected the certificate, on an HTTP read and a
	/// WebSocket alike, never the `Host` the client wrote; plain HTTP has no SNI, so it
	/// reports none rather than the header. Real sockets, so this runs on the wall clock.
	#[cfg(all(unix, feature = "websocket"))]
	#[tokio::test]
	async fn server_name_is_the_sni_never_the_host() {
		use tokio::io::{AsyncReadExt, AsyncWriteExt};

		trait Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static {}
		impl<T: AsyncRead + AsyncWrite + Unpin + Send + 'static> Stream for T {}

		let dir = TempDir::new().unwrap();
		let (ca, cert, key) = make_certs(&dir);
		let mut config = Config::default();
		config.http.listen = Some("127.0.0.1:0".parse().unwrap());
		config.https.listen = Some("127.0.0.1:0".parse().unwrap());
		config.https.cert = vec![cert.clone()];
		config.https.key = vec![key];
		config.ws = true;
		let (auth, mut admissions) = crate::auth::Auth::embedded("test");
		let cluster = cluster::Cluster::new(crate::cluster::Options::default()).unwrap();
		let certificates = moq_tokio::tls::Certificates::from_pem(&std::fs::read(&cert).unwrap()).unwrap();
		let web = Web::new(auth, cluster, certificates, config).bind().unwrap();
		let Addrs {
			http: Some(http),
			https: Some(https),
		} = web.addrs()
		else {
			panic!("both listeners are configured");
		};
		let routes = web.routes();
		let serving = tokio::spawn(web.serve(routes));

		let connect = async |tls: bool| -> Box<dyn Stream> {
			let tcp = tokio::net::TcpStream::connect(if tls { https } else { http })
				.await
				.unwrap();
			if !tls {
				return Box::new(tcp) as Box<dyn Stream>;
			}
			let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
			Box::new(tls_connector(&ca).connect(name, tcp).await.expect("TLS handshake"))
		};

		for tls in [true, false] {
			let expected = tls.then_some("localhost");

			let mut stream = connect(tls).await;
			stream
				.write_all(b"GET /announced HTTP/1.1\r\nHost: tenant.example\r\nConnection: close\r\n\r\n")
				.await
				.unwrap();
			let admission = admissions.next().await.expect("an HTTP admission");
			assert_eq!(admission.request.transport, moq_auth::Transport::Http);
			assert_eq!(admission.request.server_name.as_deref(), expected, "HTTP, tls={tls}");
			// Unanswered, the read fails as unavailable; only the request mattered.
			drop(admission);
			let _ = stream.read_to_end(&mut Vec::new()).await;

			let mut stream = connect(tls).await;
			stream
				.write_all(b"GET /room HTTP/1.1\r\nHost: tenant.example\r\n")
				.await
				.unwrap();
			let client = tokio::spawn(drain::upgrade_while_draining(stream, true));
			let admission = admissions.next().await.expect("a WebSocket admission");
			assert_eq!(admission.request.transport, moq_auth::Transport::WebSocket);
			assert_eq!(
				admission.request.server_name.as_deref(),
				expected,
				"WebSocket, tls={tls}"
			);
			admission.grant(moq_auth::lease::Consumer::fixed(moq_auth::Grant::new(
				moq_auth::Pattern::all(),
				moq_auth::Pattern::all(),
			)));
			drop(client.await.expect("the WebSocket session connects"));
		}

		serving.abort();
	}

	/// An embedded route can read the accepted address with Axum's standard
	/// extractor, regardless of client-supplied proxy headers.
	#[tokio::test]
	async fn embedded_routes_receive_the_socket_address() {
		use tokio::io::{AsyncReadExt, AsyncWriteExt};
		let dir = TempDir::new().unwrap();
		let (_, cert, _) = make_certs(&dir);
		let mut config = Config::default();
		config.http.listen = Some("127.0.0.1:0".parse().unwrap());
		let auth = crate::auth::Config {
			public_subscribe: vec![moq_auth::Pattern::all()],
			..Default::default()
		}
		.init("test", &moq_tokio::tls::Connect::default(), false)
		.unwrap();
		let cluster = cluster::Cluster::new(crate::cluster::Options::default()).unwrap();
		let certificates = moq_tokio::tls::Certificates::from_pem(&std::fs::read(&cert).unwrap()).unwrap();
		let web = Web::new(auth, cluster, certificates, config).bind().unwrap();
		let http = web.addrs().http.unwrap();
		let app = Router::new().route(
			"/remote",
			get(|ConnectInfo(remote): ConnectInfo<net::SocketAddr>| async move { remote.to_string() }),
		);
		let serving = tokio::spawn(web.serve(app));
		let mut tcp = tokio::net::TcpStream::connect(http).await.unwrap();
		let expected = tcp.local_addr().unwrap().to_string();
		tcp.write_all(
			b"GET /remote HTTP/1.1\r\nHost: localhost\r\nX-Forwarded-For: 192.0.2.1\r\nConnection: close\r\n\r\n",
		)
		.await
		.unwrap();
		let mut response = String::new();
		tcp.read_to_string(&mut response).await.unwrap();
		assert_eq!(response.split("\r\n\r\n").nth(1), Some(expected.as_str()), "{response}");
		serving.abort();
		let _ = serving.await;
	}

	/// HTTPS settings without the HTTPS listener that would read them are refused,
	/// not ignored.
	#[tokio::test]
	async fn https_settings_require_the_https_listener() {
		type Set = fn(&mut Https);
		let cases: [(&str, Set); 3] = [
			("--web-https-cert", |https| https.cert = vec!["cert.pem".into()]),
			("--web-https-key", |https| https.key = vec!["key.pem".into()]),
			("--web-https-root", |https| https.root = vec!["ca.pem".into()]),
		];
		let dir = TempDir::new().unwrap();
		let (_, cert, _) = make_certs(&dir);
		let certificates = moq_tokio::tls::Certificates::from_pem(&std::fs::read(&cert).unwrap()).unwrap();
		for (flag, set) in cases {
			let mut config = Config::default();
			set(&mut config.https);
			let cluster = cluster::Cluster::new(crate::cluster::Options::default()).unwrap();
			let web = Web::new(crate::auth::Auth::refuse("test"), cluster, certificates.clone(), config);
			let err = match web.bind() {
				Ok(_) => panic!("{flag} was ignored"),
				Err(err) => err.to_string(),
			};
			assert!(err.contains(flag) && err.contains("--web-https-listen"), "{err}");
		}
	}

	/// A timeout past the clock's range waits forever, rather than reaching hyper,
	/// which would panic adding it to the clock on every connection.
	#[tokio::test]
	async fn unreachable_timeout_waits_forever() {
		let auth_config = crate::auth::Config {
			public_subscribe: vec![moq_auth::Pattern::all()],
			..Default::default()
		};
		let auth = auth_config
			.init("test", &moq_tokio::tls::Connect::default(), false)
			.unwrap();
		let cluster = cluster::Cluster::new(crate::cluster::Options::default()).unwrap();
		let dir = TempDir::new().unwrap();
		let (_, cert, _) = make_certs(&dir);
		let certificates = moq_tokio::tls::Certificates::from_pem(&std::fs::read(&cert).unwrap()).unwrap();
		let web = Web::new(auth, cluster, certificates, Config::default()).with_timeout(Some(std::time::Duration::MAX));
		assert_eq!(web.state.timeout, None);
	}

	/// A client that never finishes its request headers is disconnected once the
	/// timeout passes. Real sockets, so this runs on the wall clock and bounds the
	/// close from below.
	#[tokio::test]
	async fn slow_request_headers_are_timed_out() {
		use tokio::io::{AsyncReadExt, AsyncWriteExt};

		let timeout = std::time::Duration::from_millis(200);
		let mut config = Config::default();
		config.http.listen = Some("127.0.0.1:0".parse().unwrap());
		let auth_config = crate::auth::Config {
			public_subscribe: vec![moq_auth::Pattern::all()],
			..Default::default()
		};
		let auth = auth_config
			.init("test", &moq_tokio::tls::Connect::default(), false)
			.unwrap();
		let cluster = cluster::Cluster::new(crate::cluster::Options::default()).unwrap();
		let dir = TempDir::new().unwrap();
		let (_, cert, _) = make_certs(&dir);
		let certificates = moq_tokio::tls::Certificates::from_pem(&std::fs::read(&cert).unwrap()).unwrap();
		let web = Web::new(auth, cluster, certificates, config)
			.with_timeout(Some(timeout))
			.bind()
			.unwrap();
		let http = web.addrs().http.expect("the HTTP listener is configured");
		let serving = tokio::spawn(web.serve(Router::new()));

		let mut tcp = tokio::net::TcpStream::connect(http).await.unwrap();
		let start = kio::clock::now();
		tcp.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n").await.unwrap();

		let mut rest = Vec::new();
		kio::clock::timeout(std::time::Duration::from_secs(10), tcp.read_to_end(&mut rest))
			.await
			.expect("slow headers held the connection open")
			.ok();
		assert!(start.elapsed() >= timeout, "closed before the timeout");

		serving.abort();
	}

	/// An HTTP read's admission is bounded by the listener's timeout, like its request
	/// headers: a decision that never comes answers 502 once the timeout passes. Real
	/// sockets, so this runs on the wall clock and bounds the answer from below.
	#[tokio::test]
	async fn an_unanswered_http_admission_is_refused_at_the_timeout() {
		use tokio::io::{AsyncReadExt, AsyncWriteExt};

		let timeout = std::time::Duration::from_millis(200);
		let mut config = Config::default();
		config.http.listen = Some("127.0.0.1:0".parse().unwrap());
		// Held and never answered: the decider that never decides.
		let (auth, _admissions) = crate::auth::Auth::embedded("test");
		let cluster = cluster::Cluster::new(crate::cluster::Options::default()).unwrap();
		let dir = TempDir::new().unwrap();
		let (_, cert, _) = make_certs(&dir);
		let certificates = moq_tokio::tls::Certificates::from_pem(&std::fs::read(&cert).unwrap()).unwrap();
		let web = Web::new(auth, cluster, certificates, config)
			.with_timeout(Some(timeout))
			.bind()
			.unwrap();
		let http = web.addrs().http.expect("the HTTP listener is configured");
		let routes = web.routes();
		let serving = tokio::spawn(web.serve(routes));

		let mut tcp = tokio::net::TcpStream::connect(http).await.unwrap();
		let start = kio::clock::now();
		tcp.write_all(b"GET /announced HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
			.await
			.unwrap();

		let mut response = Vec::new();
		kio::clock::timeout(std::time::Duration::from_secs(10), tcp.read_to_end(&mut response))
			.await
			.expect("the unanswered admission held the request open")
			.ok();
		let response = String::from_utf8_lossy(&response);
		assert!(response.starts_with("HTTP/1.1 502 "), "{response}");
		assert!(start.elapsed() >= timeout, "refused before the timeout");

		serving.abort();
	}

	/// An HTTP/2 connection carries no more requests at once than a session may have
	/// credentials verifying, advertised up front so the client queues the rest rather
	/// than having them refused.
	#[tokio::test]
	async fn h2_streams_are_bounded_by_the_verification_budget() {
		use tokio::io::{AsyncReadExt, AsyncWriteExt};

		let mut config = Config::default();
		config.http.listen = Some("127.0.0.1:0".parse().unwrap());
		let (auth, _admissions) = crate::auth::Auth::embedded("test");
		let auth = auth.with_verifications(7);
		let cluster = cluster::Cluster::new(crate::cluster::Options::default()).unwrap();
		let dir = TempDir::new().unwrap();
		let (_, cert, _) = make_certs(&dir);
		let certificates = moq_tokio::tls::Certificates::from_pem(&std::fs::read(&cert).unwrap()).unwrap();
		let web = Web::new(auth, cluster, certificates, config).bind().unwrap();
		let http = web.addrs().http.expect("the HTTP listener is configured");
		let routes = web.routes();
		let serving = tokio::spawn(web.serve(routes));

		// Prior-knowledge HTTP/2: the preface, then an empty SETTINGS frame.
		let mut tcp = tokio::net::TcpStream::connect(http).await.unwrap();
		tcp.write_all(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n\x00\x00\x00\x04\x00\x00\x00\x00\x00")
			.await
			.unwrap();

		// The server's own SETTINGS frame comes first.
		let mut header = [0; 9];
		tcp.read_exact(&mut header).await.unwrap();
		assert_eq!(header[3], 0x4, "the server opened with something other than SETTINGS");
		let len = u32::from_be_bytes([0, header[0], header[1], header[2]]) as usize;
		let mut payload = vec![0; len];
		tcp.read_exact(&mut payload).await.unwrap();
		let streams = payload
			.as_chunks::<6>()
			.0
			.iter()
			.find(|setting| setting[..2] == [0, 0x3])
			.map(|setting| u32::from_be_bytes(setting[2..].try_into().unwrap()));
		assert_eq!(streams, Some(7));

		serving.abort();
	}

	/// An upgraded WebSocket reads no more than one qmux record at a time: a frame
	/// declaring more is refused at its header, before SETUP and without buffering it,
	/// even with no timeout to end the wait. Real sockets, so this runs on the wall clock.
	#[cfg(feature = "websocket")]
	#[tokio::test]
	async fn a_websocket_frame_past_one_record_is_refused_at_its_header() {
		use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

		let mut config = Config::default();
		config.http.listen = Some("127.0.0.1:0".parse().unwrap());
		config.ws = true;
		let auth_config = crate::auth::Config {
			public_subscribe: vec![moq_auth::Pattern::all()],
			..Default::default()
		};
		let auth = auth_config
			.init("test", &moq_tokio::tls::Connect::default(), false)
			.unwrap();
		let cluster = cluster::Cluster::new(crate::cluster::Options::default()).unwrap();
		let dir = TempDir::new().unwrap();
		let (_, cert, _) = make_certs(&dir);
		let certificates = moq_tokio::tls::Certificates::from_pem(&std::fs::read(&cert).unwrap()).unwrap();
		let web = Web::new(auth, cluster, certificates, config)
			.with_timeout(None)
			.bind()
			.unwrap();
		let http = web.addrs().http.expect("the HTTP listener is configured");
		let routes = web.routes();
		let serving = tokio::spawn(web.serve(routes));

		let tcp = tokio::net::TcpStream::connect(http).await.unwrap();
		let mut tcp = tokio::io::BufReader::new(tcp);
		tcp.write_all(
			b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\
			Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
			Sec-WebSocket-Protocol: qmux-01.moq-lite-06\r\n\r\n",
		)
		.await
		.unwrap();
		let mut status = String::new();
		tcp.read_line(&mut status).await.unwrap();
		assert!(status.starts_with("HTTP/1.1 101"), "{status}");
		loop {
			let mut header = String::new();
			tcp.read_line(&mut header).await.unwrap();
			if header == "\r\n" {
				break;
			}
		}

		// A masked binary frame declaring 1 MiB, with none of its payload sent.
		let mut frame = vec![0x82, 0x80 | 127];
		frame.extend_from_slice(&(1u64 << 20).to_be_bytes());
		frame.extend_from_slice(&[0; 4]);
		tcp.write_all(&frame).await.unwrap();

		let mut rest = Vec::new();
		kio::clock::timeout(std::time::Duration::from_secs(10), tcp.read_to_end(&mut rest))
			.await
			.expect("the socket waited to buffer a frame larger than a record")
			.ok();

		serving.abort();
	}

	/// Confirm `SetConnectionExtensions` injects the marker into request extensions
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

		// This test is about the mTLS marker; the socket handle rides the same
		// per-connection service but is independent of it.
		let mut with_peer = SetConnectionExtensions {
			inner: EchoExt,
			peer: Some(MtlsPeer(moq_tokio::tls::PeerIdentity::from_chain(Vec::new()))),
			server_name: None,
			socket: None,
			admission: None,
		};
		let mut no_peer = SetConnectionExtensions {
			inner: EchoExt,
			peer: None,
			server_name: None,
			socket: None,
			admission: None,
		};

		let req = Request::builder().body(()).unwrap();
		assert!(
			with_peer.call(req).await.unwrap(),
			"SetConnectionExtensions(Some) must surface MtlsPeer"
		);

		let req = Request::builder().body(()).unwrap();
		assert!(
			!no_peer.call(req).await.unwrap(),
			"SetConnectionExtensions(None) must NOT surface MtlsPeer"
		);
	}

	#[cfg(all(feature = "websocket", any(unix, windows)))]
	mod drain {
		use super::*;

		// Keep real socket progress from automatically advancing the paused clock.
		// Tests advance the drain deadline explicitly, after the protocol step they inspect.
		async fn socket_step<T>(future: impl Future<Output = T>) -> T {
			tokio::pin!(future);
			std::future::poll_fn(|cx| {
				let result = future.as_mut().poll(cx);
				if result.is_pending() {
					cx.waker().wake_by_ref();
				}
				result
			})
			.await
		}

		struct DrainingWeb {
			stream: tokio::net::TcpStream,
			tls: Option<tokio_rustls::TlsConnector>,
			shutdown: crate::shutdown::Observer,
			trigger: crate::shutdown::Trigger,
			running: tokio::task::JoinHandle<anyhow::Result<()>>,
			_certs: TempDir,
		}

		const DRAIN_WINDOW: std::time::Duration = std::time::Duration::from_secs(10);
		const UPGRADE_HEAD: &[u8] = b"GET /room HTTP/1.1\r\nHost: localhost\r\n";
		const UPGRADE_TAIL: &[u8] = b"Connection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: qmux-01.moq-lite-06\r\n\r\n";

		impl DrainingWeb {
			async fn new(tls: bool, embedded: bool) -> Self {
				let certs = TempDir::new().unwrap();
				let (ca, cert, key) = make_certs(&certs);
				let mut config = crate::Config::default();
				config.listen.tcp.bind = Some("127.0.0.1:0".parse().unwrap());
				config.listen.timeout = Some((std::time::Duration::ZERO).into());
				config.web.ws = true;
				config.auth.public = vec![moq_auth::Pattern::all()];
				config.drain_timeout = Some((DRAIN_WINDOW).into());
				if tls {
					config.web.https.listen = Some("127.0.0.1:0".parse().unwrap());
					config.web.https.cert = vec![cert];
					config.web.https.key = vec![key];
				} else {
					config.web.http.listen = Some("127.0.0.1:0".parse().unwrap());
				}
				let mut relay = crate::Relay::load(config).await.unwrap().with_signals(false);
				if embedded {
					let routes = relay.web().routes().route("/embedded", get(|| async { "ok" }));
					relay = relay.with_web(routes);
				}
				let health = relay.web().accept_health().unwrap();
				// The next successful accept clears this marker, giving the test a
				// socket-accept barrier without a wall-clock delay or a new test hook.
				#[cfg(unix)]
				let exhausted = libc::EMFILE;
				#[cfg(windows)]
				let exhausted = 10024; // Winsock WSAEMFILE.
				let _ = health.failed(&std::io::Error::from_raw_os_error(exhausted));
				assert!(health.stalled().is_some(), "the accept barrier was not armed");
				let addr = relay.web_addrs();
				let addr = if tls { addr.https.unwrap() } else { addr.http.unwrap() };
				let shutdown = relay.shutdown().clone();
				let trigger = relay.shutdown_trigger().clone();
				let running = tokio::spawn(relay.run());
				let stream = socket_step(tokio::net::TcpStream::connect(addr)).await.unwrap();
				while health.stalled().is_some() {
					tokio::task::yield_now().await;
				}
				let tls = if tls {
					use rustls::pki_types::{CertificateDer, pem::PemObject};
					let mut roots = rustls::RootCertStore::empty();
					for cert in CertificateDer::pem_slice_iter(&std::fs::read(ca).unwrap()) {
						roots.add(cert.unwrap()).unwrap();
					}
					let mut config = rustls::ClientConfig::builder()
						.with_root_certificates(roots)
						.with_no_client_auth();
					config.alpn_protocols = vec![b"http/1.1".to_vec()];
					Some(tokio_rustls::TlsConnector::from(Arc::new(config)))
				} else {
					None
				};
				Self {
					stream,
					tls,
					shutdown,
					trigger,
					running,
					_certs: certs,
				}
			}

			fn holds_drain(&self) {
				use futures::FutureExt;
				assert!(
					self.shutdown.activity().close().now_or_never().is_none(),
					"a TCP-accepted web connection was absent from the drain before its upgrade"
				);
			}
		}

		pub(super) async fn upgrade_while_draining<I>(mut stream: I, head_sent: bool) -> moq_net::Session
		where
			I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
		{
			use tokio::io::{AsyncReadExt, AsyncWriteExt};
			if !head_sent {
				stream.write_all(UPGRADE_HEAD).await.unwrap();
			}
			stream.write_all(UPGRADE_TAIL).await.unwrap();
			// Read only the HTTP headers: QMux may start writing immediately after
			// the upgrade, and those bytes belong to the WebSocket decoder.
			let mut response = Vec::new();
			while !response.ends_with(b"\r\n\r\n") {
				response.push(stream.read_u8().await.unwrap());
			}
			assert!(
				response.starts_with(b"HTTP/1.1 101"),
				"{}",
				String::from_utf8_lossy(&response)
			);
			let socket = qmux::ws::tokio_tungstenite::WebSocketStream::from_raw_socket(
				stream,
				qmux::ws::tungstenite::protocol::Role::Client,
				None,
			)
			.await;
			let transport = qmux::ws::Upgraded::new(socket, "qmux-01.moq-lite-06")
				.connect()
				.unwrap();
			let origin = moq_tokio::origin::spawn();
			let (session, driver) = moq_net::Client::new()
				.with_versions("moq-lite-06".parse::<moq_net::Version>().unwrap().into())
				.with_publisher(origin.consume())
				.connect(moq_net::time::Instant::now(), transport)
				.await
				.unwrap();
			tokio::spawn(moq_net::time::run(driver));
			session
		}

		#[tokio::test(start_paused = true)]
		async fn accepted_web_connection_holds_drain_until_upgrade() {
			use tokio::io::AsyncWriteExt;
			for tls in [false, true] {
				for embedded in [false, true] {
					let mut web = DrainingWeb::new(tls, embedded).await;
					if !tls {
						socket_step(web.stream.write_all(UPGRADE_HEAD)).await.unwrap();
					}
					web.holds_drain();
					web.trigger.start();
					let session = match web.tls {
						Some(connector) => {
							let stream = socket_step(connector.connect("localhost".try_into().unwrap(), web.stream))
								.await
								.unwrap();
							socket_step(upgrade_while_draining(stream, false)).await
						}
						None => socket_step(upgrade_while_draining(web.stream, true)).await,
					};
					let draining = session.draining();
					let goaway = socket_step(moq_net::kio::wait(|waiter| draining.poll(waiter)))
						.await
						.unwrap();
					assert_eq!(goaway.uri(), "");
					session.abort(moq_net::Error::Cancel);
					socket_step(web.running).await.unwrap().unwrap();
				}
			}
		}

		#[tokio::test(start_paused = true)]
		async fn accepted_web_connection_reaches_drain_deadline() {
			for tls in [false, true] {
				let web = DrainingWeb::new(tls, true).await;
				web.holds_drain();
				web.trigger.start();
				tokio::time::advance(DRAIN_WINDOW).await;
				web.running.await.unwrap().unwrap();
			}
		}

		#[tokio::test(start_paused = true)]
		async fn plain_first_web_request_releases_drain() {
			use tokio::io::AsyncWriteExt;
			let mut web = DrainingWeb::new(false, true).await;
			socket_step(web.stream.write_all(b"GET /embedded HTTP/1.1\r\nHost: localhost\r\n"))
				.await
				.unwrap();
			web.holds_drain();
			web.trigger.start();
			socket_step(web.stream.write_all(b"\r\n")).await.unwrap();
			socket_step(web.running).await.unwrap().unwrap();
		}

		#[tokio::test(start_paused = true)]
		async fn h2_alpn_releases_admission_before_headers() {
			use axum_server::AddrListener;
			use futures::FutureExt;
			use rustls::pki_types::{CertificateDer, pem::PemObject};
			let dir = TempDir::new().unwrap();
			let (ca, cert, key) = make_certs(&dir);
			let tls = build_https_config(&[cert], &[key], &[]).unwrap();
			let mut roots = rustls::RootCertStore::empty();
			for cert in CertificateDer::pem_slice_iter(&std::fs::read(ca).unwrap()) {
				roots.add(cert.unwrap()).unwrap();
			}
			let mut client = rustls::ClientConfig::builder()
				.with_root_certificates(roots)
				.with_no_client_auth();
			client.alpn_protocols = vec![b"h2".to_vec()];
			let connector = tokio_rustls::TlsConnector::from(Arc::new(client));
			let activity = moq_net::server::Activity::default();
			let bound = moq_tokio::bind::tcp("127.0.0.1:0".parse().unwrap()).unwrap();
			let addr = bound.local_addr().unwrap();
			let listener =
				crate::listener::Listener::new(bound, moq_tokio::accept::Health::new("h2"), Some(activity.clone()))
					.unwrap();
			let ((accepted, _), connected) =
				socket_step(async { tokio::try_join!(listener.accept_stream(), tokio::net::TcpStream::connect(addr)) })
					.await
					.unwrap();
			assert!(activity.close().now_or_never().is_none(), "TLS was not counted");
			let acceptor = WebAcceptor::tls(RustlsConfig::from_config(tls), false);
			let ((stream, _service), _client) = socket_step(async {
				tokio::try_join!(
					acceptor.accept(accepted, ()),
					connector.connect("localhost".try_into().unwrap(), connected)
				)
			})
			.await
			.unwrap();
			assert!(stream.is_http2());
			assert!(
				activity.close().now_or_never().is_some(),
				"h2 awaited request headers before releasing its admission"
			);
		}

		async fn plain_request(stream: &mut tokio::net::TcpStream) {
			use tokio::io::{AsyncReadExt, AsyncWriteExt};
			stream
				.write_all(b"GET /embedded HTTP/1.1\r\nHost: localhost\r\n\r\n")
				.await
				.unwrap();
			let mut response = Vec::new();
			while !response.ends_with(b"\r\n\r\n") {
				response.push(stream.read_u8().await.unwrap());
			}
			assert!(
				response.starts_with(b"HTTP/1.1 200"),
				"{}",
				String::from_utf8_lossy(&response)
			);
			let mut body = [0; 2];
			stream.read_exact(&mut body).await.unwrap();
			assert_eq!(&body, b"ok");
		}

		#[tokio::test(start_paused = true)]
		async fn later_keep_alive_upgrade_enters_from_the_handler() {
			let mut web = DrainingWeb::new(false, true).await;
			let pending = web.shutdown.activity().enter().unwrap();
			socket_step(plain_request(&mut web.stream)).await;
			web.trigger.start();
			let session = socket_step(upgrade_while_draining(web.stream, false)).await;
			drop(pending);
			let draining = session.draining();
			let goaway = socket_step(moq_net::kio::wait(|waiter| draining.poll(waiter))).await;
			assert_eq!(goaway.unwrap().uri(), "");
			session.abort(moq_net::Error::Cancel);
			socket_step(web.running).await.unwrap().unwrap();
		}

		async fn refused_upgrade(stream: tokio::net::TcpStream) {
			use qmux::ws::tungstenite::client::IntoClientRequest;
			let mut request = format!("ws://{}/room", stream.peer_addr().unwrap())
				.into_client_request()
				.unwrap();
			request
				.headers_mut()
				.insert("sec-websocket-protocol", "qmux-01.moq-lite-06".parse().unwrap());
			let result = qmux::ws::tokio_tungstenite::client_async(request, stream).await;
			let Err(qmux::ws::tungstenite::Error::Http(response)) = result else {
				panic!("an upgrade after drain admission closed was not refused");
			};
			assert_eq!(response.status(), http::StatusCode::SERVICE_UNAVAILABLE);
		}

		#[tokio::test(start_paused = true)]
		async fn committed_drain_refuses_upgrades_but_serves_plain_http() {
			let mut web = DrainingWeb::new(false, true).await;
			let addr = web.stream.peer_addr().unwrap();
			socket_step(plain_request(&mut web.stream)).await;
			// Keep the listener running while admission is closed, exposing the
			// same boundary a connection can reach just before Relay::run stops it.
			socket_step(web.shutdown.activity().close()).await;
			socket_step(refused_upgrade(web.stream)).await;
			let mut fresh = socket_step(tokio::net::TcpStream::connect(addr)).await.unwrap();
			socket_step(plain_request(&mut fresh)).await;
			socket_step(refused_upgrade(fresh)).await;
			web.trigger.start();
			socket_step(web.running).await.unwrap().unwrap();
		}
	}
}
