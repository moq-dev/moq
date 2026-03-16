//! Integration tests for moq-ffi UniFFI objects.
//!
//! Tests the full publish/consume pipeline using the UniFFI API,
//! exercising local origin-based pub/sub without requiring a network connection.

use std::time::Duration;

use moq_ffi::origin::*;
use moq_ffi::producer::*;
use moq_ffi::session::MoqClient;

const TIMEOUT: Duration = Duration::from_secs(10);

/// Build a valid OpusHead init buffer (RFC 7845 §5.1).
fn opus_head() -> Vec<u8> {
	let mut head = Vec::with_capacity(19);
	head.extend_from_slice(b"OpusHead");
	head.push(1); // version
	head.push(2); // channel count (stereo)
	head.extend_from_slice(&0u16.to_le_bytes()); // pre-skip
	head.extend_from_slice(&48000u32.to_le_bytes()); // sample rate
	head.extend_from_slice(&0u16.to_le_bytes()); // output gain
	head.push(0); // channel mapping family
	head
}

/// H.264 Annex B init with SPS + PPS extracted from Big Buck Bunny (1280x720, High profile, Level 3.1).
fn h264_init() -> Vec<u8> {
	let mut init = Vec::new();
	// SPS NAL unit (from bbb.mp4 avcC)
	init.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // start code
	init.extend_from_slice(&[
		0x67, 0x64, 0x00, 0x1f, 0xac, 0x24, 0x84, 0x01, 0x40, 0x16, 0xec, 0x04, 0x40, 0x00, 0x00, 0x03, 0x00, 0x40,
		0x00, 0x00, 0x0c, 0x23, 0xc6, 0x0c, 0x92,
	]);
	// PPS NAL unit (from bbb.mp4 avcC)
	init.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]); // start code
	init.extend_from_slice(&[0x68, 0xee, 0x32, 0xc8, 0xb0]);
	init
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn origin_lifecycle() {
	let origin = MoqOriginProducer::new();
	let _consumer = origin.consume();
	// Origin and consumer live until dropped — no explicit close needed.
}

#[test]
fn publish_media_lifecycle() {
	let broadcast = MoqBroadcastProducer::new().unwrap();

	// Create an opus media track.
	let init = opus_head();
	let media = broadcast.publish_media("opus".into(), init).unwrap();

	// Write a frame.
	media.write_frame(b"opus frame".to_vec(), 1000).unwrap();

	// Finish media, then broadcast.
	media.finish().unwrap();
	broadcast.finish().unwrap();
}

#[test]
fn unknown_format() {
	let broadcast = MoqBroadcastProducer::new().unwrap();

	let err = broadcast
		.publish_media("nope".into(), vec![])
		.err()
		.expect("unknown format should fail");
	assert!(
		matches!(err, moq_ffi::error::MoqError::Codec(_)),
		"expected Codec error, got {err}"
	);
}

#[tokio::test]
async fn local_publish_consume_audio() {
	// ── publisher ──────────────────────────────────────────────────
	let origin = MoqOriginProducer::new();
	let broadcast = MoqBroadcastProducer::new().unwrap();

	let init = opus_head();
	let media = broadcast.publish_media("opus".into(), init).unwrap();

	origin.publish("live".into(), &broadcast).unwrap();

	// ── consumer ───────────────────────────────────────────────────
	let consumer = origin.consume();
	let announced = consumer.announced("".into()).unwrap();

	let announcement = tokio::time::timeout(TIMEOUT, announced.next())
		.await
		.expect("timed out waiting for announcement")
		.unwrap()
		.expect("expected an announcement");

	assert_eq!(announcement.path(), "live");

	let broadcast_consumer = announcement.broadcast();
	let catalog_consumer = broadcast_consumer.subscribe_catalog().unwrap();

	let catalog = tokio::time::timeout(TIMEOUT, catalog_consumer.next())
		.await
		.expect("timed out waiting for catalog")
		.unwrap()
		.expect("expected a catalog");

	// Verify audio config.
	assert_eq!(catalog.audio.len(), 1);
	let (track_name, audio) = catalog.audio.iter().next().unwrap();
	assert_eq!(audio.codec, "opus");
	assert_eq!(audio.sample_rate, 48000);
	assert_eq!(audio.channel_count, 2);

	// No video tracks in this broadcast.
	assert!(catalog.video.is_empty());

	// Subscribe to the audio track.
	let media_consumer = broadcast_consumer.subscribe_media(track_name.clone(), 10_000).unwrap();

	// Write a frame after subscribing so the consumer definitely sees it.
	let payload = b"opus audio payload data".to_vec();
	media.write_frame(payload.clone(), 1_000_000).unwrap();

	let frame = tokio::time::timeout(TIMEOUT, media_consumer.next())
		.await
		.expect("timed out waiting for frame")
		.unwrap()
		.expect("expected a frame");

	assert_eq!(frame.payload, payload);
	assert_eq!(frame.timestamp_us, 1_000_000);
}

#[tokio::test]
async fn video_publish_consume() {
	let origin = MoqOriginProducer::new();
	let broadcast = MoqBroadcastProducer::new().unwrap();

	let init = h264_init();
	let media = broadcast.publish_media("avc3".into(), init).unwrap();

	origin.publish("video-test".into(), &broadcast).unwrap();

	let consumer = origin.consume();
	let announced = consumer.announced("".into()).unwrap();

	let announcement = tokio::time::timeout(TIMEOUT, announced.next())
		.await
		.expect("timed out")
		.unwrap()
		.expect("expected announcement");

	let broadcast_consumer = announcement.broadcast();
	let catalog_consumer = broadcast_consumer.subscribe_catalog().unwrap();

	let catalog = tokio::time::timeout(TIMEOUT, catalog_consumer.next())
		.await
		.expect("timed out")
		.unwrap()
		.expect("expected catalog");

	// Verify video config.
	assert_eq!(catalog.video.len(), 1);
	let (track_name, video) = catalog.video.iter().next().unwrap();
	assert!(
		video.codec.starts_with("avc1.") || video.codec.starts_with("avc3."),
		"codec should be avc1/avc3, got {}",
		video.codec
	);
	let coded = video.coded.as_ref().expect("coded dimensions should be set");
	assert_eq!(coded.width, 1280);
	assert_eq!(coded.height, 720);

	// No audio tracks.
	assert!(catalog.audio.is_empty());

	// Subscribe and publish a keyframe.
	let media_consumer = broadcast_consumer.subscribe_media(track_name.clone(), 10_000).unwrap();

	// IDR keyframe (Annex B with start code).
	let keyframe = vec![0x00, 0x00, 0x00, 0x01, 0x65, 0xAA, 0xBB, 0xCC];
	media.write_frame(keyframe, 0).unwrap();

	let frame = tokio::time::timeout(TIMEOUT, media_consumer.next())
		.await
		.expect("timed out")
		.unwrap()
		.expect("expected frame");

	assert_eq!(frame.timestamp_us, 0);
	assert!(!frame.payload.is_empty(), "frame should have payload data");
}

#[tokio::test]
async fn multiple_frames_ordering() {
	let origin = MoqOriginProducer::new();
	let broadcast = MoqBroadcastProducer::new().unwrap();

	let init = opus_head();
	let media = broadcast.publish_media("opus".into(), init).unwrap();

	origin.publish("ordering-test".into(), &broadcast).unwrap();

	let consumer = origin.consume();
	let announced = consumer.announced("".into()).unwrap();
	let announcement = tokio::time::timeout(TIMEOUT, announced.next())
		.await
		.unwrap()
		.unwrap()
		.unwrap();

	let broadcast_consumer = announcement.broadcast();
	let catalog_consumer = broadcast_consumer.subscribe_catalog().unwrap();
	let catalog = tokio::time::timeout(TIMEOUT, catalog_consumer.next())
		.await
		.unwrap()
		.unwrap()
		.unwrap();

	let track_name = catalog.audio.keys().next().unwrap().clone();
	let media_consumer = broadcast_consumer.subscribe_media(track_name, 10_000).unwrap();

	// Publish 5 frames with increasing timestamps.
	let timestamps: [u64; 5] = [0, 20_000, 40_000, 60_000, 80_000];
	for (i, &ts) in timestamps.iter().enumerate() {
		let payload = format!("frame-{i}");
		media.write_frame(payload.into_bytes(), ts).unwrap();
	}

	// Verify frames arrive in order with correct timestamps.
	for (i, &expected_ts) in timestamps.iter().enumerate() {
		let frame = tokio::time::timeout(TIMEOUT, media_consumer.next())
			.await
			.unwrap_or_else(|_| panic!("timed out waiting for frame {i}"))
			.unwrap()
			.unwrap_or_else(|| panic!("expected frame {i}"));

		assert_eq!(frame.timestamp_us, expected_ts, "frame {i} has wrong timestamp");

		let expected = format!("frame-{i}");
		assert_eq!(frame.payload, expected.as_bytes(), "frame {i} has wrong payload");
	}
}

#[tokio::test]
async fn catalog_update_on_new_track() {
	let origin = MoqOriginProducer::new();
	let broadcast = MoqBroadcastProducer::new().unwrap();

	// Create first audio track.
	let init = opus_head();
	let _media1 = broadcast.publish_media("opus".into(), init.clone()).unwrap();

	origin.publish("catalog-update".into(), &broadcast).unwrap();

	let consumer = origin.consume();
	let announced = consumer.announced("".into()).unwrap();
	let announcement = tokio::time::timeout(TIMEOUT, announced.next())
		.await
		.unwrap()
		.unwrap()
		.unwrap();

	let broadcast_consumer = announcement.broadcast();
	let catalog_consumer = broadcast_consumer.subscribe_catalog().unwrap();

	// First catalog: 1 audio track.
	let catalog1 = tokio::time::timeout(TIMEOUT, catalog_consumer.next())
		.await
		.unwrap()
		.unwrap()
		.unwrap();
	assert_eq!(catalog1.audio.len(), 1);

	// Add a second audio track — should trigger a catalog update.
	let _media2 = broadcast.publish_media("opus".into(), init).unwrap();

	// Wait for the updated catalog.
	let catalog2 = tokio::time::timeout(TIMEOUT, catalog_consumer.next())
		.await
		.unwrap()
		.unwrap()
		.unwrap();
	assert_eq!(catalog2.audio.len(), 2);
}

#[test]
fn finish_closes_producer() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let init = opus_head();
	let _media = broadcast.publish_media("opus".into(), init).unwrap();

	broadcast.finish().unwrap();

	// Finishing again should return Closed.
	let err = broadcast.finish().unwrap_err();
	assert!(
		matches!(err, moq_ffi::error::MoqError::Closed),
		"expected Closed error, got {err}"
	);
}

#[tokio::test]
async fn announced_broadcast() {
	let origin = MoqOriginProducer::new();
	let broadcast = MoqBroadcastProducer::new().unwrap();

	origin.publish("test/broadcast".into(), &broadcast).unwrap();

	let consumer = origin.consume();
	let announced = consumer.announced("".into()).unwrap();

	let announcement = tokio::time::timeout(TIMEOUT, announced.next())
		.await
		.expect("timed out")
		.unwrap()
		.expect("expected announcement");

	assert_eq!(announcement.path(), "test/broadcast");

	// The broadcast consumer should be usable.
	let _catalog = announcement.broadcast().subscribe_catalog().unwrap();
}

/// Verify FFI objects work on a thread with no tokio runtime.
///
/// If any FFI method (including Drop) forgets to enter the RUNTIME,
/// the thread panics and `.join().unwrap()` catches it.
#[test]
fn without_runtime() {
	// Use a plain thread — no tokio runtime anywhere.
	std::thread::spawn(|| {
		// Create and use FFI origin objects.
		let origin = MoqOriginProducer::new();
		let consumer = origin.consume();

		// Publish a broadcast via FFI.
		let broadcast = MoqBroadcastProducer::new().unwrap();
		let init = opus_head();
		let media = broadcast.publish_media("opus".into(), init).unwrap();
		media.write_frame(b"hello".to_vec(), 1000).unwrap();
		origin.publish("test".into(), &broadcast).unwrap();

		// Subscribe to announcements via FFI.
		let announced = consumer.announced("".into()).unwrap();
		let announcement = pollster::block_on(announced.next()).unwrap().unwrap();
		assert_eq!(announcement.path(), "test");
		let _bc = announcement.broadcast();

		// Create a client (but don't connect — that needs a server).
		let client = MoqClient::new();
		client.set_tls_disable_verify(true);
		client.set_consume(Some(origin));

		// Cancel and drop everything — no runtime on this thread.
		announced.cancel();
		client.cancel();
		media.finish().unwrap();
		broadcast.finish().unwrap();
		drop(client);
		drop(consumer);
		drop(announcement);
		drop(announced);
	})
	.join()
	.expect("client thread panicked — FFI method missing runtime guard");
}
