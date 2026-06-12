//! Tests for the MPEG-TS importer.
//!
//! `bbb.ts` is `fmp4/test_data/bbb.mp4` remuxed to MPEG-TS with `ffmpeg -c copy`
//! (H.264 + AAC), so it exercises the real demux -> codec path.

use bytes::BytesMut;

/// Decode a whole TS buffer into a fresh broadcast and return the catalog.
fn import_ts(data: &[u8]) -> crate::catalog::hang::Catalog {
	let mut broadcast = moq_net::Broadcast::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();

	let mut import = crate::container::ts::Import::new(broadcast, catalog.clone());
	let mut buf = BytesMut::from(data);
	import.decode(&mut buf).unwrap();
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

	let mut broadcast = moq_net::Broadcast::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.clone());
	for chunk in misaligned.chunks(100) {
		import.decode(&mut BytesMut::from(chunk)).unwrap();
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
	let mut broadcast = moq_net::Broadcast::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.clone());
	let mut buf = BytesMut::from(&data[..]);
	import.decode(&mut buf).unwrap();
	import.finish().unwrap();

	// Re-export to TS. `import` and `catalog` stay alive so the exporter can
	// subscribe to the finished, retained tracks.
	let mut exporter = crate::container::ts::Export::new(consumer).unwrap();
	let mut out = BytesMut::new();
	while let Ok(res) = tokio::time::timeout(std::time::Duration::from_secs(1), exporter.next()).await {
		match res.expect("exporter error") {
			Some(chunk) => out.extend_from_slice(&chunk),
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

	let mut broadcast = moq_net::Broadcast::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.clone());
	import
		.decode(&mut BytesMut::from(&buf[..]))
		.expect("a mid-stream join must not abort the demux");
	import.finish().unwrap();

	let snapshot = catalog.snapshot();
	assert_eq!(snapshot.video.renditions.len(), 1, "video track lost across the join");
	let name = snapshot.video.renditions.keys().next().unwrap().clone();

	// The track resumes at the keyframe: the leading delta was dropped, the IDR
	// anchors the one and only group.
	let track = consumer.subscribe_track(&moq_net::Track::new(name)).unwrap();
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
	let mut broadcast = moq_net::Broadcast::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::with_catalog(
		&mut broadcast,
		crate::catalog::hang::Catalog::<crate::container::ts::scte35::Ext>::default(),
	)
	.unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.clone());
	import
		.decode(&mut BytesMut::from(&data[..]))
		.expect("a dirty mid-stream join must not abort the demux");
	import.finish().unwrap();

	let snap = catalog.snapshot();
	assert_eq!(snap.video.renditions.len(), 1, "video track lost across the dirty join");
	let name = snap.scte35.renditions.keys().next().expect("scte35 track").clone();
	let track = consumer.subscribe_track(&moq_net::Track::new(name)).unwrap();
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
		cues.iter().all(|(_, ts)| *ts != crate::container::Timestamp::ZERO),
		"cues stamped with the video PTS, not zero"
	);
}

#[test]
fn import_handles_unaligned_chunks() {
	// Feed the fixture in 100-byte chunks so most `decode` calls end mid-packet,
	// exercising the partial-packet retention across calls.
	let data = include_bytes!("test_data/bbb.ts");

	let mut broadcast = moq_net::Broadcast::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
	let mut import = crate::container::ts::Import::new(broadcast, catalog.clone());

	for chunk in data.chunks(100) {
		let mut buf = BytesMut::from(chunk);
		import.decode(&mut buf).unwrap();
	}
	import.finish().unwrap();

	let snapshot = catalog.snapshot();
	assert_eq!(snapshot.video.renditions.len(), 1);
	assert_eq!(snapshot.audio.renditions.len(), 1);
}
