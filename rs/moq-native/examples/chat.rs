// cargo run --example chat

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
	let config = moq_native::ClientConfig::default();
	let client = moq_native::Client::new(config)?;

	// For local development, use: http://localhost:4443/anon
	// The "anon" path is usually configured to bypass authentication; be careful!
	let url = url::Url::parse("https://cdn.moq.dev/anon/chat-example").unwrap();

	// Establish a WebTransport/QUIC connection.
	let connection = client.connect(url).await?;

	// Perform the MoQ handshake.
	// None means we're not consuming anything from the session, otherwise we would provide an OriginProducer.
	let session = moq_lite::Session::connect(connection, origin, None).await?;

	// Wait until the session is closed.
	session.closed().await.map_err(Into::into)
}

// Produce a broadcast and publish it to the origin.
async fn run_broadcast(origin: moq_lite::OriginProducer) -> anyhow::Result<()> {
	// Create and publish a broadcast to the origin..
	// A broadcast is a collection of tracks, but in this example we'll only create one.
	let mut broadcast = moq_lite::BroadcastProducer::new();

	// Create a track that we'll insert into the broadcast.
	// A track is a series of groups representing a live stream.
	let mut track = broadcast.create_track(moq_lite::Track {
		name: "chat".to_string(),
		priority: 0,
		// You can configure the amount of time to keep old messages in cache.
		max_latency: moq_lite::Time::from_secs(10)?,
	});

	// NOTE: The path is empty because we're using the URL to scope the broadcast.
	// If you put "alice" here, it would be published as "anon/chat-example/alice".
	// OPTIONAL: We publish after inserting the track just to avoid a nearly impossible race condition.
	origin.publish_broadcast("", broadcast.consume());

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
