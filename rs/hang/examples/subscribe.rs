// cargo run --example subscribe

use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	// Optional: Use moq_tokio to configure a logger.
	moq_tokio::Log::new(tracing::Level::DEBUG).init()?;

	// Create an origin that the session can publish incoming broadcasts to.
	let origin = moq_tokio::origin::spawn();
	let consumer = origin.consume();

	// Run the subscription and the session in parallel.
	tokio::select! {
		res = run_session(origin) => res,
		res = run_subscribe(consumer) => res,
	}
}

// Connect to the server and subscribe to broadcasts.
// Automatically reconnects if the connection drops.
async fn run_session(origin: moq_net::origin::Producer) -> anyhow::Result<()> {
	// Optional: Use moq_tokio to make a QUIC client.
	let client = moq_tokio::connect::Config::default().init(Default::default())?;

	// For local development, use: http://localhost:4443/video-example
	// The "anon" path is usually configured to bypass authentication; be careful!
	let url = url::Url::parse("https://cdn.moq.dev/anon/video-example").unwrap();

	// Establish a connection with automatic reconnection.
	// with_subscriber() registers an OriginProducer for incoming data.
	// Use with_publisher() if you also want to publish from the session.
	let reconnect = client.with_subscriber(origin).connect(url);

	// Wait until the reconnect loop stops (e.g. timeout exceeded).
	Ok(reconnect.closed().await?)
}

// Follow the announced broadcast: play it once it starts, play it afresh when another
// publisher instance replaces it (a restart), and stop when it ends.
async fn run_subscribe(consumer: moq_net::origin::Consumer) -> anyhow::Result<()> {
	let mut announced = consumer.announced();
	let mut playing: Option<tokio::task::JoinHandle<()>> = None;
	while let Some(event) = announced.next().await {
		match event {
			// The convention is that a publisher announces each broadcast's exact path.
			moq_net::announce::Event::Start(announce) | moq_net::announce::Event::Restart(announce) => {
				tracing::info!(path = %announce.prefix, "broadcast announced");
				if let Some(old) = playing.take() {
					old.abort();
				}
				let consumer = consumer.clone();
				playing = Some(tokio::spawn(async move {
					if let Err(err) = play(consumer, announce).await {
						tracing::warn!(%err, "playback failed");
					}
				}));
			}
			// The same broadcast over another route: nothing to do.
			moq_net::announce::Event::Update(_) => {}
			moq_net::announce::Event::End(announce) => {
				tracing::info!(path = %announce.prefix, "broadcast ended");
				if let Some(old) = playing.take() {
					old.abort();
				}
			}
		}
	}
	anyhow::bail!("origin closed")
}

// Resolve an announced broadcast and read media frames.
async fn play(consumer: moq_net::origin::Consumer, announce: moq_net::announce::Announce) -> anyhow::Result<()> {
	let broadcast = consumer
		.request_broadcast(&announce.prefix, announce.route.epoch)
		.await?;

	// Read the catalog to discover available tracks.
	let catalog_track = broadcast
		.track(hang::Catalog::DEFAULT_NAME)?
		.subscribe(hang::Catalog::default_subscription())
		.await?;
	let mut catalog = moq_mux::catalog::hang::Consumer::<()>::new(catalog_track);

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
	let latency = Duration::from_millis(500);
	let track_consumer = broadcast
		.track(name)?
		.subscribe(
			moq_net::track::Subscription::default()
				.with_priority(1)
				.with_max_delay(latency),
		)
		.await?;
	let mut ordered =
		moq_mux::container::Consumer::new(track_consumer, moq_mux::catalog::hang::Container::try_from(config)?);

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
