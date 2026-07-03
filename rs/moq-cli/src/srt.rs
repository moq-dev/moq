//! SRT endpoints. Like RTMP, listeners are directional: an import listener
//! accepts publishes only, an export listener serves requests only.

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Context;
use hang::moq_net;
use moq_srt::{Request, Server};
use url::Url;

use crate::moq::notify_ready;

/// SRT endpoint args: exactly one of `--connect` (dial) / `--listen` (bind).
#[derive(clap::Args, Clone)]
#[command(group = clap::ArgGroup::new("srt-mode").required(true).multiple(false).args(["srt-connect", "srt-listen"]))]
pub struct Args {
	/// Dial `srt://host:port?streamid=...`.
	#[arg(id = "srt-connect", long = "connect", value_name = "URL")]
	pub connect: Option<Url>,

	/// Bind an SRT listener, bridging the single `--broadcast` (the SRT stream id
	/// is accepted but not used for routing).
	#[arg(id = "srt-listen", long = "listen", value_name = "ADDR")]
	pub listen: Option<SocketAddr>,

	/// SRT receive latency: the buffering delay traded for loss-recovery headroom.
	#[arg(long, default_value = "200ms", value_parser = humantime::parse_duration)]
	pub latency: Duration,
}

/// Accept incoming SRT publishes into the Origin as `name`; reject requests (import).
pub async fn listen_import(
	origin: moq_net::OriginProducer,
	addr: SocketAddr,
	name: String,
	latency: Duration,
) -> anyhow::Result<()> {
	let mut server = Server::bind(addr, latency).await?;
	tracing::info!(%addr, %name, "SRT listening (import)");
	notify_ready();

	while let Some(request) = server.accept().await {
		match request {
			Request::Publish(publish) => {
				let origin = origin.clone();
				let name = name.clone();
				tokio::spawn(async move {
					if let Err(err) = publish.accept(&origin, &name).await {
						tracing::warn!(%name, %err, "SRT ingest ended with error");
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

/// Serve SRT requests for `name` from the Origin; reject publishes (export).
pub async fn listen_export(
	origin: moq_net::OriginConsumer,
	addr: SocketAddr,
	name: String,
	latency: Duration,
) -> anyhow::Result<()> {
	let mut server = Server::bind(addr, latency).await?;
	tracing::info!(%addr, %name, "SRT listening (export)");
	notify_ready();

	while let Some(request) = server.accept().await {
		match request {
			Request::Subscribe(subscribe) => {
				let origin = origin.clone();
				let name = name.clone();
				tokio::spawn(async move {
					if let Err(err) = subscribe.accept(&origin, &name).await {
						tracing::warn!(%name, %err, "SRT request ended with error");
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

/// Dial a remote SRT server and pull its stream into the Origin under `name` (import).
pub async fn connect_import(
	origin: moq_net::OriginProducer,
	url: Url,
	name: String,
	latency: Duration,
) -> anyhow::Result<()> {
	let (addr, resource) = parse_url(&url).await?;
	tracing::info!(%url, %name, "SRT client pulling");
	notify_ready();

	Ok(moq_srt::dial::pull(addr, &resource, latency, &origin, &name).await?)
}

/// Push a broadcast from the Origin to a remote SRT server (export).
pub async fn connect_export(
	origin: moq_net::OriginConsumer,
	url: Url,
	name: String,
	latency: Duration,
) -> anyhow::Result<()> {
	let (addr, resource) = parse_url(&url).await?;
	tracing::info!(%url, %name, "SRT client pushing");
	notify_ready();

	Ok(moq_srt::dial::publish(addr, &resource, latency, &origin, &name).await?)
}

/// Parse `srt://host:port?streamid=<resource>` into a resolved address and resource.
/// The resource falls back to the URL path when `streamid` is absent.
async fn parse_url(url: &Url) -> anyhow::Result<(SocketAddr, String)> {
	anyhow::ensure!(url.scheme() == "srt", "srt url must use the srt scheme: {url}");

	let host = url.host_str().with_context(|| format!("srt url missing host: {url}"))?;
	let port = url.port().context("srt url must include a port: srt://host:port")?;
	let addr = tokio::net::lookup_host((host, port))
		.await?
		.next()
		.with_context(|| format!("could not resolve {host}:{port}"))?;

	let resource = url
		.query_pairs()
		.find(|(key, _)| key == "streamid")
		.map(|(_, value)| value.into_owned())
		.unwrap_or_else(|| url.path().trim_matches('/').to_string());
	anyhow::ensure!(!resource.is_empty(), "srt url must include a streamid or path");

	Ok((addr, resource))
}

#[cfg(test)]
mod tests {
	use super::*;

	// Numeric hosts resolve without touching DNS, so these stay offline.
	async fn parse(url: &str) -> anyhow::Result<(SocketAddr, String)> {
		parse_url(&Url::parse(url).unwrap()).await
	}

	#[tokio::test]
	async fn resource_from_streamid() {
		let (addr, resource) = parse("srt://127.0.0.1:9000?streamid=live/cam").await.unwrap();
		assert_eq!(addr.port(), 9000);
		assert_eq!(resource, "live/cam");
	}

	#[tokio::test]
	async fn resource_from_path() {
		let (_, resource) = parse("srt://127.0.0.1:9000/live/cam").await.unwrap();
		assert_eq!(resource, "live/cam");
	}

	#[tokio::test]
	async fn rejects_non_srt_scheme() {
		assert!(parse("udp://127.0.0.1:9000").await.is_err());
	}

	#[tokio::test]
	async fn requires_port_and_resource() {
		assert!(parse("srt://127.0.0.1").await.is_err());
		assert!(parse("srt://127.0.0.1:9000").await.is_err());
	}
}
