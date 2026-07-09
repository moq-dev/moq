use futures::{SinkExt, StreamExt};
use qmux::tungstenite;
use std::{
	future::Future,
	pin::Pin,
	sync::{Arc, atomic::Ordering},
};

use axum::{
	extract::{
		Extension, Path, Query, State, WebSocketUpgrade, rejection::QueryRejection,
		ws::rejection::WebSocketUpgradeRejection,
	},
	http::StatusCode,
	response::Response,
};
use moq_net::StatsHandle;
use moq_net::origin;

use crate::{AuthParams, web::AuthQuery, web::MtlsPeer, web::WebState, web::landing_response};

pub(crate) async fn serve_ws(
	ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
	path: Option<Path<String>>,
	query: Result<Query<AuthQuery>, QueryRejection>,
	mtls: Option<Extension<MtlsPeer>>,
	State(state): State<Arc<WebState>>,
) -> axum::response::Result<Response> {
	// If this isn't a WebSocket upgrade (e.g. a plain browser visit), serve
	// the informational landing page instead of an error response.
	let (Ok(ws), Ok(Query(query))) = (ws, query) else {
		return Ok(landing_response());
	};

	// The `/{*path}` route captures the path; the bare `/` route captures none,
	// which is the empty (root) auth scope. Mirrors `serve_announced`.
	let path = path.map_or_else(String::new, |Path(path)| path);

	// Advertise the full qmux × moq-net subprotocol matrix, with bare qmux
	// fallbacks last. axum picks the first entry that the client also offered,
	// so a modern client lands on `qmux-00.moq-lite-04`; old clients still
	// match `webtransport` or `qmux-00.moql` and negotiate via SETUP.
	let ws = ws.protocols(supported_subprotocols());

	let params = AuthParams {
		path,
		jwt: query.jwt,
		transport: Some("websocket".to_string()),
	};
	let token = if mtls.is_some() {
		// mTLS peers: the API returns the canonical root and the billing tier.
		state
			.auth
			.verify_mtls(&params.path, params.transport.as_deref())
			.await?
	} else {
		state.auth.verify(&params).await?
	};
	let publish = state.cluster.publisher(&token);
	let subscribe = state.cluster.subscriber(&token);
	let stats = state.cluster.stats.tier(token.tier.clone());

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
	publish: Option<origin::Producer>,
	subscribe: Option<origin::Producer>,
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
		server = server.with_publisher(&subscribe);
	}
	if let Some(publish) = publish {
		server = server.with_subscriber(publish);
	}
	let session = server.accept(ws).await?;
	session.closed().await.map_err(Into::into)
}

/// QMux wire-format versions that can ride under a `{prefix}.{alpn}` pair.
/// Newest first so axum's exact-string match picks the freshest one.
const QMUX_VERSIONS: &[qmux::Version] = &[qmux::Version::QMux01, qmux::Version::QMux00];

/// moq-transport-18 and -19 require qmux-01, so we never pair them with qmux-00.
/// Mirrors `js/net`'s `connect.ts` and moq-native's `websocket_subprotocols`.
const QMUX01_ONLY_ALPNS: &[&str] = &["moqt-18", "moqt-19"];

/// Subprotocols to advertise on the WebSocket upgrade.
///
/// Generates the cross product of [`QMUX_VERSIONS`] × `moq_net::ALPNS`, with
/// the bare qmux fallbacks (`qmux-01`, `qmux-00`, `webtransport`) appended
/// last so versioned subprotocols always win the exact-string match axum
/// performs. Without the versioned entries, axum picks bare `webtransport`,
/// qmux can't resolve a moq version from it, and the relay silently
/// downgrades clients to Lite02 via SETUP-based negotiation.
///
/// `qmux-00.moqt-1{8,9}` is excluded: moq-transport-18 and -19 require qmux-01, so
/// those pairs are illegal.
fn supported_subprotocols() -> Vec<String> {
	let mut out = Vec::with_capacity(QMUX_VERSIONS.len() * moq_net::ALPNS.len() + qmux::ALPNS.len());
	for &version in QMUX_VERSIONS {
		for &alpn in moq_net::ALPNS {
			if version == qmux::Version::QMux00 && QMUX01_ONLY_ALPNS.contains(&alpn) {
				continue;
			}
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
	use std::time::Duration;
	use tokio::sync::mpsc;
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
		// Guard the literals: they must stay the IETF draft-18/19 ALPNs
		// (wire 0xff000012 / 0xff000013).
		assert_eq!(
			QMUX01_ONLY_ALPNS
				.iter()
				.map(|&a| moq_net::Version::from_alpn(a).map(|v| v.code()))
				.collect::<Vec<_>>(),
			vec![Some(0xff000012), Some(0xff000013)]
		);

		let list = supported_subprotocols();

		// Newest moq ALPN under the preferred prefix must come first so axum
		// picks it whenever the client offers it.
		let expected_first = format!("{}{}", preferred_qmux_prefix(), newest_moq_alpn());
		assert_eq!(list.first().map(String::as_str), Some(expected_first.as_str()));

		// Every moq ALPN must appear under every qmux wire version, except the
		// illegal `qmux-00.moqt-1{8,9}` pairs (moq-transport-18/19 need qmux-01).
		for &version in QMUX_VERSIONS {
			for &alpn in moq_net::ALPNS {
				let entry = format!("{}{alpn}", version.prefix());
				if version == qmux::Version::QMux00 && QMUX01_ONLY_ALPNS.contains(&alpn) {
					assert!(!list.contains(&entry), "illegal pair {entry} must not be advertised");
					continue;
				}
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

	/// What a single accepted WebSocket connection negotiated, as seen by the server.
	#[derive(Debug)]
	struct Observed {
		/// Raw `Sec-WebSocket-Protocol` axum selected (keeps the `qmux-XX.` prefix).
		wire: Option<String>,
		/// The moq app ALPN qmux derived from it (prefix stripped, `None` for bare).
		app: Option<String>,
	}

	/// Spawn an axum server that mirrors `serve_ws`'s subprotocol wiring.
	///
	/// Returns its address and a receiver yielding one [`Observed`] per accepted
	/// connection, in acceptance order.
	async fn spawn_test_server() -> (std::net::SocketAddr, mpsc::UnboundedReceiver<Observed>) {
		let (tx, rx) = mpsc::unbounded_channel::<Observed>();

		let route = any(move |ws: WebSocketUpgrade| {
			let tx = tx.clone();
			async move {
				let ws = ws.protocols(supported_subprotocols());
				ws.on_upgrade(move |socket| async move {
					let wire = socket.protocol().and_then(|h| h.to_str().ok()).map(str::to_owned);
					let socket = socket
						.map(axum_to_tungstenite)
						.sink_map_err(|_| tungstenite::Error::ConnectionClosed)
						.with(tungstenite_to_axum);

					let upgraded = qmux::ws::Upgraded::new(socket);
					let upgraded = match wire.as_deref() {
						Some(alpn) => upgraded.with_alpn(alpn),
						None => upgraded,
					};
					let session = upgraded.accept();
					let _ = tx.send(Observed {
						wire,
						app: session.protocol().map(str::to_owned),
					});
					// Hold the session open so the client stays alive long enough
					// to observe the negotiated subprotocol.
					let _ = session.closed().await;
				})
			}
		});

		let app = Router::new().route("/", route);
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
			.await
			.expect("bind listener");
		let addr = listener.local_addr().expect("local addr");
		tokio::spawn(async move {
			axum::serve(listener, app).await.expect("axum serve");
		});
		(addr, rx)
	}

	async fn next_observed(rx: &mut mpsc::UnboundedReceiver<Observed>) -> Observed {
		tokio::time::timeout(Duration::from_secs(5), rx.recv())
			.await
			.expect("server did not report a connection")
			.expect("server channel closed")
	}

	/// Split an advertised `{qmux-XX.}{moq-alpn}` pair into its parts, or `None`
	/// for a bare fallback (`qmux-01`, `qmux-00`, `webtransport`).
	fn split_pair(entry: &str) -> Option<(qmux::Version, &str)> {
		QMUX_VERSIONS
			.iter()
			.find_map(|&v| entry.strip_prefix(v.prefix()).map(|app| (v, app)))
	}

	/// End-to-end regression: a qmux client offering the full moq ALPN list must
	/// land on the newest moq ALPN (`moq_net::ALPNS[0]`) on both sides. A bug in
	/// `supported_subprotocols` or the `with_alpn` plumbing collapses this to
	/// `None` / bare `webtransport`, and moq-net then downgrades to Lite02.
	#[tokio::test]
	async fn axum_ws_negotiates_newest_moq_alpn() {
		let (addr, mut rx) = spawn_test_server().await;

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

		let observed = next_observed(&mut rx).await;
		assert_eq!(
			observed.app.as_deref(),
			Some(newest_moq_alpn()),
			"server side should see the newest moq ALPN after with_alpn",
		);

		drop(session);
	}

	/// Every versioned `(qmux, moq)` pair we advertise must be acceptable: a
	/// client offering exactly that pair negotiates it end-to-end. Conversely,
	/// the excluded `qmux-00.moqt-18` pair must never be selected, even when a
	/// client explicitly offers it.
	#[tokio::test]
	async fn every_advertised_pair_is_acceptable() {
		let (addr, mut rx) = spawn_test_server().await;
		let url = format!("ws://{addr}/");

		for entry in supported_subprotocols() {
			// Bare fallbacks can't be offered in isolation via the qmux client API;
			// they're covered by `axum_ws_negotiates_newest_moq_alpn`.
			let Some((version, app)) = split_pair(&entry) else {
				continue;
			};

			let session = qmux::Client::new()
				.with_protocol(app, &[version])
				.connect(&url)
				.await
				.unwrap_or_else(|e| panic!("server rejected advertised pair {entry}: {e}"));

			let observed = next_observed(&mut rx).await;
			assert_eq!(
				observed.wire.as_deref(),
				Some(entry.as_str()),
				"offered advertised pair {entry}, but server negotiated {:?}",
				observed.wire,
			);
			drop(session);
		}

		// The illegal pair is advertised by nobody (moqt-18 requires qmux-01). A client
		// offering only `qmux-00.moqt-18` still carries qmux's default bare fallbacks
		// (`webtransport`, etc.), so the server gracefully downgrades to a bare ALPN
		// rather than failing: we always accept a bare web-transport connection, and
		// there's no other qmux version worth negotiating for moqt-18. It just never
		// lands on `moqt-18`.
		let session = qmux::Client::new()
			.with_protocol("moqt-18", &[qmux::Version::QMux00])
			.connect(&url)
			.await
			.expect("qmux-00.moqt-18 should downgrade to a bare fallback, not fail");

		let observed = next_observed(&mut rx).await;
		assert_eq!(
			observed.app, None,
			"qmux-00.moqt-18 must never be selected; expected a bare downgrade, got {:?}",
			observed.app,
		);
		assert!(
			observed.wire.as_deref().is_none_or(|wire| split_pair(wire).is_none()),
			"the illegal pair must downgrade to a bare fallback, got {:?}",
			observed.wire,
		);
		drop(session);
	}
}
