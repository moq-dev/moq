//! Bitstream-shape tests that don't need a live WebRTC peer.
//!
//! The H.264 Annex-B -> AVCC conversion is provided by `moq_mux::codec::h264`,
//! but the WHIP path depends on the importer parsing the SPS for the catalog
//! and accepting Annex-B input via the bridge. These tests guard against
//! regressions in the contract the moq-rtc bridge depends on.
//!
//! Inline (rather than under `tests/`) because they exercise the crate-private
//! `codec` module directly.

use bytes::{Bytes, BytesMut};

use crate::codec::{self, Bridge, Frame, Track};

const START_CODE_4: &[u8] = &[0, 0, 0, 1];

fn annexb(nals: &[&[u8]]) -> Bytes {
	let mut buf = BytesMut::new();
	for nal in nals {
		buf.extend_from_slice(START_CODE_4);
		buf.extend_from_slice(nal);
	}
	buf.freeze()
}

#[tokio::test(start_paused = true)]
async fn h264_annexb_frame_publishes_catalog_entry() {
	// Real SPS+PPS pair lifted from moq-mux's avc3 catalog test. Anything
	// shorter and h264-parser's RBSP decoder runs out of bytes parsing
	// vui_parameters; not worth synthesizing a smaller one by hand.
	let sps: &[u8] = &[
		0x67, 0x42, 0xc0, 0x1f, 0xda, 0x01, 0x40, 0x16, 0xe9, 0xb8, 0x08, 0x08, 0x0a, 0x00, 0x00, 0x07, 0xd0, 0x00,
		0x01, 0xd4, 0xc0, 0x80,
	];
	let pps: &[u8] = &[0x68, 0xce, 0x3c, 0x80];
	let idr: &[u8] = &[0x65, 0x88, 0x84, 0x21];

	let frame = annexb(&[sps, pps, idr]);

	let broadcast = moq_net::broadcast::Info::new();
	let mut producer = broadcast.produce();
	let catalog = moq_mux::catalog::Producer::new(&mut producer).expect("catalog");

	let mut bridge = codec::h264::Bridge::new(producer, catalog.clone()).expect("bridge");

	let codec_frame = Frame {
		timestamp_us: 0,
		payload: frame,
	};
	Bridge::push(&mut bridge, codec_frame).expect("push");

	let snapshot = catalog.snapshot();
	assert_eq!(
		snapshot.video.renditions.len(),
		1,
		"one video rendition must land in catalog"
	);

	let cfg = snapshot.video.renditions.values().next().unwrap();
	let hang::catalog::VideoCodec::H264(h264) = &cfg.codec else {
		panic!("expected H.264 video config, got {:?}", cfg.codec);
	};
	assert!(h264.inline, "WHIP path uses Avc3 (inline SPS/PPS)");
	assert_eq!(h264.profile, sps[1], "profile_idc from SPS");
	assert_eq!(h264.level, sps[3], "level_idc from SPS");
}

#[tokio::test(start_paused = true)]
async fn opus_frame_publishes_catalog_entry() {
	let broadcast = moq_net::broadcast::Info::new();
	let mut producer = broadcast.produce();
	let catalog = moq_mux::catalog::Producer::new(&mut producer).expect("catalog");

	let mut bridge = codec::opus::Bridge::new(producer, catalog.clone(), 48_000, 2).expect("bridge");

	let payload = Bytes::from_static(&[0xfc, 0xff, 0xfe]); // arbitrary 3-byte Opus packet
	let codec_frame = Frame {
		timestamp_us: 20_000,
		payload,
	};
	Bridge::push(&mut bridge, codec_frame).expect("push");

	let snapshot = catalog.snapshot();
	assert_eq!(snapshot.audio.renditions.len(), 1);
	let cfg = snapshot.audio.renditions.values().next().unwrap();
	assert_eq!(cfg.sample_rate, 48_000);
	assert_eq!(cfg.channel_count, 2);
	assert!(matches!(cfg.codec, hang::catalog::AudioCodec::Opus));
}

#[tokio::test(start_paused = true)]
async fn idle_video_bridges_do_not_gate_audio_catalog() {
	let broadcast = moq_net::broadcast::Info::new();
	let mut producer = broadcast.produce();
	let catalog = moq_mux::catalog::Producer::new(&mut producer).expect("catalog");
	let mut updates = catalog.consume().expect("catalog consumer");

	let _vp8 = codec::vp8::Bridge::new(producer.clone(), catalog.clone()).expect("vp8 bridge");
	let _vp9 = codec::vp9::Bridge::new(producer.clone(), catalog.clone()).expect("vp9 bridge");
	let mut opus = codec::opus::Bridge::new(producer, catalog, 48_000, 2).expect("opus bridge");
	Bridge::push(
		&mut opus,
		Frame {
			timestamp_us: 0,
			payload: Bytes::from_static(&[0xfc, 0xff, 0xfe]),
		},
	)
	.expect("push opus");

	let snapshot = tokio::time::timeout(std::time::Duration::from_millis(1), updates.next())
		.await
		.expect("idle video must not gate active audio")
		.expect("catalog update")
		.expect("catalog snapshot");
	assert_eq!(snapshot.audio.renditions.len(), 1);
	assert!(snapshot.video.renditions.is_empty());
}

#[tokio::test(start_paused = true)]
async fn vp9_keyframe_publishes_dimensions_and_starts_group() {
	let broadcast = moq_net::broadcast::Info::new();
	let mut producer = broadcast.produce();
	let catalog = moq_mux::catalog::Producer::new(&mut producer).expect("catalog");

	let mut bridge = codec::vp9::Bridge::new(producer, catalog.clone()).expect("bridge");

	Bridge::push(
		&mut bridge,
		Frame {
			timestamp_us: 0,
			// VP9 profile 0 keyframe header for 320x240.
			payload: Bytes::from_static(&[0x82, 0x49, 0x83, 0x42, 0x20, 0x13, 0xf0, 0x0e, 0xf0, 0x00]),
		},
	)
	.expect("keyframe accepted");

	// A non-keyframe right after a keyframe must not panic; the underlying
	// container Producer requires keyframes start groups, and a stray
	// inter-frame following a keyframe should extend the current group.
	Bridge::push(
		&mut bridge,
		Frame {
			timestamp_us: 33_000,
			// frame_marker=10, profile=0, show_existing=0, frame_type=1.
			payload: Bytes::from_static(&[0x84, 0x00, 0x00]),
		},
	)
	.expect("interframe accepted");

	let snapshot = catalog.snapshot();
	assert_eq!(snapshot.video.renditions.len(), 1, "vp9 rendition announced");
	let config = snapshot.video.renditions.values().next().unwrap();
	assert_eq!(config.coded_width, Some(320));
	assert_eq!(config.coded_height, Some(240));
}

// ── Egress (RTP-out) round-trip tests ─────────────────────────────────────
//
// Each test feeds the *ingest* bridge with a representative codec frame,
// then sets up an `EgressSource` against the same broadcast and walks the
// catalog rendition through `codec::Track::next()`. Verifies that the
// emitted payload is in the shape str0m's Frame API expects:
//
// - Opus / VP8 / VP9: passthrough.
// - H.264 with avc3 storage (inline SPS/PPS): passthrough.
// - H.264 with avc1 storage: length-prefix -> start-code with SPS+PPS
//   prefixed on every keyframe (regression in moq-mux already covers this;
//   this test confirms moq-rtc's wrapper plumbs through correctly).

#[tokio::test(start_paused = true)]
async fn egress_opus_passthrough() {
	// Build an opus broadcast via the ingest bridge.
	let broadcast = moq_net::broadcast::Info::new();
	let mut producer = broadcast.produce();
	let catalog = moq_mux::catalog::Producer::new(&mut producer).expect("catalog");
	let mut bridge = codec::opus::Bridge::new(producer.clone(), catalog.clone(), 48_000, 2).expect("bridge");

	let payload = Bytes::from_static(&[0xfc, 0xff, 0xfe]);
	Bridge::push(
		&mut bridge,
		Frame {
			timestamp_us: 20_000,
			payload: payload.clone(),
		},
	)
	.expect("push");

	// Snapshot the catalog while the bridge is still alive (Drop tears down
	// the rendition entry). Open the egress track from the snapshot first.
	let snapshot = catalog.snapshot();
	let (name, _) = snapshot.audio.renditions.iter().next().expect("rendition");
	let consumer = producer.consume();
	let track = consumer
		.track(name)
		.expect("track")
		.subscribe(None)
		.await
		.expect("subscribe");
	let mut track = Track::opus(track);

	let frame = track.next().await.expect("ok").expect("frame");
	assert_eq!(frame.timestamp_us, 20_000);
	assert_eq!(frame.payload.as_ref(), payload.as_ref());
}

#[tokio::test(start_paused = true)]
async fn egress_h264_avc3_passthrough() {
	let sps: &[u8] = &[
		0x67, 0x42, 0xc0, 0x1f, 0xda, 0x01, 0x40, 0x16, 0xe9, 0xb8, 0x08, 0x08, 0x0a, 0x00, 0x00, 0x07, 0xd0, 0x00,
		0x01, 0xd4, 0xc0, 0x80,
	];
	let pps: &[u8] = &[0x68, 0xce, 0x3c, 0x80];
	let idr: &[u8] = &[0x65, 0x88, 0x84, 0x21];

	let broadcast = moq_net::broadcast::Info::new();
	let mut producer = broadcast.produce();
	let catalog = moq_mux::catalog::Producer::new(&mut producer).expect("catalog");

	let mut bridge = codec::h264::Bridge::new(producer.clone(), catalog.clone()).expect("bridge");
	Bridge::push(
		&mut bridge,
		Frame {
			timestamp_us: 0,
			payload: annexb(&[sps, pps, idr]),
		},
	)
	.expect("push");

	let snapshot = catalog.snapshot();
	let (name, config) = snapshot.video.renditions.iter().next().expect("rendition");
	let consumer = producer.consume();
	let track = consumer
		.track(name)
		.expect("track")
		.subscribe(None)
		.await
		.expect("subscribe");
	let mut track = Track::video(track, config).expect("h264 track");

	let frame = track.next().await.expect("ok").expect("frame");
	// avc3 storage means catalog `description` is empty; the egress track
	// is passthrough, so the bitstream is whatever the ingest bridge wrote
	// (Annex-B with SPS/PPS prepended ahead of the IDR).
	assert!(
		frame.payload.windows(4).any(|w| w == [0, 0, 0, 1]),
		"Annex-B start codes preserved"
	);
	assert!(
		frame.payload.windows(sps.len()).any(|w| w == sps),
		"SPS NAL present in egress frame"
	);
	assert!(
		frame.payload.windows(idr.len()).any(|w| w == idr),
		"IDR NAL present in egress frame"
	);
}
