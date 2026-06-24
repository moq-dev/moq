//! Tests for the FLV muxer.
//!
//! Round-trip: ingest a synthetic FLV via the importer, re-export via the
//! exporter, and assert the bytes parse back into the same catalog shape.

use std::time::Duration;

use hang::catalog::{AudioCodec, VideoCodec};

use super::{Export, Import};

/// A minimal `AVCDecoderConfigurationRecord` (profile 0x42, level 0x1f, one SPS + PPS).
fn avcc() -> Vec<u8> {
	let sps = [0x67u8, 0x42, 0xc0, 0x1f];
	let mut out = vec![0x01, 0x42, 0xc0, 0x1f, 0xff, 0xe1, 0x00, sps.len() as u8];
	out.extend_from_slice(&sps);
	out.extend_from_slice(&[0x01, 0x00, 0x04, 0x68, 0xce, 0x3c, 0x80]);
	out
}

/// AudioSpecificConfig for AAC-LC, 44100 Hz, stereo.
const ASC: [u8; 2] = [0x12, 0x10];

fn write_tag(out: &mut Vec<u8>, tag_type: u8, timestamp: u32, body: &[u8]) {
	out.push(tag_type);
	out.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
	out.extend_from_slice(&timestamp.to_be_bytes()[1..]);
	out.push((timestamp >> 24) as u8);
	out.extend_from_slice(&[0, 0, 0]);
	out.extend_from_slice(body);
	out.extend_from_slice(&(11 + body.len() as u32).to_be_bytes());
}

/// Build an FLV with AVC + AAC sequence headers and a couple of frames each.
fn synth_flv() -> Vec<u8> {
	let mut out = Vec::new();
	out.extend_from_slice(b"FLV");
	out.push(1);
	out.push(0x05);
	out.extend_from_slice(&9u32.to_be_bytes());
	out.extend_from_slice(&0u32.to_be_bytes());

	let mut vseq = vec![
		(super::FRAME_TYPE_KEY << 4) | super::VIDEO_CODEC_AVC,
		super::AVC_SEQUENCE_HEADER,
		0,
		0,
		0,
	];
	vseq.extend_from_slice(&avcc());
	write_tag(&mut out, super::TAG_VIDEO, 0, &vseq);

	let mut aseq = vec![super::AAC_AUDIO_TAG_HEADER, super::AAC_SEQUENCE_HEADER];
	aseq.extend_from_slice(&ASC);
	write_tag(&mut out, super::TAG_AUDIO, 0, &aseq);

	// Video keyframe at t=0, inter frame at t=33ms.
	let idr = [0, 0, 0, 5, 0x65, 0x88, 0x84, 0x21, 0x00];
	let mut vkey = vec![
		(super::FRAME_TYPE_KEY << 4) | super::VIDEO_CODEC_AVC,
		super::AVC_NALU,
		0,
		0,
		0,
	];
	vkey.extend_from_slice(&idr);
	write_tag(&mut out, super::TAG_VIDEO, 0, &vkey);

	let p = [0, 0, 0, 4, 0x41, 0xe0, 0x12, 0x34];
	let mut vinter = vec![
		(super::FRAME_TYPE_INTER << 4) | super::VIDEO_CODEC_AVC,
		super::AVC_NALU,
		0,
		0,
		0,
	];
	vinter.extend_from_slice(&p);
	write_tag(&mut out, super::TAG_VIDEO, 33, &vinter);

	// Audio frames at t=0 and t=23ms.
	let mut a0 = vec![super::AAC_AUDIO_TAG_HEADER, super::AAC_RAW];
	a0.extend_from_slice(&[0xde, 0xad]);
	write_tag(&mut out, super::TAG_AUDIO, 0, &a0);

	let mut a1 = vec![super::AAC_AUDIO_TAG_HEADER, super::AAC_RAW];
	a1.extend_from_slice(&[0xbe, 0xef]);
	write_tag(&mut out, super::TAG_AUDIO, 23, &a1);

	out
}

/// Drive the exporter to completion, dropping the importer to signal EOS.
async fn drain_export(mut exporter: Export, mut importer: Import) -> Vec<u8> {
	// Finish the tracks cleanly so the exporter can reach end-of-stream instead of
	// seeing the producer dropped out from under it.
	importer.finish().unwrap();
	let mut exported = Vec::new();
	let mut importer = Some(importer);
	for _ in 0..64 {
		match tokio::time::timeout(Duration::from_millis(100), exporter.next()).await {
			Ok(Ok(Some(chunk))) => exported.extend_from_slice(&chunk),
			Ok(Ok(None)) => break,
			Ok(Err(e)) => panic!("exporter error: {e}"),
			Err(_) => importer = None, // close the broadcast so the exporter can EOS
		}
	}
	drop(importer);
	exported
}

#[tokio::test(start_paused = true)]
async fn export_roundtrips_through_import() {
	let mut producer = moq_net::BroadcastInfo::new().produce();
	let consumer = producer.consume();
	let mut catalog = crate::catalog::Producer::new(&mut producer).unwrap();

	let mut importer = Import::new(producer, catalog.clone());
	importer.decode(&bytes::BytesMut::from(synth_flv().as_slice())).unwrap();
	catalog.finish().unwrap();

	let exporter = Export::new(consumer).await.unwrap();
	let exported = drain_export(exporter, importer).await;

	// The export must be a real FLV stream.
	assert_eq!(&exported[0..3], b"FLV");

	// Re-import the exported bytes and confirm the catalog rebuilds identically.
	let mut bcast2 = moq_net::BroadcastInfo::new().produce();
	let cat2 = crate::catalog::Producer::new(&mut bcast2).unwrap();
	let mut imp2 = Import::new(bcast2, cat2.clone());
	imp2.decode(&bytes::BytesMut::from(exported.as_slice())).unwrap();
	imp2.finish().unwrap();

	let snap = cat2.snapshot();
	assert_eq!(snap.video.renditions.len(), 1);
	assert_eq!(snap.audio.renditions.len(), 1);

	let v = snap.video.renditions.values().next().unwrap();
	assert!(matches!(v.codec, VideoCodec::H264(_)));
	assert_eq!(v.description.as_ref().map(|b| b.as_ref()), Some(avcc().as_slice()));

	let a = snap.audio.renditions.values().next().unwrap();
	assert!(matches!(a.codec, AudioCodec::AAC(_)));
	assert_eq!(a.sample_rate, 44100);
	assert_eq!(a.description.as_ref().map(|b| b.as_ref()), Some(&ASC[..]));
}

#[tokio::test(start_paused = true)]
async fn export_emits_sequence_headers_and_frames() {
	let mut producer = moq_net::BroadcastInfo::new().produce();
	let consumer = producer.consume();
	let mut catalog = crate::catalog::Producer::new(&mut producer).unwrap();

	let mut importer = Import::new(producer, catalog.clone());
	importer.decode(&bytes::BytesMut::from(synth_flv().as_slice())).unwrap();
	catalog.finish().unwrap();

	let exporter = Export::new(consumer).await.unwrap();
	let exported = drain_export(exporter, importer).await;

	let tags = parse_tags(&exported);

	// One AVC sequence header, one AAC sequence header.
	let avc_seq = tags
		.iter()
		.filter(|t| t.tag_type == super::TAG_VIDEO && t.body[1] == super::AVC_SEQUENCE_HEADER)
		.count();
	let aac_seq = tags
		.iter()
		.filter(|t| t.tag_type == super::TAG_AUDIO && t.body[1] == super::AAC_SEQUENCE_HEADER)
		.count();
	assert_eq!(avc_seq, 1, "expected one AVC sequence header");
	assert_eq!(aac_seq, 1, "expected one AAC sequence header");

	// Two video NALU frames and two raw AAC frames.
	let video_frames = tags
		.iter()
		.filter(|t| t.tag_type == super::TAG_VIDEO && t.body[1] == super::AVC_NALU)
		.count();
	let audio_frames = tags
		.iter()
		.filter(|t| t.tag_type == super::TAG_AUDIO && t.body[1] == super::AAC_RAW)
		.count();
	assert_eq!(video_frames, 2, "expected two video frames");
	assert_eq!(audio_frames, 2, "expected two audio frames");
}

/// A real VP9 key frame (profile 0, 320x240) from the VP9 parser's test vector.
const VP9_KEYFRAME: &[u8] = &[0x82, 0x49, 0x83, 0x42, 0x20, 0x13, 0xf0, 0x0e, 0xf0, 0x00];

/// Build an enhanced-RTMP FLV: VP9 video + Opus audio via the FourCC payloads.
fn synth_enhanced_flv() -> Vec<u8> {
	let head = crate::codec::opus::Config {
		sample_rate: 48000,
		channel_count: 2,
	}
	.encode();

	let mut out = Vec::new();
	out.extend_from_slice(b"FLV");
	out.push(1);
	out.push(0x05);
	out.extend_from_slice(&9u32.to_be_bytes());
	out.extend_from_slice(&0u32.to_be_bytes());

	// Opus sequence start.
	let mut aseq = vec![(super::AUDIO_FORMAT_EX << 4) | super::AUDIO_PACKET_SEQUENCE_START];
	aseq.extend_from_slice(b"Opus");
	aseq.extend_from_slice(&head);
	write_tag(&mut out, super::TAG_AUDIO, 0, &aseq);

	// VP9 key frame (enhanced CodedFrames, no composition time).
	let mut vkey = vec![super::VIDEO_EX_HEADER | (super::FRAME_TYPE_KEY << 4) | super::VIDEO_PACKET_CODED_FRAMES];
	vkey.extend_from_slice(b"vp09");
	vkey.extend_from_slice(VP9_KEYFRAME);
	write_tag(&mut out, super::TAG_VIDEO, 0, &vkey);

	// One Opus frame.
	let mut a0 = vec![(super::AUDIO_FORMAT_EX << 4) | super::AUDIO_PACKET_CODED_FRAMES];
	a0.extend_from_slice(b"Opus");
	a0.extend_from_slice(&[0xfc, 0xff, 0xfe]);
	write_tag(&mut out, super::TAG_AUDIO, 20, &a0);

	out
}

/// Enhanced codecs (VP9 video, Opus audio) survive an import -> export -> import
/// round trip as enhanced-RTMP FourCC payloads.
#[tokio::test(start_paused = true)]
async fn export_roundtrips_enhanced() {
	let mut producer = moq_net::BroadcastInfo::new().produce();
	let consumer = producer.consume();
	let mut catalog = crate::catalog::Producer::new(&mut producer).unwrap();

	let mut importer = Import::new(producer, catalog.clone());
	importer
		.decode(&bytes::BytesMut::from(synth_enhanced_flv().as_slice()))
		.unwrap();
	catalog.finish().unwrap();

	let exporter = Export::new(consumer).await.unwrap();
	let exported = drain_export(exporter, importer).await;

	let tags = parse_tags(&exported);
	// The video frame is an enhanced (FourCC) vp09 tag.
	assert!(
		tags.iter().any(|t| t.tag_type == super::TAG_VIDEO
			&& t.body[0] & super::VIDEO_EX_HEADER != 0
			&& &t.body[1..5] == b"vp09"),
		"expected an enhanced vp09 video tag"
	);
	// The audio carries an enhanced Opus sequence header.
	assert!(
		tags.iter().any(|t| t.tag_type == super::TAG_AUDIO
			&& (t.body[0] >> 4) == super::AUDIO_FORMAT_EX
			&& &t.body[1..5] == b"Opus"),
		"expected an enhanced Opus audio tag"
	);

	// Re-import the exported bytes and confirm the codecs rebuild.
	let mut bcast2 = moq_net::BroadcastInfo::new().produce();
	let cat2 = crate::catalog::Producer::new(&mut bcast2).unwrap();
	let mut imp2 = Import::new(bcast2, cat2.clone());
	imp2.decode(&bytes::BytesMut::from(exported.as_slice())).unwrap();
	imp2.finish().unwrap();

	let snap = cat2.snapshot();
	assert!(matches!(
		snap.video.renditions.values().next().unwrap().codec,
		VideoCodec::VP9(_)
	));
	assert!(matches!(
		snap.audio.renditions.values().next().unwrap().codec,
		AudioCodec::Opus
	));
}

struct ParsedTag {
	tag_type: u8,
	timestamp: u32,
	body: Vec<u8>,
}

/// Parse an FLV byte stream into its tags (skipping the 9-byte file header and
/// every `PreviousTagSize`).
fn parse_tags(flv: &[u8]) -> Vec<ParsedTag> {
	let mut tags = Vec::new();
	let mut off = 9 + 4; // file header + PreviousTagSize0
	while off + 11 <= flv.len() {
		let tag_type = flv[off];
		let size = super::read_u24(&flv[off + 1..off + 4]) as usize;
		let timestamp = super::read_u24(&flv[off + 4..off + 7]) | ((flv[off + 7] as u32) << 24);
		let body_start = off + 11;
		if body_start + size + 4 > flv.len() {
			break;
		}
		tags.push(ParsedTag {
			tag_type,
			timestamp,
			body: flv[body_start..body_start + size].to_vec(),
		});
		off = body_start + size + 4;
	}
	tags
}

/// A frame's tag timestamp must survive the round trip (PTS in milliseconds).
#[tokio::test(start_paused = true)]
async fn export_preserves_timestamps() {
	let mut producer = moq_net::BroadcastInfo::new().produce();
	let consumer = producer.consume();
	let mut catalog = crate::catalog::Producer::new(&mut producer).unwrap();

	let mut importer = Import::new(producer, catalog.clone());
	importer.decode(&bytes::BytesMut::from(synth_flv().as_slice())).unwrap();
	catalog.finish().unwrap();

	let exporter = Export::new(consumer).await.unwrap();
	let exported = drain_export(exporter, importer).await;

	let tags = parse_tags(&exported);
	let video_ts: Vec<u32> = tags
		.iter()
		.filter(|t| t.tag_type == super::TAG_VIDEO && t.body[1] == super::AVC_NALU)
		.map(|t| t.timestamp)
		.collect();
	assert_eq!(video_ts, vec![0, 33]);
}
