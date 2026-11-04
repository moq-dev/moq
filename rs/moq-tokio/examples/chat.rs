// cargo run --example chat

use anyhow::Context;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	// Optional: Use moq_tokio to configure a logger.
	moq_tokio::Log::new(tracing::Level::DEBUG).init()?;

	// Create an origin that we can publish to and the session can consume from.
	let origin = moq_tokio::origin::spawn();

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
	// Optional: Use moq_tokio to make a QUIC client.
	let client = moq_tokio::connect::Config::default().init(Default::default())?;

	// For local development, use: http://localhost:4443
	// The "anon" path is usually configured to bypass authentication; be careful!
	let url = url::Url::parse("https://cdn.moq.dev/anon/chat-example").unwrap();

	// Establish a WebTransport/QUIC connection and MoQ handshake.
	// The connection automatically redials with backoff if it drops.
	let connection = client.with_publisher(&origin).connect(url);

	// Wait until the connection gives up for good.
	connection.closed().await?;
	Ok(())
}

// Produce a broadcast and publish it to the origin.
async fn run_broadcast(origin: moq_net::origin::Producer) -> anyhow::Result<()> {
	// Create a route on the origin, which announces a new broadcast. A broadcast is a
	// collection of tracks, but in this example we'll only create one.
	// NOTE: The path is empty because we're using the URL to scope the broadcast.
	// If you put "alice" here, it would be published as "anon/chat-example/alice".
	// Dropping the route withdraws the broadcast, so keep it for as long as it should be live.
	let route = origin
		.route("", moq_net::origin::RouteOptions::default())
		.context("failed to announce broadcast")?;

	// Create a track that we'll insert into the broadcast.
	// A track is a series of groups representing a live stream.
	// Frames are stamped with the wall clock, so the track declares a timescale.
	let info = moq_tokio::moq_net::track::Info::default().with_timescale(moq_tokio::moq_net::Timescale::MILLI);
	let mut track = route.broadcast().create_track("chat", info)?;

	// Create a group.
	// Each group is independent and the newest group(s) will be prioritized.
	let mut group = track.append_group()?;

	// Write frames to the group.
	// Each frame is dependent on the previous frame, so older frames are prioritized.
	group.write_frame(
		moq_tokio::moq_net::Timestamp::now(),
		bytes::Bytes::from_static(b"Hello"),
	)?;
	group.write_frame(
		moq_tokio::moq_net::Timestamp::now(),
		bytes::Bytes::from_static(b"World"),
	)?;
	group.finish()?;

	tracing::info!("wrote hello + world");

	// Sleep before sending our next message.
	kio::clock::sleep(tokio::time::Duration::from_secs(10)).await;

	// There's also a helper method to create a group with a single frame.
	track.write_frame(
		moq_tokio::moq_net::Timestamp::now(),
		bytes::Bytes::from_static(b"foobarbaz"),
	)?;
	tracing::info!("wrote foobarbaz");

	// Sleep before exiting and closing the broadcast.
	kio::clock::sleep(tokio::time::Duration::from_secs(10)).await;

	// End the broadcast. Dropping the route withdraws it; closing ends it for good.
	route.broadcast().close();

	Ok(())
}
