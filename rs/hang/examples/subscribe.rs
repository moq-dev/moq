// cargo run --example subscribe

use std::time::Duration;

use anyhow::Context;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	// Optional: Use moq_native to configure a logger.
	moq_native::Log::new(tracing::Level::DEBUG).init()?;

	// Create an origin that the session can publish incoming broadcasts to.
	let origin = moq_net::Origin::random().produce();
	let consumer = origin.consume();

	// Run the subscription and the session in parallel.
	tokio::select! {
		res = run_session(origin) => res,
		res = run_subscribe(consumer) => res,
	}
}

// Connect to the server and subscribe to broadcasts.
// Automatically reconnects if the connection drops.
async fn run_session(origin: moq_net::OriginProducer) -> anyhow::Result<()> {
	// Optional: Use moq_native to make a QUIC client.
	let client = moq_native::ClientConfig::default().init()?;

	// For local development, use: http://localhost:4443/anon/video-example
	// The "anon" path is usually configured to bypass authentication; be careful!
	let url = url::Url::parse("https://cdn.moq.dev/anon/video-example").unwrap();

	// Establish a connection with automatic reconnection.
	// with_consumer() registers an OriginProducer for incoming data.
	// Use with_publisher() if you also want to publish from the session.
	let reconnect = client.with_consumer(origin).reconnect(url);

	// Wait until the reconnect loop stops (e.g. timeout exceeded).
	reconnect.closed().await
}

// Subscribe to a broadcast and read media frames.
async fn run_subscribe(consumer: moq_net::OriginConsumer) -> anyhow::Result<()> {
	// Wait for a broadcast to be announced.
	let (path, broadcast) = consumer.announced().next().await.context("origin closed")?;

	let broadcast = broadcast
		.broadcast()
		.with_context(|| format!("broadcast unannounced: {path}"))?;

	tracing::info!(%path, "broadcast announced");

	// Read the catalog to discover available tracks.
	let catalog_track = broadcast
		.consume_track(hang::Catalog::DEFAULT_NAME)
		.subscribe(hang::Catalog::default_subscription())
		.await?;
	let mut catalog = moq_mux::catalog::hang::Consumer::new(catalog_track);

	let info = catalog.next().await?.ok_or_else(|| anyhow::anyhow!("no catalog"))?;

	// Find the first video track.
	let (name, config) = info
		.video
		.renditions
		.iter()
		.next()
		.ok_or_else(|| anyhow::anyhow!("no video renditions"))?;

	tracing::info!(
		%name,
		codec = %config.codec,
		width = ?config.coded_width,
		height = ?config.coded_height,
		"subscribing to video track"
	);

	// Subscribe to the video track.
	let track_consumer = broadcast
		.consume_track(name)
		.subscribe(moq_net::Subscription {
			priority: 1,
			..Default::default()
		})
		.await?;
	let mut ordered = moq_mux::container::Consumer::new(track_consumer, moq_mux::catalog::hang::Container::Legacy)
		.with_latency(Duration::from_millis(500));

	// Read frames in latency-bounded presentation order.
	while let Some(frame) = ordered.read().await? {
		tracing::info!(
			timestamp = ?frame.timestamp,
			keyframe = frame.keyframe,
			bytes = frame.payload.len(),
			"received frame"
		);
	}

	Ok(())
}
