use futures::{SinkExt, StreamExt};
use std::{
	net,
	path::PathBuf,
	pin::Pin,
	sync::{
		atomic::{AtomicU64, Ordering},
		Arc,
	},
	task::{ready, Context, Poll},
};
use web_transport_ws::tungstenite;

use axum::{
	body::Body,
	extract::{Path, Query, State, WebSocketUpgrade},
	http::{Method, StatusCode},
	response::{IntoResponse, Response},
	routing::{any, get},
	Router,
};
use bytes::Bytes;
use clap::Parser;
use moq_lite::{OriginConsumer, OriginProducer};
use serde::{Deserialize, Serialize};
use std::future::Future;
use tower_http::cors::{Any, CorsLayer};

use crate::{Auth, Cluster};

#[derive(Debug, Deserialize)]
struct Params {
	jwt: Option<String>,
}

#[derive(Parser, Clone, Debug, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct WebConfig {
	#[command(flatten)]
	#[serde(default)]
	pub http: HttpConfig,

	#[command(flatten)]
	#[serde(default)]
	pub https: HttpsConfig,

	// If true (default), expose a WebTransport compatible WebSocket polyfill.
	#[arg(long = "web-ws", env = "MOQ_WEB_WS", default_value = "true")]
	#[serde(default = "default_true")]
	pub ws: bool,
}

#[derive(clap::Args, Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HttpConfig {
	#[arg(long = "web-http-listen", id = "http-listen", env = "MOQ_WEB_HTTP_LISTEN")]
	pub listen: Option<net::SocketAddr>,
}

#[derive(clap::Args, Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HttpsConfig {
	#[arg(long = "web-https-listen", id = "web-https-listen", env = "MOQ_WEB_HTTPS_LISTEN", requires_all = ["web-https-cert", "web-https-key"])]
	pub listen: Option<net::SocketAddr>,

	/// Load the given certificate from disk.
	#[arg(long = "web-https-cert", id = "web-https-cert", env = "MOQ_WEB_HTTPS_CERT")]
	pub cert: Option<PathBuf>,

	/// Load the given key from disk.
	#[arg(long = "web-https-key", id = "web-https-key", env = "MOQ_WEB_HTTPS_KEY")]
	pub key: Option<PathBuf>,
}

pub struct WebState {
	pub auth: Auth,
	pub cluster: Cluster,
	pub tls_info: Arc<std::sync::RwLock<moq_native::TlsInfo>>,
	pub conn_id: AtomicU64,
}

// Run a HTTP server using Axum
pub struct Web {
	state: WebState,
	config: WebConfig,
}

impl Web {
	pub fn new(state: WebState, config: WebConfig) -> Self {
		Self { state, config }
	}

	pub async fn run(self) -> anyhow::Result<()> {
		let app = Router::new()
			.route("/certificate.sha256", get(serve_fingerprint))
			.route("/announced", get(serve_announced))
			.route("/announced/{*prefix}", get(serve_announced))
			.route("/fetch/{*path}", get(serve_fetch));

		// If WebSocket is enabled, add the WebSocket route.
		let app = match self.config.ws {
			true => app.route("/{*path}", any(serve_ws)),
			false => app,
		}
		.layer(CorsLayer::new().allow_origin(Any).allow_methods([Method::GET]))
		.with_state(Arc::new(self.state))
		.into_make_service();

		let http = if let Some(listen) = self.config.http.listen {
			let server = hyper_serve::bind(listen);
			Some(server.serve(app.clone()))
		} else {
			None
		};

		let https = if let Some(listen) = self.config.https.listen {
			let cert = self.config.https.cert.expect("missing https.cert");
			let key = self.config.https.key.expect("missing https.key");
			let config = hyper_serve::tls_rustls::RustlsConfig::from_pem_file(cert.clone(), key.clone()).await?;

			#[cfg(unix)]
			tokio::spawn(reload_certs(config.clone(), cert, key));

			let server = hyper_serve::bind_rustls(listen, config);
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

#[cfg(unix)]
async fn reload_certs(config: hyper_serve::tls_rustls::RustlsConfig, cert: PathBuf, key: PathBuf) {
	use tokio::signal::unix::{signal, SignalKind};

	// Dunno why we wouldn't be allowed to listen for signals, but just in case.
	let mut listener = signal(SignalKind::user_defined1()).expect("failed to listen for signals");

	while listener.recv().await.is_some() {
		tracing::info!("reloading web certificate");

		if let Err(err) = config.reload_from_pem_file(cert.clone(), key.clone()).await {
			tracing::warn!(%err, "failed to reload web certificate");
		}
	}
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

async fn serve_ws(
	ws: WebSocketUpgrade,
	Path(path): Path<String>,
	Query(params): Query<Params>,
	State(state): State<Arc<WebState>>,
) -> axum::response::Result<Response> {
	let ws = ws.protocols(["webtransport"]);

	let token = state.auth.verify(&path, params.jwt.as_deref())?;
	let publish = state.cluster.publisher(&token);
	let subscribe = state.cluster.subscriber(&token);

	if publish.is_none() && subscribe.is_none() {
		// Bad token, we can't publish or subscribe.
		return Err(StatusCode::UNAUTHORIZED.into());
	}

	Ok(ws.on_upgrade(async move |socket| {
		let id = state.conn_id.fetch_add(1, Ordering::Relaxed);

		// Unfortunately, we need to convert from Axum to Tungstenite.
		// Axum uses Tungstenite internally, but it's not exposed to avoid semvar issues.
		let socket = socket
			.map(axum_to_tungstenite)
			// TODO Figure out how to avoid swallowing errors.
			.sink_map_err(|err| {
				tracing::warn!(%err, "WebSocket error");
				tungstenite::Error::ConnectionClosed
			})
			.with(tungstenite_to_axum);
		let _ = handle_socket(id, socket, publish, subscribe).await;
	}))
}

#[tracing::instrument("ws", err, skip_all, fields(id = _id))]
async fn handle_socket<T>(
	_id: u64,
	socket: T,
	publish: Option<OriginProducer>,
	subscribe: Option<OriginConsumer>,
) -> anyhow::Result<()>
where
	T: futures::Stream<Item = Result<tungstenite::Message, tungstenite::Error>>
		+ futures::Sink<tungstenite::Message, Error = tungstenite::Error>
		+ Send
		+ Unpin
		+ 'static,
{
	// Wrap the WebSocket in a WebTransport compatibility layer.
	let ws = web_transport_ws::Session::new(socket, true);
	let session = moq_lite::Session::accept(ws, subscribe, publish).await?;
	session.closed().await.map_err(Into::into)
}

/// Serve the announced broadcasts for a given prefix.
async fn serve_announced(
	path: Option<Path<String>>,
	Query(params): Query<Params>,
	State(state): State<Arc<WebState>>,
) -> axum::response::Result<String> {
	let prefix = match path {
		Some(Path(prefix)) => prefix,
		None => String::new(),
	};

	let token = state.auth.verify(&prefix, params.jwt.as_deref())?;
	let mut origin = match state.cluster.subscriber(&token) {
		Some(origin) => origin,
		None => return Err(StatusCode::UNAUTHORIZED.into()),
	};

	let mut broadcasts = Vec::new();

	while let Some((suffix, active)) = origin.try_announced() {
		if active.is_some() {
			broadcasts.push(suffix);
		}
	}

	Ok(broadcasts.iter().map(|p| p.to_string()).collect::<Vec<_>>().join("\n"))
}

/// Serve the latest group for a given track
async fn serve_fetch(
	Path(path): Path<String>,
	Query(params): Query<Params>,
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
	let token = state.auth.verify(&broadcast, params.jwt.as_deref())?;

	let origin = match state.cluster.subscriber(&token) {
		Some(origin) => origin,
		None => return Err(StatusCode::UNAUTHORIZED.into()),
	};

	tracing::info!(%broadcast, %track, "fetching track");

	let track = moq_lite::Track {
		name: track,
		priority: 0,
	};

	// NOTE: The auth token is already scoped to the broadcast.
	let broadcast = origin.consume_broadcast("").ok_or(StatusCode::NOT_FOUND)?;
	let mut track = broadcast.subscribe_track(&track);

	let group = match track.next_group().await {
		Ok(Some(group)) => group,
		Ok(None) => return Err(StatusCode::NOT_FOUND.into()),
		Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR.into()),
	};

	Ok(ServeGroup::new(group))
}

struct ServeGroup {
	group: moq_lite::GroupConsumer,
	frame: Option<moq_lite::FrameConsumer>,
}

impl ServeGroup {
	fn new(group: moq_lite::GroupConsumer) -> Self {
		Self { group, frame: None }
	}

	async fn next(&mut self) -> moq_lite::Result<Option<Bytes>> {
		loop {
			if let Some(frame) = self.frame.as_mut() {
				let data = frame.read_all().await?;
				self.frame.take();
				return Ok(Some(data));
			}

			self.frame = self.group.next_frame().await?;
			if self.frame.is_none() {
				return Ok(None);
			}
		}
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

// https://github.com/tokio-rs/axum/discussions/848#discussioncomment-11443587

#[allow(clippy::result_large_err)]
fn axum_to_tungstenite(
	message: Result<axum::extract::ws::Message, axum::Error>,
) -> Result<tungstenite::Message, tungstenite::Error> {
	match message {
		Ok(msg) => Ok(match msg {
			axum::extract::ws::Message::Text(text) => tungstenite::Message::Text(text.to_string()),
			axum::extract::ws::Message::Binary(bin) => tungstenite::Message::Binary(bin.into()),
			axum::extract::ws::Message::Ping(ping) => tungstenite::Message::Ping(ping.into()),
			axum::extract::ws::Message::Pong(pong) => tungstenite::Message::Pong(pong.into()),
			axum::extract::ws::Message::Close(close) => {
				tungstenite::Message::Close(close.map(|c| tungstenite::protocol::CloseFrame {
					code: c.code.into(),
					reason: c.reason.to_string().into(),
				}))
			}
		}),
		Err(_err) => Err(tungstenite::Error::ConnectionClosed),
	}
}

#[allow(clippy::result_large_err)]
fn tungstenite_to_axum(
	message: tungstenite::Message,
) -> Pin<Box<dyn Future<Output = Result<axum::extract::ws::Message, tungstenite::Error>> + Send + Sync>> {
	Box::pin(async move {
		Ok(match message {
			tungstenite::Message::Text(text) => axum::extract::ws::Message::Text(text.into()),
			tungstenite::Message::Binary(bin) => axum::extract::ws::Message::Binary(bin.into()),
			tungstenite::Message::Ping(ping) => axum::extract::ws::Message::Ping(ping.into()),
			tungstenite::Message::Pong(pong) => axum::extract::ws::Message::Pong(pong.into()),
			tungstenite::Message::Frame(_frame) => unreachable!(),
			tungstenite::Message::Close(close) => {
				axum::extract::ws::Message::Close(close.map(|c| axum::extract::ws::CloseFrame {
					code: c.code.into(),
					reason: c.reason.to_string().into(),
				}))
			}
		})
	})
}

fn default_true() -> bool {
	true
}
