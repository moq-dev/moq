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
use moq_net::{OriginConsumer, OriginProducer, QmuxVersion, StatsHandle, Tier};

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

	let ws = ws.protocols(moq_net::QMUX_ALPN_STRINGS.iter().copied());

	let params = AuthParams { path, jwt: query.jwt };
	let token = if mtls.is_some() {
		AuthToken::unrestricted()
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

		// Pull the negotiated subprotocol off the WebSocket before we wrap it
		// in adapters. Without it we can't tell which qmux draft the peer
		// expects to speak.
		let Some(negotiated) = socket.protocol().and_then(|h| h.to_str().ok()).map(str::to_owned) else {
			tracing::warn!("client connected with no Sec-WebSocket-Protocol");
			return;
		};
		// Axum filtered to QMUX_ALPN_STRINGS for us, so the negotiated value
		// must be one of those entries; recover the qmux draft by index.
		let Some(idx) = moq_net::QMUX_ALPN_STRINGS
			.iter()
			.position(|s| *s == negotiated.as_str())
		else {
			tracing::warn!(%negotiated, "client negotiated an unrecognized Sec-WebSocket-Protocol");
			return;
		};
		let (qv, _) = moq_net::QMUX_ALPNS[idx];

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
		let handler = Handler {
			id,
			qv,
			negotiated,
			publish,
			subscribe,
			stats,
		};
		let _ = handler.run(socket).await;
	}))
}

/// Owns the per-connection state for one upgraded WebSocket, ready to be wrapped
/// in a qmux session and handed off to `moq_net::Server`.
struct Handler {
	id: u64,
	qv: QmuxVersion,
	negotiated: String,
	publish: Option<OriginProducer>,
	subscribe: Option<OriginConsumer>,
	stats: StatsHandle,
}

impl Handler {
	#[tracing::instrument("ws", err, skip_all, fields(id = self.id, qmux = ?self.qv, alpn = %self.negotiated))]
	async fn run<T>(self, socket: T) -> anyhow::Result<()>
	where
		T: futures::Stream<Item = Result<tungstenite::Message, tungstenite::Error>>
			+ futures::Sink<tungstenite::Message, Error = tungstenite::Error>
			+ Send
			+ Unpin
			+ 'static,
	{
		// Wrap the WebSocket in a qmux session pinned to the negotiated draft.
		let ws = qmux::ws::Upgraded::new(socket, qmux_version(self.qv))
			.with_alpn(&self.negotiated)
			.accept();
		let session = moq_net::Server::new()
			.with_publish(self.subscribe)
			.with_consume(self.publish)
			.with_stats(self.stats)
			.accept(ws)
			.await?;
		session.closed().await.map_err(Into::into)
	}
}

fn qmux_version(qv: QmuxVersion) -> qmux::Version {
	// `QmuxVersion` is `#[non_exhaustive]`, hence the catch-all arm.
	match qv {
		QmuxVersion::QMux00 => qmux::Version::QMux00,
		QmuxVersion::QMux01 => qmux::Version::QMux01,
		_ => unreachable!("unknown QmuxVersion variant"),
	}
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
