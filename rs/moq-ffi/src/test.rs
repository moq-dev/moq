use super::origin::*;
use super::producer::*;
use super::server::MoqServer;
use super::session::MoqClient;
use crate::consumer::MoqFetchGroupOptions;
use crate::consumer::MoqSubscription;
use crate::error::MoqError;
use crate::json::{MoqJsonConfig, MoqJsonStreamConfig};
use crate::media::MoqInit;

use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);

/// A bare [`MoqInit`] with a format and init bytes, no catalog hints.
fn media_init(format: &str, data: Vec<u8>) -> MoqInit {
	MoqInit {
		format: format.to_string(),
		data,
		video: None,
	}
}

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

#[test]
fn origin_lifecycle() {
	let origin = MoqOriginProducer::new(MoqOriginOptions::default());
	let _consumer = origin.consume();
}

#[test]
fn origin_options_set_cache_capacity() {
	let origin = MoqOriginProducer::new(MoqOriginOptions {
		cache_capacity_bytes: Some(4096),
	});
	assert_eq!(origin.inner().info().pool.capacity(), Some(4096));
}

#[test]
fn publish_media_lifecycle() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let init = opus_head();
	let media = broadcast.publish_media(media_init("opus", init)).unwrap();
	media.write_frame(b"opus frame".to_vec(), 1000).unwrap();
	media.finish().unwrap();
	broadcast.finish().unwrap();
}

#[tokio::test]
async fn raw_track_activity() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("status".into(), None).unwrap();
	assert_eq!(track.name().unwrap(), "status");

	let consumer = track.consume(None).unwrap();
	tokio::time::timeout(TIMEOUT, track.used())
		.await
		.expect("timed out waiting for raw track to become used")
		.unwrap();

	drop(consumer);
	tokio::time::timeout(TIMEOUT, track.unused())
		.await
		.expect("timed out waiting for raw track to become unused")
		.unwrap();
}

#[tokio::test]
async fn raw_track_datagram_roundtrip() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast
		.publish_track(
			"events".into(),
			Some(MoqTrackInfo {
				priority: 0,
				ordered: true,
				cache_ms: None,
				timescale: Some(1_000_000),
			}),
		)
		.unwrap();
	let consumer = track.consume(None).unwrap();
	let payload = b"hello datagram".to_vec();

	let sequence = track.append_datagram(123_456, payload.clone()).unwrap();
	let datagram = tokio::time::timeout(TIMEOUT, consumer.recv_datagram())
		.await
		.expect("timed out waiting for datagram")
		.unwrap()
		.expect("expected a datagram");

	assert_eq!(datagram.sequence, sequence);
	assert_eq!(datagram.timestamp_us, 123_456);
	assert_eq!(datagram.payload, payload);
}

#[tokio::test]
async fn raw_track_info_reports_publisher_properties() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let info = MoqTrackInfo {
		priority: 7,
		ordered: false,
		cache_ms: Some(2_500),
		timescale: Some(90_000),
	};
	let track = broadcast.publish_track("status".into(), Some(info)).unwrap();
	let consumer = track.consume(None).unwrap();

	let got = consumer.info().await.unwrap();
	assert_eq!(got.priority, 7);
	assert!(!got.ordered);
	assert_eq!(got.cache_ms, Some(2_500));
	assert_eq!(got.timescale, Some(90_000));
}

#[tokio::test]
async fn raw_track_info_defaults_to_unordered() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("status".into(), None).unwrap();
	let consumer = track.consume(None).unwrap();

	assert!(!consumer.info().await.unwrap().ordered);
}

#[tokio::test]
async fn raw_track_update_does_not_wait_for_pending_read() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("status".into(), None).unwrap();
	let consumer = track.consume(None).unwrap();

	let read = {
		let consumer = consumer.clone();
		tokio::spawn(async move { consumer.read_frame().await })
	};

	consumer.update(MoqSubscription {
		priority: 10,
		ordered: false,
		stale_ms: 25,
		group_start: Some(0),
		group_end: None,
	});

	let payload = b"updated subscription".to_vec();
	track.write_frame(payload.clone(), 20_000).unwrap();

	let frame = tokio::time::timeout(TIMEOUT, read)
		.await
		.expect("timed out waiting for raw frame")
		.expect("read task panicked")
		.unwrap()
		.expect("expected a frame");
	assert_eq!(frame.payload, payload);
	assert_eq!(frame.timestamp_us, 20_000);
}

#[tokio::test]
async fn json_snapshot_roundtrip() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let config = MoqJsonConfig {
		delta_ratio: 8,
		compression: true,
	};
	let producer = broadcast.publish_json("meta".into(), config.clone()).unwrap();
	let consumer = broadcast
		.consume()
		.unwrap()
		.subscribe_json("meta".into(), config)
		.await
		.unwrap();

	producer.update(r#"{"a":1}"#.into()).unwrap();
	let value = tokio::time::timeout(TIMEOUT, consumer.next())
		.await
		.expect("timed out waiting for json snapshot")
		.unwrap()
		.expect("expected a value");
	assert_eq!(
		serde_json::from_str::<serde_json::Value>(&value).unwrap(),
		serde_json::json!({ "a": 1 })
	);

	// A second update supersedes the first; a late reader collapses to the latest.
	producer.update(r#"{"a":2}"#.into()).unwrap();
	let value = tokio::time::timeout(TIMEOUT, consumer.next())
		.await
		.expect("timed out waiting for json snapshot delta")
		.unwrap()
		.expect("expected a value");
	assert_eq!(
		serde_json::from_str::<serde_json::Value>(&value).unwrap(),
		serde_json::json!({ "a": 2 })
	);

	producer.finish().unwrap();
	assert!(matches!(producer.update(r#"{"a":3}"#.into()), Err(MoqError::Closed)));
}

#[tokio::test]
async fn json_stream_roundtrip() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let config = MoqJsonStreamConfig { compression: true };
	let producer = broadcast.publish_json_stream("events".into(), config.clone()).unwrap();
	let consumer = broadcast
		.consume()
		.unwrap()
		.subscribe_json_stream("events".into(), config)
		.await
		.unwrap();

	for n in 0..3 {
		producer.append(format!(r#"{{"n":{n}}}"#)).unwrap();
		let value = tokio::time::timeout(TIMEOUT, consumer.next())
			.await
			.expect("timed out waiting for json stream record")
			.unwrap()
			.expect("expected a record");
		assert_eq!(
			serde_json::from_str::<serde_json::Value>(&value).unwrap(),
			serde_json::json!({ "n": n })
		);
	}
	producer.finish().unwrap();
}

#[tokio::test]
async fn dynamic_track_request() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let dynamic = broadcast.dynamic().unwrap();
	let consumer = broadcast.consume().unwrap();

	// The subscribe stays pending until the request is accepted below, so run it on a
	// concurrent task.
	let subscribe = {
		let consumer = consumer.clone();
		tokio::spawn(async move { consumer.subscribe_track("events".into(), None).await })
	};

	let request = tokio::time::timeout(TIMEOUT, dynamic.requested_track())
		.await
		.expect("timed out waiting for requested track")
		.unwrap();
	assert_eq!(request.name().unwrap(), "events");

	// Accept the request as a raw track (which unblocks the subscribe), then write.
	let track = request.accept(None).unwrap();
	let payload = b"hello dynamic track".to_vec();
	track.write_frame(payload.clone(), 0).unwrap();

	let track_consumer = tokio::time::timeout(TIMEOUT, subscribe)
		.await
		.expect("timed out waiting for subscribe")
		.expect("subscribe task panicked")
		.unwrap();

	let frame = tokio::time::timeout(TIMEOUT, track_consumer.read_frame())
		.await
		.expect("timed out waiting for dynamic track frame")
		.unwrap()
		.expect("expected a frame");

	assert_eq!(frame.payload, payload);
	assert_eq!(frame.timestamp_us, 0);
	track.finish().unwrap();
}

#[tokio::test]
async fn raw_frame_timestamps() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("status".into(), None).unwrap();
	let consumer = track.consume(None).unwrap();

	let payload = b"ready".to_vec();
	track.write_frame(payload.clone(), 12_345).unwrap();

	let frame = tokio::time::timeout(TIMEOUT, consumer.read_frame())
		.await
		.expect("timed out waiting for raw track frame")
		.unwrap()
		.expect("expected a frame");
	assert_eq!(frame.payload, payload);
	assert_eq!(frame.timestamp_us, 12_345);
	assert!(!frame.keyframe);

	let group = track.append_group().unwrap();
	let group_consumer = group.consume().unwrap();
	let payload = b"group frame".to_vec();
	group.write_frame(payload.clone(), 23_456).unwrap();
	group.finish().unwrap();

	let frame = tokio::time::timeout(TIMEOUT, group_consumer.read_frame())
		.await
		.expect("timed out waiting for raw group frame")
		.unwrap()
		.expect("expected a frame");
	assert_eq!(frame.payload, payload);
	assert_eq!(frame.timestamp_us, 23_456);
	assert!(!frame.keyframe);

	track.finish().unwrap();
}

#[tokio::test]
async fn dynamic_track_request_can_abort() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let dynamic = broadcast.dynamic().unwrap();
	let consumer = broadcast.consume().unwrap();

	// The subscribe stays pending until the request is resolved; aborting an
	// unaccepted request rejects it, so the subscribe fails instead of succeeding.
	let subscribe = {
		let consumer = consumer.clone();
		tokio::spawn(async move { consumer.subscribe_track("unknown".into(), None).await })
	};

	let track = tokio::time::timeout(TIMEOUT, dynamic.requested_track())
		.await
		.expect("timed out waiting for requested track")
		.unwrap();

	track.abort(404).unwrap();
	assert!(matches!(track.name(), Err(MoqError::Closed)));

	let result = tokio::time::timeout(TIMEOUT, subscribe)
		.await
		.expect("timed out waiting for subscribe")
		.expect("subscribe task panicked");
	assert!(result.is_err(), "subscribe to a rejected track should fail");
}

#[tokio::test]
async fn fetches_cached_group_without_subscribing() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("events".into(), None).unwrap();
	let group = track.append_group().unwrap();
	group.write_frame(b"first".to_vec(), 0).unwrap();
	group.write_frame(b"second".to_vec(), 20_000).unwrap();
	group.finish().unwrap();

	let consumer = broadcast.consume().unwrap();
	let fetched = consumer
		.fetch_group("events".into(), 0, Some(MoqFetchGroupOptions { priority: 7 }))
		.await
		.unwrap();

	assert_eq!(fetched.sequence(), 0);
	let frame = fetched.read_frame().await.unwrap().expect("expected first frame");
	assert_eq!(frame.payload, b"first".to_vec());
	assert_eq!(frame.timestamp_us, 0);
	let frame = fetched.read_frame().await.unwrap().expect("expected second frame");
	assert_eq!(frame.payload, b"second".to_vec());
	assert_eq!(frame.timestamp_us, 20_000);
	assert!(fetched.read_frame().await.unwrap().is_none());
}

#[tokio::test]
async fn dynamic_track_serves_fetch_miss_and_priority() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("events".into(), None).unwrap();
	let dynamic = track.dynamic().unwrap();
	let consumer = broadcast.consume().unwrap();

	let fetch = tokio::spawn(async move {
		consumer
			.fetch_group("events".into(), 5, Some(MoqFetchGroupOptions { priority: 11 }))
			.await
	});

	let request = tokio::time::timeout(TIMEOUT, dynamic.requested_group())
		.await
		.expect("timed out waiting for group request")
		.unwrap();
	assert_eq!(request.sequence(), 5);
	assert_eq!(request.priority(), 11);

	let group = request.accept().unwrap();
	group.write_frame(b"fetched".to_vec(), 100_000).unwrap();
	group.finish().unwrap();

	let fetched = tokio::time::timeout(TIMEOUT, fetch)
		.await
		.expect("timed out waiting for fetch")
		.expect("fetch task panicked")
		.unwrap();
	assert_eq!(fetched.sequence(), 5);
	let frame = fetched.read_frame().await.unwrap().expect("expected fetched frame");
	assert_eq!(frame.payload, b"fetched".to_vec());
	assert_eq!(frame.timestamp_us, 100_000);
}

#[tokio::test]
async fn dynamic_track_rejects_fetch_miss() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let track = broadcast.publish_track("events".into(), None).unwrap();
	let dynamic = track.dynamic().unwrap();
	let consumer = broadcast.consume().unwrap();

	let fetch = tokio::spawn(async move { consumer.fetch_group("events".into(), 5, None).await });
	let request = tokio::time::timeout(TIMEOUT, dynamic.requested_group())
		.await
		.expect("timed out waiting for group request")
		.unwrap();
	request.abort(404).unwrap();

	let result = tokio::time::timeout(TIMEOUT, fetch)
		.await
		.expect("timed out waiting for rejected fetch")
		.expect("fetch task panicked");
	assert!(matches!(result, Err(MoqError::Protocol(moq_net::Error::App(404)))));
	assert!(matches!(request.accept(), Err(MoqError::Closed)));
}

#[tokio::test]
async fn fetch_miss_without_dynamic_is_not_found() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let _track = broadcast.publish_track("events".into(), None).unwrap();
	let consumer = broadcast.consume().unwrap();

	let result = consumer.fetch_group("events".into(), 5, None).await;
	assert!(matches!(result, Err(MoqError::NotFound)));
}

#[tokio::test]
async fn fetch_unknown_track_is_not_found() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let consumer = broadcast.consume().unwrap();

	let result = consumer.fetch_group("missing".into(), 0, None).await;
	assert!(matches!(result, Err(MoqError::NotFound)));
}

#[tokio::test]
async fn requested_track_dynamic_survives_accept() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let broadcast_dynamic = broadcast.dynamic().unwrap();
	let consumer = broadcast.consume().unwrap();

	let fetch = tokio::spawn(async move { consumer.fetch_group("archive".into(), 9, None).await });
	let request = tokio::time::timeout(TIMEOUT, broadcast_dynamic.requested_track())
		.await
		.expect("timed out waiting for track request")
		.unwrap();
	let track_dynamic = request.dynamic().unwrap();
	let _track = request.accept(None).unwrap();

	let group_request = tokio::time::timeout(TIMEOUT, track_dynamic.requested_group())
		.await
		.expect("timed out waiting for group request")
		.unwrap();
	assert_eq!(group_request.sequence(), 9);
	let group = group_request.accept().unwrap();
	group.write_frame(b"archive".to_vec(), 180_000).unwrap();
	group.finish().unwrap();

	let fetched = tokio::time::timeout(TIMEOUT, fetch)
		.await
		.expect("timed out waiting for fetch")
		.expect("fetch task panicked")
		.unwrap();
	let frame = fetched.read_frame().await.unwrap().expect("expected archive frame");
	assert_eq!(frame.payload, b"archive".to_vec());
	assert_eq!(frame.timestamp_us, 180_000);
}

#[tokio::test]
async fn dynamic_track_request_can_publish_media() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let dynamic = broadcast.dynamic().unwrap();
	let consumer = broadcast.consume().unwrap();
	let catalog_consumer = consumer.subscribe_catalog().await.unwrap();

	// publish_media_on_track accepts the request (at the media timescale), which is what
	// unblocks subscribe_media, so the subscribe runs on a concurrent task until then.
	let subscribe = {
		let consumer = consumer.clone();
		tokio::spawn(async move {
			consumer
				.subscribe_media("requested-audio".into(), crate::media::Container::Legacy, 10_000, None)
				.await
		})
	};

	let track = tokio::time::timeout(TIMEOUT, dynamic.requested_track())
		.await
		.expect("timed out waiting for requested track")
		.unwrap();
	assert_eq!(track.name().unwrap(), "requested-audio");

	let media = broadcast
		.publish_media_on_track(&track, media_init("opus", opus_head()))
		.unwrap();
	assert_eq!(media.name().unwrap(), "requested-audio");
	assert!(matches!(track.name(), Err(MoqError::Closed)));

	let media_consumer = tokio::time::timeout(TIMEOUT, subscribe)
		.await
		.expect("timed out waiting for subscribe")
		.expect("subscribe task panicked")
		.unwrap();

	let catalog = tokio::time::timeout(TIMEOUT, catalog_consumer.next())
		.await
		.expect("timed out waiting for catalog")
		.unwrap()
		.expect("expected a catalog");
	let audio = catalog
		.audio
		.get("requested-audio")
		.expect("requested track should be in catalog");
	assert_eq!(audio.codec, "opus");
	assert_eq!(audio.sample_rate, 48000);
	assert_eq!(audio.channel_count, 2);

	let payload = b"dynamic opus frame".to_vec();
	media.write_frame(payload.clone(), 20_000).unwrap();

	let frame = tokio::time::timeout(TIMEOUT, media_consumer.next())
		.await
		.expect("timed out waiting for media frame")
		.unwrap()
		.expect("expected a frame");
	assert_eq!(frame.payload, payload);
	assert_eq!(frame.timestamp_us, 20_000);

	media.finish().unwrap();
}

#[tokio::test]
async fn media_track_activity_and_name() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let init = opus_head();
	let media = broadcast.publish_media(media_init("opus", init)).unwrap();
	let track_name = media.name().unwrap();
	assert_eq!(track_name, "0.opus");

	let broadcast_consumer = broadcast.consume().unwrap();
	let catalog_consumer = broadcast_consumer.subscribe_catalog().await.unwrap();
	let catalog = tokio::time::timeout(TIMEOUT, catalog_consumer.next())
		.await
		.expect("timed out waiting for catalog")
		.unwrap()
		.expect("expected a catalog");
	assert!(catalog.audio.contains_key(&track_name));

	let track_consumer = broadcast_consumer.subscribe_track(track_name, None).await.unwrap();
	tokio::time::timeout(TIMEOUT, media.used())
		.await
		.expect("timed out waiting for media track to become used")
		.unwrap();

	drop(track_consumer);
	tokio::time::timeout(TIMEOUT, media.unused())
		.await
		.expect("timed out waiting for media track to become unused")
		.unwrap();
}

#[tokio::test]
async fn publish_media_aac_populates_description() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let config = moq_mux::codec::aac::Config {
		profile: 2,
		sample_rate: 44_100,
		channel_count: 2,
	};
	let init = config.encode();
	let _media = broadcast.publish_media(media_init("aac", init.to_vec())).unwrap();

	let consumer = broadcast.consume().unwrap();
	let catalog_consumer = consumer.subscribe_catalog().await.unwrap();
	let catalog = tokio::time::timeout(TIMEOUT, catalog_consumer.next())
		.await
		.expect("timed out waiting for catalog")
		.unwrap()
		.expect("expected a catalog");

	assert_eq!(catalog.audio.len(), 1);
	let audio = catalog.audio.values().next().unwrap();
	assert_eq!(audio.codec, "mp4a.40.2");
	assert_eq!(audio.sample_rate, config.sample_rate);
	assert_eq!(audio.channel_count, config.channel_count);
	assert_eq!(audio.description.as_deref(), Some(init.as_ref()));
}

#[test]
fn unknown_format() {
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let err = broadcast
		.publish_media(media_init("nope", vec![]))
		.err()
		.expect("unknown format should fail");
	assert!(
		matches!(err, crate::error::MoqError::Codec(_)),
		"expected Codec error, got {err}"
	);
}

#[tokio::test]
async fn local_publish_consume_audio() {
	let origin = MoqOriginProducer::new(MoqOriginOptions::default());
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let init = opus_head();
	let media = broadcast.publish_media(media_init("opus", init)).unwrap();
	origin.announce("live".into(), &broadcast).unwrap();

	let consumer = origin.consume();
	let announced = consumer.announced("".into()).unwrap();

	let announcement = tokio::time::timeout(TIMEOUT, announced.next())
		.await
		.expect("timed out waiting for announcement")
		.unwrap()
		.expect("expected an announcement");

	assert_eq!(announcement.path(), "live");

	let broadcast_consumer = announcement.broadcast();
	let catalog_consumer = broadcast_consumer.subscribe_catalog().await.unwrap();

	let catalog = tokio::time::timeout(TIMEOUT, catalog_consumer.next())
		.await
		.expect("timed out waiting for catalog")
		.unwrap()
		.expect("expected a catalog");

	assert_eq!(catalog.audio.len(), 1);
	let (track_name, audio) = catalog.audio.iter().next().unwrap();
	assert_eq!(audio.codec, "opus");
	assert_eq!(audio.sample_rate, 48000);
	assert_eq!(audio.channel_count, 2);
	assert!(catalog.video.is_empty());

	let media_consumer = broadcast_consumer
		.subscribe_media(track_name.clone(), audio.container.clone(), 10_000, None)
		.await
		.unwrap();

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
	let origin = MoqOriginProducer::new(MoqOriginOptions::default());
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let init = h264_init();
	let media = broadcast.publish_media(media_init("avc3", init)).unwrap();
	origin.announce("video-test".into(), &broadcast).unwrap();

	let consumer = origin.consume();
	let announced = consumer.announced("".into()).unwrap();

	let announcement = tokio::time::timeout(TIMEOUT, announced.next())
		.await
		.expect("timed out")
		.unwrap()
		.expect("expected announcement");

	let broadcast_consumer = announcement.broadcast();
	let catalog_consumer = broadcast_consumer.subscribe_catalog().await.unwrap();

	let catalog = tokio::time::timeout(TIMEOUT, catalog_consumer.next())
		.await
		.expect("timed out")
		.unwrap()
		.expect("expected catalog");

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
	assert!(catalog.audio.is_empty());

	let media_consumer = broadcast_consumer
		.subscribe_media(track_name.clone(), video.container.clone(), 10_000, None)
		.await
		.unwrap();

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
	let origin = MoqOriginProducer::new(MoqOriginOptions::default());
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let init = opus_head();
	let media = broadcast.publish_media(media_init("opus", init)).unwrap();
	origin.announce("ordering-test".into(), &broadcast).unwrap();

	let consumer = origin.consume();
	let announced = consumer.announced("".into()).unwrap();
	let announcement = tokio::time::timeout(TIMEOUT, announced.next())
		.await
		.unwrap()
		.unwrap()
		.unwrap();

	let broadcast_consumer = announcement.broadcast();
	let catalog_consumer = broadcast_consumer.subscribe_catalog().await.unwrap();
	let catalog = tokio::time::timeout(TIMEOUT, catalog_consumer.next())
		.await
		.unwrap()
		.unwrap()
		.unwrap();

	let (track_name, audio) = catalog.audio.iter().next().unwrap();
	let media_consumer = broadcast_consumer
		.subscribe_media(track_name.clone(), audio.container.clone(), 10_000, None)
		.await
		.unwrap();

	let timestamps: [u64; 5] = [0, 20_000, 40_000, 60_000, 80_000];
	for (i, &ts) in timestamps.iter().enumerate() {
		let payload = format!("frame-{i}");
		media.write_frame(payload.into_bytes(), ts).unwrap();
	}

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
	let origin = MoqOriginProducer::new(MoqOriginOptions::default());
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let init = opus_head();
	let _media1 = broadcast.publish_media(media_init("opus", init.clone())).unwrap();
	origin.announce("catalog-update".into(), &broadcast).unwrap();

	let consumer = origin.consume();
	let announced = consumer.announced("".into()).unwrap();
	let announcement = tokio::time::timeout(TIMEOUT, announced.next())
		.await
		.unwrap()
		.unwrap()
		.unwrap();

	let broadcast_consumer = announcement.broadcast();
	let catalog_consumer = broadcast_consumer.subscribe_catalog().await.unwrap();

	let catalog1 = tokio::time::timeout(TIMEOUT, catalog_consumer.next())
		.await
		.unwrap()
		.unwrap()
		.unwrap();
	assert_eq!(catalog1.audio.len(), 1);

	let _media2 = broadcast.publish_media(media_init("opus", init)).unwrap();

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
	let _media = broadcast.publish_media(media_init("opus", init)).unwrap();
	broadcast.finish().unwrap();

	let err = broadcast.finish().unwrap_err();
	assert!(
		matches!(err, crate::error::MoqError::Closed),
		"expected Closed error, got {err}"
	);
}

#[tokio::test]
async fn announced_broadcast() {
	let origin = MoqOriginProducer::new(MoqOriginOptions::default());
	let broadcast = MoqBroadcastProducer::new().unwrap();
	origin.announce("test/broadcast".into(), &broadcast).unwrap();

	let consumer = origin.consume();
	let announced = consumer.announced("".into()).unwrap();

	let announcement = tokio::time::timeout(TIMEOUT, announced.next())
		.await
		.expect("timed out")
		.unwrap()
		.expect("expected announcement");

	assert_eq!(announcement.path(), "test/broadcast");
	let _catalog = announcement.broadcast().subscribe_catalog().await.unwrap();
}

#[tokio::test]
async fn dynamic_broadcast_request() {
	let origin = MoqOriginProducer::new(MoqOriginOptions::default());
	let dynamic = origin.dynamic();
	let consumer = origin.consume();

	let request_broadcast = {
		let consumer = consumer.clone();
		tokio::spawn(async move { consumer.request_broadcast("dynamic/broadcast".into()).await })
	};

	let request = tokio::time::timeout(TIMEOUT, dynamic.requested_broadcast())
		.await
		.expect("timed out waiting for requested broadcast")
		.unwrap();
	assert_eq!(request.path().unwrap(), "dynamic/broadcast");

	let served = MoqBroadcastProducer::new().unwrap();
	let track = served.publish_track("status".into(), None).unwrap();
	request.accept(&served).unwrap();
	assert!(matches!(request.path(), Err(MoqError::Closed)));

	let broadcast = tokio::time::timeout(TIMEOUT, request_broadcast)
		.await
		.expect("timed out waiting for requested broadcast result")
		.expect("request task panicked")
		.unwrap();

	let track_consumer = broadcast.subscribe_track("status".into(), None).await.unwrap();
	let payload = b"served dynamically".to_vec();
	track.write_frame(payload.clone(), 20_000).unwrap();

	let frame = tokio::time::timeout(TIMEOUT, track_consumer.read_frame())
		.await
		.expect("timed out waiting for dynamic broadcast frame")
		.unwrap()
		.expect("expected a frame");
	assert_eq!(frame.payload, payload);
	assert_eq!(frame.timestamp_us, 20_000);

	track.finish().unwrap();
	served.finish().unwrap();
}

#[tokio::test]
async fn dynamic_broadcast_request_can_reject() {
	let origin = MoqOriginProducer::new(MoqOriginOptions::default());
	let dynamic = origin.dynamic();
	let consumer = origin.consume();

	let request_broadcast = {
		let consumer = consumer.clone();
		tokio::spawn(async move { consumer.request_broadcast("missing".into()).await })
	};

	let request = tokio::time::timeout(TIMEOUT, dynamic.requested_broadcast())
		.await
		.expect("timed out waiting for requested broadcast")
		.unwrap();
	assert_eq!(request.path().unwrap(), "missing");

	request.abort(404).unwrap();
	assert!(matches!(request.path(), Err(MoqError::Closed)));

	let result = tokio::time::timeout(TIMEOUT, request_broadcast)
		.await
		.expect("timed out waiting for rejected broadcast")
		.expect("request task panicked");
	assert!(result.is_err(), "request for a rejected broadcast should fail");
}

#[test]
fn without_runtime() {
	std::thread::spawn(|| {
		let origin = MoqOriginProducer::new(MoqOriginOptions::default());
		let consumer = origin.consume();

		let broadcast = MoqBroadcastProducer::new().unwrap();
		let init = opus_head();
		let media = broadcast.publish_media(media_init("opus", init)).unwrap();
		media.write_frame(b"hello".to_vec(), 1000).unwrap();
		origin.announce("test".into(), &broadcast).unwrap();

		let announced = consumer.announced("".into()).unwrap();
		let announcement = pollster::block_on(announced.next()).unwrap().unwrap();
		assert_eq!(announcement.path(), "test");
		let _bc = announcement.broadcast();

		let client = MoqClient::new();
		client.set_tls_disable_verify(true);
		client.set_consume(Some(origin));

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
	.expect("client thread panicked, FFI method missing runtime guard");
}

#[tokio::test]
async fn server_client_roundtrip() {
	// Server side: bind, set a publish origin, accept incoming sessions.
	let server_origin = MoqOriginProducer::new(MoqOriginOptions::default());
	let server = MoqServer::new();
	server.set_bind("127.0.0.1:0".into()).unwrap();
	server.set_tls_generate(vec!["localhost".into()]);
	server.set_publish(Some(server_origin.clone()));

	let addr = tokio::time::timeout(TIMEOUT, server.listen())
		.await
		.expect("listen timed out")
		.expect("listen failed");
	let url = format!("https://{addr}");

	let accept_server = server.clone();
	let accept = tokio::spawn(async move {
		let request = accept_server
			.accept()
			.await
			.expect("accept errored")
			.expect("accept returned None");
		request.ok().await.expect("handshake failed")
	});

	// Client side: connect, subscribe via a consume origin.
	let client_origin = MoqOriginProducer::new(MoqOriginOptions::default());
	let client = MoqClient::new();
	client.set_tls_disable_verify(true);
	client.set_bind("127.0.0.1:0".into()).unwrap();
	client.set_consume(Some(client_origin.clone()));
	let cs = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("connect timed out")
		.expect("connect failed");

	let server_session = tokio::time::timeout(TIMEOUT, accept)
		.await
		.expect("server accept timed out")
		.expect("server accept task panicked");

	// Publish a broadcast on the server side.
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let init = opus_head();
	let media = broadcast.publish_media(media_init("opus", init)).unwrap();
	server_origin.announce("hello".into(), &broadcast).unwrap();

	// Receive the announcement on the client side via the consume origin.
	let consumer = client_origin.consume();
	let announced = consumer.announced("".into()).unwrap();
	let announcement = tokio::time::timeout(TIMEOUT, announced.next())
		.await
		.expect("timed out waiting for announcement over the wire")
		.unwrap()
		.expect("expected an announcement");
	assert_eq!(announcement.path(), "hello");

	// Subscribe to the audio track and verify a frame round-trips.
	let bc = announcement.broadcast();
	let catalog_consumer = bc.subscribe_catalog().await.unwrap();
	let catalog = tokio::time::timeout(TIMEOUT, catalog_consumer.next())
		.await
		.expect("timed out waiting for catalog")
		.unwrap()
		.expect("expected a catalog");
	let (track_name, audio) = catalog.audio.iter().next().unwrap();
	let media_consumer = bc
		.subscribe_media(track_name.clone(), audio.container.clone(), 10_000, None)
		.await
		.unwrap();

	let payload = b"hello over the wire".to_vec();
	media.write_frame(payload.clone(), 1_000_000).unwrap();

	let frame = tokio::time::timeout(TIMEOUT, media_consumer.next())
		.await
		.expect("timed out waiting for frame")
		.unwrap()
		.expect("expected a frame");
	assert_eq!(frame.payload, payload);
	assert_eq!(frame.timestamp_us, 1_000_000);

	// Clean up. Exercise `shutdown()` on the client side and the underlying
	// `cancel(code)` on the server side, so both shutdown paths run.
	media.finish().unwrap();
	broadcast.finish().unwrap();
	cs.shutdown();
	server_session.cancel(0);
	server.cancel();
}

#[tokio::test]
async fn server_client_roundtrip_auto_origin() {
	// Same shape as `server_client_roundtrip` but the client never calls
	// `set_publish` / `set_consume`: the auto-created origin sides on
	// `MoqClientSession` are what drive publishing and subscribing.
	let server_origin = MoqOriginProducer::new(MoqOriginOptions::default());
	let server = MoqServer::new();
	server.set_bind("127.0.0.1:0".into()).unwrap();
	server.set_tls_generate(vec!["localhost".into()]);
	server.set_publish(Some(server_origin.clone()));

	let addr = tokio::time::timeout(TIMEOUT, server.listen())
		.await
		.expect("listen timed out")
		.expect("listen failed");
	let url = format!("https://{addr}");

	let accept_server = server.clone();
	let accept = tokio::spawn(async move {
		let request = accept_server
			.accept()
			.await
			.expect("accept errored")
			.expect("accept returned None");
		request.ok().await.expect("handshake failed")
	});

	// No set_publish / set_consume, so this uses the auto-origin path.
	let client = MoqClient::new();
	client.set_tls_disable_verify(true);
	client.set_bind("127.0.0.1:0".into()).unwrap();
	let cs = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("connect timed out")
		.expect("connect failed");

	let publisher = cs.publisher();
	let consumer = cs.consumer();

	let server_session = tokio::time::timeout(TIMEOUT, accept)
		.await
		.expect("server accept timed out")
		.expect("server accept task panicked");

	// Server publishes; client receives via the auto consumer.
	let broadcast = MoqBroadcastProducer::new().unwrap();
	let init = opus_head();
	let media = broadcast.publish_media(media_init("opus", init)).unwrap();
	server_origin.announce("hello".into(), &broadcast).unwrap();

	let announced = consumer.announced("".into()).unwrap();
	let announcement = tokio::time::timeout(TIMEOUT, announced.next())
		.await
		.expect("timed out waiting for announcement over the wire")
		.unwrap()
		.expect("expected an announcement");
	assert_eq!(announcement.path(), "hello");

	// The auto publisher is wired too: dropping it should not break anything,
	// and a local announce() on it should succeed (though the server
	// isn't consuming, so we only verify the call doesn't error).
	let local_broadcast = MoqBroadcastProducer::new().unwrap();
	publisher.announce("local-only".into(), &local_broadcast).unwrap();
	local_broadcast.finish().unwrap();

	media.finish().unwrap();
	broadcast.finish().unwrap();
	cs.shutdown();
	server_session.cancel(0);
	server.cancel();
}

#[tokio::test]
async fn server_set_bind_validates() {
	let server = MoqServer::new();
	assert!(server.set_bind("127.0.0.1:0".into()).is_ok());
	assert!(server.set_bind("[::]:443".into()).is_ok());
	assert!(server.set_bind("localhost:4443".into()).is_ok());
	assert!(matches!(
		server.set_bind("not-an-address".into()),
		Err(crate::error::MoqError::Bind(_))
	));
}

#[tokio::test]
async fn server_cert_fingerprints_available_after_listen() {
	let server = MoqServer::new();
	server.set_bind("127.0.0.1:0".into()).unwrap();
	server.set_tls_generate(vec!["localhost".into()]);

	// Not available before listen().
	assert!(matches!(
		server.cert_fingerprints(),
		Err(crate::error::MoqError::Bind(_))
	));

	tokio::time::timeout(TIMEOUT, server.listen())
		.await
		.expect("listen timed out")
		.expect("listen failed");

	let fps = server.cert_fingerprints().expect("fingerprints available");
	assert_eq!(fps.len(), 1, "one generated cert => one fingerprint");
	// Hex-encoded SHA-256 is 64 chars.
	assert_eq!(fps[0].len(), 64, "fingerprint should be hex SHA-256");
	assert!(fps[0].chars().all(|c| c.is_ascii_hexdigit()));
}

#[tokio::test]
async fn request_double_respond_returns_already_responded() {
	use crate::error::MoqError;

	let server = MoqServer::new();
	server.set_bind("127.0.0.1:0".into()).unwrap();
	server.set_tls_generate(vec!["localhost".into()]);
	let addr = server.listen().await.expect("listen failed");

	let url = format!("https://{addr}");
	let accept_server = server.clone();
	let accept = tokio::spawn(async move {
		let request = accept_server
			.accept()
			.await
			.expect("accept errored")
			.expect("accept returned None");

		// Accept once, then try a second response. It must error.
		let session = request.ok().await.expect("first ok succeeds");
		let second_ok = request.ok().await;
		assert!(
			matches!(second_ok, Err(MoqError::AlreadyResponded)),
			"second ok() must fail"
		);
		let second_close = request.close(403).await;
		assert!(
			matches!(second_close, Err(MoqError::AlreadyResponded)),
			"close after ok must fail"
		);
		session
	});

	let client = MoqClient::new();
	client.set_tls_disable_verify(true);
	client.set_bind("127.0.0.1:0".into()).unwrap();
	let _session = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("connect timed out")
		.expect("connect failed");

	let server_session = tokio::time::timeout(TIMEOUT, accept)
		.await
		.expect("accept timed out")
		.expect("accept task panicked");

	server_session.cancel(0);
	server.cancel();
}

#[tokio::test]
async fn request_per_session_publish_override() {
	// The server's publish origin is empty; a per-request override is used instead.
	let server = MoqServer::new();
	server.set_bind("127.0.0.1:0".into()).unwrap();
	server.set_tls_generate(vec!["localhost".into()]);

	let addr = server.listen().await.expect("listen failed");
	let url = format!("https://{addr}");

	let override_origin = MoqOriginProducer::new(MoqOriginOptions::default());
	let override_for_task = override_origin.clone();

	let accept_server = server.clone();
	let accept = tokio::spawn(async move {
		let request = accept_server
			.accept()
			.await
			.expect("accept errored")
			.expect("accept returned None");
		// Override publish on a per-request basis.
		request.set_publish(Some(override_for_task));
		request.ok().await.expect("ok succeeds")
	});

	let client_origin = MoqOriginProducer::new(MoqOriginOptions::default());
	let client = MoqClient::new();
	client.set_tls_disable_verify(true);
	client.set_bind("127.0.0.1:0".into()).unwrap();
	client.set_consume(Some(client_origin.clone()));
	let cs = tokio::time::timeout(TIMEOUT, client.connect(url))
		.await
		.expect("connect timed out")
		.expect("connect failed");

	let server_session = tokio::time::timeout(TIMEOUT, accept)
		.await
		.expect("accept timed out")
		.expect("accept task panicked");

	// Publishing on the override origin must reach the client.
	let broadcast = MoqBroadcastProducer::new().unwrap();
	override_origin.announce("override-only".into(), &broadcast).unwrap();

	let consumer = client_origin.consume();
	let announced = consumer.announced("".into()).unwrap();
	let announcement = tokio::time::timeout(TIMEOUT, announced.next())
		.await
		.expect("timed out waiting for override announcement")
		.unwrap()
		.expect("expected an announcement");
	assert_eq!(announcement.path(), "override-only");

	broadcast.finish().unwrap();
	cs.cancel(0);
	server_session.cancel(0);
	server.cancel();
}
