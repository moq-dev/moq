//! RTMP endpoints. Listeners are directional: an import listener accepts
//! publishes only (rejecting plays), an export listener serves plays only
//! (rejecting publishes). The operator declares direction; the peer can't choose.

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Context;
use moq_rtmp::{Client, Connect, Request, Server};
use url::Url;

use crate::moq::{ImportTarget, notify_ready};

/// RTMP endpoint args: exactly one of `--connect` (dial) / `--listen` (bind).
/// The parent direction fixes whether that dial/bind pushes or pulls. Import uses
/// this directly; export wraps it in [`ExportArgs`] for the egress-only knobs.
#[derive(usage_rs::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[usage(group("rtmp-mode", required))]
pub struct Args {
	/// Dial `rtmp://host[:1935]/<app>/<key>` or `rtmps://host[:443]/<app>/<key>`,
	/// trusting the system's root certificates for RTMPS.
	#[usage(name = "rtmp-connect", long = "connect", value_name = "URL", group = "rtmp-mode")]
	pub connect: Option<Url>,

	/// Bind an RTMP listener, bridging the single `--broadcast` (the RTMP app/key
	/// is accepted but not used for routing).
	#[usage(name = "rtmp-listen", long = "listen", value_name = "ADDR", group = "rtmp-mode")]
	pub listen: Option<SocketAddr>,
}

/// RTMP export args: the endpoint plus egress-only tuning. Split from the import
/// side so the frame-drop knob only shows where it applies.
#[derive(usage_rs::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct ExportArgs {
	#[usage(flatten)]
	pub endpoint: Args,

	/// FLV frame release delay and stalled-group budget. Late video resumes at
	/// its next keyframe; zero releases available frames without deadline drops.
	#[usage(long, default = "500ms")]
	pub delay: moq_tokio::cli::Duration,

	/// The released spelling of `--delay`.
	#[usage(long = "max-age", hide = true)]
	max_age: Option<moq_tokio::cli::Duration>,

	/// The released spelling of `--delay`, before `--max-age`.
	#[usage(long = "latency-max", hide = true)]
	latency_max: Option<moq_tokio::cli::Duration>,
}

impl ExportArgs {
	pub(crate) fn deprecated(&self) -> moq_tokio::cli::Deprecated {
		let mut found = moq_tokio::cli::Deprecated::default();
		if self.max_age.is_some() {
			found.flag("--max-age", None, "--delay");
		}
		if self.latency_max.is_some() {
			found.flag("--latency-max", None, "--delay");
		}
		found
	}
}

/// Accept incoming RTMP publishes into the Origin as `target.name`; reject plays (import).
pub async fn listen_import(target: ImportTarget, addr: SocketAddr) -> anyhow::Result<()> {
	let ImportTarget { origin, name, ingest } = target;
	let name = moq_net::Path::new(&name)?;
	let mut server = Server::bind(addr).await?;
	tracing::info!(%addr, %name, "RTMP listening (import)");
	notify_ready();

	while let Some(request) = server.accept().await {
		match request {
			Request::Publish(publish) => {
				let origin = origin.clone();
				let name = name.clone();
				let ingest = ingest.clone();
				tokio::spawn(async move {
					if let Err(err) = publish.accept(&origin, &name, &ingest).await {
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
	origin: moq_net::origin::Consumer,
	addr: SocketAddr,
	name: String,
	delay: Duration,
) -> anyhow::Result<()> {
	let name = moq_net::Path::new(&name)?;
	let mut server = Server::bind(addr).await?;
	tracing::info!(%addr, %name, "RTMP listening (export)");
	notify_ready();

	while let Some(request) = server.accept().await {
		match request {
			Request::Play(play) => {
				let origin = origin.clone();
				let name = name.clone();
				tokio::spawn(async move {
					if let Err(err) = play.with_delay(delay).accept(&origin, &name).await {
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

/// Dial a remote RTMP server and pull its play into the Origin under `target.name` (import).
pub async fn connect_import(target: ImportTarget, url: Url) -> anyhow::Result<()> {
	let (connect, key) = parse_url(&url)?;
	let name = moq_net::Path::new(&target.name)?;
	// The stream key is the ingest credential, so log the URL without it instead.
	tracing::info!(url = %connect.url, %name, "RTMP client pulling");
	notify_ready();

	let client = Client::connect(connect).await?;
	Ok(client.pull(&key, &target.origin, name, &target.ingest).await?)
}

/// Push a broadcast from the Origin to a remote RTMP server (export).
pub async fn connect_export(
	origin: moq_net::origin::Consumer,
	url: Url,
	name: String,
	delay: Duration,
) -> anyhow::Result<()> {
	let name = moq_net::Path::new(&name)?;
	let (connect, key) = parse_url(&url)?;
	// Wait for the broadcast to be announced before dialing; the FLV export resolves any
	// referenced sibling broadcast through the same origin.
	let broadcast = origin
		.follow(&name, None)?
		.available()
		.await
		.with_context(|| format!("failed to resolve broadcast `{name}`"))?
		.context("went offline before serving")?;
	let source = moq_mux::Source::new(origin, broadcast);

	// The stream key is the ingest credential, so log the URL without it instead.
	tracing::info!(url = %connect.url, %name, "RTMP client pushing");
	notify_ready();

	let client = Client::connect(connect).await?.with_delay(delay);
	Ok(client.publish(&key, source).await?)
}

/// Split `rtmp[s]://host[:port]/<app>/<key>` into what to dial and the stream key.
/// `rtmps://` trusts the system's root certificates.
fn parse_url(url: &Url) -> anyhow::Result<(Connect, String)> {
	let path = url.path().trim_matches('/').to_string();
	let (app, key) = path.split_once('/').unwrap_or((&path, ""));
	anyhow::ensure!(
		!app.is_empty() && !key.is_empty(),
		"rtmp url must include an app and stream key: rtmp://host/<app>/<key>"
	);
	let mut dial = url.clone();
	dial.set_path(app);
	let mut connect = Connect::new(dial);
	if url.scheme() == "rtmps" {
		connect = connect.with_tls(std::sync::Arc::new(moq_tokio::tls::Connect::default().build()?));
	}
	Ok((connect, key.to_string()))
}

#[cfg(test)]
mod tests {
	use super::*;

	fn parse(url: &str) -> anyhow::Result<(Connect, String)> {
		parse_url(&Url::parse(url).unwrap())
	}

	#[test]
	fn splits_the_key_from_the_dial_url() {
		let (connect, key) = parse("rtmp://127.0.0.1:1936/live/cam0").unwrap();
		assert_eq!(connect.url.as_str(), "rtmp://127.0.0.1:1936/live");
		assert_eq!(key, "cam0");
	}

	#[test]
	fn requires_app_and_key() {
		assert!(parse("rtmp://127.0.0.1/live").is_err());
		assert!(parse("rtmp://127.0.0.1/").is_err());
	}
}
