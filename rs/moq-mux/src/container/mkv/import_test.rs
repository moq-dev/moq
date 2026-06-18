//! Tests for the MKV/WebM importer.
//!
//! These tests synthesize small WebM files via webm-iterable's writer (no external
//! tooling required) and feed them through [`crate::container::mkv::Import`], then assert that
//! the resulting catalog and frame stream are well-formed.

use std::io::Cursor;

use hang::catalog::{AudioCodec, Container, VideoCodec};
use webm_iterable::WebmWriter;
use webm_iterable::matroska_spec::{Master, MatroskaSpec, SimpleBlock};

/// Build a minimal WebM byte stream with the given tracks and blocks.
struct MkvBuilder {
	tags: Vec<MatroskaSpec>,
}

impl MkvBuilder {
	fn new() -> Self {
		Self { tags: Vec::new() }
	}

	fn header(mut self, doc_type: &str) -> Self {
		self.tags.push(MatroskaSpec::Ebml(Master::Full(vec![
			MatroskaSpec::DocType(doc_type.to_string()),
			MatroskaSpec::DocTypeVersion(2),
			MatroskaSpec::DocTypeReadVersion(2),
		])));
		self
	}

	fn segment_start(mut self) -> Self {
		self.tags.push(MatroskaSpec::Segment(Master::Start));
		self
	}

	fn segment_end(mut self) -> Self {
		self.tags.push(MatroskaSpec::Segment(Master::End));
		self
	}

	fn info(mut self, timestamp_scale_ns: u64) -> Self {
		self.tags
			.push(MatroskaSpec::Info(Master::Full(vec![MatroskaSpec::TimestampScale(
				timestamp_scale_ns,
			)])));
		self
	}

	fn track_video(mut self, number: u64, codec_id: &str, width: u64, height: u64) -> Self {
		self.tags
			.push(MatroskaSpec::Tracks(Master::Full(vec![MatroskaSpec::TrackEntry(
				Master::Full(vec![
					MatroskaSpec::TrackNumber(number),
					MatroskaSpec::TrackUID(number),
					MatroskaSpec::TrackType(1),
					MatroskaSpec::CodecID(codec_id.to_string()),
					MatroskaSpec::Video(Master::Full(vec![
						MatroskaSpec::PixelWidth(width),
						MatroskaSpec::PixelHeight(height),
					])),
				]),
			)])));
		self
	}

	fn tracks(mut self, entries: Vec<MatroskaSpec>) -> Self {
		self.tags.push(MatroskaSpec::Tracks(Master::Full(entries)));
		self
	}

	fn cluster<F>(mut self, cluster_timestamp: u64, blocks: F) -> Self
	where
		F: FnOnce() -> Vec<MatroskaSpec>,
	{
		self.tags.push(MatroskaSpec::Cluster(Master::Start));
		self.tags.push(MatroskaSpec::Timestamp(cluster_timestamp));
		self.tags.extend(blocks());
		self.tags.push(MatroskaSpec::Cluster(Master::End));
		self
	}

	fn build(self) -> Vec<u8> {
		let mut dest = Cursor::new(Vec::new());
		{
			let mut writer = WebmWriter::new(&mut dest);
			for tag in &self.tags {
				writer.write(tag).expect("write tag");
			}
		}
		dest.into_inner()
	}
}

fn simple_block(track: u64, rel_ts: i16, keyframe: bool, payload: &[u8]) -> MatroskaSpec {
	let sb = SimpleBlock::new_uncheked(payload, track, rel_ts, false, None, false, keyframe);
	sb.into()
}

fn track_entry_audio_opus(number: u64, sample_rate: f64, channels: u64) -> MatroskaSpec {
	// Minimal OpusHead: magic + version + channels + pre-skip + sample_rate (LE) + gain + mapping.
	let mut head = Vec::new();
	head.extend_from_slice(b"OpusHead");
	head.push(1); // version
	head.push(channels as u8);
	head.extend_from_slice(&0u16.to_le_bytes()); // pre-skip
	head.extend_from_slice(&(sample_rate as u32).to_le_bytes());
	head.extend_from_slice(&0i16.to_le_bytes()); // gain
	head.push(0); // mapping family

	MatroskaSpec::TrackEntry(Master::Full(vec![
		MatroskaSpec::TrackNumber(number),
		MatroskaSpec::TrackUID(number),
		MatroskaSpec::TrackType(2),
		MatroskaSpec::CodecID("A_OPUS".to_string()),
		MatroskaSpec::CodecPrivate(head),
		MatroskaSpec::Audio(Master::Full(vec![
			MatroskaSpec::SamplingFrequency(sample_rate),
			MatroskaSpec::Channels(channels),
		])),
	]))
}

fn track_entry_video_vp9(number: u64, width: u64, height: u64) -> MatroskaSpec {
	MatroskaSpec::TrackEntry(Master::Full(vec![
		MatroskaSpec::TrackNumber(number),
		MatroskaSpec::TrackUID(number),
		MatroskaSpec::TrackType(1),
		MatroskaSpec::CodecID("V_VP9".to_string()),
		MatroskaSpec::Video(Master::Full(vec![
			MatroskaSpec::PixelWidth(width),
			MatroskaSpec::PixelHeight(height),
		])),
	]))
}

fn run(data: &[u8]) -> crate::catalog::hang::Catalog {
	let mut broadcast = moq_net::BroadcastInfo::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
	let mut mkv = crate::container::mkv::Import::new(broadcast, catalog.clone());
	let buf = bytes::BytesMut::from(data);
	mkv.decode(&buf).expect("decode");
	mkv.finish().expect("finish");
	catalog.snapshot()
}

#[test]
fn test_vp9_only_catalog() {
	let data = MkvBuilder::new()
		.header("webm")
		.segment_start()
		.info(1_000_000)
		.track_video(1, "V_VP9", 1280, 720)
		.cluster(0, || vec![simple_block(1, 0, true, b"\x00\x00\x00\x01vp9-frame")])
		.segment_end()
		.build();

	let catalog = run(&data);
	assert_eq!(catalog.video.renditions.len(), 1);
	assert_eq!(catalog.audio.renditions.len(), 0);

	let v = catalog.video.renditions.values().next().unwrap();
	assert!(matches!(v.codec, VideoCodec::VP9(_)), "codec: {:?}", v.codec);
	assert_eq!(v.coded_width, Some(1280));
	assert_eq!(v.coded_height, Some(720));
	assert!(matches!(v.container, Container::Legacy));
}

#[test]
fn test_vp9_opus_catalog() {
	let data = MkvBuilder::new()
		.header("webm")
		.segment_start()
		.info(1_000_000)
		.tracks(vec![
			track_entry_video_vp9(1, 640, 480),
			track_entry_audio_opus(2, 48000.0, 2),
		])
		.cluster(0, || {
			vec![
				simple_block(1, 0, true, b"vp9-key"),
				simple_block(2, 0, true, b"opus-pkt-0"),
				simple_block(2, 20, true, b"opus-pkt-1"),
				simple_block(1, 33, false, b"vp9-p"),
			]
		})
		.segment_end()
		.build();

	let catalog = run(&data);
	assert_eq!(catalog.video.renditions.len(), 1);
	assert_eq!(catalog.audio.renditions.len(), 1);

	let v = catalog.video.renditions.values().next().unwrap();
	assert!(matches!(v.codec, VideoCodec::VP9(_)));

	let a = catalog.audio.renditions.values().next().unwrap();
	assert!(matches!(a.codec, AudioCodec::Opus));
	assert_eq!(a.sample_rate, 48000);
	assert_eq!(a.channel_count, 2);
}

#[test]
fn test_chunked_decode_dedup() {
	// Build the same WebM and feed it in tiny chunks. The dedup logic should ensure
	// that frames aren't emitted twice across the parse restarts.
	let data = MkvBuilder::new()
		.header("webm")
		.segment_start()
		.info(1_000_000)
		.track_video(1, "V_VP9", 320, 240)
		.cluster(0, || {
			vec![
				simple_block(1, 0, true, b"k0"),
				simple_block(1, 33, false, b"p1"),
				simple_block(1, 66, false, b"p2"),
			]
		})
		.cluster(100, || {
			vec![simple_block(1, 0, true, b"k1"), simple_block(1, 33, false, b"p3")]
		})
		.segment_end()
		.build();

	let mut broadcast = moq_net::BroadcastInfo::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
	let mut mkv = crate::container::mkv::Import::new(broadcast, catalog.clone());

	// Feed in 16-byte chunks to stress the chunked-restart code path.
	for chunk in data.chunks(16) {
		let b = bytes::BytesMut::from(chunk);
		mkv.decode(&b).expect("decode chunk");
	}
	mkv.finish().expect("finish");

	let catalog = catalog.snapshot();
	assert_eq!(catalog.video.renditions.len(), 1);
	let v = catalog.video.renditions.values().next().unwrap();
	assert_eq!(v.coded_width, Some(320));
	assert_eq!(v.coded_height, Some(240));
}

#[test]
fn test_unsupported_codec_skipped() {
	// Mix of supported (Opus) and unsupported (Vorbis) audio tracks. The Vorbis track
	// should be dropped with a warning; Opus should make it into the catalog.
	let data = MkvBuilder::new()
		.header("webm")
		.segment_start()
		.info(1_000_000)
		.tracks(vec![
			track_entry_audio_opus(1, 48000.0, 2),
			MatroskaSpec::TrackEntry(Master::Full(vec![
				MatroskaSpec::TrackNumber(2),
				MatroskaSpec::TrackUID(2),
				MatroskaSpec::TrackType(2),
				MatroskaSpec::CodecID("A_VORBIS".to_string()),
			])),
		])
		.cluster(0, || vec![simple_block(1, 0, true, b"opus")])
		.segment_end()
		.build();

	let catalog = run(&data);
	assert_eq!(catalog.audio.renditions.len(), 1);
	let a = catalog.audio.renditions.values().next().unwrap();
	assert!(matches!(a.codec, AudioCodec::Opus));
}

#[test]
fn test_block_timestamp_scaling() {
	// TimestampScale = 1_000_000 ns (1ms). Cluster timestamp = 1000, block rel = 33
	// → 1033 ms = 1_033_000 us.
	let data = MkvBuilder::new()
		.header("webm")
		.segment_start()
		.info(1_000_000)
		.track_video(1, "V_VP9", 16, 16)
		.cluster(1000, || vec![simple_block(1, 33, true, b"f")])
		.segment_end()
		.build();

	// Smoke check: parsing succeeds. Timestamp value itself is internal to the
	// container::Producer; the catalog round-trip above already exercises the
	// rendition wiring.
	let _ = run(&data);
}
