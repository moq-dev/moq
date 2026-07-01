//! RTMP endpoints. Listeners are directional: an import listener accepts
//! publishes only (rejecting plays), an export listener serves plays only
//! (rejecting publishes). The operator declares direction; the peer can't choose.

use std::net::SocketAddr;

use hang::moq_net;
use moq_rtmp::{Client, Request, Server};
use url::Url;

use crate::moq::notify_ready;

/// RTMP endpoint args: exactly one of `--connect` (dial) / `--listen` (bind).
/// The parent direction fixes whether that dial/bind pushes or pulls.
#[derive(clap::Args, Clone)]
#[command(group = clap::ArgGroup::new("rtmp-mode").required(true).multiple(false).args(["rtmp-connect", "rtmp-listen"]))]
pub struct Args {
	/// Dial `rtmp://host[:1935]/<app>/<key>`.
	#[arg(id = "rtmp-connect", long = "connect", value_name = "URL")]
	pub connect: Option<Url>,

	/// Bind an RTMP listener. Broadcasts are named from the RTMP app/key.
	#[arg(id = "rtmp-listen", long = "listen", value_name = "ADDR")]
	pub listen: Option<SocketAddr>,

	/// Prefix prepended to the app/key when naming broadcasts.
	#[arg(long, requires = "rtmp-listen")]
	pub prefix: Option<String>,
}

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

/// Dial a remote RTMP server and pull its play into the Origin under `name` (import).
pub async fn connect_import(origin: moq_net::OriginProducer, url: Url, name: String) -> anyhow::Result<()> {
	let (addr, app, key) = parse_url(&url).await?;
	tracing::info!(%url, %name, "RTMP client pulling");
	notify_ready();

	let client = Client::connect(addr, &app).await?;
	Ok(client.pull(&key, &origin, &name).await?)
}

/// Push a broadcast from the Origin to a remote RTMP server (export).
pub async fn connect_export(origin: moq_net::OriginConsumer, url: Url, name: String) -> anyhow::Result<()> {
	let (addr, app, key) = parse_url(&url).await?;
	let broadcast = origin
		.announced_broadcast(&name)
		.await
		.ok_or_else(|| anyhow::anyhow!("origin closed before broadcast `{name}` was announced"))?;

	tracing::info!(%url, %name, "RTMP client pushing");
	notify_ready();

	let client = Client::connect(addr, &app).await?;
	Ok(client.publish(&key, broadcast).await?)
}

/// Parse `rtmp://host[:1935]/<app>/<key>` into a resolved address, app, and stream key.
async fn parse_url(url: &Url) -> anyhow::Result<(SocketAddr, String, String)> {
	let host = url
		.host_str()
		.ok_or_else(|| anyhow::anyhow!("rtmp url missing host: {url}"))?;
	let port = url.port().unwrap_or(1935);
	let addr = tokio::net::lookup_host((host, port))
		.await?
		.next()
		.ok_or_else(|| anyhow::anyhow!("could not resolve {host}:{port}"))?;

	let mut segments = url.path().trim_matches('/').splitn(2, '/');
	let app = segments.next().unwrap_or_default().to_string();
	let key = segments.next().unwrap_or_default().to_string();
	anyhow::ensure!(!app.is_empty(), "rtmp url must include an app: rtmp://host/<app>/<key>");

	Ok((addr, app, key))
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
