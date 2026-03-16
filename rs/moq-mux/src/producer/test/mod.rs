use base64::Engine;
use hang::catalog::Container;
use mp4_atom::{Decode, Encode};

fn run_fmp4(data: &[u8]) -> hang::Catalog {
	let mut broadcast = moq_lite::Broadcast::new().produce();
	let catalog = crate::CatalogProducer::new(&mut broadcast).unwrap();

	let mut fmp4 = super::Fmp4::new(broadcast, catalog.clone());

	let mut buf = bytes::BytesMut::from(data);
	// Ignore errors from incomplete/malformed trailing fragments in test files.
	let _ = fmp4.decode(&mut buf);

	catalog.snapshot()
}

fn decode_init_data(init_data: &str) -> (mp4_atom::Ftyp, mp4_atom::Moov) {
	let bytes = base64::engine::general_purpose::STANDARD
		.decode(init_data)
		.expect("invalid base64");
	let mut cursor = std::io::Cursor::new(&bytes);
	let ftyp = mp4_atom::Ftyp::decode(&mut cursor).expect("invalid ftyp");
	let moov = mp4_atom::Moov::decode(&mut cursor).expect("invalid moov");
	(ftyp, moov)
}

#[test]
fn test_bbb_catalog() {
	let data = include_bytes!("bbb.mp4");
	let catalog = run_fmp4(data);

	assert_eq!(catalog.video.renditions.len(), 1);
	assert_eq!(catalog.audio.renditions.len(), 1);

	let video = catalog.video.renditions.values().next().unwrap();
	assert_eq!(video.codec.to_string(), "avc1.64001f");
	assert_eq!(video.coded_width, Some(1280));
	assert_eq!(video.coded_height, Some(720));
	assert!(matches!(video.container, Container::Cmaf { .. }));

	let audio = catalog.audio.renditions.values().next().unwrap();
	assert_eq!(audio.codec.to_string(), "mp4a.40.2");
	assert_eq!(audio.sample_rate, 44100);
	assert_eq!(audio.channel_count, 2);
	assert!(matches!(audio.container, Container::Cmaf { .. }));
}

#[test]
fn test_bbb_init_data_roundtrip() {
	let data = include_bytes!("bbb.mp4");
	let catalog = run_fmp4(data);

	// Check video init data
	let video = catalog.video.renditions.values().next().unwrap();
	let Container::Cmaf { init_data } = &video.container else {
		panic!("expected Cmaf container");
	};
	let (ftyp, moov) = decode_init_data(init_data);
	assert_eq!(ftyp.major_brand, mp4_atom::FourCC::new(b"isom"));
	assert_eq!(moov.trak.len(), 1);
	assert_eq!(moov.trak[0].tkhd.track_id, 1);
	assert_eq!(moov.trak[0].mdia.mdhd.timescale, 24000);
	let mvex = moov.mvex.as_ref().unwrap();
	assert_eq!(mvex.trex.len(), 1);
	assert_eq!(mvex.trex[0].track_id, 1);

	// Verify it round-trips through encode/decode
	let mut buf = Vec::new();
	ftyp.encode(&mut buf).unwrap();
	moov.encode(&mut buf).unwrap();
	let (ftyp2, moov2) = decode_init_data(&base64::engine::general_purpose::STANDARD.encode(&buf));
	assert_eq!(ftyp2.major_brand, mp4_atom::FourCC::new(b"isom"));
	assert_eq!(moov2.trak.len(), 1);

	// Check audio init data
	let audio = catalog.audio.renditions.values().next().unwrap();
	let Container::Cmaf { init_data } = &audio.container else {
		panic!("expected Cmaf container");
	};
	let (ftyp, moov) = decode_init_data(init_data);
	assert_eq!(ftyp.major_brand, mp4_atom::FourCC::new(b"isom"));
	assert_eq!(moov.trak.len(), 1);
	assert_eq!(moov.trak[0].tkhd.track_id, 2);
	assert_eq!(moov.trak[0].mdia.mdhd.timescale, 44100);
	let mvex = moov.mvex.as_ref().unwrap();
	assert_eq!(mvex.trex.len(), 1);
	assert_eq!(mvex.trex[0].track_id, 2);
}

#[test]
fn test_av1_catalog() {
	let data = include_bytes!("av1.mp4");
	let catalog = run_fmp4(data);

	assert_eq!(catalog.video.renditions.len(), 1);
	assert_eq!(catalog.audio.renditions.len(), 0);

	let video = catalog.video.renditions.values().next().unwrap();
	assert!(video.codec.to_string().starts_with("av01."), "codec: {}", video.codec);
	assert!(matches!(video.container, Container::Cmaf { .. }));

	let Container::Cmaf { init_data } = &video.container else {
		panic!("expected Cmaf container");
	};
	let (ftyp, moov) = decode_init_data(init_data);
	assert_eq!(ftyp.major_brand, mp4_atom::FourCC::new(b"isom"));
	assert_eq!(moov.trak.len(), 1);
	let mvex = moov.mvex.as_ref().unwrap();
	assert_eq!(mvex.trex.len(), 1);
	assert_eq!(mvex.trex[0].track_id, moov.trak[0].tkhd.track_id);
}

#[test]
fn test_vp9_catalog() {
	let data = include_bytes!("vp9.mp4");
	let catalog = run_fmp4(data);

	assert_eq!(catalog.video.renditions.len(), 1);
	assert_eq!(catalog.audio.renditions.len(), 0);

	let video = catalog.video.renditions.values().next().unwrap();
	assert!(video.codec.to_string().starts_with("vp09."), "codec: {}", video.codec);
	assert!(matches!(video.container, Container::Cmaf { .. }));

	let Container::Cmaf { init_data } = &video.container else {
		panic!("expected Cmaf container");
	};
	let (ftyp, moov) = decode_init_data(init_data);
	assert_eq!(ftyp.major_brand, mp4_atom::FourCC::new(b"isom"));
	assert_eq!(moov.trak.len(), 1);
	let mvex = moov.mvex.as_ref().unwrap();
	assert_eq!(mvex.trex.len(), 1);
	assert_eq!(mvex.trex[0].track_id, moov.trak[0].tkhd.track_id);
}
