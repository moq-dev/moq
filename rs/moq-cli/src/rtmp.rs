//! RTMP endpoints. Listeners are directional: an import listener accepts
//! publishes only (rejecting plays), an export listener serves plays only
//! (rejecting publishes). The operator declares direction; the peer can't choose.

use std::net::SocketAddr;

use hang::moq_net;
use moq_rtmp::{Request, Server};

use crate::moq::notify_ready;

/// Accept incoming RTMP publishes into the Origin; reject plays (import).
pub async fn listen_import(
	origin: moq_net::OriginProducer,
	addr: SocketAddr,
	prefix: Option<String>,
) -> anyhow::Result<()> {
	let mut server = Server::bind(addr).await?;
	tracing::info!(%addr, "RTMP listening (import)");
	notify_ready();

	let prefix = prefix.unwrap_or_default();
	while let Some(request) = server.accept().await {
		match request {
			Request::Publish(publish) => {
				let Some(path) = resolve_path(&prefix, publish.app(), publish.stream_key()) else {
					let _ = publish.reject("empty broadcast path").await;
					continue;
				};
				let origin = origin.clone();
				tokio::spawn(async move {
					if let Err(err) = publish.accept(&origin, &path).await {
						tracing::warn!(%path, %err, "RTMP ingest ended with error");
					}
				});
			}
			Request::Play(play) => {
				tokio::spawn(async move {
					let _ = play.reject("this is an import listener; it does not serve plays").await;
				});
			}
			_ => {}
		}
	}

	Ok(())
}

/// Serve RTMP plays from the Origin; reject publishes (export).
pub async fn listen_export(
	origin: moq_net::OriginConsumer,
	addr: SocketAddr,
	prefix: Option<String>,
) -> anyhow::Result<()> {
	let mut server = Server::bind(addr).await?;
	tracing::info!(%addr, "RTMP listening (export)");
	notify_ready();

	let prefix = prefix.unwrap_or_default();
	while let Some(request) = server.accept().await {
		match request {
			Request::Play(play) => {
				let Some(path) = resolve_path(&prefix, play.app(), play.stream_key()) else {
					let _ = play.reject("empty broadcast path").await;
					continue;
				};
				let origin = origin.clone();
				tokio::spawn(async move {
					if let Err(err) = play.accept(&origin, &path).await {
						tracing::warn!(%path, %err, "RTMP play ended with error");
					}
				});
			}
			Request::Publish(publish) => {
				tokio::spawn(async move {
					let _ = publish
						.reject("this is an export listener; it does not accept publishes")
						.await;
				});
			}
			_ => {}
		}
	}

	Ok(())
}

/// Dial a remote RTMP server. Pending the dial-out (`Client`) library (#1982).
pub async fn connect_import(_origin: moq_net::OriginProducer, _url: url::Url) -> anyhow::Result<()> {
	anyhow::bail!("`import rtmp --connect` (RTMP dial-out) is not implemented yet; see moq-dev/moq#1982");
}

/// Push a broadcast to a remote RTMP server. Pending the dial-out library (#1982).
pub async fn connect_export(_origin: moq_net::OriginConsumer, _url: url::Url, _name: String) -> anyhow::Result<()> {
	anyhow::bail!("`export rtmp --connect` (RTMP dial-out) is not implemented yet; see moq-dev/moq#1982");
}

/// Join a prefix and the RTMP app/key into a broadcast path (empty -> None).
fn resolve_path(prefix: &str, app: &str, key: &str) -> Option<String> {
	let app = app.trim_matches('/');
	let key = key.trim_matches('/');
	let base = match (app.is_empty(), key.is_empty()) {
		(true, true) => return None,
		(false, true) => app.to_string(),
		(true, false) => key.to_string(),
		(false, false) => format!("{app}/{key}"),
	};
	Some(format!("{prefix}{base}"))
}
