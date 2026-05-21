//! Tests for the MKV/WebM exporter.
//!
//! Round-trip tests: ingest a synthetic WebM via the importer, re-export via the
//! exporter, and assert that the re-exported bytes parse back into the same catalog
//! shape.

use std::io::Cursor;

use bytes::Bytes;
use hang::catalog::{AudioCodec, VideoCodec};
use webm_iterable::WebmIterator;
use webm_iterable::matroska_spec::{Master, MatroskaSpec, SimpleBlock};

#[tokio::test(start_paused = true)]
async fn export_header_roundtrip_vp9_opus() {
	// Build a tiny synthetic WebM with one VP9 video track and one Opus audio track.
	let import_bytes = synth_webm();

	// Ingest into a broadcast.
	let broadcast = moq_net::Broadcast::new();
	let mut producer = broadcast.produce();
	let consumer = producer.consume();

	let catalog = crate::catalog::Producer::new(&mut producer).unwrap();
	let mut importer = crate::import::Mkv::new(producer, catalog.clone());
	let mut buf = bytes::BytesMut::from(import_bytes.as_slice());
	importer.decode(&mut buf).unwrap();
	importer.finish().unwrap();

	// Now subscribe via the exporter and pull bytes.
	let mut exporter = crate::export::Mkv::new(consumer).unwrap();

	// First `next()` should give us the header (EBML + Segment-start + Info + Tracks).
	let header = tokio::time::timeout(std::time::Duration::from_secs(1), exporter.next())
		.await
		.expect("exporter timed out")
		.expect("exporter result")
		.expect("expected header bytes");

	// Parse the emitted header back and assert structure.
	let mut cursor = Cursor::new(header.as_ref());
	let iter = WebmIterator::new(
		&mut cursor,
		&[
			MatroskaSpec::Ebml(Master::Start),
			MatroskaSpec::Tracks(Master::Start),
			MatroskaSpec::TrackEntry(Master::Start),
			MatroskaSpec::Info(Master::Start),
		],
	);

	let mut saw_ebml = false;
	let mut saw_segment_start = false;
	let mut saw_info = false;
	let mut track_entries: Vec<Vec<MatroskaSpec>> = Vec::new();

	for tag in iter {
		match tag.expect("parse header") {
			MatroskaSpec::Ebml(Master::Full(children)) => {
				saw_ebml = true;
				let doc_type = children
					.iter()
					.find_map(|c| {
						if let MatroskaSpec::DocType(d) = c {
							Some(d.clone())
						} else {
							None
						}
					})
					.expect("DocType in header");
				assert_eq!(doc_type, "webm", "should be webm when only VP9+Opus");
			}
			MatroskaSpec::Segment(Master::Start) => saw_segment_start = true,
			MatroskaSpec::Info(Master::Full(children)) => {
				saw_info = true;
				let scale = children
					.iter()
					.find_map(|c| {
						if let MatroskaSpec::TimestampScale(v) = c {
							Some(*v)
						} else {
							None
						}
					})
					.expect("TimestampScale");
				assert_eq!(scale, 1_000_000);
			}
			MatroskaSpec::Tracks(Master::Full(entries)) => {
				for entry in entries {
					if let MatroskaSpec::TrackEntry(Master::Full(children)) = entry {
						track_entries.push(children);
					}
				}
			}
			_ => {}
		}
	}

	assert!(saw_ebml, "header missing EBML");
	assert!(saw_segment_start, "header missing Segment::Start");
	assert!(saw_info, "header missing Info");
	assert_eq!(track_entries.len(), 2, "expected 2 track entries (1 video + 1 audio)");

	let codec_ids: Vec<String> = track_entries
		.iter()
		.map(|e| {
			e.iter()
				.find_map(|c| {
					if let MatroskaSpec::CodecID(s) = c {
						Some(s.clone())
					} else {
						None
					}
				})
				.unwrap()
		})
		.collect();
	assert!(codec_ids.iter().any(|c| c == "V_VP9"));
	assert!(codec_ids.iter().any(|c| c == "A_OPUS"));

	// Verify the round-trip by re-importing the header (a header alone is enough
	// to populate the catalog).
	let mut broadcast2 = moq_net::Broadcast::new().produce();
	let catalog2 = crate::catalog::Producer::new(&mut broadcast2).unwrap();
	let mut importer2 = crate::import::Mkv::new(broadcast2, catalog2.clone());
	let mut hbuf = bytes::BytesMut::from(header.as_ref());
	importer2.decode(&mut hbuf).unwrap();
	let snap = catalog2.snapshot();
	assert_eq!(snap.video.renditions.len(), 1);
	assert_eq!(snap.audio.renditions.len(), 1);

	let v = snap.video.renditions.values().next().unwrap();
	assert!(matches!(v.codec, VideoCodec::VP9(_)));
	let a = snap.audio.renditions.values().next().unwrap();
	assert!(matches!(a.codec, AudioCodec::Opus));
	assert_eq!(a.sample_rate, 48000);
}

#[tokio::test(start_paused = true)]
async fn export_emits_blocks_for_each_frame() {
	// Import a WebM that contains 3 video frames + 2 audio frames, export it,
	// and assert that the exported byte stream parses back into the same number
	// of SimpleBlock elements with the right track assignments.
	let import_bytes = synth_webm_with_frames();

	let broadcast = moq_net::Broadcast::new();
	let mut producer = broadcast.produce();
	let consumer = producer.consume();

	let catalog = crate::catalog::Producer::new(&mut producer).unwrap();
	let mut importer = crate::import::Mkv::new(producer, catalog.clone());
	let mut buf = bytes::BytesMut::from(import_bytes.as_slice());
	importer.decode(&mut buf).unwrap();
	importer.finish().unwrap();

	let mut exporter = crate::export::Mkv::new(consumer).unwrap();
	let mut exported: Vec<u8> = Vec::new();

	// Pull a bounded number of chunks: 1 header + up to 5 block fragments.
	// Past that we assume the exporter is idle waiting for more frames.
	for _ in 0..10 {
		let next = tokio::time::timeout(std::time::Duration::from_millis(100), exporter.next()).await;
		match next {
			Ok(Ok(Some(chunk))) => exported.extend_from_slice(&chunk),
			Ok(Ok(None)) => break,
			Ok(Err(e)) => panic!("exporter error: {e}"),
			Err(_) => break,
		}
	}

	drop(importer);
	drop(exporter);

	// Parse exported bytes and count SimpleBlock occurrences per track.
	let mut cursor = Cursor::new(exported.as_slice());
	let iter = WebmIterator::new(&mut cursor, &[]);
	let mut blocks_per_track: std::collections::HashMap<u64, usize> = Default::default();
	for tag in iter {
		if let Ok(MatroskaSpec::SimpleBlock(data)) = tag
			&& let Ok(sb) = SimpleBlock::try_from(data.as_slice())
		{
			*blocks_per_track.entry(sb.track).or_default() += 1;
		}
	}

	assert_eq!(blocks_per_track.values().sum::<usize>(), 5, "expected 5 total blocks");
	assert_eq!(blocks_per_track.len(), 2, "expected 2 tracks with blocks");

	// Round-trip verification: feed the exported bytes back through the importer
	// and check the catalog repopulates with the same codecs.
	let mut bcast2 = moq_net::Broadcast::new().produce();
	let cat2 = crate::catalog::Producer::new(&mut bcast2).unwrap();
	let mut imp2 = crate::import::Mkv::new(bcast2, cat2.clone());
	let mut rt = bytes::BytesMut::from(exported.as_slice());
	imp2.decode(&mut rt).unwrap();
	imp2.finish().unwrap();
	let snap = cat2.snapshot();
	assert_eq!(snap.video.renditions.len(), 1);
	assert_eq!(snap.audio.renditions.len(), 1);
	assert!(matches!(
		snap.video.renditions.values().next().unwrap().codec,
		VideoCodec::VP9(_)
	));
	assert!(matches!(
		snap.audio.renditions.values().next().unwrap().codec,
		AudioCodec::Opus
	));
}

#[tokio::test(start_paused = true)]
async fn export_rejects_cmaf_track() {
	// Manually construct a broadcast whose catalog advertises a Cmaf-container
	// video track. The exporter should bail.
	use hang::catalog::{Container, H264, VideoConfig};

	let broadcast = moq_net::Broadcast::new();
	let mut producer = broadcast.produce();
	let consumer = producer.consume();

	let mut catalog = crate::catalog::Producer::new(&mut producer).unwrap();
	let track = producer.unique_track(".avc1").unwrap();
	catalog.lock().video.renditions.insert(
		track.name.clone(),
		VideoConfig {
			coded_width: Some(640),
			coded_height: Some(480),
			codec: H264 {
				profile: 0x64,
				constraints: 0,
				level: 0x1f,
				inline: false,
			}
			.into(),
			description: Some(Bytes::from(vec![0u8; 8])),
			framerate: None,
			bitrate: None,
			display_ratio_width: None,
			display_ratio_height: None,
			optimize_for_latency: None,
			container: Container::Cmaf {
				init: Bytes::from(vec![0u8; 32]),
			},
			jitter: None,
		},
	);

	let mut exporter = crate::export::Mkv::new(consumer).unwrap();
	let result = tokio::time::timeout(std::time::Duration::from_secs(1), exporter.next())
		.await
		.expect("exporter timed out");

	let err = result.expect_err("expected an error");
	assert!(err.to_string().contains("CMAF"), "expected CMAF rejection, got: {err}");
}

/// Build a small WebM with VP9 video + Opus audio and several frames per track.
fn synth_webm_with_frames() -> Vec<u8> {
	use webm_iterable::WebmWriter;

	let mut opus_head = Vec::new();
	opus_head.extend_from_slice(b"OpusHead");
	opus_head.push(1);
	opus_head.push(2);
	opus_head.extend_from_slice(&0u16.to_le_bytes());
	opus_head.extend_from_slice(&48000u32.to_le_bytes());
	opus_head.extend_from_slice(&0i16.to_le_bytes());
	opus_head.push(0);

	let simple_block = |track: u64, rel_ts: i16, keyframe: bool, payload: &[u8]| -> MatroskaSpec {
		SimpleBlock::new_uncheked(payload, track, rel_ts, false, None, false, keyframe).into()
	};

	let tags: Vec<MatroskaSpec> = vec![
		MatroskaSpec::Ebml(Master::Full(vec![
			MatroskaSpec::DocType("webm".to_string()),
			MatroskaSpec::DocTypeVersion(2),
			MatroskaSpec::DocTypeReadVersion(2),
		])),
		MatroskaSpec::Segment(Master::Start),
		MatroskaSpec::Info(Master::Full(vec![MatroskaSpec::TimestampScale(1_000_000)])),
		MatroskaSpec::Tracks(Master::Full(vec![
			MatroskaSpec::TrackEntry(Master::Full(vec![
				MatroskaSpec::TrackNumber(1),
				MatroskaSpec::TrackUID(1),
				MatroskaSpec::TrackType(1),
				MatroskaSpec::CodecID("V_VP9".to_string()),
				MatroskaSpec::Video(Master::Full(vec![
					MatroskaSpec::PixelWidth(320),
					MatroskaSpec::PixelHeight(240),
				])),
			])),
			MatroskaSpec::TrackEntry(Master::Full(vec![
				MatroskaSpec::TrackNumber(2),
				MatroskaSpec::TrackUID(2),
				MatroskaSpec::TrackType(2),
				MatroskaSpec::CodecID("A_OPUS".to_string()),
				MatroskaSpec::CodecPrivate(opus_head),
				MatroskaSpec::Audio(Master::Full(vec![
					MatroskaSpec::SamplingFrequency(48000.0),
					MatroskaSpec::Channels(2),
				])),
			])),
		])),
		MatroskaSpec::Cluster(Master::Start),
		MatroskaSpec::Timestamp(0),
		simple_block(1, 0, true, b"v0"),
		simple_block(2, 0, true, b"a0"),
		simple_block(1, 33, false, b"v1"),
		simple_block(2, 20, true, b"a1"),
		simple_block(1, 66, false, b"v2"),
		MatroskaSpec::Cluster(Master::End),
		MatroskaSpec::Segment(Master::End),
	];

	let mut dest = Cursor::new(Vec::new());
	{
		let mut writer = WebmWriter::new(&mut dest);
		for tag in &tags {
			writer.write(tag).unwrap();
		}
		writer.flush().unwrap();
	}
	dest.into_inner()
}

/// Build a small WebM with one VP9 video track and one Opus audio track.
fn synth_webm() -> Vec<u8> {
	use webm_iterable::WebmWriter;

	let mut opus_head = Vec::new();
	opus_head.extend_from_slice(b"OpusHead");
	opus_head.push(1); // version
	opus_head.push(2); // channels
	opus_head.extend_from_slice(&0u16.to_le_bytes()); // pre-skip
	opus_head.extend_from_slice(&48000u32.to_le_bytes()); // sample rate
	opus_head.extend_from_slice(&0i16.to_le_bytes()); // gain
	opus_head.push(0); // mapping family

	let tags: Vec<MatroskaSpec> = vec![
		MatroskaSpec::Ebml(Master::Full(vec![
			MatroskaSpec::DocType("webm".to_string()),
			MatroskaSpec::DocTypeVersion(2),
			MatroskaSpec::DocTypeReadVersion(2),
		])),
		MatroskaSpec::Segment(Master::Start),
		MatroskaSpec::Info(Master::Full(vec![MatroskaSpec::TimestampScale(1_000_000)])),
		MatroskaSpec::Tracks(Master::Full(vec![
			MatroskaSpec::TrackEntry(Master::Full(vec![
				MatroskaSpec::TrackNumber(1),
				MatroskaSpec::TrackUID(1),
				MatroskaSpec::TrackType(1),
				MatroskaSpec::CodecID("V_VP9".to_string()),
				MatroskaSpec::Video(Master::Full(vec![
					MatroskaSpec::PixelWidth(640),
					MatroskaSpec::PixelHeight(480),
				])),
			])),
			MatroskaSpec::TrackEntry(Master::Full(vec![
				MatroskaSpec::TrackNumber(2),
				MatroskaSpec::TrackUID(2),
				MatroskaSpec::TrackType(2),
				MatroskaSpec::CodecID("A_OPUS".to_string()),
				MatroskaSpec::CodecPrivate(opus_head),
				MatroskaSpec::Audio(Master::Full(vec![
					MatroskaSpec::SamplingFrequency(48000.0),
					MatroskaSpec::Channels(2),
				])),
			])),
		])),
		MatroskaSpec::Segment(Master::End),
	];

	let mut dest = Cursor::new(Vec::new());
	{
		let mut writer = WebmWriter::new(&mut dest);
		for tag in &tags {
			writer.write(tag).unwrap();
		}
	}
	dest.into_inner()
}
