// cargo run --example chat

use anyhow::Context;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	// Optional: Use moq_native to configure a logger.
	moq_native::Log::new(tracing::Level::DEBUG).init()?;

	// Create an origin that we can publish to and the session can consume from.
	let origin = moq_net::Origin::random().produce();

	// Run the broadcast production and the session in parallel.
	// This is a simple example of how you can concurrently run multiple tasks.
	// tokio::spawn works too.
	tokio::select! {
		res = run_session(origin.clone()) => res,
		res = run_broadcast(origin) => res,
	}
}

// Connect to the server and publish our origin of broadcasts.
async fn run_session(origin: moq_net::origin::Producer) -> anyhow::Result<()> {
	// Optional: Use moq_native to make a QUIC client.
	let client = moq_native::ClientConfig::default().init()?;

	// For local development, use: http://localhost:4443
	// The "anon" path is usually configured to bypass authentication; be careful!
	let url = url::Url::parse("https://cdn.moq.dev/anon/chat-example").unwrap();

	// Establish a WebTransport/QUIC connection and MoQ handshake.
	let cs = client.with_publisher(&origin).connect(url).await?;

	// Wait until the session is closed.
	Err(cs.closed().await.into())
}

// Produce a broadcast and publish it to the origin.
async fn run_broadcast(origin: moq_net::origin::Producer) -> anyhow::Result<()> {
	// Create a broadcast on the origin. A broadcast is a collection of tracks,
	// but in this example we'll only create one. The live route announces it.
	// NOTE: The path is empty because we're using the URL to scope the broadcast.
	// If you put "alice" here, it would be published as "anon/chat-example/alice".
	let mut broadcast = origin
		.create_broadcast("", moq_net::broadcast::Route::new().with_live(true))
		.context("failed to create broadcast")?;

	// Create a track that we'll insert into the broadcast.
	// A track is a series of groups representing a live stream.
	let mut track = broadcast.create_track("chat", None)?;

	// Create a group.
	// Each group is independent and the newest group(s) will be prioritized.
	let mut group = track.append_group()?;

	// Write frames to the group.
	// Each frame is dependent on the previous frame, so older frames are prioritized.
	group.write_frame(
		moq_native::moq_net::Timestamp::now(),
		bytes::Bytes::from_static(b"Hello"),
	)?;
	group.write_frame(
		moq_native::moq_net::Timestamp::now(),
		bytes::Bytes::from_static(b"World"),
	)?;
	group.finish()?;

	tracing::info!("wrote hello + world");

	// Sleep before sending our next message.
	tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;

	// There's also a helper method to create a group with a single frame.
	track.write_frame(
		moq_native::moq_net::Timestamp::now(),
		bytes::Bytes::from_static(b"foobarbaz"),
	)?;
	tracing::info!("wrote foobarbaz");

	// Sleep before exiting and closing the broadcast.
	tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;

	// Cleanly close the broadcast so subscribers see a normal end. Dropping the
	// producer works too, but closing is explicit (and dropping it by accident
	// while still publishing is a common bug this makes obvious).
	broadcast.finish();

	Ok(())
}
