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
	run_fmp4_with(data, false)
}

/// Like [`run_fmp4`], republishing each fragment as CMAF.
fn run_fmp4_passthrough(data: &[u8]) -> crate::catalog::hang::Catalog {
	run_fmp4_with(data, true)
}

fn run_fmp4_with(data: &[u8], passthrough: bool) -> crate::catalog::hang::Catalog {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();

	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve()).with_passthrough(passthrough);

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
	video_config_result(codec).unwrap()
}

fn video_config_result(codec: mp4_atom::Codec) -> crate::Result<hang::catalog::VideoConfig> {
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
	import.decode(&init)?;
	import.finish()?;
	Ok(catalog.snapshot().video.renditions.values().next().unwrap().clone())
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

// Exercise every visual sample-entry path, including initializers with no in-band headers yet.
fn aspect_entries(anamorphic: bool) -> Vec<mp4_atom::Codec> {
	use crate::codec::{h264, h265};
	let avcc = mp4_atom::Avcc::new(
		if anamorphic {
			h264::fixtures::SPS_ANAMORPHIC
		} else {
			h264::fixtures::SPS_IPB
		},
		h264::fixtures::PPS,
	)
	.unwrap();
	let mut bytes = h265::build_hvcc(
		&[bytes::Bytes::from_static(h265::fixtures::VPS)],
		&[bytes::Bytes::from_static(if anamorphic {
			h265::fixtures::SPS_ANAMORPHIC
		} else {
			h265::fixtures::SPS_NVENC
		})],
		&[bytes::Bytes::from_static(h265::fixtures::PPS)],
	)
	.unwrap();
	let hvcc = mp4_atom::Hvcc::decode_body(&mut bytes).unwrap();
	let visual = mp4_atom::Visual {
		width: 1440,
		height: 1080,
		..Default::default()
	};
	let mut codecs = vec![
		mp4_atom::Codec::Avc1(mp4_atom::Avc1 {
			avcc: avcc.clone(),
			visual: visual.clone(),
			..Default::default()
		}),
		mp4_atom::Codec::Hvc1(mp4_atom::Hvc1 {
			hvcc: hvcc.clone(),
			visual: visual.clone(),
			..Default::default()
		}),
	];
	if !anamorphic {
		let mut avcc = avcc;
		avcc.sequence_parameter_sets.clear();
		avcc.picture_parameter_sets.clear();
		let mut hvcc = hvcc;
		hvcc.arrays.clear();
		codecs.extend([
			mp4_atom::Codec::Avc3(mp4_atom::Avc3 {
				avcc,
				visual: visual.clone(),
				..Default::default()
			}),
			mp4_atom::Codec::Hev1(mp4_atom::Hev1 {
				hvcc,
				visual: visual.clone(),
				..Default::default()
			}),
			mp4_atom::Codec::Av01(mp4_atom::Av01 {
				visual: visual.clone(),
				..Default::default()
			}),
			mp4_atom::Codec::Vp08(mp4_atom::Vp08 {
				visual: visual.clone(),
				..Default::default()
			}),
			mp4_atom::Codec::Vp09(mp4_atom::Vp09 {
				visual,
				..Default::default()
			}),
		]);
	}
	codecs
}

fn with_pasp(mut codec: mp4_atom::Codec, horizontal: u32, vertical: u32) -> mp4_atom::Codec {
	let pasp = Some(mp4_atom::Pasp {
		h_spacing: horizontal,
		v_spacing: vertical,
	});
	match &mut codec {
		mp4_atom::Codec::Avc1(c) => c.pasp = pasp,
		mp4_atom::Codec::Avc3(c) => c.pasp = pasp,
		mp4_atom::Codec::Hvc1(c) => c.pasp = pasp,
		mp4_atom::Codec::Hev1(c) => c.pasp = pasp,
		mp4_atom::Codec::Av01(c) => c.pasp = pasp,
		mp4_atom::Codec::Vp08(c) => c.pasp = pasp,
		mp4_atom::Codec::Vp09(c) => c.pasp = pasp,
		_ => unreachable!(),
	}
	codec
}

#[test]
fn container_aspect_pasp_fills_missing_sps_aspect() {
	for codec in aspect_entries(false) {
		for (spacing, expected) in [
			((4, 3), (Some(16), Some(9))),
			((1, 1), (None, None)),
			((3, 4), (Some(1), Some(1))),
		] {
			let config = video_config(with_pasp(codec.clone(), spacing.0, spacing.1));
			assert_eq!(
				(config.display_aspect_width, config.display_aspect_height),
				expected,
				"{} {spacing:?}",
				config.codec
			);
		}
	}
}

#[test]
#[tracing_test::traced_test]
fn container_aspect_pasp_conflict_keeps_sps() {
	for codec in aspect_entries(true) {
		let config = video_config(with_pasp(codec, 1, 1));
		assert_eq!(
			(config.display_aspect_width, config.display_aspect_height),
			(Some(16), Some(9))
		);
	}
	logs_assert(|lines| {
		let conflicts = lines
			.iter()
			.filter(|line| line.contains("display aspect disagrees") && line.contains("pasp"))
			.count();
		if conflicts == 2 {
			Ok(())
		} else {
			Err(format!("expected two SPS conflicts, got {conflicts}"))
		}
	});
}

#[test]
#[tracing_test::traced_test]
fn container_aspect_equivalent_pasp_does_not_warn() {
	for codec in aspect_entries(true) {
		let config = video_config(with_pasp(codec, 8, 6));
		assert_eq!(
			(config.display_aspect_width, config.display_aspect_height),
			(Some(16), Some(9))
		);
	}
	assert!(!logs_contain("display aspect disagrees"));
}

#[test]
fn container_aspect_pasp_rejects_invalid_ratios() {
	for (horizontal, vertical) in [(0, 1), (1, 0), (u32::MAX, 1)] {
		let codec = mp4_atom::Codec::Av01(mp4_atom::Av01 {
			visual: mp4_atom::Visual {
				width: 1440,
				height: 1080,
				..Default::default()
			},
			..Default::default()
		});
		assert!(matches!(
			video_config_result(with_pasp(codec, horizontal, vertical)),
			Err(crate::Error::Cmaf(super::Error::InvalidAspect(_)))
		));
	}
	// Large intermediate dimensions are valid when the reduced ratio fits the catalog.
	let codec = aspect_entries(false).remove(0);
	let config = video_config(with_pasp(codec, u32::MAX - 3, (u32::MAX - 3) / 4 * 3));
	assert_eq!(
		(config.display_aspect_width, config.display_aspect_height),
		(Some(16), Some(9))
	);
}

fn with_spatial(mut codec: mp4_atom::Codec, spatial: mp4_atom::Spatial) -> mp4_atom::Codec {
	let spatial = Some(Box::new(spatial));
	match &mut codec {
		mp4_atom::Codec::Avc1(c) => c.spatial = spatial,
		mp4_atom::Codec::Avc3(c) => c.spatial = spatial,
		mp4_atom::Codec::Hvc1(c) => c.spatial = spatial,
		mp4_atom::Codec::Hev1(c) => c.spatial = spatial,
		mp4_atom::Codec::Av01(c) => c.spatial = spatial,
		mp4_atom::Codec::Vp08(c) => c.spatial = spatial,
		mp4_atom::Codec::Vp09(c) => c.spatial = spatial,
		_ => unreachable!(),
	}
	codec
}

/// Spherical Video V2 boxes: an `st3d` with `stereo_mode`, and an `sv3d` projecting with `kind`.
fn spherical(stereo_mode: Option<u8>, kind: Option<mp4_atom::ProjKind>) -> mp4_atom::Spatial {
	mp4_atom::Spatial {
		st3d: stereo_mode.map(|stereo_mode| mp4_atom::St3d { stereo_mode }),
		sv3d: kind.map(|kind| mp4_atom::Sv3d {
			svhd: Some(mp4_atom::Svhd {
				metadata_source: "moq".into(),
			}),
			proj: Some(mp4_atom::Proj {
				prhd: Some(mp4_atom::Prhd::default()),
				kind: Some(kind),
				unknown: vec![],
			}),
			unknown: vec![],
		}),
		..Default::default()
	}
}

fn equi(top: u32, bottom: u32, left: u32, right: u32) -> Option<mp4_atom::ProjKind> {
	Some(mp4_atom::ProjKind::Equi(mp4_atom::Equi {
		projection_bounds_top: top,
		projection_bounds_bottom: bottom,
		projection_bounds_left: left,
		projection_bounds_right: right,
	}))
}

#[test]
fn spherical_boxes_fill_projection_and_stereo() {
	use hang::catalog::{Projection, Stereo};
	let quarter = 1 << 30;
	for (spatial, projection, stereo) in [
		// Monoscopic 360.
		(
			spherical(Some(mp4_atom::St3d::MONO), equi(0, 0, 0, 0)),
			Some(Projection::Equirectangular { fov: None }),
			None,
		),
		// VR180: half the longitude, side by side.
		(
			spherical(Some(mp4_atom::St3d::LEFT_RIGHT), equi(0, 0, quarter, quarter)),
			Some(Projection::Equirectangular { fov: Some(180.0) }),
			Some(Stereo::LeftRight),
		),
		// Flat stereo, with no sv3d.
		(
			spherical(Some(mp4_atom::St3d::TOP_BOTTOM), None),
			None,
			Some(Stereo::TopBottom),
		),
	] {
		for codec in aspect_entries(false) {
			let config = video_config(with_spatial(codec, spatial.clone()));
			assert_eq!(
				(&config.projection, &config.stereo),
				(&projection, &stereo),
				"{}",
				config.codec
			);
		}
	}
	// Without the boxes, the video stays flat and mono.
	let config = video_config(aspect_entries(false).remove(0));
	assert_eq!((config.projection, config.stereo), (None, None));
}

#[test]
fn spherical_boxes_the_catalog_cannot_describe_are_refused() {
	let quarter = 1 << 30;
	let posed = |pose: mp4_atom::Prhd| {
		let mut spatial = spherical(None, equi(0, 0, 0, 0));
		spatial.sv3d.as_mut().unwrap().proj.as_mut().unwrap().prhd = Some(pose);
		spatial
	};
	let mut repeated = spherical(Some(mp4_atom::St3d::MONO), None);
	let mut st3d = Vec::new();
	mp4_atom::St3d {
		stereo_mode: mp4_atom::St3d::LEFT_RIGHT,
	}
	.encode(&mut st3d)
	.unwrap();
	// The body after the 8-byte header; mp4-atom keeps the second st3d it decodes as unknown.
	repeated.unknown = vec![mp4_atom::Any::Unknown(mp4_atom::St3d::KIND, st3d[8..].to_vec())];
	let mut unprojected = spherical(None, equi(0, 0, 0, 0));
	unprojected.sv3d.as_mut().unwrap().proj = None;

	for (name, spatial) in [
		(
			"cubemap",
			spherical(None, Some(mp4_atom::ProjKind::Cbmp(mp4_atom::Cbmp::default()))),
		),
		(
			"mesh VR180",
			spherical(
				Some(mp4_atom::St3d::LEFT_RIGHT),
				Some(mp4_atom::ProjKind::Mshp(mp4_atom::Mshp {
					crc: 0,
					encoding_four_cc: b"raw ".into(),
					data: vec![],
				})),
			),
		),
		("asymmetric bounds", spherical(None, equi(0, 0, quarter, 0))),
		("vertical crop", spherical(None, equi(quarter, quarter, 0, 0))),
		("no picture left", spherical(None, equi(0, 0, 1 << 31, 1 << 31))),
		(
			"yawed",
			posed(mp4_atom::Prhd {
				pose_yaw_degrees: 90 << 16,
				..Default::default()
			}),
		),
		(
			"rolled",
			posed(mp4_atom::Prhd {
				pose_roll_degrees: 1,
				..Default::default()
			}),
		),
		("right-left", spherical(Some(mp4_atom::St3d::RIGHT_LEFT), None)),
		("custom stereo", spherical(Some(mp4_atom::St3d::STEREO_CUSTOM), None)),
		("unknown stereo", spherical(Some(5), None)),
		("repeated st3d", repeated),
		("sv3d without proj", unprojected),
	] {
		for codec in aspect_entries(false) {
			let err = video_config_result(with_spatial(codec, spatial.clone())).unwrap_err();
			assert!(
				matches!(
					err,
					crate::Error::Cmaf(crate::container::fmp4::Error::UnsupportedSpatial(_))
				),
				"{name}: {err:?}"
			);
		}
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
	assert!(matches!(video.container, Container::Legacy));

	let audio = catalog.audio.renditions.values().next().unwrap();
	assert_eq!(audio.codec.to_string(), "mp4a.40.2");
	assert_eq!(audio.sample_rate, 44100);
	assert_eq!(audio.channel_count, 2);
	assert!(matches!(audio.container, Container::Legacy));
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

/// Every sample bbb's fragments carry, per role, as a consumer reads it back: its presentation
/// time in microseconds, whether it opens a group, and its bytes.
async fn bbb_samples(passthrough: bool) -> [Vec<(u128, bool, bytes::Bytes)>; 2] {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve()).with_passthrough(passthrough);
	// The file ends on a malformed fragment, after every whole one has published.
	let _ = fmp4.decode(include_bytes!("test_data/bbb.mp4"));
	fmp4.finish().unwrap();
	published(&consumer, &catalog).await
}

/// Every sample the broadcast published, per role (video, audio), as a consumer reads it back.
async fn published(
	consumer: &moq_net::broadcast::Consumer,
	catalog: &crate::catalog::Producer,
) -> [Vec<(u128, bool, bytes::Bytes)>; 2] {
	let snapshot = catalog.snapshot();
	let video = snapshot.video.renditions.iter().next().unwrap();
	let audio = snapshot.audio.renditions.iter().next().unwrap();
	let containers = [
		(video.0, crate::catalog::hang::Container::try_from(video.1).unwrap()),
		(audio.0, crate::catalog::hang::Container::try_from(audio.1).unwrap()),
	];

	let mut out = [Vec::new(), Vec::new()];
	for ((name, container), samples) in containers.into_iter().zip(&mut out) {
		let track = consumer.track(name).unwrap().subscribe(replay()).await.unwrap();
		let mut reader = crate::container::Consumer::new(track, container);
		while let Some(frame) = reader.read_frame().await.unwrap() {
			samples.push((frame.timestamp.as_micros(), frame.keyframe, frame.payload));
		}
	}
	out
}

/// Unwrapping publishes exactly the samples a consumer would have decoded out of the passthrough
/// fragments: the same bytes at the same presentation times, opening groups at the same samples.
#[tokio::test]
async fn unwrapped_samples_match_the_passthrough_fragments() {
	let [video, audio] = bbb_samples(false).await;
	let [cmaf_video, cmaf_audio] = bbb_samples(true).await;
	assert!(!video.is_empty() && !audio.is_empty(), "bbb carries both roles");
	assert_eq!(video, cmaf_video, "video");
	assert_eq!(audio, cmaf_audio, "audio");
}

/// A fragment whose runs are separated by padding and by another track's run publishes each
/// sample's own bytes at its own time, passthrough or unwrapped, whether a run's offset is explicit
/// or continues the run before it, and whether its sizes come from the table or the tfhd defaults.
#[tokio::test]
async fn separated_runs_keep_their_samples() {
	let (init, (video_id, video_scale), (audio_id, audio_scale)) = bbb_init();
	let video_tick = video_scale.as_u64() / 24;
	let non_sync = 0x0001_0000;

	// The mdat: video V1 V2, a gap, audio A1 A2, a gap, video V3, then V4 V5 continuing it.
	let layout: [(&str, &[u8]); 6] = [
		("v", &[0xA1; 3]),
		("v", &[0xA2; 3]),
		("gap", &[0xEE; 5]),
		("a", &[0xB1, 0xB1, 0xB2, 0xB2, 0xB2, 0xB2]),
		("gap", &[0xEF; 2]),
		("v", &[0xA3, 0xA3, 0xA3, 0xA4, 0xA4, 0xA4, 0xA5, 0xA5, 0xA5]),
	];
	let at = |index: usize| layout[..index].iter().map(|(_, bytes)| bytes.len()).sum::<usize>();

	// Video sizes and durations come from the tfhd defaults; audio's from its table.
	let video_entry = |flags| mp4_atom::TrunEntry {
		flags: Some(flags),
		..Default::default()
	};
	let audio_entry = |size| mp4_atom::TrunEntry {
		duration: Some(1024),
		size: Some(size),
		..Default::default()
	};
	let mut moof = mp4_atom::Moof {
		mfhd: mp4_atom::Mfhd { sequence_number: 1 },
		traf: vec![
			mp4_atom::Traf {
				tfhd: mp4_atom::Tfhd {
					track_id: video_id,
					default_sample_duration: Some(video_tick as u32),
					default_sample_size: Some(3),
					..Default::default()
				},
				tfdt: Some(mp4_atom::Tfdt {
					base_media_decode_time: video_scale.as_u64(),
				}),
				trun: vec![
					mp4_atom::Trun {
						data_offset: Some(0),
						samples: vec![video_entry(0), video_entry(non_sync)].into(),
					},
					mp4_atom::Trun {
						data_offset: Some(0),
						samples: vec![video_entry(non_sync)].into(),
					},
					// No data_offset: it starts where the run before it ended.
					mp4_atom::Trun {
						data_offset: None,
						samples: vec![video_entry(non_sync), video_entry(non_sync)].into(),
					},
				],
				..Default::default()
			},
			mp4_atom::Traf {
				tfhd: mp4_atom::Tfhd {
					track_id: audio_id,
					..Default::default()
				},
				tfdt: Some(mp4_atom::Tfdt {
					base_media_decode_time: audio_scale.as_u64(),
				}),
				trun: vec![mp4_atom::Trun {
					data_offset: Some(0),
					samples: vec![audio_entry(2), audio_entry(4)].into(),
				}],
				..Default::default()
			},
		],
	};
	// Offsets count from the moof's first byte, so measure it with every offset present.
	let mut buf = Vec::new();
	moof.encode(&mut buf).unwrap();
	let padding = mp4_atom::Any::Unknown(mp4_atom::FourCC::new(b"free"), vec![0; 16]);
	let mut padding_bytes = Vec::new();
	padding.encode(&mut padding_bytes).unwrap();
	let data = (buf.len() + padding_bytes.len() + 8) as i32;
	moof.traf[0].trun[0].data_offset = Some(data + at(0) as i32);
	moof.traf[0].trun[1].data_offset = Some(data + at(5) as i32);
	moof.traf[1].trun[0].data_offset = Some(data + at(3) as i32);
	let mut fragment = Vec::new();
	moof.encode(&mut fragment).unwrap();
	fragment.extend_from_slice(&padding_bytes);
	mp4_atom::Mdat {
		data: layout.iter().flat_map(|(_, bytes)| bytes.iter().copied()).collect(),
	}
	.encode(&mut fragment)
	.unwrap();

	let micros = |ticks: u64, scale: moq_net::Timescale| u128::from(ticks) * 1_000_000 / u128::from(scale.as_u64());
	let video: Vec<_> = (0..5u64)
		.map(|i| {
			let timestamp = micros(video_scale.as_u64() + i * video_tick, video_scale);
			(timestamp, i == 0, bytes::Bytes::from(vec![0xA1 + i as u8; 3]))
		})
		.collect();
	// Audio marks only the frame that opens its group.
	let audio = vec![
		(1_000_000, true, bytes::Bytes::from_static(&[0xB1; 2])),
		(
			micros(audio_scale.as_u64() + 1024, audio_scale),
			false,
			bytes::Bytes::from_static(&[0xB2; 4]),
		),
	];

	for passthrough in [false, true] {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve()).with_passthrough(passthrough);
		fmp4.decode(&init).unwrap();
		for chunk in fragment.chunks(7) {
			fmp4.decode(chunk).unwrap();
		}
		fmp4.finish().unwrap();
		let [got_video, got_audio] = published(&consumer, &catalog).await;
		assert_eq!(got_video, video, "video, passthrough {passthrough}");
		assert_eq!(got_audio, audio, "audio, passthrough {passthrough}");
	}
}

/// A sample must own input bytes even when its size comes from a default.
#[test]
fn empty_samples_are_refused() {
	let (init, (video_id, timescale), _) = bbb_init();
	for explicit in [false, true] {
		let moof = mp4_atom::Moof {
			mfhd: mp4_atom::Mfhd { sequence_number: 1 },
			traf: vec![mp4_atom::Traf {
				tfhd: mp4_atom::Tfhd {
					track_id: video_id,
					default_sample_size: Some(0),
					default_sample_duration: Some(1),
					..Default::default()
				},
				tfdt: Some(mp4_atom::Tfdt {
					base_media_decode_time: 0,
				}),
				trun: vec![mp4_atom::Trun {
					data_offset: None,
					samples: vec![
						mp4_atom::TrunEntry {
							size: explicit.then_some(0),
							..Default::default()
						};
						4
					]
					.into(),
				}],
				..Default::default()
			}],
		};
		let mut fragment = Vec::new();
		moof.encode(&mut fragment).unwrap();
		mp4_atom::Mdat { data: vec![] }.encode(&mut fragment).unwrap();
		for passthrough in [false, true] {
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
			let mut import = super::Import::new(broadcast, catalog.reserve()).with_passthrough(passthrough);
			import.decode(&init).unwrap();
			assert!(
				matches!(
					import.decode(&fragment),
					Err(crate::Error::Cmaf(super::Error::EmptySample))
				),
				"empty import: explicit={explicit}, passthrough={passthrough}"
			);
		}
		assert!(matches!(
			super::decode(fragment.into(), None, super::Track::new(timescale, super::Kind::Video)),
			Err(super::Error::EmptySample)
		));
	}
}

/// A file-relative base is explicitly unsupported on both input paths.
#[test]
fn absolute_sample_bases_are_refused() {
	let (init, (video_id, timescale), _) = bbb_init();
	for base in [0, 1024, u64::MAX] {
		let moof = mp4_atom::Moof {
			mfhd: mp4_atom::Mfhd { sequence_number: 1 },
			traf: vec![mp4_atom::Traf {
				tfhd: mp4_atom::Tfhd {
					track_id: video_id,
					base_data_offset: Some(base),
					default_sample_size: Some(1),
					..Default::default()
				},
				tfdt: Some(mp4_atom::Tfdt {
					base_media_decode_time: 0,
				}),
				trun: vec![mp4_atom::Trun {
					samples: vec![Default::default()].into(),
					..Default::default()
				}],
				..Default::default()
			}],
		};
		let mut fragment = Vec::new();
		moof.encode(&mut fragment).unwrap();
		mp4_atom::Mdat { data: vec![1] }.encode(&mut fragment).unwrap();
		for passthrough in [false, true] {
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
			let mut import = super::Import::new(broadcast, catalog.reserve()).with_passthrough(passthrough);
			import.decode(&init).unwrap();
			assert!(matches!(
				import.decode(&fragment),
				Err(crate::Error::Cmaf(super::Error::UnsupportedBaseDataOffset))
			));
		}
		assert!(matches!(
			super::decode(fragment.into(), None, super::Track::new(timescale, super::Kind::Video)),
			Err(super::Error::UnsupportedBaseDataOffset)
		));
	}
}

/// A run pointing at bytes an earlier sample took is refused, passthrough or unwrapped, whether
/// that sample is in the same track or another, rather than copying or publishing those bytes once
/// per run: a moof of many such runs would multiply its mdat.
#[tokio::test]
async fn shared_sample_bytes_are_refused() {
	let (init, (video_id, video_scale), (audio_id, audio_scale)) = bbb_init();
	let traf = |track_id, scale: moq_net::Timescale, runs| mp4_atom::Traf {
		tfhd: mp4_atom::Tfhd {
			track_id,
			default_sample_duration: Some(1024),
			default_sample_size: Some(4),
			..Default::default()
		},
		tfdt: Some(mp4_atom::Tfdt {
			base_media_decode_time: scale.as_u64(),
		}),
		trun: (0..runs)
			.map(|_| mp4_atom::Trun {
				data_offset: Some(0),
				samples: vec![Default::default()].into(),
			})
			.collect(),
		..Default::default()
	};

	// Each traf with its runs' offsets into an 8-byte mdat of 4-byte samples.
	let cases = [
		vec![(traf(video_id, video_scale, 2), vec![0, 0])],
		vec![
			(traf(video_id, video_scale, 1), vec![0]),
			(traf(audio_id, audio_scale, 1), vec![2]),
		],
	];
	for trafs in cases {
		let mut moof = mp4_atom::Moof {
			mfhd: mp4_atom::Mfhd { sequence_number: 1 },
			traf: trafs.iter().map(|(traf, _)| traf.clone()).collect(),
		};
		let mut buf = Vec::new();
		moof.encode(&mut buf).unwrap();
		let data = buf.len() as i32 + 8;
		for (traf, (_, offsets)) in moof.traf.iter_mut().zip(&trafs) {
			for (trun, offset) in traf.trun.iter_mut().zip(offsets) {
				trun.data_offset = Some(data + offset);
			}
		}
		let mut fragment = Vec::new();
		moof.encode(&mut fragment).unwrap();
		mp4_atom::Mdat { data: vec![0xA5; 8] }.encode(&mut fragment).unwrap();

		for passthrough in [false, true] {
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
			let mut fmp4 =
				crate::container::fmp4::Import::new(broadcast, catalog.reserve()).with_passthrough(passthrough);
			fmp4.decode(&init).unwrap();
			let err = fmp4.decode(&fragment).unwrap_err();
			assert!(
				matches!(
					err,
					crate::Error::Cmaf(crate::container::fmp4::Error::InvalidDataOffset)
				),
				"passthrough {passthrough}: {err:?}"
			);
		}
	}
}

/// An unwrapped sample presents at `tfdt + the durations before it + its composition offset`, so
/// reordered (B-frame) samples keep decode order on the wire with their own presentation times,
/// and only the sync sample opens a group.
#[tokio::test]
async fn unwrapped_samples_present_at_their_composition_time() {
	let (init, (video_id, video_scale), _) = bbb_init();
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve());
	fmp4.decode(&init).unwrap();

	let snapshot = catalog.snapshot();
	let (name, config) = snapshot.video.renditions.iter().next().unwrap();
	assert_eq!(config.container, Container::Legacy);
	let mut track = consumer.track(name).unwrap().subscribe(replay()).await.unwrap();

	// I P B in decode order, one frame (1/24 s) apart, the P presenting after the B.
	let tick = video_scale.as_u64() / 24;
	let non_sync = 0x0001_0000;
	let entries = [(tick, 0, 0xA), (2 * tick, non_sync, 0xB), (0, non_sync, 0xC)];
	let moof = mp4_atom::Moof {
		mfhd: mp4_atom::Mfhd { sequence_number: 1 },
		traf: vec![mp4_atom::Traf {
			tfhd: mp4_atom::Tfhd {
				track_id: video_id,
				..Default::default()
			},
			tfdt: Some(mp4_atom::Tfdt {
				base_media_decode_time: video_scale.as_u64(),
			}),
			trun: vec![mp4_atom::Trun {
				data_offset: None,
				samples: entries
					.iter()
					.map(|&(cts, flags, _)| mp4_atom::TrunEntry {
						duration: Some(tick as u32),
						size: Some(2),
						flags: Some(flags),
						cts: Some(cts as i64),
					})
					.collect::<Vec<_>>()
					.into(),
			}],
			..Default::default()
		}],
	};
	let mut fragment = Vec::new();
	moof.encode(&mut fragment).unwrap();
	mp4_atom::Mdat {
		data: entries.iter().flat_map(|&(_, _, byte)| [byte; 2]).collect(),
	}
	.encode(&mut fragment)
	.unwrap();
	fmp4.decode(&fragment).unwrap();
	fmp4.finish().unwrap();

	let mut group = track.recv_group().await.unwrap().unwrap();
	let mut frames = Vec::new();
	while let Some(frame) = group.read_frame().await.unwrap() {
		let frame = hang::container::Frame::decode(frame.payload).unwrap();
		frames.push((frame.timestamp.as_micros(), frame.payload.to_vec()));
	}
	// 25, 27, and 26 frames at 24 per second, in whole microseconds.
	assert_eq!(
		frames[..3],
		[
			(1_041_666, vec![0xA; 2]),
			(1_125_000, vec![0xB; 2]),
			(1_083_333, vec![0xC; 2]),
		],
		"decode order, each at its composition time"
	);
	assert!(
		track.recv_group().await.unwrap().is_none(),
		"the non-sync samples stay in the sync sample's group"
	);
}

/// bbb's init with `entries` as the edit list of track `track_id`.
/// Once the video group is open, the keyframe scan for a segment boundary stops at the mdat
/// rather than walking a forged moof's field-less runs of `u32::MAX` non-sync samples.
#[test]
fn fieldless_runs_do_not_stall_the_segment_scan() {
	let (_, (video_id, video_scale), _) = bbb_init();
	let init = bbb_init_with_edits(video_id, vec![media_edit(0)]);
	let tick = video_scale.as_u64() / 24;

	let traf = |runs: usize| mp4_atom::Traf {
		tfhd: mp4_atom::Tfhd {
			track_id: video_id,
			default_sample_duration: Some(tick as u32),
			default_sample_size: Some(1),
			default_sample_flags: Some(0x0001_0000),
			..Default::default()
		},
		tfdt: Some(mp4_atom::Tfdt {
			base_media_decode_time: tick,
		}),
		trun: vec![
			mp4_atom::Trun {
				data_offset: None,
				samples: mp4_atom::TrunSamples::Inherited {
					count: u32::MAX,
					first_flags: None,
				},
			};
			runs
		],
		..Default::default()
	};
	// Many runs in one traf, and many trafs over a larger mdat: the budget spans the whole moof, so
	// repeating the traf can't multiply it.
	for (trafs, runs, mdat) in [(1, 1000, 6), (1000, 1, 1 << 16)] {
		let moof = mp4_atom::Moof {
			mfhd: mp4_atom::Mfhd { sequence_number: 2 },
			traf: vec![traf(runs); trafs],
		};
		let mut forged = Vec::new();
		moof.encode(&mut forged).unwrap();
		mp4_atom::Mdat { data: vec![0; mdat] }.encode(&mut forged).unwrap();

		for passthrough in [false, true] {
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
			let mut fmp4 =
				crate::container::fmp4::Import::new(broadcast, catalog.reserve()).with_passthrough(passthrough);
			fmp4.decode(&init).unwrap();
			fmp4.decode(&video_fragment(video_id, 0, tick, &[(0, 0, 0xA)])).unwrap();
			assert!(
				matches!(
					fmp4.decode(&forged),
					Err(crate::Error::Cmaf(super::Error::SampleRangeOutOfBounds { .. }))
				),
				"{trafs} trafs, passthrough {passthrough}"
			);
		}
	}
}

fn bbb_init_with_edits(track_id: u32, entries: Vec<mp4_atom::ElstEntry>) -> Vec<u8> {
	let (init, _, _) = bbb_init();
	let (ftyp, mut moov) = decode_init(&init);
	let trak = moov
		.trak
		.iter_mut()
		.find(|trak| trak.tkhd.track_id == track_id)
		.unwrap();
	trak.edts = Some(mp4_atom::Edts {
		elst: Some(mp4_atom::Elst { entries }),
	});
	let mut data = Vec::new();
	ftyp.encode(&mut data).unwrap();
	moov.encode(&mut data).unwrap();
	data
}

/// An edit starting the media at `media_time`.
fn media_edit(media_time: u64) -> mp4_atom::ElstEntry {
	mp4_atom::ElstEntry {
		segment_duration: 0,
		media_time: Some(media_time),
		media_rate: mp4_atom::FixedPoint::from(1),
	}
}

/// The edit list moves every sample to its presentation time and drops the samples before the
/// edit, priming that fills a frame or straddles one, passthrough or unwrapped. The passthrough
/// init loses the `edts`, so no consumer applies it a second time.
#[tokio::test]
async fn audio_presents_from_its_edit() {
	let (_, _, (audio_id, audio_scale)) = bbb_init();
	let micros = |ticks: u64| u128::from(ticks) * 1_000_000 / u128::from(audio_scale.as_u64());
	let empty = mp4_atom::ElstEntry {
		// bbb's movie counts milliseconds.
		segment_duration: 1000,
		media_time: None,
		media_rate: mp4_atom::FixedPoint::from(1),
	};
	// The samples start at 0 and 512, then every 1024 ticks.
	let cases = [
		// Priming of whole frames, as ffmpeg's AAC.
		(vec![media_edit(512)], vec![0, 1024, 2048]),
		// Priming that ends inside a frame drops that frame too, as Apple's AAC.
		(vec![media_edit(1024)], vec![512, 1536]),
		// An empty edit delays the media.
		(vec![empty, media_edit(512)], vec![44_100, 45_124, 46_148]),
	];

	for (entries, expected) in cases {
		for passthrough in [false, true] {
			let init = bbb_init_with_edits(audio_id, entries.clone());
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let consumer = broadcast.consume();
			let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
			let mut fmp4 =
				crate::container::fmp4::Import::new(broadcast, catalog.reserve()).with_passthrough(passthrough);
			fmp4.decode(&init).unwrap();
			// A fragment wholly before the edit publishes nothing.
			fmp4.decode(&audio_fragment(0, 512, 4)).unwrap();
			fmp4.decode(&audio_fragment_samples(512, 1024, 4, 3)).unwrap();
			fmp4.finish().unwrap();

			let snapshot = catalog.snapshot();
			if let Container::Cmaf { init } = &snapshot.audio.renditions.values().next().unwrap().container {
				let (_, moov) = decode_init(init);
				assert!(moov.trak[0].edts.is_none(), "the init keeps no edit list");
			}
			let [_, audio] = published(&consumer, &catalog).await;
			let got: Vec<_> = audio.iter().map(|(timestamp, _, _)| *timestamp).collect();
			let expected: Vec<_> = expected.iter().map(|&ticks| micros(ticks)).collect();
			assert_eq!(got, expected, "{entries:?}, passthrough {passthrough}");
		}
	}
}

/// One video fragment decoding from `dts`, a sample per `(cts, flags, byte)` lasting `tick`.
fn video_fragment(track_id: u32, dts: u64, tick: u64, entries: &[(u64, u32, u8)]) -> Vec<u8> {
	let moof = mp4_atom::Moof {
		mfhd: mp4_atom::Mfhd { sequence_number: 1 },
		traf: vec![mp4_atom::Traf {
			tfhd: mp4_atom::Tfhd {
				track_id,
				..Default::default()
			},
			tfdt: Some(mp4_atom::Tfdt {
				base_media_decode_time: dts,
			}),
			trun: vec![mp4_atom::Trun {
				data_offset: None,
				samples: entries
					.iter()
					.map(|&(cts, flags, _)| mp4_atom::TrunEntry {
						duration: Some(tick as u32),
						size: Some(2),
						flags: Some(flags),
						cts: Some(cts as i64),
					})
					.collect::<Vec<_>>()
					.into(),
			}],
			..Default::default()
		}],
	};
	let mut fragment = Vec::new();
	moof.encode(&mut fragment).unwrap();
	mp4_atom::Mdat {
		data: entries.iter().flat_map(|&(_, _, byte)| [byte; 2]).collect(),
	}
	.encode(&mut fragment)
	.unwrap();
	fragment
}

/// The edit list drops the video samples before it and keeps every other one at its own
/// presentation time and in decode order, passthrough or unwrapped. A dropped sync sample takes the
/// samples decoding from it along, so the track resumes at the next one.
#[tokio::test]
async fn video_drops_the_samples_before_its_edit() {
	let (_, (video_id, video_scale), _) = bbb_init();
	let tick = video_scale.as_u64() / 24;
	let micros = |ticks: u64| u128::from(ticks) * 1_000_000 / u128::from(video_scale.as_u64());
	let sample = |ticks: u64, keyframe: bool, byte: u8| (micros(ticks), keyframe, bytes::Bytes::from(vec![byte; 2]));
	let non_sync = 0x0001_0000;

	let cases = [
		// I, a leading B presenting before it, P, then B, in decode order.
		(
			2 * tick,
			vec![video_fragment(
				video_id,
				0,
				tick,
				&[
					(2 * tick, 0, 0xA),
					(0, non_sync, 0xB),
					(2 * tick, non_sync, 0xC),
					(0, non_sync, 0xD),
				],
			)],
			vec![
				sample(0, true, 0xA),
				sample(2 * tick, false, 0xC),
				sample(tick, false, 0xD),
			],
		),
		// An edit after the first I, as a cut at a non-sync sample writes: the P frames after the
		// edit can't decode without it.
		(
			tick,
			vec![
				video_fragment(
					video_id,
					0,
					tick,
					&[(0, 0, 0xA), (0, non_sync, 0xB), (0, non_sync, 0xC)],
				),
				video_fragment(video_id, 3 * tick, tick, &[(0, 0, 0xD), (0, non_sync, 0xE)]),
			],
			vec![sample(2 * tick, true, 0xD), sample(3 * tick, false, 0xE)],
		),
	];
	for (media_time, fragments, expected) in cases {
		let init = bbb_init_with_edits(video_id, vec![media_edit(media_time)]);
		for passthrough in [false, true] {
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let consumer = broadcast.consume();
			let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
			let mut fmp4 =
				crate::container::fmp4::Import::new(broadcast, catalog.reserve()).with_passthrough(passthrough);
			fmp4.decode(&init).unwrap();
			for fragment in &fragments {
				fmp4.decode(fragment).unwrap();
			}
			fmp4.finish().unwrap();
			let [video, _] = published(&consumer, &catalog).await;
			assert_eq!(video, expected, "edit at {media_time}, passthrough {passthrough}");
		}
	}
}

/// A video track joined mid-GOP, at the start or after a `seek`, publishes nothing before its next
/// sync sample, passthrough or unwrapped, even when that sample sits inside a fragment, so no group
/// opens on a frame its decoder cannot use.
#[tokio::test]
async fn video_joined_mid_gop_starts_at_a_sync_sample() {
	let (_, (video_id, video_scale), _) = bbb_init();
	let tick = video_scale.as_u64() / 24;
	let micros = |ticks: u64| u128::from(ticks) * 1_000_000 / u128::from(video_scale.as_u64());
	let sample = |ticks: u64, keyframe: bool, byte: u8| (micros(ticks), keyframe, bytes::Bytes::from(vec![byte; 2]));
	let non_sync = 0x0001_0000;

	let init = bbb_init_with_edits(video_id, vec![media_edit(0)]);
	let fragments = [
		video_fragment(video_id, 0, tick, &[(0, non_sync, 0xA), (0, non_sync, 0xB)]),
		video_fragment(
			video_id,
			2 * tick,
			tick,
			&[(0, non_sync, 0xC), (0, 0, 0xD), (0, non_sync, 0xE)],
		),
		video_fragment(video_id, 5 * tick, tick, &[(0, 0, 0xF), (0, non_sync, 0x10)]),
	];
	let after_seek = [
		video_fragment(video_id, 7 * tick, tick, &[(0, non_sync, 0x11)]),
		video_fragment(video_id, 8 * tick, tick, &[(0, non_sync, 0x12), (0, 0, 0x13)]),
	];
	let expected = vec![
		sample(3 * tick, true, 0xD),
		sample(4 * tick, false, 0xE),
		sample(5 * tick, true, 0xF),
		sample(6 * tick, false, 0x10),
		sample(9 * tick, true, 0x13),
	];
	for passthrough in [false, true] {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve()).with_passthrough(passthrough);
		fmp4.decode(&init).unwrap();
		for fragment in &fragments {
			fmp4.decode(fragment).unwrap();
		}
		fmp4.seek(10).unwrap();
		for fragment in &after_seek {
			fmp4.decode(fragment).unwrap();
		}
		fmp4.finish().unwrap();
		let [video, _] = published(&consumer, &catalog).await;
		assert_eq!(video, expected, "passthrough {passthrough}");
	}
}

#[test]
fn test_bbb_init_roundtrip() {
	let data = include_bytes!("test_data/bbb.mp4");
	let catalog = run_fmp4_passthrough(data);

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
	let catalog = run_fmp4_passthrough(data);

	assert_eq!(catalog.video.renditions.len(), 1);
	assert_eq!(catalog.audio.renditions.len(), 0);

	let video = catalog.video.renditions.values().next().unwrap();
	assert!(video.codec.to_string().starts_with("av01."), "codec: {}", video.codec);
	// The av1C's sequence header leaves the code points unspecified, but AV1 always states the range.
	let color = video.color.as_ref().expect("the range");
	assert_eq!(
		(color.primaries, color.range),
		(None, Some(hang::catalog::Range::Limited))
	);
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
	assert!(matches!(video.container, Container::Legacy));
}

#[test]
fn test_vp9_catalog() {
	let data = include_bytes!("test_data/vp9.mp4");
	let catalog = run_fmp4_passthrough(data);

	assert_eq!(catalog.video.renditions.len(), 1);
	assert_eq!(catalog.audio.renditions.len(), 0);

	let video = catalog.video.renditions.values().next().unwrap();
	assert!(video.codec.to_string().starts_with("vp09."), "codec: {}", video.codec);
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
	for passthrough in [false, true] {
		test_seek_sets_initial_sequence_in(passthrough).await;
	}
}

async fn test_seek_sets_initial_sequence_in(passthrough: bool) {
	use mp4_atom::{Any, DecodeMaybe};

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let broadcast_consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve()).with_passthrough(passthrough);

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
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve()).with_passthrough(true);

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

/// Subscribe to `track`'s HLS records, before the catalog may advertise them.
async fn subscribe(consumer: &moq_net::broadcast::Consumer, track: &str) -> crate::hls::Consumer {
	let hls = hang::catalog::Hls::new(hang::hls::default_name(track), std::time::Duration::ZERO);
	crate::hls::Consumer::subscribe(consumer, &hls).await.unwrap()
}

/// Every segment a finished track's records listed, as `(first group, next segment's group)`.
async fn groups(records: &mut crate::hls::Consumer) -> Vec<(u64, u64)> {
	let mut entries = Vec::new();
	while let Some(event) = records.next().await.unwrap() {
		if let crate::hls::Event::Push { entry, .. } = event {
			entries.push(entry);
		}
	}
	assert!(
		entries.last().is_some_and(|entry| entry.is_stop()),
		"the track's end stops the last segment"
	);
	entries
		.windows(2)
		.filter(|pair| !pair[0].is_stop())
		.map(|pair| (pair[0].group, pair[1].group))
		.collect()
}

/// Passthrough writes groups by hand (no `container::Producer`), so the HLS recorders are fed
/// directly. This guards that the import advertises both renditions' records in their entries and
/// actually records their groups, the index the playlist export reads.
#[tokio::test]
async fn import_populates_the_hls_records() {
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

	// This short fixture ends before the first four-second segment closes, so finishing the import
	// stops it and declares the minimum while the catalog stays live.
	let four = std::time::Duration::from_secs(4);
	assert_eq!(
		snapshot.video.renditions[&video_name].hls.as_ref().unwrap().duration,
		four
	);
	assert_eq!(
		snapshot.audio.renditions[&audio_name].hls.as_ref().unwrap().duration,
		four
	);

	// Subscribe while the producer is alive, then finish so the record tracks close and the
	// readers terminate rather than blocking.
	let mut video = subscribe(&consumer, &video_name).await;
	let mut audio = subscribe(&consumer, &audio_name).await;
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
	assert!(matches!(a.container, Container::Legacy));
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
		channel_mapping: None,
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

/// `init` with its Opus sample entry declaring `channels` channels and `mapping`.
fn opus_init_with_mapping(init: &[u8], channels: u8, mapping: mp4_atom::OpusChannelMapping) -> Vec<u8> {
	let (ftyp, mut moov) = decode_init(init);
	let mp4_atom::Codec::Opus(opus) = &mut moov.trak[0].mdia.minf.stbl.stsd.codecs[0] else {
		panic!("expected Opus sample entry");
	};
	opus.audio.channel_count = channels.into();
	opus.dops.output_channel_count = channels;
	opus.dops.channel_mapping = Some(mapping);
	let mut data = Vec::new();
	ftyp.encode(&mut data).unwrap();
	moov.encode(&mut data).unwrap();
	data
}

/// A family 1 5.1 dOps imports with its mapping table in the OpusHead description, and
/// re-exporting that description writes the same dOps, table included.
#[test]
fn opus_dops_surround_round_trips() {
	let (_, data) = opus_init(48_000);
	let mapping = mp4_atom::OpusChannelMapping {
		family: 1,
		stream_count: 4,
		coupled_count: 2,
		channel_mapping: vec![0, 4, 1, 2, 3, 5],
	};
	let data = opus_init_with_mapping(&data, 6, mapping.clone());
	let (_, moov) = decode_init(&data);
	let mp4_atom::Codec::Opus(source) = &moov.trak[0].mdia.minf.stbl.stsd.codecs[0] else {
		panic!("expected Opus sample entry");
	};

	let catalog = run_fmp4(&data);
	let a = catalog.audio.renditions.values().next().expect("opus rendition");
	assert_eq!(a.channel_count, 6);

	let desc = a.description.as_ref().expect("opus description");
	let head = crate::codec::opus::Config::parse(&mut desc.as_ref()).unwrap();
	assert_eq!(head.channel_count, 6);
	let parsed = head.mapping.expect("mapping table");
	assert_eq!(parsed.family(), 1);
	assert_eq!((parsed.streams(), parsed.coupled()), (4, 2));
	assert_eq!(parsed.table(), &[0, 4, 1, 2, 3, 5]);

	let trak = super::synthesize_audio_trak(1, 48_000, a).expect("synthesize Opus trak");
	match &trak.mdia.minf.stbl.stsd.codecs[0] {
		mp4_atom::Codec::Opus(opus) => {
			assert_eq!(opus.dops, source.dops);
			assert_eq!(opus.dops.channel_mapping, Some(mapping));
			assert_eq!(opus.audio.channel_count, 6);
		}
		other => panic!("expected Opus sample entry, got {other:?}"),
	}
}

/// A dOps mapping the OpusHead cannot describe is refused rather than imported: family 3,
/// whose OpusHead carries a demixing matrix, and a family 1 table past its 8 channels.
#[test]
fn opus_dops_mapping_the_head_cannot_describe_is_refused() {
	let (_, init) = opus_init(48_000);
	let cases = [
		(
			mp4_atom::OpusChannelMapping {
				family: 3,
				stream_count: 2,
				coupled_count: 2,
				channel_mapping: vec![0, 1, 2, 3],
			},
			crate::codec::opus::Error::UnsupportedMappingFamily(3),
		),
		(
			mp4_atom::OpusChannelMapping {
				family: 1,
				stream_count: 9,
				coupled_count: 0,
				channel_mapping: (0..9).collect(),
			},
			crate::codec::opus::Error::UnsupportedChannelCount(9),
		),
	];
	for (mapping, expected) in cases {
		let channels = mapping.channel_mapping.len() as u8;
		let data = opus_init_with_mapping(&init, channels, mapping);

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve());
		let err = fmp4.decode(&bytes::BytesMut::from(data.as_slice())).unwrap_err();
		assert!(
			matches!(&err, crate::Error::Cmaf(super::Error::Opus(opus)) if opus.to_string() == expected.to_string()),
			"{err:?}"
		);
		assert!(catalog.snapshot().audio.renditions.is_empty());
	}
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
	for passthrough in [false, true] {
		segmented_source_groups_per_segment_in(passthrough).await;
	}
}

async fn segmented_source_groups_per_segment_in(passthrough: bool) {
	let (init, (video_id, video_scale), (audio_id, audio_scale)) = bbb_init();

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve()).with_passthrough(passthrough);
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

	// An unwrapped video group also ends on a Legacy duration marker.
	let marker = usize::from(!passthrough);
	assert_eq!(
		drain_group_frames(&mut video_track),
		vec![2 + marker; 3],
		"one video group per segment, each carrying its two fragments"
	);
	assert_eq!(
		drain_group_frames(&mut audio_track),
		vec![4, 4, 4],
		"audio follows the same segmentation instead of opening a group per fragment"
	);
}

/// Each declared segment is one group per track, and each track's HLS records list it as one
/// segment. With audio grouping per fragment those records spanned many short groups, one FETCH
/// round-trip each.
#[tokio::test]
async fn segmented_source_indexes_one_group_per_record() {
	for passthrough in [false, true] {
		segmented_source_indexes_one_group_per_record_in(passthrough).await;
	}
}

async fn segmented_source_indexes_one_group_per_record_in(passthrough: bool) {
	let (init, (video_id, video_scale), (audio_id, audio_scale)) = bbb_init();

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve()).with_passthrough(passthrough);
	fmp4.decode(&init).unwrap();

	let snapshot = catalog.snapshot();
	let video_name = snapshot.video.renditions.keys().next().unwrap().clone();
	let audio_name = snapshot.audio.renditions.keys().next().unwrap().clone();

	// Subscribe while the producer is alive so the readers terminate on finish rather than blocking.
	let mut video = subscribe(&consumer, &video_name).await;
	let mut audio = subscribe(&consumer, &audio_name).await;

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
/// and each track begins at its own nearest sample. Each track starts its own HLS segment where it
/// opens its group for a declared segment, so a skew between them moves no group into another one.
///
/// `video_offset_us` shifts video relative to audio, so the same source is exercised with audio
/// leading and lagging. Returns each track's records as `(start group, end group)`.
async fn records_with_skew(video_offset_us: i64, passthrough: bool) -> (Vec<(u64, u64)>, Vec<(u64, u64)>) {
	let (init, (video_id, video_scale), (audio_id, audio_scale)) = bbb_init();

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve()).with_passthrough(passthrough);
	fmp4.decode(&init).unwrap();

	let snapshot = catalog.snapshot();
	let video_name = snapshot.video.renditions.keys().next().unwrap().clone();
	let audio_name = snapshot.audio.renditions.keys().next().unwrap().clone();
	let mut video = subscribe(&consumer, &video_name).await;
	let mut audio = subscribe(&consumer, &audio_name).await;

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
	for passthrough in [false, true] {
		assert_eq!(
			records_with_skew(20_000, passthrough).await,
			(expected.clone(), expected.clone())
		);
	}
}

/// Video's IDR lands 20ms BEFORE audio's first sample, the other side of the same skew.
#[tokio::test]
async fn audio_lagging_video_still_indexes_one_group_each() {
	let expected = vec![(0, 1), (1, 2), (2, 3)];
	for passthrough in [false, true] {
		assert_eq!(
			records_with_skew(-20_000, passthrough).await,
			(expected.clone(), expected.clone())
		);
	}
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
	for passthrough in [false, true] {
		a_single_leading_styp_still_segments_on_keyframes_in(passthrough).await;
	}
}

async fn a_single_leading_styp_still_segments_on_keyframes_in(passthrough: bool) {
	let (init, (video_id, video_scale), (audio_id, audio_scale)) = bbb_init();

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve()).with_passthrough(passthrough);
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

	// An unwrapped video group also ends on a Legacy duration marker.
	let marker = usize::from(!passthrough);
	assert_eq!(
		drain_group_frames(&mut video_track),
		vec![1 + marker; 4],
		"each keyframe fragment opens its own segment"
	);
	assert_eq!(
		drain_group_frames(&mut audio_track),
		vec![1, 1, 1, 1],
		"audio follows the same boundaries rather than trailing a segment behind"
	);
}

/// A muxed source joined mid-GOP keeps audio and video on the same group sequences.
///
/// Unwrapped video drops samples until its first sync sample, so its segment opens there, not on
/// the skipped fragment. Counting the skipped fragment as opened rolled the next keyframe into a
/// second segment, leaving audio one group ahead of video for the rest of the stream.
#[tokio::test]
async fn a_mid_gop_join_keeps_tracks_on_the_same_groups() {
	for passthrough in [false, true] {
		a_mid_gop_join_keeps_tracks_on_the_same_groups_in(passthrough).await;
	}
}

async fn a_mid_gop_join_keeps_tracks_on_the_same_groups_in(passthrough: bool) {
	let (init, (video_id, video_scale), (audio_id, audio_scale)) = bbb_init();

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let consumer = broadcast.consume();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve()).with_passthrough(passthrough);
	fmp4.decode(&init).unwrap();

	let snapshot = catalog.snapshot();
	let video_name = snapshot.video.renditions.keys().next().unwrap().clone();
	let audio_name = snapshot.audio.renditions.keys().next().unwrap().clone();
	let mut video_track = consumer.track(&video_name).unwrap().subscribe(replay()).await.unwrap();
	let mut audio_track = consumer.track(&audio_name).unwrap().subscribe(replay()).await.unwrap();

	// No styp, so segments follow video keyframes; the first video fragment is mid-GOP.
	for segment in 0..3u64 {
		let base = segment * 1_000_000;
		let frag = super::encode_fragment(
			info(video_id, video_scale, segment as u32),
			&[sample(base, segment > 0, Some(1_000_000))],
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

	let video = drain_group_sequences(&mut video_track);
	let audio = drain_group_sequences(&mut audio_track);
	assert_eq!(audio, video, "audio and video groups line up");
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
				samples: vec![mp4_atom::TrunEntry::default()].into(),
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
				samples: vec![mp4_atom::TrunEntry::default(); samples].into(),
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
	for passthrough in [false, true] {
		fragment_jitter_never_shrinks_in(passthrough);
	}
}

fn fragment_jitter_never_shrinks_in(passthrough: bool) {
	let (ftyp, moov) = decode_init(include_bytes!("test_data/bbb.mp4"));
	let mut init = Vec::new();
	ftyp.encode(&mut init).unwrap();
	moov.encode(&mut init).unwrap();

	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve()).with_passthrough(passthrough);
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
					samples: samples
						.iter()
						.map(|(duration, cts)| mp4_atom::TrunEntry {
							duration: *duration,
							cts: Some(*cts),
							..Default::default()
						})
						.collect::<Vec<_>>()
						.into(),
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
	arrival: std::ops::RangeInclusive<moq_clock::Wall>,
	/// Where the video's first HLS segment starts.
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
	let mut catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve());
	fmp4.decode(&init).unwrap();

	let snapshot = catalog.snapshot();
	let video = snapshot.video.renditions.keys().next().unwrap().clone();
	let mut records = subscribe(&consumer, &video).await;

	let before = moq_clock::wall();
	let mut after = before;
	let mut refused = None;
	for (i, chunk) in chunks.iter().enumerate() {
		if let Err(err) = fmp4.decode(chunk) {
			refused = Some(err);
			break;
		}
		if i == 0 {
			after = moq_clock::wall();
		}
	}
	fmp4.finish().unwrap();
	catalog.finish().unwrap();

	let event = records.next().await.unwrap().expect("a record");
	let crate::hls::Event::Push { entry, .. } = event else {
		panic!("the first event was not a record");
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
/// time. The HLS records list the same times, so a playlist names each segment's wall time.
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
		"the HLS records list the source's time"
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
			let mut stream = catalog
				.json_stream::<serde_json::Value>(track, crate::json::Config::default())
				.unwrap();
			stream
				.append(moq_net::Timed::from(&serde_json::json!("before")).at(catalog.clock().now()))
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
			.append(moq_net::Timed::from(&serde_json::json!("after")).at(catalog.clock().now()))
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
/// after a seek is still refused. A signalled discontinuity skips the import's own comparison,
/// but the track still refuses a group that starts before the one before it.
#[tokio::test]
async fn seek_keeps_the_decode_time_until_a_discontinuity() {
	for passthrough in [false, true] {
		seek_keeps_the_decode_time_until_a_discontinuity_in(passthrough).await;
	}
}

async fn seek_keeps_the_decode_time_until_a_discontinuity_in(passthrough: bool) {
	let (init, _, _) = bbb_init();
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve()).with_passthrough(passthrough);
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

	fmp4.discontinuity().unwrap();
	fmp4.seek(200).unwrap();
	let err = fmp4.decode(&live_session(0, 5)).unwrap_err();
	// Passthrough writes its groups by hand, so it refuses the rewound group itself, the way the
	// media producer refuses an unwrapped track's rewound keyframe.
	assert!(matches!(err, crate::Error::TimestampRewind(_)), "{err:?}");
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
	for passthrough in [false, true] {
		imports_the_sample_defaults_fixtures_in(passthrough).await;
	}
}

async fn imports_the_sample_defaults_fixtures_in(passthrough: bool) {
	use mp4_atom::DecodeMaybe;

	for fixture in super::fixtures() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let mut fmp4 = crate::container::fmp4::Import::new(broadcast, catalog.reserve()).with_passthrough(passthrough);
		fmp4.decode(&fixture.init).unwrap();

		let snapshot = catalog.snapshot();
		let (name, video) = snapshot.video.renditions.iter().next().unwrap();
		let wire = match &video.container {
			Container::Cmaf { init } => Some(super::Wire::from_init(init).unwrap()),
			Container::Legacy => None,
			other => panic!("{}: unexpected container {other:?}", fixture.name),
		};
		assert_eq!(wire.is_some(), passthrough, "{}", fixture.name);
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

		let Some(wire) = wire else {
			// Unwrapped, each sample is its own frame and each sync sample opens a group. Legacy
			// carries no duration, so only the timing, bytes, and grouping are compared.
			let mut samples = Vec::new();
			while let Some(mut group) = track.recv_group().await.unwrap() {
				let mut index = 0;
				while let Some(frame) = group.read_frame().await.unwrap() {
					let frame = hang::container::Frame::decode(frame.payload).unwrap();
					// The duration marker closing a video group is no sample.
					if !frame.payload.is_empty() {
						samples.push((frame.timestamp.as_micros() as u64, index == 0, frame.payload.to_vec()));
					}
					index += 1;
				}
			}
			let expected: Vec<_> = fixture
				.samples
				.iter()
				.map(|sample| (sample.timestamp, sample.keyframe, sample.data.clone()))
				.collect();
			assert_eq!(samples[..expected.len()], expected, "{}", fixture.name);
			assert!(
				samples[expected.len()].1,
				"{}: the second fragment opens on a sync sample, so a group",
				fixture.name
			);
			continue;
		};

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
				samples: samples
					.iter()
					.map(|sample| mp4_atom::TrunEntry {
						size: Some(sample.len() as u32),
						..Default::default()
					})
					.collect::<Vec<_>>()
					.into(),
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
	let wall = moq_clock::wall();
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

/// An avc1 init whose avcC carries `sps` and whose sample entry carries the `colr` box for `color`.
fn avc1_init(sps: &'static [u8], color: Option<hang::catalog::Color>) -> bytes::Bytes {
	use crate::codec::h264::fixtures::PPS;
	use bytes::Bytes;
	use hang::catalog::{H264, VideoConfig};

	let avcc = crate::codec::h264::build_avcc(&[Bytes::from_static(sps)], &[Bytes::from_static(PPS)]).unwrap();
	let mut config = VideoConfig::new(H264 {
		profile: 0x42,
		constraints: 0xc0,
		level: 0x0a,
		inline: false,
	});
	config.coded_width = Some(64);
	config.coded_height = Some(64);
	config.color = color;
	let trak = super::synthesize_video_trak(1, 90_000, &config, Some(&avcc)).unwrap();
	let trex = mp4_atom::Trex {
		track_id: 1,
		default_sample_description_index: 1,
		..Default::default()
	};
	super::encode_init(None, vec![trak], vec![trex]).unwrap()
}

fn color(primaries: u8, transfer: u8, matrix: u8, full_range: bool) -> hang::catalog::Color {
	crate::codec::color::cicp(primaries, transfer, matrix, Some(full_range)).unwrap()
}

/// A colr box the SPS disagrees with loses to the SPS: the encoder signalled it.
#[test]
fn sps_color_outranks_colr() {
	let catalog = run_fmp4(&avc1_init(
		crate::codec::h264::fixtures::SPS_BT709,
		Some(color(9, 16, 9, true)),
	));
	let video = catalog.video.renditions.values().next().unwrap();
	assert_eq!(video.color, Some(color(1, 1, 1, false)));
}

/// A colr box fills what the SPS leaves unspecified.
#[test]
fn colr_fills_what_the_sps_leaves_unsaid() {
	let catalog = run_fmp4(&avc1_init(
		crate::codec::h264::fixtures::SPS_IPB,
		Some(color(9, 16, 9, true)),
	));
	let video = catalog.video.renditions.values().next().unwrap();
	assert_eq!(video.color, Some(color(9, 16, 9, true)));
}

/// A source that signals no colour adds no catalog field and no colr box.
#[test]
fn unsignalled_color_stays_absent() {
	let init = avc1_init(crate::codec::h264::fixtures::SPS_IPB, None);
	let (_, moov) = decode_init(&init);
	let mp4_atom::Codec::Avc1(avc1) = &moov.trak[0].mdia.minf.stbl.stsd.codecs[0] else {
		panic!("expected avc1");
	};
	assert_eq!(avc1.colr, None);

	let catalog = run_fmp4(&init);
	let video = catalog.video.renditions.values().next().unwrap();
	assert_eq!(video.color, None);
	assert!(serde_json::to_value(video).unwrap().get("color").is_none());
}

/// A field-coded SPS in the avcC marks the rendition interlaced, so the player picks a decoder that accepts it.
#[test]
fn field_coded_sps_marks_interlaced() {
	for (sps, interlaced) in [
		(crate::codec::h264::fixtures::SPS_IPB, false),
		(crate::codec::h264::fixtures::SPS_FIELD, true),
	] {
		let catalog = run_fmp4(&avc1_init(sps, None));
		let video = catalog.video.renditions.values().next().unwrap();
		assert_eq!(video.interlaced, interlaced);
	}
}

/// An SDR init imported to the catalog and exported again is byte-identical.
#[test]
fn sdr_color_round_trips_byte_identical() {
	let init = avc1_init(crate::codec::h264::fixtures::SPS_BT709, Some(color(1, 1, 1, false)));
	let catalog = run_fmp4(&init);
	let video = catalog.video.renditions.values().next().unwrap();
	assert_eq!(video.color, Some(color(1, 1, 1, false)));

	let trak = super::synthesize_video_trak(1, 90_000, video, video.description.as_deref()).unwrap();
	let trex = mp4_atom::Trex {
		track_id: 1,
		default_sample_description_index: 1,
		..Default::default()
	};
	assert_eq!(super::encode_init(None, vec![trak], vec![trex]).unwrap(), init);
}

/// VP8's vpcC carries its colour as VP9's does: export writes the catalog's, unspecified when
/// unknown rather than mp4-atom's BT.709 default, and import reads it back.
#[test]
fn vp8_color_round_trips_through_vpcc() {
	use hang::catalog::{VideoCodec, VideoConfig};

	let init = |color: Option<hang::catalog::Color>| {
		let mut config = VideoConfig::new(VideoCodec::VP8);
		config.coded_width = Some(64);
		config.coded_height = Some(64);
		config.color = color;
		let trak = super::synthesize_video_trak(1, 90_000, &config, None).unwrap();
		let trex = mp4_atom::Trex {
			track_id: 1,
			default_sample_description_index: 1,
			..Default::default()
		};
		super::encode_init(None, vec![trak], vec![trex]).unwrap()
	};

	let catalog = run_fmp4(&init(Some(color(9, 18, 9, true))));
	let video = catalog.video.renditions.values().next().unwrap();
	assert_eq!(video.color, Some(color(9, 18, 9, true)));

	// Without a catalog colour, only vpcC's always-explicit range survives, never a made-up BT.709.
	let catalog = run_fmp4(&init(None));
	let color = catalog.video.renditions.values().next().unwrap().color.clone().unwrap();
	assert_eq!((color.primaries, color.transfer, color.matrix), (None, None, None));
}

/// A VP9 codec string holding a container's placeholder BT.709 does not contradict the resolved
/// colour: export writes the catalog colour into vpcC too, so a reimport keeps it.
#[test]
fn vp9_vpcc_carries_the_resolved_color() {
	use hang::catalog::{VP9, VideoConfig};

	let mut config = VideoConfig::new(VP9 {
		profile: 0,
		level: 0,
		bit_depth: 8,
		chroma_subsampling: 1,
		color_primaries: 1,
		transfer_characteristics: 1,
		matrix_coefficients: 1,
		full_range: false,
	});
	config.coded_width = Some(64);
	config.coded_height = Some(64);
	let reimport = |resolved: hang::catalog::Color| {
		let mut config = config.clone();
		config.color = Some(resolved);
		let trak = super::synthesize_video_trak(1, 90_000, &config, None).unwrap();
		let trex = mp4_atom::Trex {
			track_id: 1,
			default_sample_description_index: 1,
			..Default::default()
		};
		let catalog = run_fmp4(&super::encode_init(None, vec![trak], vec![trex]).unwrap());
		catalog.video.renditions.values().next().unwrap().color.clone()
	};

	assert_eq!(reimport(color(9, 16, 9, false)), Some(color(9, 16, 9, false)));

	// A partial colour leaves the rest unspecified, never the placeholder's BT.709.
	let mut primaries = hang::catalog::Color::default();
	primaries.primaries = Some(9);
	let back = reimport(primaries).unwrap();
	assert_eq!((back.primaries, back.transfer, back.matrix), (Some(9), None, None));
}

/// QuickTime's `nclc` carries the code points without a range, and keeps them; a code point past
/// 255 is refused.
#[test]
fn colr_nclc_keeps_its_code_points() {
	let nclc = mp4_atom::Colr::Nclc {
		colour_primaries: 9,
		transfer_characteristics: 16,
		matrix_coefficients: 9,
	};
	let color = super::colr_color(&nclc).unwrap().unwrap();
	assert_eq!(
		(color.primaries, color.transfer, color.matrix, color.range),
		(Some(9), Some(16), Some(9), None)
	);
	assert!(super::colr_color(&mp4_atom::Colr::new(300, 1, 1, false).unwrap()).is_err());
}

/// A declared boundary waits for decodable video; dependent fragments stay in the old group.
#[tokio::test(start_paused = true)]
async fn declared_boundaries_wait_for_sync_samples() {
	for passthrough in [false, true] {
		for explicit in [false, true] {
			let (init, (video_id, video_scale), _) = bbb_init();
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let consumer = broadcast.consume();
			let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
			let mut import = super::Import::new(broadcast, catalog.reserve()).with_passthrough(passthrough);
			import.decode(&init).unwrap();
			let snapshot = catalog.snapshot();
			let name = snapshot.video.renditions.keys().next().unwrap();
			let mut track = consumer.track(name).unwrap().subscribe(replay()).await.unwrap();
			for (index, sync) in [true, false, false, true, false, true].into_iter().enumerate() {
				if index == 1 {
					if explicit {
						import.cut().unwrap();
					} else {
						import.decode(&styp()).unwrap();
					}
				}
				let fragment = super::encode_fragment(
					info(video_id, video_scale, index as u32),
					&[sample(index as u64 * 1_000_000, sync, Some(1_000_000))],
				)
				.unwrap();
				import.decode(&fragment).unwrap();
			}
			import.finish().unwrap();
			let marker = usize::from(!passthrough);
			assert_eq!(
				drain_group_frames(&mut track),
				vec![3 + marker, 2 + marker, 1 + marker],
				"passthrough {passthrough}, explicit {explicit}"
			);
		}
	}
}

#[tokio::test(start_paused = true)]
async fn declared_boundaries_do_not_split_passthrough_fragments() {
	for passthrough in [false, true] {
		let (init, (video_id, video_scale), _) = bbb_init();
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let mut import = super::Import::new(broadcast, catalog.reserve()).with_passthrough(passthrough);
		import.decode(&init).unwrap();
		let snapshot = catalog.snapshot();
		let name = snapshot.video.renditions.keys().next().unwrap();
		let mut track = consumer.track(name).unwrap().subscribe(replay()).await.unwrap();
		let tick = video_scale.as_u64() / 24;
		let non_sync = 0x0001_0000;
		import
			.decode(&video_fragment(video_id, 0, tick, &[(0, 0, 0xA)]))
			.unwrap();
		import.cut().unwrap();
		import
			.decode(&video_fragment(
				video_id,
				tick,
				tick,
				&[(0, non_sync, 0xB), (0, 0, 0xC)],
			))
			.unwrap();
		import
			.decode(&video_fragment(video_id, 3 * tick, tick, &[(0, non_sync, 0xD)]))
			.unwrap();
		import
			.decode(&video_fragment(video_id, 4 * tick, tick, &[(0, 0, 0xE)]))
			.unwrap();
		import.finish().unwrap();
		assert_eq!(
			drain_group_frames(&mut track),
			if passthrough { vec![3, 1] } else { vec![3, 3, 2] }
		);
	}
}
