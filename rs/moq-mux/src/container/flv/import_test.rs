//! Tests for the FLV demuxer.

use hang::catalog::{AudioCodec, VideoCodec};

use super::Import;

/// A minimal `AVCDecoderConfigurationRecord`: AVC-LC baseline (profile 0x42,
/// level 0x1f) with one SPS and one PPS.
fn avcc() -> Vec<u8> {
	let sps = [0x67u8, 0x42, 0xc0, 0x1f];
	let mut out = vec![0x01, 0x42, 0xc0, 0x1f, 0xff, 0xe1, 0x00, sps.len() as u8];
	out.extend_from_slice(&sps);
	out.extend_from_slice(&[0x01, 0x00, 0x04, 0x68, 0xce, 0x3c, 0x80]); // numPPS, PPS len, PPS
	out
}

/// AudioSpecificConfig for AAC-LC, 44100 Hz, stereo.
const ASC: [u8; 2] = [0x12, 0x10];

/// Append an FLV tag (header + body + trailing PreviousTagSize) to `out`.
fn write_tag(out: &mut Vec<u8>, tag_type: u8, timestamp: u32, body: &[u8]) {
	out.push(tag_type);
	out.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]); // 24-bit data size
	out.extend_from_slice(&timestamp.to_be_bytes()[1..]); // 24-bit timestamp (low)
	out.push((timestamp >> 24) as u8); // timestamp extension
	out.extend_from_slice(&[0, 0, 0]); // stream id
	out.extend_from_slice(body);
	out.extend_from_slice(&(11 + body.len() as u32).to_be_bytes());
}

/// Build a tiny FLV: header, AVC + AAC sequence headers, one keyframe, one audio frame.
fn synth_flv() -> Vec<u8> {
	let mut out = Vec::new();
	out.extend_from_slice(b"FLV");
	out.push(1); // version
	out.push(0x05); // flags: audio | video
	out.extend_from_slice(&9u32.to_be_bytes()); // data offset
	out.extend_from_slice(&0u32.to_be_bytes()); // PreviousTagSize0

	// AVC sequence header.
	let mut vseq = vec![
		(super::FRAME_TYPE_KEY << 4) | super::VIDEO_CODEC_AVC,
		super::AVC_SEQUENCE_HEADER,
		0,
		0,
		0,
	];
	vseq.extend_from_slice(&avcc());
	write_tag(&mut out, super::TAG_VIDEO, 0, &vseq);

	// AAC sequence header.
	let mut aseq = vec![super::AAC_AUDIO_TAG_HEADER, super::AAC_SEQUENCE_HEADER];
	aseq.extend_from_slice(&ASC);
	write_tag(&mut out, super::TAG_AUDIO, 0, &aseq);

	// One keyframe NALU (length-prefixed IDR).
	let nalu = [0, 0, 0, 5, 0x65, 0x88, 0x84, 0x21, 0x00];
	let mut vframe = vec![
		(super::FRAME_TYPE_KEY << 4) | super::VIDEO_CODEC_AVC,
		super::AVC_NALU,
		0,
		0,
		0,
	];
	vframe.extend_from_slice(&nalu);
	write_tag(&mut out, super::TAG_VIDEO, 0, &vframe);

	// One raw AAC frame.
	let mut aframe = vec![super::AAC_AUDIO_TAG_HEADER, super::AAC_RAW];
	aframe.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
	write_tag(&mut out, super::TAG_AUDIO, 0, &aframe);

	out
}

#[tokio::test(start_paused = true)]
async fn import_populates_catalog() {
	let mut producer = moq_net::BroadcastInfo::new().produce();
	let catalog = crate::catalog::Producer::new(&mut producer).unwrap();

	let mut importer = Import::new(producer, catalog.clone());
	let buf = bytes::BytesMut::from(synth_flv().as_slice());
	importer.decode(&buf).unwrap();
	importer.finish().unwrap();

	let snap = catalog.snapshot();
	assert_eq!(snap.video.renditions.len(), 1);
	assert_eq!(snap.audio.renditions.len(), 1);

	let v = snap.video.renditions.values().next().unwrap();
	assert!(matches!(v.codec, VideoCodec::H264(_)));
	assert_eq!(v.description.as_ref().map(|b| b.as_ref()), Some(avcc().as_slice()));

	let a = snap.audio.renditions.values().next().unwrap();
	assert!(matches!(a.codec, AudioCodec::AAC(_)));
	assert_eq!(a.sample_rate, 44100);
	assert_eq!(a.channel_count, 2);
	assert_eq!(a.description.as_ref().map(|b| b.as_ref()), Some(&ASC[..]));
}

#[tokio::test(start_paused = true)]
async fn import_emits_frames() {
	let mut producer = moq_net::BroadcastInfo::new().produce();
	let consumer = producer.consume();
	let catalog = crate::catalog::Producer::new(&mut producer).unwrap();

	let mut importer = Import::new(producer, catalog.clone());
	let buf = bytes::BytesMut::from(synth_flv().as_slice());
	importer.decode(&buf).unwrap();
	importer.finish().unwrap();

	let snap = catalog.snapshot();
	let video_name = snap.video.renditions.keys().next().unwrap().clone();

	// Decode the video track back through the Legacy container.
	let track = consumer
		.track(&video_name)
		.unwrap()
		.subscribe(None)
		.unwrap()
		.await
		.unwrap();
	let mut decoder = crate::container::Consumer::new(track, crate::catalog::hang::Container::Legacy)
		.with_latency(std::time::Duration::from_secs(1));
	let frame = decoder.read().await.unwrap().expect("a video frame");
	assert!(frame.keyframe);
	// The payload is the length-prefixed NALU, carried through verbatim.
	assert_eq!(frame.payload.as_ref(), &[0, 0, 0, 5, 0x65, 0x88, 0x84, 0x21, 0x00]);

	drop(importer);
}

/// Bytes split across two `decode` calls still reassemble into whole tags.
#[tokio::test(start_paused = true)]
async fn import_handles_split_input() {
	let flv = synth_flv();
	let (head, tail) = flv.split_at(flv.len() / 2);

	let mut producer = moq_net::BroadcastInfo::new().produce();
	let catalog = crate::catalog::Producer::new(&mut producer).unwrap();

	let mut importer = Import::new(producer, catalog.clone());
	importer.decode(&bytes::BytesMut::from(head)).unwrap();
	importer.decode(&bytes::BytesMut::from(tail)).unwrap();
	importer.finish().unwrap();

	let snap = catalog.snapshot();
	assert_eq!(snap.video.renditions.len(), 1);
	assert_eq!(snap.audio.renditions.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn import_rejects_non_flv() {
	let mut producer = moq_net::BroadcastInfo::new().produce();
	let catalog = crate::catalog::Producer::new(&mut producer).unwrap();

	let mut importer = Import::new(producer, catalog);
	let buf = bytes::BytesMut::from(&b"NOTFLV\x00\x00\x00"[..]);
	assert!(importer.decode(&buf).is_err());
}
