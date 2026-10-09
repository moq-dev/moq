//! Tests for the MPEG-TS importer.
//!
//! `bbb.ts` is `fmp4/test_data/bbb.mp4` remuxed to MPEG-TS with `ffmpeg -c copy`
//! (H.264 + AAC), so it exercises the real demux -> codec path.

use bytes::BytesMut;

/// A drift budget no test timeline comes close to, so the reader sees every group.
///
/// The media track's full retention window, so a reader started after importing can
/// still read every retained group. These tests import a whole file first, which the default
/// [`std::time::Duration::ZERO`](std::time::Duration::ZERO) budget collapses to the live edge:
/// completeness has to be asked for.
const RECORDING_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(30);
/// How long a drain waits for the next frame: past the recording delay, the mux-ahead
/// window a multiplex rate adds on top of it, and the clip, so the first frame goes out,
/// and then until output stops.
const DRAIN: std::time::Duration = RECORDING_MAX_AGE
	.saturating_mul(3)
	.saturating_add(std::time::Duration::from_secs(1));

/// Decode a whole TS buffer into a fresh broadcast and return the catalog.
fn import_ts(data: &[u8]) -> crate::catalog::hang::Catalog {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();

	let mut import = crate::container::ts::Import::new(broadcast, catalog.reserve());
	let buf = BytesMut::from(data);
	import.decode(&buf).unwrap();
	import.finish().unwrap();

	catalog.snapshot()
}

/// Like [`import_ts`], with a catalog that carries the `mpegts` section.
fn import_ts_ext(data: &[u8]) -> crate::catalog::hang::Catalog<crate::container::ts::Ext> {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(
		&mut broadcast,
		crate::catalog::Config::default()
			.with_catalog(crate::catalog::hang::Catalog::<crate::container::ts::Ext>::default()),
	)
	.unwrap();

	let mut import = crate::container::ts::Import::new(broadcast, catalog.reserve());
	import.decode(data).unwrap();
	import.finish().unwrap();

	catalog.snapshot()
}

#[test]
fn import_bbb_catalog() {
	let data = include_bytes!("test_data/bbb.ts");
	let catalog = import_ts(data);

	assert_eq!(catalog.video.renditions.len(), 1, "expected one H.264 track");
	assert_eq!(catalog.audio.renditions.len(), 1, "expected one AAC track");

	let video = catalog.video.renditions.values().next().unwrap();
	// TS H.264 is in-band Annex-B, so it surfaces as avc3 (not the out-of-band avc1).
	assert!(
		video.codec.to_string().starts_with("avc3"),
		"video codec was {}",
		video.codec
	);

	let audio = catalog.audio.renditions.values().next().unwrap();
	assert!(
		audio.codec.to_string().starts_with("mp4a"),
		"audio codec was {}",
		audio.codec
	);
	// AAC must carry a synthesized AudioSpecificConfig so downstream consumers
	// that need out-of-band config (fMP4/MKV export, WebCodecs) can configure.
	assert!(audio.description.is_some(), "AAC track missing AudioSpecificConfig");
}

#[tokio::test(start_paused = true)]
async fn public_container_preserves_loc_for_ts() {
	let data = include_bytes!("test_data/bbb.ts");
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let reserved = catalog.reserve();
	let mut import = super::Import::new(broadcast, reserved).with_container(hang::catalog::Container::Loc);
	import.decode(data).unwrap();
	import.finish().unwrap();

	let snapshot = catalog.snapshot();
	let (name, config) = snapshot.video.renditions.iter().next().unwrap();
	assert_eq!(config.container, hang::catalog::Container::Loc);

	let track = consumer.track(name).unwrap().subscribe(None).await.unwrap();
	let mut media = crate::container::Consumer::new(
		track,
		crate::catalog::hang::Container::Loc(crate::container::Kind::Data),
	);
	let frame = tokio::time::timeout(std::time::Duration::from_secs(1), media.read())
		.await
		.unwrap()
		.unwrap()
		.unwrap();
	assert!(frame.keyframe);
	assert!(!frame.payload.is_empty());
}

/// The Kyrion capture is H.264 1080i with B-frames (open-GOP, ~5-frame reorder despite only
/// 3 consecutive B-frames). Its video rendition's `jitter` must capture that reorder delay
/// (the source `PTS - DTS`), not just the ~33 ms frame interval, so a transmuxer/player sizes
/// its decode buffer correctly. The stream is ~30 fps, so the reorder is ~5 * 33 ms.
#[test]
fn import_kyrion_video_jitter_captures_reorder() {
	let data = include_bytes!("test_data/scte35/kyrion_dirtystart.ts");
	let catalog = import_ts(data);

	let video = catalog.video.renditions.values().next().expect("a video track");
	let jitter = video.jitter.expect("B-frame stream must publish jitter").as_millis();
	// ~5 frames of reorder at ~30 fps is ~167 ms, far above the ~33 ms frame interval.
	assert!(
		(150..=200).contains(&jitter),
		"jitter {jitter} ms should reflect the ~5-frame reorder, not the frame interval"
	);
}

/// The Kyrion capture carries two real MP2 programs (stream_type 0x03, 48 kHz
/// stereo, 192 kbps). Both must surface as catalog renditions with the
/// header-derived config and no description (verbatim carriage).
#[test]
fn import_kyrion_mp2_catalog() {
	let data = include_bytes!("test_data/scte35/kyrion_dirtystart.ts");
	let catalog = import_ts(data);

	assert_eq!(catalog.audio.renditions.len(), 2, "expected both MP2 tracks");
	for (name, audio) in &catalog.audio.renditions {
		assert_eq!(audio.codec.to_string(), "mp2", "track {name}");
		assert_eq!(audio.sample_rate, 48_000, "track {name}");
		assert_eq!(audio.channel_count, 2, "track {name}");
		assert!(
			audio.description.is_none(),
			"verbatim MP2 needs no description (track {name})"
		);
	}
}

/// `ac3.ts` is an ffmpeg-authored audio-only ATSC AC-3 program (stream_type
/// 0x81 plus the 'AC-3' registration descriptor), regenerated with:
/// `ffmpeg -f lavfi -i sine=frequency=440:sample_rate=48000:duration=0.5
/// -ac 6 -c:a ac3 -b:a 384k -f mpegts ac3.ts`. The 5.1 layout exercises the
/// lfeon bit: 5 full-bandwidth channels (acmod 3/2) plus the LFE = 6.
#[test]
fn import_ac3_catalog() {
	let data = include_bytes!("test_data/ac3.ts");
	let catalog = import_ts(data);

	assert_eq!(catalog.video.renditions.len(), 0);
	assert_eq!(catalog.audio.renditions.len(), 1, "expected one AC-3 track");
	let audio = catalog.audio.renditions.values().next().unwrap();
	assert_eq!(audio.codec.to_string(), "ac-3");
	assert_eq!(audio.sample_rate, 48_000);
	assert_eq!(audio.channel_count, 6, "5 full-bandwidth channels + LFE");
	assert!(audio.description.is_none(), "verbatim AC-3 needs no description");
}

/// `aac_quad.ts` is an ffmpeg-authored audio-only AAC program in quad, which has no
/// channelConfiguration, so its ADTS headers carry 0 and the first raw data block leads with a
/// program config element. Regenerated with (ffmpeg 9.0.1):
/// `ffmpeg -f lavfi -i sine=frequency=440:sample_rate=48000:duration=0.1
/// -af "pan=quad|FL=c0|FR=c0|BL=c0|BR=c0" -c:a aac -b:a 128k -f mpegts aac_quad.ts`.
#[test]
fn import_aac_program_config_catalog() {
	let data = include_bytes!("test_data/aac_quad.ts");
	let catalog = import_ts(data);

	assert_eq!(catalog.audio.renditions.len(), 1, "expected one AAC track");
	let audio = catalog.audio.renditions.values().next().unwrap();
	assert_eq!(audio.codec.to_string(), "mp4a.40.2");
	assert_eq!(audio.sample_rate, 48_000);
	assert_eq!(
		audio.channel_count, 4,
		"two channel pair elements, not a guessed stereo"
	);

	// The element moved into the description is byte-for-byte what ffmpeg itself writes as the
	// AudioSpecificConfig for the same stream in MP4, minus the trailing SBR sync extension.
	let mut expected = vec![0x11, 0x80, 0x04, 0xC4, 0x04, 0x00, 0x21, 0x10, 0x0C];
	expected.extend_from_slice(b"Lavc63.1.101");
	assert_eq!(audio.description.as_deref(), Some(expected.as_slice()));
}

/// `opus.ts` is an ffmpeg-authored audio-only Opus program (private stream_type 0x06
/// plus the 'Opus' registration and DVB extension descriptors), generated with:
/// `ffmpeg -f lavfi -i sine=frequency=440:sample_rate=48000:duration=0.5
/// -ac 2 -c:a libopus -b:a 96k -f mpegts opus.ts`. It validates our importer against
/// real ffmpeg control-header framing, not just our own exporter's.
#[test]
fn import_opus_catalog() {
	let data = include_bytes!("test_data/opus.ts");
	let catalog = import_ts(data);

	assert_eq!(catalog.video.renditions.len(), 0);
	assert_eq!(catalog.audio.renditions.len(), 1, "expected one Opus track");
	let audio = catalog.audio.renditions.values().next().unwrap();
	assert_eq!(audio.codec.to_string(), "opus");
	assert_eq!(audio.sample_rate, 48_000, "Opus is always reckoned at 48 kHz");
	assert_eq!(audio.channel_count, 2);
}

/// `opus_5_1.ts` is a 440 Hz center channel in 5.1, which ffmpeg's libopus
/// encodes as family 1 and its muxer labels `channel_config_code` 6:
/// `ffmpeg -f lavfi -i sine=frequency=440:sample_rate=48000:duration=0.5
/// -ac 6 -c:a libopus -b:a 128k -f mpegts opus_5_1.ts`. The descriptor names only
/// the channel count, so the importer must synthesize the Vorbis mapping table
/// or the track has no OpusHead a decoder accepts.
#[test]
fn import_opus_surround_catalog() {
	let data = include_bytes!("test_data/opus_5_1.ts");
	let catalog = import_ts(data);

	assert_eq!(catalog.audio.renditions.len(), 1, "expected one Opus track");
	let audio = catalog.audio.renditions.values().next().unwrap();
	assert_eq!(audio.channel_count, 6);

	let head = crate::codec::opus::Config::parse(&mut audio.description.as_deref().expect("an OpusHead")).unwrap();
	assert_eq!(head.channel_count, 6);
	let mapping = head.mapping.expect("a family 1 mapping");
	assert_eq!(mapping.family(), 1);
	assert_eq!((mapping.streams(), mapping.coupled()), (4, 2));
	assert_eq!(mapping.table(), &[0, 4, 1, 2, 3, 5]);
}

/// Opus frames from real ffmpeg output must decode: a non-empty run of Opus packets,
/// each a plausible size (the control header was stripped, not left in the payload).
#[tokio::test(start_paused = true)]
async fn import_opus_frames() {
	let data = include_bytes!("test_data/opus.ts");

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.reserve());
	import.decode(&BytesMut::from(&data[..])).unwrap();
	import.finish().unwrap();

	let name = catalog
		.snapshot()
		.audio
		.renditions
		.keys()
		.next()
		.expect("an Opus track")
		.clone();

	let track = consumer
		.track(&name)
		.unwrap()
		.subscribe(moq_net::track::Subscription::default().with_max_delay(RECORDING_MAX_AGE))
		.await
		.unwrap();
	let mut reader = crate::container::Consumer::new(
		track,
		crate::catalog::hang::Container::Legacy(crate::container::Kind::Data),
	);
	let mut frames = Vec::new();
	while let Ok(res) = tokio::time::timeout(std::time::Duration::from_millis(50), reader.read()).await {
		let Some(frame) = res.unwrap() else { break };
		frames.push(frame.payload);
	}

	assert!(frames.len() > 5, "expected a run of Opus packets, got {}", frames.len());
	for frame in &frames {
		assert!(!frame.is_empty(), "Opus packet must not be empty");
		// The Opus-in-TS control header starts with 0x7f; a stripped packet must not.
		assert_ne!(frame[0], 0x7f, "control header was not stripped");
	}
}

/// An Opus config comes from the PMT, before any PES, yet the catalog is first published at the
/// first PES, carrying the clock it anchors rather than a provisional one a copy-once reader
/// would keep.
#[tokio::test]
async fn opus_catalog_carries_the_anchored_clock() {
	// An hour in, so the anchored clock lands far from the provisional one.
	let data = shift_clock(include_bytes!("test_data/opus.ts"), 3_600 * 90_000);

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let provisional = catalog.clock().wall();
	let mut clocks = crate::container::test_util::Clocks::subscribe(&consumer).await;
	let mut import = crate::container::ts::Import::new(broadcast, catalog.reserve());

	// Packet by packet, so a snapshot published at the PMT is seen before the PES replaces it.
	let mut published = Vec::new();
	for pkt in data.chunks(188) {
		import.decode(pkt).unwrap();
		published.extend(clocks.drain());
	}
	import.finish().unwrap();
	published.extend(clocks.drain());

	let anchored = catalog.clock().wall();
	assert_ne!(anchored, provisional, "the first PES anchors the clock");
	assert!(!published.is_empty(), "the catalog publishes");
	assert!(published.iter().all(|clock| *clock == Some(anchored)), "{published:?}");
}

/// `eac3.ts` is an ffmpeg-authored audio-only ATSC E-AC-3 program (stream_type
/// 0x87 plus the 'EAC3' registration descriptor), regenerated with:
/// `ffmpeg -f lavfi -i sine=frequency=440:sample_rate=48000:duration=0.5
/// -ac 6 -c:a eac3 -b:a 256k -f mpegts eac3.ts`. 5.1 exercises lfeon, like
/// the AC-3 fixture.
#[test]
fn import_eac3_catalog() {
	let data = include_bytes!("test_data/eac3.ts");
	let catalog = import_ts(data);

	assert_eq!(catalog.video.renditions.len(), 0);
	assert_eq!(catalog.audio.renditions.len(), 1, "expected one E-AC-3 track");
	let audio = catalog.audio.renditions.values().next().unwrap();
	assert_eq!(audio.codec.to_string(), "ec-3");
	assert_eq!(audio.sample_rate, 48_000);
	assert_eq!(audio.channel_count, 6, "5 full-bandwidth channels + LFE");
	assert!(audio.description.is_none(), "verbatim E-AC-3 needs no description");
}

/// A second real Ateme Kyrion capture, this time in ATSC TS-compliance mode:
/// MPEG-2 video (Main, 1080i CBR), AC-3 (0x81 + 'AC-3' registration descriptor,
/// bsid 6, stereo) and MP2 (0x03, stereo) at 48 kHz, SCTE-35 cues, a dedicated
/// PCR PID, ATSC PSIP tables, and a dirty mid-stream start. Audio surfaces as
/// two typed renditions; MPEG-2 video is clock-only, so no video rendition.
///
/// `kyrion_mpeg2av_ac3_tsduck.txt` holds two TSDuck dumps of the capture, with
/// the regen command above each: the three splice_inserts (CRC32 OK; cues only
/// document the capture, the audio path is what's under test) and the PMT,
/// which evidences that the Kyrion itself pairs stream_type 0x81 with the
/// 'AC-3' registration descriptor, the same announcement our export writes.
/// `bbb_cbr.ts` is `scte35/bbb5s.ts` remuxed at a constant 400 kb/s (`ffmpeg -i
/// scte35/bbb5s.ts -t 2.8 -c copy -muxrate 400000 -f mpegts bbb_cbr.ts`): ffmpeg
/// byte-locks the PCR to the mux rate and fills the rest with null packets, so the
/// whole-multiplex rate lands in the catalog exactly.
#[test]
fn import_records_the_cbr_mux_rate() {
	let data = include_bytes!("test_data/bbb_cbr.ts");
	let catalog = import_ts_ext(data);

	let rate = catalog.ext.mpegts.mux_rate.expect("a CBR source records its mux rate");
	assert!(
		rate.abs_diff(400_000) * 1000 <= 400_000,
		"mux rate {rate} is not within 0.1% of 400 kb/s"
	);
	let json = serde_json::to_string(&catalog.ext).unwrap();
	assert!(
		json.contains("\"muxRate\":"),
		"the rate rides the mpegts section: {json}"
	);
}

/// The Kyrion stamps its clock at half-millisecond granularity and its rate wanders
/// a percent between half-second samples, which is what a hardware multiplexer's
/// "constant" looks like; the window still settles on what `tsanalyze` measures.
#[test]
fn import_records_a_hardware_mux_rate() {
	let data = include_bytes!("test_data/scte35/kyrion_dirtystart.ts");
	let catalog = import_ts_ext(data);

	let rate = catalog.ext.mpegts.mux_rate.expect("a CBR source records its mux rate");
	assert!(
		rate.abs_diff(2_573_445) * 100 <= 2_573_445,
		"mux rate {rate} is not within 1% of what tsanalyze measured"
	);
}

/// ffmpeg without `-muxrate` writes no stuffing and its PCR intervals carry whatever
/// the frames weighed, so no rate is ever stable enough to record.
#[test]
fn import_leaves_a_vbr_mux_rate_absent() {
	let data = include_bytes!("test_data/scte35/bbb5s.ts");
	let catalog = import_ts_ext(data);
	assert_eq!(catalog.ext.mpegts.mux_rate, None, "a VBR source records no mux rate");
}

#[test]
fn import_kyrion_ac3_mp2_catalog() {
	let data = include_bytes!("test_data/kyrion_mpeg2av_ac3.ts");
	let catalog = import_ts(data);

	assert_eq!(catalog.video.renditions.len(), 0, "MPEG-2 video is clock-only");
	assert_eq!(catalog.audio.renditions.len(), 2, "expected AC-3 + MP2 tracks");
	for (name, audio) in &catalog.audio.renditions {
		assert!(
			matches!(audio.codec.to_string().as_str(), "ac-3" | "mp2"),
			"unexpected codec {} (track {name})",
			audio.codec
		);
		assert_eq!(audio.sample_rate, 48_000, "track {name}");
		assert_eq!(audio.channel_count, 2, "track {name}");
		assert!(audio.description.is_none(), "track {name}");
	}
	let codecs: std::collections::HashSet<String> =
		catalog.audio.renditions.values().map(|a| a.codec.to_string()).collect();
	assert_eq!(codecs.len(), 2, "one rendition per codec");
}

#[test]
fn import_resyncs_after_byte_misalignment() {
	let data = include_bytes!("test_data/bbb.ts");
	// Prepend stray bytes so the stream no longer starts on a packet boundary. A
	// byte-wise resync still finds the first sync byte and demuxes; a 188-stride
	// resync would never re-align and the catalog would come back empty.
	let mut misaligned = vec![0x00, 0x11, 0x22];
	misaligned.extend_from_slice(data);
	let catalog = import_ts(&misaligned);
	assert_eq!(catalog.video.renditions.len(), 1, "resync failed: no video track");
	assert_eq!(catalog.audio.renditions.len(), 1, "resync failed: no audio track");
}

#[test]
fn resyncs_past_false_sync_byte() {
	let data = include_bytes!("test_data/bbb.ts");
	// Lead with a non-sync byte so demux enters resync, then a stray 0x47 (payload-like)
	// whose byte 188 ahead is not a sync byte. The confirmation must reject that candidate
	// and scan on to the real stream rather than locking onto it and routing a bogus packet.
	let mut misaligned = vec![0x00, 0x47];
	misaligned.resize(202, 0x00);
	misaligned.extend_from_slice(data);
	let catalog = import_ts(&misaligned);
	assert_eq!(catalog.video.renditions.len(), 1, "false sync derailed demux: no video");
	assert_eq!(catalog.audio.renditions.len(), 1, "false sync derailed demux: no audio");
}

#[test]
fn resyncs_across_chunk_boundaries() {
	// Misaligned start fed in small chunks, so a resync candidate often lands at a buffer
	// tail and is carried, pending confirmation, into the next decode call. The sync lock
	// must re-confirm it there (with the trailing bytes) rather than trust it blindly.
	let data = include_bytes!("test_data/bbb.ts");
	let mut misaligned = vec![0x00, 0x11, 0x22];
	misaligned.extend_from_slice(data);

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.reserve());
	for chunk in misaligned.chunks(100) {
		import.decode(&BytesMut::from(chunk)).unwrap();
	}
	import.finish().unwrap();

	let snapshot = catalog.snapshot();
	assert_eq!(
		snapshot.video.renditions.len(),
		1,
		"chunked resync failed: no video track"
	);
	assert_eq!(
		snapshot.audio.renditions.len(),
		1,
		"chunked resync failed: no audio track"
	);
}

#[tokio::test(start_paused = true)]
async fn import_export_import_roundtrip() {
	let data = include_bytes!("test_data/bbb.ts");

	// Import the fixture into a broadcast.
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.reserve());
	let buf = BytesMut::from(&data[..]);
	import.decode(&buf).unwrap();
	import.finish().unwrap();

	// Re-export to TS. `import` and `catalog` stay alive so the exporter can
	// subscribe to the finished, retained tracks.
	let mut exporter = crate::container::ts::Export::new(crate::source::announced(&consumer))
		.await
		.unwrap()
		.with_delay(RECORDING_MAX_AGE)
		.with_replay();
	let mut out = BytesMut::new();
	while let Ok(res) = tokio::time::timeout(DRAIN, exporter.next()).await {
		match res.expect("exporter error") {
			Some(frame) => out.extend_from_slice(&frame.payload),
			None => break,
		}
	}

	assert!(!out.is_empty(), "exporter produced no TS");
	assert_eq!(out.len() % 188, 0, "exported TS not packet-aligned");

	// The re-exported TS must demux back into the same track layout.
	let roundtrip = import_ts(&out);
	assert_eq!(roundtrip.video.renditions.len(), 1, "round-trip lost the video track");
	assert_eq!(roundtrip.audio.renditions.len(), 1, "round-trip lost the audio track");
}

/// A live capture joins mid-stream, which stresses two demux assumptions at once:
/// PES arrive before the PAT/PMT that route them, and the first decodable access
/// unit is a delta, not a keyframe. The importer must survive both (drop packets
/// until the layout is learned, then drop deltas until the first keyframe anchors
/// a group) instead of aborting. The buffer is carved from `bbb.ts`: a video
/// packet ahead of any PSI, then the PAT+PMT, then a delta AU, then the IDR.
#[tokio::test(start_paused = true)]
async fn survives_midstream_join() {
	let data = include_bytes!("test_data/bbb.ts");
	let pkt = |i: usize| &data[i * 188..(i + 1) * 188];
	// bbb.ts layout: pkt1=PAT, pkt2=PMT, pkt5=delta AU, pkt8+9=IDR AU (SPS/PPS/IDR).
	let mut buf = Vec::new();
	buf.extend_from_slice(pkt(5)); // video PES before any PSI: the reader would hit "Unknown PID"
	buf.extend_from_slice(pkt(1)); // PAT: learn the PMT PID
	buf.extend_from_slice(pkt(2)); // PMT: register the video/audio ES PIDs
	buf.extend_from_slice(pkt(5)); // delta AU now routes, but has no keyframe to anchor a group
	buf.extend_from_slice(pkt(8)); // IDR AU: flushes the delta, then anchors the first group
	buf.extend_from_slice(pkt(9));

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.reserve());
	import
		.decode(&BytesMut::from(&buf[..]))
		.expect("a mid-stream join must not abort the demux");
	import.finish().unwrap();

	let snapshot = catalog.snapshot();
	assert_eq!(snapshot.video.renditions.len(), 1, "video track lost across the join");
	let name = snapshot.video.renditions.keys().next().unwrap().clone();

	// The track resumes at the keyframe: the leading delta was dropped, the IDR
	// anchors the one and only group.
	let track = consumer
		.track(&name)
		.unwrap()
		.subscribe(moq_net::track::Subscription::default().with_max_delay(RECORDING_MAX_AGE))
		.await
		.unwrap();
	let mut reader = crate::container::Consumer::new(
		track,
		crate::catalog::hang::Container::Legacy(crate::container::Kind::Data),
	);
	let mut frames = Vec::new();
	while let Ok(Ok(Some(frame))) = tokio::time::timeout(std::time::Duration::from_millis(50), reader.read()).await {
		frames.push(frame);
	}
	assert_eq!(frames.len(), 1, "expected only the post-join IDR, got {}", frames.len());
	assert!(frames[0].keyframe, "the first surviving frame must be the keyframe");
}

/// A real Ateme Kyrion broadcast captured mid-stream with `nc`, so it opens dirty:
/// the first packet is a video continuation (PUSI=0) and hundreds of media packets
/// arrive before the first PAT/PMT. The importer must survive the join (gate +
/// keyframe wait) AND extract the six SCTE-35 cues the encoder emitted. TSDuck
/// decodes all six as splice_inserts, CRC32 OK; that decode is checked in alongside
/// as `kyrion_dirtystart_tsduck.txt` (regen: `tsp -I file kyrion_dirtystart.ts
/// -P tables --pid 0x14d -O drop`).
#[tokio::test(start_paused = true)]
async fn kyrion_dirtystart_extracts_real_cues() {
	let data = include_bytes!("test_data/scte35/kyrion_dirtystart.ts");
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(
		&mut broadcast,
		crate::catalog::Config::default()
			.with_catalog(crate::catalog::hang::Catalog::<crate::container::ts::catalog::Ext>::default()),
	)
	.unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.reserve());
	import
		.decode(&BytesMut::from(&data[..]))
		.expect("a dirty mid-stream join must not abort the demux");
	import.finish().unwrap();

	let snap = catalog.snapshot();
	assert_eq!(snap.video.renditions.len(), 1, "video track lost across the dirty join");
	// Select the SCTE-35 stream by its verbatim stream_type; media tracks also appear
	// in mpegts.tracks now (with their PID + descriptors).
	let name = snap
		.ext
		.mpegts
		.tracks
		.iter()
		.find(|(_, t)| t.verbatim.as_ref().is_some_and(|v| v.stream_type == 0x86))
		.map(|(name, _)| name.clone())
		.expect("scte35 track");
	let track = consumer
		.track(&name)
		.unwrap()
		.subscribe(moq_net::track::Subscription::default().with_max_delay(RECORDING_MAX_AGE))
		.await
		.unwrap();
	let mut reader = crate::container::Consumer::new(
		track,
		crate::catalog::hang::Container::Legacy(crate::container::Kind::Data),
	);
	let mut cues = Vec::new();
	while let Ok(Ok(Some(frame))) = tokio::time::timeout(std::time::Duration::from_millis(50), reader.read()).await {
		cues.push((frame.payload.to_vec(), frame.timestamp));
	}
	assert_eq!(cues.len(), 6, "expected the six real splice_inserts");
	assert!(
		cues.iter().all(|(b, _)| b.first() == Some(&0xfc)),
		"every cue is a splice_info_section (table_id 0xFC)"
	);
	assert!(
		cues.iter().all(|(b, _)| b.get(13) == Some(&0x05)),
		"every cue is a splice_insert (command type 0x05)"
	);
	let distinct: std::collections::HashSet<&Vec<u8>> = cues.iter().map(|(b, _)| b).collect();
	assert_eq!(distinct.len(), 6, "six distinct cue sections");
	assert!(
		cues.iter().all(|(_, ts)| *ts != moq_net::Timestamp::ZERO),
		"cues stamped with the video PTS, not zero"
	);
}

#[test]
fn import_handles_unaligned_chunks() {
	// Feed the fixture in 100-byte chunks so most `decode` calls end mid-packet,
	// exercising the partial-packet retention across calls.
	let data = include_bytes!("test_data/bbb.ts");

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.reserve());

	for chunk in data.chunks(100) {
		let buf = BytesMut::from(chunk);
		import.decode(&buf).unwrap();
	}
	import.finish().unwrap();

	let snapshot = catalog.snapshot();
	assert_eq!(snapshot.video.renditions.len(), 1);
	assert_eq!(snapshot.audio.renditions.len(), 1);
}

/// `data` with every PES PTS/DTS and PCR base moved `ticks` later on the 90 kHz clock, as an
/// encoder that had been running that much longer would stamp it.
fn shift_clock(data: &[u8], ticks: u64) -> Vec<u8> {
	const FIELD: u64 = (1 << 33) - 1;
	let mut out = data.to_vec();
	for pkt in out.as_chunks_mut::<188>().0 {
		assert_eq!(pkt[0], 0x47, "an aligned TS packet");
		let mut payload = 4;
		if pkt[3] & 0x20 != 0 {
			let len = pkt[4] as usize;
			if len >= 7 && pkt[5] & 0x10 != 0 {
				let pcr = &mut pkt[6..11];
				let base = (pcr[0] as u64) << 25
					| (pcr[1] as u64) << 17
					| (pcr[2] as u64) << 9
					| (pcr[3] as u64) << 1
					| (pcr[4] as u64) >> 7;
				let base = (base + ticks) & FIELD;
				pcr[0] = (base >> 25) as u8;
				pcr[1] = (base >> 17) as u8;
				pcr[2] = (base >> 9) as u8;
				pcr[3] = (base >> 1) as u8;
				pcr[4] = (pcr[4] & 0x7F) | ((base as u8 & 1) << 7);
			}
			payload += 1 + len;
		}
		// Only a payload-unit start with a payload can open a PES header.
		if pkt[1] & 0x40 == 0 || pkt[3] & 0x10 == 0 || payload + 9 > 188 {
			continue;
		}
		let pes = &mut pkt[payload..];
		if pes[..3] != [0, 0, 1] {
			continue;
		}
		let flags = pes[7] >> 6;
		let mut at = 9;
		for present in [flags & 0b10 != 0, flags == 0b11] {
			if !present {
				continue;
			}
			let t = &mut pes[at..at + 5];
			let v = ((t[0] as u64 >> 1) & 0x07) << 30
				| (t[1] as u64) << 22
				| (t[2] as u64 >> 1) << 15
				| (t[3] as u64) << 7
				| t[4] as u64 >> 1;
			let v = (v + ticks) & FIELD;
			t[0] = (t[0] & 0xF1) | (((v >> 30) as u8 & 0x07) << 1);
			t[1] = (v >> 22) as u8;
			t[2] = ((((v >> 15) & 0x7F) as u8) << 1) | 1;
			t[3] = (v >> 7) as u8;
			t[4] = (((v & 0x7F) as u8) << 1) | 1;
			at += 5;
		}
	}
	out
}

/// What a TS import published, and the wall-clock window it arrived in.
struct Imported {
	published: std::collections::BTreeMap<String, Vec<u128>>,
	/// The root clock the catalog advertised.
	clock: hang::catalog::Clock,
	arrival: std::ops::RangeInclusive<std::time::SystemTime>,
}

/// Import `data` on a catalog with the default clock.
async fn import_stream(data: &[u8]) -> Imported {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.reserve());

	let before = std::time::SystemTime::now();
	import.decode(data).unwrap();
	let after = std::time::SystemTime::now();
	import.finish().unwrap();

	let snapshot = catalog.snapshot();
	Imported {
		published: crate::container::test_util::published(&consumer, &snapshot).await,
		clock: snapshot.clock.expect("the catalog advertises a clock"),
		arrival: before..=after,
	}
}

/// A feed ten hours into its own PTS publishes those timestamps verbatim, H.264 and AAC keeping
/// the offset their PES headers gave them, and the catalog clock maps it to the arrival time.
#[tokio::test]
async fn import_publishes_stream_pts_on_an_arrival_clock() {
	let data: &[u8] = include_bytes!("test_data/bbb_cbr.ts");
	let hours = std::time::Duration::from_secs(10 * 3600);
	let original = import_stream(data).await;
	let shifted = import_stream(&shift_clock(data, hours.as_secs() * 90_000)).await;

	let offset = crate::container::test_util::common_offset(&original.published, &shifted.published);
	assert!(
		(offset - hours.as_micros() as i128).abs() <= 1_000,
		"the source's own PTS: moved {offset}us"
	);

	// The first PES anchors at its arrival, which frames muxed ahead of it may precede.
	let earliest = shifted.published.values().map(|t| t[0]).min().unwrap();
	let wall = shifted
		.clock
		.wall_clock(moq_net::Timestamp::from_micros(earliest as u64).unwrap())
		.unwrap();
	let skew = std::time::Duration::from_secs(2);
	assert!(
		*shifted.arrival.start() - skew <= wall && wall <= *shifted.arrival.end(),
		"the stream is live on arrival"
	);
}

/// The PCR a packet's adaptation field carries, in 27 MHz ticks.
fn pcr(pkt: &[u8]) -> Option<u64> {
	if pkt[3] & 0x20 == 0 || pkt[4] < 7 || pkt[5] & 0x10 == 0 {
		return None;
	}
	let base = (u64::from(pkt[6]) << 25)
		| (u64::from(pkt[7]) << 17)
		| (u64::from(pkt[8]) << 9)
		| (u64::from(pkt[9]) << 1)
		| (u64::from(pkt[10]) >> 7);
	Some(base * 300 + ((u64::from(pkt[10] & 0x01) << 8) | u64::from(pkt[11])))
}

fn packet_pid(pkt: &[u8]) -> u16 {
	(u16::from(pkt[1] & 0x1f) << 8) | u16::from(pkt[2])
}

/// Each packet of `ts` with the program clock it arrives at, counted from the first PCR.
fn timed(ts: &[u8]) -> impl Iterator<Item = (std::time::Duration, &[u8; 188])> {
	let mut first = None;
	let mut now = std::time::Duration::ZERO;
	ts.as_chunks::<188>().0.iter().map(move |pkt| {
		if let Some(pcr) = pcr(pkt) {
			let first = *first.get_or_insert(pcr);
			now = std::time::Duration::from_nanos((pcr - first) * 1_000 / 27);
		}
		(now, pkt)
	})
}

/// `ts` with the PES on `pid` suppressed from its first PES start at or after `from` to its
/// first at or after `to`, as an encoder whose one input died behind a running mux emits it.
///
/// A suppressed packet that carried the PCR keeps it in an adaptation-only packet, every
/// other one becomes null stuffing so the mux rate holds, and the counters after the gap are
/// renumbered so continuity stays legal.
fn suppress(ts: &[u8], pid: u16, from: std::time::Duration, to: std::time::Duration) -> Vec<u8> {
	let mut null = [0xff; 188];
	null[..4].copy_from_slice(&[0x47, 0x1f, 0xff, 0x10]);

	let mut out = Vec::with_capacity(ts.len());
	let (mut active, mut done) = (false, false);
	let (mut last_cc, mut dropped) = (0, 0u8);
	for (now, pkt) in timed(ts) {
		let mut pkt = *pkt;
		if packet_pid(&pkt) == pid {
			if pkt[1] & 0x40 != 0 {
				if !active && !done && now >= from {
					active = true;
				} else if active && now >= to {
					(active, done) = (false, true);
				}
			}
			let payload = pkt[3] & 0x10 != 0;
			if active {
				dropped = (dropped + u8::from(payload)) & 0x0f;
				pkt = match pcr(&pkt) {
					Some(_) => {
						let mut clock = [0xff; 188];
						clock[..6].copy_from_slice(&[0x47, pkt[1] & 0x1f, pkt[2], 0x20 | last_cc, 183, 0x10]);
						clock[6..12].copy_from_slice(&pkt[6..12]);
						clock
					}
					None => null,
				};
			} else {
				pkt[3] = (pkt[3] & 0xf0) | (pkt[3].wrapping_sub(dropped) & 0x0f);
				if payload {
					last_cc = pkt[3] & 0x0f;
				}
			}
		}
		out.extend_from_slice(&pkt);
	}
	assert!(done, "the fixture must resume the PID before it ends");
	out
}

/// What #3489 measured before MoQ saw the stimulus: the same packets and PCRs as `ts`, and
/// not one continuity error the fixture didn't already have.
fn assert_legal(ts: &[u8], stimulus: &[u8]) {
	assert_eq!(ts.len(), stimulus.len(), "the mux rate moved");
	let clocks = |ts: &[u8]| {
		ts.as_chunks::<188>()
			.0
			.iter()
			.filter_map(|pkt| pcr(pkt))
			.collect::<Vec<_>>()
	};
	assert_eq!(clocks(ts), clocks(stimulus), "a PCR went missing");
	let errors = |ts: &[u8]| {
		let mut last = std::collections::HashMap::new();
		let mut errors = std::collections::BTreeMap::<u16, usize>::new();
		for pkt in ts.as_chunks::<188>().0 {
			let pid = packet_pid(pkt);
			if pid == 0x1fff || pkt[3] & 0x10 == 0 {
				continue;
			}
			let cc = pkt[3] & 0x0f;
			if last.insert(pid, cc).is_some_and(|previous| cc != (previous + 1) & 0x0f) {
				*errors.entry(pid).or_default() += 1;
			}
		}
		errors
	};
	assert_eq!(errors(ts), errors(stimulus), "the stimulus broke continuity");
}

/// Import `ts` with an `mpegts` catalog, snapshotting the stats once the program clock
/// reaches each of `at`, and once more at the end of the input.
fn sample(ts: &[u8], at: &[std::time::Duration]) -> Vec<crate::container::ts::stats::Snapshot> {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let _catalog = crate::catalog::Producer::new(
		&mut broadcast,
		crate::catalog::Config::default()
			.with_catalog(crate::catalog::hang::Catalog::<crate::container::ts::Ext>::default()),
	)
	.unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, _catalog.reserve());

	let mut samples = Vec::new();
	let mut done = 0;
	for &at in at {
		let end = 188
			* timed(ts)
				.position(|(now, _)| now >= at)
				.expect("the fixture reaches it");
		import.decode(&ts[done..end]).unwrap();
		samples.push(import.stats());
		done = end;
	}
	import.decode(&ts[done..]).unwrap();
	samples.push(import.stats());
	samples
}

/// #3489's stimulus: `pid` goes quiet between `from` and `to` while its PCR and continuity stay
/// legal. Its row stops counting and its silence grows with the program clock, where the
/// unmodified fixture keeps counting; `peer` keeps delivering throughout; and the row counts
/// again once the PID returns.
fn assert_stalls(fixture: &[u8], pid: u16, track: &str, peer: u16, from: f64, to: f64) {
	use std::time::Duration;

	let stimulus = suppress(fixture, pid, Duration::from_secs_f64(from), Duration::from_secs_f64(to));
	assert_legal(fixture, &stimulus);

	let at = [Duration::from_secs_f64(from + 0.4), Duration::from_secs_f64(to - 0.2)];
	let control = sample(fixture, &at);
	assert!(
		control[1].streams[&pid].units > control[0].streams[&pid].units,
		"the unmodified fixture delivers on {pid:#x} across the window"
	);

	let samples = sample(&stimulus, &at);
	let [early, late, end] = [&samples[0], &samples[1], &samples[2]].map(|s| s.streams[&pid].clone());
	assert_eq!(early.track, track);
	assert!(early.units > 0, "{pid:#x} delivered before it went quiet");
	assert_eq!(late.units, early.units, "{pid:#x} stopped counting");

	let (early_quiet, late_quiet) = (early.quiet.unwrap(), late.quiet.unwrap());
	assert!(
		early_quiet >= Duration::from_millis(350),
		"{pid:#x} has been quiet since the window opened: {early_quiet:?}"
	);
	let grew = late_quiet - early_quiet;
	let elapsed = at[1] - at[0];
	assert!(
		grew.abs_diff(elapsed) <= Duration::from_millis(50),
		"the silence grows with the program clock: {grew:?} over {elapsed:?}"
	);

	let (peer_early, peer_late) = (&samples[0].streams[&peer], &samples[1].streams[&peer]);
	assert!(peer_late.units > peer_early.units, "{peer:#x} kept delivering");
	assert!(
		peer_late.quiet.unwrap() < Duration::from_millis(200),
		"{peer:#x} is not quiet: {:?}",
		peer_late.quiet
	);

	assert!(end.units > late.units, "{pid:#x} counts again once it returns");
	assert!(end.quiet.unwrap() < late_quiet, "{pid:#x} is no longer as quiet");
}

/// Video carrying the program's PCR, the layout #3489 measured: the suppressed PID still
/// drives the clock its own silence is measured on.
#[test]
fn import_reports_a_silent_video_pid() {
	let data = include_bytes!("test_data/scte35/bbb5s.ts");
	assert_stalls(data, 0x100, ".hev1", 0x101, 1.5, 3.5);
}

/// One of two MP2 programs goes quiet behind a dedicated PCR PID.
#[test]
fn import_reports_a_silent_audio_pid() {
	let data = include_bytes!("test_data/scte35/kyrion_dirtystart.ts");
	assert_stalls(data, 0x101, ".mp2", 0x102, 1.5, 3.5);
}

/// SCTE-35 is sparse, so its row reports the silence and leaves the verdict to the caller.
#[test]
fn import_reports_a_silent_scte35_pid() {
	let data = include_bytes!("test_data/scte35/kyrion_dirtystart.ts");
	assert_stalls(data, 0x14d, ".ts", 0x100, 2.5, 3.5);
}

/// Every elementary stream the importer carries reports a row, decoded, clock-only, and
/// verbatim alike, and every row has delivered.
#[test]
fn import_reports_every_elementary_stream() {
	for (data, rows) in [
		(
			&include_bytes!("test_data/kyrion_mpeg2av_ac3.ts")[..],
			&[(0x100, ""), (0x101, ".ac3"), (0x102, ".mp2"), (0x14d, ".ts")][..],
		),
		(
			&include_bytes!("test_data/scte35/bbb5s.ts")[..],
			&[(0x21, ".ts"), (0x100, ".hev1"), (0x101, ".opus")][..],
		),
	] {
		let stats = sample(data, &[]).pop().unwrap();
		let tracks: Vec<(u16, &str)> = stats.streams.iter().map(|(&pid, s)| (pid, s.track.as_str())).collect();
		assert_eq!(tracks, rows);
		for (pid, stream) in &stats.streams {
			assert!(stream.units > 0, "{pid:#x} delivered nothing: {stream:?}");
			assert!(stream.quiet.is_some(), "{pid:#x} has no program clock: {stream:?}");
		}
	}
}
