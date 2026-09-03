//! Tests for the fMP4 exporter.

use std::io::Cursor;

use bytes::{Bytes, BytesMut};
use mp4_atom::{DecodeMaybe, Encode};

use crate::container::test_util::{Live, PPS, SPS, raw_frame, video_frame};

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
	let mut live = Live::avc3();
	live.track.write(video_frame(0, true)).unwrap();
	live.track.finish().unwrap();

	let mut exporter = crate::container::fmp4::Export::new(live.source(), live.catalog_stream().await);
	let init = fragment_now(&mut exporter).await.data;

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
	assert_eq!(avc1.avcc.avc_profile_indication, SPS[1]);
	assert_eq!(avc1.avcc.avc_level_indication, SPS[3]);
	assert_eq!(avc1.avcc.sequence_parameter_sets.len(), 1);
	assert_eq!(avc1.avcc.sequence_parameter_sets[0].as_slice(), SPS);
	assert_eq!(avc1.avcc.picture_parameter_sets[0].as_slice(), PPS);
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
	use hang::catalog::{AAC, AudioConfig};

	// AAC-LC (profile 2), 44100 Hz, stereo. The TS importer sets `description`
	// via aac::Config::encode; mirror that here.
	let description = crate::codec::aac::Config {
		profile: 2,
		sample_rate: 44100,
		channel_count: 2,
	}
	.encode();
	let mut config = AudioConfig::new(AAC { profile: 2 }, 44100, 2);
	config.description = Some(description);

	let mut live = Live::audio(config);
	live.track.write(raw_frame(0, &[0x01, 0x02, 0x03, 0x04], true)).unwrap();
	live.track.finish().unwrap();

	let mut exporter = crate::container::fmp4::Export::new(live.source(), live.catalog_stream().await);
	let init = fragment_now(&mut exporter).await.data;

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

	let dec_specific = dec_config.dec_specific.as_ref().expect("AAC DecoderSpecificInfo");
	assert_eq!(dec_specific.profile, 2, "AAC-LC");
	assert_eq!(dec_specific.freq_index, 4, "44100 Hz");
	assert_eq!(dec_specific.chan_conf, 2, "stereo");

	// The synthesized init must round-trip through encode (esds included).
	let mut buf = Vec::new();
	moov.encode(&mut buf).expect("encode synthesized moov");
}

/// VP8 source (catalog `Container::Legacy`, codec `vp8`, no dimensions or
/// `description`) → fMP4 export derives geometry from the keyframe and
/// synthesizes a `vp08` sample entry. VP8 carries no out-of-band config, so
/// this exercises the dimensionless startup and description-less synthesis paths.
#[tokio::test(start_paused = true)]
async fn vp8_source_to_cmaf_export_synthesizes_vp08() {
	use hang::catalog::{Container, VideoCodec, VideoConfig};

	let mut live = Live::new(".vp8", |catalog, name| {
		let mut config = VideoConfig::new(VideoCodec::VP8);
		config.container = Container::Legacy;
		catalog.lock().video.renditions.insert(name, config);
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

	let mut exporter = crate::container::fmp4::Export::new(live.source(), live.catalog_stream().await);
	let init = fragment_now(&mut exporter).await.data;

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

/// If codec data cannot reveal geometry yet, the exporter waits for a later
/// catalog snapshot instead of freezing zero dimensions into the init segment.
#[tokio::test(start_paused = true)]
async fn dimensionless_video_waits_for_catalog_geometry() {
	use hang::catalog::{Container, VideoCodec, VideoConfig};

	let mut live = Live::new(".vp8", |catalog, name| {
		let mut config = VideoConfig::new(VideoCodec::VP8);
		config.container = Container::Legacy;
		catalog.lock().video.renditions.insert(name, config);
	});
	let name = live.track.name().to_string();
	// A VP8 interframe carries no geometry. Mark it as the group boundary only
	// so the synthetic producer accepts it before the catalog update arrives.
	live.track.write(raw_frame(0, &[0x31, 0x00, 0x00], true)).unwrap();

	let mut exporter = crate::container::fmp4::Export::new(live.source(), live.catalog_stream().await);
	let pending = tokio::time::timeout(std::time::Duration::from_secs(1), exporter.next()).await;
	assert!(
		pending.is_err(),
		"exporter emitted an init without geometry: {pending:?}"
	);

	{
		let mut catalog = live.catalog.lock();
		let config = catalog.video.renditions.get_mut(&name).unwrap();
		config.coded_width = Some(320);
		config.coded_height = Some(240);
	}

	let init = fragment_now(&mut exporter).await.data;
	let mut cursor = Cursor::new(init.as_ref());
	let mut moov = None;
	while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor).expect("decode init") {
		if let mp4_atom::Any::Moov(value) = atom {
			moov = Some(value);
		}
	}
	let trak = &moov.expect("init segment missing moov").trak[0];
	let mp4_atom::Codec::Vp08(vp08) = &trak.mdia.minf.stbl.stsd.codecs[0] else {
		panic!("expected vp08 sample entry");
	};
	assert_eq!((vp08.visual.width, vp08.visual.height), (320, 240));
}

/// A fixed codec description cannot recover on a later frame, so malformed
/// metadata must fail instead of leaving the exporter pending for geometry.
#[tokio::test(start_paused = true)]
async fn dimensionless_video_rejects_a_malformed_description() {
	use hang::catalog::{Container, H264, VideoConfig};

	let live = Live::new(".avc1", |catalog, name| {
		let mut config = VideoConfig::new(H264 {
			profile: 0x42,
			constraints: 0,
			level: 0x1f,
			inline: false,
		});
		config.description = Some(bytes::Bytes::from_static(&[1]));
		config.container = Container::Legacy;
		catalog.lock().video.renditions.insert(name, config);
	});

	let mut exporter = crate::container::fmp4::Export::new(live.source(), live.catalog_stream().await);
	let error = exporter.next().await.expect_err("malformed fixed description");
	assert!(matches!(
		error,
		crate::Error::H264(crate::codec::h264::Error::AvccTooShort)
	));
}

/// VP9 source (catalog `Container::Legacy`, codec `vp09`, no `description`) →
/// fMP4 export must synthesize a `vp09` sample entry whose `vpcC` round-trips
/// the catalog's VP9 parameters.
#[tokio::test(start_paused = true)]
async fn vp9_source_to_cmaf_export_synthesizes_vp09() {
	use hang::catalog::{Container, VP9, VideoConfig};

	let mut live = Live::new(".vp9", |catalog, name| {
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
		catalog.lock().video.renditions.insert(name, config);
	});
	live.track.write(raw_frame(0, &[0x82, 0x49, 0x83, 0x42], true)).unwrap();
	live.track.finish().unwrap();

	let mut exporter = crate::container::fmp4::Export::new(live.source(), live.catalog_stream().await);
	let init = fragment_now(&mut exporter).await.data;

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
	use hang::catalog::{AV1, Container, VideoConfig};

	let mut live = Live::new(".av01", |catalog, name| {
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
		catalog.lock().video.renditions.insert(name, config);
	});
	live.track.write(raw_frame(0, &[0x12, 0x00, 0x0a, 0x0b], true)).unwrap();
	live.track.finish().unwrap();

	let mut exporter = crate::container::fmp4::Export::new(live.source(), live.catalog_stream().await);
	let init = fragment_now(&mut exporter).await.data;

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

	let broadcast = moq_net::broadcast::Info::new();
	let mut producer = broadcast.produce();
	let consumer = producer.consume();

	let catalog = crate::catalog::Producer::new(&mut producer).unwrap();
	let mut importer = crate::container::fmp4::Import::new(producer, catalog.reserve());
	let buf = BytesMut::from(data.as_slice());
	let _ = importer.decode(&buf);

	let catalog_stream = crate::catalog::Consumer::<()>::new(&consumer, crate::catalog::CatalogFormat::Hang)
		.await
		.expect("catalog consumer");
	let mut exporter = crate::container::fmp4::Export::new(crate::source::announced(&consumer), catalog_stream);

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

/// Per-rendition export (a single non-first source track, e.g. moq-hls exporting
/// the audio rendition alone) must give the init moov the SAME track id its
/// re-encoded fragments carry.
///
/// bbb.mp4's audio is the second track, so its source CMAF init declares track id
/// 2, but an audio-only export re-encodes fragments as track 1 (the exporter's own
/// per-export numbering). If the moov kept the source id (2) while the moof said 1,
/// a player would reject every segment ("no tfhd for track") and stall -- the VOD
/// audio-playback bug this guards against.
#[tokio::test(start_paused = true)]
async fn single_track_export_init_matches_fragment_track_id() {
	use crate::catalog::Stream;

	let data = include_bytes!("test_data/bbb.mp4");

	let mut producer = moq_net::broadcast::Info::new().produce();
	let consumer = producer.consume();

	let catalog = crate::catalog::Producer::new(&mut producer).unwrap();
	let mut importer = crate::container::fmp4::Import::new(producer, catalog.reserve());
	let buf = BytesMut::from(data.as_slice());
	let _ = importer.decode(&buf);

	// Audio only: unselected video is dropped, so this is a single-track export.
	let catalog_stream = crate::catalog::Consumer::<()>::new(&consumer, crate::catalog::CatalogFormat::Hang)
		.await
		.expect("catalog consumer");
	let selected = catalog_stream.select(crate::select::Broadcast::default().audio(crate::select::Audio::default()));
	let mut exporter = crate::container::fmp4::Export::new(crate::source::announced(&consumer), selected);

	let init = tokio::time::timeout(std::time::Duration::from_secs(1), exporter.next())
		.await
		.expect("exporter timed out")
		.expect("exporter result")
		.expect("expected init bytes");

	// The next non-init fragment is a moof+mdat for the same (only) track.
	let fragment = tokio::time::timeout(std::time::Duration::from_secs(1), exporter.next())
		.await
		.expect("exporter timed out")
		.expect("exporter result")
		.expect("expected a fragment");

	drop(importer);

	// init moov: exactly one trak, whose id must equal its trex id.
	let mut cursor = Cursor::new(init.as_ref());
	let mut moov: Option<mp4_atom::Moov> = None;
	while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor).expect("decode init") {
		if let mp4_atom::Any::Moov(m) = atom {
			moov = Some(m);
		}
	}
	let moov = moov.expect("moov");
	assert_eq!(moov.trak.len(), 1, "audio-only export has one track");
	let init_id = moov.trak[0].tkhd.track_id;
	let mvex = moov.mvex.as_ref().expect("mvex");
	assert_eq!(mvex.trex[0].track_id, init_id, "trex id must match its trak");

	// fragment moof: the tfhd track id must match the init.
	let mut cursor = Cursor::new(fragment.as_ref());
	let mut moof: Option<mp4_atom::Moof> = None;
	while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor).expect("decode fragment") {
		if let mp4_atom::Any::Moof(m) = atom {
			moof = Some(m);
		}
	}
	let moof = moof.expect("moof");
	assert_eq!(
		moof.traf[0].tfhd.track_id, init_id,
		"fragment track id must match the init moov, or players reject the segment"
	);
}

/// `next_fragment` reports the init flag, per-fragment sync-sample independence,
/// and a positive duration. With a sub-GOP fragment cap, a part in the middle of
/// a GOP is reported as non-independent while the GOP's leading part stays
/// independent. This is the metadata an HLS/LL-HLS packager consumes.
#[tokio::test(start_paused = true)]
async fn next_fragment_reports_segment_metadata() {
	let mut live = Live::avc3();
	// GOP 0: keyframe@0 (SPS+PPS+IDR), delta@33ms. GOP 1: keyframe@66ms.
	live.track.write(video_frame(0, true)).unwrap();
	live.track.write(video_frame(33_000, false)).unwrap();
	live.track.write(video_frame(66_000, true)).unwrap();
	live.track.finish().unwrap();

	// Sub-GOP cap so GOP 0 splits into two parts (the trailing part non-independent).
	let mut exporter = crate::container::fmp4::Export::new(live.source(), live.catalog_stream().await)
		.with_fragment_duration(std::time::Duration::from_millis(20));

	// First emit is the init segment.
	let init = fragment_now(&mut exporter).await;
	assert!(init.init, "first fragment must be the init segment");
	assert!(!init.independent);
	assert_eq!(init.duration, 0.0);

	// The track is finished, so its three media fragments are all available. The
	// catalog stays open, so the exporter never reaches a clean end. Read the
	// known fragment count rather than looping to `None`.
	let mut independents = Vec::new();
	for _ in 0..3 {
		let frag = fragment_now(&mut exporter).await;
		assert!(!frag.init);
		assert!(frag.duration > 0.0, "media fragment duration should be positive");
		independents.push(frag.independent);
	}

	// GOP 0 leading part (independent), GOP 0 trailing part (dependent),
	// GOP 1 leading part (independent).
	assert_eq!(independents, vec![true, false, true]);
}

#[tokio::test(start_paused = true)]
async fn zero_fragment_duration_emits_without_successor() {
	let mut live = Live::avc3();
	live.track.write(video_frame(0, true)).unwrap();

	let mut exporter = crate::container::fmp4::Export::new(live.source(), live.catalog_stream().await)
		.with_fragment_duration(std::time::Duration::ZERO);
	assert!(fragment_now(&mut exporter).await.init);

	let fragment = fragment_now(&mut exporter).await;
	assert!(!fragment.init);
	assert!(fragment.independent, "a keyframe-led fragment can start a segment");
	assert!(fragment.duration > 0.0);
}

#[tokio::test(start_paused = true)]
async fn ntsc_tail_uses_a_representable_catalog_cadence() {
	use hang::catalog::{Container, H264, VideoConfig};

	let mut live = Live::new(".ntsc", |catalog, name| {
		let mut config = VideoConfig::new(H264 {
			profile: 0x42,
			constraints: 0xc0,
			level: 0x1f,
			inline: true,
		});
		config.coded_width = Some(320);
		config.coded_height = Some(240);
		config.framerate = Some(30_000.0 / 1001.0);
		config.container = Container::Legacy;
		catalog.lock().video.renditions.insert(name, config);
	});
	live.track.write(video_frame(0, true)).unwrap();
	live.track.finish().unwrap();

	let mut exporter = crate::container::fmp4::Export::new(live.source(), live.catalog_stream().await);
	assert!(fragment_now(&mut exporter).await.init);
	let fragment = fragment_now(&mut exporter).await;
	assert_eq!(super::sample_durations(&fragment.data), vec![Some(1001)]);
}

#[tokio::test(start_paused = true)]
async fn unusable_framerate_uses_the_standard_fallback_rate() {
	use hang::catalog::{Container, VideoCodec, VideoConfig};

	let mut live = Live::new(".vp8", |catalog, name| {
		let mut config = VideoConfig::new(VideoCodec::VP8);
		config.coded_width = Some(320);
		config.coded_height = Some(240);
		config.framerate = Some(0.0005);
		config.container = Container::Legacy;
		catalog.lock().video.renditions.insert(name, config);
	});
	live.track.write(raw_frame(0, &[0x82, 0x00], true)).unwrap();
	live.track.finish().unwrap();

	let mut exporter = crate::container::fmp4::Export::new(live.source(), live.catalog_stream().await);
	assert!(fragment_now(&mut exporter).await.init);

	let fragment = fragment_now(&mut exporter).await;
	assert!((fragment.duration - 1.0 / 30.0).abs() < 1e-9);
	let timescale = moq_net::Timescale::new(90_000).unwrap();
	let decoded = super::decode(fragment.data, timescale).unwrap();
	assert_eq!(decoded[0].duration.unwrap().as_scale(timescale), 3_000);
}

#[tokio::test(start_paused = true)]
async fn audio_only_default_mode_emits_without_successor() {
	use hang::catalog::{AAC, AudioConfig};

	let aac = crate::codec::aac::Config {
		profile: 2,
		sample_rate: 44100,
		channel_count: 2,
	};
	let mut config = AudioConfig::new(AAC { profile: 2 }, 44100, 2);
	config.description = Some(aac.encode());

	let mut live = Live::audio(config);
	live.track.write(raw_frame(0, &[0x01, 0x02, 0x03, 0x04], true)).unwrap();

	let mut exporter = crate::container::fmp4::Export::new(live.source(), live.catalog_stream().await);
	assert!(fragment_now(&mut exporter).await.init);

	let fragment = fragment_now(&mut exporter).await;
	assert!(!fragment.init);
	assert!(fragment.independent, "audio fragments are always independent");
	// An AAC frame is 1024 samples, so the catalog fallback is the real duration.
	assert!(
		(fragment.duration - 1024.0 / 44100.0).abs() < 1e-4,
		"expected one AAC frame of duration, got {}",
		fragment.duration
	);
}

#[tokio::test(start_paused = true)]
async fn opus_frame_duration_from_toc() {
	use hang::catalog::{AudioCodec, AudioConfig};

	let mut live = Live::audio(AudioConfig::new(AudioCodec::Opus, 48_000, 2));
	// TOC 0x08: config 1 (SILK 20 ms), code 0 (one frame) = 960 samples at 48 kHz.
	live.track.write(raw_frame(0, &[0x08, 0xaa, 0xbb, 0xcc], true)).unwrap();

	let mut exporter = crate::container::fmp4::Export::new(live.source(), live.catalog_stream().await);
	assert!(fragment_now(&mut exporter).await.init);

	let fragment = fragment_now(&mut exporter).await;
	assert!(
		(fragment.duration - 0.02).abs() < 1e-4,
		"expected the 20 ms TOC duration, got {}",
		fragment.duration
	);
}

#[test]
fn synthesize_opus_trak_preserves_pre_skip() {
	use hang::catalog::{AudioCodec, AudioConfig};

	let head = crate::codec::opus::Config::new(48_000, 2)
		.with_pre_skip(312)
		.encode()
		.unwrap();
	let mut config = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
	config.description = Some(head);

	let trak = super::synthesize_audio_trak(1, 48_000, &config).expect("synthesize Opus trak");
	let opus = match &trak.mdia.minf.stbl.stsd.codecs[0] {
		mp4_atom::Codec::Opus(opus) => opus,
		other => panic!("expected Opus sample entry, got {other:?}"),
	};
	assert_eq!(opus.dops.pre_skip, 312);
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

/// A long GOP must not cost a stack frame per sample.
///
/// Appending a frame to a track's buffer restarts the search for work, and
/// doing that by calling back into the poll leaves one stack frame behind per
/// buffered sample. A track buffers a whole GOP, so ten seconds of 60 fps video
/// overflows the stack rather than emitting a fragment.
///
/// The thread is given a small stack on purpose. The default is large enough to
/// need thousands of frames before it fails, which is more video than a test
/// should have to build; 1 MiB fails on the recursive version at this GOP
/// length and is ample for the iterative one.
#[test]
fn a_long_gop_does_not_cost_a_stack_frame_per_sample() {
	let run = std::thread::Builder::new()
		.stack_size(1024 * 1024)
		.spawn(|| {
			let rt = tokio::runtime::Builder::new_current_thread()
				.enable_time()
				.start_paused(true)
				.build()
				.expect("runtime");

			rt.block_on(async {
				let mut live = Live::avc3();
				// Ten seconds of 60 fps in one GOP, closed by the next keyframe.
				for i in 0..600u64 {
					live.track.write(video_frame(i * 16_666, i == 0)).unwrap();
				}
				live.track.write(video_frame(600 * 16_666, true)).unwrap();

				let mut exporter = crate::container::fmp4::Export::new(live.source(), live.catalog_stream().await);
				assert!(
					fragment_now(&mut exporter).await.init,
					"first fragment must be the init segment"
				);

				// The whole GOP in one fragment, which is also what says the
				// loop kept appending rather than emitting early.
				let fragment = fragment_now(&mut exporter).await;
				assert!(!fragment.init);
				assert!(fragment.independent, "a GOP opens on a keyframe");
				assert!(fragment.duration > 0.0);
			});
		})
		.expect("spawn");

	run.join().expect("a long GOP overflowed the stack");
}

/// An A/V broadcast must roll its audio fragments alongside the video ones.
///
/// Audio has no keyframes, so with the default (per-GOP) fragmenting it has no
/// rollover trigger of its own: its samples used to pile up in the track buffer
/// and land in a single `traf` after the video track had already finished, which
/// left a player seeking to a video fragment with no audio to go with it.
#[tokio::test(start_paused = true)]
async fn audio_fragments_roll_with_the_video_gop() {
	use hang::catalog::{AudioCodec, AudioConfig, Container};

	let mut live = Live::avc3();
	let mut audio = live.add_track(".opus", |catalog, name| {
		let mut config = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
		config.container = Container::Legacy;
		catalog.lock().audio.renditions.insert(name, config);
	});

	// Three GOPs of three 30 fps frames, keyframes at 0, 99 ms and 198 ms.
	for i in 0..9u64 {
		live.track.write(video_frame(i * 33_000, i % 3 == 0)).unwrap();
	}
	// 20 ms Opus packets (TOC 0x08) covering the same span and a little past it.
	for i in 0..12u64 {
		audio
			.write(raw_frame(i * 20_000, &[0x08, 0xaa, 0xbb, 0xcc], true))
			.unwrap();
	}
	live.track.finish().unwrap();
	audio.finish().unwrap();

	let mut exporter = crate::container::fmp4::Export::new(live.source(), live.catalog_stream().await);
	let init = fragment_now(&mut exporter).await;
	assert!(init.init);
	let (video_id, audio_id) = track_ids(&init.data);

	let fragments = drain_now(&mut exporter).await;
	assert_ascending_starts(&init.data, &fragments);
	assert_ascending_sequence_numbers(&fragments);

	// Every fragment, tagged with the track it belongs to and its sample count.
	let fragments: Vec<(u32, usize)> = fragments
		.iter()
		.map(|fragment| {
			let trafs = traf_samples(&fragment.data);
			assert_eq!(trafs.len(), 1, "expected one traf per fragment");
			trafs[0]
		})
		.collect();

	// Audio rolls with the video GOP, so the two tracks alternate: five 20 ms
	// packets beside each three-frame GOP, then the tail of both tracks.
	assert_eq!(
		fragments,
		vec![
			(video_id, 3),
			(audio_id, 5),
			(video_id, 3),
			(audio_id, 5),
			(video_id, 3),
			(audio_id, 2),
		],
	);
}

/// A video track that ends while the audio plays on must not leave its last GOP
/// behind the whole rest of the audio.
///
/// Audio falls back to per-frame fragmenting once no unfinished video track is
/// left to roll it, so a source with another packet always ready (a recording, a
/// fetch) keeps producing fragments and the exporter never reaches the idle path
/// that drains a finished track's buffer. The video's tail then lands after every
/// audio fragment, however long the audio runs on: a `tfdt` inversion with no
/// bound on it.
#[tokio::test(start_paused = true)]
async fn video_that_ends_first_writes_its_tail_in_order() {
	use hang::catalog::{AudioCodec, AudioConfig, Container};

	let mut live = Live::avc3();
	let mut audio = live.add_track(".opus", |catalog, name| {
		let mut config = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
		config.container = Container::Legacy;
		catalog.lock().audio.renditions.insert(name, config);
	});

	// Two 30 fps GOPs of three frames each, and then the video ends.
	for i in 0..6u64 {
		live.track.write(video_frame(i * 33_000, i % 3 == 0)).unwrap();
	}
	live.track.finish().unwrap();
	// The audio runs on for another half second, always ready.
	for i in 0..30u64 {
		audio
			.write(raw_frame(i * 20_000, &[0x08, 0xaa, 0xbb, 0xcc], true))
			.unwrap();
	}

	let mut exporter = crate::container::fmp4::Export::new(live.source(), live.catalog_stream().await);
	let init = fragment_now(&mut exporter).await;
	assert!(init.init);
	let (video_id, audio_id) = track_ids(&init.data);

	let fragments = drain_now(&mut exporter).await;
	assert_ascending_starts(&init.data, &fragments);
	assert_ascending_sequence_numbers(&fragments);

	let counts: Vec<(u32, usize)> = fragments
		.iter()
		.map(|fragment| {
			let trafs = traf_samples(&fragment.data);
			assert_eq!(trafs.len(), 1, "expected one traf per fragment");
			trafs[0]
		})
		.collect();

	// The first GOP rolls the first five packets with it; the second GOP is the
	// video tail, which goes out as soon as the video ends rather than waiting
	// for the audio, followed by the audio buffered up to that point.
	assert_eq!(
		counts[..4],
		[(video_id, 3), (audio_id, 5), (video_id, 3), (audio_id, 4)],
		"the video tail must not trail the audio",
	);
	// Everything after it is audio, one fragment per packet.
	assert!(counts[4..].iter().all(|(id, samples)| *id == audio_id && *samples == 1));
	let audio_samples: usize = counts.iter().filter(|(id, _)| *id == audio_id).map(|(_, n)| n).sum();
	assert_eq!(audio_samples, 30, "every audio packet must be written exactly once");
	let video_samples: usize = counts.iter().filter(|(id, _)| *id == video_id).map(|(_, n)| n).sum();
	assert_eq!(video_samples, 6, "every video frame must be written exactly once");
}

/// Audio must not buffer without bound while a video track stalls.
///
/// Rolling the audio at the video GOP boundary leaves it with no trigger of its
/// own, and a video track that stops delivering without ending, or an encoder
/// with an effectively infinite keyframe interval (screen sharing), never draws
/// another boundary. The audio buffer would then grow for the length of the
/// session, which is the failure the GOP rollover set out to remove.
#[tokio::test(start_paused = true)]
async fn audio_rolls_on_its_own_cap_when_the_video_stalls() {
	use hang::catalog::{AudioCodec, AudioConfig, Container};

	let mut live = Live::avc3();
	let mut audio = live.add_track(".opus", |catalog, name| {
		let mut config = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
		config.container = Container::Legacy;
		catalog.lock().audio.renditions.insert(name, config);
	});

	// One keyframe, enough to build the init segment, and then the video stalls:
	// it delivers nothing more and never ends.
	live.track.write(video_frame(0, true)).unwrap();
	// Twelve seconds of 20 ms Opus packets.
	for i in 0..600u64 {
		audio
			.write(raw_frame(i * 20_000, &[0x08, 0xaa, 0xbb, 0xcc], true))
			.unwrap();
	}

	let mut exporter = crate::container::fmp4::Export::new(live.source(), live.catalog_stream().await);
	let init = fragment_now(&mut exporter).await;
	assert!(init.init);
	let audio_id = track_ids(&init.data).1;

	let fragments = drain_now(&mut exporter).await;
	assert_ascending_starts(&init.data, &fragments);
	assert_ascending_sequence_numbers(&fragments);
	for fragment in &fragments {
		assert!(
			fragment.duration <= 5.0,
			"an audio fragment spanned {} seconds with no video to roll it",
			fragment.duration,
		);
	}

	// Two full runs of the cap; the last two seconds stay buffered, still able to
	// roll with the video if it ever comes back.
	let counts: Vec<(u32, usize)> = fragments
		.iter()
		.map(|fragment| {
			let trafs = traf_samples(&fragment.data);
			assert_eq!(trafs.len(), 1, "expected one traf per fragment");
			trafs[0]
		})
		.collect();
	assert_eq!(counts, vec![(audio_id, 250), (audio_id, 250)]);
}

/// A simulcast ladder rolls each video track on its own keyframes: one
/// rendition's GOP boundary must not split another's, or the second rendition's
/// fragments stop being independently decodable. Audio rolls at whichever
/// boundary comes first, so it still never accumulates past one GOP.
#[tokio::test(start_paused = true)]
async fn simulcast_keyframes_only_roll_their_own_video_track() {
	use hang::catalog::{AudioCodec, AudioConfig, Container, H264, VideoConfig};

	let mut live = Live::avc3();
	let mut fast = live.add_track(".avc3", |catalog, name| {
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
		catalog.lock().video.renditions.insert(name, config);
	});
	let mut audio = live.add_track(".opus", |catalog, name| {
		let mut config = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
		config.container = Container::Legacy;
		catalog.lock().audio.renditions.insert(name, config);
	});

	// The same 30 fps cadence on both video tracks, but a three-frame GOP on one
	// and a two-frame GOP on the other, so their keyframes rarely coincide.
	for i in 0..9u64 {
		live.track.write(video_frame(i * 33_000, i % 3 == 0)).unwrap();
	}
	for i in 0..8u64 {
		fast.write(video_frame(i * 33_000, i % 2 == 0)).unwrap();
	}
	for i in 0..12u64 {
		audio
			.write(raw_frame(i * 20_000, &[0x08, 0xaa, 0xbb, 0xcc], true))
			.unwrap();
	}
	live.track.finish().unwrap();
	fast.finish().unwrap();
	audio.finish().unwrap();

	let mut exporter = crate::container::fmp4::Export::new(live.source(), live.catalog_stream().await);
	let init = fragment_now(&mut exporter).await;
	assert!(init.init);
	let audio_id = track_ids(&init.data).1;

	let fragments = drain_now(&mut exporter).await;
	assert_ascending_sequence_numbers(&fragments);
	// Only per track: two renditions rolling on keyframes that don't coincide
	// interleave their fragments out of order, which is what `Export::next`
	// promises and all it promises.
	assert_each_track_ascends(&init.data, &fragments);

	let mut per_track: std::collections::BTreeMap<u32, Vec<usize>> = std::collections::BTreeMap::new();
	for fragment in fragments {
		let trafs = traf_samples(&fragment.data);
		assert_eq!(trafs.len(), 1, "expected one traf per fragment");
		let (track_id, samples) = trafs[0];
		assert!(
			fragment.independent,
			"track {track_id} fragment of {samples} samples was split off a keyframe",
		);
		per_track.entry(track_id).or_default().push(samples);
	}

	// Each video track keeps its own GOP length; the audio track rolls at every
	// boundary either of them draws, plus the tail both leave behind.
	let mut video: Vec<Vec<usize>> = per_track
		.iter()
		.filter(|(id, _)| **id != audio_id)
		.map(|(_, samples)| samples.clone())
		.collect();
	video.sort();
	assert_eq!(video, vec![vec![2, 2, 2, 2], vec![3, 3, 3]]);
	assert_eq!(per_track[&audio_id], vec![4, 1, 2, 3, 2]);
}

/// The `(video, audio)` track ids declared by an init segment.
fn track_ids(init: &Bytes) -> (u32, u32) {
	let mut cursor = Cursor::new(init.as_ref());
	let mut video = None;
	let mut audio = None;
	while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor).expect("decode init") {
		let mp4_atom::Any::Moov(moov) = atom else {
			continue;
		};
		for trak in &moov.trak {
			match trak.mdia.hdlr.handler.to_string().as_str() {
				"vide" => video = Some(trak.tkhd.track_id),
				"soun" => audio = Some(trak.tkhd.track_id),
				other => panic!("unexpected handler {other}"),
			}
		}
	}
	(video.expect("a video trak"), audio.expect("an audio trak"))
}

/// The `(track_id, sample count)` of every `traf` in a media fragment.
fn traf_samples(fragment: &Bytes) -> Vec<(u32, usize)> {
	let mut cursor = Cursor::new(fragment.as_ref());
	let mut trafs = Vec::new();
	while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor).expect("decode fragment") {
		let mp4_atom::Any::Moof(moof) = atom else {
			continue;
		};
		for traf in moof.traf {
			let samples = traf.trun.iter().map(|trun| trun.entries.len()).sum();
			trafs.push((traf.tfhd.track_id, samples));
		}
	}
	trafs
}

/// The timescale the init segment declares for each track id.
fn track_timescales(init: &Bytes) -> std::collections::BTreeMap<u32, u64> {
	let mut cursor = Cursor::new(init.as_ref());
	let mut scales = std::collections::BTreeMap::new();
	while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor).expect("decode init") {
		if let mp4_atom::Any::Moov(moov) = atom {
			for trak in &moov.trak {
				scales.insert(trak.tkhd.track_id, u64::from(trak.mdia.mdhd.timescale));
			}
		}
	}
	scales
}

/// The presentation time each fragment starts at (its `tfdt`), in seconds, read
/// at the timescale its track declares in the init segment.
fn fragment_starts(init: &Bytes, fragments: &[crate::container::fmp4::Fragment]) -> Vec<(u32, f64)> {
	let scales = track_timescales(init);
	fragments
		.iter()
		.map(|fragment| {
			let traf = super::first_traf(&fragment.data);
			let track_id = traf.tfhd.track_id;
			let tfdt = traf.tfdt.expect("a tfdt").base_media_decode_time;
			let scale = *scales.get(&track_id).expect("a track in the init segment");
			(track_id, tfdt as f64 / scale as f64)
		})
		.collect()
}

/// Assert the fragments' start times never go backwards, across every track.
fn assert_ascending_starts(init: &Bytes, fragments: &[crate::container::fmp4::Fragment]) {
	let starts = fragment_starts(init, fragments);
	for pair in starts.windows(2) {
		let [(previous_id, previous), (id, start)] = pair else {
			unreachable!();
		};
		assert!(
			start >= previous,
			"track {id} starts at {start}s after track {previous_id} started at {previous}s: {starts:?}",
		);
	}
}

/// Every track's own fragments must ascend in start time, whatever the order
/// they are interleaved in.
fn assert_each_track_ascends(init: &Bytes, fragments: &[crate::container::fmp4::Fragment]) {
	let starts = fragment_starts(init, fragments);
	let mut previous: std::collections::BTreeMap<u32, f64> = std::collections::BTreeMap::new();
	for (id, start) in &starts {
		if let Some(previous) = previous.insert(*id, *start) {
			assert!(
				*start > previous,
				"track {id} starts at {start}s after starting at {previous}s: {starts:?}",
			);
		}
	}
}

/// The `mfhd` sequence numbers must ascend from one fragment to the next, which
/// is what ISO/IEC 14496-12 section 8.8.5 asks of a file.
fn assert_ascending_sequence_numbers(fragments: &[crate::container::fmp4::Fragment]) {
	let numbers: Vec<u32> = fragments
		.iter()
		.map(|fragment| first_moof(&fragment.data).mfhd.sequence_number)
		.collect();
	for pair in numbers.windows(2) {
		assert!(
			pair[1] > pair[0],
			"sequence numbers must ascend in file order, got {numbers:?}",
		);
	}
}

/// The first `moof` of a media fragment.
fn first_moof(fragment: &Bytes) -> mp4_atom::Moof {
	let mut cursor = Cursor::new(fragment.as_ref());
	while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor).expect("decode fragment") {
		if let mp4_atom::Any::Moof(moof) = atom {
			return moof;
		}
	}
	panic!("a fragment with no moof");
}

/// Every fragment the exporter can produce without waiting for more input.
async fn drain_now(
	exporter: &mut crate::container::fmp4::Export<crate::catalog::Consumer>,
) -> Vec<crate::container::fmp4::Fragment> {
	let mut fragments = Vec::new();
	while let Ok(next) = tokio::time::timeout(std::time::Duration::from_millis(1), exporter.next_fragment()).await {
		match next.expect("exporter failed") {
			Some(fragment) => fragments.push(fragment),
			None => break,
		}
	}
	fragments
}

/// The next fragment, required to be ready without another frame arriving.
async fn fragment_now(
	exporter: &mut crate::container::fmp4::Export<crate::catalog::Consumer>,
) -> crate::container::fmp4::Fragment {
	tokio::time::timeout(std::time::Duration::from_millis(1), exporter.next_fragment())
		.await
		.expect("waited for a successor frame")
		.expect("exporter failed")
		.expect("expected a fragment")
}
