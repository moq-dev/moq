// cargo run --example video
use moq_lite::coding::Bytes;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	// Optional: Use moq_native to configure a logger.
	moq_native::Log::new(tracing::Level::DEBUG).init();

	// Create an origin that we can publish to and the session can consume from.
	let origin = moq_lite::Origin::produce();

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
	let url = url::Url::parse("https://cdn.moq.dev/anon/video-example").unwrap();

	// Establish a WebTransport/QUIC connection and MoQ handshake for publishing.
	// with_publish() registers an OriginConsumer for outgoing data.
	// Use with_consume() if you also want to subscribe/consume from the session.
	let session = client.with_publish(origin).connect(url).await?;

	// Wait until the session is closed.
	session.closed().await.map_err(Into::into)
}

// Create a video track with a catalog that describes it.
// The catalog can contain multiple tracks, used by the viewer to choose the best track.
fn create_track(broadcast: &mut moq_lite::BroadcastProducer) -> moq_lite::TrackProducer {
	// Basic information about the video track.
	let video_track = moq_lite::Track {
		name: "video".to_string(),
		priority: 1, // Video typically has lower priority than audio
	};

	// Example video configuration
	// In a real application, you would get this from the encoder
	let video_config = hang::catalog::VideoConfig {
		codec: hang::catalog::H264 {
			profile: 0x4D, // Main profile
			constraints: 0,
			level: 0x28,  // Level 4.0
			inline: true, // SPS/PPS inline in bitstream (avc3)
		}
		.into(),
		// Codec-specific data (e.g., SPS/PPS for H.264)
		// Not needed if you're using annex.b (inline: true)
		description: None,
		// There are optional but good to have.
		coded_width: Some(1920),
		coded_height: Some(1080),
		bitrate: Some(5_000_000), // 5 Mbps
		framerate: Some(30.0),
		display_ratio_width: None,
		display_ratio_height: None,
		optimize_for_latency: None,
		container: hang::catalog::Container::Legacy,
		jitter: None,
	};

	// Create a map of video renditions
	// Multiple renditions allow the viewer to choose based on their capabilities
	let mut renditions = std::collections::BTreeMap::new();
	renditions.insert(video_track.name.clone(), video_config);

	// Create a producer/consumer pair for the catalog.
	// This JSON encodes the catalog as a "catalog.json" track.
	let catalog = hang::catalog::Catalog {
		video: hang::catalog::Video {
			renditions,
			display: None,
			rotation: None,
			flip: None,
		},
		..Default::default()
	}
	.produce();

	// Publish the catalog track to the broadcast.
	broadcast.insert_track(catalog.track.clone());

	// Actually create the media track now.
	broadcast.create_track(video_track)
}

// Produce a broadcast and publish it to the origin.
async fn run_broadcast(origin: moq_lite::OriginProducer) -> anyhow::Result<()> {
	// Create and publish a broadcast to the origin.
	let mut broadcast = moq_lite::Broadcast::produce();
	let mut track = create_track(&mut broadcast);

	// NOTE: The path is empty because we're using the URL to scope the broadcast.
	// OPTIONAL: We publish after inserting the tracks just to avoid a nearly impossible race condition.
	origin.publish_broadcast("", broadcast.consume());

	// Create a new group.
	let mut group = track.append_group();

	// Not real frames of course.
	let frame = hang::container::Frame {
		timestamp: hang::container::Timestamp::from_secs(1).unwrap(),
		keyframe: true,
		payload: Bytes::from_static(b"keyframe NAL data").into(),
	};
	frame.encode(&mut group)?;

	tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

	let frame = hang::container::Frame {
		timestamp: hang::container::Timestamp::from_secs(2).unwrap(),
		keyframe: false,
		payload: Bytes::from_static(b"delta NAL data").into(),
	};
	frame.encode(&mut group)?;
	group.close();

	tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

	// Create a new group for each keyframe.
	let mut group = track.append_group();
	let frame = hang::container::Frame {
		timestamp: hang::container::Timestamp::from_secs(3).unwrap(),
		keyframe: true,
		payload: Bytes::from_static(b"keyframe NAL data").into(),
	};
	frame.encode(&mut group)?;

	// Sleep before exiting and closing the broadcast.
	tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;

	group.close();

	Ok(())
}
