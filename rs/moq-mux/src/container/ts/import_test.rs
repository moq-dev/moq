//! Tests for the MPEG-TS importer.
//!
//! `bbb.ts` is `fmp4/test_data/bbb.mp4` remuxed to MPEG-TS with `ffmpeg -c copy`
//! (H.264 + AAC), so it exercises the real demux -> codec path.

use bytes::BytesMut;

/// Decode a whole TS buffer into a fresh broadcast and return the catalog.
fn import_ts(data: &[u8]) -> hang::Catalog {
	let mut broadcast = moq_net::Broadcast::new().produce();
	let catalog = crate::catalog::hang::Producer::new(&mut broadcast).unwrap();

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

#[tokio::test(start_paused = true)]
async fn import_export_import_roundtrip() {
	let data = include_bytes!("test_data/bbb.ts");

	// Import the fixture into a broadcast.
	let mut broadcast = moq_net::Broadcast::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::hang::Producer::new(&mut broadcast).unwrap();
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

#[test]
fn import_handles_unaligned_chunks() {
	// Feed the fixture in 100-byte chunks so most `decode` calls end mid-packet,
	// exercising the partial-packet retention across calls.
	let data = include_bytes!("test_data/bbb.ts");

	let mut broadcast = moq_net::Broadcast::new().produce();
	let catalog = crate::catalog::hang::Producer::new(&mut broadcast).unwrap();
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
