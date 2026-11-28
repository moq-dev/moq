//! Tests for the FLV demuxer.

use hang::catalog::{AudioCodec, VideoCodec};

use super::Import;
use std::time::Duration;

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
const AV1C: [u8; 4] = [0x81, 0x08, 0x0c, 0x00];
const AC3_FRAME: [u8; 7] = [0x0B, 0x77, 0x00, 0x00, 0x1C, 0x40, 0xE1];
const EAC3_FRAME: [u8; 6] = [0x0B, 0x77, 0x00, 0xFF, 0x3F, 0x80];

fn flv_header(flags: u8) -> Vec<u8> {
	let mut out = Vec::new();
	out.extend_from_slice(b"FLV");
	out.push(1);
	out.push(flags);
	out.extend_from_slice(&9u32.to_be_bytes());
	out.extend_from_slice(&0u32.to_be_bytes());
	out
}

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
	let mut producer = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut producer, crate::catalog::Config::default()).unwrap();

	let mut importer = Import::new(producer, catalog.reserve());
	let buf = bytes::BytesMut::from(synth_flv().as_slice());
	importer.decode(&buf).unwrap();
	let _finished = importer.finish().unwrap();

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
	let mut producer = moq_net::broadcast::Info::new().produce();
	let consumer = producer.consume();
	let catalog = crate::catalog::Producer::new(&mut producer, crate::catalog::Config::default()).unwrap();

	let mut importer = Import::new(producer, catalog.reserve());
	let buf = bytes::BytesMut::from(synth_flv().as_slice());
	importer.decode(&buf).unwrap();
	let _finished = importer.finish().unwrap();

	let snap = catalog.snapshot();
	let video_name = snap.video.renditions.keys().next().unwrap().clone();

	// Decode the video track back through the Legacy container.
	let track = consumer
		.track(&video_name)
		.unwrap()
		.subscribe(
			moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
				moq_net::track::Start::Live {
					min: Some(Duration::from_secs(1)),
					max: Some(Duration::from_secs(1)),
				},
			)]),
		)
		.await
		.unwrap();
	let mut decoder = crate::container::Consumer::new(
		track,
		crate::catalog::hang::Container::Legacy(crate::container::Kind::Data),
	);
	let frame = decoder.read_frame().await.unwrap().expect("a video frame");
	assert!(frame.keyframe);
	// The payload is the length-prefixed NALU, carried through verbatim.
	assert_eq!(frame.payload.as_ref(), &[0, 0, 0, 5, 0x65, 0x88, 0x84, 0x21, 0x00]);
}

#[tokio::test(start_paused = true)]
async fn public_container_preserves_loc_for_flv() {
	let data = synth_flv();
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let reserved = catalog.reserve();
	let mut import = super::Import::new(broadcast, reserved).with_container(hang::catalog::Container::Loc);
	import.decode(&data).unwrap();
	let _finished = import.finish().unwrap();

	let snapshot = catalog.snapshot();
	let (name, config) = snapshot.video.renditions.iter().next().unwrap();
	assert_eq!(config.container, hang::catalog::Container::Loc);

	let track = consumer.track(name).unwrap().subscribe(None).await.unwrap();
	let mut media = crate::container::Consumer::new(
		track,
		crate::catalog::hang::Container::Loc(crate::container::Kind::Data),
	);
	let frame = kio::clock::timeout(std::time::Duration::from_secs(1), media.read_frame())
		.await
		.unwrap()
		.unwrap()
		.unwrap();
	assert_eq!(frame.payload.as_ref(), &[0, 0, 0, 5, 0x65, 0x88, 0x84, 0x21, 0x00]);
}

/// Bytes split across two `decode` calls still reassemble into whole tags.
#[tokio::test(start_paused = true)]
async fn import_handles_split_input() {
	let flv = synth_flv();
	let (head, tail) = flv.split_at(flv.len() / 2);

	let mut producer = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut producer, crate::catalog::Config::default()).unwrap();

	let mut importer = Import::new(producer, catalog.reserve());
	importer.decode(&bytes::BytesMut::from(head)).unwrap();
	importer.decode(&bytes::BytesMut::from(tail)).unwrap();
	let _finished = importer.finish().unwrap();

	let snap = catalog.snapshot();
	assert_eq!(snap.video.renditions.len(), 1);
	assert_eq!(snap.audio.renditions.len(), 1);
}

/// A real VP9 key frame (profile 0, 320x240), borrowed from the VP9 parser's
/// own test vector. Bytes after the frame size are irrelevant to the header.
const VP9_KEYFRAME_320X240: &[u8] = &[0x82, 0x49, 0x83, 0x42, 0x20, 0x13, 0xf0, 0x0e, 0xf0, 0x00];

/// Enhanced-RTMP (FourCC) VP9 video configures from the key frame and emits it.
#[tokio::test(start_paused = true)]
async fn import_enhanced_vp9() {
	let mut out = Vec::new();
	out.extend_from_slice(b"FLV");
	out.push(1);
	out.push(0x01); // video only
	out.extend_from_slice(&9u32.to_be_bytes());
	out.extend_from_slice(&0u32.to_be_bytes());

	// Ex-video CodedFrames keyframe: high bit set, frame type 1, packet type 1.
	let first = super::VIDEO_EX_HEADER | (super::FRAME_TYPE_KEY << 4) | super::VIDEO_PACKET_CODED_FRAMES;
	let mut body = vec![first];
	body.extend_from_slice(b"vp09");
	body.extend_from_slice(VP9_KEYFRAME_320X240);
	write_tag(&mut out, super::TAG_VIDEO, 0, &body);

	let mut producer = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut producer, crate::catalog::Config::default()).unwrap();
	let mut importer = Import::new(producer, catalog.reserve());
	importer.decode(&bytes::BytesMut::from(out.as_slice())).unwrap();
	let _finished = importer.finish().unwrap();

	let snap = catalog.snapshot();
	assert_eq!(snap.video.renditions.len(), 1);
	let v = snap.video.renditions.values().next().unwrap();
	assert!(matches!(v.codec, VideoCodec::VP9(_)));
	assert_eq!(v.coded_width, Some(320));
	assert_eq!(v.coded_height, Some(240));
}

/// Enhanced-RTMP (FourCC) Opus audio configures from the `OpusHead` sequence
/// header and carries the frames through.
#[tokio::test(start_paused = true)]
async fn import_enhanced_opus() {
	// A 44.1 kHz input rate is informational; the catalog still reports the decoder's 48 kHz.
	let head = crate::codec::opus::Config::new(44_100, 2).encode().unwrap();

	let mut out = Vec::new();
	out.extend_from_slice(b"FLV");
	out.push(1);
	out.push(0x04); // audio only
	out.extend_from_slice(&9u32.to_be_bytes());
	out.extend_from_slice(&0u32.to_be_bytes());

	// Ex-audio SequenceStart: SoundFormat 9, packet type 0.
	let mut seq = vec![(super::AUDIO_FORMAT_EX << 4) | super::AUDIO_PACKET_SEQUENCE_START];
	seq.extend_from_slice(b"Opus");
	seq.extend_from_slice(&head);
	write_tag(&mut out, super::TAG_AUDIO, 0, &seq);

	// Ex-audio CodedFrames: SoundFormat 9, packet type 1.
	let mut frame = vec![(super::AUDIO_FORMAT_EX << 4) | super::AUDIO_PACKET_CODED_FRAMES];
	frame.extend_from_slice(b"Opus");
	frame.extend_from_slice(&[0xfc, 0xff, 0xfe]);
	write_tag(&mut out, super::TAG_AUDIO, 20, &frame);

	let mut producer = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut producer, crate::catalog::Config::default()).unwrap();
	let mut importer = Import::new(producer, catalog.reserve());
	importer.decode(&bytes::BytesMut::from(out.as_slice())).unwrap();
	let _finished = importer.finish().unwrap();

	let snap = catalog.snapshot();
	assert_eq!(snap.audio.renditions.len(), 1);
	let a = snap.audio.renditions.values().next().unwrap();
	assert!(matches!(a.codec, AudioCodec::Opus));
	assert_eq!(a.sample_rate, 48000);
	assert_eq!(a.channel_count, 2);
	assert_eq!(a.description.as_ref().map(|b| b.as_ref()), Some(head.as_ref()));
}

/// Legacy SoundFormat 2 MP3 configures from the first frame's in-band header and
/// carries the frame through verbatim.
#[tokio::test(start_paused = true)]
async fn import_legacy_mp3() {
	// MPEG-1 Layer III, 128 kbps, 44.1 kHz, joint stereo, padded to a plausible frame.
	let mut mp3 = vec![0xFF, 0xFB, 0x90, 0x44];
	mp3.resize(417, 0xAA);

	let mut out = Vec::new();
	out.extend_from_slice(b"FLV");
	out.push(1);
	out.push(0x04); // audio only
	out.extend_from_slice(&9u32.to_be_bytes());
	out.extend_from_slice(&0u32.to_be_bytes());

	// Legacy audio tag: SoundFormat 2 (MP3) header byte, then the raw frame.
	let mut tag = vec![super::MP3_AUDIO_TAG_HEADER];
	tag.extend_from_slice(&mp3);
	write_tag(&mut out, super::TAG_AUDIO, 0, &tag);

	let mut producer = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut producer, crate::catalog::Config::default()).unwrap();
	let mut importer = Import::new(producer, catalog.reserve());
	importer.decode(&bytes::BytesMut::from(out.as_slice())).unwrap();
	let _finished = importer.finish().unwrap();

	let snap = catalog.snapshot();
	assert_eq!(snap.audio.renditions.len(), 1);
	let a = snap.audio.renditions.values().next().unwrap();
	assert!(matches!(a.codec, AudioCodec::Mp3));
	assert_eq!(a.sample_rate, 44100);
	assert_eq!(a.channel_count, 2);
	assert!(a.description.is_none(), "MP3 config is in band");
}

/// A legacy MP3 frame header that changes the channel count is a new decoder identity, so the
/// frames after it publish on a new track rather than rewriting the first.
#[tokio::test(start_paused = true)]
async fn legacy_mp3_channel_change_starts_a_new_track() {
	// MPEG-1 Layer III, 128 kbps, 44.1 kHz: joint stereo, then mono.
	let frame = |mode: u8| {
		let mut mp3 = vec![0xFF, 0xFB, 0x90, mode];
		mp3.resize(417, 0xAA);
		let mut tag = vec![super::MP3_AUDIO_TAG_HEADER];
		tag.extend_from_slice(&mp3);
		tag
	};
	let header = || {
		let mut out = Vec::new();
		out.extend_from_slice(b"FLV");
		out.push(1);
		out.push(0x04); // audio only
		out.extend_from_slice(&9u32.to_be_bytes());
		out.extend_from_slice(&0u32.to_be_bytes());
		out
	};

	let mut producer = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut producer, crate::catalog::Config::default()).unwrap();
	let mut importer = Import::new(producer, catalog.reserve());
	let mut out = header();
	write_tag(&mut out, super::TAG_AUDIO, 0, &frame(0x44));
	write_tag(&mut out, super::TAG_AUDIO, 26, &frame(0x44));
	importer.decode(&bytes::BytesMut::from(out.as_slice())).unwrap();
	let first = catalog.snapshot().audio.renditions.keys().next().unwrap().clone();

	let mut out = Vec::new();
	write_tag(&mut out, super::TAG_AUDIO, 52, &frame(0xC4));
	importer.decode(&bytes::BytesMut::from(out.as_slice())).unwrap();
	let snap = catalog.snapshot();
	assert_eq!(snap.audio.renditions.len(), 1, "the stereo track is retired");
	let (name, a) = snap.audio.renditions.iter().next().unwrap();
	assert_ne!(name, &first, "the mono frames publish under a new name");
	assert_eq!(a.channel_count, 1);
	let _finished = importer.finish().unwrap();
}

/// A minimal avcC like [`avcc`], but with a caller-chosen level byte so two video
/// renditions carry distinct codec configs.
fn avcc_level(level: u8) -> Vec<u8> {
	let sps = [0x67u8, 0x42, 0xc0, level];
	let mut out = vec![0x01, 0x42, 0xc0, level, 0xff, 0xe1, 0x00, sps.len() as u8];
	out.extend_from_slice(&sps);
	out.extend_from_slice(&[0x01, 0x00, 0x04, 0x68, 0xce, 0x3c, 0x80]);
	out
}

/// Build one enhanced-RTMP multitrack video tag body: the ex-header + framing
/// byte, the shared FourCC (unless per-track), then each track's optional FourCC,
/// one-byte id, UI24 size (omitted for `OneTrack`), and payload.
fn multitrack_video_body(
	frame_type: u8,
	multitrack_type: u8,
	packet_type: u8,
	shared_fourcc: Option<&[u8; 4]>,
	tracks: &[(u8, [u8; 4], Vec<u8>)],
) -> Vec<u8> {
	let mut body = vec![super::VIDEO_EX_HEADER | (frame_type << 4) | super::VIDEO_PACKET_MULTITRACK];
	body.push((multitrack_type << 4) | packet_type);
	if let Some(fourcc) = shared_fourcc {
		body.extend_from_slice(fourcc);
	}
	for (track_id, fourcc, payload) in tracks {
		if shared_fourcc.is_none() {
			body.extend_from_slice(fourcc);
		}
		body.push(*track_id);
		if multitrack_type != super::MULTITRACK_ONE_TRACK {
			body.extend_from_slice(&(payload.len() as u32).to_be_bytes()[1..]); // UI24 size
		}
		body.extend_from_slice(payload);
	}
	body
}

/// A `ManyTracks` multitrack tag (several length-prefixed tracks sharing one
/// codec in a single tag) demuxes into one rendition per track id.
#[tokio::test(start_paused = true)]
async fn import_multitrack_video_many_tracks() {
	let (avcc0, avcc1) = (avcc_level(0x1f), avcc_level(0x1e));

	let mut out = Vec::new();
	out.extend_from_slice(b"FLV");
	out.push(1);
	out.push(0x01); // video only
	out.extend_from_slice(&9u32.to_be_bytes());
	out.extend_from_slice(&0u32.to_be_bytes());

	// Two-track SequenceStart sharing the avc1 codec.
	let seq = multitrack_video_body(
		super::FRAME_TYPE_KEY,
		super::MULTITRACK_MANY_TRACKS,
		super::VIDEO_PACKET_SEQUENCE_START,
		Some(b"avc1"),
		&[(0, *b"avc1", avcc0.clone()), (1, *b"avc1", avcc1.clone())],
	);
	write_tag(&mut out, super::TAG_VIDEO, 0, &seq);

	// Two-track CodedFrames: avc1 prefixes a 3-byte composition time (zero here).
	let nalu0 = vec![0, 0, 0, 0, 0, 5, 0x65, 0x88, 0x84, 0x21, 0x00];
	let nalu1 = vec![0, 0, 0, 0, 0, 5, 0x65, 0x11, 0x22, 0x33, 0x44];
	let frames = multitrack_video_body(
		super::FRAME_TYPE_KEY,
		super::MULTITRACK_MANY_TRACKS,
		super::VIDEO_PACKET_CODED_FRAMES,
		Some(b"avc1"),
		&[(0, *b"avc1", nalu0), (1, *b"avc1", nalu1)],
	);
	write_tag(&mut out, super::TAG_VIDEO, 0, &frames);

	let mut producer = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut producer, crate::catalog::Config::default()).unwrap();
	let mut importer = Import::new(producer, catalog.reserve());
	importer.decode(&bytes::BytesMut::from(out.as_slice())).unwrap();
	let _finished = importer.finish().unwrap();

	let snap = catalog.snapshot();
	assert_eq!(snap.video.renditions.len(), 2, "one rendition per multitrack track id");
	let mut descriptions: Vec<Vec<u8>> = snap
		.video
		.renditions
		.values()
		.filter_map(|c| c.description.as_ref().map(|b| b.to_vec()))
		.collect();
	descriptions.sort();
	let mut want = vec![avcc0, avcc1];
	want.sort();
	assert_eq!(descriptions, want, "each track keeps its own avcC");
}

/// A `ManyTracksManyCodecs` tag carries a FourCC per track; both are demuxed.
#[tokio::test(start_paused = true)]
async fn import_multitrack_video_many_codecs() {
	let mut out = Vec::new();
	out.extend_from_slice(b"FLV");
	out.push(1);
	out.push(0x01);
	out.extend_from_slice(&9u32.to_be_bytes());
	out.extend_from_slice(&0u32.to_be_bytes());

	// Two tracks, each carrying its own FourCC (both avc1 here for a simple config).
	let seq = multitrack_video_body(
		super::FRAME_TYPE_KEY,
		super::MULTITRACK_MANY_TRACKS_MANY_CODECS,
		super::VIDEO_PACKET_SEQUENCE_START,
		None,
		&[(0, *b"avc1", avcc_level(0x1f)), (1, *b"avc1", avcc_level(0x1e))],
	);
	write_tag(&mut out, super::TAG_VIDEO, 0, &seq);

	// A record publishes at its track's first keyframe.
	let idr = vec![0, 0, 0, 0, 0, 5, 0x65, 0x88, 0x84, 0x21, 0x00];
	let frames = multitrack_video_body(
		super::FRAME_TYPE_KEY,
		super::MULTITRACK_MANY_TRACKS_MANY_CODECS,
		super::VIDEO_PACKET_CODED_FRAMES,
		None,
		&[(0, *b"avc1", idr.clone()), (1, *b"avc1", idr)],
	);
	write_tag(&mut out, super::TAG_VIDEO, 0, &frames);

	let mut producer = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut producer, crate::catalog::Config::default()).unwrap();
	let mut importer = Import::new(producer, catalog.reserve());
	importer.decode(&bytes::BytesMut::from(out.as_slice())).unwrap();
	let _finished = importer.finish().unwrap();

	assert_eq!(catalog.snapshot().video.renditions.len(), 2);
}

/// FLV video tags: an AVC sequence header carrying `avcc()`, then one keyframe of `nalus`.
fn avc_session(nalus: &[u8]) -> (Vec<u8>, Vec<u8>) {
	let mut headers = flv_header(0x05);
	let mut vseq = vec![
		(super::FRAME_TYPE_KEY << 4) | super::VIDEO_CODEC_AVC,
		super::AVC_SEQUENCE_HEADER,
		0,
		0,
		0,
	];
	vseq.extend_from_slice(&avcc());
	write_tag(&mut headers, super::TAG_VIDEO, 0, &vseq);
	let mut aseq = vec![super::AAC_AUDIO_TAG_HEADER, super::AAC_SEQUENCE_HEADER];
	aseq.extend_from_slice(&ASC);
	write_tag(&mut headers, super::TAG_AUDIO, 0, &aseq);

	let mut keyframe = Vec::new();
	let mut vframe = vec![
		(super::FRAME_TYPE_KEY << 4) | super::VIDEO_CODEC_AVC,
		super::AVC_NALU,
		0,
		0,
		0,
	];
	vframe.extend_from_slice(nalus);
	write_tag(&mut keyframe, super::TAG_VIDEO, 0, &vframe);
	(headers, keyframe)
}

/// An avcC whose keyframes repeat the SPS in band lands as `avc3`; one whose keyframes don't stays
/// `avc1`. The record holds the catalog until the keyframe, even past an earlier audio frame.
#[tokio::test(start_paused = true)]
async fn avc_keyframe_decides_where_parameter_sets_live() {
	let idr = [0, 0, 0, 5, 0x65, 0x88, 0x84, 0x21, 0x00];
	let mut in_band = vec![0, 0, 0, 4, 0x67, 0x42, 0xc0, 0x1f];
	in_band.extend_from_slice(&idr);

	for (nalus, inline) in [(idr.to_vec(), false), (in_band, true)] {
		let (headers, keyframe) = avc_session(&nalus);
		let mut audio = Vec::new();
		write_tag(
			&mut audio,
			super::TAG_AUDIO,
			0,
			&[super::AAC_AUDIO_TAG_HEADER, super::AAC_RAW, 0xde, 0xad],
		);

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let mut clocks = crate::container::test_util::Clocks::subscribe(&consumer).await;
		let mut importer = Import::new(broadcast, catalog.reserve());

		importer.decode(&headers).unwrap();
		importer.decode(&audio).unwrap();
		assert_eq!(
			clocks.drain(),
			vec![],
			"the record withholds the catalog until its keyframe"
		);

		importer.decode(&keyframe).unwrap();
		assert!(!clocks.drain().is_empty(), "the keyframe publishes the catalog");
		let _finished = importer.finish().unwrap();

		let snap = catalog.snapshot();
		assert_eq!(snap.audio.renditions.len(), 1);
		let video = snap.video.renditions.values().next().expect("a video rendition");
		let VideoCodec::H264(codec) = &video.codec else {
			panic!("{:?}", video.codec);
		};
		assert_eq!(codec.inline, inline, "{}", video.codec);
		assert_eq!(video.description.as_deref(), Some(avcc().as_slice()));
	}
}

/// A publisher that keeps changing its audio and video config replaces each track under a fresh
/// name. A replaced track flushes its last frame and ends, and the importer retires it, so the HLS
/// records it holds stay those of the live renditions. Dropping the importer ends and retires those.
#[tokio::test(start_paused = true)]
async fn config_churn_keeps_flat_state() {
	const NALU: [u8; 9] = [0, 0, 0, 5, 0x65, 0x88, 0x84, 0x21, 0x00];
	// AAC-LC stereo at 44.1 kHz, then at 48 kHz.
	let ascs = [ASC, [0x11, 0x90]];
	let avccs = [avcc_level(0x1f), avcc_level(0x1e)];
	// One config change on both tracks, each followed by a frame.
	let round = |round: u32| {
		let mut out = Vec::new();
		let config = round as usize % 2;
		let timestamp = round * 40;
		let video = (super::FRAME_TYPE_KEY << 4) | super::VIDEO_CODEC_AVC;

		let mut vseq = vec![video, super::AVC_SEQUENCE_HEADER, 0, 0, 0];
		vseq.extend_from_slice(&avccs[config]);
		write_tag(&mut out, super::TAG_VIDEO, timestamp, &vseq);
		let mut aseq = vec![super::AAC_AUDIO_TAG_HEADER, super::AAC_SEQUENCE_HEADER];
		aseq.extend_from_slice(&ascs[config]);
		write_tag(&mut out, super::TAG_AUDIO, timestamp, &aseq);

		let mut vframe = vec![video, super::AVC_NALU, 0, 0, 0];
		vframe.extend_from_slice(&NALU);
		write_tag(&mut out, super::TAG_VIDEO, timestamp, &vframe);
		let aframe = [super::AAC_AUDIO_TAG_HEADER, super::AAC_RAW, 0x21, 0x00];
		write_tag(&mut out, super::TAG_AUDIO, timestamp, &aframe);
		out
	};

	let mut producer = moq_net::broadcast::Info::new().produce();
	let consumer = producer.consume();
	let catalog = crate::catalog::Producer::new(&mut producer, crate::catalog::Config::default()).unwrap();
	let mut importer = Import::new(producer, catalog.reserve());
	let mut first = flv_header(0x05);
	first.extend_from_slice(&round(0));
	importer.decode(&first).unwrap();
	let replaced = catalog.snapshot().video.renditions.into_keys().next().unwrap();
	let replaced = consumer
		.track(&replaced)
		.unwrap()
		.subscribe(
			moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
				moq_net::track::Start::Live {
					min: Some(Duration::from_secs(1)),
					max: Some(Duration::from_secs(1)),
				},
			)]),
		)
		.await
		.unwrap();

	for n in 1..100 {
		importer.decode(&round(n)).unwrap();
	}
	let snap = catalog.snapshot();
	assert_eq!(snap.video.renditions.len(), 1);
	assert_eq!(snap.audio.renditions.len(), 1);

	let live = consumer
		.track(snap.video.renditions.keys().next().unwrap())
		.unwrap()
		.subscribe(
			moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
				moq_net::track::Start::Live {
					min: Some(Duration::from_secs(1)),
					max: Some(Duration::from_secs(1)),
				},
			)]),
		)
		.await
		.unwrap();
	drop(importer);

	for (track, label) in [(replaced, "replaced"), (live, "dropped")] {
		let mut track = crate::container::Consumer::new(
			track,
			crate::catalog::hang::Container::Legacy(crate::container::Kind::Data),
		);
		let frame = track.read_frame().await.unwrap();
		assert_eq!(frame.expect(label).payload.as_ref(), &NALU, "{label}");
		assert!(track.read_frame().await.unwrap().is_none(), "the {label} track ends");
	}
}

#[tokio::test(start_paused = true)]
async fn import_enhanced_av1() {
	let mut out = flv_header(0x01);

	let mut seq = vec![super::VIDEO_EX_HEADER | (super::FRAME_TYPE_KEY << 4) | super::VIDEO_PACKET_SEQUENCE_START];
	seq.extend_from_slice(b"av01");
	seq.extend_from_slice(&AV1C);
	write_tag(&mut out, super::TAG_VIDEO, 0, &seq);

	let payload = [0x12, 0x00, 0x34, 0x56];
	let mut frame = vec![super::VIDEO_EX_HEADER | (super::FRAME_TYPE_KEY << 4) | super::VIDEO_PACKET_CODED_FRAMES_X];
	frame.extend_from_slice(b"av01");
	frame.extend_from_slice(&payload);
	write_tag(&mut out, super::TAG_VIDEO, 33, &frame);

	let mut producer = moq_net::broadcast::Info::new().produce();
	let consumer = producer.consume();
	let catalog = crate::catalog::Producer::new(&mut producer, crate::catalog::Config::default()).unwrap();
	let mut importer = Import::new(producer, catalog.reserve());
	importer.decode(&bytes::BytesMut::from(out.as_slice())).unwrap();
	let _finished = importer.finish().unwrap();

	let snap = catalog.snapshot();
	let (name, v) = snap.video.renditions.iter().next().unwrap();
	assert!(matches!(v.codec, VideoCodec::AV1(_)));
	assert_eq!(v.description.as_ref().map(|b| b.as_ref()), Some(&AV1C[..]));

	let track = consumer
		.track(name)
		.unwrap()
		.subscribe(
			moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
				moq_net::track::Start::Live {
					min: Some(Duration::from_secs(1)),
					max: Some(Duration::from_secs(1)),
				},
			)]),
		)
		.await
		.unwrap();
	let mut decoder = crate::container::Consumer::new(
		track,
		crate::catalog::hang::Container::Legacy(crate::container::Kind::Data),
	);
	let frame = decoder.read_frame().await.unwrap().expect("an AV1 frame");
	assert!(frame.keyframe);
	assert_eq!(frame.payload.as_ref(), payload);
}

/// GStreamer 1.28 `fdkaacenc` output at 48 kHz stereo, remuxed to FLV by ffmpeg 9.0.1, whose
/// AudioSpecificConfig signals SBR (and PS for v2) over a 24 kHz LC core. The catalog names the
/// output, as ffprobe does, not the core.
#[tokio::test(start_paused = true)]
async fn import_explicit_sbr_names_its_output() {
	let fixtures: [(&[u8], u8); 2] = [
		(include_bytes!("../ts/test_data/he_aac.flv"), 5),
		(include_bytes!("../ts/test_data/he_aac_v2.flv"), 29),
	];
	for (data, profile) in fixtures {
		let mut producer = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut producer, crate::catalog::Config::default()).unwrap();
		let mut importer = Import::new(producer, catalog.reserve());
		importer.decode(&bytes::BytesMut::from(data)).unwrap();
		let _finished = importer.finish().unwrap();

		let snap = catalog.snapshot();
		let a = snap.audio.renditions.values().next().expect("an AAC track");
		assert!(matches!(&a.codec, AudioCodec::AAC(aac) if aac.profile == profile));
		assert_eq!(a.sample_rate, 48_000);
		assert_eq!(a.channel_count, 2);
	}
}

#[tokio::test(start_paused = true)]
async fn import_enhanced_ac3() {
	let mut out = flv_header(0x04);
	let mut frame = vec![(super::AUDIO_FORMAT_EX << 4) | super::AUDIO_PACKET_CODED_FRAMES];
	frame.extend_from_slice(b"ac-3");
	frame.extend_from_slice(&AC3_FRAME);
	write_tag(&mut out, super::TAG_AUDIO, 0, &frame);

	let mut producer = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut producer, crate::catalog::Config::default()).unwrap();
	let mut importer = Import::new(producer, catalog.reserve());
	importer.decode(&bytes::BytesMut::from(out.as_slice())).unwrap();
	let _finished = importer.finish().unwrap();

	let snap = catalog.snapshot();
	let a = snap.audio.renditions.values().next().unwrap();
	assert!(matches!(a.codec, AudioCodec::Ac3));
	assert_eq!(a.sample_rate, 48_000);
	assert_eq!(a.channel_count, 6);
	assert!(a.description.is_none());
}

#[tokio::test(start_paused = true)]
async fn import_enhanced_eac3() {
	let mut out = flv_header(0x04);
	let mut frame = vec![(super::AUDIO_FORMAT_EX << 4) | super::AUDIO_PACKET_CODED_FRAMES];
	frame.extend_from_slice(b"ec-3");
	frame.extend_from_slice(&EAC3_FRAME);
	write_tag(&mut out, super::TAG_AUDIO, 0, &frame);

	let mut producer = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut producer, crate::catalog::Config::default()).unwrap();
	let mut importer = Import::new(producer, catalog.reserve());
	importer.decode(&bytes::BytesMut::from(out.as_slice())).unwrap();
	let _finished = importer.finish().unwrap();

	let snap = catalog.snapshot();
	let a = snap.audio.renditions.values().next().unwrap();
	assert!(matches!(a.codec, AudioCodec::Ec3));
	assert_eq!(a.sample_rate, 48_000);
	assert_eq!(a.channel_count, 6);
	assert!(a.description.is_none());
}

#[tokio::test(start_paused = true)]
async fn import_reports_negative_pts_and_can_resume() {
	let mut out = flv_header(0x01);

	let mut seq = vec![
		(super::FRAME_TYPE_KEY << 4) | super::VIDEO_CODEC_AVC,
		super::AVC_SEQUENCE_HEADER,
		0,
		0,
		0,
	];
	seq.extend_from_slice(&avcc());
	write_tag(&mut out, super::TAG_VIDEO, 0, &seq);

	let mut negative = vec![
		(super::FRAME_TYPE_KEY << 4) | super::VIDEO_CODEC_AVC,
		super::AVC_NALU,
		0xff,
		0xff,
		0xff,
	];
	negative.extend_from_slice(&[0, 0, 0, 1, 0x65]);
	write_tag(&mut out, super::TAG_VIDEO, 0, &negative);

	let mut good = vec![
		(super::FRAME_TYPE_KEY << 4) | super::VIDEO_CODEC_AVC,
		super::AVC_NALU,
		0,
		0,
		0,
	];
	good.extend_from_slice(&[0, 0, 0, 1, 0x65]);
	write_tag(&mut out, super::TAG_VIDEO, 10, &good);

	let mut producer = moq_net::broadcast::Info::new().produce();
	let consumer = producer.consume();
	let catalog = crate::catalog::Producer::new(&mut producer, crate::catalog::Config::default()).unwrap();
	let mut importer = Import::new(producer, catalog.reserve());
	let err = importer.decode(&bytes::BytesMut::from(out.as_slice())).unwrap_err();
	assert!(matches!(
		err,
		crate::Error::NegativeFlvPts {
			dts_ms: 0,
			composition_time_ms: -1
		}
	));
	importer.decode(&[]).unwrap();
	let _finished = importer.finish().unwrap();

	let snap = catalog.snapshot();
	let name = snap.video.renditions.keys().next().unwrap();
	let track = consumer
		.track(name)
		.unwrap()
		.subscribe(
			moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
				moq_net::track::Start::Live {
					min: Some(Duration::from_secs(1)),
					max: Some(Duration::from_secs(1)),
				},
			)]),
		)
		.await
		.unwrap();
	let mut decoder = crate::container::Consumer::new(
		track,
		crate::catalog::hang::Container::Legacy(crate::container::Kind::Data),
	);
	let frame = decoder.read_frame().await.unwrap().expect("the good frame");
	assert_eq!(frame.timestamp.as_millis().unwrap(), 10);
}

#[tokio::test(start_paused = true)]
async fn import_enhanced_hvc1_applies_composition_time() {
	let mut out = flv_header(0x01);

	// Use avc1 to bootstrap a video track with a tiny config record; the coded
	// frame below uses hvc1, which is the branch this regression covers.
	let mut seq = vec![super::VIDEO_EX_HEADER | (super::FRAME_TYPE_KEY << 4) | super::VIDEO_PACKET_SEQUENCE_START];
	seq.extend_from_slice(b"avc1");
	seq.extend_from_slice(&avcc());
	write_tag(&mut out, super::TAG_VIDEO, 0, &seq);

	let mut coded = vec![super::VIDEO_EX_HEADER | (super::FRAME_TYPE_KEY << 4) | super::VIDEO_PACKET_CODED_FRAMES];
	coded.extend_from_slice(b"hvc1");
	coded.extend_from_slice(&[0, 0, 7]);
	coded.extend_from_slice(&[0, 0, 0, 1, 0x65]);
	write_tag(&mut out, super::TAG_VIDEO, 10, &coded);

	let mut producer = moq_net::broadcast::Info::new().produce();
	let consumer = producer.consume();
	let catalog = crate::catalog::Producer::new(&mut producer, crate::catalog::Config::default()).unwrap();
	let mut importer = Import::new(producer, catalog.reserve());
	importer.decode(&bytes::BytesMut::from(out.as_slice())).unwrap();
	let _finished = importer.finish().unwrap();

	let snap = catalog.snapshot();
	let name = snap.video.renditions.keys().next().unwrap();
	let track = consumer
		.track(name)
		.unwrap()
		.subscribe(
			moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
				moq_net::track::Start::Live {
					min: Some(Duration::from_secs(1)),
					max: Some(Duration::from_secs(1)),
				},
			)]),
		)
		.await
		.unwrap();
	let mut decoder = crate::container::Consumer::new(
		track,
		crate::catalog::hang::Container::Legacy(crate::container::Kind::Data),
	);
	let frame = decoder.read_frame().await.unwrap().expect("a video frame");
	assert_eq!(frame.timestamp.as_millis().unwrap(), 17);
}

#[tokio::test(start_paused = true)]
async fn import_rejects_non_flv() {
	let mut producer = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut producer, crate::catalog::Config::default()).unwrap();

	let mut importer = Import::new(producer, catalog.reserve());
	let buf = bytes::BytesMut::from(&b"NOTFLV\x00\x00\x00"[..]);
	assert!(importer.decode(&buf).is_err());
}

/// One encoder session: sequence headers (after the file header when `header`), then `frames`
/// video frames 40ms apart whose composition offsets reorder like B-frames, interleaved with AAC
/// frames up to 300ms earlier in PTS, the way a muxer leads audio.
fn session(header: bool, start_ms: u32, frames: u32) -> Vec<u8> {
	let mut out = if header { flv_header(0x05) } else { Vec::new() };

	let mut vseq = vec![
		(super::FRAME_TYPE_KEY << 4) | super::VIDEO_CODEC_AVC,
		super::AVC_SEQUENCE_HEADER,
		0,
		0,
		0,
	];
	vseq.extend_from_slice(&avcc());
	write_tag(&mut out, super::TAG_VIDEO, start_ms, &vseq);
	let mut aseq = vec![super::AAC_AUDIO_TAG_HEADER, super::AAC_SEQUENCE_HEADER];
	aseq.extend_from_slice(&ASC);
	write_tag(&mut out, super::TAG_AUDIO, start_ms, &aseq);

	for i in 0..frames {
		let dts = start_ms + i * 40;
		let frame_type = if i % 25 == 0 {
			super::FRAME_TYPE_KEY
		} else {
			super::FRAME_TYPE_INTER
		};
		// IPBPB...: P-frames present two slots late and the B-frames between them step back.
		let cts: u8 = match i % 25 {
			0 => 40,
			j if j % 2 == 1 => 80,
			_ => 0,
		};
		let mut video = vec![(frame_type << 4) | super::VIDEO_CODEC_AVC, super::AVC_NALU, 0, 0, cts];
		video.extend_from_slice(&[0, 0, 0, 5, 0x65, 0x88, 0x84, 0x21, 0x00]);
		write_tag(&mut out, super::TAG_VIDEO, dts, &video);

		let mut audio = vec![super::AAC_AUDIO_TAG_HEADER, super::AAC_RAW];
		audio.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
		write_tag(&mut out, super::TAG_AUDIO, dts + 20 - start_ms.min(300), &audio);
	}
	out
}

/// What an FLV import published, and the wall-clock window its first chunk arrived in.
struct Imported {
	published: std::collections::BTreeMap<String, Vec<u128>>,
	video: String,
	/// The root clock the catalog advertised.
	clock: hang::catalog::Clock,
	arrival: std::ops::RangeInclusive<kio::clock::Wall>,
	/// Why a chunk was refused, if one was.
	refused: Option<crate::Error>,
}

/// Import `chunks` in order on a catalog with the default clock, stopping at the first refused
/// chunk.
async fn import(chunks: &[Vec<u8>]) -> Imported {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let mut importer = Import::new(broadcast, catalog.reserve());

	let before = kio::clock::wall();
	let mut after = before;
	let mut refused = None;
	for (i, chunk) in chunks.iter().enumerate() {
		if let Err(err) = importer.decode(chunk) {
			refused = Some(err);
			break;
		}
		if i == 0 {
			after = kio::clock::wall();
		}
	}
	let _finished = importer.finish().unwrap();

	let snapshot = catalog.snapshot();
	Imported {
		published: crate::container::test_util::published(&consumer, &snapshot).await,
		video: snapshot.video.renditions.keys().next().unwrap().clone(),
		clock: snapshot.sync.clock.expect("the catalog advertises a clock"),
		arrival: before..=after,
		refused,
	}
}

/// A feed an hour into its own timeline publishes its tag timestamps verbatim, and the catalog
/// clock maps its first frame to the arrival time.
#[tokio::test]
async fn import_publishes_tag_timestamps_on_an_arrival_clock() {
	let start_ms = 3_600_000;
	let import = import(&[session(true, start_ms, 50)]).await;
	assert!(import.refused.is_none());

	// The first video frame presents 40ms after its decode time.
	let first = import.published[&import.video][0];
	assert_eq!(first, (start_ms as u128 + 40) * 1000, "the source's own timestamp");

	let tick = Duration::from_micros(1);
	let wall = import
		.clock
		.wall_clock(moq_net::Timestamp::from_micros(first as u64).unwrap())
		.unwrap();
	assert!(
		*import.arrival.start() - tick <= wall && wall <= *import.arrival.end() + tick,
		"the first frame is live on arrival"
	);
}

/// An encoder restarting its timestamps is a new epoch: the import refuses the rewind rather than
/// re-anchoring it forward, keeping everything published before it.
#[tokio::test]
async fn import_refuses_a_restart() {
	let import = import(&[session(true, 5_000, 50), session(false, 0, 50)]).await;
	let err = import.refused.expect("the restart is refused");
	assert!(matches!(err, crate::Error::TimestampRewind(_)), "{err:?}");
	assert_eq!(
		import.published[&import.video].len(),
		50,
		"the first session stays published"
	);
}

/// The catalog is first published at the first frame, carrying the clock that frame anchors,
/// rather than at the sequence headers on a provisional clock a copy-once reader would keep.
#[tokio::test]
async fn first_catalog_carries_the_anchored_clock() {
	let start_ms = 3_600_000;
	let data = session(true, start_ms, 10);
	let (headers, frames) = data.split_at(session(true, start_ms, 0).len());

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let provisional = catalog.snapshot().sync.clock.expect("a clock");
	let mut clocks = crate::container::test_util::Clocks::subscribe(&consumer).await;
	let mut importer = Import::new(broadcast, catalog.reserve());

	importer.decode(headers).unwrap();
	assert_eq!(clocks.drain(), vec![], "the sequence headers alone publish nothing");

	importer.decode(frames).unwrap();
	let anchored = catalog.snapshot().sync.clock.expect("a clock");
	assert_ne!(anchored, provisional, "the first frame anchors the clock");
	let published = clocks.drain();
	assert!(!published.is_empty(), "the first frame publishes the catalog");
	assert!(published.iter().all(|clock| *clock == Some(anchored)), "{published:?}");

	let _finished = importer.finish().unwrap();
	assert!(clocks.drain().iter().all(|clock| *clock == Some(anchored)));
}

#[test]
#[tracing_test::traced_test]
fn script_metadata_drops_warn_once() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let mut import = Import::new(broadcast, catalog.reserve());
	let mut data = synth_flv();
	for _ in 0..2 {
		write_tag(
			&mut data,
			super::TAG_SCRIPT,
			0,
			b"\x02\x00\x0aonMetaData\x08\x00\x00\x00\x00\x00\x00\x09",
		);
	}
	import.decode(&data).unwrap();
	assert_eq!(import.dropped_metadata(), 2);
	let _import = import.finish().unwrap();
	assert_eq!(catalog.snapshot().audio.renditions.len(), 1);
	logs_assert(|lines| {
		let warnings = lines
			.iter()
			.filter(|line| line.contains("discarding FLV script metadata"))
			.count();
		if warnings == 1 {
			Ok(())
		} else {
			Err(format!("expected one metadata warning, got {warnings}"))
		}
	});
}

#[test]
fn unsupported_codecs_fail_at_the_public_importer_boundary() {
	let mut cases = vec![(super::TAG_VIDEO, vec![0x12, 0]), (super::TAG_AUDIO, vec![0xb0, 0])];
	for (tag, packets) in [
		(super::TAG_VIDEO, &[0, 1, 2, 3, 4][..]),
		(super::TAG_AUDIO, &[0, 1, 2, 4][..]),
	] {
		for packet in packets {
			cases.push((tag, vec![0x90 | packet, b'n', b'o', b'p', b'e']));
			// OneTrack enhanced framing carries the same codec after its framing byte.
			cases.push((tag, vec![0x95, *packet, b'n', b'o', b'p', b'e', 0]));
		}
	}

	for (tag, payload) in cases {
		let mut data = synth_flv();
		write_tag(&mut data, tag, 0, &payload);
		for initial in [false, true] {
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
			let init = crate::import::ContainerInit::new(
				crate::import::ContainerFormat::Flv,
				if initial {
					bytes::Bytes::copy_from_slice(&data)
				} else {
					bytes::Bytes::new()
				},
			);
			let result = (|| -> crate::Result<()> {
				let mut import: crate::import::Container =
					crate::import::Container::new(broadcast, catalog.reserve(), &init)?;
				if !initial {
					for chunk in data.chunks(7) {
						import.decode(chunk)?;
					}
				}
				import.finish().map(drop)
			})();
			assert!(
				matches!(result, Err(crate::Error::UnknownFormat(_))),
				"tag {tag}, initial {initial}: {result:?}"
			);
		}
	}
}

/// A restart that cannot shift onto a shared catalog clock, because another source anchored it
/// behind this one, is a publish failure like a rewind: it refuses all later input.
#[tokio::test]
async fn an_unmappable_restart_refuses_later_input() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	catalog
		.timebase()
		.anchor(moq_net::Timestamp::from_secs(1).unwrap())
		.unwrap();
	let mut importer = Import::new(broadcast, catalog.reserve());

	importer.decode(&session(true, 3_600_000, 5)).unwrap();
	// The sequence headers and first video frame of a session: a tag header, a 14-byte body, and
	// its trailing size.
	let first_frame = |start_ms| {
		let mut data = session(false, start_ms, 1);
		data.truncate(session(false, start_ms, 0).len() + 11 + 14 + 4);
		data
	};
	let err = importer.decode(&first_frame(0)).unwrap_err();
	assert!(matches!(err, crate::Error::UnmappableTimestamp(_)), "{err:?}");
	let err = importer.decode(&first_frame(3_700_000)).unwrap_err();
	assert!(
		matches!(err, crate::Error::UnmappableTimestamp(_)),
		"refused with the cause: {err:?}"
	);
}

/// A newer sequence header replaces an AVC record still waiting for its keyframe, so the stale
/// record never claims the new codec's keyframe.
#[tokio::test(start_paused = true)]
async fn a_newer_sequence_header_supersedes_a_held_record() {
	let (mut out, _) = avc_session(&[]);
	let mut seq = vec![super::VIDEO_EX_HEADER | (super::FRAME_TYPE_KEY << 4) | super::VIDEO_PACKET_SEQUENCE_START];
	seq.extend_from_slice(b"av01");
	seq.extend_from_slice(&AV1C);
	write_tag(&mut out, super::TAG_VIDEO, 0, &seq);
	let mut frame = vec![super::VIDEO_EX_HEADER | (super::FRAME_TYPE_KEY << 4) | super::VIDEO_PACKET_CODED_FRAMES_X];
	frame.extend_from_slice(b"av01");
	frame.extend_from_slice(&[0x12, 0x00, 0x34, 0x56]);
	write_tag(&mut out, super::TAG_VIDEO, 0, &frame);

	let mut producer = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut producer, crate::catalog::Config::default()).unwrap();
	let mut importer = Import::new(producer, catalog.reserve());
	importer.decode(&out).unwrap();
	let _finished = importer.finish().unwrap();

	let codecs: Vec<_> = catalog
		.snapshot()
		.video
		.renditions
		.into_values()
		.map(|v| v.codec)
		.collect();
	assert!(matches!(codecs[..], [VideoCodec::AV1(_)]), "{codecs:?}");
}

/// A seek made while a record waits for its keyframe keys the group that keyframe opens, even when
/// the sequence header repeats in between.
#[tokio::test(start_paused = true)]
async fn a_seek_reaches_a_track_waiting_for_its_keyframe() {
	let (headers, keyframe) = avc_session(&[0, 0, 0, 5, 0x65, 0x88, 0x84, 0x21, 0x00]);

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let mut importer = Import::new(broadcast, catalog.reserve());
	importer.decode(&headers).unwrap();
	importer.seek(100).unwrap();
	// The same sequence headers again, past the FLV file header.
	importer.decode(&headers[flv_header(0).len()..]).unwrap();
	importer.decode(&keyframe).unwrap();
	let _finished = importer.finish().unwrap();

	let snapshot = catalog.snapshot();
	let name = snapshot.video.renditions.keys().next().unwrap();
	let replay = moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
		moq_net::track::Start::Live {
			min: Some(Duration::from_secs(3600)),
			max: Some(Duration::from_secs(3600)),
		},
	)]);
	let mut track = consumer.track(name).unwrap().subscribe(replay).await.unwrap();
	let mut starts = Vec::new();
	while let Some(group) = track.recv_group().await.unwrap() {
		starts.push(group.start);
	}
	assert_eq!(starts, [100]);
}

/// A changed sequence header on a live track skips the deltas before its keyframe, which can't
/// decode against the old record and whose timestamps the old track never checks, and a seek in
/// between keys the group that keyframe opens.
#[tokio::test(start_paused = true)]
async fn a_changed_sequence_header_waits_for_its_keyframe() {
	let video = |frame_type: u8, nalus: &[u8], ms: u32, cts: i32| {
		let cts = cts.to_be_bytes();
		let mut body = vec![
			(frame_type << 4) | super::VIDEO_CODEC_AVC,
			super::AVC_NALU,
			cts[1],
			cts[2],
			cts[3],
		];
		body.extend_from_slice(nalus);
		let mut out = Vec::new();
		write_tag(&mut out, super::TAG_VIDEO, ms, &body);
		out
	};
	let idr = [0, 0, 0, 5, 0x65, 0x88, 0x84, 0x21, 0x00];
	let delta = [0, 0, 0, 5, 0x41, 0x9a, 0x02, 0x03, 0x04];
	let (headers, keyframe) = avc_session(&idr);
	let mut changed = vec![
		(super::FRAME_TYPE_KEY << 4) | super::VIDEO_CODEC_AVC,
		super::AVC_SEQUENCE_HEADER,
		0,
		0,
		0,
	];
	changed.extend_from_slice(&avcc_level(0x28));
	let mut header = Vec::new();
	write_tag(&mut header, super::TAG_VIDEO, 133, &changed);

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let mut importer = Import::new(broadcast, catalog.reserve());
	let replay = || {
		moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
			moq_net::track::Start::Live {
				min: Some(Duration::from_secs(3600)),
				max: Some(Duration::from_secs(3600)),
			},
		)])
	};
	importer.decode(&headers).unwrap();
	importer.decode(&keyframe).unwrap();
	importer.decode(&video(2, &delta, 33, 0)).unwrap();
	importer.decode(&video(super::FRAME_TYPE_KEY, &idr, 80, 0)).unwrap();
	importer.decode(&video(super::FRAME_TYPE_KEY, &idr, 100, 0)).unwrap();
	let old = catalog.snapshot().video.renditions.keys().next().unwrap().clone();
	let mut old = consumer.track(&old).unwrap().subscribe(replay()).await.unwrap();

	importer.decode(&header).unwrap();
	// Presented before the old track's 80 ms group, the floor a delta is checked against, which
	// would refuse it as a rewind.
	importer.decode(&video(2, &delta, 133, -60)).unwrap();
	importer.seek(200).unwrap();
	importer.decode(&video(super::FRAME_TYPE_KEY, &idr, 200, 0)).unwrap();
	let _finished = importer.finish().unwrap();

	let mut deltas = 0;
	while let Some(mut group) = old.recv_group().await.unwrap() {
		while let Some(frame) = group.read_frame().await.unwrap() {
			deltas += usize::from(frame.payload.ends_with(&delta));
		}
	}
	assert_eq!(deltas, 1, "the delta after the new header never reaches the old track");

	let snapshot = catalog.snapshot();
	let name = snapshot.video.renditions.keys().next().unwrap();
	let mut track = consumer.track(name).unwrap().subscribe(replay()).await.unwrap();
	let mut starts = Vec::new();
	while let Some(group) = track.recv_group().await.unwrap() {
		starts.push(group.start);
	}
	assert_eq!(starts, [200]);
}

/// An audio-only FLV of raw AAC frames at `timestamps` (ms).
fn aac_tags(timestamps: &[u32]) -> Vec<u8> {
	aac_tags_with(&ASC, timestamps)
}

/// An audio-only FLV of raw AAC frames configured by `asc`, at `timestamps` (ms).
fn aac_tags_with(asc: &[u8], timestamps: &[u32]) -> Vec<u8> {
	let mut out = flv_header(0x04);
	let mut seq = vec![super::AAC_AUDIO_TAG_HEADER, super::AAC_SEQUENCE_HEADER];
	seq.extend_from_slice(asc);
	write_tag(&mut out, super::TAG_AUDIO, 0, &seq);
	for &timestamp in timestamps {
		write_tag(
			&mut out,
			super::TAG_AUDIO,
			timestamp,
			&[super::AAC_AUDIO_TAG_HEADER, super::AAC_RAW, 0xde, 0xad],
		);
	}
	out
}

/// Import an audio-only FLV and return its published frame timestamps (µs).
async fn audio_published(data: &[u8]) -> Vec<u128> {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let mut importer = Import::new(broadcast, catalog.reserve());
	importer.decode(data).unwrap();
	let _finished = importer.finish().unwrap();
	let snapshot = catalog.snapshot();
	let mut published = crate::container::test_util::published(&consumer, &snapshot).await;
	let name = snapshot.audio.renditions.keys().next().unwrap().clone();
	published.remove(&name).unwrap()
}

/// An audio-only FLV of 20 ms Opus packets at `timestamps` (ms).
fn opus_tags(timestamps: &[u32]) -> Vec<u8> {
	let mut out = flv_header(0x04);
	let mut seq = vec![(super::AUDIO_FORMAT_EX << 4) | super::AUDIO_PACKET_SEQUENCE_START];
	seq.extend_from_slice(b"Opus");
	seq.extend_from_slice(&crate::codec::opus::Config::new(48_000, 2).encode().unwrap());
	write_tag(&mut out, super::TAG_AUDIO, 0, &seq);
	for &timestamp in timestamps {
		out.extend_from_slice(&opus_tag(timestamp));
	}
	out
}

/// One tag of a 20 ms Opus packet at `timestamp` (ms).
fn opus_tag(timestamp: u32) -> Vec<u8> {
	let mut frame = vec![(super::AUDIO_FORMAT_EX << 4) | super::AUDIO_PACKET_CODED_FRAMES];
	frame.extend_from_slice(b"Opus");
	// CELT fullband, one 20 ms frame.
	frame.extend_from_slice(&[0xf8, 0xff, 0xfe]);
	let mut out = Vec::new();
	write_tag(&mut out, super::TAG_AUDIO, timestamp, &frame);
	out
}

/// Dropped packets leave a gap the group's end doesn't extrapolate, so the packet after it keeps
/// its own timestamp rather than moving past an end estimated from the gap.
#[tokio::test]
async fn an_audio_gap_keeps_the_next_timestamp() {
	for timestamps in [[0, 20, 80, 100, 120, 140], [0, 80, 100, 120, 140, 160]] {
		let published = audio_published(&opus_tags(&timestamps)).await;
		let expected: Vec<u128> = timestamps.iter().map(|&ms| u128::from(ms) * 1000).collect();
		assert_eq!(published, expected);
	}
}

/// A tag repeating the previous timestamp adds no time, so it is dropped rather than holding a
/// group open while the clock stalls.
#[tokio::test]
async fn a_repeated_audio_timestamp_is_dropped() {
	let published = audio_published(&aac_tags(&[0, 20, 20, 20, 40])).await;
	assert_eq!(published, [0, 20_000, 40_000]);
}

/// AudioSpecificConfig for AAC-LC, 48 kHz, stereo: 1024 samples, 21.333 ms, a packet.
const ASC_48K: [u8; 2] = [0x11, 0x90];

/// AudioSpecificConfig for HE-AAC with explicit SBR: a 24 kHz core played at 48 kHz, stereo, so
/// 2048 samples, 42.667 ms, a packet.
const ASC_HE: [u8; 4] = [0x2b, 0x11, 0x88, 0x00];

/// [`ASC_HE`] with its frameLengthFlag set: 1920 samples, 40 ms, a packet.
const ASC_HE_960: [u8; 4] = [0x2b, 0x11, 0x8a, 0x00];

/// AudioSpecificConfig for AAC Main, 48 kHz, stereo: 1024 samples a packet, as AAC-LC.
const ASC_MAIN: [u8; 2] = [0x09, 0x90];

/// AudioSpecificConfig for ER BSAC, 48 kHz, stereo, whose packets this importer states no
/// duration for.
const ASC_BSAC: [u8; 3] = [0xb1, 0x90, 0x00];

/// An audio-only FLV of 24 ms MP3 frames (MPEG-1 Layer III, 48 kHz) at `timestamps` (ms).
fn mp3_tags(timestamps: &[u32]) -> Vec<u8> {
	let mut out = flv_header(0x04);
	for &timestamp in timestamps {
		let header = (super::AUDIO_FORMAT_MP3 << 4) | (3 << 2) | (1 << 1) | 1;
		write_tag(
			&mut out,
			super::TAG_AUDIO,
			timestamp,
			&[header, 0xff, 0xfb, 0x94, 0x44, 0, 0],
		);
	}
	out
}

/// A gap before the first spacing never sets where a group ends: MP3 frames state their duration
/// in their header, AAC packets by their config, and a codec whose packets state none closes its
/// group at the next tag.
#[tokio::test]
async fn a_gap_before_the_first_spacing_keeps_the_next_timestamp() {
	let published = audio_published(&mp3_tags(&[0, 72, 96, 120])).await;
	assert_eq!(published, [0, 72_000, 96_000, 120_000]);

	for asc in [&ASC_HE[..], &ASC_MAIN, &ASC_BSAC] {
		let published = audio_published(&aac_tags_with(asc, &[0, 85, 128, 170])).await;
		assert_eq!(published, [0, 85_000, 128_000, 170_000], "{asc:x?}");
	}
}

/// Import an audio-only FLV and return each audio group's start and declared end (µs).
async fn audio_groups(data: &[u8]) -> Vec<(u64, Option<u64>)> {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let mut importer = Import::new(broadcast, catalog.reserve());
	importer.decode(data).unwrap();
	let _finished = importer.finish().unwrap();
	let name = catalog.snapshot().audio.renditions.keys().next().unwrap().clone();
	groups_of(&consumer, &name).await
}

/// Each group of the track `name`, by start, with its declared end.
async fn groups_of(consumer: &moq_net::broadcast::Consumer, name: &str) -> Vec<(u64, Option<u64>)> {
	let replay = moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
		moq_net::track::Start::Live {
			min: Some(Duration::from_secs(3600)),
			max: Some(Duration::from_secs(3600)),
		},
	)]);
	let mut track = consumer.track(name).unwrap().subscribe(replay).await.unwrap();
	let mut groups = Vec::new();
	while let Some(mut group) = track.recv_group().await.unwrap() {
		while group.read_frame().await.unwrap().is_some() {}
		groups.push((group.start, group.end()));
	}
	// The newest group arrives first.
	groups.sort();
	groups
}

/// A group holding a packet whose duration is unknown declares no end, so a gap after it isn't
/// declared as audio. One whose packets state theirs ends where they do, at the end of input too.
#[tokio::test]
async fn groups_end_only_where_their_audio_is_known_to() {
	let groups = audio_groups(&aac_tags_with(&ASC_BSAC, &[0, 85, 128])).await;
	assert_eq!(groups, [(0, None), (128_000, None)]);

	// 21.333 ms AAC-LC packets, the last ending at 85 ms, not one 64 ms spacing past it.
	let groups = audio_groups(&aac_tags_with(&ASC_48K, &[0, 64])).await;
	assert_eq!(groups, [(0, Some(85_000))]);
}

/// A seek closes the open audio group as a gap does: where its stated audio ends, capped at the
/// seek, or with no declared end when a packet's duration is unknown.
#[tokio::test]
async fn a_seek_declares_only_known_audio_ends() {
	for (asc, ends) in [
		(&ASC_BSAC[..], [None, None]),
		(&ASC_48K, [Some(106_000), Some(321_000)]),
	] {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let mut importer = Import::new(broadcast, catalog.reserve());
		let tags = aac_tags_with(asc, &[0, 85]);
		importer.decode(&tags).unwrap();
		importer.seek(300_000).unwrap();
		let mut tag = Vec::new();
		write_tag(
			&mut tag,
			super::TAG_AUDIO,
			300,
			&[super::AAC_AUDIO_TAG_HEADER, super::AAC_RAW, 0xde, 0xad],
		);
		importer.decode(&tag).unwrap();
		let _finished = importer.finish().unwrap();

		let name = catalog.snapshot().audio.renditions.keys().next().unwrap().clone();
		let groups = groups_of(&consumer, &name).await;
		assert_eq!(groups, [(0, ends[0]), (300_000, ends[1])], "{asc:x?}");
	}
}

/// A seek caps the open group only past its highest packet: one between reordered packets, or at
/// the last one's own time, leaves the group ending where its audio does.
#[tokio::test]
async fn a_seek_caps_only_past_the_newest_packet() {
	for (tags, seek, end) in [(&[0, 60, 40][..], 50_000, 80_000), (&[0, 20, 40], 40_000, 60_000)] {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let mut importer = Import::new(broadcast, catalog.reserve());
		importer.decode(&opus_tags(tags)).unwrap();
		importer.seek(seek).unwrap();
		let _finished = importer.finish().unwrap();

		let name = catalog.snapshot().audio.renditions.keys().next().unwrap().clone();
		assert_eq!(groups_of(&consumer, &name).await, [(0, Some(end))], "{tags:?}");
	}
}

/// HE-AAC signaling 960-sample core frames plays 40 ms a packet, so a group closing before the
/// next tag ends where that tag lands.
#[tokio::test]
async fn he_aac_honors_its_frame_length() {
	let published = audio_published(&aac_tags_with(&ASC_HE_960, &[0, 40, 80, 120, 160])).await;
	assert_eq!(published, [0, 40_000, 80_000, 120_000, 160_000]);
}

/// A group full of Opus packets, whose durations count 48 kHz samples against FLV's ms clock,
/// closes with the packet that fills it rather than waiting for the next tag.
#[tokio::test]
async fn a_full_opus_group_closes_at_once() {
	use futures::FutureExt;

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let mut importer = Import::new(broadcast, catalog.reserve());
	importer.decode(&opus_tags(&[0, 20, 40, 60, 80])).unwrap();

	let snapshot = catalog.snapshot();
	let name = snapshot.audio.renditions.keys().next().unwrap();
	let replay = moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
		moq_net::track::Start::Live {
			min: Some(Duration::from_secs(3600)),
			max: Some(Duration::from_secs(3600)),
		},
	)]);
	let mut track = consumer.track(name).unwrap().subscribe(replay).await.unwrap();
	let mut group = track.recv_group().await.unwrap().unwrap();
	for _ in 0..5 {
		group.read_frame().await.unwrap().unwrap();
	}
	let end = group.read_frame().now_or_never();
	assert!(
		matches!(end, Some(Ok(None))),
		"the fifth packet closes its group: {end:?}"
	);
	drop(importer);
}

/// AAC frames whose ms timestamps truncate their 21.333 ms spacing keep them, so a group closing
/// before the next tag never ends past where that tag lands.
#[tokio::test]
async fn aac_keeps_its_millisecond_timestamps() {
	let timestamps = [0, 21, 42, 64, 85, 106, 128];
	let published = audio_published(&aac_tags_with(&ASC_48K, &timestamps)).await;
	let expected: Vec<u128> = timestamps.iter().map(|&ms| u128::from(ms) * 1000).collect();
	assert_eq!(published, expected);
}

/// An AAC import into a microsecond CMAF track publishes, though no whole count of microseconds
/// holds a packet's 1024/48000 s.
#[tokio::test]
async fn aac_imports_to_a_microsecond_cmaf_track() {
	let mut config = hang::catalog::AudioConfig::new(AudioCodec::AAC(hang::catalog::AAC { profile: 2 }), 48_000, 2);
	config.description = Some(bytes::Bytes::copy_from_slice(&ASC_48K));
	let trak = crate::container::fmp4::synthesize_audio_trak(1, 1_000_000, &config).unwrap();
	let trex = mp4_atom::Trex {
		track_id: 1,
		default_sample_description_index: 1,
		..Default::default()
	};
	let init = crate::container::fmp4::encode_init(None, vec![trak], vec![trex]).unwrap();

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let mut importer =
		Import::new(broadcast, catalog.reserve()).with_container(hang::catalog::Container::Cmaf { init });
	importer
		.decode(&aac_tags_with(&ASC_48K, &[0, 21, 42, 64, 85, 106]))
		.unwrap();
	let _finished = importer.finish().unwrap();
	let snapshot = catalog.snapshot();
	let mut published = crate::container::test_util::published(&consumer, &snapshot).await;
	let name = snapshot.audio.renditions.keys().next().unwrap().clone();
	assert_eq!(
		published.remove(&name).unwrap(),
		[0, 21_000, 42_000, 64_000, 85_000, 106_000]
	);
}
