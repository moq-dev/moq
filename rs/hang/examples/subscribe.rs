// cargo run --example subscribe

use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	// Optional: Use moq_native to configure a logger.
	moq_native::Log::new(tracing::Level::DEBUG).init();

	// Create an origin that the session can publish incoming broadcasts to.
	let origin = moq_lite::Origin::produce();
	let consumer = origin.consume();

	// Run the subscription and the session in parallel.
	tokio::select! {
		res = run_session(origin) => res,
		res = run_subscribe(consumer) => res,
	}
}

// Connect to the server and subscribe to broadcasts.
async fn run_session(origin: moq_lite::OriginProducer) -> anyhow::Result<()> {
	// Optional: Use moq_native to make a QUIC client.
	let client = moq_native::ClientConfig::default().init()?;

	// For local development, use: http://localhost:4443/anon/video-example
	// The "anon" path is usually configured to bypass authentication; be careful!
	let url = url::Url::parse("https://cdn.moq.dev/anon/video-example").unwrap();

	// Establish a WebTransport/QUIC connection and MoQ handshake for subscribing.
	// with_consume() registers an OriginProducer for incoming data.
	// Use with_publish() if you also want to publish from the session.
	let session = client.with_consume(origin).connect(url).await?;

	// Wait until the session is closed.
	session.closed().await.map_err(Into::into)
}

// Subscribe to a broadcast and read media frames.
async fn run_subscribe(mut consumer: moq_lite::OriginConsumer) -> anyhow::Result<()> {
	// Wait for a broadcast to be announced.
	let (path, broadcast) = consumer
		.announced()
		.await
		.ok_or_else(|| anyhow::anyhow!("origin closed"))?;

	let broadcast = broadcast.ok_or_else(|| anyhow::anyhow!("broadcast unannounced: {path}"))?;

	tracing::info!(%path, "broadcast announced");

	// Read the catalog to discover available tracks.
	let catalog_track = broadcast.subscribe_track(&hang::Catalog::default_track());
	let mut catalog = hang::CatalogConsumer::new(catalog_track);

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
	let track = moq_lite::Track {
		name: name.clone(),
		priority: 1,
	};

	let track_consumer = broadcast.subscribe_track(&track);
	let mut ordered = hang::container::OrderedConsumer::new(track_consumer, Duration::from_millis(500));

	// Read frames in presentation order.
	while let Some(frame) = ordered.read().await? {
		tracing::info!(
			timestamp = ?frame.timestamp,
			keyframe = frame.keyframe,
			bytes = frame.payload.num_bytes(),
			"received frame"
		);
	}

	Ok(())
}
