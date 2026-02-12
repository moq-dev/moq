use futures::{SinkExt, StreamExt};
use std::{
	future::Future,
	pin::Pin,
	sync::{Arc, atomic::Ordering},
};
use web_transport_ws::tungstenite;

use axum::{
	extract::{Path, Query, State, WebSocketUpgrade},
	http::StatusCode,
	response::Response,
};
use moq_lite::{OriginConsumer, OriginProducer};

use crate::{AuthParams, WebState, web::AuthQuery};

pub(crate) async fn serve_ws(
	ws: WebSocketUpgrade,
	Path(path): Path<String>,
	Query(query): Query<AuthQuery>,
	State(state): State<Arc<WebState>>,
) -> axum::response::Result<Response> {
	let ws = ws.protocols(["webtransport"]);

	let params = AuthParams {
		path,
		jwt: query.jwt,
		register: query.register,
	};
	let token = state.auth.verify(&params)?;
	let publish = state.cluster.publisher(&token);
	let subscribe = state.cluster.subscriber(&token);
	let registration = state.cluster.register(&token);

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
		drop(registration);
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
	let session = moq_lite::Server::new()
		.with_publish(subscribe)
		.with_consume(publish)
		// TODO: Uncomment when observability feature is merged
		// .with_stats(stats)
		.accept(ws)
		.await?;
	session.closed().await.map_err(Into::into)
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
