use futures::{SinkExt, StreamExt};
use qmux::tungstenite;
use std::{
	future::Future,
	pin::Pin,
	sync::{Arc, atomic::Ordering},
};

use axum::{
	extract::{
		Extension, Path, Query, State, WebSocketUpgrade,
		rejection::{PathRejection, QueryRejection},
		ws::rejection::WebSocketUpgradeRejection,
	},
	http::StatusCode,
	response::Response,
};
use moq_net::{OriginProducer, StatsHandle, Tier};

use crate::{AuthParams, AuthToken, WebState, web::AuthQuery, web::MtlsPeer, web::landing_response};

pub(crate) async fn serve_ws(
	ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
	path: Result<Path<String>, PathRejection>,
	query: Result<Query<AuthQuery>, QueryRejection>,
	mtls: Option<Extension<MtlsPeer>>,
	State(state): State<Arc<WebState>>,
) -> axum::response::Result<Response> {
	// If this isn't a WebSocket upgrade (e.g. a plain browser visit), serve
	// the informational landing page instead of an error response.
	let (Ok(ws), Ok(Path(path)), Ok(Query(query))) = (ws, path, query) else {
		return Ok(landing_response());
	};

	// Advertise the full qmux × moq-net subprotocol matrix, with bare qmux
	// fallbacks last. axum picks the first entry that the client also offered,
	// so a modern client lands on `qmux-00.moq-lite-04`; old clients still
	// match `webtransport` or `qmux-00.moql` and negotiate via SETUP.
	let ws = ws.protocols(supported_subprotocols());

	let params = AuthParams { path, jwt: query.jwt };
	let token = if mtls.is_some() {
		AuthToken::unrestricted(moq_net::Path::new(&params.path).to_owned())
	} else {
		state.auth.verify(&params).await?
	};
	let publish = state.cluster.publisher(&token);
	let subscribe = state.cluster.subscriber(&token);
	// mTLS sessions record on the internal tier; everything else on external.
	let tier = match token.internal {
		true => Tier::Internal,
		false => Tier::External,
	};
	let stats = state.cluster.stats.tier(tier);

	if publish.is_none() && subscribe.is_none() {
		// Bad token, we can't publish or subscribe.
		return Err(StatusCode::UNAUTHORIZED.into());
	}

	Ok(ws.on_upgrade(async move |socket| {
		let id = state.conn_id.fetch_add(1, Ordering::Relaxed);

		// Capture the negotiated subprotocol before we erase the WebSocket type
		// in the Stream/Sink adapters; qmux needs it to derive the moq version.
		let alpn = socket.protocol().and_then(|h| h.to_str().ok()).map(str::to_owned);

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
		let _ = handle_socket(id, socket, alpn, publish, subscribe, stats).await;
	}))
}

#[tracing::instrument("ws", err, skip_all, fields(id = _id))]
async fn handle_socket<T>(
	_id: u64,
	socket: T,
	alpn: Option<String>,
	publish: Option<OriginProducer>,
	subscribe: Option<OriginProducer>,
	stats: StatsHandle,
) -> anyhow::Result<()>
where
	T: futures::Stream<Item = Result<tungstenite::Message, tungstenite::Error>>
		+ futures::Sink<tungstenite::Message, Error = tungstenite::Error>
		+ Send
		+ Unpin
		+ 'static,
{
	// Wrap the WebSocket in a WebTransport compatibility layer. We have to
	// forward the negotiated subprotocol explicitly; axum performed the
	// upgrade, so qmux can't sniff it from the handshake.
	let upgraded = qmux::ws::Upgraded::new(socket);
	let upgraded = match alpn.as_deref() {
		Some(alpn) => upgraded.with_alpn(alpn),
		None => upgraded,
	};
	let ws = upgraded.accept();
	// Only set the side the token actually grants. moq-net defaults the
	// unset side to a fresh no-op origin, which is fine for a
	// publish-only or subscribe-only token.
	let mut server = moq_net::Server::new().with_stats(stats);
	if let Some(subscribe) = subscribe {
		server = server.with_publisher(subscribe);
	}
	if let Some(publish) = publish {
		server = server.with_consumer(publish);
	}
	let session = server.accept(ws).await?;
	session.closed().await.map_err(Into::into)
}

/// QMux wire-format versions that can ride under a `{prefix}.{alpn}` pair.
/// Newest first so axum's exact-string match picks the freshest one.
const QMUX_VERSIONS: &[qmux::Version] = &[qmux::Version::QMux01, qmux::Version::QMux00];

/// Subprotocols to advertise on the WebSocket upgrade.
///
/// Generates the cross product of [`QMUX_VERSIONS`] × `moq_net::ALPNS`, with
/// the bare qmux fallbacks (`qmux-01`, `qmux-00`, `webtransport`) appended
/// last so versioned subprotocols always win the exact-string match axum
/// performs. Without the versioned entries, axum picks bare `webtransport`,
/// qmux can't resolve a moq version from it, and the relay silently
/// downgrades clients to Lite02 via SETUP-based negotiation.
fn supported_subprotocols() -> Vec<String> {
	let mut out = Vec::with_capacity(QMUX_VERSIONS.len() * moq_net::ALPNS.len() + qmux::ALPNS.len());
	for &version in QMUX_VERSIONS {
		for &alpn in moq_net::ALPNS {
			out.push(format!("{}{alpn}", version.prefix()));
		}
	}
	for &alpn in qmux::ALPNS {
		out.push(alpn.to_string());
	}
	out
}

// https://github.com/tokio-rs/axum/discussions/848#discussioncomment-11443587

#[allow(clippy::result_large_err)]
fn axum_to_tungstenite(
	message: Result<axum::extract::ws::Message, axum::Error>,
) -> Result<tungstenite::Message, tungstenite::Error> {
	match message {
		Ok(msg) => Ok(match msg {
			axum::extract::ws::Message::Text(text) => tungstenite::Message::Text(text.to_string().into()),
			axum::extract::ws::Message::Binary(bin) => tungstenite::Message::Binary(Vec::from(bin).into()),
			axum::extract::ws::Message::Ping(ping) => tungstenite::Message::Ping(Vec::from(ping).into()),
			axum::extract::ws::Message::Pong(pong) => tungstenite::Message::Pong(Vec::from(pong).into()),
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
			tungstenite::Message::Text(text) => axum::extract::ws::Message::Text(text.to_string().into()),
			tungstenite::Message::Binary(bin) => axum::extract::ws::Message::Binary(Vec::from(bin).into()),
			tungstenite::Message::Ping(ping) => axum::extract::ws::Message::Ping(Vec::from(ping).into()),
			tungstenite::Message::Pong(pong) => axum::extract::ws::Message::Pong(Vec::from(pong).into()),
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

#[cfg(test)]
mod tests {
	use super::*;
	use axum::{Router, extract::WebSocketUpgrade, routing::any};
	use std::sync::Mutex;
	use tokio::sync::oneshot;
	// Brings `qmux::Session::protocol` and `::closed` into scope.
	use web_transport_trait::Session as _;

	/// The newest moq ALPN both sides agree on. Derived from the same source
	/// of truth that `supported_subprotocols` and `qmux::Client::with_protocols`
	/// consume, so adding a new ALPN doesn't break these tests independently
	/// of the production logic.
	fn newest_moq_alpn() -> &'static str {
		moq_net::ALPNS.first().copied().expect("moq_net::ALPNS is empty")
	}

	fn preferred_qmux_prefix() -> &'static str {
		QMUX_VERSIONS.first().expect("QMUX_VERSIONS is empty").prefix()
	}

	#[test]
	fn supported_subprotocols_lists_full_matrix() {
		let list = supported_subprotocols();

		// Newest moq ALPN under the preferred prefix must come first so axum
		// picks it whenever the client offers it.
		let expected_first = format!("{}{}", preferred_qmux_prefix(), newest_moq_alpn());
		assert_eq!(list.first().map(String::as_str), Some(expected_first.as_str()));

		// Every moq ALPN must appear under every qmux version's prefix.
		for &version in QMUX_VERSIONS {
			for &alpn in moq_net::ALPNS {
				let entry = format!("{}{alpn}", version.prefix());
				assert!(list.contains(&entry), "missing {entry}");
			}
		}

		// Bare qmux fallbacks must come after every versioned entry so they
		// only win when the client offers nothing better. The buggy
		// `["webtransport"]` advertise list would put a bare entry first and
		// silently downgrade modern clients to Lite02.
		let last_versioned_idx = list
			.iter()
			.rposition(|s| s.contains('.'))
			.expect("no versioned entries");
		for &bare in qmux::ALPNS {
			let bare_idx = list
				.iter()
				.position(|s| s == bare)
				.unwrap_or_else(|| panic!("missing bare fallback {bare}"));
			assert!(
				bare_idx > last_versioned_idx,
				"bare {bare} must come after every versioned entry, got {list:?}",
			);
		}
	}

	/// End-to-end regression: connect a qmux client offering the full moq
	/// ALPN list to an axum router that mirrors `serve_ws`'s subprotocol
	/// wiring. Both client and server must observe the newest moq ALPN
	/// (`moq_net::ALPNS[0]`) on the resulting qmux session. A bug in
	/// `supported_subprotocols` or in the `Bare::with_alpn` plumbing
	/// collapses this to `None` / bare `webtransport`, and moq-net then
	/// downgrades to Lite02 via SETUP.
	#[tokio::test]
	async fn axum_ws_negotiates_newest_moq_alpn() {
		let (server_alpn_tx, server_alpn_rx) = oneshot::channel::<Option<String>>();
		let server_alpn_tx = Arc::new(Mutex::new(Some(server_alpn_tx)));

		let route = {
			let server_alpn_tx = server_alpn_tx.clone();
			any(move |ws: WebSocketUpgrade| {
				let server_alpn_tx = server_alpn_tx.clone();
				async move {
					let ws = ws.protocols(supported_subprotocols());
					ws.on_upgrade(move |socket| async move {
						let alpn = socket.protocol().and_then(|h| h.to_str().ok()).map(str::to_owned);
						let socket = socket
							.map(axum_to_tungstenite)
							.sink_map_err(|_| tungstenite::Error::ConnectionClosed)
							.with(tungstenite_to_axum);

						let upgraded = qmux::ws::Upgraded::new(socket);
						let upgraded = match alpn.as_deref() {
							Some(alpn) => upgraded.with_alpn(alpn),
							None => upgraded,
						};
						let session = upgraded.accept();
						if let Some(tx) = server_alpn_tx.lock().unwrap().take() {
							let _ = tx.send(session.protocol().map(str::to_owned));
						}
						// Hold the session open so the client side stays alive
						// long enough to observe the negotiated subprotocol.
						let _ = session.closed().await;
					})
				}
			})
		};

		let app = Router::new().route("/", route);

		let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
			.await
			.expect("bind listener");
		let addr = listener.local_addr().expect("local addr");
		let server = tokio::spawn(async move {
			axum::serve(listener, app).await.expect("axum serve");
		});

		let session = qmux::Client::new()
			.with_protocols(moq_net::ALPNS.iter().map(|&a| (a, &[] as &[qmux::Version])))
			.connect(&format!("ws://{addr}/"))
			.await
			.expect("qmux client connect");

		assert_eq!(
			session.protocol(),
			Some(newest_moq_alpn()),
			"client side should see the newest moq ALPN, got {:?}",
			session.protocol(),
		);

		let server_alpn = tokio::time::timeout(std::time::Duration::from_secs(5), server_alpn_rx)
			.await
			.expect("server alpn channel timed out")
			.expect("server alpn channel dropped");
		assert_eq!(
			server_alpn.as_deref(),
			Some(newest_moq_alpn()),
			"server side should see the newest moq ALPN after Bare::with_alpn",
		);

		drop(session);
		server.abort();
	}
}
