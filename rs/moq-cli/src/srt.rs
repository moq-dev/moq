//! SRT endpoints. Like RTMP, listeners are directional: an import listener
//! accepts publishes only, an export listener serves requests only.

use std::net::SocketAddr;
use std::time::Duration;

use hang::moq_net;
use moq_srt::{Request, Server};

use crate::moq::notify_ready;

/// Accept incoming SRT publishes into the Origin; reject requests (import).
pub async fn listen_import(
	origin: moq_net::OriginProducer,
	addr: SocketAddr,
	prefix: Option<String>,
	latency: Duration,
) -> anyhow::Result<()> {
	let mut server = Server::bind(addr, latency).await?;
	tracing::info!(%addr, "SRT listening (import)");
	notify_ready();

	let prefix = prefix.unwrap_or_default();
	while let Some(request) = server.accept().await {
		match request {
			Request::Publish(publish) => {
				let path = format!("{prefix}{}", publish.resource());
				let origin = origin.clone();
				tokio::spawn(async move {
					if let Err(err) = publish.accept(&origin, &path).await {
						tracing::warn!(%path, %err, "SRT ingest ended with error");
					}
				});
			}
			Request::Subscribe(subscribe) => {
				tokio::spawn(async move {
					let _ = subscribe.reject().await;
				});
			}
			_ => {}
		}
	}

	Ok(())
}

/// Serve SRT requests from the Origin; reject publishes (export).
pub async fn listen_export(
	origin: moq_net::OriginConsumer,
	addr: SocketAddr,
	prefix: Option<String>,
	latency: Duration,
) -> anyhow::Result<()> {
	let mut server = Server::bind(addr, latency).await?;
	tracing::info!(%addr, "SRT listening (export)");
	notify_ready();

	let prefix = prefix.unwrap_or_default();
	while let Some(request) = server.accept().await {
		match request {
			Request::Subscribe(subscribe) => {
				let path = format!("{prefix}{}", subscribe.resource());
				let origin = origin.clone();
				tokio::spawn(async move {
					if let Err(err) = subscribe.accept(&origin, &path).await {
						tracing::warn!(%path, %err, "SRT request ended with error");
					}
				});
			}
			Request::Publish(publish) => {
				tokio::spawn(async move {
					let _ = publish.reject().await;
				});
			}
			_ => {}
		}
	}

	Ok(())
}

/// Dial a remote SRT server. Pending the dial-out (`dial`) library (#1982).
pub async fn connect_import(_origin: moq_net::OriginProducer, _url: url::Url) -> anyhow::Result<()> {
	anyhow::bail!("`import srt --connect` (SRT dial-out) is not implemented yet; see moq-dev/moq#1982");
}

/// Push a broadcast to a remote SRT server. Pending the dial-out library (#1982).
pub async fn connect_export(_origin: moq_net::OriginConsumer, _url: url::Url, _name: String) -> anyhow::Result<()> {
	anyhow::bail!("`export srt --connect` (SRT dial-out) is not implemented yet; see moq-dev/moq#1982");
}
