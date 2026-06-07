// cargo run --example video
use anyhow::Context;
use bytes::Bytes;

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
// Automatically reconnects if the connection drops.
async fn run_session(origin: moq_net::OriginProducer) -> anyhow::Result<()> {
	// Optional: Use moq_native to make a QUIC client.
	let client = moq_native::ClientConfig::default().init()?;

	// For local development, use: http://localhost:4443/anon
	// The "anon" path is usually configured to bypass authentication; be careful!
	let url = url::Url::parse("https://cdn.moq.dev/anon/video-example").unwrap();

	// Establish a connection with automatic reconnection.
	// with_publisher() registers an OriginProducer. moq-net reads from its
	// consumer view internally. Pair with with_consumer() if you also want
	// to subscribe to remote announcements.
	let reconnect = client.with_publisher(origin).reconnect(url);

	// Wait until the reconnect loop stops (e.g. timeout exceeded).
	reconnect.closed().await
}

// Create a video track with a catalog that describes it.
// The catalog can contain multiple tracks, used by the viewer to choose the best track.
fn create_track(broadcast: &mut moq_net::BroadcastProducer) -> anyhow::Result<moq_net::TrackProducer> {
	// Basic information about the video track.
	let video_track = "video";

	// Example video configuration
	// In a real application, you would get this from the encoder
	let mut video_config = hang::catalog::VideoConfig::new(hang::catalog::H264 {
		profile: 0x4D, // Main profile
		constraints: 0,
		level: 0x28,  // Level 4.0
		inline: true, // SPS/PPS inline in bitstream (avc3)
	});
	video_config.coded_width = Some(1920);
	video_config.coded_height = Some(1080);
	video_config.bitrate = Some(5_000_000); // 5 Mbps
	video_config.framerate = Some(30.0);
	video_config.container = hang::catalog::Container::Legacy;

	// Create a map of video renditions
	// Multiple renditions allow the viewer to choose based on their capabilities
	let mut renditions = std::collections::BTreeMap::new();
	renditions.insert(video_track.to_string(), video_config);

	// Create the catalog describing our video track.
	let catalog = hang::catalog::Catalog {
		video: hang::catalog::Video {
			renditions,
			display: None,
			rotation: None,
			flip: None,
		},
		..Default::default()
	};

	// Publish the catalog as a "catalog.json" track in the broadcast.
	let mut catalog_track = broadcast.create_track(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info())?;
	let mut group = catalog_track.append_group()?;
	group.write_frame(catalog.to_string()?)?;
	group.finish()?;

	// Actually create the media track now.
	let track = broadcast.create_track(video_track, None)?;

	Ok(track)
}

// Produce a broadcast and publish it to the origin.
async fn run_broadcast(origin: moq_net::OriginProducer) -> anyhow::Result<()> {
	// Create and publish a broadcast to the origin.
	let mut broadcast = moq_net::BroadcastInfo::new().produce();
	let track = create_track(&mut broadcast)?;

	// NOTE: The path is empty because we're using the URL to scope the broadcast.
	// OPTIONAL: We publish after inserting the tracks just to avoid a nearly impossible race condition.
	let _publish = origin
		.publish_broadcast("", broadcast.consume())
		.context("failed to publish broadcast")?;

	// Wrap in a Producer for keyframe-based group management.
	let mut producer = moq_mux::container::Producer::new(track, moq_mux::catalog::hang::Container::Legacy);

	// Not real frames of course. The first frame is a keyframe and starts the first group.
	let frame = moq_mux::container::Frame {
		timestamp: moq_net::Timestamp::from_secs(1).unwrap(),
		payload: Bytes::from_static(b"keyframe NAL data"),
		keyframe: true,
		duration: None,
	};
	producer.write(frame)?;

	tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

	let frame = moq_mux::container::Frame {
		timestamp: moq_net::Timestamp::from_secs(2).unwrap(),
		payload: Bytes::from_static(b"delta NAL data"),
		keyframe: false,
		duration: None,
	};
	producer.write(frame)?;

	tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

	// Marking this frame as a keyframe closes the current group and starts a new one.
	let frame = moq_mux::container::Frame {
		timestamp: moq_net::Timestamp::from_secs(3).unwrap(),
		payload: Bytes::from_static(b"keyframe NAL data"),
		keyframe: true,
		duration: None,
	};
	producer.write(frame)?;

	// Sleep before exiting and closing the broadcast.
	tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;

	Ok(())
}
