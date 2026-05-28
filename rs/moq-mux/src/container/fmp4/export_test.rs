//! Tests for the fMP4 exporter.

use std::io::Cursor;

use bytes::BytesMut;
use mp4_atom::{DecodeMaybe, Encode};

/// Avc3-shape source (catalog `Container::Legacy`, `H264 { inline: true }`,
/// `description: None`) → fMP4 / CMAF export must synthesize a valid init
/// segment from the codec config the Avc1 transform builds on the wire.
///
/// Verifies:
/// - Exporter doesn't bail on a Legacy source (the historical behavior).
/// - Init segment is deferred until SPS+PPS arrive.
/// - The synthesized init segment parses back and carries an avc1 sample
///   entry whose avcC is built from the inline SPS+PPS.
#[tokio::test(start_paused = true)]
async fn avc3_source_to_cmaf_export_roundtrip() {
	use hang::catalog::{Container, H264, VideoConfig};
	use moq_net::Timestamp;

	let broadcast = moq_net::Broadcast::new();
	let mut producer = broadcast.produce();
	let consumer = producer.consume();

	let mut catalog = crate::catalog::hang::Producer::new(&mut producer).unwrap();
	let track = producer.unique_track(".avc3").unwrap();
	let mut config = VideoConfig::new(H264 {
		profile: 0x42,
		constraints: 0xc0,
		level: 0x1f,
		inline: true,
	});
	config.coded_width = Some(320);
	config.coded_height = Some(240);
	config.container = Container::Legacy;
	catalog.lock().video.renditions.insert(track.name.clone(), config);

	const SC: &[u8] = &[0, 0, 0, 1];
	let sps = &[0x67u8, 0x42, 0xc0, 0x1f, 0xde, 0xad, 0xbe, 0xef][..];
	let pps = &[0x68u8, 0xce, 0x3c, 0x80][..];
	let idr = &[0x65u8, 0x88, 0x84, 0x21, 0x00, 0x11, 0x22, 0x33][..];

	let mut keyframe_payload = BytesMut::new();
	for nal in [sps, pps, idr] {
		keyframe_payload.extend_from_slice(SC);
		keyframe_payload.extend_from_slice(nal);
	}
	let keyframe_payload = keyframe_payload.freeze();

	let mut track_producer = crate::container::Producer::new(track, crate::catalog::hang::Container::Legacy);
	track_producer
		.write(crate::container::Frame {
			timestamp: Timestamp::from_micros(0).unwrap(),
			payload: keyframe_payload,
			keyframe: true,
		})
		.unwrap();
	track_producer.finish().unwrap();

	let catalog_stream =
		crate::catalog::Consumer::new(&consumer, crate::catalog::CatalogFormat::Hang).expect("catalog consumer");
	let mut exporter = crate::container::fmp4::Export::new(consumer, catalog_stream);

	let init = tokio::time::timeout(std::time::Duration::from_secs(1), exporter.next())
		.await
		.expect("exporter timed out")
		.expect("exporter result")
		.expect("expected init bytes");

	drop(track_producer);
	drop(catalog);
	drop(producer);

	let mut cursor = Cursor::new(init.as_ref());
	let mut saw_ftyp = false;
	let mut moov: Option<mp4_atom::Moov> = None;
	while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor).expect("decode init") {
		match atom {
			mp4_atom::Any::Ftyp(_) => saw_ftyp = true,
			mp4_atom::Any::Moov(m) => moov = Some(m),
			_ => {}
		}
	}
	assert!(saw_ftyp, "init segment missing ftyp");
	let moov = moov.expect("init segment missing moov");
	assert_eq!(moov.trak.len(), 1, "expected single track in moov");

	let trak = &moov.trak[0];
	let stsd = &trak.mdia.minf.stbl.stsd;
	assert_eq!(stsd.codecs.len(), 1, "expected single sample entry");
	let avc1 = match &stsd.codecs[0] {
		mp4_atom::Codec::Avc1(avc1) => avc1,
		other => panic!("expected Avc1 sample entry, got {:?}", other),
	};
	assert_eq!(avc1.avcc.avc_profile_indication, sps[1]);
	assert_eq!(avc1.avcc.avc_level_indication, sps[3]);
	assert_eq!(avc1.avcc.sequence_parameter_sets.len(), 1);
	assert_eq!(avc1.avcc.sequence_parameter_sets[0].as_slice(), sps);
	assert_eq!(avc1.avcc.picture_parameter_sets[0].as_slice(), pps);
	assert_eq!(avc1.visual.width, 320);
	assert_eq!(avc1.visual.height, 240);

	let mvex = moov.mvex.as_ref().expect("init segment missing mvex");
	assert_eq!(mvex.trex.len(), 1);
	assert_eq!(mvex.trex[0].track_id, trak.tkhd.track_id);
}

/// CMAF source (catalog `Container::Cmaf`) → fMP4 export should keep using
/// the passthrough init path: existing init bytes are merged into the moov.
///
/// Regression check that adding the Avc3 path didn't break the existing one.
#[tokio::test(start_paused = true)]
async fn cmaf_source_to_cmaf_export_passthrough() {
	let data = include_bytes!("test_data/bbb.mp4");

	let broadcast = moq_net::Broadcast::new();
	let mut producer = broadcast.produce();
	let consumer = producer.consume();

	let catalog = crate::catalog::hang::Producer::new(&mut producer).unwrap();
	let mut importer = crate::container::fmp4::Import::new(producer, catalog);
	let mut buf = BytesMut::from(data.as_slice());
	let _ = importer.decode(&mut buf);

	let catalog_stream =
		crate::catalog::Consumer::new(&consumer, crate::catalog::CatalogFormat::Hang).expect("catalog consumer");
	let mut exporter = crate::container::fmp4::Export::new(consumer, catalog_stream);

	let init = tokio::time::timeout(std::time::Duration::from_secs(1), exporter.next())
		.await
		.expect("exporter timed out")
		.expect("exporter result")
		.expect("expected init bytes");

	drop(importer);

	let mut cursor = Cursor::new(init.as_ref());
	let mut moov: Option<mp4_atom::Moov> = None;
	let mut saw_ftyp = false;
	while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor).expect("decode init") {
		match atom {
			mp4_atom::Any::Ftyp(_) => saw_ftyp = true,
			mp4_atom::Any::Moov(m) => moov = Some(m),
			_ => {}
		}
	}
	assert!(saw_ftyp);
	let moov = moov.expect("moov");
	// bbb.mp4 has one video + one audio track.
	assert_eq!(moov.trak.len(), 2, "expected two tracks (one video, one audio)");
	let mvex = moov.mvex.as_ref().expect("mvex");
	assert_eq!(mvex.trex.len(), 2);

	// Sanity check: the merged moov must round-trip cleanly through encode.
	let mut buf = Vec::new();
	moov.encode(&mut buf).expect("encode merged moov");
}
