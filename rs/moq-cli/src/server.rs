use anyhow::Context;
use hang::moq_net;

pub async fn run_server(
	mut server: moq_native::Server,
	name: String,
	consumer: moq_net::BroadcastConsumer,
) -> anyhow::Result<()> {
	#[cfg(unix)]
	// Notify systemd that we're ready.
	let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);

	let mut conn_id = 0;

	tracing::info!(addr = ?server.local_addr(), "listening");

	while let Some(session) = server.accept().await {
		let id = conn_id;
		conn_id += 1;

		let name = name.clone();

		let consumer = consumer.clone();
		// Handle the connection in a new task.
		tokio::spawn(async move {
			if let Err(err) = run_serve_session(id, session, name, consumer).await {
				tracing::warn!(%err, "failed to accept session");
			}
		});
	}

	Ok(())
}

#[tracing::instrument("session", skip_all, fields(id))]
async fn run_serve_session(
	id: u64,
	session: moq_native::Request,
	name: String,
	consumer: moq_net::BroadcastConsumer,
) -> anyhow::Result<()> {
	// Create an origin producer to publish to the broadcast.
	let origin = moq_net::Origin::random().produce();
	let _publish = origin
		.publish_broadcast(&name, consumer)
		.context("failed to publish broadcast")?;

	// Blindly accept the session (WebTransport or QUIC), regardless of the URL.
	let session = session.with_publisher(origin.clone()).ok().await?;

	tracing::info!(id, "accepted session");

	session.closed().await.map_err(Into::into)
}

pub async fn run_accept(mut server: moq_native::Server, origin: moq_net::OriginProducer) -> anyhow::Result<()> {
	#[cfg(unix)]
	// Notify systemd that we're ready.
	let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);

	let mut conn_id = 0;

	tracing::info!(addr = ?server.local_addr(), "listening");

	while let Some(session) = server.accept().await {
		let id = conn_id;
		conn_id += 1;

		let origin = origin.clone();
		tokio::spawn(async move {
			if let Err(err) = run_accept_session(id, session, origin).await {
				tracing::warn!(%err, "failed to accept session");
			}
		});
	}

	Ok(())
}

#[tracing::instrument("session", skip_all, fields(id))]
async fn run_accept_session(
	id: u64,
	session: moq_native::Request,
	origin: moq_net::OriginProducer,
) -> anyhow::Result<()> {
	let session = session.with_consumer(origin).ok().await?;

	tracing::info!(id, "accepted session");

	session.closed().await.map_err(Into::into)
}
