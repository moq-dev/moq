// cargo run --example chat

use anyhow::Context;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	// Optional: Use moq_native to configure a logger.
	moq_native::Log {
		level: tracing::Level::DEBUG,
	}
	.init();

	// Create an origin that we can publish to and the session can consume from.
	let origin = moq_lite::OriginProducer::new();

	// Run the broadcast production and the session in parallel.
	// This is a simple example of how you can concurrently run multiple tasks.
	// tokio::spawn works too.
	tokio::select! {
		res = run_session(origin.consume()) => res,
		res = run_broadcast(origin) => res,
	}
}

// Connect to the server and publish our origin of broadcasts.
async fn run_session(origin: moq_lite::OriginConsumer) -> anyhow::Result<()> {
	// Optional: Use moq_native to make a QUIC client.
	let client = moq_native::ClientConfig::default().init()?;

	// For local development, use: http://localhost:4443/anon
	// The "anon" path is usually configured to bypass authentication; be careful!
	let url = url::Url::parse("https://cdn.moq.dev/anon/chat-example").unwrap();

	// Establish a WebTransport/QUIC connection and MoQ handshake.
	// Optional: You could do this as two separate steps, but this is more convenient.
	// Optional: Use connect_with_fallback if you also want to support WebSocket too.
	let session = client.connect(url, origin, None).await?;

	// Wait until the session is closed.
	session.closed().await.map_err(Into::into)
}

// Produce a broadcast and publish it to the origin.
async fn run_broadcast(origin: moq_lite::OriginProducer) -> anyhow::Result<()> {
	// NOTE: The path is empty because we're using the URL to scope the broadcast.
	// If you put "alice" here, it would be published as "anon/chat-example/alice".
	let mut broadcast = origin.create_broadcast("").context("not allowed to publish")?;

	// A track is a series of groups representing a live stream.
	let mut track = broadcast.create_track(
		"chat",
		moq_lite::Delivery {
			priority: 0,
			max_latency: moq_lite::Time::from_secs(10)?,
			ordered: true,
		},
	)?;

	// Create a group.
	// Each group is independent and the newest group(s) will be prioritized.
	let mut group = track.append_group()?;

	// Write frames to the group.
	// Each frame is dependent on the previous frame, so older frames are prioritized.
	group.write_frame(bytes::Bytes::from_static(b"Hello"), moq_lite::Time::from_secs(1)?)?;
	group.write_frame(bytes::Bytes::from_static(b"World"), moq_lite::Time::from_secs(2)?)?;
	group.close()?;

	tracing::info!("wrote hello + world");

	// Sleep before sending our next message.
	tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;

	Ok(())
}
