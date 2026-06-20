//! Local QUIC/WebTransport server for `serve` mode.
//!
//! Accepts MoQ sessions and serves every broadcast the RTMP listener has
//! published into `origin`, so subscribers connect straight to this binary
//! instead of a separate relay.

use moq_net::{OriginConsumer, OriginProducer};

/// Accept sessions and publish `origin`'s broadcasts to each subscriber.
pub async fn run(mut server: moq_native::Server, origin: OriginProducer) -> anyhow::Result<()> {
	tracing::info!(addr = ?server.local_addr(), "listening");

	let mut conn_id = 0;
	while let Some(request) = server.accept().await {
		let id = conn_id;
		conn_id += 1;

		let consumer = origin.consume();
		tokio::spawn(async move {
			if let Err(err) = serve_session(id, request, consumer).await {
				tracing::warn!(%err, "session ended");
			}
		});
	}

	anyhow::bail!("server stopped accepting connections")
}

#[tracing::instrument("session", skip_all, fields(id))]
async fn serve_session(id: u64, request: moq_native::Request, consumer: OriginConsumer) -> anyhow::Result<()> {
	// Blindly accept the session (WebTransport or QUIC), serving every ingested
	// broadcast to the subscriber.
	let session = request.with_publisher(consumer).ok().await?;

	tracing::info!(id, "accepted session");

	session.closed().await.map_err(Into::into)
}
