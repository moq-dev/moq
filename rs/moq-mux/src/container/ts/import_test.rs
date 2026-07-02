//! Tests for the MPEG-TS importer.
//!
//! `bbb.ts` is `fmp4/test_data/bbb.mp4` remuxed to MPEG-TS with `ffmpeg -c copy`
//! (H.264 + AAC), so it exercises the real demux -> codec path.

use bytes::BytesMut;

/// Decode a whole TS buffer into a fresh broadcast and return the catalog.
fn import_ts(data: &[u8]) -> crate::catalog::hang::Catalog {
	let mut broadcast = moq_net::BroadcastInfo::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();

	let mut import = crate::container::ts::Import::new(broadcast, catalog.clone());
	let buf = BytesMut::from(data);
	import.decode(&buf).unwrap();
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

/// Opus frames from real ffmpeg output must decode: a non-empty run of Opus packets,
/// each a plausible size (the control header was stripped, not left in the payload).
#[tokio::test(start_paused = true)]
async fn import_opus_frames() {
	let data = include_bytes!("test_data/opus.ts");

	let mut broadcast = moq_net::BroadcastInfo::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.clone());
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

	let track = consumer.track(&name).unwrap().subscribe(None).await.unwrap();
	let mut reader = crate::container::Consumer::new(track, crate::catalog::hang::Container::Legacy);
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

	let mut broadcast = moq_net::BroadcastInfo::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.clone());
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
	let mut broadcast = moq_net::BroadcastInfo::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.clone());
	let buf = BytesMut::from(&data[..]);
	import.decode(&buf).unwrap();
	import.finish().unwrap();

	// Re-export to TS. `import` and `catalog` stay alive so the exporter can
	// subscribe to the finished, retained tracks.
	let mut exporter = crate::container::ts::Export::new(consumer).await.unwrap();
	let mut out = BytesMut::new();
	while let Ok(res) = tokio::time::timeout(std::time::Duration::from_secs(1), exporter.next()).await {
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

	let mut broadcast = moq_net::BroadcastInfo::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.clone());
	import
		.decode(&BytesMut::from(&buf[..]))
		.expect("a mid-stream join must not abort the demux");
	import.finish().unwrap();

	let snapshot = catalog.snapshot();
	assert_eq!(snapshot.video.renditions.len(), 1, "video track lost across the join");
	let name = snapshot.video.renditions.keys().next().unwrap().clone();

	// The track resumes at the keyframe: the leading delta was dropped, the IDR
	// anchors the one and only group.
	let track = consumer.track(&name).unwrap().subscribe(None).await.unwrap();
	let mut reader = crate::container::Consumer::new(track, crate::catalog::hang::Container::Legacy);
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
	let mut broadcast = moq_net::BroadcastInfo::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::with_catalog(
		&mut broadcast,
		crate::catalog::hang::Catalog::<crate::container::ts::catalog::Ext>::default(),
	)
	.unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.clone());
	import
		.decode(&BytesMut::from(&data[..]))
		.expect("a dirty mid-stream join must not abort the demux");
	import.finish().unwrap();

	let snap = catalog.snapshot();
	assert_eq!(snap.video.renditions.len(), 1, "video track lost across the dirty join");
	// Select the SCTE-35 stream by its verbatim stream_type; media tracks also appear
	// in mpegts.tracks now (with their PID + descriptors).
	let name = snap
		.mpegts
		.tracks
		.iter()
		.find(|(_, t)| t.verbatim.as_ref().is_some_and(|v| v.stream_type == 0x86))
		.map(|(name, _)| name.clone())
		.expect("scte35 track");
	let track = consumer.track(&name).unwrap().subscribe(None).await.unwrap();
	let mut reader = crate::container::Consumer::new(track, crate::catalog::hang::Container::Legacy);
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

	let mut broadcast = moq_net::BroadcastInfo::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.clone());

	for chunk in data.chunks(100) {
		let buf = BytesMut::from(chunk);
		import.decode(&buf).unwrap();
	}
	import.finish().unwrap();

	let snapshot = catalog.snapshot();
	assert_eq!(snapshot.video.renditions.len(), 1);
	assert_eq!(snapshot.audio.renditions.len(), 1);
}
