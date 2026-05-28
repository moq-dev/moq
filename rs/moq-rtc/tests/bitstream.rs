//! Bitstream-shape tests that don't need a live WebRTC peer.
//!
//! The H.264 Annex-B -> AVCC conversion is provided by `moq_mux::codec::h264`,
//! but the WHIP path depends on the importer parsing the SPS for the catalog
//! and accepting Annex-B input via `decode_frame`. These tests guard against
//! regressions in the contract the moq-rtc bridge depends on.

use bytes::{Bytes, BytesMut};

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

	let broadcast = moq_net::Broadcast::new();
	let mut producer = broadcast.produce();
	let catalog = moq_mux::catalog::hang::Producer::new(&mut producer).expect("catalog");

	let mut bridge = moq_rtc::codec::h264::Bridge::new(producer, catalog.clone()).expect("bridge");

	let codec_frame = moq_rtc::codec::Frame {
		timestamp_us: 0,
		payload: frame,
	};
	moq_rtc::codec::Bridge::push(&mut bridge, codec_frame).expect("push");

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
	let broadcast = moq_net::Broadcast::new();
	let mut producer = broadcast.produce();
	let catalog = moq_mux::catalog::hang::Producer::new(&mut producer).expect("catalog");

	let mut bridge = moq_rtc::codec::opus::Bridge::new(producer, catalog.clone(), 48_000, 2).expect("bridge");

	let payload = Bytes::from_static(&[0xfc, 0xff, 0xfe]); // arbitrary 3-byte Opus packet
	let codec_frame = moq_rtc::codec::Frame {
		timestamp_us: 20_000,
		payload,
	};
	moq_rtc::codec::Bridge::push(&mut bridge, codec_frame).expect("push");

	let snapshot = catalog.snapshot();
	assert_eq!(snapshot.audio.renditions.len(), 1);
	let cfg = snapshot.audio.renditions.values().next().unwrap();
	assert_eq!(cfg.sample_rate, 48_000);
	assert_eq!(cfg.channel_count, 2);
	assert!(matches!(cfg.codec, hang::catalog::AudioCodec::Opus));
}

#[tokio::test(start_paused = true)]
async fn vp9_keyframe_flag_from_uncompressed_header() {
	let broadcast = moq_net::Broadcast::new();
	let mut producer = broadcast.produce();
	let catalog = moq_mux::catalog::hang::Producer::new(&mut producer).expect("catalog");

	let mut bridge = moq_rtc::codec::vp9::Bridge::new(producer, catalog.clone()).expect("bridge");

	// VP9 uncompressed header: frame_type is bit 2. 0 = keyframe, 1 = inter.
	// Byte with bit 2 cleared is a keyframe; with bit 2 set is an inter frame.
	let keyframe_byte = 0b1000_0010; // frame_marker=10, profile bits, frame_type=0
	let interframe_byte = 0b1000_0110; // same shape but frame_type=1

	moq_rtc::codec::Bridge::push(
		&mut bridge,
		moq_rtc::codec::Frame {
			timestamp_us: 0,
			payload: Bytes::from(vec![keyframe_byte, 0, 0]),
		},
	)
	.expect("keyframe accepted");

	// A non-keyframe right after a keyframe must not panic; the underlying
	// container Producer requires keyframes start groups, and a stray
	// inter-frame following a keyframe should extend the current group.
	moq_rtc::codec::Bridge::push(
		&mut bridge,
		moq_rtc::codec::Frame {
			timestamp_us: 33_000,
			payload: Bytes::from(vec![interframe_byte, 0, 0]),
		},
	)
	.expect("interframe accepted");

	assert_eq!(catalog.snapshot().video.renditions.len(), 1, "vp9 rendition announced");
}
