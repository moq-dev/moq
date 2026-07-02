//! RTMP endpoints. Listeners are directional: an import listener accepts
//! publishes only (rejecting plays), an export listener serves plays only
//! (rejecting publishes). The operator declares direction; the peer can't choose.

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Context;
use hang::moq_net;
use moq_rtmp::{Client, Request, Server};
use url::Url;

use crate::moq::notify_ready;

/// RTMP endpoint args: exactly one of `--connect` (dial) / `--listen` (bind).
/// The parent direction fixes whether that dial/bind pushes or pulls. Import uses
/// this directly; export wraps it in [`ExportArgs`] for the egress-only knobs.
#[derive(clap::Args, Clone)]
#[command(group = clap::ArgGroup::new("rtmp-mode").required(true).multiple(false).args(["rtmp-connect", "rtmp-listen"]))]
pub struct Args {
	/// Dial `rtmp://host[:1935]/<app>/<key>`.
	#[arg(id = "rtmp-connect", long = "connect", value_name = "URL")]
	pub connect: Option<Url>,

	/// Bind an RTMP listener, bridging the single `--broadcast` (the RTMP app/key
	/// is accepted but not used for routing).
	#[arg(id = "rtmp-listen", long = "listen", value_name = "ADDR")]
	pub listen: Option<SocketAddr>,
}

/// RTMP export args: the endpoint plus egress-only tuning. Split from the import
/// side so the frame-drop knob only shows where it applies.
#[derive(clap::Args, Clone)]
pub struct ExportArgs {
	#[command(flatten)]
	pub endpoint: Args,

	/// Maximum latency before skipping a stalled group. RTMP is unpaced, so this
	/// bounds buffering, not the wire rate.
	#[arg(long = "latency-max", default_value = "500ms", value_parser = humantime::parse_duration)]
	pub latency_max: Duration,
}

/// Accept incoming RTMP publishes into the Origin as `name`; reject plays (import).
pub async fn listen_import(origin: moq_net::OriginProducer, addr: SocketAddr, name: String) -> anyhow::Result<()> {
	let mut server = Server::bind(addr).await?;
	tracing::info!(%addr, %name, "RTMP listening (import)");
	notify_ready();

	while let Some(request) = server.accept().await {
		match request {
			Request::Publish(publish) => {
				let origin = origin.clone();
				let name = name.clone();
				tokio::spawn(async move {
					if let Err(err) = publish.accept(&origin, &name).await {
						tracing::warn!(%name, %err, "RTMP ingest ended with error");
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

/// Serve RTMP plays of `name` from the Origin; reject publishes (export).
pub async fn listen_export(
	origin: moq_net::OriginConsumer,
	addr: SocketAddr,
	name: String,
	latency: Duration,
) -> anyhow::Result<()> {
	let mut server = Server::bind(addr).await?;
	tracing::info!(%addr, %name, "RTMP listening (export)");
	notify_ready();

	while let Some(request) = server.accept().await {
		match request {
			Request::Play(play) => {
				let origin = origin.clone();
				let name = name.clone();
				tokio::spawn(async move {
					if let Err(err) = play.with_latency(latency).accept(&origin, &name).await {
						tracing::warn!(%name, %err, "RTMP play ended with error");
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

/// Dial a remote RTMP server and pull its play into the Origin under `name` (import).
pub async fn connect_import(origin: moq_net::OriginProducer, url: Url, name: String) -> anyhow::Result<()> {
	let (addr, app, key) = parse_url(&url).await?;
	tracing::info!(%url, %name, "RTMP client pulling");
	notify_ready();

	let client = Client::connect(addr, &app).await?;
	Ok(client.pull(&key, &origin, &name).await?)
}

/// Push a broadcast from the Origin to a remote RTMP server (export).
pub async fn connect_export(
	origin: moq_net::OriginConsumer,
	url: Url,
	name: String,
	latency: Duration,
) -> anyhow::Result<()> {
	let (addr, app, key) = parse_url(&url).await?;
	let broadcast = origin
		.announced_broadcast(&name)
		.await
		.with_context(|| format!("origin closed before broadcast `{name}` was announced"))?;

	tracing::info!(%url, %name, "RTMP client pushing");
	notify_ready();

	let client = Client::connect(addr, &app).await?.with_latency(latency);
	Ok(client.publish(&key, broadcast).await?)
}

/// Parse `rtmp://host[:1935]/<app>/<key>` into a resolved address, app, and stream key.
async fn parse_url(url: &Url) -> anyhow::Result<(SocketAddr, String, String)> {
	anyhow::ensure!(url.scheme() == "rtmp", "rtmp url must use the rtmp scheme: {url}");

	let host = url
		.host_str()
		.with_context(|| format!("rtmp url missing host: {url}"))?;
	let port = url.port().unwrap_or(1935);
	let addr = tokio::net::lookup_host((host, port))
		.await?
		.next()
		.with_context(|| format!("could not resolve {host}:{port}"))?;

	let mut segments = url.path().trim_matches('/').splitn(2, '/');
	let app = segments.next().unwrap_or_default().to_string();
	let key = segments.next().unwrap_or_default().to_string();
	anyhow::ensure!(
		!app.is_empty() && !key.is_empty(),
		"rtmp url must include an app and stream key: rtmp://host/<app>/<key>"
	);

	Ok((addr, app, key))
}

#[cfg(test)]
mod tests {
	use super::*;

	// Numeric hosts resolve without touching DNS, so these stay offline.
	async fn parse(url: &str) -> anyhow::Result<(SocketAddr, String, String)> {
		parse_url(&Url::parse(url).unwrap()).await
	}

	#[tokio::test]
	async fn ok_default_port() {
		let (addr, app, key) = parse("rtmp://127.0.0.1/live/cam0").await.unwrap();
		assert_eq!(addr.port(), 1935);
		assert_eq!((app.as_str(), key.as_str()), ("live", "cam0"));
	}

	#[tokio::test]
	async fn ok_explicit_port() {
		let (addr, _, _) = parse("rtmp://127.0.0.1:1936/live/cam0").await.unwrap();
		assert_eq!(addr.port(), 1936);
	}

	#[tokio::test]
	async fn rejects_non_rtmp_scheme() {
		assert!(parse("http://127.0.0.1/live/cam0").await.is_err());
	}

	#[tokio::test]
	async fn requires_app_and_key() {
		assert!(parse("rtmp://127.0.0.1/live").await.is_err());
		assert!(parse("rtmp://127.0.0.1/").await.is_err());
	}
}
