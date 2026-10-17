use futures::FutureExt;
use hang::catalog::Container;
use mp4_atom::{Atom, Decode, Encode};

/// A drift budget no test timeline comes close to, so every group is read.
///
/// The media track's full retention window, so a reader started after publishing can
/// still read every retained group. These tests write every group up front, which the
/// default [`std::time::Duration::ZERO`](std::time::Duration::ZERO) budget collapses to the live
/// edge: completeness has to be asked for.
const RECORDING_MAX_DELAY: std::time::Duration = std::time::Duration::from_secs(30);

/// A replay window with that budget, for a test asserting every group is delivered.
fn replay() -> moq_net::track::Subscription {
	moq_net::track::Subscription::default().with_max_delay(RECORDING_MAX_DELAY)
}

/// Drain every group currently buffered on the consumer without waiting for new ones.
/// Used in tests where the producer is still alive after writing.
#[cfg(test)]
fn drain_group_sequences(consumer: &mut moq_net::track::Subscriber) -> Vec<u64> {
	let mut sequences = Vec::new();
	while let Some(group) = consumer.recv_group().now_or_never().and_then(|r| r.ok().flatten()) {
		sequences.push(group.sequence);
	}
	sequences
}

fn run_fmp4(data: &[u8]) -> crate::catalog::hang::Catalog {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();

	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve());

	let buf = bytes::BytesMut::from(data);
	// Ignore errors from incomplete/malformed trailing fragments in test files.
	let _ = fmp4.decode(&buf);

	catalog.snapshot()
}

fn run_fmp4_select(data: &[u8], select: crate::select::Broadcast) -> crate::catalog::hang::Catalog {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();

	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve()).with_select(select);

	// A dropped track's moof fragments must be skipped, not raise `UnknownTrack`.
	// (The test files end on a malformed fragment, so other decode errors are expected
	// and ignored; only `UnknownTrack` would mean the skip path regressed.)
	let buf = bytes::BytesMut::from(data);
	if let Err(err) = fmp4.decode(&buf) {
		assert!(
			!matches!(err, crate::Error::Cmaf(crate::container::fmp4::Error::UnknownTrack(_))),
			"a skipped track's fragment raised UnknownTrack: {err:?}"
		);
	}

	catalog.snapshot()
}

fn decode_init(init: &[u8]) -> (mp4_atom::Ftyp, mp4_atom::Moov) {
	let mut cursor = std::io::Cursor::new(init);
	let ftyp = mp4_atom::Ftyp::decode(&mut cursor).expect("invalid ftyp");
	let moov = mp4_atom::Moov::decode(&mut cursor).expect("invalid moov");
	(ftyp, moov)
}

fn video_config(codec: mp4_atom::Codec) -> hang::catalog::VideoConfig {
	let (ftyp, mut moov) = decode_init(include_bytes!("test_data/bbb.mp4"));
	let trak = moov
		.trak
		.iter_mut()
		.find(|trak| matches!(trak.mdia.minf.stbl.stsd.codecs.first(), Some(mp4_atom::Codec::Avc1(_))))
		.unwrap();
	trak.mdia.minf.stbl.stsd.codecs = vec![codec];
	let mut init = Vec::new();
	ftyp.encode(&mut init).unwrap();
	moov.encode(&mut init).unwrap();
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let mut import = super::Import::new(broadcast, catalog.reserve());
	import.decode(&init).unwrap();
	import.finish().unwrap();
	catalog.snapshot().video.renditions.values().next().unwrap().clone()
}

#[test]
fn sample_entries_resolve_display_aspect() {
	use crate::codec::{h264, h265};
	for (avc, hevc, expected) in [
		(h264::fixtures::SPS_IPB, h265::fixtures::SPS_NVENC, (None, None)),
		(
			h264::fixtures::SPS_ANAMORPHIC,
			h265::fixtures::SPS_ANAMORPHIC,
			(Some(16), Some(9)),
		),
	] {
		let avcc = mp4_atom::Avcc::new(avc, h264::fixtures::PPS).unwrap();
		let mut bytes = h265::build_hvcc(
			&[bytes::Bytes::from_static(h265::fixtures::VPS)],
			&[bytes::Bytes::from_static(hevc)],
			&[bytes::Bytes::from_static(h265::fixtures::PPS)],
		)
		.unwrap();
		let hvcc = mp4_atom::Hvcc::decode_body(&mut bytes).unwrap();
		let mut avc_visual = mp4_atom::Visual {
			width: 1440,
			height: 1080,
			..Default::default()
		};
		let mut hevc_visual = avc_visual.clone();
		if expected == (None, None) {
			avc_visual.width = 64;
			avc_visual.height = 64;
			hevc_visual.width = 1280;
			hevc_visual.height = 720;
		}
		for codec in [
			mp4_atom::Codec::Avc1(mp4_atom::Avc1 {
				avcc: avcc.clone(),
				visual: avc_visual.clone(),
				..Default::default()
			}),
			mp4_atom::Codec::Avc3(mp4_atom::Avc3 {
				avcc: avcc.clone(),
				visual: avc_visual.clone(),
				..Default::default()
			}),
			mp4_atom::Codec::Hvc1(mp4_atom::Hvc1 {
				hvcc: hvcc.clone(),
				visual: hevc_visual.clone(),
				..Default::default()
			}),
			mp4_atom::Codec::Hev1(mp4_atom::Hev1 {
				hvcc: hvcc.clone(),
				visual: hevc_visual.clone(),
				..Default::default()
			}),
		] {
			let config = video_config(codec);
			assert_eq!(
				(config.display_aspect_width, config.display_aspect_height),
				expected,
				"{}",
				config.codec
			);
		}
	}
}

#[test]
fn in_band_entries_without_parameter_sets_keep_visual_dimensions() {
	let mut avcc =
		mp4_atom::Avcc::new(crate::codec::h264::fixtures::SPS_IPB, crate::codec::h264::fixtures::PPS).unwrap();
	avcc.sequence_parameter_sets.clear();
	avcc.picture_parameter_sets.clear();
	let mut bytes = crate::codec::h265::fixtures::hvcc();
	let mut hvcc = mp4_atom::Hvcc::decode_body(&mut bytes).unwrap();
	hvcc.arrays.clear();
	let visual = mp4_atom::Visual {
		width: 720,
		height: 576,
		..Default::default()
	};
	for codec in [
		mp4_atom::Codec::Avc3(mp4_atom::Avc3 {
			avcc,
			visual: visual.clone(),
			..Default::default()
		}),
		mp4_atom::Codec::Hev1(mp4_atom::Hev1 {
			hvcc,
			visual,
			..Default::default()
		}),
	] {
		let config = video_config(codec);
		assert_eq!((config.coded_width, config.coded_height), (Some(720), Some(576)));
		assert_eq!(
			(config.display_aspect_width, config.display_aspect_height),
			(None, None)
		);
		assert!(
			matches!(&config.codec, hang::catalog::VideoCodec::H264(h264) if h264.inline)
				|| matches!(&config.codec, hang::catalog::VideoCodec::H265(h265) if h265.in_band)
		);
	}
}

#[test]
fn test_bbb_catalog() {
	let data = include_bytes!("test_data/bbb.mp4");
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

/// fMP4 is passthrough: it republishes the fragments its source already produced, so it exposes no
/// container selection (unlike the demuxers, which decode to elementary payloads) and every
/// rendition it publishes carries the CMAF init it parsed.
#[test]
fn every_rendition_is_cmaf() {
	let data = include_bytes!("test_data/bbb.mp4");
	let mut cursor = std::io::Cursor::new(data.as_slice());
	mp4_atom::Ftyp::decode(&mut cursor).unwrap();
	mp4_atom::Moov::decode(&mut cursor).unwrap();
	let init = &data[..cursor.position() as usize];
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let mut import = super::Import::new(broadcast, catalog.reserve());
	import.decode(init).unwrap();
	import.finish().unwrap();

	let snapshot = catalog.snapshot();
	let video = snapshot.video.renditions.values().next().expect("a video rendition");
	assert!(matches!(video.container, Container::Cmaf { .. }));
	let audio = snapshot.audio.renditions.values().next().expect("an audio rendition");
	assert!(matches!(audio.container, Container::Cmaf { .. }));
}

#[test]
fn aac_without_decoder_specific_info_is_rejected() {
	let data = include_bytes!("test_data/bbb.mp4");
	let (ftyp, mut moov) = decode_init(data);

	let dec_config = moov
		.trak
		.iter_mut()
		.find_map(|trak| match &mut trak.mdia.minf.stbl.stsd.codecs[0] {
			mp4_atom::Codec::Mp4a(mp4a) => Some(&mut mp4a.esds.es_desc.dec_config),
			_ => None,
		})
		.expect("bbb has an AAC track");
	dec_config.dec_specific = None;

	let mut init = Vec::new();
	ftyp.encode(&mut init).unwrap();
	moov.encode(&mut init).unwrap();

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve());
	let err = fmp4.decode(&init).unwrap_err();

	assert!(matches!(
		err,
		crate::Error::Cmaf(crate::container::fmp4::Error::MissingDecoderSpecific)
	));
}

#[test]
fn aac_decoder_specific_info_is_preserved() {
	let data = include_bytes!("test_data/bbb.mp4");
	let (ftyp, mut moov) = decode_init(data);
	let expected = vec![0x12, 0x10, 0x56, 0xe5, 0x00];

	let dec_specific = moov
		.trak
		.iter_mut()
		.find_map(|trak| match &mut trak.mdia.minf.stbl.stsd.codecs[0] {
			mp4_atom::Codec::Mp4a(mp4a) => mp4a.esds.es_desc.dec_config.dec_specific.as_mut(),
			_ => None,
		})
		.expect("bbb has an AAC DecoderSpecificInfo");
	dec_specific.raw = expected.clone();

	let mut init = Vec::new();
	ftyp.encode(&mut init).unwrap();
	moov.encode(&mut init).unwrap();

	let catalog = run_fmp4(&init);
	let audio = catalog.audio.renditions.values().next().expect("AAC rendition");
	assert_eq!(audio.description.as_deref(), Some(expected.as_slice()));
}

#[test]
fn dropping_import_retires_catalog_renditions() {
	let data = include_bytes!("test_data/bbb.mp4");
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();

	{
		let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve());
		let mut cursor = std::io::Cursor::new(data);
		mp4_atom::Ftyp::decode(&mut cursor).unwrap();
		mp4_atom::Moov::decode(&mut cursor).unwrap();
		fmp4.decode(&data[..cursor.position() as usize]).unwrap();
		let snapshot = catalog.snapshot();
		assert_eq!(snapshot.video.renditions.len(), 1);
		assert_eq!(snapshot.audio.renditions.len(), 1);
	}

	let snapshot = catalog.snapshot();
	assert!(snapshot.video.renditions.is_empty());
	assert!(snapshot.audio.renditions.is_empty());
}

#[test]
fn select_video_only() {
	use crate::select::{Broadcast, Video};

	let data = include_bytes!("test_data/bbb.mp4");
	let catalog = run_fmp4_select(data, Broadcast::default().video(Video::default()));

	// The muxed audio track is dropped; only video is published.
	assert_eq!(catalog.video.renditions.len(), 1);
	assert!(catalog.audio.renditions.is_empty());
}

#[test]
fn select_audio_only() {
	use crate::select::{Audio, Broadcast};

	let data = include_bytes!("test_data/bbb.mp4");
	let catalog = run_fmp4_select(data, Broadcast::default().audio(Audio::default()));

	assert!(catalog.video.renditions.is_empty());
	assert_eq!(catalog.audio.renditions.len(), 1);
}

#[test]
fn select_nothing_publishes_nothing() {
	let data = include_bytes!("test_data/bbb.mp4");
	let catalog = run_fmp4_select(data, crate::select::Broadcast::default());

	assert!(catalog.video.renditions.is_empty());
	assert!(catalog.audio.renditions.is_empty());
}

#[test]
fn test_bbb_init_roundtrip() {
	let data = include_bytes!("test_data/bbb.mp4");
	let catalog = run_fmp4(data);

	let video = catalog.video.renditions.values().next().unwrap();
	let Container::Cmaf { init, .. } = &video.container else {
		panic!("expected Cmaf container");
	};
	let (ftyp, moov) = decode_init(init);
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
	let (ftyp2, moov2) = decode_init(&buf);
	assert_eq!(ftyp2.major_brand, mp4_atom::FourCC::new(b"isom"));
	assert_eq!(moov2.trak.len(), 1);

	let audio = catalog.audio.renditions.values().next().unwrap();
	let Container::Cmaf { init, .. } = &audio.container else {
		panic!("expected Cmaf container");
	};
	let (ftyp, moov) = decode_init(init);
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
	let data = include_bytes!("test_data/av1.mp4");
	let catalog = run_fmp4(data);

	assert_eq!(catalog.video.renditions.len(), 1);
	assert_eq!(catalog.audio.renditions.len(), 0);

	let video = catalog.video.renditions.values().next().unwrap();
	assert!(video.codec.to_string().starts_with("av01."), "codec: {}", video.codec);
	assert!(matches!(video.container, Container::Cmaf { .. }));

	let Container::Cmaf { init, .. } = &video.container else {
		panic!("expected Cmaf container");
	};
	let (ftyp, moov) = decode_init(init);
	assert_eq!(ftyp.major_brand, mp4_atom::FourCC::new(b"isom"));
	assert_eq!(moov.trak.len(), 1);
	let mvex = moov.mvex.as_ref().unwrap();
	assert_eq!(mvex.trex.len(), 1);
	assert_eq!(mvex.trex[0].track_id, moov.trak[0].tkhd.track_id);
}

/// An avc3 track imports as in-band H.264. Its avcC, though it lists no parameter
/// sets, is still the description: it carries the NAL length size of the samples.
#[test]
fn avc3_imports_in_band() {
	use hang::catalog::{H264, VideoCodec, VideoConfig};
	use mp4_atom::Atom;

	// configurationVersion, profile, compatibility, level, 4-byte lengths, no SPS, no PPS.
	const AVCC: &[u8] = &[0x01, 0x42, 0xc0, 0x1f, 0xff, 0xe0, 0x00];

	let mut config = VideoConfig::new(H264 {
		profile: 0x42,
		constraints: 0xc0,
		level: 0x1f,
		inline: true,
	});
	config.coded_width = Some(320);
	config.coded_height = Some(240);
	let mut trak = super::synthesize_video_trak(1, 90_000, &config, Some(AVCC)).unwrap();
	trak.mdia.minf.stbl.stsd.codecs = vec![
		mp4_atom::Avc3 {
			visual: mp4_atom::Visual {
				data_reference_index: 1,
				width: 320,
				height: 240,
				..Default::default()
			},
			avcc: mp4_atom::Avcc::decode_body(&mut std::io::Cursor::new(AVCC)).unwrap(),
			..Default::default()
		}
		.into(),
	];
	let trex = mp4_atom::Trex {
		track_id: 1,
		default_sample_description_index: 1,
		..Default::default()
	};
	let init = super::encode_init(None, vec![trak], vec![trex]).unwrap();

	let catalog = run_fmp4(&init);
	let video = catalog.video.renditions.values().next().expect("an avc3 rendition");
	let VideoCodec::H264(h264) = &video.codec else {
		panic!("expected H.264, got {}", video.codec);
	};
	assert!(h264.inline, "avc3 carries its parameter sets in band");
	assert_eq!(video.codec.to_string(), "avc3.42c01f");
	assert_eq!(video.description.as_deref(), Some(AVCC));
	assert_eq!((video.coded_width, video.coded_height), (Some(320), Some(240)));
	assert!(matches!(video.container, Container::Cmaf { .. }));
}

#[test]
fn test_vp9_catalog() {
	let data = include_bytes!("test_data/vp9.mp4");
	let catalog = run_fmp4(data);

	assert_eq!(catalog.video.renditions.len(), 1);
	assert_eq!(catalog.audio.renditions.len(), 0);

	let video = catalog.video.renditions.values().next().unwrap();
	assert!(video.codec.to_string().starts_with("vp09."), "codec: {}", video.codec);
	assert!(matches!(video.container, Container::Cmaf { .. }));

	let Container::Cmaf { init, .. } = &video.container else {
		panic!("expected Cmaf container");
	};
	let (ftyp, moov) = decode_init(init);
	assert_eq!(ftyp.major_brand, mp4_atom::FourCC::new(b"isom"));
	assert_eq!(moov.trak.len(), 1);
	let mvex = moov.mvex.as_ref().unwrap();
	assert_eq!(mvex.trex.len(), 1);
	assert_eq!(mvex.trex[0].track_id, moov.trak[0].tkhd.track_id);
}

/// `Import::seek(n)` starts the next group at sequence `n`; subsequent fragments
/// auto-increment from there.
#[tokio::test]
async fn test_seek_sets_initial_sequence() {
	use mp4_atom::{Any, DecodeMaybe};

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let broadcast_consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve());

	let data = include_bytes!("test_data/bbb.mp4");

	// Walk the file atom-by-atom so we can seek before any fragments are processed.
	// Init atoms (ftyp/moov) come first; everything after is moof/mdat pairs.
	let mut init_buf = bytes::BytesMut::new();
	let mut frag_buf = bytes::BytesMut::new();
	let mut cursor = std::io::Cursor::new(&data[..]);
	let mut init_done = false;
	let mut position = 0;
	while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor).unwrap_or(None) {
		let end = cursor.position() as usize;
		let bytes = &data[position..end];
		match atom {
			Any::Ftyp(_) | Any::Styp(_) | Any::Moov(_) => init_buf.extend_from_slice(bytes),
			_ => {
				init_done = true;
				frag_buf.extend_from_slice(bytes);
			}
		}
		position = end;
		if init_done && frag_buf.len() > 1024 {
			break;
		}
	}

	// Decode init so the tracks exist, then seek, then decode the fragments.
	fmp4.decode(&init_buf).unwrap();

	let snap = catalog.snapshot();
	let video_name = snap.video.renditions.keys().next().expect("video track").clone();
	let mut video_track = broadcast_consumer
		.track(&video_name)
		.unwrap()
		.subscribe(None)
		.await
		.expect("video track should exist");

	fmp4.seek(100).unwrap();
	// Trailing partial fragments may error; ignore.
	let _ = fmp4.decode(&frag_buf);
	fmp4.finish().unwrap();

	let sequences = drain_group_sequences(&mut video_track);
	assert!(!sequences.is_empty(), "expected at least one group");
	assert_eq!(sequences[0], 100, "first group should land at the seeked sequence");
	for win in sequences.windows(2) {
		assert_eq!(win[1], win[0] + 1, "subsequent groups should auto-increment");
	}
}

/// E2E test: publish via the fMP4 importer, subscribe to the MSF catalog track,
/// and verify the resulting `hang::Catalog` matches what the hang catalog would
/// have produced.
///
/// `catalog::Producer` publishes both the hang (`catalog.json`) and MSF (`catalog`)
/// catalog tracks, so subscribing to the MSF one and decoding via `MsfConsumer`
/// exercises the full unified pipeline (hang -> MSF JSON on the wire -> hang).
#[tokio::test]
async fn test_msf_catalog_roundtrip() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	// Take the consumer before adding tracks; track() is called after the
	// MSF catalog track has been created by `catalog::Producer::new`.
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve());

	let data = include_bytes!("test_data/bbb.mp4");
	let buf = bytes::BytesMut::from(&data[..]);
	// Trailing fragments may error out (e.g. partial mdat); ignore.
	let _ = fmp4.decode(&buf);

	let track = consumer
		.track(moq_msf::DEFAULT_NAME)
		.unwrap()
		.subscribe(None)
		.await
		.expect("MSF catalog track should exist");
	let mut msf = crate::catalog::msf::Consumer::<()>::new(track);

	let catalog = msf
		.next()
		.await
		.expect("MSF catalog should decode")
		.expect("MSF catalog should be present");

	// Same expectations as `test_bbb_catalog`, ensuring hang -> MSF -> hang preserves
	// codec, geometry, and CMAF init data.
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

/// Subscribe to `track`'s timeline as `section` advertises it.
async fn subscribe(
	consumer: &moq_net::broadcast::Consumer,
	section: &hang::catalog::Archive,
	track: &str,
) -> crate::timeline::Consumer {
	let mut section = section.clone();
	section
		.timelines
		.entry(track.to_string())
		.or_insert_with(|| hang::catalog::Timeline::new(hang::timeline::default_name(track), 0));
	crate::timeline::Consumer::<()>::subscribe(consumer, &section, track)
		.await
		.unwrap()
}

/// Every record a finished timeline published, as `(start group, end group)`.
async fn groups(timeline: &mut crate::timeline::Consumer) -> Vec<(u64, u64)> {
	let mut out = Vec::new();
	while let Some(event) = timeline.next().await.unwrap() {
		if let crate::timeline::Event::Push { entry, .. } = event {
			assert_eq!(entry.start.frame, 0, "fragments never split a group here");
			assert_eq!(entry.end.frame, 0, "fragments never split a group here");
			out.push((entry.start.group, entry.end.group));
		}
	}
	out
}

/// Passthrough writes groups by hand (no `container::Producer`), so the timeline recorders are fed
/// directly. This guards that the import advertises both renditions' timelines at the catalog root
/// and actually records their groups, the index the VOD/playlist export reads. Regression for the
/// missing-timeline break that skipped VOD renditions.
#[tokio::test]
async fn import_populates_the_broadcast_timeline() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve());

	let data = include_bytes!("test_data/bbb.mp4");
	let buf = bytes::BytesMut::from(&data[..]);
	// Trailing partial fragments may error; ignore.
	let _ = fmp4.decode(&buf);
	fmp4.finish().unwrap();

	let snapshot = catalog.snapshot();
	assert_eq!(snapshot.video.renditions.len(), 1, "bbb has one video rendition");
	assert_eq!(snapshot.audio.renditions.len(), 1, "bbb has one audio rendition");
	let video_name = snapshot.video.renditions.keys().next().unwrap().clone();
	let audio_name = snapshot.audio.renditions.keys().next().unwrap().clone();

	// This short fixture ends before the four-second estimation record closes, so finishing the
	// import flushes it and declares the minimum while the catalog stays live.
	let section = snapshot.archive.clone().expect("the import advertises its timelines");
	assert_eq!(section.timelines[&video_name].duration, Some(4_000));
	assert_eq!(section.timelines[&audio_name].duration, Some(4_000));

	// Subscribe while the producer is alive, then finish so the timeline groups close and the
	// readers terminate rather than blocking.
	let mut video = subscribe(&consumer, &section, &video_name).await;
	let mut audio = subscribe(&consumer, &section, &audio_name).await;
	catalog.finish().unwrap();

	assert_eq!(groups(&mut video).await[0].0, 0, "the first video group is sequence 0");
	assert_eq!(groups(&mut audio).await[0].0, 0, "the first audio group is sequence 0");
}

// ---- Sample-duration handling in decode() ----

fn scale() -> moq_net::Timescale {
	moq_net::Timescale::new(1_000_000).unwrap()
}

fn info(track_id: u32, timescale: moq_net::Timescale, sequence_number: u32) -> super::FragmentInfo {
	super::FragmentInfo {
		track_id,
		timescale,
		sequence_number,
		kind: super::Kind::Video,
	}
}

fn sample(timestamp_us: u64, keyframe: bool, duration_us: Option<u64>) -> crate::container::Frame {
	crate::container::Frame {
		timestamp: moq_net::Timestamp::from_micros(timestamp_us).unwrap(),
		payload: bytes::Bytes::from_static(&[0xDE, 0xAD]),
		keyframe,
		duration: duration_us.map(|d| moq_net::Timestamp::from_micros(d).unwrap()),
	}
}

/// A multi-sample fragment whose non-final sample carries no duration can't have its
/// DTS reconstructed, so decode rejects it rather than collapsing the timestamps.
#[test]
fn decode_rejects_durationless_multisample() {
	let frames = vec![sample(0, true, None), sample(33_000, false, None)];
	let frag = super::encode_fragment(info(1, scale(), 0), &frames).unwrap();
	let err = super::decode(
		frag,
		None,
		super::Track::new(scale(), crate::container::fmp4::Kind::Video),
	)
	.unwrap_err();
	assert!(matches!(err, super::Error::MissingSampleDuration), "got {err:?}");
}

/// A single-sample fragment needs no duration (nothing follows it), so it still decodes.
#[test]
fn decode_single_sample_no_duration_ok() {
	let frag = super::encode_fragment(info(1, scale(), 0), &[sample(0, true, None)]).unwrap();
	let out = super::decode(
		frag,
		None,
		super::Track::new(scale(), crate::container::fmp4::Kind::Video),
	)
	.unwrap();
	assert_eq!(out.len(), 1);
	assert_eq!(out[0].timestamp.as_micros(), 0);
}

/// With the durations the producer now backfills, every sample's DTS round-trips
/// through a multi-sample fragment.
#[test]
fn decode_multisample_with_durations_roundtrips() {
	let frames = vec![sample(0, true, Some(33_000)), sample(33_000, false, Some(33_000))];
	let frag = super::encode_fragment(info(1, scale(), 0), &frames).unwrap();
	let out = super::decode(
		frag,
		None,
		super::Track::new(scale(), crate::container::fmp4::Kind::Video),
	)
	.unwrap();
	assert_eq!(out.len(), 2);
	assert_eq!(out[0].timestamp.as_micros(), 0);
	assert_eq!(out[1].timestamp.as_micros(), 33_000);
}

/// A FLAC track (fLaC sample entry + dfLa STREAMINFO) imports into the catalog with
/// rate/channels taken from STREAMINFO (not the 16.16 audio box) and the WebCodecs
/// description carried out of band.
#[test]
fn test_flac_catalog() {
	// 96 kHz can't be represented in the sample entry's 16.16 `Audio` rate field, so
	// this also proves STREAMINFO is the source of truth.
	let stream_info = mp4_atom::FlacMetadataBlock::StreamInfo {
		minimum_block_size: 4096,
		maximum_block_size: 4096,
		minimum_frame_size: 0u32.try_into().unwrap(),
		maximum_frame_size: 0u32.try_into().unwrap(),
		sample_rate: 96_000,
		num_channels_minus_one: 1,
		bits_per_sample_minus_one: 23,
		number_of_interchannel_samples: 0,
		md5_checksum: vec![0; 16],
	};
	let flac = mp4_atom::Flac {
		audio: mp4_atom::Audio {
			data_reference_index: 1,
			channel_count: 2,
			sample_size: 24,
			sample_rate: mp4_atom::FixedPoint::from(0u16),
		},
		dfla: mp4_atom::Dfla {
			blocks: vec![stream_info],
		},
	};

	let data = audio_init(super::build_audio_trak(1, 96_000, mp4_atom::Codec::from(flac)));

	let catalog = run_fmp4(&data);
	assert_eq!(catalog.audio.renditions.len(), 1);

	let a = catalog.audio.renditions.values().next().unwrap();
	assert!(matches!(a.codec, hang::catalog::AudioCodec::Flac));
	assert_eq!(a.sample_rate, 96_000);
	assert_eq!(a.channel_count, 2);
	// fmp4 import is CMAF passthrough.
	assert!(matches!(a.container, Container::Cmaf { .. }));
	// The WebCodecs FLAC description: `fLaC` marker + STREAMINFO.
	let desc = a.description.as_ref().expect("flac description");
	assert_eq!(&desc[..4], b"fLaC");
}

/// An init segment (ftyp + moov) holding a single audio trak with track ID 1.
fn audio_init(trak: mp4_atom::Trak) -> Vec<u8> {
	let moov = mp4_atom::Moov {
		mvhd: mp4_atom::Mvhd {
			timescale: 1000,
			..Default::default()
		},
		trak: vec![trak],
		mvex: Some(mp4_atom::Mvex {
			mehd: None,
			trex: vec![mp4_atom::Trex {
				track_id: 1,
				default_sample_description_index: 1,
				..Default::default()
			}],
		}),
		..Default::default()
	};
	let ftyp = mp4_atom::Ftyp {
		major_brand: b"isom".into(),
		minor_version: 0x200,
		compatible_brands: vec![b"isom".into(), b"iso6".into()],
	};

	let mut data = Vec::new();
	ftyp.encode(&mut data).unwrap();
	moov.encode(&mut data).unwrap();
	data
}

/// An Opus init segment whose dOps declares a 44.1 kHz input, 312 samples of pre-skip,
/// and -6 dB of gain, behind a sample entry claiming `entry_rate`.
fn opus_init(entry_rate: u16) -> (mp4_atom::Dops, Vec<u8>) {
	let dops = mp4_atom::Dops {
		output_channel_count: 2,
		pre_skip: 312,
		input_sample_rate: 44_100,
		output_gain: -1536,
	};
	let opus = mp4_atom::Opus {
		audio: mp4_atom::Audio {
			data_reference_index: 1,
			channel_count: 2,
			sample_size: 16,
			sample_rate: mp4_atom::FixedPoint::from(entry_rate),
		},
		dops: dops.clone(),
		btrt: None,
	};
	let data = audio_init(super::build_audio_trak(1, 48_000, mp4_atom::Codec::from(opus)));
	(dops, data)
}

/// dOps becomes the OpusHead description, and a track re-exported from that description
/// writes the same dOps back, so pre-skip and gain survive the round trip.
#[test]
fn opus_dops_round_trips() {
	let (dops, data) = opus_init(48_000);

	let catalog = run_fmp4(&data);
	let a = catalog.audio.renditions.values().next().expect("opus rendition");
	assert!(matches!(a.codec, hang::catalog::AudioCodec::Opus));
	assert_eq!(a.sample_rate, 48_000);
	assert_eq!(a.channel_count, 2);

	let desc = a.description.as_ref().expect("opus description");
	let head = crate::codec::opus::Config::parse(&mut desc.as_ref()).unwrap();
	assert_eq!(head.sample_rate, 44_100);
	assert_eq!(head.channel_count, 2);
	assert_eq!(head.pre_skip, 312);
	assert_eq!(head.output_gain, -1536);

	let trak = super::synthesize_audio_trak(1, 48_000, a).expect("synthesize Opus trak");
	match &trak.mdia.minf.stbl.stsd.codecs[0] {
		mp4_atom::Codec::Opus(opus) => assert_eq!(opus.dops, dops),
		other => panic!("expected Opus sample entry, got {other:?}"),
	}
}

/// The catalog describes the decoder's 48 kHz output even when the sample entry claims
/// another rate.
#[test]
fn opus_catalog_uses_the_decode_rate() {
	let (_, data) = opus_init(44_100);
	let catalog = run_fmp4(&data);
	let a = catalog.audio.renditions.values().next().expect("opus rendition");
	assert_eq!(a.sample_rate, 48_000);
	assert_eq!(a.warmup, Some(std::time::Duration::from_millis(80)));
}

/// A dOps with a channel mapping table is refused rather than imported without it.
#[test]
fn opus_dops_mapping_family_is_refused() {
	let (_, mut data) = opus_init(48_000);

	// The mapping family byte follows version, channels, pre-skip, rate, and gain.
	let at = data.windows(4).position(|w| w == b"dOps").unwrap() + 4 + 10;
	assert_eq!(data[at], 0);
	data[at] = 1;

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve());
	assert!(fmp4.decode(&bytes::BytesMut::from(data.as_slice())).is_err());
	assert!(catalog.snapshot().audio.renditions.is_empty());
}

// ---- Segment-driven grouping ----

/// bbb.mp4's init (ftyp + moov), plus the (track_id, timescale) of its video and audio traks.
/// Reused to synthesize a segmented source against a real, valid moov.
fn bbb_init() -> (bytes::BytesMut, (u32, moq_net::Timescale), (u32, moq_net::Timescale)) {
	let data = include_bytes!("test_data/bbb.mp4");
	let mut cursor = std::io::Cursor::new(&data[..]);
	mp4_atom::Ftyp::decode(&mut cursor).unwrap();
	let moov = mp4_atom::Moov::decode(&mut cursor).unwrap();
	let init = bytes::BytesMut::from(&data[..cursor.position() as usize]);

	let mut video = None;
	let mut audio = None;
	for trak in &moov.trak {
		let id = trak.tkhd.track_id;
		let scale = moq_net::Timescale::new(trak.mdia.mdhd.timescale as u64).unwrap();
		match trak.mdia.hdlr.handler.as_ref() {
			b"vide" => video = Some((id, scale)),
			b"soun" => audio = Some((id, scale)),
			_ => {}
		}
	}
	(init, video.expect("video trak"), audio.expect("audio trak"))
}

/// A `styp`, marking the start of a CMAF segment on disk.
fn styp() -> Vec<u8> {
	let mut buf = Vec::new();
	mp4_atom::Styp {
		major_brand: mp4_atom::FourCC::new(b"msdh"),
		minor_version: 0,
		compatible_brands: vec![mp4_atom::FourCC::new(b"msdh")],
	}
	.encode(&mut buf)
	.unwrap();
	buf
}

/// Drain a track, returning how many frames each group carried.
fn drain_group_frames(consumer: &mut moq_net::track::Subscriber) -> Vec<usize> {
	let mut groups = Vec::new();
	while let Some(mut group) = consumer.recv_group().now_or_never().and_then(|r| r.ok().flatten()) {
		let mut frames = 0;
		while group
			.next_frame()
			.now_or_never()
			.and_then(|r| r.ok().flatten())
			.is_some()
		{
			frames += 1;
		}
		groups.push(frames);
	}
	groups
}

/// A CMAF source that declares its segmentation with `styp` gets one group per *segment* on every
/// track, with each fragment inside it a frame.
///
/// Regression guard for audio opening a group (a QUIC stream) per fragment: audio flags every
/// sample a sync sample, so keying the group off the keyframe bit made audio's unit a fragment
/// while video's stayed a segment. The two tracks then disagreed on where segments fell, which is
/// what an HLS/DASH export needs them to agree on, and LL-HLS parts have no consistent unit at all.
#[tokio::test]
async fn segmented_source_groups_per_segment() {
	let (init, (video_id, video_scale), (audio_id, audio_scale)) = bbb_init();

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve());
	fmp4.decode(&init).unwrap();

	let snapshot = catalog.snapshot();
	let video_name = snapshot.video.renditions.keys().next().unwrap().clone();
	let audio_name = snapshot.audio.renditions.keys().next().unwrap().clone();
	let mut video_track = consumer.track(&video_name).unwrap().subscribe(replay()).await.unwrap();
	let mut audio_track = consumer.track(&audio_name).unwrap().subscribe(replay()).await.unwrap();

	// Three 1s segments: 2 video fragments each (only the first an IDR) and 4 audio fragments,
	// so a per-fragment grouping and a per-segment one can't be confused.
	for segment in 0..3u64 {
		let base = segment * 1_000_000;
		fmp4.decode(&styp()).unwrap();

		for (i, offset) in [0u64, 500_000].into_iter().enumerate() {
			let frames = [sample(base + offset, i == 0, Some(500_000))];
			let frag = super::encode_fragment(info(video_id, video_scale, segment as u32), &frames).unwrap();
			fmp4.decode(&frag).unwrap();
		}
		for offset in [0u64, 250_000, 500_000, 750_000] {
			let frames = [sample(base + offset, true, Some(250_000))];
			let frag = super::encode_fragment(info(audio_id, audio_scale, segment as u32), &frames).unwrap();
			fmp4.decode(&frag).unwrap();
		}
	}
	fmp4.finish().unwrap();

	assert_eq!(
		drain_group_frames(&mut video_track),
		vec![2, 2, 2],
		"one video group per segment, each carrying its two fragments"
	);
	assert_eq!(
		drain_group_frames(&mut audio_track),
		vec![4, 4, 4],
		"audio follows the same segmentation instead of opening a group per fragment"
	);
}

/// Each declared segment is one group per track, and each track's timeline indexes it as one
/// record. With audio grouping per fragment those records spanned many short groups, one FETCH
/// round-trip each.
#[tokio::test]
async fn segmented_source_indexes_one_group_per_record() {
	let (init, (video_id, video_scale), (audio_id, audio_scale)) = bbb_init();

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve());
	fmp4.decode(&init).unwrap();

	let snapshot = catalog.snapshot();
	let video_name = snapshot.video.renditions.keys().next().unwrap().clone();
	let audio_name = snapshot.audio.renditions.keys().next().unwrap().clone();
	let section = snapshot.archive.clone().expect("the import advertises its timelines");

	// Subscribe while the producer is alive so the readers terminate on finish rather than blocking.
	let mut video = subscribe(&consumer, &section, &video_name).await;
	let mut audio = subscribe(&consumer, &section, &audio_name).await;

	for segment in 0..3u64 {
		let base = segment * 1_000_000;
		fmp4.decode(&styp()).unwrap();
		for (i, offset) in [0u64, 500_000].into_iter().enumerate() {
			let frag = super::encode_fragment(
				info(video_id, video_scale, segment as u32),
				&[sample(base + offset, i == 0, Some(500_000))],
			)
			.unwrap();
			fmp4.decode(&frag).unwrap();
		}
		for offset in [0u64, 250_000, 500_000, 750_000] {
			let frag = super::encode_fragment(
				info(audio_id, audio_scale, segment as u32),
				&[sample(base + offset, true, Some(250_000))],
			)
			.unwrap();
			fmp4.decode(&frag).unwrap();
		}
	}
	fmp4.finish().unwrap();
	catalog.finish().unwrap();

	let expected = vec![(0, 1), (1, 2), (2, 3)];
	assert_eq!(groups(&mut video).await, expected, "one video group per record");
	assert_eq!(
		groups(&mut audio).await,
		expected,
		"one audio group per record, not one per fragment"
	);
}

/// Audio and video rarely start a segment at exactly the same timestamp: the boundary is nominal
/// and each track begins at its own nearest sample. Each track cuts its own timeline where it opens
/// its group for a declared segment, so a skew between them moves no group into another record.
///
/// `video_offset_us` shifts video relative to audio, so the same source is exercised with audio
/// leading and lagging. Returns each track's records as `(start group, end group)`.
async fn records_with_skew(video_offset_us: i64) -> (Vec<(u64, u64)>, Vec<(u64, u64)>) {
	let (init, (video_id, video_scale), (audio_id, audio_scale)) = bbb_init();

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve());
	fmp4.decode(&init).unwrap();

	let snapshot = catalog.snapshot();
	let video_name = snapshot.video.renditions.keys().next().unwrap().clone();
	let audio_name = snapshot.audio.renditions.keys().next().unwrap().clone();
	let section = snapshot.archive.clone().unwrap();
	let mut video = subscribe(&consumer, &section, &video_name).await;
	let mut audio = subscribe(&consumer, &section, &audio_name).await;

	for segment in 0..3u64 {
		let base = segment * 1_000_000;
		let video_base = (base as i64 + video_offset_us).max(0) as u64;
		fmp4.decode(&styp()).unwrap();

		// Audio first, so the segment transition is detected on the audio fragment and video's
		// IDR lands after it. Two fragments each keeps the records unambiguous.
		for offset in [0u64, 500_000] {
			let frag = super::encode_fragment(
				info(audio_id, audio_scale, segment as u32),
				&[sample(base + offset, true, Some(500_000))],
			)
			.unwrap();
			fmp4.decode(&frag).unwrap();
		}
		for (i, offset) in [0u64, 500_000].into_iter().enumerate() {
			let frag = super::encode_fragment(
				info(video_id, video_scale, segment as u32),
				&[sample(video_base + offset, i == 0, Some(500_000))],
			)
			.unwrap();
			fmp4.decode(&frag).unwrap();
		}
	}
	fmp4.finish().unwrap();
	catalog.finish().unwrap();

	(groups(&mut video).await, groups(&mut audio).await)
}

/// Video's IDR lands 20ms AFTER audio's first sample of the segment, the normal interleave.
#[tokio::test]
async fn audio_leading_video_still_indexes_one_group_each() {
	let expected = vec![(0, 1), (1, 2), (2, 3)];
	assert_eq!(records_with_skew(20_000).await, (expected.clone(), expected));
}

/// Video's IDR lands 20ms BEFORE audio's first sample, the other side of the same skew.
#[tokio::test]
async fn audio_lagging_video_still_indexes_one_group_each() {
	let expected = vec![(0, 1), (1, 2), (2, 3)];
	assert_eq!(records_with_skew(-20_000).await, (expected.clone(), expected));
}

/// A source that emits one leading `styp` and then never another still segments on its video
/// keyframes, rather than collapsing the whole stream into a single group.
///
/// Trusting a `styp` to keep arriving cost exactly that: one group per track holding every
/// fragment, which strands late subscribers, never leaves the cache, and gives the timeline no
/// second boundary. A keyframe is always a legal segment start, so it is the honest fallback.
/// Sub-segment IDRs may therefore split a declared segment into several groups, which is
/// harmless: a timeline range spans as many groups as the segment needs.
#[tokio::test]
async fn a_single_leading_styp_still_segments_on_keyframes() {
	let (init, (video_id, video_scale), (audio_id, audio_scale)) = bbb_init();

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve());
	fmp4.decode(&init).unwrap();

	let snapshot = catalog.snapshot();
	let video_name = snapshot.video.renditions.keys().next().unwrap().clone();
	let audio_name = snapshot.audio.renditions.keys().next().unwrap().clone();
	let mut video_track = consumer.track(&video_name).unwrap().subscribe(replay()).await.unwrap();
	let mut audio_track = consumer.track(&audio_name).unwrap().subscribe(replay()).await.unwrap();

	// One styp up front, then four segments' worth of fragments each opening on an IDR.
	fmp4.decode(&styp()).unwrap();
	for segment in 0..4u64 {
		let base = segment * 1_000_000;
		let frag = super::encode_fragment(
			info(video_id, video_scale, segment as u32),
			&[sample(base, true, Some(1_000_000))],
		)
		.unwrap();
		fmp4.decode(&frag).unwrap();
		let frag = super::encode_fragment(
			info(audio_id, audio_scale, segment as u32),
			&[sample(base, true, Some(1_000_000))],
		)
		.unwrap();
		fmp4.decode(&frag).unwrap();
	}
	fmp4.finish().unwrap();

	assert_eq!(
		drain_group_frames(&mut video_track),
		vec![1, 1, 1, 1],
		"each keyframe fragment opens its own segment"
	);
	assert_eq!(
		drain_group_frames(&mut audio_track),
		vec![1, 1, 1, 1],
		"audio follows the same boundaries rather than trailing a segment behind"
	);
}

/// One audio fragment for bbb's track 2, carrying a single sample at the track's 44100 timescale.
///
/// `trun.data_offset` is left unset so the samples start at the front of the mdat, which is all
/// the importer needs to slice them back out.
fn audio_fragment(base_media_decode_time: u64, duration: u32, size: u32) -> Vec<u8> {
	let moof = mp4_atom::Moof {
		mfhd: mp4_atom::Mfhd { sequence_number: 0 },
		traf: vec![mp4_atom::Traf {
			tfhd: mp4_atom::Tfhd {
				track_id: 2,
				default_sample_duration: Some(duration),
				default_sample_size: Some(size),
				..Default::default()
			},
			tfdt: Some(mp4_atom::Tfdt { base_media_decode_time }),
			trun: vec![mp4_atom::Trun {
				data_offset: None,
				entries: vec![mp4_atom::TrunEntry::default()],
			}],
			..Default::default()
		}],
	};

	let mut buf = Vec::new();
	moof.encode(&mut buf).unwrap();
	mp4_atom::Mdat {
		data: vec![0xAA; size as usize],
	}
	.encode(&mut buf)
	.unwrap();
	buf
}

/// Every fragment restates its decode time, so a stale one puts two different samples on the same
/// timestamp, which a decoder reads as an undeclared hole and resets on. ffmpeg writes exactly that
/// when `frag_every_frame` interleaves audio and video, so refuse the fragment rather than publish
/// the collision.
#[test]
fn non_advancing_fragment_decode_time_is_rejected() {
	let data = include_bytes!("test_data/bbb.mp4");
	let (ftyp, moov) = decode_init(data);
	let mut init = Vec::new();
	ftyp.encode(&mut init).unwrap();
	moov.encode(&mut init).unwrap();

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve());
	fmp4.decode(&init).unwrap();

	// An advancing pair imports, one AAC frame apart.
	fmp4.decode(&audio_fragment(3072, 1024, 327)).unwrap();
	fmp4.decode(&audio_fragment(4096, 1024, 361)).unwrap();

	// A third fragment repeating the previous decode time is a different sample landing on the
	// same timestamp.
	let err = fmp4.decode(&audio_fragment(4096, 1024, 339)).unwrap_err();
	assert!(
		matches!(
			err,
			crate::Error::Cmaf(crate::container::fmp4::Error::NonMonotonicDecodeTime {
				track: 2,
				decode_time,
				previous,
			}) if decode_time == moq_net::Timestamp::from_scale(4096, 44100).unwrap()
				&& previous == decode_time
		),
		"expected a non-monotonic decode time, got {err:?}"
	);
}

#[test]
fn rejected_fragment_preserves_decode_time() {
	let (ftyp, moov) = decode_init(include_bytes!("test_data/bbb.mp4"));
	let mut init = Vec::new();
	ftyp.encode(&mut init).unwrap();
	moov.encode(&mut init).unwrap();
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve());
	fmp4.decode(&init).unwrap();
	fmp4.decode(&audio_fragment(4096, 1024, 327)).unwrap();
	assert!(fmp4.decode(&audio_fragment(3000, 1024, 327)).is_err());
	assert!(matches!(
		fmp4.decode(&audio_fragment(3500, 1024, 327)),
		Err(crate::Error::Cmaf(
			crate::container::fmp4::Error::NonMonotonicDecodeTime { decode_time, previous, .. }
		)) if previous == moq_net::Timestamp::from_scale(4096, 44100).unwrap()
			&& decode_time == moq_net::Timestamp::from_scale(3500, 44100).unwrap()
	));
}

/// Like [`audio_fragment`] but carrying `samples` samples, the shape a real segmenter emits.
fn audio_fragment_samples(base_media_decode_time: u64, duration: u32, size: u32, samples: usize) -> Vec<u8> {
	let moof = mp4_atom::Moof {
		mfhd: mp4_atom::Mfhd { sequence_number: 0 },
		traf: vec![mp4_atom::Traf {
			tfhd: mp4_atom::Tfhd {
				track_id: 2,
				default_sample_duration: Some(duration),
				default_sample_size: Some(size),
				..Default::default()
			},
			tfdt: Some(mp4_atom::Tfdt { base_media_decode_time }),
			trun: vec![mp4_atom::Trun {
				data_offset: None,
				entries: vec![mp4_atom::TrunEntry::default(); samples],
			}],
			..Default::default()
		}],
	};

	let mut buf = Vec::new();
	moof.encode(&mut buf).unwrap();
	mp4_atom::Mdat {
		data: vec![0xAA; size as usize * samples],
	}
	.encode(&mut buf)
	.unwrap();
	buf
}

/// Each fragment advertises its media span immediately. A source whose
/// fragments shrink (or whose last fragment holds a single sample) must not walk the advertised
/// value back down: the publisher can emit a long fragment again at any point.
#[test]
fn fragment_jitter_never_shrinks() {
	let (ftyp, moov) = decode_init(include_bytes!("test_data/bbb.mp4"));
	let mut init = Vec::new();
	ftyp.encode(&mut init).unwrap();
	moov.encode(&mut init).unwrap();

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve());
	fmp4.decode(&init).unwrap();

	let audio_jitter = |catalog: &crate::catalog::Producer| {
		catalog
			.snapshot()
			.audio
			.renditions
			.values()
			.next()
			.expect("an audio rendition")
			.jitter
	};

	// bbb's AAC track runs at 44100; eight 1024-sample frames span 8192 ticks (~186 ms).
	fmp4.decode(&audio_fragment_samples(0, 1024, 327, 8)).unwrap();
	assert!(
		audio_jitter(&catalog).is_some(),
		"the first fragment must publish its own span"
	);
	fmp4.decode(&audio_fragment_samples(8192, 1024, 327, 8)).unwrap();
	let cadence = audio_jitter(&catalog).expect("the fragment cadence is published");
	assert!(
		cadence >= std::time::Duration::from_millis(180) && cadence <= std::time::Duration::from_millis(190),
		"two fragments establish the cadence: {cadence:?}"
	);

	// Single-sample fragments 23 ms apart. Publishing that spacing would tell a consumer to size
	// its buffer for one frame when the next fragment is 186 ms again.
	fmp4.decode(&audio_fragment_samples(16384, 1024, 327, 1)).unwrap();
	fmp4.decode(&audio_fragment_samples(17408, 1024, 327, 1)).unwrap();
	assert_eq!(
		audio_jitter(&catalog),
		Some(cadence),
		"a tighter fragment pair lowered the jitter"
	);

	fmp4.finish().unwrap();
	assert_eq!(audio_jitter(&catalog), Some(cadence));
}

#[test]
fn fragment_jitter_uses_sample_endpoints() {
	for (samples, expected) in [
		(vec![(Some(882), 0), (Some(4410), 0)], 120),
		(vec![(Some(441), 0), (Some(4410), -441)], 100),
		(vec![(Some(882), 0), (None, 0)], 40),
	] {
		let (ftyp, mut moov) = decode_init(include_bytes!("test_data/bbb.mp4"));
		for trex in &mut moov.mvex.as_mut().unwrap().trex {
			trex.default_sample_duration = 0;
		}
		let mut init = Vec::new();
		ftyp.encode(&mut init).unwrap();
		moov.encode(&mut init).unwrap();
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut import = crate::container::fmp4::Import::new(broadcast, catalog.reserve());
		import.decode(&init).unwrap();
		let moof = mp4_atom::Moof {
			mfhd: mp4_atom::Mfhd { sequence_number: 0 },
			traf: vec![mp4_atom::Traf {
				tfhd: mp4_atom::Tfhd {
					track_id: 2,
					default_sample_size: Some(8),
					..Default::default()
				},
				tfdt: Some(mp4_atom::Tfdt {
					base_media_decode_time: 0,
				}),
				trun: vec![mp4_atom::Trun {
					data_offset: None,
					entries: samples
						.iter()
						.map(|(duration, cts)| mp4_atom::TrunEntry {
							duration: *duration,
							cts: Some(*cts),
							..Default::default()
						})
						.collect(),
				}],
				..Default::default()
			}],
		};
		let mut fragment = Vec::new();
		moof.encode(&mut fragment).unwrap();
		mp4_atom::Mdat {
			data: vec![0xAA; samples.len() * 8],
		}
		.encode(&mut fragment)
		.unwrap();
		import.decode(&fragment).unwrap();
		let snapshot = catalog.snapshot();
		let jitter = snapshot.audio.renditions.values().next().unwrap().jitter.unwrap();
		assert_eq!(jitter.as_nanos().div_ceil(1_000_000), expected);
	}
}

/// One encoder session of bbb-shaped fragments: `frames` 100ms video fragments from `start_us`,
/// a keyframe each second, interleaved with audio fragments up to 300ms earlier in PTS.
fn live_session(start_us: u64, frames: u64) -> Vec<u8> {
	let (_, (video_id, video_scale), (audio_id, audio_scale)) = bbb_init();
	let lead = start_us.min(300_000);
	let mut out = Vec::new();
	for j in 0..frames {
		let pts = start_us + j * 100_000;
		let video = sample(pts, j % 10 == 0, Some(100_000));
		out.extend_from_slice(&super::encode_fragment(info(video_id, video_scale, j as u32), &[video]).unwrap());
		let audio = sample(pts - lead, true, Some(100_000));
		out.extend_from_slice(&super::encode_fragment(info(audio_id, audio_scale, j as u32), &[audio]).unwrap());
	}
	out
}

/// What an fMP4 import published, and the wall-clock window its first chunk arrived in.
struct Imported {
	published: std::collections::BTreeMap<String, Vec<u128>>,
	video: String,
	/// The root clock the catalog advertised.
	clock: hang::catalog::Clock,
	arrival: std::ops::RangeInclusive<std::time::SystemTime>,
	/// The first record the video timeline published, the index an archive replays from.
	record: moq_net::Timestamp,
	/// Why a chunk was refused, if one was.
	refused: Option<crate::Error>,
}

/// Import bbb's init then `chunks` on a catalog with the default clock, stopping at the first
/// refused chunk.
async fn import_session(chunks: &[Vec<u8>]) -> Imported {
	let (init, _, _) = bbb_init();
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve());
	fmp4.decode(&init).unwrap();

	let snapshot = catalog.snapshot();
	let video = snapshot.video.renditions.keys().next().unwrap().clone();
	let section = snapshot.archive.clone().expect("the import advertises its timelines");
	let mut timeline = subscribe(&consumer, &section, &video).await;

	let before = std::time::SystemTime::now();
	let mut after = before;
	let mut refused = None;
	for (i, chunk) in chunks.iter().enumerate() {
		if let Err(err) = fmp4.decode(chunk) {
			refused = Some(err);
			break;
		}
		if i == 0 {
			after = std::time::SystemTime::now();
		}
	}
	fmp4.finish().unwrap();
	catalog.finish().unwrap();

	let event = timeline.next().await.unwrap().expect("a recorded span");
	let crate::timeline::Event::Push { entry, .. } = event else {
		panic!("the first timeline event was not a record");
	};

	let snapshot = catalog.snapshot();
	Imported {
		published: crate::container::test_util::published(&consumer, &snapshot).await,
		video,
		clock: snapshot.clock.expect("the catalog advertises a clock"),
		arrival: before..=after,
		record: entry.pts,
		refused,
	}
}

/// A feed an hour into its own decode timeline publishes that hour verbatim (the `tfdt` a
/// decoder reads is untouched), and the catalog clock maps its first fragment to the arrival
/// time. The archive index records the same times, so a replay names each segment's wall time.
#[tokio::test]
async fn import_publishes_decode_times_on_an_arrival_clock() {
	let start = 3_600_000_000;
	let import = import_session(&[live_session(start, 20)]).await;
	assert!(import.refused.is_none());

	let first = import.published[&import.video][0];
	assert_eq!(first, start as u128, "the source's own decode time");
	assert_eq!(
		import.record.as_micros(),
		first,
		"the archive indexes the source's time"
	);

	// The advertised clock is micros, so allow its rounding.
	let tick = std::time::Duration::from_micros(1);
	let wall = import.clock.wall_clock(import.record).unwrap();
	assert!(
		*import.arrival.start() - tick <= wall && wall <= *import.arrival.end() + tick,
		"the first fragment is live on arrival"
	);
}

/// The most a shifted timestamp rounds down, in microseconds: under one unit of bbb's track scales.
const TICK: u128 = 1_000;

/// What fixes the catalog's clock before an fMP4 import's first fragment.
#[derive(Clone, Copy, Debug)]
enum Earlier {
	/// A data track, whose registration publishes the catalog.
	Data,
	/// A capture, which takes the clock and stamps on its own copy.
	Capture,
}

/// Something stamps on the catalog clock before an fMP4 import whose decode times start at
/// `start_us`. The import offsets onto that clock instead of moving it: its first fragment lands
/// at now on the earlier source's timeline, which keeps advancing with no rewind.
async fn join_after(earlier: Earlier, start_us: u64) {
	let (init, _, _) = bbb_init();
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();

	let mut data = match earlier {
		Earlier::Data => {
			let track = broadcast
				.create_track(
					"telemetry",
					moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI),
				)
				.unwrap();
			let mut stream = catalog.binary_stream(track, crate::binary::Config::default()).unwrap();
			stream
				.append(moq_net::Timed::from(&b"before"[..]).at(catalog.clock().now()))
				.unwrap();
			Some(stream)
		}
		Earlier::Capture => None,
	};
	let mut stamps = data.as_ref().map(|stream| stream.consume());
	let clock = catalog.clock();
	let fixed = catalog.snapshot().clock;

	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve());
	fmp4.decode(&init).unwrap();
	let before = clock.now().as_micros();
	fmp4.decode(&live_session(start_us, 20)).unwrap();
	let after = clock.now().as_micros();
	if let Some(stream) = data.as_mut() {
		stream
			.append(moq_net::Timed::from(&b"after"[..]).at(catalog.clock().now()))
			.unwrap();
	}
	fmp4.finish().unwrap();

	let snapshot = catalog.snapshot();
	assert_eq!(snapshot.clock, fixed, "{earlier:?}: the import never moves the clock");
	let published = crate::container::test_util::published(&consumer, &snapshot).await;
	let video = &published[snapshot.video.renditions.keys().next().unwrap()];
	// The offset rounds down to the track's own units.
	assert!(
		(before - TICK..=after).contains(&video[0]),
		"{earlier:?}: the first fragment lands at now: {before} {} {after}",
		video[0]
	);
	assert!(
		video.windows(2).all(|w| (w[1] - w[0]).abs_diff(100_000) <= 1),
		"the fragments keep their spacing: {video:?}"
	);

	if let Some(stamps) = stamps.as_mut() {
		let waiter = kio::Waiter::noop();
		let mut times = Vec::new();
		while let std::task::Poll::Ready(Ok(Some(mut group))) = stamps.poll_recv_group(&waiter) {
			while let std::task::Poll::Ready(Ok(Some(frame))) = group.poll_read_frame(&waiter) {
				times.push(frame.timestamp.unwrap().as_micros());
			}
		}
		assert_eq!(times.len(), 2);
		assert!(
			times[0] <= times[1],
			"{earlier:?}: the data track never rewinds: {times:?}"
		);
	}
}

/// A data track registered first fixes the clock; a feed an hour in still lands at now.
#[tokio::test]
async fn an_import_joins_a_data_tracks_clock() {
	join_after(Earlier::Data, 3_600_000_000).await;
}

/// A capture that took the clock first keeps it; a feed at zero lands on its timeline rather than
/// stepping it back ten seconds.
#[tokio::test]
async fn an_import_joins_a_captures_clock() {
	join_after(Earlier::Capture, 0).await;
}

/// A `with_clock` catalog is fixed from the start, so even the first import shifts onto it.
#[tokio::test]
async fn an_import_shifts_onto_an_explicit_clock() {
	let (init, _, _) = bbb_init();
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let clock = crate::Clock::new();
	let config = crate::catalog::Config::default().with_clock(clock);
	let catalog = crate::catalog::Producer::new(&mut broadcast, config).unwrap();

	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve());
	fmp4.decode(&init).unwrap();
	let before = clock.now().as_micros();
	fmp4.decode(&live_session(3_600_000_000, 5)).unwrap();
	let after = clock.now().as_micros();
	fmp4.finish().unwrap();

	let snapshot = catalog.snapshot();
	assert_eq!(snapshot.clock, clock.wall());
	let published = crate::container::test_util::published(&consumer, &snapshot).await;
	let first = published[snapshot.video.renditions.keys().next().unwrap()][0];
	assert!((before - TICK..=after).contains(&first), "{before} {first} {after}");
}

/// A source whose decode times restart is a new epoch: the import refuses the rewind rather than
/// re-anchoring it forward, keeping everything published before it.
#[tokio::test]
async fn import_refuses_a_restart() {
	let import = import_session(&[live_session(5_000_000, 20), live_session(0, 20)]).await;
	let err = import.refused.expect("the restart is refused");
	assert!(
		matches!(
			err,
			crate::Error::Cmaf(crate::container::fmp4::Error::NonMonotonicDecodeTime { .. })
		),
		"{err:?}"
	);
	assert_eq!(
		import.published[&import.video].len(),
		20,
		"the first session stays published"
	);
}

/// Skipping sequences (an HLS media-sequence gap) is not a new timeline: a rewound decode time
/// after a seek is still refused. Only a signalled discontinuity starts the comparison afresh.
#[tokio::test]
async fn seek_keeps_the_decode_time_until_a_discontinuity() {
	let (init, _, _) = bbb_init();
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve());
	fmp4.decode(&init).unwrap();
	fmp4.decode(&live_session(5_000_000, 5)).unwrap();

	fmp4.seek(100).unwrap();
	let err = fmp4.decode(&live_session(0, 5)).unwrap_err();
	assert!(
		matches!(
			err,
			crate::Error::Cmaf(crate::container::fmp4::Error::NonMonotonicDecodeTime { .. })
		),
		"{err:?}"
	);

	fmp4.discontinuity();
	fmp4.seek(200).unwrap();
	fmp4.decode(&live_session(0, 5))
		.expect("a signalled discontinuity starts a new timeline");
}

/// The catalog is first published at the first fragment, carrying the clock that fragment
/// anchors, rather than at the moov on a provisional clock a copy-once reader would keep.
#[tokio::test]
async fn first_catalog_carries_the_anchored_clock() {
	let (init, _, _) = bbb_init();
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let provisional = catalog.snapshot().clock.expect("a clock");
	let mut clocks = crate::container::test_util::Clocks::subscribe(&consumer).await;
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve());

	fmp4.decode(&init).unwrap();
	assert_eq!(clocks.drain(), vec![], "the moov alone publishes nothing");

	fmp4.decode(&live_session(3_600_000_000, 20)).unwrap();
	let anchored = catalog.snapshot().clock.expect("a clock");
	assert_ne!(anchored, provisional, "the first fragment anchors the clock");
	let published = clocks.drain();
	assert!(!published.is_empty(), "the first fragment publishes the catalog");
	assert!(published.iter().all(|clock| *clock == Some(anchored)), "{published:?}");

	fmp4.finish().unwrap();
	assert!(clocks.drain().iter().all(|clock| *clock == Some(anchored)));
}

/// A second moov is refused before the first fragment too, while the reservation is still held.
#[test]
fn a_second_moov_is_refused() {
	let (init, _, _) = bbb_init();
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve());

	fmp4.decode(&init).unwrap();
	let err = fmp4.decode(&init).unwrap_err();
	assert!(
		matches!(err, crate::Error::Cmaf(crate::container::fmp4::Error::DuplicateMoov)),
		"{err:?}"
	);
}

/// A moov with every track deselected releases its reservation at once, since no fragment will
/// ever anchor. It declares no track, so the catalog stays unpublished until one is listed.
#[tokio::test]
async fn a_moov_with_nothing_selected_releases_its_reservation() {
	let (init, _, _) = bbb_init();
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let mut clocks = crate::container::test_util::Clocks::subscribe(&consumer).await;
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve())
		.with_select(crate::select::Broadcast::default());

	fmp4.decode(&init).unwrap();
	catalog.timebase().anchor(moq_net::Timestamp::ZERO).unwrap();
	assert!(clocks.drain().is_empty(), "nothing servable is published");

	catalog
		.mutate(|catalog| catalog.m2ts = Some(hang::catalog::M2ts::new("input.m2ts")))
		.unwrap();
	assert_eq!(clocks.drain().len(), 1, "no reservation still gates the catalog");
}

/// A moov after `finish()` is refused: its tracks would be declared but never finished.
#[test]
fn a_moov_after_finish_is_refused() {
	let (init, _, _) = bbb_init();
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve());

	fmp4.finish().unwrap();
	let err = fmp4.decode(&init).unwrap_err();
	assert!(
		matches!(err, crate::Error::Cmaf(crate::container::fmp4::Error::MoovAfterFinish)),
		"{err:?}"
	);
	let snapshot = catalog.snapshot();
	assert!(snapshot.video.renditions.is_empty() && snapshot.audio.renditions.is_empty());
}

/// The catalog waits for a selected track's fragment: a deselected track's fragment is skipped,
/// so it doesn't release the reservation even when it arrives first.
#[tokio::test]
async fn a_deselected_fragment_does_not_publish() {
	use crate::select::{Broadcast, Video};

	let (init, (video_id, video_scale), (audio_id, audio_scale)) = bbb_init();
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let mut clocks = crate::container::test_util::Clocks::subscribe(&consumer).await;
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve())
		.with_select(Broadcast::default().video(Video::default()));
	fmp4.decode(&init).unwrap();

	let audio = super::encode_fragment(info(audio_id, audio_scale, 0), &[sample(0, true, Some(100_000))]).unwrap();
	fmp4.decode(&audio).unwrap();
	assert_eq!(clocks.drain(), vec![], "a deselected fragment publishes nothing");

	let video = super::encode_fragment(info(video_id, video_scale, 0), &[sample(0, true, Some(100_000))]).unwrap();
	fmp4.decode(&video).unwrap();
	assert_eq!(clocks.drain().len(), 1, "the selected track's first fragment publishes");
}

/// The importer resolves sample flags the way every CMAF reader does, so a fragment opening on a
/// sync sample rolls a group even when its flags are left to `tfhd` or `trex`, or say nothing.
/// The fragments it passes through then decode against its catalog init to the same samples.
#[tokio::test]
async fn imports_the_sample_defaults_fixtures() {
	use mp4_atom::DecodeMaybe;

	for fixture in super::fixtures() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve());
		fmp4.decode(&fixture.init).unwrap();

		let snapshot = catalog.snapshot();
		let (name, video) = snapshot.video.renditions.iter().next().unwrap();
		let Container::Cmaf { init } = &video.container else {
			panic!("{}: a CMAF rendition", fixture.name);
		};
		let wire = super::Wire::from_init(init).unwrap();
		let mut track = consumer.track(name).unwrap().subscribe(replay()).await.unwrap();

		// The same fragment again, one second later, so its first sample can open a second group.
		let mut cursor = std::io::Cursor::new(fixture.fragment.as_slice());
		let Some(mp4_atom::Any::Moof(mut moof)) = mp4_atom::Any::decode_maybe(&mut cursor).unwrap() else {
			panic!("{}: a moof first", fixture.name);
		};
		moof.traf[0].tfdt.as_mut().unwrap().base_media_decode_time += 1_000;
		let mut later = Vec::new();
		moof.encode(&mut later).unwrap();
		later.extend_from_slice(&fixture.fragment[cursor.position() as usize..]);

		fmp4.decode(&fixture.fragment).unwrap();
		fmp4.decode(&later).unwrap();
		fmp4.finish().unwrap();

		let mut first = track.recv_group().await.unwrap().unwrap();
		let frame = first.read_frame().await.unwrap().unwrap();
		let frames = super::decode(frame.payload, frame.timestamp, wire.track().unwrap()).unwrap();
		let samples: Vec<super::FixtureSample> = frames.iter().map(super::FixtureSample::from).collect();
		assert_eq!(samples, fixture.samples, "{}", fixture.name);

		assert!(
			track.recv_group().await.unwrap().is_some(),
			"{}: the second fragment opens on a sync sample, so a group",
			fixture.name
		);
	}
}

#[test]
fn an_unknown_subtitle_track_does_not_refuse_supported_media() {
	use mp4_atom::{DecodeMaybe, Encode};
	let mut cursor = std::io::Cursor::new(include_bytes!("test_data/bbb.mp4"));
	let mut input = Vec::new();
	while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor).unwrap() {
		if let mp4_atom::Any::Moov(mut moov) = atom {
			let mut subtitle = moov.trak[0].clone();
			subtitle.tkhd.track_id = 99;
			subtitle.mdia.hdlr.handler = b"sbtl".into();
			moov.trak.push(subtitle);
			moov.encode(&mut input).unwrap();
			break;
		} else {
			atom.encode(&mut input).unwrap();
		}
	}
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let mut importer = super::Import::new(broadcast, catalog.reserve());
	importer.decode(&input).unwrap();
	importer.finish().unwrap();
	assert!(!catalog.snapshot().video.renditions.is_empty());
	assert!(catalog.snapshot().text.renditions.is_empty());
}

fn webvtt_init(header: &str) -> Vec<u8> {
	let (_, moov) = decode_init(include_bytes!("test_data/bbb.mp4"));
	let mut trak = moov.trak[0].clone();
	trak.tkhd.track_id = 1;
	trak.mdia.mdhd.timescale = 1000;
	trak.mdia.mdhd.language = "eng".into();
	trak.mdia.hdlr.handler = b"text".into();
	trak.mdia.hdlr.name = "English captions".into();
	trak.mdia.minf.stbl.stsd.codecs = vec![mp4_atom::Codec::Wvtt(mp4_atom::Wvtt {
		plaintext: mp4_atom::PlainText {
			data_reference_index: 1,
		},
		config: mp4_atom::VttC { config: header.into() },
		label: Some(mp4_atom::Vlab {
			source_label: "urn:example:source".into(),
		}),
		btrt: None,
	})];
	audio_init(trak)
}

/// One fragment carrying `samples` back to back, each lasting `duration` ms from `decode_time`.
fn webvtt_fragment(decode_time: u64, samples: &[Vec<u8>], duration: u32) -> Vec<u8> {
	let moof = mp4_atom::Moof {
		mfhd: mp4_atom::Mfhd { sequence_number: 1 },
		traf: vec![mp4_atom::Traf {
			tfhd: mp4_atom::Tfhd {
				track_id: 1,
				default_sample_duration: Some(duration),
				..Default::default()
			},
			tfdt: Some(mp4_atom::Tfdt {
				base_media_decode_time: decode_time,
			}),
			trun: vec![mp4_atom::Trun {
				data_offset: None,
				entries: samples
					.iter()
					.map(|sample| mp4_atom::TrunEntry {
						size: Some(sample.len() as u32),
						..Default::default()
					})
					.collect(),
			}],
			..Default::default()
		}],
	};
	let mut output = Vec::new();
	moof.encode(&mut output).unwrap();
	mp4_atom::Mdat { data: samples.concat() }.encode(&mut output).unwrap();
	output
}

fn webvtt_input(payload: Vec<u8>, duration: u32) -> Vec<u8> {
	let mut input = webvtt_init("WEBVTT");
	input.extend(webvtt_fragment(250, &[payload], duration));
	input
}

fn subtitle_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
	let mut bytes = Vec::new();
	bytes.extend_from_slice(&((8 + payload.len()) as u32).to_be_bytes());
	bytes.extend_from_slice(kind);
	bytes.extend_from_slice(payload);
	bytes
}

#[tokio::test(start_paused = true)]
async fn webvtt_samples_become_timed_text_with_source_metadata() {
	let mut cue = subtitle_box(b"iden", b"cue-1");
	cue.extend(subtitle_box(b"sttg", b"align:start"));
	cue.extend(subtitle_box(b"payl", b"Hello"));
	let payload = subtitle_box(b"vttc", &cue);
	let input = webvtt_input(payload, 1250);
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let mut importer = super::Import::new(broadcast, catalog.reserve());
	importer.decode(&input).unwrap();
	importer.finish().unwrap();
	let snapshot = catalog.snapshot();
	assert_eq!(snapshot.text.renditions.len(), 1);
	let (name, config) = snapshot.text.renditions.iter().next().unwrap();
	assert_eq!(config.lang.as_deref(), Some("eng"));
	assert_eq!(
		config.label.as_deref(),
		Some("English captions"),
		"hdlr name, not the vlab source"
	);
	assert_eq!(config.role, hang::catalog::TextRole::Subtitle);
	let track = consumer
		.track(name)
		.unwrap()
		.subscribe(moq_net::track::Subscription::default())
		.await
		.unwrap();
	let mut reader = crate::container::Consumer::new(
		track,
		crate::catalog::hang::Container::Legacy(crate::container::Kind::Data),
	);
	let frame = reader.read_frame().await.unwrap().unwrap();
	assert_eq!(frame.timestamp.as_micros(), 250_000);
	assert_eq!(
		&frame.payload[..],
		b"WEBVTT\n\ncue-1\n00:00:00.250 --> 00:00:01.500 align:start\nHello\n\n"
	);
}

#[test]
fn malformed_webvtt_samples_are_refused() {
	let payl = subtitle_box(b"payl", b"hello");
	let mut duplicate = payl.clone();
	duplicate.extend_from_slice(&payl);
	for (payload, duration) in [
		(Vec::new(), 1250),
		(vec![0, 0, 0, 4], 1250),
		(subtitle_box(b"vttc", &[]), 1250),
		(subtitle_box(b"vttc", &duplicate), 1250),
		(subtitle_box(b"vttc", &subtitle_box(b"payl", b"\xff")), 1250),
		(subtitle_box(b"vttc", &payl), 0),
		// Inline timestamps need `ctim` to anchor them.
		(subtitle_box(b"vttc", &subtitle_box(b"payl", b"a <00:01.000>b")), 1250),
	] {
		let input = webvtt_input(payload, duration);
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let mut importer = super::Import::new(broadcast, catalog.reserve());
		assert!(importer.decode(&input).is_err());
	}
}

#[test]
fn empty_and_multiple_webvtt_cues_are_supported() {
	let cue = subtitle_box(b"vttc", &subtitle_box(b"payl", b"hello"));
	let mut multiple = cue.clone();
	multiple.extend(cue);
	for payload in [subtitle_box(b"vtte", &[]), multiple] {
		let input = webvtt_input(payload, 1250);
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let mut importer = super::Import::new(broadcast, catalog.reserve());
		importer.decode(&input).unwrap();
		importer.finish().unwrap();
		assert_eq!(catalog.snapshot().text.renditions.len(), 1);
	}
}

/// The WebVTT `Kind` header sets the role, and lone CR line terminators are accepted.
#[tokio::test(start_paused = true)]
async fn webvtt_kind_and_lone_cr_terminators() {
	let mut input = webvtt_init("WEBVTT\rKind: captions\r");
	input.extend(webvtt_fragment(
		250,
		&[subtitle_box(b"vttc", &subtitle_box(b"payl", b"Hello\rworld"))],
		1250,
	));
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let mut importer = super::Import::new(broadcast, catalog.reserve());
	importer.decode(&input).unwrap();
	importer.finish().unwrap();
	let snapshot = catalog.snapshot();
	let (name, config) = snapshot.text.renditions.iter().next().unwrap();
	assert_eq!(config.role, hang::catalog::TextRole::Caption);
	let track = consumer.track(name).unwrap().subscribe(None).await.unwrap();
	let mut reader = crate::container::Consumer::new(
		track,
		crate::catalog::hang::Container::Legacy(crate::container::Kind::Data),
	);
	let frame = reader.read_frame().await.unwrap().unwrap();
	assert_eq!(
		&frame.payload[..],
		b"WEBVTT\nKind: captions\n\n00:00:00.250 --> 00:00:01.500\nHello\nworld\n\n"
	);
}

/// Text closes a group per cue, so a segment seek must not rewind onto a cue group already written.
#[test]
fn webvtt_seek_after_several_cues_does_not_reuse_a_group() {
	let cue = subtitle_box(b"vttc", &subtitle_box(b"payl", b"hello"));
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let mut importer = super::Import::new(broadcast, catalog.reserve());
	importer.decode(&webvtt_init("WEBVTT")).unwrap();
	importer.seek(100).unwrap();
	importer
		.decode(&webvtt_fragment(0, &[cue.clone(), cue.clone(), cue.clone()], 1000))
		.unwrap();
	// Segment 101 was missed; groups 100..=102 already hold the three cues.
	importer.seek(102).unwrap();
	importer.decode(&webvtt_fragment(4000, &[cue], 1000)).unwrap();
	importer.finish().unwrap();
}

/// A text fragment may not start inside the decode span of the previous fragment's samples.
#[test]
fn webvtt_fragment_overlapping_previous_samples_is_refused() {
	let cue = subtitle_box(b"vttc", &subtitle_box(b"payl", b"hello"));
	for (start, valid) in [(1500, false), (2000, true)] {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let mut importer = super::Import::new(broadcast, catalog.reserve());
		importer.decode(&webvtt_init("WEBVTT")).unwrap();
		importer
			.decode(&webvtt_fragment(0, &[cue.clone(), cue.clone()], 1000))
			.unwrap();
		let result = importer.decode(&webvtt_fragment(start, std::slice::from_ref(&cue), 1000));
		assert_eq!(result.is_ok(), valid, "start {start}: {result:?}");
	}
}

#[test]
#[tracing_test::traced_test]
fn emsg_metadata_drops_warn_once() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let mut import = super::Import::new(broadcast, catalog.reserve());
	let event = mp4_atom::Emsg {
		timescale: 1000,
		presentation_time: mp4_atom::EmsgTimestamp::Absolute(0),
		event_duration: 1000,
		id: 1,
		scheme_id_uri: "urn:test:event".into(),
		value: "cue".into(),
		message_data: b"metadata".to_vec(),
	};
	let mut data = Vec::new();
	event.encode(&mut data).unwrap();
	event.encode(&mut data).unwrap();
	let fixture = include_bytes!("test_data/bbb.mp4");
	let mut cursor = std::io::Cursor::new(fixture.as_slice());
	mp4_atom::Ftyp::decode(&mut cursor).unwrap();
	mp4_atom::Moov::decode(&mut cursor).unwrap();
	data.extend_from_slice(&fixture[..cursor.position() as usize]);
	data.extend_from_slice(&audio_fragment(0, 1024, 327));
	import.decode(&data).unwrap();
	import.finish().unwrap();
	assert_eq!(import.dropped_metadata(), 2);
	assert_eq!(catalog.snapshot().video.renditions.len(), 1);
	assert_eq!(catalog.snapshot().audio.renditions.len(), 1);
	logs_assert(|lines| {
		let warnings = lines
			.iter()
			.filter(|line| line.contains("discarding fMP4 emsg metadata"))
			.count();
		if warnings == 1 {
			Ok(())
		} else {
			Err(format!("expected one metadata warning, got {warnings}"))
		}
	});
}

/// A cue shorter than a millisecond keeps a positive WebVTT duration.
#[tokio::test(start_paused = true)]
async fn webvtt_sub_millisecond_cue_stays_positive() {
	// A 1 MHz track, so the cue can last 400 us.
	let (ftyp, mut moov) = decode_init(&webvtt_init("WEBVTT"));
	moov.trak[0].mdia.mdhd.timescale = 1_000_000;
	let mut init = Vec::new();
	ftyp.encode(&mut init).unwrap();
	moov.encode(&mut init).unwrap();
	init.extend(webvtt_fragment(
		250_200,
		&[subtitle_box(b"vttc", &subtitle_box(b"payl", b"hi"))],
		400,
	));
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let mut importer = super::Import::new(broadcast, catalog.reserve());
	importer.decode(&init).unwrap();
	importer.finish().unwrap();
	let snapshot = catalog.snapshot();
	let name = snapshot.text.renditions.keys().next().unwrap();
	let track = consumer.track(name).unwrap().subscribe(None).await.unwrap();
	let mut reader = crate::container::Consumer::new(
		track,
		crate::catalog::hang::Container::Legacy(crate::container::Kind::Data),
	);
	let frame = reader.read_frame().await.unwrap().unwrap();
	assert_eq!(&frame.payload[..], b"WEBVTT\n\n00:00:00.250 --> 00:00:00.251\nhi\n\n");
}

async fn imported_webvtt(payload: Vec<u8>) -> bytes::Bytes {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let mut importer = super::Import::new(broadcast, catalog.reserve());
	importer.decode(&webvtt_input(payload, 1250)).unwrap();
	importer.finish().unwrap();
	let snapshot = catalog.snapshot();
	let name = snapshot.text.renditions.keys().next().unwrap();
	let track = consumer.track(name).unwrap().subscribe(None).await.unwrap();
	let mut reader = crate::container::Consumer::new(
		track,
		crate::catalog::hang::Container::Legacy(crate::container::Kind::Data),
	);
	reader.read_frame().await.unwrap().unwrap().payload
}

#[tokio::test(start_paused = true)]
async fn webvtt_ctim_rebases_inline_timestamps_to_the_sample_clock() {
	let mut cue = subtitle_box(b"ctim", b"00:00:10.000");
	cue.extend(subtitle_box(b"payl", b"Hello <00:00:10.500>world"));
	assert_eq!(
		&imported_webvtt(subtitle_box(b"vttc", &cue)).await[..],
		b"WEBVTT\n\n00:00:00.250 --> 00:00:01.500\nHello <00:00:00.750>world\n\n"
	);
}

#[tokio::test(start_paused = true)]
async fn webvtt_vtta_preserves_comments_in_sample_order() {
	let mut sample = subtitle_box(b"vtta", b"NOTE before");
	sample.extend(subtitle_box(b"vttc", &subtitle_box(b"payl", b"first")));
	sample.extend(subtitle_box(b"vtta", b"NOTE between"));
	sample.extend(subtitle_box(b"vttc", &subtitle_box(b"payl", b"second")));
	assert_eq!(
		&imported_webvtt(sample).await[..],
		b"WEBVTT\n\nNOTE before\n\n00:00:00.250 --> 00:00:01.500\nfirst\n\nNOTE between\n\n00:00:00.250 --> 00:00:01.500\nsecond\n\n"
	);
}

#[test]
fn malformed_webvtt_origins_and_comments_are_refused() {
	let mut duplicate = subtitle_box(b"ctim", b"00:00:10.000");
	duplicate.extend(subtitle_box(b"ctim", b"00:00:10.000"));
	duplicate.extend(subtitle_box(b"payl", b"text"));
	let mut cases = vec![
		subtitle_box(b"vttc", &duplicate),
		subtitle_box(b"vtta", b"not a comment"),
		subtitle_box(b"vtta", b"NOTE invalid\0comment"),
		subtitle_box(b"vtta", b"NOTE 00:00.000 --> 00:01.000"),
		subtitle_box(b"vtta", b"\xff"),
	];
	for origin in [b"bad".as_slice(), b"00:60.000", b"\xff"] {
		let mut cue = subtitle_box(b"ctim", origin);
		cue.extend(subtitle_box(b"payl", b"text"));
		cases.push(subtitle_box(b"vttc", &cue));
	}
	for body in [
		b"before <00:00:09.999>origin".as_slice(),
		b"at <00:00:10.000>origin",
		b"at <00:00:11.250>end",
		b"after <00:00:11.251>end",
	] {
		let mut cue = subtitle_box(b"ctim", b"00:00:10.000");
		cue.extend(subtitle_box(b"payl", body));
		cases.push(subtitle_box(b"vttc", &cue));
	}
	for payload in cases {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let mut importer = super::Import::new(broadcast, catalog.reserve());
		assert!(importer.decode(&webvtt_input(payload, 1250)).is_err());
	}
}

/// An `stpp` init segment. mp4-atom cannot encode an `stpp` entry, so encode a `metx` entry, whose
/// strings line up with `stpp`'s namespace, schema location and auxiliary MIME types, and rename it.
fn ttml_init(namespace: &str, mime: &str, timescale: u32) -> Vec<u8> {
	let (_, moov) = decode_init(&webvtt_init("WEBVTT"));
	let mut trak = moov.trak[0].clone();
	trak.mdia.hdlr.handler = b"subt".into();
	trak.mdia.mdhd.timescale = timescale;
	trak.mdia.minf.stbl.stsd.codecs = vec![mp4_atom::Codec::Metx(mp4_atom::Metx {
		metadata: mp4_atom::MetaData {
			data_reference_index: 1,
		},
		content_encoding: namespace.into(),
		namespace: String::new(),
		schema_location: mime.into(),
		btrt: None,
	})];
	let mut init = audio_init(trak);
	let at = init.windows(4).position(|kind| kind == b"metx").unwrap();
	init[at..at + 4].copy_from_slice(b"stpp");
	init
}

const TTML: &str = r#"<tt xmlns="http://www.w3.org/ns/ttml" xml:lang="en"><body><div begin="00:01:41"><p begin="0.5s" end="2s">Hello</p></div></body></tt>"#;

/// Import one TTML sample lasting 4 s from source time 100 s, returning its rendition and frame.
async fn imported_ttml(
	timebase: crate::catalog::Timebase,
	broadcast: moq_net::broadcast::Producer,
	catalog: &crate::catalog::Producer,
	consumer: &moq_net::broadcast::Consumer,
) -> (hang::catalog::TextConfig, crate::container::Frame) {
	let mut input = ttml_init("http://www.w3.org/ns/ttml", "", 1000);
	input.extend(webvtt_fragment(100_000, &[TTML.as_bytes().to_vec()], 4000));
	let mut importer = super::Import::new(broadcast, timebase.reserve());
	importer.decode(&input).unwrap();
	importer.finish().unwrap();
	let snapshot = catalog.snapshot();
	let (name, config) = snapshot.text.renditions.iter().next().unwrap();
	let track = consumer.track(name).unwrap().subscribe(None).await.unwrap();
	let mut reader = crate::container::Consumer::new(
		track,
		crate::catalog::hang::Container::Legacy(crate::container::Kind::Data),
	);
	(config.clone(), reader.read_frame().await.unwrap().unwrap())
}

#[tokio::test(start_paused = true)]
async fn ttml_samples_become_timed_text_with_source_metadata() {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let (config, frame) = imported_ttml(catalog.timebase(), broadcast, &catalog, &consumer).await;
	assert_eq!(config.format, hang::catalog::TextFormat::Ttml);
	assert_eq!(config.lang.as_deref(), Some("eng"));
	assert_eq!(config.label.as_deref(), Some("English captions"));
	assert_eq!(frame.timestamp.as_micros(), 100_000_000);
	assert_eq!(
		&frame.payload[..],
		br#"<tt xmlns="http://www.w3.org/ns/ttml" xml:lang="en"><body begin="00:01:40" end="00:01:44"><div begin="00:00:01"><p begin="0.5s" end="2s">Hello</p></div></body></tt>"#
	);
}

/// The source joins a catalog clock another input already fixed, so its TTML times move with it.
#[tokio::test(start_paused = true)]
async fn ttml_follows_a_shifted_source_clock() {
	let wall = std::time::SystemTime::now();
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	catalog
		.timebase()
		.place(moq_net::Timestamp::from_secs(10).unwrap(), wall)
		.unwrap();
	let timebase = catalog.timebase();
	timebase
		.place(moq_net::Timestamp::from_secs(100).unwrap(), wall)
		.unwrap();
	let (_, frame) = imported_ttml(timebase, broadcast, &catalog, &consumer).await;
	assert_eq!(frame.timestamp.as_micros(), 10_000_000);
	assert_eq!(
		&frame.payload[..],
		br#"<tt xmlns="http://www.w3.org/ns/ttml" xml:lang="en"><body begin="00:00:10" end="00:00:14"><div begin="00:00:01"><p begin="0.5s" end="2s">Hello</p></div></body></tt>"#
	);
}

/// An `stpp` entry naming another XML namespace, or carrying image subsamples, skips only its track.
#[test]
fn unsupported_stpp_tracks_are_skipped() {
	for (namespace, mime) in [("urn:example:not-ttml", ""), ("http://www.w3.org/ns/ttml", "image/png")] {
		let mut input = ttml_init(namespace, mime, 1000);
		input.extend(webvtt_fragment(0, &[TTML.as_bytes().to_vec()], 1000));
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let mut importer = super::Import::new(broadcast, catalog.reserve());
		importer.decode(&input).unwrap();
		importer.finish().unwrap();
		assert!(catalog.snapshot().text.renditions.is_empty(), "{namespace} {mime}");
	}
}

/// A document the profile refuses fails the import instead of publishing as TTML.
#[test]
fn ttml_documents_outside_the_profile_are_refused() {
	for document in [
		r#"<tt xmlns="urn:example:not-ttml"/>"#,
		r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttp="http://www.w3.org/ns/ttml#parameter" ttp:timeBase="smpte"/>"#,
	] {
		let mut input = ttml_init("http://www.w3.org/ns/ttml", "", 1000);
		input.extend(webvtt_fragment(0, &[document.as_bytes().to_vec()], 1000));
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let mut importer = super::Import::new(broadcast, catalog.reserve());
		assert!(importer.decode(&input).is_err(), "{document}");
	}
}

/// A 90 kHz sample start is not a whole microsecond; the body still begins at the serialized
/// frame timestamp.
#[tokio::test(start_paused = true)]
async fn ttml_body_begins_at_the_serialized_frame_timestamp() {
	let mut input = ttml_init("http://www.w3.org/ns/ttml", "", 90_000);
	input.extend(webvtt_fragment(9_000_001, &[TTML.as_bytes().to_vec()], 360_000));

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let mut importer = super::Import::new(broadcast, catalog.reserve());
	importer.decode(&input).unwrap();
	importer.finish().unwrap();
	let snapshot = catalog.snapshot();
	let name = snapshot.text.renditions.keys().next().unwrap();
	let track = consumer.track(name).unwrap().subscribe(None).await.unwrap();
	let mut reader = crate::container::Consumer::new(
		track,
		crate::catalog::hang::Container::Legacy(crate::container::Kind::Data),
	);
	let frame = reader.read_frame().await.unwrap().unwrap();
	assert_eq!(frame.timestamp.as_micros(), 100_000_011);
	let payload = std::str::from_utf8(&frame.payload).unwrap();
	assert!(
		payload.contains(r#"<body begin="00:01:40.000011" end="00:01:44.000011">"#),
		"{payload}"
	);
}
