// cargo run --example video
use moq_lite::coding::Bytes;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	// Optional: Use moq_native to configure a logger.
	moq_native::Log {
		level: tracing::Level::DEBUG,
	}
	.init();

	// Create an origin that we can publish to and the session can consume from.
	let origin = moq_lite::Origin::produce();

	// Run the broadcast production and the session in parallel.
	// This is a simple example of how you can concurrently run multiple tasks.
	// tokio::spawn works too.
	tokio::select! {
		res = run_broadcast(origin.producer) => res,
		res = run_session(origin.consumer) => res,
	}
}

// Connect to the server and publish our origin of broadcasts.
async fn run_session(origin: moq_lite::OriginConsumer) -> anyhow::Result<()> {
	// Optional: Use moq_native to make a QUIC client.
	let config = moq_native::ClientConfig::default();
	let client = moq_native::Client::new(config)?;

	// For local development, use: http://localhost:4443/anon
	// The "anon" path is usually configured to bypass authentication; be careful!
	let url = url::Url::parse("https://cdn.moq.dev/anon/video-example").unwrap();

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
	// Create and publish a broadcast to the origin.
	let mut broadcast = moq_lite::Broadcast::produce();

	// Create the track and get a producer handle.
	let mut track = create_track(&mut broadcast.producer);

	// NOTE: The path is empty because we're using the URL to scope the broadcast.
	// OPTIONAL: We publish after inserting the tracks just to avoid a nearly impossible race condition.
	origin.publish_broadcast("", broadcast.consumer);

	// Create a group of frames.
	// Each group must start with a keyframe.
	let mut group = track.append_group()?;

	// Encode a simple container that consists of a timestamp and a payload.
	// NOTE: This will be removed in the future; it's for backwards compatibility.
	hang::Container {
		timestamp: moq_lite::Time::from_secs(1).unwrap(),
		payload: Bytes::from_static(b"keyframe NAL data").into(),
	}
	.encode(&mut group)?;

	tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

	hang::Container {
		timestamp: moq_lite::Time::from_secs(2).unwrap(),
		payload: Bytes::from_static(b"delta NAL data").into(),
	}
	.encode(&mut group)?;

	tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

	// You can create a new group for each keyframe.
	group.close()?;
	let mut group = track.append_group()?;

	hang::Container {
		timestamp: moq_lite::Time::from_secs(3).unwrap(),
		payload: Bytes::from_static(b"keyframe NAL data").into(),
	}
	.encode(&mut group)?;

	tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

	// You can also abort a group if you want to abandon delivery immediately.
	group.abort(moq_lite::Error::Expired)?;

	Ok(())
}

// Create a video track with a catalog that describes it.
//
// The catalog can contain multiple tracks, used by the viewer to choose the best track.
fn create_track(broadcast: &mut moq_lite::BroadcastProducer) -> moq_lite::TrackProducer {
	// We also need a catalog to describe our tracks.
	// NOTE: You would reuse this for all tracks; we're creating a new one here for simplicity.
	let mut catalog = hang::CatalogProducer::new(broadcast.clone());

	// Once we unlock (drop) the catalog, it will be published to the broadcast.
	let mut catalog = catalog.lock();

	// Example video configuration
	// In a real application, you would get this from the encoder
	let config = hang::VideoConfig {
		codec: hang::H264 {
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
	};

	// This is a helper that creates a unique track name and adds it to the catalog.
	// You can also set `catalog.video` fields directly.
	let track = catalog.video.create("example", config);

	// We also need some details on how to deliver the track over the network.
	let delivery = moq_lite::Delivery {
		// Video typically has lower priority than audio; we'll try to transmit it later
		priority: 1,
		// You can configure the amount of time to keep old groups in cache.
		max_latency: moq_lite::Time::from_secs(10).unwrap(),
		// You can even tell the CDN if it should try to delver in group order.
		ordered: false,
	};

	// Actually create the media track now and return it
	broadcast.create_track(track, delivery)
}
