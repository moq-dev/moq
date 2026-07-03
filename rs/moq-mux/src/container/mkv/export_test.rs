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
	let broadcast = moq_net::broadcast::Info::new();
	let mut producer = broadcast.produce();
	let consumer = producer.consume();

	let catalog = crate::catalog::Producer::new(&mut producer).unwrap();
	let mut importer = crate::container::mkv::Import::new(producer, catalog.reserve());
	let buf = bytes::BytesMut::from(import_bytes.as_slice());
	importer.decode(&buf).unwrap();
	importer.finish().unwrap();

	// Now subscribe via the exporter and pull bytes.
	let catalog_stream = crate::catalog::Consumer::<()>::new(&consumer, crate::catalog::CatalogFormat::Hang)
		.await
		.expect("catalog consumer");
	let mut exporter = crate::container::mkv::Export::new(consumer, catalog_stream);

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
	let mut broadcast2 = moq_net::broadcast::Info::new().produce();
	let catalog2 = crate::catalog::Producer::new(&mut broadcast2).unwrap();
	let mut importer2 = crate::container::mkv::Import::new(broadcast2, catalog2.reserve());
	let hbuf = bytes::BytesMut::from(header.as_ref());
	importer2.decode(&hbuf).unwrap();
	let snap = catalog2.snapshot();
	assert_eq!(snap.video.renditions.len(), 1);
	assert_eq!(snap.audio.renditions.len(), 1);

	let v = snap.video.renditions.values().next().unwrap();
	assert!(matches!(v.codec, VideoCodec::VP9(_)));
	let a = snap.audio.renditions.values().next().unwrap();
	assert!(matches!(a.codec, AudioCodec::Opus));
	assert_eq!(a.sample_rate, 48000);
}

/// A FLAC rendition exports as an `A_FLAC` track whose CodecPrivate is the catalog
/// description (the `fLaC` header), which round-trips back through the importer.
#[test]
fn build_flac_audio_track_entry() {
	let description = crate::codec::flac::Config {
		min_block_size: 4096,
		max_block_size: 4096,
		min_frame_size: 0,
		max_frame_size: 0,
		sample_rate: 48_000,
		channel_count: 2,
		bits_per_sample: 16,
		total_samples: 0,
		md5: [0; 16],
	}
	.description();

	let mut config = hang::catalog::AudioConfig::new(AudioCodec::Flac, 48_000, 2);
	config.description = Some(description.clone());

	let entry = super::export::build_audio_track_entry(2, &config).expect("build A_FLAC entry");
	let MatroskaSpec::TrackEntry(Master::Full(children)) = entry else {
		panic!("expected a TrackEntry");
	};

	let codec_id = children
		.iter()
		.find_map(|c| {
			if let MatroskaSpec::CodecID(s) = c {
				Some(s.clone())
			} else {
				None
			}
		})
		.expect("codec id");
	assert_eq!(codec_id, "A_FLAC");

	let private = children
		.iter()
		.find_map(|c| {
			if let MatroskaSpec::CodecPrivate(p) = c {
				Some(p.clone())
			} else {
				None
			}
		})
		.expect("codec private");
	// CodecPrivate is the FLAC header verbatim, ready for the importer to parse.
	assert_eq!(private, description.to_vec());
	crate::codec::flac::Config::parse(&mut private.as_slice()).expect("valid FLAC header");
}

/// MP3 (config in band, no codec private) survives an import -> export -> re-import
/// round trip as the `A_MPEG/L3` track entry.
#[tokio::test(start_paused = true)]
async fn export_header_roundtrip_mp3() {
	let import_bytes = synth_matroska_mp3();

	let broadcast = moq_net::broadcast::Info::new();
	let mut producer = broadcast.produce();
	let consumer = producer.consume();

	let catalog = crate::catalog::Producer::new(&mut producer).unwrap();
	let mut importer = crate::container::mkv::Import::new(producer, catalog.reserve());
	importer
		.decode(&bytes::BytesMut::from(import_bytes.as_slice()))
		.unwrap();
	importer.finish().unwrap();

	let catalog_stream = crate::catalog::Consumer::<()>::new(&consumer, crate::catalog::CatalogFormat::Hang)
		.await
		.expect("catalog consumer");
	let mut exporter = crate::container::mkv::Export::new(consumer, catalog_stream);

	let header = tokio::time::timeout(std::time::Duration::from_secs(1), exporter.next())
		.await
		.expect("exporter timed out")
		.expect("exporter result")
		.expect("expected header bytes");

	// Re-import the exported header and confirm the codec rebuilds.
	let mut broadcast2 = moq_net::broadcast::Info::new().produce();
	let catalog2 = crate::catalog::Producer::new(&mut broadcast2).unwrap();
	let mut importer2 = crate::container::mkv::Import::new(broadcast2, catalog2.reserve());
	importer2.decode(&bytes::BytesMut::from(header.as_ref())).unwrap();

	let snap = catalog2.snapshot();
	assert_eq!(snap.audio.renditions.len(), 1);
	let a = snap.audio.renditions.values().next().unwrap();
	assert!(matches!(a.codec, AudioCodec::Mp3));
	assert_eq!(a.sample_rate, 44100);
	assert_eq!(a.channel_count, 2);
}

/// Build a small Matroska with a single MP3 audio track (no codec private).
fn synth_matroska_mp3() -> Vec<u8> {
	use webm_iterable::WebmWriter;

	let tags: Vec<MatroskaSpec> = vec![
		MatroskaSpec::Ebml(Master::Full(vec![
			MatroskaSpec::DocType("matroska".to_string()),
			MatroskaSpec::DocTypeVersion(2),
			MatroskaSpec::DocTypeReadVersion(2),
		])),
		MatroskaSpec::Segment(Master::Start),
		MatroskaSpec::Info(Master::Full(vec![MatroskaSpec::TimestampScale(1_000_000)])),
		MatroskaSpec::Tracks(Master::Full(vec![MatroskaSpec::TrackEntry(Master::Full(vec![
			MatroskaSpec::TrackNumber(1),
			MatroskaSpec::TrackUID(1),
			MatroskaSpec::TrackType(2),
			MatroskaSpec::CodecID("A_MPEG/L3".to_string()),
			MatroskaSpec::Audio(Master::Full(vec![
				MatroskaSpec::SamplingFrequency(44100.0),
				MatroskaSpec::Channels(2),
			])),
		]))])),
		MatroskaSpec::Cluster(Master::Start),
		MatroskaSpec::Timestamp(0),
		SimpleBlock::new_uncheked(b"mp3-frame", 1, 0, false, None, false, true).into(),
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

/// A mid-stream subscriber may poll the exporter before the catalog track has
/// arrived. With `tracks` empty, `header_ready()` must not be vacuously true and
/// drive `build_header` into a "no catalog snapshot" error; it should stay
/// pending until the catalog lands.
#[tokio::test(start_paused = true)]
async fn export_waits_for_catalog_before_header() {
	let broadcast = moq_net::broadcast::Info::new();
	let mut producer = broadcast.produce();
	let consumer = producer.consume();

	// The catalog track exists (so the subscriber can attach) but no renditions
	// have been published yet: `tracks` stays empty on the first polls.
	let _catalog = crate::catalog::Producer::new(&mut producer).unwrap();

	let catalog_stream = crate::catalog::Consumer::<()>::new(&consumer, crate::catalog::CatalogFormat::Hang)
		.await
		.unwrap();
	let mut exporter = crate::container::mkv::Export::new(consumer, catalog_stream);

	// next() must remain pending (timing out), not surface a "no catalog
	// snapshot" error from a vacuously-ready empty track set.
	let result = tokio::time::timeout(std::time::Duration::from_secs(1), exporter.next()).await;
	assert!(
		result.is_err(),
		"exporter should stay pending before any rendition arrives, got {result:?}"
	);

	drop(producer);
}

#[tokio::test(start_paused = true)]
async fn export_emits_blocks_for_each_frame() {
	// Import a WebM that contains 3 video frames + 2 audio frames, export it,
	// and assert that the exported byte stream parses back into the same number
	// of SimpleBlock elements with the right track assignments.
	let import_bytes = synth_webm_with_frames();

	let broadcast = moq_net::broadcast::Info::new();
	let mut producer = broadcast.produce();
	let consumer = producer.consume();

	let catalog = crate::catalog::Producer::new(&mut producer).unwrap();
	let mut importer = crate::container::mkv::Import::new(producer, catalog.reserve());
	let buf = bytes::BytesMut::from(import_bytes.as_slice());
	importer.decode(&buf).unwrap();
	importer.finish().unwrap();

	let catalog_stream = crate::catalog::Consumer::<()>::new(&consumer, crate::catalog::CatalogFormat::Hang)
		.await
		.expect("catalog consumer");
	let mut exporter = crate::container::mkv::Export::new(consumer, catalog_stream)
		// Use per-frame clustering so each frame is observable as its own
		// Cluster chunk; batching is exercised in a dedicated test below.
		.with_fragment_duration(std::time::Duration::ZERO);
	let mut exported: Vec<u8> = Vec::new();

	let mut importer = Some(importer);
	for _ in 0..32 {
		let next = tokio::time::timeout(std::time::Duration::from_millis(100), exporter.next()).await;
		match next {
			Ok(Ok(Some(chunk))) => exported.extend_from_slice(&chunk),
			Ok(Ok(None)) => break,
			Ok(Err(e)) => panic!("exporter error: {e}"),
			Err(_) => {
				// Drop the importer to close the broadcast so the exporter can EOS.
				importer = None;
			}
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
	let mut bcast2 = moq_net::broadcast::Info::new().produce();
	let cat2 = crate::catalog::Producer::new(&mut bcast2).unwrap();
	let mut imp2 = crate::container::mkv::Import::new(bcast2, cat2.reserve());
	let rt = bytes::BytesMut::from(exported.as_slice());
	imp2.decode(&rt).unwrap();
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

	let broadcast = moq_net::broadcast::Info::new();
	let mut producer = broadcast.produce();
	let consumer = producer.consume();

	let mut catalog = crate::catalog::Producer::new(&mut producer).unwrap();
	let track = producer
		.create_track(
			producer.unique_name(".avc1"),
			moq_net::track::Info::default().with_timescale(hang::container::TIMESCALE),
		)
		.unwrap();
	let mut config = VideoConfig::new(H264 {
		profile: 0x64,
		constraints: 0,
		level: 0x1f,
		inline: false,
	});
	config.coded_width = Some(640);
	config.coded_height = Some(480);
	config.description = Some(Bytes::from(vec![0u8; 8]));
	config.container = Container::Cmaf {
		init: Bytes::from(vec![0u8; 32]),
	};
	catalog.lock().video.renditions.insert(track.name().to_string(), config);

	let catalog_stream = crate::catalog::Consumer::<()>::new(&consumer, crate::catalog::CatalogFormat::Hang)
		.await
		.expect("catalog consumer");
	let mut exporter = crate::container::mkv::Export::new(consumer, catalog_stream);
	let result = tokio::time::timeout(std::time::Duration::from_secs(1), exporter.next())
		.await
		.expect("exporter timed out");

	let err = result.expect_err("expected an error");
	assert!(err.to_string().contains("CMAF"), "expected CMAF rejection, got: {err}");
}

#[tokio::test(start_paused = true)]
async fn export_avc3_source_synthesizes_avcc_and_length_prefixes() {
	// Avc3-shape source: H264 { inline: true }, description = None, frames in
	// Annex-B with inline SPS+PPS before keyframes. The exporter must
	// (a) defer the header until SPS+PPS arrive, (b) emit avcC in CodecPrivate,
	// (c) length-prefix the sample bytes in each SimpleBlock.
	use hang::catalog::{Container, H264, VideoConfig};
	use moq_net::Timestamp;

	let broadcast = moq_net::broadcast::Info::new();
	let mut producer = broadcast.produce();
	let consumer = producer.consume();

	let mut catalog = crate::catalog::Producer::new(&mut producer).unwrap();
	let track = producer
		.create_track(
			producer.unique_name(".avc3"),
			moq_net::track::Info::default().with_timescale(hang::container::TIMESCALE),
		)
		.unwrap();
	let mut config = VideoConfig::new(H264 {
		profile: 0x42,
		constraints: 0xc0,
		level: 0x1f,
		inline: true,
	});
	config.coded_width = Some(320);
	config.coded_height = Some(240);
	config.container = Container::Legacy;
	catalog.lock().video.renditions.insert(track.name().to_string(), config);

	// Annex-B start code.
	const SC: &[u8] = &[0, 0, 0, 1];
	let sps = &[0x67u8, 0x42, 0xc0, 0x1f, 0xde, 0xad, 0xbe, 0xef][..];
	let pps = &[0x68u8, 0xce, 0x3c, 0x80][..];
	let idr = &[0x65u8, 0x88, 0x84, 0x21, 0x00, 0x11, 0x22, 0x33][..];
	let pslice = &[0x61u8, 0xe0, 0x12, 0x34][..];

	let mut keyframe_payload = bytes::BytesMut::new();
	for nal in [sps, pps, idr] {
		keyframe_payload.extend_from_slice(SC);
		keyframe_payload.extend_from_slice(nal);
	}
	let keyframe_payload = keyframe_payload.freeze();

	let mut pslice_payload = bytes::BytesMut::new();
	pslice_payload.extend_from_slice(SC);
	pslice_payload.extend_from_slice(pslice);
	let pslice_payload = pslice_payload.freeze();

	// Publish frames via the container::Producer.
	let mut track_producer = crate::container::Producer::new(track, crate::catalog::hang::Container::Legacy);
	track_producer
		.write(crate::container::Frame {
			timestamp: Timestamp::from_micros(0).unwrap(),
			payload: keyframe_payload,
			keyframe: true,
			duration: None,
		})
		.unwrap();
	track_producer
		.write(crate::container::Frame {
			timestamp: Timestamp::from_micros(33_000).unwrap(),
			payload: pslice_payload,
			keyframe: false,
			duration: None,
		})
		.unwrap();
	track_producer.finish().unwrap();
	let mut catalog = catalog;
	catalog.finish().unwrap();

	let catalog_stream = crate::catalog::Consumer::<()>::new(&consumer, crate::catalog::CatalogFormat::Hang)
		.await
		.expect("catalog consumer");
	let mut exporter =
		crate::container::mkv::Export::new(consumer, catalog_stream).with_fragment_duration(std::time::Duration::ZERO);
	let mut exported: Vec<u8> = Vec::new();

	let mut held_producer = Some(producer);
	for _ in 0..32 {
		let next = tokio::time::timeout(std::time::Duration::from_millis(100), exporter.next()).await;
		match next {
			Ok(Ok(Some(chunk))) => exported.extend_from_slice(&chunk),
			Ok(Ok(None)) => break,
			Ok(Err(e)) => panic!("exporter error: {e}"),
			Err(_) => {
				held_producer = None;
			}
		}
	}
	drop(held_producer);
	drop(catalog);
	drop(track_producer);
	drop(exporter);

	// Parse the exported MKV and inspect the Tracks element and SimpleBlocks.
	let mut cursor = Cursor::new(exported.as_slice());
	let iter = WebmIterator::new(
		&mut cursor,
		&[
			MatroskaSpec::Tracks(Master::Start),
			MatroskaSpec::TrackEntry(Master::Start),
		],
	);

	let mut codec_id: Option<String> = None;
	let mut codec_private: Option<Vec<u8>> = None;
	let mut sample_payloads: Vec<Vec<u8>> = Vec::new();

	for tag in iter {
		match tag.expect("parse exported") {
			MatroskaSpec::Tracks(Master::Full(entries)) => {
				for entry in entries {
					if let MatroskaSpec::TrackEntry(Master::Full(children)) = entry {
						for c in children {
							match c {
								MatroskaSpec::CodecID(s) => codec_id = Some(s),
								MatroskaSpec::CodecPrivate(p) => codec_private = Some(p),
								_ => {}
							}
						}
					}
				}
			}
			MatroskaSpec::SimpleBlock(data) => {
				let sb = SimpleBlock::try_from(data.as_slice()).expect("parse SimpleBlock");
				sample_payloads.push(sb.raw_frame_data().to_vec());
			}
			_ => {}
		}
	}

	assert_eq!(codec_id.as_deref(), Some("V_MPEG4/ISO/AVC"));

	// avcC layout: configurationVersion=1, AVCProfileIndication, profile_compat,
	// AVCLevelIndication, lengthSizeMinusOne=3, numSPS=1, SPS_len(u16), SPS,
	// numPPS=1, PPS_len(u16), PPS.
	let avcc = codec_private.expect("avcC in CodecPrivate");
	assert_eq!(avcc[0], 1, "configurationVersion");
	assert_eq!(avcc[1], sps[1], "AVCProfileIndication");
	assert_eq!(avcc[2], sps[2], "profile_compatibility");
	assert_eq!(avcc[3], sps[3], "AVCLevelIndication");
	assert_eq!(avcc[4] & 0x03, 3, "lengthSizeMinusOne");

	// SPS should be embedded after the 5+1=6 byte header.
	let sps_len = u16::from_be_bytes([avcc[6], avcc[7]]) as usize;
	assert_eq!(sps_len, sps.len());
	assert_eq!(&avcc[8..8 + sps_len], sps);

	// PPS follows: 1 byte numPPS + 2 byte length + PPS bytes.
	let pps_offset = 8 + sps_len + 3;
	assert_eq!(&avcc[pps_offset..pps_offset + pps.len()], pps);

	// Both frames should be length-prefixed (no Annex-B start codes).
	assert_eq!(sample_payloads.len(), 2, "expected 2 sample blocks");

	// First frame: keyframe contained SPS+PPS+IDR. SPS/PPS are stripped, only
	// IDR makes it through as length-prefixed.
	let first = &sample_payloads[0];
	let idr_len = u32::from_be_bytes([first[0], first[1], first[2], first[3]]) as usize;
	assert_eq!(idr_len, idr.len(), "IDR length prefix");
	assert_eq!(&first[4..4 + idr_len], idr, "IDR payload");

	// Second frame: P-slice, length-prefixed.
	let second = &sample_payloads[1];
	let pslice_len = u32::from_be_bytes([second[0], second[1], second[2], second[3]]) as usize;
	assert_eq!(pslice_len, pslice.len(), "P-slice length prefix");
	assert_eq!(&second[4..4 + pslice_len], pslice, "P-slice payload");

	// Round-trip: feed the exported MKV back through the Mkv importer and
	// verify the catalog rebuilds as an H264 (avc1-shape) rendition with the
	// avcC carried through as `description`. This catches subtle structural
	// mistakes in the avcC layout that the slot-by-slot check above might
	// pass even when the record as a whole is malformed.
	let mut bcast2 = moq_net::broadcast::Info::new().produce();
	let cat2 = crate::catalog::Producer::new(&mut bcast2).unwrap();
	let mut imp2 = crate::container::mkv::Import::new(bcast2, cat2.reserve());
	let rt = bytes::BytesMut::from(exported.as_slice());
	imp2.decode(&rt).unwrap();
	imp2.finish().unwrap();
	let snap = cat2.snapshot();
	assert_eq!(snap.video.renditions.len(), 1);
	let v = snap.video.renditions.values().next().unwrap();
	assert!(matches!(v.codec, hang::catalog::VideoCodec::H264(_)));
	assert_eq!(
		v.description.as_ref().map(|b| b.as_ref()),
		Some(avcc.as_slice()),
		"re-imported description should equal the avcC we wrote"
	);
}

#[tokio::test(start_paused = true)]
async fn export_fragment_duration_batches_blocks() {
	// With fragment_duration = 2s and 5 frames within 100ms, all 5 SimpleBlocks
	// should land in ONE Cluster (vs 5 separate Clusters in per-frame mode).
	let import_bytes = synth_webm_with_frames();

	let broadcast = moq_net::broadcast::Info::new();
	let mut producer = broadcast.produce();
	let consumer = producer.consume();

	let mut catalog = crate::catalog::Producer::new(&mut producer).unwrap();
	let mut importer = crate::container::mkv::Import::new(producer, catalog.reserve());
	let buf = bytes::BytesMut::from(import_bytes.as_slice());
	importer.decode(&buf).unwrap();
	importer.finish().unwrap();
	catalog.finish().unwrap();

	let catalog_stream = crate::catalog::Consumer::<()>::new(&consumer, crate::catalog::CatalogFormat::Hang)
		.await
		.expect("catalog consumer");
	let mut exporter = crate::container::mkv::Export::new(consumer, catalog_stream)
		.with_fragment_duration(std::time::Duration::from_secs(2));
	let mut exported: Vec<u8> = Vec::new();

	let mut importer = Some(importer);
	for _ in 0..32 {
		let next = tokio::time::timeout(std::time::Duration::from_millis(100), exporter.next()).await;
		match next {
			Ok(Ok(Some(chunk))) => exported.extend_from_slice(&chunk),
			Ok(Ok(None)) => break,
			Ok(Err(e)) => panic!("exporter error: {e}"),
			Err(_) => {
				importer = None;
			}
		}
	}
	drop(importer);
	drop(catalog);
	drop(exporter);

	// Count Clusters and SimpleBlocks in the export.
	let mut cursor = Cursor::new(exported.as_slice());
	let iter = WebmIterator::new(&mut cursor, &[]);
	let mut cluster_count = 0;
	let mut block_count = 0;
	for tag in iter {
		match tag.expect("parse") {
			MatroskaSpec::Cluster(Master::Start) => cluster_count += 1,
			MatroskaSpec::SimpleBlock(_) => block_count += 1,
			_ => {}
		}
	}

	assert_eq!(block_count, 5, "all blocks should be emitted");
	assert_eq!(cluster_count, 1, "all blocks should batch into one cluster");
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
