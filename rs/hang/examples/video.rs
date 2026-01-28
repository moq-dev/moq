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
fn create_track(broadcast: &mut moq_lite::BroadcastProducer) -> hang::TrackProducer {
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
		min_buffer: None,
	};

	// Create a map of video renditions
	// Multiple renditions allow the viewer to choose based on their capabilities
	let mut renditions = std::collections::BTreeMap::new();
	renditions.insert(video_track.name.clone(), video_config);

	// Create the video catalog entry with the renditions
	let video = hang::catalog::Video {
		renditions,
		priority: 1,
		display: None,
		rotation: None,
		flip: None,
	};

	// Create a producer/consumer pair for the catalog.
	// This JSON encodes the catalog as a "catalog.json" track.
	let catalog = hang::catalog::Catalog {
		video: Some(video),
		..Default::default()
	}
	.produce();

	// Publish the catalog track to the broadcast.
	broadcast.insert_track(catalog.track.clone());

	// Actually create the media track now.
	let track = broadcast.create_track(video_track);

	// Wrap the track in a hang:TrackProducer for convenience methods.
	track.into()
}

// Produce a broadcast and publish it to the origin.
async fn run_broadcast(origin: moq_lite::OriginProducer) -> anyhow::Result<()> {
	// Create and publish a broadcast to the origin.
	let mut broadcast = moq_lite::Broadcast::produce();
	let mut track = create_track(&mut broadcast);

	// NOTE: The path is empty because we're using the URL to scope the broadcast.
	// OPTIONAL: We publish after inserting the tracks just to avoid a nearly impossible race condition.
	origin.publish_broadcast("", broadcast.consume());

	// Not real frames of course.
	track.write(hang::Frame {
		keyframe: true,
		timestamp: hang::Timestamp::from_secs(1).unwrap(),
		payload: Bytes::from_static(b"keyframe NAL data").into(),
	})?;

	tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

	track.write(hang::Frame {
		keyframe: false,
		timestamp: hang::Timestamp::from_secs(2).unwrap(),
		payload: Bytes::from_static(b"delta NAL data").into(),
	})?;

	tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

	// Automatically creates a new group if you write a new keyframe.

	track.write(hang::Frame {
		keyframe: true,
		timestamp: hang::Timestamp::from_secs(3).unwrap(),
		payload: Bytes::from_static(b"keyframe NAL data").into(),
	})?;

	// Sleep before exiting and closing the broadcast.
	tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;

	Ok(())
}
