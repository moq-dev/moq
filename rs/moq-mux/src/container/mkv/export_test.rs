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

use crate::container::test_util::{IDR, Live, PPS, SPS, raw_frame, video_frame};

/// A drift budget no test timeline comes close to, so the exporter reads every group.
///
/// The media track's full retention window, so an exporter started after publishing
/// can still read every retained group. These tests import a whole file and only then
/// export it, which the exporter's default
/// [`std::time::Duration::ZERO`](std::time::Duration::ZERO) collapses to the live edge:
/// completeness has to be asked for, exactly as a real recorder does.
const RECORDING_MAX_DELAY: std::time::Duration = std::time::Duration::from_secs(30);

/// An AVC rendition that declares a reorder depth of two.
fn declared_avc() -> Live {
	Live::new(".avc1", |catalog, name| {
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::H264 {
			profile: 100,
			constraints: 0,
			level: 10,
			inline: false,
		});
		config.coded_width = Some(64);
		config.coded_height = Some(64);
		config.reorder = Some(2);
		config.container = hang::catalog::Container::Legacy;
		config.description = Some(
			crate::codec::h264::build_avcc(
				&[Bytes::from_static(crate::codec::h264::fixtures::SPS_PYRAMID)],
				&[Bytes::from_static(PPS)],
			)
			.unwrap(),
		);
		catalog.modify().unwrap().video.renditions.insert(name, config);
	})
}

#[tokio::test(start_paused = true)]
async fn a_declared_depth_bounds_malformed_video_by_count_and_payload() {
	for backwards in [false, true] {
		for count in [16, 128, 1024] {
			for size in [256, 16_384] {
				let mut live = declared_avc();
				for index in 0..count {
					let mut frame = raw_frame(1_000_000 - if backwards { index } else { 0 }, b"", index == 0);
					let mut bytes = vec![0; size];
					bytes[..4].copy_from_slice(&((size - 4) as u32).to_be_bytes());
					bytes[4] = if index == 0 { 0x65 } else { 0x41 };
					frame.payload = bytes.into();
					live.track.write(frame).unwrap();
				}
				live.track.finish().unwrap();
				live.catalog.finish().unwrap();
				let mut export = super::Export::new(live.source().await, live.catalog_stream().await)
					.with_fragment_duration(std::time::Duration::ZERO)
					.with_replay();
				export.next().await.unwrap().unwrap();
				let mut refused = false;
				loop {
					match export.next().await {
						Err(err) => {
							assert!(err.to_string().contains("video"), "{err}");
							refused = true;
							break;
						}
						Ok(None) => break,
						Ok(Some(_)) => {}
					}
					assert!(export.held() <= 2, "a depth-two clock");
				}
				assert!(refused, "backwards={backwards} count={count} size={size}");
				assert!(export.held() <= 2 && export.buffered() <= 4);
			}
		}
	}
}

/// MKV retains PTS while signed, group-local DTS orders video against audio.
#[tokio::test(start_paused = true)]
async fn declared_reorder_starts_before_zero_and_resets_each_group() {
	let mut live = declared_avc();
	let mut audio = live.add_track("audio", |catalog, name| {
		let mut config = hang::catalog::AudioConfig::new(hang::catalog::AAC { profile: 2 }, 44100, 2);
		config.description = Some(Bytes::from_static(&[0x12, 0x10]));
		config.container = hang::catalog::Container::Legacy;
		catalog.modify().unwrap().audio.renditions.insert(name, config);
	});
	for start in [0, 400_000] {
		for (index, offset) in [0, 160_000, 80_000, 40_000, 120_000].into_iter().enumerate() {
			let payload = if index == 0 {
				&b"\x00\x00\x00\x01\x65"[..]
			} else {
				&b"\x00\x00\x00\x01\x41"[..]
			};
			live.track
				.write(raw_frame(start + offset, payload, index == 0))
				.unwrap();
		}
		audio.write(raw_frame(start, b"audio", true)).unwrap();
	}
	live.track.finish().unwrap();
	audio.finish().unwrap();
	live.catalog.finish().unwrap();
	let mut export = super::Export::new(live.source().await, live.catalog_stream().await)
		.with_delay(std::time::Duration::from_secs(5))
		.with_fragment_duration(std::time::Duration::ZERO)
		.with_replay();
	export.next().await.unwrap().unwrap();
	let mut timestamps = Vec::new();
	while let Some(chunk) = export.next().await.unwrap() {
		let mut cursor = Cursor::new(chunk);
		let mut base = 0;
		for tag in WebmIterator::new(&mut cursor, &[]) {
			match tag.unwrap() {
				MatroskaSpec::Timestamp(timestamp) => base = timestamp as i64,
				MatroskaSpec::SimpleBlock(bytes) => {
					let block = SimpleBlock::try_from(bytes.as_slice()).unwrap();
					timestamps.push(base + i64::from(block.timestamp));
				}
				_ => {}
			}
		}
	}
	assert_eq!(timestamps, [0, 160, 80, 0, 40, 120, 400, 560, 480, 400, 440, 520]);
}

#[tokio::test(start_paused = true)]
async fn export_header_roundtrip_vp9_opus() {
	// Build a tiny synthetic WebM with one VP9 video track and one Opus audio track.
	let import_bytes = synth_webm();

	// Ingest into a broadcast.
	let broadcast = moq_net::broadcast::Info::new();
	let mut producer = broadcast.produce();
	let consumer = producer.consume();

	let catalog = crate::catalog::Producer::new(&mut producer, crate::catalog::Config::default()).unwrap();
	let mut importer = crate::container::mkv::Import::new(producer, catalog.reserve());
	let buf = bytes::BytesMut::from(import_bytes.as_slice());
	importer.decode(&buf).unwrap();
	importer.finish().unwrap();

	// Now subscribe via the exporter and pull bytes.
	let catalog_stream = crate::catalog::Consumer::<()>::new(&consumer, crate::catalog::CatalogFormat::Hang)
		.await
		.expect("catalog consumer");
	let mut exporter = crate::container::mkv::Export::new(crate::source::announced(&consumer).await, catalog_stream);

	// First `next()` should give us the header (EBML + Segment-start + Info + Tracks).
	let header = moq_clock::timeout(std::time::Duration::from_secs(1), exporter.next())
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
	let catalog2 = crate::catalog::Producer::new(&mut broadcast2, crate::catalog::Config::default()).unwrap();
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

/// The catalog names HE-AAC's output rate; Matroska wants the SBR core's rate as
/// SamplingFrequency and the output as OutputSamplingFrequency.
#[test]
fn build_he_aac_audio_track_entry() {
	// fdkaacenc HE-AAC at 48 kHz stereo: SBR over a 24 kHz LC core.
	let mut config = hang::catalog::AudioConfig::new(AudioCodec::AAC(hang::catalog::AAC { profile: 5 }), 48_000, 2);
	config.description = Some(Bytes::from_static(&[0x2B, 0x11, 0x88, 0x00]));
	assert_eq!(audio_rates(&config), (24_000.0, Some(48_000.0)));

	// HE-AACv2: the same rates over a mono PS core.
	config.codec = AudioCodec::AAC(hang::catalog::AAC { profile: 29 });
	config.description = Some(Bytes::from_static(&[0xEB, 0x09, 0x88, 0x00]));
	assert_eq!(audio_rates(&config), (24_000.0, Some(48_000.0)));

	// AAC-LC 48 kHz stereo plays at its own rate.
	config.codec = AudioCodec::AAC(hang::catalog::AAC { profile: 2 });
	config.description = Some(Bytes::from_static(&[0x11, 0x90]));
	assert_eq!(audio_rates(&config), (48_000.0, None));

	// AAC-LC keeps the catalog's rate without parsing its description (a reserved rate index here).
	config.description = Some(Bytes::from_static(&[0x16, 0x90]));
	assert_eq!(audio_rates(&config), (48_000.0, None));
}

/// The SamplingFrequency and OutputSamplingFrequency of an exported audio track entry.
fn audio_rates(config: &hang::catalog::AudioConfig) -> (f64, Option<f64>) {
	let entry = super::export::build_audio_track_entry(1, config).expect("build audio entry");
	let MatroskaSpec::TrackEntry(Master::Full(children)) = entry else {
		panic!("expected a TrackEntry");
	};
	let audio = children
		.into_iter()
		.find_map(|c| match c {
			MatroskaSpec::Audio(Master::Full(audio)) => Some(audio),
			_ => None,
		})
		.expect("an Audio element");
	let sampling = audio.iter().find_map(|c| match c {
		MatroskaSpec::SamplingFrequency(rate) => Some(*rate),
		_ => None,
	});
	let output = audio.iter().find_map(|c| match c {
		MatroskaSpec::OutputSamplingFrequency(rate) => Some(*rate),
		_ => None,
	});
	(sampling.expect("SamplingFrequency"), output)
}

/// MP3 (config in band, no codec private) survives an import -> export -> re-import
/// round trip as the `A_MPEG/L3` track entry.
#[tokio::test(start_paused = true)]
async fn export_header_roundtrip_mp3() {
	let import_bytes = synth_matroska_mp3();

	let broadcast = moq_net::broadcast::Info::new();
	let mut producer = broadcast.produce();
	let consumer = producer.consume();

	let catalog = crate::catalog::Producer::new(&mut producer, crate::catalog::Config::default()).unwrap();
	let mut importer = crate::container::mkv::Import::new(producer, catalog.reserve());
	importer
		.decode(&bytes::BytesMut::from(import_bytes.as_slice()))
		.unwrap();
	importer.finish().unwrap();

	let catalog_stream = crate::catalog::Consumer::<()>::new(&consumer, crate::catalog::CatalogFormat::Hang)
		.await
		.expect("catalog consumer");
	let mut exporter = crate::container::mkv::Export::new(crate::source::announced(&consumer).await, catalog_stream);

	let header = moq_clock::timeout(std::time::Duration::from_secs(1), exporter.next())
		.await
		.expect("exporter timed out")
		.expect("exporter result")
		.expect("expected header bytes");

	// Re-import the exported header and confirm the codec rebuilds.
	let mut broadcast2 = moq_net::broadcast::Info::new().produce();
	let catalog2 = crate::catalog::Producer::new(&mut broadcast2, crate::catalog::Config::default()).unwrap();
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
	let _catalog = crate::catalog::Producer::new(&mut producer, crate::catalog::Config::default()).unwrap();

	let catalog_stream = crate::catalog::Consumer::<()>::new(&consumer, crate::catalog::CatalogFormat::Hang)
		.await
		.unwrap();
	let mut exporter = crate::container::mkv::Export::new(crate::source::announced(&consumer).await, catalog_stream);

	// next() must remain pending (timing out), not surface a "no catalog
	// snapshot" error from a vacuously-ready empty track set.
	let result = moq_clock::timeout(std::time::Duration::from_secs(1), exporter.next()).await;
	assert!(
		result.is_err(),
		"exporter should stay pending before any rendition arrives, got {result:?}"
	);

	drop(producer);
}

/// A catalog may publish a codec before its optional dimensions. The MKV
/// header is immutable, so derive geometry from the first keyframe before
/// writing `PixelWidth` and `PixelHeight`.
#[tokio::test(start_paused = true)]
async fn export_derives_video_geometry_before_header() {
	use hang::catalog::{Container, VideoConfig};

	let mut live = Live::new(".vp8", |catalog, name| {
		let mut config = VideoConfig::new(VideoCodec::VP8);
		config.container = Container::Legacy;
		catalog.modify().unwrap().video.renditions.insert(name, config);
	});
	// Geometry-less startup frames must not park the source before the keyframe.
	live.track.write(raw_frame(0, &[0x31, 0x00, 0x00], true)).unwrap();
	live.track
		.write(raw_frame(
			33_000,
			&[0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0x40, 0x01, 0xf0, 0x00],
			true,
		))
		.unwrap();
	live.track.finish().unwrap();

	let mut exporter = crate::container::mkv::Export::new(live.source().await, live.catalog_stream().await);
	let header = moq_clock::timeout(std::time::Duration::from_secs(1), exporter.next())
		.await
		.expect("exporter timed out")
		.expect("exporter result")
		.expect("expected header bytes");

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let mut importer = crate::container::mkv::Import::new(broadcast, catalog.reserve());
	importer.decode(&bytes::BytesMut::from(header.as_ref())).unwrap();
	let video = catalog.snapshot().video.renditions.values().next().unwrap().clone();
	assert_eq!(video.coded_width, Some(320));
	assert_eq!(video.coded_height, Some(240));
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

	let catalog = crate::catalog::Producer::new(&mut producer, crate::catalog::Config::default()).unwrap();
	let mut importer = crate::container::mkv::Import::new(producer, catalog.reserve());
	let buf = bytes::BytesMut::from(import_bytes.as_slice());
	importer.decode(&buf).unwrap();
	importer.finish().unwrap();

	let catalog_stream = crate::catalog::Consumer::<()>::new(&consumer, crate::catalog::CatalogFormat::Hang)
		.await
		.expect("catalog consumer");
	let mut exporter = crate::container::mkv::Export::new(crate::source::announced(&consumer).await, catalog_stream)
		// Use per-frame clustering so each frame is observable as its own
		// Cluster chunk; batching is exercised in a dedicated test below.
		.with_fragment_duration(std::time::Duration::ZERO)
		.with_delay(RECORDING_MAX_DELAY)
		.with_replay();
	let mut exported: Vec<u8> = Vec::new();

	let mut importer = Some(importer);
	for _ in 0..32 {
		let next = moq_clock::timeout(std::time::Duration::from_millis(100), exporter.next()).await;
		match next {
			Ok(Ok(Some(chunk))) => exported.extend_from_slice(&chunk),
			Ok(Ok(None)) => break,
			Ok(Err(e)) => panic!("exporter error: {e}"),
			Err(_) => {
				if let Some(deadline) = exporter.next_deadline() {
					tokio::time::advance(deadline.saturating_duration_since(moq_clock::now())).await;
				} else {
					importer = None;
				}
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
	let cat2 = crate::catalog::Producer::new(&mut bcast2, crate::catalog::Config::default()).unwrap();
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
	// A catalog advertising a Cmaf-container video track: the exporter should bail.
	use hang::catalog::{Container, H264, VideoConfig};

	let live = Live::new(".avc1", |catalog, name| {
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
		catalog.modify().unwrap().video.renditions.insert(name, config);
	});

	let mut exporter = crate::container::mkv::Export::new(live.source().await, live.catalog_stream().await);
	let result = moq_clock::timeout(std::time::Duration::from_secs(1), exporter.next())
		.await
		.expect("exporter timed out");

	let err = result.expect_err("expected an error");
	assert!(err.to_string().contains("CMAF"), "expected CMAF rejection, got: {err}");
}

#[tokio::test(start_paused = true)]
async fn export_avc3_source_synthesizes_avcc_and_length_prefixes() {
	// Avc3-shape source: frames in Annex-B with inline SPS+PPS before keyframes.
	// The exporter must (a) defer the header until SPS+PPS arrive, (b) emit avcC
	// in CodecPrivate, (c) length-prefix the sample bytes in each SimpleBlock.

	// P-slice (0x61) behind an Annex-B start code.
	const PSLICE_ANNEXB: &[u8] = &[0, 0, 0, 1, 0x61, 0xe0, 0x12, 0x34];
	let pslice = &PSLICE_ANNEXB[4..];

	let mut live = Live::avc3();
	live.track.write(video_frame(0, true)).unwrap();
	live.track.write(raw_frame(33_000, PSLICE_ANNEXB, false)).unwrap();
	live.track.finish().unwrap();
	live.catalog.finish().unwrap();

	let mut exporter = crate::container::mkv::Export::new(live.source().await, live.catalog_stream().await)
		.with_fragment_duration(std::time::Duration::ZERO);
	let mut exported: Vec<u8> = Vec::new();

	let mut live = Some(live);
	for _ in 0..32 {
		let next = moq_clock::timeout(std::time::Duration::from_millis(100), exporter.next()).await;
		match next {
			Ok(Ok(Some(chunk))) => exported.extend_from_slice(&chunk),
			Ok(Ok(None)) => break,
			Ok(Err(e)) => panic!("exporter error: {e}"),
			Err(_) => {
				// Drop the producers to close the broadcast so the exporter can EOS.
				live = None;
			}
		}
	}
	drop(live);
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
	assert_eq!(avcc[1], SPS[1], "AVCProfileIndication");
	assert_eq!(avcc[2], SPS[2], "profile_compatibility");
	assert_eq!(avcc[3], SPS[3], "AVCLevelIndication");
	assert_eq!(avcc[4] & 0x03, 3, "lengthSizeMinusOne");

	// SPS should be embedded after the 5+1=6 byte header.
	let sps_len = u16::from_be_bytes([avcc[6], avcc[7]]) as usize;
	assert_eq!(sps_len, SPS.len());
	assert_eq!(&avcc[8..8 + sps_len], SPS);

	// PPS follows: 1 byte numPPS + 2 byte length + PPS bytes.
	let pps_offset = 8 + sps_len + 3;
	assert_eq!(&avcc[pps_offset..pps_offset + PPS.len()], PPS);

	// Both frames should be length-prefixed (no Annex-B start codes).
	assert_eq!(sample_payloads.len(), 2, "expected 2 sample blocks");

	// First frame: keyframe contained SPS+PPS+IDR. SPS/PPS are stripped, only
	// IDR makes it through as length-prefixed.
	let first = &sample_payloads[0];
	let idr_len = u32::from_be_bytes([first[0], first[1], first[2], first[3]]) as usize;
	assert_eq!(idr_len, IDR.len(), "IDR length prefix");
	assert_eq!(&first[4..4 + idr_len], IDR, "IDR payload");

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
	let cat2 = crate::catalog::Producer::new(&mut bcast2, crate::catalog::Config::default()).unwrap();
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

	let mut catalog = crate::catalog::Producer::new(&mut producer, crate::catalog::Config::default()).unwrap();
	let mut importer = crate::container::mkv::Import::new(producer, catalog.reserve());
	let buf = bytes::BytesMut::from(import_bytes.as_slice());
	importer.decode(&buf).unwrap();
	importer.finish().unwrap();
	catalog.finish().unwrap();

	let catalog_stream = crate::catalog::Consumer::<()>::new(&consumer, crate::catalog::CatalogFormat::Hang)
		.await
		.expect("catalog consumer");
	let mut exporter = crate::container::mkv::Export::new(crate::source::announced(&consumer).await, catalog_stream)
		.with_fragment_duration(std::time::Duration::from_secs(2))
		.with_delay(RECORDING_MAX_DELAY)
		.with_replay();
	let mut exported: Vec<u8> = Vec::new();

	let mut importer = Some(importer);
	for _ in 0..32 {
		let next = moq_clock::timeout(std::time::Duration::from_millis(100), exporter.next()).await;
		match next {
			Ok(Ok(Some(chunk))) => exported.extend_from_slice(&chunk),
			Ok(Ok(None)) => break,
			Ok(Err(e)) => panic!("exporter error: {e}"),
			Err(_) => {
				if let Some(deadline) = exporter.next_deadline() {
					tokio::time::advance(deadline.saturating_duration_since(moq_clock::now())).await;
				} else {
					importer = None;
				}
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

#[tokio::test(start_paused = true)]
async fn zero_fragment_duration_emits_without_successor() {
	let mut live = Live::avc3();
	live.track.write(video_frame(0, true)).unwrap();

	let mut exporter = crate::container::mkv::Export::new(live.source().await, live.catalog_stream().await)
		.with_fragment_duration(std::time::Duration::ZERO);
	let header = chunk_now(&mut exporter).await;
	let cluster = chunk_now(&mut exporter).await;
	assert_eq!(count_blocks(&header, &cluster), 1, "the cluster carries the live frame");
}

#[tokio::test(start_paused = true)]
async fn reconfigured_zero_duration_drains_open_cluster() {
	let mut live = Live::avc3();
	live.track.write(video_frame(0, true)).unwrap();
	live.track.write(video_frame(33_000, false)).unwrap();

	// Default per-GOP clustering: both frames land in one open cluster.
	let mut exporter = crate::container::mkv::Export::new(live.source().await, live.catalog_stream().await);
	let header = chunk_now(&mut exporter).await;
	moq_clock::timeout(std::time::Duration::from_millis(1), exporter.next())
		.await
		.expect_err("the GOP cluster must stay open while the producer is live");

	let mut exporter = exporter.with_fragment_duration(std::time::Duration::ZERO);
	live.track.write(video_frame(66_000, false)).unwrap();

	let leftover = chunk_now(&mut exporter).await;
	let per_frame = chunk_now(&mut exporter).await;
	assert_eq!(count_blocks(&header, &leftover), 2, "the open GOP cluster rolls first");
	assert_eq!(
		count_blocks(&header, &per_frame),
		1,
		"the live frame gets its own cluster"
	);
}

/// The next chunk, required to be ready without another frame arriving.
async fn chunk_now(exporter: &mut crate::container::mkv::Export<crate::catalog::Consumer>) -> Bytes {
	moq_clock::timeout(std::time::Duration::from_millis(1), exporter.next())
		.await
		.expect("waited for a successor frame")
		.expect("exporter failed")
		.expect("expected a chunk")
}

/// Every chunk the exporter can produce without waiting for more input, or its error.
async fn drain_now(exporter: &mut crate::container::mkv::Export<crate::catalog::Consumer>) -> crate::Result<Vec<u8>> {
	let mut out = Vec::new();
	while let Ok(next) = moq_clock::timeout(std::time::Duration::from_millis(1), exporter.next()).await {
		match next? {
			Some(chunk) => out.extend_from_slice(&chunk),
			None => break,
		}
	}
	Ok(out)
}

/// A rendition the catalog moves to another track, in its broadcast or a sibling one, writes
/// that track's media from then on under the track number the header declared, and none of
/// the track it left.
#[tokio::test(start_paused = true)]
async fn a_moved_rendition_continues_on_its_new_track() {
	use crate::container::test_util::{Moving, marked_frame, marks};

	for (track, broadcast) in [("b", None), ("a", Some("other"))] {
		let mut moving = Moving::new();
		let mut exporter = crate::container::mkv::Export::new(moving.source().await, moving.catalog_stream().await);
		let mut out = Vec::new();
		for i in 0..45 {
			moving.a.write(marked_frame(i, 0xa1)).unwrap();
			out.extend(drain_now(&mut exporter).await.unwrap());
		}

		moving.point(track, broadcast, SPS);
		out.extend(drain_now(&mut exporter).await.unwrap());
		let next = match broadcast {
			None => &mut moving.b,
			Some(_) => &mut moving.other,
		};
		for i in 45..120 {
			moving.a.write(marked_frame(i, 0xa2)).unwrap();
			if i >= 60 {
				next.write(marked_frame(i, 0xb1)).unwrap();
			}
			out.extend(drain_now(&mut exporter).await.unwrap());
		}

		let expected: Vec<u8> = [vec![0xa1; 45], vec![0xb1; 30]].concat();
		assert_eq!(marks(&out), expected, "moved to {track} in {broadcast:?}");
		let numbers: std::collections::HashSet<u64> = WebmIterator::new(&mut Cursor::new(out), &[])
			.filter_map(|tag| match tag {
				Ok(MatroskaSpec::SimpleBlock(data)) => Some(SimpleBlock::try_from(&data).unwrap().track),
				_ => None,
			})
			.collect();
		assert_eq!(numbers.len(), 1, "every block is written under the declared track");
	}
}

/// The header's TrackEntry carries the rendition's first avcC, which a move to a track
/// described by another SPS cannot change mid-file. It fails before writing any of that track.
#[tokio::test(start_paused = true)]
async fn a_rendition_moved_to_another_codec_configuration_fails() {
	use crate::container::test_util::{Moving, marked_frame, marks};

	let mut moving = Moving::new();
	let mut exporter = crate::container::mkv::Export::new(moving.source().await, moving.catalog_stream().await);
	let mut out = Vec::new();
	for i in 0..30 {
		moving.a.write(marked_frame(i, 0xa1)).unwrap();
		out.extend(drain_now(&mut exporter).await.unwrap());
	}

	moving.point("b", None, crate::container::test_util::Moving::MAIN_SPS);
	out.extend(drain_now(&mut exporter).await.unwrap());
	moving.b.write(marked_frame(30, 0xb1)).unwrap();
	let err = drain_now(&mut exporter).await.unwrap_err();
	assert!(
		matches!(&err, crate::Error::Mkv(crate::container::mkv::Error::TrackChanged(name)) if name == Moving::RENDITION),
		"{err:?}"
	);
	assert!(!marks(&out).contains(&0xb1));
}

/// A move to a CMAF track is refused like a CMAF rendition listed from the start.
#[tokio::test(start_paused = true)]
async fn a_rendition_moved_to_a_cmaf_track_fails() {
	use crate::container::test_util::{Moving, marked_frame};

	let mut moving = Moving::new();
	let mut exporter = crate::container::mkv::Export::new(moving.source().await, moving.catalog_stream().await);
	for i in 0..30 {
		moving.a.write(marked_frame(i, 0xa1)).unwrap();
		drain_now(&mut exporter).await.unwrap();
	}

	let mut config = Moving::config("b", None, SPS);
	config.container = hang::catalog::Container::Cmaf {
		init: Bytes::from(vec![0u8; 32]),
	};
	moving
		.catalog
		.modify()
		.unwrap()
		.video
		.renditions
		.insert(Moving::RENDITION.to_string(), config);
	let err = drain_now(&mut exporter).await.unwrap_err();
	assert!(
		matches!(&err, crate::Error::Mkv(crate::container::mkv::Error::UnsupportedCmafTrack { name, .. }) if name == Moving::RENDITION),
		"{err:?}"
	);
}

/// SimpleBlock count of one cluster, parsed behind the file header.
fn count_blocks(header: &[u8], cluster: &[u8]) -> usize {
	let mut cursor = Cursor::new([header, cluster].concat());
	WebmIterator::new(&mut cursor, &[])
		.filter(|tag| matches!(tag, Ok(MatroskaSpec::SimpleBlock(_))))
		.count()
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

/// A live container export holds a frame for the configured release delay.
#[tokio::test(start_paused = true)]
async fn configured_delay_holds_live_frames() {
	use crate::container::test_util::{Live, raw_frame};
	use hang::catalog::{AAC, AudioConfig};
	let mut config = AudioConfig::new(AAC { profile: 2 }, 44100, 2);
	config.description = Some(Bytes::from_static(&[0x12, 0x10]));
	let mut live = Live::audio(config);
	let mut export = super::Export::new(live.source().await, live.catalog_stream().await)
		.with_delay(std::time::Duration::from_millis(100))
		.with_fragment_duration(Some(std::time::Duration::ZERO));
	assert!(export.next().await.unwrap().is_some(), "container header");
	live.track.write(raw_frame(0, &[0xde, 0xad], true)).unwrap();
	assert!(
		moq_clock::timeout(std::time::Duration::from_millis(1), export.next())
			.await
			.is_err(),
		"the release delay must hold a live frame instead of emitting on arrival"
	);
}

/// Release deadlines survive source EOS, and expired frames never reach the container.
#[tokio::test(start_paused = true)]
async fn delay_drops_late_frames_and_drains_the_final_frame() {
	use crate::container::test_util::{Live, raw_frame};
	use hang::catalog::{AAC, AudioConfig};
	let mut config = AudioConfig::new(AAC { profile: 2 }, 44100, 2);
	config.description = Some(Bytes::from_static(&[0x12, 0x10]));
	let mut live = Live::audio(config);
	let delay = std::time::Duration::from_millis(100);
	let mut export = super::Export::new(live.source().await, live.catalog_stream().await)
		.with_delay(delay)
		.with_fragment_duration(std::time::Duration::ZERO);
	export.next().await.unwrap().unwrap();
	let start = moq_clock::now();
	live.track.write(raw_frame(0, b"first", true)).unwrap();
	let first = export.next().await.unwrap().unwrap();
	assert!(first.windows(5).any(|part| part == b"first"));
	assert!(start.elapsed() >= delay);

	// This frame's 120ms deadline has already passed.
	tokio::time::advance(std::time::Duration::from_millis(200)).await;
	assert!(export.poll_next(&kio::Waiter::noop()).is_pending());
	live.track.write(raw_frame(20_000, b"expired", true)).unwrap();
	// Sent a delay ahead: any further would be a step in its timeline, which re-acquires.
	live.track.write(raw_frame(300_000, b"final", true)).unwrap();
	live.track.finish().unwrap();
	live.catalog.finish().unwrap();
	let final_frame = export.next().await.unwrap().unwrap();
	assert!(final_frame.windows(5).any(|part| part == b"final"));
	assert!(!final_frame.windows(7).any(|part| part == b"expired"));
	assert!(start.elapsed() >= std::time::Duration::from_millis(400));
	assert!(export.next().await.unwrap().is_none());
}

#[tokio::test(start_paused = true)]
async fn decode_order_is_independent_of_cross_track_arrival_order() {
	use crate::container::test_util::{Live, raw_frame};
	use hang::catalog::{AAC, AudioConfig, Container};
	for reversed in [false, true] {
		let mut config = AudioConfig::new(AAC { profile: 2 }, 44100, 2);
		config.description = Some(Bytes::from_static(&[0x12, 0x10]));
		config.container = Container::Legacy;
		let mut live = Live::audio(config.clone());
		let mut other = live.add_track("other", |catalog, name| {
			catalog.modify().unwrap().audio.renditions.insert(name, config);
		});
		let mut export = super::Export::new(live.source().await, live.catalog_stream().await)
			.with_delay(std::time::Duration::from_millis(100))
			.with_fragment_duration(std::time::Duration::ZERO);
		export.next().await.unwrap().unwrap();
		live.track.write(raw_frame(0, b"initial-a", true)).unwrap();
		other.write(raw_frame(0, b"initial-b", true)).unwrap();
		export.next().await.unwrap().unwrap();
		export.next().await.unwrap().unwrap();
		if reversed {
			other.write(raw_frame(100_000, b"earlier", true)).unwrap();
		} else {
			live.track.write(raw_frame(200_000, b"later", true)).unwrap();
		}
		assert!(export.poll_next(&kio::Waiter::noop()).is_pending());
		tokio::time::advance(std::time::Duration::from_millis(20)).await;
		if reversed {
			live.track.write(raw_frame(200_000, b"later", true)).unwrap();
		} else {
			other.write(raw_frame(100_000, b"earlier", true)).unwrap();
		}
		let first = export.next().await.unwrap().unwrap();
		let second = export.next().await.unwrap().unwrap();
		assert!(first.windows(7).any(|part| part == b"earlier"));
		assert!(second.windows(5).any(|part| part == b"later"));
	}
}

#[tokio::test(start_paused = true)]
async fn reordered_video_uses_decode_deadlines_but_keeps_block_pts() {
	use crate::container::test_util::{Live, raw_frame, video_frame};
	use hang::catalog::{AAC, AudioConfig, Container};
	let mut live = Live::avc3();
	for config in live.catalog.modify().unwrap().video.renditions.values_mut() {
		config.jitter = Some(std::time::Duration::from_millis(120));
	}
	let mut audio = live.add_track("audio", |catalog, name| {
		let mut config = AudioConfig::new(AAC { profile: 2 }, 44100, 2);
		config.description = Some(Bytes::from_static(&[0x12, 0x10]));
		config.container = Container::Legacy;
		catalog.modify().unwrap().audio.renditions.insert(name, config);
	});
	for (index, timestamp) in [1000, 1120, 1040, 1080].into_iter().enumerate() {
		live.track.write(video_frame(timestamp * 1000, index == 0)).unwrap();
	}
	audio.write(raw_frame(1_060_000, b"audio", true)).unwrap();
	live.track.finish().unwrap();
	audio.finish().unwrap();
	live.catalog.finish().unwrap();
	let mut export = super::Export::new(live.source().await, live.catalog_stream().await)
		.with_delay(std::time::Duration::from_secs(5))
		.with_fragment_duration(std::time::Duration::ZERO)
		.with_replay();
	export.next().await.unwrap().unwrap();
	let mut timestamps = Vec::new();
	while let Some(chunk) = export.next().await.unwrap() {
		let mut cursor = Cursor::new(chunk);
		let mut base = 0;
		for tag in WebmIterator::new(&mut cursor, &[]) {
			match tag.unwrap() {
				MatroskaSpec::Timestamp(timestamp) => base = timestamp as i64,
				MatroskaSpec::SimpleBlock(bytes) => {
					let block = SimpleBlock::try_from(bytes.as_slice()).unwrap();
					timestamps.push(base + i64::from(block.timestamp));
				}
				_ => {}
			}
		}
	}
	assert_eq!(timestamps, [1000, 1120, 1040, 1060, 1080]);
}

/// A backlog the source already holds, as after the sink stalls, goes out a frame at a time
/// as each falls due, rather than being read into the export whole first.
#[tokio::test(start_paused = true)]
async fn a_backlog_stays_in_the_source_until_due() {
	use crate::container::test_util::{Live, raw_frame};
	use hang::catalog::{AAC, AudioConfig};
	for delay in [std::time::Duration::ZERO, std::time::Duration::from_millis(100)] {
		let mut config = AudioConfig::new(AAC { profile: 2 }, 44100, 2);
		config.description = Some(Bytes::from_static(&[0x12, 0x10]));
		let mut live = Live::audio(config);
		let mut export = super::Export::new(live.source().await, live.catalog_stream().await)
			.with_delay(delay)
			.with_fragment_duration(std::time::Duration::ZERO);
		export.next().await.unwrap().unwrap();
		live.track.write(raw_frame(0, b"first", true)).unwrap();
		export.next().await.unwrap().unwrap();
		for index in 1..=50 {
			tokio::time::advance(std::time::Duration::from_millis(20)).await;
			live.track.write(raw_frame(index * 20_000, b"backlog", false)).unwrap();
		}
		for _ in 1..=50 {
			let cluster = export.next().await.unwrap().unwrap();
			assert!(cluster.windows(7).any(|part| part == b"backlog"));
			assert!(
				export.buffered() <= 1,
				"{delay:?}: {} frames buffered",
				export.buffered()
			);
		}
	}
}

/// A joiner reads the retained group to its live edge to anchor the clock, then holds no
/// more than the delay of it.
#[tokio::test(start_paused = true)]
async fn a_retained_group_trims_to_the_delay() {
	use crate::container::test_util::{Live, raw_frame};
	use hang::catalog::{AAC, AudioConfig};
	let mut config = AudioConfig::new(AAC { profile: 2 }, 44100, 2);
	config.description = Some(Bytes::from_static(&[0x12, 0x10]));
	let mut live = Live::audio(config);
	for index in 0..50 {
		live.track
			.write(raw_frame(index * 20_000, b"retained", index == 0))
			.unwrap();
		tokio::time::advance(std::time::Duration::from_millis(20)).await;
	}
	let delay = std::time::Duration::from_millis(100);
	let mut export = super::Export::new(live.source().await, live.catalog_stream().await)
		.with_delay(delay)
		.with_fragment_duration(std::time::Duration::ZERO);
	export.next().await.unwrap().unwrap();
	export.next().await.unwrap().unwrap();
	// What is left spans no more than the delay: five 20 ms frames.
	assert!(export.buffered() <= 5, "{} frames buffered", export.buffered());
}

/// A disabled rendition stays out of the header. One disabled after the header keeps its
/// track, idle, without ending the export, and resumes when it re-enables.
#[tokio::test(start_paused = true)]
async fn disabled_renditions_stay_out_of_the_header_and_idle_after_it() {
	use hang::catalog::{AAC, AudioConfig, Container};

	let mut config = AudioConfig::new(AAC { profile: 2 }, 44100, 2);
	config.description = Some(Bytes::from_static(&[0x12, 0x10]));
	config.container = Container::Legacy;
	let mut live = Live::audio(config.clone());
	let main = live.track.name().to_string();
	let mut other = live.add_track("other", |catalog, name| {
		catalog.modify().unwrap().audio.renditions.insert(name, config.clone());
	});
	let _disabled = live.add_track("disabled", |catalog, name| {
		let mut config = config.clone();
		config.enabled = false;
		catalog.modify().unwrap().audio.renditions.insert(name, config);
	});
	let set_enabled = |live: &mut Live, enabled: bool| {
		live.catalog
			.modify()
			.unwrap()
			.audio
			.renditions
			.get_mut(&main)
			.unwrap()
			.enabled = enabled;
	};

	let mut export = super::Export::new(live.source().await, live.catalog_stream().await)
		.with_fragment_duration(std::time::Duration::ZERO);
	let header = chunk_now(&mut export).await;
	let mut cursor = Cursor::new(header.as_ref());
	let entries = WebmIterator::new(&mut cursor, &[])
		.filter(|tag| matches!(tag, Ok(MatroskaSpec::TrackEntry(Master::Start))))
		.count();
	assert_eq!(entries, 2, "the disabled rendition is left out");

	set_enabled(&mut live, false);
	other.write(raw_frame(0, b"other-0", true)).unwrap();
	assert!(chunk_now(&mut export).await.windows(7).any(|part| part == b"other-0"));

	set_enabled(&mut live, true);
	live.track.write(raw_frame(20_000, b"main-1", true)).unwrap();
	assert!(chunk_now(&mut export).await.windows(6).any(|part| part == b"main-1"));
}

/// Video whose PTS stops advancing past the reorder lookahead in an open group: the export
/// holds no more than what arrives within the lookahead and the delay. Flat PTS waits the
/// catalog `jitter`; PTS running backward reads as reordering up to the deepest supported.
#[tokio::test(start_paused = true)]
async fn stalled_presentation_time_keeps_the_export_bounded() {
	use std::time::Duration;
	let step = Duration::from_millis(20);
	let delay = Duration::from_millis(100);
	let jitter = Duration::from_millis(120);
	let deepest = Duration::from_secs(2);
	let flat: fn(u64) -> u64 = |_| 10_000_000;
	let backward: fn(u64) -> u64 = |index| 10_000_000 - index * 10_000;
	for (name, pts, lookahead) in [("flat", flat, jitter), ("backward", backward, deepest)] {
		let mut live = Live::avc3();
		for config in live.catalog.modify().unwrap().video.renditions.values_mut() {
			config.jitter = Some(jitter);
		}
		let mut export = super::Export::new(live.source().await, live.catalog_stream().await)
			.with_delay(delay)
			.with_fragment_duration(Duration::ZERO);
		live.track.write(video_frame(pts(0), true)).unwrap();
		export.next().await.unwrap().unwrap();
		let mut most = 0;
		for index in 1..500 {
			tokio::time::advance(step).await;
			live.track.write(video_frame(pts(index), false)).unwrap();
			while let std::task::Poll::Ready(chunk) = export.poll_next(&kio::Waiter::noop()) {
				chunk.unwrap().unwrap();
			}
			most = most.max(export.buffered());
		}
		// What arrived within the wait, the frame just read, and one due to go out.
		let bound = ((lookahead + delay).as_millis() / step.as_millis()) as usize + 2;
		assert!(most <= bound, "{name}: {most} frames buffered, over {bound}");
	}
}

/// Live B-frames, read as they are sent, keep every frame and its presentation order: the
/// bound on what the decode clock holds never cuts a valid reorder short.
#[tokio::test(start_paused = true)]
async fn live_reordered_video_keeps_every_frame() {
	use std::time::Duration;
	let frame = 40_000;
	let mut live = Live::avc3();
	for config in live.catalog.modify().unwrap().video.renditions.values_mut() {
		config.jitter = Some(Duration::from_millis(120));
	}
	let mut export = super::Export::new(live.source().await, live.catalog_stream().await)
		.with_delay(Duration::from_millis(500))
		.with_fragment_duration(Duration::ZERO);
	// A B-pyramid in decode order, a group of nine 40 ms frames at a time.
	let pyramid = [0, 4, 2, 1, 3, 8, 6, 5, 7];
	let order: Vec<u64> = (0..10u64)
		.flat_map(|group| pyramid.map(|i| 1_000_000 + (group * 9 + i) * frame))
		.collect();
	live.track.write(video_frame(order[0], true)).unwrap();
	export.next().await.unwrap().unwrap();
	let mut blocks = 0;
	let mut count = |chunk: Bytes| {
		let mut cursor = Cursor::new(chunk);
		blocks += WebmIterator::new(&mut cursor, &[])
			.filter(|tag| matches!(tag, Ok(MatroskaSpec::SimpleBlock(_))))
			.count();
	};
	for (index, &pts) in order.iter().enumerate().skip(1) {
		tokio::time::advance(Duration::from_micros(frame)).await;
		live.track.write(video_frame(pts, index % 9 == 0)).unwrap();
		while let std::task::Poll::Ready(chunk) = export.poll_next(&kio::Waiter::noop()) {
			count(chunk.unwrap().unwrap());
		}
	}
	live.track.finish().unwrap();
	live.catalog.finish().unwrap();
	while let Some(chunk) = export.next().await.unwrap() {
		count(chunk);
	}
	assert_eq!(blocks, order.len());
}
