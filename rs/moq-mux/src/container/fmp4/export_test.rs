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

	let broadcast = moq_net::BroadcastInfo::new();
	let mut producer = broadcast.produce();
	let consumer = producer.consume();

	let mut catalog = crate::catalog::Producer::new(&mut producer).unwrap();
	let track = producer
		.create_track(
			producer.unique_name(".avc3"),
			moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
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
			duration: None,
		})
		.unwrap();
	track_producer.finish().unwrap();

	let catalog_stream = crate::catalog::Consumer::<()>::new(&consumer, crate::catalog::CatalogFormat::Hang)
		.await
		.expect("catalog consumer");
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

/// Legacy AAC source (catalog `Container::Legacy`, codec `mp4a.40.2`, with a
/// `description` carrying the AudioSpecificConfig — the shape an MPEG-TS import
/// produces) → fMP4 export must synthesize an mp4a sample entry whose esds
/// carries that AudioSpecificConfig, instead of bailing with UnsupportedSynthesis.
#[tokio::test(start_paused = true)]
async fn legacy_aac_source_to_cmaf_export_synthesizes_esds() {
	use bytes::Bytes;
	use hang::catalog::{AAC, AudioConfig, Container};
	use moq_net::Timestamp;

	let broadcast = moq_net::BroadcastInfo::new();
	let mut producer = broadcast.produce();
	let consumer = producer.consume();

	let mut catalog = crate::catalog::Producer::new(&mut producer).unwrap();
	let track = producer
		.create_track(
			producer.unique_name(".aac"),
			moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
		)
		.unwrap();

	// AAC-LC (profile 2), 44100 Hz, stereo. The TS importer sets `description`
	// via aac::Config::encode; mirror that here.
	let description = crate::codec::aac::Config {
		profile: 2,
		sample_rate: 44100,
		channel_count: 2,
	}
	.encode();
	let mut config = AudioConfig::new(AAC { profile: 2 }, 44100, 2);
	config.description = Some(description.clone());
	config.container = Container::Legacy;
	catalog.lock().audio.renditions.insert(track.name().to_string(), config);

	let mut track_producer = crate::container::Producer::new(track, crate::catalog::hang::Container::Legacy);
	track_producer
		.write(crate::container::Frame {
			timestamp: Timestamp::from_micros(0).unwrap(),
			duration: None,
			payload: Bytes::from_static(&[0x01, 0x02, 0x03, 0x04]),
			keyframe: true,
		})
		.unwrap();
	track_producer.finish().unwrap();

	let catalog_stream = crate::catalog::Consumer::<()>::new(&consumer, crate::catalog::CatalogFormat::Hang)
		.await
		.unwrap();
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
	let mut moov: Option<mp4_atom::Moov> = None;
	while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor).expect("decode init") {
		if let mp4_atom::Any::Moov(m) = atom {
			moov = Some(m);
		}
	}
	let moov = moov.expect("init segment missing moov");
	assert_eq!(moov.trak.len(), 1, "expected single track in moov");

	let trak = &moov.trak[0];
	let stsd = &trak.mdia.minf.stbl.stsd;
	assert_eq!(stsd.codecs.len(), 1, "expected single sample entry");
	let mp4a = match &stsd.codecs[0] {
		mp4_atom::Codec::Mp4a(mp4a) => mp4a,
		other => panic!("expected Mp4a sample entry, got {:?}", other),
	};

	assert_eq!(mp4a.audio.channel_count, 2);
	assert_eq!(mp4a.audio.sample_rate.integer(), 44100);

	let dec_config = &mp4a.esds.es_desc.dec_config;
	assert_eq!(dec_config.object_type_indication, 0x40, "MPEG-4 AAC");
	assert_eq!(dec_config.stream_type, 0x05, "audio stream");

	let dec_specific = &dec_config.dec_specific;
	assert_eq!(dec_specific.profile, 2, "AAC-LC");
	assert_eq!(dec_specific.freq_index, 4, "44100 Hz");
	assert_eq!(dec_specific.chan_conf, 2, "stereo");

	// The synthesized init must round-trip through encode (esds included).
	let mut buf = Vec::new();
	moov.encode(&mut buf).expect("encode synthesized moov");
}

/// VP8 source (catalog `Container::Legacy`, codec `vp8`, no `description`) →
/// fMP4 export must synthesize a `vp08` sample entry. VP8 carries no out-of-band
/// config, so this exercises the description-less synthesis path.
#[tokio::test(start_paused = true)]
async fn vp8_source_to_cmaf_export_synthesizes_vp08() {
	use bytes::Bytes;
	use hang::catalog::{Container, VideoCodec, VideoConfig};
	use moq_net::Timestamp;

	let broadcast = moq_net::BroadcastInfo::new();
	let mut producer = broadcast.produce();
	let consumer = producer.consume();

	let mut catalog = crate::catalog::Producer::new(&mut producer).unwrap();
	let track = producer
		.create_track(
			producer.unique_name(".vp8"),
			moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
		)
		.unwrap();
	let mut config = VideoConfig::new(VideoCodec::VP8);
	config.coded_width = Some(320);
	config.coded_height = Some(240);
	config.container = Container::Legacy;
	catalog.lock().video.renditions.insert(track.name().to_string(), config);

	let mut track_producer = crate::container::Producer::new(track, crate::catalog::hang::Container::Legacy);
	track_producer
		.write(crate::container::Frame {
			timestamp: Timestamp::from_micros(0).unwrap(),
			payload: Bytes::from_static(&[0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a]),
			keyframe: true,
			duration: None,
		})
		.unwrap();
	track_producer.finish().unwrap();

	let catalog_stream = crate::catalog::Consumer::<()>::new(&consumer, crate::catalog::CatalogFormat::Hang)
		.await
		.expect("catalog consumer");
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
	let mut moov: Option<mp4_atom::Moov> = None;
	while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor).expect("decode init") {
		if let mp4_atom::Any::Moov(m) = atom {
			moov = Some(m);
		}
	}
	let moov = moov.expect("init segment missing moov");
	assert_eq!(moov.trak.len(), 1, "expected single track in moov");

	let trak = &moov.trak[0];
	let stsd = &trak.mdia.minf.stbl.stsd;
	assert_eq!(stsd.codecs.len(), 1, "expected single sample entry");
	let vp08 = match &stsd.codecs[0] {
		mp4_atom::Codec::Vp08(vp08) => vp08,
		other => panic!("expected Vp08 sample entry, got {:?}", other),
	};
	assert_eq!(vp08.visual.width, 320);
	assert_eq!(vp08.visual.height, 240);
	assert_eq!(vp08.vpcc.bit_depth, 8);

	// The synthesized init (vpcC included) must round-trip through encode.
	let mut buf = Vec::new();
	moov.encode(&mut buf).expect("encode synthesized moov");
}

/// VP9 source (catalog `Container::Legacy`, codec `vp09`, no `description`) →
/// fMP4 export must synthesize a `vp09` sample entry whose `vpcC` round-trips
/// the catalog's VP9 parameters.
#[tokio::test(start_paused = true)]
async fn vp9_source_to_cmaf_export_synthesizes_vp09() {
	use bytes::Bytes;
	use hang::catalog::{Container, VP9, VideoConfig};
	use moq_net::Timestamp;

	let broadcast = moq_net::BroadcastInfo::new();
	let mut producer = broadcast.produce();
	let consumer = producer.consume();

	let mut catalog = crate::catalog::Producer::new(&mut producer).unwrap();
	let track = producer
		.create_track(
			producer.unique_name(".vp9"),
			moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
		)
		.unwrap();
	let mut config = VideoConfig::new(VP9 {
		profile: 0,
		level: 20,
		bit_depth: 8,
		chroma_subsampling: 1,
		color_primaries: 2,
		transfer_characteristics: 2,
		matrix_coefficients: 5,
		full_range: false,
	});
	config.coded_width = Some(320);
	config.coded_height = Some(240);
	config.container = Container::Legacy;
	catalog.lock().video.renditions.insert(track.name().to_string(), config);

	let mut track_producer = crate::container::Producer::new(track, crate::catalog::hang::Container::Legacy);
	track_producer
		.write(crate::container::Frame {
			timestamp: Timestamp::from_micros(0).unwrap(),
			payload: Bytes::from_static(&[0x82, 0x49, 0x83, 0x42]),
			keyframe: true,
			duration: None,
		})
		.unwrap();
	track_producer.finish().unwrap();

	let catalog_stream = crate::catalog::Consumer::<()>::new(&consumer, crate::catalog::CatalogFormat::Hang)
		.await
		.expect("catalog consumer");
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
	let mut moov: Option<mp4_atom::Moov> = None;
	while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor).expect("decode init") {
		if let mp4_atom::Any::Moov(m) = atom {
			moov = Some(m);
		}
	}
	let moov = moov.expect("init segment missing moov");
	assert_eq!(moov.trak.len(), 1, "expected single track in moov");

	let trak = &moov.trak[0];
	let stsd = &trak.mdia.minf.stbl.stsd;
	let vp09 = match &stsd.codecs[0] {
		mp4_atom::Codec::Vp09(vp09) => vp09,
		other => panic!("expected Vp09 sample entry, got {:?}", other),
	};
	assert_eq!(vp09.visual.width, 320);
	assert_eq!(vp09.visual.height, 240);
	assert_eq!(vp09.vpcc.profile, 0);
	assert_eq!(vp09.vpcc.bit_depth, 8);
	assert_eq!(vp09.vpcc.matrix_coefficients, 5);

	// The synthesized init (vpcC included) must round-trip through encode.
	let mut buf = Vec::new();
	moov.encode(&mut buf).expect("encode synthesized moov");
}

/// AV1 source (catalog `Container::Legacy`, codec `av01`, no `description`) →
/// fMP4 export must synthesize an `av01` sample entry whose `av1C` round-trips
/// the catalog's AV1 parameters. AV1 publishes its sequence header in-band
/// (like `hev1`/`avc3`), so there is no out-of-band config and `config_obus`
/// stays empty.
#[tokio::test(start_paused = true)]
async fn av1_source_to_cmaf_export_synthesizes_av01() {
	use bytes::Bytes;
	use hang::catalog::{AV1, Container, VideoConfig};
	use moq_net::Timestamp;

	let broadcast = moq_net::BroadcastInfo::new();
	let mut producer = broadcast.produce();
	let consumer = producer.consume();

	let mut catalog = crate::catalog::Producer::new(&mut producer).unwrap();
	let track = producer
		.create_track(
			producer.unique_name(".av01"),
			moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
		)
		.unwrap();
	let mut config = VideoConfig::new(AV1 {
		profile: 0,
		level: 8,
		tier: 'M',
		bitdepth: 10,
		mono_chrome: false,
		chroma_subsampling_x: true,
		chroma_subsampling_y: true,
		chroma_sample_position: 2,
		color_primaries: 9,
		transfer_characteristics: 16,
		matrix_coefficients: 9,
		full_range: false,
	});
	config.coded_width = Some(320);
	config.coded_height = Some(240);
	config.container = Container::Legacy;
	catalog.lock().video.renditions.insert(track.name().to_string(), config);

	let mut track_producer = crate::container::Producer::new(track, crate::catalog::hang::Container::Legacy);
	track_producer
		.write(crate::container::Frame {
			timestamp: Timestamp::from_micros(0).unwrap(),
			payload: Bytes::from_static(&[0x12, 0x00, 0x0a, 0x0b]),
			keyframe: true,
			duration: None,
		})
		.unwrap();
	track_producer.finish().unwrap();

	let catalog_stream = crate::catalog::Consumer::<()>::new(&consumer, crate::catalog::CatalogFormat::Hang)
		.await
		.expect("catalog consumer");
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
	let mut moov: Option<mp4_atom::Moov> = None;
	while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor).expect("decode init") {
		if let mp4_atom::Any::Moov(m) = atom {
			moov = Some(m);
		}
	}
	let moov = moov.expect("init segment missing moov");
	assert_eq!(moov.trak.len(), 1, "expected single track in moov");

	let trak = &moov.trak[0];
	let stsd = &trak.mdia.minf.stbl.stsd;
	assert_eq!(stsd.codecs.len(), 1, "expected single sample entry");
	let av01 = match &stsd.codecs[0] {
		mp4_atom::Codec::Av01(av01) => av01,
		other => panic!("expected Av01 sample entry, got {:?}", other),
	};
	assert_eq!(av01.visual.width, 320);
	assert_eq!(av01.visual.height, 240);

	let av1c = &av01.av1c;
	assert_eq!(av1c.seq_profile, 0);
	assert_eq!(av1c.seq_level_idx_0, 8);
	assert!(!av1c.seq_tier_0, "Main tier");
	assert!(av1c.high_bitdepth, "10-bit");
	assert!(!av1c.twelve_bit);
	assert!(av1c.chroma_subsampling_x);
	assert!(av1c.chroma_subsampling_y);
	assert_eq!(av1c.chroma_sample_position, 2);
	assert!(av1c.config_obus.is_empty(), "sequence header stays in-band");

	// The synthesized init (av1C included) must round-trip through encode.
	let mut buf = Vec::new();
	moov.encode(&mut buf).expect("encode synthesized moov");
}

/// CMAF source (catalog `Container::Cmaf`) → fMP4 export should keep using
/// the passthrough init path: existing init bytes are merged into the moov.
///
/// Regression check that adding the Avc3 path didn't break the existing one.
#[tokio::test(start_paused = true)]
async fn cmaf_source_to_cmaf_export_passthrough() {
	let data = include_bytes!("test_data/bbb.mp4");

	let broadcast = moq_net::BroadcastInfo::new();
	let mut producer = broadcast.produce();
	let consumer = producer.consume();

	let catalog = crate::catalog::Producer::new(&mut producer).unwrap();
	let mut importer = crate::container::fmp4::Import::new(producer, catalog);
	let buf = BytesMut::from(data.as_slice());
	let _ = importer.decode(&buf);

	let catalog_stream = crate::catalog::Consumer::<()>::new(&consumer, crate::catalog::CatalogFormat::Hang)
		.await
		.expect("catalog consumer");
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

/// `next_fragment` reports the init flag, per-fragment sync-sample independence,
/// and a positive duration. With a sub-GOP fragment cap, a part in the middle of
/// a GOP is reported as non-independent while the GOP's leading part stays
/// independent. This is the metadata an HLS/LL-HLS packager consumes.
#[tokio::test(start_paused = true)]
async fn next_fragment_reports_segment_metadata() {
	use std::time::Duration;

	use bytes::BytesMut;
	use hang::catalog::{Container, H264, VideoConfig};
	use moq_net::Timestamp;

	let broadcast = moq_net::BroadcastInfo::new();
	let mut producer = broadcast.produce();
	let consumer = producer.consume();

	let mut catalog = crate::catalog::Producer::new(&mut producer).unwrap();
	let track = producer
		.create_track(
			producer.unique_name(".avc3"),
			moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
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
	config.framerate = Some(30.0);
	config.container = Container::Legacy;
	catalog.lock().video.renditions.insert(track.name().to_string(), config);

	const SC: &[u8] = &[0, 0, 0, 1];
	let sps = &[0x67u8, 0x42, 0xc0, 0x1f, 0xde, 0xad, 0xbe, 0xef][..];
	let pps = &[0x68u8, 0xce, 0x3c, 0x80][..];
	let idr = &[0x65u8, 0x88, 0x84, 0x21, 0x00, 0x11, 0x22, 0x33][..];
	let slice = &[0x41u8, 0x9a, 0x00, 0x01][..];

	let annexb = |nals: &[&[u8]]| {
		let mut buf = BytesMut::new();
		for nal in nals {
			buf.extend_from_slice(SC);
			buf.extend_from_slice(nal);
		}
		buf.freeze()
	};

	let frame = |timestamp_us: u64, payload, keyframe| crate::container::Frame {
		timestamp: Timestamp::from_micros(timestamp_us).unwrap(),
		payload,
		keyframe,
		duration: None,
	};

	let mut track_producer = crate::container::Producer::new(track, crate::catalog::hang::Container::Legacy);
	// GOP 0: keyframe@0 (SPS+PPS+IDR), delta@33ms. GOP 1: keyframe@66ms.
	track_producer.write(frame(0, annexb(&[sps, pps, idr]), true)).unwrap();
	track_producer.write(frame(33_000, annexb(&[slice]), false)).unwrap();
	track_producer
		.write(frame(66_000, annexb(&[sps, pps, idr]), true))
		.unwrap();
	track_producer.finish().unwrap();

	let catalog_stream = crate::catalog::Consumer::<()>::new(&consumer, crate::catalog::CatalogFormat::Hang)
		.await
		.expect("catalog consumer");
	// Sub-GOP cap so GOP 0 splits into two parts (the trailing part non-independent).
	let mut exporter =
		crate::container::fmp4::Export::new(consumer, catalog_stream).with_fragment_duration(Duration::from_millis(20));

	// First emit is the init segment.
	let init = tokio::time::timeout(Duration::from_secs(1), exporter.next_fragment())
		.await
		.expect("exporter timed out")
		.expect("exporter result")
		.expect("expected init fragment");
	assert!(init.init, "first fragment must be the init segment");
	assert!(!init.independent);
	assert_eq!(init.duration, 0.0);

	// The track is finished, so its three media fragments are all available. Keep
	// the broadcast/catalog producers alive (dropping them aborts the consumer);
	// the catalog stays open, so the exporter never reaches a clean end — read the
	// known fragment count rather than looping to `None`.
	let mut independents = Vec::new();
	for _ in 0..3 {
		let frag = tokio::time::timeout(Duration::from_secs(1), exporter.next_fragment())
			.await
			.expect("exporter timed out")
			.expect("exporter result")
			.expect("expected a media fragment");
		assert!(!frag.init);
		assert!(frag.duration > 0.0, "media fragment duration should be positive");
		independents.push(frag.independent);
	}

	// GOP 0 leading part (independent), GOP 0 trailing part (dependent),
	// GOP 1 leading part (independent).
	assert_eq!(independents, vec![true, false, true]);

	drop(track_producer);
	drop(catalog);
	drop(producer);
}

/// A legacy FLAC rendition (no init segment) synthesizes a `fLaC` sample entry
/// whose `dfLa` STREAMINFO is rebuilt from the catalog description.
#[test]
fn synthesize_flac_trak() {
	let description = crate::codec::flac::Config {
		min_block_size: 4096,
		max_block_size: 4096,
		min_frame_size: 0,
		max_frame_size: 0,
		sample_rate: 96_000,
		channel_count: 2,
		bits_per_sample: 24,
		total_samples: 0,
		md5: [0; 16],
	}
	.description();

	let mut config = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Flac, 96_000, 2);
	config.description = Some(description);

	let trak = super::synthesize_audio_trak(1, 96_000, &config).expect("synthesize FLAC trak");
	let codec = &trak.mdia.minf.stbl.stsd.codecs[0];
	let mp4_atom::Codec::Flac(flac) = codec else {
		panic!("expected a FLAC sample entry, got {codec:?}");
	};

	let stream_info = flac
		.dfla
		.blocks
		.iter()
		.find_map(|b| match b {
			mp4_atom::FlacMetadataBlock::StreamInfo {
				sample_rate,
				num_channels_minus_one,
				..
			} => Some((*sample_rate, *num_channels_minus_one)),
			_ => None,
		})
		.expect("STREAMINFO block");
	// STREAMINFO carries the real 96 kHz rate even though the 16.16 audio box can't.
	assert_eq!(stream_info, (96_000, 1));
}
