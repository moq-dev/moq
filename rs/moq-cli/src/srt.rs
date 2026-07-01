//! SRT endpoints. Like RTMP, listeners are directional: an import listener
//! accepts publishes only, an export listener serves requests only.

use std::net::SocketAddr;
use std::time::Duration;

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

	/// Bind an SRT listener. Broadcasts are named from the stream id.
	#[arg(id = "srt-listen", long = "listen", value_name = "ADDR")]
	pub listen: Option<SocketAddr>,

	/// Prefix prepended to the stream id when naming broadcasts.
	#[arg(long)]
	pub prefix: Option<String>,

	/// SRT receive latency: the negotiated buffer trading delay for loss recovery.
	#[arg(long, default_value = "200ms", value_parser = humantime::parse_duration)]
	pub latency: Duration,
}

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
	let host = url
		.host_str()
		.ok_or_else(|| anyhow::anyhow!("srt url missing host: {url}"))?;
	let port = url
		.port()
		.ok_or_else(|| anyhow::anyhow!("srt url must include a port: srt://host:port"))?;
	let addr = tokio::net::lookup_host((host, port))
		.await?
		.next()
		.ok_or_else(|| anyhow::anyhow!("could not resolve {host}:{port}"))?;

	let resource = url
		.query_pairs()
		.find(|(key, _)| key == "streamid")
		.map(|(_, value)| value.into_owned())
		.unwrap_or_else(|| url.path().trim_matches('/').to_string());
	anyhow::ensure!(!resource.is_empty(), "srt url must include a streamid or path");

	Ok((addr, resource))
}
