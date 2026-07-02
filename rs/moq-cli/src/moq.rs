//! The MoQ side of the router: attaching the shared Origin to the network via a
//! dialed client (`--client-connect`) and/or a hosted server (`--server-bind`).
//!
//! Direction determines data flow off the one shared Origin:
//! - import: MoQ publishes the Origin outward (client publishes to the relay;
//!   each accepted server session serves subscribers).
//! - export: MoQ fills the Origin (client subscribes from the relay; each
//!   accepted server session ingests a remote publish).

use hang::moq_net;
use url::Url;

/// Dial a relay and publish the Origin's broadcasts to it (import).
pub async fn client_import(
	client: moq_native::Client,
	url: Url,
	origin: &moq_net::OriginProducer,
) -> anyhow::Result<()> {
	let reconnect = client.with_publisher(origin).reconnect(url);
	notify_ready();
	Ok(reconnect.closed().await?)
}

/// Dial a relay and subscribe its broadcasts into the Origin (export).
pub async fn client_export(
	client: moq_native::Client,
	url: Url,
	origin: moq_net::OriginProducer,
) -> anyhow::Result<()> {
	let reconnect = client.with_subscriber(origin).reconnect(url);
	notify_ready();
	Ok(reconnect.closed().await?)
}

/// Host a MoQ server; each accepted session serves the Origin to subscribers (import).
pub async fn server_import(mut server: moq_native::Server, origin: moq_net::OriginProducer) -> anyhow::Result<()> {
	notify_ready();
	tracing::info!(addr = ?server.local_addr(), "listening");

	while let Some(session) = server.accept().await {
		let origin = origin.clone();
		tokio::spawn(async move {
			if let Err(err) = serve_session(session, origin).await {
				tracing::warn!(%err, "session ended with error");
			}
		});
	}

	Ok(())
}

async fn serve_session(session: moq_native::Request, origin: moq_net::OriginProducer) -> anyhow::Result<()> {
	let session = session.with_publisher(&origin).ok().await?;
	Ok(session.closed().await?)
}

/// Host a MoQ server; each accepted session ingests a remote publish into the Origin (export).
pub async fn server_export(mut server: moq_native::Server, origin: moq_net::OriginProducer) -> anyhow::Result<()> {
	notify_ready();
	tracing::info!(addr = ?server.local_addr(), "listening");

	while let Some(session) = server.accept().await {
		let origin = origin.clone();
		tokio::spawn(async move {
			if let Err(err) = accept_session(session, origin).await {
				tracing::warn!(%err, "session ended with error");
			}
		});
	}

	Ok(())
}

async fn accept_session(session: moq_native::Request, origin: moq_net::OriginProducer) -> anyhow::Result<()> {
	let session = session.with_subscriber(origin).ok().await?;
	Ok(session.closed().await?)
}

/// Notify systemd (if any) that the endpoint is up.
pub fn notify_ready() {
	#[cfg(unix)]
	let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);
}
